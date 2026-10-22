#!/usr/bin/env bash
# scripts/vban-receiver-bench.sh -- issue 1460 slice 1: the VBAN RECEIVER BENCH (full header below).
set -euo pipefail
#
# WHY (issue 1460, design 6338685774): the issue-1439 fix replaces the PP stream box's program path
# with lv1 VBAN -> our obs-vban receive source (a fixed reserve R, following lv1's clock) -> the
# genlock ASRC -> the OBS mix. lv1 is off at PP and the SNV stream box takes Dante, so the proof runs
# at SNV: an EMULATED lv1 on dev1 (scripts/vban_emulator.py) sends to a TEMPORARY obs-vban input on
# strih-snv OBS (the same Linux build family as stream-pp), OBS records it, and the recording is
# graded (scripts/vban_sine_continuity.py) together with the OBS log rows (scripts/vban_bench_log.py):
#   1. no pop: no lost / late slot and no buffering-guard re-anchor under lv1-like jitter;
#   2. clock following: the sender's offset is re-clocked smoothly -- the recorded pilot runs at the
#      injected ppm, the ASRC `estimated` equals it, no sample is inserted or dropped;
#   3. a lost packet is silence for exactly its slot, and the timeline continues across it.
#
# WHAT ONE RUN DOES (every rig action is an existing primitive or vban_bench_obs.py over
# obs_phase2.py's websocket client):
#   - takes the issue-830 rig lease as `camera-box-vban-bench` (declared resource strih-snv), keeps
#     it alive (issue 1383) every poll, releases it at the end;
#   - READS first, refuses (exit 4) before any change: TEST mode (the cam2 painter, rig-mode-state.sh),
#     a PROVEN idle rig (obs_phase2.py rig-busy-check busy=false, an unreadable read retried), the
#     OBS preflight (vban_bench_obs.py: the vban kind loaded, the track free of the program / stream
#     track, Advanced output, no recording, the port and the input name free), the UDP port free on
#     strih-snv and whether strih-snv has ffmpeg;
#   - the shared issue-1271 rig-busy guard before EVERY mutation (the setup, each StartRecord);
#   - setup: the temporary input in the strih program scene, routed to the dedicated track only,
#     monitoring off, the track ORed into the recording's RecTracks, active (vban_bench_obs.py);
#   - per profile (`clean`, `lv1`, `stress:+50`, `stress:-50` by default): mark the OBS log,
#     StartRecord, LEAD_S of silence, the emulator for --minutes, TAIL_S, StopRecord on a PROVEN idle
#     rig, the log rows since the mark, the test track pulled from strih-snv (ffmpeg stream copy on
#     the box, else the whole file), the sine analysis + the log grade -> one bench.tsv row;
#   - cleanup on EVERY exit (signals ignored): the emulator stopped, a recording the bench started
#     stopped only on a proven idle rig (else KEPT, exit 5, the lease kept for --cleanup), the test
#     input removed and RecTracks restored (vban_bench_obs.py teardown -- a silent input on a non-
#     program track is safe to remove even during a broadcast), the lease released.
#   The recordings stay on strih-snv (deleting them is the owner's step): exact-path lines go to
#   <run-dir>/cleanup-plan.txt.
#
# THE RIG RULE "only the QPSK marker may sound" (owner): the pilot reaches only the test input, which
# feeds only the dedicated track (never track 1 = the program / NDI / HDMI mix, never the stream
# track) with monitoring off; the emulator refuses the ports a real receiver listens on.
#
# A LATER SLICE wires this bench into the full-path E2E through a sourced helper lib (the #675
# pattern, never an edit of recording-e2e.sh's pinned anchors). Not in this slice.
#
# MODES:
#   --plan  (DEFAULT) print every step with the exact commands; touches NOTHING (no lease, no ssh,
#           no OBS, no packet).
#   --run   the bench. Needs CAM_PW (cam2 root, read-only probe), STRIH_USER + STRIH_PW (strih-snv
#           ssh: the log read, the port check, the audio pull; the same values recording-e2e.sh
#           uses). OBS_PASSWORD when the boxes' obs-websocket needs one (strih/stream run without).
#   --cleanup RUN_DIR  after a killed run: teardown from <run-dir>/obs-state.json, a recording the run
#           flagged stopped only when av_soak_rig_state.py proves it is the bench's on an idle rig,
#           then the run's lease released (holder-checked). Refuses while the run still runs.
#
# OPTIONS (env in brackets):
#   --profiles "P ..."   [VBAN_BENCH_PROFILES, "clean lv1 stress:+50 stress:-50"]  NAME[:PPM]
#   --minutes N          [VBAN_BENCH_MINUTES, 10]  per profile, 2..14 (the strih recording splits at
#                        15 min; an `asrc:` line is written once a minute)
#   --port P             [VBAN_BENCH_PORT, 6995]   never 6980 / 6981 / 6990
#   --stream-name S      [VBAN_BENCH_STREAM, bench1460]
#   --track T            [VBAN_BENCH_TRACK, 6]     2..6, never the stream track
#   --input-name N       [VBAN_BENCH_INPUT, "VBAN bench (issue 1460)"]
#   --pilot-hz F         [VBAN_BENCH_PILOT_HZ, 997]
#   --seed N             [VBAN_BENCH_SEED, 1460]
#   --ppm-bound P        [VBAN_BENCH_PPM_BOUND, 5]
#   --extra-setting K=V  (repeatable) an extra input setting, e.g. the issue-1439 receive reserve
#   --run-dir D          [VBAN_BENCH_RUN_DIR, ~/.camera-box/vban-bench/<UTC stamp>]
# Other env: STRIH_HOST / STREAM_HOST (default from scripts/lib/obs-fleet.sh), PAINTER_IP (cam2),
#   VBAN_BENCH_POLL_S (20), VBAN_BENCH_OBS_TIMEOUT_S (30), VBAN_BENCH_SSH_TIMEOUT_S (60),
#   AV_SOAK_BROADCAST_READS (3) / AV_SOAK_BROADCAST_RETRY_S (20) (the soak's retried idle proof).
#   Test seams: VBAN_BENCH_OBS_DIR (obs_phase2.py + vban_bench_obs.py), VBAN_BENCH_EMULATOR,
#   RIG_LEASE_DIR.
#
# STOP: `touch <run-dir>/STOP` (ends after the running profile's emulator, with cleanup) or SIGTERM.
#
# EXIT: 0 PASS (every profile), 1 FAIL, 3 usage error, 4 refused before any rig change, 5 aborted
#       after a change (a broadcast went live, a step failed, a signal; a recording the bench may have
#       left keeps the lease -- run --cleanup).
#
# Runbook + the rule: .claude/rules/vban-receiver-bench.md.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/strih-platform.sh
. "$HERE/lib/strih-platform.sh"
# shellcheck source=scripts/lib/strih-log-read.sh
. "$HERE/lib/strih-log-read.sh"
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh"
# shellcheck source=scripts/lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"
# shellcheck source=scripts/lib/rig-mode-state.sh
. "$HERE/lib/rig-mode-state.sh"
# shellcheck source=scripts/lib/av-soak.sh
. "$HERE/lib/av-soak.sh"

log() { printf '%s [vban-bench] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { local rc="$1"; shift; log "ERROR: $*" >&2; exit "$rc"; }
is_uint() { case "${1:-}" in '' | *[!0123456789]*) return 1 ;; *) return 0 ;; esac; }

MODE=plan
CLEANUP_DIR=""
PROFILES="${VBAN_BENCH_PROFILES:-clean lv1 stress:+50 stress:-50}"
MINUTES="${VBAN_BENCH_MINUTES:-10}"
PORT="${VBAN_BENCH_PORT:-6995}"
STREAM_NAME="${VBAN_BENCH_STREAM:-bench1460}"
TRACK="${VBAN_BENCH_TRACK:-6}"
INPUT_NAME="${VBAN_BENCH_INPUT:-VBAN bench (issue 1460)}"
PILOT_HZ="${VBAN_BENCH_PILOT_HZ:-997}"
SEED="${VBAN_BENCH_SEED:-1460}"
PPM_BOUND="${VBAN_BENCH_PPM_BOUND:-5}"
RUN_DIR="${VBAN_BENCH_RUN_DIR:-}"
EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    --plan) MODE=plan ;;
    --run) MODE=run ;;
    --cleanup) MODE=cleanup; CLEANUP_DIR="${2:-}"; shift ;;
    --profiles) PROFILES="${2:-}"; shift ;;
    --minutes) MINUTES="${2:-}"; shift ;;
    --port) PORT="${2:-}"; shift ;;
    --stream-name) STREAM_NAME="${2:-}"; shift ;;
    --track) TRACK="${2:-}"; shift ;;
    --input-name) INPUT_NAME="${2:-}"; shift ;;
    --pilot-hz) PILOT_HZ="${2:-}"; shift ;;
    --seed) SEED="${2:-}"; shift ;;
    --ppm-bound) PPM_BOUND="${2:-}"; shift ;;
    --extra-setting) EXTRA+=(--extra-setting "${2:-}"); shift ;;
    --run-dir) RUN_DIR="${2:-}"; shift ;;
    -h | --help) sed -n '2,/^# Runbook/p' "$0"; exit 0 ;;
    *) die 3 "unknown argument '$1' (see --help)" ;;
  esac
  shift
done

OBS_DIR="${VBAN_BENCH_OBS_DIR:-$HERE}"
EMULATOR="${VBAN_BENCH_EMULATOR:-$HERE/vban_emulator.py}"
ANALYZE="$HERE/vban_sine_continuity.py"
LOG_GRADE="$HERE/vban_bench_log.py"
RIG_STATE="$HERE/av_soak_rig_state.py"
STRIH_HOST="${STRIH_HOST:-$(obs_fleet_host strih-snv)}"
STREAM_HOST="${STREAM_HOST:-$(obs_fleet_host stream-snv)}"
POLL_S="${VBAN_BENCH_POLL_S:-20}"
OBS_TIMEOUT_S="${VBAN_BENCH_OBS_TIMEOUT_S:-30}"
SSH_TIMEOUT_S="${VBAN_BENCH_SSH_TIMEOUT_S:-60}"
BROADCAST_READS="${AV_SOAK_BROADCAST_READS:-3}"
BROADCAST_RETRY_S="${AV_SOAK_BROADCAST_RETRY_S:-20}"
LEAD_S=3
TAIL_S=2
ANALYSIS_LEAD_S=2
LEASE_REPO="camera-box-vban-bench"
LEASE_RESOURCES="strih-snv"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
[ -n "$RUN_DIR" ] || RUN_DIR="$HOME/.camera-box/vban-bench/$STAMP"

# --- validation (plan + run) -------------------------------------------------------------------------

PROFILES="${PROFILES//,/ }"
[ -n "${PROFILES// /}" ] || die 3 "--profiles is empty"
for _p in $PROFILES; do
  _name="${_p%%:*}"
  case "$_name" in clean | lv1 | stress) ;; *) die 3 "unknown profile '$_name' (clean, lv1, stress; NAME[:PPM])" ;; esac
  if [ "$_p" != "$_name" ]; then
    [[ "${_p#*:}" =~ ^[+-]?[0123456789]+(\.[0123456789]+)?$ ]] || die 3 "profile '$_p': the ppm must be a number"
  fi
done
is_uint "$MINUTES" && [ "$MINUTES" -ge 2 ] && [ "$MINUTES" -le 14 ] \
  || die 3 "--minutes must be 2..14 (got '$MINUTES'; the strih recording splits at 15 min)"
is_uint "$PORT" || die 3 "--port must be a number"
case "$PORT" in 6980 | 6981 | 6990) die 3 "--port $PORT belongs to a real receiver on the rig (6980 hub/stream program, 6981 strih program, 6990 Janus) -- the pilot must never reach it" ;; esac
[ "$PORT" -ge 1024 ] && [ "$PORT" -le 65535 ] || die 3 "--port must be 1024..65535"
is_uint "$TRACK" && [ "$TRACK" -ge 2 ] && [ "$TRACK" -le 6 ] || die 3 "--track must be 2..6 (track 1 is the program mix)"
stream_name_ok() { local LC_ALL=C; [[ "$1" =~ ^[[:graph:]]{1,16}$ ]]; }  # C locale: [:graph:] = ASCII only
stream_name_ok "$STREAM_NAME" || die 3 "--stream-name must be 1..16 printable ASCII characters"
is_uint "$SEED" || die 3 "--seed must be a non-negative integer"
for _x in "${EXTRA[@]}"; do
  [ "$_x" = --extra-setting ] && continue
  case "${_x%%=*}" in
    "" | "$_x") die 3 "--extra-setting '$_x' is not KEY=VALUE" ;;
    port | stream_name | ip_from) die 3 "--extra-setting may not set the bench's own '${_x%%=*}' (use --port / --stream-name)" ;;
  esac
done
for _n in "$POLL_S" "$OBS_TIMEOUT_S" "$SSH_TIMEOUT_S" "$BROADCAST_READS" "$BROADCAST_RETRY_S"; do
  is_uint "$_n" || die 3 "the VBAN_BENCH_* / AV_SOAK_* timings must be integers (got '$_n')"
done
[ -n "$STRIH_HOST" ] || die 3 "no strih-snv host (obs-fleet)"
camera_resolve cam2
PAINTER_IP="${PAINTER_IP:-$CAMERA_IP}"
N_PROFILES=0
for _p in $PROFILES; do N_PROFILES=$((N_PROFILES + 1)); done
EXPECTED_S=$(( N_PROFILES * (MINUTES * 60 + 300) + 600 ))

profile_name() { printf '%s' "${1%%:*}"; }
emulator_argv() {  # INDEX TOKEN -> EMU_ARGV: the emulator command of one profile (plan == run)
  local i="$1" tok="$2"
  EMU_ARGV=(python3 "$EMULATOR" --target "$STRIH_HOST" --port "$PORT" --stream-name "$STREAM_NAME"
    --profile "$(profile_name "$tok")")
  if [ "$tok" != "${tok%%:*}" ]; then EMU_ARGV+=(--ppm "${tok#*:}"); fi
  EMU_ARGV+=(--duration-s "$((MINUTES * 60))" --seed "$SEED" --pilot-hz "$PILOT_HZ"
    --schedule-out "$RUN_DIR/p$i-schedule.json" --stats-out "$RUN_DIR/p$i-emulator.json")
}
obs_tool() {  # OP -> vban_bench_obs.py OP with the bench's identity
  python3 "$OBS_DIR/vban_bench_obs.py" "$1" --host "$STRIH_HOST" --password "${OBS_PASSWORD:-}" \
    --state "$RUN_DIR/obs-state.json" --input-name "$INPUT_NAME" --port "$PORT" \
    --stream-name "$STREAM_NAME" --track "$TRACK" "${EXTRA[@]}"
}

# --- --plan ------------------------------------------------------------------------------------------

if [ "$MODE" = plan ]; then
  echo "VBAN RECEIVER BENCH -- PLAN (issue 1460; nothing below has run, nothing was touched)"
  echo "  strih-snv OBS ${STRIH_HOST}; stream OBS ${STREAM_HOST} (rig-busy only); cam2 ${PAINTER_IP} (TEST-mode probe)"
  echo "  test input '${INPUT_NAME}' (net.nagater.obs-vban.source) port ${PORT} stream '${STREAM_NAME}' -> track ${TRACK} ONLY"
  echo "  profiles: ${PROFILES} x ${MINUTES} min; run dir ${RUN_DIR}"
  echo "  lease: ${LEASE_REPO} (resources ${LEASE_RESOURCES}), hold ceiling ${EXPECTED_S} s"
  echo "1. reads (refuse = exit 4, nothing changed):"
  echo "   - TEST mode: ssh root@${PAINTER_IP} <rig_mode_state_probe_remote_snippet> -> must read TEST"
  echo "   - idle rig: python3 ${OBS_DIR}/obs_phase2.py rig-busy-check --strih-host ${STRIH_HOST} --stream-host ${STREAM_HOST} -> busy=false"
  echo "   - OBS: $(printf '%q ' python3 "$OBS_DIR/vban_bench_obs.py" preflight --host "$STRIH_HOST" --state "$RUN_DIR/obs-state.json" --port "$PORT" --track "$TRACK")"
  echo "   - strih-snv: ss -Hlun 'sport = :${PORT}' empty; command -v ffmpeg (on-box track extract, else the whole file is pulled)"
  _i=0
  for _p in $PROFILES; do
    _i=$((_i + 1))
    echo "2.${_i} profile ${_p} (a FRESH test input per profile: its counters and its receiver start clean):"
    echo "     the rig-busy guard, then setup (the input in the program scene, track ${TRACK} only, monitor off,"
    echo "     RecTracks |= track, active -- the program scene re-programmed once in Studio Mode):"
    echo "     $(printf '%q ' python3 "$OBS_DIR/vban_bench_obs.py" setup --host "$STRIH_HOST" --state "$RUN_DIR/obs-state.json" --input-name "$INPUT_NAME" --port "$PORT" --stream-name "$STREAM_NAME" --track "$TRACK" "${EXTRA[@]}")"
    echo "     the guard; mark the OBS log; StartRecord; ${LEAD_S} s of silence; then the emulator:"
    emulator_argv "$_i" "$_p"
    echo "     $(printf '%q ' "${EMU_ARGV[@]}")"
    echo "     (every ${POLL_S} s: the lease kept alive, a live broadcast aborts); ${TAIL_S} s tail; a proven idle"
    echo "     rig; StopRecord (verified); teardown (input removed, RecTracks restored); the log since the mark;"
    echo "     the test track pulled to $RUN_DIR/p${_i}-audio.mkv; then"
    echo "     $(printf '%q ' python3 "$ANALYZE" "$RUN_DIR/p${_i}-audio.mkv" --schedule "$RUN_DIR/p${_i}-schedule.json" --lead-s "$ANALYSIS_LEAD_S" --ppm-bound "$PPM_BOUND" --json)"
    echo "     $(printf '%q ' python3 "$LOG_GRADE" --log "$RUN_DIR/p${_i}-obs.log" --input-name "$INPUT_NAME" --ppm-bound "$PPM_BOUND" --json)"
  done
  echo "3. cleanup (always): the emulator stopped; StopRecord only on a proven idle rig; teardown; the lease released"
  echo "   $(printf '%q ' python3 "$OBS_DIR/vban_bench_obs.py" teardown --host "$STRIH_HOST" --state "$RUN_DIR/obs-state.json")"
  echo "verdict: PASS only when every profile's sine analysis AND log grade pass"
  exit 0
fi

# --- shared --run / --cleanup helpers ----------------------------------------------------------------

remote() {  # USER PW HOST CMD -> run CMD on HOST (password via the environment, never argv)
  SSHPASS="$2" timeout "$SSH_TIMEOUT_S" sshpass -e ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 "$1@$3" "$4"
}
broadcast_now() {  # -> live | unknown | idle (the soak's retried read + pure decision)
  av_soak_broadcast_of "$RIG_STATE" "$(av_soak_rig_busy_settled "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" \
    "$OBS_TIMEOUT_S" "$RIG_STATE" "$BROADCAST_READS" "$BROADCAST_RETRY_S" "${IN_CLEANUP:-0}")"
}
record() {  # ACTION -> obs_phase2.py record on strih-snv, bounded
  timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" \
    --password "${OBS_PASSWORD:-}" --action "$1"
}
write_recording_state() {  # FLAG [SINCE]: the av_soak_rig_state.py recording.state shape
  printf 'strih=%s\nstrih_since=%s\nstream=0\nlease=%s\nstart_window_s=%s\n' \
    "$1" "${2:-}" "${LEASE_ID:-}" "$((OBS_TIMEOUT_S + 30))" > "$RUN_DIR/recording.state"
}
stop_recording_if_idle() {  # -> 0 stopped and verified, 1 kept (named in the log)
  local b st
  b="$(broadcast_now)"
  if [ "$b" != idle ]; then
    log "NOT stopping the strih recording: the rig is '$b' (a broadcast may own it)"
    return 1
  fi
  record stop >> "$RUN_DIR/recordings.txt" 2>> "$RUN_DIR/bench.log" || true
  st="$(record status 2>/dev/null || true)"
  case "$st" in
    active=False*) write_recording_state 0; return 0 ;;
  esac
  log "the strih recording still reads '${st:-unreadable}' after StopRecord"
  return 1
}

if [ "$MODE" = cleanup ]; then
  [ -d "$CLEANUP_DIR" ] || die 3 "--cleanup needs a bench run dir (got '$CLEANUP_DIR')"
  RUN_DIR="$CLEANUP_DIR"
  _pid="$(head -n 1 "$RUN_DIR/pid" 2>/dev/null || true)"
  if is_uint "$_pid" && kill -0 "$_pid" 2>/dev/null \
      && grep -qa 'vban-receiver-bench\.sh' "/proc/$_pid/cmdline" 2>/dev/null; then
    die 4 "the bench (pid $_pid) still runs -- touch $RUN_DIR/STOP first"
  fi
  IN_CLEANUP=1
  rc=0
  LEASE_ID="$(sed -n 's/^lease=//p' "$RUN_DIR/recording.state" 2>/dev/null | head -n 1)"
  if grep -qx 'strih=1' "$RUN_DIR/recording.state" 2>/dev/null; then
    _raw="$(av_soak_rig_busy_settled "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$OBS_TIMEOUT_S" "$RIG_STATE" "$BROADCAST_READS" "$BROADCAST_RETRY_S" 1)"
    _plan="$(printf '%s' "$_raw" | python3 "$RIG_STATE" leftovers --state "$RUN_DIR/recording.state" --now "$(date +%s)" --start-window-s 60 2>/dev/null || true)"
    case "$_plan" in
      strih$'\t'stop*) record stop >> "$RUN_DIR/recordings.txt" 2>/dev/null || true; write_recording_state 0; log "cleanup: stopped the bench's strih recording" ;;
      strih$'\t'clear*) write_recording_state 0; log "cleanup: the strih recording already stopped" ;;
      *) log "cleanup: strih recording KEPT (${_plan:-the plan could not be made})"; rc=5 ;;
    esac
  fi
  if [ -s "$RUN_DIR/obs-state.json" ]; then
    obs_tool teardown || rc=5
  fi
  if [ "$rc" = 0 ] && [ -n "$LEASE_ID" ]; then
    rig_lease_release "$LEASE_ID" >/dev/null 2>&1 || true
    log "cleanup: lease released ($LEASE_ID; a lease another run holds is never touched)"
  fi
  exit "$rc"
fi

# --- --run -------------------------------------------------------------------------------------------

for _v in CAM_PW STRIH_USER STRIH_PW; do
  [ -n "${!_v:-}" ] || die 4 "$_v is not set (the same value recording-e2e.sh uses; see targets.md)"
done
for _t in python3 sshpass timeout setsid ffmpeg; do
  command -v "$_t" >/dev/null 2>&1 || die 4 "'$_t' not found on PATH"
done
[ "$(strih_platform "$STRIH_HOST")" = linux ] || die 4 "strih ${STRIH_HOST} is not the Linux strih-snv"
for _f in pid bench.tsv obs-state.json recording.state; do
  [ ! -e "$RUN_DIR/$_f" ] || die 4 "the run dir $RUN_DIR already holds a run ($_f) -- give --run-dir a new directory"
done
mkdir -p "$RUN_DIR"
printf '%s\n' "$$" > "$RUN_DIR/pid"
rm -f "$RUN_DIR/STOP"
printf 'profile\tppm\tsine\tlog\tverdict\trecording\n' > "$RUN_DIR/bench.tsv"
export RIG_LEASE_MAX_HOLD_SECS="$EXPECTED_S"

LEASE_ID="vban-bench-${STAMP}-$$"  # set BEFORE the acquire: a TERM inside it still releases (holder-checked)
MUTATED=0
RECORDING=0
EMU_PID=""
ABORT_REASON=""
OVERALL=PASS
IN_CLEANUP=0

cleanup() {
  local rc=$? kept=0
  set +e
  trap '' INT TERM HUP PIPE
  IN_CLEANUP=1
  if [ -n "$EMU_PID" ] && kill -0 "$EMU_PID" 2>/dev/null; then
    kill -TERM "$EMU_PID" 2>/dev/null
    wait "$EMU_PID" 2>/dev/null
  fi
  if [ "$RECORDING" = 1 ]; then
    if stop_recording_if_idle; then RECORDING=0; else kept=1; fi
  fi
  if [ "$MUTATED" = 1 ]; then
    setsid -w python3 "$OBS_DIR/vban_bench_obs.py" teardown --host "$STRIH_HOST" --password "${OBS_PASSWORD:-}" \
      --state "$RUN_DIR/obs-state.json" >> "$RUN_DIR/bench.log" 2>&1 || {
      log "teardown left something on strih-snv OBS (see $RUN_DIR/bench.log) -- run: bash $0 --cleanup $RUN_DIR" 2>/dev/null
      kept=1
    }
  fi
  if [ -n "$ABORT_REASON" ]; then log "ABORTED: $ABORT_REASON" 2>/dev/null; fi
  if [ "$kept" = 1 ]; then
    log "RECORDING OR TEST INPUT MAY STILL BE ON strih-snv -- lease KEPT ($LEASE_ID); run: bash $0 --cleanup $RUN_DIR" 2>/dev/null
    exit 5
  fi
  rig_lease_release "$LEASE_ID" >/dev/null 2>&1  # only ever our own holder (run_id checked)
  [ -s "$RUN_DIR/bench.tsv" ] && cat "$RUN_DIR/bench.tsv" 2>/dev/null
  if [ -n "$ABORT_REASON" ]; then
    case "$rc" in 4 | 5) exit "$rc" ;; *) exit 5 ;; esac
  fi
  case "$rc" in 0) ;; *) exit "$rc" ;; esac
  log "verdict: $OVERALL" 2>/dev/null
  [ "$OVERALL" = PASS ] && exit 0
  exit 1
}
trap cleanup EXIT
trap 'ABORT_REASON="SIGINT"; exit 5' INT
trap 'ABORT_REASON="SIGTERM"; exit 5' TERM
trap 'ABORT_REASON="SIGHUP"; exit 5' HUP

refuse() {  # REASON -> exit 4 while nothing changed, else 5
  ABORT_REASON="$1"
  if [ "$MUTATED" = 1 ]; then exit 5; fi
  exit 4
}
guard() {  # WHAT -> the shared issue-1271 guard; a busy rig refuses
  ( stray_session_check_assert "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$1" ) || refuse "the rig-busy guard refused $1"
}
keepalive() {
  local out="" r=0
  out="$(rig_lease_refresh_if_mine "$LEASE_REPO" "$LEASE_ID" 2>&1)" || r=$?
  case "$r" in
    0 | 2) return 0 ;;
    *) refuse "the rig lease is no longer ours (${out#RIG_LEASE_REFRESH=})" ;;
  esac
}

# ---- the lease ----
log "run dir $RUN_DIR; profiles: $PROFILES x ${MINUTES} min; input '$INPUT_NAME' port $PORT track $TRACK"
write_recording_state 0
set +e
lease_out="$(RIG_LEASE_RESOURCES="$LEASE_RESOURCES" rig_lease_acquire "$LEASE_REPO" "$LEASE_ID" "" vban-receiver-bench \
  "$(date -u -d '+900 seconds' +%Y-%m-%dT%H:%M:%SZ)")"
lease_rc=$?
set -e
log "$lease_out"
[ "$lease_rc" -eq 0 ] || refuse "the rig lease is held (${lease_out#RIG_LEASE_HELD_BY=}) -- rerun when it is free"

# ---- reads (exit 4, nothing changed) ----
_mode="$(rig_mode_from_painter_snapshot "$(remote root "$CAM_PW" "$PAINTER_IP" "$(rig_mode_state_probe_remote_snippet)" 2>/dev/null || true)")"
[ "$_mode" = TEST ] || refuse "the rig is not in TEST mode (cam2 painter reads $_mode) -- run scripts/rig-mode.sh test first"
_raw="$(av_soak_rig_busy_settled "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$OBS_TIMEOUT_S" "$RIG_STATE" "$BROADCAST_READS" "$BROADCAST_RETRY_S")"
_busy="$(printf '%s' "$_raw" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("busy"))' 2>/dev/null || true)"
[ "$_busy" = False ] || refuse "the rig is not provably idle (rig-busy-check: ${_raw:-unreadable})"
set +e
obs_tool preflight > "$RUN_DIR/preflight.json" 2>> "$RUN_DIR/bench.log"
_prc=$?
set -e
[ "$_prc" -eq 0 ] || refuse "the OBS preflight refused (rc $_prc): $(cat "$RUN_DIR/preflight.json" 2>/dev/null)"
_feeders="$(python3 -c 'import json,sys; print(", ".join(json.load(open(sys.argv[1])).get("track_feeders") or []))' "$RUN_DIR/preflight.json" 2>/dev/null || true)"
[ -z "$_feeders" ] || log "NOTE: inputs that also feed track $TRACK: $_feeders (the analysis FAILs a track that is not clean)"
_ports="$(remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" "ss -Hlun 'sport = :${PORT}'; echo SS_RC=\$?" 2>/dev/null || true)"
case "$_ports" in
  SS_RC=0) ;;
  *SS_RC=0*) refuse "UDP port $PORT is already bound on strih-snv: ${_ports%SS_RC=*}" ;;
  *) refuse "could not read the UDP ports of strih-snv (${_ports:-no answer})" ;;
esac
HAS_FFMPEG=0
if remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" 'command -v ffmpeg >/dev/null' 2>/dev/null; then HAS_FFMPEG=1; fi

# ---- the profiles: a FRESH test input each (its receiver and its ASRC counters start clean) ----
setup_input() {  # LABEL -> the guard, then vban_bench_obs.py setup; sets STREAM_INDEX
  guard "the VBAN bench test input setup ($1)"
  local was="$MUTATED" rc=0
  MUTATED=1
  obs_tool setup > "$RUN_DIR/setup.json" 2>> "$RUN_DIR/bench.log" || rc=$?
  case "$rc" in
    0) ;;
    4) MUTATED="$was"; refuse "the OBS setup refused before creating the input (see $RUN_DIR/bench.log)" ;;
    *) refuse "the OBS setup failed after a change (rc $rc, see $RUN_DIR/bench.log)" ;;
  esac
  STREAM_INDEX="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["audio_stream_index"])' "$RUN_DIR/setup.json")"
  log "test input active on '$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["program_scene"])' "$RUN_DIR/setup.json")', track $TRACK = audio stream $STREAM_INDEX of the recording"
}
teardown_input() {  # -> the input removed + RecTracks restored, else abort (the state file stays for --cleanup)
  obs_tool teardown >> "$RUN_DIR/bench.log" 2>&1 || refuse "the teardown left something on strih-snv OBS (see $RUN_DIR/bench.log)"
  rm -f "$RUN_DIR/obs-state.json"
}
pull_audio() {  # PATH OUT -> the test track (on-box stream copy) or the whole file; 0 when OUT is non-empty
  local path="$1" out="$2" q i
  q="'${path//\'/\'\\\'\'}'"
  for i in 1 2 3 4 5; do
    if [ "$HAS_FFMPEG" = 1 ]; then
      remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" \
        "nice -n 19 ffmpeg -v error -nostdin -i $q -map 0:a:${STREAM_INDEX} -c copy -f matroska -" > "$out" 2>> "$RUN_DIR/bench.log"
    else
      remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" "cat -- $q" > "$out" 2>> "$RUN_DIR/bench.log"
    fi
    if [ -s "$out" ]; then return 0; fi
    sleep 3
  done
  return 1
}
_i=0
for _p in $PROFILES; do
  _i=$((_i + 1))
  if [ -e "$RUN_DIR/STOP" ]; then log "STOP file -- ending before profile $_p"; break; fi
  keepalive
  setup_input "profile $_p"
  guard "the profile ${_p} StartRecord"
  _mark="$(strih_log_line_count "$STRIH_HOST" "$STRIH_USER" "$STRIH_PW")"
  is_uint "$_mark" || refuse "the strih OBS log cannot be read (line count '${_mark}')"
  write_recording_state 1 "$(date +%s)"
  RECORDING=1
  record start 2>> "$RUN_DIR/bench.log" || refuse "StartRecord failed on strih-snv (profile $_p)"
  sleep "$LEAD_S"
  log "profile $_p: emulator for ${MINUTES} min"
  emulator_argv "$_i" "$_p"
  "${EMU_ARGV[@]}" >> "$RUN_DIR/bench.log" 2>&1 &
  EMU_PID=$!
  while kill -0 "$EMU_PID" 2>/dev/null; do
    sleep "$POLL_S" &
    wait $! || true
    kill -0 "$EMU_PID" 2>/dev/null || break
    keepalive
    if [ "$(broadcast_now)" = live ]; then refuse "a broadcast went live during profile $_p"; fi
  done
  _erc=0
  wait "$EMU_PID" || _erc=$?
  EMU_PID=""
  sleep "$TAIL_S"
  stop_recording_if_idle || refuse "the strih recording could not be stopped on a proven idle rig (profile $_p)"
  RECORDING=0
  teardown_input
  _rec="$(tail -n 1 "$RUN_DIR/recordings.txt" 2>/dev/null || true)"
  strih_log_since_line "$STRIH_HOST" "$STRIH_USER" "$STRIH_PW" "$_mark" > "$RUN_DIR/p$_i-obs.log"
  if [ -n "$_rec" ]; then strih_lx_recording_cleanup_note "" "$STRIH_HOST" "$_rec" "$STRIH_USER" >> "$RUN_DIR/cleanup-plan.txt"; fi
  _sine=FAIL
  _stream=0
  if [ "$HAS_FFMPEG" != 1 ]; then _stream="$STREAM_INDEX"; fi
  if [ "$_erc" != 0 ]; then
    log "profile $_p: the emulator exited $_erc"
  elif [ -z "$_rec" ] || ! pull_audio "$_rec" "$RUN_DIR/p$_i-audio.mkv"; then
    log "profile $_p: the recording could not be pulled from strih-snv (path '${_rec}')"
  else
    _arc=0
    python3 "$ANALYZE" "$RUN_DIR/p$_i-audio.mkv" --schedule "$RUN_DIR/p$_i-schedule.json" --stream "$_stream" \
      --lead-s "$ANALYSIS_LEAD_S" --ppm-bound "$PPM_BOUND" --json > "$RUN_DIR/p$_i-analysis.json" || _arc=$?
    if [ "$_arc" = 0 ]; then _sine=PASS; fi
  fi
  _lost="$(python3 -c 'import json,sys; print(sum(l["count"] for l in json.load(open(sys.argv[1]))["losses"]))' "$RUN_DIR/p$_i-schedule.json" 2>/dev/null || echo 0)"
  _ppm="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ppm"])' "$RUN_DIR/p$_i-schedule.json" 2>/dev/null || echo 0)"
  _lrc=0
  python3 "$LOG_GRADE" --log "$RUN_DIR/p$_i-obs.log" --input-name "$INPUT_NAME" --expected-lost "$_lost" \
    --expect-ppm "$_ppm" --ppm-bound "$PPM_BOUND" --json > "$RUN_DIR/p$_i-log.json" || _lrc=$?
  _logv=FAIL
  if [ "$_lrc" = 0 ]; then _logv=PASS; fi
  _v=FAIL
  if [ "$_sine" = PASS ] && [ "$_logv" = PASS ]; then _v=PASS; fi
  if [ "$_v" != PASS ]; then OVERALL=FAIL; fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$_p" "$_ppm" "$_sine" "$_logv" "$_v" "$_rec" >> "$RUN_DIR/bench.tsv"
  log "profile $_p: sine $_sine, log $_logv -> $_v"
done
exit 0
