#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh and the
# Tier-0 harness), and strict mode here would leak into both.
#
# scripts/lib/strih-dantesync.sh -- issue 1372: setup-strih step 2's dantesync install ACTION, which
# restarts the fleet dantesync DATE MASTER only on a real change. Its own lib (the strih-drm-output.sh
# precedent) so setup-strih.sh stays under its 1000-line budget; the PURE parts it runs live next to
# the other dantesync helpers in scripts/lib/strih-provision.sh (sourced by setup-strih.sh AND
# verify-strih.sh):
#   strih_dantesync_unit_text ROLE [ARGS]                    the unit text for the box's role
#   strih_dantesync_unit_matches UNIT_TEXT PATH              byte-exact compare with the installed unit
#   strih_dantesync_dropins_present DIR                      a *.conf drop-in exists
#   strih_dantesync_restart_decision UNIT_CHANGED BINARY_CHANGED ACTIVE PRESENT
#                                                            restart | start | keep | absent
#
# WHY: strih-lx is the fleet's dantesync date master in daily mode. Its fleet line drifts ~0.7 s a day
# against NTP and is stepped only in the nightly 02:00Z window. Step 2 used to rewrite the unit,
# remove /var/run/dantesync.lock and restart the dantesync unit on EVERY run, and every strih-lx
# genlock deploy runs setup-strih -- so a deploy stepped the whole rig's date mid-day (29.9.2026
# 00:35Z: a 0.67 s fleet step, cg OBS program audio broken until a relaunch).
#
# Provisioning ACTION (root, setup-strih.sh step 2; uses the caller's warn):
#   strih_dantesync_install UNIT_TEXT [ROLE]
#     * rewrites the unit (and daemon-reloads) only when its text differs from the installed file;
#       removing a stale dantesync.service.d/*.conf drop-in counts as a change only when one existed;
#     * picks restart | start | keep | absent through strih_dantesync_restart_decision from
#       unit_changed, binary_changed, `systemctl is-active` and the binary's presence;
#     * clears the lock ONLY on `start` (the daemon is not active -- the lock is an flock, so removing
#       the file of a RUNNING daemon would let a second instance lock a new inode);
#     * logs one line naming the decision.
#   binary_changed is always 0: setup-strih never installs the binary and the box keeps no checksum
#   marker -- a new binary arrives only through dantesync-fleet-upgrade.sh, which restarts the daemon
#   itself (the deliberate, canaried path).
#   The STRIH_DANTESYNC_* paths are test seams (tests/python/test_strih_dantesync_keep_running_1372.py
#   runs this against a temp root with a fake systemctl on PATH); they default to the real box paths.

strih_dantesync_install() {
  local unit_text="${1-}" role="${2-}"
  local unit="${STRIH_DANTESYNC_UNIT:-/etc/systemd/system/dantesync.service}"
  local dropin_dir="${STRIH_DANTESYNC_DROPIN_DIR:-${unit}.d}"
  local lock="${STRIH_DANTESYNC_LOCK:-/var/run/dantesync.lock}"
  local bin="${STRIH_DANTESYNC_BIN:-/usr/local/bin/dantesync}"
  local text_changed=0 dropin_removed=0 unit_changed=0 binary_changed=0 active=0 present=0
  local decision reason="" tail
  [ -n "$unit_text" ] || { echo "strih_dantesync_install: empty unit text" >&2; return 1; }

  if ! strih_dantesync_unit_matches "$unit_text" "$unit"; then
    text_changed=1
    if [ -f "$unit" ]; then reason="unit changed"; else reason="unit installed"; fi
  fi
  # A stale drop-in (the live box had a hand 10-ntp-master.conf resetting ExecStart) would hide the
  # role that is now IN the unit -- remove it; that is a change only when a drop-in actually existed.
  if strih_dantesync_dropins_present "$dropin_dir"; then
    rm -f "$dropin_dir"/*.conf || { echo "cannot remove the dantesync drop-ins in $dropin_dir" >&2; return 1; }
    dropin_removed=1
    reason="${reason:+$reason + }drop-in removed"
  fi
  rmdir "$dropin_dir" 2>/dev/null || true
  if [ "$text_changed" = 1 ]; then
    printf '%s\n' "$unit_text" > "$unit" || { echo "cannot write $unit" >&2; return 1; }
  fi
  if [ "$text_changed" = 1 ] || [ "$dropin_removed" = 1 ]; then
    unit_changed=1
    systemctl daemon-reload || { echo "systemctl daemon-reload failed" >&2; return 1; }
  fi
  systemctl enable dantesync 2>/dev/null || true
  if systemctl is-active --quiet dantesync; then active=1; fi
  if [ -x "$bin" ]; then present=1; fi
  decision="$(strih_dantesync_restart_decision "$unit_changed" "$binary_changed" "$active" "$present")" \
    || { echo "strih_dantesync_restart_decision refused its inputs" >&2; return 1; }

  case "$decision" in
    keep)
      echo "  dantesync.service: kept running (unit unchanged) -- no restart, the fleet date is untouched"
      ;;
    restart)
      if [ "$role" = server ]; then
        tail="the fleet DATE MASTER re-derives the date now and the whole rig steps -- schedule unit changes for the nightly window"
      else
        tail="this box re-syncs its date now"
      fi
      echo "  dantesync.service: RESTARTED (${reason:-changed}) -- ${tail}"
      systemctl restart dantesync 2>/dev/null \
        || warn "  dantesync.service failed to restart -- check journalctl -u dantesync"
      ;;
    start)
      rm -f "$lock" 2>/dev/null || true
      echo "  dantesync.service: started (was not active${reason:+; $reason}) -- stale lock cleared"
      systemctl start dantesync 2>/dev/null \
        || warn "  dantesync.service failed to start -- check journalctl -u dantesync"
      ;;
    absent)
      warn "  dantesync.service installed + enabled but NOT started (binary absent) -- install ${bin} then: systemctl start dantesync"
      ;;
  esac
}
