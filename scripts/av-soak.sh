#!/usr/bin/env bash
# scripts/av-soak.sh -- issue 1367: the MEASURE-ONLY 8 h stream-output A/V soak (full header below).
set -euo pipefail
#
# WHY (issue 1367, owner goal 24.9.2026): on the stream OBS output the picture/sound offset and the
# camera-to-camera alignment must hold for 8 h without drifting; a real drift is already visible
# after about 1 h. The full-path E2E is a single ~300 s snapshot AND it corrects the rig in its own
# cleanup (the A/V apply, the per-run pin align), so a loop of E2E runs would hide the very drift a
# soak must expose. This harness only MEASURES: it never writes a latency pin, an audio sync offset
# or any correction (no av_sync_calibrate --apply, no qr_align, no measurement pins, no NDI mapping).
#
# WHAT ONE RUN DOES (every rig action is an existing primitive, see scripts/lib/av-soak.sh):
#   setup, once:
#     - take the issue-830 rig lease under its OWN holder name (repo `camera-box-av-soak`) with an
#       expected release = the whole run, so a CI E2E fails fast instead of waiting; refuse when a
#       live foreign holder has it; start the issue-281 rig-active heartbeat
#     - READ-ONLY checks first: the shared issue-1271 rig-busy guard; the stream program must
#       ALREADY be the development scene (`Development`, issue 1380 -- TEST mode is a precondition,
#       `scripts/rig-mode.sh test`); the strih program scene (snapshot, restored at cleanup); the
#       permanent cam2 painter active with a growing QPSK marker log; every burn state readable
#     - the rig-busy guard again, then the mutations: the issue-1242 connect-on-show HOLD the E2E
#       uses (connect-on-show-hold.sh -- a hidden program-path input would otherwise be cut in cold
#       and parked), and the measurement burns ON (obs_burn_filter.py) only where they were OFF
#   every slot (default every 600 s, on a fixed grid from the first slot):
#     - the lease still ours, both record volumes above RECORDINGS_FREE_MIN_GB (else the run STOPS),
#       the stream program still the development scene and the painter service active (else the rig
#       left TEST mode and the run STOPS; an unreadable read or a stalled marker log is a skipped
#       row), the rig-busy guard, the strih-side hold marker re-asserted (its TTL is 4 h)
#     - StartRecord strih + stream (obs_phase2.py record), ONE sweep that cuts each soak camera into
#       strih program for AV_SOAK_SEGMENT_SECS (obs_phase2.py switch, switch_schedule.py plan/build
#       -- the E2E all-cambox sweep), StopRecord (verified by a status read)
#     - a live broadcast (a box streams) aborts the soak, exit 5: checked by the slot guard, before
#       every sweep cut, before the StopRecords and every ~60 s between slots -- never a cut and
#       never a StopRecord while it streams (the soak's file may then be the show's recording).
#       A recording is started or stopped only on a PROVEN idle rig: an unreadable read is retried
#       (AV_SOAK_BROADCAST_READS x AV_SOAK_BROADCAST_RETRY_S); still unreadable at a slot start =
#       a skipped row (nothing started), before the StopRecords = abort with the recordings kept
#     - a tail of the painter's marker log (read-only ssh), pushed with the schedule to the stream
#       box; the strih recording decoded IN PLACE on strih-lx (recording-verdict-on-strih-lx.sh),
#       the stream recording IN PLACE on the stream box (recording-verdict-on-stream.sh --execute),
#       both bounded + in parallel; the dev1 merge (recording-verdict --merge-partials)
#     - ONE CSV row (av_soak_decision.py row) + one timing.tsv line; the recordings are NOT deleted
#       (deletion is an owner-only step): their exact paths go to recordings.tsv + cleanup-plan.txt
#   cleanup, on EVERY exit (signals ignored, remote calls in their own session): stop this run's
#   remote decodes; ONLY on a proven idle rig (no box streams, both readable) StopRecord what is
#   still recording and restore the strih program scene when swept -- never while a broadcast is
#   live; restore connect-on-show, turn off the burns this run turned on (both are production
#   state), stop the heartbeat, write + print the report; release the lease -- unless a recording
#   the soak started may still run: then exit 5 with the lease KEPT for --stop-leftovers.
#
# MODES:
#   --plan  (DEFAULT) print every step with the exact commands; touches NOTHING (no lease, no ssh,
#           no OBS, no curl). Needs no credential and no binary.
#   --run   the soak. Needs CAM_PW (cam2 root, read-only use), STREAM_USER + STREAM_PW and
#           STRIH_USER + STRIH_PW (the stream box / strih-lx ssh, the same values recording-e2e.sh
#           uses), PROBE_BIN_DIR with the CI-built Linux recording-verdict, WIN_VERDICT_EXE_LOCAL =
#           the CI-built recording-verdict.exe.
#   --report RUN_DIR  re-grade an existing (or still running) run's CSV and exit with its verdict.
#   --stop-leftovers RUN_DIR  the systemd ExecStopPost hook (scripts/lib/av-soak-leftovers.sh):
#           never while a broadcast is live or a box is unreadable (ANY box streams -- strih never
#           streams, so its recording alone proves nothing); stops a recording the run flagged in
#           <run-dir>/recording.state only when the recording's own age puts its start at that
#           flag time; then releases the soak's lease (holder-checked).
#
# OPTIONS (env equivalent in brackets):
#   --hours H          [AV_SOAK_HOURS, 8]        run length; the last window starts at H
#   --slot-secs S      [AV_SOAK_SLOT_SECS, 600]  one window per slot (the acceptance: >= 1 per 10 min)
#   --segment-secs S   [AV_SOAK_SEGMENT_SECS, 30] seconds each camera is on strih program per window
#   --run-dir D        [AV_SOAK_RUN_DIR, ~/.camera-box/av-soak/<UTC stamp>]
#   --probe-bin-dir D  [PROBE_BIN_DIR]           dir holding the Linux recording-verdict
#   --win-verdict-exe P [WIN_VERDICT_EXE_LOCAL]  the Windows recording-verdict.exe
#   --spread-columns C [AV_SOAK_SPREAD_COLUMNS]  graded spread columns (see av_soak_decision.py)
#   --lease-run-id ID  [AV_SOAK_LEASE_RUN_ID]    run under a rig lease the CALLER already holds with this
#                      run id (the restart matrix, scripts/av-restart-matrix.sh, measures each window
#                      with `--run --hours 0`): verified at setup and every slot, never acquired and
#                      never released here (cleanup, --stop-leftovers); recording.state names no lease
# Other env: AV_SOAK_CAMS (default CAMERA_ACTIVE_SET; CAMBOX_OFFLINE_ACK/rig-fleet.txt acks are
#   always removed), STRIH_HOST / STREAM_HOST (default from scripts/lib/obs-fleet.sh), PAINTER_IP
#   (default cam2 from scripts/camera-set.sh), STRIH_CAPTURE_FPS / STREAM_CAPTURE_FPS (30, as
#   recording-e2e.sh), STREAM_PROG_SOURCE ("NDI 2ME PGM", as recording-e2e.sh / rig-mode.sh),
#   AV_SOAK_MERGE_TIMEOUT_S (90), AV_SOAK_OVERHEAD_S (90, the per-slot pre/stop/upload budget),
#   AV_SOAK_DECODE_TIMEOUT_S (default: what the slot leaves = slot - window - merge - overhead),
#   AV_SOAK_OBS_TIMEOUT_S (30), AV_SOAK_BROADCAST_READS (3) / AV_SOAK_BROADCAST_RETRY_S (20, the
#   retried broadcast read), RECORDINGS_FREE_MIN_GB (50), CONNECT_ON_SHOW_HOLD_STATE (the E2E's
#   ~/.camera-box/connect-on-show-hold.json). Test seams: AV_SOAK_OBS_DIR (dir of obs_phase2.py +
#   obs_burn_filter.py), AV_SOAK_STRIH_DECODE, AV_SOAK_STREAM_DECODE, AV_SOAK_MIN_SEGMENT_SECS (10),
#   AV_SOAK_MIN_DECODE_S (60),
#   RIG_LEASE_DIR, CAMERA_BOX_RIG_HEARTBEAT, CONNECT_ON_SHOW_MARKER_CMD, CONNECT_ON_SHOW_LOG_READ_CMD.
#
# STOP: `touch <run-dir>/STOP` (stops at the next wait/slot boundary, full cleanup + report), or
# `kill -TERM $(cat <run-dir>/pid)` (cleanup runs from the trap; a SIGTERM that lands during a
# retried broadcast read runs after that read, up to ~2 min with the defaults -- `systemctl stop`
# of the unit signals every process in it and is not delayed).
#
# EXIT: 0 PASS / 1 FAIL / 2 UNKNOWN (the final report of a run that ended normally or by STOP / a
#       low record volume / the rig leaving TEST mode), 3 usage error, 4 refused before any rig
#       change (lease held, rig busy, not in TEST mode, painter dead, a burn unreadable, missing
#       credential/binary/bound, a run dir that already holds a run), 5 aborted after a rig change
#       (lease lost, a broadcast went live, a setup mutation failed, a signal, a recording the soak
#       started may still run -- then the lease is kept); the report is still written on 5.
#       --stop-leftovers: 0 nothing left (lease released), 5 something kept (lease kept), 4 the
#       soak still runs, 3 the plan could not be made.
#
# Runbook + the rule: .claude/rules/av-soak.md.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/strih-platform.sh
. "$HERE/lib/strih-platform.sh"
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh"
# shellcheck source=scripts/lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$HERE/lib/rig-heartbeat.sh"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"
# shellcheck source=scripts/lib/stream-dev-scene.sh
. "$HERE/lib/stream-dev-scene.sh"
# shellcheck source=scripts/lib/mv-reverify-escalate.sh
. "$HERE/lib/mv-reverify-escalate.sh"
# shellcheck source=scripts/lib/genlock-park.sh
. "$HERE/lib/genlock-park.sh"
# shellcheck source=scripts/lib/connect-on-show-hold.sh
. "$HERE/lib/connect-on-show-hold.sh"
# shellcheck source=scripts/lib/av-soak.sh
. "$HERE/lib/av-soak.sh"
# shellcheck source=scripts/lib/av-soak-leftovers.sh
. "$HERE/lib/av-soak-leftovers.sh"

log() { printf '%s [av-soak] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { echo "av-soak: ERROR: $2" >&2; exit "$1"; }
usage() { sed -n '2,/^# Runbook/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
need_value() { [ "$2" -ge 2 ] || die 3 "$1 needs a value (try --help)"; }

MODE=plan
REPORT_DIR=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan) MODE=plan ;;
    --run) MODE=run ;;
    --report) need_value "$1" "$#"; MODE=report; REPORT_DIR="$2"; shift ;;
    --stop-leftovers) need_value "$1" "$#"; MODE=stop-leftovers; REPORT_DIR="$2"; shift ;;
    --hours) need_value "$1" "$#"; AV_SOAK_HOURS="$2"; shift ;;
    --slot-secs) need_value "$1" "$#"; AV_SOAK_SLOT_SECS="$2"; shift ;;
    --segment-secs) need_value "$1" "$#"; AV_SOAK_SEGMENT_SECS="$2"; shift ;;
    --run-dir) need_value "$1" "$#"; AV_SOAK_RUN_DIR="$2"; shift ;;
    --probe-bin-dir) need_value "$1" "$#"; PROBE_BIN_DIR="$2"; shift ;;
    --win-verdict-exe) need_value "$1" "$#"; WIN_VERDICT_EXE_LOCAL="$2"; shift ;;
    --spread-columns) need_value "$1" "$#"; AV_SOAK_SPREAD_COLUMNS="$2"; shift ;;
    --lease-run-id) need_value "$1" "$#"; AV_SOAK_LEASE_RUN_ID="$2"; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die 3 "unknown argument '$1' (try --help)" ;;
  esac
  shift
done

DECISION="$HERE/av_soak_decision.py"
RIG_STATE="$HERE/av_soak_rig_state.py"
HOURS="${AV_SOAK_HOURS:-8}"
SLOT_S="${AV_SOAK_SLOT_SECS:-600}"
SEGMENT_S="${AV_SOAK_SEGMENT_SECS:-30}"
MIN_SEGMENT_S="${AV_SOAK_MIN_SEGMENT_SECS:-10}"
SPREAD_ARGS=()
if [ -n "${AV_SOAK_SPREAD_COLUMNS+x}" ]; then SPREAD_ARGS=(--spread-columns "$AV_SOAK_SPREAD_COLUMNS"); fi

case "$HOURS" in '' | *[!0-9.]* | *.*.*) die 3 "--hours must be a non-negative number, got '$HOURS'" ;; esac
DURATION_S="$(awk -v h="$HOURS" 'BEGIN { printf "%d", h * 3600 + 0.5 }')"

OBS_DIR="${AV_SOAK_OBS_DIR:-$HERE}"
STRIH_HOST="${STRIH_HOST:-$(obs_fleet_host strih-lx)}"
STREAM_HOST="${STREAM_HOST:-$(obs_fleet_host stream)}"
OBS_TIMEOUT_S="${AV_SOAK_OBS_TIMEOUT_S:-30}"
BROADCAST_READS="${AV_SOAK_BROADCAST_READS:-3}"
BROADCAST_RETRY_S="${AV_SOAK_BROADCAST_RETRY_S:-20}"
# StopRecord settles asynchronously: re-read the status this many times, this far apart.
REC_STOP_STATUS_READS="${AV_SOAK_REC_STOP_STATUS_READS:-10}"
REC_STOP_STATUS_POLL_S="${AV_SOAK_REC_STOP_STATUS_POLL_S:-1}"
case "$OBS_TIMEOUT_S$BROADCAST_READS$BROADCAST_RETRY_S" in
  *[!0-9]* | "") die 3 "AV_SOAK_OBS_TIMEOUT_S / AV_SOAK_BROADCAST_READS / AV_SOAK_BROADCAST_RETRY_S must be integers" ;;
esac
[ "$BROADCAST_READS" -ge 1 ] || die 3 "AV_SOAK_BROADCAST_READS must be >= 1"

if [ "$MODE" = report ]; then
  [ -f "$REPORT_DIR/soak.csv" ] || die 3 "--report needs a run dir holding soak.csv (got '${REPORT_DIR}')"
  rc=0
  python3 "$DECISION" report --csv "$REPORT_DIR/soak.csv" --min-duration-h "$HOURS" \
    --json "$REPORT_DIR/report-latest.json" "${SPREAD_ARGS[@]}" || rc=$?
  exit "$rc"
fi

# --stop-leftovers RUN_DIR: the ExecStopPost safety net (scripts/lib/av-soak-leftovers.sh).
if [ "$MODE" = stop-leftovers ]; then
  rc=0
  av_soak_stop_leftovers "$REPORT_DIR" "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$OBS_TIMEOUT_S" \
    "$RIG_STATE" || rc=$?
  exit "$rc"
fi

for _n in "$SLOT_S" "$SEGMENT_S" "$MIN_SEGMENT_S"; do
  case "$_n" in '' | *[!0-9]*) die 3 "--slot-secs / --segment-secs must be integers (got '$_n')" ;; esac
done
[ "$SEGMENT_S" -ge 1 ] && [ "$SEGMENT_S" -ge "$MIN_SEGMENT_S" ] || die 3 "--segment-secs $SEGMENT_S is below $MIN_SEGMENT_S s (each camera needs enough QPSK markers for a measured A/V offset)"

camera_resolve cam2
PAINTER_IP="${PAINTER_IP:-$CAMERA_IP}"
MARKER_LOG="${AV_SOAK_MARKER_LOG:-/run/rig-qpsk-markers.csv}"
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}"
STREAM_DEV_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}"
STRIH_CAPTURE_FPS="${STRIH_CAPTURE_FPS:-30}"
STREAM_CAPTURE_FPS="${STREAM_CAPTURE_FPS:-30}"
SSH_TIMEOUT_S="${AV_SOAK_SSH_TIMEOUT_S:-60}"
MERGE_TIMEOUT_S="${AV_SOAK_MERGE_TIMEOUT_S:-90}"
OVERHEAD_S="${AV_SOAK_OVERHEAD_S:-90}"
RECORDINGS_FREE_MIN_GB="${RECORDINGS_FREE_MIN_GB:-50}"
BUNDLE_STATE_PORT="${WIN_BUNDLE_STATE_PORT:-8899}"
HOLD_STATE="${CONNECT_ON_SHOW_HOLD_STATE:-$HOME/.camera-box/connect-on-show-hold.json}"
STRIH_DECODE="${AV_SOAK_STRIH_DECODE:-$HERE/recording-verdict-on-strih-lx.sh}"
STREAM_DECODE="${AV_SOAK_STREAM_DECODE:-$HERE/recording-verdict-on-stream.sh}"
# relative to the strih-lx login home: the same dir recording-verdict-on-strih-lx.sh defaults to
STRIH_LX_OUT_DIR="${AV_SOAK_STRIH_LX_OUT_DIR:-verdict-out}"
OUT_DIR_WIN="${OUT_DIR_WIN:-C:\\camera-box\\verdict-out}"
CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "${RIG_FLEET_ACK_FILE:-$HERE/../rig-fleet.txt}")"
export CAMBOX_OFFLINE_ACK
SOAK_CAMS="$(av_soak_unacked_cams "${AV_SOAK_CAMS:-$CAMERA_ACTIVE_SET}")"
[ -n "$SOAK_CAMS" ] || die 3 "no camera to soak (the set minus the acked-offline ones is empty)"
for _c in $SOAK_CAMS; do
  camera_resolve "$_c" >/dev/null || die 3 "unknown camera '$_c' in the soak set"
done
SWEEP="$(CAMERA_ACTIVE_SET="$SOAK_CAMS" camera_active_sweep_pairs)"
read -r -a _SOAK_CAM_ARR <<< "$SOAK_CAMS"
N_CAMS="${#_SOAK_CAM_ARR[@]}"
WINDOW_S=$(( N_CAMS * SEGMENT_S ))
MIN_SECS="$(av_soak_min_secs "$WINDOW_S")"
WINDOWS="$(av_soak_windows_count "$DURATION_S" "$SLOT_S")"
MARKER_ROWS="$(av_soak_marker_rows "$WINDOW_S")"
MIN_DECODE_S="${AV_SOAK_MIN_DECODE_S:-60}"
case "$MERGE_TIMEOUT_S$OVERHEAD_S$MIN_DECODE_S" in *[!0-9]* | "") die 3 "the timeouts must be integers" ;; esac
_decode_left=$(( SLOT_S - WINDOW_S - MERGE_TIMEOUT_S - OVERHEAD_S ))
DECODE_TIMEOUT_S="${AV_SOAK_DECODE_TIMEOUT_S:-$_decode_left}"
_budget_msg="the slot budget does not fit: window ${WINDOW_S} s (${N_CAMS} cameras x ${SEGMENT_S} s) + decode ${DECODE_TIMEOUT_S} s (>= ${MIN_DECODE_S}) + merge ${MERGE_TIMEOUT_S} s + overhead ${OVERHEAD_S} s must be <= the ${SLOT_S} s slot"
case "$DECODE_TIMEOUT_S" in '' | *[!0-9]*) die 3 "$_budget_msg" ;; esac
[ "$DECODE_TIMEOUT_S" -ge "$MIN_DECODE_S" ] \
  && [ $(( WINDOW_S + DECODE_TIMEOUT_S + MERGE_TIMEOUT_S + OVERHEAD_S )) -le "$SLOT_S" ] \
  || die 3 "$_budget_msg"
[ "$STREAM_DEV_SCENE" != "$STREAM_PRODUCTION_SCENE_DEFAULT" ] || die 3 "STREAM_PROG_SCENE names the production scene; the soak only runs on the development scene (issue 1380)"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="${AV_SOAK_RUN_DIR:-$HOME/.camera-box/av-soak/$STAMP}"
CSV="$RUN_DIR/soak.csv"
RIG_LEASE_REPO_NAME="${AV_SOAK_LEASE_REPO:-camera-box-av-soak}"
RIG_LEASE_OURS="av-soak-${STAMP}-$$"
# --lease-run-id: the caller (the restart matrix) holds the lease across its restarts; this run
# verifies it and keeps its heartbeat fresh, but never acquires or releases it, and recording.state
# names no lease (so --stop-leftovers of this run dir can never release the caller's lease).
LEASE_INHERITED="${AV_SOAK_LEASE_RUN_ID:-}"
REC_STATE_LEASE="$RIG_LEASE_OURS"
if [ -n "$LEASE_INHERITED" ]; then
  RIG_LEASE_OURS="$LEASE_INHERITED"
  REC_STATE_LEASE=""
fi
LEASE_EXPECTED_AT="$(date -u -d "+$(( DURATION_S + SLOT_S + 1800 )) seconds" +%Y-%m-%dT%H:%M:%SZ)"
VERDICT_BIN="${PROBE_BIN_DIR:+$PROBE_BIN_DIR/recording-verdict}"

# --- --plan: print every step, touch nothing ------------------------------------------------------

plan_cmd() { printf '      '; printf '%q ' "$@"; printf '\n'; }

print_plan() {
  local strih_argv stream_argv merge_argv marker_win sched_win partial_win bounds_line c seg scene label
  bounds_line="$(python3 "$DECISION" bounds 2>&1)" || bounds_line="UNREADABLE: $bounds_line"
  cat <<EOF
===== av-soak PLAN (issue 1367) -- nothing below is executed; run with --run =====
bounds (read from their single sources): ${bounds_line//$'\n'/; }
run: ${HOURS} h = ${DURATION_S} s, one window every ${SLOT_S} s -> ${WINDOWS} window(s) at +0 s .. +$(( (WINDOWS - 1) * SLOT_S )) s
cameras: ${SOAK_CAMS}   (CAMERA_ACTIVE_SET='${CAMERA_ACTIVE_SET}', acked offline: '${CAMBOX_OFFLINE_ACK:-none}')
window: ONE sweep '${SWEEP}' = ${N_CAMS} x ${SEGMENT_S} s = ${WINDOW_S} s (merge --min-secs ${MIN_SECS})
slot budget: window ${WINDOW_S} s + decode <= ${DECODE_TIMEOUT_S} s + merge <= ${MERGE_TIMEOUT_S} s + overhead ${OVERHEAD_S} s <= slot ${SLOT_S} s
boxes: strih ${STRIH_HOST} ($(strih_platform "$STRIH_HOST")), stream ${STREAM_HOST}, cam2 painter ${PAINTER_IP} (marker log ${MARKER_LOG})
run dir: ${RUN_DIR}  (soak.csv, timing.tsv, report-latest.txt, report.txt/json, recordings.tsv, cleanup-plan.txt, pid, STOP)
EOF
  if [ "$(strih_platform "$STRIH_HOST")" != linux ]; then
    echo "NOTE: --run refuses a Windows strih (retired at M4); only the strih-lx in-place decode is supported."
  fi
  echo
  echo "SETUP (once) -- reads first, nothing changes until step 6:"
  if [ -n "$LEASE_INHERITED" ]; then
    echo "  1. rig lease (issue 830): the caller's rig lease ${LEASE_INHERITED} -- verified held (else refuse, exit 4), its heartbeat kept fresh, never acquired or released here"
    echo "     rig_heartbeat_start av-soak (issue 281)"
  else
    echo "  1. rig lease (issue 830): rig_lease_acquire repo=${RIG_LEASE_REPO_NAME} run_id=${RIG_LEASE_OURS} job=av-soak expected_release_at=${LEASE_EXPECTED_AT}"
    echo "     a live foreign holder -> refuse (exit 4); rig_heartbeat_start av-soak (issue 281)"
  fi
  echo "  2. rig-busy guard (issue 1271, read-only): stray_session_check_assert ${OBS_DIR} ${STRIH_HOST} ${STREAM_HOST} 'the av-soak setup'"
  echo "  3. read-only: stream program must be '${STREAM_DEV_SCENE}' (else refuse: run scripts/rig-mode.sh test first); strih program snapshot:"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STREAM_HOST"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STRIH_HOST"
  echo "  4. read-only: the permanent cam2 painter is active + its marker log grows (ssh root@${PAINTER_IP}):"
  echo "      $(av_soak_painter_probe_cmd "$MARKER_LOG")"
  echo "  5. read-only: every burn state:"
  for c in $SOAK_CAMS; do
    plan_cmd python3 "$OBS_DIR/obs_burn_filter.py" check --host "$STRIH_HOST" --input "NDI $c"
  done
  plan_cmd python3 "$OBS_DIR/obs_burn_filter.py" check --host "$STREAM_HOST" --input "$STREAM_PROG_SOURCE"
  echo "  6. the rig-busy guard again, then: connect-on-show HOLD (issue 1242, the E2E's own helper, state ${HOLD_STATE}):"
  echo "      connect_on_show_e2e_hold ${OBS_DIR} ${STRIH_HOST} ${HOLD_STATE}; connect_on_show_e2e_wait_live (bounded)"
  echo "  7. obs_burn_filter.py add --host <ip> --input <input> for each burn that was OFF, then check it again"
  echo
  echo "EVERY SLOT k (slot start = run start + k x ${SLOT_S} s; files under ${RUN_DIR}/slot-NNN):"
  echo "  a. lease still ours; record volumes free >= ${RECORDINGS_FREE_MIN_GB} GB (curl http://<box>:${BUNDLE_STATE_PORT}/record-dir-stats.json -> bundle_state_gather.recordings_free_line; below = stop the soak)"
  echo "  b. read-only: stream program still '${STREAM_DEV_SCENE}' and the painter service active (else the rig left TEST mode: the run STOPS); an unreadable read or a stalled marker log = a skipped row"
  echo "  c. a proven idle rig (rig-busy-check, an unreadable read retried ${BROADCAST_READS}x ${BROADCAST_RETRY_S} s apart; still unreadable = a skipped row, nothing started),"
  echo "     then the rig-busy guard: stray_session_check_assert ... 'the slot-k StartRecord' (a live broadcast aborts the soak, exit 5)"
  echo "     connect_on_show_strih_marker set ${STRIH_HOST} (re-asserts the 4 h hold marker)"
  echo "  d. StartRecord (the started flag is set BEFORE the call; any start failure stops both boxes):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action start
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action start
  echo "  e. the sweep -- strih program only (the stream program is never switched); a broadcast check (rig-busy-check) before every cut and before f: live = abort, exit 5, nothing stopped:"
  plan_cmd python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S"
  while IFS= read -r seg; do
    [ -n "$seg" ] || continue
    scene="${seg%%$'\t'*}"; label="${seg##*$'\t'}"
    echo "      [$label] $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" switch --host "$STRIH_HOST" --program-scene "$scene"); sleep ${SEGMENT_S}"
  done < <(python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S")
  plan_cmd python3 "$HERE/switch_schedule.py" build --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S" --start-ns "<first switch ns>" --boundaries "<later switches + stop ns>"
  echo "  f. StopRecord, verified (the flag clears only when 'record --action status' reads active=False):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action stop
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action status
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action stop
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action status
  echo "  g. read-only marker-log tail from cam2 (${MARKER_ROWS} rows): $(av_soak_marker_snapshot_cmd "$MARKER_LOG" "$MARKER_ROWS")"
  marker_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-markers.csv")"
  sched_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-switch-schedule.json")"
  partial_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-stream-partial.json")"
  echo "     push to the stream box: win_ssh_upload markers -> ${marker_win}; schedule -> ${sched_win}"
  echo "  h. decode in place, both in parallel, each bounded by ${DECODE_TIMEOUT_S} s (a timeout stops that box's recording-verdict):"
  av_soak_strih_extract_argv strih_argv "$STRIH_DECODE" "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
    "$STRIH_LX_OUT_DIR" "$RUN_DIR/slot-NNN" "<strih StopRecord path>" "$STRIH_CAPTURE_FPS" \
    "av-soak-${STAMP}-sNNN-strih-partial.json"
  plan_cmd env STRIH_LX_BOX="$STRIH_HOST" "${strih_argv[@]}"
  av_soak_stream_extract_argv stream_argv "$STREAM_DECODE" "${WIN_VERDICT_EXE_LOCAL:-<WIN_VERDICT_EXE_LOCAL>}" \
    "$OUT_DIR_WIN" "$RUN_DIR/slot-NNN" "<stream StopRecord path>" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
    "<painter run_id>" "$marker_win" "$sched_win" "$partial_win"
  plan_cmd env STREAM_BOX="$STREAM_HOST" "${stream_argv[@]}"
  echo "  i. merge on dev1 (bounded by ${MERGE_TIMEOUT_S} s; its exit code is not the soak verdict):"
  av_soak_merge_argv merge_argv "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
    "$RUN_DIR/slot-NNN/av-soak-${STAMP}-sNNN-strih-partial.json" "$RUN_DIR/slot-NNN/av-soak-${STAMP}-sNNN-stream-partial.json" \
    "$MIN_SECS" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "<painter run_id>" "$CAMBOX_OFFLINE_ACK" \
    "$RUN_DIR/slot-NNN/switch-schedule.json" "$RUN_DIR/slot-NNN/pixel-proof" "$RUN_DIR/slot-NNN/verdict.json" \
    "${AV_EXPECTED_MS:-}"
  plan_cmd "${merge_argv[@]}"
  echo "  j. one CSV row, one timing.tsv line, the progress report (printed once the first hour is complete):"
  plan_cmd python3 "$DECISION" row --csv "$CSV" --cams "$SOAK_CAMS" --verdict-json "$RUN_DIR/slot-NNN/verdict.json" --epoch-s "<window start>" --slot "<k>" --slot-s "$SLOT_S" --window-s "$WINDOW_S"
  plan_cmd python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" "${SPREAD_ARGS[@]}"
  echo "  k. wait for the next slot start (lease heartbeat every <= 10 s; a broadcast check every ~60 s, live = abort; <run-dir>/STOP ends the run)"
  echo
  echo "CLEANUP (every exit; signals ignored; remote calls under setsid -w): stop this run's remote decodes still running;"
  echo "  on a proven idle rig only (never while a broadcast is live): StopRecord what is still recording and the strih program back"
  echo "  to the snapshot (switch --prod-floor) when swept; a recording that may still run = exit 5 and the lease is KEPT for --stop-leftovers;"
  echo "  connect_on_show_e2e_restore; obs_burn_filter.py remove on the burns this run turned on; rig_heartbeat_stop;"
  echo "  rig_lease_release ${RIG_LEASE_OURS}; the final report:"
  plan_cmd python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" --json "$RUN_DIR/report.json" "${SPREAD_ARGS[@]}"
  echo "  recordings are NOT deleted (owner-only): ${RUN_DIR}/cleanup-plan.txt lists the exact-path removal lines."
  echo
  echo "NEVER: a latency pin, an audio sync offset, a measurement pin, an NDI mapping, a stream scene switch."
  local missing=()
  [ -n "${CAM_PW:-}" ] || missing+=(CAM_PW)
  [ -n "${STREAM_USER:-}" ] || missing+=(STREAM_USER)
  [ -n "${STREAM_PW:-}" ] || missing+=(STREAM_PW)
  [ -n "${STRIH_USER:-}" ] || missing+=(STRIH_USER)
  [ -n "${STRIH_PW:-}" ] || missing+=(STRIH_PW)
  [ -n "${VERDICT_BIN:-}" ] && [ -x "$VERDICT_BIN" ] || missing+=("PROBE_BIN_DIR (recording-verdict)")
  [ -n "${WIN_VERDICT_EXE_LOCAL:-}" ] && [ -f "$WIN_VERDICT_EXE_LOCAL" ] || missing+=("WIN_VERDICT_EXE_LOCAL")
  if [ "${#missing[@]}" -gt 0 ]; then
    echo "--run would need: ${missing[*]}"
  else
    echo "--run prerequisites present."
  fi
}

if [ "$MODE" = plan ]; then
  print_plan
  exit 0
fi

# --- --run ----------------------------------------------------------------------------------------

[ "$(strih_platform "$STRIH_HOST")" = linux ] || die 4 "strih ${STRIH_HOST} is not the Linux strih-lx; the soak only decodes strih in place on strih-lx"
for _v in CAM_PW STREAM_USER STREAM_PW STRIH_USER STRIH_PW; do
  [ -n "${!_v:-}" ] || die 4 "$_v is not set (the same value recording-e2e.sh uses; see targets.md)"
done
export STREAM_USER STREAM_PW STRIH_USER STRIH_PW
[ -n "${VERDICT_BIN:-}" ] && [ -x "$VERDICT_BIN" ] || die 4 "PROBE_BIN_DIR must hold the CI-built Linux recording-verdict (probe-tools-linux-amd64)"
[ -n "${WIN_VERDICT_EXE_LOCAL:-}" ] && [ -f "$WIN_VERDICT_EXE_LOCAL" ] || die 4 "WIN_VERDICT_EXE_LOCAL must be the CI-built recording-verdict.exe (probe-tools-windows-amd64)"
for _t in sshpass curl timeout setsid python3; do
  command -v "$_t" >/dev/null 2>&1 || die 4 "'$_t' not found on PATH"
done
python3 "$DECISION" bounds >/dev/null || die 4 "the gate bounds cannot be read from their Rust sources (a renamed constant?)"

# a run dir is one run: a second --run there would reset a live run's recording.state, delete its
# STOP file and append to its CSV (two runs graded as one series)
for _f in pid soak.csv recording.state; do
  [ ! -e "$RUN_DIR/$_f" ] || die 4 "the run dir $RUN_DIR already holds a run ($_f) -- give --run-dir a new directory (--report / --stop-leftovers read an old one)"
done
mkdir -p "$RUN_DIR"
printf '%s\n' "$$" > "$RUN_DIR/pid"
rm -f "$RUN_DIR/STOP"

SETUP_STARTED=0
MUTATED=0
LEASE_HELD=0
HOLD_ATTEMPTED=0
LOOP_DONE=0
STOPPED=0
IN_CLEANUP=0
STRIH_REC_STARTED=0
STREAM_REC_STARTED=0
STRIH_REC_SINCE=""
STREAM_REC_SINCE=""
STRIH_SWEPT=0
STRIH_PROGRAM_SNAPSHOT=""
BURNS_TURNED_ON=()
BG_PIDS=()
SLEEP_PID=""
DECODES_RUNNING=0
ABORT_REASON=""
STOP_SKIPPED_WHY=""
FS_UNKNOWN_LOGGED=""
REC_PATH=""

# Every OBS / burn call is bounded; inside cleanup it runs in its own session (setsid -w) so a
# second Ctrl-C at the terminal cannot kill the restore (the issue-808 trap recipe).
obs() {
  if [ "$IN_CLEANUP" = 1 ]; then
    setsid -w timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_phase2.py" "$@"
  else
    timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_phase2.py" "$@"
  fi
}
burn() {
  if [ "$IN_CLEANUP" = 1 ]; then
    setsid -w timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_burn_filter.py" "$@"
  else
    timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_burn_filter.py" "$@"
  fi
}
cam2_read() {
  timeout "$SSH_TIMEOUT_S" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR -o ConnectTimeout=10 "root@$PAINTER_IP" "$1"
}
# win_bounded FUNC HOST ARGS... -> a scripts/lib/win-ssh-exec.sh call in a child bash, so `timeout`
# bounds the whole group; the stream credentials come from the (exported) environment, not argv.
win_bounded() {
  local pre=()
  if [ "$IN_CLEANUP" = 1 ]; then pre=(setsid -w); fi
  # shellcheck disable=SC2016  # expanded by the child bash
  "${pre[@]}" timeout "$SSH_TIMEOUT_S" bash -c '. "$1"; f="$2"; h="$3"; shift 3; "$f" "$STREAM_USER" "$STREAM_PW" "$h" "$@"' \
    _ "$HERE/lib/win-ssh-exec.sh" "$@"
}
# kill_remote_decode strih|stream -> stop this run's recording-verdict decode ON the box (a local
# timeout only kills the local ssh; the remote decode would keep loading the box into the next
# recording). Matched by this run's own output names, so no other decode on the box is touched.
kill_remote_decode() {
  local pre=()
  if [ "$IN_CLEANUP" = 1 ]; then pre=(setsid -w); fi
  if [ "$1" = strih ]; then
    "${pre[@]}" timeout "$SSH_TIMEOUT_S" sshpass -p "$STRIH_PW" ssh -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 "$STRIH_USER@$STRIH_HOST" \
      "$(av_soak_strih_decode_kill_cmd "$STAMP")" >/dev/null 2>&1 || true
  else
    win_bounded win_ssh_run "$STREAM_HOST" "$(av_soak_stream_decode_kill_ps "$STAMP")" >/dev/null 2>&1 || true
  fi
}
guard_ok() {
  ( stray_session_check_assert "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$1" )
}
refuse() {  # REASON -> exit 4 while nothing was changed, else 5
  ABORT_REASON="$1"
  if [ "$MUTATED" = 1 ]; then exit 5; fi
  exit 4
}
isleep() {  # SECS -> a sleep a signal interrupts at once (only the CURRENT sleep is tracked)
  sleep "$1" &
  SLEEP_PID=$!
  wait "$SLEEP_PID" || true
  SLEEP_PID=""
}
note_recording() {  # K BOX PATH -> recordings.tsv + the exact-path removal plan line
  local k="$1" box="$2" path="${3:-}"
  [ -n "$path" ] || return 0
  printf '%s\t%s\t%s\n' "$k" "$box" "$path" >> "$RUN_DIR/recordings.tsv"
  if [ "$box" = strih ]; then
    strih_lx_recording_cleanup_note "" "$STRIH_HOST" "$path" >> "$RUN_DIR/cleanup-plan.txt"
  else
    echo "win-stream-snv Shell: Remove-Item -Force -LiteralPath '$path'" >> "$RUN_DIR/cleanup-plan.txt"
  fi
}
# <run-dir>/recording.state -- read by --stop-leftovers: the flags, the time each flag was set (right
# before its StartRecord: the ownership proof) and the soak's lease run id.
write_rec_state() {
  printf 'strih=%s\nstrih_since=%s\nstream=%s\nstream_since=%s\nlease=%s\nstart_window_s=%s\n' \
    "$STRIH_REC_STARTED" "$STRIH_REC_SINCE" "$STREAM_REC_STARTED" "$STREAM_REC_SINCE" \
    "$REC_STATE_LEASE" "$((OBS_TIMEOUT_S + 30))" > "$RUN_DIR/recording.state"
}
set_rec_flag() {  # BOX 0|1
  if [ "$1" = strih ]; then
    STRIH_REC_STARTED="$2"
    if [ "$2" = 1 ]; then STRIH_REC_SINCE="$(date +%s)"; fi
  else
    STREAM_REC_STARTED="$2"
    if [ "$2" = 1 ]; then STREAM_REC_SINCE="$(date +%s)"; fi
  fi
  write_rec_state
}
write_rec_state  # the run starts with nothing flagged
rec_start() {  # BOX HOST LOG -> the started flag is set BEFORE the call (a start that fails after
  # StartRecord, or times out, must still be stopped)
  set_rec_flag "$1" 1
  obs record --host "$2" --action start >> "$3" 2>&1
}
rec_stop() {  # K BOX HOST LOG -> REC_PATH; the flag clears only when the status reads inactive
  local st
  local i
  REC_PATH="$(obs record --host "$3" --action stop 2>>"$4" | tail -n 1 || true)"
  # OBS stops asynchronously: the first status read after StopRecord can still say active
  for ((i = 1; i <= REC_STOP_STATUS_READS; i++)); do
    st="$(obs record --host "$3" --action status 2>>"$4" | tail -n 1 || true)"
    case "$st" in active=False*) break ;; esac
    if [ "$i" -lt "$REC_STOP_STATUS_READS" ]; then sleep "$REC_STOP_STATUS_POLL_S"; fi
  done
  case "$st" in
    active=False*) set_rec_flag "$2" 0 ;;
    *) log "WARNING: $2 is still recording after StopRecord (status '${st:-unreadable}') -- cleanup retries" ;;
  esac
  note_recording "$1" "$2" "$REC_PATH"
}

# broadcast_now -> live | unknown | idle (the pure av_soak_rig_state.py decision over one rig-busy read)
broadcast_now() {
  local out
  out="$(obs rig-busy-check --strih-host "$STRIH_HOST" --stream-host "$STREAM_HOST" \
    --password "${OBS_PASSWORD:-}" 2>/dev/null || true)"
  printf '%s' "$out" | python3 "$RIG_STATE" broadcast 2>/dev/null || echo unknown
}
# broadcast_settled -> live | unknown | idle, an unreadable read retried (BROADCAST_READS reads,
# BROADCAST_RETRY_S apart) before it counts: one stream OBS restart must neither end an 8 h run nor
# keep a recording running. Used wherever a recording is started or stopped.
broadcast_settled() {
  local i b=unknown
  for ((i = 1; i <= BROADCAST_READS; i++)); do
    b="$(broadcast_now)"
    if [ "$b" != unknown ]; then break; fi
    if [ "$i" -lt "$BROADCAST_READS" ]; then sleep "$BROADCAST_RETRY_S"; fi
  done
  printf '%s\n' "$b"
}

cleanup() {
  local rc=$? rrc=2 t pid bstate="" why=""
  set +e
  trap '' INT TERM HUP PIPE
  IN_CLEANUP=1
  for pid in "${BG_PIDS[@]}" $SLEEP_PID; do kill -TERM "$pid" 2>/dev/null; done
  if [ "$SETUP_STARTED" = 1 ]; then
    log "cleanup${ABORT_REASON:+ (stopping: $ABORT_REASON)}" 2>/dev/null
    if [ "$DECODES_RUNNING" = 1 ]; then
      kill_remote_decode strih
      kill_remote_decode stream
      log "remote decodes stopped" 2>/dev/null
    fi
    # ONE broadcast read gates every cleanup mutation that touches the air: during a broadcast the
    # soak's recording may already be the show's own (a go-live StartRecord is a no-op on a box
    # that records) and strih's program feeds the stream box's program
    if [ "$STRIH_REC_STARTED" = 1 ] || [ "$STREAM_REC_STARTED" = 1 ] || [ "$STRIH_SWEPT" = 1 ]; then
      bstate="$(broadcast_settled)"
      if [ "$bstate" = live ]; then why="a broadcast is live"; else why="the rig state is unreadable"; fi
    fi
    if [ "$STRIH_REC_STARTED" = 1 ] || [ "$STREAM_REC_STARTED" = 1 ]; then
      if [ "$bstate" = idle ]; then
        [ "$STRIH_REC_STARTED" = 1 ] && rec_stop cleanup strih "$STRIH_HOST" "$RUN_DIR/cleanup.log"
        [ "$STREAM_REC_STARTED" = 1 ] && rec_stop cleanup stream "$STREAM_HOST" "$RUN_DIR/cleanup.log"
      else
        STOP_SKIPPED_WHY="$why"
        log "WARNING: NOT stopping the soak's recording(s) -- $why: the file may be the show's recording now; --stop-leftovers stops it once the broadcast ends" 2>/dev/null
      fi
    fi
    if [ "$STRIH_SWEPT" = 1 ] && [ -n "$STRIH_PROGRAM_SNAPSHOT" ]; then
      if [ "$bstate" != idle ]; then
        log "WARNING: strih program NOT restored -- $why; a cut there would be a cut on air. It stays on the last sweep scene; set it back to '$STRIH_PROGRAM_SNAPSHOT' by hand once the broadcast ends" 2>/dev/null
      else
        # the switch sets the scene first and can then fail its non-black check on a dim operator
        # scene, so the outcome is confirmed by re-reading the program scene
        obs switch --host "$STRIH_HOST" --program-scene "$STRIH_PROGRAM_SNAPSHOT" --prod-floor \
          >> "$RUN_DIR/cleanup.log" 2>&1
        if [ "$(obs program-scene --host "$STRIH_HOST" 2>/dev/null | tr -d '\r' | head -n 1)" = "$STRIH_PROGRAM_SNAPSHOT" ]; then
          log "strih program restored to '$STRIH_PROGRAM_SNAPSHOT'" 2>/dev/null
        else
          log "WARNING: could not restore the strih program to '$STRIH_PROGRAM_SNAPSHOT'" 2>/dev/null
        fi
      fi
    fi
    if [ "$HOLD_ATTEMPTED" = 1 ]; then
      # shellcheck disable=SC2016  # expanded by the child bash
      setsid -w bash -c '. "$1"; . "$2"; shift 2; connect_on_show_e2e_restore "$@"' _ \
        "$HERE/lib/strih-platform.sh" "$HERE/lib/connect-on-show-hold.sh" "$OBS_DIR" "$STRIH_HOST" "$HOLD_STATE" \
        >> "$RUN_DIR/cleanup.log" 2>&1
      log "connect-on-show restored (see cleanup.log)" 2>/dev/null
    fi
    for t in "${BURNS_TURNED_ON[@]}"; do
      if burn remove --host "${t%%|*}" --input "${t#*|}" >> "$RUN_DIR/cleanup.log" 2>&1; then
        log "burn OFF again on ${t%%|*} '${t#*|}' (it was off before the soak)" 2>/dev/null
      else
        log "WARNING: could not turn the burn back off on ${t%%|*} '${t#*|}'" 2>/dev/null
      fi
    done
    rig_heartbeat_stop >/dev/null 2>&1
  fi
  if [ -s "$CSV" ]; then
    python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" --json "$RUN_DIR/report.json" \
      "${SPREAD_ARGS[@]}" > "$RUN_DIR/report.txt" 2>&1
    rrc=$?
    cat "$RUN_DIR/report.txt" 2>/dev/null
  else
    log "no window was recorded -- no report" 2>/dev/null
  fi
  if [ -s "$RUN_DIR/cleanup-plan.txt" ]; then
    av_soak_onbox_cleanup_lines "$STRIH_HOST" "$STRIH_LX_OUT_DIR" "$OUT_DIR_WIN" "$STAMP" \
      >> "$RUN_DIR/cleanup-plan.txt" 2>/dev/null
    log "recordings kept; the exact-path removal plan: $RUN_DIR/cleanup-plan.txt" 2>/dev/null
  fi
  local stuck=""
  [ "$STRIH_REC_STARTED" = 1 ] && stuck="strih"
  [ "$STREAM_REC_STARTED" = 1 ] && stuck="${stuck:+$stuck }stream"
  if [ -n "$stuck" ]; then
    for t in $stuck; do
      echo "av-soak: ERROR: RECORDING MAY STILL BE RUNNING on $t -- ${STOP_SKIPPED_WHY:+not stopped: $STOP_SKIPPED_WHY; }${STOP_SKIPPED_WHY:-it did not stop after two StopRecords; }run: scripts/av-soak.sh --stop-leftovers $RUN_DIR" >&2 2>/dev/null
      log "ERROR: RECORDING MAY STILL BE RUNNING on $t (see $RUN_DIR/recording.state)" 2>/dev/null
    done
    if [ "$LEASE_HELD" = 1 ]; then
      log "rig lease KEPT ($RIG_LEASE_OURS) -- no E2E may start over a recording the soak may have left; --stop-leftovers releases it once nothing is left" 2>/dev/null
    elif [ -n "$LEASE_INHERITED" ]; then
      log "the caller's rig lease ($LEASE_INHERITED) stays held -- its holder releases it once nothing is left" 2>/dev/null
    fi
    exit 5
  fi
  if [ "$LEASE_HELD" = 1 ]; then
    rig_lease_release "$RIG_LEASE_OURS" >/dev/null 2>&1
    log "rig lease released" 2>/dev/null
  fi
  if [ "$LOOP_DONE" = 1 ] || [ "$STOPPED" = 1 ]; then
    exit "$rrc"
  fi
  case "$rc" in 4 | 5) ;; *) rc=5 ;; esac
  exit "$rc"
}
trap cleanup EXIT
trap 'ABORT_REASON="SIGINT"; exit 5' INT
trap 'ABORT_REASON="SIGTERM"; exit 5' TERM
trap 'ABORT_REASON="SIGHUP"; exit 5' HUP

# ---- setup ----
log "run dir $RUN_DIR; ${WINDOWS} window(s) of ${WINDOW_S} s every ${SLOT_S} s; cameras: $SOAK_CAMS"
if [ -n "$LEASE_INHERITED" ]; then
  _holder="$(rig_lease_read_holder_field run_id 2>/dev/null || true)"
  [ "$_holder" = "$LEASE_INHERITED" ] \
    || die 4 "the rig lease is not held by the caller's run id ${LEASE_INHERITED} (holder: '${_holder:-none}') -- --lease-run-id runs only under a lease its caller holds"
  rig_lease_heartbeat_touch
  log "running under the caller's rig lease ${LEASE_INHERITED} (verified; never acquired or released here)"
else
  set +e
  lease_out="$(rig_lease_acquire "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "" av-soak "$LEASE_EXPECTED_AT")"
  lease_rc=$?
  set -e
  log "$lease_out"
  [ "$lease_rc" -eq 0 ] || die 4 "the rig lease is held (${lease_out#RIG_LEASE_HELD_BY=}) -- rerun when it is free"
  LEASE_HELD=1
fi
rig_heartbeat_start av-soak || log "WARNING: could not start the rig-active heartbeat"
SETUP_STARTED=1

guard_ok "the av-soak setup reads" || refuse "the rig is busy (recording or streaming) at setup"
stream_prog="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "")"
[ "$stream_prog" = "$STREAM_DEV_SCENE" ] \
  || refuse "the stream program is '${stream_prog:-unreadable}', not '$STREAM_DEV_SCENE' (run scripts/rig-mode.sh test first)"
STRIH_PROGRAM_SNAPSHOT="$(stream_program_scene_read "$OBS_DIR" "$STRIH_HOST" "")"
[ -n "$STRIH_PROGRAM_SNAPSHOT" ] || refuse "the strih program scene is unreadable (it could not be restored after the sweep)"
log "stream program '$stream_prog' (development scene); strih program snapshot '$STRIH_PROGRAM_SNAPSHOT'"
probe="$(cam2_read "$(av_soak_painter_probe_cmd "$MARKER_LOG")" 2>/dev/null || true)"
av_soak_painter_ok "$(av_soak_kv active "$probe")" "$(av_soak_kv markers "$probe")" "$(av_soak_kv markers2 "$probe")" \
  || refuse "the cam2 painter is not emitting (${probe//$'\n'/ }) -- TEST mode must be on"

burn_state() {  # IP INPUT -> True / False / "" (unreadable)
  burn check --host "$1" --input "$2" 2>/dev/null | sed -n 's/.*burn_on=\(True\|False\).*/\1/p' | head -n 1 || true
}
BURN_TARGETS=()
for _c in $SOAK_CAMS; do BURN_TARGETS+=("$STRIH_HOST|NDI $_c"); done
BURN_TARGETS+=("$STREAM_HOST|$STREAM_PROG_SOURCE")
BURNS_OFF=()
for _t in "${BURN_TARGETS[@]}"; do
  _st="$(burn_state "${_t%%|*}" "${_t#*|}")"
  case "$_st" in
    True) log "burn already ON: ${_t%%|*} '${_t#*|}'" ;;
    False) BURNS_OFF+=("$_t") ;;
    *) refuse "burn state unreadable: ${_t%%|*} '${_t#*|}'" ;;
  esac
done

case "$(broadcast_settled)" in
  idle) ;;
  live) refuse "a broadcast is live (a box streams)" ;;
  *) refuse "the rig state is unreadable (a box did not answer rig-busy-check) -- the soak never starts on a rig it cannot observe" ;;
esac
guard_ok "the av-soak setup mutations" || refuse "the rig became busy before the setup mutations"
MUTATED=1
HOLD_ATTEMPTED=1
connect_on_show_e2e_hold "$OBS_DIR" "$STRIH_HOST" "$HOLD_STATE" || refuse "the connect-on-show hold failed"
connect_on_show_e2e_wait_live "$OBS_DIR" "$STRIH_HOST" "$HOLD_STATE"
for _t in "${BURNS_OFF[@]}"; do
  BURNS_TURNED_ON+=("$_t")
  burn add --host "${_t%%|*}" --input "${_t#*|}" >/dev/null 2>&1 || true
  [ "$(burn_state "${_t%%|*}" "${_t#*|}")" = True ] || refuse "the burn did not turn on: ${_t%%|*} '${_t#*|}'"
  log "burn turned ON: ${_t%%|*} '${_t#*|}'"
done

# ---- one slot ----
add_row() {  # K EPOCH OUTCOME [VERDICT_JSON] [RC] [RUN_ID]
  local src=(--no-verdict)
  if [ -n "${4:-}" ] && [ -s "$4" ]; then src=(--verdict-json "$4"); fi
  if [ -n "${5:-}" ]; then src+=(--verdict-rc "$5"); fi
  python3 "$DECISION" row --csv "$CSV" --cams "$SOAK_CAMS" "${src[@]}" --epoch-s "$2" --slot "$1" \
    --slot-s "$SLOT_S" --window-s "$WINDOW_S" --outcome "$3" --painter-run-id "${6:-}" \
    || log "WARNING: could not append the slot-$1 row"
}

run_slot() {
  local k="$1" sd pk probe rid seg scene label ns start_ns="" outcome=ok bounds=() rc box host fs sp tp
  local strih_path="" stream_path="" marker_win sched_win partial_win strih_argv stream_argv merge_argv
  local spid tpid mpid src trc t0 t_rec t_stop t_dec t_merge epoch sname prog
  t0="$(date +%s)"
  pk="$(printf '%03d' "$k")"
  sd="$RUN_DIR/slot-$pk"
  mkdir -p "$sd"
  [ "$(rig_lease_read_holder_field run_id)" = "$RIG_LEASE_OURS" ] || { ABORT_REASON="the rig lease is no longer ours"; exit 5; }
  rig_lease_heartbeat_touch
  for box in strih stream; do
    if [ "$box" = strih ]; then host="$STRIH_HOST"; else host="$STREAM_HOST"; fi
    fs="$(av_soak_free_space_verdict "$host" "$BUNDLE_STATE_PORT" "$RECORDINGS_FREE_MIN_GB" "$HERE")"
    if [ "${fs%% *}" = UNKNOWN ] && [[ " $FS_UNKNOWN_LOGGED " != *" $box "* ]]; then
      log "WARNING: the $box record volume free space unreadable (:${BUNDLE_STATE_PORT}/record-dir-stats.json) -- the low-disk stop cannot fire for it (logged once)"
      FS_UNKNOWN_LOGGED="$FS_UNKNOWN_LOGGED $box"
    fi
    if [ "${fs%% *}" = WARN ]; then
      ABORT_REASON="the $box record volume has only ${fs#* } GB free (< ${RECORDINGS_FREE_MIN_GB} GB)"
      log "STOP: $ABORT_REASON"
      STOPPED=1
      return 1
    fi
  done
  prog="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "")"
  if [ -z "$prog" ]; then
    log "slot $k skipped: the stream program is unreadable"
    add_row "$k" "$(date +%s)" "skipped:stream_program_unreadable"
    return 0
  fi
  if [ "$prog" != "$STREAM_DEV_SCENE" ]; then
    # the rig was handed to production (rig-mode.sh event): end the run, never keep the lease and
    # the connect-on-show hold for hours of skipped slots
    ABORT_REASON="the rig left TEST mode: the stream program is '$prog', not '$STREAM_DEV_SCENE'"
    log "STOP: $ABORT_REASON"
    STOPPED=1
    return 1
  fi
  probe="$(cam2_read "$(av_soak_painter_probe_cmd "$MARKER_LOG")" 2>/dev/null || true)"
  if [ "$(av_soak_kv active "$probe")" = inactive ]; then
    ABORT_REASON="the rig left TEST mode: the cam2 painter service is stopped"
    log "STOP: $ABORT_REASON"
    STOPPED=1
    return 1
  fi
  if ! av_soak_painter_ok "$(av_soak_kv active "$probe")" "$(av_soak_kv markers "$probe")" "$(av_soak_kv markers2 "$probe")"; then
    log "slot $k skipped: the cam2 painter is not emitting (${probe//$'\n'/ })"
    add_row "$k" "$(date +%s)" "skipped:painter_not_emitting"
    return 0
  fi
  rid="$(av_soak_kv run_id "$probe")"
  if [ -z "$rid" ]; then
    log "WARNING: the painter's run_id is not in its journal -- slot $k decodes with an unpinned cam2 (--cam2-run-id 0)"
    rid=0
  fi
  if [ "$STRIH_REC_STARTED" = 1 ] || [ "$STREAM_REC_STARTED" = 1 ]; then
    # the previous slot's StopRecord did not take: stop it only on a proven idle rig
    if [ "$(broadcast_settled)" != idle ]; then
      ABORT_REASON="the soak's own recording from the previous slot still runs and the rig is not proven idle (slot $k)"
      exit 5
    fi
  fi
  for box in strih stream; do  # the soak's OWN recording left running by the previous slot
    if [ "$box" = strih ]; then host="$STRIH_HOST"; else host="$STREAM_HOST"; fi
    if { [ "$box" = strih ] && [ "$STRIH_REC_STARTED" = 1 ]; } || { [ "$box" = stream ] && [ "$STREAM_REC_STARTED" = 1 ]; }; then
      rec_stop "$k" "$box" "$host" "$sd/record-stop.log"
    fi
  done
  if [ "$STRIH_REC_STARTED" = 1 ] || [ "$STREAM_REC_STARTED" = 1 ]; then
    ABORT_REASON="the soak's own recording would not stop (slot $k)"
    exit 5
  fi
  case "$(broadcast_settled)" in
    idle) ;;
    live) ABORT_REASON="a broadcast is live (slot $k)"; exit 5 ;;
    *)
      log "slot $k skipped: the rig state is unreadable -- no recording is started that could not be stopped again"
      add_row "$k" "$(date +%s)" "skipped:rig_state_unreadable" "" "" "$rid"
      return 0
      ;;
  esac
  connect_on_show_strih_marker set "$STRIH_HOST"
  guard_ok "the slot-$k StartRecord" || { ABORT_REASON="a broadcast is live (slot $k)"; exit 5; }
  if ! rec_start strih "$STRIH_HOST" "$sd/record-start.log" \
     || ! rec_start stream "$STREAM_HOST" "$sd/record-start.log"; then
    log "slot $k: a StartRecord failed -- stopping both boxes (see $sd/record-start.log)"
    if [ "$(broadcast_settled)" != idle ]; then
      ABORT_REASON="a StartRecord failed and the rig is not proven idle (slot $k)"
      exit 5
    fi
    [ "$STRIH_REC_STARTED" = 1 ] && rec_stop "$k" strih "$STRIH_HOST" "$sd/record-stop.log"
    [ "$STREAM_REC_STARTED" = 1 ] && rec_stop "$k" stream "$STREAM_HOST" "$sd/record-stop.log"
    add_row "$k" "$(date +%s)" "skipped:start_record_failed" "" "" "$rid"
    return 0
  fi
  t_rec="$(date +%s)"
  while IFS= read -r seg; do
    [ -n "$seg" ] || continue
    scene="${seg%%$'\t'*}"; label="${seg##*$'\t'}"
    if [ "$(broadcast_now </dev/null)" = live ]; then
      ABORT_REASON="a broadcast went live during the slot-$k sweep"
      exit 5
    fi
    STRIH_SWEPT=1
    if ! ns="$(obs switch --host "$STRIH_HOST" --program-scene "$scene" </dev/null 2>>"$sd/sweep.log" | tail -n 1)" \
        || [ -z "$ns" ]; then
      outcome="no_verdict:switch_failed_${label}"
      break
    fi
    if [ -z "$start_ns" ]; then start_ns="$ns"; else bounds+=("$ns"); fi
    isleep "$SEGMENT_S"
  done < <(python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S")
  bounds+=("$(date +%s%N)")
  if [ "$outcome" = ok ] && ! python3 "$HERE/switch_schedule.py" build --sweep "$SWEEP" --segment-secs "$SEGMENT_S" \
      --duration "$WINDOW_S" --start-ns "$start_ns" --boundaries "$(IFS=,; echo "${bounds[*]}")" \
      > "$sd/switch-schedule.json" 2>>"$sd/sweep.log"; then
    outcome="no_verdict:schedule_build_failed"
  fi
  if [ "$(broadcast_settled)" != idle ]; then
    # cleanup re-reads: a proven idle rig there stops both recordings, a live one keeps them
    ABORT_REASON="a broadcast went live (or the rig is unreadable) before the slot-$k StopRecord"
    exit 5
  fi
  rec_stop "$k" strih "$STRIH_HOST" "$sd/record-stop.log"; strih_path="$REC_PATH"
  rec_stop "$k" stream "$STREAM_HOST" "$sd/record-stop.log"; stream_path="$REC_PATH"
  t_stop="$(date +%s)"
  if [ -n "$start_ns" ]; then epoch=$(( start_ns / 1000000000 )); else epoch="$(date +%s)"; fi
  if [ "$outcome" != ok ]; then add_row "$k" "$epoch" "$outcome" "" "" "$rid"; return 0; fi
  if [ -z "$strih_path" ] || [ -z "$stream_path" ]; then
    add_row "$k" "$epoch" "no_verdict:no_recording_path" "" "" "$rid"; return 0
  fi

  cam2_read "$(av_soak_marker_snapshot_cmd "$MARKER_LOG" "$MARKER_ROWS")" > "$sd/markers.csv" 2>/dev/null || true
  if ! av_soak_marker_csv_ok "$sd/markers.csv"; then
    add_row "$k" "$epoch" "no_verdict:marker_log_unreadable" "" "" "$rid"; return 0
  fi
  marker_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-s${pk}-markers.csv")"
  sched_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-s${pk}-switch-schedule.json")"
  partial_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-s${pk}-stream-partial.json")"
  if ! win_bounded win_ssh_run "$STREAM_HOST" \
        "New-Item -ItemType Directory -Force -Path \"$OUT_DIR_WIN\" | Out-Null" > "$sd/upload.log" 2>&1 \
     || ! win_bounded win_ssh_upload "$STREAM_HOST" "$sd/markers.csv" "$marker_win" >> "$sd/upload.log" 2>&1 \
     || ! win_bounded win_ssh_upload "$STREAM_HOST" "$sd/switch-schedule.json" "$sched_win" >> "$sd/upload.log" 2>&1; then
    add_row "$k" "$epoch" "no_verdict:stream_upload_failed" "" "" "$rid"; return 0
  fi

  sname="av-soak-${STAMP}-s${pk}-strih-partial.json"
  av_soak_strih_extract_argv strih_argv "$STRIH_DECODE" "$VERDICT_BIN" "$STRIH_LX_OUT_DIR" "$sd" \
    "$strih_path" "$STRIH_CAPTURE_FPS" "$sname"
  av_soak_stream_extract_argv stream_argv "$STREAM_DECODE" "$WIN_VERDICT_EXE_LOCAL" "$OUT_DIR_WIN" "$sd" \
    "$stream_path" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "$rid" "$marker_win" "$sched_win" "$partial_win"
  STRIH_LX_BOX="$STRIH_HOST" timeout "$DECODE_TIMEOUT_S" "${strih_argv[@]}" > "$sd/strih-extract.log" 2>&1 &
  spid=$!
  STREAM_BOX="$STREAM_HOST" timeout "$DECODE_TIMEOUT_S" "${stream_argv[@]}" > "$sd/stream-extract.log" 2>&1 &
  tpid=$!
  BG_PIDS=("$spid" "$tpid")
  DECODES_RUNNING=1
  src=0; wait "$spid" || src=$?
  trc=0; wait "$tpid" || trc=$?
  BG_PIDS=()
  DECODES_RUNNING=0
  t_dec="$(date +%s)"
  if [ "$src" -eq 124 ]; then
    log "slot $k: the strih decode hit its ${DECODE_TIMEOUT_S} s bound -- stopping recording-verdict on strih-lx"
    kill_remote_decode strih
  fi
  if [ "$trc" -eq 124 ]; then
    log "slot $k: the stream decode hit its ${DECODE_TIMEOUT_S} s bound -- stopping the box's recording-verdict"
    kill_remote_decode stream
  fi
  sp="$sd/$sname"
  tp="$sd/av-soak-${STAMP}-s${pk}-stream-partial.json"
  if [ "$src" -ne 0 ] || [ "$trc" -ne 0 ] || [ ! -s "$sp" ] || [ ! -s "$tp" ]; then
    log "slot $k: decode failed (strih rc=$src, stream rc=$trc; logs in $sd)"
    printf '%s\t%s\t%s\t%s\t-\t%s\n' "$k" $(( t_rec - t0 )) $(( t_stop - t_rec )) $(( t_dec - t_stop )) \
      $(( t_dec - t0 )) >> "$RUN_DIR/timing.tsv"
    add_row "$k" "$epoch" "no_verdict:decode_failed" "" "" "$rid"; return 0
  fi
  av_soak_merge_argv merge_argv "$VERDICT_BIN" "$sp" "$tp" "$MIN_SECS" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
    "$rid" "$CAMBOX_OFFLINE_ACK" "$sd/switch-schedule.json" "$sd/pixel-proof" "$sd/verdict.json" "${AV_EXPECTED_MS:-}"
  timeout "$MERGE_TIMEOUT_S" "${merge_argv[@]}" > "$sd/merge.log" 2>&1 &
  mpid=$!
  BG_PIDS=("$mpid")
  rc=0; wait "$mpid" || rc=$?
  BG_PIDS=()
  t_merge="$(date +%s)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$k" $(( t_rec - t0 )) $(( t_stop - t_rec )) $(( t_dec - t_stop )) \
    $(( t_merge - t_dec )) $(( t_merge - t0 )) >> "$RUN_DIR/timing.tsv"
  log "slot $k timing: pre $(( t_rec - t0 )) s, record $(( t_stop - t_rec )) s, decode $(( t_dec - t_stop )) s, merge $(( t_merge - t_dec )) s, total $(( t_merge - t0 )) s of ${SLOT_S} s"
  if [ -s "$sd/verdict.json" ]; then
    add_row "$k" "$epoch" ok "$sd/verdict.json" "$rc" "$rid"
  else
    add_row "$k" "$epoch" "no_verdict:merge_failed" "" "$rc" "$rid"
  fi
  return 0
}

wait_until() {  # TARGET_EPOCH -> 0 when reached, 1 when STOP was requested; a broadcast = exit 5
  local target="$1" now left=0 last_check=0
  while :; do
    if [ -e "$RUN_DIR/STOP" ]; then
      ABORT_REASON="the STOP file"
      log "STOP file found -- ending the run"
      STOPPED=1
      return 1
    fi
    now="$(date +%s)"
    if [ $(( now - last_check )) -ge 60 ]; then
      last_check="$now"
      if [ "$(broadcast_now)" = live ]; then
        ABORT_REASON="a broadcast went live between slots"
        exit 5
      fi
    fi
    left=$(( target - now ))
    [ "$left" -gt 0 ] || break
    rig_lease_heartbeat_touch
    isleep $(( left < 10 ? left : 10 ))
  done
  if [ "$left" -lt -60 ]; then log "slot starts $(( -left )) s late (the previous slot overran)"; fi
  return 0
}

printf 'slot\tpre_s\trecord_s\tdecode_s\tmerge_s\ttotal_s\n' > "$RUN_DIR/timing.tsv"
T0="$(date +%s)"
PARTIAL_PRINTED=0
for ((k = 0; k < WINDOWS; k++)); do
  wait_until $(( T0 + k * SLOT_S )) || break
  log "slot $k/$(( WINDOWS - 1 ))"
  run_slot "$k" || break
  if [ -s "$CSV" ]; then
    python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" "${SPREAD_ARGS[@]}" \
      > "$RUN_DIR/report-latest.txt" 2>&1 || true
    if [ "$PARTIAL_PRINTED" = 0 ] && grep -q '^AV-SOAK PARTIAL' "$RUN_DIR/report-latest.txt"; then
      log "the first hour is complete -- the 1 h partial:"
      sed -n '/^AV-SOAK PARTIAL/,/^$/p' "$RUN_DIR/report-latest.txt"
      PARTIAL_PRINTED=1
    fi
  fi
done
[ "$STOPPED" = 1 ] || LOOP_DONE=1
exit 0
