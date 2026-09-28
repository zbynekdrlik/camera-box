#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/render-freeze-alert-watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/audio-mixer-alert-watchdog.sh -- issue 1381: dev1-side ALERT watchdog for an OBS AUDIO
# MIXER leaving real time and for an obs-vban SENDER losing audio.
#
# WHY (issue 1381, 27.9.2026): the resolume cg OBS audio mixer fell behind real time from 06:00
# local (late ticks 22, 36, then 215+ per minute; 2760 / 2868 ticks per minute by 06:09, later
# 1753-2201 and 3857 in the catch-up bursts) and both obs-vban outputs to FOH lost audio. Nothing
# paged: FOH heard it at 06:56, 55 minutes late. Both signals were already in the OBS log.
#
# It reads two facet groups bundle_state_gather exposes on each box's :8899/bundle-state.json:
#   * audio_mixer_* -- the newest complete `audio-stall #1367` dump (ticks, ticks_over, tick_ms,
#     window, in-log age). The MIXER arm pages BEHIND (ticks in the minute more than 5 off real
#     time, a surplus too) or OVERLOADED (more than 30 late ticks in the minute).
#   * vban_pacer_* -- the per-destination obs-vban pacer loss-counter increase inside the last
#     660 s of the log. The VBAN arm pages VBAN_LOSS when any loss counter moved.
# The MIXER arm also pages STALLED (issue 1385): the newest dump is > 180 s behind the log head
# while the log head itself is live on the box's own clock (`obs_log_head_age_s` <= 60 s) -- the
# audio thread stopped while OBS keeps logging, silence on air on a box without VBAN outputs.
# A normal OBS start reads UNKNOWN (one partial dump), never a page; STALE (the same old dump with
# no proof the log is live: OBS down or hung, or a gather without the facet; or the VBAN status
# line stopped) is logged, never paged -- obs-liveness / bundle-state own a dead OBS. A CLOCK page
# (once, stable key) says the pager is blind: the log advances between passes, yet its head reads
# old, so OBS and the gather disagree on the time zone.
#
# PRODUCTION-CRITICAL class (issue 1308): a mixer off real time or a sender losing audio is audible
# on air, so BOTH arms TIME-BUCKET their --dedup-key (watchdog_notify_key) and re-ping "dokolecka"
# while it persists. DETECTION ONLY -- the cure (an OBS restart, a dock or pacer fix) is a
# supervisor/owner call; recovery is machine-channel log-only, never a phone ping (issue 1206).
#
# Roster = the obs-fleet `audio-mixer` facet (strih-lx stream resolume); the traveling resolume is
# polled only while home (obs_fleet_poll_now). A box whose :8899 does not answer is SKIP -- the
# bundle-state / network-reach watchdogs own that page -- so no dev1-outage guard is needed.
#
# Usage:
#   scripts/audio-mixer-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/audio-mixer-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/audio-mixer-alert-watchdog.sh --help
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
    sed -n '5,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "audio-mixer-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOXES="${AUDIO_MIXER_BOXES:-$(obs_fleet_boxes audio-mixer)}"
BUNDLE_PORT="${AUDIO_MIXER_BUNDLE_PORT:-8899}"
BUNDLE_PATH="${AUDIO_MIXER_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${AUDIO_MIXER_CURL_TIMEOUT:-10}"
TOLERANCE="${AUDIO_MIXER_TICK_TOLERANCE:-5}"          # ticks/min off real time (2812.5 at 48 kHz)
OVER_MAX="${AUDIO_MIXER_OVER_MAX:-30}"                # late ticks/min
STALE_AFTER_S="${AUDIO_MIXER_STALE_AFTER_S:-180}"     # a dump older than this: STALLED or STALE
LOG_LIVE_S="${AUDIO_MIXER_LOG_LIVE_S:-60}"            # log head this fresh = OBS still logging (1385)
VBAN_STALE_AFTER_S="${AUDIO_MIXER_VBAN_STALE_AFTER_S:-180}"
CONFIRM_THRESHOLD="${AUDIO_MIXER_CONFIRM_THRESHOLD:-2}"  # 2-pass confirm; one odd pass never fires
ALERT_THROTTLE_PASSES="${AUDIO_MIXER_ALERT_THROTTLE_PASSES:-12}"  # ~1h at the 5-min cadence (render-freeze's)

DECIDE="${AUDIO_MIXER_DECIDE:-$HERE/audio_mixer_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AUDIO_MIXER_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${AUDIO_MIXER_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-audio-mixer-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-audio-mixer-alert-dryrun.state"
STATE_FILE="${AUDIO_MIXER_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [audio-mixer-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body + returns 0 iff a `{`-body came back.
# AUDIO_MIXER_FETCH_CMD (Tier-0 seam): an executable invoked as `<cmd> <ip>` whose stdout REPLACES
# the curl fetch, so a --dry-run can replay a captured bundle-state body with no live box (the
# genlock-lock GENLOCK_LOCK_FETCH_CMD / vb-matrix VB_MATRIX_FETCH_CMD seam). A non-zero exit or a
# non-`{` body still reads as unreachable -> SKIP.
fetch_bundle_json() {
  local ip="$1" body
  if [ -n "${AUDIO_MIXER_FETCH_CMD:-}" ]; then
    body="$("$AUDIO_MIXER_FETCH_CMD" "$ip" 2>/dev/null)" || return 1
  else
    body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
      || return 1
  fi
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

# -- one arm's confirm + throttled, time-bucketed alert -----------------------------------------
# handle_arm <box> <verdict> <state-prefix> <dedup-base> <body-text>
handle_arm() {
  local box="$1" verdict="$2" prefix="$3" dedup_base="$4" body_text="$5"
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "${prefix}_confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${prefix}_confirm_${box}" "${confirm:-0}"
  log "$box $verdict confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box $verdict this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "${prefix}_alerted_${box}" 1

  local prior_sig prior_passes throttle_out alert_now new_sig new_passes
  prior_sig="$(read_state_field "${prefix}_alert_sig_${box}" "")"
  prior_passes="$(read_state_field "${prefix}_alert_passes_${box}" 0)"
  # The signature is the arm + box, never the verdict: BEHIND <-> OVERLOADED flips of one ongoing
  # mixer fault must not re-fire as a "new" incident every pass.
  throttle_out="$(obs_watchdog_alert_throttle "${prefix}:${box}" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "${prefix}_alert_sig_${box}" "$new_sig"
  write_state_field "${prefix}_alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert ($verdict): $box -- $body_text (alert_now=$alert_now)"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box $verdict"
    python3 "$NOTIFY" notify --body "$body_text" \
      --dedup-key "$(watchdog_notify_key "${dedup_base}-${box}" "$(date +%s)")" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify ($verdict) failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still $verdict"
  fi
}

# reset_arm_confirm <box> <prefix> -> zero this arm's pending confirmation only (issue 1385). The
# mixer arm's STALE means the log is not live NOW. The OBS log has no date, so a log that stopped
# days ago reads live for ~70 s once a day; a confirm HELD across STALE would pair two of those
# days into a STALLED page on a dead OBS. Alert state (throttle, alerted flag) is left alone.
reset_arm_confirm() {
  write_state_field "${2}_confirm_${1}" 0
}

# handle_log_clock <box> <ip> <verdict> <head_age> -- issue 1385: the OBS log clock vs the gather
# clock. MISMATCH (the log advances, yet its head reads older than the frozen bound) blinds the whole
# mixer arm (every pass STALE), so it pages ONCE per incident after the 2-pass confirm, with a
# STABLE key -- a config fault, not an on-air one, so no time-bucketed re-ping. OK clears it
# (machine-channel recovery); UNKNOWN holds.
handle_log_clock() {
  local box="$1" ip="$2" verdict="$3" head_age="$4"
  local prev_confirm decision confirm act body
  case "$verdict" in
    MISMATCH)
      prev_confirm="$(read_state_field "audio-mixer-clock_confirm_${box}" 0)"
      decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
      confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
      act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
      write_state_field "audio-mixer-clock_confirm_${box}" "${confirm:-0}"
      log "$box log clock MISMATCH: the log advances but its head reads ${head_age}s old -- the OBS log stamps and the gather clock disagree (time zone?), so the mixer pager is BLIND on this box (confirm=$prev_confirm -> $confirm act=$act)"
      [ "${act:-0}" = "1" ] || return 0
      [ "$(read_state_field "audio-mixer-clock_alerted_${box}" 0)" = "1" ] && return 0
      write_state_field "audio-mixer-clock_alerted_${box}" 1
      body="⚠️ Zvukový mixér ($REPO_SLUG): **$box** ($ip) — strážca zvukového mixéra je na tomto boxe SLEPÝ: OBS log stále pribúda, ale jeho čas sa nezhoduje s hodinami bundle-state servera (log vyzerá ${head_age} s starý, pravdepodobne iné časové pásmo). Kým sa to neopraví, výpadok zvukového vlákna ani mixér mimo reálneho času sa z tohto boxu nenahlási."
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD alert (CLOCK): $box -- $body"
        return 0
      fi
      log "ALERT: firing Discord notification for $box CLOCK"
      python3 "$NOTIFY" notify --body "$body" --dedup-key "audio-mixer-clock-${box}" >/dev/null 2>&1 || log "ALERT: airuleset.py notify (CLOCK) failed (non-fatal)"
      ;;
    OK)
      if [ "$(read_state_field "audio-mixer-clock_alerted_${box}" 0)" = "1" ]; then
        log "RECOVERY: $box log clock agrees again -- machine-channel only (issue 1206: recovery is not a phone ping)"
        write_state_field "audio-mixer-clock_alerted_${box}" 0
      fi
      write_state_field "audio-mixer-clock_confirm_${box}" 0
      ;;
    *) : ;;   # UNKNOWN: not judgeable this pass (no previous pass / a timer gap) -- hold
  esac
}

# handle_healthy_arm <box> <prefix> -> clear this arm's confirm + throttle and log a machine-channel
# recovery line once if it had paged. Recovery is NEVER a phone ping (issue 1206).
handle_healthy_arm() {
  local box="$1" prefix="$2"
  if [ "$(read_state_field "${prefix}_alerted_${box}" 0)" = "1" ]; then
    log "RECOVERY: $box $prefix back to normal -- machine-channel only (issue 1206: recovery is not a phone ping)"
    write_state_field "${prefix}_alerted_${box}" 0
  fi
  write_state_field "${prefix}_confirm_${box}" 0
  write_state_field "${prefix}_alert_sig_${box}" ""
  write_state_field "${prefix}_alert_passes_${box}" 0
}

# field <analyze-output> <key> -> the value of `key=` in the decision's key=value output.
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }

# -- per-box decision --------------------------------------------------------------------------
handle_box() {
  local box="$1" ip="$2" body reachable out now_epoch prev_head prev_epoch log_clock
  local mverdict ticks over tick_ms age head_age expected dev window
  local -a clock_args=()
  local vverdict events loss_ms dest vage loss_txt

  if body="$(fetch_bundle_json "$ip")"; then reachable=1; else reachable=0; body=""; fi
  # issue 1385: the previous pass's log head age feeds the log-clock check (AUDIO_MIXER_NOW_EPOCH is
  # the Tier-0 seam for the pass time).
  now_epoch="${AUDIO_MIXER_NOW_EPOCH:-$(date +%s)}"
  prev_head="$(read_state_field "audio-mixer-clock_prev_age_${box}" "")"
  prev_epoch="$(read_state_field "audio-mixer-clock_prev_epoch_${box}" "")"
  if [ -n "$prev_head" ] && [ -n "$prev_epoch" ]; then
    clock_args=(--prev-log-head-age-s "$prev_head" --pass-gap-s "$(( now_epoch - prev_epoch ))")
  fi
  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" \
    --tolerance "$TOLERANCE" --over-max "$OVER_MAX" --stale-after-s "$STALE_AFTER_S" \
    --vban-stale-after-s "$VBAN_STALE_AFTER_S" --log-live-s "$LOG_LIVE_S" \
    ${clock_args[@]+"${clock_args[@]}"} 2>/dev/null)"
  mverdict="$(field "$out" mixer_verdict)"; ticks="$(field "$out" ticks)"
  over="$(field "$out" ticks_over)"; tick_ms="$(field "$out" tick_ms)"; age="$(field "$out" age_s)"
  head_age="$(field "$out" log_head_age_s)"
  log_clock="$(field "$out" log_clock)"
  write_state_field "audio-mixer-clock_prev_age_${box}" "$head_age"
  write_state_field "audio-mixer-clock_prev_epoch_${box}" "$([ -n "$head_age" ] && printf '%s' "$now_epoch")"
  expected="$(field "$out" expected_per_min)"
  dev="$(field "$out" deviation_per_min)"; window="$(field "$out" window_ms)"
  vverdict="$(field "$out" vban_verdict)"; events="$(field "$out" vban_events)"
  loss_ms="$(field "$out" vban_loss_ms)"; dest="$(field "$out" vban_dest)"
  vage="$(field "$out" vban_age_s)"
  log "$box ($ip): reachable=$reachable mixer=${mverdict:-<none>} ticks=${ticks} over=${over} tick_ms=${tick_ms} window_ms=${window} age=${age} log_head_age=${head_age} dev=${dev} | vban=${vverdict:-<none>} events=${events} loss_ms=${loss_ms} dest=${dest} age=${vage}"

  # MIXER arm
  case "$mverdict" in
    SKIP)    log "$box mixer: :$BUNDLE_PORT not fetchable -- bundle-state / reach watchdog territory; holding, no page" ;;
    UNKNOWN) log "$box mixer: no complete audio-stall dump (fresh OBS start / older build) -- holding, no page" ;;
    STALE)   log "$box mixer: STALE -- newest audio-stall dump ${age}s behind the log head, but the log head is not live (${head_age:-no}s old: OBS down / hung / older gather) -- obs-liveness territory, machine channel only, no page"
             reset_arm_confirm "$box" "audio-mixer" ;;
    STALLED) handle_arm "$box" "STALLED" "audio-mixer" "audio-mixer" \
      "🚨 Zvukový mixér ($REPO_SLUG): **$box** ($ip) — zvukové vlákno OBS stojí: posledný zvukový záznam je aspoň **${age} s** starý, hoci OBS stále zapisuje log (pred ${head_age} s). Z tohto OBS nejde zvuk (ticho vo vysielaní). Treba zistiť príčinu a rozhodnúť o reštarte OBS. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    HEALTHY) handle_healthy_arm "$box" "audio-mixer" ;;
    BEHIND)  handle_arm "$box" "BEHIND" "audio-mixer" "audio-mixer" \
      "🚨 Zvukový mixér ($REPO_SLUG): **$box** ($ip) — OBS mixér nebeží v reálnom čase: **${ticks} tickov za minútu** (reálny čas ${expected}, odchýlka ${dev}), oneskorených tickov ${over}/min. Zvuk z tohto OBS sa trhá alebo dobieha (27.9.: FOH výpadky celú hodinu). Treba zistiť príčinu (záťaž zvukového vlákna, dock) a rozhodnúť o reštarte OBS. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    OVERLOADED) handle_arm "$box" "OVERLOADED" "audio-mixer" "audio-mixer" \
      "🚨 Zvukový mixér ($REPO_SLUG): **$box** ($ip) — zvukové vlákno OBS nestíha: **${over} oneskorených tickov za minútu** (prah ${OVER_MAX}), tickov ${ticks}/min. Takto začal 27.9. o 06:00 výpadok zvuku na FOH. Treba zistiť príčinu (záťaž zvukového vlákna, dock) a rozhodnúť o reštarte OBS. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    *)       log "$box mixer: unexpected verdict '${mverdict:-<empty>}' -- holding, no page" ;;
  esac

  handle_log_clock "$box" "$ip" "${log_clock:-UNKNOWN}" "$head_age"

  # VBAN arm (independent; disjoint state)
  case "$vverdict" in
    SKIP)    : ;;   # already logged for the mixer arm (same fetch)
    UNKNOWN) log "$box vban: no obs-vban pacing line (no VBAN output on this box) -- holding, no page" ;;
    STALE)   log "$box vban: STALE -- newest pacing line ${vage}s behind the log head (output stopped?) -- machine channel only, no page" ;;
    HEALTHY) handle_healthy_arm "$box" "vban-loss" ;;
    VBAN_LOSS)
      loss_txt=""
      [ -n "$loss_ms" ] && loss_txt=", ${loss_ms} ms ticha/zahodeného zvuku"
      handle_arm "$box" "VBAN_LOSS" "vban-loss" "vban-loss" \
        "🚨 VBAN výpadok ($REPO_SLUG): **$box** ($ip) — VBAN výstup **${dest}** stratil zvuk za posledných ~11 min: **${events} udalostí straty**${loss_txt} (podtečenie/pretečenie/orezanie alebo ticho/preskok). Príjemca (FOH/lv1) počuje výpadky. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    *)       log "$box vban: unexpected verdict '${vverdict:-<empty>}' -- holding, no page" ;;
  esac
}

# require_tools -> exit non-zero (loud) if curl/python3 or the decision module is missing (a silent
# SKIP-forever would leave a real fault unpaged -- the issue-833 preflight class).
require_tools() {
  local missing=() t
  for t in curl python3; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (fix AUDIO_MIXER_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, tolerance=${TOLERANCE}/min, over_max=${OVER_MAX}/min, boxes='$BOXES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }
  local pair box ip
  for pair in $BOXES; do
    box="${pair%%|*}"; ip="${pair##*|}"
    if ! obs_fleet_poll_now "$box"; then
      log "$box ($ip): away / retired -- not polled this pass (no fetch, no page)"
      continue
    fi
    handle_box "$box" "$ip"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
