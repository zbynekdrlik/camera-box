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

# Camera selection (#24): set CAM=cam1|cam2|cam3|cam4 to drive the full path from a chosen
# source camera. Its IP + NDI source are resolved from scripts/camera-set.sh (the single
# source of truth), not hard-coded — defaults to cam2 for back-compat. CAM_IP / CAM_SOURCE
# still override the resolved values.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
camera_resolve "${CAM:-cam2}"

# CAM_IP is the device IP of the selected source camera (was the hard-coded `CAM2` var).
CAM_IP="${CAM_IP:-$CAMERA_IP}"
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
CAM_SOURCE="${CAM_SOURCE:-$CAMERA_SOURCE}"
RUN_ID=$(( (RANDOM << 16) | RANDOM ))
DURATION="${DURATION:-300}"
OUT="${OUT:-/tmp/multitap-probe.json}"
# #32: paint at the pipeline rate (the OBS clocks run ~30 fps), NOT the 12 fps
# coverage default. At 30 fps each painted id is carried ~once per hop (oversample
# ~1), so single-copy (oversample-independent) frames are abundant on EVERY hop —
# measured ~1200 (cam→strih) / ~1780 (strih→stream) per 60 s, vs 2–63 at 12 fps —
# which is what lets the #29 guard certify strih→stream. qr_size stays 700 (the
# default): the 30 fps NDI taps decode it 100% (decode_failed=0), so no shrink is
# needed; smaller would only risk robustness across the NDI/OBS compression hops.
PAINT_FPS="${PAINT_FPS:-30}"
# #7 ABSOLUTE end-to-end latency: stamp gen_ts (painter, on the camera) and
# recv_ts (taps, on dev1) on the DanteSync-disciplined CLOCK_REALTIME (strih =
# master) so the source→endpoint latency recv(endpoint) − gen(source) is a true
# absolute number, not a per-hop relative delta. ON by default — the cluster is
# clock-synced (#8 CLOSED; scripts/clock-offset-guard.sh verifies the offset
# stays within ±2 ms). Set WALL_CLOCK=0 to fall back to Phase-2 relative-latency
# only. Per-hop relative latency is correct either way (both taps one domain).
WALL_CLOCK="${WALL_CLOCK:-1}"
# Resolved on dev1 to a literal flag, then interpolated into the (dev1-expanded)
# remote painter command and the local tap command — never evaluated remotely.
[ "$WALL_CLOCK" = "1" ] && PAINT_WALL_FLAG="--wall-clock" || PAINT_WALL_FLAG=""
# Optional hard gate (ms) on the absolute source→endpoint p99. Requires WALL_CLOCK
# (multitap-probe bails otherwise). Empty ⇒ absolute latency is report-only (still
# WRITTEN to the artifact). Baseline with a report-only run, then ratchet.
MAX_ABS_LATENCY="${MAX_ABS_LATENCY:-}"
# Catch the illegal combination up front, before the multi-minute remote painter
# launch — multitap-probe also bails, but only after the whole rig is brought up.
if [ -n "$MAX_ABS_LATENCY" ] && [ "$WALL_CLOCK" != "1" ]; then
  echo "ERROR: MAX_ABS_LATENCY requires WALL_CLOCK=1 (absolute latency needs the shared wall clock)" >&2
  exit 1
fi
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

# Pre-flight: when measuring absolute latency, the gen/recv stamps are only
# comparable if the cluster wall clocks are synced. Fail loudly up front (rather
# than emitting a meaningless absolute number) if the source camera has drifted.
if [ "$WALL_CLOCK" = "1" ]; then
  echo "[0/5] verify cluster clock sync for absolute latency (#7/#8): ${CAM:-cam2}"
  CLOCK_GUARD_TARGETS="${CAM:-cam2}=$CAM_IP" "$HERE/clock-offset-guard.sh" --bound-us "${CLOCK_GUARD_BOUND_US:-2000}"
fi

# shellcheck disable=SC2317  # cleanup() is invoked indirectly via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  echo "[cleanup] restoring OBS program scenes + ${CAMERA_NAME} service ($CAM_IP)"
  python3 scripts/obs_phase2.py teardown --host "$STREAM"
  python3 scripts/obs_phase2.py teardown --host "$STRIH"
  # pkill -x ONLY (exact process-name match). The old full-cmdline pkill form matched the
  # remote shell's OWN cmdline (it contains the pattern text), killed the shell, and the
  # restart below never ran — every run stranded a manual camera-box orphan on cam2 with
  # the service left stopped (which then broke the #9 loopback dispatch with EBUSY).
  # (sleep 1 only: if video0 is still settling, the unit's Restart=always/RestartSec=3
  # absorbs a transient first-start EBUSY — the safety lives in the unit file.)
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM_IP" \
    "pkill -x frame-probe 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
     systemctl restart camera-box 2>/dev/null; true"
}
trap cleanup EXIT HUP INT TERM

echo "[1/5] build frame-probe + multitap-probe"
cargo build --release --features probe --bin frame-probe --bin multitap-probe

echo "[2/5] bring up the ${CAMERA_NAME} NDI sender FIRST (run_id=$RUN_ID), then the painter"
# #30 ordering fix: camera-box's "CAM2 (usb)" NDI sender must EXIST before OBS
# binds its ndi_source to it. Previously OBS setup ran first and bound to the
# sender that this step then RESTARTED — DistroAV intermittently failed to
# reconnect to the new sender, so strih/stream rendered black and every tap
# downstream of OBS decoded 0 for the whole run (false-RED on min_frames).
# Starting (restarting) camera-box up front means OBS binds, in step [3], to the
# live sender that then persists for the entire tap window — no mid-run teardown,
# no reconnect, no race.
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  target/release/frame-probe root@"$CAM_IP":/tmp/frame-probe
# After the stop: sweep any orphaned manual camera-box (pkill -x — exact name, can't
# self-match the remote shell) and WAIT until /dev/video0 is actually free; uvcvideo
# teardown completes asynchronously after the process dies, and a fixed sleep races it
# into EBUSY on the manual start. (\$-escaped so the loop runs REMOTELY, not on dev1.)
# #66 GENLOCK env: the deployed camera-box service gets CAMERA_BOX_GENLOCK_FPS=30 from its
# systemd drop-in (#50); the manual launch here MUST carry the same env (GENLOCK_FPS, dev1-
# expanded to a literal, single source of truth in camera-set.sh) or the sender free-runs at
# the ~60fps capture rate (no decimation, no wall-clock external pacing) and strih's 30fps
# genlock FIFO drops ~half the frames / renders black — the ~49% cam→strih loss this harness
# falsely showed. With the env present, cam→strih is 0-loss from t+0s (no settling transient).
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM_IP" \
  "mount -o remount,rw / 2>/dev/null; systemctl stop camera-box; \
   pkill -x camera-box 2>/dev/null; \
   i=0; while fuser -s /dev/video0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS NDI_RUNTIME_DIR_V6=/usr/lib/ndi nohup /usr/local/bin/camera-box >/tmp/cbox.log 2>&1 &); \
   sleep 4; \
   (nohup /tmp/frame-probe --paint-only $PAINT_WALL_FLAG \
      --paint-fps $PAINT_FPS --run-id $RUN_ID --duration-secs $((DURATION+40)) \
      >/tmp/painter.log 2>&1 &)"
# NOTE: camera-box is started WITHOUT --display so /dev/fb0 is free for the
# painter; it still runs capture->NDI (genlock-decimated to $GENLOCK_FPS), carrying the QR
# frames onto the network.
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

# Per-hop loss gate — STRICT zero-loss by DEFAULT (#35). The canonical run fails on
# ANY dropped frame at ANY hop, because that is the exact defect the harness exists to
# expose: both OBS hops drop ~0.5-4.5% of source frames (camera-box, strih OBS and
# stream OBS each run a free-running 30 fps clock with no genlock — the compositor
# samples its NDI source on its own render tick; measured, see
# docs/phase2/strih-stream-baseline.md). A green test while frames drop is a lie, so the
# default passes NO --max-loss-pct → multitap-probe uses strict any-drop-fails and the
# gate stays RED until the pipeline is genuinely loss-free (the forcing function for the
# reason-fix #8 / #7 / #11).
#
# The #21 "documented bound" (≤N% per hop) is NOT gone — it is now an explicit OPT-IN
# ONLY, for tracking progress as the clock work lands: e.g. `MAX_LOSS_STRIH=10
# MAX_LOSS_STREAM=10 ./multitap-e2e.sh` re-enables the bounded gate per hop. Empty
# default = strict.
MAX_LOSS_STRIH="${MAX_LOSS_STRIH-}"
MAX_LOSS_STREAM="${MAX_LOSS_STREAM-}"
[ -n "$MAX_LOSS_STRIH" ]  && GATE_ARGS+=(--max-loss-pct "strih=$MAX_LOSS_STRIH")
[ -n "$MAX_LOSS_STREAM" ] && GATE_ARGS+=(--max-loss-pct "stream=$MAX_LOSS_STREAM")

# Per-hop oversample-masking guard (#29), keyed by the DOWNSTREAM tap of each hop.
# A hop is only CERTIFIED (PASS) once it carried at least this many single-copy
# (oversample-independent) frames; below it the verdict is INCONCL (exit non-zero),
# not a false-green PASS. A unique id is only "dropped" when ALL its copies are lost,
# so an oversampled run can show zero loss while frames really drop; single-copy ids
# (multiplicity exactly 1) expose the true per-frame drop.
#
# #32 RESOLVED: full-rate painting (PAINT_FPS=30, above) drives oversample→~1, so
# single-copy is now ABUNDANT on BOTH hops — measured on the live rig (60 s taps):
#   cam→strih (key 'strih'):     ~1200 single-copy  (was 48–68 at 12 fps)
#   strih→stream (key 'stream'): ~1780 single-copy  (was 2–63, often starved)
# So BOTH hops are gated. The floor 100 is the statistical evidence needed to certify
# ~<5% per-frame loss at ~95% confidence; it is met many times over at 30 fps (≥1200
# per 60 s) yet still trips a degenerate/starved run (e.g. a black-render hop). Override
# per hop to tighten. Set a value to empty to disable that hop's guard.
MIN_SC_STRIH="${MIN_SC_STRIH-100}"
MIN_SC_STREAM="${MIN_SC_STREAM-100}"
[ -n "$MIN_SC_STRIH" ]  && GATE_ARGS+=(--min-single-copy "strih=$MIN_SC_STRIH")
[ -n "$MIN_SC_STREAM" ] && GATE_ARGS+=(--min-single-copy "stream=$MIN_SC_STREAM")

# #68 Task C — leading-discard window (seconds). The harness re-points the OBS
# receiver right before a run (SetInputSettings), which transiently primes the
# genlock FIFO so the first seconds look cleaner than steady state and can MASK
# the ~1/12s loss the persistence test sees. Discard the first LEAD_DISCARD
# seconds so only the steady-state window feeds the loss/contiguity check. Default
# 0 = no leading trim (prior behaviour); set e.g. LEAD_DISCARD=60 for a
# reset-confound-free steady-state zero-loss run.
LEAD_DISCARD="${LEAD_DISCARD:-0}"
[ "$LEAD_DISCARD" -gt 0 ] && GATE_ARGS+=(--lead-discard-secs "$LEAD_DISCARD")

# #7 absolute end-to-end latency: tap on the wall clock (matches the painter) so
# the source→endpoint absolute latency is sound, and optionally gate its p99.
[ "$WALL_CLOCK" = "1" ] && GATE_ARGS+=(--wall-clock)
[ -n "$MAX_ABS_LATENCY" ] && GATE_ARGS+=(--max-abs-latency-ms "$MAX_ABS_LATENCY")

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
