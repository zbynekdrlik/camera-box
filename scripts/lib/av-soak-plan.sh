#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs this in
# the CALLER's shell (scripts/av-soak.sh, which sets its own strict mode).
#
# scripts/lib/av-soak-plan.sh -- the 8 h soak's `--plan` printout (issue 1367), moved verbatim out of
# scripts/av-soak.sh to keep the orchestrator under the file budget (issue 1474, the restart
# matrix's av-restart-matrix-plan.sh shape). It is a VIEW of the soak's resolved configuration, so
# print_plan reads the soak's globals and calls its plan_cmd -- set before it is called. Every command
# it prints comes from the SAME builders --run executes (scripts/lib/av-soak.sh), so plan == run. It
# runs nothing on the rig.

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
    echo "NOTE: --run refuses a Windows strih (retired at M4); only the strih-snv in-place decode is supported."
  fi
  echo
  echo "SETUP (once) -- reads first, nothing changes until step 6:"
  if [ -n "$LEASE_INHERITED" ]; then
    echo "  1. rig lease (issue 830): the caller's rig lease ${LEASE_INHERITED} (repo ${RIG_LEASE_REPO_NAME}) -- verified + refreshed with rig_lease_refresh_if_mine (else refuse, exit 4), never acquired or released here"
    echo "     rig_heartbeat_start av-soak ${RIG_LEASE_REPO_NAME} ${RIG_LEASE_OURS} (issue 281; every beat refreshes the caller's lease under ${LEASE_CEILING_NOTE}, issue 1383)"
  else
    echo "  1. rig lease (issue 830): rig_lease_acquire repo=${RIG_LEASE_REPO_NAME} run_id=${RIG_LEASE_OURS} job=av-soak expected_release_at=${LEASE_EXPECTED_AT} resources=${RIG_LEASE_DECLARED}"
    echo "     a live foreign holder -> refuse (exit 4); rig_heartbeat_start av-soak ${RIG_LEASE_REPO_NAME} ${RIG_LEASE_OURS} (issue 281; every beat refreshes this lease, issue 1383)"
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
  echo "  6. the rig-busy guard again, then the strih program transition -> Cut (issue 1367, the E2E's strih-transition.sh;"
  echo "     the operator's transition saved durably, a failure refuses with exit 4, nothing recorded):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" transition-cut --host "$STRIH_HOST" --save "$(strih_transition_save_path)"
  echo "     then: obs_burn_filter.py add --host <ip> --input <input> for each burn that was OFF, then check it again"
  echo
  echo "EVERY SLOT k (slot start = run start + k x ${SLOT_S} s; files under ${RUN_DIR}/slot-NNN):"
  echo "  a. lease still ours; record volumes free >= ${RECORDINGS_FREE_MIN_GB} GB (curl http://<box>:${BUNDLE_STATE_PORT}/record-dir-stats.json -> bundle_state_gather.recordings_free_line; below = stop the soak)"
  echo "  b. read-only: stream program still '${STREAM_DEV_SCENE}' and the painter service active (else the rig left TEST mode: the run STOPS); an unreadable read or a stalled marker log = a skipped row"
  echo "  c. a proven idle rig (rig-busy-check, an unreadable read retried ${BROADCAST_READS}x ${BROADCAST_RETRY_S} s apart; still unreadable = a skipped row, nothing started),"
  echo "     then the rig-busy guard: stray_session_check_assert ... 'the slot-k first-scene cut' (busy = abort, exit 5),"
  echo "     then the strih program to the FIRST sweep scene, so the recording starts on the input window 0 measures"
  echo "     (each strih camera input has its own burn counter; a failed cut = a skipped row, nothing started):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" switch --host "$STRIH_HOST" --program-scene "$FIRST_SCENE"
  echo "     then the rig-busy guard: stray_session_check_assert ... 'the slot-k StartRecord' (a live broadcast aborts the soak, exit 5)"
  echo "  d. StartRecord (the started flag is set BEFORE the call; any start failure stops both boxes):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action start
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action start
  echo "  e. the sweep -- strih program only (the stream program is never switched); a broadcast check (rig-busy-check) before every cut and before f: live = abort, exit 5, nothing stopped:"
  plan_cmd python3 "$HERE/switch_schedule.py" plan --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S"
  for seg in "${SWEEP_PLAN[@]}"; do
    [ -n "$seg" ] || continue
    scene="${seg%%$'\t'*}"; label="${seg##*$'\t'}"
    echo "      [$label] $(printf '%q ' python3 "$OBS_DIR/obs_phase2.py" switch --host "$STRIH_HOST" --program-scene "$scene"); sleep ${SEGMENT_S}"
  done
  plan_cmd python3 "$HERE/switch_schedule.py" build --sweep "$SWEEP" --segment-secs "$SEGMENT_S" --duration "$WINDOW_S" --start-ns "<first switch ns>" --boundaries "<later switches + stop ns>"
  echo "  f. StopRecord, verified (the flag clears only when 'record --action status' reads active=False):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action stop
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STRIH_HOST" --action status
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action stop
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" record --host "$STREAM_HOST" --action status
  echo "  g. read-only marker-log tail from cam2 (${MARKER_ROWS} rows): $(av_soak_marker_snapshot_cmd "$MARKER_LOG" "$MARKER_ROWS")"
  marker_win="$(av_soak_stream_remote_path "$STREAM_PLATFORM" "$OUT_DIR_WIN" "$STREAM_LX_OUT_DIR" "av-soak-${STAMP}-sNNN-markers.csv")"
  sched_win="$(av_soak_stream_remote_path "$STREAM_PLATFORM" "$OUT_DIR_WIN" "$STREAM_LX_OUT_DIR" "av-soak-${STAMP}-sNNN-switch-schedule.json")"
  partial_win="$(av_soak_stream_remote_path "$STREAM_PLATFORM" "$OUT_DIR_WIN" "$STREAM_LX_OUT_DIR" "av-soak-${STAMP}-sNNN-stream-partial.json")"
  echo "     push to the stream box: $([ "$STREAM_PLATFORM" = linux ] && echo stream_lx_push_file || echo win_ssh_upload) markers -> ${marker_win}; schedule -> ${sched_win}"
  echo "  h. decode in place, both in parallel, each bounded by ${DECODE_TIMEOUT_S} s (a timeout stops that box's recording-verdict):"
  av_soak_strih_extract_argv strih_argv "$STRIH_DECODE" "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
    "$STRIH_LX_OUT_DIR" "$RUN_DIR/slot-NNN" "<strih StopRecord path>" "$STRIH_CAPTURE_FPS" \
    "av-soak-${STAMP}-sNNN-strih-partial.json"
  plan_cmd env STRIH_LX_BOX="$STRIH_HOST" "${strih_argv[@]}"
  if [ "$STREAM_PLATFORM" = linux ]; then
    av_soak_stream_lx_extract_argv stream_argv "$STREAM_DECODE" "${VERDICT_BIN:-<PROBE_BIN_DIR>/recording-verdict}" \
      "$STREAM_LX_OUT_DIR" "$RUN_DIR/slot-NNN" "<stream StopRecord path>" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
      "<painter run_id>" "$marker_win" "$sched_win" "$partial_win"
    plan_cmd env STREAM_LX_BOX="$STREAM_HOST" "${stream_argv[@]}"
  else
  av_soak_stream_extract_argv stream_argv "$STREAM_DECODE" "${WIN_VERDICT_EXE_LOCAL:-<WIN_VERDICT_EXE_LOCAL>}" \
    "$OUT_DIR_WIN" "$RUN_DIR/slot-NNN" "<stream StopRecord path>" "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" \
    "<painter run_id>" "$marker_win" "$sched_win" "$partial_win"
  plan_cmd env STREAM_BOX="$STREAM_HOST" "${stream_argv[@]}"
  fi
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
  echo "  the operator's strih program transition back (when Cut was selected, also during a broadcast -- a setting, not a cut):"
  plan_cmd python3 "$OBS_DIR/obs_phase2.py" transition-restore --host "$STRIH_HOST" --save "$(strih_transition_save_path)"
  echo "  obs_burn_filter.py remove on the burns this run turned on; rig_heartbeat_stop;"
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
  if [ "$STREAM_PLATFORM" != linux ]; then  # issue 1474: only a Windows stream box decodes with the .exe
    [ -n "${WIN_VERDICT_EXE_LOCAL:-}" ] && [ -f "$WIN_VERDICT_EXE_LOCAL" ] || missing+=("WIN_VERDICT_EXE_LOCAL")
  fi
  if [ "${#missing[@]}" -gt 0 ]; then
    echo "--run would need: ${missing[*]}"
  else
    echo "--run prerequisites present."
  fi
}
