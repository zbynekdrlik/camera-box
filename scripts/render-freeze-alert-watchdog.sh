#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/audio-lag-alert-watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/render-freeze-alert-watchdog.sh -- #1320: dev1-side ALERT watchdog for a RECURRENCE of the
# strih PROGRAM render freeze (and its receiver-side relock storm).
#
# WHY (#1320 / issue 1318, live 15.9.2026): a scene-switch-coincident DistroAV reattach whose
# blocking NDIlib_recv_destroy ran on strih's OBS GRAPHICS thread froze the PROGRAM render ~7.5 s
# (`program-render-audit lagged=228 avg_frame_ms=782`) -> the `2ME PGM` NDI output was starved -> the
# stream receive FIFO underran -> a 462-relock overshoot STORM -> the on-air video sat +2/+3 frames
# late for ~40 min. The owner only noticed ~90 min later via the av-sync dock offset. The cure
# (bundle 02b53180b) moves the teardown off the graphics thread; THIS watchdog is the guardrail that
# pages in ~10 min if the freeze -- or the storm -- ever recurs, instead of a silent 90-min drift.
#
# It reads two bundle-state facets bundle_state_gather exposes on each box's :8899/bundle-state.json:
#   * program_render_lagged (+ _age_s) -- a `lagged>0` window == a PROGRAM render-thread freeze. The
#     RENDER arm pages RENDER_FREEZE on lagged >= a magnitude FLOOR (default 30: above the relaunch
#     startup-lag band 1/2/11, below the smallest genuine freeze 61/228) AND a fresh age.
#   * relock_bursts (+ _age_s) -- issue 1318's summarize_relock_bursts (>=8 relocks within 1 s on an
#     input). The RELOCK arm pages RELOCK_STORM on bursts >= 1 AND a fresh age.
#
# PRODUCTION-CRITICAL class (#1308): a recurrence silently desyncs the on-air A/V, so BOTH arms
# TIME-BUCKET their --dedup-key (watchdog_notify_key) -> they re-ping "dokolecka" while the state
# persists (within a bucket they card-edit, no flood). DETECTION ONLY -- the cure is a full-bundle
# redeploy / an OBS relaunch (a supervisor/owner call), so there is NO auto-action; recovery is
# machine-channel log-only, never a phone ping (.claude/rules/watchdog-notify-dedup.md #1206).
#
# NO reference-anchor / dev1-side-outage guard is needed: the ONLY page condition is a SUCCESSFULLY
# FETCHED positive reading, so a dev1-side path outage / a dark box -> box_reachable=0 -> SKIP -> no
# page (a box/:8899-down page is #732/#1001 territory). This is why resolume (traveling) is safe in
# the roster with no is_home gate.
#
# Usage:
#   scripts/render-freeze-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/render-freeze-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/render-freeze-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "render-freeze-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOXES="${RENDER_FREEZE_BOXES:-$(obs_fleet_boxes render-freeze)}"
BUNDLE_PORT="${RENDER_FREEZE_BUNDLE_PORT:-8899}"
BUNDLE_PATH="${RENDER_FREEZE_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${RENDER_FREEZE_CURL_TIMEOUT:-10}"
LAGGED_FLOOR="${RENDER_FREEZE_LAGGED_FLOOR:-30}"           # relaunch band 1/2/11 vs freeze 61/228
RENDER_FRESH_AGE_S="${RENDER_FREEZE_FRESH_AGE_S:-600}"     # a freeze older than this is stale (no page)
MIN_BURSTS="${RENDER_FREEZE_MIN_BURSTS:-1}"
RELOCK_FRESH_AGE_S="${RENDER_FREEZE_RELOCK_FRESH_AGE_S:-600}"
CONFIRM_THRESHOLD="${RENDER_FREEZE_CONFIRM_THRESHOLD:-2}"  # 2-pass confirm; a blipped parse never fires
ALERT_THROTTLE_PASSES="${RENDER_FREEZE_ALERT_THROTTLE_PASSES:-12}"  # ~1h at the 5-min cadence

DECIDE="${RENDER_FREEZE_DECIDE:-$HERE/render_freeze_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${RENDER_FREEZE_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${RENDER_FREEZE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-render-freeze-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-render-freeze-alert-dryrun.state"
STATE_FILE="${RENDER_FREEZE_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [render-freeze-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body + returns 0 iff a 200 with a `{`-body came back.
fetch_bundle_json() {
  local ip="$1" body
  body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
    || return 1
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace; a non-{ body -> SKIP (safe)
  case "$body" in
    \{*) printf '%s' "$body"; return 0 ;;
    *) return 1 ;;
  esac
}

# -- persisted per-box state (key=value lines) --------------------------------------------------
read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# recovery_now <was_alerted> -> "1" iff a recovery latch should fire (was alerted, now healthy).
recovery_now() { [ "${1:-0}" = "1" ] && printf '1' || printf '0'; }

# -- one arm's confirm + throttled, time-bucketed alert -----------------------------------------
# handle_arm <box> <arm-tag> <state-prefix> <dedup-base> <body-text>
# arm-tag identifies the incident for the log; state-prefix isolates this arm's disjoint state keys;
# dedup-base is the time-bucketed --dedup-key base; body-text is the Slovak page (already carries the box+ip).
handle_arm() {
  local box="$1" arm="$2" prefix="$3" dedup_base="$4" body_text="$5"
  # confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "${prefix}_confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${prefix}_confirm_${box}" "${confirm:-0}"
  log "$box $arm confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box $arm this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "${prefix}_alerted_${box}" 1

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${prefix}:${box}"
  prior_sig="$(read_state_field "${prefix}_alert_sig_${box}" "")"
  prior_passes="$(read_state_field "${prefix}_alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "${prefix}_alert_sig_${box}" "$new_sig"
  write_state_field "${prefix}_alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert ($arm): $box -- $body_text (alert_now=$alert_now)"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box $arm"
    python3 "$NOTIFY" notify --body "$body_text" \
      --dedup-key "$(watchdog_notify_key "${dedup_base}-${box}" "$(date +%s)")" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify ($arm) failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still $arm"
  fi
}

# handle_healthy_arm <box> <prefix> -> clear this arm's throttle + fire a machine-channel recovery
# line once if we had paged. Recovery is NEVER a phone ping (#1206).
handle_healthy_arm() {
  local box="$1" prefix="$2" was_alerted
  was_alerted="$(read_state_field "${prefix}_alerted_${box}" 0)"
  if [ "$(recovery_now "$was_alerted")" = "1" ]; then
    log "RECOVERY: $box $prefix back to normal -- machine-channel only (#1206: recovery is not a phone ping)"
    write_state_field "${prefix}_alerted_${box}" 0
  fi
  write_state_field "${prefix}_confirm_${box}" 0
  write_state_field "${prefix}_alert_sig_${box}" ""
  write_state_field "${prefix}_alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
handle_box() {
  local box="$1" ip="$2" body reachable out
  local rverdict lagged lagged_age zverdict bursts bursts_age

  if body="$(fetch_bundle_json "$ip")"; then reachable=1; else reachable=0; body=""; fi
  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" \
    --lagged-floor "$LAGGED_FLOOR" --render-fresh-age-s "$RENDER_FRESH_AGE_S" \
    --min-bursts "$MIN_BURSTS" --relock-fresh-age-s "$RELOCK_FRESH_AGE_S" 2>/dev/null)"
  rverdict="$(printf '%s\n' "$out" | sed -n 's/^render_verdict=//p')"
  lagged="$(printf '%s\n' "$out" | sed -n 's/^lagged=//p')"
  lagged_age="$(printf '%s\n' "$out" | sed -n 's/^lagged_age_s=//p')"
  zverdict="$(printf '%s\n' "$out" | sed -n 's/^relock_verdict=//p')"
  bursts="$(printf '%s\n' "$out" | sed -n 's/^bursts=//p')"
  bursts_age="$(printf '%s\n' "$out" | sed -n 's/^bursts_age_s=//p')"
  log "$box ($ip): reachable=$reachable render=${rverdict:-<none>} lagged=${lagged:-} age=${lagged_age:-} | relock=${zverdict:-<none>} bursts=${bursts:-} age=${bursts_age:-} (floor=${LAGGED_FLOOR}, fresh=${RENDER_FRESH_AGE_S}s)"

  # RENDER arm
  case "$rverdict" in
    SKIP)          log "$box render :$BUNDLE_PORT not fetchable -- #732/#1001 territory; holding, no page" ;;
    UNKNOWN)       log "$box render: no program_render_lagged facet (stock OBS / no audit line yet) -- holding, no page" ;;
    HEALTHY)       handle_healthy_arm "$box" "render-freeze" ;;
    RENDER_FREEZE) handle_arm "$box" "RENDER_FREEZE" "render-freeze" "render-freeze" \
      "🚨 Render freeze ($REPO_SLUG): **$box** ($ip) — strih PROGRAM render freeze: **lagged=${lagged}** v poslednom okne (pred ~${lagged_age}s). Scéna/NDI reattach zablokoval render vlákno → 2ME PGM hladuje → relock búrka na streame → obraz +2/+3 snímky pozadu. Toto je RECIDÍVA opravy z bundle 02b53180b — over genlock build na boxe a či treba full-bundle redeploy / reštart OBS (owner rozhodnutie). Prah lagged>=${LAGGED_FLOOR}, potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    *)             log "$box render: unexpected verdict '${rverdict:-<empty>}' -- holding, no page" ;;
  esac

  # RELOCK arm (independent; disjoint state)
  case "$zverdict" in
    SKIP)         : ;;   # already logged for the render arm (same fetch)
    UNKNOWN)      log "$box relock: no relock_bursts facet (steady state, no storm) -- holding, no page" ;;
    HEALTHY)      handle_healthy_arm "$box" "relock-storm" ;;
    RELOCK_STORM) handle_arm "$box" "RELOCK_STORM" "relock-storm" "relock-storm" \
      "🚨 Relock storm ($REPO_SLUG): **$box** ($ip) — relock búrka na NDI vstupe: **${bursts} burst(ov)** (>=8 relockov za 1 s, pred ~${bursts_age}s). Prijímacie FIFO prestrelilo hĺbku po hladovaní sendra (strih render freeze) → obraz putuje pozadu. Súvisí s issue 1318 / render freeze na sendri; over strih program-render-audit + či treba reštart. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    *)            log "$box relock: unexpected verdict '${zverdict:-<empty>}' -- holding, no page" ;;
  esac
}

# require_tools -> exit non-zero (loud) if curl/python3 or the decision module is missing (a silent
# SKIP-forever would leave a real freeze unpaged -- the imag-ssh-remote-tool-preflight #833 class).
require_tools() {
  local missing=() t
  for t in curl python3; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (fix RENDER_FREEZE_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, floor=${LAGGED_FLOOR}, fresh=${RENDER_FRESH_AGE_S}s, boxes='$BOXES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }
  local pair box ip
  for pair in $BOXES; do
    box="${pair%%|*}"; ip="${pair##*|}"
    handle_box "$box" "$ip"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
