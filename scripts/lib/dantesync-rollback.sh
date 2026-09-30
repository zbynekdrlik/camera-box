#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions, no top-level statements) -- sourced by
# scripts/dantesync-fleet-upgrade.sh, whose own `set -euo pipefail` applies; a `set` here would leak
# into the caller (the scripts/lib convention).
#
# scripts/lib/dantesync-rollback.sh -- the fleet roll's ROLLBACK programs: the remote bash a Linux
# node runs and the `.ps1` a Windows node runs to restore the pre-upgrade dantesync binary from its
# `.bak` and start the service again. The orchestrator runs them ONLY on the VERIFY-failure path
# (the swap provably completed, dantesync-fleet-upgrade.sh rollback_node). They moved here from the
# upgrade script unchanged (the #876 / #1077 / #1265 contracts), so that script stays under its
# ~1000-line budget (issue 1372; the tray arm's lib is the precedent).
#
# DEPENDS (resolved at CALL time, all from dantesync-fleet-upgrade.sh): DANTESYNC_LINUX_BIN /
# DANTESYNC_LINUX_BAK, DANTESYNC_WIN_EXE / DANTESYNC_WIN_BAK, dantesync_windows_wait_service_exit_ps.

# dantesync_linux_rollback_cmd -> restore the pre-upgrade binary from its .bak and restart. Only
# ever invoked by the orchestrator on the VERIFY-failure path (the swap provably completed).
dantesync_linux_rollback_cmd() {
  cat <<EOF
set -e
if [ ! -f "$DANTESYNC_LINUX_BAK" ]; then
  echo "no $DANTESYNC_LINUX_BAK to roll back to" >&2
  exit 1
fi
# Same read-only-root handling as the upgrade cmd (cam boxes) — restore ro on ANY exit. #1077
# defect (3): read the real mount state (findmnt, /proc/mounts fallback), never a write probe —
# mirrors setup-device.sh's ensure_root_writable() (#599); 'ro' as the FIRST comma-token.
ro_root=0
opts="\$(findmnt -no OPTIONS / 2>/dev/null || awk '\$2=="/"{print \$4; exit}' /proc/mounts 2>/dev/null)"
case "\$opts" in ro | ro,*) ro_root=1 ;; esac
if [ "\$ro_root" = 1 ]; then mount -o remount,rw /; fi
_dantesync_remount_ro() {
  if [ "\$ro_root" = 1 ]; then mount -o remount,ro / 2>/dev/null || true; fi
}
trap '_dantesync_remount_ro' EXIT
systemctl stop dantesync
cp -a "$DANTESYNC_LINUX_BAK" $DANTESYNC_LINUX_BIN
systemctl restart dantesync
dantesync --version
EOF
}

# dantesync_windows_rollback_ps -> the CONTENT of a .ps1 that restores the pre-upgrade exe from
# its .bak and starts the service. Orchestrator-invoked only on the VERIFY-failure path.
dantesync_windows_rollback_ps() {
  cat <<EOF
\$ErrorActionPreference = 'Stop'
\$exe = '$DANTESYNC_WIN_EXE'
\$bak = '$DANTESYNC_WIN_BAK'
if (-not (Test-Path \$bak)) { throw 'no dantesync.exe.bak to roll back to' }
Stop-Service dantesync
$(dantesync_windows_wait_service_exit_ps)
Copy-Item -Force \$bak \$exe
Start-Service dantesync
& \$exe --version
EOF
}
