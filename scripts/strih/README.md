# strih box — versioned Windows startup artifacts

## `NL_STARTUP.ahk` — the strih OBS/Resolume/tally auto-respawn watcher (#774)

`NL_STARTUP.ahk` is the AutoHotkey v2 script that runs on the **strih** box (10.77.9.202) and
auto-(re)launches OBS, Resolume Arena, tally and the other operator apps whenever their window
disappears. It was previously a **live-only script that nobody versioned** — the direct cause of
the 2026-07-15 event incident where OBS died at 18:49:35 and stayed dead ~20 min while the AHK
process itself was still running (#774). This file is now the **source of truth**.

### Where it lives + how it autostarts on strih

- On the box: `D:\_APPS\NL_STARTUP.ahk`.
- Autostarts via a shortcut in the user's Startup folder
  (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\*NL_STARTUP*`), which resolves the
  `.ahk` through the HKCU AutoHotkey file association (AutoHotkey v2 is installed **user-scoped**
  under `%LOCALAPPDATA%\Programs\AutoHotkey\v2\AutoHotkey64.exe`, NOT on PATH — see
  `scripts/lib/ahk-watchdog.sh`, which both `launch-obs-genlock.sh` and `obs-self-heal-install.sh`
  use to relaunch + verify it robustly).

### How the respawn works (and how it can stop)

- The engine is the `While(SafeLoop) { if (appN_run) and not WinExist(appN_name) appN() ... }`
  loop. The OBS slot (`app1`) launches via the box's **Start-Menu shortcut**
  `C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk` — NOT a bare `obs64.exe` —
  so strih's per-box parameters (`--enable-media-stream --verbose`, needed by the interkom
  VDO.ninja Browser source, else "Permissions denied" on program output) are always honored (#775).
- The OBS window match is **process-based** (`ahk_exe obs64.exe`), never a title match, so an OBS
  title change (e.g. "newlevel.media build unknown") can never stop the respawn.
- **Failure mode:** respawn only runs while `SafeLoop = 1`. `SafeLoop` latches to 0 with no
  auto-reset via (a) the startup `MsgBox("Chces aby sa vsetko zaplo?" ... "No")` branch, (b) the
  `Alt+Q` hotkey (a deliberate operator "stop respawns" control), or (c) a double-start leaving the
  run in a bad state. This is the likeliest root of the 20-min-dead incident.

### The one intentional deviation from the live capture: `#SingleInstance Force`

This committed copy adds `#SingleInstance Force` (the only change vs. the captured live script). It
makes a second launch cleanly **replace** the first — killing the "chvíľu 2 procesy" double-start
footgun the ticket named — and, because a fresh launch re-runs the startup block, it **re-arms
`SafeLoop` to 1**, self-healing a latched-off respawn guard. Operator-facing SafeLoop semantics
(Alt+Q, the startup MsgBox) are otherwise **unchanged**.

### CAPTURE-FIDELITY GUARD — read before the FIRST deploy

This file was reconstructed from a read of the live `D:\_APPS\NL_STARTUP.ahk` (win-strih MCP
`FileRead`, 2026-08-16). AutoHotkey is not whitespace-sensitive, so tabs/blank-line counts may
differ harmlessly — but **before the first deploy the supervisor MUST diff this against the live
file** (read it via `win-strih` `FileRead`) and confirm there is **no semantic drift** (paths,
`appN_name`/`appN_run` values, the respawn loop, the hotkeys). Deploy only after that confirmation;
if the live script has diverged since capture, reconcile first.

### Deploy + verify (SUPERVISOR, at integration — win-* MCP, never ssh for the GUI/AHK)

This is a Windows GUI-adjacent artifact, so per `.claude/rules/win-ssh-vs-mcp.md` all of this runs
through the `win-strih` MCP (session 1), never ssh:

1. Diff-verify capture fidelity (above).
2. Back up the live file: `win-strih` `FileRead D:\_APPS\NL_STARTUP.ahk` → keep the bytes.
3. `win-strih` `FileWrite D:\_APPS\NL_STARTUP.ahk` with this committed copy.
4. Restart the watcher (its old process still runs the OLD script): stop it
   (`Stop-Process -Name AutoHotkey64 -Force`) then relaunch via the resolved user-scoped exe /
   Startup shortcut — the exact robust resolve+verify PowerShell is
   `ahk_resolve_and_relaunch_ps` in `scripts/lib/ahk-watchdog.sh` (probes
   `%LOCALAPPDATA%\Programs\AutoHotkey\v2\AutoHotkey64.exe` first, polls `Get-Process AutoHotkey64`
   to confirm it came back). Confirm it **loaded without a syntax error** (no AHK error dialog;
   the tray tip "Safe loop ZAPNUTY." appears).
5. **Functional check (the #774 acceptance test):** kill `obs64` (`Stop-Process -Name obs64
   -Force`), then within ~10 s confirm exactly one `obs64` is back AND that it launched with
   strih's params (`--enable-media-stream` in the cmdline) — i.e. the AHK respawned it via the
   `.lnk`, not a bare exe.

### Independent, AHK-agnostic backstop (recommended — #411)

The primary respawn is this AHK. The **AHK-independent** backstop is `scripts/obs-self-heal-install.sh`
(#411) — a Windows scheduled task that force-kills/relaunches a wedged `obs64` through
`launch-obs-genlock.sh`'s `.lnk` program (StopAhk-first / RestartAhk-last). It ships **disabled**
(`Enabled=false`); enabling it after a supervisor live-verify closes the "OBS dead AND respawn
guard off" hole completely. Tracked by #411 — not re-filed here. Detection/alerting is already
covered by #391 (obs-liveness-watchdog) and #979 (obs-session-watchdog).
