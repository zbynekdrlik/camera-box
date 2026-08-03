# scripts/avsync-watchdog.ps1 -- #812 productized copy of the live C:\avsync\watchdog.ps1 (this is
# the RED baseline: a faithful port of what is ACTUALLY deployed today, verified live via
# read-only ssh to 10.77.9.204 -- no --webhook, no heartbeat, no bounded measurement call). See
# issue 812's design comment for the fix that replaces this in the very next commit.
$log  = 'C:\avsync\watchdog.log'
$clip = 'C:\avsync\live-clip.mp4'
$ferr = 'C:\avsync\ffmpeg-grab.log'
"WATCHDOG START $(Get-Date -Format s)" | Out-File $log -Append
while ($true) {
  $t0 = Get-Date
  Remove-Item $clip -Force -ErrorAction SilentlyContinue
  & ffmpeg -v error -y -i 'rtmp://127.0.0.1:1234/live/obs-e2e-test' -t 35 -vf "scale=1280:-2,fps=25" -c:v libx264 -preset veryfast -crf 26 -c:a aac -ar 16000 -ac 1 $clip 2>&1 | Out-File $ferr
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
  if ($reason) {
    "LIVE :: NO-SIGNAL - no verdict ($reason)" | Out-File $log -Append
  } else {
    $out = & C:\avsync\venv\Scripts\python.exe C:\avsync\av_sync_measure.py --media $clip --repo C:\avsync\syncnet_python 2>&1 | Select-Object -Last 1
    "LIVE :: $out" | Out-File $log -Append
  }
  $elapsed = ((Get-Date) - $t0).TotalSeconds
  Start-Sleep -Seconds ([Math]::Max(30, 90 - $elapsed))
}
