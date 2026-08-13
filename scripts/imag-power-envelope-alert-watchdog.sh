#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/imag-obs-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/imag-power-envelope-alert-watchdog.sh — #1040 imag-nb power/thermal-envelope alert,
# DEV1-SIDE.
#
# WHY: imag-nb's on-box power-envelope guard (imag-power-envelope-guard.sh) journald-tags every
# STEP-DOWN / RE-ASSERT transition, but imag-nb has NO ~/devel/airuleset checkout and no Discord
# credentials to alert from there (the SAME topology as scripts/imag-obs-alert-watchdog.sh #882).
# So a DEV1 systemd --user timer SSH-polls the guard's journal window + the live PL1/TCPU/act_freq
# and fires `airuleset.py notify` from HERE when a clamp episode (STEP-DOWN) or a foreign
# re-program (RE-ASSERT) is seen. PROCHOT-only silent degradation is exactly what the standing
# rig-alert rule forbids — this is the loud half.
#
# The "is there a concerning transition?" decision is the SHARED pure imag_power_alert_condition
# (scripts/lib/imag-power-envelope.sh); the alert throttle is the SAME pure
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) #391/#882 already use — no
# second alert mechanism.
#
# Usage:
#   scripts/imag-power-envelope-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/imag-power-envelope-alert-watchdog.sh --dry-run  # measure + decide + LOG only
#   scripts/imag-power-envelope-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/imag-power-envelope.sh
. "$HERE/lib/imag-power-envelope.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "imag-power-envelope-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
IMAG_IP="${IMAG_IP:-10.77.9.182}"
IMAG_USER_SSH="${IMAG_USER:-newlevel}"
IMAG_PW_SSH="${IMAG_PW:-newlevel}"
WINDOW="${IMAG_POWER_ALERT_WINDOW:--10min}"                       # journal look-back window
ALERT_THROTTLE_PASSES="${IMAG_POWER_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${IMAG_POWER_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${IMAG_POWER_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${IMAG_POWER_ALERT_STATE_FILE:-$STATE_DIR/camera-box-imag-power-alert.state}"

log() { printf '%s [imag-power-envelope-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure: the guard's journal window + a live PL1/TCPU/act_freq snapshot ──────────────────
# JOURNAL empty means an ssh/connectivity failure (the fleet's own preflight owns THAT condition),
# treated as "nothing to decide" this pass, never a false alert.
measure() {
  JOURNAL="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" \
    "journalctl -t imag-power-envelope --no-pager --since \"$WINDOW\" 2>/dev/null" 2>/dev/null || true)"
  SNAPSHOT="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_power_envelope_gather_remote_snippet)" 2>/dev/null || true)"
}

read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || echo "$STATE_FILE")"
  { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

main() {
  log "pass start (dry_run=$DRY_RUN, window=$WINDOW)"
  measure
  if [ -z "${JOURNAL:-}" ]; then
    log "no guard journal from imag-nb (ssh/connectivity failure, or the guard has logged nothing in $WINDOW) -- nothing to decide this pass"
    # A quiet window is NOT an incident: clear the throttle sig so a genuinely NEW episode later
    # pages fresh instead of being dedup'd against a stale signature.
    write_state_field alert_sig ""
    write_state_field alert_passes 0
    log "pass end"
    return 0
  fi

  local markers
  markers="$(imag_power_alert_condition "$JOURNAL")"
  if [ -z "$markers" ]; then
    log "imag-nb power envelope: no STEP-DOWN/RE-ASSERT in $WINDOW (healthy)"
    write_state_field alert_sig ""
    write_state_field alert_passes 0
    log "pass end"
    return 0
  fi

  # Concerning transition seen -> throttle-dedup on the marker signature.
  local pl1 tcpu current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  pl1="$(printf '%s\n' "$SNAPSHOT" | { imag_power_zone_select "$(cat)" 2>/dev/null || true; })"
  tcpu="$(printf '%s\n' "$SNAPSHOT" | sed -n 's/^TCPU|//p' | head -1)"
  current_sig="imag-power:$(printf '%s' "$markers" | tail -1)"
  prior_sig="$(read_state_field alert_sig "")"
  prior_passes="$(read_state_field alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field alert_sig "$new_sig"
  write_state_field alert_passes "$new_passes"

  local detail
  detail="PL1=${pl1:-?}uW TCPU=${tcpu:-?}C — $(printf '%s' "$markers" | tail -1)"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: imag-nb power-envelope transition ($detail) alert_now=$alert_now"
    log "pass end"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for imag-nb power-envelope transition"
    python3 "$NOTIFY" notify --body \
      "🚨 #1040 imag-power-envelope: imag-nb power clamp/foreign-reprogram ($REPO_SLUG). ${detail}" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
