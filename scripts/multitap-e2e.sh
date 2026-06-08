#!/usr/bin/env bash
# Phase 2 multi-tap NDI per-hop frame-loss/latency E2E (dev1-orchestrated).
#
# Topology: cam2 paints QR (frame-probe --paint-only) -> camera-box capture->NDI
# "CAM2 (usb)" -> OBS strih ingests it, re-emits NDI "STRIH-PHASE2" -> OBS stream
# ingests that, re-emits NDI "STREAM-PHASE2". dev1 taps all three and differences
# adjacent pairs. strih + stream are off-air-freely during the run; their OBS
# scene/output state is saved and restored by the trap.
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi (libndi.so.6), cargo, sshpass.
# OBS setup is performed by scripts/obs_phase2.py (obs-websocket; see its header).
set -euo pipefail

CAM2=10.77.9.62
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
RUN_ID=$(( RANDOM << 16 | RANDOM ))
DURATION="${DURATION:-300}"
OUT="${OUT:-/tmp/multitap-probe.json}"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

# shellcheck disable=SC2317  # cleanup() is invoked indirectly via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  echo "[cleanup] restoring OBS state + cam2 service"
  python3 scripts/obs_phase2.py teardown --strih "$STRIH" --stream "$STREAM"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
    "pkill -f 'frame-probe' 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
}
trap cleanup EXIT HUP INT TERM

echo "[1/5] OBS setup (strih ingests CAM2, stream ingests STRIH-PHASE2)"
python3 scripts/obs_phase2.py setup --strih "$STRIH" --stream "$STREAM" \
  --cam-source "CAM2 (usb)" --strih-out STRIH-PHASE2 --stream-out STREAM-PHASE2

echo "[2/5] build frame-probe + multitap-probe"
cargo build --release --features probe --bin frame-probe --bin multitap-probe

echo "[3/5] start cam2 painter (run_id=$RUN_ID), camera-box keeps capture->NDI"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  target/release/frame-probe root@"$CAM2":/tmp/frame-probe
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
  "mount -o remount,rw / 2>/dev/null; systemctl stop camera-box; \
   (NDI_RUNTIME_DIR_V6=/usr/lib/ndi nohup /usr/local/bin/camera-box >/tmp/cbox.log 2>&1 &); \
   sleep 3; \
   (nohup /tmp/frame-probe --paint-only --run-id $RUN_ID --duration-secs $((DURATION+15)) \
      >/tmp/painter.log 2>&1 &)"
# NOTE: camera-box is started WITHOUT --display so /dev/fb0 is free for the
# painter; it still runs capture->NDI, carrying the QR frames onto the network.

echo "[4/5] dev1 taps (run_id=$RUN_ID, ${DURATION}s)"
sleep 5  # let OBS NDI outputs become discoverable
./target/release/multitap-probe \
  --run-id "$RUN_ID" \
  --tap cam="CAM2 (usb)" \
  --tap strih=STRIH-PHASE2 \
  --tap stream=STREAM-PHASE2 \
  --duration-secs "$DURATION" \
  --out "$OUT"
GATE=$?

echo "[5/5] artifact: $OUT"
cat "$OUT"
exit $GATE
