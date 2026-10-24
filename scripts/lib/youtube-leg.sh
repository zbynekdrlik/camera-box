#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, sets no shell options: sourcing runs it
# in the CALLER's shell -- recording-e2e.sh under `set -euo pipefail` -- which owns the strict mode).
#
# scripts/lib/youtube-leg.sh -- issue 1404 (owner 24.10.2026: "aby test bol end to end a nie len
# cast"; the acceptance point of 5.10.2026 = YouTube's CI test stream "e2e rtmp", read back as a
# viewer). The full-path E2E's BLOCKING YouTube stage, wired through this lib so recording-e2e.sh
# gains only call lines (the issue-675 anchor-safe pattern). The work is scripts/youtube_leg_e2e.py
# (restreamer's av-gate session through scripts/av_gate_session.py, the stream OBS output through
# scripts/youtube_leg_obs.py, the verdict through scripts/youtube_leg_verdict.py ON THE MEASUREMENT
# HOST dev2, scripts/youtube_leg_host.py: design 6387919956 -- dev1 only orchestrates and folds).
#
#   youtube_leg_enabled                                   rc 0 iff this run executes its verdict
#   youtube_leg_start HERE STRIH STREAM USER PW OUTDIR      before StartRecord; always rc 0
#   youtube_leg_stop HERE STREAM USER PW OUTDIR [STREAM_HOST_PATH]
#                                                         after the stream StopRecord; always rc 0;
#                                                         with the recording's path it starts the
#                                                         prefetch (pull to dev2 + its early decode)
#                                                         in the background
#   youtube_leg_cleanup_stop HERE STREAM USER PW OUTDIR     cleanup(): one bounded background stop
#                                                         + the purge of dev2's stage dir
#   youtube_leg_gate HERE STREAM USER PW OUTDIR REPORT_JSON STREAM_HOST_PATH MARKER_CSV SCHEDULE PROBE_BIN_DIR
#                                                         after the merge: rc 0 = YouTube PASS (or a
#                                                         mode that executes no verdict), 1 = RED
#
# The stage runs exactly when the run executes its verdict (E2E_EXECUTE_VERDICT=1, the release
# pull_request gate): there is no switch to skip it there. A start that fails (a refused delivery,
# the channel guard, no live broadcast) is recorded, everything started is stopped, and the run goes
# on to measure the other points; the gate then ends RED with the reason. StartStream is a rig
# mutation, so the shared rig-busy guard runs right before it (.claude/rules/rig-mutation-broadcast-guard.md).

youtube_leg_enabled() {
  [ "${E2E_EXECUTE_VERDICT:-0}" = 1 ]
}

_youtube_leg_py() {
  printf '%s' "${YOUTUBE_LEG_E2E_PY:-$1/youtube_leg_e2e.py}"
}

youtube_leg_start() {
  local here="$1" strih="$2" stream="$3" user="$4" pw="$5" outdir="$6"
  local d="$outdir/youtube-leg"
  if ! youtube_leg_enabled; then
    echo "    issue 1404: the YouTube stage is not run in this mode (E2E_EXECUTE_VERDICT!=1): the run's end point is the stream recording"
    return 0
  fi
  echo "    issue 1404: the YouTube end point -- channel guard, restreamer delivery to e2e rtmp, StartStream, wait for the live broadcast"
  stray_session_check_assert "$here" "$strih" "$stream" "the YouTube stage StartStream"
  mkdir -p "$d"
  if AV_GATE_SSH_PASSWORD="$pw" python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
      start --title "camera-box E2E ${RUN_ID:-}"; then
    (AV_GATE_SSH_PASSWORD="$pw" exec python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
      watch --parent "$$") >>"$d/watch.log" 2>&1 &
    echo "$!" >"$d/watch.pid"
    echo "    issue 1404: YouTube broadcast LIVE; channel guard watch every ${YOUTUBE_LEG_WATCH_POLL_S:-10} s (log $d/watch.log)"
  else
    echo "!! issue 1404: the YouTube stage did NOT start (reason above) -- the run goes on to measure the other points, its gate ends RED" >&2
  fi
  return 0
}

_youtube_leg_watch_end() {
  local d="$1" pid=""
  [ -f "$d/watch.pid" ] || return 0
  pid="$(cat "$d/watch.pid" 2>/dev/null || true)"
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -f "$d/watch.pid"
  return 0
}

youtube_leg_stop() {
  local here="$1" stream="$2" user="$3" pw="$4" outdir="$5" host_path="${6-}"
  local d="$outdir/youtube-leg"
  [ -f "$d/state.json" ] || return 0
  if ! AV_GATE_SSH_PASSWORD="$pw" timeout 240 python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" \
      --user "$user" stop --record-stopped --why "the stream StopRecord"; then
    echo "!! issue 1404: the YouTube stage stop did not complete -- cleanup() retries it, restreamer's idle reaper is the backstop" >&2
  fi
  _youtube_leg_watch_end "$d"
  # design 6387919956 part b: the recording is complete now -- pull it to the measurement host and
  # decode its tick map there while the E2E merges and YouTube processes the VOD (bounded inside;
  # the gate waits for it, cleanup() purges it)
  if [ -n "$host_path" ]; then
    (AV_GATE_SSH_PASSWORD="$pw" exec python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
      prefetch --stream-path "$host_path") >>"$d/prefetch.log" 2>&1 &
    echo "$!" >"$d/prefetch.pid"
    echo "    issue 1404: the stream recording goes to the measurement host now and is decoded there during the merge (log $d/prefetch.log)"
  fi
  return 0
}

youtube_leg_cleanup_stop() {
  local here="$1" stream="$2" user="$3" pw="$4" outdir="$5"
  local d="$outdir/youtube-leg"
  [ -f "$d/state.json" ] || return 0
  # the StopStream half FIRST and in the foreground, bounded: a cancelled job is killed seconds after
  # its SIGINT, and a stream left running blocks every later run's rig-busy gate
  timeout 10 python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
    stop --stream-only --why "cleanup()" >>"$d/cleanup-stop.log" 2>&1 \
    || echo "!! issue 1404: cleanup() could not stop the stream OBS output in 10 s -- the background stop, the next run's recover (dev1 marker) and restreamer's idle reaper retry" >&2
  # the session half in the background: never delays the device restores; idempotent
  (AV_GATE_SSH_PASSWORD="$pw" timeout 120 python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" \
    --user "$user" stop --why "cleanup()") >>"$d/cleanup-stop.log" 2>&1 &
  # the measurement host: stop a prefetch still running, delete its stage dir (the videos with it);
  # a no-op after a gate that measured (it deleted it already)
  (timeout 120 python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
    purge) >>"$d/cleanup-stop.log" 2>&1 &
  return 0
}

youtube_leg_gate() {
  local here="$1" stream="$2" user="$3" pw="$4" outdir="$5" report="$6" host_path="$7" markers="$8"
  local schedule="$9" probe_dir="${10}"
  local d="$outdir/youtube-leg" rc=0
  if ! youtube_leg_enabled; then
    echo "    issue 1404: the YouTube end point was NOT measured in this mode (E2E_EXECUTE_VERDICT!=1)"
    return 0
  fi
  echo "    issue 1404: the YouTube end point (BLOCKING): the VOD against the stream recording, measured on ${YOUTUBE_LEG_MEASURE_HOST:-newlevel@dev2}"
  mkdir -p "$d"
  # no video ever reaches dev1: the measurement host pulled the recording (the prefetch, or measure's
  # one retry), fetches the VOD, decodes, judges, sends back the verdict JSON + tick maps, deletes both
  AV_GATE_SSH_PASSWORD="$pw" python3 "$(_youtube_leg_py "$here")" --dir "$d" --host "$stream" --user "$user" \
    measure --verdict-json "$report" --stream-path "$host_path" --markers "$markers" --schedule "$schedule" \
    --probe-bin "$probe_dir/recording-verdict" || rc=$?
  [ "$rc" = 0 ]
}
