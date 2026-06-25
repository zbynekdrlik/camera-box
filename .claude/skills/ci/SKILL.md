---
name: ci
description: >
  CI pipeline for camera-box — artifact download, Discord CI notifications, and
  probe binary flow. Load when working on CI (#25 notify-on-red, CI jobs, artifact
  download, or Discord notification delivery).
---

# CI

## Artifact Download

```bash
# Main camera-box binary (Linux amd64)
gh run download --repo zbynekdrlik/camera-box -n camera-box-linux-amd64 --dir ./dist

# Probe tools — Linux (recording-verdict, frame-probe, camera-box-probe)
gh run download --repo zbynekdrlik/camera-box -n probe-tools-linux-amd64 --dir ./probe-bins

# Probe tools — Windows (recording-verdict.exe)
gh run download --repo zbynekdrlik/camera-box -n probe-tools-windows-amd64 --dir ./probe-win
```

**`gh run download` strips the +x bit** — always re-add:
```bash
chmod +x ./probe-bins/*   # or ./dist/camera-box
```

## Discord CI Notifications (#25)

camera-box CI posts failures to Discord via the NewLevelMedia Discord bot REST API,
channel `#notifications🔔` (ID `1257652233714270219`).

**Mechanism:** `POST https://discord.com/api/v10/channels/<CHANNEL_ID>/messages`
with `Authorization: Bot <token>` and `{content: "..."}`.
Discord returns HTTP 200 + the **created message object** — the `id` field is real
delivery proof (unlike a relay "accepted" ack).

**GitHub secrets:** `DISCORD_BOT_TOKEN` (sourced from `~/.claude/channels/discord/.env`)
+ `DISCORD_CHANNEL_ID` = `1257652233714270219`. The `notify-on-failure` job (`needs` all
gates, `if: failure()`) skips gracefully (`exit 0`) if either secret is unset.

**DEAD PATH — do NOT reuse:** `CLAUDE_DISCORD_WEBHOOK_URL` (the n8n relay at
`~/.claude/env`) ACCEPTS POSTs (HTTP 200 `Workflow was started`) but **no longer DELIVERS
to Discord** (retired). HTTP 200 from it is a liveness ack, NOT delivery.

**DM vs notifications channel:**
- `1491798759103795301` (`DISCORD_NOTIFICATION_CHANNEL_ID` in the channels .env) = bot↔user
  **DM** channel used by `notify-discord.sh` idle pings — this is a DM, NOT the shared
  #notifications channel. Cannot hold webhooks (404 Unknown Channel on the webhooks endpoint).

**Lesson:** a relay/webhook returning 2xx only means "accepted", never "delivered".
Confirm via the created-message `id` in the response body.

The Discord bot can't `fetch_messages` on these channels from an in-session Discord plugin
(not allowlisted) — verify delivery by the bot REST POST returning a message `id`.

## Probe Binary Flow — Run on stream.lan, NOT dev1

OBS records the 0.7–6 GB program file on stream box (10.77.9.204 — strong CPU, fast disks).
**Decode WHERE THE VIDEO IS** — run `recording-verdict` on stream against the LOCAL recording;
bring back ONLY the small verdict JSON (+ pixel-proof PNGs). dev1 holds NOTHING big.

```bash
# 1. Download Windows verdict for the commit under test
gh run download --repo zbynekdrlik/camera-box -n probe-tools-windows-amd64 --dir ./probe-win

# 2. Upload to stream box via win-stream-snv MCP FileUpload:
#    path='C:\camera-box\recording-verdict.exe'

# 3. Run on stream box via win-stream-snv Shell (see scripts/recording-verdict-on-stream.sh)
scripts/recording-verdict-on-stream.sh \
  --verdict-exe 'C:\camera-box\recording-verdict.exe' \
  --out-dir 'C:\camera-box\verdict-out' \
  --stream-rec 'C:\path\on\box\stream-REC.mp4' \
  -- --strih 'C:\path\on\box\strih-REC.mkv' \
     --stream 'C:\path\on\box\stream-REC.mp4' \
     --min-secs 300 --json 'C:\camera-box\verdict-out\verdict.json' \
     --out-dir 'C:\camera-box\verdict-out\pixel-proof'

# 4. Pull back ONLY verdict JSON + pixel-proof PNGs via win-stream-snv FileDownload.
```

`scripts/recording-e2e.sh` defaults to this on-stream flow (`VERDICT_ON_STREAM=1`).
`VERDICT_ON_STREAM=0` selects the legacy decode-on-dev1 path (kept for boxes without
uploaded verdict.exe).

scp/ssh to Windows (stream.lan) is **DENIED** on this rig — use the win-stream-snv MCP.
ffmpeg/ffprobe are already installed on stream.lan (winget; ffmpeg 8.0.1 on PATH).

## Probe is CI-only locally — never compile `--features probe` on dev1 (#185)

The shared dev1 `target/` has no GC (rust-lang/cargo#5026). Compiling the heavy `probe` feature
locally (qrcode/rqrr/image/drm/lz4_flex + 5 `required-features=["probe"]` bins) across the day's
workers ballooned it to 18GB and filled the disk. The fix: **local Tier-0 cheap-checks run DEFAULT
features only** — `cargo clippy --all-targets -- -D warnings` (NO `--all-features`), `cargo test
--no-run`. Probe is compile-checked + built ON CI (`ci.yml` runs `--all-features`; #101 C++ gate +
#192 probe-tools artifact). See CLAUDE.md "Local Build Policy".

**Convention when adding an integration test that imports `camera_box::probe::…`:** gate the whole
file with `#![cfg(feature = "probe")]` (after the `//!` doc block, before the first `use`) — exactly
like `tests/recording_decode.rs`. An UNGATED probe test forces the local default-feature
clippy/test --no-run to compile the probe feature and re-balloons `target/`. CI (`--all-features`)
still runs gated tests. `tests/local_build_policy_bounds_target.rs` enforces both invariants (it
FAILS if any probe-using test is ungated or if the CLAUDE.md local gate command block uses
`--all-features`/`--features probe`).

**Backstop:** `scripts/install-git-hooks.sh` installs a non-blocking pre-push hook running
`scripts/purge-target.sh` (cargo clean when `target/` > `${THRESHOLD_MB:-4096}`; SKIPS while a live
E2E has probe binaries running — matched by truncated `/proc` comm, e.g. `recording-verdi`,
`camera-box-prob`, NOT a `pgrep -f` cmdline substring which false-positives on the script's own args).
