#!/usr/bin/env bash
# lipsync-test-mode.sh -- issue 930: swap cam2's TEST-mode output from the dual-QR/QPSK painter to
# the lipsync cross-validation asset, and back again.
#
set -euo pipefail
#
# WHY / WHAT (issue 930): rig-mode.sh's TEST mode already puts camera-box on cam2 into no-display
# mode (fb0 free, capture+emit keep running -- #291/#528) and launches the transient dual-QR
# painter WITH the QPSK audio-marker thread (#420 -- the marker is a THREAD inside the SAME
# frame-probe process, not a separate daemon) on /dev/fb0 + hw:CARD=PCH,DEV=3. `start` below just
# needs to STOP that one process (which frees BOTH fb0 and the ALSA device in one kill, since the
# marker dies with it) and start ONE ffmpeg process playing the lipsync asset into the SAME two
# sinks (video -> fbdev, audio -> the SAME ALSA device the QPSK marker used) from a SINGLE
# demux/decode timeline (verified live on cam2, #930: bgra pixel format matches the painter's own
# convention in src/probe/fb.rs; `-ac 2` needed -- the ALSA device refuses a mono stream). `stop`
# kills the ffmpeg playback and calls rig-mode.sh test to fully restore + re-verify TEST mode
# (dual-QR + QPSK marker, burns, NDI mapping) -- never a partial/ad-hoc restore.
#
# Usage:
#   lipsync-test-mode.sh start [media]   -- stop the TEST-mode painter, play [media] (default
#                                           assets/lipsync/test.mp4) looped on cam2's fb0+ALSA
#   lipsync-test-mode.sh stop            -- kill the lipsync playback, restore TEST mode via
#                                           rig-mode.sh test (dual-QR + QPSK marker back + verified)
#
# Env:
#   PAINTER_IP        cam2 device IP (default 10.77.9.62, matches rig-mode.sh)
#   CAM_PW            cam2 root ssh password (default newlevel, matches targets.md)
#   PAINTER_PIDFILE   the TEST-mode painter's pidfile (default /run/rig-painter.pid, matches
#                     rig-mode.sh's own constant -- MUST stay in lock-step, it is the SAME painter)
#   LIPSYNC_FB_DEVICE      cam2 framebuffer device (default /dev/fb0)
#   LIPSYNC_AUDIO_DEVICE   cam2 ALSA device for playback audio (default hw:CARD=PCH,DEV=3 -- the
#                          SAME device the QPSK marker uses, per issue 930's scope item 2)
#   LIPSYNC_PLAYBACK_PIDFILE  where this script's own ffmpeg PID is tracked on cam2 (default
#                             /run/rig-lipsync-playback.pid)

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"

PAINTER_IP="${PAINTER_IP:-10.77.9.62}"
CAM_PW="${CAM_PW:-newlevel}"
PAINTER_PIDFILE="${PAINTER_PIDFILE:-/run/rig-painter.pid}"
LIPSYNC_FB_DEVICE="${LIPSYNC_FB_DEVICE:-/dev/fb0}"
LIPSYNC_AUDIO_DEVICE="${LIPSYNC_AUDIO_DEVICE:-hw:CARD=PCH,DEV=3}"
LIPSYNC_PLAYBACK_PIDFILE="${LIPSYNC_PLAYBACK_PIDFILE:-/run/rig-lipsync-playback.pid}"

# --------------------------------------------------------------------------------------------- #
# PURE functions (print remote-bash text; no network) -- sourced + unit-tested by
# tests/harness_lipsync_test_mode.rs, mirrors rig-mode.sh's own painter_launch_remote/
# painter_stop_remote convention (a REMOTE bash string the caller ssh's over).
# --------------------------------------------------------------------------------------------- #

# lipsync_stop_painter_cmds PIDFILE -- kill the TEST-mode painter by its OWN pidfile (never a bare
# `pkill -f frame-probe`, which would also match this very ssh command's cmdline -- same
# discipline rig-mode.sh's own painter_stop_remote already documents). Killing it frees BOTH
# /dev/fb0 (video) AND the QPSK marker's ALSA device (audio) in one shot, since the marker is a
# thread inside this same process (#420).
lipsync_stop_painter_cmds() {
  local pidfile="$1"
  cat <<CMDS
PID=\$(cat '$pidfile' 2>/dev/null || true)
if [ -n "\$PID" ] && kill -0 "\$PID" 2>/dev/null; then
  kill "\$PID" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "\$PID" 2>/dev/null || break; sleep 0.3; done
  # issue 930 live incident: a wedged painter SURVIVED the bare TERM (kept flipping KMS pages,
  # so the whole lipsync recording captured the dual-QR instead of the face while ffmpeg wrote
  # into an invisible fb0). Escalate to SIGKILL, and FAIL LOUD if even that leaves it alive --
  # a surviving painter makes the upcoming playback silently unrecordable.
  if kill -0 "\$PID" 2>/dev/null; then
    kill -9 "\$PID" 2>/dev/null || true
    for _ in 1 2 3 4 5; do kill -0 "\$PID" 2>/dev/null || break; sleep 0.3; done
  fi
  if kill -0 "\$PID" 2>/dev/null; then
    echo "FAIL: TEST-mode painter (pid \$PID) survived TERM+KILL -- refusing to start lipsync playback under a live painter" >&2
    exit 1
  fi
fi
rm -f '$pidfile'
CMDS
}

# lipsync_pacing_guard_cmd MEDIA FB_DEVICE AUDIO_DEVICE -- issue 930 finding 9 (elapsed-vs-
# duration budget) + the follow-up pacing-hypothesis measurement (comment on #930): /dev/fb0 has
# no clock of its own -- ffmpeg's frame pacing rests entirely on ALSA backpressure from the audio
# sink, and WITHOUT `-re` (real-time input read rate) that backpressure does NOT actually pace the
# video: a direct wall-clock measurement found frames delivered in 4-5-frame BURSTS ~4ms apart
# separated by ~80ms STALLS (a ~12.5fps slideshow), while running ~5% cumulatively fast vs audio --
# and the OLD elapsed-vs-duration-only check PASSED throughout, because ffmpeg's total wall-clock
# runtime is governed by the audio drain, not by whether the video was paced evenly inside that
# window. `-re` (added below and in `lipsync_playback_cmds`) fixes the pacing (measured: p50
# 16.663ms, std 1.7ms, zero stalls, steady-state drift <1ms/40s); this guard now ALSO instruments
# the SAME foreground pass with `-vf showinfo` and a python3 probe that wall-clock-timestamps every
# frame the filter reports, so a future pacing regression can never again hide behind a
# total-elapsed-only budget check. Cadence is asserted from `startup_skip_s` seconds in (default
# 2s, overridable via LIPSYNC_PACING_STARTUP_SKIP_S -- the fix's own one-time ~0.5s startup step is
# a documented, accepted exception, not a defect) onward: p95 deviation from the asset's own
# nominal (fps-derived) frame interval must stay within 5ms, zero deltas may exceed 33ms (a
# dropped-frame-class stall), and fewer than 2% of deltas may be sub-4ms bursts. Kept as its OWN
# function/ssh round trip (not folded into `lipsync_playback_cmds`) so it never touches the
# persistent launch's `/run/*.pid`/`/run/*.log` paths -- those need a real root/remote session,
# while this guard is independently testable (a fake ffmpeg/ffprobe on PATH, see
# tests/harness_lipsync_test_mode.rs). The python3 probe is piped to `python3 -` via a nested
# heredoc rather than written to a file under /run -- same "prints remote bash, no network in the
# function itself" convention, without needing real filesystem access to construct/test the string.
lipsync_pacing_guard_cmd() {
  local media="$1" fb="$2" audio="$3"
  cat <<CMDS
python3 - '$media' '$fb' '$audio' <<'PYEOF'
import os
import subprocess
import sys
import time

media, fb, audio = sys.argv[1:4]


def ffprobe(extra_args):
    out = subprocess.run(
        ["ffprobe"] + extra_args + [media], capture_output=True, text=True
    )
    return out.stdout.strip()


duration = float(
    ffprobe(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
)

fps_raw = ffprobe(
    [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=r_frame_rate",
        "-of",
        "csv=p=0",
    ]
)
try:
    num, _, den = fps_raw.partition("/")
    fps = float(num) / float(den) if den else float(num)
    if fps <= 0:
        raise ValueError(fps)
except (ValueError, ZeroDivisionError):
    sys.stderr.write(
        "WARN: could not parse fps from ffprobe output {!r} -- defaulting to "
        "60fps for the cadence nominal\n".format(fps_raw)
    )
    fps = 60.0
nominal_ms = 1000.0 / fps

startup_skip_s = float(os.environ.get("LIPSYNC_PACING_STARTUP_SKIP_S", "2"))

cmd = [
    "ffmpeg",
    "-y",
    "-re",
    "-i",
    media,
    "-map",
    "0:v",
    "-vf",
    "showinfo",
    "-pix_fmt",
    "bgra",
    "-nostats",
    "-f",
    "fbdev",
    fb,
    "-map",
    "0:a",
    "-ac",
    "2",
    "-f",
    "alsa",
    audio,
]

start = time.monotonic()
proc = subprocess.Popen(
    cmd, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, text=True, bufsize=1
)
frame_times = []
for line in proc.stderr:
    if "Parsed_showinfo" in line and "pts_time" in line:
        frame_times.append(time.monotonic() - start)
proc.wait()
elapsed = time.monotonic() - start

if proc.returncode != 0:
    sys.stderr.write(
        "FAIL: lipsync playback pacing check -- ffmpeg exited {} after {:.3f}s ({} frames "
        "observed before it died) -- not a pacing verdict, the preflight pass itself "
        "failed\n".format(proc.returncode, elapsed, len(frame_times))
    )
    sys.exit(1)

budget = duration * 0.005 + 1
elapsed_over = abs(elapsed - duration) > budget

deltas_ms = []
for i in range(1, len(frame_times)):
    if frame_times[i] >= startup_skip_s:
        deltas_ms.append((frame_times[i] - frame_times[i - 1]) * 1000.0)

n = len(deltas_ms)
stalls = sum(1 for d in deltas_ms if d > 33.0)
bursts = sum(1 for d in deltas_ms if d < 4.0)
burst_frac = (bursts / n) if n else 0.0
devs = sorted(abs(d - nominal_ms) for d in deltas_ms)
p95_dev = devs[int(0.95 * (len(devs) - 1))] if devs else 0.0

cadence_bad = n > 0 and (p95_dev > 5.0 or stalls > 0 or burst_frac >= 0.02)

summary = (
    "elapsed={:.3f}s duration={:.3f}s budget={:.3f}s cadence nominal={:.3f}ms "
    "p95_dev={:.3f}ms stalls(>33.0ms)={} bursts(<4.0ms)={:.1f}% deltas={}"
).format(elapsed, duration, budget, nominal_ms, p95_dev, stalls, burst_frac * 100.0, n)

if elapsed_over or cadence_bad:
    sys.stderr.write("FAIL: lipsync playback pacing check -- " + summary + "\n")
    sys.exit(1)

print("ok: playback pacing check passed (" + summary + ")")
PYEOF
CMDS
}

# lipsync_playback_cmds MEDIA FB_DEVICE AUDIO_DEVICE PLAYBACK_PIDFILE -- the ONE persistent ffmpeg
# process feeding both sinks from a single demux/decode timeline (live-verified on cam2, issue
# 930): bgra pixel format (matches src/probe/fb.rs's own painter convention), -ac 2 (the ALSA
# device refused a mono stream in the live sanity test), backgrounded + its PID tracked so `stop`
# can find it. `-stream_loop -1` loops the (short, ~60s) asset continuously for an arbitrary-length
# recording window. `-re` (930 pacing follow-up) reads the input in real time -- without it
# /dev/fb0's own lack of a clock means ALSA backpressure alone does not pace the video (measured:
# 4-5-frame bursts, ~80ms stalls); this is the SAME fix as `lipsync_pacing_guard_cmd`'s, applied
# to the actual persistent playback that plays during a recording, not just the preflight check.
# Callers should run `lipsync_pacing_guard_cmd` first (see above).
lipsync_playback_cmds() {
  local media="$1" fb="$2" audio="$3" pidfile="$4"
  cat <<CMDS
nohup ffmpeg -y -re -stream_loop -1 -i '$media' \\
  -map 0:v -pix_fmt bgra -f fbdev '$fb' \\
  -map 0:a -ac 2 -f alsa '$audio' \\
  > /run/rig-lipsync-playback.log 2>&1 &
echo \$! > '$pidfile'
disown
sleep 1
PID=\$(cat '$pidfile')
kill -0 "\$PID" 2>/dev/null || { echo "FAIL: lipsync playback ffmpeg (pid \$PID) died immediately -- see /run/rig-lipsync-playback.log" >&2; cat /run/rig-lipsync-playback.log >&2 || true; exit 1; }
echo "ok: lipsync playback running (pid \$PID, media=$media, fb=$fb, audio=$audio)"
CMDS
}

# lipsync_stop_playback_cmds PLAYBACK_PIDFILE -- the counterpart kill for `stop`.
lipsync_stop_playback_cmds() {
  local pidfile="$1"
  cat <<CMDS
PID=\$(cat '$pidfile' 2>/dev/null || true)
if [ -n "\$PID" ] && kill -0 "\$PID" 2>/dev/null; then
  kill "\$PID" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "\$PID" 2>/dev/null || break; sleep 0.3; done
  kill -9 "\$PID" 2>/dev/null || true
fi
rm -f '$pidfile'
CMDS
}

cam_ssh() {
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 root@"$PAINTER_IP" "$1"
}

# --------------------------------------------------------------------------------------------- #
# Subcommands
# --------------------------------------------------------------------------------------------- #

cmd_start() {
  local media="${1:-$REPO_ROOT/assets/lipsync/test.mp4}"
  [ -f "$media" ] || {
    echo "[lipsync-test-mode] FAIL: $media not found -- run 'lipsync-asset.sh fetch' first" >&2
    exit 1
  }
  # /run (tmpfs): cam2 is a READ-ONLY-root appliance (issue 547) -- /root is not writable, the
  # first live run failed the scp with `dest open "/root/lipsync-test.mp4": Failure`.
  local remote_media="/run/lipsync-test.mp4"
  echo "[lipsync-test-mode] cam2 (${PAINTER_IP}): stopping TEST-mode painter (frees /dev/fb0 + the ALSA marker device)"
  cam_ssh "$(lipsync_stop_painter_cmds "$PAINTER_PIDFILE")"
  # From here on cam2 has NEITHER the QR/QPSK painter NOR (yet) the lipsync playback running -- a
  # scp/ssh failure in either of the next two steps would otherwise abort under `set -e` and leave
  # cam2 with no painter and no marker at all. `errtrace` makes the ERR trap fire even when the
  # failing command is inside a called function (`cam_ssh`), so this restores TEST mode
  # automatically on ANY failure in this window; cleared right before this function returns
  # successfully (930 finding 8).
  set -o errtrace
  trap 'bash "$HERE/rig-mode.sh" test' ERR
  echo "[lipsync-test-mode] uploading $media -> cam2:$remote_media"
  sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "$media" root@"${PAINTER_IP}:${remote_media}"
  echo "[lipsync-test-mode] cam2: pacing sanity check (fb=${LIPSYNC_FB_DEVICE}, audio=${LIPSYNC_AUDIO_DEVICE})"
  cam_ssh "$(lipsync_pacing_guard_cmd "$remote_media" "$LIPSYNC_FB_DEVICE" "$LIPSYNC_AUDIO_DEVICE")"
  echo "[lipsync-test-mode] cam2: starting lipsync playback (fb=${LIPSYNC_FB_DEVICE}, audio=${LIPSYNC_AUDIO_DEVICE})"
  cam_ssh "$(lipsync_playback_cmds "$remote_media" "$LIPSYNC_FB_DEVICE" "$LIPSYNC_AUDIO_DEVICE" "$LIPSYNC_PLAYBACK_PIDFILE")"
  trap - ERR
  echo "[lipsync-test-mode] RESULT: lipsync-test mode ACTIVE on cam2 -- record now, then run 'lipsync-test-mode.sh stop' to restore TEST mode"
}

cmd_stop() {
  echo "[lipsync-test-mode] cam2 (${PAINTER_IP}): stopping lipsync playback"
  cam_ssh "$(lipsync_stop_playback_cmds "$LIPSYNC_PLAYBACK_PIDFILE")" || true
  cam_ssh "rm -f /run/lipsync-test.mp4" || true
  echo "[lipsync-test-mode] restoring TEST mode (dual-QR + QPSK marker) via rig-mode.sh test"
  bash "$HERE/rig-mode.sh" test
}

main() {
  case "${1:-}" in
    start) shift; cmd_start "$@" ;;
    stop) cmd_stop ;;
    *)
      echo "usage: $0 {start [media]|stop}" >&2
      exit 2
      ;;
  esac
}

# Run main only when EXECUTED, not when SOURCED (tests/harness_lipsync_test_mode.rs sources this
# file and calls the pure *_cmds functions directly without touching the network).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
