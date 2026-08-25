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

  THE LOAD-BEARING DESIGN DECISION: the trigger is REACHABILITY of multiple LAN targets, NOT
  `Get-NetAdapter` status. Yesterday the adapter almost certainly read `Up` while dropping every
  packet, so a status=="Down" trigger would have MISSED the exact incident. Adapter status here is
  advisory/log only and is used only to choose which adapter to Restart-NetAdapter.

  FAIL-SAFE (fail toward inaction): a pass is 'dead' ONLY when every probed target returns a clean
  negative. Any probe error, or nothing probed at all, is 'unknown' -- it never advances the ladder
  and never resets it. Any single reachable target is 'alive' and resets every counter.

  THE LADDER (constants below, ~2 min cadence; mirrors scripts/strih_nic_selfheal_decision.py --
  the pure Tier-0-tested source of truth -- byte-for-byte in these three constants):
    normal    --5 dead (~10 min)--> Restart-NetAdapter, phase=restarted
    restarted --5 dead (~10 min)--> graceful reboot (best-effort OBS StopStream/StopRecord over the
                                    local WebSocket, then shutdown /r), phase=rebooted, reboots+1
    rebooted  --5 dead----------->  re-arm with a cheap adapter restart, phase=restarted
    reboot cap MaxReboots: once reached, phase=exhausted -- stop rebooting, keep cheap adapter
                          restarts + loud logging (the physical card replacement is the real fix).
    alive resets phase=normal, counts=0, reboots=0.

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
  Classify + decide + LOG, but perform NO Restart-NetAdapter / reboot (state is still advanced so a
  dry run can be walked through a simulated outage). For live-verify by the supervisor.

.NOTES
  Runs as SYSTEM so Restart-NetAdapter and shutdown /r are permitted. No pwsh runtime is assumed on
  dev1 CI, so this file is validated STATICALLY (tests/python/test_strih_nic_selfheal_1199.py);
  the behavioural RED->GREEN tests live against the python mirror. Live install + a real NIC-fail
  exercise are the supervisor's step after integration (UNVERIFIED here).
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
# these three lines equal the python constants; keep them in lock-step). ---------------------------
$DeadPassesBeforeRestart = 5   # ~10 min of confirmed all-targets-dead before Restart-NetAdapter
$DeadPassesBeforeReboot  = 5   # ~10 min more, still dead after the restart, before a graceful reboot
$MaxReboots              = 2   # hard cap on self-heal reboots before giving up (physical fix needed)

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
# anything that is not a CLEAN all-dead result is 'unknown', never 'dead').
# ------------------------------------------------------------------------------------------------
function Get-PassClass([int]$reachable, [int]$clean, [bool]$probeError) {
  if ($probeError) { return 'unknown' }
  if ($clean -le 0) { return 'unknown' }   # nothing probed cleanly -> cannot conclude
  if ($reachable -ge 1) { return 'alive' }
  return 'dead'
}

# Probe every target. A target that returns a definite reachable/unreachable answer counts toward
# `clean`; a target whose probe THREW counts toward neither. If EVERY probe threw -> probeError.
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
      Write-Log ("probe THREW for {0}: {1}" -f $t, $_.Exception.Message)
    }
  }
  $probeError = ($threw -gt 0 -and $clean -eq 0)
  return [pscustomobject]@{ Reachable = $reachable; Clean = $clean; Threw = $threw; ProbeError = $probeError }
}

# Read Get-NetAdapter status for LOGGING/adapter-selection only (never a trigger). Never throws out.
function Get-NicSummary {
  try {
    $ads = Get-NetAdapter -Physical -ErrorAction Stop
    return ($ads | ForEach-Object { '{0}={1}' -f $_.Name, $_.Status }) -join ', '
  } catch {
    return ('nic-status-unreadable: {0}' -f $_.Exception.Message)
  }
}

# ------------------------------------------------------------------------------------------------
# Get-SelfHealDecision -> @{ Action; Phase; ConsecutiveDead; Reboots; Reason } -- the PowerShell
# MIRROR of scripts/strih_nic_selfheal_decision.py's decide(). Same state machine, same constants.
# ------------------------------------------------------------------------------------------------
function Get-SelfHealDecision($state, [string]$passClass) {
  $phase = $state.phase; $cd = [int]$state.consecutive_dead; $rb = [int]$state.reboots

  if ($passClass -eq 'unknown') {
    # fail-safe / fail toward inaction: never advance, never reset on an unprovable pass.
    return @{ Action = 'none'; Phase = $phase; ConsecutiveDead = $cd; Reboots = $rb;
             Reason = 'unknown pass (probe error or nothing probed) -> fail-safe inaction' }
  }
  if ($passClass -eq 'alive') {
    return @{ Action = 'none'; Phase = 'normal'; ConsecutiveDead = 0; Reboots = 0;
             Reason = 'alive (a target answered) -> reset' }
  }

  # dead
  $cd++
  switch ($phase) {
    'normal' {
      if ($cd -ge $DeadPassesBeforeRestart) {
        return @{ Action = 'restart_adapter'; Phase = 'restarted'; ConsecutiveDead = 0; Reboots = $rb;
                 Reason = ("{0} confirmed dead passes -> Restart-NetAdapter" -f $cd) }
      }
      return @{ Action = 'none'; Phase = 'normal'; ConsecutiveDead = $cd; Reboots = $rb;
               Reason = ("dead {0}/{1} (normal window)" -f $cd, $DeadPassesBeforeRestart) }
    }
    'restarted' {
      if ($cd -ge $DeadPassesBeforeReboot) {
        if ($rb -lt $MaxReboots) {
          return @{ Action = 'reboot'; Phase = 'rebooted'; ConsecutiveDead = 0; Reboots = ($rb + 1);
                   Reason = ("still dead after Restart-NetAdapter -> graceful reboot ({0}/{1})" -f ($rb + 1), $MaxReboots) }
        }
        return @{ Action = 'give_up'; Phase = 'exhausted'; ConsecutiveDead = 0; Reboots = $rb;
                 Reason = ("reboot cap {0} reached -> stop rebooting, keep alerting (physical card fix needed)" -f $MaxReboots) }
      }
      return @{ Action = 'none'; Phase = 'restarted'; ConsecutiveDead = $cd; Reboots = $rb;
               Reason = ("dead {0}/{1} (confirming after restart)" -f $cd, $DeadPassesBeforeReboot) }
    }
    'rebooted' {
      if ($cd -ge $DeadPassesBeforeRestart) {
        return @{ Action = 'restart_adapter'; Phase = 'restarted'; ConsecutiveDead = 0; Reboots = $rb;
                 Reason = 'still dead after reboot -> re-arm with a cheap adapter restart' }
      }
      return @{ Action = 'none'; Phase = 'rebooted'; ConsecutiveDead = $cd; Reboots = $rb;
               Reason = ("dead {0}/{1} (post-reboot window)" -f $cd, $DeadPassesBeforeRestart) }
    }
    default {
      # exhausted: cheap adapter restarts only, never reboot again.
      if ($cd -ge $DeadPassesBeforeRestart) {
        return @{ Action = 'restart_adapter'; Phase = 'exhausted'; ConsecutiveDead = 0; Reboots = $rb;
                 Reason = 'exhausted -> cheap adapter restart only, never reboot again' }
      }
      return @{ Action = 'none'; Phase = 'exhausted'; ConsecutiveDead = $cd; Reboots = $rb;
               Reason = ("dead {0}/{1} (exhausted, awaiting physical card replacement)" -f $cd, $DeadPassesBeforeRestart) }
    }
  }
}

# --- state load / save --------------------------------------------------------------------------
function Read-State {
  $s = [pscustomobject]@{ phase = 'normal'; consecutive_dead = 0; reboots = 0 }
  try {
    if (Test-Path $StateFile) {
      $j = Get-Content -LiteralPath $StateFile -Raw | ConvertFrom-Json
      if ($j.phase -in @('normal', 'restarted', 'rebooted', 'exhausted')) { $s.phase = $j.phase }
      if ($null -ne $j.consecutive_dead) { $s.consecutive_dead = [int]$j.consecutive_dead }
      if ($null -ne $j.reboots) { $s.reboots = [int]$j.reboots }
    }
  } catch { Write-Log ("state read error (starting fresh): {0}" -f $_.Exception.Message) }
  return $s
}

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
  } catch { Write-Log ("state write error: {0}" -f $_.Exception.Message) }
}

# --- action: restart the physical up/disconnected adapters --------------------------------------
function Invoke-AdapterRestart {
  try {
    $ads = Get-NetAdapter -Physical -ErrorAction Stop | Where-Object { $_.Status -in @('Up', 'Disconnected') }
    if (-not $ads) { Write-Log 'Restart-NetAdapter: no physical Up/Disconnected adapter found'; return }
    foreach ($a in $ads) {
      try {
        Write-Log ("Restart-NetAdapter -> {0} [{1}]" -f $a.Name, $a.InterfaceDescription)
        Restart-NetAdapter -Name $a.Name -Confirm:$false -ErrorAction Stop
      } catch { Write-Log ("Restart-NetAdapter FAILED for {0}: {1}" -f $a.Name, $_.Exception.Message) }
    }
  } catch { Write-Log ("Restart-NetAdapter enumeration failed: {0}" -f $_.Exception.Message) }
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
  try {
    $pw = Resolve-WsPassword
    $ws = New-Object System.Net.WebSockets.ClientWebSocket
    $cts = New-Object System.Threading.CancellationTokenSource
    $cts.CancelAfter(5000)
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
    Write-Log ("OBS graceful stop SKIPPED (best-effort; rebooting anyway): {0}" -f $_.Exception.Message)
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
  $passClass = Get-PassClass $probe.Reachable $probe.Clean $probe.ProbeError

  Write-Log ("pass={0} reachable={1}/{2} threw={3} phase={4} cd={5} reboots={6} nic=[{7}]" -f `
      $passClass, $probe.Reachable, $probe.Clean, $probe.Threw, $state.phase, `
      $state.consecutive_dead, $state.reboots, $nic)

  $d = Get-SelfHealDecision $state $passClass
  Write-Log ("decision: action={0} -> phase={1} cd={2} reboots={3} :: {4}" -f `
      $d.Action, $d.Phase, $d.ConsecutiveDead, $d.Reboots, $d.Reason)

  # Persist the decided state BEFORE acting, so a reboot cannot re-trigger within its own grace
  # window and a crash mid-action still leaves the ladder advanced.
  Write-State $d.Phase $d.ConsecutiveDead $d.Reboots $passClass $d.Action

  if ($DryRun) {
    Write-Log ("DRY-RUN: would perform '{0}' (no action taken)" -f $d.Action)
  } else {
    switch ($d.Action) {
      'restart_adapter' { Invoke-AdapterRestart }
      'reboot'          { Invoke-GracefulReboot }
      'give_up'         { Write-Log 'GIVE-UP: NIC self-heal exhausted -- physical card replacement required; dev1 reach watchdog (#1001) continues to page.' }
      default           { }
    }
  }
} catch {
  # Any unexpected internal error is fail-safe UNKNOWN: log, take no action, leave state untouched.
  Write-Log ("PASS ERROR (fail-safe: no action, state untouched): {0}" -f $_.Exception.Message)
}
exit 0
