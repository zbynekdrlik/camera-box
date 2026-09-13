#!/usr/bin/env bash
# scripts/dantesync-clock-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so it
# runs with `-e` OFF -- the sibling convention (genlock-lock / audio-lag / bundle-state /
# network-reach alert watchdogs all use `set -uo pipefail`, NOT -e). The `set -euo pipefail` above
# satisfies the new-.sh script-failure-policy check; this turns -e back off so one node's
# fetch/decision hiccup never aborts the whole pass.
set +e
set -uo pipefail
#
# scripts/dantesync-clock-alert-watchdog.sh -- #1307: dev1-side ALERT watchdog for the fleet losing
# the DANTE (PTP) CLOCK.
#
# WHY (#1307 / #1297, owner ruling ROZHODNUTÉ 2026-09-13): the camboxes cam1-7 are HEADLESS (no
# desktop, no operator) -- their only path to a human is dev1 -> Discord. On 2026-09-13 the Yamaha
# AIC128-D PTP grandmaster's DHCP lease moved off 10.77.9.184, every node's gm_allowlist stopped
# matching, the WHOLE fleet silently ran NTP-only for hours (mode=ACQ is_locked=false; strih as NTP
# master stepping 165x/h) and NOBODY noticed until the release E2E gate failed. The owner's rule
# (verbatim): "nech kazdu minutu chodia notifikacie ze nemaju dante clock ... byt o tom dokolecka
# notifikovany". This watchdog closes that gap: every 60s it reads EVERY dantesync node's
# http://<ip>:8898/status and pages (repeatedly, while it persists) when a node is NOT PTP-locked to
# the rig grandmaster, or the grandmaster DNS name stops resolving, or the resolved grandmaster IP
# MOVES between passes.
#
# PRODUCTION-CRITICAL RE-PING (owner ruling): unlike the one-ping-per-incident sibling watchdogs,
# this class must re-ping while the fault PERSISTS. Mechanism (no new notify channel): a TIME-
# BUCKETED airuleset --dedup-key (dante-clock-<box>-<floor(now/REPING_INTERVAL_S)>, 600s default,
# floor 60). Within a bucket an identical state EDITS the card (no ping); every new bucket is a fresh
# ping. Recovery is ONE machine-channel log line, never a phone ping. The bucket + the verdict are
# computed by the PURE scripts/dantesync_clock_decision.py (the #1199 python-mirror pattern) so the
# cadence + grading are exhaustively unit-tested under Tier-0 (#557 kills local cargo).
#
# DETECTION ONLY (alert-only) -- there is deliberately NO auto-action. The cure for a lost clock is a
# rig-ops decision (fix the grandmaster DNS/DHCP, restart dantesync, re-provision an allowlist), not
# something a dev1 timer should drive blind.
#
# GRADING REUSE: the per-node verdict mirrors scripts/clock-offset-guard.sh's field semantics
# (ptp_locked_from_pipe_json: is_locked + mode in NANO/LOCK; gm_matches_expected: gm_source_ip vs the
# rig grandmaster; ntp_master_step_storm_verdict: the ntp_step_storm boolean) -- the SAME :8898/status
# fields the E2E gate (scripts/dantesync-gate.sh) grades. The grandmaster address is resolved from the
# DNS name video-clock.lan via scripts/lib/rig-grandmaster.sh (#1307), the ONE source of truth. The
# #1119 storm signal is dantesync's OWN ntp_step_storm boolean (its 120/h alarm) -- there is no
# camera-box numeric storm literal to re-hardcode; ntp_steps_last_hour is carried in the reason only.
#
# NODE ROSTER: cam1-7 (all powered + running dantesync today, incl. cam5-7 retired from
# CAMERA_ACTIVE_SET) resolved via scripts/camera-set.sh's camera_resolve (single IP source of truth)
# + strih/stream/imag/resolume resolved via scripts/lib/obs-fleet.sh's obs_fleet_host. resolume is a
# TRAVELING CG box: paged only while obs_fleet_is_home resolume holds (the #1296 condition); away ->
# skipped (never a false page against a box that is simply not here). An OFF cam reads UNREACHABLE ->
# SKIP (deferred to the #1001 network-reach watchdog), never a page.
#
# Usage:
#   scripts/dantesync-clock-alert-watchdog.sh            # one pass: fetch -> decide -> alert
#   scripts/dantesync-clock-alert-watchdog.sh --dry-run  # fetch + decide + LOG only; never alert
#   scripts/dantesync-clock-alert-watchdog.sh --help

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/rig-grandmaster.sh
. "$HERE/lib/rig-grandmaster.sh"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '10,55p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "dantesync-clock-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The cam nodes to watch (space-separated NAMES). Default = the WHOLE powered dantesync cam fleet
# cam1-7 -- NOT CAMERA_ACTIVE_SET, which excludes cam5-7 (retired grabbers) that are still powered
# and still running dantesync (so they can silently lose the clock too). Each name resolves to its IP
# via camera-set.sh's camera_resolve (single source of truth, no second IP literal).
DANTE_CLOCK_CAM_NODES="${DANTE_CLOCK_CAM_NODES:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}"
# The OBS-box dantesync nodes (space-separated NAMES); IPs + the resolume home-gate come from
# obs-fleet.sh. imag(-nb) IS a dantesync node; resolume is the traveling CG box.
DANTE_CLOCK_OBS_NODES="${DANTE_CLOCK_OBS_NODES:-strih stream imag resolume}"

STATUS_PORT="${DANTE_CLOCK_STATUS_PORT:-8898}"          # dantesync#47 network status endpoint
STATUS_PATH="${DANTE_CLOCK_STATUS_PATH:-/status}"
CURL_TIMEOUT="${DANTE_CLOCK_CURL_TIMEOUT:-10}"          # :8898 HTTP fetch (s)

# updated_ts freshness: a reachable-but-STALE payload (HTTP alive, servo/updated_ts frozen) is a
# silent clock loss the E2E gate already fails on -- page it. Default 300s (mirrors the gate's
# DANTESYNC_OFFSET_FRESHNESS_S), generous over the ~30s updated_ts cadence.
FRESHNESS_S="${DANTE_CLOCK_FRESHNESS_S:-300}"

# 2-pass confirm before the FIRST page (matches the siblings): a single blipped reading (a daemon
# reload, a one-tick relock) must never fire. A genuine loss persists across the 60s cadence.
CONFIRM_THRESHOLD="${DANTE_CLOCK_CONFIRM_THRESHOLD:-2}"

# Production-critical re-ping bucket (owner ruling): while the fault persists, re-ping every
# REPING_INTERVAL_S. The pure module floors this at 60s.
REPING_INTERVAL_S="${DANTE_CLOCK_REPING_INTERVAL_S:-600}"

DECIDE="${DANTE_CLOCK_DECIDE:-$HERE/dantesync_clock_decision.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${DANTE_CLOCK_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${DANTE_CLOCK_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-dantesync-clock-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-dantesync-clock-alert-dryrun.state"
STATE_FILE="${DANTE_CLOCK_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [dantesync-clock-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- roster: NAME|IP|HOMEGATE triples ----------------------------------------------------------
# HOMEGATE ∈ always (cams: probe unconditionally; an OFF box -> UNREACHABLE -> SKIP) |
# obsfleet (obs_fleet_is_home decides -- always-true for strih/stream/imag, the traveling gate for
# resolume). DANTE_CLOCK_NODES (space-separated NAME|IP[|HOMEGATE]) overrides the whole roster.
build_roster() {
  if [ -n "${DANTE_CLOCK_NODES:-}" ]; then
    printf '%s\n' $DANTE_CLOCK_NODES
    return 0
  fi
  local n host
  for n in $DANTE_CLOCK_CAM_NODES; do
    if camera_resolve "$n" >/dev/null 2>&1; then
      printf '%s|%s|always\n' "$n" "$CAMERA_IP"
    else
      log "roster: camera_resolve failed for '$n' -- skipping (check camera-set.sh)"
    fi
  done
  for n in $DANTE_CLOCK_OBS_NODES; do
    host="$(obs_fleet_host "$n" 2>/dev/null || true)"
    if [ -n "$host" ]; then
      printf '%s|%s|obsfleet\n' "$n" "$host"
    else
      log "roster: obs_fleet_host failed for '$n' -- skipping (check obs-fleet.sh)"
    fi
  done
}

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_status_json <ip> -> prints the :8898/status JSON body to stdout, returns 0 iff a 200 with a
# body starting `{` came back. A curl failure or a wedged non-JSON answer returns 1 (box_reachable=0
# -> SKIP; deferred to #1001). DANTE_CLOCK_FETCH_CMD (Tier-0 seam, mirrors genlock-lock's
# GENLOCK_LOCK_FETCH_CMD): when set, it is invoked as `<cmd> <ip>` and its stdout REPLACES curl -- so
# a --dry-run against captured fixtures needs no live box.
fetch_status_json() {
  local ip="$1" body
  if [ -n "${DANTE_CLOCK_FETCH_CMD:-}" ]; then
    body="$("$DANTE_CLOCK_FETCH_CMD" "$ip" 2>/dev/null)" || return 1
  else
    body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${STATUS_PORT}${STATUS_PATH}" 2>/dev/null)" \
      || return 1
  fi
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace
  case "$body" in
    \{*) printf '%s' "$body"; return 0 ;;
    *) return 1 ;;
  esac
}

# -- persisted per-key state (key=value lines) -- verbatim shape from the genlock-lock sibling -----
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

# now_epoch -> seconds since the epoch (injectable for tests via DANTE_CLOCK_NOW).
now_epoch() { printf '%s' "${DANTE_CLOCK_NOW:-$(date +%s)}"; }

# bucketed_key <base> -> the time-bucketed --dedup-key from the pure module (deterministic now).
bucketed_key() {
  python3 "$DECIDE" dedup-key --base "$1" --now "$(now_epoch)" --interval "$REPING_INTERVAL_S" 2>/dev/null || printf '%s' "$1"
}

# fire_alert <dedup-base> <emoji-body...> -- the ONE ALERT emit seam. Always computes a time-bucketed
# --dedup-key (owner ruling: re-ping while it persists; airuleset edits the card within a bucket).
fire_alert() {
  local base="$1"; shift
  local key; key="$(bucketed_key "$base")"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert (dedup-key=$key): $*"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$*" --dedup-key "$key" \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# confirm_then_alert <state-key-suffix> <fault 0|1> <dedup-base> <body...>
#   Drives obs_watchdog_confirm (2-pass) on the fault, latches `alerted_<suffix>` for recovery, and
#   -- once confirmed -- calls fire_alert EVERY pass (the time-bucket, not a per-pass throttle,
#   controls the re-ping cadence). A cleared fault logs a machine-channel recovery once.
confirm_then_alert() {
  local suffix="$1" fault="$2" base="$3"; shift 3
  local prev decision confirm act
  prev="$(read_state_field "confirm_${suffix}" 0)"
  decision="$(obs_watchdog_confirm "$prev" "$fault" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${suffix}" "${confirm:-0}"

  if [ "$fault" != "1" ]; then
    # healthy this pass: recovery latch (machine-channel only, #1206), never a phone ping.
    local was_alerted
    was_alerted="$(read_state_field "alerted_${suffix}" 0)"
    if [ "$was_alerted" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: ${suffix} back to healthy"
      else
        log "RECOVERY: ${suffix} clock healthy again -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${suffix}" 0
    fi
    return 0
  fi

  if [ "${act:-0}" != "1" ]; then
    log "${suffix} fault this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "alerted_${suffix}" 1
  fire_alert "$base" "$@"
}

# -- per-node decision --------------------------------------------------------------------------
# handle_node <name> <ip> <homegate> <grandmaster_ip>
handle_node() {
  local name="$1" ip="$2" homegate="$3" gm="$4" body reachable out verdict reason steps

  if [ "$homegate" = "obsfleet" ] && ! obs_fleet_is_home "$name"; then
    log "$name away (obs_fleet_is_home false) -- traveling box, skipping this pass (no fetch, no page)"
    return 0
  fi

  if body="$(fetch_status_json "$ip")"; then reachable=1; else reachable=0; body=""; fi

  out="$(printf '%s' "$body" | python3 "$DECIDE" analyze --box-reachable "$reachable" --grandmaster-ip "$gm" --now "$(now_epoch)" --freshness-s "$FRESHNESS_S" 2>/dev/null)"
  verdict="$(printf '%s\n' "$out" | sed -n 's/^verdict=//p')"
  reason="$(printf '%s\n' "$out" | sed -n 's/^reason=//p')"
  steps="$(printf '%s\n' "$out" | sed -n 's/^ntp_steps_last_hour=//p')"
  log "$name ($ip): reachable=$reachable verdict=${verdict:-<none>} reason=${reason:-}"

  case "$verdict" in
    SKIP)
      log "$name :$STATUS_PORT not fetchable this pass -- box/:$STATUS_PORT-down is #1001 territory; holding, no page"
      return 0
      ;;
    UNKNOWN)
      log "$name reachable but status carried no clock fields (non-dantesync / partial payload) -- no reading, holding, no page"
      return 0
      ;;
    OK)
      confirm_then_alert "node_${name}" 0 "dante-clock-${name}"
      return 0
      ;;
    NO_CLOCK) : ;;
    *)
      log "$name: unexpected verdict '${verdict:-<empty>}' from dantesync_clock_decision.py (analyze failed?) -- holding, no page"
      return 0
      ;;
  esac

  local steps_note=""
  [ -n "$steps" ] && [ "$steps" != "null" ] && steps_note=", ${steps} NTP krokov/h"
  confirm_then_alert "node_${name}" 1 "dante-clock-${name}" \
    "🚨 Dante-clock ($REPO_SLUG): **$name** ($ip) STRATIL dante clock -- dôvod **${reason}**${steps_note}. Uzol nie je PTP-zosynchronizovaný na rig grandmaster (${gm:-<neznámy>}); pri produkcii to potichu rozhodí A/V aj genlock celej fleet. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol; re-ping každých ~$((REPING_INTERVAL_S/60)) min kým to trvá. Náprava: over grandmaster (DNS video-clock.lan / DHCP), dantesync na boxe, gm_allowlist."
}

# handle_grandmaster <resolved_ok 0|1> <grandmaster_ip> -> DNS_UNRESOLVABLE + GM_CHANGED global pages.
handle_grandmaster() {
  local resolved="$1" gm="$2"

  # DNS_UNRESOLVABLE -- rig_grandmaster_ip could not resolve video-clock.lan. This IS the silent-
  # failure class the owner banned, so it gets its own bucketed page.
  if [ "$resolved" != "1" ]; then
    confirm_then_alert "dns" 1 "dante-clock-dns" \
      "🚨 Dante-clock ($REPO_SLUG): grandmaster DNS **video-clock.lan** sa NEROZLÍŠI (rig DNS / MikroTik statický záznam chýba alebo je nedostupný). Bez neho nevie gate ani fleet overiť clock -- presne tichý pád, čo sa nesmie stať. Re-ping každých ~$((REPING_INTERVAL_S/60)) min kým to trvá. Náprava: MikroTik DNS video-clock.lan -> Yamaha Dante karta."
    return 0
  fi
  # resolved OK: clear any DNS fault latch (recovery log-only).
  confirm_then_alert "dns" 0 "dante-clock-dns"

  # GM_CHANGED -- the resolved grandmaster IP MOVED between passes (exactly today's "ip sa zmenila").
  # 2-pass confirm against a candidate; persist the new IP only once confirmed + paged, so a single
  # DNS flap does not re-page and the change normalises next pass.
  local last cnt
  last="$(read_state_field "gm_last_ip" "")"
  if [ -z "$last" ]; then
    write_state_field "gm_last_ip" "$gm"
    return 0
  fi
  if [ "$(python3 "$DECIDE" gm-change --prev "$last" --cur "$gm" 2>/dev/null)" != "1" ]; then
    write_state_field "gm_change_confirm" 0
    return 0
  fi
  cnt="$(read_state_field "gm_change_confirm" 0)"
  local decision act confirm
  decision="$(obs_watchdog_confirm "$cnt" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "gm_change_confirm" "${confirm:-0}"
  if [ "${act:-0}" != "1" ]; then
    log "grandmaster IP candidate change $last -> $gm not yet confirmed across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  fire_alert "dante-clock-gm-change" \
    "🚨 Dante-clock ($REPO_SLUG): PTP grandmaster adresa sa ZMENILA **$last -> $gm** (video-clock.lan teraz ukazuje inam). Presne dnešný prípad -- musíš o tom vedieť aj keď uzly nový GM nasledujú. Over, že je to zámer (nová statická DHCP lease Yamaha karty), inak vráť DNS/DHCP."
  write_state_field "gm_last_ip" "$gm"
  write_state_field "gm_change_confirm" 0
}

# require_tools -> loud exit if a REQUIRED tool / the decision module is missing (a missing curl ->
# every node SKIP; a missing/unreadable $DECIDE -> analyze emits nothing -> every node "holding" ->
# SILENT FOREVER, exactly the "missing dependency must fail LOUD by name" class, #833).
require_tools() {
  local missing=() t
  for t in curl python3 getent; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (would silently SKIP every node and never page a real clock loss)"
    return 1
  fi
  if [ ! -r "$DECIDE" ]; then
    log "FATAL: decision module not readable: $DECIDE -- refusing to run (analyze would emit nothing -> every node 'holding' forever; fix DANTE_CLOCK_DECIDE)"
    return 1
  fi
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, confirm=$CONFIRM_THRESHOLD, reping=${REPING_INTERVAL_S}s, cams='$DANTE_CLOCK_CAM_NODES', obs='$DANTE_CLOCK_OBS_NODES')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  # Resolve the grandmaster ONCE per pass (the DNS name video-clock.lan -> its IPv4, #1307).
  local gm="" resolved=0
  if gm="$(rig_grandmaster_ip 2>/dev/null)" && [ -n "$gm" ]; then
    resolved=1
    log "grandmaster resolved: video-clock.lan -> $gm"
  else
    gm=""
    log "grandmaster UNRESOLVABLE (rig_grandmaster_ip failed) -- DNS_UNRESOLVABLE page; per-node grading continues without the gm check"
  fi
  handle_grandmaster "$resolved" "$gm"

  local triple name ip homegate
  while IFS= read -r triple; do
    [ -n "$triple" ] || continue
    name="${triple%%|*}"
    homegate="${triple##*|}"
    ip="${triple#*|}"; ip="${ip%%|*}"
    handle_node "$name" "$ip" "$homegate" "$gm"
  done < <(build_roster)
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
