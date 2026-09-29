#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh and the
# Tier-0 harness), and strict mode here would leak into both.
#
# scripts/lib/strih-dantesync.sh -- issue 1372: setup-strih step 2's dantesync install, which restarts
# the fleet dantesync DATE MASTER only on a real change, and the unit grade verify-strih item 6c uses.
# Its own lib (the strih-drm-output.sh precedent) so setup-strih.sh stays under its 1000-line budget;
# sourced by setup-strih.sh AND verify-strih.sh. The unit text (strih_dantesync_unit_text) and the pure
# restart decision (strih_dantesync_restart_decision) live in scripts/lib/strih-provision.sh.
#
# WHY: strih-lx is the fleet's dantesync date master in daily mode. Its fleet line drifts ~0.7 s a day
# against NTP and is stepped only in the nightly 02:00Z window. Step 2 used to rewrite the unit,
# remove /var/run/dantesync.lock and restart the dantesync unit on EVERY run, and every strih-lx
# genlock deploy runs setup-strih -- so a deploy stepped the whole rig's date mid-day (29.9.2026
# 00:35Z: a 0.67 s fleet step, cg OBS program audio broken until a relaunch).
#
# Pure helpers (tests/python/test_strih_dantesync_keep_running_1372.py):
#   strih_dantesync_unit_matches UNIT_TEXT PATH   0 identical | 1 differs or missing | 2 unreadable
#   strih_dantesync_dropins_present DIR           0 iff DIR holds a *.conf drop-in
#   strih_dantesync_unit_verdict WANT PATH DIR NEED_RELOAD
#                                                 ok | differs | dropin | not-loaded | unreadable
#
# Provisioning ACTION (root, setup-strih.sh step 2; uses the caller's warn):
#   strih_dantesync_install UNIT_TEXT [ROLE]
#     * a change = the unit text differs from the installed file, a stale *.conf drop-in existed and
#       was removed, or systemd has not loaded the unit on disk (NeedDaemonReload=yes: a daemon-reload
#       that failed, or a run killed after the write). Only then is the unit written (temp + rename)
#       and daemon-reloaded;
#     * strih_dantesync_restart_decision picks restart | start | keep | absent from that change,
#       binary_changed, `systemctl is-active` and the binary's presence; the restart runs on the
#       RELOADED unit;
#     * fail closed: an unreadable unit or a missing decision helper touches nothing, never a restart;
#     * keep has no side effects: `systemctl enable` (which reloads the manager) runs only when the
#       unit is not enabled yet;
#     * the lock is removed ONLY on `start` and only when no process holds it (`flock -n`). It is an
#       flock: a stale file never blocks a start, and removing the file of a RUNNING dantesync (one
#       outside the unit) would let a second instance lock a new inode;
#     * one log line names the decision.
#   binary_changed is always 0: setup-strih never installs the binary and the box keeps no checksum
#   marker -- a new binary arrives only through dantesync-fleet-upgrade.sh, which restarts the daemon
#   itself (the deliberate, canaried path).
#   The STRIH_DANTESYNC_* paths are test seams (the pytest runs this against a temp root with a fake
#   systemctl on PATH); they default to the real box paths.

# strih_dantesync_unit_matches UNIT_TEXT PATH -> 0 iff PATH holds exactly UNIT_TEXT plus its one
# trailing newline -- the bytes step 2 writes (`printf '%s\n' "$UNIT_TEXT"`, UNIT_TEXT being
# strih_dantesync_unit_text captured by `$(...)`, which strips that newline). Byte for byte, so a
# missing/extra trailing newline or any edited line is a difference (1). A missing PATH is 1. A read
# error is cmp's own 2, never reported as a difference: a caller must not restart on it.
strih_dantesync_unit_matches() {
  local text="${1-}" path="${2-}"
  [ -n "$path" ] && [ -f "$path" ] || return 1
  # process substitution, not a pipe: the writer's status never reaches a caller's pipefail.
  cmp -s <(printf '%s\n' "$text") "$path"
}

# strih_dantesync_dropins_present DIR -> 0 iff DIR holds at least one `*.conf` drop-in (the only
# files systemd reads from a unit's `.d` dir). A missing or empty DIR, or other files, is 1.
strih_dantesync_dropins_present() {
  local dir="${1-}"
  [ -n "$dir" ] || return 1
  compgen -G "${dir}/*.conf" >/dev/null
}

# strih_dantesync_unit_verdict WANT_TEXT UNIT_PATH DROPIN_DIR NEED_RELOAD -> ONE token, rc 0 only on
# ok. NEED_RELOAD = `systemctl show -p NeedDaemonReload --value dantesync` (an unread value is no fault).
#   unreadable  the unit could not be read -- never reported as a difference
#   differs     the unit is missing or differs from WANT_TEXT
#   dropin      a *.conf drop-in overrides it
#   not-loaded  it matches on disk but systemd has not loaded it (a daemon-reload is pending)
#   ok          matches, no drop-in, loaded
strih_dantesync_unit_verdict() {
  local want="${1-}" path="${2-}" dir="${3-}" need="${4-}" rc=0
  strih_dantesync_unit_matches "$want" "$path" || rc=$?
  case "$rc" in
    0) ;;
    1) printf 'differs'; return 1 ;;
    *) printf 'unreadable'; return 1 ;;
  esac
  if strih_dantesync_dropins_present "$dir"; then printf 'dropin'; return 1; fi
  if [ "$need" = yes ]; then printf 'not-loaded'; return 1; fi
  printf 'ok'
}

strih_dantesync_install() {
  local unit_text="${1-}" role="${2-}"
  local unit="${STRIH_DANTESYNC_UNIT:-/etc/systemd/system/dantesync.service}"
  local dropin_dir="${STRIH_DANTESYNC_DROPIN_DIR:-${unit}.d}"
  local lock="${STRIH_DANTESYNC_LOCK:-/var/run/dantesync.lock}"
  local bin="${STRIH_DANTESYNC_BIN:-/usr/local/bin/dantesync}"
  local text_changed=0 dropin_removed=0 unit_changed=0 binary_changed=0 active=0 present=0
  local decision reason="" tail need_reload rc=0 tmp
  [ -n "$unit_text" ] || { echo "strih_dantesync_install: empty unit text" >&2; return 1; }
  declare -F strih_dantesync_restart_decision >/dev/null || {
    echo "strih_dantesync_install: strih_dantesync_restart_decision is not loaded (scripts/lib/strih-provision.sh) -- nothing touched" >&2
    return 1
  }

  # Read BEFORE any write (after our own write it would read yes for that reason alone).
  need_reload="$(systemctl show -p NeedDaemonReload --value dantesync 2>/dev/null || true)"
  strih_dantesync_unit_matches "$unit_text" "$unit" || rc=$?
  case "$rc" in
    0) ;;
    1)
      text_changed=1
      if [ -f "$unit" ]; then reason="unit changed"; else reason="unit installed"; fi
      ;;
    *)
      echo "strih_dantesync_install: cannot read ${unit} (rc ${rc}) -- nothing touched, dantesync left as it runs" >&2
      return 1
      ;;
  esac
  if [ "$text_changed" = 0 ] && [ "$need_reload" = yes ]; then reason="unit not loaded"; fi
  # A stale drop-in (the live box had a hand 10-ntp-master.conf resetting ExecStart) would hide the
  # role that is now IN the unit -- remove it; that is a change only when a drop-in actually existed.
  if strih_dantesync_dropins_present "$dropin_dir"; then
    rm -f "$dropin_dir"/*.conf || { echo "cannot remove the dantesync drop-ins in $dropin_dir" >&2; return 1; }
    dropin_removed=1
    reason="${reason:+$reason + }drop-in removed"
  fi
  rmdir "$dropin_dir" 2>/dev/null || true
  if [ "$text_changed" = 1 ]; then
    # temp + rename in the unit's own dir: systemd never reads a half-written unit.
    tmp="$(mktemp "$(dirname "$unit")/.dantesync.service.XXXXXX")" \
      || { echo "cannot create a temp file next to $unit" >&2; return 1; }
    if ! { printf '%s\n' "$unit_text" > "$tmp" && chmod 0644 "$tmp" && mv -f "$tmp" "$unit"; }; then
      rm -f "$tmp"
      echo "cannot write $unit" >&2
      return 1
    fi
  fi
  if [ "$text_changed" = 1 ] || [ "$dropin_removed" = 1 ] || [ "$need_reload" = yes ]; then
    unit_changed=1
    systemctl daemon-reload || {
      echo "systemctl daemon-reload failed -- the unit on disk is not loaded; the next run reloads and restarts on it" >&2
      return 1
    }
  fi
  # `systemctl enable` reloads the manager: only when not enabled yet, so keep has no side effects.
  systemctl is-enabled -q dantesync 2>/dev/null || systemctl enable dantesync 2>/dev/null || true
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
        tail="this NTP server re-derives the date now (on the fleet DATE MASTER the whole rig steps) -- schedule unit changes for the nightly window"
      else
        tail="this box re-syncs its date now"
      fi
      echo "  dantesync.service: RESTARTED (${reason:-changed}) -- ${tail}"
      systemctl restart dantesync 2>/dev/null \
        || warn "  dantesync.service failed to restart -- check journalctl -u dantesync"
      ;;
    start)
      if [ ! -e "$lock" ]; then
        :
      elif flock -n "$lock" true 2>/dev/null; then
        rm -f "$lock" 2>/dev/null || true
      else
        warn "  a process holds ${lock} (a dantesync outside the unit?), or flock could not probe it -- the lock is kept"
      fi
      echo "  dantesync.service: started (was not active${reason:+; $reason})"
      systemctl start dantesync 2>/dev/null \
        || warn "  dantesync.service failed to start -- check journalctl -u dantesync"
      ;;
    absent)
      warn "  dantesync.service unit in place + enabled but NOT started (binary absent) -- install ${bin} then: systemctl start dantesync"
      ;;
  esac
}
