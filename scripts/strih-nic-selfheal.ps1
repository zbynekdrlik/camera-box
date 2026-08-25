<#
.SYNOPSIS
  #1199 -- ONE self-heal PASS of the strih on-box NIC-fail watcher. Designed to be run by a
  SYSTEM Scheduled Task every ~2 min (see scripts/install-strih-nic-selfheal.ps1). Each run is
  independent and crash-safe: the consecutive-failure counts live in a JSON STATE FILE that
  persists across passes, so a wedged/crashed pass simply loses one tick -- there is no
  long-running loop to hang (deliberately unlike scripts/avsync-watchdog.ps1; see issue 1199's
  design comment, "Prístup 2").

.DESCRIPTION
  WHY (issue 1199): on 2026-08-24 ~22:00 the strih NIC stopped passing packets while the box
  stayed alive; WoL did not wake it and only a morning physical power-cycle recovered it. The
  dev1-side reach watchdog (#1001) ALERTS but never self-heals, and WoL from S5 is unverified on
  strih (#1053). Until the card is physically replaced, the box must recover itself.

  OWNER RULING (2026-08-25): there is NO adapter disable/enable/restart rung of any kind. On strih
  that operation HANGS (the owner tried it by hand; a past session's attempt also failed), so the
  ONLY self-heal action is a graceful reboot. The ladder is a single step -- keep it small.

  THE LOAD-BEARING DESIGN DECISION: the trigger is REACHABILITY of multiple LAN targets, NOT
  `Get-NetAdapter` status. Yesterday the adapter almost certainly read `Up` while dropping every
  packet, so a status=="Down" trigger would have MISSED the exact incident. Adapter status here is
  READ for the log ONLY -- never a trigger, never touched.

  FAIL-SAFE (fail toward inaction): a pass is 'dead' ONLY when EVERY probed target returns a clean
  negative (no probe threw). Any reachable target is 'alive' and resets every counter; any probe
  error with nothing reachable, or nothing probed, is 'unknown' -- it never advances the ladder
  and never resets it.

  THE LADDER (constants below, ~2 min cadence; mirrors scripts/strih_nic_selfheal_decision.py --
  the pure Tier-0-tested source of truth -- byte-for-byte in the two constants):
    armed --5 dead (~10 min)--> graceful reboot (best-effort OBS StopStream/StopRecord over the
                                local WebSocket, then shutdown /r), reboots+1, stays armed
    reboot cap MaxReboots: once reached, phase=exhausted -- stop rebooting, keep loud logging
                          (the physical card replacement is the real fix).
    alive resets phase=armed, counts=0, reboots=0.

.PARAMETER Targets
  LAN targets to probe. Default: the rig gateway + dev1 + stream. dev1's LAN IP DRIFTS across the
  venue-switch fallback network (see machine-identities); that is fine here -- because ANY reachable
  target resets the counters, a stale dev1 IP just means that one target always votes dead, and as
  long as the gateway OR stream answers the pass is 'alive'. The gateway and stream are the stable
  anchors; dev1 is a third corroborating target. Override with -Targets to re-point after a move.

.PARAMETER WsPassword
  OBS WebSocket password for the BEST-EFFORT graceful StopStream/StopRecord before a reboot. strih's
  OBS-WS HAS a password, so this is needed for the graceful stop to work -- but it is NEVER a hard
  dependency: absent/empty/wrong password, or an unreachable/incompatible WS, is caught and the
  reboot proceeds regardless. Resolution order: -WsPassword, then $env:STRIH_OBS_WS_PASSWORD, then
  the out-of-band secret file C:\ProgramData\camera-box\obs-ws-password.txt (the SAME convention
  scripts/run-bundle-state-server.ps1 already uses on this box).

.PARAMETER StateFile
  JSON state carried across passes. Default C:\ProgramData\camera-box\nic-selfheal-state.json.

.PARAMETER LogFile
  Append-only action log. Default C:\ProgramData\camera-box\nic-selfheal.log.

.PARAMETER DryRun
  Classify + decide + LOG, but perform NO reboot (state is still advanced so a dry run can be
  walked through a simulated outage). For live-verify by the supervisor.

.NOTES
  Runs as SYSTEM so `shutdown /r` is permitted. No pwsh runtime on dev1 CI, so this file is
  validated STATICALLY (tests/python/test_strih_nic_selfheal_1199.py); the behavioural RED->GREEN
  tests live against the python mirror. Live install + a real NIC-fail exercise are the
  supervisor's step after integration (UNVERIFIED here).
#>
[CmdletBinding()]
param(
  [string[]]$Targets = @('10.77.9.1', '10.77.9.165', '10.77.9.204'),  # gateway, dev1 (drifts), stream
  [string]$WsPassword,
  [string]$StateFile = 'C:\ProgramData\camera-box\nic-selfheal-state.json',
  [string]$LogFile   = 'C:\ProgramData\camera-box\nic-selfheal.log',
  [int]$PingCount    = 2,
  [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$ProgressPreference     = 'SilentlyContinue'

# --- ladder constants: MIRROR of scripts/strih_nic_selfheal_decision.py (the static test asserts
# these two lines equal the python constants; keep them in lock-step). -----------------------------
$DeadPassesBeforeReboot = 5   # ~10 min of confirmed all-targets-dead before a graceful reboot
$MaxReboots             = 2   # hard cap on self-heal reboots before giving up (physical fix needed)

$TaskName = 'strih-nic-selfheal'

function Write-Log($msg) {
  $line = ('{0}  {1}' -f (Get-Date -Format 'yyyy-MM-ddTHH:mm:ss'), $msg)
  try {
    $dir = Split-Path -Parent $LogFile
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    Add-Content -LiteralPath $LogFile -Value $line -Encoding utf8
  } catch { }
  Write-Host $line
}

# ------------------------------------------------------------------------------------------------
# classify-pass -> 'alive' | 'dead' | 'unknown' (mirror of the python classify_pass; fail-safe:
# a pass is 'dead' ONLY when every probed target returned a CLEAN negative -- ANY throw with
# nothing reachable is 'unknown', never 'dead').
# ------------------------------------------------------------------------------------------------
function Get-PassClass([int]$reachable, [int]$clean, [int]$threw) {
  if ($reachable -ge 1) { return 'alive' }
  if ($threw -ge 1) { return 'unknown' }   # a broken probe can never PROVE a total outage
  if ($clean -ge 1) { return 'dead' }
  return 'unknown'                          # nothing probed -> cannot conclude
}

# Probe every target. A target that returns a definite reachable/unreachable answer counts toward
# `clean`; a target whose probe THREW counts toward `threw`.
function Invoke-Probe([string[]]$targets, [int]$count) {
  $reachable = 0; $clean = 0; $threw = 0
  foreach ($t in $targets) {
    try {
      # Windows PowerShell 5.1 semantics (the SYSTEM task launches powershell.exe): -Quiet returns
      # a plain [bool] and does NOT throw on an unreachable host (that is a clean 'dead' vote); it
      # throws only on a genuine error (no adapter at all, bad name), which is the 'unknown' path.
      $ok = Test-Connection -ComputerName $t -Count $count -Quiet -ErrorAction Stop
      $clean++
      if ($ok) { $reachable++ }
    } catch {
      $threw++
      Write-Log ("probe THREW for {0}: {1}" -f $t, $_.Exception.GetBaseException().Message)
    }
  }
  return [pscustomobject]@{ Reachable = $reachable; Clean = $clean; Threw = $threw }
}

# Read Get-NetAdapter status for LOGGING only (never a trigger, never touched). Never throws out.
function Get-NicSummary {
  try {
    $ads = Get-NetAdapter -Physical -ErrorAction Stop
    return ($ads | ForEach-Object { '{0}={1}' -f $_.Name, $_.Status }) -join ', '
  } catch {
    return ('nic-status-unreadable: {0}' -f $_.Exception.GetBaseException().Message)
  }
}

# ------------------------------------------------------------------------------------------------
# Get-SelfHealDecision -> @{ Action; Phase; ConsecutiveDead; Reboots; Reason } -- the PowerShell
# MIRROR of scripts/strih_nic_selfheal_decision.py's decide(). Same state machine, same constants.
# Any non-'exhausted' phase (incl. a corrupt value) is treated as 'armed', matching python's
# normalize-to-least-aggressive.
# ------------------------------------------------------------------------------------------------
function Get-SelfHealDecision($state, [string]$passClass) {
  $phase = $state.phase; $cd = [int]$state.consecutive_dead; $rb = [int]$state.reboots

  if ($passClass -eq 'unknown') {
    # fail-safe / fail toward inaction: never advance, never reset on an unprovable pass.
    return @{ Action = 'none'; Phase = $phase; ConsecutiveDead = $cd; Reboots = $rb;
             Reason = 'unknown pass (probe error or nothing probed) -> fail-safe inaction' }
  }
  if ($passClass -eq 'alive') {
    return @{ Action = 'none'; Phase = 'armed'; ConsecutiveDead = 0; Reboots = 0;
             Reason = 'alive (a target answered) -> reset' }
  }

  # dead
  $cd++
  if ($phase -eq 'exhausted') {
    # reboot cap already spent -- never reboot again; just keep counting + logging loudly.
    return @{ Action = 'none'; Phase = 'exhausted'; ConsecutiveDead = $cd; Reboots = $rb;
             Reason = ("dead {0} (exhausted, awaiting physical card replacement)" -f $cd) }
  }
  # phase 'armed' (also any normalized/corrupt phase)
  if ($cd -ge $DeadPassesBeforeReboot) {
    if ($rb -lt $MaxReboots) {
      return @{ Action = 'reboot'; Phase = 'armed'; ConsecutiveDead = 0; Reboots = ($rb + 1);
               Reason = ("{0} confirmed dead passes -> graceful reboot ({1}/{2})" -f $cd, ($rb + 1), $MaxReboots) }
    }
    return @{ Action = 'give_up'; Phase = 'exhausted'; ConsecutiveDead = 0; Reboots = $rb;
             Reason = ("reboot cap {0} reached -> stop rebooting, keep alerting (physical card fix needed)" -f $MaxReboots) }
  }
  return @{ Action = 'none'; Phase = 'armed'; ConsecutiveDead = $cd; Reboots = $rb;
           Reason = ("dead {0}/{1} (arming window)" -f $cd, $DeadPassesBeforeReboot) }
}

# --- state load / save --------------------------------------------------------------------------
function Read-State {
  $s = [pscustomobject]@{ phase = 'armed'; consecutive_dead = 0; reboots = 0 }
  try {
    if (Test-Path $StateFile) {
      $j = Get-Content -LiteralPath $StateFile -Raw | ConvertFrom-Json
      if ($j.phase -in @('armed', 'exhausted')) { $s.phase = $j.phase }
      if ($null -ne $j.consecutive_dead) { $s.consecutive_dead = [int]$j.consecutive_dead }
      if ($null -ne $j.reboots) { $s.reboots = [int]$j.reboots }
    }
  } catch { Write-Log ("state read error (starting fresh): {0}" -f $_.Exception.GetBaseException().Message) }
  return $s
}

# Returns $true iff the new state was durably written. #1199 review W2: a reboot must NOT fire when
# its incremented-reboots state could not be persisted, or after the reboot the box would re-read
# stale state and reboot again past the cap (an unbounded loop). Callers gate the reboot on this.
function Write-State($phase, [int]$cd, [int]$rb, [string]$lastPass, [string]$lastAction) {
  $obj = [ordered]@{
    version          = 1
    phase            = $phase
    consecutive_dead = $cd
    reboots          = $rb
    last_pass        = $lastPass
    last_action      = $lastAction
    updated_utc      = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  }
  try {
    $dir = Split-Path -Parent $StateFile
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    ($obj | ConvertTo-Json) | Set-Content -LiteralPath $StateFile -Encoding utf8
    return $true
  } catch {
    Write-Log ("state write error: {0}" -f $_.Exception.GetBaseException().Message)
    return $false
  }
}

# --- BEST-EFFORT OBS graceful stop over the local WebSocket (:4455) ------------------------------
# obs-websocket v5 handshake in pure .NET; wrapped so ANY failure (no password, WS down, protocol
# mismatch, timeout) is caught and the caller proceeds to reboot REGARDLESS. Never a hard dependency.
function Resolve-WsPassword {
  if ($WsPassword) { return $WsPassword }
  if ($env:STRIH_OBS_WS_PASSWORD) { return $env:STRIH_OBS_WS_PASSWORD }
  $pwFile = 'C:\ProgramData\camera-box\obs-ws-password.txt'
  try { if (Test-Path $pwFile) { return (Get-Content -LiteralPath $pwFile -Raw).Trim() } } catch { }
  return $null
}

function Invoke-ObsGracefulStop {
  # Returns nothing meaningful -- purely best-effort. The reboot NEVER depends on the outcome.
  $ws = $null; $cts = $null
  try {
    $pw = Resolve-WsPassword
    $ws = New-Object System.Net.WebSockets.ClientWebSocket
    $cts = New-Object System.Threading.CancellationTokenSource
    $cts.CancelAfter(5000)   # ONE 5s budget bounds the WHOLE handshake -- every await below is
                             # $cts.Token-bound, so a hung OBS THROWS (caught), never HANGS.
    $uri = [Uri]'ws://127.0.0.1:4455'
    $ws.ConnectAsync($uri, $cts.Token).Wait()

    $recv = {
      param($sock, $tok)
      $buf = New-Object byte[] 8192
      $seg = New-Object System.ArraySegment[byte] (,$buf)
      $r = $sock.ReceiveAsync($seg, $tok)
      $r.Wait()
      return [System.Text.Encoding]::UTF8.GetString($buf, 0, $r.Result.Count)
    }
    $send = {
      param($sock, $tok, $json)
      $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
      $seg = New-Object System.ArraySegment[byte] (,$bytes)
      $sock.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $tok).Wait()
    }

    $hello = (& $recv $ws $cts.Token) | ConvertFrom-Json
    $identify = @{ op = 1; d = @{ rpcVersion = 1 } }
    if ($hello.d.authentication) {
      $salt = $hello.d.authentication.salt
      $challenge = $hello.d.authentication.challenge
      $sha = [System.Security.Cryptography.SHA256]::Create()
      $secret = [Convert]::ToBase64String($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes(($pw + $salt))))
      $auth = [Convert]::ToBase64String($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes(($secret + $challenge))))
      $identify.d.authentication = $auth
    }
    & $send $ws $cts.Token (($identify | ConvertTo-Json -Depth 5))
    [void](& $recv $ws $cts.Token)  # Identified (op 2)

    foreach ($rt in @('StopStream', 'StopRecord')) {
      $req = @{ op = 6; d = @{ requestType = $rt; requestId = ([guid]::NewGuid().ToString()) } }
      & $send $ws $cts.Token (($req | ConvertTo-Json -Depth 5))
      Start-Sleep -Milliseconds 300
      Write-Log ("OBS graceful stop: sent {0}" -f $rt)
    }
    try { $ws.CloseAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, 'bye', $cts.Token).Wait() } catch { }
  } catch {
    Write-Log ("OBS graceful stop SKIPPED (best-effort; rebooting anyway): {0}" -f $_.Exception.GetBaseException().Message)
  } finally {
    if ($ws) { try { $ws.Dispose() } catch { } }
    if ($cts) { try { $cts.Dispose() } catch { } }
  }
}

function Invoke-GracefulReboot {
  Write-Log 'graceful reboot: best-effort OBS StopStream/StopRecord, then shutdown /r'
  Invoke-ObsGracefulStop
  # graceful REBOOT (/r), never a power-OFF; 30s grace so OBS/AHK can wind down; box returns via
  # auto-logon + the AHK startup chain (verified by yesterday's fresh boot).
  & shutdown /r /t 30 /c 'camera-box NIC self-heal reboot (issue 1199)'
}

# =================================================================================================
# ONE PASS (top-level try/catch: any UNEXPECTED error is treated as an 'unknown' pass -- fail-safe;
# the watcher never crashes the box or advances the ladder on an internal error).
# =================================================================================================
try {
  $state = Read-State
  $nic = Get-NicSummary
  $probe = Invoke-Probe $Targets $PingCount
  $passClass = Get-PassClass $probe.Reachable $probe.Clean $probe.Threw

  Write-Log ("pass={0} reachable={1}/{2} threw={3} phase={4} cd={5} reboots={6} nic=[{7}]" -f `
      $passClass, $probe.Reachable, $probe.Clean, $probe.Threw, $state.phase, `
      $state.consecutive_dead, $state.reboots, $nic)

  $d = Get-SelfHealDecision $state $passClass
  Write-Log ("decision: action={0} -> phase={1} cd={2} reboots={3} :: {4}" -f `
      $d.Action, $d.Phase, $d.ConsecutiveDead, $d.Reboots, $d.Reason)

  # Persist the decided state BEFORE acting, so a reboot cannot re-trigger within its own grace
  # window and a crash mid-action still leaves the ladder advanced.
  $persisted = Write-State $d.Phase $d.ConsecutiveDead $d.Reboots $passClass $d.Action

  if ($DryRun) {
    Write-Log ("DRY-RUN: would perform '{0}' (no action taken)" -f $d.Action)
  } else {
    switch ($d.Action) {
      'reboot' {
        # #1199 review W2: refuse to reboot if the incremented-reboots state was NOT persisted --
        # otherwise after the reboot we'd re-read stale state and reboot again past MaxReboots (an
        # unbounded loop). Fail toward NOT rebooting when the watcher cannot remember it did.
        if ($persisted) {
          Invoke-GracefulReboot
        } else {
          Write-Log 'REBOOT SUPPRESSED: could not persist reboot state (fail-safe against an unbounded reboot loop). Fix the state file, dev1 reach watchdog (#1001) keeps paging.'
        }
      }
      'give_up' { Write-Log 'GIVE-UP: NIC self-heal exhausted -- physical card replacement required; dev1 reach watchdog (#1001) continues to page.' }
      default   { }
    }
  }
} catch {
  # Any unexpected internal error is fail-safe UNKNOWN: log, take no action, leave state untouched.
  Write-Log ("PASS ERROR (fail-safe: no action, state untouched): {0}" -f $_.Exception.GetBaseException().Message)
}
exit 0
