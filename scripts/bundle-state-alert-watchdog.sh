#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/network-reach-alert-watchdog.sh /
# optical-chain-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/bundle-state-alert-watchdog.sh -- #732: dev1-side ACTIVE health-check watchdog for the
# strih/stream `:8899` BundleStateServer, with auto-restart.
#
# WHY (#732, four live recurrences through 2026-08-13): the strih/stream `BundleStateServer`
# Scheduled Task dies with `SCHED_S_TASK_TERMINATED` (`0x40010004`, an informational/SUCCESS class)
# on session/parent teardown (dominant post-reboot), so Windows Task Scheduler's restart-on-failure
# (`RestartCount=999`) never engages; it also failed to restart a real `0xC000013A` crash (silent 3
# days) and cannot cover a cold-start that never fired at all. Nothing off the box probes `:8899`, so
# the version-integrity E2E gate reads the box UNKNOWN and blames itself. A passive Task-Scheduler
# policy can never cover a non-failure termination -- this needs an ACTIVE external prober (dev1)
# that restarts the task regardless of its last exit code.
#
# The dev1-side network-reachability watchdog (#1001) already probes `:8899`, but ONLY as one of
# three OR-signals for "is the box on the network at all", so a `:8899`-only death while the box is
# otherwise fully up classifies the box REACHABLE and never pages. THIS watchdog closes that gap: a
# box that is UP (ping OR OBS-WS :4455) but whose `:8899/bundle-state.json` does NOT serve 200+JSON
# is CONFIRMED across 2 passes, then (a) auto-restarted via `schtasks /run /tn BundleStateServer`
# over ssh -- session-agnostic (a HIDDEN, headless supervisor task; never the `/it` form) per
# .claude/rules/win-ssh-vs-mcp.md -- and (b) a throttled Discord alert fires. A box that is FULLY
# unreachable (ping + :4455 + :8899 all down) is deferred to the #1001 watchdog (no double-page, no
# pointless restart against a dark box).
#
# Detection is by `curl` FROM dev1 (200 + a JSON body -- catches a wedged-but-listening server, and
# is the method the ops-SKILL note mandates: an MCP-side Invoke-WebRequest hangs even when the server
# logs a prompt 200). Alerting/restart run from dev1, which has the airuleset checkout + Discord
# credentials + ssh reach; the boxes have none. SAME topology as network-reach / imag-obs /
# optical-chain alert watchdogs.
#
# Usage:
#   scripts/bundle-state-alert-watchdog.sh            # one pass: measure -> decide -> restart+alert
#   scripts/bundle-state-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never restart/alert
#   scripts/bundle-state-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/network-reach-health.sh
. "$HERE/lib/network-reach-health.sh"
# shellcheck source=scripts/lib/bundle-state-health.sh
. "$HERE/lib/bundle-state-health.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,44p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "bundle-state-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The two OBS boxes to watch, as "name|ip" pairs (space-separated).
BOXES="${BUNDLE_STATE_BOXES:-strih|10.77.9.202 stream|10.77.9.204}"
OBS_WS_PORT="${BUNDLE_STATE_OBS_WS_PORT:-4455}"       # OBS WebSocket, live on both boxes (box-up signal)
BUNDLE_PORT="${BUNDLE_STATE_BUNDLE_PORT:-8899}"       # the bundle-state HTTP service under test (#650)
BUNDLE_PATH="${BUNDLE_STATE_BUNDLE_PATH:-/bundle-state.json}"
# Reference rig nodes that share the rig's network fate (cam1 cam2 imag-nb) -- the dev1-side-outage
# anchor. If NONE answer AND no watched box is reachable, dev1's own path to the rig subnet is down.
REFERENCE_HOSTS="${BUNDLE_STATE_REFERENCE_HOSTS:-10.77.9.61 10.77.9.62 10.77.9.182}"

PING_COUNT="${BUNDLE_STATE_PING_COUNT:-2}"
PING_TIMEOUT="${BUNDLE_STATE_PING_TIMEOUT:-2}"        # per-packet wait (s); generous for a mobile link
TCP_TIMEOUT="${BUNDLE_STATE_TCP_TIMEOUT:-4}"          # per :4455 TCP connect (s)
CURL_TIMEOUT="${BUNDLE_STATE_CURL_TIMEOUT:-10}"       # :8899 HTTP fetch (s); the server has been seen
                                                       # answering in ~6.6s, so keep this generous
SSH_TIMEOUT="${BUNDLE_STATE_SSH_TIMEOUT:-25}"         # bound the whole restart ssh (never wedge a pass)

# Auto-restart the scheduled task on a confirmed DOWN (the #732 self-heal). Set 0 for alert-only.
AUTO_RESTART="${BUNDLE_STATE_AUTO_RESTART:-1}"

# 2-pass confirm before acting (matches the sibling watchdogs): a single slow/blipped probe must
# never trigger a restart or a page. A genuinely dead :8899 stays down across the 5-min cadence.
CONFIRM_THRESHOLD="${BUNDLE_STATE_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${BUNDLE_STATE_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${BUNDLE_STATE_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${BUNDLE_STATE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit override still wins).
_state_default="$STATE_DIR/camera-box-bundle-state-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-bundle-state-alert-dryrun.state"
STATE_FILE="${BUNDLE_STATE_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [bundle-state-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- per-box ssh creds (same convention as obs-session-watchdog.sh; targets.md "SSH: newlevel/newlevel")
box_ssh_user() {
  case "$1" in
    strih) printf '%s' "${STRIH_USER:-newlevel}" ;;
    stream) printf '%s' "${STREAM_USER:-newlevel}" ;;
    *) printf '%s' "${BUNDLE_STATE_SSH_USER:-newlevel}" ;;
  esac
}
box_ssh_pw() {
  case "$1" in
    strih) printf '%s' "${STRIH_PW:-newlevel}" ;;
    stream) printf '%s' "${STREAM_PW:-newlevel}" ;;
    *) printf '%s' "${BUNDLE_STATE_SSH_PW:-newlevel}" ;;
  esac
}

# -- I/O probes (dev1-local; NOT pure -- kept out of the lib) ------------------------------------
# probe_ping <ip> -> stdout: 1 (a reply came back) | 0
probe_ping() {
  local ip="$1"
  if ping -c "$PING_COUNT" -W "$PING_TIMEOUT" "$ip" >/dev/null 2>&1; then
    printf '1'
  else
    printf '0'
  fi
}
# probe_tcp <ip> <port> -> stdout: 1 (a TCP connect succeeded) | 0. bash /dev/tcp, no nc dependency;
# `timeout` bounds a filtered/no-route port that would otherwise hang.
probe_tcp() {
  local ip="$1" port="$2"
  # $ip/$port passed as positional args ($0/$1), never interpolated into the -c string, so a config
  # value can never be shell-injected. Single quotes DELIBERATE: $0/$1 expand in the INNER bash.
  # shellcheck disable=SC2016
  if timeout "$TCP_TIMEOUT" bash -c 'exec 3<>/dev/tcp/"$0"/"$1"' "$ip" "$port" >/dev/null 2>&1; then
    printf '1'
  else
    printf '0'
  fi
}
# probe_http_bundle <ip> -> stdout: 1 (200 AND a JSON body) | 0. `curl -f` fails on non-2xx; the
# body-starts-with-`{` check additionally catches a wedged-but-listening server that answers non-JSON.
probe_http_bundle() {
  local ip="$1" body
  body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
    || { printf '0'; return 0; }
  # Strip any leading whitespace/newline/BOM, then require the body to START with `{` (a real JSON
  # object) -- so a 200 with a leading blank line does not false-read as DOWN, while a wedged server
  # answering non-JSON still reads DOWN.
  body="${body#"${body%%[![:space:]]*}"}"
  case "$body" in
    \{*) printf '1' ;;
    *) printf '0' ;;
  esac
}

# restart_bundle_state_task <box> <ip> -> exit 0 iff the ssh `schtasks /run` returned 0. Session-
# agnostic (starts the HIDDEN headless task; never `/it`). Self-contained sshpass -- deliberately does
# NOT source win-ssh-exec.sh (that sets `set -euo pipefail`, which would leak -e into this watchdog).
# Best-effort: a failure is logged by the caller, never fatal (the alert still fires). `timeout`
# bounds a hung ssh so a restart attempt can never wedge the pass.
restart_bundle_state_task() {
  local box="$1" ip="$2" user pw
  user="$(box_ssh_user "$box")"; pw="$(box_ssh_pw "$box")"
  timeout "$SSH_TIMEOUT" sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
    -o ServerAliveInterval=10 -o ServerAliveCountMax=3 \
    "${user}@${ip}" "$(bundle_state_restart_remote_cmd)" >/dev/null 2>&1
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
  # mktemp-failure fallback (a direct rewrite of STATE_FILE) can never truncate-before-read and drop
  # them (the sibling watchdogs' `tmp=$STATE_FILE` fallback has exactly that latent state-loss bug).
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    # mktemp unavailable: `existing` is already captured, so a direct (non-atomic) rewrite is safe.
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# A HEALTHY box is not an incident: clear its confirm counter AND its throttle sig so a genuinely NEW
# outage later pages fresh instead of being dedup'd against a stale signature. Does NOT clear the
# `alerted` flag -- that is the recovery-ping latch, handled separately.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip> <ping_ok> <ws_ok> <bundle_ok> -- probe results gathered ONCE in main().
handle_box() {
  local box="$1" ip="$2" ping_ok="$3" ws_ok="$4" bundle_ok="$5"
  local box_reachable verdict
  box_reachable="$(bundle_state_box_reachable "$ping_ok" "$ws_ok")"
  verdict="$(bundle_state_classify "$box_reachable" "$bundle_ok")"
  log "$box ($ip): ping=$ping_ok ws:$OBS_WS_PORT=$ws_ok bundle:$BUNDLE_PORT=$bundle_ok box_reachable=$box_reachable -> $verdict"

  if [ "$verdict" = "BOX_UNREACHABLE" ]; then
    log "$box is fully UNREACHABLE (box down) -- deferring to the #1001 network-reach watchdog; nothing to decide here"
    return 0
  fi

  if [ "$verdict" = "HEALTHY" ]; then
    local was_alerted recover
    was_alerted="$(read_state_field "alerted_${box}" 0)"
    recover="$(net_reach_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
    if [ "$recover" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: $box :$BUNDLE_PORT serving again"
      else
        log "RECOVERY: $box :$BUNDLE_PORT serving again -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${box}" 0
    fi
    clear_box_throttle "$box"
    return 0
  fi

  # DOWN (box up, :8899 not serving) -> confirm across consecutive passes before acting.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box :$BUNDLE_PORT DOWN this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED :8899 outage on an otherwise-up box -> latch recovery, auto-restart, throttled alert.
  write_state_field "alerted_${box}" 1

  local restart_note="auto-restart disabled"
  if [ "$AUTO_RESTART" = "1" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      restart_note="[dry-run] WOULD run: $(bundle_state_restart_remote_cmd) on $box"
      log "[dry-run] WOULD auto-restart BundleStateServer on $box via ssh"
    elif restart_bundle_state_task "$box" "$ip"; then
      restart_note="auto-restart (schtasks /run) issued OK"
      log "AUTO-RESTART: schtasks /run BundleStateServer issued OK on $box (recovery confirmed next pass)"
    else
      restart_note="auto-restart (schtasks /run) FAILED (ssh/creds?) -- alert still firing"
      log "AUTO-RESTART: schtasks /run FAILED on $box (ssh/creds?) -- alerting anyway"
    fi
  fi

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="bundlestate:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  detail="$(bundle_state_alert_detail "$box" "$ip" "$ping_ok" "$ws_ok" "$bundle_ok")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED :$BUNDLE_PORT down ($detail) alert_now=$alert_now restart=[$restart_note]"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box :$BUNDLE_PORT down"
    python3 "$NOTIFY" notify --body \
      "🚨 BundleStateServer ($REPO_SLUG): **$box** ($ip) :$BUNDLE_PORT je DOLE, hoci box beží. ${detail}. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol — Task Scheduler ukončenú úlohu sám nereštartuje. Rieši Claude automaticky (${restart_note}), ty nemusíš nič robiť." \
      --dedup-key "bundle-state-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES}) -- restart still attempted every pass"
  fi
}

# require_tools -> exit non-zero (loud) if a REQUIRED external tool is missing. A missing `curl`
# would otherwise make probe_http_bundle return 0 for EVERY box, so both boxes would false-classify
# DOWN and get a real `schtasks /run` restart + a Discord page -- exactly the "a missing tool must
# fail LOUD by name, never read as a measured zero" class .claude/rules/imag-ssh-remote-tool-preflight.md
# (#833) exists to prevent. `sshpass` is deliberately NOT required: its absence degrades safely to
# alert-only (the restart fails, the alert still fires).
require_tools() {
  local missing=() t
  for t in curl ping timeout; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing curl would false-read :$BUNDLE_PORT as DOWN and trigger false restarts/alerts on every box)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, auto_restart=$AUTO_RESTART, boxes='$BOXES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  # -- gather each box's probes ONCE (feeds both the anchor and the per-box decision) --
  local pair box ip names=() ips=() pings=() wss=() bundles=()
  local anchor_reach_flags=()
  for pair in $BOXES; do
    box="${pair%%|*}"; ip="${pair##*|}"
    names+=("$box"); ips+=("$ip")
    local p w b
    p="$(probe_ping "$ip")"; w="$(probe_tcp "$ip" "$OBS_WS_PORT")"; b="$(probe_http_bundle "$ip")"
    pings+=("$p"); wss+=("$w"); bundles+=("$b")
    # For the anchor, ANY live signal (incl. :8899) proves dev1<->rig connectivity.
    anchor_reach_flags+=("$([ "$p" = 1 ] || [ "$w" = 1 ] || [ "$b" = 1 ] && echo 1 || echo 0)")
  done

  # -- dev1-side-outage guard --------------------------------------------------------------------
  # dev1's path to the rig subnet is PROVEN up if ANY reference rig node answers ping OR any watched
  # box is reachable in any way. Only when NOTHING is reachable is the pass "nothing to decide"
  # (per-box state untouched), so a dev1-side uplink flap never false-restarts/pages. An EMPTY
  # reference set disables the reference half (the box-reachability proof still applies).
  local ref anchor_flags=() anchor
  for ref in $REFERENCE_HOSTS; do
    anchor_flags+=("$(probe_ping "$ref")")
  done
  anchor_flags+=("${anchor_reach_flags[@]}")
  anchor="$(net_reach_any_reachable "${anchor_flags[@]}")"
  if [ "$anchor" != "1" ]; then
    log "no reference rig node AND no watched box reachable -- dev1-side path to the rig subnet is down -- nothing to decide this pass"
    log "pass end"
    return 0
  fi

  local i
  for i in "${!names[@]}"; do
    handle_box "${names[$i]}" "${ips[$i]}" "${pings[$i]}" "${wss[$i]}" "${bundles[$i]}"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
