---
name: genlock
description: >
  Genlock OBS build — current deployment state on strih+stream, monorepo direction,
  fork history. Load when working on genlock (#8/#11), vendored OBS/DistroAV,
  drift-guard, or anything touching the broadcast OBS on strih/stream.
---

# Genlock

## Sub-frame ms reserve (#184, validated 2026-06-24 — prod is on reserve=3ms)

`OBS_GENLOCK_RESERVE_MS=N` (>0) switches the genlock ts-align RELEASE deadline from the
whole-frame `preload·interval` (=33ms@30fps) to `wall_now − N·1e6` (ms-granular). reserve_ms=0
= the #136 frame path verbatim (back-compat). Validated zero-loss at reserve=3ms on BOTH hops
(strict recording-verdict overall_pass, FIFO audits show `reserve_ms=3`, 0 new underruns during
active feed). **Prod (strih + stream) is LEFT ON reserve=3ms.** obs.dll sha `24e22357…` (build
19472506e). Rollback DLL: `C:\obs-backup\pre-184\obs.dll` (cdce8c3a… = the old whole-frame build).

**STALE-ENV TRAP launching OBS via win-* MCP (cost the prior #184 worker its stream deploy):**
the win-* MCP Shell child inherits the long-lived MCP process's env SNAPSHOT — if a genlock var
(e.g. `OBS_GENLOCK_RESERVE_MS`) was set AFTER the MCP started, the MCP shell reads it EMPTY, and
an OBS launched from that shell inherits it UNSET (→ silently runs the whole-frame path, no
reserve line in the log). FIX: in the SAME Shell call that launches OBS, set the genlock env
EXPLICITLY from the Machine values first:
```powershell
$env:OBS_GENLOCK_RESERVE_MS = [System.Environment]::GetEnvironmentVariable('OBS_GENLOCK_RESERVE_MS','Machine')
# (also WALL_CLOCK / PRELOAD_FRAMES / TS_ALIGN), THEN Start-Process obs64 -WorkingDirectory bin\64bit
```
Verify it took: the OBS log must show `genlock: sub-frame jitter reserve = N ms` (prints lazily
when a genlock_fifo input first activates) AND the FIFO audit line must carry `reserve_ms=N`.
NB the audit's `underruns=` is CUMULATIVE per OBS process — a huge value can be IDLE accumulation
between runs; what matters is the DELTA during active feed (0 = clean).

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
| Env var | HKLM OBS_GENLOCK_WALL_CLOCK=1 | HKLM OBS_GENLOCK_WALL_CLOCK=1 |

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
- **#121** (post-deploy byte/SHA verify vs the manifest — needs the rig) still OPEN —
  it ships the manifest TO the rig + byte-checks the deploy; #122 (above) is the runtime
  drift-guard facet that consumes it. Both consume `BUNDLE_MANIFEST.json`.
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

GOTCHA: the 150-min `windows-genlock.yml` is `workflow_dispatch`-only (can't run
per-PR), so manifest LOGIC is proven on the Linux `test` job; editing
`windows-genlock-fast.yml` itself triggers the fast Windows build (its `paths:` lists
the workflow file), which then runs the manifest gate on a real built obs.dll.

**win-* MCP env reads are STALE:** The MCP Shell spawns a child that inherits the
long-lived MCP process's env snapshot — `$env:OBS_GENLOCK_WALL_CLOCK` reads EMPTY while
the var is really set (HKLM). For persistent value read:
`HKLM SYSTEM\CurrentControlSet\Control\Session Manager\Environment`
For running OBS genlock state: read the OBS log line
`genlock: wall-clock-slaved render tick ENABLED|DISABLED` (latched at OBS launch).

**AHK on strih:** `D:\_APPS\NL_STARTUP.ahk` auto-relaunches obs64 from
`C:\Program Files\obs-studio` (which is the genlock build). On reboot AHK inherits
the Machine env var → launches genlock automatically.

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
