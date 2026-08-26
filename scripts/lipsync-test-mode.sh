#!/usr/bin/env bash
# lipsync-test-mode.sh -- issue 930 + issue 1187: swap cam2's TEST-mode output from the dual-QR/QPSK
# painter to the lipsync cross-validation asset, and back again.
#
set -euo pipefail
#
# WHY / WHAT (issue 930): rig-mode.sh's TEST mode already puts camera-box on cam2 into no-display
# mode (fb0 free, capture+emit keep running -- #291/#528) and launches the transient dual-QR
# painter WITH the QPSK audio-marker thread (#420 -- the marker is a THREAD inside the SAME
# frame-probe process, not a separate daemon) on the HDMI CRTC + hw:CARD=PCH,DEV=3. `start` below
# just needs to STOP that one process (which frees BOTH the display AND the ALSA device in one kill,
# since the marker dies with it) and start ONE playback process playing the lipsync asset into the
# SAME two sinks (video -> HDMI, audio -> the SAME ALSA device the QPSK marker used) from a SINGLE
# demux/decode timeline. `stop` kills the playback and calls rig-mode.sh test to fully restore +
# re-verify TEST mode (dual-QR + QPSK marker, burns, NDI mapping) -- never a partial/ad-hoc restore.
#
# WHY DRM/KMS, not raw fb0 (issue 1187, the owner-preferred ROOT fix of issue 1176 prong 3):
# the ORIGINAL implementation wrote video into raw /dev/fb0 via `ffmpeg -f fbdev`. fbdev is legacy:
# it has no CRTC ownership and no exit teardown, so when ffmpeg was killed its last decoded frame
# STAYED resident in fb0 memory, and the kernel's generic fbdev emulation revealed it on cam2's
# HDMI monitor the instant the painter's DRM master was released (issue 1176). The fix moves
# playback onto DRM/KMS via `mpv --vo=drm`: mpv takes the DRM master, page-flips its OWN buffers at
# vblank (never touches fb0), and cleanly restores the CRTC on exit -- the stale-frame class
# disappears STRUCTURALLY. mpv also paces off vblank natively, so the old fbdev-specific pacing
# guard (whose whole reason to exist was "/dev/fb0 has no clock of its own") is replaced by a
# lightweight decode + presence PREFLIGHT that touches neither fb0 nor the CRTC. The stop path still
# blanks fb0 belt-and-braces (the #660 mechanism) because after ANY DRM master release the kernel
# fbdev emulation can re-take scanout from fb0 memory -- neutralizing that legacy surface stays
# necessary regardless (issue 1176 owner note; relates to the open issue-1173 deadman half). mpv is
# provisioned by scripts/setup-device.sh STEP 16 and acceptance-checked by scripts/verify-device.sh.
#
# Usage:
#   lipsync-test-mode.sh start [media]   -- stop the TEST-mode painter, play [media] (default
#                                           assets/lipsync/test.mp4) looped on cam2's HDMI+ALSA
#   lipsync-test-mode.sh stop            -- kill the lipsync playback, blank fb0, restore TEST mode
#                                           via rig-mode.sh test (dual-QR + QPSK marker back + verified)
#
# Env:
#   PAINTER_IP        cam2 device IP (default 10.77.9.62, matches rig-mode.sh)
#   CAM_PW            cam2 root ssh password (default newlevel, matches targets.md)
#   PAINTER_PIDFILE   the TEST-mode painter's pidfile (default /run/rig-painter.pid, matches
#                     rig-mode.sh's own constant -- MUST stay in lock-step, it is the SAME painter)
#   LIPSYNC_DRM_DEVICE     cam2 DRM/KMS device for mpv's video sink (default empty -- mpv
#                          auto-selects the connected KMS card; #854: /dev/dri/cardN numbering is not
#                          a stable ABI, so auto is the safe default, pin only if a box needs it)
#   LIPSYNC_FB_DEVICE      cam2 framebuffer device to BLANK on stop (default /dev/fb0 -- the legacy
#                          surface the kernel fbdev emulation re-takes after a DRM master release)
#   LIPSYNC_AUDIO_DEVICE   cam2 ALSA device for playback audio (default hw:CARD=PCH,DEV=3 -- the
#                          SAME device the QPSK marker uses, per issue 930's scope item 2)
#   LIPSYNC_MPV_BIN        mpv binary to use (default mpv -- overridable for a pinned build/test)
#   LIPSYNC_PLAYBACK_GAIN_DB  fixed playback gain in dB (default 9), applied via mpv's
#                          `--af=volume=<N>dB` audio filter. NOTE: this is a FIXED gain, not a
#                          dynamic peak-normalizer -- +9 dB is CALIBRATED to bring THIS asset's known
#                          -9.8 dBFS peak to ~-1 dBFS (re-derive it for a different asset). Issue
#                          1191: the asset speech (peak -9.8 dBFS) is ~25 dB under the mic-chain AGC
#                          operating point set by the
#                          loud QPSK marker (~0 dBFS), so un-boosted speech captures ~-50 dBFS and
#                          SyncNet reads conf ~1 on every chunk (unmeasurable). +9 dB brings speech
#                          to ~-1 dBFS, into the AGC operating point (live-verified: envelope corr
#                          0.976, SyncNet conf 6.4). Expanded on the REMOTE (cam2) side so the
#                          default is baked self-documenting into the generated mpv command; the
#                          supervisor can re-tune via the paired cross-check campaign without a
#                          code change.
#   LIPSYNC_AUDIO_RATE     forced mpv audio OUTPUT sample rate in Hz (default 48000), applied via
#                          `--audio-samplerate=<Hz>` (mpv resamples the asset's native rate to it).
#                          Issue 1174 round-3/4 diagnosis: the HDMI->mic chain runs 48k natively and
#                          a 44.1k ALSA stream's mode lock is FLAKY per stream-start (round-1 locked
#                          spontaneously after 26.8 min; rounds 2-4 never locked, envelope corr
#                          ~0.23-0.35), while the ONE manual probe played at 48k locked FIRST TRY
#                          (corr 0.976, SyncNet conf 6.4). Forcing 48k output removes the 44.1k
#                          mode-switch from the chain entirely. Expanded on the REMOTE (cam2) side
#                          like the gain seam; set 44100 to reproduce the flaky-lock case.
#   LIPSYNC_PLAYBACK_PIDFILE  where this script's own mpv PID is tracked on cam2 (default
#                             /run/rig-lipsync-playback.pid)
#   LIPSYNC_AUDIO_LEAD_MS  static audio-lead compensation, in ms (default 0, non-negative integer
#                          only). Issue 930 derived the ffmpeg/ALSA output pipeline depth D at ~408ms
#                          via R = C + L - D (R = SyncNet-measured rig-added offset, C = the chain
#                          offset per QR/QPSK at the same genlock_latency knob, L = this lead) and
#                          used 408 as the default. Issue 1187 changed the compensation MECHANISM
#                          from a two-demux ffmpeg -itsoffset to mpv's native --audio-delay (a
#                          NEGATIVE value, which delays VIDEO relative to audio -- the exact
#                          equivalent of the old positive video -itsoffset). Issue 1191 changes the
#                          DEFAULT to 0: under mpv the measured offset at lead=0 is +40ms (≈ ±1 frame
#                          of zero), so 408 was a stale ffmpeg-era constant that injected a false
#                          ~0.4s shift. 408 stays available via this env seam for re-derivation on
#                          mpv's ALSA buffering (the supervisor re-tunes via the paired cross-check
#                          campaign without a code change). 0 = no compensation (--audio-delay=0.000).
#                          Only the RELATIVE offset between the two streams matters for lipsync
#                          perception.
#   LIPSYNC_ARRIVAL_ENABLE  1 (default) = after starting playback, VERIFY the asset speech actually
#                          reached the mbc mic chain (issue 1192); 0 = skip (cam2-only test, stream
#                          OBS unreachable). The HDMI->mic audio sink lock is flaky per
#                          audio-stream-start (issue 1174), so a blind start otherwise wastes whole
#                          recording rounds with dead speech.
#   LIPSYNC_ARRIVAL_CORR_MIN  min envelope correlation (probe recording vs local asset) counted as
#                          "speech arrived" (default 0.6 -- live: ~0.22-0.35 dead, 0.976 arrived).
#   LIPSYNC_ARRIVAL_RETRIES  max probe attempts; each failed attempt recycles the mpv playback (a
#                          fresh audio-stream-start = a new chance at the flaky sink lock) (default 4).
#   LIPSYNC_ARRIVAL_PROBE_S  length of each stream-OBS probe recording, seconds (default 15).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"

# The canonical #660 fb0-blank builder (rig_test_ledger_clean_paint_fallback_cmds) lives here --
# ONE source of truth for the blank mechanism, reused by the stop path below. rig-test-ledger.sh is
# a pure function library (it deliberately never sets `set -euo pipefail`, so sourcing it does not
# mutate this script's shell options).
. "$HERE/lib/rig-test-ledger.sh"
# issue 1192: the speech-arrival VERIFY probes the STREAM OBS box (record + pull) and reuses the
# throwaway-probe delete builder. win-ssh-exec.sh gives win_ssh_download/win_ssh_run (it sets
# `set -euo pipefail`, harmless -- this script already does at the top); audio-presence-preflight.sh
# gives audio_preflight_delete_ps (the same "delete a throwaway probe recording on the box" builder).
. "$HERE/lib/win-ssh-exec.sh"
. "$HERE/lib/audio-presence-preflight.sh"

PAINTER_IP="${PAINTER_IP:-10.77.9.62}"
CAM_PW="${CAM_PW:-newlevel}"
PAINTER_PIDFILE="${PAINTER_PIDFILE:-/run/rig-painter.pid}"
LIPSYNC_DRM_DEVICE="${LIPSYNC_DRM_DEVICE:-}"
LIPSYNC_FB_DEVICE="${LIPSYNC_FB_DEVICE:-/dev/fb0}"
LIPSYNC_AUDIO_DEVICE="${LIPSYNC_AUDIO_DEVICE:-hw:CARD=PCH,DEV=3}"
LIPSYNC_MPV_BIN="${LIPSYNC_MPV_BIN:-mpv}"
LIPSYNC_PLAYBACK_PIDFILE="${LIPSYNC_PLAYBACK_PIDFILE:-/run/rig-lipsync-playback.pid}"
LIPSYNC_AUDIO_LEAD_MS="${LIPSYNC_AUDIO_LEAD_MS:-0}"
case "$LIPSYNC_AUDIO_LEAD_MS" in
  ''|*[!0-9]*)
    echo "[lipsync-test-mode] FAIL: LIPSYNC_AUDIO_LEAD_MS must be a non-negative integer (ms), got '$LIPSYNC_AUDIO_LEAD_MS'" >&2
    exit 1
    ;;
esac

# --- issue 1192: speech-arrival VERIFY seams ------------------------------------------------- #
# After the mpv playback starts, PROVE the asset speech reached the mbc mic chain via a short probe
# recording on stream OBS + envelope correlation vs the local asset, and recycle the playback on a
# low correlation (the HDMI->mic audio sink lock is flaky per audio-stream-start, issue 1174). ON by
# default (the needed check must never be a forgettable toggle); ENABLE=0 is the escape for a
# cam2-only test where stream OBS is unreachable.
LIPSYNC_ARRIVAL_ENABLE="${LIPSYNC_ARRIVAL_ENABLE:-1}"
LIPSYNC_ARRIVAL_CORR_MIN="${LIPSYNC_ARRIVAL_CORR_MIN:-0.6}"       # min envelope corr = "speech arrived"
LIPSYNC_ARRIVAL_RETRIES="${LIPSYNC_ARRIVAL_RETRIES:-4}"           # max probe attempts (each recycles mpv)
LIPSYNC_ARRIVAL_PROBE_S="${LIPSYNC_ARRIVAL_PROBE_S:-15}"          # probe recording length, seconds
LIPSYNC_ARRIVAL_SSH_TIMEOUT="${LIPSYNC_ARRIVAL_SSH_TIMEOUT:-90}"  # per scp/ssh bound to the stream box
LIPSYNC_ARRIVAL_READ_ATTEMPTS="${LIPSYNC_ARRIVAL_READ_ATTEMPTS:-4}"      # pull+decode retries (moov race)
LIPSYNC_ARRIVAL_READ_RETRY_SLEEP="${LIPSYNC_ARRIVAL_READ_RETRY_SLEEP:-3}" # settle between those retries
# The stream OBS box that carries the mbc measurement audio (targets.md; env names match
# scripts/recording-e2e.sh so an operator setting them once covers both scripts).
STREAM="${STREAM:-10.77.9.204}"
STREAM_USER="${STREAM_USER:-newlevel}"
STREAM_PW="${STREAM_PW:-newlevel}"

# --------------------------------------------------------------------------------------------- #
# PURE functions (print remote-bash text; no network) -- sourced + unit-tested by
# tests/harness_lipsync_test_mode.rs, mirrors rig-mode.sh's own painter_launch_remote/
# painter_stop_remote convention (a REMOTE bash string the caller ssh's over).
# --------------------------------------------------------------------------------------------- #

# lipsync_stop_painter_cmds PIDFILE -- kill the TEST-mode painter by its OWN pidfile (never a bare
# `pkill -f frame-probe`, which would also match this very ssh command's cmdline -- same
# discipline rig-mode.sh's own painter_stop_remote already documents). Killing it frees BOTH the
# HDMI display (video) AND the QPSK marker's ALSA device (audio) in one shot, since the marker is a
# thread inside this same process (#420).
lipsync_stop_painter_cmds() {
  local pidfile="$1"
  cat <<CMDS
# issue 1190: the steady-state painter runs under cam2-painter.service (Restart=always, issue 1008
# model). A pidfile-ONLY kill lets systemd respawn it ~100ms later and the respawn re-takes the DRM
# master, so the mpv --vo=drm playback started ~10s later cannot acquire the CRTC and dies instantly.
# Stop the UNIT first -- systemd will not respawn a stopped unit; best-effort (the unit may be absent
# in a transient-only scenario). The pidfile kill below then stays as a belt for the transient,
# unit-less verification-only nohup painter (issue 930/1008 lifecycle).
systemctl stop cam2-painter 2>/dev/null || true
PID=\$(cat '$pidfile' 2>/dev/null || true)
if [ -n "\$PID" ] && kill -0 "\$PID" 2>/dev/null; then
  kill "\$PID" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "\$PID" 2>/dev/null || break; sleep 0.3; done
  # issue 930 live incident: a wedged painter SURVIVED the bare TERM (kept flipping KMS pages,
  # so the whole lipsync recording captured the dual-QR instead of the face). Escalate to SIGKILL,
  # and FAIL LOUD if even that leaves it alive -- a surviving painter (still holding the DRM
  # master) makes the upcoming mpv playback silently unstartable/unrecordable.
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
# issue 1190: FAIL LOUD if the unit is somehow STILL active after the stop -- a live unit would
# respawn the painter and re-take the DRM master, making the mpv --vo=drm playback impossible
# (mirrors the survived-TERM+KILL fail-loud above). A stopped or absent unit reports not-active,
# which is the pass.
if [ "\$(systemctl is-active cam2-painter 2>/dev/null)" = "active" ]; then
  echo "FAIL: cam2-painter.service is still active after 'systemctl stop' -- it would respawn the painter and re-take the DRM master, making mpv --vo=drm playback impossible" >&2
  exit 1
fi
CMDS
}

# lipsync_preflight_cmd MEDIA -- issue 1187: a lightweight mpv decode + presence PREFLIGHT that
# replaces the old fbdev-specific pacing guard. It (1) checks mpv is installed and FAILs loud with a
# provisioning hint if not (setup-device.sh STEP 16 provisions it), and (2) decodes a bounded number
# of frames to NULL sinks (`--vo=null --ao=null`) to prove the asset is decodable and mpv is
# functional. It touches NEITHER /dev/fb0 NOR the DRM/KMS CRTC -- so running it before the painter is
# even restored can never leave a stale frame or fight the display. The old cadence-measurement
# apparatus is gone on purpose: mpv paces off vblank natively (`--vo=drm`), so the fbdev-no-clock
# bug class the guard existed to catch no longer exists. Kept as its OWN function/ssh round trip
# (not folded into `lipsync_playback_cmds`) so it never touches the persistent launch's
# /run/*.pid//run/*.log paths -- and stays independently testable (a fake mpv via LIPSYNC_MPV_BIN,
# see tests/harness_lipsync_test_mode.rs).
lipsync_preflight_cmd() {
  local media="$1" mpv_bin="${LIPSYNC_MPV_BIN:-mpv}"
  cat <<CMDS
command -v $mpv_bin >/dev/null 2>&1 || { echo "FAIL: lipsync preflight -- mpv ('$mpv_bin') not installed on cam2. Provision it via scripts/setup-device.sh (STEP 16 installs mpv) or 'apt-get install -y mpv'." >&2; exit 1; }
$mpv_bin --no-config --no-terminal --vo=null --ao=null --frames=120 '$media' >/dev/null 2>&1 || { echo "FAIL: lipsync preflight -- mpv ('$mpv_bin') could not decode '$media' (asset missing/corrupt, or mpv broken)." >&2; exit 1; }
echo "ok: mpv preflight passed (decoded '$media' to null sinks, mpv present + functional)"
CMDS
}

# lipsync_playback_cmds MEDIA DRM_DEVICE AUDIO PIDFILE [AUDIO_LEAD_MS] -- issue 1187: the ONE
# persistent mpv process feeding both sinks. Video goes to DRM/KMS (`--vo=drm`) -- mpv takes the DRM
# master, page-flips its OWN buffers at vblank (never touches /dev/fb0) and restores the CRTC on
# exit. Audio goes to the SAME ALSA device (`--audio-device=alsa/<AUDIO>`), forced stereo
# (`--audio-channels=stereo` -- the live sanity test found the device refuses mono). `--loop-file=inf`
# loops the (short, ~60s) asset continuously for an arbitrary-length recording window. Backgrounded +
# its PID tracked so `stop` can find it; fail-loud liveness check (never claim a launch succeeded
# without checking the process is actually alive). DRM_DEVICE empty = mpv auto-selects the connected
# KMS card (#854); a non-empty value pins it via `--drm-device`.
#
# GAIN (issue 1191): `--af=volume=${LIPSYNC_PLAYBACK_GAIN_DB:-9}dB` applies a FIXED +N dB gain
# (default 9, CALIBRATED to THIS asset's -9.8 dBFS peak -> ~-1 dBFS; not a dynamic normalizer) so the
# asset speech lands in the mic-chain AGC operating point set by the loud QPSK marker (~0 dBFS).
# Without it
# the ~25 dB quieter asset speech (peak -9.8 dBFS) captures at ~-50 dBFS and SyncNet reads conf ~1 on
# every chunk (unmeasurable). The env seam is expanded on the REMOTE (cam2) side -- the default (9)
# is baked self-documenting into the generated command AND a supervisor can re-tune the gain without
# a code change. It is orthogonal to AUDIO_LEAD_MS (one affects level, the other the A/V offset).
#
# AUDIO_LEAD_MS (issue 930, carried into 1187): the calibrated ALSA-output-pipeline-depth
# compensation. mpv's native `--audio-delay` replaces the old two-demux ffmpeg `-itsoffset`: a
# NEGATIVE value delays the VIDEO relative to audio (mpv semantics: positive delays audio, negative
# delays video), the exact equivalent of the old positive `-itsoffset` on the video input -- only
# the RELATIVE offset between the two streams matters for lipsync perception. 0 (or omitted) emits
# `--audio-delay=0.000` (no compensation). Still exactly ONE mpv process/PID -- the existing
# single-pidfile kill lifecycle in `lipsync_stop_playback_cmds` is unchanged.
lipsync_playback_cmds() {
  local media="$1" drm="$2" audio="$3" pidfile="$4" lead_ms="${5:-0}" mpv_bin="${LIPSYNC_MPV_BIN:-mpv}"
  local drm_opt=""
  [ -n "$drm" ] && drm_opt="--drm-device=$drm "
  local delay_s
  if [ "$lead_ms" -eq 0 ]; then
    delay_s="0.000"
  else
    delay_s="$(awk -v ms="$lead_ms" 'BEGIN { printf "%.3f", -ms / 1000 }')"
  fi
  cat <<CMDS
# issue 1190: mpv runs --no-terminal, which swallows its own log/error output -- when mpv dies
# instantly (e.g. it cannot acquire the DRM master) /run/rig-lipsync-playback.log is empty and the
# death is undiagnosable from the box. --log-file is mpv's NATIVE log sink and writes regardless of
# --no-terminal, so a fatal error is captured; the die-immediately branch below cats it too.
nohup $mpv_bin --no-config --no-terminal --log-file=/run/rig-lipsync-playback.mpv.log --vo=drm ${drm_opt}--loop-file=inf \\
  --audio-device=alsa/$audio --audio-channels=stereo \\
  --audio-samplerate=\${LIPSYNC_AUDIO_RATE:-48000} \\
  --af=volume=\${LIPSYNC_PLAYBACK_GAIN_DB:-9}dB \\
  --audio-delay=$delay_s \\
  '$media' \\
  > /run/rig-lipsync-playback.log 2>&1 &
echo \$! > '$pidfile'
disown
sleep 1
PID=\$(cat '$pidfile')
kill -0 "\$PID" 2>/dev/null || { echo "FAIL: lipsync playback mpv (pid \$PID) died immediately -- see /run/rig-lipsync-playback.log and /run/rig-lipsync-playback.mpv.log (mpv's own log)" >&2; cat /run/rig-lipsync-playback.log >&2 || true; cat /run/rig-lipsync-playback.mpv.log >&2 || true; exit 1; }
echo "ok: lipsync playback running (pid \$PID, media=$media, drm=${drm:-auto}, audio=$audio, audio_lead_ms=$lead_ms)"
CMDS
}

# lipsync_stop_playback_cmds PLAYBACK_PIDFILE [FB_DEVICE] -- the counterpart kill for `stop`, plus
# the issue-1187 belt-and-braces fb0 blank. After mpv exits it restores the CRTC, but if the kernel
# fbdev emulation re-takes scanout from /dev/fb0 memory it could reveal whatever that memory last
# held -- so zero fb0 now (the canonical #660 mechanism, reused from rig-test-ledger.sh) to
# guarantee a black screen before rig-mode.sh restores the painter.
lipsync_stop_playback_cmds() {
  local pidfile="$1" fb="${2:-/dev/fb0}"
  cat <<CMDS
PID=\$(cat '$pidfile' 2>/dev/null || true)
if [ -n "\$PID" ] && kill -0 "\$PID" 2>/dev/null; then
  kill "\$PID" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "\$PID" 2>/dev/null || break; sleep 0.3; done
  kill -9 "\$PID" 2>/dev/null || true
fi
rm -f '$pidfile'
$(rig_test_ledger_clean_paint_fallback_cmds "$fb")
CMDS
}

# lipsync_arrival_corr_meets DB MIN -> "true"/"false" (issue 1192). The speech-arrival decision:
# the measured envelope correlation MEETS the threshold iff DB >= MIN (boundary inclusive -- a corr
# exactly at the threshold counts as arrived). Float-safe via awk (values like 0.976 / 0.31), same
# pure-function shape as audio-presence-preflight.sh's audio_preflight_is_silent so it is directly
# Tier-0 unit-testable by sourcing.
lipsync_arrival_corr_meets() {
  local db="$1" min="$2"
  if awk -v d="$db" -v m="$min" 'BEGIN { exit !((d + 0) >= (m + 0)) }'; then
    echo "true"
  else
    echo "false"
  fi
}

cam_ssh() {
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 root@"$PAINTER_IP" "$1"
}

# lipsync_playback_cleanup -- issue 1194: idempotent teardown of the lipsync playback on cam2,
# shared by cmd_stop AND the cmd_start ERR-trap / retries-exhausted fail path (ONE source of truth,
# never a re-inlined copy). It (1) kills the mpv playback by its pidfile + blanks fb0 (the issue-660
# belt, reused from lipsync_stop_playback_cmds) and (2) removes the uploaded /run asset. BOTH cam_ssh
# calls are best-effort (`|| true`) so the helper is safe to call when NOTHING is running (a fresh
# box, an early abort before the playback even started) and can NEVER itself fail/abort -- essential
# for the ERR-trap caller, where a non-zero command would re-enter `set -e`. This MUST run BEFORE any
# rig-mode.sh test restore: a restore that re-launches the cam2 painter while an mpv --vo=drm playback
# still holds the DRM master + ALSA device produces the issue-1194 hybrid state (painter dead on
# `snd_pcm_open ... Device or resource busy (16)` + card->fbdev fallback, mpv alive). Mirror of the
# issue-1190 start-side ordering (stop the resource holder BEFORE launching what needs the resource).
lipsync_playback_cleanup() {
  cam_ssh "$(lipsync_stop_playback_cmds "$LIPSYNC_PLAYBACK_PIDFILE" "$LIPSYNC_FB_DEVICE")" || true
  cam_ssh "rm -f /run/lipsync-test.mp4" || true
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
  echo "[lipsync-test-mode] cam2 (${PAINTER_IP}): stopping TEST-mode painter (frees the HDMI display + the ALSA marker device)"
  cam_ssh "$(lipsync_stop_painter_cmds "$PAINTER_PIDFILE")"
  # From here on cam2 has NEITHER the QR/QPSK painter NOR (yet) the lipsync playback running -- a
  # scp/ssh failure in either of the next two steps would otherwise abort under `set -e` and leave
  # cam2 with no painter and no marker at all. `errtrace` makes the ERR trap fire even when the
  # failing command is inside a called function (`cam_ssh`), so this restores TEST mode
  # automatically on ANY failure in this window; cleared right before this function returns
  # successfully (930 finding 8).
  set -o errtrace
  # issue 1194: kill the lipsync playback FIRST, THEN restore TEST mode -- a restore that
  # re-launches the cam2 painter while mpv still holds the DRM master + ALSA device leaves a
  # hybrid state (painter dead on a busy device, mpv alive). lipsync_playback_cleanup is
  # idempotent + never-fail (both cam_ssh calls `|| true`), so it is safe to run in the ERR trap
  # even before playback started, and cannot re-enter `set -e`.
  trap 'lipsync_playback_cleanup; bash "$HERE/rig-mode.sh" test' ERR
  echo "[lipsync-test-mode] uploading $media -> cam2:$remote_media"
  sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 "$media" root@"${PAINTER_IP}:${remote_media}"
  echo "[lipsync-test-mode] cam2: mpv decode preflight (media=$remote_media)"
  cam_ssh "$(lipsync_preflight_cmd "$remote_media")"
  echo "[lipsync-test-mode] cam2: starting lipsync playback (drm=${LIPSYNC_DRM_DEVICE:-auto}, audio=${LIPSYNC_AUDIO_DEVICE}, audio_lead_ms=${LIPSYNC_AUDIO_LEAD_MS})"
  cam_ssh "$(lipsync_playback_cmds "$remote_media" "$LIPSYNC_DRM_DEVICE" "$LIPSYNC_AUDIO_DEVICE" "$LIPSYNC_PLAYBACK_PIDFILE" "$LIPSYNC_AUDIO_LEAD_MS")"
  # issue 1192: PROVE the asset speech actually reached the mbc mic chain before claiming ACTIVE.
  # The HDMI->mic (mbc/Dante) audio sink lock is flaky per audio-stream-start (issue 1174); the host
  # side is always healthy in the dead state, so the ONLY reliable signal is a CONTENT check -- a
  # short probe recording on stream OBS, pulled and envelope-correlated against the LOCAL asset
  # (volumedetect is NOT sufficient: the mic-chain AGC pumps ambient to the ceiling even with dead
  # speech). On a low correlation, recycle the mpv playback (a fresh audio-stream-start = a new shot
  # at the flaky lock) and retry; fail loud with the attempt matrix on exhaustion. This whole block
  # stays INSIDE the ERR-trap window (trap cleared only after it), so a genuine infra failure AND the
  # exhaustion path both restore TEST mode via the trap set above.
  if [ "$LIPSYNC_ARRIVAL_ENABLE" = "1" ]; then
    echo "[lipsync-test-mode] arrival verify: proving the asset speech reached mbc on stream OBS (${STREAM}) -- envelope corr >= ${LIPSYNC_ARRIVAL_CORR_MIN}, up to ${LIPSYNC_ARRIVAL_RETRIES} attempts"
    local arrival_ok=0 arrival_matrix="" arrival_attempt _ap_win _ap_local _ap_corr _ap_out _ap_read
    for arrival_attempt in $(seq 1 "$LIPSYNC_ARRIVAL_RETRIES"); do
      # Short throwaway probe recording on stream (the mbc audio rides the program recording). A
      # leftover from an abort in the probe window self-heals -- obs_phase2.py record --action start
      # stops any orphan before it re-records. NOTE (deliberate): StartRecord is a bare command with
      # no `|| true` -- an OBS-WS/encoder failure here is INFRA, not the flaky sink lock, so it fails
      # loud through the ERR trap (restore TEST mode) rather than being counted as a retry attempt;
      # the retry loop exists ONLY for the arrival correlation, same split as the [4b2/8] preflight.
      python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start --password "${OBS_PASSWORD:-}" >/dev/null
      sleep "$LIPSYNC_ARRIVAL_PROBE_S"
      _ap_win="$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop --password "${OBS_PASSWORD:-}" || true)"
      if [ -z "$_ap_win" ]; then
        echo "[lipsync-test-mode] FAIL: stream StopRecord returned no path -- the arrival probe recording never started (OBS-WS/encoder problem on stream)" >&2
        false  # -> ERR trap: restore TEST mode, exit nonzero
      fi
      # Pull the probe to dev1 + envelope-correlate against the local asset, with a bounded retry for
      # the moov-atom-not-finalized race (StopRecord's RPC reply lands before the mp4 muxer finalizes
      # -- the same race the audio-presence preflight documents). `timeout` execvp()s its command so
      # it cannot invoke a shell FUNCTION; re-source the lib inside bash -c so bash resolves it.
      _ap_local="$(mktemp --suffix=.mp4)"
      _ap_corr=""
      _ap_out=""
      for _ap_read in $(seq 1 "$LIPSYNC_ARRIVAL_READ_ATTEMPTS"); do
        timeout "$LIPSYNC_ARRIVAL_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_download "$2" "$3" "$4" "$5" "$6"' _ \
          "$HERE/lib/win-ssh-exec.sh" "$STREAM_USER" "$STREAM_PW" "$STREAM" "$_ap_win" "$_ap_local" >/dev/null 2>&1 || true
        _ap_out="$(python3 "$HERE/lipsync_envelope_corr.py" --probe "$_ap_local" --asset "$media" --audio-map 0:a:0 2>&1 || true)"
        _ap_corr="$(printf '%s\n' "$_ap_out" | sed -n 's/^corr=//p' | head -1 || true)"
        [ -n "$_ap_corr" ] && break
        [ "$_ap_read" -lt "$LIPSYNC_ARRIVAL_READ_ATTEMPTS" ] && sleep "$LIPSYNC_ARRIVAL_READ_RETRY_SLEEP"
      done
      # Best-effort cleanup: delete the throwaway probe on the box + locally (never abort the run).
      timeout "$LIPSYNC_ARRIVAL_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
        "$HERE/lib/win-ssh-exec.sh" "$STREAM_USER" "$STREAM_PW" "$STREAM" "$(audio_preflight_delete_ps "$_ap_win")" >/dev/null 2>&1 || true
      rm -f "$_ap_local"
      if [ -z "$_ap_corr" ]; then
        echo "[lipsync-test-mode] FAIL: could not decode/correlate the arrival probe recording (ffmpeg/moov). Raw: ${_ap_out}" >&2
        false  # -> ERR trap: restore TEST mode, exit nonzero
      fi
      arrival_matrix="${arrival_matrix}"$'\n'"    attempt ${arrival_attempt}/${LIPSYNC_ARRIVAL_RETRIES}: envelope corr=${_ap_corr} (min ${LIPSYNC_ARRIVAL_CORR_MIN})"
      echo "[lipsync-test-mode] arrival attempt ${arrival_attempt}/${LIPSYNC_ARRIVAL_RETRIES}: envelope corr=${_ap_corr} (threshold ${LIPSYNC_ARRIVAL_CORR_MIN})"
      if [ "$(lipsync_arrival_corr_meets "$_ap_corr" "$LIPSYNC_ARRIVAL_CORR_MIN")" = "true" ]; then
        arrival_ok=1
        break
      fi
      if [ "$arrival_attempt" -lt "$LIPSYNC_ARRIVAL_RETRIES" ]; then
        echo "[lipsync-test-mode] speech arrival corr ${_ap_corr} < ${LIPSYNC_ARRIVAL_CORR_MIN} -- recycling mpv playback (fresh audio-stream-start) and retrying" >&2
        cam_ssh "$(lipsync_stop_playback_cmds "$LIPSYNC_PLAYBACK_PIDFILE" "$LIPSYNC_FB_DEVICE")"
        cam_ssh "$(lipsync_playback_cmds "$remote_media" "$LIPSYNC_DRM_DEVICE" "$LIPSYNC_AUDIO_DEVICE" "$LIPSYNC_PLAYBACK_PIDFILE" "$LIPSYNC_AUDIO_LEAD_MS")"
      fi
    done
    if [ "$arrival_ok" != "1" ]; then
      echo "[lipsync-test-mode] FAIL: asset speech never reached mbc after ${LIPSYNC_ARRIVAL_RETRIES} attempts (envelope corr stayed < ${LIPSYNC_ARRIVAL_CORR_MIN}) -- the HDMI->mic audio sink never locked (issue 1192). Attempt matrix:${arrival_matrix}" >&2
      false  # -> ERR trap: bash rig-mode.sh test restores TEST mode, script exits nonzero
    fi
    echo "[lipsync-test-mode] arrival verify PASSED: asset speech reached mbc (envelope corr ${_ap_corr} >= ${LIPSYNC_ARRIVAL_CORR_MIN})"
  else
    echo "[lipsync-test-mode] arrival verify SKIPPED (LIPSYNC_ARRIVAL_ENABLE=0) -- NOT confirming the asset speech reached mbc"
  fi
  trap - ERR
  echo "[lipsync-test-mode] RESULT: lipsync-test mode ACTIVE on cam2 -- record now, then run 'lipsync-test-mode.sh stop' to restore TEST mode"
}

cmd_stop() {
  echo "[lipsync-test-mode] cam2 (${PAINTER_IP}): stopping lipsync playback + blanking fb0"
  # issue 1194: kill mpv (+ blank fb0) and drop the /run asset via the shared idempotent helper --
  # ONE source of truth with the cmd_start ERR trap, no re-inlined copy. Always BEFORE the restore.
  lipsync_playback_cleanup
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
