#!/usr/bin/env bash
# scripts/av-restart-matrix.sh -- issue 1367: the RESTART MATRIX (full header below).
set -euo pipefail
#
# WHY (issue 1367, owner goal 24.9.2026): after EVERY restart -- stream OBS, strih OBS (strih-lx),
# a cambox service, dantesync -- the picture-to-sound latency on the stream output must be right by
# itself, and the cameras must stay in sync with each other, with no manual step. The acceptance:
# "for each restart kind, the stream output meets the A/V and spread bounds within the settle time,
# with no manual step, 3/3 repeats". Nothing tested restart survival: the 27.9 A/V failure after a
# stream OBS restart was found only by the next release E2E, hours later.
#
# WHAT ONE RUN DOES (measure-only; every rig action is an existing primitive):
#   - takes the issue-830 rig lease under its OWN holder (repo `camera-box-av-restart-matrix`) for
#     the whole run and keeps its heartbeat fresh every <= AV_MATRIX_KEEPALIVE_S (10 s), also while
#     a window runs (the issue-1383 keep-alive: a peer never reads a live matrix as hung);
#   - before every mutation: the lease still this run's, the shared issue-1271 rig-busy guard, a
#     proven idle rig (the soak's own retried read, av_soak_rig_busy_settled + the
#     `av_soak_rig_state.py broadcast` decision) and TEST mode (the stream program is the
#     development scene, the cam2 painter emits) -- nothing restarts while a box records or streams,
#     and a rig handed back to production (out of TEST mode) ends the run with its report;
#   - ONE baseline window, then for each restart kind x repeat (default 3):
#       restart ONE component -> wait until it reports healthy (bounded) -> wait the settle time ->
#       measure ONE window -> one matrix.tsv row (time to healthy, the window);
#     every window is the SOAK ITSELF: `scripts/av-soak.sh --run --hours 0 --lease-run-id <this
#     run's lease>` -- its own reads-before-writes setup (TEST mode, the Development stream program,
#     the painter, the burns), the connect-on-show hold, ONE strih-program sweep recorded on strih +
#     stream and decoded in place, and its cleanup that a signal cannot cut short. Never a copy of
#     that step. It writes no latency pin, no audio offset, no correction, and never switches the
#     stream program (the stream program must stay `Development`; each window re-reads it);
#   - grades every window POINTWISE (scripts/av_restart_matrix_decision.py): A/V per camera within
#     AV_OFFSET_GATE_TOLERANCE_MS, the camera spread within SPREAD_THRESHOLD_MS, the gate's own loss
#     term, the hop burns -- both bounds read from their Rust sources; each kind PASS only 3/3.
#   The restart kinds (scripts/lib/av-restart-matrix.sh builds every remote text):
#     strih-obs  -- strih-lx `systemctl --user restart strih-obs.service` over plain ssh (the ONE
#                   headless restart, mv_reverify_obs_restart_linux_cmd); healthy = the unit active
#                   AND its OBS WebSocket answers.
#     cambox     -- `systemctl restart camera-box` on ONE camera (root ssh), NEVER a reboot (a remote
#                   cambox reboot is banned); healthy = active AND a `Streaming:` journal line of the
#                   NEW process (its systemd invocation, never the old process's last lines).
#     dantesync  -- `systemctl restart dantesync` on ONE camera node; healthy = its :8898/status
#                   graded OK by dantesync_clock_decision.py analyze (locked on the rig grandmaster).
#     stream-obs -- a SUPERVISOR step: the stream box's canonical launch is its `OBS Studio.lnk` ->
#                   obs-guarded-launch.ps1; no session-agnostic path runs it (every interactive-token
#                   OBS task there launches a bare obs64.exe), an ssh GUI launch is banned. The run
#                   writes the exact program to <run-dir>/supervisor-step-stream-obs-rN.txt, waits
#                   (bounded) for <run-dir>/confirm-stream-obs-rN, then grades it like the others;
#                   healthy = the WebSocket answers on the development program scene (another scene
#                   fails at once -- the matrix never switches a scene).
#   A component that never reports healthy, or whose restart fails, is a FAIL and stops the run; a
#   window the soak itself stopped (no measurement) or refused stops it too; a baseline that is not
#   PASS stops it before any restart (--keep-going runs the restarts anyway).
#   cleanup, on EVERY exit (signals ignored): a running window gets SIGTERM and its own cleanup is
#   waited for; every restarted component is left running (`is-active || start`; the stream OBS is
#   read, and the supervisor step printed when it does not answer); the report; the lease released
#   -- KEPT when a window may have left a recording (then `--stop-leftovers <run-dir>`).
#
# MODES:
#   --plan  (DEFAULT) print every step with the exact commands; touches NOTHING.
#   --run   the matrix. Needs CAM_PW (the cambox root login), STRIH_USER + STRIH_PW (strih-lx),
#           STREAM_USER + STREAM_PW, PROBE_BIN_DIR + WIN_VERDICT_EXE_LOCAL (the soak's own window
#           needs all of them; the same values recording-e2e.sh uses).
#   --report RUN_DIR          re-grade a run (exit = its verdict).
#   --stop-leftovers RUN_DIR  the systemd ExecStopPost safety net: `av-soak.sh --stop-leftovers` on
#           every window that may have left a recording, the restarted components left running, then
#           the matrix's own lease released (holder-checked) when nothing is left.
#
# OPTIONS (env equivalent in brackets):
#   --kinds "K ..."            [AV_MATRIX_KINDS, "strih-obs cambox dantesync stream-obs"]
#   --repeats N                [AV_MATRIX_REPEATS, 3]
#   --settle-secs S            [AV_MATRIX_SETTLE_SECS, 120]   after healthy, before the window
#   --healthy-timeout-secs S   [AV_MATRIX_HEALTHY_TIMEOUT_S, 300]
#   --supervisor-timeout-secs S [AV_MATRIX_SUPERVISOR_TIMEOUT_S, 1800]  the stream OBS step
#   --cambox camN              [AV_MATRIX_CAMBOX, the first soak camera that is not cam2]
#   --dantesync-node camN      [AV_MATRIX_DANTESYNC_NODE, the --cambox camera]
#   --run-dir D                [AV_MATRIX_RUN_DIR, ~/.camera-box/av-restart-matrix/<UTC stamp>]
#   --probe-bin-dir D / --win-verdict-exe P / --spread-columns C   passed to every window / grade
#   --keep-going               [AV_MATRIX_KEEP_GOING=1]  restart even after a non-PASS baseline
# Other env: AV_SOAK_CAMS (the soak's camera set; acks removed), STRIH_HOST / STREAM_HOST,
#   AV_MATRIX_POLL_S (5), AV_MATRIX_KEEPALIVE_S (10), AV_MATRIX_SSH_TIMEOUT_S (60),
#   AV_SOAK_BROADCAST_READS / AV_SOAK_BROADCAST_RETRY_S (the soak's retried idle proof). Test
#   seams: AV_MATRIX_SOAK (the window script), AV_SOAK_OBS_DIR (obs_phase2.py), RIG_LEASE_DIR.
#
# STOP: `touch <run-dir>/STOP` (ends before the next restart, full cleanup + report), or SIGTERM.
#
# EXIT: 0 PASS / 1 FAIL / 2 UNKNOWN (the report of a run that ended normally, by STOP, or stopped
#       on a failure / a non-PASS baseline / a refused window), 3 usage error, 4 refused before any
#       rig change (lease held, rig busy, a missing credential/binary/bound, a used run dir), 5
#       aborted after a rig change (a broadcast / busy rig before a restart, a signal, a window
#       that may have left a recording -- then the lease is kept). The report is written on 5 too.
#       --stop-leftovers: 0 nothing left (lease released), 5 something kept, 4 the matrix still runs.
#
# Runbook + the rule: .claude/rules/av-restart-matrix.md.

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
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"
# shellcheck source=scripts/lib/stream-dev-scene.sh
. "$HERE/lib/stream-dev-scene.sh"
# shellcheck source=scripts/lib/rig-grandmaster.sh
. "$HERE/lib/rig-grandmaster.sh"
# shellcheck source=scripts/lib/mv-reverify-escalate.sh
. "$HERE/lib/mv-reverify-escalate.sh"
# shellcheck source=scripts/lib/av-soak.sh
. "$HERE/lib/av-soak.sh"
# shellcheck source=scripts/lib/av-restart-matrix.sh
. "$HERE/lib/av-restart-matrix.sh"
# shellcheck source=scripts/lib/av-restart-matrix-plan.sh
. "$HERE/lib/av-restart-matrix-plan.sh"

log() { printf '%s [av-restart-matrix] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { echo "av-restart-matrix: ERROR: $2" >&2; exit "$1"; }
usage() { sed -n '2,/^# Runbook/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
need_value() { [ "$2" -ge 2 ] || die 3 "$1 needs a value (try --help)"; }
is_uint() { case "${1:-}" in '' | *[!0-9]*) return 1 ;; esac; }

MODE=plan
REPORT_DIR=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan) MODE=plan ;;
    --run) MODE=run ;;
    --report) need_value "$1" "$#"; MODE=report; REPORT_DIR="$2"; shift ;;
    --stop-leftovers) need_value "$1" "$#"; MODE=stop-leftovers; REPORT_DIR="$2"; shift ;;
    --kinds) need_value "$1" "$#"; AV_MATRIX_KINDS="$2"; shift ;;
    --repeats) need_value "$1" "$#"; AV_MATRIX_REPEATS="$2"; shift ;;
    --settle-secs) need_value "$1" "$#"; AV_MATRIX_SETTLE_SECS="$2"; shift ;;
    --healthy-timeout-secs) need_value "$1" "$#"; AV_MATRIX_HEALTHY_TIMEOUT_S="$2"; shift ;;
    --supervisor-timeout-secs) need_value "$1" "$#"; AV_MATRIX_SUPERVISOR_TIMEOUT_S="$2"; shift ;;
    --cambox) need_value "$1" "$#"; AV_MATRIX_CAMBOX="$2"; shift ;;
    --dantesync-node) need_value "$1" "$#"; AV_MATRIX_DANTESYNC_NODE="$2"; shift ;;
    --run-dir) need_value "$1" "$#"; AV_MATRIX_RUN_DIR="$2"; shift ;;
    --probe-bin-dir) need_value "$1" "$#"; PROBE_BIN_DIR="$2"; shift ;;
    --win-verdict-exe) need_value "$1" "$#"; WIN_VERDICT_EXE_LOCAL="$2"; shift ;;
    --spread-columns) need_value "$1" "$#"; AV_SOAK_SPREAD_COLUMNS="$2"; shift ;;
    --keep-going) AV_MATRIX_KEEP_GOING=1 ;;
    -h | --help) usage; exit 0 ;;
    *) die 3 "unknown argument '$1' (try --help)" ;;
  esac
  shift
done

DECISION="$HERE/av_restart_matrix_decision.py"
SOAK_DECISION="$HERE/av_soak_decision.py"
RIG_STATE="$HERE/av_soak_rig_state.py"
DANTE_DECISION="$HERE/dantesync_clock_decision.py"
SOAK="${AV_MATRIX_SOAK:-$HERE/av-soak.sh}"
OBS_DIR="${AV_SOAK_OBS_DIR:-$HERE}"
# the stream program the matrix requires and never changes (the soak's own name source, issue 1380)
STREAM_DEV_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}"
STRIH_HOST="${STRIH_HOST:-$(obs_fleet_host strih-lx)}"
STREAM_HOST="${STREAM_HOST:-$(obs_fleet_host stream)}"
OBS_TIMEOUT_S="${AV_SOAK_OBS_TIMEOUT_S:-30}"
SSH_TIMEOUT_S="${AV_MATRIX_SSH_TIMEOUT_S:-60}"
POLL_S="${AV_MATRIX_POLL_S:-5}"
KEEPALIVE_S="${AV_MATRIX_KEEPALIVE_S:-10}"
BROADCAST_READS="${AV_SOAK_BROADCAST_READS:-3}"
BROADCAST_RETRY_S="${AV_SOAK_BROADCAST_RETRY_S:-20}"
SPREAD_ARGS=()
if [ -n "${AV_SOAK_SPREAD_COLUMNS:-}" ]; then
  SPREAD_ARGS=(--spread-columns "$AV_SOAK_SPREAD_COLUMNS")
  export AV_SOAK_SPREAD_COLUMNS
fi
IN_CLEANUP=0
SLEEP_PID=""

# remote USER PASSWORD HOST TEXT -> the text run over plain ssh, bounded; the password reaches
# sshpass through its environment (-e), never argv. In cleanup it runs in its own session.
remote() {
  local pre=()
  if [ "$IN_CLEANUP" = 1 ]; then pre=(setsid -w); fi
  SSHPASS="$2" "${pre[@]}" timeout "$SSH_TIMEOUT_S" sshpass -e ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 "$1@$3" "$4"
}
isleep() {  # SECS -> a sleep a signal interrupts at once
  sleep "$1" &
  SLEEP_PID=$!
  wait "$SLEEP_PID" || true
  SLEEP_PID=""
}

# ensure_running KIND -> leave the kind's component running (a no-op on a running one); always 0.
ensure_running() {
  local out scene
  case "$1" in
    strih-obs) out="$(remote "${STRIH_USER:-}" "${STRIH_PW:-}" "$STRIH_HOST" "$(av_matrix_ensure_running_remote_cmd strih-obs)" 2>&1 || true)" ;;
    cambox) out="$(remote root "${CAM_PW:-}" "$CAMBOX_IP" "$(av_matrix_ensure_running_remote_cmd cambox)" 2>&1 || true)" ;;
    dantesync) out="$(remote root "${CAM_PW:-}" "$DANTE_IP" "$(av_matrix_ensure_running_remote_cmd dantesync)" 2>&1 || true)" ;;
    stream-obs)
      scene="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "" 2>/dev/null || true)"
      if [ "$scene" = "$STREAM_DEV_SCENE" ]; then
        log "leave running: the stream OBS answers (program '$scene')"
      elif [ -n "$scene" ]; then
        log "WARNING: the stream OBS answers but its program is '$scene', not '$STREAM_DEV_SCENE' -- SUPERVISOR: set it back by hand (never PRO); the matrix never switches a scene"
      else
        log "WARNING: the stream OBS does NOT answer on its WebSocket -- SUPERVISOR: relaunch it (bash $HERE/launch-obs-genlock.sh --box stream --force, pasted into the win-stream-snv MCP Shell)"
      fi
      return 0
      ;;
  esac
  case "$out" in
    *active=active*) log "leave running: $1 is active" ;;
    *) log "WARNING: $1 is NOT confirmed active after cleanup's start (${out//$'\n'/ }) -- check it by hand" ;;
  esac
  return 0
}

# --- --report / --stop-leftovers read an existing run dir --------------------------------------------

if [ "$MODE" = report ]; then
  rc=0
  python3 "$DECISION" report --dir "$REPORT_DIR" "${SPREAD_ARGS[@]}" || rc=$?
  exit "$rc"
fi

conf_value() {  # RUN_DIR KEY -> the value from <run-dir>/matrix.conf
  sed -n "s/^$2=//p" "$1/matrix.conf" 2>/dev/null | head -n 1 || true
}

if [ "$MODE" = stop-leftovers ]; then
  [ -d "$REPORT_DIR" ] || die 3 "--stop-leftovers needs a matrix run dir (got '$REPORT_DIR')"
  _pid="$(head -n 1 "$REPORT_DIR/pid" 2>/dev/null || true)"
  if is_uint "$_pid" && kill -0 "$_pid" 2>/dev/null \
      && grep -qa 'av-restart-matrix\.sh' "/proc/$_pid/cmdline" 2>/dev/null; then
    die 4 "the matrix (pid $_pid) is still running -- stop it first (touch $REPORT_DIR/STOP); --stop-leftovers only cleans up after it"
  fi
  rc=0
  for _w in "$REPORT_DIR"/w-*; do
    [ -d "$_w" ] || continue
    av_matrix_window_stuck "$_w" || continue
    log "stop-leftovers: the window $_w may have left a recording -- av-soak.sh --stop-leftovers"
    _r=0
    bash "$SOAK" --stop-leftovers "$_w" || _r=$?
    [ "$_r" = 0 ] || rc=5
  done
  CAMBOX_IP="$(conf_value "$REPORT_DIR" cambox_ip)"
  DANTE_IP="$(conf_value "$REPORT_DIR" dantesync_ip)"
  if [ -s "$REPORT_DIR/restarted" ]; then
    for _k in $(sort -u "$REPORT_DIR/restarted"); do ensure_running "$_k"; done
  fi
  _lease="$(conf_value "$REPORT_DIR" lease_run_id)"
  if [ "$rc" = 0 ]; then
    if [ -n "$_lease" ]; then
      rig_lease_release "$_lease" >/dev/null 2>&1 || true
      log "stop-leftovers: rig lease released ($_lease; a lease another run holds is never touched)"
    fi
    exit 0
  fi
  log "stop-leftovers: rig lease KEPT (${_lease:-none}) -- a recording a window may have left is still there"
  exit 5
fi

# --- configuration (plan + run) ---------------------------------------------------------------------

KINDS="${AV_MATRIX_KINDS:-$AV_MATRIX_KINDS_ALL}"
REPEATS="${AV_MATRIX_REPEATS:-3}"
SETTLE_S="${AV_MATRIX_SETTLE_SECS:-120}"
HEALTHY_TIMEOUT_S="${AV_MATRIX_HEALTHY_TIMEOUT_S:-300}"
SUPERVISOR_TIMEOUT_S="${AV_MATRIX_SUPERVISOR_TIMEOUT_S:-1800}"
KEEP_GOING="${AV_MATRIX_KEEP_GOING:-0}"
[ -n "${KINDS// /}" ] || die 3 "--kinds is empty"
_seen=" "
for _k in $KINDS; do
  av_matrix_kind_valid "$_k" || die 3 "unknown restart kind '$_k' (expected: $AV_MATRIX_KINDS_ALL)"
  case "$_seen" in *" $_k "*) die 3 "restart kind '$_k' given twice" ;; esac
  _seen="$_seen$_k "
done
is_uint "$REPEATS" && [ "$REPEATS" -ge 1 ] || die 3 "--repeats must be an integer >= 1 (got '$REPEATS')"
is_uint "$SETTLE_S" || die 3 "--settle-secs must be a non-negative integer (got '$SETTLE_S')"
is_uint "$HEALTHY_TIMEOUT_S" && [ "$HEALTHY_TIMEOUT_S" -ge 1 ] || die 3 "--healthy-timeout-secs must be an integer >= 1"
is_uint "$SUPERVISOR_TIMEOUT_S" && [ "$SUPERVISOR_TIMEOUT_S" -ge 1 ] || die 3 "--supervisor-timeout-secs must be an integer >= 1"
for _n in "$POLL_S" "$KEEPALIVE_S" "$SSH_TIMEOUT_S" "$OBS_TIMEOUT_S" "$BROADCAST_READS" "$BROADCAST_RETRY_S"; do
  is_uint "$_n" || die 3 "the AV_MATRIX_* / AV_SOAK_* timings must be integers (got '$_n')"
done
[ "$BROADCAST_READS" -ge 1 ] || die 3 "AV_SOAK_BROADCAST_READS must be >= 1"
[ "$STREAM_DEV_SCENE" != "$STREAM_PRODUCTION_SCENE_DEFAULT" ] \
  || die 3 "STREAM_PROG_SCENE names the production scene; the matrix only runs on the development scene (issue 1380)"
camera_resolve cam2
PAINTER_IP="${PAINTER_IP:-$CAMERA_IP}"
MARKER_LOG="${AV_SOAK_MARKER_LOG:-/run/rig-qpsk-markers.csv}"
has_kind() { case " $KINDS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "${RIG_FLEET_ACK_FILE:-$HERE/../rig-fleet.txt}")"
export CAMBOX_OFFLINE_ACK
SOAK_CAMS="$(av_soak_unacked_cams "${AV_SOAK_CAMS:-$CAMERA_ACTIVE_SET}")"
CAMBOX="${AV_MATRIX_CAMBOX:-$(av_matrix_default_cambox "$SOAK_CAMS")}"
DANTE_NODE="${AV_MATRIX_DANTESYNC_NODE:-$CAMBOX}"
CAMBOX_IP=""
DANTE_IP=""
if has_kind cambox; then
  [ -n "$CAMBOX" ] || die 3 "no cambox to restart (the soak cameras '$SOAK_CAMS' hold none but cam2)"
  [ "$CAMBOX" != cam2 ] || die 3 "--cambox cam2 is refused: cam2 captures the imag-nb projection and carries the painter, not a camera path"
  camera_resolve "$CAMBOX" >/dev/null 2>&1 || die 3 "unknown camera '$CAMBOX' for --cambox"
  CAMBOX_IP="$CAMERA_IP"
fi
if has_kind dantesync; then
  [ -n "$DANTE_NODE" ] || die 3 "no dantesync node (give --dantesync-node camN)"
  camera_resolve "$DANTE_NODE" >/dev/null 2>&1 \
    || die 3 "--dantesync-node '$DANTE_NODE' is not a camera: this slice restarts dantesync on a camera node only (root ssh, the camera credential)"
  DANTE_IP="$CAMERA_IP"
fi
N_RESTARTS=0
N_STREAM=0
for _k in $KINDS; do
  N_RESTARTS=$(( N_RESTARTS + REPEATS ))
  if [ "$_k" = stream-obs ]; then N_STREAM="$REPEATS"; fi
done
N_STEPS=$(( 1 + N_RESTARTS ))
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="${AV_MATRIX_RUN_DIR:-$HOME/.camera-box/av-restart-matrix/$STAMP}"
RIG_LEASE_REPO_NAME="${AV_MATRIX_LEASE_REPO:-camera-box-av-restart-matrix}"
RIG_LEASE_OURS="av-matrix-${STAMP}-$$"
# one window's upper bound: the soak's own slot (read from av-soak.sh, the single source, whatever
# AV_MATRIX_SOAK points at) + 600 s for its setup and cleanup
SOAK_SLOT_S="$(av_matrix_soak_slot_s "$HERE/av-soak.sh")" || die 3 "cannot read the soak's slot from $HERE/av-soak.sh"
WINDOW_BOUND_S=$(( SOAK_SLOT_S + 600 ))
EXPECTED_S="$(av_matrix_expected_duration_s "$N_STEPS" "$WINDOW_BOUND_S" "$SETTLE_S" "$HEALTHY_TIMEOUT_S" "$SUPERVISOR_TIMEOUT_S" "$N_STREAM")"
LEASE_EXPECTED_AT="$(date -u -d "+${EXPECTED_S} seconds" +%Y-%m-%dT%H:%M:%SZ)"

target_of() {
  case "$1" in
    strih-obs) printf 'strih-lx %s\n' "$STRIH_HOST" ;;
    cambox) printf '%s %s\n' "$CAMBOX" "$CAMBOX_IP" ;;
    dantesync) printf '%s %s\n' "$DANTE_NODE" "$DANTE_IP" ;;
    stream-obs) printf 'stream %s\n' "$STREAM_HOST" ;;
  esac
}

# --- --plan: print every step, touch nothing (scripts/lib/av-restart-matrix-plan.sh) ----------------

if [ "$MODE" = plan ]; then
  print_plan
  exit 0
fi

# --- --run -------------------------------------------------------------------------------------------

for _v in CAM_PW STREAM_USER STREAM_PW STRIH_USER STRIH_PW; do
  [ -n "${!_v:-}" ] || die 4 "$_v is not set (the same value recording-e2e.sh uses; see targets.md)"
done
export CAM_PW STREAM_USER STREAM_PW STRIH_USER STRIH_PW
[ -n "${PROBE_BIN_DIR:-}" ] && [ -x "$PROBE_BIN_DIR/recording-verdict" ] \
  || die 4 "PROBE_BIN_DIR must hold the CI-built Linux recording-verdict (every window needs it)"
[ -n "${WIN_VERDICT_EXE_LOCAL:-}" ] && [ -f "$WIN_VERDICT_EXE_LOCAL" ] \
  || die 4 "WIN_VERDICT_EXE_LOCAL must be the CI-built recording-verdict.exe (every window needs it)"
export PROBE_BIN_DIR WIN_VERDICT_EXE_LOCAL
for _t in sshpass curl timeout setsid python3; do
  command -v "$_t" >/dev/null 2>&1 || die 4 "'$_t' not found on PATH"
done
[ "$(strih_platform "$STRIH_HOST")" = linux ] || die 4 "strih ${STRIH_HOST} is not the Linux strih-lx (the windows decode strih in place on strih-lx)"
python3 "$SOAK_DECISION" bounds >/dev/null || die 4 "the gate bounds cannot be read from their Rust sources"
GM_IP=""
if has_kind dantesync; then
  GM_IP="$(rig_grandmaster_ip)" || die 4 "the rig grandmaster does not resolve -- the dantesync health cannot be graded"
fi
for _f in pid matrix.tsv matrix.conf; do
  [ ! -e "$RUN_DIR/$_f" ] || die 4 "the run dir $RUN_DIR already holds a run ($_f) -- give --run-dir a new directory"
done
mkdir -p "$RUN_DIR"
printf '%s\n' "$$" > "$RUN_DIR/pid"
rm -f "$RUN_DIR/STOP"
cat > "$RUN_DIR/matrix.conf" <<EOF
kinds=${KINDS}
repeats=${REPEATS}
settle_s=${SETTLE_S}
healthy_timeout_s=${HEALTHY_TIMEOUT_S}
supervisor_timeout_s=${SUPERVISOR_TIMEOUT_S}
spread_columns=${AV_SOAK_SPREAD_COLUMNS:-av_spread_ms}
cambox=${CAMBOX}
cambox_ip=${CAMBOX_IP}
dantesync_node=${DANTE_NODE}
dantesync_ip=${DANTE_IP}
strih_host=${STRIH_HOST}
stream_host=${STREAM_HOST}
lease_run_id=${RIG_LEASE_OURS}
started=${STAMP}
EOF
# issue 1383: the lease's hold ceiling for the whole run -- the matrix's own keep-alive AND every
# window's (the soak keeps its caller's exported ceiling under --lease-run-id)
export RIG_LEASE_MAX_HOLD_SECS="$EXPECTED_S"

SETUP_STARTED=0
MUTATED=0
LEASE_HELD=0
LOOP_DONE=0
STOPPED=0
SOAK_PID=""
WIN_WAITED=0
# the step whose window runs now: STEP KIND REPEAT TARGET RESTART_EPOCH HEALTHY_EPOCH HEALTHY
CUR=()
ABORT_REASON=""
WIN_DIR=""
WIN_RC=0

refuse() {  # REASON -> exit 4 while nothing was changed, else 5
  ABORT_REASON="$1"
  log "STOP: $1"
  if [ "$MUTATED" = 1 ]; then exit 5; fi
  exit 4
}
guard_ok() {
  ( stray_session_check_assert "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" "$1" )
}
# lease_keepalive -> the ONE holder keep-alive (issue 1383, rig_lease_refresh_if_mine): bumps the
# heartbeat and rolls expected_release_at, only while holder.json names this run. 0 refreshed (or a
# filesystem error, retried next beat); 1 the lease is no longer ours or the run is past its
# declared hold ceiling (ABORT_REASON set; the caller exits 5 and never touches the other lease).
lease_keepalive() {
  local out="" rc=0
  out="$(rig_lease_refresh_if_mine "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" 2>&1)" || rc=$?
  case "$rc" in
    0) return 0 ;;
    2) log "WARNING: could not refresh the rig lease (${out#RIG_LEASE_REFRESH=}) -- next beat retries"; return 0 ;;
    3) ABORT_REASON="the matrix ran past its declared lease window (${out#RIG_LEASE_REFRESH=})"; return 1 ;;
    *) ABORT_REASON="the rig lease is no longer ours (${out#RIG_LEASE_REFRESH=})"; return 1 ;;
  esac
}
# test_mode_state -> `ok`, `left<TAB>reason` (a DEFINITE change: the stream program reads another
# scene, the painter service reads inactive) or `unknown<TAB>reason` (unreadable / not emitting after
# BROADCAST_READS reads, BROADCAST_RETRY_S apart -- one ssh blip is never a mode change). Read-only.
test_mode_state() {
  local i prog="" probe="" active=""
  for ((i = 1; i <= BROADCAST_READS; i++)); do
    prog="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "")"
    if [ -n "$prog" ] && [ "$prog" != "$STREAM_DEV_SCENE" ]; then
      printf "left\tthe stream program is '%s', not '%s'\n" "$prog" "$STREAM_DEV_SCENE"
      return 0
    fi
    if [ -n "$prog" ]; then
      probe="$(remote root "$CAM_PW" "$PAINTER_IP" "$(av_soak_painter_probe_cmd "$MARKER_LOG")" 2>/dev/null || true)"
      active="$(av_soak_kv active "$probe")"
      if [ "$active" = inactive ]; then
        printf 'left\tthe cam2 painter service is stopped\n'
        return 0
      fi
      if av_soak_painter_ok "$active" "$(av_soak_kv markers "$probe")" "$(av_soak_kv markers2 "$probe")"; then
        printf 'ok\n'
        return 0
      fi
    fi
    if [ "$i" -lt "$BROADCAST_READS" ]; then sleep "$BROADCAST_RETRY_S"; fi
  done
  if [ -z "$prog" ]; then
    printf 'unknown\tthe stream program is unreadable\n'
  elif [ -z "$active" ]; then
    printf 'unknown\tthe cam2 painter probe is unreadable (no answer)\n'
  else
    printf 'unknown\tthe cam2 painter is not emitting (%s)\n' "${probe//$'\n'/ }"
  fi
  return 0
}
# before_mutation LABEL -> 0 on a rig the matrix may change now: the lease still ours, the rig-busy
# guard, a proven idle rig (the soak's retried read), TEST mode. Busy / live / unreadable / a lost
# lease -> refuse (exit 4 before any change, else 5). Out of TEST mode -> exit 4 before any change,
# else 1 with STOPPED=1 (the rig was handed back: the run ends with its report, like the soak).
before_mutation() {
  local st why
  if [ "$LEASE_HELD" = 1 ] && ! lease_keepalive; then
    log "STOP: $ABORT_REASON (before $1)"
    exit 5
  fi
  guard_ok "$1" || refuse "the rig is busy (a box records or streams) before $1"
  case "$(av_soak_broadcast_of "$RIG_STATE" "$(av_soak_rig_busy_settled "$OBS_DIR" "$STRIH_HOST" "$STREAM_HOST" \
      "$OBS_TIMEOUT_S" "$RIG_STATE" "$BROADCAST_READS" "$BROADCAST_RETRY_S")")" in
    idle) ;;
    live) refuse "a broadcast is live before $1" ;;
    *) refuse "the rig state is unreadable before $1 -- nothing is restarted on a rig the matrix cannot observe" ;;
  esac
  st="$(test_mode_state)"
  why="${st#*$'\t'}"
  case "$st" in
    ok) return 0 ;;
    left*)
      [ "$MUTATED" = 1 ] || refuse "the rig is not in TEST mode: $why (run scripts/rig-mode.sh test first)"
      log "STOP: the rig left TEST mode before $1: $why"
      ;;
    *)
      [ "$MUTATED" = 1 ] || refuse "TEST mode cannot be confirmed: $why"
      log "STOP: TEST mode is unreadable before $1: $why -- nothing is restarted on a rig the matrix cannot observe"
      ;;
  esac
  STOPPED=1
  return 1
}
stop_requested() { [ -e "$RUN_DIR/STOP" ]; }
# keepalive_wait SECS -> 0 after SECS, 1 when the STOP file appeared; the lease heartbeat is bumped
keepalive_wait() {
  local end=$(( $(date +%s) + $1 )) left
  while :; do
    if stop_requested; then return 1; fi
    left=$(( end - $(date +%s) ))
    [ "$left" -gt 0 ] || return 0
    lease_keepalive || exit 5
    isleep $(( left < KEEPALIVE_S ? left : KEEPALIVE_S ))
  done
}
# record_step STEP KIND REPEAT TARGET RESTART_EPOCH HEALTHY_EPOCH HEALTHY WINDOW_DIR WINDOW_RC OUTCOME
#   NOTE -> one matrix.tsv row
record_step() {
  python3 "$DECISION" record --tsv "$RUN_DIR/matrix.tsv" --step "$1" --kind "$2" --repeat "$3" \
    --target "$4" --restart-epoch "$5" --healthy-epoch "$6" --healthy "$7" --window-dir "$8" \
    --window-rc "$9" --outcome "${10}" --note "${11:-}" \
    || log "WARNING: could not record step $1 ($2 r$3)"
}
step_recorded() { av_matrix_step_recorded "$RUN_DIR/matrix.tsv" "$1"; }
window_outcome() { av_matrix_window_outcome "$@"; }
window_note() { av_matrix_window_note "$@"; }

# run_window STEP KIND REPEAT TARGET RESTART_EPOCH HEALTHY_EPOCH HEALTHY -> WIN_DIR, WIN_RC,
# and the step's matrix.tsv row. The window is the soak's own one-window run, in the background so
# the lease heartbeat stays fresh while it records + decodes. CUR marks the step until its row is
# written, so a signal in between still records it (cleanup).
run_window() {
  local argv
  CUR=("$@")
  WIN_WAITED=0
  WIN_DIR="$(av_matrix_window_dir "$RUN_DIR" "$1" "$2" "$3")"
  mkdir -p "$WIN_DIR"
  av_matrix_window_argv argv "$SOAK" "$WIN_DIR" "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "$PROBE_BIN_DIR" \
    "$WIN_VERDICT_EXE_LOCAL"
  log "window $1 ($2${3:+ r$3}): ${argv[*]}  (log: $WIN_DIR/soak.log)"
  MUTATED=1
  "${argv[@]}" > "$WIN_DIR/soak.log" 2>&1 &
  SOAK_PID=$!
  while kill -0 "$SOAK_PID" 2>/dev/null; do
    lease_keepalive || exit 5
    isleep "$KEEPALIVE_S"
  done
  WIN_RC=0
  wait "$SOAK_PID" || WIN_RC=$?
  WIN_WAITED=1
  log "window $1 ($2${3:+ r$3}) ended: soak exit $WIN_RC ($(window_outcome "$WIN_RC" "$WIN_DIR"))"
  record_step "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$WIN_DIR" "$WIN_RC" \
    "$(window_outcome "$WIN_RC" "$WIN_DIR")" "$(window_note "$WIN_DIR")"
  CUR=()
  SOAK_PID=""
}

# after_window -> handle the window's exit: a measurement goes on; a window the soak stopped (the rig
# left TEST mode, a low record volume) or refused (4) stops the run with its report; 5 aborts (a
# recording may be left: cleanup keeps the lease); 3/other aborts -- as a refusal (4) while no
# component was restarted yet (a usage/budget error changes nothing).
after_window() {
  case "$(window_outcome "$WIN_RC" "$WIN_DIR")" in
    measured) return 0 ;;
    window_stopped)
      STOPPED=1
      log "STOP: the soak ended the window without a measurement ($(window_note "$WIN_DIR"))"
      return 1
      ;;
    window_refused)
      STOPPED=1
      log "STOP: the window was refused ($(window_note "$WIN_DIR")) -- the rig is not measurable"
      return 1
      ;;
    window_aborted) ABORT_REASON="the window aborted ($(window_note "$WIN_DIR"))"; exit 5 ;;
    *)
      ABORT_REASON="the window failed to run (soak exit $WIN_RC: $(window_note "$WIN_DIR"))"
      # only the soak's own usage/budget error (3, before it touches the rig) with nothing restarted
      # yet is a refusal; anything else (a killed window) may have changed the rig
      if [ "$WIN_RC" = 3 ] && [ ! -s "$RUN_DIR/restarted" ]; then log "STOP: $ABORT_REASON"; exit 4; fi
      exit 5
      ;;
  esac
}

HEALTH_PROBE=""
HEALTH_SCENE=""
probe_health() {  # KIND -> HEALTH_PROBE / HEALTH_SCENE
  local body
  HEALTH_PROBE=""
  HEALTH_SCENE=""
  case "$1" in
    strih-obs)
      HEALTH_PROBE="$(remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" "$(av_matrix_health_remote_cmd strih-obs)" 2>/dev/null || true)"
      HEALTH_SCENE="$(stream_program_scene_read "$OBS_DIR" "$STRIH_HOST" "")"
      ;;
    cambox)
      HEALTH_PROBE="$(remote root "$CAM_PW" "$CAMBOX_IP" "$(av_matrix_health_remote_cmd cambox)" 2>/dev/null || true)"
      ;;
    dantesync)
      body="$(curl -fsS --max-time 10 "http://${DANTE_IP}:8898/status" 2>/dev/null || true)"
      if [ -n "$body" ]; then
        HEALTH_PROBE="$(printf '%s' "$body" | python3 "$DANTE_DECISION" analyze --box-reachable 1 \
          --grandmaster-ip "$GM_IP" --now "$(date +%s)" 2>/dev/null || true)"
      fi
      ;;
    stream-obs)
      HEALTH_SCENE="$(stream_program_scene_read "$OBS_DIR" "$STREAM_HOST" "")"
      ;;
  esac
}
HEALTHY_EPOCH=""
HEALTH_NOTE=""
# wait_healthy KIND -> 0 healthy (HEALTHY_EPOCH set) | 1 the bound passed | 2 the stream OBS came
# back on another program scene (never healthy: the matrix stops at once, it never switches it back)
wait_healthy() {
  local deadline=$(( $(date +%s) + HEALTHY_TIMEOUT_S )) extra=""
  HEALTHY_EPOCH=""
  HEALTH_NOTE=""
  case "$1" in
    cambox) extra="$RESTART_INV_BEFORE" ;;
    stream-obs) extra="$STREAM_DEV_SCENE" ;;
  esac
  while :; do
    probe_health "$1"
    if av_matrix_health_ok "$1" "$HEALTH_PROBE" "$HEALTH_SCENE" "$extra"; then
      HEALTHY_EPOCH="$(date +%s)"
      return 0
    fi
    if [ "$1" = stream-obs ] && [ -n "$HEALTH_SCENE" ]; then
      HEALTH_NOTE="the stream OBS came back on program scene '$HEALTH_SCENE', not '$STREAM_DEV_SCENE' -- SUPERVISOR: set the program back to '$STREAM_DEV_SCENE' by hand (never PRO); the matrix never switches a scene"
      return 2
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
      HEALTH_NOTE="last read: ${HEALTH_PROBE//$'\n'/ }${HEALTH_SCENE:+ scene=$HEALTH_SCENE}"
      return 1
    fi
    lease_keepalive || exit 5
    isleep "$POLL_S"
  done
}

RESTART_OUTCOME=""
RESTART_EPOCH=""
RESTART_INV_BEFORE=""
RESTART_NOTE=""
note_restarted() { printf '%s\n' "$1" >> "$RUN_DIR/restarted"; }
# do_restart KIND REPEAT -> RESTART_OUTCOME (ok|failed|not_performed), RESTART_EPOCH (dev1, the
# instant the restart was issued / the supervisor's confirmed instant), RESTART_INV_BEFORE (the
# unit's InvocationID the restart replaced), RESTART_NOTE
do_restart() {
  local kind="$1" rep="$2" out="" confirm step_file deadline
  RESTART_NOTE=""
  RESTART_INV_BEFORE=""
  MUTATED=1
  note_restarted "$kind"
  RESTART_EPOCH="$(date +%s)"
  case "$kind" in
    strih-obs) out="$(remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" "$(av_matrix_restart_remote_cmd strih-obs)" 2>&1 || true)" ;;
    cambox) out="$(remote root "$CAM_PW" "$CAMBOX_IP" "$(av_matrix_restart_remote_cmd cambox)" 2>&1 || true)" ;;
    dantesync) out="$(remote root "$CAM_PW" "$DANTE_IP" "$(av_matrix_restart_remote_cmd dantesync)" 2>&1 || true)" ;;
    stream-obs)
      confirm="$(av_matrix_confirm_path "$RUN_DIR" "$rep")"
      step_file="$RUN_DIR/supervisor-step-stream-obs-r${rep}.txt"
      av_matrix_stream_supervisor_step "$HERE" "$confirm" "$SUPERVISOR_TIMEOUT_S" > "$step_file"
      while IFS= read -r _line; do log "$_line"; done < "$step_file"
      deadline=$(( $(date +%s) + SUPERVISOR_TIMEOUT_S ))
      while [ ! -e "$confirm" ]; do
        if stop_requested; then
          RESTART_OUTCOME=not_performed; RESTART_NOTE="the STOP file before the supervisor confirmed"
          return 0
        fi
        if [ "$(date +%s)" -ge "$deadline" ]; then
          RESTART_OUTCOME=not_performed
          RESTART_NOTE="the supervisor did not confirm the stream OBS restart within ${SUPERVISOR_TIMEOUT_S} s"
          return 0
        fi
        lease_keepalive || exit 5
        isleep "$POLL_S"
      done
      RESTART_OUTCOME=ok
      RESTART_EPOCH="$(av_matrix_confirm_epoch "$confirm")"
      log "the supervisor confirmed the stream OBS restart (at $RESTART_EPOCH)"
      return 0
      ;;
  esac
  printf '%s\n' "$out" | sed 's/^/    [restart] /'
  RESTART_OUTCOME="$(av_matrix_restart_outcome "$kind" "$out")"
  RESTART_INV_BEFORE="$(av_matrix_invocation_before "$out")"
  case "$RESTART_OUTCOME" in
    ok) ;;
    failed) RESTART_NOTE="$(printf '%s\n' "$out" | tail -n 1 | cut -c1-200)" ;;
    *) RESTART_NOTE="no restart marker ($(printf '%s' "$out" | tr '\n' ' ' | cut -c1-200))" ;;
  esac
  return 0
}

cleanup() {
  local rc=$? rrc=2 k stuck=""
  set +e
  trap '' INT TERM HUP PIPE
  IN_CLEANUP=1
  if [ -n "$SLEEP_PID" ]; then kill -TERM "$SLEEP_PID" 2>/dev/null; fi
  if [ "$SETUP_STARTED" = 1 ]; then
    log "cleanup${ABORT_REASON:+ (stopping: $ABORT_REASON)}" 2>/dev/null
    if [ "${#CUR[@]}" -gt 0 ]; then
      # a window was running (or had just ended) when the run stopped: let it finish its own
      # cleanup, then record its step once
      if [ "$WIN_WAITED" = 0 ] && [ -n "$SOAK_PID" ]; then
        if kill -0 "$SOAK_PID" 2>/dev/null; then
          log "the running window (pid $SOAK_PID) gets SIGTERM -- waiting for its own cleanup" 2>/dev/null
          kill -TERM "$SOAK_PID" 2>/dev/null
        fi
        wait "$SOAK_PID" 2>/dev/null
        WIN_RC=$?
      fi
      if ! step_recorded "${CUR[0]}"; then
        if [ "$WIN_WAITED" = 1 ]; then
          record_step "${CUR[0]}" "${CUR[1]}" "${CUR[2]}" "${CUR[3]}" "${CUR[4]}" "${CUR[5]}" "${CUR[6]}" \
            "$WIN_DIR" "$WIN_RC" "$(window_outcome "$WIN_RC" "$WIN_DIR")" "$(window_note "$WIN_DIR")" 2>/dev/null
        else
          record_step "${CUR[0]}" "${CUR[1]}" "${CUR[2]}" "${CUR[3]}" "${CUR[4]}" "${CUR[5]}" "${CUR[6]}" \
            "$WIN_DIR" "$WIN_RC" window_aborted "stopped: ${ABORT_REASON:-a signal}" 2>/dev/null
        fi
      fi
      CUR=()
      SOAK_PID=""
    fi
    for k in $(jobs -p); do kill -TERM "$k" 2>/dev/null; done
    wait 2>/dev/null
    if [ -s "$RUN_DIR/restarted" ]; then
      for k in $(sort -u "$RUN_DIR/restarted"); do ensure_running "$k" 2>/dev/null; done
    fi
    python3 "$DECISION" report --dir "$RUN_DIR" --json "$RUN_DIR/report.json" "${SPREAD_ARGS[@]}" \
      > "$RUN_DIR/report.txt" 2>&1
    rrc=$?
    cat "$RUN_DIR/report.txt" 2>/dev/null
  fi
  for k in "$RUN_DIR"/w-*; do
    [ -d "$k" ] || continue
    av_matrix_window_stuck "$k" && stuck="${stuck:+$stuck }${k##*/}"
  done
  if [ -n "$stuck" ]; then
    echo "av-restart-matrix: ERROR: a window may have left a recording ($stuck) -- run: bash $0 --stop-leftovers $RUN_DIR" >&2 2>/dev/null
    if [ "$LEASE_HELD" = 1 ]; then
      log "rig lease KEPT ($RIG_LEASE_OURS) -- no E2E may start over it; --stop-leftovers releases it once nothing is left" 2>/dev/null
    fi
    exit 5
  fi
  if [ "$LEASE_HELD" = 1 ]; then
    rig_lease_release "$RIG_LEASE_OURS" >/dev/null 2>&1
    log "rig lease released (a lease another run holds is never touched)" 2>/dev/null
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
log "run dir $RUN_DIR; kinds: $KINDS x $REPEATS; cameras: $SOAK_CAMS"
set +e
lease_out="$(rig_lease_acquire "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" "" av-restart-matrix "$LEASE_EXPECTED_AT")"
lease_rc=$?
set -e
log "$lease_out"
[ "$lease_rc" -eq 0 ] || die 4 "the rig lease is held (${lease_out#RIG_LEASE_HELD_BY=}) -- rerun when it is free"
LEASE_HELD=1
SETUP_STARTED=1

# ---- the baseline ----
if before_mutation "the baseline window"; then
  run_window 0 baseline 0 - "" "" ""
  if after_window; then
    bv="$(python3 "$DECISION" grade-window --window-dir "$WIN_DIR" "${SPREAD_ARGS[@]}" 2>/dev/null | sed -n '1s/^verdict=//p' || true)"
    log "baseline: ${bv:-ungradable}"
    if [ "$bv" != PASS ] && [ "$KEEP_GOING" != 1 ]; then
      log "STOP: the baseline is ${bv:-ungradable} -- no component is restarted on a rig that does not pass before any restart (--keep-going runs the restarts anyway)"
      STOPPED=1
    fi
  fi
fi

# ---- the restarts ----
STEP=0
if [ "$STOPPED" = 0 ]; then
  for KIND in $KINDS; do
    for ((R = 1; R <= REPEATS; R++)); do
      STEP=$(( STEP + 1 ))
      if stop_requested; then
        log "STOP file found -- ending the run"
        ABORT_REASON="the STOP file"; STOPPED=1; break 2
      fi
      TARGET="$(target_of "$KIND")"
      before_mutation "the $KIND r$R restart" || break 2
      log "restart $KIND r$R/$REPEATS on $TARGET"
      do_restart "$KIND" "$R"
      if [ "$RESTART_OUTCOME" != ok ]; then
        _o=restart_failed
        [ "$RESTART_OUTCOME" = failed ] || _o=not_performed
        log "STOP: $KIND r$R restart $RESTART_OUTCOME -- $RESTART_NOTE"
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "" "" "" "" "$_o" "$RESTART_NOTE"
        STOPPED=1; break 2
      fi
      _hrc=0
      wait_healthy "$KIND" || _hrc=$?
      if [ "$_hrc" != 0 ]; then
        if [ "$_hrc" = 2 ]; then
          log "STOP: $KIND r$R: $HEALTH_NOTE"
        else
          log "STOP: $KIND r$R did not report healthy within ${HEALTHY_TIMEOUT_S} s ($HEALTH_NOTE)"
        fi
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "" 0 "" "" not_healthy "$HEALTH_NOTE"
        STOPPED=1; break 2
      fi
      log "$KIND r$R healthy after $(( HEALTHY_EPOCH - RESTART_EPOCH )) s -- settling ${SETTLE_S} s"
      if ! keepalive_wait "$SETTLE_S"; then
        log "STOP file found during the settle -- ending the run"
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "$HEALTHY_EPOCH" 1 "" "" not_performed \
          "the STOP file before the window"
        ABORT_REASON="the STOP file"; STOPPED=1; break 2
      fi
      run_window "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "$HEALTHY_EPOCH" 1
      after_window || break 2
    done
  done
fi
[ "$STOPPED" = 1 ] || LOOP_DONE=1
exit 0
