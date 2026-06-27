#!/usr/bin/env bash
# Recording-based full-path E2E (#105 / #7 / #179), dev1-orchestrated — TRUE STREAM-ONLY.
#
# The loss verdict + per-hop latency come ONLY from the strih/stream OBS PROGRAM
# recordings and the cam2 painter ground truth — NEVER an NDI tap (the live NDI-tap harness
# produced false sampling artifacts and was removed, #210) AND, since #179, NEVER the
# 7.3GB cam1 grab. The cam1-capture render-time burn (#174) puts cam1's id + CAPTURE
# wall-clock ts INTO the emitted NDI frame, which rides through strih → stream, so the
# SINGLE stream recording already carries cam1's mark — decoding a separate multi-GB cam1
# grab is REDUNDANT and was the repeated ~15-40 min decode sink that stalled every proof
# run (it also crashed the full 4-node run, #187). The grab is GONE; the verdict runs
# stream-only in minutes (per the e2e-zero-loss memory + the #179 user directive).
#
# THE EVIDENCE NODES (per-frame id + timestamp, dual-QR tick=max), all in ONE stream rec:
#   1. cam2  — QR GENERATED on its monitor: painter `tick,gen_ts_ns` CSV
#              (frame-probe --paint-only --dual-qr --paint-log). cam2's paint ts also rides
#              into the stream recording INSIDE its own QR (used for cam2→cam1, #179).
#   2. cam1  — render-time CAPTURE BURN (#174): camera-box (CAMERA_BOX_BURN_RUN_ID set)
#              burns cam1's run_id + per-emit frame_id + CAPTURE wall-clock ts into the
#              emitted YUYV frame; it rides through NDI into strih's then stream's program.
#              NO grab is recorded or downloaded any more (#179).
#   3. strih — OBS PROGRAM recording (obs-ws StartRecord/StopRecord) .mkv.
#   4. stream— OBS PROGRAM recording .mp4 — carries cam2 optical QR + cam1 + strih + stream
#              burns, so the WHOLE per-hop analysis comes from it alone.
#
# recording-verdict consumes strih + stream (+ painter) and reports, per hop, loss+latency
# from the stream recording ALONE via the clean digital burn-id pairing (#174/#181):
#   cam2→cam1 (optical-injection): REAL latency, cam1 burn's capture-ts vs the co-located
#               cam2 QR's paint-ts in the SAME stream frame, matched per frame (#179 — no grab).
#   cam1→strih: per-hop loss + latency (clean burn-id, no 60→30 beat ambiguity).
#   strih→stream: per-hop loss + latency.
#   PASS = 0 undecodable AND 0 net loss on the strict hops AND span ≥ 300 s.
#
# TEST RIG: this routes the strih + stream OBS program to the CERTIFIED PRODUCTION scenes
# (strih 'Cam 5' = cam1 via the genlock 'NDI cam5' input; stream a full-screen scene over
# the prod 'NDI 2ME PGM' = strih's feed) and RECORDS that program for the run — NEVER a
# probe ndi_source (which collides with the always-on prod input on the same NDI
# source-name and records black, #163). The teardown trap restores both program scenes +
# the cam1/cam2 camera-box services on exit (incl. cancel). The operator is the guard
# (project decision: no automated streaming guard).
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi, cargo, sshpass, python3 +
# websocket-client, matplotlib (for the report). OBS WebSocket :4455 on strih+stream,
# DistroAV "NDI Main Output" enabled on both. cam1/cam2 SSH (root, pw newlevel).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
camera_resolve "${CAM:-cam1}"

CAM1_IP="${CAM1_IP:-10.77.9.61}"      # the SOURCE camera (films cam2's monitor, emits NDI w/ #174 burn)
PAINTER_IP="${PAINTER_IP:-10.77.9.62}" # cam2 — the box with the physical monitor cam1 films
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
RUN_ID="${RUN_ID:-$(( (RANDOM << 16) | RANDOM ))}"
DURATION="${DURATION:-1800}"
if [ "$DURATION" -lt 300 ]; then
  echo "ERROR: DURATION=${DURATION} below the 300 s zero-loss floor (default 1800)." >&2
  exit 1
fi
QR_SIZE="${QR_SIZE:-700}"
# #11 60fps rollout: the chain runs at 60fps end-to-end (cam1 emits 60, both OBS canvas/output
# at 60). PAINT_FPS is moot under KMS (the painter is vblank-paced at the monitor's 60 Hz, one
# dual-QR id per flip, --paint-fps ignored), but defaulting it to 60 keeps the non-KMS fallback
# correct. GENLOCK_FPS is cam1's CAMERA_BOX_GENLOCK_FPS — the NDI emit rate the genlock gate
# wall-paces the 60 fps capture onto (60 = 1:1 pass-through onto the 60 fps wall boundaries).
PAINT_FPS="${PAINT_FPS:-60}"
GENLOCK_FPS="${GENLOCK_FPS:-60}"
# #11 mixed 60/30 topology: the recorded OBS program fps is now PER BOX — the strih recording is
# 60fps (strih renders the 60fps LED-wall IMAG program) and the stream recording is 30fps (the
# stream box decimates 60→30 for the restreamer). Each feeds ITS recording's DIAGNOSTIC span
# (analyzed_secs = frames / capture_fps) and optical expected-step (refresh_hz / capture_fps), so
# the strih extract gets 60 and the stream extract gets 30 — a single shared 60 would mis-report
# the stream recording's span + optical beat (the #1c shadow). The decimation LOSS step (the 60fps
# strih burn read from the 30fps stream recording steps by 2) is a RIG-PINNED topology constant,
# threaded as --strih-emit-fps / --stream-capture-fps below and DECOUPLED from these diagnostic
# rates so it is always correct regardless of which recording's --capture-fps is in effect.
STRIH_CAPTURE_FPS="${STRIH_CAPTURE_FPS:-60}"
STREAM_CAPTURE_FPS="${STREAM_CAPTURE_FPS:-30}"
# #174 cam1-capture render-time burn run_id (the value CAMERA_BOX_BURN_RUN_ID is set to on
# cam1). Mirrors the verdict's BURN_RUN_ID_CAM1 default (911001). Distinct from the strih
# (911002) / stream (911004) burn ids so all four marks are told apart by run_id. This burn
# IS the cam1 mark in the stream recording — the reason #179 can drop the cam1 grab.
BURN_CAM1_RUN_ID="${BURN_CAM1_RUN_ID:-911001}"
OUTDIR="${OUTDIR:-/tmp/recording-e2e-${RUN_ID}}"
mkdir -p "$OUTDIR"
PAINTER_CSV="$OUTDIR/painter-${RUN_ID}.csv"
STRIH_REC="$OUTDIR/strih-${RUN_ID}.mkv"
STREAM_REC="$OUTDIR/stream-${RUN_ID}.mp4"
REPORT_JSON="$OUTDIR/verdict-${RUN_ID}.json"
REPORT_PNG="$OUTDIR/report-${RUN_ID}.png"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

# #220: CAMERA PRE-RUN CHECKLIST. The cam2->cam1 OPTICAL injection leg (cam1 broadcast camera
# filming the cam2 monitor QR) depends on the cam1 camera's MANUAL settings, which the harness
# CANNOT read or set: camera-box reads /dev/video0 (the ShadowCast capture card), which does NOT
# expose the BMPCC's shutter/focus/exposure. A 1/60 shutter integrates a full 60Hz monitor refresh
# and SMEARS the dual-QR Vernier mid-change -> the optical read drops (the #216 ~175s gap; the
# DIGITAL burns were unaffected, so the chain stayed 0 real loss — purely the optical-INJECTION leg).
# Satisfy this BEFORE the run, then the cam2->cam1 read is reliable with no spurious optical gap.
echo "=================================================================================="
echo " CAMERA PRE-RUN CHECKLIST (cam1 broadcast camera — the harness CANNOT auto-set these)"
echo "   [ ] SHUTTER FAST: >= 1/500 s (ideally 1/1000) — freezes the 60Hz monitor QR, no smear"
echo "   [ ] FOCUS: MANUAL, locked on the cam2 monitor (no autofocus hunting)"
echo "   [ ] EXPOSURE: FIXED / manual gain (no auto-exposure drift)"
echo " A 1/60 shutter caused the #216 ~175s optical-read gap. Fix the camera, THEN run."
echo "=================================================================================="

echo "[0/8] reachability preflight (cam1 source, cam2 painter, strih, stream)"
for hp in "cam1=$CAM1_IP" "cam2(painter)=$PAINTER_IP" "strih=$STRIH" "stream=$STREAM"; do
  _name="${hp%%=*}"; _ip="${hp#*=}"
  if ping -c1 -W2 "$_ip" >/dev/null 2>&1; then echo "    ok: $_name ($_ip)"; else
    echo "ERROR: $_name ($_ip) UNREACHABLE from dev1 — fix route/host, then re-run." >&2; exit 1; fi
done

# Disk preflight (#179): the 7.3GB cam1 grab is GONE — only the two downloaded OBS program
# recordings land on dev1 (~3 MB/s each, strih .mkv + stream .mp4). FAIL EARLY if $OUTDIR's
# filesystem cannot hold both (with headroom), so a long run never dies mid-flight on ENOSPC.
EST_MB=$(( DURATION * 3 ))             # one OBS recording estimate (MB)
NEED_MB=$(( EST_MB * 3 ))              # strih + stream + headroom
AVAIL_MB=$(df -Pm "$(dirname "$OUTDIR")" | awk 'NR==2{print $4}')
echo "    disk: need ~${NEED_MB} MB (strih + stream recordings, no grab), have ${AVAIL_MB} MB"
if [ "${AVAIL_MB:-0}" -lt "$NEED_MB" ]; then
  echo "ERROR: insufficient disk for a ${DURATION}s run (~${NEED_MB} MB needed, ${AVAIL_MB} MB free)." >&2
  echo "       Free space on $(dirname "$OUTDIR") or lower DURATION, then re-run." >&2
  exit 1
fi

# DanteSync NTP+PTP precondition gate (#7) — THE FIRST hard step. The whole test is
# worthless unless EVERY measured node (cam1, cam2, strih, stream) is BOTH NTP-synced
# AND PTP-locked (µs-grade fine servo, GM 10.77.9.184 up — NOT the ±1 ms NTP sawtooth
# fallback): cross-node per-hop latency and per-frame timestamp alignment are meaningless
# otherwise. The gate fails fast (non-zero, per-node diagnostic) and the run does NOT
# proceed to recording. The Linux cams are read over SSH; the Windows boxes (ssh denied)
# need their DanteSync status-pipe JSON pre-fetched to a file — fetched here over the same
# standing http.server the OBS recordings use, or supplied by the caller via
# DANTE_STRIH_STATUS / DANTE_STREAM_STATUS (the win-* MCP holder writes them).
echo "[0/8] DanteSync NTP+PTP gate — cam1, cam2, strih, stream must ALL be synced+locked (#7/#8)"
WIN_DANTE_PORT="${WIN_DANTE_PORT:-8898}"
DANTE_STRIH_STATUS="${DANTE_STRIH_STATUS:-$OUTDIR/dante-strih.json}"
DANTE_STREAM_STATUS="${DANTE_STREAM_STATUS:-$OUTDIR/dante-stream.json}"
# Try to fetch each Windows box's DanteSync status JSON over its http.server (a standing
# helper on the box dumps \\.\pipe\dantesync to a file the server exposes as /dantesync.json).
# A failure leaves the file absent -> the gate reports that node UNKNOWN and refuses to pass,
# unless the caller already placed a status file there via the win-* MCP.
fetch_dante_status() {
  local host="$1" dest="$2"
  [ -s "$dest" ] && { echo "    using pre-fetched DanteSync status: $dest"; return 0; }
  if curl -fsS --max-time 10 -o "$dest" "http://${host}:${WIN_DANTE_PORT}/dantesync.json" 2>/dev/null; then
    echo "    fetched DanteSync status from ${host}:${WIN_DANTE_PORT} -> $dest"
  else
    echo "    NOTE: could not fetch DanteSync status from ${host} (http :$WIN_DANTE_PORT) — the" >&2
    echo "          win-* MCP holder must write it to $dest, else the gate will fail this node." >&2
  fi
}
fetch_dante_status "$STRIH"  "$DANTE_STRIH_STATUS"  || true
fetch_dante_status "$STREAM" "$DANTE_STREAM_STATUS" || true
# ALWAYS pass --win-status for strih AND stream (NOT conditional on the file existing). If a
# fetch failed and the file is absent, the gate marks that node UNKNOWN and FAILS — never a
# silent pass with the Windows boxes unverified. Dropping the node here (the previous bug) let
# the gate certify only cam1+cam2 and exit 0 with strih/stream NTP/PTP never checked.
# #253: the explicit --bound-us arg below already carries the bound (and OVERRIDES the gate's own
# CLOCK_GUARD_BOUND_US default), so the leading CLOCK_GUARD_BOUND_US=... env-prefix was redundant
# AND shellcheck-flagged (SC2097/SC2098: the prefix is only seen by the forked process, while the
# same-line $CLOCK_GUARD_BOUND_US expansion is resolved by the CURRENT shell before the prefix
# takes effect). Pass the value purely as the argument — behavior is identical.
"$HERE/dantesync-gate.sh" \
  --bound-us "${CLOCK_GUARD_BOUND_US:-2000}" \
  --linux "cam1=$CAM1_IP cam2=$PAINTER_IP" \
  --win-status "strih=$DANTE_STRIH_STATUS" \
  --win-status "stream=$DANTE_STREAM_STATUS"

# Version-integrity precondition gate (#123) — THE OTHER hard step, alongside DanteSync. The whole
# test is worthless unless the LIVE strih+stream OBS stack is the PINNED build (a randomly-deployed /
# drifted / stock-OBS build silently produces a false result — that is #119). So before bringing up
# the rig, gather each Windows box's observed stack state and run drift-guard --compare against the
# pinned set (vendor/README.md); REFUSE (non-zero) on DRIFT (20) or UNKNOWN (11). Same Windows-box
# access pattern as the DanteSync gate above: ssh is denied, so each box's state JSON is fetched over
# its standing http.server (a helper exposes the read-only /drift-guard observed values as
# /bundle-state.json), falling back to a caller-pre-fetched file (the win-* MCP holder writes it).
# Optionally pass VERSION_GATE_MANIFEST=<BUNDLE_MANIFEST.json> to also assert the build SHAs.
echo "[0/8] version-integrity gate — live strih+stream stack MUST match the pinned set (#123/#119)"
WIN_BUNDLE_STATE_PORT="${WIN_BUNDLE_STATE_PORT:-8899}"
VERSION_STRIH_STATE="${VERSION_STRIH_STATE:-$OUTDIR/version-strih.json}"
VERSION_STREAM_STATE="${VERSION_STREAM_STATE:-$OUTDIR/version-stream.json}"
# Try to fetch each Windows box's stack-state JSON over its http.server; a failure leaves the file
# absent -> the gate reports that box UNKNOWN and refuses, unless the caller already placed a state
# file there via the win-* MCP. Mirrors fetch_dante_status() exactly.
fetch_box_state() {
  local host="$1" dest="$2"
  [ -s "$dest" ] && { echo "    using pre-fetched version-integrity state: $dest"; return 0; }
  if curl -fsS --max-time 10 -o "$dest" "http://${host}:${WIN_BUNDLE_STATE_PORT}/bundle-state.json" 2>/dev/null; then
    echo "    fetched version-integrity state from ${host}:${WIN_BUNDLE_STATE_PORT} -> $dest"
  else
    echo "    NOTE: could not fetch version-integrity state from ${host} (http :$WIN_BUNDLE_STATE_PORT) — the" >&2
    echo "          win-* MCP holder must write the drift-guard observed values to $dest, else the gate refuses." >&2
  fi
}
fetch_box_state "$STRIH"  "$VERSION_STRIH_STATE"  || true
fetch_box_state "$STREAM" "$VERSION_STREAM_STATE" || true
# ALWAYS pass --win-state for strih AND stream (NOT conditional on the file existing): an absent file
# is UNKNOWN -> the gate REFUSES, never a silent pass with a box's build unverified.
"$HERE/version-integrity-gate.sh" \
  ${VERSION_GATE_MANIFEST:+--manifest "$VERSION_GATE_MANIFEST"} \
  --win-state "strih=$VERSION_STRIH_STATE" \
  --win-state "stream=$VERSION_STREAM_STATE"

# cam1 v4l2 capture controls (#156 durable): apply the certified sharp controls
# (saturation=0, contrast=75) BEFORE the run so a soft-default device can never silently
# degrade the camera's optical dual-QR decode. The cam1 launch step ([2/8]) re-applies them
# at open too; this is the belt-and-braces preflight the harness owns regardless.
echo "[0/8] apply certified cam1 v4l2 capture controls (saturation=0, contrast=75) (#156)"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "v4l2-ctl -d /dev/video0 --set-ctrl=saturation=0,contrast=75 2>/dev/null; \
   v4l2-ctl -d /dev/video0 --get-ctrl=saturation,contrast 2>/dev/null" \
  || echo "WARNING: could not pre-apply cam1 v4l2 controls (the cam1 launch step re-applies them)" >&2

# shellcheck disable=SC2317  # cleanup() runs via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  echo "[cleanup] restore OBS program scenes + cam1/cam2 services"
  python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop >/dev/null 2>&1
  python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop >/dev/null 2>&1
  python3 "$HERE/obs_phase2.py" teardown --host "$STREAM"
  python3 "$HERE/obs_phase2.py" teardown --host "$STRIH"
  # cam1: stop the manual #174 burn binary (its own basename) AND any camera-box, remove
  # the deployed test binary, restore the clean deployed service.
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
    "pkill -f 'camera-box-burn-' 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
     rm -f /tmp/camera-box-burn-* 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
  # cam2 (painter): we stopped its camera-box to free /dev/fb0; restart it.
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
    "pkill -x frame-probe 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
  # Defense-in-depth (#166 review BUG 1): if the verdict's process group is still
  # running (e.g. the run is aborting for another reason), stop the whole group so a
  # multi-GB decode is never orphaned. The monitor already group-kills on STALL; this
  # covers the other exit paths.
  [ -n "${VERDICT_PID:-}" ] && { kill -- -"$VERDICT_PID" 2>/dev/null; kill "$VERDICT_PID" 2>/dev/null; }
  pkill -x recording-verdict 2>/dev/null
  # #246/#257: clear + VERIFY OBS burns OFF on strih + stream after EVERY run (incl. failure/abort),
  # so a QR test-burn can never linger onto the live broadcast. #257: the burn is the per-source
  # `genlock_burn` bool, toggled over obs-websocket with NO relaunch — `remove` sets genlock_burn=false
  # on each box's program input (a no-op if already off), then `check` VERIFIES burn_on=false. No
  # Machine-scope env to clear any more (OBS_BURN_* is gone); drift-guard's #246 facet now asserts
  # "no source has genlock_burn=on" over WS. The rich live OBS dock is the separate #188.
  echo "[cleanup] #246/#257 clear + verify OBS burns OFF (genlock_burn=false) on strih + stream"
  for _hbs in "${BURN_TARGETS[@]}"; do  # #252: shared burn triples (defined before the trap)
    _bn="${_hbs%%=*}"; _brest="${_hbs#*=}"; _bip="${_brest%%=*}"; _bsrc="${_brest#*=}"
    python3 "$HERE/obs_burn_filter.py" remove --host "$_bip" --input "$_bsrc" 2>&1 \
      | sed "s/^/    [$_bn burn-clear] /" || true
    _vrf="$(python3 "$HERE/obs_burn_filter.py" check --host "$_bip" --input "$_bsrc" 2>&1 || true)"
    printf '%s\n' "$_vrf" | sed "s/^/    [$_bn burn-verify] /"
    # The block above PROMISES to VERIFY burns OFF; surface a LOUD warning if a burn is still on
    # (e.g. the remove SetInputSettings was swallowed by a transient WS hiccup) so a lingering
    # test-burn onto the live broadcast can't pass silently. (cleanup runs in the EXIT trap, so it
    # WARNS rather than exits non-zero; drift-guard --compare burn_env= is the fail-loud gate.)
    if printf '%s' "$_vrf" | grep -q 'burn_on=True'; then
      echo "    [$_bn burn-verify] WARNING #246: genlock_burn still ON after clear — re-clear via" >&2
      echo "        scripts/rig-mode.sh event (or obs_burn_filter.py remove) before any live broadcast." >&2
    fi
  done
}
# #246: define the prod scene/source names BEFORE the trap so cleanup()'s burn-clear loop (which
# references $STRIH_PROG_SOURCE / $STREAM_PROG_SOURCE) never hits a `set -u` unbound-variable on an
# early abort (failed prebuilt-probe check / cargo build / cam scp-ssh, or Ctrl-C) — the exact
# failure/abort window the burn-off guard must cover. Detailed rationale at the #183 block below.
STRIH_PROG_SCENE="${STRIH_PROG_SCENE:-Cam 5}"          # prod scene showing cam1 (NDI cam5)
STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-NDI cam5}"     # the prod input behind 'Cam 5' (#246 burn-off target)
STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-REC-STRIH-TMP}" # full-screen scene over NDI 2ME PGM
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}" # the prod input the scene shows
# #252: single source of truth for the host=ip=source burn triples. The #195 pre-record burn-ON
# gate and the #246 cleanup() burn-clear loop iterate the SAME set; keeping it in one array means a
# third box (or a triple-structure change) can never green-light a set the cleanup does not clear
# (the #246 linger-onto-live-broadcast hazard). Defined HERE — after the *_PROG_SOURCE vars and
# BEFORE the cleanup trap is armed — so cleanup()'s array expansion is never an unbound `set -u`
# var on an early abort (same ordering reason the *_PROG_SOURCE vars precede the trap).
BURN_TARGETS=("strih=$STRIH=$STRIH_PROG_SOURCE" "stream=$STREAM=$STREAM_PROG_SOURCE")
trap cleanup EXIT HUP INT TERM

# PROBE_BIN_DIR holds the three probe binaries the harness deploys/runs:
#   $PROBE_BIN_DIR/camera-box      — PROBE-featured appliance with the #174 cam1 burn
#   $PROBE_BIN_DIR/frame-probe     — cam2 dual-QR painter
#   $PROBE_BIN_DIR/recording-verdict — the #186/#198 burn-id contiguity verdict
# Default: a local Tier-0 release build into target/release (airuleset:build-ok).
# USE_PREBUILT_PROBE_DIR (#133): point at a directory holding the CI
# probe-tools-linux-amd64 artifact instead — NO dev1 cargo build (no-local-builds.md).
# In that artifact the PROBE camera-box is named `camera-box-probe` (so it can never be
# confused with the clean production camera-box-linux-amd64); the harness symlinks it to
# the `camera-box` name it deploys.
PROBE_BIN_DIR="${PROBE_BIN_DIR:-target/release}"
if [ -n "${USE_PREBUILT_PROBE_DIR:-}" ]; then
  PROBE_BIN_DIR="$USE_PREBUILT_PROBE_DIR"
  echo "[1/8] USE_PREBUILT_PROBE_DIR=$PROBE_BIN_DIR — using CI-built probe binaries, NO dev1 build (#133)"
  # Normalise the CI artifact's camera-box-probe → camera-box (the name the deploy uses).
  if [ ! -x "$PROBE_BIN_DIR/camera-box" ] && [ -f "$PROBE_BIN_DIR/camera-box-probe" ]; then
    cp "$PROBE_BIN_DIR/camera-box-probe" "$PROBE_BIN_DIR/camera-box"
  fi
  for b in camera-box frame-probe recording-verdict; do
    if [ ! -f "$PROBE_BIN_DIR/$b" ]; then
      echo "ERROR: prebuilt probe binary '$b' missing in $PROBE_BIN_DIR — download the CI" >&2
      echo "       probe-tools-linux-amd64 artifact into it, then re-run." >&2
      exit 1
    fi
    chmod +x "$PROBE_BIN_DIR/$b" 2>/dev/null || true
  done
else
  echo "[1/8] build frame-probe + recording-verdict + camera-box (probe-featured for the #174 cam1 burn)"
  # #174: build camera-box WITH --features probe so the cam1-capture render-time QR burn is
  # present (the production artifact stays probe-free / clean; only this TEST binary carries
  # the burn + qrcode dep). The burn is still gated at runtime by CAMERA_BOX_BURN_RUN_ID.
  cargo build --release --features probe --bin frame-probe --bin recording-verdict --bin camera-box  # airuleset:build-ok
fi

echo "[2/8] cam1 (${CAM1_IP}) — probe-featured camera-box with the #174 capture BURN (emits NDI w/ cam1 mark, NO grab #179)"
# #174 + #179: deploy the freshly-built PROBE-featured camera-box (carries the cam1-capture
# burn) to a cam1-LOCAL /tmp path and launch THAT — NOT the prod /usr/local/bin/camera-box
# (the clean production binary with no burn). The burn is runtime-gated by
# CAMERA_BOX_BURN_RUN_ID, so it draws the cam1 run_id + per-emit frame_id + CAPTURE
# wall-clock ts into the EMITTED frame, which rides through NDI → strih → stream. #179: the
# grab-record flags are GONE — the cam1 mark in the stream recording fully replaces the
# 7.3GB grab, so cam1 just emits NDI with the burn. Re-apply the #156 certified v4l2 controls
# (saturation=0/contrast=75) directly here (the grab path that used to self-apply is gone).
CAM1_BURN_BIN="/tmp/camera-box-burn-${RUN_ID}"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  "$PROBE_BIN_DIR"/camera-box root@"$CAM1_IP":"$CAM1_BURN_BIN"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   chmod +x $CAM1_BURN_BIN; \
   i=0; while fuser -s /dev/video0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   v4l2-ctl -d /dev/video0 --set-ctrl=saturation=0,contrast=75 2>/dev/null; \
   (CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS CAMERA_BOX_BURN_RUN_ID=$BURN_CAM1_RUN_ID \
     CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
     nohup $CAM1_BURN_BIN >/tmp/cbox-burn.log 2>&1 &)"
sleep 4  # let cam1's NDI sender (with the burn) become discoverable

echo "[3/8] cam2 (${PAINTER_IP}) — free /dev/fb0, paint dual-QR with --paint-log ground truth"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  "$PROBE_BIN_DIR"/frame-probe root@"$PAINTER_IP":/tmp/frame-probe
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (nohup /tmp/frame-probe --paint-only --dual-qr --wall-clock --paint-log /tmp/painter.csv \
      --paint-fps $PAINT_FPS --qr-size $QR_SIZE --run-id $RUN_ID --duration-secs $((DURATION+60)) \
      >/tmp/painter.log 2>&1 &)"
sleep 3  # let the painter put the QR on the monitor cam1 films

# #163: record the CERTIFIED PRODUCTION scene program on each box — NOT a probe
# ndi_source. The old probe path pointed `phase2-probe-src` at "CAM1 (usb)", the SAME
# NDI source-name the always-on prod input `NDI cam5` already holds; DistroAV allows ONE
# receiver per source-name, so the probe got no NDI and the probe scene recorded pure
# BLACK (every frame undecodable). Instead we route program to the EXISTING prod scenes:
#   strih  : 'Cam 5'  already shows cam1 via the genlock-certified `NDI cam5` input.
#   stream : a full-screen scene over the prod `NDI 2ME PGM` input (shows strih's feed).
# No second receiver, no source-name collision — proven NON-black on the live rig and by
# the prior 3-node run (~0.35% real strih→stream loss). prod-scene runs a fail-fast
# non-black self-check before returning so a black ingest never wastes a full run.
# (STRIH_PROG_SCENE/SOURCE + STREAM_PROG_SCENE/SOURCE are defined earlier, just before the cleanup
#  trap — #246, so the burn-off teardown survives an early abort. They are `${VAR:-default}` so any
#  caller override set in the environment still wins.)
# #183: the upstream NDI source-name of each box's recorded prod GENLOCK input — used to
# FORCE genlock_preload=1 on it for the test window (then restore prod on teardown), so the
# run measures the TRUE genlock hop (~33ms) not the prod audio-sync delay (preload≈31 ≈ 1s).
#   strih records 'NDI cam5' whose source-name is cam1's NDI name ("CAM1 (usb)").
#   stream records 'NDI 2ME PGM' whose source-name is strih's program NDI name ($STRIH_OUT).
STRIH_UPSTREAM_NDI="${STRIH_UPSTREAM_NDI:-CAM1 (usb)}"  # cam1's NDI name (NDI cam5 input src)
TEST_PRELOAD="${TEST_PRELOAD:-1}"                       # #183: force preload=1 for the test
echo "[4/8] OBS prod-scene routing — strih program='$STRIH_PROG_SCENE' (cam1 via NDI cam5),"
echo "      stream program='$STREAM_PROG_SCENE' (strih feed via '$STREAM_PROG_SOURCE')"
echo "      #183: forcing genlock_preload=$TEST_PRELOAD on both recorded prod inputs for the test"
STRIH_OUT=$(python3 "$HERE/obs_phase2.py" prod-scene --host "$STRIH" \
  --program-scene "$STRIH_PROG_SCENE" \
  --upstream "$STRIH_UPSTREAM_NDI" --test-preload "$TEST_PRELOAD")
# stream's upstream is strih's program NDI name (just printed above) — force preload=1 on the
# stream box's 'NDI 2ME PGM' input (the prod copy of 31 the issue calls out).
STREAM_OUT=$(python3 "$HERE/obs_phase2.py" prod-scene --host "$STREAM" \
  --program-scene "$STREAM_PROG_SCENE" --ensure-source "$STREAM_PROG_SOURCE" \
  --upstream "$STRIH_OUT" --test-preload "$TEST_PRELOAD")
echo "    strih program NDI='$STRIH_OUT'  stream program NDI='$STREAM_OUT'"
sleep 6  # let both OBS chains stabilise before recording

# #195/#257: PRE-RECORD BURN-ON GATE — burns MUST be ON before recording, else the run is wasted.
# #257 made the burn a per-source `genlock_burn` bool (no OBS_BURN_QR env, no relaunch): the strih
# (911002) + stream (911004) burns fire only when each box's program input has genlock_burn=true AND
# the renderer filter is attached. If genlock_burn is off (e.g. rig-mode event left it off, or this
# is a fresh OBS) the recordings carry NO strih/stream burn → strih→stream can't pair (a full
# 300s+decode run produces no measurable hop). obs_burn_filter.py check prints `burn_on=<bool>` (the
# authoritative tell: genlock_burn=true AND filter present). FAIL FAST when it is off — no more
# silently-wasted runs. (Same host=ip=source triples cleanup()'s burn-clear loop uses.)
echo "[4b/8] #195/#257 pre-record burn-ON gate — genlock_burn MUST be ON on strih + stream before recording"
for _hbs in "${BURN_TARGETS[@]}"; do  # #252: shared burn triples (same set cleanup() clears)
  _bn="${_hbs%%=*}"; _brest="${_hbs#*=}"; _bip="${_brest%%=*}"; _bsrc="${_brest#*=}"
  # First turn the burn ON over WebSocket (idempotent, no relaunch — #257). `|| true` so a non-zero
  # exit (e.g. OBS unreachable) does not set -e-abort before our own clear diagnostic on the check.
  python3 "$HERE/obs_burn_filter.py" add --host "$_bip" --input "$_bsrc" 2>&1 \
    | sed "s/^/    [$_bn burn-on] /" || true
  _chk="$(python3 "$HERE/obs_burn_filter.py" check --host "$_bip" --input "$_bsrc" 2>&1 || true)"
  echo "    [$_bn burn-check] $_chk"
  if ! printf '%s' "$_chk" | grep -q 'burn_on=True'; then
    echo "ERROR: $_bn burn is NOT on (genlock_burn=true) for the recorded input '$_bsrc' — the $_bn" >&2
    echo "       burn would be absent from the recording and the run would be wasted (#195/#257)." >&2
    echo "       Confirm $_bn OBS ($_bip) is up + is the genlock build, then re-run (or scripts/rig-mode.sh test)." >&2
    exit 1
  fi
  echo "    [$_bn burn-check] OK — burns ON (genlock_burn=true on '$_bsrc', runtime, no relaunch)"
done

echo "[5/8] StartRecord on strih + stream (program = certified prod scene)"
python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action start
python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start

echo "[6/8] steady-state run: ${DURATION}s (run_id=$RUN_ID)"
sleep "$DURATION"

echo "[7/8] StopRecord + download strih + stream recordings to dev1 (NO grab #179)"
# #178: the StopRecord→verdict region is RESILIENT. run 172046073 completed the recording
# + StopRecord, then a set -e abort (a non-zero $(StopRecord) capture / a transient ssh /
# an absent optional recording hitting a `[ -f ] && ...` guard) jumped straight to the
# cleanup EXIT trap and the verdict — the WHOLE POINT of the run — never ran. Disable
# abort-on-error for the orchestration here; each step is guarded explicitly, and set -e is
# re-enabled at the verdict run (which manages its own exit via verdict-monitor.sh → GATE).
set +e
# StopRecord can return non-zero (OBS-WS already stopped, a transient WS hiccup). Capture the
# host path best-effort; an empty path just means recording-fetch-windows.sh has nothing to
# pull and the local recording (if already placed) is used. NEVER abort the run here.
STRIH_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop) \
  || echo "WARNING: strih StopRecord returned non-zero (continuing; recording may already be stopped)" >&2
STREAM_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop) \
  || echo "WARNING: stream StopRecord returned non-zero (continuing; recording may already be stopped)" >&2
echo "    strih host file:  ${STRIH_HOST_PATH:-<unknown>}"
echo "    stream host file: ${STREAM_HOST_PATH:-<unknown>}"
# Stop the painter + the cam1 burn binary so the files finalise (no grab stream to flush).
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" "pkill -x frame-probe 2>/dev/null; true"
# cam1: send SIGINT (graceful) so camera-box's shutdown handler runs and writes the
# cam2→cam1 LOSS sidecar (CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt — cam1's V4L2
# capture-drop count). Give it a moment to flush, then SIGKILL any straggler.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "pkill -INT -f 'camera-box-burn-' 2>/dev/null; pkill -INT -x camera-box 2>/dev/null; \
   sleep 3; pkill -9 -f 'camera-box-burn-' 2>/dev/null; pkill -9 -x camera-box 2>/dev/null; true"

# Download the cam2 painter ground-truth CSV (tick,gen_ts_ns) for the honest cam→strih
# optical assessment. (cam2→cam1 latency no longer needs it — #179 reads cam2's paint-ts
# CO-LOCATED from the cam2 QR next to the cam1 burn IN the stream recording.)
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$PAINTER_IP":/tmp/painter.csv "$PAINTER_CSV" 2>/dev/null || \
  echo "WARNING: could not fetch painter CSV (cam→strih assessment omitted)" >&2
# Download cam1's V4L2 capture-drop sidecar (the cam2→cam1 LOSS — the camera leg). The
# verdict reports v4l2_dropped as cam2→cam1 loss (NOT a painter-tick compare). Best effort:
# absent ⇒ the verdict simply omits the cam2→cam1 loss line.
CAM1_CAPTURE_STATS="$OUTDIR/cam1-capture-stats.txt"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$CAM1_IP":/tmp/cam1-capture-stats.txt "$CAM1_CAPTURE_STATS" 2>/dev/null || \
  echo "WARNING: could not fetch cam1 capture-stats sidecar (cam2→cam1 loss omitted)" >&2
# #193: by DEFAULT decode ON stream.lan where the video lives — do NOT download the multi-GB
# recordings to slow dev1 (the root of the download + #187 OOM + disk drain). When
# VERDICT_ON_STREAM=1 (the default), the harness SKIPS the dev1 fetch entirely and the verdict
# runs on the box (see [8/8]). Set VERDICT_ON_STREAM=0 ONLY for the legacy decode-on-dev1 path
# (e.g. a box with no uploaded verdict.exe), which DOES download the recordings here.
VERDICT_ON_STREAM="${VERDICT_ON_STREAM:-1}"
if [ "$VERDICT_ON_STREAM" = "1" ]; then
  echo "    #193: VERDICT_ON_STREAM=1 — NOT downloading the multi-GB recordings to dev1; the"
  echo "          verdict runs ON stream.lan against the LOCAL recording (dev1 gets only JSON+PNGs)."
else
  # LEGACY decode-on-dev1: download the OBS recordings from the Windows boxes via the win-* MCP
  # / http.server. scp to Windows is DENIED on this rig; the harness expects the caller (the
  # autopilot worker or operator) to pull STRIH_HOST_PATH / STREAM_HOST_PATH via the win-* MCP
  # and place them at $STRIH_REC / $STREAM_REC. If they are already present, proceed.
  "$HERE/recording-fetch-windows.sh" \
    "$STRIH"  "$STRIH_HOST_PATH"  "$STRIH_REC" \
    "$STREAM" "$STREAM_HOST_PATH" "$STREAM_REC" || \
    echo "NOTE: recording-fetch-windows.sh not run/failed — place strih/stream recordings at $STRIH_REC / $STREAM_REC manually" >&2
fi

echo "[8/8] recording-verdict — TRUE STREAM-ONLY (strih + stream + painter, NO 7.3GB grab) + report"
# #111/#174 per-hop ABSOLUTE latency + loss: pass the node burn run_ids so the verdict
# decodes the burned render-time stamps (cam1 capture burn rides into stream; strih/stream
# burns from their DistroAV filters) and computes the full chain cam1→strih→stream loss +
# latency from the STREAM recording ALONE, plus cam2→cam1 CO-LOCATED from the cam1 burn vs
# the cam2 QR in the same stream frame (#179 — no grab, no painter-CSV pairing). They match
# the burn filters' defaults; when a burn is OFF the affected hop reports NO SAMPLES (never
# a wrong number). Override via BURN_*_RUN_ID.
# #179: the cam1-grab verdict inputs are GONE — the 7.3GB grab is never decoded.
BURN_STRIH_RUN_ID="${BURN_STRIH_RUN_ID:-911002}"
BURN_STREAM_RUN_ID="${BURN_STREAM_RUN_ID:-911004}"
# #11: --capture-fps = the strih recording's rate (the fused fallback reads the cam1 burn from the
# strih recording). The decimation step for the strih burn (read from the 30fps stream recording)
# is pinned via --strih-emit-fps / --stream-capture-fps, decoupled from the diagnostic --capture-fps.
VERDICT_ARGS=(--strih "$STRIH_REC" --min-secs 300 --capture-fps "$STRIH_CAPTURE_FPS" \
  --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" --cam2-run-id "$RUN_ID" \
  --burn-strih-run-id "$BURN_STRIH_RUN_ID" --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
  --burn-cam1-run-id "$BURN_CAM1_RUN_ID" \
  --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
# #178: use `if` blocks for the optional verdict inputs (NOT a `test && append` one-liner) —
# a FALSE file-test returns non-zero and would `set -e`-abort the script before the verdict;
# an `if` condition is exempt, so an absent optional recording degrades gracefully (the
# verdict simply omits that input).
if [ -f "$STREAM_REC" ]; then VERDICT_ARGS+=(--stream "$STREAM_REC"); fi
if [ -f "$PAINTER_CSV" ]; then VERDICT_ARGS+=(--painter "$PAINTER_CSV"); fi
if [ -f "$CAM1_CAPTURE_STATS" ]; then VERDICT_ARGS+=(--cam1-capture-stats "$CAM1_CAPTURE_STATS"); fi

# #208 PER-BOX DECODE-IN-PLACE (refines #193): by default decode EACH recording ON ITS OWN BOX —
# the strih recording ON the strih box, the stream recording ON the stream box — and merge the
# SMALL partial JSONs on dev1. A recording is NEVER copied box-to-box (nor to dev1); only the
# small partial JSONs (+ the painter CSV) move. The OLD #193 flow ran a SINGLE fused verdict on
# the stream box, which forced the ~700 MB strih .mkv to be copied strih→stream first — that copy
# is GONE. The harness EMITS the per-box plans (upload recording-verdict.exe → extract the partial
# on each box → pull back ONLY the small JSON); the agent/operator holding the win-* MCP executes
# them (scp/ssh to Windows is DENIED, so bash cannot run them itself), then runs the dev1 merge.
# Set VERDICT_ON_STREAM=0 for the LEGACY single-box decode-on-dev1 fallback (no box-decode .exe).
if [ "$VERDICT_ON_STREAM" = "1" ]; then
  set -e
  echo "    #208: emitting the PER-BOX decode-in-place plan (strih ON strih, stream ON stream — NOTHING copied)."
  # The recordings stay AS THEY LIVE ON THEIR OWN BOX (the win-* MCP holder substitutes each box's
  # local Windows path). Each box writes its small partial JSON into a box-local OUT_DIR that is
  # pulled back to dev1; the merge runs on dev1 from the two small JSONs (no recording on dev1).
  VERDICT_EXE_WIN="${VERDICT_EXE_WIN:-C:\\camera-box\\recording-verdict.exe}"
  OUT_DIR_WIN="${OUT_DIR_WIN:-C:\\camera-box\\verdict-out}"
  STREAM_REC_WIN="${STREAM_REC_WIN:-<the stream recording AS IT LIVES ON THE STREAM BOX>}"
  STRIH_REC_WIN="${STRIH_REC_WIN:-<the strih recording AS IT LIVES ON THE STRIH BOX>}"
  STRIH_PARTIAL_WIN="$OUT_DIR_WIN\\strih-partial-${RUN_ID}.json"
  STREAM_PARTIAL_WIN="$OUT_DIR_WIN\\stream-partial-${RUN_ID}.json"
  STRIH_PARTIAL="$OUTDIR/strih-partial-${RUN_ID}.json"   # pulled back to dev1
  STREAM_PARTIAL="$OUTDIR/stream-partial-${RUN_ID}.json"  # pulled back to dev1
  # #186/#208: each box's --extract-partial writes its flagged/undecodable-frame pixel proofs into
  # the SIBLING `<partial>-pixels` dir; pull each dir back BESIDE its partial on dev1 (the merge
  # derives the same `<partial>-pixels` path to locate the #186 "SEE the frame" proofs). Small —
  # only the handful of flagged frames; absent on a clean (zero-loss, fully decodable) run.
  STRIH_PIXELS_WIN="$OUT_DIR_WIN\\strih-partial-${RUN_ID}-pixels"
  STREAM_PIXELS_WIN="$OUT_DIR_WIN\\stream-partial-${RUN_ID}-pixels"
  STRIH_PIXELS="$OUTDIR/strih-partial-${RUN_ID}-pixels"   # #186 pixel proofs pulled back to dev1
  STREAM_PIXELS="$OUTDIR/stream-partial-${RUN_ID}-pixels"  # #186 pixel proofs pulled back to dev1

  echo "    --- [8/8a] extract the STRIH partial ON the strih box (win-strih), in place ---"
  # The strih recording carries cam1 (forwarded) + strih burns; --extract-partial strih decodes
  # it IN PLACE on the strih box and writes the small partial JSON. It is NEVER copied off-box.
  "$HERE/recording-verdict-on-strih.sh" \
    --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" --strih-rec "$STRIH_REC_WIN" \
    -- --extract-partial strih --strih "$STRIH_REC_WIN" --capture-fps "$STRIH_CAPTURE_FPS" \
       --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$BURN_STRIH_RUN_ID" \
       --out "$STRIH_PARTIAL_WIN"
  echo "    pull back to dev1: $STRIH_PARTIAL  AND the #186 pixel-proof dir $STRIH_PIXELS"
  echo "      (win-strih FileDownload $STRIH_PARTIAL_WIN -> $STRIH_PARTIAL;"
  echo "       win-strih FileDownload $STRIH_PIXELS_WIN -> $STRIH_PIXELS  [absent on a clean run])"

  echo "    --- [8/8b] extract the STREAM partial ON the stream box (win-stream-snv), in place ---"
  # The stream recording carries all three burns; --extract-partial stream decodes it IN PLACE on
  # the stream box. It is passed ONLY its own --stream recording — NEVER the strih recording (the
  # strih recording is decoded on the strih box above), so no box-to-box copy is ever needed.
  "$HERE/recording-verdict-on-stream.sh" \
    --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" --stream-rec "$STREAM_REC_WIN" \
    -- --extract-partial stream --stream "$STREAM_REC_WIN" --capture-fps "$STREAM_CAPTURE_FPS" \
       --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" \
       --cam2-run-id "$RUN_ID" \
       --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$BURN_STRIH_RUN_ID" \
       --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
       --out "$STREAM_PARTIAL_WIN"
  echo "    pull back to dev1: $STREAM_PARTIAL  AND the #186 pixel-proof dir $STREAM_PIXELS"
  echo "      (win-stream-snv FileDownload $STREAM_PARTIAL_WIN -> $STREAM_PARTIAL;"
  echo "       win-stream-snv FileDownload $STREAM_PIXELS_WIN -> $STREAM_PIXELS  [absent on a clean run])"

  echo "    --- [8/8c] MERGE the two small partials ON dev1 (no recording on dev1) ---"
  echo "    After pulling both partials (+ their <partial>-pixels dirs) to dev1, run the merge:"
  # The merge reads ONLY the small JSONs (+ the small painter CSV / capture-stats already on dev1)
  # and produces the SAME full-chain verdict the fused path would — equivalent fields + PASS.
  MERGE_ARGS=(--merge-partials "strih=$STRIH_PARTIAL" --merge-partials "stream=$STREAM_PARTIAL" \
    --min-secs 300 --capture-fps "$STRIH_CAPTURE_FPS" \
    --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" --cam2-run-id "$RUN_ID" \
    --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$BURN_STRIH_RUN_ID" \
    --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
    --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
  if [ -f "$PAINTER_CSV" ]; then MERGE_ARGS+=(--painter "$PAINTER_CSV"); fi
  if [ -f "$CAM1_CAPTURE_STATS" ]; then MERGE_ARGS+=(--cam1-capture-stats "$CAM1_CAPTURE_STATS"); fi
  VERDICT_BIN="$(cd "$PROBE_BIN_DIR" && pwd)/recording-verdict"
  printf '      %q ' "$VERDICT_BIN" "${MERGE_ARGS[@]}"; echo
  echo "    The win-* MCP holder runs 8/8a + 8/8b on each box, pulls both small partials (+ their"
  echo "    <partial>-pixels #186 proof dirs) to dev1, then runs the 8/8c merge above on dev1. A"
  echo "    recording is NEVER copied box-to-box nor to dev1 — only the small partial JSONs (+ the"
  echo "    painter CSV + the handful of flagged-frame PNGs) move (#208/#186)."
  echo "    ============================================================================"
  echo "    NOTE: this exit code is NOT the zero-loss verdict. In per-box mode the harness only"
  echo "          EMITS the plan (scp/ssh to Windows is denied, so bash cannot run it itself). The"
  echo "          PASS/FAIL is the merge recording-verdict EXIT CODE on dev1 + the pulled-back"
  echo "          JSON — read THOSE, not this script's exit 0."
  echo "    ============================================================================"
  exit 0
fi

# #178: re-enable abort-on-error for the verdict run below — it manages its own exit via
# verdict-monitor.sh (GATE), so set -e here does not abort the run; it just restores strict
# mode for the remainder. (The orchestration that could fail transiently is above, guarded.)
# (LEGACY decode-on-dev1 path — reached only when VERDICT_ON_STREAM=0.)
set -e

# #166 LIVENESS-GUARDED verdict run. The verdict decodes multi-GB recordings for
# minutes; if it CRASHES (the #166 night: it died silently after >1 h) or HANGS, a
# naive "wait for it to finish" would block FOREVER (a crashed process writes no
# completion marker). So we run it in the BACKGROUND, tee its output to a file, write
# its exit code to a marker on completion, and let verdict-monitor.sh fail LOUDLY on a
# dead-or-stalled process instead of hanging the whole run. RUST_LOG=info makes the
# per-recording progress (probe/decode/complete lines) visible as output growth so the
# stall detector has a real liveness signal.
VERDICT_OUT="$OUTDIR/verdict-${RUN_ID}.out"
VERDICT_EXIT_MARKER="$OUTDIR/verdict-${RUN_ID}.exit"
rm -f "$VERDICT_EXIT_MARKER"
# No progress for this many seconds ⇒ the verdict is wedged → fail fast. The parallel
# decode (#166) emits an INFO line per recording phase; the longest silent stretch is a
# single recording's decode loop, well under this bound even for a 30-min 4K clip.
VERDICT_STALL_TIMEOUT="${VERDICT_STALL_TIMEOUT:-600}"
echo "    verdict output: $VERDICT_OUT (stall-timeout ${VERDICT_STALL_TIMEOUT}s, parallel decode #166)"
# Run the verdict in its OWN process group via setsid: $! is then the group leader
# (pid == pgid), so the monitor's STALL kill can signal the WHOLE group (the wrapper
# AND the heavy recording-verdict child) and never orphan the runaway decode (#166
# review BUG 1). The wrapper writes the verdict's exit code to the marker on exit; a
# verdict CRASH still writes a non-zero code (so the monitor fails loud), and a total
# death (no marker) is caught by the monitor's DEAD path.
export RUST_LOG="${RUST_LOG:-info}"
# bash -c body: $0=verdict binary (absolute), $1=marker path, $2.. = verdict args. The
# single-quoted body is expanded by the INNER bash (SC2016 is expected here), and every
# path is passed as an argument (no string interpolation), so a path with spaces/quotes is
# safe. The verdict binary comes from $PROBE_BIN_DIR (target/release for a local build, or
# the downloaded CI probe-tools artifact when USE_PREBUILT_PROBE_DIR is set, #133) —
# resolved to an absolute path so the inner bash can run it without a cwd assumption.
VERDICT_BIN="$(cd "$PROBE_BIN_DIR" && pwd)/recording-verdict"
# shellcheck disable=SC2016
setsid bash -c 'v="$0"; m="$1"; shift; "$v" "$@"; echo "$?" > "$m"' \
  "$VERDICT_BIN" "$VERDICT_EXIT_MARKER" "${VERDICT_ARGS[@]}" >"$VERDICT_OUT" 2>&1 &
VERDICT_PID=$!
# Monitor to a terminal state: returns the verdict's own exit code on clean completion,
# 124 on STALL, 126 on a silent death. Either failure mode aborts the run with a clear
# diagnostic — never an all-night hang (#166).
if "$HERE/verdict-monitor.sh" \
     --pid "$VERDICT_PID" --output "$VERDICT_OUT" --exit-marker "$VERDICT_EXIT_MARKER" \
     --stall-timeout "$VERDICT_STALL_TIMEOUT" --poll 5 --label verdict; then
  GATE=0
else
  GATE=$?
fi
# Surface the verdict's own output (the human-readable per-hop verdict) in the run log.
echo "    ----- recording-verdict output -----"
cat "$VERDICT_OUT" 2>/dev/null || true
echo "    ------------------------------------"

echo "[8/8] render the 2-graph report PNG"
if [ -f "$REPORT_JSON" ]; then
  python3 "$HERE/recording-e2e-report.py" --json "$REPORT_JSON" --out "$REPORT_PNG" || \
    echo "WARNING: report render failed (non-fatal; JSON at $REPORT_JSON)" >&2
fi

echo "artifacts in $OUTDIR (verdict json: $REPORT_JSON, report: $REPORT_PNG)"
exit "$GATE"
