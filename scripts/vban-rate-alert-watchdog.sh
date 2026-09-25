#!/usr/bin/env bash
# scripts/vban-rate-alert-watchdog.sh -- see the extended header below.
set -euo pipefail
# A watchdog must SURVIVE every per-pass failure and keep polling on the next timer tick, so it runs
# with `-e` OFF -- the sibling convention (dantesync-clock / genlock-lock / audio-lag alert watchdogs
# all use `set -uo pipefail`). The `set -euo pipefail` above satisfies the new-.sh
# script-failure-policy check; this turns -e back off so one stream's hiccup never aborts the pass.
set +e
set -uo pipefail
#
# scripts/vban-rate-alert-watchdog.sh -- issue 1372 part C: is the AUDIO travelling between PCs in
# the Dante tick? A dev1-side watchdog that measures every VBAN stream ARRIVING at the disciplined
# receiver (strih-lx) and pages when one runs off its nominal rate or loses packets.
#
# WHY: owner (verbatim, 25.9.2026): "nie len obraz je genlocknuty ale z eaj zvuk ktory cestuje medzi
# pocitacmi ci uz ide cez dante alebo vban ide s maximalnym genlockom". Nothing measured it: the FOH
# clicks were found by hand with pktmon, and the first reading was WRONG because it counted packets
# instead of reading the frame counter (-132.9 ppm by count; +18.1 ppm by counter; #1367 comment
# 5832526338). A VBAN sender that runs off the Dante tick slips one packet per few minutes in one
# direction; a sender that drops packets clicks -- both invisible until someone listens.
#
# HOW (one pass): a ~60 s `tcpdump` on strih-lx (Linux: adjtimex slews CLOCK_REALTIME, so its capture
# timestamps are on the dantesync-disciplined clock) filtered to the VBAN magic (`udp[8:4] =
# 0x5642414e`, snaplen >= 96 so the 28-byte header survives), sudo fed on stdin exactly like the
# other strih-lx tools; the pcap streams back over ssh to dev1 and the PURE scripts/vban_rate.py
# grades every stream arriving at strih-lx by FRAME COUNTER (rate = least-squares slope of nuFrame x
# samples-per-frame vs capture time; loss = counter holes) against nominal +-VBAN_RATE_PPM_BOUND and
# the VBAN_RATE_LOSS_CEILING. The hub's own OUTGOING streams (strih-lx -> camN) are left out: they
# say nothing about a peer's clock.
#
# REPORT-ONLY, SHIPS DISABLED: it gates nothing (no E2E step reads it). Its FAULT is on-air audio
# (a slipping or clicking FOH/program feed), so it is in the production-critical class: a confirmed
# fault re-pings via the ONE shared watchdog_notify_key time bucket while it persists
# (.claude/rules/watchdog-notify-dedup.md); recovery is ONE machine-channel log line. The ppm bound
# and loss ceiling are PROVISIONAL: calibrate them from data once part A (the Windows OBS media clock
# on the dantesync rate) is live -- a Dante-clocked sender still differs from the system-time rate by
# dantesync's f_phase (the Dante-GM-vs-UTC term), so the bound must leave room for that.
#
# SKIP (no page): the capture failed while strih-lx is DOWN (the #1001 network-reach watchdog's
# territory), no VBAN stream arrived, or a stream is too short to grade. A capture that keeps failing
# while the box IS up (tcpdump missing, a wrong sudo password) would leave this watchdog blind, so
# after VBAN_RATE_CAPTURE_FAIL_PASSES consecutive failures on a live box (ssh :22 answers) it pages
# once, with the remote error in the text. CAPTURE_TRUNCATED (snaplen cut every header) is logged
# loudly as a configuration error, never a page. A stream not GRADED for longer than
# VBAN_RATE_STALE_S restarts its confirm count when it is graded again (an old single-pass fault never
# completes a page). The blind-capture page is a chronic config fault: ONE page on a stable key.
#
# Usage:
#   scripts/vban-rate-alert-watchdog.sh            # one pass: capture -> grade -> alert
#   scripts/vban-rate-alert-watchdog.sh --dry-run  # capture + grade + LOG only; never alert
#   scripts/vban-rate-alert-watchdog.sh --help
# Seams (Tier-0): VBAN_RATE_CAPTURE_CMD, when set, is invoked as `<cmd> <outfile>` and replaces the
#   ssh capture (a fixture pcap, no box); VBAN_RATE_BOX_UP_CMD prints 1/0 in place of the ssh :22
#   box-up probe.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/watchdog-tcp-probe.sh
. "$HERE/lib/watchdog-tcp-probe.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '11,58p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "vban-rate-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
BOX="${VBAN_RATE_BOX:-strih-lx}"                          # the disciplined receiver (obs-fleet name)
HOST="${VBAN_RATE_HOST:-$(obs_fleet_host "$BOX" 2>/dev/null || true)}"
SSH_USER="${VBAN_RATE_SSH_USER:-${STRIH_USER:-newlevel}}"
SSH_PASS="${VBAN_RATE_SSH_PASS:-${STRIH_PW:-newlevel}}"
CAPTURE_S="${VBAN_RATE_CAPTURE_S:-60}"
SNAPLEN="${VBAN_RATE_SNAPLEN:-96}"
[ "$SNAPLEN" -ge 96 ] 2>/dev/null || SNAPLEN=96            # never below the header-survival floor
BPF="${VBAN_RATE_BPF:-udp and udp[8:4] = 0x5642414e}"
PPM_BOUND="${VBAN_RATE_PPM_BOUND:-20}"                   # provisional (see the header)
LOSS_CEILING="${VBAN_RATE_LOSS_CEILING:-1e-4}"           # provisional (see the header)
MIN_SPAN_S="${VBAN_RATE_MIN_SPAN_S:-20}"
CONFIRM_THRESHOLD="${VBAN_RATE_CONFIRM_THRESHOLD:-2}"
CAPTURE_FAIL_PASSES="${VBAN_RATE_CAPTURE_FAIL_PASSES:-3}"  # consecutive failures on a live box -> page
STALE_S="${VBAN_RATE_STALE_S:-900}"                         # 3 timer periods: an unseen stream's state is stale
REPING_INTERVAL_S="${VBAN_RATE_REPING_INTERVAL_S:-600}"
DECIDE="${VBAN_RATE_DECIDE:-$HERE/vban_rate.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${VBAN_RATE_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${VBAN_RATE_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-vban-rate-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-vban-rate-alert-dryrun.state"
STATE_FILE="${VBAN_RATE_ALERT_STATE_FILE:-$_state_default}"

log() { printf '%s [vban-rate-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- persisted per-key state (key=value lines) -- the dantesync-clock sibling's shape --------------
read_state_field() {
  local key="$1" default="$2" v
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  [ -n "$tmp" ] || tmp="$STATE_FILE"
  { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } >"$tmp" 2>/dev/null || true
  [ "$tmp" = "$STATE_FILE" ] || mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

# state_key <stream key> -> a key safe for the key=value state file and the dedup key.
state_key() { printf '%s' "$1" | tr -c 'A-Za-z0-9_-' '_'; }

now_epoch() { printf '%s' "${VBAN_RATE_NOW:-$(date +%s)}"; }

# capture <outfile> <errfile> -> rc 0 and a pcap in OUTFILE (maybe empty of VBAN), else rc != 0.
# The remote sudo/tcpdump stderr lands in ERRFILE (the reason a failing capture is logged + paged).
capture() {
  local out="$1" err="$2" rc
  if [ -n "${VBAN_RATE_CAPTURE_CMD:-}" ]; then
    "$VBAN_RATE_CAPTURE_CMD" "$out" 2>"$err"
    return
  fi
  [ -n "$HOST" ] || { echo "no address for box '$BOX' (obs_fleet_host failed)" >"$err"; return 1; }
  printf '%s\n' "$SSH_PASS" | sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 "${SSH_USER}@${HOST}" \
    "sudo -S -p '' timeout ${CAPTURE_S} tcpdump -i any -s ${SNAPLEN} -U -w - '${BPF}'" >"$out" 2>"$err"
  rc=$?
  # tcpdump stopped by `timeout` exits 124: a complete capture, not an error.
  [ "$rc" -eq 0 ] || [ "$rc" -eq 124 ] || return "$rc"
  [ -s "$out" ]
}

# fire_alert <base key> <body> -- the ON-AIR stream alert seam (time-bucketed: production-critical class).
# The key falls back to the stable base if the shared helper ever fails -- never an empty key.
fire_alert() {
  local base="$1" body="$2" key
  key="$(watchdog_notify_key "$base" "$(now_epoch)" "$REPING_INTERVAL_S" 2>/dev/null || printf '%s' "$base")"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert (dedup-key=$key): $body"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$body" --dedup-key "$(watchdog_notify_key "$base" "$(now_epoch)" "$REPING_INTERVAL_S" 2>/dev/null || printf '%s' "$base")" \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# box_up -> 1 when the receiver answers ssh :22 (a failing capture is then OUR problem), else 0.
box_up() {
  if [ -n "${VBAN_RATE_BOX_UP_CMD:-}" ]; then
    "$VBAN_RATE_BOX_UP_CMD" 2>/dev/null || printf '0'
    return 0
  fi
  [ -n "$HOST" ] || { printf '0'; return 0; }
  watchdog_probe_tcp "$HOST" 22 4
}

# capture_failed <reason> -- count consecutive failures on a LIVE box; page once confirmed.
capture_failed() {
  local reason="$1" n
  if [ "$(box_up)" != "1" ]; then
    log "capture on $BOX failed or empty -- SKIP: box down (ssh :22 silent), the network-reach watchdog's territory; no page"
    write_state_field "capture_fail" 0
    return 0
  fi
  n=$(( $(read_state_field "capture_fail" 0) + 1 ))
  write_state_field "capture_fail" "$n"
  log "capture on $BOX failed or empty -- SKIP ($n/$CAPTURE_FAIL_PASSES consecutive while the box is up): ${reason:-no error text}"
  [ "$n" -ge "$CAPTURE_FAIL_PASSES" ] || return 0
  write_state_field "alerted_capture" 1
  # A blind watchdog is a CHRONIC config fault (tcpdump missing, a wrong sudo password), not an on-air
  # fault: ONE page per incident on a STABLE key -- airuleset edits the card on every repeat -- never
  # the production-critical time bucket (review round 2; .claude/rules/watchdog-notify-dedup.md).
  local body="⚠️ VBAN ($REPO_SLUG): meranie VBAN na **$BOX** zlyháva už $n kontrol po sebe, hoci box je hore -- watchdog je slepý. Chyba: ${reason:-bez textu}. Náprava: tcpdump + sudo na $BOX (heslo / balík)."
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert (dedup-key=vban-rate-capture-${BOX}): $body"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$body" --dedup-key "vban-rate-capture-${BOX}" \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# handle_stream <key> <name> <src> <verdict> <rate> <lost> <loss_ratio> <why>
handle_stream() {
  local key="$1" name="$2" src="$3" verdict="$4" rate="$5" lost="$6" ratio="$7" why="$8"
  local sk prev decision confirm act fault=0
  # keyed on stream NAME + sender IP -- never the sender's ephemeral source port (a sender restart
  # would otherwise open a new incident and reset its confirm count); KEY stays in the log only.
  sk="$(state_key "${name}-${src}")"
  log "$name ($src -> $BOX) [$key]: verdict=$verdict rate_ppm=$rate lost=$lost loss_ratio=$ratio${why:+ why=$why}"
  case "$verdict" in
    FAULT) fault=1 ;;
    OK) fault=0 ;;
    *) log "$name: $verdict -- not graded this pass, holding (no page, no recovery)"; return 0 ;;
  esac
  # a stream not GRADED for longer than STALE_S restarts its confirm count (never completes an old
  # page); only an OK/FAULT pass refreshes last_seen -- a SHORT/UNCERTAIN pass is not a reading
  local now last
  now="$(now_epoch)"
  last="$(read_state_field "last_seen_${sk}" "")"
  if [ -n "$last" ] && [ $((now - last)) -gt "$STALE_S" ]; then
    log "$name: last graded $((now - last))s ago (> ${STALE_S}s) -- its confirm count restarts"
    write_state_field "confirm_${sk}" 0
  fi
  write_state_field "last_seen_${sk}" "$now"
  prev="$(read_state_field "confirm_${sk}" 0)"
  decision="$(obs_watchdog_confirm "$prev" "$fault" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${sk}" "${confirm:-0}"
  if [ "$fault" != "1" ]; then
    if [ "$(read_state_field "alerted_${sk}" 0)" = "1" ]; then
      log "RECOVERY: $name back inside the bound -- machine-channel only (#1206: recovery is not a phone ping)"
      write_state_field "alerted_${sk}" 0
    fi
    return 0
  fi
  if [ "${act:-0}" != "1" ]; then
    log "$name fault this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi
  write_state_field "alerted_${sk}" 1
  fire_alert "vban-rate-${sk}" \
    "⚠️ VBAN ($REPO_SLUG): stream **$name** ($src -> $BOX) je mimo normy -- rýchlosť ${rate} ppm oproti nominálu (hranica ±${PPM_BOUND} ppm), strata ${lost} rámcov (${ratio}; strop ${LOSS_CEILING}). Dôvod: ${why}. Meranie počítadlom rámcov na strih-lx (disciplinované hodiny), REPORT-ONLY. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol; re-ping každých ~$((REPING_INTERVAL_S/60)) min kým to trvá."
}

require_tools() {
  local missing=() t
  command -v python3 >/dev/null 2>&1 || missing+=(python3)
  if [ -z "${VBAN_RATE_CAPTURE_CMD:-}" ]; then
    for t in sshpass ssh; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
  fi
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (every pass would silently SKIP)"
    return 1
  fi
  [ -r "$DECIDE" ] || { log "FATAL: decision module not readable: $DECIDE"; return 1; }
  return 0
}

main() {
  log "pass start (dry_run=$DRY_RUN, box=$BOX host=${HOST:-?}, capture=${CAPTURE_S}s snaplen=$SNAPLEN, ppm_bound=$PPM_BOUND loss_ceiling=$LOSS_CEILING, confirm=$CONFIRM_THRESHOLD)"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }
  local work pcap errf out overall reason
  work="$(mktemp -d "${TMPDIR:-/tmp}/vban-rate.XXXXXX")" || { log "cannot create a work dir"; return 1; }
  pcap="$work/capture.pcap"
  errf="$work/capture.err"
  if ! capture "$pcap" "$errf"; then
    reason="$(grep -v -e '^listening on' -e 'packets captured' -e 'packets received by filter' \
      -e 'packets dropped by kernel' "$errf" 2>/dev/null | tail -n 3 | tr '\n' ' ' || true)"
    capture_failed "$reason"
    rm -rf "$work"
    log "pass end"
    return 0
  fi
  if [ "$(read_state_field "alerted_capture" 0)" = "1" ]; then
    log "RECOVERY: the capture on $BOX works again -- machine-channel only (#1206: recovery is not a phone ping)"
    write_state_field "alerted_capture" 0
  fi
  write_state_field "capture_fail" 0
  local -a dst=()
  [ -n "$HOST" ] && [ -z "${VBAN_RATE_ALL_STREAMS:-}" ] && {
    local ip; ip="$(obs_fleet_resolve_host_v4 "$HOST")"; [ -n "$ip" ] && dst=(--dst "$ip")
  }
  out="$(python3 "$DECIDE" analyze "$pcap" "${dst[@]}" --ppm-bound "$PPM_BOUND" \
    --loss-ceiling "$LOSS_CEILING" --min-span-s "$MIN_SPAN_S" --tsv 2>&1)" || {
    log "vban_rate.py could not read the capture: $out -- SKIP"
    rm -rf "$work"; log "pass end"; return 0
  }
  rm -rf "$work"
  overall="$(printf '%s\n' "$out" | awk -F'\t' '$1=="overall"{print $2}' | head -n 1 || true)"
  case "$overall" in
    CAPTURE_TRUNCATED) log "CONFIG ERROR: every VBAN header was cut by the snaplen ($SNAPLEN) -- nothing graded (no page)" ;;
    NO_STREAMS) log "no VBAN stream arrived at $BOX in ${CAPTURE_S}s -- nothing to grade (no page)" ;;
  esac
  while IFS=$'\t' read -r kind key name src verdict rate lost ratio why; do
    [ "$kind" = "stream" ] || continue
    [ "$why" = "-" ] && why=""
    handle_stream "$key" "$name" "$src" "$verdict" "$rate" "$lost" "$ratio" "$why"
  done <<<"$out"
  log "pass end (overall=${overall:-?})"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
