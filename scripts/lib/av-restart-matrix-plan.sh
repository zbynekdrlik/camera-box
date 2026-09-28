#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs this in
# the CALLER's shell (scripts/av-restart-matrix.sh, which sets its own strict mode).
#
# scripts/lib/av-restart-matrix-plan.sh -- issue 1367: the restart matrix's `--plan` printout, split
# out of scripts/av-restart-matrix.sh to keep the orchestrator under the file budget. It is a VIEW of
# the orchestrator's resolved configuration, so print_plan reads the orchestrator's globals (KINDS,
# REPEATS, SETTLE_S, HEALTHY_TIMEOUT_S, POLL_S, SUPERVISOR_TIMEOUT_S, WINDOW_BOUND_S, SOAK_SLOT_S,
# EXPECTED_S, N_RESTARTS, the targets, RUN_DIR, the lease identity, the paths) -- set before it is
# called. Every command it prints comes from the SAME builders --run executes
# (scripts/lib/av-restart-matrix.sh, the soak's lib), so plan == run. It runs nothing on the rig.

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
one window <= ${WINDOW_BOUND_S} s (the soak's own slot ${SOAK_SLOT_S} s + 600 s setup/cleanup); the lease's hold ceiling ${EXPECTED_S} s
targets:
EOF
  for k in $KINDS; do
    case "$k" in
      strih-obs) echo "  strih-obs = strih-lx ${STRIH_HOST} (strih-obs.service, the --user unit; $(strih_platform "$STRIH_HOST"))" ;;
      cambox) echo "  cambox = ${CAMBOX} ${CAMBOX_IP} (camera-box.service)" ;;
      dantesync) echo "  dantesync = ${DANTE_NODE} ${DANTE_IP} (dantesync.service; with no --dantesync-node it follows the cambox)" ;;
      stream-obs) echo "  stream-obs = stream ${STREAM_HOST} (SUPERVISOR step: no session-agnostic path launches the canonical stream OBS)" ;;
    esac
  done
  cat <<EOF
run dir: ${RUN_DIR}  (matrix.conf, matrix.tsv, w-NN-<kind>-rR/ = one soak run each, report.txt/json, restarted, lease, pid, STOP)

SETUP:
  1. rig lease (issue 830): rig_lease_acquire repo=${RIG_LEASE_REPO_NAME} run_id=${RIG_LEASE_OURS} job=av-restart-matrix expected_release_at=${LEASE_EXPECTED_AT}
     a live foreign holder -> refuse (exit 4); kept alive with rig_lease_refresh_if_mine (issue 1383) every <= ${KEEPALIVE_S} s
     for the whole run under RIG_LEASE_MAX_HOLD_SECS=${EXPECTED_S} (exported: every window's own keep-alive beats this lease under it)
  2. before EVERY mutation (the baseline window, each restart), read-only:
     the lease still ours: rig_lease_refresh_if_mine ${RIG_LEASE_REPO_NAME} ${RIG_LEASE_OURS} (1 not ours / 3 past the ceiling -> exit 5, the
       other lease untouched; 2 a filesystem error -> retried at the next beat)
     the rig-busy guard: stray_session_check_assert ${OBS_DIR} ${STRIH_HOST} ${STREAM_HOST} '<step>'
     a proven idle rig (av_soak_rig_busy_settled: rig-busy-check | av_soak_rig_state.py broadcast = idle; unreadable retried ${BROADCAST_READS}x ${BROADCAST_RETRY_S} s apart)
       busy / live / unreadable -> nothing is restarted (exit 4 before any change, else 5)
     TEST mode: the stream program is '${STREAM_DEV_SCENE}' ($(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STREAM_HOST"))
       AND the cam2 painter emits (ssh root@${PAINTER_IP}: $(av_soak_painter_probe_cmd "$MARKER_LOG"))
       out of TEST mode -> refused before any change (exit 4), else the run ends with its report

BASELINE -- ONE window = the soak's own one-window run under the matrix's lease:
EOF
  av_matrix_window_argv argv "$SOAK" "$(av_matrix_window_dir "$RUN_DIR" 0 baseline 0)" "$RIG_LEASE_REPO_NAME" \
    "$RIG_LEASE_OURS" "${PROBE_BIN_DIR:-}" "${WIN_VERDICT_EXE_LOCAL:-}"
  plan_cmd "${argv[@]}"
  cat <<EOF
      (bash ${HERE}/av-soak.sh --plan --hours 0 prints every step of one window: TEST-mode reads, the
      connect-on-show hold, the burns, ONE strih-program sweep recorded on strih + stream, the in-place
      decodes, the merge, its cleanup)
      graded: python3 ${DECISION} grade-window --window-dir <dir>
      a baseline that is not PASS stops the matrix before any restart (--keep-going runs them anyway)

PER RESTART, kind by kind, repeat r = 1..${REPEATS} (window dir w-NN-<kind>-rR):
  a. <run-dir>/STOP? then the guard + the idle proof (2.)
  b. restart ONE component:
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
        av_matrix_health_remote_cmd cambox | plan_text
        echo "             active AND a NEW invocation (not the one the restart replaced) AND >= 1 of its Streaming: lines" ;;
      dantesync)
        echo "     dantesync: curl http://${DANTE_IP}:8898/status | python3 ${DANTE_DECISION} analyze --box-reachable 1 --grandmaster-ip <rig grandmaster> --now <now> -> verdict=OK" ;;
      stream-obs)
        echo "     stream-obs: the OBS WebSocket answers on '${STREAM_DEV_SCENE}': $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" program-scene --host "$STREAM_HOST")"
        echo "                 another program scene -> the repeat FAILS at once, no settle, no window, never switched back" ;;
    esac
  done
  cat <<EOF
     never healthy -> the repeat FAILS and the matrix stops; a failed restart FAILS and stops; a
     restart never performed (no unit, ssh unreachable, no supervisor confirmation) is UNKNOWN and stops
  d. settle ${SETTLE_S} s (heartbeat kept), then ONE window exactly like the baseline; a window the soak
     stops without a measurement (it left TEST mode, a low record volume) or refuses ends the run
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
