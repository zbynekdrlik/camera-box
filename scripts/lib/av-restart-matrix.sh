#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs this in
# the CALLER's shell (scripts/av-restart-matrix.sh, which sets its own strict mode). Every function
# is safe under the caller's `set -euo pipefail`.
#
# scripts/lib/av-restart-matrix.sh -- issue 1367: the pure builders of the RESTART MATRIX
# (scripts/av-restart-matrix.sh). The decision half is scripts/av_restart_matrix_decision.py.
#
# What lives here, used by BOTH the --plan printout and the --run execution (plan == run):
#   - the four restart kinds and their targets,
#   - the REMOTE text of each restart, each health read and each "leave it running" step -- plain
#     `systemctl restart` of ONE service (never a reboot: a remote cambox reboot is banned, a warm
#     reboot can leave a box down until someone is at the rig),
#   - the pure "is it healthy" predicate over those reads,
#   - the argv of ONE measurement window = the soak itself (`av-soak.sh --run --hours 0
#     --lease-run-id <the matrix's lease>`), never a copy of its record/sweep/decode/merge step,
#   - the stream OBS SUPERVISOR step (no session-agnostic path launches the canonical stream OBS).
#
# Each remote text is sent WHOLE as one ssh command, never spliced into a larger string, so the
# newline-strip gotcha of a `$(...)`-embedded helper (CLAUDE.md) cannot glue it to anything; the
# single-statement lines still end with `;`.

# The restart kinds, in run order (the same four as av_restart_matrix_decision.KINDS).
AV_MATRIX_KINDS_ALL="strih-obs cambox dantesync stream-obs"

# av_matrix_kind_valid KIND -> 0 iff KIND is one of the four restart kinds.
av_matrix_kind_valid() {
  case " $AV_MATRIX_KINDS_ALL " in *" ${1:-} "*) [ -n "${1:-}" ] ;; *) return 1 ;; esac
}

# av_matrix_default_cambox CAMS -> the first camera of CAMS that is not cam2 (cam2 captures the
# imag-nb projection and carries the painter; its camera-box is not a camera path), empty when none.
av_matrix_default_cambox() {
  local c
  for c in ${1:-}; do
    [ "$c" = cam2 ] && continue
    printf '%s\n' "$c"
    return 0
  done
  printf '\n'
}

# av_matrix_restart_remote_cmd KIND -> the REMOTE bash text that restarts ONE service and says so:
# `AV_MATRIX_RESTART_AT=<the box's own epoch>` first (the health read counts log lines from THAT
# instant, on the box's own clock), then the outcome marker.
#   strih-obs  -- the strih-lx `strih-obs.service` --user unit, through the ONE existing headless
#                 restart (mv_reverify_obs_restart_linux_cmd, scripts/lib/mv-reverify-escalate.sh:
#                 unit-installed guard, reset-failed, a BLOCKING restart so the old OBS is gone
#                 before the health read; markers MV_REVERIFY_OBS_RESTART / _NO_UNIT / _FAILED).
#   cambox     -- `systemctl restart camera-box` on the camera (root).
#   dantesync  -- `systemctl restart dantesync` on the camera (root).
# The stream OBS has no remote text: it is a supervisor step (av_matrix_stream_supervisor_step).
av_matrix_restart_remote_cmd() {
  local unit
  case "${1:-}" in
    strih-obs)
      printf '%s\n' "printf 'AV_MATRIX_RESTART_AT=%s\\n' \"\$(date +%s)\";"
      mv_reverify_obs_restart_linux_cmd
      return 0
      ;;
    cambox) unit=camera-box ;;
    dantesync) unit=dantesync ;;
    *) echo "av-restart-matrix: no remote restart for kind '${1:-}'" >&2; return 1 ;;
  esac
  printf '%s\n' "printf 'AV_MATRIX_RESTART_AT=%s\\n' \"\$(date +%s)\";"
  printf '%s\n' "if systemctl restart ${unit}; then echo AV_MATRIX_RESTART_OK; else echo AV_MATRIX_RESTART_FAILED; exit 1; fi;"
}

# av_matrix_restart_outcome KIND OUTPUT -> ok | not_performed | failed. A restart is `ok` only on
# its POSITIVE marker (an ssh/auth/timeout failure prints none and is `not_performed`, never a
# performed restart); the strih-lx unit missing is `not_performed` (nothing would relaunch OBS, so
# it was never touched); the unit's own restart job failing is `failed`.
av_matrix_restart_outcome() {
  local kind="$1" out="${2:-}"
  case "$kind" in
    strih-obs)
      case "$out" in
        *MV_REVERIFY_NO_UNIT*) printf 'not_performed\n' ;;
        *MV_REVERIFY_RESTART_FAILED*) printf 'failed\n' ;;
        *MV_REVERIFY_OBS_RESTART:*) printf 'ok\n' ;;
        *) printf 'not_performed\n' ;;
      esac
      ;;
    *)
      case "$out" in
        *AV_MATRIX_RESTART_FAILED*) printf 'failed\n' ;;
        *AV_MATRIX_RESTART_OK*) printf 'ok\n' ;;
        *) printf 'not_performed\n' ;;
      esac
      ;;
  esac
}

# av_matrix_restart_at OUTPUT -> the box's own restart epoch from the restart output (empty when
# absent or not a number).
av_matrix_restart_at() {
  local v
  v="$(printf '%s\n' "${1:-}" | sed -n 's/^AV_MATRIX_RESTART_AT=\([0-9][0-9]*\)$/\1/p' | head -n 1 || true)"
  printf '%s\n' "$v"
}

# av_matrix_health_remote_cmd KIND [SINCE_EPOCH] -> the REMOTE (read-only) text of the kind's
# health read, `key=value` lines:
#   strih-obs -- `active=` of the strih-obs.service --user unit (the WebSocket half is read from dev1)
#   cambox    -- `active=` of camera-box + `streaming=` the count of its `Streaming:` journal lines
#                since SINCE_EPOCH (the box's own restart instant): the capture loop is emitting again
# dantesync and stream-obs are read from dev1 (:8898/status, the OBS WebSocket) -- no remote text.
av_matrix_health_remote_cmd() {
  local since="${2:-}"
  case "$since" in '' | *[!0-9]*) since=0 ;; esac
  case "${1:-}" in
    strih-obs)
      printf '%s\n' "printf 'active=%s\\n' \"\$(systemctl --user is-active strih-obs.service 2>/dev/null || true)\";"
      ;;
    cambox)
      printf '%s\n' "printf 'active=%s\\n' \"\$(systemctl is-active camera-box 2>/dev/null || true)\";"
      printf '%s\n' "printf 'streaming=%s\\n' \"\$(journalctl -u camera-box --since @${since} -o cat --no-pager 2>/dev/null | grep -c 'Streaming: ' || true)\";"
      ;;
    *) echo "av-restart-matrix: no remote health read for kind '${1:-}'" >&2; return 1 ;;
  esac
}

# av_matrix_health_ok KIND PROBE [SCENE] -> 0 iff the kind reports healthy:
#   strih-obs  -- PROBE `active=active` AND the OBS WebSocket answered (SCENE, the program scene
#                 read from dev1, non-empty)
#   cambox     -- PROBE `active=active` AND `streaming=` >= 1
#   dantesync  -- PROBE (dantesync_clock_decision.py analyze output) `verdict=OK`: locked, on the
#                 rig grandmaster, no step storm, a fresh /status
#   stream-obs -- the OBS WebSocket answered (SCENE non-empty)
av_matrix_health_ok() {
  local kind="$1" probe="${2:-}" scene="${3:-}" v n
  v="$(printf '%s\n' "$probe" | sed -n 's/^active=//p' | head -n 1 || true)"
  case "$kind" in
    strih-obs) [ "$v" = active ] && [ -n "$scene" ] ;;
    cambox)
      n="$(printf '%s\n' "$probe" | sed -n 's/^streaming=//p' | head -n 1 || true)"
      case "$n" in '' | *[!0-9]*) return 1 ;; esac
      [ "$v" = active ] && [ "$n" -ge 1 ]
      ;;
    dantesync) [ "$(printf '%s\n' "$probe" | sed -n 's/^verdict=//p' | head -n 1 || true)" = OK ] ;;
    stream-obs) [ -n "$scene" ] ;;
    *) return 1 ;;
  esac
}

# av_matrix_ensure_running_remote_cmd KIND -> the REMOTE text of cleanup's "leave it running":
# start the service only when it is not active (a no-op on a running one), then report `active=`.
av_matrix_ensure_running_remote_cmd() {
  local sc unit
  case "${1:-}" in
    strih-obs) sc="systemctl --user"; unit=strih-obs.service ;;
    cambox) sc="systemctl"; unit=camera-box ;;
    dantesync) sc="systemctl"; unit=dantesync ;;
    *) echo "av-restart-matrix: no remote ensure-running for kind '${1:-}'" >&2; return 1 ;;
  esac
  printf '%s\n' "if ! ${sc} is-active --quiet ${unit}; then ${sc} reset-failed ${unit} 2>/dev/null || true; ${sc} start ${unit} 2>/dev/null || true; fi;"
  printf '%s\n' "printf 'active=%s\\n' \"\$(${sc} is-active ${unit} 2>/dev/null || true)\";"
}

# av_matrix_window_dir RUN_DIR STEP KIND REPEAT -> the window's own soak run dir:
# <run>/w-00-baseline, <run>/w-NN-<kind>-rR.
av_matrix_window_dir() {
  local run="$1" step="$2" kind="$3" rep="${4:-0}"
  if [ "$kind" = baseline ]; then
    printf '%s/w-%02d-baseline\n' "$run" "$step"
  else
    printf '%s/w-%02d-%s-r%s\n' "$run" "$step" "$kind" "$rep"
  fi
}

# av_matrix_window_argv OUTVAR SOAK_SCRIPT WINDOW_DIR LEASE_RUN_ID [PROBE_BIN_DIR] [WIN_EXE] ->
# fills OUTVAR with ONE measurement window: the soak's own one-window run under the matrix's lease.
av_matrix_window_argv() {
  local -n _av_mw="$1"
  local soak="$2" dir="$3" lease="$4" probe="${5:-}" exe="${6:-}"
  _av_mw=(bash "$soak" --run --hours 0 --run-dir "$dir" --lease-run-id "$lease")
  if [ -n "$probe" ]; then _av_mw+=(--probe-bin-dir "$probe"); fi
  if [ -n "$exe" ]; then _av_mw+=(--win-verdict-exe "$exe"); fi
}

# av_matrix_confirm_path RUN_DIR REPEAT -> the file the supervisor writes once the stream OBS
# restart of repeat REPEAT is done.
av_matrix_confirm_path() {
  printf '%s/confirm-stream-obs-r%s\n' "$1" "$2"
}

# av_matrix_confirm_epoch FILE -> the epoch the supervisor wrote into FILE, else its mtime.
av_matrix_confirm_epoch() {
  local v
  v="$(head -n 1 "$1" 2>/dev/null | tr -d '[:space:]' || true)"
  case "$v" in '' | *[!0-9]*) v="$(stat -c %Y "$1" 2>/dev/null || date +%s)" ;; esac
  printf '%s\n' "$v"
}

# av_matrix_stream_supervisor_step SCRIPTS_DIR CONFIRM_FILE TIMEOUT_S -> the SUPERVISOR step text
# for a stream OBS restart. The stream box's canonical launch is its `OBS Studio.lnk` ->
# obs-guarded-launch.ps1 (the issue-786 audio-buffering launch gate); every interactive-token OBS
# task there launches a bare obs64.exe instead, ssh lands in session 0 (a GUI launch there is
# banned) and `schtasks /it` is a dead end -- so the restart is the supervisor's, through the ONE
# relaunch program pasted into the win-stream-snv MCP Shell.
av_matrix_stream_supervisor_step() {
  local here="$1" confirm="$2" timeout="$3"
  cat <<EOF
SUPERVISOR STEP -- restart the stream OBS through its canonical launch path (no session-agnostic
path launches it; never over ssh):
  1. bash ${here}/launch-obs-genlock.sh --box stream --force
     and paste the printed PowerShell program into the win-stream-snv MCP Shell; it must verify
     (genlock render tick ENABLED + DistroAV loaded). Do NOT change the program scene.
  2. then confirm with the epoch you ran it:  date +%s > ${confirm}
The matrix waits up to ${timeout} s for that file, then waits for the stream OBS WebSocket, the
settle time, and measures the window.
EOF
}

# av_matrix_window_stuck WINDOW_DIR -> 0 iff the window's recording.state still flags a recording
# the soak started (the soak kept it: exit 5, "RECORDING MAY STILL BE RUNNING").
av_matrix_window_stuck() {
  [ -f "$1/recording.state" ] && grep -q '^\(strih\|stream\)=1$' "$1/recording.state"
}

# av_matrix_expected_duration_s NSTEPS WINDOW_BOUND_S SETTLE_S HEALTHY_S SUPERVISOR_S NSTREAM ->
# the run length the lease announces (expected_release_at): every step's window bound + settle +
# healthy bound, the supervisor wait of every stream OBS repeat, + 30 min.
av_matrix_expected_duration_s() {
  local n="$1" w="$2" s="$3" h="$4" sup="$5" ns="$6"
  printf '%s\n' "$(( n * (w + s + h) + ns * sup + 1800 ))"
}
