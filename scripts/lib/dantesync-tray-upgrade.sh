#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + two defaults + one collector array, no other
# top-level statements) -- sourced by scripts/dantesync-fleet-upgrade.sh, whose own
# `set -euo pipefail` applies; a `set` here would leak into the caller (the scripts/lib convention).
#
# scripts/lib/dantesync-tray-upgrade.sh -- issue 1372: the dantesync TRAY rides every fleet roll.
#
# WHY: dantesync-fleet-upgrade.sh swapped only the dantesync SERVICE binary on a Windows node, so
# dantesync-tray.exe -- the version the operator actually sees -- stayed behind on every roll, and
# the version gate's tray sha-pin ALARMed until someone swapped it by hand (after the 1.11.0 and
# 1.11.1 rolls, 26.9.2026). This lib emits the PowerShell tray arm the upgrade program carries (and a
# tray-only program for a node whose service is already on the target), and reads its one-line
# outcome back. A tray failure is a named WARNING in the roll summary, NEVER a service rollback and
# never a non-zero exit: the tray is UI, the service is the clock.
#
# The emitted text runs inside a `.ps1` sent as a FILE and run with `powershell -File` (never nested
# PowerShell over ssh). It relies on the upgrade program's `$ErrorActionPreference = 'Stop'`, so
# every tray step is inside its own try/catch that only appends to `$trayNotes`.
#
# DEPENDS (resolved at CALL time): dantesync_release_url_windows_tray (dantesync-fleet-upgrade.sh).

# The tray's install path: the one dantesync-version-gate.sh sha-pins (DANTESYNC_TRAY_GATE_WIN_EXE).
DANTESYNC_WIN_TRAY_EXE='C:\Program Files\DanteSync\dantesync-tray.exe'
# The temporary relaunch task: registered, started and unregistered in the same program.
DANTESYNC_WIN_TRAY_TASK='DanteSyncTrayRelaunch-1372'
# The per-roll collector of "<node>: <reason>" tray warnings (dantesync_tray_note / _report).
DANTESYNC_TRAY_WARNINGS=()

# dantesync_windows_tray_fetch_ps VERSION -> the PowerShell lines that read the SAME pinned release's
# tray `.sha256`, and download + verify the tray asset only when the installed tray differs, BEFORE
# anything on the box is stopped. A tray already on the release sets $trayCurrent (left running, not
# downloaded again). Every failure is recorded in $trayNotes and never thrown, so a tray that cannot
# be fetched never stops the service upgrade that follows.
dantesync_windows_tray_fetch_ps() {
  local version="$1" url
  url="$(dantesync_release_url_windows_tray "$version")"
  cat <<EOF
# 1b. the tray (issue 1372): the same pinned release's tray asset, fetched + verified before any stop
\$trayUrl = '$url'
\$trayExe = '$DANTESYNC_WIN_TRAY_EXE'
\$trayPre = '$DANTESYNC_WIN_TRAY_EXE.pre-$version'
\$trayTask = '$DANTESYNC_WIN_TRAY_TASK'
EOF
  cat <<'EOF'
$trayTmp = Join-Path $env:TEMP 'dantesync-tray-new.exe'
$trayNotes = @()
$trayExpected = ''
$trayCurrent = $false
$trayProcs = @()
try {
    if (-not (Test-Path $trayExe)) { throw ('no tray installed at ' + $trayExe) }
    Invoke-WebRequest -UseBasicParsing -Uri ($trayUrl + '.sha256') -OutFile ($trayTmp + '.sha256')
    $trayExpected = ((Get-Content ($trayTmp + '.sha256')) -split '\s+')[0].Trim()
    if ((Get-FileHash -Algorithm SHA256 $trayExe).Hash -eq $trayExpected) {
        $trayCurrent = $true
    } else {
        Invoke-WebRequest -UseBasicParsing -Uri $trayUrl -OutFile $trayTmp
        $trayGot = (Get-FileHash -Algorithm SHA256 $trayTmp).Hash
        if ($trayExpected -ne $trayGot) { throw ('download SHA256 MISMATCH expected ' + $trayExpected + ' got ' + $trayGot) }
    }
} catch {
    $trayNotes += ('tray not fetched: ' + $_.Exception.Message)
}
EOF
}

# dantesync_windows_tray_swap_ps -> the PowerShell lines that run AFTER the service is back:
#   5. a tray whose exe is not current: stop it (exact process name), back it up to .pre-<version>
#      unless that backup already exists (a re-run never overwrites the original pre-roll tray),
#      replace it and verify the installed sha -- a failed replace or a wrong sha restores the
#      backup, and the restore itself is checked by hash before it is reported. A tray whose exe is
#      already current is only checked for a running process (a relaunch that found nobody logged
#      on leaves exactly that state, so a re-run must launch it, never report it OK unread).
#   6. whenever the tray was stopped -- even after a failed backup/replace/sha -- or a current tray
#      is not running, launch it through a temporary BUILTIN\Users (Limited) scheduled task, named
#      by its well-known SID S-1-5-32-545 because the account name is localized. A group principal
#      starts it in the logged-on user's interactive session with no password. The name form with
#      default settings was proven live on mbc + fohabl (26.9.2026); this SID + settings form is the
#      same principal by the documented API and is UNVERIFIED live until the next roll. The task has
#      explicit settings (start and keep running on battery, no time limit) and is unregistered in a
#      finally; the count is read again AFTER that, and exactly ONE tray process in an interactive
#      session (SessionId >= 1; session 0 is the ssh/service session) is required.
# The block never throws: it ends with ONE `TRAY OK:` or `TRAY-WARNING:` line that
# dantesync_tray_outcome reads.
dantesync_windows_tray_swap_ps() {
  cat <<'EOF'
# 5. the tray (issue 1372): swapped after the service is back; a failure is a TRAY-WARNING, never a throw
$trayStopped = $false
$trayLaunch = $false
$trayRunning = @()
if ($trayNotes.Count -eq 0 -and $trayCurrent) {
    $trayRunning = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -ge 1 })
    if ($trayRunning.Count -eq 0) { $trayLaunch = $true }
}
if ($trayNotes.Count -eq 0 -and -not $trayCurrent) {
    try {
        Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Stop-Process -Force
        $trayStopped = $true
        Wait-Process -Name dantesync-tray -Timeout 15 -ErrorAction SilentlyContinue
        if (Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue) { throw 'the running tray did not exit' }
        if (-not (Test-Path $trayPre)) { Copy-Item -Force $trayExe $trayPre }
        try {
            Copy-Item -Force $trayTmp $trayExe
            $trayNow = (Get-FileHash -Algorithm SHA256 $trayExe).Hash
            if ($trayNow -ne $trayExpected) { throw ('installed tray SHA256 ' + $trayNow + ' is not ' + $trayExpected) }
        } catch {
            $trayWhy = $_.Exception.Message
            $trayRestored = $false
            try {
                Copy-Item -Force $trayPre $trayExe
                $trayRestored = ((Get-FileHash -Algorithm SHA256 $trayExe).Hash -eq (Get-FileHash -Algorithm SHA256 $trayPre).Hash)
            } catch { }
            if ($trayRestored) {
                $trayNotes += ('tray swap failed, the previous tray restored: ' + $trayWhy)
            } else {
                $trayNotes += ('tray swap failed AND the restore from ' + $trayPre + ' failed, the tray exe may be partial: ' + $trayWhy)
            }
        }
    } catch {
        $trayNotes += ('tray: ' + $_.Exception.Message)
    }
}
# 6. relaunch the tray whenever it was stopped or is not running, through a temporary BUILTIN\Users (Limited) task
if ($trayStopped -or $trayLaunch) {
    try {
        $trayAction = New-ScheduledTaskAction -Execute $trayExe
        $trayPrincipal = New-ScheduledTaskPrincipal -GroupId 'S-1-5-32-545' -RunLevel Limited
        $traySettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)
        Register-ScheduledTask -TaskName $trayTask -Action $trayAction -Principal $trayPrincipal -Settings $traySettings -Force | Out-Null
        try {
            Start-ScheduledTask -TaskName $trayTask
            for ($i = 0; $i -lt 30; $i++) {
                $trayProcs = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -ge 1 })
                if ($trayProcs.Count -ge 1) { break }
                Start-Sleep -Milliseconds 500
            }
        } finally {
            try {
                Unregister-ScheduledTask -TaskName $trayTask -Confirm:$false -ErrorAction Stop
            } catch {
                $trayNotes += ('the temporary task ' + $trayTask + ' could not be unregistered: ' + $_.Exception.Message)
            }
        }
        $trayProcs = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -ge 1 })
        if ($trayProcs.Count -ne 1) { throw ('expected one tray process in an interactive session after the relaunch, found ' + $trayProcs.Count + ' (is a user logged on?)') }
    } catch {
        $trayNotes += ('tray relaunch: ' + $_.Exception.Message)
    }
}
Remove-Item -Force -ErrorAction SilentlyContinue $trayTmp, ($trayTmp + '.sha256')
if ($trayNotes.Count -gt 0) {
    Write-Output ('TRAY-WARNING: ' + ($trayNotes -join '; '))
} elseif ($trayStopped -or $trayLaunch) {
    Write-Output ('TRAY OK: dantesync-tray.exe sha256 ' + $trayExpected + ' running in session ' + $trayProcs[0].SessionId)
} else {
    Write-Output ('TRAY OK: dantesync-tray.exe already on sha256 ' + $trayExpected + ', running in session ' + $trayRunning[0].SessionId)
}
EOF
}

# dantesync_windows_tray_only_ps VERSION -> the CONTENT of a `.ps1` that refreshes ONLY the tray, for
# a Windows node whose service is already on VERSION (so re-running the roll repairs an earlier
# TRAY-WARNING). A tray already on the release is left running and not downloaded again.
dantesync_windows_tray_only_ps() {
  printf '%s\n' "\$ErrorActionPreference = 'Stop'"
  dantesync_windows_tray_fetch_ps "$1"
  dantesync_windows_tray_swap_ps
}

# dantesync_tray_outcome OUTPUT -> "OK <detail>" or "WARNING <reason>" from the LAST `TRAY OK:` /
# `TRAY-WARNING:` line a tray program printed; a missing line is a WARNING too (the program never
# reached its tray report), never a silent OK. Windows CRs are dropped.
dantesync_tray_outcome() {
  local line
  line="$(printf '%s\n' "$1" | tr -d '\r' | grep -E '^TRAY( OK|-WARNING): ' | tail -1 || true)"
  case "$line" in
    'TRAY OK: '*) printf 'OK %s' "${line#TRAY OK: }" ;;
    'TRAY-WARNING: '*) printf 'WARNING %s' "${line#TRAY-WARNING: }" ;;
    *) printf 'WARNING no tray report in the upgrade output' ;;
  esac
}

# dantesync_tray_note NAME OUTPUT -> logs `[NAME] tray OK: ...` or `[NAME] tray WARNING: ...` and
# collects a warning into DANTESYNC_TRAY_WARNINGS for dantesync_tray_report.
dantesync_tray_note() {
  local outcome
  outcome="$(dantesync_tray_outcome "$2")"
  case "$outcome" in
    "OK "*) printf '[%s] tray OK: %s\n' "$1" "${outcome#OK }" ;;
    *)
      printf '[%s] tray WARNING: %s\n' "$1" "${outcome#WARNING }"
      DANTESYNC_TRAY_WARNINGS+=("$1: ${outcome#WARNING }") ;;
  esac
}

# dantesync_tray_report -> the named roll-summary WARNING (one reason per node); silent when every
# tray was refreshed. Printed before every exit after the roll started.
dantesync_tray_report() {
  local w
  [ "${#DANTESYNC_TRAY_WARNINGS[@]}" -gt 0 ] || return 0
  printf 'WARNING: dantesync-tray was NOT refreshed on %s node(s) -- the service roll is unaffected (the tray is UI, the service is the clock); re-run the roll to retry the tray, the version gate sha-pin names it meanwhile:\n' \
    "${#DANTESYNC_TRAY_WARNINGS[@]}"
  for w in "${DANTESYNC_TRAY_WARNINGS[@]}"; do printf '  - %s\n' "$w"; done
}
