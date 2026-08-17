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
IMAG_RENDER_WINDOW_S="${IMAG_RENDER_WINDOW_S:-4}"                 # #799 OBS-WS render delta window (s)

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${IMAG_POWER_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${IMAG_POWER_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${IMAG_POWER_ALERT_STATE_FILE:-$STATE_DIR/camera-box-imag-power-alert.state}"

log() { printf '%s [imag-power-envelope-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure: the guard's journal window + a live PL1/TCPU/act_freq snapshot ──────────────────
# JOURNAL empty means an ssh/connectivity failure (the fleet's own preflight owns THAT condition) OR
# a quiet window (the guard logs only on transitions) -- either way UNMEASURED, so alert_from_journal
# treats it as "no new info": it preserves the existing dedup signature (#1076), never a false alert.
measure() {
  JOURNAL="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" \
    "journalctl -t imag-power-envelope --no-pager --since \"$WINDOW\" 2>/dev/null" 2>/dev/null || true)"
  SNAPSHOT="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_power_envelope_gather_remote_snippet)" 2>/dev/null || true)"
  # #880: a short throttle+freq BURST (~6 s) for the throttle-under-floor path. Separate ssh so the
  # instantaneous SNAPSHOT gather above (also used by drift-guard/verify-imag) is never slowed.
  BURST="$(sshpass -p "$IMAG_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER_SSH}@${IMAG_IP}" "$(imag_power_throttle_burst_remote_snippet)" 2>/dev/null || true)"
  # #799: the OBS render stats over the OBS WebSocket (dev1 -> imag OBS WS, NOT ssh), for the
  # render-degradation CAUSE discriminator. Only attempt it when the box is REACHABLE (a non-empty
  # BURST proves the ssh path works) -- an unreachable box (e.g. the dry-run TEST-NET IP) skips it,
  # keeping the pass fast. Best-effort: the reader emits nothing on any WS failure, so RENDER stays
  # empty -> imag_render_cause_from_signals returns `unknown` -> no false alert.
  RENDER=""
  if [ -n "${BURST:-}" ]; then
    # Hard-bound the WS read so a hung OBS WS can never blow the timer's TimeoutStartSec: a `timeout`
    # kill (or any reader failure) leaves RENDER empty -> discriminator `unknown` -> no alert.
    # Forward the OBS WS password (imag OBS is AuthRequired=false today, so empty works, but this
    # keeps the alert alive if a future setup enables auth -- parity with recording-e2e.sh:2272).
    RENDER="$(OBS_PASSWORD_IMAG="${OBS_PASSWORD_IMAG:-${OBS_PASSWORD:-}}" OBS_OP_TIMEOUT_S=8 \
      timeout 15 python3 "$HERE/imag-render-stats.py" --host "$IMAG_IP" \
      --window-s "$IMAG_RENDER_WINDOW_S" || true)"
  fi
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

# Path 1 — the guard's STEP-DOWN / RE-ASSERT journal transitions (a thermal step-down or a foreign
# PL1 re-program). Own dedup signature (alert_sig / alert_passes).
alert_from_journal() {
  if [ -z "${JOURNAL:-}" ]; then
    log "no guard journal from imag-nb (ssh/connectivity failure, or a quiet journal window) -- UNMEASURED this pass"
    # #1076: an empty journal is UNMEASURED (an ssh hiccup, OR a quiet window -- the guard logs ONLY
    # on STEP-DOWN/RESTORE/RE-ASSERT transitions, never a heartbeat, so a quiet window genuinely
    # carries no reading), NOT a resolved episode. Treat it as no-new-info and PRESERVE the dedup
    # signature + pass count (do NOT write them) so a persistent episode stays deduped across a
    # transient gap instead of re-paging every time the measurement flaps. Only a genuinely
    # MEASURED-healthy pass (a non-empty journal with no STEP-DOWN/RE-ASSERT marker, below) resolves
    # the episode and resets, so a later NEW episode still pages fresh.
    return 0
  fi

  local markers
  markers="$(imag_power_alert_condition "$JOURNAL")"
  if [ -z "$markers" ]; then
    log "imag-nb power envelope: no STEP-DOWN/RE-ASSERT in $WINDOW (healthy)"
    write_state_field alert_sig ""
    write_state_field alert_passes 0
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
}

# Path 2 (#880) — a SUSTAINED iGPU clock clamp holding actual freq below the pinned floor while OBS
# renders. INDEPENDENT of the guard journal: this clamp is the punit steering below the #841 floor
# at the MMIO RAPL PL1 power budget, which fires NO guard STEP-DOWN (the guard steps down only on a
# TCPU excursion), so it would otherwise be silent judder. Own dedup signature (throttle_sig /
# throttle_passes) so it never clobbers the journal path's dedup.
alert_from_throttle() {
  local markers
  markers="$(imag_power_throttle_alert_condition "${BURST:-}")"
  if [ -z "$markers" ]; then
    # #1076: no THROTTLE-UNDER-FLOOR marker is TWO distinct states the 2-state condition collapses.
    # Use the 3-state imag_power_throttle_state to tell them apart: `unknown` (no FLOOR / too few
    # samples = an ssh/burst hiccup) is UNMEASURED -> PRESERVE the dedup signature + passes so a
    # persistent clamp stays deduped across a transient gap; `clean` (a valid burst = the GPU has
    # headroom) is MEASURED-healthy -> the clamp genuinely resolved -> reset so a later new clamp
    # pages fresh.
    if [ "$(imag_power_throttle_state "${BURST:-}")" = "unknown" ]; then
      log "imag-nb iGPU freq: throttle burst UNMEASURED (no FLOOR / too few samples) -- preserving dedup signature"
      return 0
    fi
    log "imag-nb iGPU freq: no sustained throttle-under-floor in this burst (GPU headroom) -- healthy"
    write_state_field throttle_sig ""
    write_state_field throttle_passes 0
    return 0
  fi

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  # STABLE episode signature (NOT the fluctuating clamped/total count) so a sustained clamp pages
  # once then suppresses for ~1h, instead of re-paging every pass as the count wobbles.
  current_sig="$(imag_power_throttle_alert_sig "$(printf '%s' "$markers" | tail -1)")"
  prior_sig="$(read_state_field throttle_sig "")"
  prior_passes="$(read_state_field throttle_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field throttle_sig "$new_sig"
  write_state_field throttle_passes "$new_passes"

  detail="$(printf '%s' "$markers" | tail -1)"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: imag-nb iGPU throttle-under-floor ($detail) alert_now=$alert_now"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for imag-nb throttle-under-floor"
    python3 "$NOTIFY" notify --body \
      "🚨 #880 imag iGPU clock floor not holding under load ($REPO_SLUG): ${detail}. Pinned floor is a software request the punit overrides at the power/thermal envelope; cooling headroom (issue 1043) is the residual fix." \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# Path 3 (#799) — the render-degradation CAUSE discriminator. Fuses the OBS render stats (RENDER,
# read over WS) with the GPU throttle burst (BURST, already measured) via the shared pure
# imag_render_cause_from_signals. NAMES which cause is active instead of an ambiguous "render
# degraded": `churn-leak` (render degraded while the GPU has HEADROOM — throttle clean — the #799
# connection-churn leak an OBS restart clears) PAGES here; `power-clamp` (render degraded WHILE
# clamped) is left to alert_from_throttle above (NO duplicate page); healthy/stalled/unknown never
# page. The churn signal is noisier than the throttle burst (one WS window can catch a transient),
# so it is CONFIRMED across 2 consecutive passes (obs_watchdog_confirm) before the shared throttle
# dedup (obs_watchdog_alert_throttle) applies. Own dedup + confirm state keys (render_*) so it never
# clobbers the journal/throttle paths' dedup.
alert_from_render_discriminator() {
  local cause_out cause detail
  cause_out="$(imag_render_cause_from_signals "${RENDER:-}" "${BURST:-}")"
  cause="$(printf '%s\n' "$cause_out" | head -1 | cut -d'|' -f1)"
  detail="$(printf '%s\n' "$cause_out" | head -1 | cut -d'|' -f2-)"

  # Only churn-leak is the NEW, previously-silent, restart-clears alert. power-clamp is already paged
  # by alert_from_throttle (no duplicate); healthy/stalled/unknown never page. A MEASURED non-churn
  # outcome (healthy/stalled/power-clamp) clears the confirm + dedup state so a genuinely NEW episode
  # later pages fresh; the UNMEASURED `unknown` outcome preserves the dedup state (see #1076 below).
  if [ "$cause" != "churn-leak" ]; then
    log "imag render discriminator: cause=$cause ($detail)"
    # #1076: `unknown` (the render sample or the throttle burst was unreadable this pass) is
    # UNMEASURED -- the ticket's headline "a persistent churn leak whose WS read intermittently
    # fails" case. A churn candidate cannot carry its 2-pass confirmation across a measurement gap,
    # so ALWAYS reset the confirm counter; but on `unknown` PRESERVE the dedup signature + passes so
    # the persistent churn stays deduped instead of re-paging every gap. Every OTHER non-churn cause
    # (healthy/stalled/power-clamp) is a MEASURED outcome -> genuinely resolved (or owned by the
    # throttle path) -> reset the full dedup state so a later new episode pages fresh.
    write_state_field render_confirm 0
    if [ "$cause" != "unknown" ]; then
      write_state_field render_sig ""
      write_state_field render_passes 0
    fi
    return 0
  fi

  # churn-leak: require 2 consecutive confirmations (a single WS window can catch a transient GC
  # pause / scene transition) before it is treated as a real episode.
  local confirm_prev confirm_out confirm act
  confirm_prev="$(read_state_field render_confirm 0)"
  confirm_out="$(obs_watchdog_confirm "$confirm_prev" 1 2)"
  confirm="$(printf '%s\n' "$confirm_out" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$confirm_out" | sed -n 's/^act=//p')"
  write_state_field render_confirm "$confirm"
  if [ "${act:-0}" != "1" ]; then
    log "imag render discriminator: churn-leak candidate (confirm ${confirm}/2) — not yet confirmed"
    return 0
  fi

  # Confirmed -> dedup on a STABLE churn signature (once-then-suppress-for-~1h), same shared throttle.
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="imag-render:churn-leak"
  prior_sig="$(read_state_field render_sig "")"
  prior_passes="$(read_state_field render_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field render_sig "$new_sig"
  write_state_field render_passes "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: imag render churn-leak #799 ($detail) alert_now=$alert_now"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for imag render churn-leak #799"
    python3 "$NOTIFY" notify --body \
      "🚨 #799 imag OBS render degraded with GPU headroom normal (throttle CLEAN) on $REPO_SLUG: ${detail}. Distinct from the issue 880/1043 power clamp — a graceful OBS restart (systemctl --user restart imag-obs) clears it." \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, window=$WINDOW)"
  measure
  # Three INDEPENDENT alert paths, each with its OWN dedup state keys — a quiet guard journal must
  # NOT short-circuit the throttle path (the throttle-under-floor clamp fires no guard step-down, so
  # it lives only in the burst), and the render-discriminator path (#799) names churn-leak vs the
  # power clamp without double-paging a clamp the throttle path already owns.
  alert_from_journal
  alert_from_throttle
  alert_from_render_discriminator
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
