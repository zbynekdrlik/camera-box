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
   "genlock_wall_clock=$([int]([bool]$env:OBS_GENLOCK_WALL_CLOCK))"
   ```

   (`ndi_runtime` from the DLL `FileVersion` is the robust source; the OBS log
   `NDI Library Version detected:` line is an equivalent fallback. If OBS is **not running**, the
   newest log is stale — note that, do not treat a stale read as live truth.)

2. **Compare against the pinned set** — feed every observed value to the engine:

   ```bash
   ./scripts/drift-guard.sh --compare host=strih \
     obs_version=<v> distroav_version=<v> ndi_runtime=<v> output_fps=<n> genlock_wall_clock=<0|1>
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
