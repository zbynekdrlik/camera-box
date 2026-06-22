#!/usr/bin/env bash
# Recording-based 4-node full-path E2E (#105 / #7), dev1-orchestrated.
#
# The loss verdict + per-hop latency come ONLY from RECORDED output (cam1 grab file,
# strih/stream OBS program recordings) and the cam2 painter ground truth — NEVER an
# NDI tap (the tap harness, scripts/multitap-e2e.sh, produced false sampling
# artifacts; this is its recording-based replacement, per the e2e-zero-loss memory).
#
# THE FOUR EVIDENCE NODES (per-frame id + timestamp at each, dual-QR tick=max):
#   1. cam2  — QR GENERATED on its monitor: painter `tick,gen_ts_ns` CSV
#              (frame-probe --paint-only --dual-qr --paint-log).
#   2. cam1  — camera GRAB: camera-box --record-grab streams the gray8 luma of each
#              EMITTED frame to a dev1 ffmpeg listener (cam1 has no ffmpeg/disk) which
#              encodes ffv1 cam1.mkv; a grab-ts sidecar carries cam1's grab instant.
#   3. strih — OBS PROGRAM recording (obs-ws StartRecord/StopRecord) .mkv.
#   4. stream— OBS PROGRAM recording .mp4.
#
# recording-verdict consumes ALL FOUR and reports, per hop, per-frame loss + latency:
#   cam2→cam1 (optical+grab): readability + honest assessment + REAL latency.
#   cam1→strih: STRICT zero-loss + latency (cam1→strih absolute needs #111, marked
#               RELATIVE/UNAVAILABLE rather than faked).
#   strih→stream: STRICT zero-loss + latency.
#   PASS = 0 undecodable AND 0 net loss on the strict hops AND span ≥ 300 s.
#
# TEST RIG: this reroutes the strih + stream OBS program to the probe scene and RECORDS
# them for the run; the teardown trap restores both program scenes + the cam1/cam2
# camera-box services + kills the dev1 ffmpeg listener on exit (incl. cancel). The
# operator is the guard (project decision: no automated streaming guard).
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi, cargo, sshpass, ffmpeg, python3 +
# websocket-client, matplotlib (for the report). OBS WebSocket :4455 on strih+stream,
# DistroAV "NDI Main Output" enabled on both. cam1/cam2 SSH (root, pw newlevel).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
camera_resolve "${CAM:-cam1}"

CAM1_IP="${CAM1_IP:-10.77.9.61}"      # the SOURCE camera (films cam2's monitor, records its grab)
PAINTER_IP="${PAINTER_IP:-10.77.9.62}" # cam2 — the box with the physical monitor cam1 films
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
# dev1 LAN IP cam1 connects to for the grab stream (resolved toward cam1, never localhost).
DEV1_IP="${DEV1_IP:-$(ip route get "$CAM1_IP" 2>/dev/null | grep -oP 'src \K\S+')}"
GRAB_PORT="${GRAB_PORT:-9099}"
RUN_ID="${RUN_ID:-$(( (RANDOM << 16) | RANDOM ))}"
DURATION="${DURATION:-1800}"
if [ "$DURATION" -lt 300 ]; then
  echo "ERROR: DURATION=${DURATION} below the 300 s zero-loss floor (default 1800)." >&2
  exit 1
fi
QR_SIZE="${QR_SIZE:-700}"
PAINT_FPS="${PAINT_FPS:-30}"
GENLOCK_FPS="${GENLOCK_FPS:-30}"
# The cam1 grab is the genlock-EMITTED 30 fps grid (the frames that reach NDI), at the
# capture resolution. ffmpeg on dev1 decodes the gray8 raw stream with these.
GRAB_W="${GRAB_W:-1920}"
GRAB_H="${GRAB_H:-1080}"
GRAB_FPS="${GRAB_FPS:-$GENLOCK_FPS}"
OUTDIR="${OUTDIR:-/tmp/recording-e2e-${RUN_ID}}"
mkdir -p "$OUTDIR"
CAM1_MKV="$OUTDIR/cam1-${RUN_ID}.mkv"
CAM1_GRAB_TS="$OUTDIR/cam1-grab-ts-${RUN_ID}.csv"
PAINTER_CSV="$OUTDIR/painter-${RUN_ID}.csv"
STRIH_REC="$OUTDIR/strih-${RUN_ID}.mkv"
STREAM_REC="$OUTDIR/stream-${RUN_ID}.mp4"
REPORT_JSON="$OUTDIR/verdict-${RUN_ID}.json"
REPORT_PNG="$OUTDIR/report-${RUN_ID}.png"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

echo "[0/8] reachability preflight (cam1 source, cam2 painter, strih, stream)"
for hp in "cam1=$CAM1_IP" "cam2(painter)=$PAINTER_IP" "strih=$STRIH" "stream=$STREAM"; do
  _name="${hp%%=*}"; _ip="${hp#*=}"
  if ping -c1 -W2 "$_ip" >/dev/null 2>&1; then echo "    ok: $_name ($_ip)"; else
    echo "ERROR: $_name ($_ip) UNREACHABLE from dev1 — fix route/host, then re-run." >&2; exit 1; fi
done
[ -n "$DEV1_IP" ] || { echo "ERROR: could not resolve dev1 LAN IP toward cam1." >&2; exit 1; }
echo "    dev1 grab-stream endpoint: tcp://${DEV1_IP}:${GRAB_PORT}"

# Clock-sync preflight: the cam2 paint gen_ts and cam1 grab_ts must share the wall
# clock for the cam2→cam1 latency to be sound. Fail loudly if cam1 has drifted.
echo "[0/8] verify cluster clock sync (cam1) for cam2→cam1 latency (#7/#8)"
CLOCK_GUARD_TARGETS="cam1=$CAM1_IP" "$HERE/clock-offset-guard.sh" \
  --bound-us "${CLOCK_GUARD_BOUND_US:-2000}"

FFMPEG_PID=""
# shellcheck disable=SC2317  # cleanup() runs via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  echo "[cleanup] restore OBS program scenes + cam1/cam2 services + kill grab listener"
  python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop >/dev/null 2>&1
  python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop >/dev/null 2>&1
  python3 "$HERE/obs_phase2.py" teardown --host "$STREAM"
  python3 "$HERE/obs_phase2.py" teardown --host "$STRIH"
  # cam1: stop the manual --record-grab camera-box, restore the deployed service.
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
    "pkill -x camera-box 2>/dev/null; sleep 1; systemctl restart camera-box 2>/dev/null; true"
  # cam2 (painter): we stopped its camera-box to free /dev/fb0; restart it.
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
    "pkill -x frame-probe 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
  # Kill the dev1 ffmpeg grab listener if still up.
  [ -n "$FFMPEG_PID" ] && kill "$FFMPEG_PID" 2>/dev/null
  pkill -f "listen=1.*${GRAB_PORT}" 2>/dev/null
}
trap cleanup EXIT HUP INT TERM

echo "[1/8] build frame-probe + recording-verdict + camera-box"
cargo build --release --features probe --bin frame-probe --bin recording-verdict  # airuleset:build-ok
cargo build --release --bin camera-box  # airuleset:build-ok

echo "[2/8] start dev1 ffmpeg grab listener → ${CAM1_MKV} (gray8 ${GRAB_W}x${GRAB_H}@${GRAB_FPS} → ffv1)"
# ffmpeg LISTENS; cam1's --record-grab connects and streams raw gray8. ffv1 is lossless
# so analyze_recording (rqrr) decodes the filmed QR exactly. -y overwrites a stale file.
ffmpeg -hide_banner -loglevel warning -y \
  -f rawvideo -pix_fmt gray -s "${GRAB_W}x${GRAB_H}" -r "$GRAB_FPS" \
  -i "tcp://0.0.0.0:${GRAB_PORT}?listen=1" \
  -c:v ffv1 -level 3 "$CAM1_MKV" >/tmp/ffmpeg-grab-${RUN_ID}.log 2>&1 &
FFMPEG_PID=$!
sleep 1  # let the listener bind before cam1 connects

echo "[3/8] cam1 (${CAM1_IP}) — manual camera-box with --record-grab (grabs cam2 monitor, records, emits NDI)"
# Stop the deployed service (single-open /dev/video0), launch a manual camera-box that
# grabs + emits NDI (genlock 30) AND tees the gray8 grab to dev1 + writes the sidecar.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   i=0; while fuser -s /dev/video0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
     nohup /usr/local/bin/camera-box \
       --record-grab tcp://${DEV1_IP}:${GRAB_PORT} \
       --record-grab-ts ${CAM1_GRAB_TS} \
       >/tmp/cbox-grab.log 2>&1 &)"
sleep 4  # let cam1's NDI sender become discoverable + the grab stream connect

echo "[4/8] cam2 (${PAINTER_IP}) — free /dev/fb0, paint dual-QR with --paint-log ground truth"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  target/release/frame-probe root@"$PAINTER_IP":/tmp/frame-probe
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (nohup /tmp/frame-probe --paint-only --dual-qr --wall-clock --paint-log /tmp/painter.csv \
      --paint-fps $PAINT_FPS --qr-size $QR_SIZE --run-id $RUN_ID --duration-secs $((DURATION+60)) \
      >/tmp/painter.log 2>&1 &)"
sleep 3  # let the painter put the QR on the monitor cam1 films

echo "[5/8] OBS setup — route strih program ← cam1 NDI, stream program ← strih NDI"
STRIH_OUT=$(python3 "$HERE/obs_phase2.py" setup --host "$STRIH"  --upstream "$CAMERA_SOURCE")
STREAM_OUT=$(python3 "$HERE/obs_phase2.py" setup --host "$STREAM" --upstream "$STRIH_OUT" --terminal)
echo "    strih program NDI='$STRIH_OUT'  stream program NDI='$STREAM_OUT'"
sleep 6  # let both OBS chains stabilise before recording

echo "[6/8] StartRecord on strih + stream (program = probe scene)"
python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action start
python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start

echo "[7/8] steady-state run: ${DURATION}s (run_id=$RUN_ID)"
sleep "$DURATION"

echo "[7/8] StopRecord + download the four artifacts to dev1"
STRIH_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop)
STREAM_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop)
echo "    strih host file:  $STRIH_HOST_PATH"
echo "    stream host file: $STREAM_HOST_PATH"
# Stop the painter + cam1 grab so the files finalise.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" "pkill -x frame-probe 2>/dev/null; true"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" "pkill -x camera-box 2>/dev/null; true"
sleep 2  # let cam1 flush the grab stream + ffmpeg finalise the mkv
[ -n "$FFMPEG_PID" ] && { wait "$FFMPEG_PID" 2>/dev/null || true; FFMPEG_PID=""; }

# Download cam1's grab-ts sidecar (cam1 is Linux — scp works).
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$CAM1_IP":"$CAM1_GRAB_TS" "$CAM1_GRAB_TS" 2>/dev/null || \
  echo "WARNING: could not fetch cam1 grab-ts sidecar (cam2→cam1 latency will be omitted)" >&2
# Download the cam2 painter ground-truth CSV.
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$PAINTER_IP":/tmp/painter.csv "$PAINTER_CSV" 2>/dev/null || \
  echo "WARNING: could not fetch painter CSV (cam→strih/cam2→cam1 assessment omitted)" >&2
# Download the OBS recordings from the Windows boxes via the win-* MCP / http.server.
# scp to Windows is DENIED on this rig; the harness expects the caller (the autopilot
# worker or operator) to pull STRIH_HOST_PATH / STREAM_HOST_PATH via the win-* MCP and
# place them at $STRIH_REC / $STREAM_REC. If they are already present, proceed.
"$HERE/recording-fetch-windows.sh" \
  "$STRIH"  "$STRIH_HOST_PATH"  "$STRIH_REC" \
  "$STREAM" "$STREAM_HOST_PATH" "$STREAM_REC" || \
  echo "NOTE: recording-fetch-windows.sh not run/failed — place strih/stream recordings at $STRIH_REC / $STREAM_REC manually" >&2

echo "[8/8] recording-verdict over all four nodes + report"
VERDICT_ARGS=(--strih "$STRIH_REC" --min-secs 300 --cam2-run-id "$RUN_ID" \
  --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
[ -f "$STREAM_REC" ]    && VERDICT_ARGS+=(--stream "$STREAM_REC")
[ -f "$CAM1_MKV" ]      && VERDICT_ARGS+=(--cam1 "$CAM1_MKV")
[ -f "$CAM1_GRAB_TS" ]  && VERDICT_ARGS+=(--cam1-grab-ts "$CAM1_GRAB_TS")
[ -f "$PAINTER_CSV" ]   && VERDICT_ARGS+=(--painter "$PAINTER_CSV")

if ./target/release/recording-verdict "${VERDICT_ARGS[@]}"; then GATE=0; else GATE=$?; fi

echo "[8/8] render the 2-graph report PNG"
if [ -f "$REPORT_JSON" ]; then
  python3 "$HERE/recording-e2e-report.py" --json "$REPORT_JSON" --out "$REPORT_PNG" || \
    echo "WARNING: report render failed (non-fatal; JSON at $REPORT_JSON)" >&2
fi

echo "artifacts in $OUTDIR (verdict json: $REPORT_JSON, report: $REPORT_PNG)"
exit "$GATE"
