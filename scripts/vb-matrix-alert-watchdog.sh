#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/audio-lag-alert-watchdog.sh /
# bundle-state-alert-watchdog.sh / network-reach-alert-watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/vb-matrix-alert-watchdog.sh -- #1227: dev1-side ALERT watchdog for a stopped VB-Audio
# Matrix on stream/strih.
#
# WHY (#1227, live incident 2026-08-30 -> 09-02): VB-Audio Matrix (VBAudioMatrix_x64.exe) was NOT
# RUNNING on the stream box from the 2026-08-30 10:45 reboot until 2026-09-02 14:01, because the
# Scheduled Task StartVBMatrix has only a stale one-shot TIME trigger, no AtLogon trigger. Its
# virtual "VB-Matrix VASIO-8" ASIO driver therefore had no host, so BOTH stream OBS inputs bound to
# it (`ASIO Input Capture`, `test-audio`) starved for 3+ days (`asrc: … starved_blocks≈2940/min`)
# while `mbc` (Dante VSC) stayed healthy. NOTHING alarmed: the #1023 asio-starve watchdog ships
# DISABLED and needs a healthy-sibling discriminator; the #1226 audio-lag watchdog reads ts_lag_ms,
# not process presence. This watchdog closes that gap: it reads the `vb_matrix_running` facet
# bundle_state_gather (#1227) now exposes on `:8899/bundle-state.json`, and pages when a box that
# HAS a VB-Matrix install is not running its VBAudioMatrix* process (confirmed across 2 passes).
#
# DETECTION ONLY (alert-only) -- there is deliberately NO auto-action. The cure is
# `schtasks /run /tn StartVBMatrix` on the box (start VB-Matrix into the interactive newlevel
# session) -- an owner/supervisor step, not automation from a dev1 headless timer (which also has no
# session-aware win-* MCP to drive a GUI app). Recovery is log-only (machine channel), never a phone
# ping (.claude/rules/watchdog-notify-dedup.md #1206).
#
# Topology: SAME dev1 alert-watchdog family as audio-lag (#1226) / network-reach (#1001) /
# bundle-state (#732) -- a `set -uo pipefail` systemd `--user` oneshot + timer (5-min cadence), a
# PURE decision core (scripts/vb_matrix_decision.py, #1199 python-mirror), and `airuleset.py notify`
# from dev1. It reuses scripts/lib/obs-watchdog-decision.sh (`obs_watchdog_confirm` 2-pass +
# `obs_watchdog_alert_throttle` ~1h) VERBATIM.
#
# NO reference-anchor / dev1-side-outage guard is needed here (unlike bundle-state #732, which
# RESTARTS tasks + pages on a DOWN box): this watchdog's ONLY page condition is a SUCCESSFULLY
# FETCHED running="0" reading, so a dev1-side path outage makes every fetch fail -> box_reachable=0
# -> SKIP -> no page. A box/`:8899`-down page is #732 / #1001 territory, deferred to here as SKIP.
# A box with no VB-Matrix install (imag) omits the facet -> UNKNOWN -> no page (never a false
# negative on a non-VB-Matrix box).
#
# Usage:
#   scripts/vb-matrix-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/vb-matrix-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/vb-matrix-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "vb-matrix-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The boxes to watch, as "name|ip" pairs (space-separated). Both strih and stream run VB-Matrix;
# a box with no install (imag) simply omits the facet -> UNKNOWN -> never a page.
BOXES="${VB_MATRIX_BOXES:-strih|10.77.9.202 stream|10.77.9.204}"
BUNDLE_PORT="${VB_MATRIX_BUNDLE_PORT:-8899}"          # the bundle-state HTTP service (#650) carrying the facet
BUNDLE_PATH="${VB_MATRIX_BUNDLE_PATH:-/bundle-state.json}"
CURL_TIMEOUT="${VB_MATRIX_CURL_TIMEOUT:-10}"          # :8899 HTTP fetch (s); server has answered ~6.6s

# 2-pass confirm before paging (matches the sibling watchdogs): a single blipped reading must never
# fire. A genuine VB-Matrix outage persists across the 5-min cadence (it was down 3 days).
CONFIRM_THRESHOLD="${VB_MATRIX_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${VB_MATRIX_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

DECIDE="${VB_MATRIX_DECIDE:-$HERE/vb_matrix_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${VB_MATRIX_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${VB_MATRIX_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit override still wins).
_state_default="$STATE_DIR/camera-box-vb-matrix-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-vb-matrix-alert-dryrun.state"
STATE_FILE="${VB_MATRIX_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [vb-matrix-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_bundle_json <ip> -> prints the JSON body to stdout and returns 0 iff a 200 with a body
# that starts with `{` came back. A curl failure or a wedged-but-listening non-JSON answer returns
# 1 (box_reachable=0 for this pass -> SKIP; deferred to #732/#1001). Overridable via
# VB_MATRIX_FETCH_CMD (run with <ip>, stdout = the bundle-state JSON body) for a --dry-run smoke
# test or the Tier-0 harness -- same seam convention as asio-starve's ASIO_STARVE_PROBE_CMD.
fetch_bundle_json() {
  local ip="$1" body
  if [ -n "${VB_MATRIX_FETCH_CMD:-}" ]; then
    body="$($VB_MATRIX_FETCH_CMD "$ip" 2>/dev/null)" || return 1
  else
    body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
      || return 1
  fi
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace (a python-json body carries no
                                            # BOM; a hypothetical BOM'd body fails the {* case -> SKIP,
                                            # the safe direction — never a false page)
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

# A RUNNING box is not an incident: clear its confirm counter AND its throttle sig so a genuinely NEW
# outage later pages fresh instead of being dedup'd against a stale signature. Does NOT clear the
# `alerted` flag -- that is the recovery-ping latch, handled separately.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# net_reach_recovery_decision_local <was_alerted> -> "1" iff a recovery latch should fire (was
# alerted, now running). Kept trivially local (a RUNNING pass IS the "now up" side) so this watchdog
# needs no extra lib; mirrors net_reach_recovery_decision's was_alerted-AND-up shape.
net_reach_recovery_decision_local() {
  [ "${1:-0}" = "1" ] && printf '1' || printf '0'
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip>
handle_box() {
  local box="$1" ip="$2" body reachable verdict running name pid start analyze_out

  if body="$(fetch_bundle_json "$ip")"; then
    reachable=1
  else
    reachable=0
    body=""
  fi

  analyze_out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" 2>/dev/null)"
  verdict="$(printf '%s\n' "$analyze_out" | sed -n 's/^verdict=//p')"
  running="$(printf '%s\n' "$analyze_out" | sed -n 's/^running=//p')"
  name="$(printf '%s\n' "$analyze_out" | sed -n 's/^name=//p')"
  pid="$(printf '%s\n' "$analyze_out" | sed -n 's/^pid=//p')"
  start="$(printf '%s\n' "$analyze_out" | sed -n 's/^start=//p')"
  log "$box ($ip): reachable=$reachable verdict=${verdict:-<none>} running=${running:-} name=${name:-} pid=${pid:-} start=${start:-}"

  case "$verdict" in
    SKIP)
      log "$box :$BUNDLE_PORT not fetchable this pass -- box/:$BUNDLE_PORT-down is #732/#1001 territory; holding vb-matrix state, no page"
      return 0
      ;;
    UNKNOWN)
      log "$box reachable but no vb_matrix_running facet (no VB-Matrix install on this box, e.g. imag, or an old bundle-state-server not serving it) -- no reading, holding state, no page"
      return 0
      ;;
    RUNNING)
      local was_alerted recover
      was_alerted="$(read_state_field "alerted_${box}" 0)"
      recover="$(net_reach_recovery_decision_local "$was_alerted")"
      if [ "$recover" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: $box VB-Matrix running again (${name} pid ${pid})"
        else
          log "RECOVERY: $box VB-Matrix running again (${name} pid ${pid}) -- machine-channel only (#1206: recovery is not a phone ping)"
        fi
        write_state_field "alerted_${box}" 0
      fi
      clear_box_throttle "$box"
      return 0
      ;;
    DOWN) : ;;   # fall through to confirm + alert
    *)
      log "$box: unexpected verdict '${verdict:-<empty>}' from vb_matrix_decision.py (analyze failed?) -- holding state, no page"
      return 0
      ;;
  esac

  # DOWN -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box VB-Matrix DOWN this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED VB-Matrix down -> latch recovery, throttled alert.
  write_state_field "alerted_${box}" 1

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="vbmatrixdown:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box VB-Matrix CONFIRMED DOWN (install present, no VBAudioMatrix* process) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box VB-Matrix down"
    python3 "$NOTIFY" notify --body \
      "🚨 VB-Matrix down ($REPO_SLUG): **$box** ($ip) — VB-Audio Matrix (VBAudioMatrix*) NEBEŽÍ, hoci je na boxe nainštalovaný. Jeho virtuálny ASIO driver 'VB-Matrix VASIO-8' tak nemá hostiteľa → OBS ASIO vstupy naň (napr. 'ASIO Input Capture' zo cam2) hladujú (starved_blocks≈2940/interval) a zvuk z nich nejde. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol. Náprava (owner/supervisor krok): na boxe spusti \`schtasks /run /tn StartVBMatrix\`, potom over že proces beží a starved_blocks spadne na 0. Trvalé riešenie: pridať AtLogon trigger na task StartVBMatrix (po reboote sa inak VB-Matrix nespustí)." \
      --dedup-key "vb-matrix-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- still down"
  fi
}

# require_tools -> exit non-zero (loud) if a REQUIRED external tool OR the decision module is
# missing. A missing `curl` would make every fetch fail -> every box SKIP; a missing/unreadable
# $DECIDE would make `analyze` emit nothing -> every box "unexpected verdict, holding" -> both are
# SILENT FOREVER (a real VB-Matrix outage goes unpaged), exactly the "a missing dependency must fail
# LOUD by name, never read as a measured zero" class .claude/rules/imag-ssh-remote-tool-preflight.md
# (#833) exists to prevent. `timeout` is NOT required: curl bounds itself with --max-time and the
# local python decision reads already-fetched stdin (it cannot hang on the network).
require_tools() {
  local missing=() t need=(python3)
  # curl is the REAL fetch; a VB_MATRIX_FETCH_CMD override (--dry-run / harness) supplies the body
  # itself, so curl is not needed then. python3 (the decision) is always required.
  [ -z "${VB_MATRIX_FETCH_CMD:-}" ] && need+=(curl)
  for t in "${need[@]}"; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing curl/python3 would silently SKIP every box and never page a real VB-Matrix outage)"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (analyze would emit nothing -> every box 'holding' forever, a real outage unpaged; fix VB_MATRIX_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, boxes='$BOXES')"
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
