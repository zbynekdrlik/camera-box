#!/usr/bin/env bash
# Phase-1 NDI frame-loss/latency loopback E2E.
#
# Builds frame-probe on the dev machine, deploys it to a camera-box device (the
# off-air rig), runs the QR HDMI-loopback probe, pulls the JSON artifact, and
# exits non-zero on a failing verdict.
#
# On the device, camera-box normally runs with `--display`, which holds /dev/fb0.
# The probe's painter needs fb0, so this script stops the service and runs
# camera-box WITHOUT --display for the duration (keeps capture->NDI alive, frees
# fb0), then ALWAYS restores the service via a trap — even on failure.
#
# Env overrides: CAM_IP CAM_PASS SOURCE MODE DURATION_SECS QR_SIZE SETTLE_MS
#                CAPTURE_FPS MAX_P99_MS MAX_FREEZE_PERIODS LOCAL_OUT
set -euo pipefail

CAM_IP="${CAM_IP:-10.77.9.62}"
CAM_PASS="${CAM_PASS:-newlevel}"
SOURCE="${SOURCE:-CAM2 (usb)}"        # NDI name is "<machine> (<ndi_name>)"
MODE="${MODE:-coverage}"               # coverage (gate) | full-rate (stress)
DURATION_SECS="${DURATION_SECS:-300}"  # default 5 min evidence run
QR_SIZE="${QR_SIZE:-700}"
SETTLE_MS="${SETTLE_MS:-500}"          # must exceed observed max latency (1080p60 max ~290 ms)
# camera-box now captures + sends NDI at 1080p60 (#11). The probe's coverage
# oversample math needs the real capture rate; at 60 fps capture the default
# 12 fps coverage paint is ~5x oversampled (>= 2 samples/id required).
CAPTURE_FPS="${CAPTURE_FPS:-60}"
# Hard latency/freeze gates (spec §15). Defaults are RIG-SPECIFIC — derived from
# the cam2 1080p60 baseline (2026-06-08, 180s coverage: p99 280 ms, max 290 ms,
# zero loss; ShadowCast 2 USB capture + V4L2 4-buffer queue + NDI roundtrip
# dominate the single-box loopback path) with ~1.25x margin so normal jitter
# does not flake. The 60fps loopback latency is ~2x the old 30fps baseline
# (p99 157 ms) — a deeper but stable pipeline, NOT CPU starvation (box was 50%
# idle). RE-BASELINE when the capture dongle / cabling / device / fps changes.
MAX_P99_MS="${MAX_P99_MS:-350}"            # fail if p99 latency > this (1080p60)
MAX_FREEZE_PERIODS="${MAX_FREEZE_PERIODS:-6}"  # fail if a stall repeats > this many frames
LOCAL_OUT="${LOCAL_OUT:-./frame-probe-${MODE}.json}"
REMOTE_BIN="/tmp/frame-probe"
REMOTE_OUT="/tmp/frame-probe.json"

ssh_cam() { sshpass -p "$CAM_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@${CAM_IP}" "$@"; }

echo ">> Building frame-probe (release, --features probe)"
cargo build --release --features probe --bin frame-probe

echo ">> Deploying to ${CAM_IP}:${REMOTE_BIN}"
sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no target/release/frame-probe "root@${CAM_IP}:${REMOTE_BIN}"

echo ">> Running ${MODE} loopback for ${DURATION_SECS}s on ${CAM_IP} (source: '${SOURCE}')"
echo "   camera-box runs WITHOUT --display for the duration; service is restored afterwards."

# Remote orchestration. Quoted heredoc — values are passed via env on the ssh line.
set +e
ssh_cam "MODE='${MODE}' DURATION='${DURATION_SECS}' SOURCE='${SOURCE}' QR_SIZE='${QR_SIZE}' SETTLE_MS='${SETTLE_MS}' CAPTURE_FPS='${CAPTURE_FPS}' MAX_P99_MS='${MAX_P99_MS}' MAX_FREEZE_PERIODS='${MAX_FREEZE_PERIODS}' OUT='${REMOTE_OUT}' bash -s" <<'REMOTE'
set -uo pipefail
export NDI_RUNTIME_DIR_V6=/usr/lib/ndi
chmod +x /tmp/frame-probe
MANUAL_PID=""
cleanup() {
  if [ -n "$MANUAL_PID" ]; then kill "$MANUAL_PID" 2>/dev/null || true; fi
  sleep 1
  systemctl start camera-box
  echo ">> CLEANUP: camera-box service restarted ($(systemctl is-active camera-box))"
}
trap cleanup EXIT HUP INT TERM

echo ">> stop camera-box (release fb0 + video0)"
systemctl stop camera-box
sleep 2

echo ">> start camera-box WITHOUT --display (capture->NDI only)"
nohup /usr/local/bin/camera-box >/tmp/cam-manual.log 2>&1 &
MANUAL_PID=$!
sleep 7
if ! kill -0 "$MANUAL_PID" 2>/dev/null; then
  echo "ERROR: manual camera-box died:"; tail -20 /tmp/cam-manual.log; exit 3
fi

setterm --blank 0 --powerdown 0 >/dev/null 2>&1 || true

echo ">> running frame-probe"
/tmp/frame-probe --mode "$MODE" --source "$SOURCE" --duration-secs "$DURATION" \
  --qr-size "$QR_SIZE" --settle-ms "$SETTLE_MS" --capture-fps "$CAPTURE_FPS" \
  --max-p99-latency-ms "$MAX_P99_MS" --max-freeze-periods "$MAX_FREEZE_PERIODS" \
  --out "$OUT" --connect-timeout-secs 20
rc=$?
echo "PROBE_RC=$rc"
exit $rc
REMOTE
PROBE_RC=$?
set -e

echo ">> Pulling artifact to ${LOCAL_OUT}"
sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no "root@${CAM_IP}:${REMOTE_OUT}" "$LOCAL_OUT" || echo "!! could not pull artifact"

if [ -f "$LOCAL_OUT" ]; then
  echo ">> Result summary:"
  python3 -c "
import json,sys
d=json.load(open('$LOCAL_OUT'))
l=d.get('latency') or {}
print(f\"  mode={d['mode']} verdict={'PASS' if d['verdict_pass'] else 'FAIL'}\")
print(f\"  emitted={d['emitted_count']} observed={d['observed_count']} unique={d['unique_observed']}\")
print(f\"  missing={len(d['missing_ids'])} reorders={len(d['reorders'])} freezes={len(d['freezes'])}\")
c=d.get('coverage') or {}
if c: print(f\"  coverage: oversample_p50={c['oversample_p50']} (floor {c['min_confirm_samples']}, oversampled={c['run_oversampled']}) confirmed_drops={len(c['confirmed_drops'])} inconclusive_gaps={len(c['inconclusive_gaps'])} low_coverage={len(c['low_coverage_ids'])}\")
if c and c['confirmed_drops']: print(f\"    CONFIRMED DROP ids: {c['confirmed_drops'][:20]}\")
if c and c['inconclusive_gaps']: print(f\"    inconclusive (torn-prone) ids: {c['inconclusive_gaps'][:20]}\")
if l: print(f\"  latency_ms: mean={l['mean_ms']:.1f} p50={l['p50_ms']:.1f} p95={l['p95_ms']:.1f} p99={l['p99_ms']:.1f} max={l['max_ms']:.1f} (n={l['samples']})\")
" 2>/dev/null || cat "$LOCAL_OUT"
fi

if [ "$PROBE_RC" -ne 0 ]; then
  echo "!! FRAME-PROBE FAILED (rc=${PROBE_RC}) — see ${LOCAL_OUT}"
  exit "$PROBE_RC"
fi
echo ">> PASS"
