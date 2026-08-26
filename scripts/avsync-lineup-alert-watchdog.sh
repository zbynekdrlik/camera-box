#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/avsync-heartbeat-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/avsync-lineup-alert-watchdog.sh -- #813 measurement A/V-sync LINE GO/NO-GO + liveness alarm,
# DEV1-SIDE. Two modes over ONE gather + the ONE pure decider scripts/avsync_lineup.py:
#
#   (default)  run-time liveness pass: read the stream box heartbeat + the stream's outputActive,
#              decide via avsync_lineup.py liveness (BOUND TO stream state), and -- only when the
#              stream is LIVE and the line is NO-GO -- fire a Discord alert through the SAME
#              scripts/lib/obs-watchdog-decision.sh confirm/throttle + airuleset.py notify path the
#              #391/#812 siblings use (never a second alerting mechanism).
#   --assert   one-shot pre-event GO/NO-GO of the whole measurement line: FRESH heartbeat + VALID
#              last reading + the dev1 forwarder/alert timers active + a REAL Discord test-ping
#              delivered (HTTP 200). Prints a one-line GO/NO-GO, exits 0/1, and on NO-GO fires a loud
#              alert BEFORE the event so a dead line is caught at ~08:00, not 7h later at the E2E.
#
# WHY (#813): the existing scripts/avsync-heartbeat-alert-watchdog.sh alarms on heartbeat STALENESS
# only, and UNCONDITIONALLY -- so (a) it would NOT have paged the 2026-08-17 silent-audio incident
# (the heartbeat stayed FRESH; only the CONTENT died -> "measured: unknown, candidates: 0"), and (b)
# a plain stale-log alarm can't tell a legitimately-off box from a dead watchdog during a live event.
# This watchdog closes both gaps by reading the SAME on-box heartbeat + the stream's outputActive and
# routing the whole judgment through the pure avsync_lineup.py decider -- no fourth measurement path.
#
# Usage:
#   scripts/avsync-lineup-alert-watchdog.sh                # one run-time liveness pass
#   scripts/avsync-lineup-alert-watchdog.sh --assert       # one-shot pre-event GO/NO-GO (exit 0/1)
#   scripts/avsync-lineup-alert-watchdog.sh --dry-run      # gather + decide + LOG only; never alert
#   scripts/avsync-lineup-alert-watchdog.sh --assert --dry-run
#   scripts/avsync-lineup-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/avsync-heartbeat.sh
. "$HERE/lib/avsync-heartbeat.sh"
# avsync-heartbeat.sh sets `-e` for ITS OWN sourcing safety, and that `-e` LEAKS into this caller
# (source runs in the same shell). `set -uo pipefail` alone does NOT clear it (it only turns options
# ON), so we EXPLICITLY `set +e` -- otherwise a `var="$(decider)"` assignment where the decider
# legitimately exits non-zero (a preflight NO-GO, exit 1) would abort the whole pass at the
# ASSIGNMENT, before the verdict is ever printed (ci-testing-gotchas.md's leaked-`set -e` trap). This
# watchdog must survive a bad pass and keep polling on the next timer tick (see the header).
set +e -uo pipefail

MODE="liveness"
DRY_RUN=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --assert) MODE="assert" ;;
    --dry-run) DRY_RUN=1 ;;
    --help|-h)
      sed -n '5,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "avsync-lineup-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
  esac
  shift
done

# ── config (all env-overridable) ─────────────────────────────────────────────
STREAM_IP="${STREAM_IP:-10.77.9.204}"
STREAM_USER_SSH="${STREAM_USER:-newlevel}"
STREAM_PW_SSH="${STREAM_PW:-newlevel}"
# run-time staleness window: 20 min (the ticket's operator-tolerable event window). The pure decider
# defaults to the same values; passing them explicitly keeps this script the single knob surface.
STALE_S="${AVSYNC_LINEUP_STALE_S:-1200}"
PREFLIGHT_STALE_S="${AVSYNC_LINEUP_PREFLIGHT_STALE_S:-300}"
CONFIRM_THRESHOLD="${AVSYNC_LINEUP_CONFIRM_THRESHOLD:-1}"
ALERT_THROTTLE_PASSES="${AVSYNC_LINEUP_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

# stream OBS WebSocket (for the outputActive read via obs_phase2.py stream-status). A missing/wrong
# password just makes the read fail -> stream state UNKNOWN -> SUPPRESSED (fail-safe: never a false
# page, and the network-reach/obs-liveness watchdogs own "OBS unreachable").
STREAM_OBS_WS_HOST="${STREAM_OBS_WS_HOST:-$STREAM_IP}"
# The stream box OBS-WS requires a password; the repo-standard env is OBS_WS_PASSWORD (rig-mode.sh's
# convention). Defaulting to it (NOT an empty string) is what keeps the stream-state read from
# silently failing -> None -> SUPPRESSED every pass -> a permanently-inert alarm (the #3 review
# finding). `--assert` additionally FAILS if the read comes back None, so a mis-set password is caught
# before the event rather than muting the alarm during it.
STREAM_OBS_WS_PW="${STREAM_OBS_WS_PW:-${OBS_WS_PASSWORD:-${OBS_PASSWORD:-}}}"
OBS_PHASE2="${AVSYNC_LINEUP_OBS_PHASE2:-$HERE/obs_phase2.py}"

LINEUP_DECIDER="${AVSYNC_LINEUP_DECIDER:-$HERE/avsync_lineup.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AVSYNC_LINEUP_REPO:-zbynekdrlik/camera-box}"

# the dev1-side alert timers that must be ACTIVE for an alarm to reach the phone during the event
# (the --assert "forwarder bezi na dev1" check). Space-separated; ALL must be is-active.
FORWARDER_UNITS="${AVSYNC_LINEUP_FORWARDER_UNITS:-avsync-lineup-alert-watchdog.timer avsync-heartbeat-alert-watchdog.timer}"
SYSTEMCTL="${AVSYNC_LINEUP_SYSTEMCTL:-systemctl}"

# Discord test-ping (--assert). Same env-file + bot-API shape as scripts/avsync-heartbeat-alert-
# watchdog.sh's post_discord_verdict (never a second convention); only channel/thread IDs committed.
DISCORD_ENV_FILE="${AVSYNC_DISCORD_ENV:-$HOME/.claude/channels/discord/.env}"
DISCORD_THREAD_ID="${AVSYNC_DISCORD_THREAD_ID:-1373592666733940816}"   # alerts-snv thread

STATE_DIR="${AVSYNC_LINEUP_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${AVSYNC_LINEUP_STATE_FILE:-$STATE_DIR/camera-box-avsync-lineup.state}"

log() { printf '%s [avsync-lineup-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# #813 (#6): fail LOUD by name if a hard dependency is missing. Without this the run-time liveness
# pass fails OPEN on a tooling gap (a missing jq -> empty facts -> the decider's json.load errors,
# swallowed by 2>/dev/null -> action="" -> no alarm AND the pending state is reset). A dev1 watchdog
# must never silently mute itself because a dependency is absent (mirrors the sibling dev1
# watchdogs' require_tools discipline).
require_tools() {
  local t missing=""
  for t in "$@"; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done
  if [ -n "$missing" ]; then
    log "FATAL: missing required tool(s):$missing -- refusing to run (a missing dependency must fail LOUD, never silently mute the alarm)"
    exit 3
  fi
}

# ── gather: the stream box heartbeat (reuse the shared probe/parse lib) ──────
HEARTBEAT_EPOCH=""
HEARTBEAT_STATUS=""
gather_heartbeat() {
  local probe_out watchdog_segment
  probe_out="$(sshpass -p "$STREAM_PW_SSH" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${STREAM_USER_SSH}@${STREAM_IP}" "$(avsync_heartbeat_probe_cmd)" 2>/dev/null || true)"
  watchdog_segment="$(avsync_heartbeat_extract_segment "$probe_out" watchdog)"
  HEARTBEAT_EPOCH="$(avsync_heartbeat_last_epoch "$watchdog_segment")"
  HEARTBEAT_STATUS="$(avsync_heartbeat_last_status "$watchdog_segment")"
}

# ── gather: the stream's outputActive (reuse obs_phase2.py stream-status) ────
# -> STREAM_ACTIVE_JSON is a JSON literal: true / false / null (unknown). obs_phase2 prints
# "active=True path=" / "active=False path="; an unreachable OBS-WS (no line / non-zero) -> null.
STREAM_ACTIVE_JSON="null"
gather_stream_state() {
  local out active
  local args=(stream-status --host "$STREAM_OBS_WS_HOST")
  [ -n "$STREAM_OBS_WS_PW" ] && args+=(--password "$STREAM_OBS_WS_PW")
  out="$(python3 "$OBS_PHASE2" "${args[@]}" 2>/dev/null || true)"
  active="$(printf '%s\n' "$out" | sed -n 's/^active=\([A-Za-z0-9]*\).*/\1/p' | tail -1)"
  case "$active" in
    True|true|1)  STREAM_ACTIVE_JSON="true" ;;
    False|false|0) STREAM_ACTIVE_JSON="false" ;;
    *) STREAM_ACTIVE_JSON="null" ;;
  esac
}

# epoch as a JSON literal (an int, or null when the heartbeat never parsed) -- fail-CLOSED matches
# the decider's own "None epoch = not fresh".
epoch_json() {
  case "$HEARTBEAT_EPOCH" in
    ''|*[!0-9]*) printf 'null' ;;
    *) printf '%s' "$HEARTBEAT_EPOCH" ;;
  esac
}

# ── state (same key=value shape as the #391/#812 siblings) ───────────────────
read_state_field() {
  local key="$1" default="$2" v
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
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

# ── Discord test-ping for --assert (returns the HTTP code on stdout) ─────────
read_discord_env_field() {
  local key="$1"
  [ -f "$DISCORD_ENV_FILE" ] || { printf ''; return 0; }
  sed -n "s/^${key}=//p" "$DISCORD_ENV_FILE" 2>/dev/null | tail -1
}
# send_discord_test_ping TEXT -> prints the HTTP status code (e.g. 200) of a real POST to the
# alerts-snv thread. Bounded with --max-time (mirrors the sibling's post_discord_verdict exactly).
# No token / a failed curl -> prints an empty/non-200 code so preflight_verdict fails CLOSED.
send_discord_test_ping() {
  local text="$1" token payload response http_code
  token="$(read_discord_env_field DISCORD_BOT_TOKEN)"
  if [ -z "$token" ]; then
    log "TEST-PING: no DISCORD_BOT_TOKEN at $DISCORD_ENV_FILE -- cannot prove delivery"
    printf ''
    return 0
  fi
  payload="$(jq -n --arg c "$text" '{content:$c}')"
  response="$(curl -sS --max-time 10 -w '\n%{http_code}' -X POST \
    -H "Authorization: Bot $token" \
    -H 'Content-Type: application/json' \
    -H 'User-Agent: DiscordBot (https://github.com/zbynekdrlik/airuleset, 1.0)' \
    -d "$payload" \
    "https://discord.com/api/v10/channels/${DISCORD_THREAD_ID}/messages" 2>/dev/null)"
  http_code="${response##*$'\n'}"
  printf '%s' "$http_code"
}

fire_notify() {
  local body="$1" key="${2:-avsync-lineup}"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $body"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$body" --dedup-key "$key" >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# ── run-time liveness pass (default mode) ───────────────────────────────────
run_liveness_pass() {
  gather_heartbeat
  gather_stream_state
  local now facts action reason
  now="$(date +%s)"
  facts="$(jq -n \
    --argjson epoch "$(epoch_json)" \
    --argjson now "$now" \
    --argjson stale_s "$STALE_S" \
    --arg status "$HEARTBEAT_STATUS" \
    --argjson active "$STREAM_ACTIVE_JSON" \
    '{heartbeat_epoch:$epoch, now:$now, stale_s:$stale_s, heartbeat_status:$status, stream_output_active:$active}')"
  local factfile; factfile="$(mktemp)"
  printf '%s' "$facts" > "$factfile"
  local out; out="$(python3 "$LINEUP_DECIDER" liveness --facts "$factfile" 2>/dev/null || true)"
  rm -f "$factfile"
  action="$(printf '%s\n' "$out" | sed -n 's/^action=\([A-Za-z]*\).*/\1/p' | tail -1)"
  # reason is everything between "reason=" and the trailing " sig=..."; sig is the COARSE stamp-free
  # token the decider emits for throttling (#4 -- never the volatile heartbeat text, whose [stamp]
  # changes every pass and would defeat the throttle into re-paging every 5 min).
  local sig
  reason="$(printf '%s\n' "$out" | sed -n 's/^action=[A-Za-z]* reason=\(.*\) sig=[A-Za-z0-9-]*$/\1/p' | tail -1)"
  sig="$(printf '%s\n' "$out" | sed -n 's/.* sig=\([A-Za-z0-9-]*\)$/\1/p' | tail -1)"
  log "stream_active=$STREAM_ACTIVE_JSON heartbeat_epoch=${HEARTBEAT_EPOCH:-<none>} status='${HEARTBEAT_STATUS:-<none>}' -> action=${action:-<none>} sig=${sig:-<none>}"

  # confirm/throttle ONLY on ALARM; OK and SUPPRESSED both reset the pending state (a live-again OK,
  # or an off-air stream, clears any pending confirmation -- same "reset on clean signal" discipline
  # as the #391/#812 siblings). SUPPRESSED is DELIBERATELY not an alarm: off-air or OBS-unreachable.
  local wedged=0
  [ "$action" = "ALARM" ] && wedged=1
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "lineup_confirm" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "lineup_confirm" "${confirm:-0}"

  if [ "$wedged" -eq 0 ]; then
    write_state_field "lineup_alert_sig" ""
    write_state_field "lineup_alert_passes" 0
    return 0
  fi
  [ "${act:-0}" = "1" ] || { log "ALARM pending (confirm ${confirm}/${CONFIRM_THRESHOLD})"; return 0; }

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  # #4: throttle on the COARSE decider sig (no-audio/wedged/stale/no-signal), never the timestamped
  # heartbeat text -- a sustained condition keeps ONE signature so the ~1h throttle actually holds.
  current_sig="lineup:${sig:-alarm}"
  prior_sig="$(read_state_field "lineup_alert_sig" "")"
  prior_passes="$(read_state_field "lineup_alert_passes" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "lineup_alert_sig" "$new_sig"
  write_state_field "lineup_alert_passes" "$new_passes"

  if [ "${alert_now:-0}" = "1" ]; then
    fire_notify "🚨 avsync-lineup-alert-watchdog: meracia A/V-sync linka je MRTVA pocas ZIVEHO streamu -- ${reason} (${REPO_SLUG})." "avsync-lineup-liveness"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# ── pre-event GO/NO-GO assert (--assert mode) ───────────────────────────────
forwarder_present_json() {
  local unit present="true"
  for unit in $FORWARDER_UNITS; do
    "$SYSTEMCTL" --user is-active --quiet "$unit" 2>/dev/null || present="false"
  done
  printf '%s' "$present"
}

run_preflight_assert() {
  gather_heartbeat
  # #3: also read the stream state so preflight can prove the OBS-WS read WORKS -- a None (unreadable)
  # read means the run-time alarm's own stream gate can't function, and the decider makes that NO-GO.
  gather_stream_state
  [ "$DRY_RUN" -eq 1 ] || require_tools curl
  local now fwd http factfile facts go_out rc
  now="$(date +%s)"
  fwd="$(forwarder_present_json)"
  if [ "$DRY_RUN" -eq 1 ]; then
    http=""   # never send a real ping in dry-run; preflight then correctly reports it undelivered
    log "[dry-run] skipping the real Discord test-ping"
  else
    http="$(send_discord_test_ping "✅ avsync-lineup pre-event test ping ($(date '+%Y-%m-%d %H:%M:%S'))")"
  fi
  local http_json
  case "$http" in
    ''|*[!0-9]*) http_json="null" ;;
    *) http_json="$http" ;;
  esac
  factfile="$(mktemp)"
  facts="$(jq -n \
    --argjson epoch "$(epoch_json)" \
    --argjson now "$now" \
    --argjson stale_s "$PREFLIGHT_STALE_S" \
    --arg status "$HEARTBEAT_STATUS" \
    --argjson fwd "$fwd" \
    --argjson http "$http_json" \
    --argjson active "$STREAM_ACTIVE_JSON" \
    '{heartbeat_epoch:$epoch, now:$now, preflight_stale_s:$stale_s, heartbeat_status:$status, forwarder_present:$fwd, discord_ping_http:$http, stream_output_active:$active}')"
  printf '%s' "$facts" > "$factfile"
  go_out="$(python3 "$LINEUP_DECIDER" preflight --facts "$factfile" 2>/dev/null)"; rc=$?
  rm -f "$factfile"
  printf '%s\n' "$go_out"
  if [ "$rc" -ne 0 ]; then
    # NO-GO -> loud alert BEFORE the event (not just an exit code buried in a terminal).
    fire_notify "🚨 avsync-lineup PRE-EVENT NO-GO: meracia A/V-sync linka NIE JE pripravena -- ${go_out} (${REPO_SLUG})." "avsync-lineup-preflight-nogo"
  fi
  return "$rc"
}

# ── main ─────────────────────────────────────────────────────────────────────
main() {
  # #6: both modes ssh (sshpass/ssh) the heartbeat, build facts (jq) and call the decider (python3) --
  # a missing one must fail LOUD, never silently mute the alarm.
  require_tools sshpass ssh python3 jq
  if [ "$MODE" = "assert" ]; then
    log "pre-event assert (dry_run=$DRY_RUN)"
    run_preflight_assert
    exit $?
  fi
  log "liveness pass (dry_run=$DRY_RUN, stale_s=$STALE_S, threshold=$CONFIRM_THRESHOLD)"
  run_liveness_pass
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
