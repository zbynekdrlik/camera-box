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
#   3. bounded raw-V4L2 grab on cam2    (ffmpeg -use_wallclock_as_timestamps 1 -> a file in /tmp)
#   4. restart+verify cam2 camera-box   (scripts/lib/camera-box-restart-verify.sh; a remote EXIT
#                                        trap ALWAYS restores the service even if the grab fails)
#   5. burn OFF the imag program input  (a dev1-side EXIT trap ALWAYS turns the burn off)
#   6. scp the capture back to dev1
# then the offline decode/report is scripts/drm_latency_report.py (per-frame pairing of the decoded
# gen_ts_ns against the capture wall-ts; median/p95/p99 + jitter; a DORMANT-ENABLED delta table).
#
# RIG STATE IS AN INPUT, NOT A KNOB: the DORMANT / ENABLED state is passed as --label; this script
# NEVER writes ~/.camera-box/drm-output.json (the ENABLE flip is the supervisor's M4 runbook step).
# The DORMANT-ENABLED DELTA cancels the grabber's fixed systematic offset, so the absolute number
# does not matter -- the delta does.
#
# SHAPE: this is a PLANNER + bounded ssh-executor in the exact shape of
# scripts/deploy-genlock-fleet.sh -- pure builder functions (drm_latency_cam2_program /
# drm_latency_burn_cmd / drm_latency_scp_cmd) that print command text and take NO network, a
# source-guard so the unit tests (tests/python/test_drm_latency_report_1152.py) can source this
# file with no rig, then main(). PLAN/dry-run is the DEFAULT; --execute performs the rig I/O.
#
# Usage (PLAN -- print the whole measurement plan, touch nothing; the DEFAULT):
#   scripts/drm-latency-measure.sh --label DORMANT --imag-input "CAM1 (usb)"
#
# Usage (EXECUTE -- run the measurement against the live rig; supervisor rig-campaign step):
#   OBS_WS_PASSWORD=... scripts/drm-latency-measure.sh --execute --label DORMANT \
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

# drm_latency_cam2_program NODE FMT SIZE FPS SECONDS OUTFILE LABEL
#   -> the full REMOTE bash for the cam2 leg: an EXIT trap that ALWAYS restarts+verifies
#   camera-box (reusing camera_box_verify_active_cmds), stop camera-box, then a bounded ffmpeg
#   raw-V4L2 grab with per-frame wall-clock timestamps into OUTFILE. Meant to be fed to
#   `ssh <cam2> bash -s` on stdin. The local $args are expanded here; the remote-side $? / $_drm_rc
#   are backslash-escaped so they survive to the remote shell (unquoted-heredoc idiom, matching
#   scripts/lib/*.sh). The spliced camera_box_verify_active_cmds output already carries real remote
#   $ (its own unquoted heredoc consumed the backslashes), and a command substitution's output is
#   not re-scanned, so it passes through literally.
drm_latency_cam2_program() {
  local node="$1" fmt="$2" size="$3" fps="$4" seconds="$5" outfile="$6" label="$7"
  cat <<CAM2PROG
set +e
_drm_restore() {
  echo "[drm-latency] restoring camera-box on cam2 (label=$label)" >&2
  systemctl restart camera-box 2>/dev/null || true
$(camera_box_verify_active_cmds "cam2 (drm-latency $label)")
}
trap _drm_restore EXIT
echo "[drm-latency] stop camera-box to free $node (label=$label)"
systemctl stop camera-box 2>/dev/null || true
sleep 1
echo "[drm-latency] grab ${seconds}s from $node ($fmt $size@$fps) with wallclock ts -> $outfile"
ffmpeg -hide_banner -loglevel warning -nostdin -use_wallclock_as_timestamps 1 -f v4l2 -input_format $fmt -video_size $size -framerate $fps -i $node -t $seconds -c:v copy -f nut $outfile
_drm_rc=\$?
echo "[drm-latency] grab exit=\$_drm_rc label=$label out=$outfile" ;
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

# drm_latency_scp_cmd USER HOST REMOTE LOCAL -> pull the capture back to dev1 (scp -O).
drm_latency_scp_cmd() {
  local user="$1" host="$2" remote="$3" local_dst="$4"
  printf 'scp -O %s@%s:%s %s ;\n' "$user" "$host" "$remote" "$local_dst"
}

# ============================================================================================
# source-guard: when sourced (the unit tests), stop here -- everything below runs only when the
# script is executed directly.
# ============================================================================================
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

usage() {
  sed -n '2,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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

  [ -n "$label" ] || { echo "drm-latency-measure: --label is REQUIRED" >&2; exit 2; }
  if [ "$mode" = "execute" ] && [ -z "$imag_input" ]; then
    echo "drm-latency-measure: --imag-input is REQUIRED in --execute mode" >&2; exit 2
  fi
  [ -n "$imag_input" ] || imag_input="<IMAG_INPUT>"

  local ts remote_out local_dst
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  remote_out="/tmp/drm-lat-${label}-${ts}.nut"
  local_dst="${outdir%/}/drm-lat-${label}-${ts}.nut"

  local cam2prog burn_on burn_off scp_line
  cam2prog="$(drm_latency_cam2_program "$node" "$fmt" "$size" "$fps" "$seconds" "$remote_out" "$label")"
  burn_on="$(drm_latency_burn_cmd add "$imag_host" "$imag_input")"
  burn_off="$(drm_latency_burn_cmd remove "$imag_host" "$imag_input")"
  scp_line="$(drm_latency_scp_cmd "$cam2_user" "$cam2_host" "$remote_out" "$local_dst")"

  if [ "$mode" = "plan" ]; then
    echo "=== #1152 M3 DRM-latency measurement PLAN (label=$label) -- dry-run, touches nothing ==="
    echo "# rig state ($label) is an INPUT here; this script never flips ~/.camera-box/drm-output.json."
    echo
    echo "# 1) burn ON the imag program input (dev1 -> imag OBS WebSocket):"
    echo "$burn_on"
    echo "# 2-4) on cam2 ($cam2_user@$cam2_host): stop camera-box, grab, restart+verify (EXIT-trap restore):"
    echo "ssh $cam2_user@$cam2_host bash -s <<'CAM2'"
    echo "$cam2prog"
    echo "CAM2"
    echo "# 5) burn OFF the imag program input (also run by the dev1 EXIT trap in execute mode):"
    echo "$burn_off"
    echo "# 6) pull the capture back to dev1:"
    echo "$scp_line"
    echo
    echo "# then report:  python3 $HERE/drm_latency_report.py run --label $label --capture $local_dst --out ${outdir%/}/drm-lat-${label}.json"
    return 0
  fi

  echo "[drm-latency] EXECUTE label=$label imag=$imag_host input='$imag_input' cam2=$cam2_user@$cam2_host node=$node ${seconds}s"
  echo "[drm-latency] burn ON $imag_input on imag ($imag_host)"
  python3 "$HERE/obs_burn_filter.py" add --host "$imag_host" --input "$imag_input" --password "${OBS_WS_PASSWORD:-}"
  # From here on, ALWAYS turn the burn off on exit (a failed grab must never leave a burn live).
  # shellcheck disable=SC2064
  trap "python3 '$HERE/obs_burn_filter.py' remove --host '$imag_host' --input '$imag_input' --password \"\${OBS_WS_PASSWORD:-}\" 2>/dev/null || true" EXIT

  echo "[drm-latency] cam2 leg (stop -> grab -> restart+verify) on $cam2_user@$cam2_host"
  ssh -o ConnectTimeout=15 "$cam2_user@$cam2_host" "bash -s" <<<"$cam2prog" || \
    echo "[drm-latency] WARNING: cam2 leg returned non-zero (the remote EXIT trap still restarted camera-box)" >&2

  echo "[drm-latency] pull capture -> $local_dst"
  scp -O "$cam2_user@$cam2_host:$remote_out" "$local_dst"

  echo "[drm-latency] DONE label=$label capture=$local_dst"
  echo "[drm-latency] next: python3 $HERE/drm_latency_report.py run --label $label --capture $local_dst --out ${outdir%/}/drm-lat-${label}.json"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
