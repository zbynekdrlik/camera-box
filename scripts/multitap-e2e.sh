#!/usr/bin/env bash
# Phase 2 multi-tap NDI per-hop frame-loss/latency E2E (dev1-orchestrated).
#
# Topology: cam2 paints QR (frame-probe --paint-only) -> camera-box capture->NDI
# "CAM2 (usb)" -> OBS strih program (DistroAV "NDI Main Output", e.g. "2ME PGM")
# -> OBS stream program (its own DistroAV "NDI Main Output"). dev1 taps all three
# and differences adjacent pairs (cam->strih, strih->stream). The OBS program NDI
# names are DISCOVERED at setup (not hardcoded) and echoed by scripts/obs_phase2.py.
# strih + stream are off-air-freely during the run; their program scene is saved
# and restored by the trap.
#
# PREREQUISITE: DistroAV "NDI Main Output" must be ENABLED in OBS (Tools menu) on
# BOTH strih and stream so each re-emits its program as NDI. obs_phase2.py fails
# loudly if it is not enabled on a host.
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi (libndi.so.6), cargo, sshpass,
# python3 + websocket-client. OBS WebSocket :4455 reachable on strih and stream.
set -euo pipefail

CAM2=10.77.9.62
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
CAM_SOURCE="CAM2 (usb)"
RUN_ID=$(( (RANDOM << 16) | RANDOM ))
DURATION="${DURATION:-300}"
OUT="${OUT:-/tmp/multitap-probe.json}"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

# shellcheck disable=SC2317  # cleanup() is invoked indirectly via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  echo "[cleanup] restoring OBS program scenes + cam2 service"
  python3 scripts/obs_phase2.py teardown --host "$STREAM"
  python3 scripts/obs_phase2.py teardown --host "$STRIH"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
    "pkill -f 'frame-probe' 2>/dev/null; pkill -f '/usr/local/bin/camera-box' 2>/dev/null; \
     systemctl restart camera-box 2>/dev/null; true"
}
trap cleanup EXIT HUP INT TERM

echo "[1/5] build frame-probe + multitap-probe"
cargo build --release --features probe --bin frame-probe --bin multitap-probe

echo "[2/5] bring up the cam2 NDI sender FIRST (run_id=$RUN_ID), then the painter"
# #30 ordering fix: camera-box's "CAM2 (usb)" NDI sender must EXIST before OBS
# binds its ndi_source to it. Previously OBS setup ran first and bound to the
# sender that this step then RESTARTED — DistroAV intermittently failed to
# reconnect to the new sender, so strih/stream rendered black and every tap
# downstream of OBS decoded 0 for the whole run (false-RED on min_frames).
# Starting (restarting) camera-box up front means OBS binds, in step [3], to the
# live sender that then persists for the entire tap window — no mid-run teardown,
# no reconnect, no race.
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  target/release/frame-probe root@"$CAM2":/tmp/frame-probe
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
  "mount -o remount,rw / 2>/dev/null; systemctl stop camera-box; sleep 1; \
   (NDI_RUNTIME_DIR_V6=/usr/lib/ndi nohup /usr/local/bin/camera-box >/tmp/cbox.log 2>&1 &); \
   sleep 4; \
   (nohup /tmp/frame-probe --paint-only --run-id $RUN_ID --duration-secs $((DURATION+40)) \
      >/tmp/painter.log 2>&1 &)"
# NOTE: camera-box is started WITHOUT --display so /dev/fb0 is free for the
# painter; it still runs capture->NDI, carrying the QR frames onto the network.
# Painter outlasts the tap window by +40s because OBS setup ([3]) now runs AFTER
# the painter starts and consumes a few seconds before the taps begin.
sleep 3  # let the fresh "CAM2 (usb)" NDI sender become discoverable on the LAN

echo "[3/5] OBS setup — route the chain to the LIVE sender, discover program NDI names"
# strih ingests the camera's live QR NDI; STRIH_OUT = strih program NDI name.
STRIH_OUT=$(python3 scripts/obs_phase2.py setup --host "$STRIH" --upstream "$CAM_SOURCE")
# stream ingests strih's program NDI; STREAM_OUT = stream program NDI name.
STREAM_OUT=$(python3 scripts/obs_phase2.py setup --host "$STREAM" --upstream "$STRIH_OUT")
echo "    tap names: cam='$CAM_SOURCE'  strih='$STRIH_OUT'  stream='$STREAM_OUT'"

echo "[4/5] dev1 taps (run_id=$RUN_ID, ${DURATION}s)"
sleep 6  # let OBS NDI outputs become discoverable + the chain stabilise

# Per-hop latency/freeze gate bounds — report-only unless set (#23). Keyed by the
# DOWNSTREAM tap of each hop: 'strih' = cam→strih, 'stream' = strih→stream.
# Baseline first with a report-only run, then ratchet. Rig-specific p99 baselines
# (spec 11.3): cam→strih ~109 ms, strih→stream ~190 ms. Leave unset to disable.
GATE_ARGS=()
[ -n "${MAX_P99_STRIH:-}" ]    && GATE_ARGS+=(--max-p99-latency-ms "strih=$MAX_P99_STRIH")
[ -n "${MAX_P99_STREAM:-}" ]   && GATE_ARGS+=(--max-p99-latency-ms "stream=$MAX_P99_STREAM")
[ -n "${MAX_FREEZE_STRIH:-}" ]  && GATE_ARGS+=(--max-freeze-periods "strih=$MAX_FREEZE_STRIH")
[ -n "${MAX_FREEZE_STREAM:-}" ] && GATE_ARGS+=(--max-freeze-periods "stream=$MAX_FREEZE_STREAM")

# Per-hop DOCUMENTED-BOUND loss gate (#21). Both OBS hops drop frames because
# camera-box, strih OBS and stream OBS each run a free-running 30 fps clock with
# no genlock — the compositor samples its NDI source on its own render tick and
# drops ~1-4.5% of source frames (measured; see docs/phase2/strih-stream-baseline.md).
# This is irreducible until cluster genlock/clock-truth lands (#8 / #7 / #11), so
# the gate accepts the quantified single-copy (oversample-independent) per-frame
# loss up to 10% — ~2x the observed ~4.6% ceiling, far below the #14 catastrophe
# (~39%) — and FAILS on any regression past it. Override per-hop to tighten as the
# clock work progresses. Set to empty to fall back to strict zero-loss.
MAX_LOSS_STRIH="${MAX_LOSS_STRIH-10}"
MAX_LOSS_STREAM="${MAX_LOSS_STREAM-10}"
[ -n "$MAX_LOSS_STRIH" ]  && GATE_ARGS+=(--max-loss-pct "strih=$MAX_LOSS_STRIH")
[ -n "$MAX_LOSS_STREAM" ] && GATE_ARGS+=(--max-loss-pct "stream=$MAX_LOSS_STREAM")

# Per-hop oversample-masking guard (#29), keyed by the DOWNSTREAM tap of each hop.
# A hop is only CERTIFIED (PASS) once it carried at least this many single-copy
# (oversample-independent) frames; below it the verdict is INCONCL (exit non-zero),
# not a false-green PASS. The sub-fps QR painter oversamples each id, so a unique
# id is only "dropped" when ALL its copies are lost and a lucky run can show zero
# loss while frames really drop; single-copy ids expose the true per-frame drop.
#
# Measured single-copy yield is HOP-DEPENDENT and, on the second hop, NOT
# duration-stable:
#   cam→strih (key 'strih'):  48 / 68 / 49 / 60  over 120-300 s  — reliably ~50-68
#   strih→stream (key 'stream'): 12 / 63 / 17 / 2 — starved, often too few (2 at 300 s!)
# So guard cam→strih at 20 (always met → catches a degenerate <20 run) and leave
# strih→stream UNGATED until the full-fps painter (#32) gives it a stable supply —
# gating it now would INCONCL healthy runs. Override per hop to tighten as #32
# lands. Set a value to empty to disable that hop's guard.
MIN_SC_STRIH="${MIN_SC_STRIH-20}"
MIN_SC_STREAM="${MIN_SC_STREAM-}"   # ungated until #32 (full-fps painter)
[ -n "$MIN_SC_STRIH" ]  && GATE_ARGS+=(--min-single-copy "strih=$MIN_SC_STRIH")
[ -n "$MIN_SC_STREAM" ] && GATE_ARGS+=(--min-single-copy "stream=$MIN_SC_STREAM")

# Optional raw per-frame dump for drop/oversample root-cause analysis (#21).
[ -n "${DUMP_RAW:-}" ] && GATE_ARGS+=(--dump-raw "$DUMP_RAW")

# A failing per-hop gate is multitap-probe exiting 1 — its designed FAIL signal.
# Capture it without `set -e` aborting before the artifact dump (the failure case
# is exactly when we want the JSON shown), then propagate the code as the exit.
if ./target/release/multitap-probe \
  --run-id "$RUN_ID" \
  --tap cam="$CAM_SOURCE" \
  --tap strih="$STRIH_OUT" \
  --tap stream="$STREAM_OUT" \
  --duration-secs "$DURATION" \
  ${GATE_ARGS[@]+"${GATE_ARGS[@]}"} \
  --out "$OUT"; then
  GATE=0
else
  GATE=$?
fi

echo "[5/5] artifact: $OUT"
cat "$OUT"
exit "$GATE"
