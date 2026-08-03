# scripts/avsync-vlc-monitor.ps1 -- #807 productized copy of the live C:\avsync\vlc-babysitter.ps1
# (this is the RED baseline: a faithful port of what is ACTUALLY on the box today, verified live
# via read-only ssh to 10.77.9.204 -- correct URL, but NO RTMP-publishing check, NO audio-device
# check, NO heartbeat, and it is not even scheduled/running). See issue 807's design comment for
# the self-verifying fix that replaces this in the very next commit.
$ErrorActionPreference = 'Continue'
$vlc = 'C:\avsync\vlc\vlc.exe'
$url = 'rtmp://127.0.0.1:1234/live/obs-e2e-test'
$vlcArgs = @('--one-instance', '--no-qt-updates-notif', '--repeat', '--network-caching=1000',
  '--extraintf', 'http', '--http-host', '127.0.0.1', '--http-port', '8090', '--http-password', 'avsync', $url)
$log = 'C:\avsync\vlc-babysitter.log'
$auth = @{ Authorization = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes(':avsync')) }

function Log($m) { Add-Content $log "$(Get-Date -Format 'HH:mm:ss') $m" }
function Get-Demux {
  try {
    $r = Invoke-RestMethod -Uri 'http://127.0.0.1:8090/requests/status.json' -Headers $auth -TimeoutSec 3
    return [long]$r.stats.demuxreadbytes
  } catch { return -1 }
}

Log 'BABYSITTER START'
$strikes = 0
while ($true) {
  $p = Get-Process vlc -ErrorAction SilentlyContinue
  if (-not $p) {
    Log 'vlc not running - launching'
    Start-Process $vlc -ArgumentList $vlcArgs
    Start-Sleep 10
    $strikes = 0
    continue
  }
  $d1 = Get-Demux
  Start-Sleep 5
  $d2 = Get-Demux
  if ($d2 -lt 0 -or $d2 -le $d1) {
    $strikes++
    Log "no demux progress (d1=$d1 d2=$d2) strike=$strikes"
    if ($strikes -ge 2) {
      Log 'FROZEN - restarting vlc'
      Stop-Process -Name vlc -Force -ErrorAction SilentlyContinue
      Start-Sleep 3
      $strikes = 0
    }
  } else {
    $strikes = 0
  }
  Start-Sleep 15
}
