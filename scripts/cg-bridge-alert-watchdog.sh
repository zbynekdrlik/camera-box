#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/optical-chain-alert-watchdog.sh (set -uo pipefail,
# not -e).
#
# scripts/cg-bridge-alert-watchdog.sh -- #1006: strih CG-bridge republish-black alert, DEV1-SIDE.
#
# WHY (#1006, measured live 2026-08-06 + re-confirmed 2026-08-17): strih's `CG bridge` scene
# renders fully BLACK on air because Resolume Arena's "CG_Bridge light" composition output is black
# WHILE its own upstream NDI feed (`cg` / RESOLUME-SNV (cg-obs)) is live -- and NO alarm fires
# anywhere (Arena up, spout plugin up, sender registered, the sibling `spout moderatori` renders
# fine). Same silent-black-on-air class as #721/#860: only reading the rendered pixels catches it.
# The root cause is INTERNAL to Arena (a third-party app) -- camera-box can DETECT the fault (issue
# 941 already rejected rewiring Spout->NDI, so the fix must keep Spout), not prevent it.
#
# A blanket "every production scene renders non-black" gate false-fails on every legitimately-idle
# overlay scene (CG bridge AND Ableset lyrics are both black at idle on a healthy rig -- measured).
# So this watchdog uses the DIFFERENTIAL probe `obs_phase2.py republish-black-check`: it pages ONLY
# when the upstream reference is LIVE but its Spout republish is BLACK (the exact 2026-08-06
# signature). Both-black = legitimately idle = no alarm.
#
# A DEV1 systemd --user timer runs the read-only differential probe against strih over the OBS
# WebSocket, runs the SHARED classifier (scripts/lib/cg-bridge-health.sh), and -- once a genuine
# republish-black is CONFIRMED across N passes -- fires `airuleset.py notify` from HERE (dev1 has
# the airuleset checkout + Discord credentials; the SAME topology as optical-chain #860 /
# imag-obs #882). The confirm + throttle are the SAME pure obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) #391/#860/#882 already use --
# no second alert mechanism, no second black-check.
#
# Ships DISABLED (the systemd/cg-bridge-alert-watchdog.timer is not enabled anywhere) -- the same
# convention as #732/#794/#860; the operator/supervisor enables it after review.
#
# Usage:
#   scripts/cg-bridge-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/cg-bridge-alert-watchdog.sh --dry-run  # measure + decide + LOG only
#   scripts/cg-bridge-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/cg-bridge-health.sh
. "$HERE/lib/cg-bridge-health.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '6,36p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "cg-bridge-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
STRIH="${STRIH:-10.77.9.202}"                          # the box whose program renders the leg
OBS_PW="${OBS_PASSWORD:-}"
REFERENCE="${CG_BRIDGE_REFERENCE:-cg}"                  # the upstream NDI input (RESOLUME-SNV (cg-obs))
SUBJECT="${CG_BRIDGE_SUBJECT:-spout CG}"                # its Spout republish (sender "Arena - cg-bridge")
MIN_MEAN="${CG_BRIDGE_MIN_MEAN:-}"                      # "" -> peak-only default (the ticket's semantics)
PROBE_TIMEOUT="${CG_BRIDGE_PROBE_TIMEOUT:-40}"
ALERT_THROTTLE_PASSES="${CG_BRIDGE_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence
# 2-pass confirm before paging (same obs_watchdog_confirm the obs-liveness / optical-chain siblings
# use): a single pass landing during an Arena clip transition / a brief upstream flicker could read
# a transient republish-black; a genuine dropped feed stays black across the 5-min cadence.
CONFIRM_THRESHOLD="${CG_BRIDGE_ALERT_CONFIRM_THRESHOLD:-2}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${CG_BRIDGE_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${CG_BRIDGE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${CG_BRIDGE_ALERT_STATE_FILE:-$STATE_DIR/camera-box-cg-bridge-alert.state}"

log() { printf '%s [cg-bridge-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- measure: the read-only differential republish-black probe against strih --------------------
# Bounded by `timeout`; the rc is classified purely below. A WS/connectivity failure (or the probe's
# own UNKNOWN=4) classifies as "nothing to decide", NEVER a false alert.
measure() {
  local min_arg=()
  [ -n "$MIN_MEAN" ] && min_arg=(--min-mean "$MIN_MEAN")
  PROBE_OUT="$(timeout "$PROBE_TIMEOUT" python3 "$HERE/obs_phase2.py" republish-black-check \
    --host "$STRIH" --password "$OBS_PW" --reference "$REFERENCE" --subject "$SUBJECT" \
    --label "#1006 cg-bridge-watchdog" "${min_arg[@]}" 2>&1)"
  PROBE_RC=$?
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
  # A healthy / unknown pass is NOT an incident: clear the confirm counter AND the throttle sig so a
  # genuinely NEW episode later pages fresh instead of being dedup'd against a stale signature
  # (mirrors the optical-chain / imag-obs alert-watchdog reset discipline).
  write_state_field confirm 0
  write_state_field alert_sig ""
  write_state_field alert_passes 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, strih=$STRIH, reference='$REFERENCE', subject='$SUBJECT')"
  measure

  local verdict
  verdict="$(cg_bridge_classify_probe "$PROBE_RC")"
  log "probe_rc=$PROBE_RC -> verdict=$verdict"

  case "$verdict" in
    alert:*) : ;;   # an incident this pass -- confirm across passes below before paging
    *)
      # healthy (OK/IDLE) or unknown (unreadable/transport) -- no incident.
      log "no cg-bridge incident this pass ($verdict): ${PROBE_OUT:-}"
      clear_throttle
      log "pass end"
      return 0
      ;;
  esac

  # CONFIRM across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field confirm 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field confirm "${confirm:-0}"
  log "confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "incident seen ($verdict) but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    log "pass end"
    return 0
  fi

  # An incident is CONFIRMED -> throttle-dedup on the verdict signature.
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="cg-bridge:${verdict}"
  prior_sig="$(read_state_field alert_sig "")"
  prior_passes="$(read_state_field alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field alert_sig "$new_sig"
  write_state_field alert_passes "$new_passes"

  detail="strih '$SUBJECT' (Spout republish of Arena's CG-bridge output) renders BLACK while its upstream '$REFERENCE' is LIVE -- Resolume Arena is dropping the live CG-bridge feed (issue 1006). The OBS receiver/binding are fine; the fix is Arena-side (re-trigger the CG-bridge clip / check the composition output). A cut to the 'CG bridge' scene right now would put BLACK on air."

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: cg-bridge incident ($detail) alert_now=$alert_now"
    log "pass end"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for cg-bridge republish-black incident"
    python3 "$NOTIFY" notify --body \
      "🚨 #1006 CG bridge: ${detail} (${REPO_SLUG})." \
      --dedup-key "cg-bridge" \
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
