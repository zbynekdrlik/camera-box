#!/usr/bin/env bash
# imag-nb power-envelope runtime GUARD (#1040) — see the extended header below.
set -euo pipefail
# =============================================================================================
# Runs on a ~45 s ROOT timer (imag-power-envelope-guard.timer). It REPLACES thermald's one useful
# behavior — reacting to a thermal excursion — with a LOUD, journald-tagged version, so a clamp
# episode ALERTS (dev1-side, off the journal) instead of silently degrading the render. It also
# re-asserts the envelope if something foreign re-programmed PL1. The DECISION is the shared pure
# imag_power_guard_decision (scripts/lib/imag-power-envelope.sh) — never a second copy:
#   TCPU >= IMAG_TCPU_STEPDOWN_C for 2 consecutive reads -> step PL1 down to IMAG_PL1_STEPDOWN_W.
#   TCPU <  IMAG_TCPU_RESTORE_C sustained (2 consecutive) -> restore PL1 to IMAG_PL1_W.
#   live PL1 != expected (foreign re-program) while nominal -> re-assert.
#   otherwise (incl. an unreadable TCPU) -> hold (never a blind step).
# Every transition is `logger -t imag-power-envelope` so `journalctl -t imag-power-envelope` shows
# the whole history. State (streaks + stepped-down flag) is carried in a /run tmpfs file that
# resets on boot — the boot oneshot re-establishes the full envelope, so a fresh boot starts clean.
# PROCHOT remains the hardware backstop underneath all of this. Env knobs: IMAG_PL1_W,
# IMAG_PL1_STEPDOWN_W, IMAG_TCPU_STEPDOWN_C, IMAG_TCPU_RESTORE_C, IMAG_POWER_GUARD_STATE.
# =============================================================================================

for _cand in \
  /usr/local/lib/imag-power-envelope.sh \
  "$(dirname "${BASH_SOURCE[0]}")/lib/imag-power-envelope.sh"; do
  # shellcheck source=scripts/lib/imag-power-envelope.sh disable=SC1090
  [ -r "$_cand" ] && { . "$_cand"; break; }
done
if ! declare -F imag_power_guard_decision >/dev/null 2>&1; then
  logger -t imag-power-envelope -- "FATAL: shared lib imag-power-envelope.sh not found — guard cannot run" 2>/dev/null || true
  echo "imag-power-envelope-guard: FATAL: shared lib not found" >&2
  exit 1
fi

PL1_W="${IMAG_PL1_W:-45}"
STEPDOWN_W="${IMAG_PL1_STEPDOWN_W:-25}"
CEIL_C="${IMAG_TCPU_STEPDOWN_C:-93}"
RESTORE_C="${IMAG_TCPU_RESTORE_C:-85}"
STATE="${IMAG_POWER_GUARD_STATE:-/run/imag-power-envelope-guard.state}"
LOG_TAG="${IMAG_POWER_LOG_TAG:-imag-power-envelope}"

log() { logger -t "$LOG_TAG" -- "$*" 2>/dev/null || true; }

EXPECTED_UW="$(imag_pl1_watts_to_uw "$PL1_W")" || { log "FATAL: invalid IMAG_PL1_W='$PL1_W'"; exit 1; }
STEPDOWN_UW="$(imag_pl1_watts_to_uw "$STEPDOWN_W")" || { log "FATAL: invalid IMAG_PL1_STEPDOWN_W='$STEPDOWN_W'"; exit 1; }

# --- prior state (streaks + stepped-down flag) ------------------------------------------------
HOT=0; COOL=0; STEPPED=0
if [ -r "$STATE" ]; then
  # shellcheck disable=SC1090
  . "$STATE" 2>/dev/null || true
fi
case "$HOT" in ''|*[!0-9]*) HOT=0 ;; esac
case "$COOL" in ''|*[!0-9]*) COOL=0 ;; esac
case "$STEPPED" in 1) : ;; *) STEPPED=0 ;; esac

# --- gather TCPU (x86_pkg_temp, whole Celsius; identity by TYPE, never thermal_zoneN) ----------
TCPU=""
for tz in /sys/class/thermal/thermal_zone*; do
  [ -e "$tz/type" ] || continue
  if [ "$(cat "$tz/type" 2>/dev/null || true)" = "x86_pkg_temp" ]; then
    t="$(cat "$tz/temp" 2>/dev/null || true)"
    [ -n "$t" ] && [ "$t" -eq "$t" ] 2>/dev/null && TCPU=$(( t / 1000 ))
    break
  fi
done

# --- gather live PL1 long_term uW + its write path (identity-based) ---------------------------
CUR_UW=""; PL1_PATH=""
for z in /sys/class/powercap/intel-rapl-mmio:*/; do
  [ -e "${z}name" ] || continue
  [ "$(cat "${z}name" 2>/dev/null || true)" = "package-0" ] || continue
  for cn in "${z}"constraint_*_name; do
    [ -e "$cn" ] || continue
    if [ "$(cat "$cn" 2>/dev/null || true)" = "long_term" ]; then
      idx="${cn##*constraint_}"; idx="${idx%_name}"
      PL1_PATH="${z}constraint_${idx}_power_limit_uw"
      CUR_UW="$(cat "$PL1_PATH" 2>/dev/null || true)"
      break
    fi
  done
  break
done

# --- decide (the shared pure function) --------------------------------------------------------
ACTION="$(imag_power_guard_decision \
  "$CUR_UW" "$EXPECTED_UW" "$STEPDOWN_UW" \
  "$TCPU" "$CEIL_C" "$RESTORE_C" \
  "$HOT" "$COOL" "$STEPPED")"

# --- execute ----------------------------------------------------------------------------------
write_pl1() {  # write_pl1 <uw> ; echoes nothing, logs its own failure
  local uw="$1"
  if [ -z "$PL1_PATH" ] || [ ! -w "$PL1_PATH" ]; then
    log "WARN: PL1 path unavailable/unwritable — cannot apply ${uw}uW"
    return 1
  fi
  echo "$uw" > "$PL1_PATH" 2>/dev/null || { log "WARN: write of ${uw}uW to $PL1_PATH failed"; return 1; }
}

case "$ACTION" in
  stepdown)
    write_pl1 "$STEPDOWN_UW" \
      && log "STEP-DOWN: TCPU=${TCPU}C >= ${CEIL_C}C for 2 consecutive reads — PL1 ${EXPECTED_UW}->${STEPDOWN_UW}uW (=${STEPDOWN_W}W)"
    ;;
  restore)
    write_pl1 "$EXPECTED_UW" \
      && log "RESTORE: TCPU=${TCPU}C < ${RESTORE_C}C sustained — PL1 restored to ${EXPECTED_UW}uW (=${PL1_W}W)"
    ;;
  reassert)
    write_pl1 "$EXPECTED_UW" \
      && log "RE-ASSERT: live PL1=${CUR_UW}uW != expected ${EXPECTED_UW}uW (foreign re-program) — re-applied ${EXPECTED_UW}uW (=${PL1_W}W)"
    ;;
  hold) : ;;
  *) log "WARN: unexpected guard decision '${ACTION}' — holding" ;;
esac

# --- next state (the shared pure streak-bookkeeping function, unit-tested) ---------------------
this_hot=0; this_cool=0
if [ -n "$TCPU" ]; then
  if [ "$TCPU" -ge "$CEIL_C" ]; then this_hot=1
  elif [ "$TCPU" -lt "$RESTORE_C" ]; then this_cool=1
  fi
fi
read -r NEW_HOT NEW_COOL NEW_STEPPED <<< "$(imag_power_guard_next_streaks \
  "$ACTION" "$this_hot" "$this_cool" "$HOT" "$COOL" "$STEPPED")"

_tmp="$(mktemp "${STATE}.XXXXXX" 2>/dev/null || echo "${STATE}.tmp")"
{
  printf 'HOT=%s\n' "$NEW_HOT"
  printf 'COOL=%s\n' "$NEW_COOL"
  printf 'STEPPED=%s\n' "$NEW_STEPPED"
} > "$_tmp" 2>/dev/null && mv -f "$_tmp" "$STATE" 2>/dev/null || rm -f "$_tmp" 2>/dev/null || true

exit 0
