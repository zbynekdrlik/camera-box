#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/obs-liveness-watchdog.sh / scripts/imag-obs-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/avsync-heartbeat-alert-watchdog.sh -- #812/#807 stream-box avsync heartbeat alert, DEV1-SIDE.
#
# WHY: neither avsync-watchdog.ps1 (#812, the A/V-sync measurement loop) nor avsync-vlc-monitor.ps1
# (#807, the VLC program-audio babysitter) can alert Discord on their OWN silence -- a process that
# has crashed or hung obviously cannot report its own death, and the stream box has no
# ~/devel/airuleset checkout / Discord credentials of its own (the SAME topology gap #882's
# imag-obs-alert-watchdog.sh and #391's obs-liveness-watchdog.sh already close for their own boxes
# -- see .claude/rules/imag-obs-supervision.md). This script applies that SAME dev1-side alert
# topology to BOTH avsync heartbeats: a dev1 systemd --user timer SSHes into the stream box, reads
# both heartbeat files in ONE round-trip (scripts/lib/avsync-heartbeat.sh), and fires a Discord
# alert via airuleset.py notify the moment either heartbeat goes stale -- independent
# confirm/throttle state per leg, reusing the SAME pure scripts/lib/obs-watchdog-decision.sh #391
# already established (never a third alerting mechanism).
#
# Usage:
#   scripts/avsync-heartbeat-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/avsync-heartbeat-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/avsync-heartbeat-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/avsync-heartbeat.sh
. "$HERE/lib/avsync-heartbeat.sh"
# avsync-heartbeat.sh sets `-e` for ITS OWN sourcing safety; re-assert this script's own intended
# options afterward so a stray non-zero return from a plain assignment never aborts a pass early
# (this watchdog must survive a bad pass and keep polling on the next timer tick, see the header).
set -uo pipefail

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '5,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "avsync-heartbeat-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
STREAM_IP="${STREAM_IP:-10.77.9.204}"
STREAM_USER_SSH="${STREAM_USER:-newlevel}"
STREAM_PW_SSH="${STREAM_PW:-newlevel}"
# 2x avsync-watchdog.ps1's ~90s natural cadence AND 2x avsync-vlc-monitor.ps1's ~15-35s cadence,
# with comfortable margin either way -- one env override covers both legs (they run independently
# but on similar timescales; a per-leg override was not worth the extra complexity here).
STALE_S="${AVSYNC_HEARTBEAT_STALE_S:-300}"
CONFIRM_THRESHOLD="${AVSYNC_HEARTBEAT_CONFIRM_THRESHOLD:-1}"
ALERT_THROTTLE_PASSES="${AVSYNC_HEARTBEAT_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AVSYNC_HEARTBEAT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${AVSYNC_HEARTBEAT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${AVSYNC_HEARTBEAT_STATE_FILE:-$STATE_DIR/camera-box-avsync-heartbeat.state}"

log() { printf '%s [avsync-heartbeat-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure (SSH + the shared avsync-heartbeat probe) ───────────────────────
measure() {
  PROBE_OUT="$(sshpass -p "$STREAM_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${STREAM_USER_SSH}@${STREAM_IP}" "$(avsync_heartbeat_probe_cmd)" 2>/dev/null || true)"
}

# ── read / write persisted state (same key=value shape as the #391/#882 siblings) ──────────────
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

# ── one leg's confirm/throttle/alert pass -- reused for BOTH "watchdog" and "vlc" ───────────────
process_leg() {
  local leg="$1" epoch="$2" now epoch_display wedged prev_confirm decision confirm act
  now="$(date +%s)"
  wedged=0
  if avsync_heartbeat_is_stale "$epoch" "$now" "$STALE_S"; then wedged=1; fi
  epoch_display="${epoch:-<none>}"
  log "$leg: last_heartbeat_epoch=$epoch_display now=$now stale_s=$STALE_S wedged=$wedged"

  prev_confirm="$(read_state_field "${leg}_confirm" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${leg}_confirm" "${confirm:-0}"
  log "$leg: confirm=$prev_confirm -> $confirm act=$act"

  if [ "$wedged" -eq 0 ]; then
    write_state_field "${leg}_alert_sig" ""
    write_state_field "${leg}_alert_passes" 0
    return 0
  fi
  [ "${act:-0}" = "1" ] || return 0

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${leg}:stale"
  prior_sig="$(read_state_field "${leg}_alert_sig" "")"
  prior_passes="$(read_state_field "${leg}_alert_passes" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "${leg}_alert_sig" "$new_sig"
  write_state_field "${leg}_alert_passes" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $leg heartbeat CONFIRMED stale (last=$epoch_display) alert_now=$alert_now"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $leg"
    python3 "$NOTIFY" notify --body \
      "🚨 avsync-heartbeat-alert-watchdog: stream-box $leg heartbeat is STALE (last=${epoch_display}, threshold=${STALE_S}s) ($REPO_SLUG)." \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle for $leg (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, stale_s=$STALE_S, threshold=$CONFIRM_THRESHOLD)"

  measure
  if [ -z "${PROBE_OUT:-}" ]; then
    log "ERROR: no probe output from stream box (ssh/connectivity failure) -- nothing to decide this pass"
    return 0
  fi

  local watchdog_segment vlc_segment watchdog_epoch vlc_epoch
  watchdog_segment="$(avsync_heartbeat_extract_segment "$PROBE_OUT" watchdog)"
  vlc_segment="$(avsync_heartbeat_extract_segment "$PROBE_OUT" vlc)"
  watchdog_epoch="$(avsync_heartbeat_last_epoch "$watchdog_segment")"
  vlc_epoch="$(avsync_heartbeat_last_epoch "$vlc_segment")"

  process_leg "watchdog" "$watchdog_epoch"
  process_leg "vlc" "$vlc_epoch"

  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
