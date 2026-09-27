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
#   5. a tray whose exe is not current: kill the running trays (exact process name) and wait on
#      THOSE PIDs only, back it up to .pre-<version> unless that backup already exists (a re-run
#      never overwrites the original pre-roll tray), kill again any tray relaunched since (at most 3
#      tries, recording its parent process), then replace it and verify the installed sha -- a
#      failed replace or a wrong sha restores the backup, and the restore itself is checked by hash
#      before it is reported. A replace that fails because a relaunched tray holds the exe is the
#      named warning `a tray keeps relaunching: <parent>` and restores nothing (the file was never
#      written). A tray whose exe is already current is only checked for a running process (a
#      relaunch that found nobody logged on leaves exactly that state, so a re-run must launch it,
#      never report it OK unread).
#      WHY by PID (the 1.12.0 roll, 27.9.2026): a wait and a re-check by NAME also see a tray started
#      after the kill (the Task Scheduler, or the HKLM Run `DanteSyncTray` entry), so a clean kill
#      read as "the running tray did not exit" once each on stream, mbc and fohabl.
#   6. whenever the tray was stopped -- even after a failed backup/replace/sha -- or a current tray
#      is not running, launch it through a temporary BUILTIN\Users (Limited) scheduled task, named
#      by its well-known SID S-1-5-32-545 because the account name is localized. A group principal
#      starts it in the logged-on user's interactive session with no password. The name form with
#      default settings was proven live on mbc + fohabl (26.9.2026); this SID + settings form is the
#      same principal by the documented API and is UNVERIFIED live until the next roll. The task has
#      explicit settings (start and keep running on battery, no time limit) and is unregistered in a
#      finally; the count is read again AFTER that, and exactly ONE tray process in an interactive
#      session (SessionId >= 1; session 0 is the ssh/service session) is required. A tray already
#      running in an interactive session when this step starts (relaunched meanwhile) is kept: the
#      task is not started, so there is never a second tray.
# The block never throws: it ends with ONE `TRAY OK:` or `TRAY-WARNING:` line that
# dantesync_tray_outcome reads.
dantesync_windows_tray_swap_ps() {
  cat <<'EOF'
# 5. the tray (issue 1372): swapped after the service is back; a failure is a TRAY-WARNING, never a throw
$trayStopped = $false
$trayLaunch = $false
$trayRunning = @()
$trayRespawnedBy = @()
$trayKept = $false
# the process that started a tray (svchost -s Schedule for a scheduled task, explorer for the HKLM
# Run entry at logon); never throws, it only names
function Get-TrayParent([int]$trayChildId) {
    try {
        $trayChild = Get-CimInstance Win32_Process -Filter ('ProcessId=' + $trayChildId) -ErrorAction Stop
        if (-not $trayChild) { return ('pid ' + $trayChildId + ' already exited') }
        $trayParent = Get-CimInstance Win32_Process -Filter ('ProcessId=' + $trayChild.ParentProcessId) -ErrorAction Stop
        if (-not $trayParent) { return ('parent pid ' + $trayChild.ParentProcessId + ' already exited') }
        if ($trayParent.CommandLine) {
            # the line lands in the roll summary: never an unexpected parent's arguments in full
            $trayCmd = [string]$trayParent.CommandLine
            if ($trayCmd.Length -gt 120) { $trayCmd = $trayCmd.Substring(0, 120) + '...' }
            return ($trayParent.Name + ' (' + $trayCmd + ')')
        }
        return $trayParent.Name
    } catch {
        return ('parent unknown: ' + $_.Exception.Message)
    }
}
if ($trayNotes.Count -eq 0 -and $trayCurrent) {
    $trayRunning = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -ge 1 })
    if ($trayRunning.Count -eq 0) { $trayLaunch = $true }
}
if ($trayNotes.Count -eq 0 -and -not $trayCurrent) {
    try {
        # kill the running trays, then wait on THEIR PIDs only: a tray started after the kill is a
        # relaunch, handled right before the replace, never read as the killed one still running
        $trayKilled = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue)
        $trayKilled | Stop-Process -Force -ErrorAction SilentlyContinue
        $trayStopped = $true
        $trayPids = @($trayKilled | ForEach-Object { $_.Id })
        $trayTried = @($trayPids)
        if ($trayPids.Count -gt 0) {
            Wait-Process -Id $trayPids -Timeout 15 -ErrorAction SilentlyContinue
            # a reused PID is not the tray: only a still-alive dantesync-tray counts
            if (@(Get-Process -Id $trayPids -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -eq 'dantesync-tray' }).Count -gt 0) { throw "the killed tray (pid $($trayPids -join ', ')) did not exit" }
        }
        if (-not (Test-Path $trayPre)) { Copy-Item -Force $trayExe $trayPre }
        # the exe as it is now: a failed replace is "untouched" only when the file still hashes to this.
        # On Windows PowerShell 5.1 Get-FileHash is a script function whose read failure is a
        # non-terminating error (a null hash), so -ErrorAction Stop + an empty check: two unreadable
        # hashes must never compare equal
        $trayBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath $trayExe -ErrorAction Stop).Hash
        if (-not $trayBefore) { throw 'could not hash the tray exe before the replace' }
        # a tray relaunched since the kill holds the exe again: kill it right before the replace,
        # at most 3 times, and remember who started each NEW one (a PID tried before is not a relaunch)
        for ($trayTry = 1; $trayTry -le 3; $trayTry++) {
            $trayFresh = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue)
            if ($trayFresh.Count -eq 0) { break }
            foreach ($trayP in $trayFresh) {
                if ($trayTried -notcontains $trayP.Id) {
                    $trayRespawnedBy += (Get-TrayParent $trayP.Id)
                    $trayTried += $trayP.Id
                }
            }
            $trayFresh | Stop-Process -Force -ErrorAction SilentlyContinue
            Wait-Process -Id @($trayFresh | ForEach-Object { $_.Id }) -Timeout 5 -ErrorAction SilentlyContinue
        }
        try {
            Copy-Item -Force $trayTmp $trayExe
            $trayNow = (Get-FileHash -Algorithm SHA256 $trayExe).Hash
            if ($trayNow -ne $trayExpected) { throw ('installed tray SHA256 ' + $trayNow + ' is not ' + $trayExpected) }
        } catch {
            $trayWhy = $_.Exception.Message
            # a sharing violation (Win32 32: a running exe) fails before the file is opened for
            # write, but a lock violation (33) can come after truncation -- so "untouched" is PROVEN
            # by the hash read before the copy, never assumed; an untouched exe needs no restore
            # (a restore would only hit the same lock), anything else is restored
            $trayInUse = @(32, 33) -contains ($_.Exception.HResult -band 0xFFFF)
            $trayUntouched = $false
            if ($trayInUse) {
                try {
                    $trayNowHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $trayExe -ErrorAction Stop).Hash
                    $trayUntouched = ($trayNowHash -and $trayNowHash -eq $trayBefore)
                } catch { }
            }
            # who holds it now: a NEW PID is a relaunch, a PID already tried is a tray that did not die
            $trayHolders = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue)
            $trayNew = @($trayHolders | Where-Object { $trayTried -notcontains $_.Id })
            $trayStuck = @($trayHolders | Where-Object { $trayTried -contains $_.Id })
            foreach ($trayP in $trayNew) { $trayRespawnedBy += (Get-TrayParent $trayP.Id) }
            if ($trayUntouched -and $trayNew.Count -gt 0) {
                $trayNotes += ('a tray keeps relaunching: ' + (($trayRespawnedBy | Select-Object -Unique) -join ' / ') + '; the previous tray exe is untouched: ' + $trayWhy)
            } elseif ($trayUntouched -and $trayStuck.Count -gt 0) {
                $trayNotes += ('a tray did not die (pid ' + (($trayStuck | ForEach-Object { $_.Id }) -join ', ') + '), the previous tray exe is untouched: ' + $trayWhy)
            } elseif ($trayUntouched) {
                $trayNotes += ('the tray exe is locked by another process (no tray running), the previous tray exe is untouched: ' + $trayWhy)
            } else {
                $trayRestored = $false
                try {
                    Copy-Item -Force $trayPre $trayExe
                    # an unreadable (null) hash never proves a restore
                    $trayRestoredHash = (Get-FileHash -Algorithm SHA256 $trayExe -ErrorAction Stop).Hash
                    $trayRestored = ($trayRestoredHash -and $trayRestoredHash -eq (Get-FileHash -Algorithm SHA256 $trayPre).Hash)
                } catch { }
                if ($trayRestored) {
                    $trayNotes += ('tray swap failed, the previous tray restored: ' + $trayWhy)
                } else {
                    $trayNotes += ('tray swap failed AND the restore from ' + $trayPre + ' failed, the tray exe may be partial: ' + $trayWhy)
                }
            }
        }
    } catch {
        $trayNotes += ('tray: ' + $_.Exception.Message)
    }
}
# 6. relaunch the tray whenever it was stopped or is not running, through a temporary BUILTIN\Users (Limited) task
if ($trayStopped -or $trayLaunch) {
    try {
        # a tray relaunched meanwhile (the scheduler, the HKLM Run entry) is kept: never a second tray
        $trayProcs = @(Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -ge 1 })
        if ($trayProcs.Count -eq 0) {
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
        } else {
            $trayKept = $true
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
    $trayOkNote = ''
    if ($trayRespawnedBy.Count -gt 0) {
        $trayOkNote = ' (killed a relaunched tray before the swap, started by: ' + (($trayRespawnedBy | Select-Object -Unique) -join ' / ') + ')'
    }
    if ($trayKept) {
        # the swap succeeded, so a tray running afterwards was started after the new exe landed
        $trayOkNote += ' (kept a tray that was running again, not launched)'
    }
    Write-Output ('TRAY OK: dantesync-tray.exe sha256 ' + $trayExpected + ' running in session ' + $trayProcs[0].SessionId + $trayOkNote)
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
