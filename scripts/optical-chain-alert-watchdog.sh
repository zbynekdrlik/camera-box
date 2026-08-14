#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/imag-power-envelope-alert-watchdog.sh (set -uo pipefail,
# not -e).
#
# scripts/optical-chain-alert-watchdog.sh -- #860: cam2 optical-injection-leg alert, DEV1-SIDE.
#
# WHY (#860, live incident 2026-08-14 ~07:00): a chain of FAILED E2E runs whose cleanups each
# logged `WARNING #712: cam2/painter restore failed/timed out` left the painter DEAD -- all camera
# views pitch black -- and the next gate run's cam2->cam1 hop reported UNAVAILABLE / breached the
# undecodable floor, with NO alert firing anywhere; found only by manual screenshot triage. Every
# OTHER silent-degradation class on this fleet already has a dev1-side alert watchdog (imag-obs
# #882, imag-power-envelope #1040, obs-liveness #391, avsync-heartbeat #812) -- the optical
# injection leg had none. The standing rig-degradation-alert rule demands a dead painter (dark
# monitor) pages immediately, never silently poisoning consecutive gate runs.
#
# A DEV1 systemd --user timer SSH-probes the cam2 painter (pidfile + permanent service) + reads a
# LIVE optical proof off strih (the #901 `obs_phase2.py assert-program-nonblack`, reused as-is),
# runs the SHARED pure decision (scripts/lib/optical-chain-health.sh), and -- once a genuine
# dead-painter / black-monitor is seen while a painter is EXPECTED (TEST mode) -- fires
# `airuleset.py notify` from HERE (cam2 has no airuleset checkout / Discord credentials, the SAME
# topology as imag-obs-alert-watchdog.sh #882). The alert throttle is the SAME pure
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) #391/#882/#1040 already use --
# no second alert mechanism, no second black-check.
#
# TEST-mode-aware: only alerts when a painter is EXPECTED (pidfile present OR cam2-painter.service
# enabled). EVENT mode (pidfile removed + service disabled, #892) -> painter_expected=0 -> SKIP, so
# a deliberately-dark broadcast monitor never pages.
#
# Usage:
#   scripts/optical-chain-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/optical-chain-alert-watchdog.sh --dry-run  # measure + decide + LOG only
#   scripts/optical-chain-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/optical-chain-health.sh
. "$HERE/lib/optical-chain-health.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "optical-chain-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
PAINTER_IP="${PAINTER_IP:-10.77.9.62}"                   # cam2 -- the box with the physical monitor
CAM_USER_SSH="${CAM_USER:-root}"
CAM_PW_SSH="${CAM_PW:-newlevel}"
PAINTER_PIDFILE="${PAINTER_PIDFILE:-/run/rig-painter.pid}"
PAINTER_SERVICE="${PAINTER_SERVICE:-cam2-painter.service}"
STRIH="${STRIH:-10.77.9.202}"                            # the box whose program renders the leg
OBS_PW="${OBS_PASSWORD:-}"
OPTICAL_SCENE="${OPTICAL_CHAIN_SCENE:-}"                  # "" -> the CURRENT program scene (#901)
OPTICAL_PROBE_TIMEOUT="${OPTICAL_CHAIN_PROBE_TIMEOUT:-40}"
ALERT_THROTTLE_PASSES="${OPTICAL_CHAIN_ALERT_THROTTLE_PASSES:-12}"  # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${OPTICAL_CHAIN_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${OPTICAL_CHAIN_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${OPTICAL_CHAIN_ALERT_STATE_FILE:-$STATE_DIR/camera-box-optical-chain-alert.state}"

log() { printf '%s [optical-chain-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- measure: the cam2 painter probe (one ssh) + a LIVE strih optical proof ----------------------
# An empty SNAPSHOT means an ssh/connectivity failure to cam2 (the fleet's own preflight owns THAT
# condition) -- treated as "nothing to decide" this pass, never a false alert.
measure() {
  SNAPSHOT="$(sshpass -p "$CAM_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${CAM_USER_SSH}@${PAINTER_IP}" "$(optical_chain_painter_probe_remote_snippet "$PAINTER_PIDFILE" "$PAINTER_SERVICE")" 2>/dev/null || true)"
  # The #901 optical proof off strih. Bounded by `timeout`; rc+output classified purely below. A
  # WS/connectivity failure classifies as UNKNOWN (nothing to decide about the optical read).
  local scene_arg=()
  [ -n "$OPTICAL_SCENE" ] && scene_arg=(--scene "$OPTICAL_SCENE")
  OPTICAL_OUT="$(timeout "$OPTICAL_PROBE_TIMEOUT" python3 "$HERE/obs_phase2.py" assert-program-nonblack \
    --host "$STRIH" --password "$OBS_PW" --label "#860 optical-chain-watchdog" "${scene_arg[@]}" 2>&1)"
  OPTICAL_RC=$?
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

clear_throttle() {
  # A healthy / skip / unverified window is NOT an incident: clear the throttle sig so a genuinely
  # NEW episode later pages fresh instead of being dedup'd against a stale signature.
  write_state_field alert_sig ""
  write_state_field alert_passes 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, cam2=$PAINTER_IP, strih=$STRIH)"
  measure
  if [ -z "${SNAPSHOT:-}" ]; then
    log "no painter probe from cam2 ($PAINTER_IP) -- ssh/connectivity failure -- nothing to decide this pass"
    clear_throttle
    log "pass end"
    return 0
  fi

  local painter_expected painter_alive optical verdict
  painter_expected="$(optical_chain_painter_expected_from_snapshot "$SNAPSHOT")"
  painter_alive="$(optical_chain_painter_alive_from_snapshot "$SNAPSHOT")"
  optical="$(optical_chain_classify_nonblack_probe "$OPTICAL_RC" "$OPTICAL_OUT")"
  verdict="$(optical_chain_alert_condition "$painter_expected" "$painter_alive" "$optical")"
  log "painter_expected=$painter_expected painter_alive=$painter_alive optical=$optical -> verdict=$verdict"

  case "$verdict" in
    alert:*) : ;;   # fall through to the throttle+alert path below
    *)
      # skip (EVENT mode / not-expected), healthy, or healthy-unverified -- no incident.
      log "no optical-chain incident this pass ($verdict)"
      clear_throttle
      log "pass end"
      return 0
      ;;
  esac

  # An incident is confirmed -> throttle-dedup on the verdict signature.
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="optical-chain:${verdict}"
  prior_sig="$(read_state_field alert_sig "")"
  prior_passes="$(read_state_field alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field alert_sig "$new_sig"
  write_state_field alert_passes "$new_passes"

  case "$verdict" in
    alert:PAINTER-DEAD)
      detail="cam2 painter DEAD while a painter is expected (pidfile present or ${PAINTER_SERVICE} enabled, but neither alive) — cam2 monitor is dark, the cam2→cam1 optical leg cannot be read. Restore: scripts/rig-mode.sh test" ;;
    alert:OPTICAL-BLACK)
      detail="cam2 painter process ALIVE but strih program renders BLACK (issue 901/754 class — process-alive is not QR-on-screen). Check the monitor / painter output; scripts/rig-mode.sh test re-verifies non-black" ;;
    *) detail="$verdict" ;;
  esac

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: optical-chain incident ($detail) alert_now=$alert_now"
    log "pass end"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for optical-chain incident"
    python3 "$NOTIFY" notify --body \
      "🚨 #860 optical-chain: ${detail} (${REPO_SLUG})." \
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
