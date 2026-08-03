# scripts/avsync-keepalive.ps1 -- #812/#807 Task Scheduler keep-alive check for BOTH long-running
# avsync monitors on the stream box: watchdog.ps1 (#812, the A/V-sync measurement loop, deployed
# FROM scripts/avsync-watchdog.ps1) and avsync-vlc-monitor.ps1 (#807, the VLC program-audio
# babysitter). Invoked every ~5 min by a Repetition-triggered scheduled task (see
# scripts/avsync-watchdog-install.sh) -- idempotent: a healthy pass is a silent no-op; if either
# loop is not running (crash, hang, orphaned/killed process, or a boot the loop's own BootTrigger
# never recovered from), this relaunches it.
#
# WHY a keep-alive CHECK rather than a bigger self-heal state machine: unlike the OBS self-heal
# gate (multi-step recovery, GPU-cause branching, an AHK-race ordering hazard --
# scripts/obs-self-heal-install.sh), each of these loops has exactly one recovery action (restart
# the same process), so a Rust decide()-style state machine would be disproportionate. See issue
# 812's design comment for the full rejected-alternative reasoning.
$ErrorActionPreference = 'Stop'
$log = 'C:\avsync\avsync-keepalive.log'

function Log($m) {
  "$(Get-Date -Format o) [avsync-keepalive] $m" | Add-Content -Path $log
}

# Ensure-Running SCRIPT_NAME SCRIPT_PATH -> checks whether a powershell.exe process whose
# CommandLine names SCRIPT_NAME is running (matching on CommandLine, not just process name, so
# this check is never confused by ANY other powershell.exe on the box -- including itself);
# relaunches SCRIPT_PATH (hidden, detached) if not found.
function Ensure-Running([string]$scriptName, [string]$scriptPath) {
  $running = Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine -like "*$scriptName*" }
  if ($running) {
    Log "$scriptName already running (pid $($running[0].ProcessId)) - no-op"
    return
  }
  if (-not (Test-Path $scriptPath)) {
    Log "FATAL: $scriptPath not found - cannot (re)launch $scriptName, refusing to guess"
    return
  }
  Log "$scriptName NOT running - launching $scriptPath"
  Start-Process -FilePath 'powershell.exe' `
    -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden', '-File', $scriptPath) `
    -WindowStyle Hidden
}

Ensure-Running 'watchdog.ps1' 'C:\avsync\watchdog.ps1'
Ensure-Running 'avsync-vlc-monitor.ps1' 'C:\avsync\avsync-vlc-monitor.ps1'
