---
description: Read OBS/DistroAV/NDI versions + critical settings off live strih/stream (read-only) and FAIL loudly on drift from the pinned zero-loss set (vendor/README.md).
argument-hint: "[strih | stream | both]"
allowed-tools: Bash, Read, mcp__win-strih__Shell, mcp__win-strih__FileRead, mcp__win-stream-snv__Shell, mcp__win-stream-snv__FileRead
---

# /drift-guard — verify the production OBS boxes still match the pinned zero-loss set

User directive (2026-06-12): strih (`10.77.9.202`) + stream (`10.77.9.204`) must be **kept** on the
exact versions + settings that guarantee permanent zero-loss functionality. This command reads the
live state **read-only** via the win-* MCP tools and compares it against the pinned set in
`vendor/README.md` using `scripts/drift-guard.sh --compare`, which **FAILS loudly on any drift**.

The deterministic engine is `scripts/drift-guard.sh` (unit-tested in `tests/drift_guard.rs`); CI runs
its `--check-pins` facet on every build. This command drives the **live** facet, which CI cannot do
(GitHub runners can't reach the production LAN).

**Read-only only.** This command NEVER writes settings, restarts OBS, or touches prod. Restoring a
drifted box (redeploy / settings change / OBS restart) is a separate, off-air, **user-approved** step.

## Steps (run for `strih`, `stream`, or both — default both)

1. **Gather observed values off each box via its win-* MCP `Shell` (read-only).** Newest OBS log +
   NDI runtime DLL + genlock master-gate env var:

   ```powershell
   $d="$env:APPDATA\obs-studio\logs"
   $f=Get-ChildItem $d -Filter *.txt | Sort-Object LastWriteTime -Desc | Select-Object -First 1
   $log=Get-Content $f.FullName -Raw
   if($log -match 'OBS (\d+\.\d+\.\d+)'){ "obs_version=$($Matches[1])" }
   if($log -match 'DistroAV \(Version (\d+\.\d+\.\d+)\)'){ "distroav_version=$($Matches[1])" }
   $dll="C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll"
   if(Test-Path $dll){ "ndi_runtime=$((Get-Item $dll).VersionInfo.FileVersion)" }
   # OUTPUT fps = the `fps:` line inside the "video settings reset:" block:
   $vs=($log -split "`n"); for($i=0;$i -lt $vs.Count;$i++){ if($vs[$i] -match 'video settings reset:'){ for($j=$i;$j -lt $vs.Count;$j++){ if($vs[$j] -match 'fps:\s+(\d+)/'){ "output_fps=$($Matches[1])"; break } }; break } }
   # genlock master gate = the RUNNING OBS state from the log (the gate is read at OBS launch, so
   # a later $env: read can be stale — esp. via a long-lived launcher/MCP process). ENABLED -> 1,
   # DISABLED -> 0. Cross-check the PERSISTENT Machine setting in HKLM (survives reboot); if the log
   # says 1 but HKLM != 1 (or vice versa), report it — a reboot would then launch the other state.
   if($log -match 'genlock:.*render tick ENABLED'){ "genlock_wall_clock=1" } elseif($log -match 'genlock:.*render tick DISABLED'){ "genlock_wall_clock=0" }
   $hklm=(Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name OBS_GENLOCK_WALL_CLOCK -EA SilentlyContinue).OBS_GENLOCK_WALL_CLOCK
   "# persistent HKLM OBS_GENLOCK_WALL_CLOCK=[$hklm] (must agree with the log gate)"
   ```

   (`ndi_runtime` from the DLL `FileVersion` is the robust source; the OBS log
   `NDI Library Version detected:` line is an equivalent fallback. If OBS is **not running**, the
   newest log is stale — note that, do not treat a stale read as live truth. Do **not** read the
   genlock gate from `$env:OBS_GENLOCK_WALL_CLOCK` via the MCP shell: that shell inherits an env
   snapshot from a long-lived parent and showed empty on 2026-06-14 while HKLM + the OBS log both
   correctly read `1`.)

1b. **Gather the per-input NDI ingest latency off the running OBS (#84).** The `latency` mode is a
   per-input DistroAV setting, read from the live obs-websocket (`ws://strih.lan:4455` /
   `ws://stream.lan:4455`, pw `JhRfqdTmuifYq60y`), NOT the OBS log/registry. For each genlocked
   **broadcast-path** input — on strih the camera ingests (`NDI cam5`=CAM1, `NDI cam1`=CAM3,
   `NDI cam3`=CAM4), on stream the strih→stream program feed (`NDI 2ME PGM`) — read
   `GetInputSettings`→`latency` (`0`=Normal is the pin — the certified low-latency zero-loss mode, #84) and build a comma-separated
   `input name=latency` list. (Non-broadcast inputs — preview/CG/lyrics — are out of scope of the
   pin; do not include them.) The reusable reader is `~/.cache/obsprobe/obs_inputs.py <host> <pw>`
   (read-only; lists every NDI input + its settings). If OBS is not running there is no live
   obs-websocket — omit the key so the engine reports it UNKNOWN rather than a stale guess.

1c. **Gather every `distroav.dll` location across the OBS scan paths (#124, single canonical plugin
   path).** OBS scans MULTIPLE module locations; the SAME `distroav.dll` in more than one of them lets
   a stale copy silently shadow the intended build (the mixed-version incident #119). Read-only enumerate
   every `distroav.dll` under the three scan paths and build a comma-separated list:

   ```powershell
   $paths = @(
     'C:\Program Files\obs-studio\obs-plugins\64bit',
     'C:\ProgramData\obs-studio\plugins',
     "$env:APPDATA\obs-studio\plugins"
   )
   $found = foreach($p in $paths){ if(Test-Path $p){ Get-ChildItem $p -Recurse -Filter distroav.dll -EA SilentlyContinue | Select-Object -ExpandProperty FullName } }
   "distroav_dll_paths=$($found -join ',')"
   ```

   The invariant: exactly ONE `distroav.dll`, at the canonical `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit`.
   (The `C:\Program Files\obs-studio\data\obs-plugins\distroav` folder is resources/locale — it carries
   NO `.dll`, so it never matches this scan. D:\_APPS\*, C:\genlock-*, C:\obs-backup\* are SEPARATE
   portable installs / staging / backups, NOT in the Program Files genlock OBS's scan paths — do not
   include them.) If the scan returns nothing, OBS may not be installed at the expected path — omit the
   key so the engine reports UNKNOWN rather than a false clean.

2. **Compare against the pinned set** — feed every observed value to the engine:

   ```bash
   ./scripts/drift-guard.sh --compare host=strih \
     obs_version=<v> distroav_version=<v> ndi_runtime=<v> output_fps=<n> genlock_wall_clock=<0|1> \
     ndi_input_latency="NDI cam5=<n>,NDI cam1=<n>,NDI cam3=<n>" \
     distroav_dll_paths="<every distroav.dll location, comma-separated>"   # stream: ndi_input_latency="NDI 2ME PGM=<n>"
   ```

   - Exit `0` → **NO DRIFT**, the box matches the pinned zero-loss set. Report it.
   - Exit `20` → **DRIFT**: the output names each setting that differs (expected vs observed).
     Report loudly. Do NOT fix it silently — restoring prod is off-air + user-approved.
   - Exit `11` → at least one value was **UNKNOWN** (not read). Drift status is incomplete, not
     clean — re-read the missing value (e.g. OBS not running, DLL path moved) before trusting it.

3. **Report** per box: the observed set, the verdict (NO DRIFT / DRIFT `<settings>` / INCOMPLETE),
   and — on drift — exactly what to restore. Never claim a clean box you did not fully read.

## Notes

- The OBS **auto-update dialog disabled** (#43) is a *build* property, not runtime-readable off a
  running box, so it is guarded against the vendored source by `tests/obs_updater_disabled.rs`, not
  here. A box running stock OBS 32.1.2 instead of our genlock build is otherwise indistinguishable
  by version alone — the protection against that is deploying only our build (off-air, approved).
- Re-pin (edit the table in `vendor/README.md`) only as part of a *deliberate* rollout — e.g. the
  30→60 fps step (#11) or activating genlock — never to silence a drift you did not intend.
