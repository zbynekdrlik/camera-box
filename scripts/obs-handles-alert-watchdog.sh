#!/usr/bin/env bash
# scripts/obs-handles-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
#
# scripts/obs-handles-alert-watchdog.sh -- issue 1406: dev1-side ALERT watchdog for an OBS process
# that LEAKS HANDLES (Windows handles / Linux open fds) until it hits its per-process cap.
#
# WHY (issue 1406, 5.10.2026): the stream obs64 held 4,066,772 handles after ~22 h. The Audio
# Monitor plugin's output listed an absent Focusrite endpoint and opened its registry key on every
# audio tick without closing it: +46.875 handles/s, 168,750/h, kernel paged pool 1.7 GB. At that
# rate OBS reaches the 16,777,216 per-process cap ~100 h after its start, i.e. during the next
# production. Nothing read a handle count; the leak was found by accident.
#
# It reads the `obs_handles*` facet bundle_state_gather exposes on each box's
# :8899/bundle-state.json -- the OBS process's handle count (Windows, from one NtQuerySystemInformation
# snapshot) or open-fd count (Linux /proc), its pid + start epoch, and on Linux the soft open-files
# limit -- and grades it with scripts/obs_handles_decision.py against the reference sample this
# watchdog stored on its previous pass:
#   * GROWING -- >= 5,000 handles/h over one pass interval; pages after 3 consecutive passes
#     (~15 min), so a one-off step (a scene loading sources, an NDI reconnect) never pages;
#   * CEILING -- >= 500,000 handles (86x a healthy ~5,790, 3% of the Windows cap) or 80% of a Linux
#     box's own soft open-files limit; pages after 2 passes.
# A new pid / start time (an OBS restart) re-baselines and clears the alarm; a manual run less than
# 240 s after the reference HOLDs (the reference is kept). SKIP (:8899 not fetchable -- the
# bundle-state / network-reach watchdogs own that) and UNKNOWN (facet absent: an older server or no
# readable OBS) never page.
#
# PRODUCTION-CRITICAL class (issue 1308): a handle-capped OBS stops working mid-production, so the
# page TIME-BUCKETS its --dedup-key (watchdog_notify_key "obs-handles-<box>") and re-pings while the
# state persists. DETECTION ONLY -- the cure (find the leaking plugin / device, plan an OBS restart)
# is an owner call; recovery is a machine-channel log line, never a phone ping.
#
# Roster = the obs-fleet `obs-handles` facet (strih-lx stream resolume); the traveling resolume is
# polled only while home (obs_fleet_poll_now).
#
# Usage:
#   scripts/obs-handles-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/obs-handles-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/obs-handles-alert-watchdog.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/watchdog-common.sh
. "$HERE/lib/watchdog-common.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so the
# pass runs with -e OFF (the family convention, .claude/rules/watchdog-common.md). The
# `set -euo pipefail` above satisfies the new-.sh script-failure-policy check.
set +e
set -uo pipefail

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,39p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "obs-handles-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOXES="${OBS_HANDLES_BOXES:-$(obs_fleet_boxes obs-handles)}"
BUNDLE_PORT="${OBS_HANDLES_BUNDLE_PORT:-8899}"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
BUNDLE_PATH="${OBS_HANDLES_BUNDLE_PATH:-/bundle-state.json}"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
CURL_TIMEOUT="${OBS_HANDLES_CURL_TIMEOUT:-10}"
CEILING="${OBS_HANDLES_CEILING:-500000}"                # 86x a healthy stream OBS, 3% of the cap
LIMIT_FRACTION="${OBS_HANDLES_LIMIT_FRACTION:-0.8}"     # of a reported (Linux) soft fd limit
GROWTH_PER_H="${OBS_HANDLES_GROWTH_PER_H:-5000}"        # 34x below the 5.10 leak rate
MIN_INTERVAL_S="${OBS_HANDLES_MIN_INTERVAL_S:-240}"     # a shorter interval HOLDs
GROWTH_CONFIRM="${OBS_HANDLES_GROWTH_CONFIRM:-3}"       # consecutive GROWING passes (~15 min)
CEILING_CONFIRM="${OBS_HANDLES_CEILING_CONFIRM:-2}"
ALERT_THROTTLE_PASSES="${OBS_HANDLES_ALERT_THROTTLE_PASSES:-12}"  # ~1h at the 5-min cadence

DECIDE="${OBS_HANDLES_DECIDE:-$HERE/obs_handles_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${OBS_HANDLES_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${OBS_HANDLES_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-obs-handles-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-obs-handles-alert-dryrun.state"
# shellcheck disable=SC2034  # read by scripts/lib/watchdog-common.sh
STATE_FILE="${OBS_HANDLES_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [obs-handles-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> OBS_HANDLES_FETCH_CMD (scripts/lib/watchdog-common.sh) -> prints the JSON
# body + returns 0 iff a `{`-body came back. OBS_HANDLES_FETCH_CMD (Tier-0 seam): an executable run
# as `<cmd> <ip>` whose stdout replaces the curl fetch, so a --dry-run replays a recorded body with
# no live box. OBS_HANDLES_NOW_EPOCH pins the pass time (the reference interval) the same way.

# field <analyze-output> <key> -> the value of `key=` in the decision's key=value output.
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }

# -- the page: confirm + throttle + time-bucketed notify ----------------------------------------
# handle_arm <box> <verdict> <threshold> <now_epoch> <body-text>
handle_arm() {
  local box="$1" verdict="$2" threshold="$3" now_epoch="$4" body_text="$5"
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "obs-handles_confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$threshold")"
  confirm="$(field "$decision" confirm)"
  act="$(field "$decision" act)"
  write_state_field "obs-handles_confirm_${box}" "${confirm:-0}"
  log "$box $verdict confirm=$prev_confirm -> $confirm act=$act (threshold=$threshold)"
  if [ "${act:-0}" != "1" ]; then
    log "$box $verdict this pass but not yet CONFIRMED across $threshold passes -- holding"
    return 0
  fi
  write_state_field "obs-handles_alerted_${box}" 1

  local prior_sig prior_passes throttle_out alert_now new_sig new_passes
  prior_sig="$(read_state_field "obs-handles_alert_sig_${box}" "")"
  prior_passes="$(read_state_field "obs-handles_alert_passes_${box}" 0)"
  # The signature is the box, never the verdict: GROWING -> CEILING of one ongoing leak is not a new
  # incident to re-fire every pass.
  throttle_out="$(obs_watchdog_alert_throttle "obs-handles:${box}" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(field "$throttle_out" alert_now)"
  new_sig="$(field "$throttle_out" new_sig)"
  new_passes="$(field "$throttle_out" new_passes)"
  write_state_field "obs-handles_alert_sig_${box}" "$new_sig"
  write_state_field "obs-handles_alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert ($verdict): $box -- $body_text (alert_now=$alert_now)"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box $verdict"
    python3 "$NOTIFY" notify --body "$body_text" \
      --dedup-key "$(watchdog_notify_key "obs-handles-${box}" "$now_epoch")" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify ($verdict) failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still $verdict"
  fi
}

# handle_healthy <box> -> clear the confirm + throttle and log a machine-channel recovery line once
# if the box had paged. Recovery is NEVER a phone ping (issue 1206).
handle_healthy() {
  local box="$1"
  if [ "$(recovery_latch_fires "$(read_state_field "obs-handles_alerted_${box}" 0)")" = "1" ]; then
    log "RECOVERY: $box OBS handle count back to normal -- machine-channel only (issue 1206: recovery is not a phone ping)"
    write_state_field "obs-handles_alerted_${box}" 0
  fi
  write_state_field "obs-handles_confirm_${box}" 0
  write_state_field "obs-handles_alert_sig_${box}" ""
  write_state_field "obs-handles_alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
handle_box() {
  local box="$1" ip="$2" body reachable out now_epoch verdict
  local handles pid rate interval ceiling cap htc

  if body="$(fetch_bundle_json "$ip" OBS_HANDLES_FETCH_CMD)"; then reachable=1; else reachable=0; body=""; fi
  now_epoch="${OBS_HANDLES_NOW_EPOCH:-$(date +%s)}"
  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" \
    --now-epoch "$now_epoch" \
    --ref-ident "$(read_state_field "obs-handles_ref_ident_${box}" "")" \
    --ref-handles "$(read_state_field "obs-handles_ref_handles_${box}" "")" \
    --ref-epoch "$(read_state_field "obs-handles_ref_epoch_${box}" "")" \
    --ceiling "$CEILING" --limit-fraction "$LIMIT_FRACTION" --growth-per-h "$GROWTH_PER_H" \
    --min-interval-s "$MIN_INTERVAL_S" 2>/dev/null)"
  verdict="$(field "$out" verdict)"
  handles="$(field "$out" handles)"; pid="$(field "$out" pid)"
  rate="$(field "$out" rate_per_h)"; interval="$(field "$out" interval_s)"
  ceiling="$(field "$out" ceiling)"; cap="$(field "$out" cap)"; htc="$(field "$out" hours_to_cap)"
  if [ -n "$out" ]; then
    write_state_field "obs-handles_ref_ident_${box}" "$(field "$out" next_ident)"
    write_state_field "obs-handles_ref_handles_${box}" "$(field "$out" next_handles)"
    write_state_field "obs-handles_ref_epoch_${box}" "$(field "$out" next_epoch)"
  fi
  log "$box ($ip): reachable=$reachable verdict=${verdict:-<none>} handles=${handles} pid=${pid} rate=${rate}/h interval=${interval}s ceiling=${ceiling} cap=${cap} hours_to_cap=${htc}"

  case "$verdict" in
    SKIP)     log "$box :$BUNDLE_PORT not fetchable -- bundle-state / reach watchdog territory; holding, no page" ;;
    UNKNOWN)  log "$box: no obs_handles facet (older bundle-state server / no readable OBS process) -- holding, no page" ;;
    HOLD)     log "$box: ${interval}s since the reference sample (< ${MIN_INTERVAL_S}s) -- the reference is kept, no verdict this pass" ;;
    BASELINE) log "$box: new reference for OBS pid ${pid} (first reading or a restart)"
              handle_healthy "$box" ;;
    HEALTHY)  handle_healthy "$box" ;;
    GROWING)  handle_arm "$box" GROWING "$GROWTH_CONFIRM" "$now_epoch" \
      "🚨 OBS handle leak ($REPO_SLUG): **$box** ($ip) — OBS (pid ${pid}) drží **${handles} handlov** a pribúda ich **~${rate} za hodinu** súvisle počas ${GROWTH_CONFIRM} kontrol (zdravé OBS je stabilné, stream ~5 800). Pri tomto tempe narazí na limit procesu (${cap}) o ~${htc} h a potom zamrzne alebo spadne. 5.10. to bol plugin Audio Monitor s výstupom na odpojené zvukové zariadenie (1 handle na každý zvukový tick). Treba nájsť príčinu (plugin, zariadenie) a naplánovať reštart OBS." ;;
    CEILING)  handle_arm "$box" CEILING "$CEILING_CONFIRM" "$now_epoch" \
      "🚨 OBS handle leak ($REPO_SLUG): **$box** ($ip) — OBS (pid ${pid}) drží **${handles} handlov**, nad stropom ${ceiling} (zdravé OBS ~5 800, limit procesu ${cap}). Keď narazí na limit, OBS zamrzne alebo spadne. 5.10. to bol plugin Audio Monitor s výstupom na odpojené zvukové zariadenie. Treba nájsť príčinu (plugin, zariadenie) a naplánovať reštart OBS. Potvrdené počas ${CEILING_CONFIRM} kontrol." ;;
    *)        log "$box: unexpected verdict '${verdict:-<empty>}' -- holding, no page" ;;
  esac
}

# require_tools -> exit non-zero (loud) if curl/python3 or the decision module is missing (a silent
# SKIP-forever would leave a real leak unpaged -- the issue-833 preflight class).
require_tools() {
  local missing=() t
  for t in curl python3; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (fix OBS_HANDLES_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, ceiling=${CEILING}, growth>=${GROWTH_PER_H}/h, boxes='$BOXES')"
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
