#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by verify-device.sh /
# setup-device.sh / create-usb-linux.sh — mirrors every sibling in scripts/lib/ (e.g.
# timesync-authority.sh, log-bound.sh), none of which set -euo pipefail either: sourcing a
# `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/log-diet.sh — #762 "appliance logging diet".
#
# Live-proven chain (cam1, 2026-07-15 03:00-03:10 UTC): the 50MB /var/log tmpfs filled after
# ~2.5 days uptime -> rsyslogd entered a write-error feedback loop (write fails -> error line ->
# journald forwards -> write fails again...; measured ~400 lines/s) -> rsyslogd 42.8% + journald
# 17.8% CPU on a 3-core box -> the camera-box send path starved -> cam1 delivery p50 drifted
# 71->80->92ms across the night's fused runs, ts_head_skew 241ms, and cam1->imag NDI
# under-delivery at ~51.4fps -- which manifested as imag's uniform ~14% optical duplicates and
# poisoned the delivery-spread + imag-continuity gates. TCP was exonerated (a dual-leg sampler
# showed zero Send-Q, zero retransmits both legs) -- the loss was CPU-side on the sender.
#
# rsyslog is REDUNDANT on the cam appliances: journald already captures everything (it is the
# ACTUAL log store any operator/harness reads, e.g. `journalctl -u camera-box`), and nothing
# reads /var/log/syslog on a read-only appliance with no operator logging in. So the permanent
# fix is architecturally the SAME class as #591's competing-timesync-daemon purge: rsyslog must
# be PURGED (masking alone is not enough -- an installed-but-masked daemon can still be
# re-enabled by a stray unit reset or a future package pulling it back in as a dependency), and
# journald itself gets a RuntimeMaxUse cap so the journal can never grow to fill the SAME tmpfs
# rsyslog used to (it already lives on the /run tmpfs journal by default, so "can it also fill a
# tmpfs" is a real, not hypothetical, question).
#
# NOTE: this does NOT reuse scripts/lib/timesync-authority.sh's dpkg_status_installed /
# timesync_daemon_verdict (even though the shape is structurally identical -- "a redundant
# daemon must be purged, not merely masked") to avoid a new lib-sources-lib coupling that has no
# precedent anywhere in scripts/lib/; the equivalent few lines of case-statement logic are
# duplicated here instead, matching scripts/lib/preflight-fleet-check.sh's #762 threshold
# constants for the same reason.
#
# Source-only: this file defines pure functions + one shared constant, no side effects on its own.

# The journald drop-in camera-box provisions on every box (setup-device.sh + create-usb-linux.sh
# mirror it). Single source of truth so the provisioners AND verify-device.sh's read-back check
# can never drift from each other -- the SAME discipline scripts/lib/log-bound.sh already applies
# to its own logrotate files.
LOG_DIET_JOURNALD_DROPIN_PATH="/etc/systemd/journald.conf.d/99-camera-box-diet.conf"
LOG_DIET_JOURNALD_RUNTIME_MAX="20M"

# log_diet_journald_dropin -> the full desired content of ${LOG_DIET_JOURNALD_DROPIN_PATH}.
log_diet_journald_dropin() {
  cat <<EOF
[Journal]
RuntimeMaxUse=${LOG_DIET_JOURNALD_RUNTIME_MAX}
EOF
}

# log_diet_gather_remote_snippet -> the REMOTE bash run over ssh that reports rsyslog's
# dpkg/active/enabled state plus the live journald drop-in content, one KEY=VALUE line each (the
# same convention scripts/lib/log-bound.sh's own gather snippet uses). No side effects — read-only.
log_diet_gather_remote_snippet() {
  cat <<'REMOTE'
echo "RSYSLOG_DPKG=$(dpkg -s rsyslog 2>/dev/null | sed -n 's/^Status: //p')"
echo "RSYSLOG_ACTIVE=$(systemctl is-active rsyslog 2>/dev/null)"
echo "RSYSLOG_ENABLED=$(systemctl is-enabled rsyslog 2>/dev/null)"
echo "JOURNALD_DROPIN=$(cat /etc/systemd/journald.conf.d/99-camera-box-diet.conf 2>/dev/null | tr '\n' '|')"
REMOTE
}

# log_diet_rsyslog_purged_from_dpkg DPKG_STATUS -> 0 (true, genuinely purged) or 1 (false, still
# installed in some form). Same "files genuinely gone" states as timesync-authority.sh's
# dpkg_status_installed: EMPTY (dpkg has never heard of it / fully purged), "not-installed", or
# "config-files" (removed, only leftover conffiles). Every other state means rsyslog's files are
# present in some form. Extracted so verify-device.sh's (s) check (#679, the OLD rsyslog-owned
# logrotate size-cap check) can ask "is rsyslog even here any more?" with the EXACT same
# definition log_diet_provision_verdict itself uses, instead of a second, driftable copy -- once
# rsyslog is genuinely purged, /etc/logrotate.d/rsyslog is REMOVED WITH IT (it is one of
# rsyslog's own conffiles), so #679's size-cap check becomes structurally impossible to satisfy
# and must be treated as "superseded by #762", not "FAIL".
log_diet_rsyslog_purged_from_dpkg() {
  case "$1" in
    '' | *' not-installed' | *' config-files') return 0 ;;
    *) return 1 ;;
  esac
}

# log_diet_rsyslog_purged STATE_BLOCK -> 0 (true) or 1 (false). Block-level convenience wrapper
# around log_diet_rsyslog_purged_from_dpkg for a caller that already has the FULL
# log_diet_gather_remote_snippet block (verify-device.sh's (s)/(u) checks share ONE ssh round
# trip -- see the (s)/(u) call sites).
log_diet_rsyslog_purged() {
  local dpkg
  dpkg="$(printf '%s\n' "$1" | sed -n 's/^RSYSLOG_DPKG=//p')"
  log_diet_rsyslog_purged_from_dpkg "$dpkg"
}

# log_diet_provision_verdict STATE_BLOCK -> "ok" or the newline-joined "FAIL: ..." reasons.
# STATE_BLOCK is the KEY=VALUE-per-line text produced by log_diet_gather_remote_snippet (or an
# equivalent hand-built fixture in tests). Fail-closed on anything unreadable/missing -- an
# absent or unparseable value is never read as "safely purged/capped" (test-strictness), mirroring
# log_bound_verdict's own fail-closed discipline.
log_diet_provision_verdict() {
  local block="$1" dpkg active enabled dropin fails="" nl
  nl=$'\n'
  dpkg="$(printf '%s\n' "$block" | sed -n 's/^RSYSLOG_DPKG=//p')"
  active="$(printf '%s\n' "$block" | sed -n 's/^RSYSLOG_ACTIVE=//p')"
  enabled="$(printf '%s\n' "$block" | sed -n 's/^RSYSLOG_ENABLED=//p')"
  dropin="$(printf '%s\n' "$block" | sed -n 's/^JOURNALD_DROPIN=//p')"

  if ! log_diet_rsyslog_purged_from_dpkg "$dpkg"; then
    fails="${fails:+$fails$nl}FAIL: rsyslog is INSTALLED (dpkg status: ${dpkg:-<empty>}) -- purge it, journald already captures everything on this read-only appliance (#762)"
  fi

  if [ "$(printf '%s' "$active" | tr -d '[:space:]')" = "active" ]; then
    fails="${fails:+$fails$nl}FAIL: rsyslog is ACTIVE -- it must be purged, not merely stopped (#762: a masked-but-installed daemon can still be re-enabled)"
  fi

  case "$(printf '%s' "$enabled" | tr -d '[:space:]')" in
    '' | masked | disabled | not-found) ;;
    *)
      fails="${fails:+$fails$nl}FAIL: rsyslog is enabled (state=${enabled:-<none>}) -- must be purged (#762)"
      ;;
  esac

  case "$dropin" in
    *"RuntimeMaxUse=${LOG_DIET_JOURNALD_RUNTIME_MAX}"*) ;;
    *)
      fails="${fails:+$fails$nl}FAIL: journald RuntimeMaxUse=${LOG_DIET_JOURNALD_RUNTIME_MAX} drop-in missing/wrong at ${LOG_DIET_JOURNALD_DROPIN_PATH} (#762) -- the journal could refill the tmpfs the same way rsyslog's write-error loop did"
      ;;
  esac

  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    printf 'ok\n'
  fi
}
