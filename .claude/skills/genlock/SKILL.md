---
name: genlock
description: >
  Genlock OBS build — current deployment state on strih+stream, monorepo direction,
  fork history. Load when working on genlock (#8/#11), vendored OBS/DistroAV,
  drift-guard, or anything touching the broadcast OBS on strih/stream.
---

# Genlock

## #257 — PRODUCTION-SAFE HARD-LOCK (the CURRENT state; supersedes the env model below)

**The genlock build is hard-locked and ENV-FREE. There is NO `OBS_GENLOCK_*` / `OBS_BURN_*` env any
more** — the old env knobs were removed in #257. The current model:

- **Render tick + ts-align are ALWAYS ON in the build** (`obs-video.c genlock_tick_enabled` /
  `obs-source.c genlock_ts_align_enabled` just `return true`). No `OBS_GENLOCK_WALL_CLOCK` /
  `OBS_GENLOCK_TS_ALIGN`. The proof is the OBS-log line `genlock: … render tick ENABLED` (drift-guard
  capability marker + the launch-wrapper log-verify key on it).
- **Genlock latency is a BUILD CONST: 3 ms, floor 3** (`GENLOCK_LATENCY_MS_DEFAULT`/`_MIN` = 3 in
  `obs-source.c`, mirrored in `src/probe/genlock.rs`). No `OBS_GENLOCK_LATENCY_MS` / `_RESERVE_MS`.
  The PER-SOURCE override is the DistroAV UI int **"Latency (ms)"** (min 3, max 2000, default 3),
  applied at runtime via `obs_source_set_genlock_latency_ms` (clamps 1→3, 0→3). preload is fully
  internal/auto (no `OBS_GENLOCK_PRELOAD_FRAMES`).
- **DistroAV NDI source UI is a HARD WHITELIST** (`ndi_source_getproperties`): exactly four props —
  `PROP_SOURCE`, `PROP_GENLOCK_FIFO` (Genlock, default ON), `PROP_GENLOCK_LATENCY_MS_SRC`
  (Latency ms), `PROP_BURN` (Measurement burn, default OFF). Every other DistroAV knob is removed
  from the UI and FORCED to a certified value (`force_genlock_certified_settings` ← the
  `GENLOCK_FORCED_SETTINGS` const table, the complement of `GENLOCK_WHITELIST_PROPS`).
- **Measurement burn is a per-source `genlock_burn` bool, runtime, NO restart** — toggled over OBS
  WebSocket `SetInputSettings genlock_burn` (`scripts/obs_burn_filter.py add|remove`, driven by
  `scripts/rig-mode.sh test|event`). libobs stores it (`obs_source_set/get_genlock_burn`); the QR
  burn filter reads `obs_source_get_genlock_burn(parent)` each render. run_id/corner come from the
  box's **host role** (strih 911002/bottom-left, stream 911004/bottom-right — no `OBS_BURN_RUN_ID/
  _CORNER`), qr size canvas-relative auto (no `OBS_BURN_QR_PX`).
- **`launch-obs-genlock.sh` is env-free** — relaunch = clear sentinels → Start-Process cwd=bin\64bit
  → log-verify `render tick ENABLED` + DistroAV. No PEB env check (there is no env to carry). The
  `--mode test|event` is gone (the burn is a WS toggle, not a relaunch).
- **drift-guard #246 burn facet** now means "no prod source has `genlock_burn=on`" (read over WS),
  not "no `OBS_BURN_*` in Machine env". `genlock_wall_clock=1` is a build-default sentinel proven by
  the capability marker.

The `#235` env model and the `STALE-ENV TRAP` notes below are HISTORY (pre-#257) — there is no
genlock env to lose any more. Tests: `tests/genlock_preload.rs`, `tests/distroav_genlock_lockdown.rs`,
`tests/launch_obs_genlock.rs`, `tests/rig_mode.rs`, `tests/drift_guard.rs`, `tests/burn_payload_parity.rs`.

**PLAYBOOK HYGIENE (when you kill an env var / identifier):** grep the WHOLE playbook —
`grep -rE 'OBS_GENLOCK_|OBS_BURN_' .claude vendor/README.md` (and the same for any newly-killed
name) — and historicize/remove EVERY operator-facing instance, not just the obvious skill. The #261
no-env rewrite covered genlock + obs-ops + drift-guard + vendor/README, but `.claude/skills/e2e/SKILL.md`
still had active `$env:OBS_BURN_QR=1` launch steps (filed #262) — a killed knob hides in the skill you
didn't think to open.

## (HISTORY, pre-#257) Genlock latency env knobs — ALL REMOVED in #257

⚠️ **These env vars NO LONGER EXIST.** Latency is now a build const (3 ms, floor 3) with the
per-source override in the DistroAV source UI — see the #257 section at the top. This section is kept
only to explain the lineage; **never set any `OBS_GENLOCK_*` env — there are none.**

Pre-#257, genlock latency went through ONE env knob in MILLISECONDS (#235), which had superseded an
earlier confusing dual model (`OBS_GENLOCK_PRELOAD_FRAMES` whole frames + `OBS_GENLOCK_RESERVE_MS`
ms, reserve overriding preload only under TS_ALIGN):

- `OBS_GENLOCK_LATENCY_MS=N` *was* THE held latency in ms — release deadline `wall_now − N·1e6` (#184),
  implying ts-align ON. `OBS_GENLOCK_RESERVE_MS` *was* a back-compat alias; `OBS_GENLOCK_TS_ALIGN` and
  `OBS_GENLOCK_PRELOAD_FRAMES` *were* the older gates. **All four were deleted in #257** — render tick
  + ts-align are now build defaults and the latency is the UI int (floor 3).
- **preload is internal** (auto-derived FIFO depth = 1 frame for jitter/dropout resilience,
  latency-free under the ms deadline so the #110 0-loss floor holds) — unchanged, still true.
- **Display:** the OBS startup + audit log show `genlock: latency = N ms (≈ M frames @ Ffps)` — MS
  PRIMARY, frame-equivalent in PARENS (this log format is unchanged across #257). Pre-#257 the latency
  was env-set and the DistroAV source props showed only a READ-ONLY `Genlock latency = N ms` label;
  post-#257 that prop is the EDITABLE "Latency (ms)" int (min 3) — a user sets ONE ms value in the UI
  (not an env) and never reasons about preload-vs-reserve precedence.

Resolution + display mirrored & unit-tested in `src/probe/genlock.rs` (resolve_latency_ms /
ms_to_frames / genlock_auto_preload / format_latency_label) with vendored-source guards keeping the
C (`vendor/obs-studio/libobs/obs-source.c` genlock_latency_ms) + DistroAV in lock-step.

**GOTCHA — a `genlock:` log line has THREE consumers; change all of them together.** When you edit a
`genlock:` line in `obs-source.c` (e.g. the #235 rename from `sub-frame jitter reserve = N ms` to
`latency = N ms (≈ M frames)`), you MUST update in the SAME PR: (1) the `tests/genlock_preload.rs`
vendored-source guard string, (2) `scripts/launch-obs-genlock.sh` (#128 wrapper) log-verify regex,
(3) `scripts/drift-guard.sh` `genlock_capability_from_log` regex (which keys on the build-unique
`genlock:` lines to catch a stock-OBS #119 wrong-build). Missing any one silently breaks the launch
verify or capability detection while every other test stays green.

## (HISTORY, pre-#257) Sub-frame ms reserve (#184) + the stale-env launch trap — env removed

⚠️ **No genlock env any more (removed in #257).** Kept for lineage only; **never set an
`OBS_GENLOCK_*` env.**

Pre-#257, `OBS_GENLOCK_RESERVE_MS=N` (the #235 latency alias) switched the genlock ts-align release
deadline from the whole-frame `preload·interval` (=33ms@30fps) to `wall_now − N·1e6` (ms-granular);
`latency_ms=0` was the #136 frame path verbatim. Validated zero-loss at 3 ms on BOTH hops (strict
recording-verdict `overall_pass`, FIFO audits `latency_ms=3 reserve_ms=3`, 0 new underruns during
active feed). Prod ran at 3 ms via the Machine env; **#257 cleared all genlock env on both boxes and
made render tick + ts-align build defaults with the 3 ms floor** — so prod still runs at 3 ms, now from
the build const + per-source UI int, NOT env. Rollback DLL (pre-#184 whole-frame build):
`C:\obs-backup\pre-184\obs.dll` (`cdce8c3a…`).

The pre-#257 **STALE-ENV TRAP** (a win-* MCP shell inheriting a stale env snapshot, so an OBS launched
from it silently ran the whole-frame path with no latency line) is **moot now — there is no genlock
env to inherit.** The lasting lesson survives the env: a running OBS's genlock state is read from the
OBS log (`genlock: … render tick ENABLED` + `genlock: latency = N ms` once a genlock_fifo input is
live), NEVER from an env read. NB the FIFO audit's `underruns=` is CUMULATIVE per OBS process — a huge
value can be IDLE accumulation between runs; what matters is the DELTA during active feed (0 = clean).

**dev1 ⇄ rig transfers:** dev1 file-drop (`:8788`) is NOT reachable from the rig. dev1→stream
binary push works via SMB admin share `smbclient //10.77.9.204/C$ -U "newlevel%newlevel"` (newlevel
is admin). strih→stream SMB works (net use \\10.77.9.204\C$); strih's own C$ DENIES dev1.

**recording-verdict cam1 contiguity:** the STREAM-ONLY single-recording cam1 read is SOFTENED and
may OVER-COUNT real_drops (#133/#216) — supply BOTH `--strih` + `--stream` for the STRICT cam1
verdict (the both-hops #184 run: softened stream-only = 37 cam1 drops, strict = 0).

## Deployed State (strih + stream, since 2026-06-13)

Both production broadcast OBS boxes upgraded in-place to the camera-box genlock build.

| | strih (10.77.9.202) | stream (10.77.9.204) |
|---|---|---|
| OBS version | 32.1.2 | 32.1.2 |
| Build SHA | cf7b0606 | cf7b0606 |
| Genlock active | YES | YES |
| Genlock env | none — build default (#257) | none — build default (#257) |

(The version/SHA row above is the 2026-06-13 baseline; the current deployed bytes are the #257 build —
see `docs/autopilot-log.md` for the live obs.dll / distroav.dll SHAs and the drift-guard `--compare`
manifest check. Genlock is no longer gated by any env: it is a build default since #257.)

**Genlock is ACTIVE:** both boxes log `genlock: wall-clock-slaved render tick ENABLED` at OBS launch.
stream's live `NDI 2ME PGM` input has `genlock_fifo=True` → production strih→stream hop is genlocked.

**Measured on production (2026-06-13, synth→strih→stream, strict gate, 120s):**
VERDICT=PASS, 0 dropped on both hops (0/3556 and 0/3535 single-copy), p99 77/92 ms.

Camera→strih hops: genlock tick active but camera ingests are NOT genlock_fifo yet
(camera-box senders must wall-pace first, #11).

**Backups (instant rollback):** `C:\obs-backup\2026-06-13\` on each box.
Rollback = stop OBS, robocopy backups back over `C:\Program Files\obs-studio` + the
ProgramData distroav, clear `%APPDATA%\obs-studio\.sentinel\*`, relaunch.

## Bundle version integrity (EPIC #125)

Two LAYERS guard "the deployed stack is the build we think it is":

- **drift-guard (#45)** — marketing versions + critical settings (OBS 32.1.2 / DistroAV
  6.2.1 / fps / genlock gate / input latency / canonical plugin path).
  `scripts/drift-guard.sh --check-pins` (CI) + `--compare` (live box). Pins live in
  the `vendor/README.md` version + settings tables. The version+settings facet alone
  cannot catch stale-BYTES-of-the-right-version (that was #119: a pre-#97 DistroAV of
  version 6.2.1 → preload inert) — **#122 (below) closes that** with a per-component
  BUILD-SHA + capability check.
- **per-component SHA manifest (#120)** — the windows-genlock build emits
  `stage/BUNDLE_MANIFEST.json` via `scripts/genlock-manifest.sh` (unit-tested
  `tests/genlock_manifest.rs`): `components[]` = each rebuilt component's pinned
  version + vendored subtree commit (DistroAV version cross-checked vs
  `vendor/distroav/buildspec.json`, same source-of-truth as drift-guard) + NDI
  `min_version`; `files[]` = every shipped file's sha256+size (walked from `stage/`,
  self-consistent by construction). `--check FILE --stage DIR` is the consistency gate
  (exit 21 on sha-drift / extra / missing file). Both windows-genlock.yml and
  windows-genlock-fast.yml generate + assert it. **The build genuinely rebuilds OBS +
  DistroAV from `vendor/` source — zero checked-in/cached DLLs** (`git ls-files vendor |
  grep .dll` = EMPTY), so #119's stale-prebuilt root cause is structurally gone.
  **GIT-BASH FOOTGUNS (#239) — this script runs under `set -euo pipefail` on the
  windows-2022 runner's git-bash, where the ~2000-file real bundle hits races the 5-file
  Linux unit stages NEVER do (a Windows-only break can pass every PR — full build is
  workflow_dispatch-only, see #240):** (1) **SIGPIPE poisons pipefail** — a per-item
  `printf … | grep -q…`/`… | head -1` lets the downstream early-exit SIGPIPE the upstream,
  and pipefail nondeterministically marks the pipeline failed → ~half a VALID bundle
  falsely "not in manifest" (exit 21 on correct bytes). Fix: single-pass `comm` over
  `LC_ALL=C`-sorted lists (extra=`comm -23`, missing=`comm -13`, both=`comm -12`), and
  `sed '…;q'` instead of `| head -1`. **`comm` MUST run `LC_ALL=C` matching the sort** or it
  sees the lists as unsorted and emits garbage. (2) **proc-sub can truncate** — `done < <(…)`
  FIFO can be cut short on git-bash; materialise the list into a var + iterate with `<<<`,
  and `assert_manifest_complete EXPECTED ACTUAL` (exit **22**, distinct from --check's 21)
  fails LOUD at generation if intended-count ≠ written-count so a partial manifest never
  reaches --check. RED→GREEN must be proven at the SHELL level (Tier-0 blocks the Rust test
  runner); the Windows-specific fix is ONLY verifiable by dispatching `windows-genlock.yml`
  on your HEAD — the broken step never ran green on a real Windows bundle before.
- **per-component BUILD SHA + capability (#122, DONE)** — drift-guard `--compare` now
  CONSUMES `BUNDLE_MANIFEST.json`: supply `manifest=<path>` and it ALSO checks the live
  rig's `obs.dll`/`distroav.dll` Get-FileHash SHA256 vs the manifest's `files[]` entry
  (matched by BASENAME → both the flat fast-dll `obs.dll` and the nested full-bundle
  `bin/64bit/obs.dll` + `obs-plugins/64bit/distroav.dll` resolve) AND the genlock
  CAPABILITY marker text (`genlock_capability=` — the build-unique `render tick ENABLED`
  / `sub-frame jitter reserve` / `timestamp-aligned release` log lines). A STOCK OBS
  32.1.2 (same version, different bytes, emits NO genlock marker) → DRIFT (exit 20) even
  though every version/setting line reads OK — closes the #119 gap the marketing-version
  facet alone could not. Facet is OPT-IN: no `manifest=` → historic version-only contract;
  with it, an unread live SHA/capability is UNKNOWN (exit 11), never a silent clean. New
  pure fns `manifest_sha_for_component` + `genlock_capability_from_log` +
  `drift_check_capability` (tested in `tests/drift_guard.rs`). Driven by `/drift-guard`
  step 1d (Get-FileHash the DLLs read-only + grep the genlock markers + `gh run download`
  the build's manifest). GOTCHA: `genlock_capability_from_log` MUST `return 0` on the
  absent case (empty output IS the "stock" signal) — a bare `[ -n "$line" ] && echo 1`
  returns 1 and trips the test harness's `set -e`. LIVE-PROVEN both boxes 2026-06-25:
  obs.dll `24e22357…` (= #184 fast manifest), distroav.dll `66cea70…` (= full bundle),
  marker present → NO DRIFT; a wrong SHA + no-marker log → exit 20.
- **whole-bundle post-deploy byte/SHA verify (#121, DONE)** — #122 above checks only the
  two genlock DLLs; #121 raises it to deploy-from-clean-tree's contract: drift-guard
  `--compare` now also takes `bundle_hashes=<relpath=sha256,…>` (every deployed bundle
  file's live `Get-FileHash`, gathered off the box) and walks the manifest's WHOLE
  `files[]`, FAILING on ANY mismatch (DRIFT exit 20) or any unread file (UNKNOWN exit 11)
  — so a partial/corrupted deploy where even one NON-DLL file (`obs64.exe`, a first-party
  plugin, a locale) is stale can never pass. New pure fns `manifest_all_paths` +
  `manifest_sha_for_path` + `observed_sha_for` + `drift_check_all_files` (tested in
  `tests/drift_guard.rs`). The facet is OPT-IN and SUPERSEDES the #122 two-DLL SHA checks
  when `bundle_hashes=` is supplied (it already covers obs.dll + distroav.dll by exact
  path); without it the #122 hot-swap obs.dll-only verify is unchanged. The deploy step
  ALSO records a `DEPLOYED_MANIFEST.json` next to the install on each box (the live
  per-file `Get-FileHash` set, same shape as `BUNDLE_MANIFEST.json`) so the deployed bytes
  are auditable on the box after the fact. Driven by `/drift-guard` post-deploy verify
  (gather every file's `Get-FileHash` → `bundle_hashes=`). Both consume
  `BUNDLE_MANIFEST.json`.
- **single canonical OBS plugin-load path (#124)** — OBS scans MULTIPLE module
  locations (`C:\Program Files\obs-studio\obs-plugins\64bit` first-party,
  `C:\ProgramData\obs-studio\plugins\<plugin>\bin\64bit` global,
  `%APPDATA%\obs-studio\plugins\<plugin>\bin\64bit` per-user), so the SAME
  `distroav.dll` in more than one lets a **stale copy silently shadow the intended
  build** (that's #119 in another guise). **CANONICAL = `C:\ProgramData\obs-studio\
  plugins\distroav\bin\64bit\distroav.dll` — exactly ONE copy, there.** Verified live
  on strih + stream 2026-06-25: one `distroav.dll` per box (663040 B), loaded by the
  Program Files genlock `obs64.exe`, render tick ENABLED; **none** under
  `Program Files\obs-studio\obs-plugins\64bit` (the `data\obs-plugins\distroav` folder
  there is resources/locale, not the binary). First-party OBS plugins ship in
  Program Files\obs-plugins; DistroAV is the one in ProgramData — a deploy MUST NOT
  also drop `distroav.dll` into Program Files\obs-plugins (re-creates the shadow). The
  drift-guard now reads `distroav_dll_paths` (every `distroav.dll` location across the
  scan paths, gathered via win-* MCP — `/drift-guard` step 1c) and FAILS if there is
  more than one, or the lone one is off the canonical path. Pin row +
  `drift_check_plugin_paths` (tested in `tests/drift_guard.rs`) live in
  `vendor/README.md` under `canonical_plugin_path`.
- **prod burn guard + `--status` (#246, DONE; burn model updated by #257)** — the measurement burn
  must NEVER be left on in prod. **Pre-#257** it was a launch-env QR burn
  (`OBS_BURN_QR`/`OBS_BURN_QR_PX`/`OBS_BURN_RUN_ID`); RUN 235001 set them in **Machine** scope on
  stream+strih → QR drew on the LIVE broadcast (survives reboot) — the incident this guard exists for.
  **#257 removed all `OBS_BURN_*` env**: the burn is now a per-source `genlock_burn` bool toggled at
  runtime over OBS WebSocket (no env, no restart). drift-guard `--compare` keeps the `burn_env=` key
  for contract stability, but its value is now the **`genlock_burn` WS state** read off each
  program-feeding source (`none` when all off, else a `SOURCE=on` list) — it FAILS (exit 20) on ANY
  source left burning. OPT-IN like the manifest facet (omit the key → dormant, no UNKNOWN → every
  historic `--compare` call unchanged). `drift_check_burn_env` (tested in `tests/drift_guard.rs`).
  Also a read-only `scripts/drift-guard.sh --status host=… genlock_wall_clock=… genlock_capability=…
  burn_env=…` that prints genlock gate + build marker + burn state in ONE place (always exit 0;
  `--compare` is the gate; the rich live OBS dock is the separate #188). Toggling the rig into / out
  of test mode (the burns) is `scripts/rig-mode.sh test|event`, which drives `obs_burn_filter.py
  add|remove` over WS (the #128 wrapper itself is env-free now). The recording-e2e cleanup trap also
  clears+verifies burns off via `obs_burn_filter.py remove`+`check` on both boxes over obs-websocket
  (the harness has no SSH to Windows; with no burn env any more, the WS `genlock_burn` toggle +
  `rig-mode event` is the whole story).
- **#237 (DONE)** — `manifest_sha_for_component` bracket-escapes the dll-basename dot
  (`obs[.]dll`) so it is matched literally not as a regex wildcard; an obs.dll-only
  manifest labels a supplied distroav SHA `SKIPPED` (not `OK`) — an unchecked value must
  never read as verified (verdict stays NO DRIFT; SKIPPED ≠ DRIFT/UNKNOWN).

GOTCHA: the 150-min `windows-genlock.yml` is `workflow_dispatch`-only (can't run
per-PR), so manifest LOGIC is proven on the Linux `test` job; editing
`windows-genlock-fast.yml` itself triggers the fast Windows build (its `paths:` lists
the workflow file), which then runs the manifest gate on a real built obs.dll.

GOTCHA (workflow source-token gates): BOTH Windows workflows re-assert the genlock patch
tokens in pwsh BEFORE their build (the Linux Rust guards in `tests/genlock_preload.rs` are
probe-gated, can't compile on the runner) — keep them in LOCK-STEP. The slow
`windows-genlock.yml` and the FAST `windows-genlock-fast.yml` must each carry the #136 AND
#245 (`obs_source_set_genlock_latency_ms` / `PROP_GENLOCK_LATENCY_MS_SRC`) gates; the fast
gate was added in #249 (the slow one in #248). `tests/genlock_preload.rs` has a
`windows_genlock[_fast]_workflow_gates_on_the_per_source_latency` guard per workflow — add
the matching guard when you add a new token gate.

GOTCHA (Tier-0 local test verification): the probe-gated test files
(`#![cfg(feature="probe")]`, e.g. `genlock_preload.rs`) AND the whole `src/probe/genlock.rs`
Rust mirror are NOT seen by the default-feature gate (`cargo check/clippy/test --no-run`
compile them to nothing) — so the default gate green proves NOTHING about a genlock C/mirror
change. Grep-level verification of the vendored-source guard strings is the cheap default.
BUT: when a change is a bug-fix needing RED→GREEN proof (regression-test-first — grep alone
can't show a guard test actually FAILS then PASSES), OR to avoid burning the ~150-min
windows-genlock CI on a probe compile/lint/logic error, run the probe-gated genlock tests
locally ONCE via the documented `# airuleset:build-ok` bypass — TARGETED so it's cheap:
`cargo test --features probe --test genlock_preload <name>  # airuleset:build-ok`,
`cargo test --features probe --lib genlock  # airuleset:build-ok`,
`cargo clippy --features probe --all-targets -- -D warnings  # airuleset:build-ok`.
This pulls the probe deps (image/qrcode/rqrr/drm/lz4) → `target/` jumps to ~3.5–4 GB; the
pre-push hook (`scripts/purge-target.sh`) trims it. The C (`obs.dll`) still builds on the
windows-genlock CI only — local can't compile it; eyeball the C diff for correctness.
The NON-probe test files (`drift_guard.rs`, `harness_recording_e2e_paths.rs`) CAN run
fully Tier-0: `cargo test --no-run` to compile, then run the built
`target/debug/deps/<name>-*` binary DIRECTLY (no rebuild, no violation) to prove GREEN.

**Reading genlock state — from the OBS LOG, never an env (no genlock env exists post-#257):**
render tick + ts-align are build defaults, so there is nothing to read in env. The running genlock
state is the OBS log line `genlock: … render tick ENABLED|DISABLED` (latched at OBS launch) +
`genlock: latency = N ms` (once a genlock_fifo input is live). (Pre-#257 a win-* MCP `$env:` read was
additionally a STALE snapshot — the child inherits the long-lived MCP process's env — which is why the
log, not env, was always the source of truth.)

**AHK on strih:** `D:\_APPS\NL_STARTUP.ahk` auto-relaunches obs64 from
`C:\Program Files\obs-studio` (which is the genlock build). On reboot AHK relaunches it and genlock
comes up automatically — it is a build default (#257), no env needed.

Other OBS installs on strih (`D:\_APPS` — 1ME/2ME/vestibul/input/light) — NOT touched;
only the Program Files 2ME is the broadcast one.

## Monorepo Direction (User Directive — zapamätaj si)

1. **strih.lan is the master NTP clock** (DanteSync). Verify clock parity first before any genlock work.
2. Achieving proper OBS genlock is Claude's task (the team's earlier forks never reached a flawless result).
3. **Do NOT use or modify the existing forks** (`~/devel/obs-studio`, `~/devel/DistroAV`) — they are reference/history only (superseded 2026-06-12).
4. **Fresh vendored OBS + DistroAV + NDI SDK** go INSIDE the camera-box repo (ONE common repo). A new NDI SDK version is the basis.
5. Disable the OBS upgrade dialog in the build (prevents stock OBS auto-overwriting the custom version).
6. **Audio sync comes later** — only after zero-loss frames achieved.
7. A future slash command applies new upstream releases into the repo.

## Old Forks (Read-Only Reference)

`~/devel/obs-studio` (branch dev) — adds `get_scheduled_frame` / `async_scheduled` /
`async_wall_clock` to libobs; "Patch os_gettime_ns to apply PTP clock correction from DanteSync".
`~/devel/DistroAV` (branch dev) — runtime-loads `obs_source_set_async_scheduled` via GetProcAddress.
`~/devel/camera-box/distroav-fixed/…/distroav.so` — Linux ELF, NOT deployable to Windows boxes.

These are reference only — the correct direction (scheduled-frame / PTP path and its pitfalls)
is captured here. Do NOT copy or commit changes to them for new work.

## Drift Guard

`scripts/drift-guard.sh` + `/drift-guard` enforces the pinned zero-loss set:
OBS 32.1.2, DistroAV 6.2.1, NDI runtime 6.3.2.0, output 1080@30, genlock_wall_clock=1.
`--check-pins` in CI, `--compare` read-only live. Both boxes verified NO DRIFT (2026-06-14).

## strih NDI Input → Camera Mapping (INVERTED)

strih OBS NDI input labels are INVERTED vs the real cameras. Always resolve by the
input's `ndi_source_name`, NEVER by the OBS input label.

| OBS input label | actual NDI src | real camera |
|---|---|---|
| `NDI cam1` | `CAM3 (usb)` | CAM3 (10.77.9.63) |
| `NDI cam3` | `CAM4 (usb)` | CAM4 (10.77.9.64) |
| `NDI cam5` | `CAM1 (usb)` | CAM1 (10.77.9.61) |
| `NDI cam2` | (empty) | CAM2 unbound |

Scene names ("Cam 1"/"Cam 3"/"Cam 5") follow the input labels — same inversion.

To enable genlock on a camera's strih ingest: `SetInputSettings genlock_fifo=true`
on the input whose `ndi_source_name` matches that camera (`overlay=true` so other settings persist).

OBS only renders an NDI source when it's on an active scene — `GetSourceScreenshot`
fails (702) on an off-program source; that is not an error.

## OBS NDI-Output Timecode Lag (Root Cause)

OBS NDI-output `timecode` lags real emit ~150 ms. Root-caused 2026-06-15.

**Cause:** DistroAV Main Output (`vendor/distroav/src/ndi-output.cpp:372`) stamps
`NDIlib_send_timecode_synthesize` (sentinel INT64_MAX) and drops OBS's own
`frame->timestamp`. The NDI SDK's `synthesize` seeds a counter from system time ONCE at
stream start (T0) then emits `T0 + N×(1/fps)`. The lag = pipeline buffering frozen into
the seed. `clock_video=false` (:230-231) so SDK doesn't pace.

**Why option B (p_metadata) is impossible:** `struct obs_source_frame` has NO metadata
field → p_metadata dropped at ingest; the output re-creates a fresh NDI frame → NULL.
A per-frame emit-stamp in p_metadata is structurally dropped twice across one OBS hop.

**The fix (B′):** patch DistroAV fork to stamp the real DanteSync wall-clock boundary
instead of `synthesize` — mirror what camera-box already does in `src/ndi.rs:792-805`.
The genlock wall-clock infra exists in the OBS fork. ~10-line helper + change :372 and :423.
Tracked: #76.

The lag CANCELS OBS↔OBS (strih→stream measured correctly = 187 ms). It BREAKS cam→OBS
(cam→strih timecode gave nonsense 17.7 ms / negative). Fix B′ unlocks exact cam→strih
measurement.
