#!/usr/bin/env bash
# scripts/cambox-ro-units-apply.sh -- plan / apply the issue-1394 read-only-root unit set on live camboxes (header below).
set -euo pipefail
#
# Live 8.10.2026 (issue 1394): every cambox read `degraded` -- logrotate kept its state on the
# read-only root, the apt timers were only disabled, the issue-1311 netconsole oneshot never
# retried a boot-time miss. setup-device.sh writes the fixes on a new box; THIS brings an
# already-provisioned box up to date without a setup-device re-run, through the ONE remote program
# scripts/lib/cambox-ro-units.sh builds (cambox_ro_units_apply_program):
#   - the logrotate drop-in, the netconsole unit (where issue 1311 put one) and the four apt masks,
#     written inside ONE rw window closed by the verified close (scripts/lib/ro-window.sh): nothing
#     is started before the root reads read-only again, and a root that stays writable fails by
#     name with its writers;
#   - the apt timers/services stopped before the window; after the close: daemon-reload,
#     reset-failed, one logrotate run, the netconsole restart, and the `is-system-running` read-back;
#   - the box's off-box log sink (issue 1394, owner 9.10.2026: its OWN venue strih, never dev1):
#     remote_log_sink_ip resolves it per box from the venue fact files BEFORE the first box is
#     touched, and the same window rewrites the netconsole script, journal-upload.conf and the upload
#     drop-in where issue 1311 put them; systemd-journal-upload restarts after the close.
#
# It is a root write on a cambox, so --apply TAKES the rig lease (scripts/lib/rig-lease.sh, repo
# camera-box-ro-units-apply) for its whole run and releases it on every exit: a live foreign holder
# (an E2E / the soak) refuses the run before any box, and no E2E can take the rig while a box is
# applied. It declares the rolling 15-min look-ahead and refreshes it before every box (never the
# whole run: a full-path E2E then WAITS for the release instead of failing fast on a long hold).
# Before EVERY box it also REFUSES (exit 1, that box and the rest untouched, the boxes
# already done summarised) while:
#   - the issue-281 rig heartbeat (scripts/lib/rig-heartbeat.sh rig_held_reason; recording-e2e /
#     rig-mode TEST, which does not take the lease) is fresh, or a lease that is not ours is live;
#   - strih or stream records or streams (the ONE shared rig-busy guard,
#     scripts/lib/stray-session-check.sh, as bkshading-deploy-relay.sh uses it).
# --force-live skips the lease and the guard, loudly (supervisor-only). --plan runs no guard and
# touches nothing.
# It never reboots anything.
#
# Usage:
#   scripts/cambox-ro-units-apply.sh --plan  --box <name> [--box <name> ...]   print the program, touch nothing
#   scripts/cambox-ro-units-apply.sh --apply --box <name> [--box <name> ...]   run it as root over ssh (bash -s)
#   scripts/cambox-ro-units-apply.sh --plan|--apply --active                   every box in CAMERA_ACTIVE_SET
#   scripts/cambox-ro-units-apply.sh --plan|--apply --venue <venue>            every camera a venue lists
# A box name resolves through scripts/camera-set.sh (camera_resolve); --active reads the fleet from
# CAMERA_ACTIVE_SET (never a typed camera range); --venue reads the cameras a venue's strih fact file
# lists, each dialled at its dev1 address from scripts/lib/dantesync-fleet.sh
# (dantesync_fleet_venue_camera_rows: the Poprad boxes are 10.76.9.x through companion-pp). --apply goes on to the next box after a failure and
# names every failed box at the end, and every box that has no issue-1311 netconsole unit.
#
# Env: CAM_PW (cambox root password, default newlevel -- the fleet's dev password, as verify-device.sh),
#      SSH_TIMEOUT (ssh ConnectTimeout, default 10), STRIH_HOST / STREAM_HOST (rig-busy OBS-WS hosts,
#      default 10.77.9.202 / 10.77.9.204), OBS_PASSWORD, RIG_LEASE_DIR / RIG_LEASE_STALE_SECS /
#      RIG_LEASE_LOOKAHEAD_SECS / CAMERA_BOX_RIG_HEARTBEAT (the lease + heartbeat libs' own knobs),
#      CAMBOX_RO_UNITS_OBS_PHASE2_DIR (dir holding obs_phase2.py for the rig-busy guard; tests).
# Exit: 0 every box OK; 1 a box failed or the rig guard refused (named on stderr); 2 a bad
# invocation (nothing touched).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# shellcheck source=scripts/lib/remote-logging.sh
. "$HERE/lib/remote-logging.sh"  # remote_log_sink_ip: the box's venue strih log sink (issue 1394)
# shellcheck source=scripts/lib/cambox-ro-units.sh
. "$HERE/lib/cambox-ro-units.sh"
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$HERE/lib/rig-heartbeat.sh"     # rig_held_reason: the issue-281 heartbeat + the rig lease (rig-lease.sh)
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh"  # stray_session_check_assert -- the ONE shared rig-busy guard

CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}"
RIG_BUSY_HERE="${CAMBOX_RO_UNITS_OBS_PHASE2_DIR:-$HERE}"

usage() {
  cat <<'EOF'
Usage: scripts/cambox-ro-units-apply.sh --plan|--apply [--force-live] (--box <name> [--box <name> ...] | --active | --venue <venue>)
  --plan        print, per box, the remote program --apply would run; touches nothing
  --apply       run it as root on each box over ssh (bash -s); refused while the rig lease is held,
                the rig heartbeat is fresh, or strih/stream broadcast
  --force-live  skip that rig guard (supervisor-only, logged)
  --box         a camera name from scripts/camera-set.sh (repeatable)
  --active      every box in CAMERA_ACTIVE_SET
  --venue       every camera a venue's strih fact file lists, at its dev1 address (e.g. --venue pp)
Env: CAM_PW (default newlevel), SSH_TIMEOUT (default 10). Exit 0 ok, 1 a box failed or the rig
guard refused, 2 usage.
EOF
}

MODE=""
FORCE_LIVE=0
BOXES=()
VENUES=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan | --apply)
      if [ -n "$MODE" ]; then
        echo "ERROR: give --plan or --apply, not both" >&2
        usage >&2
        exit 2
      fi
      MODE="${1#--}"
      ;;
    --force-live) FORCE_LIVE=1 ;;
    --box)
      if [ "$#" -lt 2 ]; then
        echo "ERROR: --box needs a camera name" >&2
        usage >&2
        exit 2
      fi
      BOXES+=("$2")
      shift
      ;;
    --active)
      read -r -a _active <<<"$CAMERA_ACTIVE_SET"
      BOXES+=("${_active[@]}")
      ;;
    --venue)
      if [ "$#" -lt 2 ]; then
        echo "ERROR: --venue needs a venue name" >&2
        usage >&2
        exit 2
      fi
      VENUES+=("$2")
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done
if [ -z "$MODE" ] || { [ "${#BOXES[@]}" -eq 0 ] && [ "${#VENUES[@]}" -eq 0 ]; }; then
  usage >&2
  exit 2
fi

# Resolve every name, its dial address and its log sink BEFORE the first box is touched: a typo, an
# unknown venue or a box no venue lists stops the run with nothing changed. A box named twice (cam1
# and CAM1, or by --box and --venue) is applied once, at the first address it was named with.
NAMES=()
IPS=()
SINKS=()
add_box() {  # add_box NAME DIAL_ADDRESS -> 0 added / 1 named before; exit 2 when no sink resolves
  local sink
  case " ${NAMES[*]:-} " in
    *" $1 "*)
      echo "NOTE: $1 is named more than once -- applied once" >&2
      return 1
      ;;
  esac
  sink="$(remote_log_sink_ip "$1")" || {
    echo "ERROR: no log sink for $1 (the line above) -- nothing was touched" >&2
    exit 2
  }
  NAMES+=("$1")
  IPS+=("$2")
  SINKS+=("$sink")
}
for _box in "${BOXES[@]}"; do
  _name="$(printf '%s' "$_box" | tr '[:upper:]' '[:lower:]')"
  camera_resolve "$_name" || exit 2
  add_box "$CAMERA_NAME" "$CAMERA_IP" || continue
  camera_is_active "$CAMERA_NAME" \
    || echo "NOTE: $CAMERA_NAME is not in CAMERA_ACTIVE_SET ('$CAMERA_ACTIVE_SET') -- going on, as asked" >&2
done
if [ "${#VENUES[@]}" -gt 0 ]; then
  # shellcheck source=scripts/lib/dantesync-fleet.sh
  . "$HERE/lib/dantesync-fleet.sh"
  for _venue in "${VENUES[@]}"; do
    _rows="$(dantesync_fleet_venue_camera_rows "$_venue")" || {
      echo "ERROR: venue '$_venue': the strih fact files do not resolve cleanly (the line above names it) -- nothing was touched" >&2
      exit 2
    }
    [ -n "$_rows" ] || {
      echo "ERROR: venue '$_venue' lists no camera (scripts/strih-boxes/*.env STRIH_VENUE + STRIH_CAMERAS) -- nothing was touched" >&2
      exit 2
    }
    while IFS='|' read -r _name _addr _; do
      [ -n "$_name" ] || continue
      add_box "$_name" "$_addr" || true
    done <<<"$_rows"
  done
fi

if [ "$MODE" = plan ]; then
  for _i in "${!NAMES[@]}"; do
    printf '== %s (%s): the program --apply runs as root over ssh (bash -s); log sink %s; nothing was touched ==\n' \
      "${NAMES[$_i]}" "${IPS[$_i]}" "${SINKS[$_i]}"
    cambox_ro_units_apply_program "${SINKS[$_i]}"
  done
  exit 0
fi

command -v sshpass >/dev/null 2>&1 || {
  echo "ERROR: sshpass is required for --apply" >&2
  exit 2
}

# The rig lease this run holds (review round 3): taken before the first box and released on every
# exit, so no E2E / soak can take the rig while a box is applied (one box can take minutes: the
# netconsole arm waits for the log sink). A foreign live holder refuses the run before any box; a stale one is
# reclaimed (rig_lease_acquire). Its identity is this repo's apply + a per-run id.
RIG_LEASE_REPO_NAME="camera-box-ro-units-apply"
RIG_LEASE_OURS=""

# rig_refused BOX -> 0 and RIG_REFUSAL set when BOX must not be touched NOW: the shared rig-held read
# (rig_held_reason: a fresh issue-281 heartbeat, which the lease does not cover -- rig-mode.sh TEST
# writes only that -- or a live lease that is not ours) or the shared rig-busy guard
# (stray_session_check_assert, run in a subshell so its own `exit 1` comes back here and the boxes
# already done still get their summary). Read before EVERY box (review round 2).
rig_refused() {
  RIG_REFUSAL=""
  if RIG_REFUSAL="$(rig_held_reason "$RIG_LEASE_STALE_SECS" "$RIG_LEASE_OURS")"; then
    return 0
  fi
  RIG_REFUSAL=""
  if ! (stray_session_check_assert "$RIG_BUSY_HERE" "$STRIH_HOST" "$STREAM_HOST" "the issue-1394 read-only-root apply on $1"); then
    RIG_REFUSAL="strih or stream records or streams (the rig-busy guard above)"
    return 0
  fi
  return 1
}

# first_fail_line TEXT -> the first `FAIL: [issue 1394] ...` line of a box's output, prefix cut.
first_fail_line() {
  local line
  while IFS= read -r line; do
    case "$line" in
      "FAIL: [issue 1394] "*) printf '%s\n' "${line#"FAIL: [issue 1394] "}"; return 0 ;;
    esac
  done <<<"${1:-}"
  printf '%s\n' "(no FAIL line; see the output above)"
}

cleanup() {
  rm -f "$LOG"
  if [ -n "$RIG_LEASE_OURS" ]; then
    rig_lease_release "$RIG_LEASE_OURS" >&2 || true
  fi
}

LOG="$(mktemp)"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$FORCE_LIVE" = 1 ]; then
  echo "WARNING: --force-live -- SKIPPING the rig guard (lease, rig heartbeat, rig-busy) for the issue-1394 apply on: ${NAMES[*]}" >&2
else
  # Set BEFORE the acquire (review round 4): a TERM that lands while the holder is written still
  # finds our id in cleanup and gives the lease back. rig_lease_release checks the holder's run_id,
  # so a foreign holder is never touched.
  RIG_LEASE_OURS="ro-units-apply-$(date -u +%Y%m%dT%H%M%SZ)-$$"
  # Declare the rolling look-ahead the per-box refresh keeps rolling (rig_lease_refresh_if_mine,
  # RIG_LEASE_LOOKAHEAD_SECS, 900 s), never the whole run: rig-busy-gate.sh fails FAST (exit 44) on
  # a holder whose expected_release_at lies past its 1800 s wait budget, so a longer declaration made
  # every full-path E2E that started during an apply fail at once instead of waiting (round 4).
  case "${RIG_LEASE_LOOKAHEAD_SECS:-}" in
    "" | *[!0-9]*) _hold_s=900 ;;
    *) _hold_s="$RIG_LEASE_LOOKAHEAD_SECS" ;;
  esac
  if _lease_out="$(rig_lease_acquire "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "" ro-units-apply \
    "$(date -u -d "+${_hold_s} seconds" +%Y-%m-%dT%H:%M:%SZ)" "$RIG_LEASE_STALE_SECS")"; then
    echo "[rig-lease] ${_lease_out} as ${RIG_LEASE_REPO_NAME}#${RIG_LEASE_OURS}, held until this run ends"
  else
    RIG_LEASE_OURS=""
    echo "ERROR: refused -- the rig lease is held: ${_lease_out#RIG_LEASE_HELD_BY=}. No box was touched; run --apply after it is released." >&2
    exit 1
  fi
fi

DONE=()
FAILED=()
NO_NETCONSOLE=()
UNTOUCHED=()
for _i in "${!NAMES[@]}"; do
  _name="${NAMES[$_i]}"
  _ip="${IPS[$_i]}"
  PROGRAM="$(cambox_ro_units_apply_program "${SINKS[$_i]}")" || {
    echo "ERROR: the program for $_name could not be built (log sink ${SINKS[$_i]})" >&2
    FAILED+=("$_name (the program could not be built)")
    continue
  }
  if [ "$FORCE_LIVE" = 0 ]; then
    # keep our own lease truthful (the :8890 view) while the run goes on; a no-op when not ours
    rig_lease_refresh_if_mine "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "$_hold_s" >/dev/null 2>&1 || true
    if rig_refused "$_name"; then
      UNTOUCHED=("${NAMES[@]:_i}")
      echo "ERROR: refused before $_name -- ${RIG_REFUSAL}. Not touched: ${UNTOUCHED[*]}; run --apply for them after it ends." >&2
      break
    fi
  fi
  echo "== $_name ($_ip): applying the issue-1394 read-only-root unit set + the log sink ${SINKS[$_i]} =="
  _rc=0
  # ServerAlive: a dead connection ends this ssh within ~60 s instead of holding the fleet loop.
  printf '%s\n' "$PROGRAM" \
    | sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout="$SSH_TIMEOUT" -o ServerAliveInterval=10 -o ServerAliveCountMax=6 \
      "root@$_ip" bash -s 2>&1 | tee "$LOG" || _rc=$?
  _out="$(cat "$LOG")"
  case "$_out" in
    *"issue 1311 netconsole is not provisioned here"*) NO_NETCONSOLE+=("$_name") ;;
  esac
  if [ "$_rc" -eq 0 ]; then
    DONE+=("$_name")
    echo "== $_name: OK =="
    continue
  fi
  if ro_window_close_failed "$_out"; then
    FAILED+=("$_name (root-rw: $(ro_window_holders "$_out"); nothing was started)")
  else
    case "$_out" in
      *"OK: the unit files are written"* | *"OK: nothing to write on"*)
        FAILED+=("$_name (the unit files are in place; a step outside the rw window failed: $(first_fail_line "$_out"))") ;;
      *) FAILED+=("$_name (rc=$_rc; the unit set did not land: $(first_fail_line "$_out"))") ;;
    esac
  fi
  echo "== $_name: FAILED (rc=$_rc) ==" >&2
done

if [ "${#NO_NETCONSOLE[@]}" -gt 0 ]; then
  echo "NOTE: no issue-1311 netconsole unit on: ${NO_NETCONSOLE[*]} -- re-run setup-device.sh there; verify-device (ak)/(ar) fail it until then" >&2
fi
if [ "${#FAILED[@]}" -gt 0 ] || [ "${#UNTOUCHED[@]}" -gt 0 ]; then
  [ "${#DONE[@]}" -eq 0 ] || echo "RESULT: in place on: ${DONE[*]}"
  [ "${#FAILED[@]}" -eq 0 ] || echo "RESULT: not clean on: $(printf '%s; ' "${FAILED[@]}")" >&2
  [ "${#UNTOUCHED[@]}" -eq 0 ] || echo "RESULT: refused by the rig guard, not touched: ${UNTOUCHED[*]}" >&2
  exit 1
fi
echo "RESULT: the issue-1394 unit set is in place on: ${NAMES[*]}"
