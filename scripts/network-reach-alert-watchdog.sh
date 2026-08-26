#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/imag-power-envelope-alert-watchdog.sh /
# optical-chain-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/network-reach-alert-watchdog.sh -- #1001: strih/stream network-UNREACHABLE alert,
# DEV1-SIDE.
#
# WHY (#1001, live 2026-08-06 07:57-10:51 ~50 min + recurrence 2026-08-13): strih's optical NIC died
# and the box fell fully off the network -- no DHCP, ARP INCOMPLETE, OBS-WS + ssh + win-strih MCP all
# dead -- while stream's `NDI 2ME PGM` silently held the last frozen frame the whole time. NO Discord
# alert fired. Every existing watchdog probes a box it assumes is UP: obs-liveness (#391) polls OBS-WS
# GetStats (no WS on a dead box -> its own "no probe output = nothing to decide" short-circuit),
# obs-session (#979) "by design does NOT alert on unreachability", imag-obs/imag-power/optical-chain
# ssh INTO the box first. So a dead NIC / powered-off box / unplugged cable is nobody's job. The
# reachability question can only be answered by a prober that is UP while the target is DOWN -- dev1,
# which sits on the same rig LAN and already hosts every dev1-side alert timer + the airuleset
# checkout + Discord credentials (the SAME topology as imag-obs-alert-watchdog.sh #882 /
# imag-power-envelope-alert-watchdog.sh #1040 / optical-chain-alert-watchdog.sh #860).
#
# A DEV1 systemd --user timer probes strih (10.77.9.202) + stream (10.77.9.204) from dev1 with a
# MULTI-SIGNAL check -- ping OR the OBS-WS :4455 TCP port OR the bundle-state :8899 TCP port (both
# ports live on both boxes) -- so a single dropped ping or a Windows box that firewalls ICMP but
# answers TCP is never a false outage. A box is UNREACHABLE only when ALL THREE fail. The shared PURE
# net_reach_* decisions (scripts/lib/network-reach-health.sh) classify each box; the confirm-counter +
# alert throttle are the SAME pure obs_watchdog_confirm / obs_watchdog_alert_throttle
# (scripts/lib/obs-watchdog-decision.sh) #391/#882/#1040 already use -- no second alert mechanism.
# Per-box state, so strih and stream page independently. A recovery ("reachable again") ping fires
# once when a box we paged for returns.
#
# NOTE (issue 1199): strih ALSO carries an ON-BOX NIC-fail self-heal watcher
# (scripts/strih-nic-selfheal.ps1, a SYSTEM scheduled task) that, until the flaky card is physically
# replaced, restarts the NIC and then gracefully reboots strih on a confirmed total LAN outage. This
# dev1-side alert is unchanged and complementary: it still pages so a human knows, while the on-box
# watcher attempts recovery -- do NOT wait for a manual fix on a strih outage.
#
# DEV1-SIDE-OUTAGE GUARD: before deciding, probe a set of REFERENCE rig nodes (cam1/cam2/imag-nb --
# nodes that share the rig's network fate). If NONE answer, dev1's own path to the rig subnet is down
# (or the whole rig link stalled, e.g. an event-day mobile uplink) -> "nothing to decide" this pass,
# per-box state untouched, never a false "both OBS boxes down". Mirrors the siblings' "empty ssh probe
# = nothing to decide" discipline and encodes the ticket's own discriminator (every OTHER rig node up).
#
# Usage:
#   scripts/network-reach-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/network-reach-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/network-reach-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/network-reach-health.sh
. "$HERE/lib/network-reach-health.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "network-reach-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The boxes to watch, as "name|ip" pairs (space-separated). strih/stream are permanent PAGING boxes;
# resolume (#811) is a TRAVELING CG box (RESOLUME-SNV) added here as a REPORT-ONLY node (see below)
# so it is monitored without false-paging while it is powered off/away between events. resolume.lan
# currently resolves to 10.77.9.201 (event-LAN DHCP -- may drift, and collides with `bridge`, an
# ACTIVE box, in targets.md) -- harmless for a report-only node: a wrong/colliding IP may LOG a
# FALSE reachable (e.g. bridge answering at .201 while resolume is off) or a false unreachable, but
# NEVER pages. Always confirm box identity with `getent hosts resolume.lan` + its OBS profile
# (rig-state-inspection.md §2) before ever flipping it to a paging node.
BOXES="${NETWORK_REACH_BOXES:-strih|10.77.9.202 stream|10.77.9.204 resolume|10.77.9.201}"
# Report-only boxes (space-separated NAMES): probed + classified + logged + per-box state-tracked
# exactly like any other, but they NEVER page (no alert, no recovery ping) -- for a TRAVELING box
# whose absence is the NORMAL state (resolume, #811). A supervisor "flips one required" by removing
# its name here (it stays in BOXES), at which point it pages like strih/stream with all
# confirm/throttle/recovery state already warm. net_reach_box_is_report_only (lib) is the pure test.
REPORT_ONLY_BOXES="${NETWORK_REACH_REPORT_ONLY_BOXES:-resolume}"
OBS_WS_PORT="${NETWORK_REACH_OBS_WS_PORT:-4455}"       # OBS WebSocket, live on both OBS boxes
BUNDLE_PORT="${NETWORK_REACH_BUNDLE_PORT:-8899}"       # bundle-state HTTP, on strih/stream only (#650)
# Reference rig nodes that share the rig's network fate (cam1 cam2 imag-nb) -- the dev1-side-outage
# anchor. If NONE answer, dev1's own path to the rig subnet is down -> nothing to decide.
REFERENCE_HOSTS="${NETWORK_REACH_REFERENCE_HOSTS:-10.77.9.61 10.77.9.62 10.77.9.182}"

PING_COUNT="${NETWORK_REACH_PING_COUNT:-2}"
PING_TIMEOUT="${NETWORK_REACH_PING_TIMEOUT:-2}"        # per-packet wait (s); generous for event-day mobile link
TCP_TIMEOUT="${NETWORK_REACH_TCP_TIMEOUT:-4}"          # per TCP connect (s)

# 2-pass confirm before paging (matches the sibling watchdogs): a single full-LAN blip / packet storm
# must never page. A genuinely off-the-wire box stays down across the 5-min cadence.
CONFIRM_THRESHOLD="${NETWORK_REACH_ALERT_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${NETWORK_REACH_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${NETWORK_REACH_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${NETWORK_REACH_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
# A manual --dry-run defaults to a SEPARATE state file so it never consumes a pending recovery latch
# or advances the live throttle counters of the real timer (an explicit STATE_FILE override still
# wins, for a test that deliberately wants to inspect a specific state).
_state_default="$STATE_DIR/camera-box-network-reach-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-network-reach-alert-dryrun.state"
STATE_FILE="${NETWORK_REACH_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [network-reach-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

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
# probe_tcp <ip> <port> -> stdout: 1 (a TCP connect succeeded) | 0. Uses bash /dev/tcp so no
# netstat/nc dependency; `timeout` bounds a filtered/no-route port that would otherwise hang.
probe_tcp() {
  local ip="$1" port="$2"
  # $ip/$port passed as positional args ($0/$1), never interpolated into the -c string, so a
  # config value can never be shell-injected into the probe. The single quotes are DELIBERATE:
  # $0/$1 must expand in the INNER bash, not here.
  # shellcheck disable=SC2016
  if timeout "$TCP_TIMEOUT" bash -c 'exec 3<>/dev/tcp/"$0"/"$1"' "$ip" "$port" >/dev/null 2>&1; then
    printf '1'
  else
    printf '0'
  fi
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
  # mktemp-failure fallback (a direct rewrite of STATE_FILE) can never truncate-before-read and
  # drop them (the previous `tmp=$STATE_FILE` fallback had exactly that latent state-loss bug;
  # fixed here in-line at the issue-732 round integration, mirroring bundle-state-alert-watchdog).
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s
' "$existing"; printf '%s=%s
' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    # mktemp unavailable: `existing` is already captured, so a direct (non-atomic) rewrite is safe.
    { [ -n "$existing" ] && printf '%s
' "$existing"; printf '%s=%s
' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# A REACHABLE box is not an incident: clear its confirm counter AND its throttle sig so a genuinely
# NEW outage later pages fresh instead of being dedup'd against a stale signature (mirrors the sibling
# reset discipline). Does NOT clear the `alerted` flag -- that is the recovery-ping latch, handled
# separately so a box we paged for still emits a "reachable again" ping.
clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

# -- per-box decision --------------------------------------------------------------------------
# handle_box <box> <ip> <ping_ok> <ws_ok> <bundle_ok> — the probe results are gathered ONCE in main()
# (so they can also feed the dev1-side-outage anchor) and passed in, never re-probed here.
handle_box() {
  local box="$1" ip="$2" ping_ok="$3" ws_ok="$4" bundle_ok="$5" report_only="${6:-0}"
  local verdict
  verdict="$(net_reach_classify_box "$ping_ok" "$ws_ok" "$bundle_ok")"
  log "$box ($ip): ping=$ping_ok ws:$OBS_WS_PORT=$ws_ok bundle:$BUNDLE_PORT=$bundle_ok report_only=$report_only -> $verdict"

  if [ "$verdict" = "REACHABLE" ]; then
    # A report-only box never paged, so it has no recovery ping to fire -- log + clear only.
    if [ "$report_only" != "1" ]; then
      local was_alerted recover
      was_alerted="$(read_state_field "alerted_${box}" 0)"
      recover="$(net_reach_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
      if [ "$recover" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: $box reachable again"
        else
          log "RECOVERY: $box reachable again -- machine-channel only (#1206: recovery is not a phone ping)"
        fi
        write_state_field "alerted_${box}" 0
      fi
    else
      # A report-only box never latches alerted_<box>, but clear it defensively so a
      # required->report-only flip while the box was paged can never leak a stale recovery latch.
      write_state_field "alerted_${box}" 0
      log "[report-only] $box reachable (traveling box #811 -- no recovery ping)"
    fi
    clear_box_throttle "$box"
    return 0
  fi

  # UNREACHABLE -> confirm across consecutive passes before paging.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${box}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${box}" "${confirm:-0}"
  log "$box confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$box UNREACHABLE this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED outage. A REPORT-ONLY box (a traveling box, #811) is tracked + logged but NEVER pages
  # and never latches the recovery flag -- its absence is the normal state, so a page would be pure
  # noise. Flip it to a paging node by removing its name from NETWORK_REACH_REPORT_ONLY_BOXES.
  if [ "$report_only" = "1" ]; then
    # Report-only detail names ONLY the signals actually probed (ping + :4455); :8899 is deliberately
    # skipped for a report-only box (no bundle server), so net_reach_alert_detail's bundle field
    # would misleadingly imply a dead server that was never checked.
    local p_s="DOWN" w_s="DOWN"
    [ "$ping_ok" = "1" ] && p_s="up"
    [ "$ws_ok" = "1" ] && w_s="up"
    log "[report-only] $box CONFIRMED unreachable (ping $p_s, OBS-WS:$OBS_WS_PORT $w_s; :$BUNDLE_PORT not probed for a report-only box) -- traveling box (#811), NOT paging. Verify manually via ops SKILL resolume-snv."
    return 0
  fi

  # CONFIRMED outage -> latch the recovery flag, then throttle-dedup on the box signature.
  write_state_field "alerted_${box}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail
  current_sig="netreach:${box}"
  prior_sig="$(read_state_field "alert_sig_${box}" "")"
  prior_passes="$(read_state_field "alert_passes_${box}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${box}" "$new_sig"
  write_state_field "alert_passes_${box}" "$new_passes"

  detail="$(net_reach_alert_detail "$box" "$ping_ok" "$ws_ok" "$bundle_ok")"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $box CONFIRMED unreachable ($detail) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $box unreachable"
    python3 "$NOTIFY" notify --body \
      "🚨 nedostupný box ($REPO_SLUG): **$box** ($ip) je NEDOSTUPNÝ z dev1. ${detail}. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol. Pravdepodobne mŕtvy NIC / vypnutý box / odpojený kábel — OBS-WS aj ssh aj MCP sú tmavé. Potrebný fyzický zásah — skontroluj box fyzicky (napájanie, sieťový kábel)." \
      --dedup-key "network-reach-$box" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, boxes='$BOXES')"

  # -- gather each box's multi-signal probe ONCE (feeds both the anchor and the per-box decision) --
  local pair box ip names=() ips=() pings=() wss=() bundles=() ronly=()
  local reach_flags=()
  for pair in $BOXES; do
    box="${pair%%|*}"; ip="${pair##*|}"
    names+=("$box"); ips+=("$ip")
    local ro
    ro="$(net_reach_box_is_report_only "$box" "$REPORT_ONLY_BOXES" | sed -n 's/^report_only=//p')"
    ronly+=("$ro")
    local p w b
    p="$(probe_ping "$ip")"; w="$(probe_tcp "$ip" "$OBS_WS_PORT")"
    # A report-only box (resolume) has no bundle-state :8899 server -- skip that probe (it would only
    # ever read a meaningless "down"); classify it on ping OR :4455 only (#811).
    if [ "$ro" = "1" ]; then b=0; else b="$(probe_tcp "$ip" "$BUNDLE_PORT")"; fi
    pings+=("$p"); wss+=("$w"); bundles+=("$b")
    reach_flags+=("$([ "$(net_reach_classify_box "$p" "$w" "$b")" = REACHABLE ] && echo 1 || echo 0)")
  done

  # -- dev1-side-outage guard --------------------------------------------------------------------
  # dev1's path to the rig subnet is PROVEN up if ANY reference rig node answers ping OR any watched
  # box is itself reachable (a reachable box is direct proof of connectivity). Only when NOTHING is
  # reachable — no reference node AND neither box — is the pass "nothing to decide" (per-box state
  # left untouched), so a dev1-side uplink flap never false-pages both boxes. An EMPTY reference set
  # disables the guard (the box-reachability proof still applies), never silently muting the whole
  # watchdog.
  local ref anchor_flags=() anchor
  for ref in $REFERENCE_HOSTS; do
    anchor_flags+=("$(probe_ping "$ref")")
  done
  anchor_flags+=("${reach_flags[@]}")
  anchor="$(net_reach_any_reachable "${anchor_flags[@]}")"
  if [ "$anchor" != "1" ]; then
    log "no reference rig node AND no watched box reachable -- dev1-side path to the rig subnet is down -- nothing to decide this pass"
    log "pass end"
    return 0
  fi

  local i
  for i in "${!names[@]}"; do
    handle_box "${names[$i]}" "${ips[$i]}" "${pings[$i]}" "${wss[$i]}" "${bundles[$i]}" "${ronly[$i]}"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
