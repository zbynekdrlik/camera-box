# obs-guarded-launch.ps1 -- #786 guarded OBS launch, runs ON the box (stream; strih post-event).
#
# THE POINT (user directive 2026-07-16): the STANDARD operator launch path itself must carry the
# audio-buffering gate -- never an external/remote monitor (the rig travels to events without
# dev1). The box's "OBS Studio" shortcuts are retargeted to run THIS script; the script launches
# the real obs64.exe and gates the ASIO draw. Operator behavior is unchanged: same icon, same
# double-click. There is NO hidden mode: the genlock build is env-free/hard-locked (#257), so OBS
# itself runs IDENTICALLY however it is started -- this wrapper only adds the launch-draw CHECK.
#
# WHY: some launches hit an ASIO init race ('ASIO Input Capture' on VB-Matrix VASIO-8 floods
# stale audio before Dante VSC finishes init) -> libobs ratchets its GLOBAL audio buffering to
# the 960 ms max within the first seconds and it NEVER shrinks until OBS restarts -> the whole
# session's A/V is off by ~0.9 s (live incident 2026-07-15). Box standard is 64 ms (some days
# 85); threshold 100 = standard + small headroom. Permanent fix is OBS-level (#786) -- this is
# the launch-path hotfix.
#
# Flow:
#   OBS already running -> read current session's buffer peak; bad -> visible warning (NO kill,
#     an operator decides mid-event); clean -> exit silently (same as clicking .lnk today).
#   OBS not running -> up to 3 launch draws: clear sentinels, start obs64 (correct cwd), wait,
#     read fresh log; clean -> done (silent); bad -> kill + redraw; 3rd bad -> leave OBS RUNNING
#     (a running OBS with bad A/V beats no OBS at a live event) + LOUD message box with the fix.
#   Every run appends one line to C:\camera-box\obs-guarded-launch.log.
#
# ASCII ONLY in this file (a cp1252 misread turns a UTF-8 em-dash into a smart quote that
# PowerShell treats as a string terminator -- proven on this rig, see #786).

param([switch]$CheckOnly)

$ErrorActionPreference = 'SilentlyContinue'   # operator path: degrade, log, never die invisibly
$exe        = 'C:\Program Files\obs-studio\bin\64bit\obs64.exe'
$cwd        = 'C:\Program Files\obs-studio\bin\64bit'
$logDir     = "$env:APPDATA\obs-studio\logs"
$runLog     = 'C:\camera-box\obs-guarded-launch.log'
$threshold  = 100   # ms; box standard 64/85 -- same bound as launch-obs-genlock.sh (3b) + its pin test
$maxDraws   = 3

Add-Type -AssemblyName System.Windows.Forms | Out-Null

function Write-RunLog([string]$msg) {
    $line = "{0} {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
    Add-Content -Path $runLog -Value $line -ErrorAction SilentlyContinue
}

function Get-BufferDraw {
    $log = Get-ChildItem $logDir -Filter *.txt -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $log) { return [pscustomobject]@{ Peak = 0; Maxed = $false; Name = '(no log)' } }
    $t = Get-Content $log.FullName -Raw -ErrorAction SilentlyContinue
    $peak = 0
    foreach ($m in [regex]::Matches([string]$t, 'total audio buffering is now (\d+) milliseconds')) {
        $v = [int]$m.Groups[1].Value; if ($v -gt $peak) { $peak = $v }
    }
    [pscustomobject]@{ Peak = $peak; Maxed = ([string]$t -match 'Max audio buffering reached'); Name = $log.Name }
}

function Show-Alert([string]$text) {
    [System.Windows.Forms.MessageBox]::Show($text, 'OBS audio buffer (#786)',
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Warning) | Out-Null
}

if ($CheckOnly) {
    $d = Get-BufferDraw
    $verdict = if ($d.Peak -le $threshold -and -not $d.Maxed) { 'CLEAN' } else { 'BAD' }
    Write-Output ("check-only: {0} peak={1}ms maxed={2} log={3}" -f $verdict, $d.Peak, $d.Maxed, $d.Name)
    exit 0
}

$running = Get-Process obs64 -ErrorAction SilentlyContinue
if ($running) {
    # Same as today's .lnk double-click on a running OBS: do not touch the live process.
    $d = Get-BufferDraw
    if ($d.Peak -gt $threshold -or $d.Maxed) {
        Write-RunLog ("already-running BAD peak={0}ms maxed={1} log={2}" -f $d.Peak, $d.Maxed, $d.Name)
        Show-Alert ("POZOR: beziace OBS ma audio buffer {0} ms (norma 64 ms) -- zvuk ide o ~{0} ms neskor, A/V sync je MIMO.`n`nFix: zavri OBS a spusti ho znova (tato ochrana pri starte zly zreb sama vymeni). OBS teraz NEVYPINAM -- rozhodni ty." -f $d.Peak)
    } else {
        Write-RunLog ("already-running clean peak={0}ms log={1}" -f $d.Peak, $d.Name)
    }
    exit 0
}

for ($draw = 1; $draw -le $maxDraws; $draw++) {
    Remove-Item "$env:APPDATA\obs-studio\.sentinel\*" -Force -ErrorAction SilentlyContinue
    Start-Process -FilePath $exe -WorkingDirectory $cwd
    $proc = $null
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        $proc = Get-Process obs64 -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($proc -and $proc.WorkingSet64 -gt 100MB) { break }
    }
    if (-not $proc) {
        Write-RunLog ("draw {0}: obs64 did not start" -f $draw)
        Show-Alert 'OBS sa nespustilo (proces nenabehol do 30 s). Skus znova; ak to trva, pozri C:\camera-box\obs-guarded-launch.log.'
        exit 1
    }
    Start-Sleep -Seconds 12   # ASIO starts right after module load; a bad burst completes within ~5 s
    $d = Get-BufferDraw
    if ($d.Peak -le $threshold -and -not $d.Maxed) {
        Write-RunLog ("draw {0}: CLEAN peak={1}ms log={2}" -f $draw, $d.Peak, $d.Name)
        exit 0
    }
    Write-RunLog ("draw {0}: BAD peak={1}ms maxed={2} log={3}" -f $draw, $d.Peak, $d.Maxed, $d.Name)
    if ($draw -eq $maxDraws) {
        Show-Alert ("OBS 3x za sebou nabehli so zlym audio bufferom (teraz {0} ms, norma 64 ms) -- A/V sync bude MIMO o ~{0} ms.`n`nOBS necham BEZAT (radsej bezi nez nic), ale zvuk/obraz nesedi. Skus OBS vypnut a spustit este raz; ak to nepomoze, problem je v ASIO zariadeniach (VB-Matrix / Dante) -- pozri #786." -f $d.Peak)
        exit 7
    }
    Stop-Process -Name obs64 -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 4
}
