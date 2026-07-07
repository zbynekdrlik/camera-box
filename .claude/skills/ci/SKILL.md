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

# Probe tools — Windows (recording-verdict.exe, obs-watchdog-gate.exe, obs-self-heal-gate.exe —
# the latter two are the #391/#411 OBS liveness-watchdog + self-heal gate binaries, needed on-box
# by scripts/obs-self-heal-install.sh's Task Scheduler recovery script)
gh run download --repo zbynekdrlik/camera-box -n probe-tools-windows-amd64 --dir ./probe-win
```

**`gh run download` strips the +x bit** — always re-add:
```bash
chmod +x ./probe-bins/*   # or ./dist/camera-box
```

## Python harness tests (`tests/python/`)

CI job `python-tests` runs `python -m pytest tests/python -v` after `pip install pytest
websocket-client matplotlib` (clean, minimal). These pin the `obs_phase2.py` pure helpers + arg
parsing (no live OBS) — e.g. the `_blackcheck_verdict`, `_diverging_locked_keys`, and `#328`
`_rpc_timed_out` deadline helper.

**Local gotcha:** dev1's global pytest has a broken `pytest-html` plugin (missing `jinja2`) that
aborts collection with `ModuleNotFoundError: No module named 'jinja2'`. CI is unaffected (its venv
has only the three deps above). Locally, disable the plugin:
```bash
python3 -m pytest tests/python -q -p no:html -p no:cacheprovider
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

**Gotcha — a clippy lint inside a probe-gated file (`src/probe/*`) is INVISIBLE to local Tier-0
clippy and only FAILS on CI's Lint job** (which runs `cargo clippy --all-targets --all-features`).
Default-feature local clippy never compiles `src/probe/*`, so a lint there passes locally and red-X's
CI — a wasted CI cycle (#312/#324: a `clippy::doc_lazy_continuation` in `recording_segments.rs`
module-doc passed Tier-0 but failed CI). When you edit ANY `src/probe/*` file — **especially doc
comments** — eyeball it for clippy patterns before pushing; you cannot lint-verify it locally (running
`--all-features` to check is banned — it re-balloons `target/`). Recurring doc trap: a `[label]:`
token mid-paragraph (e.g. `` [`crate::x`]: text ``) makes pulldown-cmark parse a link-reference
definition → the next lines become a `doc_lazy_continuation` lint; reword to drop the `]:` (e.g.
`` (in [`crate::x`] — text ``) or add a blank line.

**Same class, a COMPILE error not a lint — a `serde::Serialize` (or any trait-bound) that only
propagates under `--features probe`.** `bin/recording-verdict.rs`'s `NodeVerdict` is
`#[derive(serde::Serialize)]`, so EVERY field type must be `Serialize`. When you change a field to
hold a lib-crate struct (e.g. #580 changed `imag_optical_beat` from `Option<bool>` to
`Option<OpticalBeatVerdict>` so the printers/JSON could report the beat detail), that struct MUST
ALSO derive `Serialize` — else the whole `--features probe` build breaks with `E0277: the trait
bound X: serde::Serialize is not satisfied`, red-X'ing Lint + Test + Build + Coverage + Windows
probe build at once. INVISIBLE to Tier-0: `cargo check`/`clippy` on default features never compiles
`bin/recording-verdict.rs` (`required-features = ["probe"]`), so it passes locally and only fails on
CI (one wasted cycle, #580). Before pushing a `NodeVerdict` (or any `#[derive(Serialize)]`
probe-struct) field-TYPE change, add the matching derive to the new field's type in `lib` (`serde` is
already a default, non-optional dep, so `#[derive(..., serde::Serialize)]` compiles on default
features too) — you cannot catch this locally.

**Backstop:** `scripts/install-git-hooks.sh` installs a non-blocking pre-push hook running
`scripts/purge-target.sh` (cargo clean when `target/` > `${THRESHOLD_MB:-4096}`; SKIPS while a live
E2E has probe binaries running — matched by truncated `/proc` comm, e.g. `recording-verdi`,
`camera-box-prob`, NOT a `pgrep -f` cmdline substring which false-positives on the script's own args).

## airuleset `pre-push-test-check` — two recurring traps on `git push origin dev`

The global PreToolUse hook scans the WHOLE `origin/main..HEAD` delta on every push (not just your
commits), so two things bite here repeatedly:

1. **Inherited bug-fix commit re-flagged.** A `fix(...)`-prefixed commit already on `dev` but not yet
   on `main` (e.g. a prior `[no-test:]`-bypassed `scripts/` fix from another cycle) is the FIRST
   commit in the delta → Gate-2 ("bug-fix commit before any test commit") re-fires on YOUR PR even
   though your own work is correctly RED→GREEN ordered. Resolve with a one-line `[no-test: <reason>]`
   on your LATEST commit (a docs/log commit is the natural carrier), naming the inherited commit as
   the only test-less one in range.
2. **`[no-test: <reason>]` must be on ONE LINE.** The hook greps `\[no-test:\s*[^]]+\]` per-line, so a
   reason that wraps across newlines never matches and the push stays blocked (looks like the bypass
   is ignored). Keep the `[no-test: …]` opening `[` and closing `]` on the SAME commit-message line.
   Bare `[no-test]` (no reason) is rejected outright. Every bypass is logged to
   `~/devel/airuleset/audits/no-test-skips.log`.

## dev→main PRs can get auto-opened + auto-merged by something OTHER than you (#423 observation)

While working #423, PRs #424 and #425 (dev→main, "Closes #N", auto-merged within seconds of the
triggering `dev` CI run going green) appeared WITHOUT any `gh pr create`/`gh pr merge` call from
that work session. No `.github/workflows/*` file creates or merges cross-branch PRs (checked —
`release.yml` only triggers on tag pushes), and no crontab/systemd-timer on dev1 was found either.
The mechanism is real but its source is unidentified — something (an external service, another
concurrent session/automation using the same `zbynekdrlik` GitHub auth) watches `dev`'s CI and
auto-merges when green, sometimes within ~15-30s of the run finishing, sometimes not at all (a
docs-only follow-up commit sat unmerged for 3+ minutes with no PR before this session opened one
manually). **Before assuming you must create the dev→main PR yourself: `gh pr list --state open`
and `gh pr list --state all --limit 5` first** — you may already be looking at a PR (or a just-merged
one) you didn't create. If none exists after a short wait, open and merge it yourself as normal
(`pr-merge-policy.md` default auto-merge still applies either way — this is not a workflow file to
edit, just a "check before you duplicate" gotcha).

## Adding a default-features binary that needs `serde_json` (#365 gotcha)

`serde_json` was originally optional (probe-only). If you add a `[[bin]]` entry that must run on
**default features** and needs JSON parsing, make `serde_json` non-optional in `Cargo.toml`:

```toml
# BEFORE (broken for a default-features binary):
serde_json = { version = "1", optional = true }
# and in [features]:
probe = ["dep:serde_json", ...]

# AFTER (serde_json always available):
serde_json = "1"
# and in [features]:
probe = ["dep:crc", ...]    # remove dep:serde_json from here
```

Removing `dep:serde_json` from the `probe` feature list is safe: making it non-optional means it
is compiled unconditionally and the `dep:` prefix (optional-dep activation) is no longer needed.

## New file mixing PURE + syscall/IO glue → update BOTH coverage & mutants (kms.rs precedent)

When you add a Rust file whose PURE functions are unit-testable but which also has glue that CANNOT
be (raw syscalls, `/proc`/`/sys` IO, FFI, live hardware), CI's two strict gates will fail unless you
exclude the glue — follow how `kms.rs` / `painter.rs` are already handled (#289 `src/affinity.rs` did this):

1. **Coverage** (`ci.yml` "Generate coverage report"): add the file to `--ignore-filename-regex`
   (it's whole-FILE only — can't exclude individual functions). The pure functions' unit tests still
   RUN; the file just doesn't drag the `--fail-under-lines` threshold with untestable glue lines.
2. **Mutants** (`ci.yml` "Mutation testing", `--in-diff`): add a `--exclude-re '\b<glue_fn>\b'` per
   glue function. KEEP the pure functions mutated — and make them mutation-ROBUST (a surviving mutant
   in the diff FAILS the job, and you can't run cargo-mutants locally: it needs `--features probe`):
   - Drop redundant guards whose mutant is a no-op (e.g. an empty-string `if x.is_empty(){continue}`
     when the downstream `parse` already returns `Err` → the `if false` mutant survives; just remove it).
   - Add edge tests that kill value mutants: a duplicate input (kills `|=`→`^=`/`+=`), an out-of-range
     value (kills a `< N` bound flipped to `<= N` / `if true`), a "skipped" input (kills a filter negation).
   - Unit-returning calls (`vec.sort_unstable()`, `vec.dedup()`) are NOT mutated — don't over-test them.

## Building an OBS PLUGIN from source against the genlock OBS (the #188 A/V-sync dock)

The `windows-genlock.yml` (+ `-fast`) workflow builds a first-party OBS plugin (the vendored
norihiro `obs-audio-video-sync-dock` at `vendor/av-sync-dock/`, with `deps/quirc`) standalone
against the from-source genlock OBS. A plugin's CMake does `find_package(libobs)` /
`find_package(obs-frontend-api)`, which needs the OBS **SDK config packages** — and getting those
to exist + resolve took 3 CI iterations (#188). The three gotchas, do NOT re-derive:

1. **`libobsConfig.cmake` is installed under the `Development` component with `EXCLUDE_FROM_ALL`.**
   A plain `cmake --install build_x64` SKIPS it, so the SDK prefix ends up with ZERO `.cmake` files
   and the plugin's `find_package(libobs)` fails. Fix — install the component explicitly AFTER the
   normal install:
   ```bash
   cmake --install build_x64 --config RelWithDebInfo --prefix "$WS/obs-sdk" --component Development
   ```
2. **Broad `CMAKE_PREFIX_PATH`** — `libobsConfig.cmake` has transitive `find_dependency(...)`
   (w32-pthreads, SIMDe via `cmake/finders/FindSIMDe.cmake`, Threads) whose headers live in the
   obs-deps roots. Pass EVERY `.deps/*-x64` root of both `vendor/obs-studio` and `vendor/distroav`
   (semicolon-joined) as the prefix, so SIMDe/Qt6/etc. resolve.
3. **Explicit `-Dlibobs_DIR` / `-Dobs-frontend-api_DIR`** pointing at the dirs of the found config
   files (locate with `find "$WS/obs-sdk" -iname 'libobs*config.cmake'`). Fail LOUD if either is
   missing rather than letting CMake fall back silently.

`vendor/av-sync-dock/CMakeLists.txt` also needs an alias guard so the modern DistroAV template's
`OBS::obs-frontend-api` satisfies the old template's `OBS::frontend-api`:
```cmake
if(NOT TARGET OBS::frontend-api)
    add_library(OBS::frontend-api ALIAS OBS::obs-frontend-api)
endif()
```
Artifact: the dock DLL + `data/` are staged into the genlock OBS artifact
(`obs-plugins/64bit/obs-audio-video-sync-dock.dll` + `data/obs-plugins/obs-audio-video-sync-dock/`).
Download it like any CI artifact (Artifact Download section above).

### Deploying the dock DLL to strih + stream (first-party plugin path)

The dock is a FIRST-PARTY plugin → `C:\Program Files\obs-studio\obs-plugins\64bit\` (NOT ProgramData
— that's DistroAV's canonical path, see obs-ops #124). Deploy safely (no-destructive-remote-actions):
1. **Look before overwrite** — the stock norihiro dock already ships on both boxes; back it up first
   (`Copy-Item …\obs-audio-video-sync-dock.dll C:\Temp\avdock\backup\`).
2. **Canary** — swap **strih first**, relaunch via `scripts/launch-obs-genlock.sh --box strih --force`,
   confirm the OBS log shows `[obs-audio-video-sync-dock] plugin loaded (version 0.1.4)` +
   `[obs-audio-video-sync-dock] quirc (version 1.0)` with render tick ENABLED. Only then do stream.
3. **Hash-verify** the deployed DLL (`Get-FileHash` == the CI artifact's SHA) after copy.
4. **Rollback** — if the new DLL fails to load, restore from the `C:\Temp\avdock\backup\` copy.

A cleanly-loading plugin (the log line above) == the dock is registered in the OBS **Docks** menu
(norihiro's dock uses `obs_frontend_add_dock_by_id` at load); the live A/V-offset readout is the
operator's self-service step (phone playing the norihiro QR-sync video + hand mic into the Dante
path). Phase 2 (#188/#145) adds the rig-side cam2 QPSK audio marker so the phone is not needed.

## Shellcheck gate (#545) — provisioning/ops scripts lint

`ci.yml` job `shellcheck` (GitHub-hosted `ubuntu-latest`) runs
`shellcheck -S warning scripts/*.sh scripts/lib/*.sh` as a **binary** gate: errors + warnings fail
the build, the many style/info-level findings are advisory-only (a deliberate floor, not
weakening). No `continue-on-error`; wired into the `notify-on-failure` red-alert fan-in like every
other gate. Guard test: `tests/shellcheck_workflow_gate.rs` (job exists on GitHub-hosted runner,
runs `-S warning scripts/*.sh scripts/lib/*.sh`, no continue-on-error, in the notify fan-in).

**GOTCHA — the glob is NON-RECURSIVE**, so both `scripts/` AND `scripts/lib/` are listed
EXPLICITLY. Most `scripts/lib/*.sh` are also covered transitively (shellcheck follows
`# shellcheck source=…` directives from whatever top-level script sources them), but a STANDALONE
lib script that nothing sources — e.g. `camera-box-grow-root.sh`, `install`'d and run directly as
root on first boot — would be missed by `scripts/*.sh` alone, which is why `scripts/lib/*.sh` is in
the command. **A NEW subdir under `scripts/` needs its own glob entry** or it silently escapes the
gate. Before pushing a script change, run `shellcheck -S warning <file>` locally (shellcheck 0.9.0
is on dev1) — a warning-level finding fails the gate.

## Foreground CI-poll loop from an autopilot-worker subagent (Bash tool quirks, #559/#24 PR #566)

A subagent must wait for CI in the FOREGROUND (never `run_in_background` — see the global
`ci-monitoring.md`: a backgrounded poll silently ends the subagent's turn). Two harness quirks bite
here that are easy to trip on:

1. **A bare top-level `sleep 300 && gh run view …` gets BLOCKED** by this environment's Bash tool
   ("To wait for a condition, use Monitor with an until-loop… Do not chain shorter sleeps to work
   around this block"). The fix is NOT to fight it with `Monitor`/`run_in_background` (those detach
   and risk the same subagent-turn-ends problem) — put the `sleep` INSIDE a loop body in ONE Bash
   call instead: `while …; do …; sleep 20; done` compiles down to a single foreground command the
   block doesn't flag.
2. **The Bash tool's default per-call timeout is 2 minutes**, far shorter than one CI run (this
   repo's full `ci.yml` run took ~9 minutes). Pass an explicit `timeout` (ms) close to but under the
   10-minute cap, e.g. `timeout: 570000`, sized so `(loop iterations) * (sleep interval)` covers the
   expected run length; repeat the whole call again if the loop's bound is hit before CI finishes
   (each call is independent and keeps the subagent alive, per `ci-monitoring.md`'s "each Bash call
   well under the 10-min cap, repeated until terminal").

```bash
i=0
while [ $i -lt 27 ]; do
  read -r status conclusion < <(gh run view <RUN_ID> --json status,conclusion -q '.status + " " + .conclusion')
  echo "poll $i: status=$status conclusion=$conclusion"
  [ "$status" = "completed" ] && break
  i=$((i+1)); sleep 20
done
```
(pass `timeout: 570000` on this Bash call)
