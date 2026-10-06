#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure printer functions, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`.
#
# scripts/lib/obs-clean-close-win.sh -- issue 1367: the Windows genlock deploy closes OBS CLEANLY so
# the runtime A/V settings persist. The ONE shared PowerShell fragment the deploy program
# (build_windows_deploy_program in scripts/deploy-genlock-fleet.sh) carries, byte-identical for
# every Windows OBS box (stream, resolume).
#
# WHY: the E2E gate's A/V correction (the #856 apply) and the latency pins are written over
# obs-websocket and live in OBS RUNTIME until OBS next saves its scene collection. The deploy used to
# stop OBS with `Stop-Process -Force`, which skips the save on exit (OBSBasic::closeWindow ->
# saveAll), so the relaunch reloaded the older saved values and production A/V moved by a deploy
# (6.10.2026: runtime `mbc` sync 37 ms vs saved 29 ms; the 30.9 runtime pin 1020 came back as a
# saved 1040). Design: issue 1367 comment 6014590298; evidence: comment 6013239473.
#
# The deploy program runs in the box's win-* MCP Shell, which is the interactive session (session 1),
# so a WM_CLOSE reaches OBS's window (6.10.2026: a MANUAL WM_CLOSE of stream OBS in the interactive
# session exited in 6.6 s; this fragment's own close is first run live by the next deploy). The
# fragment is printed as three ORDERED blocks from this one lib, plus one optional hook:
#
#   obs_clean_close_preflight_ps   -- step (0a), BEFORE anything on the box changes: the obs-websocket
#                                     helpers (defined once, used by (2) too) and the stream/record
#                                     read. A live broadcast stream is REFUSED (exit 12). The read
#                                     sits here, not only at (2), because steps (1)/(1b) stop
#                                     AutoHotkey64 and disable the keep-alive tasks before (2): a
#                                     refusal there would leave them changed under a live stream.
#   obs_clean_close_stop_ps        -- step (2): re-read right before the close (a broadcast can start
#                                     after (0a), the issue-1271 rule), StopRecord + confirm when a
#                                     recording runs, CloseMainWindow on the ONE live obs64 in this
#                                     session when its main window is OBS's own (title `OBS ...`),
#                                     a 45 s bound, `clean close OK in N ms`; only when it cannot
#                                     close or times out, the old Stop-Process -Force, named
#                                     (`clean close timed out -- forcing`).
#   obs_saved_settings_readback_ps -- step (2b), REPORT-ONLY: from the active scene collection JSON,
#                                     every NDI input's saved genlock_latency_ms_src and every audio
#                                     input's saved sync -- what the relaunch will load.
#   obs_clean_close_refusal_restore_ps -- the deploy program emits it after its keep-alive disable
#                                     (step 1b, stream): a step-(2) refusal calls it, so a live
#                                     broadcast keeps the self-heal tasks the deploy had disabled.
#
# obs-websocket: ws://127.0.0.1:4455 on the box, rpc v1, no event subscriptions. Both boxes run it
# with auth_required false (read live 6.10.2026); when a Hello carries a challenge anyway, the
# password comes from the box's own plugin_config\obs-websocket\config.json, never from this program.
# An unreadable :4455 while obs64 runs is a named WARNING and the deploy goes on -- the rig-busy
# guard's rule (scripts/lib/stray-session-check.sh): refuse what it can READ as live. A stream it
# cannot read is NOT protected: OBS then asks to confirm the exit, nobody answers, and the 45 s bound
# forces it (the same as before this change).
#
# Exit codes added to the deploy program: 12 = a broadcast stream is live (refused).
# The PowerShell avoids Write-Error: under the program's $ErrorActionPreference = 'Stop' it throws and
# the following `exit` never runs. Windows PowerShell 5.1 syntax only.
#
# Not covered, stated: scripts/lib/mv-reverify-escalate.sh restarts strih OBS headless over ssh
# (session 0 cannot post WM_CLOSE to a session-1 window, issue 958) and this OBS build has no
# ExitOBS request, so that escalation stays a force-kill (.claude/rules/genlock-fleet-deploy.md).
# Tests: tests/python/test_deploy_clean_close_win_1367.py.

# The close bound: the 6.10.2026 clean close took 6.6 s; 45 s leaves room for a slow save.
OBS_CLEAN_CLOSE_TIMEOUT_S=45

# obs_clean_close_preflight_ps -> step (0a): the shared obs-websocket helpers + the broadcast refusal.
obs_clean_close_preflight_ps() {
  cat <<'PSCCPRE'
# (0a) issue 1367 -- OBS is closed CLEANLY in step (2), so it saves its scene collection on exit
#      and the runtime A/V settings (the gate's sync correction, the latency pins) survive the
#      deploy. These are its obs-websocket helpers. A broadcast stream is refused HERE, before this
#      program changes anything on the box; a running recording is stopped in step (2).
$ccObsWsUri = 'ws://127.0.0.1:4455'
# Every awaited void Task is assigned to $null: its GetResult() returns a VoidTaskResult object, which
# would otherwise land in the function's output and turn a returned socket into an array.
function Send-CcObsWsMessage($ws, $obj) {
  $bytes = [System.Text.Encoding]::UTF8.GetBytes(($obj | ConvertTo-Json -Depth 6 -Compress))
  $cts = [System.Threading.CancellationTokenSource]::new(5000)
  $null = $ws.SendAsync([System.ArraySegment[byte]]::new($bytes), [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $cts.Token).GetAwaiter().GetResult()
}
function Receive-CcObsWsMessage($ws) {
  $buf = New-Object byte[] 65536
  $ms = New-Object System.IO.MemoryStream
  do {
    $cts = [System.Threading.CancellationTokenSource]::new(5000)
    $r = $ws.ReceiveAsync([System.ArraySegment[byte]]::new($buf), $cts.Token).GetAwaiter().GetResult()
    if ($r.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
      throw "obs-websocket closed the connection ($($ws.CloseStatus) $($ws.CloseStatusDescription))"
    }
    $ms.Write($buf, 0, $r.Count)
  } while (-not $r.EndOfMessage)
  return ([System.Text.Encoding]::UTF8.GetString($ms.ToArray()) | ConvertFrom-Json)
}
function Close-CcObsWs($ws) {
  try {
    $cts = [System.Threading.CancellationTokenSource]::new(2000)
    $null = $ws.CloseAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, 'done', $cts.Token).GetAwaiter().GetResult()
  } catch {
    Write-Host "issue 1367: obs-websocket close handshake did not finish ($($_.Exception.Message)) -- the socket is dropped"
  }
  $ws.Dispose()
}
function Open-CcObsWs {
  $ws = [System.Net.WebSockets.ClientWebSocket]::new()
  try {
    $cts = [System.Threading.CancellationTokenSource]::new(5000)
    $null = $ws.ConnectAsync([Uri]$ccObsWsUri, $cts.Token).GetAwaiter().GetResult()
    $hello = Receive-CcObsWsMessage $ws
    if ($hello.op -ne 0) { throw "expected the obs-websocket Hello, got op $($hello.op)" }
    $d = @{ rpcVersion = 1; eventSubscriptions = 0 }
    if ($hello.d.authentication) {
      # Only when the server asks: the box's own obs-websocket password, never one in this program.
      $cfg = Join-Path $env:APPDATA 'obs-studio\plugin_config\obs-websocket\config.json'
      $pw = (Get-Content -LiteralPath $cfg -Raw | ConvertFrom-Json).server_password
      if (-not $pw) { throw "obs-websocket asks for a password and $cfg holds none" }
      $sha = [System.Security.Cryptography.SHA256]::Create()
      $secret = [Convert]::ToBase64String($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($pw + $hello.d.authentication.salt)))
      $d.authentication = [Convert]::ToBase64String($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($secret + $hello.d.authentication.challenge)))
    }
    Send-CcObsWsMessage $ws @{ op = 1; d = $d }
    $ident = Receive-CcObsWsMessage $ws
    if ($ident.op -ne 2) { throw "obs-websocket Identify was not accepted (op $($ident.op))" }
    return $ws
  } catch {
    $ws.Dispose()
    throw
  }
}
function Invoke-CcObsWsRequest($ws, [string]$type) {
  $id = [guid]::NewGuid().ToString()
  Send-CcObsWsMessage $ws @{ op = 6; d = @{ requestType = $type; requestId = $id } }
  for ($i = 0; $i -lt 20; $i++) {
    $m = Receive-CcObsWsMessage $ws
    if ($m.op -eq 7 -and $m.d.requestId -eq $id) {
      if (-not $m.d.requestStatus.result) {
        throw "obs-websocket $type failed (code $($m.d.requestStatus.code) $($m.d.requestStatus.comment))"
      }
      return $m.d.responseData
    }
  }
  throw "obs-websocket $type got no response"
}
function Get-CcLiveObs {
  # LIVE obs64 only: a stale handle of an exited obs64 (HasExited / 0 threads, the 12.9.2026
  # RESOLUME-SNV case) must neither count as a second OBS nor keep the close waiting.
  @(Get-Process obs64 -ErrorAction SilentlyContinue | Where-Object { -not $_.HasExited -and $_.Threads.Count -gt 0 })
}
function Get-CcObsOutputState {
  $ws = Open-CcObsWs
  try {
    $st = Invoke-CcObsWsRequest $ws 'GetStreamStatus'
    $rc = Invoke-CcObsWsRequest $ws 'GetRecordStatus'
    return @{ streaming = [bool]$st.outputActive; recording = [bool]$rc.outputActive }
  } finally {
    Close-CcObsWs $ws
  }
}
if (@(Get-CcLiveObs).Count -eq 0) {
  Write-Host "issue 1367 clean close: obs64 is not running -- no broadcast to protect"
} else {
  $ccState = $null
  $ccStateErr = ''
  try { $ccState = Get-CcObsOutputState } catch { $ccStateErr = $_.Exception.Message }
  if ($null -eq $ccState) {
    Write-Warning "issue 1367 clean close: obs-websocket $ccObsWsUri unreadable ($ccStateErr) -- the stream state is UNKNOWN; continuing (the rig-busy guard refuses only what it can read as live)"
  } elseif ($ccState.streaming) {
    Write-Host "issue 1367 clean close REFUSED: obs64 is STREAMING -- a broadcast may be live, and this deploy never stops or replaces OBS under a live stream. Nothing on this box was changed. Stop the stream, then run the program again."
    exit 12
  } else {
    Write-Host "issue 1367 clean close preflight: streaming=False recording=$($ccState.recording)"
  }
}
PSCCPRE
}

# obs_clean_close_stop_ps -> step (2): the clean close, with the old force-kill as its named fallback.
obs_clean_close_stop_ps() {
  cat <<PSCCSTOPHEAD
# (2) issue 1367 -- close OBS CLEANLY so it saves its scene collection on exit, then stop
#     obs-browser-page. Force only when that cannot happen: a force-kill skips the save, and the
#     relaunch would reload older A/V settings. Steps 1/1b already stopped the box's respawners
#     (its keep-alive tasks, its watcher), so nothing respawns obs64 behind the close.
\$ccCloseTimeoutMs = $((OBS_CLEAN_CLOSE_TIMEOUT_S * 1000))
PSCCSTOPHEAD
  cat <<'PSCCSTOP'
$ccForce = $false
$ccAll = @(Get-CcLiveObs)
if ($ccAll.Count -eq 0) {
  Write-Host "issue 1367 clean close: obs64 is not running -- nothing to close"
} else {
  # Read again right before the close: a broadcast can start after the step-(0a) read.
  $ccState = $null
  $ccStateErr = ''
  try { $ccState = Get-CcObsOutputState } catch { $ccStateErr = $_.Exception.Message }
  if ($null -eq $ccState) {
    Write-Warning "issue 1367 clean close: obs-websocket $ccObsWsUri unreadable ($ccStateErr) -- closing without the stream/record read"
  } elseif ($ccState.streaming) {
    Write-Host "issue 1367 clean close REFUSED: obs64 STARTED STREAMING after the step-(0a) read -- this deploy never stops OBS under a live stream. OBS was not stopped and no file was copied; steps 0b/1/1b already ran and their lines above name what they changed."
    if (Get-Command Invoke-CcRefusalRestore -ErrorAction SilentlyContinue) { Invoke-CcRefusalRestore }
    exit 12
  } elseif ($ccState.recording) {
    # The deploy restarts OBS anyway: a stopped recording is a finished file, a killed one is not.
    Write-Host "issue 1367 clean close: obs64 is RECORDING -- StopRecord before the close"
    $ccRecSw = [System.Diagnostics.Stopwatch]::StartNew()
    $ccRecStopped = $false
    try {
      $ccWs = Open-CcObsWs
      try {
        $null = Invoke-CcObsWsRequest $ccWs 'StopRecord'
        while (-not $ccRecStopped -and $ccRecSw.ElapsedMilliseconds -lt 30000) {
          if (-not (Invoke-CcObsWsRequest $ccWs 'GetRecordStatus').outputActive) { $ccRecStopped = $true } else { Start-Sleep -Milliseconds 500 }
        }
      } finally {
        Close-CcObsWs $ccWs
      }
    } catch {
      Write-Warning "issue 1367 clean close: StopRecord failed ($($_.Exception.Message))"
    }
    if ($ccRecStopped) {
      Write-Host "issue 1367 clean close: recording stopped in $($ccRecSw.ElapsedMilliseconds) ms"
    } else {
      Write-Warning "issue 1367 clean close: the recording is NOT confirmed stopped -- closing anyway (OBS may ask to confirm the exit; the bound below then forces it)"
    }
  }
  $ccSession = (Get-Process -Id $PID).SessionId
  $ccHere = @($ccAll | Where-Object { $_.SessionId -eq $ccSession })
  if ($ccAll.Count -ne 1 -or $ccHere.Count -ne 1) {
    Write-Host "issue 1367 clean close not possible: $($ccAll.Count) obs64 running, $($ccHere.Count) in this session $ccSession (expected exactly one, here) -- forcing"
    $ccForce = $true
  } else {
    $ccP = $ccHere[0]
    $ccTitle = [string]$ccP.MainWindowTitle
    if (-not $ccTitle) {
      Write-Host "issue 1367 clean close not possible: obs64 pid $($ccP.Id) has no main window -- forcing"
      $ccForce = $true
    } elseif ($ccTitle -notlike 'OBS *') {
      # WM_CLOSE goes to the front unowned window: a projector there would close (and drop out of
      # the saved projector list) instead of OBS.
      Write-Host "issue 1367 clean close not possible: obs64 pid $($ccP.Id) shows '$ccTitle' in front, not the OBS main window -- forcing"
      $ccForce = $true
    } else {
      $ccSw = [System.Diagnostics.Stopwatch]::StartNew()
      $ccPosted = $false
      try { $ccPosted = $ccP.CloseMainWindow() } catch { Write-Host "issue 1367 clean close: CloseMainWindow failed ($($_.Exception.Message))" }
      if (-not $ccPosted) {
        Write-Host "issue 1367 clean close not possible: the OBS main window did not take the close (disabled behind a modal dialog) -- forcing"
        $ccForce = $true
      } else {
        while ($ccSw.ElapsedMilliseconds -lt $ccCloseTimeoutMs -and @(Get-CcLiveObs | Where-Object { $_.Id -eq $ccP.Id }).Count -gt 0) {
          Start-Sleep -Milliseconds 250
        }
        if (@(Get-CcLiveObs | Where-Object { $_.Id -eq $ccP.Id }).Count -gt 0) {
          Write-Host "issue 1367 clean close timed out -- forcing (obs64 pid $($ccP.Id) '$ccTitle' still running after $($ccCloseTimeoutMs / 1000) s)"
          $ccForce = $true
        } else {
          Write-Host "issue 1367 clean close OK in $($ccSw.ElapsedMilliseconds) ms (obs64 pid $($ccP.Id) '$ccTitle' exited and saved its scene collection)"
        }
      }
    }
  }
}
if ($ccForce) {
  Get-Process obs64,obs-browser-page -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
} else {
  # A browser page left behind by a clean exit would keep files of the swap locked.
  $ccPages = @(Get-Process obs-browser-page -ErrorAction SilentlyContinue)
  if ($ccPages.Count -gt 0) {
    Write-Host "issue 1367 clean close: stopping $($ccPages.Count) leftover obs-browser-page process(es)"
    $ccPages | Stop-Process -Force -ErrorAction SilentlyContinue
  }
}
PSCCSTOP
}

# obs_saved_settings_readback_ps -> step (2b): report-only read-back of what the relaunch will load.
obs_saved_settings_readback_ps() {
  cat <<'PSCCREAD'
# (2b) issue 1367 -- REPORT-ONLY: what the relaunch will load, from the active scene collection
#      (user.ini SceneCollectionFile, else global.ini). NDI input = ndi_source -> its saved
#      genlock_latency_ms_src; audio input = any saved source with mixers != 0 (libobs saves 0 for a
#      source without audio) -> its saved sync. Never fails the deploy.
try {
  $ccObsCfg = Join-Path $env:APPDATA 'obs-studio'
  $ccName = $null
  foreach ($ccIni in @((Join-Path $ccObsCfg 'user.ini'), (Join-Path $ccObsCfg 'global.ini'))) {
    if (-not $ccName -and (Test-Path -LiteralPath $ccIni)) {
      $ccLine = Select-String -LiteralPath $ccIni -Pattern '^SceneCollectionFile=(.+)$' | Select-Object -First 1
      if ($ccLine) { $ccName = $ccLine.Matches[0].Groups[1].Value.Trim() }
    }
  }
  if (-not $ccName) { throw "no SceneCollectionFile= in user.ini or global.ini under $ccObsCfg" }
  if ($ccName -notmatch '\.json$') { $ccName = "$ccName.json" }
  $ccCollection = Join-Path (Join-Path $ccObsCfg 'basic\scenes') $ccName
  $ccSaved = Get-Content -LiteralPath $ccCollection -Raw | ConvertFrom-Json
  $ccWritten = (Get-Item -LiteralPath $ccCollection).LastWriteTimeUtc.ToString('o')
  Write-Host "issue 1367 SAVED scene collection $ccCollection (written $ccWritten) -- what the relaunch loads:"
  $ccSources = @($ccSaved.sources)
  foreach ($ccS in $ccSources) {
    $ccKind = $ccS.versioned_id
    if (-not $ccKind) { $ccKind = $ccS.id }
    if ($ccKind -eq 'ndi_source') {
      $ccPin = $ccS.settings.genlock_latency_ms_src
      if ($null -eq $ccPin) { $ccPinText = 'absent (the build default)' } else { $ccPinText = "$ccPin ms" }
      Write-Host "  NDI input '$($ccS.name)': genlock_latency_ms_src = $ccPinText"
    }
  }
  foreach ($ccS in $ccSources) {
    if ([int64]$ccS.mixers -ne 0) {
      $ccSyncMs = ([int64]$ccS.sync / 1000000.0).ToString([System.Globalization.CultureInfo]::InvariantCulture)
      Write-Host "  audio input '$($ccS.name)': sync = $ccSyncMs ms"
    }
  }
} catch {
  Write-Host "issue 1367 saved-settings read-back UNREAD: $($_.Exception.Message) -- report-only, the deploy goes on"
}
PSCCREAD
}

# obs_clean_close_refusal_restore_ps -> emitted by the deploy program right after its keep-alive
# disable (step 1b, a box with keep-alive tasks): defines Invoke-CcRefusalRestore, which the step-(2)
# refusal calls, so a live broadcast keeps the self-heal tasks the deploy had disabled. A box without
# keep-alive tasks defines no hook, and the refusal only exits.
obs_clean_close_refusal_restore_ps() {
  cat <<'PSCCHOOK'
# (1c) issue 1367 -- if step (2) refuses (a stream started after the step-(0a) read), this hook
#      re-enables the keep-alive tasks step (1b) disabled before the program exits.
function Invoke-CcRefusalRestore {
  foreach ($t in $disabledKeepAlive) {
    schtasks /Change /TN $t /ENABLE | Out-Null
    if ($LASTEXITCODE -eq 0) {
      Write-Host "issue 1367: keep-alive task '$t' re-enabled after the refusal"
    } else {
      Write-Host "issue 1367: keep-alive task '$t' did NOT re-enable (rc $LASTEXITCODE) -- re-enable it by hand"
    }
  }
}
PSCCHOOK
}
