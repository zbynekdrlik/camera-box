#!/usr/bin/env bash
# scripts/drm-latency-measure.sh -- dev1-side orchestrator for the #1152 M3 DRM-latency measurement. See the extended header below.
set -euo pipefail
#
# ============================================================================================
# #1152 M3 (design of record: issue-1152 comment 5428521213, Approach 1) -- measure the
# render-tick -> HDMI-glass latency + jitter of the imag Program output, DORMANT (today's X
# projector path) vs ENABLED (the M1/M2 in-OBS vendored DRM-lease output), with ONE methodology.
#
# TOPOLOGY: the cam2 grabber is physically wired to imag's HDMI (projection-tap, issue 781/1196),
# so cam2 /dev/video0 IS the imag scanout. During a measurement window imag's Program carries the
# existing QR burn whose gen_ts_ns field is the emit wall clock (genlock CLOCK_REALTIME ns). This
# script (run FROM dev1 by the supervisor's rig campaign) does, in order:
#   1. burn ON the imag program input   (obs_burn_filter.py add, over OBS WebSocket to imag)
#   2. stop cam2 camera-box.service     (free /dev/video0 -- udev-device-ownership discipline)
#   3. bounded V4L2 grab on cam2, STREAMED over the ssh pipe straight into a dev1-local file
#      (ffmpeg -use_wallclock_as_timestamps 1 ... -f nut - ; a raw grab cannot fit cam2's /tmp,
#      proven live -- and the remote program keeps stdout CLEAN: every progress echo goes to
#      stderr, since stdout IS the NUT stream). Bounded by -frames:v (seconds*fps), NEVER -t:
#      with -copyts the -t seconds compare against EPOCH-scale timestamps -> EMPTY file.
#   4. restart+verify cam2 camera-box   (scripts/lib/camera-box-restart-verify.sh; a remote EXIT
#                                        trap ALWAYS restores the service even if the grab fails)
#   5. burn OFF the imag program input  (a dev1-side EXIT trap ALWAYS turns the burn off)
# then the offline decode/report is scripts/drm_latency_report.py (per-frame pairing of the decoded
# gen_ts_ns against the capture wall-ts; median/p95/p99 + jitter; a DORMANT-ENABLED delta table).
#
# AUTH: the cam fleet authenticates by PASSWORD. Export CAM_PW to route the ssh through
# `sshpass -p "$CAM_PW"`; unset CAM_PW = plain ssh (key auth). The value is never printed.
#
# RIG STATE IS AN INPUT, NOT A KNOB: the DORMANT / ENABLED state is passed as --label; this script
# NEVER writes ~/.camera-box/drm-output.json (the ENABLE flip is the supervisor's M4 runbook step).
# The DORMANT-ENABLED DELTA cancels the grabber's fixed systematic offset, so the absolute number
# does not matter -- the delta does.
#
# SHAPE: this is a PLANNER + bounded ssh-executor in the exact shape of
# scripts/deploy-genlock-fleet.sh -- pure builder functions (drm_latency_cam2_program /
# drm_latency_burn_cmd) that print command text and take NO network, a source-guard so the unit
# tests (tests/python/test_drm_latency_report_1152.py) can source this file with no rig, then
# main(). PLAN/dry-run is the DEFAULT; --execute performs the rig I/O.
#
# Usage (PLAN -- print the whole measurement plan, touch nothing; the DEFAULT):
#   scripts/drm-latency-measure.sh --label DORMANT --imag-input "CAM1 (usb)"
#
# Usage (EXECUTE -- run the measurement against the live rig; supervisor rig-campaign step):
#   CAM_PW=... OBS_WS_PASSWORD=... scripts/drm-latency-measure.sh --execute --label DORMANT \
#       --imag-input "CAM1 (usb)" [--imag-host 10.77.9.182] [--cam2-host 10.77.9.62] \
#       [--cam2-user root] [--node /dev/video0] [--input-format mjpeg] \
#       [--video-size 1920x1080] [--framerate 60] [--seconds 8] [--outdir /tmp]
#
#   --label        REQUIRED. Rig state this run measures (e.g. DORMANT / ENABLED) -- tags the
#                  capture filename + the report; does NOT change the rig.
#   --imag-input   REQUIRED (execute). The imag OBS NDI input name to burn on (the program-carrying
#                  source; get it from `python3 scripts/obs_burn_filter.py check --host <imag>`).
#   --input-format cam2 /dev/video0 pixel/input format for ffmpeg -f v4l2 (device-specific --
#                  default mjpeg for the ShadowCast grabber; override per the live device caps).
# ============================================================================================

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Reuse the restart+verify remote-command builder (source-only lib, no set -e leak).
# shellcheck source=scripts/lib/camera-box-restart-verify.sh
. "$HERE/lib/camera-box-restart-verify.sh"

# --------------------------------------------------------------------------- #
# PURE builders -- print command/program text, take NO network (unit-tested by
# sourcing this file; a regression in an emitted program is caught with no rig).
# --------------------------------------------------------------------------- #

# drm_latency_cam2_program NODE FMT SIZE FPS SECONDS LABEL
#   -> the full REMOTE bash for the cam2 leg: an EXIT trap that ALWAYS restarts+verifies
#   camera-box (reusing camera_box_verify_active_cmds), stop camera-box, then a bounded ffmpeg
#   V4L2 grab with per-frame wall-clock timestamps STREAMED to stdout as NUT (`-f nut -`) -- the
#   caller redirects the ssh pipe into the dev1-local capture file, so nothing lands on cam2's
#   /tmp (a raw grab overflows it, proven live). stdout is the NUT stream, so EVERY progress echo
#   here goes to stderr, and the restore fn redirects its WHOLE body to stderr (the spliced
#   verify block prints its own success line to stdout). The grab is bounded by -frames:v
#   (seconds*fps); `-t` is FORBIDDEN here -- with -copyts it compares the CLI seconds against
#   epoch-scale timestamps and silently writes an EMPTY capture (proven live). Meant to be fed to
#   `ssh <cam2> bash -s` on stdin. The local $args are expanded here; the remote-side $? / $_drm_rc
#   are backslash-escaped so they survive to the remote shell (unquoted-heredoc idiom, matching
#   scripts/lib/*.sh). The spliced camera_box_verify_active_cmds output already carries real remote
#   $ (its own unquoted heredoc consumed the backslashes), and a command substitution's output is
#   not re-scanned, so it passes through literally.
drm_latency_cam2_program() {
  local node="$1" fmt="$2" size="$3" fps="$4" seconds="$5" label="$6"
  local frames=$((seconds * fps))
  cat <<CAM2PROG
set +e
_drm_restore() {
  echo "[drm-latency] restoring camera-box on cam2 (label=$label)" >&2
  systemctl restart camera-box 2>/dev/null || true
$(camera_box_verify_active_cmds "cam2 (drm-latency $label)")
} >&2
trap _drm_restore EXIT
echo "[drm-latency] stop camera-box to free $node (label=$label)" >&2
systemctl stop camera-box 2>/dev/null || true
sleep 1
echo "[drm-latency] grab $frames frames from $node ($fmt $size@$fps) with wallclock ts, NUT to stdout" >&2
timeout -k 5 $((seconds + 30)) ffmpeg -hide_banner -loglevel warning -nostdin -use_wallclock_as_timestamps 1 -f v4l2 -input_format $fmt -video_size $size -framerate $fps -i $node -frames:v $frames -copyts -avoid_negative_ts disabled -c:v copy -f nut -
_drm_rc=\$?
echo "[drm-latency] grab exit=\$_drm_rc label=$label" >&2 ;
CAM2PROG
}

# drm_latency_burn_cmd ACTION HOST INPUT
#   -> the dev1-side obs_burn_filter.py line that toggles the measurement burn on the imag program
#   input over OBS WebSocket (ACTION = add | remove). OBS_WS_PASSWORD expands at RUN time. Trailing
#   ';' terminates the statement regardless of what a caller concatenates after the $(...).
drm_latency_burn_cmd() {
  local action="$1" host="$2" input="$3"
  printf 'python3 "%s/obs_burn_filter.py" %s --host %s --input "%s" --password "${OBS_WS_PASSWORD:-}" ;\n' \
    "$HERE" "$action" "$host" "$input"
}

# ============================================================================================
# source-guard: when sourced (the unit tests), stop here -- everything below runs only when the
# script is executed directly.
# ============================================================================================
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# _drm_burn_off_and_warn HOST INPUT -> turn the measurement burn OFF; if that FAILS, warn LOUDLY
# (never swallow it -- a leaked live burn is the #246/#938/#1011 fail-closed class). Script scope so
# the EXIT trap can call it after main() has returned (its locals gone); the host/input are baked
# into the trap string at set-time and passed here as args.
_drm_burn_off_and_warn() {
  local host="$1" input="$2"
  if ! python3 "$HERE/obs_burn_filter.py" remove --host "$host" --input "$input" --password "${OBS_WS_PASSWORD:-}"; then
    echo "[drm-latency] WARNING: burn OFF FAILED -- the measurement burn may still be LIVE on '$input' (imag $host). Clear it manually: python3 $HERE/obs_burn_filter.py check --host $host --input \"$input\" ; then 'remove'." >&2
  fi
}

usage() {
  sed -n '2,56p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

main() {
  local label="" imag_host="10.77.9.182" imag_input="" cam2_host="10.77.9.62" cam2_user="root"
  local node="/dev/video0" fmt="mjpeg" size="1920x1080" fps="60" seconds="8" outdir="/tmp"
  local mode="plan"

  while [ $# -gt 0 ]; do
    case "$1" in
      --label)        label="${2:-}"; shift 2 ;;
      --imag-host)    imag_host="${2:-}"; shift 2 ;;
      --imag-input)   imag_input="${2:-}"; shift 2 ;;
      --cam2-host)    cam2_host="${2:-}"; shift 2 ;;
      --cam2-user)    cam2_user="${2:-}"; shift 2 ;;
      --node)         node="${2:-}"; shift 2 ;;
      --input-format) fmt="${2:-}"; shift 2 ;;
      --video-size)   size="${2:-}"; shift 2 ;;
      --framerate)    fps="${2:-}"; shift 2 ;;
      --seconds)      seconds="${2:-}"; shift 2 ;;
      --outdir)       outdir="${2:-}"; shift 2 ;;
      --plan)         mode="plan"; shift ;;
      --execute|--yes) mode="execute"; shift ;;
      -h|--help)      usage; exit 0 ;;
      *) echo "drm-latency-measure: unknown arg '$1'" >&2; usage; exit 2 ;;
    esac
  done

  # --label is interpolated into file paths AND the remote command text, so constrain it hard
  # (a space / slash would word-split the emitted ffmpeg line -- review 🔵).
  case "$label" in
    "") echo "drm-latency-measure: --label is REQUIRED" >&2; exit 2 ;;
    *[!A-Za-z0-9_-]*) echo "drm-latency-measure: --label must be [A-Za-z0-9_-]+ (got '$label')" >&2; exit 2 ;;
  esac
  if [ "$mode" = "execute" ] && [ -z "$imag_input" ]; then
    echo "drm-latency-measure: --imag-input is REQUIRED in --execute mode" >&2; exit 2
  fi
  [ -n "$imag_input" ] || imag_input="<IMAG_INPUT>"

  local ts local_dst
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  local_dst="${outdir%/}/drm-lat-${label}-${ts}.nut"

  # Password-auth seam: the cam fleet uses password ssh. CAM_PW set -> sshpass wraps the ssh
  # (the plan prints the UNEXPANDED "$CAM_PW" reference, never the value); unset -> plain ssh.
  local ssh_prefix=() ssh_prefix_txt=""
  if [ -n "${CAM_PW:-}" ]; then
    ssh_prefix=(sshpass -p "$CAM_PW")
    ssh_prefix_txt='sshpass -p "$CAM_PW" '
  fi

  local cam2prog burn_on burn_off
  cam2prog="$(drm_latency_cam2_program "$node" "$fmt" "$size" "$fps" "$seconds" "$label")"
  burn_on="$(drm_latency_burn_cmd add "$imag_host" "$imag_input")"
  burn_off="$(drm_latency_burn_cmd remove "$imag_host" "$imag_input")"

  if [ "$mode" = "plan" ]; then
    echo "=== #1152 M3 DRM-latency measurement PLAN (label=$label) -- dry-run, touches nothing ==="
    echo "# rig state ($label) is an INPUT here; this script never flips ~/.camera-box/drm-output.json."
    echo
    echo "# 1) burn ON the imag program input (dev1 -> imag OBS WebSocket):"
    echo "$burn_on"
    echo "# 2-4) on cam2 ($cam2_user@$cam2_host): stop camera-box, grab, restart+verify (EXIT-trap"
    echo "#      restore); the NUT capture STREAMS over the ssh pipe into the dev1-local file:"
    echo "${ssh_prefix_txt}ssh $cam2_user@$cam2_host bash -s > $local_dst <<'CAM2'"
    echo "$cam2prog"
    echo "CAM2"
    echo "# 5) burn OFF the imag program input (also run by the dev1 EXIT trap in execute mode):"
    echo "$burn_off"
    echo
    echo "# then report:  python3 $HERE/drm_latency_report.py run --label $label --capture $local_dst --out ${outdir%/}/drm-lat-${label}.json"
    return 0
  fi

  echo "[drm-latency] EXECUTE label=$label imag=$imag_host input='$imag_input' cam2=$cam2_user@$cam2_host node=$node ${seconds}s"
  echo "[drm-latency] burn ON $imag_input on imag ($imag_host)"
  python3 "$HERE/obs_burn_filter.py" add --host "$imag_host" --input "$imag_input" --password "${OBS_WS_PASSWORD:-}"
  # From here on, ALWAYS turn the burn off on exit (a failed grab must never leave a burn LIVE). The
  # host/input are baked into the trap at set-time (SC2064, deliberate) because main's locals are
  # gone by the time the EXIT trap fires; the callee warns loudly if the remove fails.
  # shellcheck disable=SC2064
  trap "_drm_burn_off_and_warn '$imag_host' '$imag_input'" EXIT

  # NOTE (review 🔵): plan mode PRINTS drm_latency_burn_cmd for the human; execute uses direct
  # argv here (safer than eval) -- the two are kept textually parallel by hand.
  echo "[drm-latency] cam2 leg (stop -> grab -> restart+verify), NUT streaming -> $local_dst"
  timeout $((seconds + 90)) "${ssh_prefix[@]}" ssh -o ConnectTimeout=15 "$cam2_user@$cam2_host" "bash -s" <<<"$cam2prog" > "$local_dst" || \
    echo "[drm-latency] WARNING: cam2 leg returned non-zero or timed out (the remote EXIT trap still restarted camera-box)" >&2

  if [ ! -s "$local_dst" ]; then
    echo "[drm-latency] FAIL: capture $local_dst is EMPTY -- the grab produced no frames (device busy? wrong --node/--input-format?)" >&2
    exit 1
  fi

  echo "[drm-latency] DONE label=$label capture=$local_dst ($(stat -c%s "$local_dst") bytes)"
  echo "[drm-latency] next: python3 $HERE/drm_latency_report.py run --label $label --capture $local_dst --out ${outdir%/}/drm-lat-${label}.json"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
