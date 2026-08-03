# scripts/avsync-vlc-monitor.ps1 -- #807 self-verifying VLC babysitter for the stream-OBS program
# audio tap. Evolution of the hand-built C:\avsync\vlc-babysitter.ps1: BEFORE ever concluding VLC
# itself is frozen, first verifies the RTMP SOURCE is actually publishing -- an off-air/no-signal
# period must never be misread as "VLC froze" (confirmed live: the old babysitter's own log shows
# an infinite restart loop against a genuinely-idle source, demuxreadbytes stuck at 0 forever).
# Separately audits whether the configured playback device (Focusrite -- confirmed ABSENT on this
# box today, see issue 807's validated comment) is present, and fails LOUD (named) rather than
# silently leaving playback pointed at nothing/the wrong device.
#
# WHY THIS FILE EXISTS (#807): the correct launch URL previously lived in only TWO places (the
# Desktop "AV-sync monitor.bat" and this script's hand-built predecessor, vlc-babysitter.ps1,
# which was not even scheduled/running) -- a human sometimes typed the RTMP URL by hand instead,
# silently dropping the "/obs-e2e-test" playpath (an RTMP URL with no stream name opens and plays
# nothing, with ZERO error). This script hardcodes the ONE correct URL (matching
# scripts/avsync-watchdog.ps1's own constant -- these are two independent standalone files copied
# onto the box, see .claude/rules/imag-obs-supervision.md's "a standalone script cannot safely
# source a sibling lib" gotcha, so the constant is deliberately duplicated with a cross-reference
# comment rather than sourced) and is the ONE scheduled, self-verifying launch path -- no
# hand-typed URL should ever be needed again.
#
# Usage: scheduled by Task Scheduler (see scripts/avsync-watchdog-install.sh) as a long-running
# process; also runnable directly for manual testing:
#   powershell -NoProfile -ExecutionPolicy Bypass -File C:\avsync\avsync-vlc-monitor.ps1
$ErrorActionPreference = 'Continue'

$vlc         = 'C:\avsync\vlc\vlc.exe'
$url         = 'rtmp://127.0.0.1:1234/live/obs-e2e-test'   # matches scripts/avsync-watchdog.ps1's own $grabUrl -- the REAL production broadcast
$vlcArgs     = @('--one-instance', '--no-qt-updates-notif', '--repeat', '--network-caching=1000',
  '--extraintf', 'http', '--http-host', '127.0.0.1', '--http-port', '8090', '--http-password', 'avsync', $url)
$log         = 'C:\avsync\avsync-vlc-monitor.log'
$heartbeat   = 'C:\avsync\avsync-vlc-monitor-heartbeat.txt'
$audioDevice = 'Focusrite'   # the configured/expected playback device; absence is FAILED LOUD, named, but never blocks playback (Windows' own default-device fallback still gives audible output)
$auth = @{ Authorization = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes(':avsync')) }

function Log($m) { Add-Content $log "$(Get-Date -Format 'yyyy-MM-ddTHH:mm:ss') $m" }
function Write-Heartbeat($status) {
  "$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())`t$status" | Set-Content -Path $heartbeat
}
function Get-Demux {
  try {
    $r = Invoke-RestMethod -Uri 'http://127.0.0.1:8090/requests/status.json' -Headers $auth -TimeoutSec 3
    return [long]$r.stats.demuxreadbytes
  } catch { return -1 }
}

# Test-RtmpPublishing -> $true when ffprobe can read a video stream off the SAME pull URL VLC
# uses -- independent of VLC entirely. This is what distinguishes "the source is off-air" from
# "VLC itself is frozen", which the pre-#807 babysitter could not tell apart (its own log shows an
# infinite restart loop against a genuinely-idle source). Bounded via Start-Job/Wait-Job (not an
# ffmpeg/ffprobe-specific timeout flag, which varies by protocol/build) so a hung ffprobe call can
# never itself freeze this monitor loop.
function Test-RtmpPublishing {
  $job = Start-Job -ScriptBlock {
    param($u)
    & ffprobe -v error -select_streams v:0 -show_entries stream=codec_name -of csv=p=0 $u 2>$null
  } -ArgumentList $url
  $done = Wait-Job $job -Timeout 8
  if (-not $done) {
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
    return $false
  }
  $out = Receive-Job $job
  Remove-Job $job -Force -ErrorAction SilentlyContinue
  return [bool]$out
}

# Test-AudioDeviceConfigured -> $true when a device whose name contains $audioDevice is present
# among Win32_SoundDevice entries. Get-CimInstance simply OMITS an absent device entirely (does
# NOT list it with an error Status) -- confirmed live: the Focusrite/Scarlett hardware does not
# appear in the list at all today, which is exactly what this check is meant to catch.
function Test-AudioDeviceConfigured {
  try {
    $devices = Get-CimInstance Win32_SoundDevice -ErrorAction Stop
    return [bool]($devices | Where-Object { $_.Name -like "*$audioDevice*" })
  } catch { return $false }
}

Log 'BABYSITTER START (#807 self-verifying)'
$strikes = 0
while ($true) {
  $audioPresent = Test-AudioDeviceConfigured
  $suffix = if ($audioPresent) { '' } else { ', audio-device-missing' }
  if (-not $audioPresent) {
    Log "FATAL: configured audio device '$audioDevice' is ABSENT -- playback is falling back to the Windows default device (may be silent/wrong output)"
  }

  $publishing = Test-RtmpPublishing
  if (-not $publishing) {
    Log 'no-signal: RTMP source is not publishing -- not a VLC fault, not striking'
    $strikes = 0
    Write-Heartbeat "no-signal$suffix"
    Start-Sleep 15
    continue
  }

  $p = Get-Process vlc -ErrorAction SilentlyContinue
  if (-not $p) {
    Log 'vlc not running - launching'
    Start-Process $vlc -ArgumentList $vlcArgs
    Start-Sleep 10
    $strikes = 0
    Write-Heartbeat "launched$suffix"
    continue
  }

  $d1 = Get-Demux
  Start-Sleep 5
  $d2 = Get-Demux
  if ($d2 -lt 0 -or $d2 -le $d1) {
    $strikes++
    Log "no demux progress (d1=$d1 d2=$d2) strike=$strikes"
    if ($strikes -ge 2) {
      Log 'FROZEN - restarting vlc (source IS publishing, so this genuinely is a VLC fault)'
      Stop-Process -Name vlc -Force -ErrorAction SilentlyContinue
      Start-Sleep 3
      $strikes = 0
      Write-Heartbeat "frozen-restarted$suffix"
      Start-Sleep 15
      continue
    }
  } else {
    $strikes = 0
  }
  Write-Heartbeat "ok$suffix"
  Start-Sleep 15
}
