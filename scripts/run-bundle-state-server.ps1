# #650 — auto-restart supervisor loop for bundle-state-server.py on strih/stream.
#
# Deployed alongside bundle-state-server.py + bundle_state_gather.py under
# C:\ProgramData\camera-box\ and launched at boot by a Scheduled Task (ONSTART trigger, see the
# #650 issue / .claude/skills/genlock playbook note for the exact TaskCreate invocation). A plain
# Scheduled Task has no built-in "restart on crash" for a bare `python.exe` command, so THIS script
# is the task's actual action: it re-launches bundle-state-server.py in a loop, so a transient
# Python crash (or OBS not being up yet at boot) self-heals a few seconds later instead of leaving
# :8899 dead until someone notices. Chose a scheduled-task+restart-loop over a native Windows
# service wrapper (NSSM etc.) per the #650 issue's own guidance ("a scheduled task with auto-start
# is acceptable if a full service wrapper is overkill") — this mirrors the box's existing
# `StartOBS` task (InteractiveToken, ONSTART-equivalent) rather than introducing a new dependency.
#
# The OBS WebSocket password is READ FROM A LOCAL FILE, never committed to this repo
# (security-basics.md): deploy `C:\ProgramData\camera-box\obs-ws-password.txt` out-of-band (e.g.
# via the win-* MCP FileWrite, one line, no trailing newline needed) with the SAME password
# documented in the (local, not git) `rig-obs-ws-credentials` memory. If the file is absent the
# server still starts with an empty password — the ndi_input_latency facet (and the record-dir
# resolution) then simply fails to authenticate and the bundle-state payload omits that key
# (UNKNOWN downstream, never a silent guess); everything else it can read off the log/filesystem
# is unaffected.

$ErrorActionPreference = "Continue"

$InstallDir = "C:\ProgramData\camera-box"
$Script = Join-Path $InstallDir "bundle-state-server.py"
$PasswordFile = Join-Path $InstallDir "obs-ws-password.txt"
$LogFile = Join-Path $InstallDir "bundle-state-server.log"

if (Test-Path $PasswordFile) {
    $env:OBS_PASSWORD = (Get-Content -LiteralPath $PasswordFile -Raw).Trim()
}

while ($true) {
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Add-Content -LiteralPath $LogFile -Value "$ts [supervisor] starting bundle-state-server.py"
    python $Script *>> $LogFile
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Add-Content -LiteralPath $LogFile -Value "$ts [supervisor] bundle-state-server.py exited (code $LASTEXITCODE) — restarting in 5s"
    Start-Sleep -Seconds 5
}
