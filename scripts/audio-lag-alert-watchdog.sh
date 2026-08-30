#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/bundle-state-alert-watchdog.sh /
# network-reach-alert-watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/audio-lag-alert-watchdog.sh -- #1226: dev1-side ALERT watchdog for stream/strih OBS's
# audio pipeline falling behind realtime.
#
# WHY (#1226, live incident 2026-08-30 nedeľná služba): stream OBS's audio subsystem began lagging
# realtime PRECISELY at StartStream and lost ~24-27 s/min; `audio-telemetry #800 '<src>': ts_lag_ms=N`
# (vendor/obs-studio/libobs/obs-audio.c:698) grew to 1 672 741 ms (27,9 min) and SCREAMED into the
# OBS log the whole hour -- but NOTHING off the box read it, so the YouTube stream's A/V desynced for
# a whole service before a viewer noticed. obs-liveness (#391) watches RENDER not audio timeline;
# av-sync dock is structurally blind during a service (program = real cameras, no QR); asio-starve
# (#1023) measures per-source starvation, not the global audio-tick lag. This watchdog closes that
# detection gap: it reads the `audio_ts_lag_ms` facet bundle_state_gather (#1226) now exposes on
# `:8899/bundle-state.json`, and pages when a box's audio timeline sits sustained > threshold behind
# realtime (confirmed across 2 passes).
#
# DETECTION ONLY (alert-only) -- there is deliberately NO auto-action. The observed cure was a PC
# reboot of a live prod box (a genuinely destructive owner-call per no-destructive-remote); the
# preventive pre-service reboot is an OWNER decision on the ticket, not automation. Recovery is
# log-only (machine channel), never a phone ping (.claude/rules/watchdog-notify-dedup.md #1206).
#
# Topology: SAME dev1 alert-watchdog family as network-reach (#1001) / bundle-state (#732) --
# a `set -uo pipefail` systemd `--user` oneshot + timer (5-min cadence), a PURE decision core
# (scripts/audio_lag_decision.py, #1199 python-mirror pattern), and `airuleset.py notify` from dev1.
# It reuses scripts/lib/obs-watchdog-decision.sh (`obs_watchdog_confirm` 2-pass + `obs_watchdog_alert_throttle`
# ~1h) VERBATIM.
#
# NO reference-anchor / dev1-side-outage guard is needed here (unlike bundle-state #732, which
# RESTARTS tasks + pages on a DOWN box): this watchdog's ONLY page condition is a SUCCESSFULLY
# FETCHED positive lag reading, so a dev1-side path outage makes every fetch fail -> box_reachable=0
# -> SKIP -> no page. A box/`:8899`-down page is #732 / #1001 territory, deferred to here as SKIP.
#
# Usage:
#   scripts/audio-lag-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/audio-lag-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/audio-lag-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,44p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "audio-lag-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The two OBS boxes to watch, as "name|ip" pairs (space-separated).
BOXES="${AUDIO_LAG_BOXES:-strih|10.77.9.202 stream|10.77.9.204}"
BUNDLE_PORT="${AUDIO_LAG_BUNDLE_PORT:-8899}"          # the bundle-state HTTP service (#650) carrying the facet
BUNDLE_PATH="${AUDIO_LAG_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${AUDIO_LAG_CURL_TIMEOUT:-10}"          # :8899 HTTP fetch (s); server has answered ~6.6s

# Page threshold: audio timeline > this many ms behind realtime = LAGGING. Healthy baseline is
# ~107-132 ms; a genuine desync grows into the thousands-to-millions. 5000 ms is a wide margin above
# any healthy jitter and well below the point a viewer notices, so it catches the growth EARLY.
THRESHOLD_MS="${AUDIO_LAG_THRESHOLD_MS:-5000}"

# 2-pass confirm before paging (matches the sibling watchdogs): a single blipped reading must never
# fire. A genuine desync grows monotonically and stays lagging across the 5-min cadence.
CONFIRM_THRESHOLD="${AUDIO_LAG_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${AUDIO_LAG_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

DECIDE="${AUDIO_LAG_DECIDE:-$HERE/audio_lag_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AUDIO_LAG_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${AUDIO_LAG_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit override still wins).
_state_default="$STATE_DIR/camera-box-audio-lag-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-audio-lag-alert-dryrun.state"
STATE_FILE="${AUDIO_LAG_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [audio-lag-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body to stdout and returns 0 iff a 200 with a body
# that starts with `{` came back (a real JSON object). A curl failure or a wedged-but-listening
# non-JSON answer returns 1 (box_reachable=0 for this pass -> SKIP; deferred to #732/#1001).
fetch_bundle_json() {
  local ip="$1" body
  body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
    || return 1
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace/BOM
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
  # Read the OTHER keys into memory FIRST, before any file is opened for writing -- so even the
  # mktemp-failure fallback can never truncate-before-read and drop them.
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

# A HEALTHY box is not an incident: clear its confirm counter AND its throttle sig so a genuinely NEW
# desync later pages fresh instead of being dedup'd against a stale signature. Does NOT clear the
# `alerted` flag -- that is the recovery-ping latch, handled separately.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# lag_minutes <lag_ms> -> "M.m" minutes (one decimal), for a human-readable alert body.
lag_minutes() {
  awk -v ms="$1" 'BEGIN { printf "%.1f", ms / 60000.0 }' 2>/dev/null || printf '?'
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip>
handle_box() {
  local box="$1" ip="$2" body reachable verdict lag src analyze_out

  if body="$(fetch_bundle_json "$ip")"; then
    reachable=1
  else
    reachable=0
    body=""
  fi

  analyze_out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" --threshold-ms "$THRESHOLD_MS" 2>/dev/null)"
  verdict="$(printf '%s\n' "$analyze_out" | sed -n 's/^verdict=//p')"
  lag="$(printf '%s\n' "$analyze_out" | sed -n 's/^lag_ms=//p')"
  src="$(printf '%s\n' "$analyze_out" | sed -n 's/^src=//p')"
  log "$box ($ip): reachable=$reachable verdict=${verdict:-<none>} lag_ms=${lag:-} src=${src:-} (threshold=${THRESHOLD_MS}ms)"

  case "$verdict" in
    SKIP)
      log "$box :$BUNDLE_PORT not fetchable this pass -- box/:$BUNDLE_PORT-down is #732/#1001 territory; holding audio-lag state, no page"
      return 0
      ;;
    UNKNOWN)
      log "$box reachable but no audio_ts_lag_ms facet (no #800 telemetry in the tail yet) -- no reading, holding state, no page"
      return 0
      ;;
    HEALTHY)
      local was_alerted recover
      was_alerted="$(read_state_field "alerted_${box}" 0)"
      recover="$(net_reach_recovery_decision_local "$was_alerted")"
      if [ "$recover" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: $box audio lag back to normal (${lag}ms)"
        else
          log "RECOVERY: $box audio lag back to normal (${lag}ms) -- machine-channel only (#1206: recovery is not a phone ping)"
        fi
        write_state_field "alerted_${box}" 0
      fi
      clear_box_throttle "$box"
      return 0
      ;;
    LAGGING) : ;;   # fall through to confirm + alert
    *)
      log "$box: unexpected verdict '${verdict:-<empty>}' from audio_lag_decision.py (analyze failed?) -- holding state, no page"
      return 0
      ;;
  esac

  # LAGGING -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box audio LAGGING (${lag}ms) this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED audio lag -> latch recovery, throttled alert.
  write_state_field "alerted_${box}" 1

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes mins
  current_sig="audiolag:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  mins="$(lag_minutes "$lag")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box audio CONFIRMED lagging ${lag}ms (~${mins}min, src '${src}') alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box audio lag ${lag}ms"
    python3 "$NOTIFY" notify --body \
      "🚨 Audio-lag ($REPO_SLUG): **$box** ($ip) — audio v OBS zaostáva za realtime o **${lag} ms (~${mins} min)** (zdroj '${src}'). A/V sa na streame rozíde (YouTube desync). Prah ${THRESHOLD_MS} ms prekročený, potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol. Skontroluj OBS na boxe; ak lag rastie ďalej, pomôže reštart OBS/boxu (owner rozhodnutie)." \
      --dedup-key "audio-lag-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still lagging"
  fi
}

# net_reach_recovery_decision_local <was_alerted> -> "1" iff a recovery latch should fire (was
# alerted, now healthy). Kept trivially local (a HEALTHY pass IS the "now up" side) so this watchdog
# needs no extra lib; mirrors net_reach_recovery_decision's was_alerted-AND-up shape.
net_reach_recovery_decision_local() {
  [ "${1:-0}" = "1" ] && printf '1' || printf '0'
}

# require_tools -> exit non-zero (loud) if a REQUIRED external tool is missing. A missing `curl`
# would make every fetch fail -> every box SKIP -> silent forever (a real desync goes unpaged),
# which is exactly the "a missing tool must fail LOUD by name, never read as a measured zero" class
# .claude/rules/imag-ssh-remote-tool-preflight.md (#833) exists to prevent.
require_tools() {
  local missing=() t
  for t in curl python3 timeout; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing curl/python3 would silently SKIP every box and never page a real audio desync)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, threshold=${THRESHOLD_MS}ms, boxes='$BOXES')"
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
