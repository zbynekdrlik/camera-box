#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + text builders, no other top-level statements) --
# sourced by scripts/dantesync-fleet-upgrade.sh and scripts/lib/dantesync-install.sh; a `set` here
# would leak into the caller (the scripts/lib convention).
#
# scripts/lib/dsync-swap.sh -- issue 1487: the fleet's move from dantesync to dsync (owner: every PC
# runs dsync, no PC carries a mix), and the upgrade programs of the fleet roll (moved here from
# dantesync-fleet-upgrade.sh, which sits at its ~1000-line budget).
#
# WHAT THE RENAME CHANGED (dsync 1.25.0): the binary, the unit / Windows service, the install dir, the
# state dir. dsync does not move anything itself: it uses /etc/dsync when it exists, else /etc/dantesync
# IN PLACE, else it creates /etc/dsync (src/state_dir.rs, "resolve, never copy"), and it takes BOTH
# singleton locks, so a dantesync still running is refused.
#
# THE SWAP KEEPS THE WAY BACK until the node verified:
#   1. the new binary is verified before anything stops (the roll's existing order);
#   2. inside the read-only-root window: dantesync stopped + disabled, the state dir RENAMED (so the
#      DATE MASTER's date-offset.json moves whole: same format since 1.15, restored if under a day old,
#      so its restart does not step the fleet date), dsync.service DERIVED from the node's own unit (it
#      keeps that box's ExecStart arguments and ExecStartPre) with its drop-ins, the binary installed,
#      dsync enabled;
#   3. the verified close, then the start;
#   4. only after the orchestrator VERIFIED the node, a separate finalize program removes the old
#      binary, unit, drop-ins (Windows: the service, install dir, autostart entries).
# Until step 4 a rollback is the reverse swap. A dsync-to-dsync upgrade never enters this path: the
# legacy state is `dantesync.service present, dsync.service absent`, read on the box at run time.
#
# The DSYNC_LEGACY_LINUX_* paths are test seams: dev1 itself may still carry a real dantesync.
#
# DEPENDS (resolved at CALL time, from dantesync-fleet-upgrade.sh + dantesync-rollback.sh):
# DANTESYNC_LINUX_BIN / _BAK / _STAGED, DANTESYNC_WIN_EXE / _BAK, DANTESYNC_DEAD_TASK,
# dantesync_release_url_linux / _windows, dantesync_windows_wait_service_exit_ps,
# dantesync_windows_tray_fetch_ps / _swap_ps, _dantesync_linux_rw_window_sh,
# _dantesync_linux_date_state_rm_sh, _dantesync_windows_date_state_rm_ps.

DSYNC_LINUX_UNIT="/etc/systemd/system/dsync.service"
DSYNC_LINUX_STATE="/etc/dsync"
DSYNC_LEGACY_LINUX_UNIT="${DSYNC_LEGACY_LINUX_UNIT:-/etc/systemd/system/dantesync.service}"
DSYNC_LEGACY_LINUX_BIN="${DSYNC_LEGACY_LINUX_BIN:-/usr/local/bin/dantesync}"
DSYNC_LEGACY_LINUX_STATE="${DSYNC_LEGACY_LINUX_STATE:-/etc/dantesync}"
DSYNC_WIN_STATE='C:\ProgramData\dsync'
DSYNC_LEGACY_WIN_DIR='C:\Program Files\DanteSync'
DSYNC_LEGACY_WIN_EXE='C:\Program Files\DanteSync\dantesync.exe'
DSYNC_LEGACY_WIN_STATE='C:\ProgramData\DanteSync'

# dsync_linux_state_migrate_sh -> the bash lines that move the state dir to its new name. A rename, so
# nothing is left behind and drift.json / date-offset.json stay single. An /etc/dsync that already
# exists (a run cut short) keeps what it has and takes only what it lacks, then the old dir goes: dsync
# would otherwise read the near-empty new one and never see the old config.
dsync_linux_state_migrate_sh() {
  cat <<EOF
if [ -d "$DSYNC_LEGACY_LINUX_STATE" ]; then
  if [ ! -e "$DSYNC_LINUX_STATE" ]; then
    mv "$DSYNC_LEGACY_LINUX_STATE" "$DSYNC_LINUX_STATE"
  else
    cp -an "$DSYNC_LEGACY_LINUX_STATE"/. "$DSYNC_LINUX_STATE"/
    rm -rf "$DSYNC_LEGACY_LINUX_STATE"
  fi
elif [ ! -e "$DSYNC_LINUX_STATE" ]; then
  install -d -m 755 "$DSYNC_LINUX_STATE"
fi
EOF
}

# dsync_unit_from_legacy_sed -> the sed arguments that turn a dantesync unit text into its dsync unit.
# setup-strih's strih_dantesync_unit_text and the setup-device / setup-imag units render exactly what
# this produces (pinned by tests/python/test_dsync_swap_1487.py), so the next deploy after a roll reads
# the unit as unchanged and does not restart the date master.
dsync_unit_from_legacy_sed() {
  printf '%s' "-e 's/Dante Time Sync/dsync/' -e 's/DanteSync/dsync/g' -e 's/dantesync/dsync/g'"
}

# dsync_linux_swap_in_sh -> the lines that run INSIDE the rw window: stop + disable the old service,
# move the state, derive the unit and its drop-ins, install the verified "$tmp/dsync", enable dsync.
# Only stops, writes and enable/disable, the issue-1407 window rule.
dsync_linux_swap_in_sh() {
  local sedargs
  sedargs="$(dsync_unit_from_legacy_sed)"
  cat <<EOF
systemctl stop dantesync
systemctl disable dantesync
$(dsync_linux_state_migrate_sh)
sed $sedargs "$DSYNC_LEGACY_LINUX_UNIT" > "$DSYNC_LINUX_UNIT.new"
chmod 0644 "$DSYNC_LINUX_UNIT.new"
mv -f "$DSYNC_LINUX_UNIT.new" "$DSYNC_LINUX_UNIT"
if [ -d "$DSYNC_LEGACY_LINUX_UNIT.d" ]; then
  install -d -m 755 "$DSYNC_LINUX_UNIT.d"
  for _ds_f in "$DSYNC_LEGACY_LINUX_UNIT.d"/*.conf; do
    [ -f "\$_ds_f" ] || continue
    sed $sedargs "\$_ds_f" > "$DSYNC_LINUX_UNIT.d/\${_ds_f##*/}"
  done
fi
install -m 0755 "\$tmp/dsync" "$DANTESYNC_LINUX_BIN"
systemctl enable dsync
EOF
}

# dsync_linux_swap_back_sh -> the reverse of the swap, INSIDE a window, with dsync already stopped: the
# dsync files go, the state dir takes its old name back, dantesync is enabled. The start is the caller's,
# after the close.
dsync_linux_swap_back_sh() {
  cat <<EOF
systemctl disable dsync 2>/dev/null || true
rm -rf "$DSYNC_LINUX_UNIT" "$DSYNC_LINUX_UNIT.d" "$DANTESYNC_LINUX_BIN" "$DANTESYNC_LINUX_BAK"
if [ -d "$DSYNC_LINUX_STATE" ] && [ ! -e "$DSYNC_LEGACY_LINUX_STATE" ]; then
  mv "$DSYNC_LINUX_STATE" "$DSYNC_LEGACY_LINUX_STATE"
fi
systemctl enable dantesync
EOF
}

# dsync_linux_finalize_cmd -> the program that removes what the swap kept for the way back, run after
# the node verified (and on a node already on the target, so an interrupted roll is finished by the next
# one). It opens no window when nothing is left, so a roll over a clean fleet touches no root.
dsync_linux_finalize_cmd() {
  cat <<EOF
set -e
if [ ! -e "$DSYNC_LEGACY_LINUX_UNIT" ] && [ ! -e "$DSYNC_LEGACY_LINUX_UNIT.d" ] \\
  && [ ! -e "$DSYNC_LEGACY_LINUX_BIN" ] && [ ! -e "$DSYNC_LEGACY_LINUX_BIN.bak" ]; then
  exit 0
fi
$(_dantesync_linux_rw_window_sh)
trap '_dantesync_remount_ro' EXIT
rm -rf "$DSYNC_LEGACY_LINUX_UNIT" "$DSYNC_LEGACY_LINUX_UNIT.d" "$DSYNC_LEGACY_LINUX_BIN" "$DSYNC_LEGACY_LINUX_BIN.bak"
_dantesync_remount_ro
systemctl daemon-reload
echo "LEGACY OK: the dantesync binary and unit are gone"
EOF
}

# dantesync_linux_upgrade_cmd VERSION [ROLE] -> the remote bash text to upgrade a Linux node to VERSION.
# Safety order: download + sha256-verify FIRST (a bad download never stops the daemon), THEN back up the
# current binary (or, from dantesync, keep it in place), THEN arm a restore-on-error trap (self-heal),
# THEN stop/swap/restart. Any failure PAST the backup point restores the previous state and restarts
# before returning non-zero; a failure BEFORE it leaves the running clock master untouched. ROLE (issue
# 1372): a forced downgrade of the ntp-master below 1.15.0 also deletes its saved fleet date, as the LAST
# step -- after the start, so a failed start self-heals to the 1.15 binary WITH that file.
dantesync_linux_upgrade_cmd() {
  local version="$1" url
  url="$(dantesync_release_url_linux "$version")"
  cat <<EOF
set -e
tmp="\$(mktemp -d)"
trap 'rm -rf "\$tmp"' EXIT
# 1. obtain the new binary BEFORE touching the running clock master. Order (#1077):
#    (a) a pre-staged binary the orchestrator downloaded+verified on dev1 and scp'd here -- the
#        curl-less path (cam3 has no curl + a broken apt) AND the metered-LAN-friendly path (one
#        download on dev1, not eight); (b) on-box curl; (c) on-box wget as a secondary. Whichever
#        path, the sha256 is re-verified below, so a corrupt scp OR download never reaches the
#        clock master. The staged file is consumed (moved into \$tmp) and removed immediately.
if [ -f "$DANTESYNC_LINUX_STAGED" ]; then
  cp -a "$DANTESYNC_LINUX_STAGED" "\$tmp/dsync"
  cp -a "$DANTESYNC_LINUX_STAGED.sha256" "\$tmp/dsync.sha256"
  rm -f "$DANTESYNC_LINUX_STAGED" "$DANTESYNC_LINUX_STAGED.sha256"
elif command -v curl >/dev/null 2>&1; then
  curl --fail -fsSL -o "\$tmp/dsync" "$url"
  curl --fail -fsSL -o "\$tmp/dsync.sha256" "$url.sha256"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "\$tmp/dsync" "$url"
  wget -q -O "\$tmp/dsync.sha256" "$url.sha256"
else
  echo "no staged binary at $DANTESYNC_LINUX_STAGED and neither curl nor wget is available" >&2
  exit 1
fi
expected="\$(awk '{print \$1}' "\$tmp/dsync.sha256")"
actual="\$(sha256sum "\$tmp/dsync" | awk '{print \$1}')"
if [ "\$expected" != "\$actual" ]; then
  echo "SHA256 MISMATCH: expected \$expected got \$actual" >&2
  exit 1
fi
chmod +x "\$tmp/dsync"
# issue 1487: a node still on dantesync takes the rename path (scripts/lib/dsync-swap.sh).
_ds_legacy=0
if [ -f "$DSYNC_LEGACY_LINUX_UNIT" ] && [ ! -f "$DSYNC_LINUX_UNIT" ]; then _ds_legacy=1; fi
# 1.5 cam boxes run a DELIBERATE read-only root (the 2026-08-16 canary failed here with
# 'cp: ... Read-only file system'). The ONE window prologue (dantesync-rollback.sh, issue 1407):
# read the ACTUAL mount state (#1077: findmnt + the /proc/mounts fallback, never a write probe),
# remount rw on a read-only root, and the window helpers. Only writes and the stop run inside it;
# the window closes VERIFIED before the restart, and the EXIT trap closes it on any failure.
$(_dantesync_linux_rw_window_sh)
trap 'rm -rf "\$tmp"; _dantesync_remount_ro' EXIT
# 2. back up the current binary BEFORE overwriting it (rollback target); dantesync stays where it is
if [ "\$_ds_legacy" = 0 ]; then cp -a "$DANTESYNC_LINUX_BIN" "$DANTESYNC_LINUX_BAK"; fi
# 3. self-heal: from here (the point of no return), restore the previous state on ANY error -- in
#    its own rw window, closed verified, so the old daemon comes back up on a read-only root
_dantesync_restore() {
  _dantesync_reopen_rw
  if [ "\$_ds_legacy" = 1 ]; then
    systemctl stop dsync 2>/dev/null || true
    {
$(dsync_linux_swap_back_sh | sed 's/^/      /')
    } || echo "SELF-HEAL FAILED: the swap back to dantesync did not finish -- inspect the unit and $DSYNC_LEGACY_LINUX_STATE by hand" >&2
    _dantesync_remount_ro
    systemctl daemon-reload 2>/dev/null || true
    systemctl restart dantesync 2>/dev/null || true
    echo "SELF-HEAL: restored previous dantesync service" >&2
  elif cp -a "$DANTESYNC_LINUX_BAK" "$DANTESYNC_LINUX_BIN"; then
    _dantesync_remount_ro
    systemctl restart dsync 2>/dev/null || true
    echo "SELF-HEAL: restored previous dsync binary" >&2
  else
    _dantesync_remount_ro
    systemctl restart dsync 2>/dev/null || true
    echo "SELF-HEAL FAILED: the previous binary could NOT be copied back -- dsync runs whatever is installed now; restore $DANTESYNC_LINUX_BAK by hand" >&2
  fi
}
trap '_dantesync_restore' ERR
# 4. swap, close the window (verified, issue 1407), then the restart on the read-only root
if [ "\$_ds_legacy" = 1 ]; then
$(dsync_linux_swap_in_sh | sed 's/^/  /')
else
  systemctl stop dsync
  install -m 0755 "\$tmp/dsync" $DANTESYNC_LINUX_BIN
fi
_dantesync_remount_ro
if [ "\$_ds_legacy" = 1 ]; then systemctl daemon-reload; fi
systemctl restart dsync
# success: re-set the EXIT trap (tmp cleanup + the ro close, a no-op once closed). The ERR self-heal
# stays ARMED through the version read below: a new binary that cannot even print its version is
# rolled back too (the orchestrator then reports the self-heal). It is disarmed right after it, so
# nothing that follows (the master's date-state delete) can roll the master back to its .bak after
# its date file is gone -- the state the issue-1372 delete-last order exists to avoid.
trap 'rm -rf "\$tmp"; _dantesync_remount_ro' EXIT
# 5. read the new version back
dsync --version
trap - ERR
EOF
  _dantesync_linux_date_state_rm_sh "${2:-}" "$version"
}

# dsync_windows_swap_back_fn_ps -> the PowerShell function that reverses the service swap: dsync out,
# the state dir back, dantesync's start type back. The start is the caller's.
dsync_windows_swap_back_fn_ps() {
  cat <<EOF
function Invoke-DsyncSwapBack {
    try { Stop-Service dsync -Force -ErrorAction SilentlyContinue } catch { }
    Wait-Process -Name dsync -Timeout 30 -ErrorAction SilentlyContinue
    sc.exe delete dsync | Out-Null
    if ((Test-Path -LiteralPath '$DSYNC_WIN_STATE') -and -not (Test-Path -LiteralPath '$DSYNC_LEGACY_WIN_STATE')) {
        Rename-Item -LiteralPath '$DSYNC_WIN_STATE' -NewName 'DanteSync'
    }
    Set-Service -Name dantesync -StartupType Automatic
}
EOF
}

# dsync_windows_swap_in_ps -> the PowerShell lines of the service swap before the exe copy. A state dir
# that cannot be renamed (something holds a file in it) is only a named line: dsync then uses it in
# place, which is its documented behaviour, and the clock is not at stake.
dsync_windows_swap_in_ps() {
  cat <<EOF
        \$dsPath = (Get-CimInstance Win32_Service -Filter "Name='dantesync'").PathName
        Stop-Service dantesync
$(dantesync_windows_wait_service_exit_ps dantesync)
        Set-Service -Name dantesync -StartupType Disabled
        Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        try {
            if ((Test-Path -LiteralPath '$DSYNC_LEGACY_WIN_STATE') -and -not (Test-Path -LiteralPath '$DSYNC_WIN_STATE')) {
                Rename-Item -LiteralPath '$DSYNC_LEGACY_WIN_STATE' -NewName 'dsync' -ErrorAction Stop
            }
        } catch {
            Write-Output ('STATE-WARNING: $DSYNC_LEGACY_WIN_STATE kept in place, dsync uses it there: ' + \$_.Exception.Message)
        }
EOF
}

# dsync_windows_new_service_ps -> create the dsync service. Its binary path is the old service's own
# with the exe replaced, so its arguments stay.
dsync_windows_new_service_ps() {
  cat <<EOF
    if (\$dsLegacy) {
        \$dsBin = \$dsPath -ireplace [regex]::Escape('$DSYNC_LEGACY_WIN_EXE'), \$exe
        New-Service -Name dsync -BinaryPathName \$dsBin -DisplayName 'dsync' -StartupType Automatic -Description 'Synchronizes system time with the PTPv1 grandmaster' | Out-Null
    }
EOF
}

# dsync_windows_finalize_ps -> the CONTENT of a .ps1 that removes the old installation after the node
# verified: the service, its autostart entries, the install dir. It also points the HKLM Run entry at the
# dsync tray. It never throws: one `LEGACY OK:` / `LEGACY-WARNING:` line, and the clock is not at stake.
dsync_windows_finalize_ps() {
  cat <<EOF
\$ErrorActionPreference = 'Continue'
\$notes = @()
\$runLm = 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run'
\$runCu = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run'
\$trayExe = '$DANTESYNC_WIN_TRAY_EXE'
if (Test-Path -LiteralPath \$trayExe) {
    try { Set-ItemProperty -Path \$runLm -Name 'DsyncTray' -Value ('"' + \$trayExe + '"') -ErrorAction Stop } catch { \$notes += ('the DsyncTray autostart entry: ' + \$_.Exception.Message) }
}
\$hadLegacy = (Test-Path -LiteralPath '$DSYNC_LEGACY_WIN_DIR') -or [bool](Get-Service -Name dantesync -ErrorAction SilentlyContinue)
if (\$hadLegacy) {
    try {
        Get-Process -Name dantesync-tray -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName 'DanteSyncTray' -Confirm:\$false -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName 'DanteSyncTrayStart' -Confirm:\$false -ErrorAction SilentlyContinue
        Remove-ItemProperty -Path \$runLm -Name 'DanteSyncTray' -ErrorAction SilentlyContinue
        Remove-ItemProperty -Path \$runCu -Name 'DanteSyncTray' -ErrorAction SilentlyContinue
        Remove-Item -Path "\$env:ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\DanteSync.lnk" -Force -ErrorAction SilentlyContinue
        Remove-Item -Path 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DanteSync' -Recurse -Force -ErrorAction SilentlyContinue
        if (Get-Service -Name dantesync -ErrorAction SilentlyContinue) { sc.exe delete dantesync | Out-Null }
        if (Test-Path -LiteralPath '$DSYNC_LEGACY_WIN_DIR') { Remove-Item -LiteralPath '$DSYNC_LEGACY_WIN_DIR' -Recurse -Force -ErrorAction SilentlyContinue }
        if (Test-Path -LiteralPath '$DSYNC_LEGACY_WIN_DIR') { \$notes += '$DSYNC_LEGACY_WIN_DIR could not be removed' }
    } catch {
        \$notes += ('the old installation: ' + \$_.Exception.Message)
    }
}
if (\$notes.Count -gt 0) {
    Write-Output ('LEGACY-WARNING: ' + (\$notes -join '; '))
} elseif (\$hadLegacy) {
    Write-Output 'LEGACY OK: the dantesync service and install dir are gone'
}
EOF
}

# dantesync_windows_upgrade_ps VERSION [ROLE] -> the CONTENT of a PowerShell .ps1 that upgrades a Windows
# node to VERSION. Sent as a FILE (scp -O) and run with `-File` -- never a nested
# `powershell -Command "..."` over ssh (which fails SILENTLY, .claude/rules/rig-state-inspection.md
# §2). Same safety order as Linux: download + Get-FileHash-verify FIRST, back up the exe (from dantesync:
# leave the old install where it is), then a try/catch self-heal around stop/swap/start, purge the dead
# relic task, read the version back. Issue 1372: the tray rides the same program -- fetched + verified
# with the service binary before any stop (dantesync_windows_tray_fetch_ps), swapped after the service
# is back (dantesync_windows_tray_swap_ps); a tray failure prints TRAY-WARNING and never throws, so it
# can never roll back or block the service. ROLE (issue 1372): a forced downgrade of the ntp-master
# below 1.15.0 deletes its saved fleet date right after the service try/catch, whose catch rethrows.
dantesync_windows_upgrade_ps() {
  local version="$1" url
  url="$(dantesync_release_url_windows "$version")"
  cat <<EOF
\$ErrorActionPreference = 'Stop'
\$url = '$url'
\$exe = '$DANTESYNC_WIN_EXE'
\$bak = '$DANTESYNC_WIN_BAK'
\$tmp = Join-Path \$env:TEMP 'dsync-new.exe'
# 1. download + sha256-verify BEFORE touching the running clock master
Invoke-WebRequest -UseBasicParsing -Uri \$url -OutFile \$tmp
Invoke-WebRequest -UseBasicParsing -Uri (\$url + '.sha256') -OutFile (\$tmp + '.sha256')
\$expected = ((Get-Content (\$tmp + '.sha256')) -split '\s+')[0].Trim()
\$actual = (Get-FileHash -Algorithm SHA256 \$tmp).Hash
if (\$expected -ne \$actual) { throw ('SHA256 MISMATCH expected ' + \$expected + ' got ' + \$actual) }
$(dantesync_windows_tray_fetch_ps "$version")
# issue 1487: a node still on dantesync takes the rename path (scripts/lib/dsync-swap.sh)
\$dsLegacy = [bool](Get-Service -Name dantesync -ErrorAction SilentlyContinue) -and -not [bool](Get-Service -Name dsync -ErrorAction SilentlyContinue)
# 2. back up the current exe BEFORE overwriting it; dantesync stays where it is
if (\$dsLegacy) { New-Item -ItemType Directory -Force -Path (Split-Path \$exe) | Out-Null } else { Copy-Item -Force \$exe \$bak }
$(dsync_windows_swap_back_fn_ps)
# 3. self-heal: any failure during stop/swap/start restores the previous state and restarts before rethrow
try {
    if (\$dsLegacy) {
$(dsync_windows_swap_in_ps)
    } else {
    Stop-Service dsync
$(dantesync_windows_wait_service_exit_ps)
    }
    Copy-Item -Force \$tmp \$exe
$(dsync_windows_new_service_ps)
    Start-Service dsync
} catch {
    # A locked-exe IOException terminates despite -ErrorAction: never let it skip the start (12.10.2026).
    if (\$dsLegacy) {
        try { Invoke-DsyncSwapBack } catch { Write-Output ('SELF-HEAL: the swap back failed: ' + \$_.Exception.Message) }
        try { Start-Service dantesync } catch { Write-Output ('SELF-HEAL: Start-Service failed: ' + \$_.Exception.Message) }
        throw
    }
    try { Copy-Item -Force \$bak \$exe } catch { Write-Output ('SELF-HEAL: restore of the .bak failed: ' + \$_.Exception.Message) }
    try { Start-Service dsync } catch { Write-Output ('SELF-HEAL: Start-Service failed: ' + \$_.Exception.Message) }
    throw
}
EOF
  _dantesync_windows_date_state_rm_ps "${2:-}" "$version"
  cat <<EOF
# 4. purge the dead DanteSyncUpdate relic task -- genuinely idempotent: routed through cmd /c
# with full redirection, because a bare schtasks on an ALREADY-ABSENT task writes to stderr and
# \$ErrorActionPreference=Stop turns that into a terminating NativeCommandError AFTER the swap
# (live 2026-08-16 v1.8.43 canary: swap completed, ps1 exited non-zero, orchestrator misreported
# a failed upgrade).
cmd /c "schtasks /Delete /TN \"$DANTESYNC_DEAD_TASK\" /F >nul 2>&1"
$(dantesync_windows_tray_swap_ps)
& \$exe --version
EOF
}
