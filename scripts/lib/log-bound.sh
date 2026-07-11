#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by verify-device.sh /
# setup-device.sh / create-usb-linux.sh — mirrors every sibling in scripts/lib/ (e.g.
# timesync-authority.sh, ndi-alive.sh), none of which set -euo pipefail either: sourcing a
# `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/log-bound.sh — shared "/var/log tmpfs is bounded against runaway growth" verdict
# (#679).
#
# Every cam box runs /var/log as a fixed 50MB tmpfs. The stock Ubuntu logrotate config
# (/etc/logrotate.d/rsyslog) rotates ONLY on a weekly calendar schedule with NO `size` cap, so a
# chatty logger (dantesync's per-second [PTP] Drift line was the dominant driver, ~65% of the
# fleet's volume) can fill the whole 50MB tmpfs in ~4-5 days -- well before the first weekly
# rotation ever fires -- and crash camera-box.service when rsyslog can no longer write
# ("No space left on device", confirmed live on cam2, 2026-07-11). A checked-only-once-a-day
# `size` cap alone is still not enough (a chatty logger can overshoot within that day), so
# camera-box also ships a systemd timer drop-in that runs logrotate every 15 minutes instead of
# the stock daily cadence.
#
# This does NOT claim to bound an arbitrarily fast/pathological logger (a tight loop spamming
# megabytes/second could still overshoot within a single 15-minute check window) -- it bounds the
# REALISTIC class of "steady chatty INFO/DEBUG logger" that has actually been observed on this
# fleet (dantesync's ~1/sec line, camera-box's own ~1/5s line), with a large safety margin.
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# The two files camera-box provisions on every box (setup-device.sh STEP 18 + create-usb-linux.sh
# mirror it). Single source of truth so the provisioners, the live-fleet apply step, and
# verify-device.sh's (s) check can never drift from each other.
LOG_BOUND_LOGROTATE_PATH="/etc/logrotate.d/rsyslog"
LOG_BOUND_TIMER_DROPIN_PATH="/etc/systemd/system/logrotate.timer.d/99-camera-box-frequent.conf"
LOG_BOUND_SIZE_CAP="5M"

# log_bound_gather_remote_snippet -> a single SSH-able remote command string that prints the two
# facts log_bound_verdict needs, one KEY=VALUE line each (mirrors timesync_gather_remote_snippet's
# convention exactly). No side effects — read-only.
log_bound_gather_remote_snippet() {
  cat <<'REMOTE'
echo "LOGROTATE_RSYSLOG_SIZE=$(grep -oE 'size[[:space:]]+[0-9]+[kKmMgG]' /etc/logrotate.d/rsyslog 2>/dev/null | head -1 | awk '{print $2}')"
echo "LOGROTATE_FREQUENT_DROPIN=$(cat /etc/systemd/system/logrotate.timer.d/99-camera-box-frequent.conf 2>/dev/null | tr '\n' '|')"
REMOTE
}

# log_bound_verdict STATE_BLOCK -> "ok" or "FAIL: <reason>" (one line).
# STATE_BLOCK is the KEY=VALUE-per-line text produced by log_bound_gather_remote_snippet (or an
# equivalent hand-built fixture in tests). Fail-closed on anything unreadable/missing -- an absent
# or unparseable value is never read as "safely bounded" (test-strictness).
log_bound_verdict() {
  local block="$1"
  local size_val dropin

  size_val="$(printf '%s\n' "$block" | sed -n 's/^LOGROTATE_RSYSLOG_SIZE=//p')"
  dropin="$(printf '%s\n' "$block" | sed -n 's/^LOGROTATE_FREQUENT_DROPIN=//p')"

  if [ -z "$size_val" ]; then
    echo "FAIL: ${LOG_BOUND_LOGROTATE_PATH} has no \`size\` cap -- a chatty logger can refill the 50MB /var/log tmpfs before the next weekly calendar rotation (#679)"
    return
  fi

  if [ -z "$dropin" ]; then
    echo "FAIL: ${LOG_BOUND_TIMER_DROPIN_PATH} is missing -- logrotate only runs on the stock DAILY cadence, too slow to bound a chatty logger against the 50MB tmpfs (#679)"
    return
  fi

  case "$dropin" in
    *'OnCalendar=*:0/'*) ;;
    *)
      echo "FAIL: ${LOG_BOUND_TIMER_DROPIN_PATH} is present but does not set a short OnCalendar interval (#679)"
      return
      ;;
  esac

  echo "ok"
}

# log_bound_logrotate_config -> the full desired content of ${LOG_BOUND_LOGROTATE_PATH}
# (/etc/logrotate.d/rsyslog). Single source of truth for BOTH provisioners (setup-device.sh STEP
# 18 writes it to the live box; create-usb-linux.sh writes it into the base-image chroot) so the
# size cap can never drift between the two. Keeps the stock file's six log paths + postrotate HUP
# script (no message loss, unlike copytruncate) — only `rotate` (4 -> 1, minimal retention; this
# appliance log is for live troubleshooting, not audit history) and `size` (added) change.
log_bound_logrotate_config() {
  cat <<EOF
/var/log/syslog
/var/log/mail.log
/var/log/kern.log
/var/log/auth.log
/var/log/user.log
/var/log/cron.log
{
	rotate 1
	size ${LOG_BOUND_SIZE_CAP}
	missingok
	notifempty
	sharedscripts
	postrotate
		/usr/lib/rsyslog/rsyslog-rotate
	endscript
}
EOF
}

# log_bound_timer_dropin -> the full desired content of ${LOG_BOUND_TIMER_DROPIN_PATH}, a systemd
# drop-in overriding the stock logrotate.timer's `OnCalendar=daily` with a 15-minute cadence. The
# leading empty `OnCalendar=` clears the inherited daily value (required systemd drop-in syntax
# for a directive that can be specified multiple times) before the real override.
log_bound_timer_dropin() {
  cat <<'EOF'
[Timer]
OnCalendar=
OnCalendar=*:0/15
AccuracySec=1min
EOF
}
