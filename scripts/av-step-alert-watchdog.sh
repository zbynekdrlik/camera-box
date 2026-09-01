#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/audio-lag-alert-watchdog.sh /
# network-reach-alert-watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/av-step-alert-watchdog.sh -- #1267: dev1-side report-only ALERT watchdog for an UPSTREAM
# audio-latency STEP on the stream box (the real A/V-residual early warning; issue 1265 follow-up).
#
# WHY (#1267, 2026-09-01 incident): the mastered Dante feed into the stream box's DVS `mbc` source
# got ~-50..-90 ms later at 17:50-18:10 local -- an UPSTREAM audio-chain latency STEP, NOT the
# stream-OBS ts_lag flap (issue 1265's band watch) and NOT the video path. The genlock pin held 926
# and strih had no reboot, yet the E2E A/V gate residual went -77/-126/-111 THREE HOURS later. The
# stream av-sync dock already measured the shift (its `LOCK-CORRECT SUGGESTED genlock_latency_ms_src
# <pin> -> <new>ms (measured offset=<X>ms)` line, monitor-only, ~2/min -- a live, E2E-independent,
# restart-independent A/V trend), but nothing off the box read it. This watchdog closes that gap: it
# reads the `av_offset_*` facets bundle_state_gather (#1267) now exposes on `:8899/bundle-state.json`,
# and pages a report-only alert when the RECENT-vs-BASELINE median offset STEPS beyond a threshold at
# a CONSTANT pin -- the early warning that would have flagged the 17:50 shift 3 h before the E2E.
#
# The genlock pin is a COVARIATE, NOT subtracted: a live pin jump 976->1024 (E2E test-latency churn)
# left the raw offset ~unchanged, so `offset - pin` reads a phantom step. The box reports a
# pin_stable flag; a pin move in the analyzed span -> the REPIN verdict (report-only, no page). So a
# STEP is only ever judged across a CONSTANT-pin window -- exactly the 2026-09-01 case.
#
# DETECTION ONLY (report-only) -- there is deliberately NO auto-action. The cure is a live-box OBS
# restart (a destructive owner-call per no-destructive-remote) or an upstream Dante investigation,
# exactly like issue 1265's band watch and the #1226 lag watch. Recovery is log-only (machine
# channel), never a phone ping (.claude/rules/watchdog-notify-dedup.md #1206).
#
# Topology: SAME dev1 alert-watchdog family as audio-lag (#1226) / network-reach (#1001) /
# bundle-state (#732) -- a `set -uo pipefail` systemd `--user` oneshot + timer (5-min cadence), a
# PURE decision core (scripts/av_step_decision.py, #1199 python-mirror), and `airuleset.py notify`
# from dev1. Reuses scripts/lib/obs-watchdog-decision.sh (`obs_watchdog_confirm` 2-pass +
# `obs_watchdog_alert_throttle` ~1h) VERBATIM.
#
# NO reference-anchor / dev1-side-outage guard is needed (unlike bundle-state #732, which RESTARTS
# tasks + pages on a DOWN box): the ONLY page condition is a SUCCESSFULLY FETCHED positive STEP
# reading, so a dev1-side path outage makes every fetch fail -> box_reachable=0 -> SKIP -> no page. A
# box/`:8899`-down page is #732 / #1001 territory, deferred to here as SKIP.
#
# Ships DISABLED (units committed but not enabled) -- see systemd/av-step-alert-watchdog.README.md.
#
# Usage:
#   scripts/av-step-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/av-step-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/av-step-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,48p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "av-step-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The box(es) to watch, as "name|ip" pairs (space-separated). Stream ONLY by default: the av-sync
# dock measured-offset series is a STREAM-only signal (strih logs `ASRC section unavailable --
# source 'mbc' not found on this box`, so it never carries the facet -> always UNKNOWN there).
BOXES="${AV_STEP_BOXES:-stream|10.77.9.204}"
BUNDLE_PORT="${AV_STEP_BUNDLE_PORT:-8899}"          # the bundle-state HTTP service (#650) carrying the facet
BUNDLE_PATH="${AV_STEP_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${AV_STEP_CURL_TIMEOUT:-10}"          # :8899 HTTP fetch (s); server has answered ~6.6s

# Step threshold: |recent_med - base_med| > this many ms = a sustained upstream A/V STEP. Normal
# 10-min dock medians wander ±30 ms within an hour; the 2026-09-01 step was ≈ −60…−90 ms. 45 ms
# cleanly separates the two (matches av_step_decision.DEFAULT_STEP_THRESHOLD_MS).
STEP_THRESHOLD_MS="${AV_STEP_THRESHOLD_MS:-45}"

# Minimum dock samples in EACH window (recent + baseline) before a STEP is judged; too few -> UNKNOWN
# (never a false step off thin data). ~2/min dock cadence, so 6 ≈ 3 min.
MIN_SAMPLES="${AV_STEP_MIN_SAMPLES:-6}"

# Staleness bound (s): a dock series whose freshest line sits more than this behind the OBS log head
# has STOPPED while the log kept advancing -> STALE (surfaced distinctly, NEVER a phone page --
# absence is #732/#1001 territory). Matches bundle_state_gather.AV_OFFSET_STALE_AFTER_S.
STALE_THRESHOLD_S="${AV_STEP_STALE_THRESHOLD_S:-300}"

# 2-pass confirm before paging (matches the sibling watchdogs): a single blipped median must never
# fire. A genuine upstream step is sustained ≥20 min and stays stepped across the 5-min cadence.
CONFIRM_THRESHOLD="${AV_STEP_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${AV_STEP_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

DECIDE="${AV_STEP_DECIDE:-$HERE/av_step_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AV_STEP_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${AV_STEP_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit override still wins).
_state_default="$STATE_DIR/camera-box-av-step-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-av-step-alert-dryrun.state"
STATE_FILE="${AV_STEP_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [av-step-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body to stdout and returns 0 iff a 200 with a body
# that starts with `{` came back. A curl failure or a wedged-but-listening non-JSON answer returns 1
# (box_reachable=0 for this pass -> SKIP; deferred to #732/#1001).
fetch_bundle_json() {
  local ip="$1" body
  body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
    || return 1
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace (a non-{ body -> SKIP, the
                                            # safe direction, never a false page)
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
# step later pages fresh instead of being dedup'd against a stale signature. Does NOT clear the
# `alerted` flag -- that is the recovery-ping latch, handled separately.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip>
handle_box() {
  local box="$1" ip="$2" body reachable verdict recent base pin step age pinstable analyze_out

  if body="$(fetch_bundle_json "$ip")"; then
    reachable=1
  else
    reachable=0
    body=""
  fi

  analyze_out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" --step-threshold-ms "$STEP_THRESHOLD_MS" --min-samples "$MIN_SAMPLES" --stale-threshold-s "$STALE_THRESHOLD_S" 2>/dev/null)"
  verdict="$(printf '%s\n' "$analyze_out" | sed -n 's/^verdict=//p')"
  recent="$(printf '%s\n' "$analyze_out" | sed -n 's/^recent_med_ms=//p')"
  base="$(printf '%s\n' "$analyze_out" | sed -n 's/^base_med_ms=//p')"
  pin="$(printf '%s\n' "$analyze_out" | sed -n 's/^pin=//p')"
  step="$(printf '%s\n' "$analyze_out" | sed -n 's/^step_ms=//p')"
  age="$(printf '%s\n' "$analyze_out" | sed -n 's/^age_s=//p')"
  pinstable="$(printf '%s\n' "$analyze_out" | sed -n 's/^pin_stable=//p')"
  log "$box ($ip): reachable=$reachable verdict=${verdict:-<none>} recent_med=${recent:-} base_med=${base:-} step_ms=${step:-} pin=${pin:-} pin_stable=${pinstable:-} age_s=${age:-} (threshold=${STEP_THRESHOLD_MS}ms, min_samples=${MIN_SAMPLES}, stale=${STALE_THRESHOLD_S}s)"

  case "$verdict" in
    SKIP)
      log "$box :$BUNDLE_PORT not fetchable this pass -- box/:$BUNDLE_PORT-down is #732/#1001 territory; holding av-step state, no page"
      return 0
      ;;
    STALE)
      # dock series PRESENT but the freshest line is > STALE_THRESHOLD_S behind the OBS log head: the
      # dock stopped emitting while the log kept advancing. Surfaced distinctly (this machine-channel
      # line), NEVER a phone page (absence is never paged; a fully-down box is #732/#1001). Holds
      # state like UNKNOWN -- an unmeasured pass neither advances nor resets the confirm counter.
      log "$box dock series STALE: freshest measured-offset line ~${age:-?}s behind the OBS log head (> ${STALE_THRESHOLD_S}s) -- dock stopped while the log advances; surfaced distinctly, holding state, no page"
      return 0
      ;;
    UNKNOWN)
      log "$box reachable but no usable av_offset facet (box not upgraded, or too few dock samples in the tail yet) -- no reading, holding state, no page"
      return 0
      ;;
    REPIN)
      # The genlock pin moved across the analyzed span (a #856/operator/E2E apply settling). The pin
      # is a covariate we never subtract, so a step is only judged at a CONSTANT pin -- report-only,
      # no page here. Holds state like UNKNOWN.
      log "$box pin moved across the analyzed span (pin=${pin:-?}, #856/operator/E2E settling) -- report-only, holding state, no page"
      return 0
      ;;
    HEALTHY)
      local was_alerted recover
      was_alerted="$(read_state_field "alerted_${box}" 0)"
      recover="$(av_step_recovery_decision_local "$was_alerted")"
      if [ "$recover" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: $box av-offset step back to normal (recent_med=${recent}ms)"
        else
          log "RECOVERY: $box av-offset step back to normal (recent_med=${recent}ms) -- machine-channel only (#1206: recovery is not a phone ping)"
        fi
        write_state_field "alerted_${box}" 0
      fi
      clear_box_throttle "$box"
      return 0
      ;;
    STEP) : ;;   # fall through to confirm + alert
    *)
      log "$box: unexpected verdict '${verdict:-<empty>}' from av_step_decision.py (analyze failed?) -- holding state, no page"
      return 0
      ;;
  esac

  # STEP -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box av-offset STEP (${step}ms, recent_med=${recent}ms vs base_med=${base}ms at pin ${pin}) this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED upstream step -> latch recovery, throttled report-only alert.
  write_state_field "alerted_${box}" 1

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="avstep:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED av-offset step ${step}ms (recent_med=${recent}ms vs base_med=${base}ms, pin ${pin}) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box av-offset step ${step}ms"
    python3 "$NOTIFY" notify --body \
      "⚠️ Upstream A/V posun ($REPO_SLUG): **$box** ($ip) — av-sync dock nameral SKOK v A/V ofsete o **${step} ms** (recent medián ${recent}ms vs baseline ${base}ms) pri konštantnom gen-pine ${pin}. To je skorá výstraha na posun latencie v audio reťazci PRED stream OBS (mastered Dante feed do 'mbc') — zvyčajne predbehne pád A/V gate o hodiny. Video cesta ani gen-pin sa nehli. Skontroluj audio chain / mastering; ak treba, pomôže reštart stream OBS (owner rozhodnutie). Report-only." \
      --dedup-key "av-step-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still stepped"
  fi
}

# av_step_recovery_decision_local <was_alerted> -> "1" iff a recovery latch should fire (was
# alerted, now healthy). Kept trivially local (a HEALTHY pass IS the "now back to normal" side) so
# this watchdog needs no extra lib; mirrors audio-lag's own net_reach_recovery_decision_local shape.
av_step_recovery_decision_local() {
  [ "${1:-0}" = "1" ] && printf '1' || printf '0'
}

# require_tools -> exit non-zero (loud) if a REQUIRED external tool OR the decision module is
# missing. A missing `curl` would make every fetch fail -> every box SKIP; a missing/unreadable
# $DECIDE would make `analyze` emit nothing -> every box "unexpected verdict, holding" -> both
# SILENT FOREVER (a real upstream step goes unpaged), exactly the "a missing dependency must fail
# LOUD by name, never read as a measured zero" class .claude/rules/imag-ssh-remote-tool-preflight.md
# (#833) exists to prevent. `timeout` is NOT required: curl bounds itself with --max-time and the
# local python decision reads already-fetched stdin (it cannot hang on the network).
require_tools() {
  local missing=() t
  for t in curl python3; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing curl/python3 would silently SKIP every box and never page a real upstream A/V step)"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (analyze would emit nothing -> every box 'holding' forever, a real step unpaged; fix AV_STEP_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, threshold=${STEP_THRESHOLD_MS}ms, min_samples=${MIN_SAMPLES}, stale=${STALE_THRESHOLD_S}s, boxes='$BOXES')"
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
