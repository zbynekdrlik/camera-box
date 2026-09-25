#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) —
# matches the sibling scripts/lib/*.sh convention (camera-box-restart-verify.sh, v4l2-neutral.sh)
# of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the
# CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
#
# scripts/lib/ahk-watchdog.sh — SINGLE SOURCE OF TRUTH for robustly relaunching + VERIFYING the
# strih-only NL_STARTUP.ahk AutoHotkey64 auto-respawn watcher (#411/#786's stop-first/restart-last
# AHK-race fix). Sourced by scripts/obs-self-heal-install.sh and scripts/launch-obs-genlock.sh.
# Ownership of the stop/restart bracket (issue 1273): launch-obs-genlock.sh's build_launch_program
# is the SINGLE OWNER — it stops AutoHotkey64 before touching obs64 and restarts it afterward.
# obs-self-heal-install.sh REUSES that program verbatim (so the embedded child owns the bracket)
# and no longer pre-stops AHK itself; it only calls this relaunch primitive from a FAILURE-PATH
# backstop (when the embedded child exited non-zero AND AutoHotkey64 is down). Either way, this is
# the ONE place that does the relaunch + confirms it actually worked.
#
# WHY (#867): both callers used to just fire-and-forget the restart and unconditionally claim
# success:
#   - obs-self-heal-install.sh's ahk_start_block ran
#     `Start-Process -FilePath 'AutoHotkey64.exe' -ArgumentList '"D:\_APPS\NL_STARTUP.ahk"'` — a
#     BARE exe name. AutoHotkey v2 is installed user-scoped under
#     `%LOCALAPPDATA%\Programs\AutoHotkey\v2\AutoHotkey64.exe` and is NOT on PATH (confirmed live
#     on strih, `where AutoHotkey64.exe` -> nothing), so Start-Process could never resolve it — yet
#     the very next line unconditionally logged "AutoHotkey64 relaunched ... restored".
#   - launch-obs-genlock.sh's ahk_restart_ps launched the NL_STARTUP Startup shortcut (which DOES
#     resolve, via the HKCU .ahk file association) but never checked the PROCESS actually came
#     back — it only warned when the shortcut FILE itself was missing.
# Root cause (comment #5121884098 on #867, correcting the issue body's wrong "AHK was
# uninstalled" premise): a THIRD script (the obs.dll swap on strih) had already been
# `Stop-Process -Name AutoHotkey64 -Force`-ing AHK before touching obs64 and relaunching it after
# — with the SAME blind-success shape — and its relaunch step silently failed. Nobody noticed
# until the user found strih's OBS with no live respawn watcher for hours.
#
# ahk_resolve_and_relaunch_ps -> PowerShell text (a self-contained statement block, not a function
# — it sets plain script-scope variables so the CALLER's surrounding script can read them) that:
#   1. Probes, in order: %LOCALAPPDATA%\Programs\AutoHotkey\v2\AutoHotkey64.exe,
#      %ProgramFiles%\AutoHotkey\v2\AutoHotkey64.exe, %ProgramFiles%\AutoHotkey\AutoHotkey64.exe,
#      then the NL_STARTUP Startup shortcut, then `Get-Command AutoHotkey64.exe` (PATH) — uses the
#      FIRST that exists/resolves. NEVER a bare `-FilePath 'AutoHotkey64.exe'` relying on PATH.
#   2. Launches the resolved exe with the NL_STARTUP.ahk script as its argument (or, for the
#      shortcut fallback, launches the shortcut itself — it already targets the .ahk file).
#   3. Polls up to ~10s for `Get-Process AutoHotkey64` and sets `$ahkRelaunchVerified`
#      ($true/$false) and `$ahkRelaunchTarget` (the resolved path/shortcut used, or $null if
#      nothing resolved at all) — for the CALLER's own log line.
# Pure string builder — no network/MCP/Windows access, safe to source from unit tests. The CALLER
# decides what to do with $ahkRelaunchVerified: obs-self-heal-install.sh's recovery pass just logs
# it (a scheduled task retries every ~2 min regardless); launch-obs-genlock.sh's one-shot relaunch
# treats it as fatal (Write-Error + a distinct non-zero exit), matching how that script already
# fails loud on its other post-launch verifications (obs64 not started, #786 audio buffering).
# Per-box AHK identity (issue 1295):
#   $1 AHK_SCRIPT  -- REQUIRED (issue 1317 part 4): the box's NL_STARTUP.ahk path to pass to the
#                     resolved exe. The retired Windows strih's D:\_APPS default was REMOVED; with no
#                     path the function writes a named error to stderr and returns 2 (nothing on
#                     stdout). The CG box (RESOLUME-SNV) passes its own v2 path
#                     'C:\Users\Resolume\Documents\_NLMEDIA resolume\_APPS\NL_STARTUP.ahk' -- it has
#                     a SPACE, which is why the ArgumentList wraps $ahkScriptPath in double quotes.
#   $2 PREFER      -- 'exe' (default) resolves the exe FIRST then the Startup .lnk; 'lnk'
#                     (resolume) tries the Startup shortcut FIRST. resolume prefers the .lnk because
#                     it is a TRAVELING box whose install path can move -- the Startup shortcut
#                     always resolves the running watcher, so it is the more durable relaunch target
#                     (the exe candidates still back it up). Every caller (launch-obs-genlock.sh,
#                     obs-self-heal-install.sh, deploy-genlock-fleet.sh) passes the box's identity
#                     from scripts/lib/genlock-fleet-boxes.sh.
ahk_resolve_and_relaunch_ps() {
  # issue 1317 part 4: NO default script -- the retired Windows strih's D:\_APPS path exists on no
  # managed box, so a call without the box's own .ahk (genlock-fleet-boxes.sh fleet_box_ahk_script)
  # fails closed instead of emitting a relaunch that can never resolve.
  local ahk_script="${1:-}"
  local prefer="${2:-exe}"
  if [ -z "$ahk_script" ]; then
    echo "ahk_resolve_and_relaunch_ps: no NL_STARTUP.ahk path given -- pass the box's own (scripts/lib/genlock-fleet-boxes.sh fleet_box_ahk_script); the retired Windows strih's default was removed (issue 1317)" >&2
    return 2
  fi
  local ahk_script_ps="${ahk_script//\'/\'\'}"  # double any ' for the PS single-quoted literal
  printf "%s\n" "\$ahkScriptPath = '${ahk_script_ps}'"
  cat <<'PS'
$ahkCandidates = @(
  (Join-Path $env:LOCALAPPDATA 'Programs\AutoHotkey\v2\AutoHotkey64.exe'),
  (Join-Path $env:ProgramFiles 'AutoHotkey\v2\AutoHotkey64.exe'),
  (Join-Path $env:ProgramFiles 'AutoHotkey\AutoHotkey64.exe')
)
$ahkExe = $ahkCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
$ahkLnk = Get-ChildItem "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup" -Filter "*NL_STARTUP*" -ErrorAction SilentlyContinue | Select-Object -First 1
PS
  if [ "$prefer" = "lnk" ]; then
    cat <<'PS'
if ($ahkLnk) {
  Start-Process -FilePath $ahkLnk.FullName
  $ahkRelaunchTarget = $ahkLnk.FullName
} elseif ($ahkExe) {
  Start-Process -FilePath $ahkExe -ArgumentList "`"$ahkScriptPath`""
  $ahkRelaunchTarget = $ahkExe
} else {
PS
  else
    cat <<'PS'
if ($ahkExe) {
  Start-Process -FilePath $ahkExe -ArgumentList "`"$ahkScriptPath`""
  $ahkRelaunchTarget = $ahkExe
} elseif ($ahkLnk) {
  Start-Process -FilePath $ahkLnk.FullName
  $ahkRelaunchTarget = $ahkLnk.FullName
} else {
PS
  fi
  cat <<'PS'
  $ahkCmd = Get-Command AutoHotkey64.exe -ErrorAction SilentlyContinue
  if ($ahkCmd) {
    Start-Process -FilePath $ahkCmd.Source -ArgumentList "`"$ahkScriptPath`""
    $ahkRelaunchTarget = $ahkCmd.Source
  } else {
    $ahkRelaunchTarget = $null
  }
}
$ahkRelaunchVerified = $false
if ($ahkRelaunchTarget) {
  for ($ahkVerifyAttempt = 0; $ahkVerifyAttempt -lt 10; $ahkVerifyAttempt++) {
    Start-Sleep -Seconds 1
    if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) { $ahkRelaunchVerified = $true; break }
  }
}
PS
}

# issue 1372 -- the AHK GUARD fragments (owner ruling "ale ahk nespustaj"). On a box whose OWNER runs
# an NL_STARTUP.ahk watcher (resolume), our deploy/launch programs no longer own that watcher: they
# stop it ONLY if it is running (so it cannot respawn a second obs64 mid-copy or mid-relaunch), they
# NEVER start or restart it, and the AutoHotkey64 count is REPORTED, never a gate. Whether AHK runs
# is the owner's choice, not a postcondition. Both fragments are pure PowerShell text shared by
# deploy-genlock-fleet.sh (build_windows_deploy_program) and launch-obs-genlock.sh
# (build_launch_program), so the two programs cannot drift apart. Neither fragment ever launches
# AutoHotkey64 -- that is the relaunch primitive above, which a guard box never uses.
#
# ahk_guard_stop_ps -> stop a RUNNING AutoHotkey64 and log what happened; never start it.
ahk_guard_stop_ps() {
  cat <<'PS'
# AutoHotkey64 GUARD (issue 1372, owner ruling): stop the owner's watcher ONLY if it is running, so it
# cannot respawn a second obs64 here. This program NEVER starts or restarts it.
if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) {
  Stop-Process -Name AutoHotkey64 -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 500
  if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) {
    Write-Error "#1372 FAIL: AutoHotkey64 is still running after Stop-Process -- it would respawn a second obs64; stop it by hand, then re-run."
    exit 11
  }
  Write-Host "#1372: stopped the running AutoHotkey64 watcher -- it stays OFF (never restarted by this program)."
} else {
  Write-Host "#1372: AutoHotkey64 is not running -- nothing to stop (it stays OFF)."
}
PS
}

# ahk_guard_report_ps -> log the AutoHotkey64 count + SessionIds. Report-only: never an exit, never a
# start. It replaces the #978 "exactly 1 AutoHotkey64" gate on a guard box.
ahk_guard_report_ps() {
  cat <<'PS'
$ahkReport = @(Get-Process AutoHotkey64 -ErrorAction SilentlyContinue)
Write-Host ("#1372 AHK REPORT (report-only, never a gate): AutoHotkey64 count=" + $ahkReport.Count + " SessionId=[" + (@($ahkReport | ForEach-Object { $_.SessionId }) -join ',') + "] -- whether it runs is the owner's choice.")
PS
}
