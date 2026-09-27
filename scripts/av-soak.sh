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
#     - the shared issue-1271 rig-busy guard (stray_session_check_assert) before the first mutation
#     - read-only: the stream program must ALREADY be the development scene (`Development`, issue
#       1380 -- TEST mode is a precondition, `scripts/rig-mode.sh test`); the soak never selects a
#       stream scene and never names the production scene. Snapshot the strih program scene.
#     - read-only: the permanent cam2 painter is active and its QPSK marker log grows
#     - the measurement burns ON (obs_burn_filter.py add + check, the E2E pre-record gate's own
#       calls) on every soak camera's strih input and the stream program input; only the ones that
#       were OFF are turned back OFF at cleanup
#   every slot (default every 600 s, on a fixed grid from the first slot):
#     - the lease still ours, both record volumes above RECORDINGS_FREE_MIN_GB, the painter live,
#       the rig-busy guard again (a broadcast that started mid-run aborts the soak)
#     - StartRecord strih + stream (obs_phase2.py record), ONE sweep that cuts each soak camera into
#       strih program for AV_SOAK_SEGMENT_SECS (obs_phase2.py switch, switch_schedule.py plan/build
#       -- the E2E all-cambox sweep), StopRecord
#     - a tail of the painter's marker log (read-only ssh), pushed with the schedule to the stream
#       box; the strih recording decoded IN PLACE on strih-lx (recording-verdict-on-strih-lx.sh),
#       the stream recording IN PLACE on the stream box (recording-verdict-on-stream.sh --execute),
#       both bounded + in parallel; the dev1 merge (recording-verdict --merge-partials)
#     - ONE CSV row (av_soak_decision.py row); the recordings are NOT deleted (deletion is an
#       owner-only step): their exact paths go to recordings.tsv + a cleanup-plan.txt printed with
#       the E2E's own plan lines
#   cleanup, on EVERY exit: StopRecord what we started, restore the strih program scene, turn off
#   the burns we turned on, stop the heartbeat, release the lease, write + print the final report
#   (av_soak_decision.py report).
#
# MODES:
#   --plan  (DEFAULT) print every step with the exact commands; touches NOTHING (no lease, no ssh,
#           no OBS, no curl). Needs no credential and no binary.
#   --run   the soak. Needs CAM_PW (cam2 root, read-only use), STREAM_USER + STREAM_PW (the stream box
#           ssh, the same values recording-e2e.sh uses), PROBE_BIN_DIR with the CI-built Linux
#           recording-verdict, WIN_VERDICT_EXE_LOCAL = the CI-built recording-verdict.exe.
#   --report RUN_DIR  re-grade an existing (or still running) run's CSV and exit with its verdict.
#
# OPTIONS (env equivalent in brackets):
#   --hours H          [AV_SOAK_HOURS, 8]        run length; the last window starts at H
#   --slot-secs S      [AV_SOAK_SLOT_SECS, 600]  one window per slot (the acceptance: >= 1 per 10 min)
#   --segment-secs S   [AV_SOAK_SEGMENT_SECS, 30] seconds each camera is on strih program per window
#   --run-dir D        [AV_SOAK_RUN_DIR, ~/.camera-box/av-soak/<UTC stamp>]
#   --probe-bin-dir D  [PROBE_BIN_DIR]           dir holding the Linux recording-verdict
#   --win-verdict-exe P [WIN_VERDICT_EXE_LOCAL]  the Windows recording-verdict.exe
#   --spread-columns C [AV_SOAK_SPREAD_COLUMNS]  graded spread columns (see av_soak_decision.py)
# Other env: AV_SOAK_CAMS (default CAMERA_ACTIVE_SET minus CAMBOX_OFFLINE_ACK/rig-fleet.txt),
#   STRIH_HOST / STREAM_HOST (default from scripts/lib/obs-fleet.sh), PAINTER_IP (default cam2 from
#   scripts/camera-set.sh), STRIH_CAPTURE_FPS / STREAM_CAPTURE_FPS (30, as recording-e2e.sh),
#   STREAM_PROG_SOURCE ("NDI 2ME PGM", as recording-e2e.sh / rig-mode.sh), AV_SOAK_DECODE_TIMEOUT_S
#   (480), AV_SOAK_MERGE_TIMEOUT_S (300), AV_SOAK_OBS_TIMEOUT_S (30), RECORDINGS_FREE_MIN_GB (50).
#   Test seams: AV_SOAK_OBS_DIR (dir of obs_phase2.py + obs_burn_filter.py), AV_SOAK_STRIH_DECODE,
#   AV_SOAK_STREAM_DECODE, AV_SOAK_MIN_SEGMENT_SECS (10), RIG_LEASE_DIR, CAMERA_BOX_RIG_HEARTBEAT.
#
# STOP: `touch <run-dir>/STOP` (stops at the next wait/slot boundary, full cleanup + report), or
# `kill -TERM $(cat <run-dir>/pid)` (cleanup runs from the trap).
#
# EXIT: 0 PASS / 1 FAIL / 2 UNKNOWN (the final report of a run that ended normally or by STOP),
#       3 usage error, 4 refused before any rig change (lease held, rig busy, not in TEST mode,
#       painter dead, burn unreadable, missing credential/binary), 5 aborted mid-run (lease lost,
#       broadcast started, a setup mutation failed); the report is still written on 5.
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
# shellcheck source=scripts/lib/av-soak.sh
. "$HERE/lib/av-soak.sh"

usage() {
  sed -n '2,/^# Runbook/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

MODE=plan
REPORT_DIR=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan) MODE=plan ;;
    --run) MODE=run ;;
    --report) MODE=report; REPORT_DIR="${2:-}"; shift ;;
    --hours) AV_SOAK_HOURS="${2:-}"; shift ;;
    --slot-secs) AV_SOAK_SLOT_SECS="${2:-}"; shift ;;
    --segment-secs) AV_SOAK_SEGMENT_SECS="${2:-}"; shift ;;
    --run-dir) AV_SOAK_RUN_DIR="${2:-}"; shift ;;
    --probe-bin-dir) PROBE_BIN_DIR="${2:-}"; shift ;;
    --win-verdict-exe) WIN_VERDICT_EXE_LOCAL="${2:-}"; shift ;;
    --spread-columns) AV_SOAK_SPREAD_COLUMNS="${2:-}"; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "av-soak: unknown argument '$1' (try --help)" >&2; exit 3 ;;
  esac
  shift
done

log() { printf '%s [av-soak] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { echo "av-soak: ERROR: $2" >&2; exit "$1"; }

DECISION="$HERE/av_soak_decision.py"
HOURS="${AV_SOAK_HOURS:-8}"
SLOT_S="${AV_SOAK_SLOT_SECS:-600}"
SEGMENT_S="${AV_SOAK_SEGMENT_SECS:-30}"
MIN_SEGMENT_S="${AV_SOAK_MIN_SEGMENT_SECS:-10}"
SPREAD_COLUMNS="${AV_SOAK_SPREAD_COLUMNS:-}"
SPREAD_ARGS=()
if [ -n "$SPREAD_COLUMNS" ]; then SPREAD_ARGS=(--spread-columns "$SPREAD_COLUMNS"); fi

case "$HOURS" in '' | *[!0-9.]* | *.*.*) die 3 "--hours must be a non-negative number, got '$HOURS'" ;; esac
DURATION_S="$(awk -v h="$HOURS" 'BEGIN { printf "%d", h * 3600 + 0.5 }')"

if [ "$MODE" = report ]; then
  [ -n "$REPORT_DIR" ] && [ -f "$REPORT_DIR/soak.csv" ] || die 3 "--report needs a run dir holding soak.csv (got '${REPORT_DIR}')"
  rc=0
  python3 "$DECISION" report --csv "$REPORT_DIR/soak.csv" --min-duration-h "$HOURS" \
    --json "$REPORT_DIR/report-latest.json" "${SPREAD_ARGS[@]}" || rc=$?
  exit "$rc"
fi

case "$SLOT_S" in '' | *[!0-9]*) die 3 "--slot-secs must be an integer, got '$SLOT_S'" ;; esac
case "$SEGMENT_S" in '' | *[!0-9]*) die 3 "--segment-secs must be an integer, got '$SEGMENT_S'" ;; esac
[ "$SEGMENT_S" -ge 1 ] && [ "$SEGMENT_S" -ge "$MIN_SEGMENT_S" ] || die 3 "--segment-secs $SEGMENT_S is below $MIN_SEGMENT_S s (each camera needs enough QPSK markers for a measured A/V offset)"

OBS_DIR="${AV_SOAK_OBS_DIR:-$HERE}"
STRIH_HOST="${STRIH_HOST:-$(obs_fleet_host strih-lx)}"
STREAM_HOST="${STREAM_HOST:-$(obs_fleet_host stream)}"
camera_resolve cam2
PAINTER_IP="${PAINTER_IP:-$CAMERA_IP}"
MARKER_LOG="${AV_SOAK_MARKER_LOG:-/run/rig-qpsk-markers.csv}"
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}"
STREAM_DEV_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}"
STRIH_CAPTURE_FPS="${STRIH_CAPTURE_FPS:-30}"
STREAM_CAPTURE_FPS="${STREAM_CAPTURE_FPS:-30}"
DECODE_TIMEOUT_S="${AV_SOAK_DECODE_TIMEOUT_S:-480}"
MERGE_TIMEOUT_S="${AV_SOAK_MERGE_TIMEOUT_S:-300}"
OBS_TIMEOUT_S="${AV_SOAK_OBS_TIMEOUT_S:-30}"
SSH_TIMEOUT_S="${AV_SOAK_SSH_TIMEOUT_S:-60}"
RECORDINGS_FREE_MIN_GB="${RECORDINGS_FREE_MIN_GB:-50}"
BUNDLE_STATE_PORT="${WIN_BUNDLE_STATE_PORT:-8899}"
STRIH_DECODE="${AV_SOAK_STRIH_DECODE:-$HERE/recording-verdict-on-strih-lx.sh}"
STREAM_DECODE="${AV_SOAK_STREAM_DECODE:-$HERE/recording-verdict-on-stream.sh}"
# relative to the strih-lx login home: the same dir recording-verdict-on-strih-lx.sh defaults to
STRIH_LX_OUT_DIR="${AV_SOAK_STRIH_LX_OUT_DIR:-verdict-out}"
OUT_DIR_WIN="${OUT_DIR_WIN:-C:\\camera-box\\verdict-out}"
CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "${RIG_FLEET_ACK_FILE:-$HERE/../rig-fleet.txt}")"
export CAMBOX_OFFLINE_ACK
SOAK_CAMS="${AV_SOAK_CAMS:-$(av_soak_unacked_cams "$CAMERA_ACTIVE_SET")}"
for _c in $SOAK_CAMS; do
  camera_resolve "$_c" >/dev/null || die 3 "unknown camera '$_c' in the soak set"
done
[ -n "$SOAK_CAMS" ] || die 3 "no camera to soak (CAMERA_ACTIVE_SET minus the acked ones is empty)"
SWEEP="$(CAMERA_ACTIVE_SET="$SOAK_CAMS" camera_active_sweep_pairs)"
read -r -a _SOAK_CAM_ARR <<< "$SOAK_CAMS"
N_CAMS="${#_SOAK_CAM_ARR[@]}"
WINDOW_S=$(( N_CAMS * SEGMENT_S ))
MIN_SECS="$(av_soak_min_secs "$WINDOW_S")"
WINDOWS="$(av_soak_windows_count "$DURATION_S" "$SLOT_S")"
MARKER_ROWS="$(av_soak_marker_rows "$WINDOW_S")"
[ $(( WINDOW_S + 60 )) -lt "$SLOT_S" ] || die 3 "a window (${N_CAMS} cameras x ${SEGMENT_S} s = ${WINDOW_S} s) + 60 s does not fit the ${SLOT_S} s slot"
[ "$STREAM_DEV_SCENE" != "$STREAM_PRODUCTION_SCENE_DEFAULT" ] || die 3 "STREAM_PROG_SCENE names the production scene; the soak only runs on the development scene (issue 1380)"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="${AV_SOAK_RUN_DIR:-$HOME/.camera-box/av-soak/$STAMP}"
CSV="$RUN_DIR/soak.csv"
RIG_LEASE_REPO_NAME="${AV_SOAK_LEASE_REPO:-camera-box-av-soak}"
RIG_LEASE_OURS="av-soak-${STAMP}-$$"
LEASE_EXPECTED_AT="$(date -u -d "+$(( DURATION_S + SLOT_S + 1800 )) seconds" +%Y-%m-%dT%H:%M:%SZ)"
VERDICT_BIN="${PROBE_BIN_DIR:+$PROBE_BIN_DIR/recording-verdict}"
STRIH_PARTIAL_NAME="av-soak-${STAMP}-sNNN-strih-partial.json"

# --- --plan: print every step, touch nothing ------------------------------------------------------

plan_cmd() { printf '      '; printf '%q ' "$@"; printf '\n'; }

print_plan() {
  local strih_argv stream_argv merge_argv marker_win sched_win partial_win bounds_line
  bounds_line="$(python3 "$DECISION" bounds 2>&1)" || bounds_line="UNREADABLE: $bounds_line"
  cat <<EOF
===== av-soak PLAN (issue 1367) -- nothing below is executed; run with --run =====
bounds (read from their single sources): ${bounds_line//$'\n'/; }
run: ${HOURS} h = ${DURATION_S} s, one window every ${SLOT_S} s -> ${WINDOWS} window(s) at +0 s .. +$(( (WINDOWS - 1) * SLOT_S )) s
cameras: ${SOAK_CAMS}   (CAMERA_ACTIVE_SET='${CAMERA_ACTIVE_SET}', acked offline: '${CAMBOX_OFFLINE_ACK:-none}')
window: ONE sweep '${SWEEP}' = ${N_CAMS} x ${SEGMENT_S} s = ${WINDOW_S} s (merge --min-secs ${MIN_SECS})
boxes: strih ${STRIH_HOST} ($(strih_platform "$STRIH_HOST")), stream ${STREAM_HOST}, cam2 painter ${PAINTER_IP} (marker log ${MARKER_LOG})
run dir: ${RUN_DIR}  (soak.csv, report-latest.txt, report.json, recordings.tsv, cleanup-plan.txt, pid, STOP)
EOF
  if [ "$(strih_platform "$STRIH_HOST")" != linux ]; then
    echo "NOTE: --run refuses a Windows strih (retired at M4); only the strih-lx in-place decode is supported."
  fi
  echo
  echo "SETUP (once):"
  echo "  1. rig lease (issue 830): rig_lease_acquire repo=${RIG_LEASE_REPO_NAME} run_id=${RIG_LEASE_OURS} job=av-soak expected_release_at=${LEASE_EXPECTED_AT}"
  echo "     a live foreign holder -> refuse (exit 4); rig_heartbeat_start av-soak (issue 281)"
  echo "  2. rig-busy guard (issue 1271, read-only): stray_session_check_assert ${OBS_DIR} ${STRIH_HOST} ${STREAM_HOST} 'the av-soak setup'"
  echo "  3. read-only: stream program must be '${STREAM_DEV_SCENE}' (else refuse: run scripts/rig-mode.sh test first)"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STREAM_HOST"
  echo "     read-only: snapshot the strih program scene (restored at cleanup)"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STRIH_HOST"
  echo "  4. read-only: the permanent cam2 painter is active + its marker log grows (ssh root@${PAINTER_IP}):"
  echo "      $(av_soak_painter_probe_cmd "$MARKER_LOG")"
  echo "  5. measurement burns ON (check; add only when off; re-check; only the ones turned on are turned off at cleanup):"
  local c
  for c in $SOAK_CAMS; do
    plan_cmd python3 "$OBS_DIR/obs_burn_filter.py" check --host "$STRIH_HOST" --input "NDI $c"
  done
  plan_cmd python3 "$OBS_DIR/obs_burn_filter.py" check --host "$STREAM_HOST" --input "$STREAM_PROG_SOURCE"
  echo "      (when burn_on=False: the rig-busy guard, then obs_burn_filter.py add --host <ip> --input <input>, then check again)"
  echo
  echo "EVERY SLOT k (slot start = run start + k x ${SLOT_S} s; files under ${RUN_DIR}/slot-NNN):"
  echo "  a. lease still ours; record volumes free >= ${RECORDINGS_FREE_MIN_GB} GB (curl http://<box>:${BUNDLE_STATE_PORT}/record-dir-stats.json -> bundle_state_gather.recordings_free_verdict; below = stop the soak)"
  echo "  b. painter probe (as setup 4); not emitting -> the slot is recorded as skipped:painter_not_emitting"
  echo "  c. rig-busy guard: stray_session_check_assert ... 'the slot-k StartRecord' (a live broadcast aborts the soak, exit 5)"
  echo "  d. StartRecord:"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action start
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action start
  echo "  e. the sweep -- strih program only (the stream program is never switched):"
  plan_cmd python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S"
  local seg scene label
  while IFS= read -r seg; do
    [ -n "$seg" ] || continue
    scene="${seg%%$'\t'*}"; label="${seg##*$'\t'}"
    echo "      [$label] $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" switch --host "$STRIH_HOST" --program-scene "$scene"); sleep ${SEGMENT_S}"
  done < <(python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S")
  plan_cmd python3 "$HERE/switch_schedule.py" build --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S" --start-ns "<first switch ns>" --boundaries "<later switches + stop ns>"
  echo "  f. StopRecord (prints each box's recording path -> recordings.tsv + cleanup-plan.txt):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action stop
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action stop
  echo "  g. read-only marker-log tail from cam2 (${MARKER_ROWS} rows): $(av_soak_marker_snapshot_cmd "$MARKER_LOG" "$MARKER_ROWS")"
  marker_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-markers.csv")"
  sched_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-switch-schedule.json")"
  partial_win="$(av_soak_win_join "$OUT_DIR_WIN" "av-soak-${STAMP}-sNNN-stream-partial.json")"
  echo "     push to the stream box: win_ssh_upload markers -> ${marker_win}; schedule -> ${sched_win}"
  echo "  h. decode in place, both in parallel, each bounded by ${DECODE_TIMEOUT_S} s:"
  av_soak_strih_extract_argv strih_argv "$STRIH_DECODE" "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
    "$STRIH_LX_OUT_DIR" "$RUN_DIR/slot-NNN" "<strih StopRecord path>" "$STRIH_CAPTURE_FPS" "$STRIH_PARTIAL_NAME"
  plan_cmd env STRIH_LX_BOX="$STRIH_HOST" "${strih_argv[@]}"
  av_soak_stream_extract_argv stream_argv "$STREAM_DECODE" "${WIN_VERDICT_EXE_LOCAL:-<WIN_VERDICT_EXE_LOCAL>}" \
    "$OUT_DIR_WIN" "$RUN_DIR/slot-NNN" "<stream StopRecord path>" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
    "<painter run_id>" "$marker_win" "$sched_win" "$partial_win"
  plan_cmd env STREAM_BOX="$STREAM_HOST" "${stream_argv[@]}"
  echo "  i. merge on dev1 (bounded by ${MERGE_TIMEOUT_S} s; its exit code is not the soak verdict):"
  av_soak_merge_argv merge_argv "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
    "$RUN_DIR/slot-NNN/$STRIH_PARTIAL_NAME" "$RUN_DIR/slot-NNN/av-soak-${STAMP}-sNNN-stream-partial.json" \
    "$MIN_SECS" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "<painter run_id>" "$CAMBOX_OFFLINE_ACK" \
    "$RUN_DIR/slot-NNN/switch-schedule.json" "$RUN_DIR/slot-NNN/pixel-proof" "$RUN_DIR/slot-NNN/verdict.json" \
    "${AV_EXPECTED_MS:-}"
  plan_cmd "${merge_argv[@]}"
  echo "  j. one CSV row + the progress report:"
  plan_cmd python3 "$DECISION" row --csv "$CSV" --cams "$SOAK_CAMS" --verdict-json "$RUN_DIR/slot-NNN/verdict.json" --epoch-s "<window start>" --slot "<k>" --window-s "$WINDOW_S"
  plan_cmd python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" "${SPREAD_ARGS[@]}"
  echo "  k. wait for the next slot start (lease heartbeat every <= 10 s; <run-dir>/STOP ends the run)"
  echo
  echo "CLEANUP (every exit): StopRecord what this run started; switch strih program back to the snapshot; obs_burn_filter.py remove"
  echo "  on the burns this run turned on; rig_heartbeat_stop; rig_lease_release ${RIG_LEASE_OURS}; the final report:"
  plan_cmd python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" --json "$RUN_DIR/report.json" "${SPREAD_ARGS[@]}"
  echo "  recordings are NOT deleted (owner-only): ${RUN_DIR}/cleanup-plan.txt lists the exact-path removal lines."
  echo
  echo "NEVER: a latency pin, an audio sync offset, a measurement pin, an NDI mapping, a stream scene switch."
  local missing=()
  [ -n "${CAM_PW:-}" ] || missing+=(CAM_PW)
  [ -n "${STREAM_USER:-}" ] || missing+=(STREAM_USER)
  [ -n "${STREAM_PW:-}" ] || missing+=(STREAM_PW)
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
for _v in CAM_PW STREAM_USER STREAM_PW; do
  [ -n "${!_v:-}" ] || die 4 "$_v is not set (the same value recording-e2e.sh uses; see targets.md)"
done
export STREAM_USER STREAM_PW
[ -n "${VERDICT_BIN:-}" ] && [ -x "$VERDICT_BIN" ] || die 4 "PROBE_BIN_DIR must hold the CI-built Linux recording-verdict (probe-tools-linux-amd64)"
[ -n "${WIN_VERDICT_EXE_LOCAL:-}" ] && [ -f "$WIN_VERDICT_EXE_LOCAL" ] || die 4 "WIN_VERDICT_EXE_LOCAL must be the CI-built recording-verdict.exe (probe-tools-windows-amd64)"
for _t in sshpass curl timeout python3; do
  command -v "$_t" >/dev/null 2>&1 || die 4 "'$_t' not found on PATH"
done

mkdir -p "$RUN_DIR"
printf '%s\n' "$$" > "$RUN_DIR/pid"
rm -f "$RUN_DIR/STOP"

SETUP_STARTED=0
LEASE_HELD=0
LOOP_DONE=0
STOPPED=0
STRIH_REC_STARTED=0
STREAM_REC_STARTED=0
STRIH_PROGRAM_SNAPSHOT=""
LAST_STRIH_SCENE=""
BURNS_TURNED_ON=()
DECODE_PIDS=()
ABORT_REASON=""

obs() { timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_phase2.py" "$@"; }
burn() { timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_burn_filter.py" "$@"; }
cam2_read() {
  timeout "$SSH_TIMEOUT_S" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR -o ConnectTimeout=10 "root@$PAINTER_IP" "$1"
}
# win_ssh_* (scripts/lib/win-ssh-exec.sh) run in a child bash so `timeout` bounds the whole group.
win_bounded() {
  timeout "$SSH_TIMEOUT_S" bash -c '. "$1"; shift; "$@"' _ "$HERE/lib/win-ssh-exec.sh" "$@"
}
guard_ok() {
  ( stray_session_check_assert "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$1" )
}

cleanup() {
  local rc=$? rrc=2 t pid
  trap - EXIT INT TERM HUP
  set +e
  for pid in "${DECODE_PIDS[@]}"; do kill -TERM "$pid" 2>/dev/null; done
  if [ "$SETUP_STARTED" = 1 ]; then
    log "cleanup${ABORT_REASON:+ (aborted: $ABORT_REASON)}"
    [ "$STRIH_REC_STARTED" = 1 ] && { obs record --host "$STRIH_HOST" --action stop >/dev/null 2>&1 || log "WARNING: strih StopRecord failed"; }
    [ "$STREAM_REC_STARTED" = 1 ] && { obs record --host "$STREAM_HOST" --action stop >/dev/null 2>&1 || log "WARNING: stream StopRecord failed"; }
    if [ -n "$STRIH_PROGRAM_SNAPSHOT" ] && [ -n "$LAST_STRIH_SCENE" ] && [ "$LAST_STRIH_SCENE" != "$STRIH_PROGRAM_SNAPSHOT" ]; then
      obs switch --host "$STRIH_HOST" --program-scene "$STRIH_PROGRAM_SNAPSHOT" >/dev/null 2>&1 \
        && log "strih program restored to '$STRIH_PROGRAM_SNAPSHOT'" \
        || log "WARNING: could not restore the strih program to '$STRIH_PROGRAM_SNAPSHOT'"
    fi
    for t in "${BURNS_TURNED_ON[@]}"; do
      burn remove --host "${t%%|*}" --input "${t#*|}" >/dev/null 2>&1 \
        && log "burn OFF again on ${t%%|*} '${t#*|}' (it was off before the soak)" \
        || log "WARNING: could not turn the burn back off on ${t%%|*} '${t#*|}'"
    done
    rig_heartbeat_stop >/dev/null 2>&1
  fi
  if [ "$LEASE_HELD" = 1 ]; then
    rig_lease_release "$RIG_LEASE_OURS" >/dev/null 2>&1
    log "rig lease released"
  fi
  if [ -s "$CSV" ]; then
    python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" --json "$RUN_DIR/report.json" \
      "${SPREAD_ARGS[@]}" > "$RUN_DIR/report.txt" 2>&1
    rrc=$?
    cat "$RUN_DIR/report.txt"
  else
    log "no window was recorded -- no report"
  fi
  [ -s "$RUN_DIR/cleanup-plan.txt" ] && log "recordings kept; the exact-path removal plan: $RUN_DIR/cleanup-plan.txt"
  if [ "$LOOP_DONE" = 1 ] || [ "$STOPPED" = 1 ]; then
    exit "$rrc"
  fi
  [ "$rc" -eq 0 ] && rc=5
  exit "$rc"
}
trap cleanup EXIT
trap 'ABORT_REASON="SIGINT"; exit 5' INT
trap 'ABORT_REASON="SIGTERM"; exit 5' TERM
trap 'ABORT_REASON="SIGHUP"; exit 5' HUP

# ---- setup ----
log "run dir $RUN_DIR; ${WINDOWS} window(s) of ${WINDOW_S} s every ${SLOT_S} s; cameras: $SOAK_CAMS"
set +e
lease_out="$(rig_lease_acquire "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "" av-soak "$LEASE_EXPECTED_AT")"
lease_rc=$?
set -e
log "$lease_out"
[ "$lease_rc" -eq 0 ] || die 4 "the rig lease is held (${lease_out#RIG_LEASE_HELD_BY=}) -- rerun when it is free"
LEASE_HELD=1
rig_heartbeat_start av-soak || log "WARNING: could not start the rig-active heartbeat"
SETUP_STARTED=1

guard_ok "the av-soak setup" || { ABORT_REASON="rig busy at setup"; exit 4; }
stream_prog="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "")"
if [ "$stream_prog" != "$STREAM_DEV_SCENE" ]; then
  ABORT_REASON="stream program is '${stream_prog:-unreadable}', not '$STREAM_DEV_SCENE' (run scripts/rig-mode.sh test first)"
  exit 4
fi
STRIH_PROGRAM_SNAPSHOT="$(stream_program_scene_read "$OBS_DIR" "$STRIH_HOST" "")"
log "stream program '$stream_prog' (development scene); strih program snapshot '${STRIH_PROGRAM_SNAPSHOT:-unreadable}'"

probe="$(cam2_read "$(av_soak_painter_probe_cmd "$MARKER_LOG")" 2>/dev/null || true)"
if ! av_soak_painter_ok "$(av_soak_kv active "$probe")" "$(av_soak_kv markers "$probe")" "$(av_soak_kv markers2 "$probe")"; then
  ABORT_REASON="the cam2 painter is not emitting (${probe//$'\n'/ }) -- TEST mode must be on"
  exit 4
fi

burn_state() {  # IP INPUT -> True / False / "" (unreadable)
  burn check --host "$1" --input "$2" 2>/dev/null | sed -n 's/.*burn_on=\(True\|False\).*/\1/p' | head -n 1 || true
}
BURN_TARGETS=()
for _c in $SOAK_CAMS; do BURN_TARGETS+=("$STRIH_HOST|NDI $_c"); done
BURN_TARGETS+=("$STREAM_HOST|$STREAM_PROG_SOURCE")
guarded_burn=0
for _t in "${BURN_TARGETS[@]}"; do
  _ip="${_t%%|*}"; _in="${_t#*|}"
  _st="$(burn_state "$_ip" "$_in")"
  case "$_st" in
    True) log "burn already ON: $_ip '$_in'" ;;
    False)
      if [ "$guarded_burn" = 0 ]; then
        guard_ok "the av-soak burn-on" || { ABORT_REASON="rig busy before the burn-on"; exit 4; }
        guarded_burn=1
      fi
      burn add --host "$_ip" --input "$_in" >/dev/null 2>&1 || true
      BURNS_TURNED_ON+=("$_t")
      [ "$(burn_state "$_ip" "$_in")" = True ] || { ABORT_REASON="the burn did not turn on: $_ip '$_in'"; exit 5; }
      log "burn turned ON: $_ip '$_in'"
      ;;
    *) ABORT_REASON="burn state unreadable: $_ip '$_in'"; exit 4 ;;
  esac
done

# ---- one slot ----
add_row() {  # K EPOCH OUTCOME [VERDICT_JSON] [RC] [RUN_ID]
  local src=(--no-verdict)
  if [ -n "${4:-}" ] && [ -s "$4" ]; then src=(--verdict-json "$4"); fi
  if [ -n "${5:-}" ]; then src+=(--verdict-rc "$5"); fi
  python3 "$DECISION" row --csv "$CSV" --cams "$SOAK_CAMS" "${src[@]}" --epoch-s "$2" --slot "$1" \
    --window-s "$WINDOW_S" --outcome "$3" --painter-run-id "${6:-}" \
    || log "WARNING: could not append the slot-$1 row"
}

run_slot() {
  local k="$1" sd pk probe rid seg scene label ns start_ns="" outcome=ok bounds=() rc
  local strih_path="" stream_path="" marker_win sched_win partial_win strih_argv stream_argv merge_argv
  local spid tpid src trc
  pk="$(printf '%03d' "$k")"
  sd="$RUN_DIR/slot-$pk"
  mkdir -p "$sd"
  [ "$(rig_lease_read_holder_field run_id)" = "$RIG_LEASE_OURS" ] || { ABORT_REASON="the rig lease is no longer ours"; exit 5; }
  rig_lease_heartbeat_touch
  local box host fs
  for box in strih stream; do
    if [ "$box" = strih ]; then host="$STRIH_HOST"; else host="$STREAM_HOST"; fi
    fs="$(av_soak_free_space_verdict "$host" "$BUNDLE_STATE_PORT" "$RECORDINGS_FREE_MIN_GB" "$HERE")"
    if [ "${fs%% *}" = WARN ]; then
      log "STOP: $box record volume has only ${fs#* } GB free (< ${RECORDINGS_FREE_MIN_GB} GB)"
      STOPPED=1
      return 1
    fi
  done
  probe="$(cam2_read "$(av_soak_painter_probe_cmd "$MARKER_LOG")" 2>/dev/null || true)"
  if ! av_soak_painter_ok "$(av_soak_kv active "$probe")" "$(av_soak_kv markers "$probe")" "$(av_soak_kv markers2 "$probe")"; then
    log "slot $k skipped: the cam2 painter is not emitting (${probe//$'\n'/ })"
    add_row "$k" "$(date +%s)" "skipped:painter_not_emitting"
    return 0
  fi
  rid="$(av_soak_kv run_id "$probe")"
  rid="${rid:-0}"
  guard_ok "the slot-$k StartRecord" || { ABORT_REASON="a broadcast is live (slot $k)"; exit 5; }
  if ! obs record --host "$STRIH_HOST" --action start > "$sd/record-start.log" 2>&1; then
    add_row "$k" "$(date +%s)" "skipped:strih_start_record_failed"
    return 0
  fi
  STRIH_REC_STARTED=1
  if ! obs record --host "$STREAM_HOST" --action start >> "$sd/record-start.log" 2>&1; then
    strih_path="$(obs record --host "$STRIH_HOST" --action stop 2>/dev/null || true)"
    STRIH_REC_STARTED=0
    add_row "$k" "$(date +%s)" "skipped:stream_start_record_failed"
    return 0
  fi
  STREAM_REC_STARTED=1
  while IFS= read -r seg; do
    [ -n "$seg" ] || continue
    scene="${seg%%$'\t'*}"; label="${seg##*$'\t'}"
    if ! ns="$(obs switch --host "$STRIH_HOST" --program-scene "$scene" </dev/null 2>>"$sd/sweep.log" | tail -n 1)" \
        || [ -z "$ns" ]; then
      outcome="no_verdict:switch_failed_${label}"
      break
    fi
    LAST_STRIH_SCENE="$scene"
    if [ -z "$start_ns" ]; then start_ns="$ns"; else bounds+=("$ns"); fi
    sleep "$SEGMENT_S"
  done < <(python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S")
  bounds+=("$(date +%s%N)")
  if [ "$outcome" = ok ] && ! python3 "$HERE/switch_schedule.py" build --sweep "$SWEEP" --segment-secs "$SEGMENT_S" \
      --duration "$WINDOW_S" --start-ns "$start_ns" --boundaries "$(IFS=,; echo "${bounds[*]}")" \
      > "$sd/switch-schedule.json" 2>>"$sd/sweep.log"; then
    outcome="no_verdict:schedule_build_failed"
  fi
  strih_path="$(obs record --host "$STRIH_HOST" --action stop 2>>"$sd/record-stop.log" | tail -n 1 || true)"
  STRIH_REC_STARTED=0
  stream_path="$(obs record --host "$STREAM_HOST" --action stop 2>>"$sd/record-stop.log" | tail -n 1 || true)"
  STREAM_REC_STARTED=0
  printf '%s\tstrih\t%s\n%s\tstream\t%s\n' "$k" "$strih_path" "$k" "$stream_path" >> "$RUN_DIR/recordings.tsv"
  {
    [ -n "$strih_path" ] && strih_lx_recording_cleanup_note "" "$STRIH_HOST" "$strih_path"
    [ -n "$stream_path" ] && echo "win-stream-snv Shell: Remove-Item -Force -LiteralPath '$stream_path'"
  } >> "$RUN_DIR/cleanup-plan.txt" || true
  local epoch
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
  if ! win_bounded win_ssh_run "$STREAM_USER" "$STREAM_PW" "$STREAM_HOST" \
        "New-Item -ItemType Directory -Force -Path \"$OUT_DIR_WIN\" | Out-Null" > "$sd/upload.log" 2>&1 \
     || ! win_bounded win_ssh_upload "$STREAM_USER" "$STREAM_PW" "$STREAM_HOST" "$sd/markers.csv" "$marker_win" >> "$sd/upload.log" 2>&1 \
     || ! win_bounded win_ssh_upload "$STREAM_USER" "$STREAM_PW" "$STREAM_HOST" "$sd/switch-schedule.json" "$sched_win" >> "$sd/upload.log" 2>&1; then
    add_row "$k" "$epoch" "no_verdict:stream_upload_failed" "" "" "$rid"; return 0
  fi

  local sname="av-soak-${STAMP}-s${pk}-strih-partial.json"
  av_soak_strih_extract_argv strih_argv "$STRIH_DECODE" "$VERDICT_BIN" "$STRIH_LX_OUT_DIR" "$sd" \
    "$strih_path" "$STRIH_CAPTURE_FPS" "$sname"
  av_soak_stream_extract_argv stream_argv "$STREAM_DECODE" "$WIN_VERDICT_EXE_LOCAL" "$OUT_DIR_WIN" "$sd" \
    "$stream_path" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "$rid" "$marker_win" "$sched_win" "$partial_win"
  STRIH_LX_BOX="$STRIH_HOST" timeout "$DECODE_TIMEOUT_S" "${strih_argv[@]}" > "$sd/strih-extract.log" 2>&1 &
  spid=$!
  STREAM_BOX="$STREAM_HOST" timeout "$DECODE_TIMEOUT_S" "${stream_argv[@]}" > "$sd/stream-extract.log" 2>&1 &
  tpid=$!
  DECODE_PIDS=("$spid" "$tpid")
  src=0; wait "$spid" || src=$?
  trc=0; wait "$tpid" || trc=$?
  DECODE_PIDS=()
  local sp="$sd/$sname" tp="$sd/av-soak-${STAMP}-s${pk}-stream-partial.json"
  if [ "$src" -ne 0 ] || [ "$trc" -ne 0 ] || [ ! -s "$sp" ] || [ ! -s "$tp" ]; then
    log "slot $k: decode failed (strih rc=$src, stream rc=$trc; logs in $sd)"
    add_row "$k" "$epoch" "no_verdict:decode_failed" "" "" "$rid"; return 0
  fi
  av_soak_merge_argv merge_argv "$VERDICT_BIN" "$sp" "$tp" "$MIN_SECS" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
    "$rid" "$CAMBOX_OFFLINE_ACK" "$sd/switch-schedule.json" "$sd/pixel-proof" "$sd/verdict.json" "${AV_EXPECTED_MS:-}"
  rc=0
  timeout "$MERGE_TIMEOUT_S" "${merge_argv[@]}" > "$sd/merge.log" 2>&1 || rc=$?
  if [ -s "$sd/verdict.json" ]; then
    add_row "$k" "$epoch" ok "$sd/verdict.json" "$rc" "$rid"
  else
    add_row "$k" "$epoch" "no_verdict:merge_failed" "" "$rc" "$rid"
  fi
  return 0
}

wait_until() {  # TARGET_EPOCH -> 0 when reached, 1 when STOP was requested
  local target="$1" now left
  while :; do
    [ -e "$RUN_DIR/STOP" ] && { log "STOP file found -- ending the run"; STOPPED=1; return 1; }
    now="$(date +%s)"
    left=$(( target - now ))
    [ "$left" -gt 0 ] || break
    rig_lease_heartbeat_touch
    sleep $(( left < 10 ? left : 10 ))
  done
  [ "$left" -lt -60 ] && log "slot starts $(( -left )) s late (the previous slot overran)"
  return 0
}

T0="$(date +%s)"
PARTIAL_PRINTED=0
for ((k = 0; k < WINDOWS; k++)); do
  wait_until $(( T0 + k * SLOT_S )) || break
  log "slot $k/$(( WINDOWS - 1 ))"
  run_slot "$k" || break
  if [ -s "$CSV" ]; then
    python3 "$DECISION" report --csv "$CSV" --min-duration-h "$HOURS" "${SPREAD_ARGS[@]}" \
      > "$RUN_DIR/report-latest.txt" 2>&1 || true
    if [ "$PARTIAL_PRINTED" = 0 ] && [ $(( $(date +%s) - T0 )) -ge 3600 ]; then
      log "1 h partial:"
      cat "$RUN_DIR/report-latest.txt"
      PARTIAL_PRINTED=1
    fi
  fi
done
[ "$STOPPED" = 1 ] || LOOP_DONE=1
exit 0
