#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + three constants, no other top-level
# statements) -- sourced by scripts/dantesync-fleet-upgrade.sh, whose own `set -euo pipefail`
# applies; a `set` here would leak into the caller (the scripts/lib convention).
#
# scripts/lib/dantesync-rollback.sh -- the fleet roll's ROLLBACK programs: the remote bash a Linux
# node runs and the `.ps1` a Windows node runs to restore the pre-upgrade dantesync binary from its
# `.bak` and start the service again. The orchestrator runs them ONLY on the VERIFY-failure path
# (the swap provably completed, dantesync-fleet-upgrade.sh rollback_node). They moved here from the
# upgrade script unchanged (the #876 / #1077 / #1265 contracts), so that script stays under its
# ~1000-line budget (issue 1372; the tray arm's lib is the precedent).
#
# THE DATE STATE (issue 1372, dantesync 1.15.0 = dantesync issue 126): dantesync 1.15.0 persists the
# NTP master's fleet date offset in date-offset.json beside config.json and restores it at start, so
# a master restart keeps the fleet date. A master rolled BACK to an older build must delete that file
# in the same step: the old build neither reads nor updates it, and a 1.15 reinstalled within a day
# would restore the session from before the rollback (a stale D). So a rollback that restores a
# version below 1.15.0 on the fleet's ntp-master deletes it (dantesync_rollback_clears_date_state).
# A forced DOWNGRADE of the master below 1.15.0 (upgrade_node's OLDER + --force) is a rollback in all
# but path, so the upgrade programs apply the same rule on (role, target) -- supervisor decision,
# issue 1372 comment 5910080606. It never happens on a slave (followers keep no file), or to 1.15.0
# or newer (the file is valid there; deleting it would boot-step the fleet during the day).
# WHERE: the rollback programs delete after the stop + restore and before the start; the downgrade
# programs delete after the START, because they self-heal to the `.bak` -- in a downgrade the 1.15
# binary -- on a failed start, and it must come back with its saved date (issue comment 5910147397).
#
# DEPENDS (resolved at CALL time, all from dantesync-fleet-upgrade.sh): DANTESYNC_LINUX_BIN /
# DANTESYNC_LINUX_BAK, DANTESYNC_WIN_EXE / DANTESYNC_WIN_BAK, dantesync_windows_wait_service_exit_ps,
# dantesync_upgrade_status (the script's one version ordering), dantesync_is_ntp_master.
#
# THE READ-ONLY ROOT (issue 1407): a cambox runs dantesync on a read-only root, so both Linux programs
# open a `mount -o remount,rw /` window for their writes. They share ONE window prologue
# (_dantesync_linux_rw_window_sh): only file writes and the service STOP run inside it, the window is
# closed with the ONE verified ro close (scripts/lib/ro-window.sh: the root mode is READ, a root left
# writable exits 1 naming the writers), and the dantesync (re)start runs only AFTER that close. A
# later write (the self-heal's .bak copy, the date-state delete) reopens its own window and closes it
# the same way. A node whose root is read-write (strih-lx, dev1) never opens a window at all.

# The ONE verified ro close (lazy-sourced: dantesync-fleet-upgrade.sh sources this lib).
command -v ro_window_close_cmds >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/ro-window.sh"

# The first dantesync release that persists the master's fleet date offset (dantesync issue 126).
DANTESYNC_DATE_STATE_FIRST_VERSION='1.15.0'
# Where it lives: dantesync src/main.rs DATE_STATE_PATH, per OS.
DANTESYNC_LINUX_DATE_STATE='/etc/dantesync/date-offset.json'
DANTESYNC_WIN_DATE_STATE='C:\ProgramData\DanteSync\date-offset.json'

# dantesync_rollback_clears_date_state ROLE RESTORED_VERSION -> 0 iff the rollback must delete the
# persisted date state: ROLE is the fleet's `ntp-master` (the date master, the dantesync-fleet.sh
# role name) AND RESTORED_VERSION -- the version the rollback puts back -- is older than 1.15.0 by the
# script's own dantesync_upgrade_status (numeric, so 1.9.0 is older). 1 otherwise, including an empty
# or unread RESTORED_VERSION (never a guess either way). A forced downgrade passes its TARGET here.
dantesync_rollback_clears_date_state() {
  [ "${1:-}" = ntp-master ] || return 1
  [ "$(dantesync_upgrade_status "${2:-}" "$DANTESYNC_DATE_STATE_FIRST_VERSION")" = NEWER ]
}

# dantesync_date_role NAME MASTER_NAME -> `ntp-master` when NAME is the roll's NTP master, named the
# way verify_node names it (dantesync_is_ntp_master against NTP_MASTER), else `slave`: the ROLE both
# the rollback and the upgrade (downgrade) programs hand to the rule above.
dantesync_date_role() {
  if dantesync_is_ntp_master "${1:-}" "${2:-}"; then printf 'ntp-master'; else printf 'slave'; fi
}

# _dantesync_linux_rw_window_sh -> the remote bash lines both Linux programs open their read-only
# root window with (issue 1407): read the root mode (findmnt, the /proc/mounts fallback) through the
# ONE first-token reading, ro_root_mount_mode (its definition emitted here; `ro` as the FIRST
# comma-token, #599/#1077), remount rw on a read-only root, and define the two window helpers:
#   _dantesync_remount_ro -- the ONE verified ro close, at most once per open window (the normal
#     path and the EXIT trap may both call it); a root left writable exits 1 naming the writers, so
#     the dantesync (re)start after it never runs on a writable root.
#   _dantesync_reopen_rw -- reopen the window for a later write (the self-heal's .bak copy, the date
#     state delete). It never fails the program: a refused remount leaves that write to fail by name.
_dantesync_linux_rw_window_sh() {
  printf '%s\n' "$(declare -f ro_root_mount_mode)"
  cat <<'EOF'
ro_root=0
opts="$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null)"
case "$(ro_root_mount_mode "$opts")" in ro) ro_root=1 ;; esac
if [ "$ro_root" = 1 ]; then mount -o remount,rw /; fi
_ds_rw_open=$ro_root
_dantesync_remount_ro() {
  [ "$_ds_rw_open" = 1 ] || return 0
  _ds_rw_open=0
EOF
  ro_window_close_cmds "issue 1407" "\$(hostname 2>/dev/null || echo this node)" \
    "No dantesync start runs on a writable root; dantesync is '\$(systemctl show -p ActiveState --value dantesync 2>/dev/null || true)' now." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then start dantesync there by hand if it is not running."
  cat <<'EOF'
}
_dantesync_reopen_rw() {
  [ "$ro_root" = 1 ] && [ "$_ds_rw_open" != 1 ] || return 0
  if mount -o remount,rw /; then _ds_rw_open=1; else echo "WARNING: could not remount / read-write again" >&2; fi
}
EOF
}

# _dantesync_linux_date_state_rm_sh ROLE VERSION -> the remote bash lines that delete the persisted
# date state, or nothing when the rule says no (VERSION = the restored one, or a downgrade's target).
# The caller places them after the service stop (the 1.15 master rewrites the file on its loop). The
# delete runs in its own read-only-root window (issue 1407): the window is reopened when the caller
# already closed it (the downgrade deletes after the start) and closed verified after. A file that
# cannot be removed is a named WARNING, never an exit: the clock master is never left stopped over it.
_dantesync_linux_date_state_rm_sh() {
  local f="$DANTESYNC_LINUX_DATE_STATE" why
  dantesync_rollback_clears_date_state "${1:-}" "${2:-}" || return 0
  why="(rollback below $DANTESYNC_DATE_STATE_FIRST_VERSION, dantesync issue 126)"
  cat <<EOF
# issue 1372: this date master goes back below $DANTESYNC_DATE_STATE_FIRST_VERSION, so the saved fleet date of its 1.15 session goes too.
_dantesync_reopen_rw
if [ -e "$f" ]; then
  rm -f "$f" || true
  if [ -e "$f" ]; then
    echo "WARNING: date-offset.json could NOT be removed from $f $why -- remove it by hand before any 1.15 reinstall"
  else
    echo "date-offset.json removed $why"
  fi
else
  echo "date-offset.json absent, nothing to remove $why"
fi
_dantesync_remount_ro
EOF
}

# _dantesync_windows_date_state_rm_ps ROLE VERSION -> the .ps1 lines that delete the
# persisted date state, or nothing when the rule says no. Remove-Item runs with -ErrorAction
# SilentlyContinue inside a try/catch: under the program's $ErrorActionPreference = 'Stop' some
# failures still THROW (a prompt in a non-interactive session), and a throw here would skip the rest
# of the program (a rollback's Start-Service). The result is read back with Test-Path and named.
_dantesync_windows_date_state_rm_ps() {
  local why
  dantesync_rollback_clears_date_state "${1:-}" "${2:-}" || return 0
  why="(rollback below $DANTESYNC_DATE_STATE_FIRST_VERSION, dantesync issue 126)"
  cat <<EOF
# issue 1372: this date master goes back below $DANTESYNC_DATE_STATE_FIRST_VERSION, so the saved fleet date of its 1.15 session goes too.
\$dateState = '$DANTESYNC_WIN_DATE_STATE'
if (Test-Path -LiteralPath \$dateState) {
    \$dateErr = ''
    try {
        Remove-Item -LiteralPath \$dateState -Force -ErrorAction SilentlyContinue
    } catch {
        \$dateErr = ': ' + \$_.Exception.Message
    }
    if (Test-Path -LiteralPath \$dateState) {
        Write-Output ('WARNING: date-offset.json could NOT be removed from ' + \$dateState + ' $why -- remove it by hand before any 1.15 reinstall' + \$dateErr)
    } else {
        Write-Output 'date-offset.json removed $why'
    }
} else {
    Write-Output 'date-offset.json absent, nothing to remove $why'
}
EOF
}

# dantesync_linux_rollback_cmd [ROLE RESTORED_VERSION] -> restore the pre-upgrade binary from its
# .bak and restart. Only ever invoked by the orchestrator on the VERIFY-failure path (the swap
# provably completed). ROLE / RESTORED_VERSION (issue 1372) add the date-state delete between the
# restore and the restart when dantesync_rollback_clears_date_state says so; without them, or when it
# says no, the program is exactly the plain rollback.
dantesync_linux_rollback_cmd() {
  cat <<EOF
set -e
if [ ! -f "$DANTESYNC_LINUX_BAK" ]; then
  echo "no $DANTESYNC_LINUX_BAK to roll back to" >&2
  exit 1
fi
# Same read-only-root window as the upgrade cmd (cam boxes), issue 1407: the stop + the restore
# inside it, the verified close, the restart after it; the EXIT trap closes it on any failure.
# #1077 defect (3): the root mode is the real mount state (findmnt, /proc/mounts fallback).
$(_dantesync_linux_rw_window_sh)
trap '_dantesync_remount_ro' EXIT
systemctl stop dantesync
cp -a "$DANTESYNC_LINUX_BAK" $DANTESYNC_LINUX_BIN
EOF
  _dantesync_linux_date_state_rm_sh "${1:-}" "${2:-}"
  cat <<EOF
_dantesync_remount_ro
systemctl restart dantesync
dantesync --version
EOF
}

# dantesync_windows_rollback_ps [ROLE RESTORED_VERSION] -> the CONTENT of a .ps1 that restores the
# pre-upgrade exe from its .bak and starts the service. Orchestrator-invoked only on the
# VERIFY-failure path. ROLE / RESTORED_VERSION (issue 1372): the date-state delete between the
# restore and Start-Service, as for Linux.
dantesync_windows_rollback_ps() {
  cat <<EOF
\$ErrorActionPreference = 'Stop'
\$exe = '$DANTESYNC_WIN_EXE'
\$bak = '$DANTESYNC_WIN_BAK'
if (-not (Test-Path \$bak)) { throw 'no dantesync.exe.bak to roll back to' }
Stop-Service dantesync
$(dantesync_windows_wait_service_exit_ps)
Copy-Item -Force \$bak \$exe
EOF
  _dantesync_windows_date_state_rm_ps "${1:-}" "${2:-}"
  cat <<EOF
Start-Service dantesync
& \$exe --version
EOF
}

# dantesync_rollback_date_state_note NAME OUTPUT -> relay the rollback program's date-state line
# (removed / absent / WARNING) to the roll log as `[NAME] <line>`; nothing when OUTPUT has none. The
# orchestrator prints a rollback's whole output only when the rollback failed, so without this a
# successful rollback would drop the line. Always returns 0 (a bare statement under set -e).
dantesync_rollback_date_state_note() {
  local line
  while IFS= read -r line; do
    line="${line%$'\r'}"
    case "$line" in
      *date-offset.json*) printf '[%s] %s\n' "${1:-}" "$line" ;;
    esac
  done <<<"${2:-}"
  return 0
}
