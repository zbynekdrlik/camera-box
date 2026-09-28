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
#   - before every mutation: the shared issue-1271 rig-busy guard AND a proven idle rig (the soak's
#     own `av_soak_rig_state.py broadcast` decision over rig-busy-check, an unreadable read retried
#     AV_SOAK_BROADCAST_READS x AV_SOAK_BROADCAST_RETRY_S) -- nothing restarts while a box records
#     or streams;
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
#                   cambox reboot is banned); healthy = active AND a `Streaming:` journal line since
#                   the box's own restart instant.
#     dantesync  -- `systemctl restart dantesync` on ONE camera node; healthy = its :8898/status
#                   graded OK by dantesync_clock_decision.py analyze (locked on the rig grandmaster).
#     stream-obs -- a SUPERVISOR step: the stream box's canonical launch is its `OBS Studio.lnk` ->
#                   obs-guarded-launch.ps1; no session-agnostic path runs it (every interactive-token
#                   OBS task there launches a bare obs64.exe), an ssh GUI launch is banned. The run
#                   writes the exact program to <run-dir>/supervisor-step-stream-obs-rN.txt, waits
#                   (bounded) for <run-dir>/confirm-stream-obs-rN, then grades it like the others.
#   A component that never reports healthy, or whose restart fails, is a FAIL and stops the run; a
#   baseline that is not PASS stops it before any restart (--keep-going runs the restarts anyway).
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
      if [ -n "$scene" ]; then
        log "leave running: the stream OBS answers (program '$scene')"
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
# one window's upper bound: the soak's own slot (600 s) + its setup/cleanup (~300 s)
EXPECTED_S="$(av_matrix_expected_duration_s "$N_STEPS" 900 "$SETTLE_S" "$HEALTHY_TIMEOUT_S" "$SUPERVISOR_TIMEOUT_S" "$N_STREAM")"
LEASE_EXPECTED_AT="$(date -u -d "+${EXPECTED_S} seconds" +%Y-%m-%dT%H:%M:%SZ)"

target_of() {
  case "$1" in
    strih-obs) printf 'strih-lx %s\n' "$STRIH_HOST" ;;
    cambox) printf '%s %s\n' "$CAMBOX" "$CAMBOX_IP" ;;
    dantesync) printf '%s %s\n' "$DANTE_NODE" "$DANTE_IP" ;;
    stream-obs) printf 'stream %s\n' "$STREAM_HOST" ;;
  esac
}

# --- --plan: print every step, touch nothing ---------------------------------------------------------

plan_cmd() { printf '      '; printf '%q ' "$@"; printf '\n'; }
plan_text() { sed 's/^/      /'; }

print_plan() {
  local k argv bounds_line
  bounds_line="$(python3 "$SOAK_DECISION" bounds 2>&1)" || bounds_line="UNREADABLE: $bounds_line"
  cat <<EOF
===== av-restart-matrix PLAN (issue 1367) -- nothing below is executed; run with --run =====
bounds (read from their single sources): ${bounds_line//$'\n'/; }; graded spread: ${AV_SOAK_SPREAD_COLUMNS:-av_spread_ms}
kinds: ${KINDS} x ${REPEATS} repeats = ${N_RESTARTS} restarts + 1 baseline window (each kind PASS only ${REPEATS}/${REPEATS})
settle after healthy: ${SETTLE_S} s; healthy bound: ${HEALTHY_TIMEOUT_S} s (polled every ${POLL_S} s); supervisor-step bound: ${SUPERVISOR_TIMEOUT_S} s
targets:
EOF
  for k in $KINDS; do
    case "$k" in
      strih-obs) echo "  strih-obs = strih-lx ${STRIH_HOST} (strih-obs.service, the --user unit; $(strih_platform "$STRIH_HOST"))" ;;
      cambox) echo "  cambox = ${CAMBOX} ${CAMBOX_IP} (camera-box.service)" ;;
      dantesync) echo "  dantesync = ${DANTE_NODE} ${DANTE_IP} (dantesync.service)" ;;
      stream-obs) echo "  stream-obs = stream ${STREAM_HOST} (SUPERVISOR step: no session-agnostic path launches the canonical stream OBS)" ;;
    esac
  done
  cat <<EOF
run dir: ${RUN_DIR}  (matrix.conf, matrix.tsv, w-NN-<kind>-rR/ = one soak run each, report.txt/json, restarted, lease, pid, STOP)

SETUP:
  1. rig lease (issue 830): rig_lease_acquire repo=${RIG_LEASE_REPO_NAME} run_id=${RIG_LEASE_OURS} job=av-restart-matrix expected_release_at=${LEASE_EXPECTED_AT}
     a live foreign holder -> refuse (exit 4); the heartbeat is bumped every <= ${KEEPALIVE_S} s for the whole run, also while a window runs
  2. before EVERY mutation (the baseline window, each restart): the rig-busy guard
     stray_session_check_assert ${OBS_DIR} ${STRIH_HOST} ${STREAM_HOST} '<step>'
     AND a proven idle rig (obs_phase2.py rig-busy-check | av_soak_rig_state.py broadcast = idle; an unreadable read retried ${BROADCAST_READS}x ${BROADCAST_RETRY_S} s apart)
     busy / live / unreadable -> nothing is restarted (exit 4 before any change, else 5)

BASELINE -- ONE window = the soak's own one-window run under the matrix's lease:
EOF
  av_matrix_window_argv argv "$SOAK" "$(av_matrix_window_dir "$RUN_DIR" 0 baseline 0)" "$RIG_LEASE_OURS" \
    "${PROBE_BIN_DIR:-}" "${WIN_VERDICT_EXE_LOCAL:-}"
  plan_cmd "${argv[@]}"
  cat <<EOF
      (bash ${HERE}/av-soak.sh --plan --hours 0 prints every step of one window: TEST-mode reads, the
      connect-on-show hold, the burns, ONE strih-program sweep recorded on strih + stream, the in-place
      decodes, the merge, its cleanup)
      graded: python3 ${DECISION} grade-window --window-dir <dir>
      a baseline that is not PASS stops the matrix before any restart (--keep-going runs them anyway)

PER RESTART, kind by kind, repeat r = 1..${REPEATS} (window dir w-NN-<kind>-rR):
  a. <run-dir>/STOP? then the guard + the idle proof (2.)
  b. restart ONE component (its own restart instant is echoed first):
EOF
  for k in $KINDS; do
    case "$k" in
      strih-obs)
        echo "     strih-obs: ssh <STRIH_USER>@${STRIH_HOST}:"
        av_matrix_restart_remote_cmd strih-obs | plan_text ;;
      cambox)
        echo "     cambox: ssh root@${CAMBOX_IP}:"
        av_matrix_restart_remote_cmd cambox | plan_text ;;
      dantesync)
        echo "     dantesync: ssh root@${DANTE_IP}:"
        av_matrix_restart_remote_cmd dantesync | plan_text ;;
      stream-obs)
        echo "     stream-obs: the SUPERVISOR step, written to <run-dir>/supervisor-step-stream-obs-rR.txt:"
        av_matrix_stream_supervisor_step "$HERE" "$(av_matrix_confirm_path "$RUN_DIR" 1)" "$SUPERVISOR_TIMEOUT_S" | plan_text ;;
    esac
  done
  echo "  c. healthy (bounded ${HEALTHY_TIMEOUT_S} s, polled every ${POLL_S} s; time to healthy is recorded):"
  for k in $KINDS; do
    case "$k" in
      strih-obs)
        echo "     strih-obs: ssh <STRIH_USER>@${STRIH_HOST}: $(av_matrix_health_remote_cmd strih-obs)"
        echo "                AND the OBS WebSocket answers: $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STRIH_HOST")" ;;
      cambox)
        echo "     cambox: ssh root@${CAMBOX_IP}:"
        av_matrix_health_remote_cmd cambox 0 | sed "s/@0 /@<the box restart instant> /" | plan_text
        echo "             active AND >= 1 Streaming: line since the box's own restart instant" ;;
      dantesync)
        echo "     dantesync: curl http://${DANTE_IP}:8898/status | python3 ${DANTE_DECISION} analyze --box-reachable 1 --grandmaster-ip <rig grandmaster> --now <now> -> verdict=OK" ;;
      stream-obs)
        echo "     stream-obs: the OBS WebSocket answers: $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STREAM_HOST")" ;;
    esac
  done
  cat <<EOF
     never healthy -> the repeat FAILS and the matrix stops; a failed restart FAILS and stops; a
     restart never performed (no unit, ssh unreachable, no supervisor confirmation) is UNKNOWN and stops
  d. settle ${SETTLE_S} s (heartbeat kept), then ONE window exactly like the baseline
  e. one matrix.tsv row: $(printf '%q ' python3 "$DECISION" record --tsv "$RUN_DIR/matrix.tsv" --step '<N>' --kind '<kind>' --repeat '<r>' '...')

CLEANUP (every exit; signals ignored): a running window gets SIGTERM and its own cleanup is waited for;
  every restarted component is left running:
EOF
  for k in $KINDS; do
    case "$k" in
      stream-obs) echo "     stream-obs: read its WebSocket; not answering -> the supervisor relaunch step is printed" ;;
      *) echo "     ${k}: $(av_matrix_ensure_running_remote_cmd "$k" | tr '\n' ' ')" ;;
    esac
  done
  echo "  the report:"
  plan_cmd python3 "$DECISION" report --dir "$RUN_DIR" --json "$RUN_DIR/report.json" "${SPREAD_ARGS[@]}"
  echo "  rig_lease_release ${RIG_LEASE_OURS} -- KEPT while a window may have left a recording: bash $0 --stop-leftovers <run-dir>"
  echo
  echo "NEVER: a reboot (only ONE service restart per step), a latency pin, an audio sync offset, a correction, a scene switch."
  local missing=()
  [ -n "${CAM_PW:-}" ] || missing+=(CAM_PW)
  [ -n "${STREAM_USER:-}" ] || missing+=(STREAM_USER)
  [ -n "${STREAM_PW:-}" ] || missing+=(STREAM_PW)
  [ -n "${STRIH_USER:-}" ] || missing+=(STRIH_USER)
  [ -n "${STRIH_PW:-}" ] || missing+=(STRIH_PW)
  [ -n "${PROBE_BIN_DIR:-}" ] && [ -x "$PROBE_BIN_DIR/recording-verdict" ] || missing+=("PROBE_BIN_DIR (recording-verdict)")
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

SETUP_STARTED=0
MUTATED=0
LEASE_HELD=0
LOOP_DONE=0
STOPPED=0
SOAK_PID=""
CUR=()          # the step a running window belongs to: STEP KIND REPEAT TARGET RESTART_EPOCH HEALTHY_EPOCH
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
# rig_state_settled -> live | unknown | idle (the soak's pure decision over one rig-busy read, an
# unreadable read retried BROADCAST_READS x BROADCAST_RETRY_S)
rig_state_settled() {
  local i out b=unknown
  for ((i = 1; i <= BROADCAST_READS; i++)); do
    out="$(timeout "$OBS_TIMEOUT_S" python3 "$OBS_DIR/obs_phase2.py" rig-busy-check --strih-host "$STRIH_HOST" \
      --stream-host "$STREAM_HOST" --password "${OBS_PASSWORD:-}" 2>/dev/null || true)"
    b="$(printf '%s' "$out" | python3 "$RIG_STATE" broadcast 2>/dev/null || echo unknown)"
    if [ "$b" != unknown ]; then break; fi
    if [ "$i" -lt "$BROADCAST_READS" ]; then sleep "$BROADCAST_RETRY_S"; fi
  done
  printf '%s\n' "$b"
}
before_mutation() {  # LABEL -> returns only on a guard-passed, proven idle rig
  guard_ok "$1" || refuse "the rig is busy (a box records or streams) before $1"
  case "$(rig_state_settled)" in
    idle) ;;
    live) refuse "a broadcast is live before $1" ;;
    *) refuse "the rig state is unreadable before $1 -- nothing is restarted on a rig the matrix cannot observe" ;;
  esac
}
stop_requested() { [ -e "$RUN_DIR/STOP" ]; }
# keepalive_wait SECS -> 0 after SECS, 1 when the STOP file appeared; the lease heartbeat is bumped
keepalive_wait() {
  local end=$(( $(date +%s) + $1 )) left
  while :; do
    if stop_requested; then return 1; fi
    left=$(( end - $(date +%s) ))
    [ "$left" -gt 0 ] || return 0
    rig_lease_heartbeat_touch
    isleep $(( left < KEEPALIVE_S ? left : KEEPALIVE_S ))
  done
}
record_step() {  # STEP KIND REPEAT TARGET RESTART_EPOCH HEALTHY_EPOCH HEALTHY WINDOW_DIR WINDOW_RC OUTCOME NOTE
  python3 "$DECISION" record --tsv "$RUN_DIR/matrix.tsv" --step "$1" --kind "$2" --repeat "$3" \
    --target "$4" --restart-epoch "$5" --healthy-epoch "$6" --healthy "$7" --window-dir "$8" \
    --window-rc "$9" --outcome "${10}" --note "${11:-}" \
    || log "WARNING: could not record step $1 ($2 r$3)"
}
window_outcome() {  # SOAK_RC -> the step outcome
  case "$1" in
    0 | 1 | 2) echo measured ;;
    4) echo window_refused ;;
    5) echo window_aborted ;;
    *) echo window_error ;;
  esac
}
window_note() {  # WINDOW_DIR -> the soak's own last ERROR/STOP line (why it refused/aborted)
  grep -E 'ERROR|STOP:|refus' "$1/soak.log" 2>/dev/null | tail -n 1 | cut -c1-300 || true
}

# run_window STEP KIND REPEAT -> WIN_DIR, WIN_RC. The window is the soak's own one-window run, in the
# background so the lease heartbeat stays fresh while it records + decodes.
run_window() {
  local argv
  WIN_DIR="$(av_matrix_window_dir "$RUN_DIR" "$1" "$2" "$3")"
  mkdir -p "$WIN_DIR"
  av_matrix_window_argv argv "$SOAK" "$WIN_DIR" "$RIG_LEASE_OURS" "$PROBE_BIN_DIR" "$WIN_VERDICT_EXE_LOCAL"
  log "window $1 ($2${3:+ r$3}): ${argv[*]}  (log: $WIN_DIR/soak.log)"
  MUTATED=1
  "${argv[@]}" > "$WIN_DIR/soak.log" 2>&1 &
  SOAK_PID=$!
  while kill -0 "$SOAK_PID" 2>/dev/null; do
    rig_lease_heartbeat_touch
    isleep "$KEEPALIVE_S"
  done
  WIN_RC=0
  wait "$SOAK_PID" || WIN_RC=$?
  SOAK_PID=""
  log "window $1 ($2${3:+ r$3}) ended: soak exit $WIN_RC ($(window_outcome "$WIN_RC"))"
}

# after_window -> handle the window's exit: 5 = abort (a recording may be left: the lease is kept by
# cleanup), 4 = the rig is not measurable (stop, report), 3/other = abort.
after_window() {
  case "$WIN_RC" in
    0 | 1 | 2) return 0 ;;
    4) STOPPED=1; log "STOP: the window was refused ($(window_note "$WIN_DIR")) -- the rig is not measurable"; return 1 ;;
    5) ABORT_REASON="the window aborted ($(window_note "$WIN_DIR"))"; exit 5 ;;
    *) ABORT_REASON="the window failed to run (soak exit $WIN_RC)"; exit 5 ;;
  esac
}

HEALTH_PROBE=""
HEALTH_SCENE=""
probe_health() {  # KIND SINCE_EPOCH -> HEALTH_PROBE / HEALTH_SCENE
  local body
  HEALTH_PROBE=""
  HEALTH_SCENE=""
  case "$1" in
    strih-obs)
      HEALTH_PROBE="$(remote "$STRIH_USER" "$STRIH_PW" "$STRIH_HOST" "$(av_matrix_health_remote_cmd strih-obs)" 2>/dev/null || true)"
      HEALTH_SCENE="$(stream_program_scene_read "$OBS_DIR" "$STRIH_HOST" "")"
      ;;
    cambox)
      HEALTH_PROBE="$(remote root "$CAM_PW" "$CAMBOX_IP" "$(av_matrix_health_remote_cmd cambox "$2")" 2>/dev/null || true)"
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
wait_healthy() {  # KIND SINCE_EPOCH -> 0 (HEALTHY_EPOCH set) | 1 (the bound passed)
  local deadline=$(( $(date +%s) + HEALTHY_TIMEOUT_S ))
  HEALTHY_EPOCH=""
  while :; do
    probe_health "$1" "$2"
    if av_matrix_health_ok "$1" "$HEALTH_PROBE" "$HEALTH_SCENE"; then
      HEALTHY_EPOCH="$(date +%s)"
      return 0
    fi
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    rig_lease_heartbeat_touch
    isleep "$POLL_S"
  done
}

RESTART_OUTCOME=""
RESTART_EPOCH=""
RESTART_SINCE=""
RESTART_NOTE=""
note_restarted() { printf '%s\n' "$1" >> "$RUN_DIR/restarted"; }
# do_restart KIND REPEAT -> RESTART_OUTCOME (ok|failed|not_performed), RESTART_EPOCH (dev1, the
# instant the restart was issued / the supervisor's confirmed instant), RESTART_SINCE (the box's own
# restart instant, for its journal), RESTART_NOTE
do_restart() {
  local kind="$1" rep="$2" out="" confirm step_file deadline
  RESTART_NOTE=""
  RESTART_SINCE=""
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
        rig_lease_heartbeat_touch
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
  RESTART_SINCE="$(av_matrix_restart_at "$out")"
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
    if [ -n "$SOAK_PID" ] && kill -0 "$SOAK_PID" 2>/dev/null; then
      log "the running window (pid $SOAK_PID) gets SIGTERM -- waiting for its own cleanup" 2>/dev/null
      kill -TERM "$SOAK_PID" 2>/dev/null
      wait "$SOAK_PID" 2>/dev/null
      WIN_RC=$?
      SOAK_PID=""
      if [ "${#CUR[@]}" -gt 0 ]; then
        record_step "${CUR[0]}" "${CUR[1]}" "${CUR[2]}" "${CUR[3]}" "${CUR[4]}" "${CUR[5]}" "${CUR[6]}" \
          "$WIN_DIR" "$WIN_RC" window_aborted "stopped: ${ABORT_REASON:-a signal}" 2>/dev/null
      fi
    fi
    if [ -s "$RUN_DIR/restarted" ]; then
      for k in $(sort -u "$RUN_DIR/restarted"); do ensure_running "$k" 2>/dev/null; done
    fi
  fi
  if [ "$SETUP_STARTED" = 1 ]; then
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
before_mutation "the baseline window"
CUR=(0 baseline 0 - "" "" "")
run_window 0 baseline 0
CUR=()
record_step 0 baseline 0 - "" "" "" "$WIN_DIR" "$WIN_RC" "$(window_outcome "$WIN_RC")" "$(window_note "$WIN_DIR")"
after_window || true
if [ "$STOPPED" = 0 ]; then
  bv="$(python3 "$DECISION" grade-window --window-dir "$WIN_DIR" "${SPREAD_ARGS[@]}" 2>/dev/null | sed -n '1s/^verdict=//p' || true)"
  log "baseline: ${bv:-ungradable}"
  if [ "$bv" != PASS ] && [ "$KEEP_GOING" != 1 ]; then
    log "STOP: the baseline is ${bv:-ungradable} -- no component is restarted on a rig that does not pass before any restart (--keep-going runs the restarts anyway)"
    STOPPED=1
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
      before_mutation "the $KIND r$R restart"
      log "restart $KIND r$R/$REPEATS on $TARGET"
      do_restart "$KIND" "$R"
      if [ "$RESTART_OUTCOME" != ok ]; then
        _o=restart_failed
        [ "$RESTART_OUTCOME" = failed ] || _o=not_performed
        log "STOP: $KIND r$R restart $RESTART_OUTCOME -- $RESTART_NOTE"
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "" "" "" "" "$_o" "$RESTART_NOTE"
        STOPPED=1; break 2
      fi
      if ! wait_healthy "$KIND" "$RESTART_SINCE"; then
        log "STOP: $KIND r$R did not report healthy within ${HEALTHY_TIMEOUT_S} s (last read: ${HEALTH_PROBE//$'\n'/ } ${HEALTH_SCENE:+scene=$HEALTH_SCENE})"
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "" 0 "" "" not_healthy \
          "last read: ${HEALTH_PROBE//$'\n'/ } ${HEALTH_SCENE:+scene=$HEALTH_SCENE}"
        STOPPED=1; break 2
      fi
      log "$KIND r$R healthy after $(( HEALTHY_EPOCH - RESTART_EPOCH )) s -- settling ${SETTLE_S} s"
      if ! keepalive_wait "$SETTLE_S"; then
        log "STOP file found during the settle -- ending the run"
        record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "$HEALTHY_EPOCH" 1 "" "" not_performed \
          "the STOP file before the window"
        ABORT_REASON="the STOP file"; STOPPED=1; break 2
      fi
      CUR=("$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "$HEALTHY_EPOCH" 1)
      run_window "$STEP" "$KIND" "$R"
      CUR=()
      record_step "$STEP" "$KIND" "$R" "$TARGET" "$RESTART_EPOCH" "$HEALTHY_EPOCH" 1 "$WIN_DIR" "$WIN_RC" \
        "$(window_outcome "$WIN_RC")" "$(window_note "$WIN_DIR")"
      after_window || break 2
    done
  done
fi
[ "$STOPPED" = 1 ] || LOOP_DONE=1
exit 0
