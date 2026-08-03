# scripts/avsync-watchdog.ps1 -- #812 productized AI A/V-sync watchdog for the stream OBS program
# output. Long-running measurement loop: grab 35s of the LIVE production RTMP relay every ~90s,
# apply the #814 grab-freshness gate (stale/short/small clip => NO-SIGNAL, never a false
# measurement), and hand a good clip to av_sync_measure.py (SyncNet-based offset estimator).
#
# WHY THIS FILE EXISTS (#812): this loop previously lived ONLY hand-built on the stream box
# (C:\avsync\watchdog.ps1) with THREE live defects, two of which this file fixes:
#   1. No --webhook was ever passed to av_sync_measure.py -- a fully alive watchdog posted
#      NOTHING to Discord (av_sync_measure.py's notify_discord() only fires when --webhook is set
#      AND the measured offset exceeds --threshold-ms). Fixed below: --webhook is read from an
#      UNCOMMITTED local secret file, never a repo literal (security-basics.md).
#   2. No heartbeat signal of any kind -- silence and health were indistinguishable from outside
#      the box. Fixed below: a heartbeat timestamp is written EVERY pass, regardless of outcome,
#      so the dev1-side scripts/avsync-heartbeat-alert-watchdog.sh can alert on staleness.
#   (The THIRD live defect -- an ONSTART-only scheduled task with no repetition/keep-alive -- is
#   fixed by scripts/avsync-watchdog-install.sh + scripts/avsync-keepalive.ps1, not by this file.)
#
# ALSO fixed here: the root incident evidence ("two orphan python PIDs...consistent with a hung
# av_sync_measure.py that took the loop down with it") shows the inline python call could hang the
# WHOLE outer loop indefinitely with no bound -- a keep-alive task that only checks "is the outer
# powershell.exe process still there" would never notice THAT kind of hang (the process is alive,
# just permanently blocked on one child call). Invoke-Measurement below bounds the python call to
# 180s (generous vs. the ~90s natural cadence -- CPU-only SyncNet inference on a 35s clip can
# legitimately take a while) and force-kills it (and, since Start-Process gives a direct process
# handle rather than going through a job-in-a-child-shell, the actual python.exe PID) on timeout,
# so the loop can never wedge on a hung measurement the way the live box did.
#
# The Discord webhook is a SECRET and is never committed: read from an UNCOMMITTED local file on
# the box, C:\avsync\discord-webhook.txt (one line, the full webhook URL). A missing/empty file
# means "no Discord leg configured yet" -- the loop still runs and still writes the heartbeat, it
# simply never passes --webhook (fail-safe: no config file => no attempt to post, never a crash).
#
# Usage: scheduled by Task Scheduler (see scripts/avsync-watchdog-install.sh) as a long-running
# process; also runnable directly for manual testing:
#   powershell -NoProfile -ExecutionPolicy Bypass -File C:\avsync\watchdog.ps1

$log         = 'C:\avsync\watchdog.log'
$clip        = 'C:\avsync\live-clip.mp4'
$ferr        = 'C:\avsync\ffmpeg-grab.log'
$heartbeat   = 'C:\avsync\avsync-watchdog-heartbeat.txt'
$webhookCfg  = 'C:\avsync\discord-webhook.txt'
$grabUrl     = 'rtmp://127.0.0.1:1234/live/obs-e2e-test'   # the REAL production broadcast (service.json) -- see issue 812's comment
$pythonExe   = 'C:\avsync\venv\Scripts\python.exe'
$measureScript = 'C:\avsync\av_sync_measure.py'
$syncnetRepo = 'C:\avsync\syncnet_python'

function Get-Webhook {
  if (Test-Path $webhookCfg) {
    $w = (Get-Content $webhookCfg -Raw).Trim()
    if ($w) { return $w }
  }
  return $null
}

function Write-Heartbeat($status) {
  "$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())`t$status" | Set-Content -Path $heartbeat
}

# Invoke-Measurement CLIP WEBHOOK_ARGS -> the last output line of av_sync_measure.py, bounded to
# 180s. A genuinely hung measurement is force-killed (direct process handle, not a job-in-a-child-
# shell -- see the file header) and reported as a TIMEOUT line rather than freezing this loop.
function Invoke-Measurement($clipPath, [string[]]$webhookArgs) {
  $stdoutFile = [System.IO.Path]::GetTempFileName()
  $stderrFile = [System.IO.Path]::GetTempFileName()
  $argsList = @($measureScript, '--media', $clipPath, '--repo', $syncnetRepo) + $webhookArgs
  $proc = Start-Process -FilePath $pythonExe -ArgumentList $argsList `
    -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile -NoNewWindow -PassThru
  $finished = $proc.WaitForExit(180000)
  if (-not $finished) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $stdoutFile, $stderrFile -Force -ErrorAction SilentlyContinue
    return 'TIMEOUT: av_sync_measure.py did not complete within 180s -- killed to prevent a wedged watchdog loop'
  }
  $lastOut = Get-Content $stdoutFile -ErrorAction SilentlyContinue | Select-Object -Last 1
  $lastErr = Get-Content $stderrFile -ErrorAction SilentlyContinue | Select-Object -Last 1
  Remove-Item $stdoutFile, $stderrFile -Force -ErrorAction SilentlyContinue
  if ($lastOut) { return $lastOut }
  return $lastErr
}

"WATCHDOG START $(Get-Date -Format s)" | Out-File $log -Append
while ($true) {
  $t0 = Get-Date
  Remove-Item $clip -Force -ErrorAction SilentlyContinue
  & ffmpeg -v error -y -i $grabUrl -t 35 -vf "scale=1280:-2,fps=25" -c:v libx264 -preset veryfast -crf 26 -c:a aac -ar 16000 -ac 1 $clip 2>&1 | Out-File $ferr
  $rc = $LASTEXITCODE
  $reason = $null
  if ($rc -ne 0) { $reason = "ffmpeg rc=$rc (relay/stream down)" }
  elseif (-not (Test-Path $clip)) { $reason = 'no clip produced' }
  else {
    $fi = Get-Item $clip
    $ageSec = ((Get-Date) - $fi.LastWriteTime).TotalSeconds
    $dur = -1
    try { $dur = [double](& ffprobe -v error -show_entries format=duration -of csv=p=0 $clip) } catch { $dur = -1 }
    if ($fi.Length -lt 200000) { $reason = "clip too small ($($fi.Length) B)" }
    elseif ($ageSec -gt 180) { $reason = "clip STALE (age $([int]$ageSec)s) - grab did not run" }
    elseif ($dur -ge 0 -and $dur -lt 20) { $reason = "clip too short ($([math]::Round($dur,1))s < 20s)" }
  }
  $webhook = Get-Webhook
  if ($reason) {
    "LIVE :: NO-SIGNAL - no verdict ($reason)" | Out-File $log -Append
    Write-Heartbeat "no-signal: $reason"
  } else {
    $webhookArgs = @()
    if ($webhook) { $webhookArgs = @('--webhook', $webhook) }
    $out = Invoke-Measurement $clip $webhookArgs
    "LIVE :: $out" | Out-File $log -Append
    Write-Heartbeat "measured: $out"
  }
  $elapsed = ((Get-Date) - $t0).TotalSeconds
  Start-Sleep -Seconds ([Math]::Max(30, 90 - $elapsed))
}
