#!/usr/bin/env bash
# scripts/genlock-lock-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so it
# runs with `-e` OFF -- the sibling convention (scripts/audio-lag-alert-watchdog.sh /
# bundle-state-alert-watchdog.sh / network-reach-alert-watchdog.sh all use `set -uo pipefail`, NOT
# -e). The `set -euo pipefail` above satisfies the new-.sh script-failure-policy check; this turns
# -e back off so one box's fetch/decision hiccup never aborts the whole pass.
set +e
set -uo pipefail
#
# scripts/genlock-lock-alert-watchdog.sh -- #1299: dev1-side ALERT watchdog for the fleet genlock
# LOCK state leaving LOCKED.
#
# WHY (#1299): the in-OBS genlock LOCK indicator (#1298) is visible ONLY to an operator standing at
# each box's statusbar. The fleet (dev1) had no way to SEE whether strih/stream/imag/resolume are
# LIVE-LOCKED to the fleet clock, and no page fired when a box silently left LOCKED -- the exact
# silent-degradation class the dev1 alert-watchdog family (#732/#1001/#1226) exists to close. This
# watchdog reads the `genlock_lock` facet bundle_state_gather (#1299) now exposes on
# `:8899/bundle-state.json` -- the DECIDED three-state verdict the #1298 statusbar emits on its
# genlock-lock-json: line -- and pages when a box is UNLOCKED or DEGRADED (confirmed across 2
# passes).
#
# DETECTION ONLY (alert-only) -- there is deliberately NO auto-action. The cure for a genuinely
# unlocked box is a rig-ops decision (restart OBS / dantesync / an NDI reattach), not something a
# dev1 timer should drive blind. Recovery is log-only (machine channel), never a phone ping
# (.claude/rules/watchdog-notify-dedup.md #1206).
#
# Topology: SAME dev1 alert-watchdog family as audio-lag (#1226) / network-reach (#1001) /
# bundle-state (#732) -- a `set -uo pipefail` systemd `--user` oneshot + timer (5-min cadence), a
# PURE decision core (scripts/genlock_lock_decision.py, #1199 python-mirror pattern), and
# `airuleset.py notify` from dev1. It reuses scripts/lib/obs-watchdog-decision.sh
# (`obs_watchdog_confirm` 2-pass + `obs_watchdog_alert_throttle` ~1h) VERBATIM, and derives its box
# roster from scripts/lib/obs-fleet.sh's `genlock-lock` facet (#1296) so a new box is one table edit.
#
# SCOPE: strih, stream, imag, resolume. resolume is a TRAVELING CG box -- it is PAGED only while
# `obs_fleet_is_home resolume` holds (the same #1296 condition the other traveling-safe watchdogs
# use); away, it is skipped entirely (never a false page against a box that is simply not here).
#
# NO reference-anchor / dev1-side-outage guard is needed here (unlike bundle-state #732): this
# watchdog's ONLY page condition is a SUCCESSFULLY FETCHED facet reading UNLOCKED/DEGRADED, so a
# dev1-side path outage makes every fetch fail -> box_reachable=0 -> SKIP -> no page. A box/`:8899`-
# down page is #732 / #1001 territory, deferred to here as SKIP.
#
# Usage:
#   scripts/genlock-lock-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/genlock-lock-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/genlock-lock-alert-watchdog.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '9,44p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "genlock-lock-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The OBS boxes to watch, as "name|ip" pairs (space-separated). Default DERIVED from the ONE
# declared fleet list (scripts/lib/obs-fleet.sh, #1296): obs_fleet_boxes genlock-lock yields
# strih-lx+stream+resolume (issue 1317: strih-lx is the production strih; imag is retired and
# dropped centrally). resolume is gated per-pass on obs_fleet_is_home (below). The
# GENLOCK_LOCK_BOXES env override still wins unchanged.
BOXES="${GENLOCK_LOCK_BOXES:-$(obs_fleet_boxes genlock-lock)}"
BUNDLE_PORT="${GENLOCK_LOCK_BUNDLE_PORT:-8899}"        # the bundle-state HTTP service carrying the facet
BUNDLE_PATH="${GENLOCK_LOCK_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${GENLOCK_LOCK_CURL_TIMEOUT:-10}"        # :8899 HTTP fetch (s)

# 2-pass confirm before paging (matches the sibling watchdogs): a single blipped reading (a reload,
# a one-tick relock) must never fire. A genuine unlock persists across the 5-min cadence.
CONFIRM_THRESHOLD="${GENLOCK_LOCK_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${GENLOCK_LOCK_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

DECIDE="${GENLOCK_LOCK_DECIDE:-$HERE/genlock_lock_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${GENLOCK_LOCK_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${GENLOCK_LOCK_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit override still wins).
_state_default="$STATE_DIR/camera-box-genlock-lock-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-genlock-lock-alert-dryrun.state"
STATE_FILE="${GENLOCK_LOCK_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [genlock-lock-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body to stdout and returns 0 iff a 200 with a body that
# starts with `{` came back. A curl failure or a wedged-but-listening non-JSON answer returns 1
# (box_reachable=0 for this pass -> SKIP; deferred to #732/#1001).
#
# GENLOCK_LOCK_FETCH_CMD (Tier-0 seam): if set, it is an executable invoked as `<cmd> <ip>` whose
# stdout REPLACES the curl fetch -- so a `--dry-run` against a CAPTURED bundle-state fixture needs no
# live box (the acceptance criterion: "the dev1 watchdog --dry-run classifies a captured UNLOCKED
# fixture"), and a glue harness can stub reachable/unreachable deterministically. The same
# `<cmd> <ip>`-returns-the-body seam as ndi_halving's NDI_HALVING_PROBE_CMD / vb-matrix's
# VB_MATRIX_FETCH_CMD. A non-zero exit (or a non-`{` body) still reads as unreachable -> SKIP.
fetch_bundle_json() {
  local ip="$1" body
  if [ -n "${GENLOCK_LOCK_FETCH_CMD:-}" ]; then
    body="$("$GENLOCK_LOCK_FETCH_CMD" "$ip" 2>/dev/null)" || return 1
  else
    body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
      || return 1
  fi
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace
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

# A LOCKED box is not an incident: clear its confirm counter AND its throttle sig so a genuinely NEW
# unlock later pages fresh instead of being dedup'd against a stale signature. Does NOT clear the
# `alerted` flag -- that is the recovery-ping latch, handled separately.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# genlock_lock_recovery_decision <was_alerted> -> "1" iff a recovery latch should fire (was
# alerted, now healthy). Kept trivially local (a HEALTHY pass IS the "now locked" side); mirrors the
# audio-lag sibling's was_alerted-AND-up shape.
genlock_lock_recovery_decision() {
  [ "${1:-0}" = "1" ] && printf '1' || printf '0'
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip>
handle_box() {
  local box="$1" ip="$2" body reachable analyze_out verdict state reason n_absent qpc_ppm qpc_exp

  # resolume is a TRAVELING box -- page it only while home (the #1296 condition). strih/stream/imag
  # are home-check=always so this never skips them; resolume away -> no fetch, no page.
  if ! obs_fleet_is_home "$box"; then
    log "$box away (obs_fleet_is_home false) -- traveling box, skipping this pass (no fetch, no page)"
    return 0
  fi

  if body="$(fetch_bundle_json "$ip")"; then
    reachable=1
  else
    reachable=0
    body=""
  fi

  analyze_out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" 2>/dev/null)"
  verdict="$(printf '%s\n' "$analyze_out" | sed -n 's/^verdict=//p')"
  state="$(printf '%s\n' "$analyze_out" | sed -n 's/^state=//p')"
  reason="$(printf '%s\n' "$analyze_out" | sed -n 's/^reason=//p')"
  # #1299: senderless input count -- observability only (an absent sender never pages; it is
  # excluded from the widget's DEGRADED gate). Logged so a HEALTHY box with idle inputs is visible.
  n_absent="$(printf '%s\n' "$analyze_out" | sed -n 's/^n_absent=//p')"
  # #1299 Part 4: windowed wall-vs-QPC drift telemetry -- observability only (the widget folded the
  # qpc_drift verdict -- since #1357 the wall STEP only -- into `state`; no rate ever pages). Logged so a
  # genuine rate anomaly (measured far off the dantesync-reported expected slew) is visible in-band.
  qpc_ppm="$(printf '%s\n' "$analyze_out" | sed -n 's/^qpc_drift_ppm=//p')"
  qpc_exp="$(printf '%s\n' "$analyze_out" | sed -n 's/^qpc_expected_ppm=//p')"
  log "$box ($ip): reachable=$reachable verdict=${verdict:-<none>} state=${state:-} reason=${reason:-} n_absent=${n_absent:-} qpc_drift_ppm=${qpc_ppm:-} qpc_expected_ppm=${qpc_exp:-}"

  case "$verdict" in
    SKIP)
      log "$box :$BUNDLE_PORT not fetchable this pass -- box/:$BUNDLE_PORT-down is #732/#1001 territory; holding genlock-lock state, no page"
      return 0
      ;;
    UNKNOWN)
      log "$box reachable but no genlock_lock facet (a stock OBS, or no genlock-lock-json: line in the tail yet) -- no reading, holding state, no page"
      return 0
      ;;
    HEALTHY)
      local was_alerted recover
      was_alerted="$(read_state_field "alerted_${box}" 0)"
      recover="$(genlock_lock_recovery_decision "$was_alerted")"
      if [ "$recover" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: $box genlock back to LOCKED"
        else
          log "RECOVERY: $box genlock back to LOCKED -- machine-channel only (#1206: recovery is not a phone ping)"
        fi
        write_state_field "alerted_${box}" 0
      fi
      clear_box_throttle "$box"
      return 0
      ;;
    DEGRADED | UNLOCKED) : ;;   # fall through to confirm + alert
    *)
      log "$box: unexpected verdict '${verdict:-<empty>}' from genlock_lock_decision.py (analyze failed?) -- holding state, no page"
      return 0
      ;;
  esac

  # UNLOCKED/DEGRADED -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box genlock $verdict this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED -> latch recovery, throttled alert.
  write_state_field "alerted_${box}" 1

  # The throttle signature includes the VERDICT so a DEGRADED->UNLOCKED escalation re-fires (a
  # genuinely different condition); the Discord --dedup-key stays box-scoped (#1206) so a repeated
  # identical state EDITS the one card instead of re-pinging.
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="genlocklock:${box}:${verdict}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box genlock CONFIRMED $verdict (reason '${reason}') alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box genlock $verdict (reason '${reason}')"
    python3 "$NOTIFY" notify --body \
      "🚨 Genlock-lock ($REPO_SLUG): **$box** ($ip) opustil LOCKED -- stav **$verdict** (dôvod '${reason}'). Box nie je zosynchronizovaný na fleet clock; na streame to môže rozhodiť A/V aj zosúladenie kamier. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol. Skontroluj genlock indikátor na boxe (dantesync / NDI vstupy / wall-stamping); náprava je rig-ops rozhodnutie (reštart OBS/dantesync alebo NDI reattach)." \
      --dedup-key "$(watchdog_notify_key "genlock-lock-$box" "$(date +%s)")" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still $verdict"
  fi
}

# require_tools -> exit non-zero (loud) if a REQUIRED external tool OR the decision module is
# missing. A missing `curl` would make every fetch fail -> every box SKIP; a missing/unreadable
# $DECIDE would make `analyze` emit nothing -> every box "unexpected verdict, holding" -> both are
# SILENT FOREVER (a real unlock goes unpaged), exactly the "a missing dependency must fail LOUD by
# name, never read as a measured zero" class .claude/rules/imag-ssh-remote-tool-preflight.md (#833).
require_tools() {
  local missing=() t
  for t in curl python3; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing curl/python3 would silently SKIP every box and never page a real genlock unlock)"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (analyze would emit nothing -> every box 'holding' forever, a real unlock unpaged; fix GENLOCK_LOCK_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, confirm=$CONFIRM_THRESHOLD, boxes='$BOXES')"
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
