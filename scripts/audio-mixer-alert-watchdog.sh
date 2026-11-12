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
#   * audio_buffering_* -- the audio mix buffering (`audio-telemetry #800: total_buffering=`) and
#     its session start (the launch's own draw since issue 1425; the genlock floor line before). The
#     BUFFERING arm pages RAISED (issue 1381) when it
#     sits at least one mixer tick above the start: a stall of the audio callback ratcheted it, and
#     the whole output stays that much later until an OBS restart (15.10.2026: 85 -> 170 ms).
#   * audio_mix_* -- the mix-wide `audio-telemetry #800 mix: mixed_late=` count (issue 1460),
#     cumulative since the audio reset. The MIX-LATE arm stores the previous pass's count per box
#     and pages LATE when it rose: a mix member fell behind the frozen mix window and was
#     re-anchored, an audible hole on air. A lower count (new audio session) only re-baselines;
#     a changed `audio_mix_session` (the audio reset's stamp) grades the new session from 0.
#   * audio_rx -- the program-audio RECEIVE facet (issue 1460, nested): per receiver the loss counter
#     deltas of the last 660 s. The PROGRAM-RX arm grades the receiver the box FACTS name
#     (scripts/lib/program-audio-rx.sh: the obs-vban source on stream-pp / strih-pp, the Dante input on
#     strih-snv / stream-snv; none on resolume) and pages LOSS when a gated counter moved (lost incl.
#     concealed slots, re_anchors, emit_missed; the Dante loss counters).
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
# Roster = the obs-fleet `audio-mixer` facet (strih-snv stream resolume); the traveling resolume is
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
# shellcheck source=scripts/lib/watchdog-common.sh
. "$HERE/lib/watchdog-common.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/program-audio-rx.sh
. "$HERE/lib/program-audio-rx.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,54p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "audio-mixer-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOXES="${AUDIO_MIXER_BOXES:-$(obs_fleet_boxes audio-mixer)}"
BUNDLE_PORT="${AUDIO_MIXER_BUNDLE_PORT:-8899}"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
BUNDLE_PATH="${AUDIO_MIXER_BUNDLE_PATH:-/bundle-state.json}"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
CURL_TIMEOUT="${AUDIO_MIXER_CURL_TIMEOUT:-10}"
TOLERANCE="${AUDIO_MIXER_TICK_TOLERANCE:-5}"          # ticks/min off real time (2812.5 at 48 kHz)
OVER_MAX="${AUDIO_MIXER_OVER_MAX:-30}"                # late ticks/min
STALE_AFTER_S="${AUDIO_MIXER_STALE_AFTER_S:-180}"     # a dump older than this: STALLED or STALE
LOG_LIVE_S="${AUDIO_MIXER_LOG_LIVE_S:-60}"            # log head this fresh = OBS still logging (1385)
VBAN_STALE_AFTER_S="${AUDIO_MIXER_VBAN_STALE_AFTER_S:-180}"
CONFIRM_THRESHOLD="${AUDIO_MIXER_CONFIRM_THRESHOLD:-2}"  # 2-pass confirm; one odd pass never fires
ALERT_THROTTLE_PASSES="${AUDIO_MIXER_ALERT_THROTTLE_PASSES:-12}"  # ~1h at the 5-min cadence (render-freeze's)
# issue 1460: the PROGRAM-RX arm SHIPS DISABLED inside this already-enabled watchdog -- it grades and
# logs every pass, but pages only with AUDIO_MIXER_PROGRAM_RX_PAGE=1 (the supervisor's unit drop-in
# after a live dry-run, systemd/audio-mixer-alert-watchdog.README.md).
PROGRAM_RX_PAGE="${AUDIO_MIXER_PROGRAM_RX_PAGE:-0}"

DECIDE="${AUDIO_MIXER_DECIDE:-$HERE/audio_mixer_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AUDIO_MIXER_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${AUDIO_MIXER_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-audio-mixer-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-audio-mixer-alert-dryrun.state"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
STATE_FILE="${AUDIO_MIXER_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [audio-mixer-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> AUDIO_MIXER_FETCH_CMD (scripts/lib/watchdog-common.sh) -> prints the JSON
# body + returns 0 iff a `{`-body came back. AUDIO_MIXER_FETCH_CMD (Tier-0 seam): an executable
# invoked as `<cmd> <ip>` whose stdout REPLACES the curl fetch, so a --dry-run can replay a captured
# bundle-state body with no live box (the genlock-lock GENLOCK_LOCK_FETCH_CMD / vb-matrix
# VB_MATRIX_FETCH_CMD seam). A non-zero exit or a non-`{` body still reads as unreachable -> SKIP.

# -- persisted per-box state (key=value lines) --------------------------------------------------
# read_state_field / write_state_field live in scripts/lib/watchdog-common.sh.

# -- one arm's confirm + throttled, time-bucketed alert -----------------------------------------
# handle_arm <box> <verdict> <state-prefix> <dedup-base> <body-text> [threshold] -> rc 0 when the
# verdict is CONFIRMED this pass (paged, throttled or a dry-run WOULD alert), rc 1 while still holding
# for the confirm (issue 1460: the MIX-LATE arm advances its stored count on this return). The
# threshold defaults to CONFIRM_THRESHOLD; the PROGRAM-RX arm passes 1 (a counter delta is a fact).
handle_arm() {
  local box="$1" verdict="$2" prefix="$3" dedup_base="$4" body_text="$5"
  local threshold="${6:-$CONFIRM_THRESHOLD}"
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "${prefix}_confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$threshold")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${prefix}_confirm_${box}" "${confirm:-0}"
  log "$box $verdict confirm=$prev_confirm -> $confirm act=$act (threshold=$threshold)"
  if [ "${act:-0}" != "1" ]; then
    log "$box $verdict this pass but not yet CONFIRMED across $threshold passes -- holding"
    return 1
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
  return 0
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
  local bverdict bnow bstart bdelta bticks bsrc bage src_txt
  local mlverdict mlnow mlprev mlage mlsess mlsess_prev
  local -a mix_args=() rx_args=()
  local rxkey rxrc=0 rxverdict

  if body="$(fetch_bundle_json "$ip" AUDIO_MIXER_FETCH_CMD)"; then reachable=1; else reachable=0; body=""; fi
  # issue 1385: the previous pass's log head age feeds the log-clock check (AUDIO_MIXER_NOW_EPOCH is
  # the Tier-0 seam for the pass time).
  now_epoch="${AUDIO_MIXER_NOW_EPOCH:-$(date +%s)}"
  prev_head="$(read_state_field "audio-mixer-clock_prev_age_${box}" "")"
  prev_epoch="$(read_state_field "audio-mixer-clock_prev_epoch_${box}" "")"
  if [ -n "$prev_head" ] && [ -n "$prev_epoch" ]; then
    clock_args=(--prev-log-head-age-s "$prev_head" --pass-gap-s "$(( now_epoch - prev_epoch ))")
  fi
  # issue 1460: the MIX-LATE arm compares against the count this watchdog stored on an earlier pass.
  mlprev="$(read_state_field "${box}_mix_late_prev" "")"
  case "$mlprev" in
    '' | *[!0123456789]*) mlprev="" ;;
    *) mix_args=(--prev-mix-late "$mlprev") ;;
  esac
  # issue 1460: the program receiver this box's FACTS name ("" = none; rc 1 = facts unreadable)
  rxkey="$(program_audio_rx_key "$box")" || rxrc=$?
  [ -n "$rxkey" ] && rx_args=(--rx-key "$rxkey")
  out="$(analyze_body "$body" "$reachable" ${clock_args[@]+"${clock_args[@]}"} \
    ${mix_args[@]+"${mix_args[@]}"} ${rx_args[@]+"${rx_args[@]}"})"
  # issue 1460: a CHANGED audio session (the audio reset's stamp) started its mixed_late count at 0,
  # whatever its count now: grade it from 0 (store 0, the confirm kept). An absent identity on either
  # side (an older gather, a first pass) changes nothing.
  mlsess="$(field "$out" mix_session)"
  mlsess_prev="$(read_state_field "${box}_mix_late_session" "")"
  if [ "$reachable" = "1" ] && [ -n "$mlsess" ] && [ -n "$mlsess_prev" ] && [ "$mlsess" != "$mlsess_prev" ] \
      && [ -n "$mlprev" ]; then
    log "$box mix_late: a new audio session (reset at ${mlsess}, was ${mlsess_prev}) -- its count is graded from 0, not the stored ${mlprev}"
    mlprev=0
    write_state_field "${box}_mix_late_prev" 0
    out="$(analyze_body "$body" "$reachable" ${clock_args[@]+"${clock_args[@]}"} \
      --prev-mix-late 0 ${rx_args[@]+"${rx_args[@]}"})"
  fi
  [ -n "$mlsess" ] && write_state_field "${box}_mix_late_session" "$mlsess"
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
  bverdict="$(field "$out" buffering_verdict)"; bnow="$(field "$out" buffering_ms)"
  bstart="$(field "$out" buffering_start_ms)"; bdelta="$(field "$out" buffering_delta_ms)"
  bticks="$(field "$out" buffering_raised_ticks)"; bsrc="$(field "$out" buffering_source)"
  bage="$(field "$out" buffering_age_s)"
  mlverdict="$(field "$out" mix_late_verdict)"; mlnow="$(field "$out" mix_late)"
  mlage="$(field "$out" mix_late_age_s)"
  log "$box ($ip): reachable=$reachable mixer=${mverdict:-<none>} ticks=${ticks} over=${over} tick_ms=${tick_ms} window_ms=${window} age=${age} log_head_age=${head_age} dev=${dev} | vban=${vverdict:-<none>} events=${events} loss_ms=${loss_ms} dest=${dest} age=${vage} | buffering=${bverdict:-<none>} now_ms=${bnow} start_ms=${bstart} delta_ms=${bdelta} ticks=${bticks} source=${bsrc} age=${bage} | mix_late=${mlverdict:-<none>} now=${mlnow} prev=${mlprev} age=${mlage}"

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

  # BUFFERING arm (issue 1381; independent, disjoint state): the mix buffering ratcheted above its
  # session start. It ends only with an OBS restart, so recovery is that restart (log-only).
  case "$bverdict" in
    SKIP)    : ;;   # already logged for the mixer arm (same fetch)
    UNKNOWN) log "$box buffering: no audio-telemetry #800 line / no start value (fresh OBS start / older build or gather) -- holding, no page" ;;
    STALE)   log "$box buffering: STALE -- newest #800 line ${bage}s behind the log head or the log head is not live (${head_age:-no}s old) -- machine channel only, no page"
             reset_arm_confirm "$box" "audio-buffering" ;;
    HEALTHY) handle_healthy_arm "$box" "audio-buffering" ;;
    RAISED)
      src_txt="zdroj: ${bsrc}"
      [ -n "$bsrc" ] || src_txt="zdroj nie je v logu (riadok so zvýšením už vypadol z konca logu)"
      handle_arm "$box" "RAISED" "audio-buffering" "audio-buffering" \
        "🚨 Oneskorenie zvuku ($REPO_SLUG): **$box** ($ip) — OBS zvýšil zvukové oneskorenie mixu z ${bstart} ms na **${bnow} ms** (+${bdelta} ms, ${src_txt}). Celý zvukový výstup tohto OBS je teraz o ${bdelta} ms neskôr a ostane tak až do reštartu OBS. Príčinou býva zaseknutie zvukového vlákna (15.10.: 432 ms → +85 ms). Treba rozhodnúť o reštarte OBS. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol." ;;
    *)       log "$box buffering: unexpected verdict '${bverdict:-<empty>}' -- holding, no page" ;;
  esac

  handle_mix_late "$box" "$ip" "${mlverdict:-}" "$mlnow" "$mlprev" "$mlage"

  # PROGRAM-RX arm (issue 1460; independent, disjoint state)
  rxverdict="$(field "$out" rx_verdict)"
  [ "$rxrc" -eq 0 ] || rxverdict="FACTS"
  handle_program_rx "$box" "$ip" "${rxverdict:-}" "$rxkey" "$out"
  return 0
}

# analyze_body <body> <reachable> [extra analyze args...] -> the decision's key=value output.
analyze_body() {
  local body="$1" reachable="$2"
  shift 2
  printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" \
    --tolerance "$TOLERANCE" --over-max "$OVER_MAX" --stale-after-s "$STALE_AFTER_S" \
    --vban-stale-after-s "$VBAN_STALE_AFTER_S" --log-live-s "$LOG_LIVE_S" "$@" 2>/dev/null
}

# program_rx_production <box> -> stdout: yes | no | unknown, then the stream box read (issue 1460,
# owner 12.11.2026): the venue's stream box (from the box facts) programs `PRO` before AND after the
# read and its output is active. Its address is its row in this pass's roster; no stream box / not in
# the roster / unreadable / hung = unknown, an absent OBS (probe rc 1) = no. Test seam
# AUDIO_MIXER_PRODUCTION_CMD: an executable run as `<cmd> <host> 4455` in place of the probe.
program_rx_production() {
  local box="$1" sb pair sip="" out rc=0 s0 s1 a0 a1
  sb="$(program_audio_rx_stream_box "$box")"
  for pair in $BOXES; do [ "${pair%%|*}" = "$sb" ] && sip="${pair##*|}"; done
  if [ -z "$sb" ] || [ -z "$sip" ]; then printf 'unknown stream_box=%s\n' "${sb:-<none>}"; return 0; fi
  if ! obs_fleet_poll_now "$sb"; then printf 'unknown stream_box=%s (away, not probed)\n' "$sb"; return 0; fi
  if [ -n "${AUDIO_MIXER_PRODUCTION_CMD:-}" ]; then
    out="$("$AUDIO_MIXER_PRODUCTION_CMD" "$sip" 4455 2>/dev/null)" || rc=$?
  else
    out="$(timeout 45 python3 "$HERE/stream_output_stuck_probe.py" "$sip" 4455 --window-s 0.5 2>/dev/null)" || rc=$?
  fi
  s0="$(field "$out" scene0)"; s1="$(field "$out" scene1)"; a0="$(field "$out" active0)"; a1="$(field "$out" active1)"
  if [ "$rc" -eq 1 ]; then printf 'no stream_box=%s (OBS absent)\n' "$sb"
  elif [ "$rc" -ne 0 ] || [ -z "$s0" ] || [ -z "$s1" ] || [ -z "$a0" ] || [ -z "$a1" ]; then
    printf 'unknown stream_box=%s (probe rc %s)\n' "$sb" "$rc"
  elif [ "$s0" = PRO ] && [ "$s1" = PRO ] && [ "$a0" = 1 ] && [ "$a1" = 1 ]; then printf 'yes stream_box=%s\n' "$sb"
  else printf "no stream_box=%s (scene '%s'/'%s', streaming %s/%s)\n" "$sb" "$s0" "$s1" "$a0" "$a1"
  fi
  return 0
}

# handle_program_rx_silent <box> <ip> <rx-key> <age> -- issue 1460: SILENT pages only during a
# production (program_rx_production yes) and with AUDIO_MIXER_PROGRAM_RX_PAGE=1, confirmed over two
# passes (a silence is a state, never one counter delta); otherwise machine channel only, and the
# arm's confirm resets so a later production starts from zero.
handle_program_rx_silent() {
  local box="$1" ip="$2" key="$3" age="$4" prod what
  prod="$(program_rx_production "$box")"
  log "$box program_rx: SILENT -- ${key} delivers no program audio (no status line for ${age}s, or no Dante frames delivered in the window) while OBS keeps logging; production=${prod}"
  if [ "${prod%% *}" != yes ]; then
    log "$box program_rx: SILENT outside a proven production -- machine channel only, no page"
    reset_arm_confirm "$box" "program-rx"
    return 0
  fi
  if [ "$PROGRAM_RX_PAGE" != "1" ]; then
    log "$box program_rx: SILENT during a production -- paging is DISABLED (AUDIO_MIXER_PROGRAM_RX_PAGE=0): machine channel only, no page"
    return 0
  fi
  case "$key" in
    dante) what="Dante vstup OBS (zariadenie dante-input)" ;;
    *) what="VBAN vstup OBS '${key#obs-vban:}'" ;;
  esac
  handle_arm "$box" "SILENT" "program-rx" "program-rx" \
    "🚨 Programový zvuk ($REPO_SLUG): **$box** ($ip) — ${what} počas produkcie nedostáva žiadny zvuk (${age} s bez zvuku, OBS beží). Odosielateľ, jeho sieťovka alebo zvuková sieť vypadli. Potvrdené počas 2 kontrol." 2
  return 0
}

# handle_program_rx <box> <ip> <verdict> <rx-key> <analyze-output> -- issue 1460, the PROGRAM-RX arm
# (prefix + key base `program-rx`, PRODUCTION-CRITICAL time-bucketed). LOSS pages on its FIRST pass
# (review round 1: a counter delta is a fact, and with the timer's 300-420 s pass gap only one pass
# may see a loss); the arm + box throttle keeps one ongoing loss one incident. HEALTHY = one
# machine-channel RECOVERY line once it had paged; SILENT (the program stopped on a live log) and
# STALE (log frozen / unproven) are machine-channel only; NONE (no receiver in the facts), UNKNOWN,
# FACTS (facts unreadable) and SKIP hold, never a page.
handle_program_rx() {
  local box="$1" ip="$2" verdict="$3" key="$4" out="$5" what deltas gap gap_txt
  deltas="$(field "$out" rx_deltas)"; gap="$(field "$out" rx_gap_max_s)"
  log "$box program_rx=${verdict:-<none>} key=${key:-<none>} events=$(field "$out" rx_events) deltas=${deltas} rebases=$(field "$out" rx_rebases) gap_max_s=${gap} age=$(field "$out" rx_age_s)"
  case "$verdict" in
    SKIP)    : ;;   # already logged for the mixer arm (same fetch)
    NONE)    : ;;   # the box facts name no program receiver (resolume)
    FACTS)   log "$box program_rx: the box facts did not load -- cannot tell which receiver to grade, holding, no page" ;;
    UNKNOWN) log "$box program_rx: no audio_rx entry for ${key} (older gather / no status line yet) -- holding, no page" ;;
    STALE)   log "$box program_rx: STALE -- no status line of ${key} for $(field "$out" rx_age_s)s and no proof the OBS log is live (frozen log / older gather) -- machine channel only, no page" ;;
    SILENT)  handle_program_rx_silent "$box" "$ip" "$key" "$(field "$out" rx_age_s)" ;;
    HEALTHY) handle_healthy_arm "$box" "program-rx" ;;
    LOSS)
      if [ "$PROGRAM_RX_PAGE" != "1" ]; then
        log "$box program_rx: LOSS (${deltas}) -- paging is DISABLED (AUDIO_MIXER_PROGRAM_RX_PAGE=0, the issue-1460 rollout default): machine channel only, no page"
        return 0
      fi
      case "$key" in
        dante) what="Dante vstup OBS (zariadenie dante-input)" ;;
        *) what="VBAN vstup OBS '${key#obs-vban:}'" ;;
      esac
      gap_txt=""
      case "$gap" in
        '' | *[!0123456789]*) : ;;
        *) [ "$gap" -gt 30 ] && gap_txt=" Medzi dvoma stavovými riadkami bola medzera ${gap} s: odosielateľ prestal posielať alebo vypadla sieť." ;;
      esac
      handle_arm "$box" "LOSS" "program-rx" "program-rx" \
        "🚨 Programový zvuk ($REPO_SLUG): **$box** ($ip) — ${what} stratil zvuk za posledných ~11 min: **${deltas}**. Každý stratený slot je diera v programovom zvuku (aj zamaskovaný slot je strata).${gap_txt}" 1 ;;
    *)       log "$box program_rx: unexpected verdict '${verdict:-<empty>}' -- holding, no page" ;;
  esac
  return 0
}

# handle_mix_late <box> <ip> <verdict> <now> <prev> <age> -- issue 1460, the MIX-LATE arm
# (independent, disjoint state, prefix `audio-mix-late`). The stored count `<box>_mix_late_prev` is
# what LATE is measured against. A rise is a one-pass edge, so on a LATE pass the stored count stays
# at its pre-rise value until the 2-pass confirm has paged: the second pass still reads LATE, and
# one real MIXED_LATE event pages. HEALTHY / a first BASELINE / a confirmed page store the current
# count; SKIP / UNKNOWN / STALE leave it alone. A BASELINE on a LOWER count is a new audio session,
# whose count started at 0: the stored count becomes 0 and the confirm is left as it is, so the
# holes of the new session page (review round 1). An unchanged pass after a page is HEALTHY: one
# machine-channel RECOVERY line (issue 1206), and the next separate rise is a new page.
handle_mix_late() {
  local box="$1" ip="$2" verdict="$3" now="$4" prev="$5" age="$6"
  case "$verdict" in
    SKIP)    : ;;   # already logged for the mixer arm (same fetch)
    UNKNOWN) log "$box mix_late: no audio-telemetry #800 mix line (fresh OBS start / older build or gather) -- holding, no page" ;;
    STALE)   log "$box mix_late: STALE -- newest #800 mix line ${age}s behind the log head -- machine channel only, no page"
             reset_arm_confirm "$box" "audio-mix-late" ;;
    BASELINE)
      if [ -n "$prev" ]; then
        # a lower count: the audio reset zeroed it, every one of the ${now} events is new
        log "$box mix_late: BASELINE -- count ${now} below the stored ${prev}: a new audio session -- compared from 0 on the next pass, no page now"
        write_state_field "${box}_mix_late_prev" 0
      else
        log "$box mix_late: BASELINE -- count ${now} (no stored count: first pass) -- stored, no page"
        handle_healthy_arm "$box" "audio-mix-late"
        write_state_field "${box}_mix_late_prev" "$now"
      fi ;;
    HEALTHY)
      handle_healthy_arm "$box" "audio-mix-late"
      write_state_field "${box}_mix_late_prev" "$now" ;;
    LATE)
      if handle_arm "$box" "LATE" "audio-mix-late" "audio-mixer-mixlate" \
        "🚨 Zvukový mix ($REPO_SLUG): **$box** ($ip) — člen zvukového mixu OBS nestihol okno mixu a bol znovu zarovnaný (MIXED_LATE): počet stúpol z ${prev} na **${now}** (+$(( now - prev ))). Každé také zarovnanie je počuteľná diera vo zvuku z tohto OBS. Treba zistiť, ktorý zdroj mešká (OBS log: buffering-guard / MIXED_LATE). Potvrdené počas ${CONFIRM_THRESHOLD} kontrol."; then
        write_state_field "${box}_mix_late_prev" "$now"
      fi ;;
    *)       log "$box mix_late: unexpected verdict '${verdict:-<empty>}' -- holding, no page" ;;
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
