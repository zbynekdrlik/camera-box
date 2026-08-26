<#
.SYNOPSIS
  #1199 -- idempotent installer for the strih on-box NIC-fail self-heal watcher. Copies
  strih-nic-selfheal.ps1 into C:\ProgramData\camera-box\ and registers a SYSTEM Scheduled Task that
  runs ONE self-heal pass every 2 minutes (plus once at startup). The supervisor runs this over the
  win-strih MCP Shell after the branch is integrated (writing a script + registering a scheduled
  task is exactly what the win-* MCP is for -- same convention as scripts/run-bundle-state-server.ps1
  / obs-self-heal-install.sh).

.DESCRIPTION
  Idempotent: re-running overwrites the deployed script and re-registers the task in place
  (Register-ScheduledTask -Force). SYSTEM principal + HighestAvailable so the watcher's
  Restart-NetAdapter and `shutdown /r` are permitted. The task uses powershell.exe (Windows
  PowerShell 5.1) to match the watcher's Test-Connection -ComputerName/-Quiet semantics.

  -Uninstall unregisters the task and removes the deployed script (the state/log files are left in
  place for forensics; pass -Purge to delete them too).

.PARAMETER SourceScript
  Path to strih-nic-selfheal.ps1 to deploy. Default: the copy sitting next to THIS installer
  (so the supervisor can upload both files to one temp dir and just run the installer).

.PARAMETER InstallDir
  Where the watcher lives on the box. Default C:\ProgramData\camera-box.

.PARAMETER IntervalMinutes
  Scheduled-task repetition cadence. Default 2 (issue 1199's "~2 min").

.PARAMETER Uninstall
  Remove the scheduled task and the deployed script.

.PARAMETER Purge
  With -Uninstall, ALSO delete nic-selfheal-state.json and nic-selfheal.log.

.NOTES
  No pwsh on dev1 CI, so this file is validated STATICALLY (tests/python/test_strih_nic_selfheal_1199.py);
  the live install + a real NIC-fail exercise are the supervisor's step (UNVERIFIED here).
#>
[CmdletBinding()]
param(
  [string]$SourceScript = (Join-Path (Split-Path -Parent $PSCommandPath) 'strih-nic-selfheal.ps1'),
  [string]$InstallDir   = 'C:\ProgramData\camera-box',
  [int]$IntervalMinutes = 2,
  [switch]$Uninstall,
  [switch]$Purge
)

$ErrorActionPreference = 'Stop'
$ProgressPreference     = 'SilentlyContinue'

$TaskName    = 'strih-nic-selfheal'
$DeployedPs1 = Join-Path $InstallDir 'strih-nic-selfheal.ps1'
$StateFile   = Join-Path $InstallDir 'nic-selfheal-state.json'
$LogFile     = Join-Path $InstallDir 'nic-selfheal.log'

function Remove-SelfHealTask {
  $existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($existing) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "removed scheduled task: $TaskName"
  } else {
    Write-Host "scheduled task not present: $TaskName"
  }
}

if ($Uninstall) {
  Remove-SelfHealTask
  if (Test-Path $DeployedPs1) { Remove-Item -LiteralPath $DeployedPs1 -Force; Write-Host "removed $DeployedPs1" }
  if ($Purge) {
    foreach ($f in @($StateFile, $LogFile)) {
      if (Test-Path $f) { Remove-Item -LiteralPath $f -Force; Write-Host "purged $f" }
    }
  }
  Write-Host 'DONE: strih-nic-selfheal uninstalled.'
  exit 0
}

# --- install ------------------------------------------------------------------------------------
if (-not (Test-Path $SourceScript)) { Write-Error "install-strih-nic-selfheal: source script not found: $SourceScript"; exit 1 }
if (-not (Test-Path $InstallDir)) { New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null }

Copy-Item -LiteralPath $SourceScript -Destination $DeployedPs1 -Force
Write-Host "deployed watcher -> $DeployedPs1"

# Register the SYSTEM task: one pass every $IntervalMinutes minutes, indefinitely, plus at startup.
$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
  -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "{0}"' -f $DeployedPs1)

$triggerRepeat = New-ScheduledTaskTrigger -Once -At (Get-Date) `
  -RepetitionInterval (New-TimeSpan -Minutes $IntervalMinutes) `
  -RepetitionDuration (New-TimeSpan -Days 3650)
$triggerBoot = New-ScheduledTaskTrigger -AtStartup

$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest

$settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew `
  -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
  -ExecutionTimeLimit (New-TimeSpan -Minutes 5)

Register-ScheduledTask -TaskName $TaskName -Action $action `
  -Trigger @($triggerRepeat, $triggerBoot) -Principal $principal -Settings $settings `
  -Description '#1199 camera-box strih NIC-fail self-heal watcher (Restart-NetAdapter -> graceful reboot). One pass every 2 min as SYSTEM.' `
  -Force | Out-Null

Write-Host "registered SYSTEM scheduled task '$TaskName' (every $IntervalMinutes min + at startup)"
Write-Host 'DONE: strih-nic-selfheal installed.'
Write-Host "  verify:   Get-ScheduledTask -TaskName $TaskName | Format-List"
Write-Host "  run once: Start-ScheduledTask -TaskName $TaskName ; Get-Content '$LogFile' -Tail 20"
Write-Host "  dry run:  powershell.exe -NoProfile -ExecutionPolicy Bypass -File '$DeployedPs1' -DryRun"
exit 0
