#!/usr/bin/env bash
# scripts/stream-output-stuck-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so its pass
# runs with -e OFF (the family convention, .claude/rules/watchdog-common.md); `set +e` is re-applied
# after the source lines below, since a sourced lib could turn -e back on.
#
# scripts/stream-output-stuck-alert-watchdog.sh -- issue 1460: page the owner when the stream OBS
# output is STUCK during a production.
#
# The owner's ruling (ROZHODNUTÉ 12.11.2026), verbatim: "na discord do alerts keď je scéna PRO na
# stream obs a je zapnuté streamovanie, vtedy má prísť správa do discordu, že sa zasekol výstup".
# So it pages ONLY while ALL hold:
#   * the stream OBS program scene is the production scene (STREAM_PRODUCTION_SCENE_DEFAULT from
#     scripts/lib/stream-dev-scene.sh, never retyped here),
#   * the stream output is active (GetStreamStatus.outputActive in both snapshots),
#   * the output is stuck: over a few seconds the program render (GetStats.renderTotalFrames), the
#     stream frames (outputTotalFrames) or the bytes sent (outputBytes) stood still, or the output is
#     reconnecting -- confirmed over 2 consecutive passes.
# Outside that state it never pages. An unreachable box (SKIP) or an unread fact (UNKNOWN) is never a
# guessed production -- EXCEPT within STREAM_OUTPUT_STUCK_CARRY_S (600 s) of a pass that PROVED
# production + streaming: then a stream OBS that stops answering is a stuck output too (a frozen OBS
# answers nothing, and network-reach sees a box that still pings), reason `unresponsive`.
#
# HOW: every 60 s the READ-ONLY probe scripts/stream_output_stuck_probe.py takes two snapshots ~3 s
# apart in one obs-websocket session (through obs_phase2's client, so it never selects a scene), the
# PURE scripts/stream_output_stuck_decision.py grades them (pytest:
# tests/python/test_stream_output_stuck_1460.py), and this orchestrator confirms + pages.
#
# DELIVERY: the airuleset `page:` class (`page:<box>-output-stuck:<bucket>`), the only class the
# owner policy lets through to the phone (issue 1495, airuleset 0.1.626: at most one message per check
# per 10 min while it lasts) -- the same path as the issue-1495 "stream bez zvuku" page. The bucket
# comes from the ONE shared helper watchdog_notify_key. Recovery is ONE machine-channel log line,
# never a phone ping (#1206).
#
# DETECTION ONLY -- no auto-action (restarting the production OBS is the owner's call). Ships
# DISABLED (systemd/stream-output-stuck-alert-watchdog.README.md).
#
# Usage:
#   scripts/stream-output-stuck-alert-watchdog.sh            # one pass: probe -> decide -> page
#   scripts/stream-output-stuck-alert-watchdog.sh --dry-run  # probe + decide + LOG only; never page
#   scripts/stream-output-stuck-alert-watchdog.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/watchdog-common.sh
. "$HERE/lib/watchdog-common.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/stream-dev-scene.sh
. "$HERE/lib/stream-dev-scene.sh"
set +e
set -uo pipefail

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '8,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "stream-output-stuck-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOX="${STREAM_OUTPUT_STUCK_BOX:-stream-snv}"
WS_HOST="${STREAM_OUTPUT_STUCK_WS_HOST:-$(obs_fleet_host "$BOX" 2>/dev/null || true)}"
WS_PORT="${STREAM_OUTPUT_STUCK_WS_PORT:-4455}"
WINDOW_S="${STREAM_OUTPUT_STUCK_WINDOW_S:-3}"
PRODUCTION_SCENE="${STREAM_OUTPUT_STUCK_PRODUCTION_SCENE:-$STREAM_PRODUCTION_SCENE_DEFAULT}"
CONFIRM_THRESHOLD="${STREAM_OUTPUT_STUCK_CONFIRM_THRESHOLD:-2}"
REPING_INTERVAL_S="${STREAM_OUTPUT_STUCK_REPING_INTERVAL_S:-600}"
CARRY_S="${STREAM_OUTPUT_STUCK_CARRY_S:-600}"

DECIDE="${STREAM_OUTPUT_STUCK_DECIDE:-$HERE/stream_output_stuck_decision.py}"
PROBE="${STREAM_OUTPUT_STUCK_PROBE:-$HERE/stream_output_stuck_probe.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"

STATE_DIR="${STREAM_OUTPUT_STUCK_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-stream-output-stuck-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-stream-output-stuck-alert-dryrun.state"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
STATE_FILE="${STREAM_OUTPUT_STUCK_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [stream-output-stuck-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# now_epoch -> seconds since the epoch (injectable for tests via STREAM_OUTPUT_STUCK_NOW).
now_epoch() { printf '%s' "${STREAM_OUTPUT_STUCK_NOW:-$(date +%s)}"; }

# fetch_probe -> the probe's key=value stdout; rc 0 iff the WS connected (box reachable).
# STREAM_OUTPUT_STUCK_FETCH_CMD (test seam): an executable run as `<cmd> <host> <port>` in place of
# the real probe, so a --dry-run replays a fixture with no live box.
fetch_probe() {
  if [ -n "${STREAM_OUTPUT_STUCK_FETCH_CMD:-}" ]; then
    "$STREAM_OUTPUT_STUCK_FETCH_CMD" "$WS_HOST" "$WS_PORT"
    return $?
  fi
  timeout 45 python3 "$PROBE" "$WS_HOST" "$WS_PORT" --window-s "$WINDOW_S"
}

# fire_page <body...> -- the ONE page seam: the airuleset page: class, time-bucketed by the shared
# helper (watchdog_notify_key must stay on the notify line, the #1206 sweep checks it).
fire_page() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD page (page:${BOX}-output-stuck): $*"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$*" --dedup-key "page:${BOX}-output-stuck:$(watchdog_notify_key "output-stuck-${BOX}" "$(now_epoch)" "$REPING_INTERVAL_S")" \
    >/dev/null 2>&1 || log "PAGE: airuleset.py notify failed (non-fatal)"
}

# confirm_then_page <fault 0|1> <body...> -- 2-pass confirm; once confirmed, pages every pass (the
# airuleset page: window, not a per-pass throttle, sets the re-ping cadence). A cleared fault logs one
# machine-channel recovery line (#1206), never a phone ping.
confirm_then_page() {
  local fault="$1"; shift
  local prev decision confirm act was_alerted
  prev="$(read_state_field "confirm_${BOX}" 0)"
  decision="$(obs_watchdog_confirm "$prev" "$fault" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${BOX}" "${confirm:-0}"
  if [ "$fault" != "1" ]; then
    was_alerted="$(read_state_field "alerted_${BOX}" 0)"
    if [ "$(recovery_latch_fires "$was_alerted")" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: $BOX output moving again"
      else
        log "RECOVERY: $BOX output moving again -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${BOX}" 0
    fi
    return 0
  fi
  if [ "${act:-0}" != "1" ]; then
    log "$BOX output STUCK this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "alerted_${BOX}" 1
  fire_page "$@"
}

# require_tools -> loud exit when python3 or the decision module is missing (a silent SKIP forever
# would never page a real stuck output, the #833 class).
require_tools() {
  if ! command -v python3 >/dev/null 2>&1; then
    log "FATAL: python3 not found on dev1 -- refusing to run"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run"
    return 1
  fi
  if ! [[ "$BOX" =~ ^[abcdefghijklmnopqrstuvwxyz0123456789][abcdefghijklmnopqrstuvwxyz0123456789.-]*$ ]]; then
    log "FATAL: box name '$BOX' is not a valid airuleset page check name (lower-case letters, digits, '.', '-') -- the page would be suppressed; refusing to run"
    return 1
  fi
  if [ -z "$WS_HOST" ]; then
    log "FATAL: no host for box '$BOX' in the obs fleet list -- refusing to run (set STREAM_OUTPUT_STUCK_WS_HOST)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, box=$BOX ws=${WS_HOST}:${WS_PORT}, production scene='$PRODUCTION_SCENE', window=${WINDOW_S}s, confirm=$CONFIRM_THRESHOLD)"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  local body reachable out verdict reasons scene rd ed bd now last_prod age=""
  if body="$(fetch_probe)"; then reachable=1; else reachable=0; body=""; fi
  now="$(now_epoch)"
  last_prod="$(read_state_field "last_production_${BOX}" "")"
  [[ "$last_prod" =~ ^[0123456789]+$ ]] && age=$(( now - last_prod ))
  # the decision's stderr reaches the journal (a traceback must never read as a silent verdict)
  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" \
    --production-scene "$PRODUCTION_SCENE" ${age:+--last-production-age-s "$age"} --carry-s "$CARRY_S")"
  verdict="$(printf '%s\n' "$out" | sed -n 's/^verdict=//p')"
  reasons="$(printf '%s\n' "$out" | sed -n 's/^reasons=//p')"
  scene="$(printf '%s\n' "$out" | sed -n 's/^scene=//p')"
  rd="$(printf '%s\n' "$out" | sed -n 's/^render_delta=//p')"
  ed="$(printf '%s\n' "$out" | sed -n 's/^encode_delta=//p')"
  bd="$(printf '%s\n' "$out" | sed -n 's/^bytes_delta=//p')"
  log "$BOX: reachable=$reachable verdict=${verdict:-<none>} reasons=${reasons:-} scene='${scene}' render+${rd} frames+${ed} bytes+${bd} over ${WINDOW_S}s (last proven production ${age:-never}s ago)"

  # A pass that READ production + streaming is the proof the unresponsive carry keys on; a pass that
  # read another scene or no stream drops it.
  case "$verdict:$reasons" in
    OK:* | STUCK:render* | STUCK:encode* | STUCK:bytes* | STUCK:reconnecting*) write_state_field "last_production_${BOX}" "$now" ;;
    NOT_PRODUCTION:*) write_state_field "last_production_${BOX}" "" ;;
  esac

  case "$verdict" in
    STUCK)
      if [ "$reasons" = "unresponsive" ]; then
        confirm_then_page 1 \
          "🚨 ZASEKNUTÝ VÝSTUP: stream OBS na $BOX neodpovedá (pred ${age} s vysielal scénu $PRODUCTION_SCENE), potvrdené ${CONFIRM_THRESHOLD}x po minúte. Skontroluj stream OBS na $BOX."
      else
        confirm_then_page 1 \
        "🚨 ZASEKNUTÝ VÝSTUP: $BOX vysiela scénu $PRODUCTION_SCENE, ale výstup stojí (${reasons}: obraz +${rd}, snímky streamu +${ed}, bajty +${bd} za ${WINDOW_S} s, potvrdené ${CONFIRM_THRESHOLD}x po minúte). Skontroluj stream OBS na $BOX (obraz programu, stav streamu)."
      fi
      ;;
    OK | NOT_PRODUCTION) confirm_then_page 0 ;;
    SKIP) log "$BOX not reachable on :$WS_PORT and no production proven in the last ${CARRY_S}s -- the network-reach watchdog's page, holding" ;;
    UNKNOWN) log "$BOX: a fact was unread (${reasons}) -- no verdict, holding, no page" ;;
    *) log "$BOX: unexpected verdict '${verdict:-<empty>}' (decision failed?) -- holding, no page" ;;
  esac
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
