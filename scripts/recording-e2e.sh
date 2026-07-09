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
# the SOURCE-camera/cam2 camera-box services on exit (incl. cancel). The operator is the
# guard (project decision: no automated streaming guard).
#
# #24 item 1: "cam1" in the comments above is the DEFAULT SOURCE-camera role, not the only
# one — CAM=cam1|cam3|cam4 selects which physical box plays it (camera_resolve() +
# camera_strih_route() below resolve its IP + strih scene/NDI-input; cam2 stays the fixed
# painter regardless). Everything downstream (the deploy, the routing, the teardown) follows
# the resolved camera; only cam1 is the unset default (back-compat with every prior run).
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi, cargo, sshpass, python3 +
# websocket-client, matplotlib (for the report). OBS WebSocket :4455 on strih+stream,
# DistroAV "NDI Main Output" enabled on both. cam1/cam2 SSH (root, pw newlevel).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# #309: single-sourced #291 no-display drop-in path + clear-on-restore builder (shared with
# rig-mode.sh) — cleanup() clears any leftover drop-in before restoring cam2's camera-box.
# shellcheck source=scripts/lib/rig-test-dropin.sh
. "$HERE/lib/rig-test-dropin.sh"
# #420/#421: SINGLE SOURCE OF TRUTH for the QPSK audio-marker AUDIBLE self-check (ALSA CARD/DEV
# parsing + the `state: RUNNING` poll + fail-loud diagnostic), shared with rig-mode.sh's TEST-mode
# painter launch (#420) so both launches can never drift on what "audible" means.
# shellcheck source=scripts/lib/audio-marker-check.sh
. "$HERE/lib/audio-marker-check.sh"
# #281 Fix#3: the rig-active heartbeat — tells the rig-restore watchdog "a legit E2E is running, do
# NOT auto-restore prod". Started after the cleanup trap is armed (below); cleanup() stops it on
# EXIT/HUP/INT/TERM, so a clean exit OR a mid-flight death clears/lapses the heartbeat and the
# watchdog may then recover a genuinely stranded rig.
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$HERE/lib/rig-heartbeat.sh"
# #359: the painter-CSV freshness verdict (pure + unit-tested in
# tests/harness_painter_csv_freshness.rs) — used by the fail-loud gate after the painter pull.
# shellcheck source=scripts/lib/painter-csv-freshness.sh
. "$HERE/lib/painter-csv-freshness.sh"
camera_resolve "${CAM:-cam1}"
# #24 item 1: this harness's SOURCE-camera role (the physical box filming cam2's monitor via
# the optical loopback + carrying the #174 render-time capture burn) is one of
# cam1/cam3/cam4/cam5/cam6 ONLY (#312 fleet growth 4→6, #451) — cam2 is deliberately EXCLUDED
# from this role: it is the fixed painter (its own monitor + /dev/fb0), and camera_strih_route()
# rejects it by design so it can never be selected as SOURCE (see that function's own doc for
# why — the device conflict with $PAINTER_IP). cam2 IS separately wired as a "camera under
# test" for the ALL-CAMBOX sweep's digital-burn contiguity check (recording-verdict.rs's
# CAMERA_UNDER_TEST_NODES) via its own dedicated scene "Cam 2"/"NDI cam2" and burn id, keyed
# off $PAINTER_IP directly in the [2b/8] deploy loop below — NEVER through this SOURCE-camera
# resolution. camera_strih_route() (camera-set.sh) fails loudly (via `set -e`, mirroring
# camera_resolve's own bare-call style above) on any unsupported CAM rather than silently
# certifying the wrong box; on success it sets CAMERA_STRIH_SCENE/CAMERA_STRIH_SOURCE, consumed
# below.
camera_strih_route "$CAMERA_NAME"
# ALL_CAMBOX=1's OWN secondary-camera deploy loop ([2b/8] below) unconditionally deploys
# cam2/cam3/cam4/cam5/cam6 at their FIXED physical IPs. If CAM=cam3/cam4/cam5/cam6 is ALSO
# picked as the primary SOURCE camera, [2/8] would deploy that SAME physical box a second time
# under a different burn binary — a real device/process conflict (two camera-box instances
# fighting over /dev/video0), not just a labeling nit. Reject the combination loudly instead.
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ "$CAMERA_NAME" != "cam1" ]; then
  echo "ERROR: CAM='$CAMERA_NAME' + ALL_CAMBOX=1 is not supported — ALL_CAMBOX's own [2b/8]" >&2
  echo "       loop already deploys cam2/cam3/cam4/cam5/cam6 at their fixed IPs alongside the" >&2
  echo "       primary; picking one of them as the primary SOURCE camera too would" >&2
  echo "       double-deploy the same physical box. Run CAM=cam3/cam4/cam5/cam6 WITHOUT" >&2
  echo "       ALL_CAMBOX for a dedicated single-node source-camera certification (#24)." >&2
  exit 1
fi

CAM1_IP="${CAM1_IP:-$CAMERA_IP}"      # the SOURCE camera (films cam2's monitor, emits NDI w/ #174 burn); resolved via CAM=/camera_resolve above (#24) — despite the name, this is whichever of cam1/cam3/cam4/cam5/cam6 was selected
PAINTER_IP="${PAINTER_IP:-10.77.9.62}" # cam2 — the box with the physical monitor cam1 films; #312: ALSO deployed as its OWN camera-under-test node ([2b/8] below), keyed off this same IP
# #624/#312: the OTHER camera-under-test boxes the ALL_CAMBOX sweep cuts into strih program
# (cam2's own chain + cam3/cam4/cam5/cam6). Only used (deployed to / restored) when
# ALL_CAMBOX=1 — the default single-camera path never touches them. Same physical IPs
# camera-set.sh / cam-disk-guard.sh / rig-restore-watchdog.sh use.
CAM3_IP="${CAM3_IP:-10.77.9.63}"
CAM4_IP="${CAM4_IP:-10.77.9.64}"
CAM5_IP="${CAM5_IP:-10.77.9.65}"
CAM6_IP="${CAM6_IP:-10.77.9.66}"
STRIH=10.77.9.202
STREAM=10.77.9.204
# #462 (EPIC #466 Topology v2): imag-nb — the NEW 60fps low-latency IMAG cutter of all 6 NDI
# cameras (Linux, own recorded program). A THIRD recorded+decoded node alongside strih+stream —
# its zero-loss proof is the cam2 OPTICAL tick's own contiguity (60fps, no beat) ANDed with its
# own 911003 digital corner burn (#463) when present.
IMAG_IP="${IMAG_IP:-10.77.9.182}"
CAM_PW=newlevel
RUN_ID="${RUN_ID:-$(( (RANDOM << 16) | RANDOM ))}"
DURATION="${DURATION:-1800}"
if [ "$DURATION" -lt 300 ]; then
  echo "ERROR: DURATION=${DURATION} below the 300 s zero-loss floor (default 1800)." >&2
  exit 1
fi
QR_SIZE="${QR_SIZE:-700}"
# Topology v2 (#459, EPIC #466, SUPERSEDES the #11 60fps-end-to-end framing below): the 60fps
# low-latency IMAG role moved OFF strih onto the new imag-nb box (10.77.9.182, #458/#463); strih
# is now cut-to-stream ONLY, at 30fps. Cam boxes still emit 60fps NDI (cam1 still films cam2's
# 60Hz-painted monitor at 60fps for the optical proof) — the 60→30 beat that used to sit at
# strih→stream now sits INSIDE strih's own ingest (cam→strih). PAINT_FPS stays 60 (cam2's monitor
# refresh is unaffected by strih's fps; moot under KMS anyway — the painter is vblank-paced at the
# monitor's 60 Hz, one dual-QR id per flip, --paint-fps ignored, but defaulting it to 60 keeps the
# non-KMS fallback correct). GENLOCK_FPS is cam1's CAMERA_BOX_GENLOCK_FPS — the NDI emit rate the
# genlock gate wall-paces the 60 fps capture onto (60 = 1:1 pass-through onto the 60 fps wall
# boundaries) — cameras are UNCHANGED by this topology move.
PAINT_FPS="${PAINT_FPS:-60}"
GENLOCK_FPS="${GENLOCK_FPS:-60}"
# #459: the recorded OBS program fps is now 30 on BOTH boxes — strih records its own 30fps
# cut-to-stream canvas (the 60→30 camera-feed decimation happens on strih's OWN ingest now, not on
# the strih→stream hop) and stream records the same 30fps feed, plain pass-through, no further
# decimation. Each feeds ITS recording's DIAGNOSTIC span (analyzed_secs = frames / capture_fps) and
# optical expected-step (refresh_hz / capture_fps) — kept as TWO separate knobs (rather than one
# shared constant) so a future topology change can re-diverge them without another rename. The
# decimation LOSS step is gap-ignore for strih/stream regardless of these rates (#360 —
# node_render_step returns 1 for them, their free-running render tick is not a clean decimation);
# --strih-emit-fps / --stream-capture-fps below are RETAINED on recording-verdict's CLI for
# provenance, decoupled from these diagnostic rates so they are always correct regardless of which
# recording's --capture-fps is in effect. #571: cam1/cam3/cam4 (the camera-under-test) now DO
# consult a decimation step for the SEPARATE cam(60fps)->strih(30fps) hop — derived from
# --refresh-hz (default 60, unset here) / --capture-fps (STRIH_CAPTURE_FPS, 30), never these two.
STRIH_CAPTURE_FPS="${STRIH_CAPTURE_FPS:-30}"
STREAM_CAPTURE_FPS="${STREAM_CAPTURE_FPS:-30}"
# #462/#461: imag-nb's OWN recording rate (its own box, its own low-latency 60fps rate — never
# strih's/stream's). Feeds recording-verdict's --imag-capture-fps (recording_span_gate's third
# rate slot, #373 duration-floor computed against imag's own rate).
IMAG_CAPTURE_FPS="${IMAG_CAPTURE_FPS:-60}"
# #174 cam1-capture render-time burn run_id (the value CAMERA_BOX_BURN_RUN_ID is set to on
# cam1). Mirrors the verdict's BURN_RUN_ID_CAM1 default (911001). Distinct from the strih
# (911002) / stream (911004) burn ids so all four marks are told apart by run_id. This burn
# IS the cam1 mark in the stream recording — the reason #179 can drop the cam1 grab.
BURN_CAM1_RUN_ID="${BURN_CAM1_RUN_ID:-911001}"
# #624: cam3/cam4 capture-burn run_ids, deployed ONLY under ALL_CAMBOX=1 (mirrors cam1's burn
# above but on the OTHER camera-under-test boxes the sweep cuts into strih program). Match
# recording-verdict's own BURN_RUN_ID_CAM3 (911008) / BURN_RUN_ID_CAM4 (911007) defaults exactly
# so the verdict finds them without any extra flag even if these are left at default.
BURN_CAM3_RUN_ID="${BURN_CAM3_RUN_ID:-911008}"
BURN_CAM4_RUN_ID="${BURN_CAM4_RUN_ID:-911007}"
# #312: cam2's OWN capture-burn run_id, deployed ONLY under ALL_CAMBOX=1 -- cam2 is the fixed
# dual-QR PAINTER but (since #291) its camera-box daemon keeps capturing+emitting its own NDI
# feed throughout the run, so its OWN chain is ALSO measurable by this SAME mechanism. Matches
# recording-verdict's BURN_RUN_ID_CAM2 (911009) default.
BURN_CAM2_RUN_ID="${BURN_CAM2_RUN_ID:-911009}"
# #312: cam5/cam6 capture-burn run_ids (fleet growth 4→6, #451), deployed ONLY under
# ALL_CAMBOX=1. Match recording-verdict's BURN_RUN_ID_CAM5 (911010) / BURN_RUN_ID_CAM6 (911011)
# defaults exactly.
BURN_CAM5_RUN_ID="${BURN_CAM5_RUN_ID:-911010}"
BURN_CAM6_RUN_ID="${BURN_CAM6_RUN_ID:-911011}"
# #24 item 1: which of the reserved ids above belongs to the box actually filling the
# SOURCE-camera role THIS run ($CAMERA_NAME, resolved via CAM= at the top; NEVER cam2 — see
# camera_strih_route()'s own doc). The ids are already mutually distinct and already read
# INDEPENDENTLY by recording-verdict's full-chain verdict (CAMERA_UNDER_TEST_NODES,
# src/bin/recording-verdict.rs) — deploying the resolved camera under the id that matches its
# OWN role below, and leaving the other ids at their own (never-deployed-this-run, so
# never-present) defaults, is all that's needed. No recording-verdict changes: every
# `--burn-cam1-run-id "$BURN_CAM1_RUN_ID"` call site elsewhere in this script stays untouched
# (it correctly reports "no cam1 present" when a different camera was actually deployed; the
# deployed camera's OWN flag/default catches it).
case "$CAMERA_NAME" in
  cam1) SRC_BURN_RUN_ID="$BURN_CAM1_RUN_ID" ;;
  cam3) SRC_BURN_RUN_ID="$BURN_CAM3_RUN_ID" ;;
  cam4) SRC_BURN_RUN_ID="$BURN_CAM4_RUN_ID" ;;
  cam5) SRC_BURN_RUN_ID="$BURN_CAM5_RUN_ID" ;;
  cam6) SRC_BURN_RUN_ID="$BURN_CAM6_RUN_ID" ;;
esac
OUTDIR="${OUTDIR:-/tmp/recording-e2e-${RUN_ID}}"
mkdir -p "$OUTDIR"
# #359: wall-clock run start. The painter ground-truth CSV (gen_ts_ns = CLOCK_REALTIME epoch
# ns under --wall-clock) is freshness-gated against this — a stale CSV whose first gen_ts is
# hours off (run 354002 was 14.9h off) is REJECTED before it can corrupt the verdict.
RUN_START_EPOCH="$(date +%s)"
PAINTER_CSV="$OUTDIR/painter-${RUN_ID}.csv"
STRIH_REC="$OUTDIR/strih-${RUN_ID}.mkv"
STREAM_REC="$OUTDIR/stream-${RUN_ID}.mp4"
REPORT_JSON="$OUTDIR/verdict-${RUN_ID}.json"
REPORT_PNG="$OUTDIR/report-${RUN_ID}.png"
SWITCH_SCHEDULE_JSON="$OUTDIR/switch-schedule.json"  # #312 Phase-2 all-cambox sweep (ALL_CAMBOX=1)
# #312 item 2 (PR A): the cam2 painter's CONTINUOUS QPSK audio-marker log for the WHOLE
# ALL_CAMBOX run duration (fuses per-camera A/V-sync into the same run/verdict, #624 deliverable
# 4). ALL_CAMBOX=1 only — the plain single-camera path never emits this.
MARKER_CSV="$OUTDIR/av-markers-${RUN_ID}.csv"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

# #328: hard timeouts so a hung obs-websocket op (the #328 prod-scene/teardown hang) can NEVER
# block the cleanup trap and strand a cam capture device. OBS_CLEANUP_TIMEOUT bounds each
# obs_phase2/obs_burn_filter call in cleanup(); CLEANUP_SSH_TIMEOUT bounds each cam-box restore ssh
# (so a stuck cam1 ssh can't block cam2's restore either). Both env-overridable. (obs_phase2.py
# also self-bounds each WS request via OBS_OP_TIMEOUT_S=60 — these are the shell-side backstop.)
OBS_CLEANUP_TIMEOUT="${OBS_CLEANUP_TIMEOUT:-90}"
CLEANUP_SSH_TIMEOUT="${CLEANUP_SSH_TIMEOUT:-30}"

# #220: CAMERA PRE-RUN CHECKLIST. The cam2->SOURCE OPTICAL injection leg (the SOURCE camera,
# #24: whichever of cam1/cam3/cam4 was resolved via CAM= above, filming the cam2 monitor QR)
# depends on THAT camera's MANUAL settings, which the harness CANNOT read or set: camera-box
# reads /dev/video0 (the ShadowCast capture card), which does NOT expose the BMPCC's
# shutter/focus/exposure. A 1/60 shutter integrates a full 60Hz monitor refresh and SMEARS the
# dual-QR Vernier mid-change -> the optical read drops (the #216 ~175s gap; the DIGITAL burns
# were unaffected, so the chain stayed 0 real loss — purely the optical-INJECTION leg).
# Satisfy this BEFORE the run, then the cam2->SOURCE read is reliable with no spurious optical gap.
echo "=================================================================================="
echo " CAMERA PRE-RUN CHECKLIST ($CAMERA_NAME broadcast camera — the harness CANNOT auto-set these)"
echo "   [ ] SHUTTER FAST: >= 1/500 s (ideally 1/1000) — freezes the 60Hz monitor QR, no smear"
echo "   [ ] FOCUS: MANUAL, locked on the cam2 monitor (no autofocus hunting)"
echo "   [ ] EXPOSURE: FIXED / manual gain (no auto-exposure drift)"
echo " A 1/60 shutter caused the #216 ~175s optical-read gap. Fix the camera, THEN run."
echo "=================================================================================="

echo "[0/8] reachability preflight ($CAMERA_NAME source, cam2 painter, strih, stream, imag — #462)"
for hp in "$CAMERA_NAME=$CAM1_IP" "cam2(painter)=$PAINTER_IP" "strih=$STRIH" "stream=$STREAM" "imag=$IMAG_IP"; do
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
echo "[0/8] DanteSync NTP+PTP gate — $CAMERA_NAME, cam2, strih, stream must ALL be synced+locked (#7/#8)"
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
  --linux "$CAMERA_NAME=$CAM1_IP cam2=$PAINTER_IP" \
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

# dev1<->painter clock-offset gate — ALL_CAMBOX sweep ONLY (#326, #312 Phase-2 robustness). The
# all-cambox sweep ([6/8] below) stamps each program-switch WINDOW boundary on dev1's
# CLOCK_REALTIME, while the painted ticks (and the burns recording-verdict --switch-schedule keys
# on) ride the painter (cam2) DanteSync clock. If dev1's clock is offset from the painter by more
# than the verdict's transition guard, frames near every boundary get attributed to the WRONG
# cambox window (silent #312 mis-attribution → false gaps/copies or a hidden real loss). So before
# the multi-minute sweep, assert the dev1<->painter offset is well within the guard and FAIL FAST
# otherwise — the same fail-fast spirit as the DanteSync/version gates above. ON by default;
# bypass with SKIP_CLOCK_OFFSET_ASSERT=1 (the gate honours it). Only the all-cambox path stamps
# windows on dev1's clock, so the gate is irrelevant to the default single-hold run.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  echo "[0/8] dev1<->painter clock-offset gate — all-cambox window attribution must be trustworthy (#326)"
  "$HERE/clock-offset-painter-gate.sh" --painter "cam2=$PAINTER_IP"
fi

# cam1 v4l2 capture controls (#338/#312): apply the device-default colour controls
# (saturation=50, contrast=50) BEFORE the run. The old "sharp set" (saturation=0,
# contrast=75) was meant to aid the optical dual-QR decode but HURT it (#312 run 312005:
# the ShadowCast box with the sharp set read ~50% undecodable while the NZXT card on
# device defaults read the SAME monitor clean). Device defaults decode fine; saturation=0
# also tinted/greyed the picture. The [2/8] deploy step re-applies the same colour
# set at open; this is the belt-and-braces preflight the harness owns regardless.
echo "[0/8] apply device-default colour controls (saturation=50, contrast=50) (#338/#312)"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "v4l2-ctl -d /dev/video0 --set-ctrl=saturation=50,contrast=50 2>/dev/null; \
   v4l2-ctl -d /dev/video0 --get-ctrl=saturation,contrast 2>/dev/null" \
  || echo "WARNING: could not pre-apply $CAMERA_NAME v4l2 controls (the [2/8] deploy step re-applies them)" >&2

# shellcheck disable=SC2317  # cleanup() runs via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  # #281 Fix#3: clear the rig-active heartbeat + stop its refresher FIRST — before the cam/OBS
  # restores (which may hang). Once the heartbeat lapses, the rig-restore watchdog is free to
  # recover prod if this run left the rig stranded (e.g. the trap itself is interrupted).
  rig_heartbeat_stop 2>/dev/null || true
  # #353: remove the E2E marker on this CLEAN exit. The marker is the durable "rig in an uncleaned
  # test state" signal: it is written on entry and removed ONLY here, so an UNCLEAN death (SIGKILL /
  # interrupted trap) leaves it behind and the watchdog detects the stranded rig regardless of which
  # scene OBS is on (replaces the fragile scene-name scraping, #353).
  rig_e2e_marker_clear 2>/dev/null || true
  # #328: FREE the cam capture devices FIRST — before, and independent of, the OBS restore — so a
  # hung obs-websocket op (the #328 prod-scene/teardown hang) can NEVER strand /dev/video0. In the
  # #312 incident the OBS teardown ran first and hung, the trap never reached the cam1 restore, and
  # cam1's burn binary kept holding /dev/video0 → the prod camera-box crash-looped. Freeing the
  # device is the safety-critical action, so it leads; every cam ssh AND every OBS call below is
  # wrapped in `timeout` so nothing in cleanup() can block the trap indefinitely.
  echo "[cleanup] #328 FREE $CAMERA_NAME/cam2 capture devices FIRST (never gated behind OBS teardown)"
  # cam1: FORCE-kill the manual #174 burn binary (pkill -9 -f, its own basename) AND any camera-box,
  # remove the deployed test binary, restore the clean deployed service — reliably frees /dev/video0.
  # #626: the pattern MUST be anchored ('camera-box-burn-[a-z0-9]') — a bare 'camera-box-burn-'
  # is a SELF-MATCH: the remote `sh -c "..."` process invoked BY ssh has this exact substring in
  # its OWN /proc/*/cmdline (it's the literal text of the pkill argument being run), so `pkill -f`
  # kills that shell before it ever reaches `systemctl restart` — a live 3h40m undetected outage
  # on cam1/cam3/cam4 traced to this exact bug (#626). The real target's argv0 always has EITHER a
  # run-id digit immediately after the hyphen (cam1's own /tmp/camera-box-burn-1783530925) OR a
  # camname letter (cam2/cam3/cam4/cam5/cam6's own #624/#312 ALL_CAMBOX deploy,
  # /tmp/camera-box-burn-cam3-1783530925 — `_cbin="/tmp/camera-box-burn-${_cn}-${RUN_ID}"`); the
  # invoking shell's own cmdline has a `[` bracket character there instead (the regex's own
  # class-open), so the anchored `[a-z0-9]` pattern matches ONLY a real target, never itself.
  # #628 CORRECTION: an earlier version of this comment claimed the DIGIT-only pattern
  # ('camera-box-burn-[0-9]') already matched the camname-infixed form too — it does NOT (the
  # character right after the hyphen there is a LETTER, not a digit). That gap orphaned
  # cam2/cam3/cam4/cam5/cam6's burn processes across multiple runs, crash-looping camera-box
  # ("Device or resource busy") until manually killed — found live while verifying #312 item 2 PR B.
  timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
    "pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
     rm -f /tmp/camera-box-burn-* 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
  # #624/#312: cam3/cam4/cam5/cam6 — same restore as cam1, ONLY when the ALL_CAMBOX deploy above
  # actually ran (gated the same way) so a plain single-camera run never touches these boxes at all.
  if [ "${ALL_CAMBOX:-0}" = "1" ]; then
    for _cip in "$CAM3_IP" "$CAM4_IP" "$CAM5_IP" "$CAM6_IP"; do
      timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
        "pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
         rm -f /tmp/camera-box-burn-* 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
    done
  fi
  # cam2 (painter): restart it. #309: FIRST clear any leftover #291 rig-mode no-display drop-in
  # (a prior `rig-mode.sh test` would otherwise make this restart bring camera-box back WITHOUT
  # --display — the interkom return monitor stays dark). The clear is single-sourced
  # (rig_test_dropin_clear_cmds) + idempotent (rm -f is a no-op if absent). #312: under
  # ALL_CAMBOX=1, [2b/8] ALSO deployed a manually nohup'd probe-featured burn binary here (the
  # SAME #628-widened kill pattern this cleanup uses elsewhere) — harmless (matches nothing) on
  # the plain single-camera path, where [2b/8] never ran.
  timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$PAINTER_IP" "pkill -x frame-probe 2>/dev/null || true
pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null || true
rm -f /tmp/camera-box-burn-* 2>/dev/null || true
$(rig_test_dropin_clear_cmds)
systemctl restart camera-box 2>/dev/null || true
systemctl start cam2-painter 2>/dev/null || true"
  # The cam devices are now freed regardless of what the OBS restore does. #328: bound every OBS
  # call by `timeout` so a hung obs-websocket op (#328) can't block the trap even if it runs.
  echo "[cleanup] restore OBS program scenes (each bounded by ${OBS_CLEANUP_TIMEOUT}s — #328)"
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop >/dev/null 2>&1
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop >/dev/null 2>&1
  # #462: imag never had its program scene routed by THIS harness (rig-mode.sh test owns that), so
  # there is no scene state to restore — only a StopRecord safety net (a leftover recording must
  # finalize even if the run aborted mid-flight).
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action stop >/dev/null 2>&1
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" teardown --host "$STREAM"
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" teardown --host "$STRIH"
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
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" remove --host "$_bip" --input "$_bsrc" 2>&1 \
      | sed "s/^/    [$_bn burn-clear] /" || true
    _vrf="$(timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" check --host "$_bip" --input "$_bsrc" 2>&1 || true)"
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
# #24: default to the resolved SOURCE camera's own scene/NDI-input (camera_strih_route above,
# e.g. 'Cam 1'/'NDI cam1' for cam3) rather than the cam1-only 'Cam 5'/'NDI cam5' — an explicit
# override still wins.
STRIH_PROG_SCENE="${STRIH_PROG_SCENE:-$CAMERA_STRIH_SCENE}"   # prod scene showing the SOURCE camera
STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-$CAMERA_STRIH_SOURCE}" # the prod input behind that scene (#246 burn-off target)
STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-PRO}"          # #343: record the ALREADY-ACTIVE prod scene (NDI 2ME PGM already warm) — no cold re-activation
STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}" # the prod input the scene shows
# #462 (EPIC #466): imag-nb's program-feeding NDI input — the #399-style 1:1 mapping from Phase 1
# (setup-imag.sh) pins 'NDI CAM1'..'NDI CAM6' -> 'CAMx (usb)' 1:1, so cam1 (the SOURCE camera that
# films cam2's monitor) rides 'NDI CAM1'. rig-mode.sh TEST mode is what actually routes imag's
# PROGRAM onto that scene + toggles this burn ON; this harness defensively ensures/verifies it too
# (the SAME "single source of truth" BURN_TARGETS array, extended below).
IMAG_PROG_SOURCE="${IMAG_PROG_SOURCE:-NDI CAM1}"
# #252: single source of truth for the host=ip=source burn triples. The #195 pre-record burn-ON
# gate and the #246 cleanup() burn-clear loop iterate the SAME set; keeping it in one array means a
# third box (or a triple-structure change) can never green-light a set the cleanup does not clear
# (the #246 linger-onto-live-broadcast hazard). Defined HERE — after the *_PROG_SOURCE vars and
# BEFORE the cleanup trap is armed — so cleanup()'s array expansion is never an unbound `set -u`
# var on an early abort (same ordering reason the *_PROG_SOURCE vars precede the trap). #462: imag
# is now a THIRD burn target (its own 911003 digital corner burn, #463) — the exact extension this
# array's design already anticipated.
BURN_TARGETS=("strih=$STRIH=$STRIH_PROG_SOURCE" "stream=$STREAM=$STREAM_PROG_SOURCE" "imag=$IMAG_IP=$IMAG_PROG_SOURCE")
trap cleanup EXIT HUP INT TERM
# #281 Fix#3: start the rig-active heartbeat NOW (trap is armed, so cleanup() will stop it on any
# exit). The background refresher keeps it fresh for the whole long run; the rig-restore watchdog
# treats a fresh heartbeat as "a legit E2E is running" and will NOT auto-restore prod underneath it.
rig_heartbeat_start "recording-e2e" || echo "WARNING: could not start rig-active heartbeat (#281)" >&2
# #353: write the E2E MARKER now (trap is armed, so cleanup() removes it on a CLEAN exit). Unlike the
# heartbeat (which the refresher removes the instant the harness dies), the marker persists across an
# UNCLEAN death — so "marker present AND heartbeat absent/stale" is the durable stranded-rig signal
# the rig-restore watchdog keys on, regardless of which scene OBS is left on.
rig_e2e_marker_set "recording-e2e" || echo "WARNING: could not write rig-in-e2e marker (#353)" >&2

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
  for b in camera-box frame-probe recording-verdict frozen-camera-gate render-budget-gate av-restart-sync-gate zero-loss-restart-gate; do
    if [ ! -f "$PROBE_BIN_DIR/$b" ]; then
      echo "ERROR: prebuilt probe binary '$b' missing in $PROBE_BIN_DIR — download the CI" >&2
      echo "       probe-tools-linux-amd64 artifact into it, then re-run." >&2
      exit 1
    fi
    chmod +x "$PROBE_BIN_DIR/$b" 2>/dev/null || true
  done
else
  echo "[1/8] build frame-probe + recording-verdict + camera-box (probe-featured for the #174 capture burn)"
  # #174: build camera-box WITH --features probe so the cam1-capture render-time QR burn is
  # present (the production artifact stays probe-free / clean; only this TEST binary carries
  # the burn + qrcode dep). The burn is still gated at runtime by CAMERA_BOX_BURN_RUN_ID.
  cargo build --release --features probe --bin frame-probe --bin recording-verdict --bin camera-box  # airuleset:build-ok
  # #365/#405/#137/#109: build the default-feature gate binaries (no probe deps, no disk balloon).
  cargo build --release --bin frozen-camera-gate --bin render-budget-gate --bin av-restart-sync-gate --bin zero-loss-restart-gate  # airuleset:build-ok
fi

echo "[2/8] $CAMERA_NAME (${CAM1_IP}) — probe-featured camera-box with the #174 capture BURN (emits NDI w/ $CAMERA_NAME mark, NO grab #179)"
# #174 + #179: deploy the freshly-built PROBE-featured camera-box (carries the #174 capture
# burn) to a $CAMERA_NAME-LOCAL /tmp path and launch THAT — NOT the prod
# /usr/local/bin/camera-box (the clean production binary with no burn). The burn is
# runtime-gated by CAMERA_BOX_BURN_RUN_ID, so it draws the resolved SOURCE camera's own
# run_id (#24: $SRC_BURN_RUN_ID, matching $CAMERA_NAME) + per-emit frame_id + CAPTURE
# wall-clock ts into the EMITTED frame, which rides through NDI → strih → stream. #179: the
# grab-record flags are GONE — the burn mark in the stream recording fully replaces the
# 7.3GB grab, so the SOURCE camera just emits NDI with the burn. Apply the device-default
# colour v4l2 controls (saturation=50/contrast=50) directly here (#338/#312: the old sharp
# set saturation=0/contrast=75 hurt the decode and tinted the picture; device defaults read
# clean).
CAM1_BURN_BIN="/tmp/camera-box-burn-${RUN_ID}"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  "$PROBE_BIN_DIR"/camera-box root@"$CAM1_IP":"$CAM1_BURN_BIN"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   chmod +x $CAM1_BURN_BIN; \
   i=0; while fuser -s /dev/video0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   v4l2-ctl -d /dev/video0 --set-ctrl=saturation=50,contrast=50 2>/dev/null; \
   (CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID \
     CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
     nohup $CAM1_BURN_BIN >/tmp/cbox-burn.log 2>&1 &)"
sleep 4  # let $CAMERA_NAME's NDI sender (with the burn) become discoverable

# #624/#312: the ALL_CAMBOX sweep also cuts cam2/cam3/cam4/cam5/cam6 into strih program —
# without their OWN capture-burn deployed the SAME way as cam1 above, recording-verdict's
# per-camera all_cambox_latency/contiguity blocks would honestly report null for them (no burn
# to pair against), which is NOT the real per-camera proof this sweep exists to produce. Mirror
# cam1's deploy exactly, once per box, gated on ALL_CAMBOX=1 (the default single-camera path
# never touches any of them).
#
# cam2 is a SPECIAL CASE in this loop: it is ALSO the fixed dual-QR PAINTER, so its manually
# nohup'd binary MUST carry CAMERA_BOX_NO_DISPLAY=1 (the SAME #291 opt-out rig-mode.sh uses) —
# every other camera-under-test box's binary is launched WITHOUT it (nothing else claims their
# fb0, so their normal unconditional HDMI preview is harmless). This is what lets the SEPARATE
# frame-probe painter (launched next, [3/8]) own /dev/fb0 without stopping cam2's OWN measured
# capture+NDI-emit chain. Stopping the PERMANENT painter unit (see the guarded stop command
# below, #440) is unconditionally attempted for every box in the loop — a harmless no-op on
# cam3/cam4/cam5/cam6 (unit doesn't exist there, `2>/dev/null || true` swallows it) — but is
# REQUIRED on cam2 to avoid the #328/#440 two-painters-fighting-over-fb0 bug (the permanent
# service and this loop's transient probe-featured binary must never both hold fb0/run at once).
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _cn_ip_burn in \
    "cam2=$PAINTER_IP=$BURN_CAM2_RUN_ID" \
    "cam3=$CAM3_IP=$BURN_CAM3_RUN_ID" \
    "cam4=$CAM4_IP=$BURN_CAM4_RUN_ID" \
    "cam5=$CAM5_IP=$BURN_CAM5_RUN_ID" \
    "cam6=$CAM6_IP=$BURN_CAM6_RUN_ID"; do
    _cn="${_cn_ip_burn%%=*}"; _crest="${_cn_ip_burn#*=}"; _cip="${_crest%%=*}"; _cburn="${_crest#*=}"
    echo "[2b/8] $_cn (${_cip}) — probe-featured camera-box with its OWN capture BURN (run_id=$_cburn, #624/#312 ALL_CAMBOX)"
    _cbin="/tmp/camera-box-burn-${_cn}-${RUN_ID}"
    _cnodisplay=""
    if [ "$_cn" = "cam2" ]; then _cnodisplay="CAMERA_BOX_NO_DISPLAY=1 "; fi
    sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
      "$PROBE_BIN_DIR"/camera-box root@"$_cip":"$_cbin"
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$_cip" \
      "systemctl stop cam2-painter 2>/dev/null || true; \
       systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
       chmod +x $_cbin; \
       i=0; while fuser -s /dev/video0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
       v4l2-ctl -d /dev/video0 --set-ctrl=saturation=50,contrast=50 2>/dev/null; \
       (${_cnodisplay}CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS CAMERA_BOX_BURN_RUN_ID=$_cburn \
         NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
         nohup $_cbin >/tmp/cbox-burn-${_cn}.log 2>&1 &)"
  done
  sleep 4  # let cam2/cam3/cam4/cam5/cam6's NDI senders (with their burns) become discoverable
fi

echo "[3/8] cam2 (${PAINTER_IP}) — free /dev/fb0, paint dual-QR with --paint-log ground truth"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  "$PROBE_BIN_DIR"/frame-probe root@"$PAINTER_IP":/tmp/frame-probe
# #359: `rm -f /tmp/painter.csv` FIRST. frame-probe writes the ground-truth CSV ONLY on its
# clean --duration-secs self-exit, so a painter killed early (or a prior aborted run) leaves a
# STALE /tmp/painter.csv in place. Removing it before launch guarantees the file we later pull
# is THIS run's — never a silently-trusted leftover (run 354002's 14.9h-offset fake FAIL).
#
# #312: under ALL_CAMBOX=1, [2b/8] above ALREADY redeployed cam2's camera-box as a
# probe-featured, no-display, OWN-burn binary — it keeps capture+NDI-emit alive (#291) and
# never touches /dev/fb0, so fb0 is free for the painter WITHOUT touching camera-box again
# here. The plain single-camera path (ALL_CAMBOX unset) never runs [2b/8], so it still needs
# the ORIGINAL stop-camera-box step here (cam2 is not a measured node in that mode).
#
# #312 item 2 (PR A): under ALL_CAMBOX=1 the painter ALSO emits the CONTINUOUS QPSK audio
# marker for the WHOLE run duration — ONE markers.csv for the entire sweep (fuses per-camera
# A/V-sync into this same run/verdict, #624 deliverable 4). Never gated to a camera window —
# attribution happens entirely on the VIDEO side, per `--switch-schedule` window
# (recording-verdict's all_cambox_av_sync). Same collection mechanism the AV_RESTART_GATE mode
# already uses below (`--audio-marker`/`--marker-log`, #420/#421) — reused, not reinvented. The
# plain single-camera path (ALL_CAMBOX unset) is UNCHANGED: no marker flags, no self-check.
AV_SYNC_MARKER_DEVICE="${AV_SYNC_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}"
AV_SYNC_MARKER_CADENCE="${AV_SYNC_MARKER_CADENCE:-180}"
_cam2_marker_flags=""
_cam2_marker_check=""
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  _cam2_prep="rm -f /tmp/painter.csv /tmp/av-markers.csv;"
  _cam2_marker_flags="--audio-marker --audio-marker-device $AV_SYNC_MARKER_DEVICE \
      --audio-marker-cadence-ticks $AV_SYNC_MARKER_CADENCE --marker-log /tmp/av-markers.csv"
  # #420/#431 fail-loud self-check (same mechanism AV_RESTART_GATE uses, scripts/lib/audio-marker-check.sh):
  # confirms the marker is RUNNING *and* the log is actually GROWING before the run proceeds — a
  # broken marker setup is caught in ~20s here, not discovered after a wasted 30-min sweep.
  _cam2_marker_check="$(audio_marker_check_cmds "$AV_SYNC_MARKER_DEVICE" \
    'pkill -x frame-probe 2>/dev/null || true' \
    'all-cambox continuous marker, #312 item 2' '/tmp/av-markers.csv')"
else
  _cam2_prep="systemctl stop cam2-painter 2>/dev/null || true; systemctl stop camera-box; pkill -x camera-box 2>/dev/null; rm -f /tmp/painter.csv;"
fi
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "$_cam2_prep \
   i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (nohup /tmp/frame-probe --paint-only --dual-qr --wall-clock --paint-log /tmp/painter.csv \
      --paint-fps $PAINT_FPS --qr-size $QR_SIZE --run-id $RUN_ID --duration-secs $((DURATION+60)) \
      $_cam2_marker_flags \
      >/tmp/painter.log 2>&1 &); \
   $_cam2_marker_check"
PAINTER_LAUNCH_EPOCH="$(date +%s)"  # #359: when the painter's --duration-secs lifetime started
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
#   strih records '$STRIH_PROG_SOURCE' whose source-name is the resolved SOURCE camera's own
#   NDI name ($CAMERA_SOURCE, e.g. "CAM1 (usb)"/"CAM3 (usb)"/"CAM4 (usb)" — #24).
#   stream records 'NDI 2ME PGM' whose source-name is strih's program NDI name ($STRIH_OUT).
STRIH_UPSTREAM_NDI="${STRIH_UPSTREAM_NDI:-$CAMERA_SOURCE}"  # the SOURCE camera's own NDI name (#24)
TEST_PRELOAD="${TEST_PRELOAD:-1}"                       # #183: force preload=1 for the test
# #358: delivery-verify gate — set stream box's 'NDI 2ME PGM' to GENLOCK_TEST_LATENCY_MS (1000ms)
# for the test window, then restore prod A/V-align (450ms) on teardown. The live FIFO audit log
# read-back (latency_ms= field) confirms the FIFO actually HELD 1000ms (the #292 silent-non-apply
# gate). Supervisor runs the live rig-validate step; this ships the code + pure-function tests.
GENLOCK_TEST_LATENCY_MS="${GENLOCK_TEST_LATENCY_MS:-1000}"
GENLOCK_TEST_LATENCY_SOURCE="${GENLOCK_TEST_LATENCY_SOURCE:-$STREAM_PROG_SOURCE}"
echo "[4/8] OBS prod-scene routing — strih program='$STRIH_PROG_SCENE' ($CAMERA_NAME via $STRIH_PROG_SOURCE),"
echo "      stream program='$STREAM_PROG_SCENE' (strih feed via '$STREAM_PROG_SOURCE')"
echo "      #183: forcing genlock_preload=$TEST_PRELOAD on both recorded prod inputs for the test"
echo "      #358: setting $GENLOCK_TEST_LATENCY_SOURCE genlock_latency_ms_src=$GENLOCK_TEST_LATENCY_MS for delivery-verify"
STRIH_OUT=$(python3 "$HERE/obs_phase2.py" prod-scene --host "$STRIH" \
  --program-scene "$STRIH_PROG_SCENE" \
  --upstream "$STRIH_UPSTREAM_NDI" --test-preload "$TEST_PRELOAD")
# stream's upstream is strih's program NDI name (just printed above) — force preload=1 on the
# stream box's 'NDI 2ME PGM' input (the prod copy of 31 the issue calls out).
# #343: record the ALREADY-ACTIVE prod scene 'PRO' (NDI 2ME PGM already warm) — NO --ensure-source.
# A fresh ephemeral scene + --ensure-source would cold-activate the 450ms-FIFO NDI 2ME PGM on the
# graphics thread → SetCurrentProgramScene blocks >60s (#328 timeout, proof can't run). With program
# already on PRO, prod_scene's `curr_prog == target` branch skips the switch entirely → no hang.
# PRECONDITION: the stream box runs on its prod 'PRO' scene in normal operation; if it has DRIFTED
# off PRO, prod_scene takes the bounded switch and fails LOUD at the #328 timeout (no silent hang).
STREAM_OUT=$(python3 "$HERE/obs_phase2.py" prod-scene --host "$STREAM" \
  --program-scene "$STREAM_PROG_SCENE" \
  --upstream "$STRIH_OUT" --test-preload "$TEST_PRELOAD" \
  --test-latency-source "$GENLOCK_TEST_LATENCY_SOURCE" \
  --test-latency-ms "$GENLOCK_TEST_LATENCY_MS")
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

echo "[4c/8] #365 frozen-camera gate — every strih raw NDI input must be updating (not a frozen feed)"
# Precondition: the Multiview projector must be OPEN on strih (#276) so all NDI inputs render.
# Hash each raw NDI camera input via GetSourceScreenshot; feed the per-camera timeline to the
# Rust binary (frozen-camera-gate) which returns FROZEN names on exit 1 / PASS on exit 0.
# Threshold, sources, and sample count are env-overridable so operators can tune without a code
# change. Default: 8 samples at 1s cadence, FROZEN if > 3 consecutive hashes identical.
# The Rust binary lives alongside the probe tools in $PROBE_BIN_DIR; the Python harness discovers
# it via FROZEN_GATE_BIN or PROBE_BIN_DIR.
FROZEN_GATE_BIN="${FROZEN_GATE_BIN:-$PROBE_BIN_DIR/frozen-camera-gate}"
export FROZEN_GATE_BIN
# #365/#399 BOUNDED RETRY — the gate must not race the harness's OWN [3/8] cam2 restart: that
# restart drops cam2's NDI sender, and a strih input bound to that box (the #399 drifted mapping
# binds 'NDI cam3' to CAM2) HOLDS the last frame while DistroAV reconnects — sampled seconds
# later the gate reads 8 identical hashes and false-aborts the run (run 7020001, twice,
# 2026-07-02). A reconnect race clears within a retry; a GENUINELY frozen camera fails every
# attempt (~2.5 min total) — the per-attempt verdict is untouched, so the gate is NOT weakened.
FROZEN_CAM_ATTEMPTS="${FROZEN_CAM_ATTEMPTS:-4}"
FROZEN_CAM_RETRY_SLEEP="${FROZEN_CAM_RETRY_SLEEP:-30}"
# #365/#399 EXCLUDE the painter box's own feed — in TEST mode cam2's display is OFF until the
# painter starts, so a strih input bound to cam2's NDI sender (the #399 drifted 'NDI cam3' →
# 'CAM2 (usb)') shows the HDMI-splitter self-view: BY DESIGN static at gate time. That is not a
# broadcast signal — sampling it false-aborts DETERMINISTICALLY (run 7020001: identical hash
# across 4 retry attempts while cam2's emitter ran healthy at 60 fps). Derive the source list
# live: keep every default input EXCEPT those bound to FROZEN_CAM_EXCLUDE_SENDER. An explicit
# FROZEN_CAM_SOURCES env still overrides everything (operator escape hatch, unchanged). #312:
# widened the checked input set to all six canonical NDI-input slots (fleet growth 4→6, #451,
# and cam2 itself is no longer skipped a priori — it is excluded here ONLY if its sender name
# actually matches FROZEN_CAM_EXCLUDE_SENDER at gate time, same as every other input).
FROZEN_CAM_EXCLUDE_SENDER="${FROZEN_CAM_EXCLUDE_SENDER:-CAM2 (usb)}"
if [ -z "${FROZEN_CAM_SOURCES:-}" ]; then
  FROZEN_CAM_SOURCES="$(python3 - "$STRIH" "$FROZEN_CAM_EXCLUDE_SENDER" "$HERE/obs_phase2.py" <<'PYEOF'
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("o", sys.argv[3])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
host, exclude = sys.argv[1], sys.argv[2]
ws = m._conn(host, os.environ.get("OBS_PASSWORD", ""))
keep = []
for inp in ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4", "NDI cam5", "NDI cam6"]:
    try:
        s = m._rpc(ws, "GetInputSettings", {"inputName": inp}).get("inputSettings", {})
        sender = s.get("ndi_source_name", "")
    except Exception:
        sender = ""
    if exclude and exclude in sender:
        print(f"    [frozen-camera-gate] excluding {inp!r} (bound to {sender!r} — the painter box's self-feed, static by design pre-paint)", file=sys.stderr)
    else:
        keep.append(inp)
ws.close()
print(",".join(keep))
PYEOF
)"
  echo "    [frozen-camera-gate] derived sources: ${FROZEN_CAM_SOURCES} (excluded any bound to '${FROZEN_CAM_EXCLUDE_SENDER}')"
fi
frozen_ok=0
for frozen_attempt in $(seq 1 "$FROZEN_CAM_ATTEMPTS"); do
  if python3 "$HERE/frozen-camera-gate.py" \
      --host "$STRIH" \
      --threshold "${FROZEN_CAM_THRESHOLD:-3}" \
      --samples   "${FROZEN_CAM_SAMPLES:-8}" \
      --sources   "${FROZEN_CAM_SOURCES:-NDI cam1,NDI cam2,NDI cam3,NDI cam4,NDI cam5,NDI cam6}"; then
    frozen_ok=1
    break
  fi
  if [ "$frozen_attempt" -lt "$FROZEN_CAM_ATTEMPTS" ]; then
    echo "    [frozen-camera-gate] attempt ${frozen_attempt}/${FROZEN_CAM_ATTEMPTS} FROZEN — settling ${FROZEN_CAM_RETRY_SLEEP}s for the post-[3/8] NDI reconnect, then re-sampling"
    sleep "$FROZEN_CAM_RETRY_SLEEP"
  fi
done
if [ "$frozen_ok" -ne 1 ]; then
  echo "    [frozen-camera-gate] FROZEN on every one of ${FROZEN_CAM_ATTEMPTS} attempts — a camera is GENUINELY stuck; aborting (#365)"
  exit 1
fi

echo "[4d/8] #405/#406/#462 render-budget gate — with burns ON + Multiview open, ALL THREE boxes MUST hold the render frame budget (strih 30fps, stream 30fps, imag 60fps — Topology v2, #459: strih's 60fps IMAG role moved to imag-nb, which now carries its own render-budget floor too)"
# The 2026-07-02 regression (found when strih was STILL the 60fps LED-wall IMAG box, pre-#459): a
# measurement burn left ON dropped strih RENDER 60->27fps (36ms > 16.6ms/60fps budget) while the
# encoder outputFps stayed a DUPLICATED 60 (green) — and NOTHING
# caught it, because the delivery verdict checks burn-id contiguity (which stays contiguous at
# 27fps) not render fps. This gate snapshots OBS WS GetStats deltas on BOTH boxes in the exact
# recording state (burns ON from [4b/8], Multiview open) and FAILS FAST if either misses its
# frame-time budget — so a choked pipeline can never be recorded and then "pass" on delivery.
# STRICT (strict-test mandate): no warn-only, no override. A fail = fix the root cause (an
# expensive burn is #404's full-frame readback; a render regression is a real regression).
# The decision lives ONLY in the Rust render-budget-gate bin (render_budget::classify) — single
# source of truth, no threshold duplicated in python.
RENDER_GATE_BIN="${RENDER_GATE_BIN:-$PROBE_BIN_DIR/render-budget-gate}"
export RENDER_GATE_BIN
# Pass the same OBS_PASSWORD to BOTH boxes: stream currently has no WS auth (empty works), but if it
# is ever set to match strih (per the shared-password note) an empty here would fail auth → false abort.
if ! OBS_PASSWORD_STRIH="${OBS_PASSWORD:-}" OBS_PASSWORD_STREAM="${OBS_PASSWORD:-}" \
    python3 "$HERE/render-budget-gate.py" \
      --box "strih=${STRIH}:${RENDER_TARGET_FPS_STRIH:-30}" \
      --box "stream=${STREAM}:${RENDER_TARGET_FPS_STREAM:-30}" \
      --box "imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}" \
      --window-s "${RENDER_GATE_WINDOW_S:-6}"; then
  echo "    [render-budget-gate] a box missed the render frame budget with burns ON — aborting BEFORE recording (#405)." >&2
  echo "    A recording made in this state would judder (encoder duplicates frames) yet pass delivery-contiguity." >&2
  echo "    Root cause is almost always the expensive measurement burn (#404 full-frame readback) or a render regression." >&2
  echo "    Clear burns with scripts/rig-mode.sh event; see EPIC #406." >&2
  exit 1
fi

# ============================================================================
# #137 OPTIONAL MODE — OBS-restart A/V-sync SURVIVAL gate. OFF by default.
# ============================================================================
# Reopened issue #137: an OBS stop->start SOMETIMES drifts the video<->audio offset by
# ~200-300ms and destroys lipsync ("niekedy sa nam rozsišiel o 200-300ms uplne
# zlikvidovalo lipsync"), with nothing automatic to catch it. This DISTINCT MODE
# measures the #188 A/V offset (cam2 QPSK audio marker vs its dual-QR video tick, via
# `recording-verdict --av-sync`) BEFORE and AFTER a real OBS stop->start on the stream
# box, then runs the strict av-restart-sync-gate (single source of truth:
# camera_box::av_restart_sync::classify) on the two measurements — FAIL if the offset
# drifted beyond tolerance.
#
# It is a MODE, not a sub-step: it reuses the rig set up by [0/8]..[4d/8] (cam2's QR
# reaches the stream program, burns/frozen/render gates passed) and then runs its OWN
# record->restart->record->gate flow INSTEAD of the normal [5/8]..[8/8] zero-loss
# record+verdict, and EXITS. It MUST live here — before [5/8] and before the
# VERDICT_ON_STREAM `exit 0` further down — or it would be unreachable on the default
# VERDICT_ON_STREAM=1 path (which exits inside [8/8] long before the end of the file).
#
# OFF by default (mirrors --colour-gate/COLOUR_GATE's env-flag shape) so a normal
# zero-loss run is COMPLETELY UNCHANGED — set AV_RESTART_GATE=1 to opt in. The OBS
# restart itself is an OPERATOR/SUPERVISOR ACTION: this script PRINTS the instruction
# and BLOCKS until the restart is confirmed — it NEVER stops/starts OBS itself (#137
# scope: this PR ships the gate + wiring; the live two-recording rig proof with a REAL
# OBS restart is supervisor-driven).
#
# Because scp/exec to the Windows boxes is DENIED to bash (same #208/#193 constraint as
# the main verdict below), the recording-verdict --av-sync DECODE step is EMITTED as a
# plan for the win-stream-snv MCP holder to run — exactly like the [8/8a-c] per-box
# decode-in-place plan. Only the final av-restart-sync-gate decision (on the two small
# JSONs, once pulled back to dev1) runs directly here.
if [ "${AV_RESTART_GATE:-0}" = "1" ]; then
  GATE=0  # this mode owns the exit code (the normal [8/8] GATE assignment is skipped)
  AV_RESTART_RECORD_SECS="${AV_RESTART_RECORD_SECS:-150}"
  # Validate the one env var used in bash arithmetic ($((AV_RESTART_RECORD_SECS + 30)))
  # so a non-integer override fails with a CLEAR diagnostic instead of an opaque
  # `set -euo pipefail` arithmetic error mid-ssh-command (a plausible operator typo,
  # e.g. "150.0" or "2m", mirroring other duration-style env vars in this tooling).
  case "$AV_RESTART_RECORD_SECS" in
    '' | *[!0-9]*)
      echo "ERROR: #137 AV_RESTART_RECORD_SECS='$AV_RESTART_RECORD_SECS' must be a positive integer (seconds)." >&2
      exit 2
      ;;
  esac
  AV_RESTART_MARKER_DEVICE="${AV_RESTART_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}"
  AV_RESTART_MARKER_CADENCE="${AV_RESTART_MARKER_CADENCE:-180}"
  AV_RESTART_AUDIO_TRACK="${AV_RESTART_AUDIO_TRACK:-0}"
  AV_RESTART_TOLERANCE_MS="${AV_RESTART_TOLERANCE_MS:-50}"
  AV_RESTART_GATE_BIN="${AV_RESTART_GATE_BIN:-$PROBE_BIN_DIR/av-restart-sync-gate}"
  VERDICT_EXE_WIN="${VERDICT_EXE_WIN:-C:\\camera-box\\recording-verdict.exe}"
  OUT_DIR_WIN="${OUT_DIR_WIN:-C:\\camera-box\\verdict-out}"

  # $1 = label ("before" | "after"). Records cam2's QPSK-marked stream program for
  # AV_RESTART_RECORD_SECS, pulls the cam2 marker CSV to dev1 (cam2 is Linux — scp
  # works, unlike the Windows boxes), and EMITS the win-stream-snv decode plan for
  # this recording (bash cannot scp/exec on Windows — #208/#193). The [3/8] plain
  # dual-QR painter (no audio marker) is replaced here by the audio-marker painter the
  # A/V measurement needs — the earlier launch is wasted in this mode, harmless.
  #
  # #421 (same risk class as #420): a dropped/mistyped --audio-marker flag or a busy ALSA device
  # would otherwise let this launch silently proceed with NO marker audio, producing an
  # unmeasured before/after pair that av-restart-sync-gate could either fall closed to Unknown on
  # (safe) or, worse, false-pair on spurious CRC-4 program-noise decodes. The shared
  # audio_marker_check_cmds self-check (scripts/lib/audio-marker-check.sh) is appended INSIDE the
  # SAME ssh command, right after backgrounding the painter and BEFORE this function starts OBS
  # recording — a silent marker makes the remote command exit 1, which (no `|| true` guards this
  # ssh call) aborts the whole AV_RESTART_GATE run under `set -euo pipefail` at the top of this
  # script, never wasting a recording on an unmeasured run.
  #
  # #431: RUNNING alone is not proof of emission (the continuous-feed emitter keeps the ALSA PCM
  # RUNNING on its silence carrier even if the painter tick stalls and zero markers ever fire) — so
  # the 4th arg below passes the SAME /tmp/av-restart-markers.csv path the launch above writes,
  # which also gates on that log's row count actually growing.
  av_restart_record_and_emit_plan() {
    local label="$1"
    local marker_csv="$OUTDIR/av-restart-${label}-${RUN_ID}.csv"
    echo "    [av-restart-sync/$label] cam2 painter: dual-QR + QPSK audio marker on $AV_RESTART_MARKER_DEVICE"
    # Free /dev/fb0 the SAME way [3/8] does — stop cam2-painter AND camera-box (which can
    # also hold fb0 via its --display path), kill any leftover frame-probe, then WAIT (bounded)
    # for fb0 to actually release before relaunching. A partial copy that skipped the
    # camera-box stop / the fuser wait would race the framebuffer and silently corrupt the
    # QR + marker paint, degrading the very measurement this gate depends on.
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
      "systemctl stop cam2-painter 2>/dev/null || true; \
       systemctl stop camera-box; pkill -x camera-box 2>/dev/null; pkill -x frame-probe 2>/dev/null || true; \
       rm -f /tmp/av-restart-markers.csv; \
       i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
       (nohup /tmp/frame-probe --paint-only --dual-qr --paint-fps $PAINT_FPS --qr-size $QR_SIZE \
          --duration-secs $((AV_RESTART_RECORD_SECS + 30)) --audio-marker \
          --audio-marker-device $AV_RESTART_MARKER_DEVICE \
          --audio-marker-cadence-ticks $AV_RESTART_MARKER_CADENCE \
          --marker-log /tmp/av-restart-markers.csv >/tmp/av-restart-painter.log 2>&1 &); \
       $(audio_marker_check_cmds "$AV_RESTART_MARKER_DEVICE" 'pkill -x frame-probe 2>/dev/null || true' "cadence=$AV_RESTART_MARKER_CADENCE ticks, label=$label" "/tmp/av-restart-markers.csv")"
    sleep 3
    # #627: record --action start self-verifies liveness (see the [5/8] call site above) and
    # aborts loud under set -e if the output is dead-on-arrival.
    python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start
    sleep "$AV_RESTART_RECORD_SECS"
    local stream_host_path
    # Log a WARNING with context on a non-zero StopRecord (mirrors the [7/8] pattern) —
    # never swallow it silently: an empty stream_host_path then flows into the emitted
    # decode plan, and a bare `|| true` would leave no trace to debug from (comprehensive-
    # logging.md / script-failure-policy.md).
    stream_host_path=$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop) \
      || { echo "WARNING: [av-restart-sync/$label] stream StopRecord returned non-zero (continuing; recording may already be stopped)" >&2; stream_host_path=""; }
    sleep 10  # let frame-probe self-exit + flush its marker CSV
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
      "pkill -x frame-probe 2>/dev/null; true"
    sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
      root@"$PAINTER_IP":/tmp/av-restart-markers.csv "$marker_csv" || \
      { echo "ERROR: could not fetch $label QPSK marker log" >&2; exit 1; }
    echo "    [av-restart-sync/$label] recording: ${stream_host_path:-<unknown>} (on stream box)"
    echo "    [av-restart-sync/$label] marker log pulled to dev1: $marker_csv"
    local rec_win="${stream_host_path:-<the ${label} recording, as it lives on the stream box>}"
    local marker_win="$OUT_DIR_WIN\\av-restart-${label}-${RUN_ID}.csv"
    local partial_win="$OUT_DIR_WIN\\av-restart-${label}-${RUN_ID}.json"
    echo "    --- win-stream-snv decode plan for '$label' (bash cannot scp/exec on Windows) ---"
    echo "    win-stream-snv FileUpload:   $marker_csv  ->  $marker_win"
    echo "    win-stream-snv Shell (PowerShell):"
    # Emit the PowerShell decode command. Each Windows path is wrapped in PowerShell DOUBLE
    # quotes verbatim (%s) — the correct way to quote a single-backslash Windows path, the
    # SAME technique the [8/8] on-box planner uses. NEVER bash `printf %q`, which doubles
    # every backslash (`C:\x` -> `C:\\x`) and corrupts the path on the box. --av-sync writes
    # its JSON to stdout, so redirect it into the partial the FileDownload below pulls back.
    # shellcheck disable=SC2016  # $env:RUST_LOG is a PowerShell var for the Windows box — must NOT expand in bash
    printf '      $env:RUST_LOG="info"; & "%s" "--av-sync" "%s" "--av-marker-log" "%s" "--av-audio-track" "%s" > "%s"\n' \
      "$VERDICT_EXE_WIN" "$rec_win" "$marker_win" "$AV_RESTART_AUDIO_TRACK" "$partial_win"
    echo "    win-stream-snv FileDownload: $partial_win  ->  $OUTDIR/av-restart-${label}-${RUN_ID}.json"
  }

  echo "[R1/R3] #137 baseline A/V-sync measurement (BEFORE the OBS restart)"
  av_restart_record_and_emit_plan before

  echo "[R2/R3] #137 OBS restart — OPERATOR/SUPERVISOR ACTION (this script does NOT execute it)"
  echo "    Manually STOP then START OBS on stream ($STREAM) now — the real-world restart #137"
  echo "    gates on. This script never stops/starts OBS itself; the restart is always driven by"
  echo "    the operator/supervisor holding the rig, never automated inside recording-e2e.sh."
  # The restart MUST be confirmed before the 'after' measurement — otherwise before/after
  # are near-identical and the gate reports a SPURIOUS PASS, masking the very regression
  # #137 exists to catch. So: an interactive TTY blocks on ENTER; a non-interactive run
  # (agent/CI/nohup/piped) REQUIRES AV_RESTART_CONFIRM=1 (an explicit assertion that the
  # operator already restarted OBS out-of-band) and otherwise ABORTS LOUD — it NEVER
  # silently proceeds to a meaningless 'after' recording.
  if [ "${AV_RESTART_CONFIRM:-}" = "1" ]; then
    echo "    AV_RESTART_CONFIRM=1 — trusting that the operator/supervisor has ALREADY restarted OBS."
  elif [ -t 0 ]; then
    read -r -p "    Press ENTER once OBS on stream has been manually restarted... " _
  else
    echo "ERROR: #137 AV_RESTART_GATE cannot confirm the OBS restart happened — stdin is not a TTY" >&2
    echo "       and AV_RESTART_CONFIRM=1 is not set. Refusing to take the 'after' measurement" >&2
    echo "       without a real restart (that would spuriously PASS and mask the #137 regression)." >&2
    echo "       Restart OBS on stream manually, then re-run interactively OR set AV_RESTART_CONFIRM=1." >&2
    exit 1
  fi

  echo "[R3/R3] #137 post-restart A/V-sync measurement (AFTER the OBS restart) + gate"
  av_restart_record_and_emit_plan after

  BEFORE_JSON="$OUTDIR/av-restart-before-${RUN_ID}.json"
  AFTER_JSON="$OUTDIR/av-restart-after-${RUN_ID}.json"
  echo "    Once both partial JSONs are pulled back to dev1 (see the win-stream-snv plans above),"
  echo "    run the strict gate (single source of truth: camera_box::av_restart_sync::classify):"
  printf '      %q %q %q %s\n' "$AV_RESTART_GATE_BIN" "$BEFORE_JSON" "$AFTER_JSON" "$AV_RESTART_TOLERANCE_MS"
  if [ -f "$BEFORE_JSON" ] && [ -f "$AFTER_JSON" ]; then
    # The gate binary PRINTS its own accurate verdict (PASS / FAIL / UNKNOWN + reasons) to
    # stdout; capture its exit code and surface an HONEST wrapper line per code. Do NOT
    # overstate an UNKNOWN (an untrustworthy measurement — not proof of drift) or a
    # bad/missing-JSON error (exit 2) as a confirmed A/V drift (no-overstatement).
    av_rc=0
    "$AV_RESTART_GATE_BIN" "$BEFORE_JSON" "$AFTER_JSON" "$AV_RESTART_TOLERANCE_MS" || av_rc=$?
    case "$av_rc" in
      0)
        echo "    [av-restart-sync-gate] PASS — A/V offset held across the OBS restart within ${AV_RESTART_TOLERANCE_MS}ms"
        ;;
      2)
        echo "ERROR: #137 av-restart-sync-gate could NOT evaluate (bad/missing measurement JSON — see its error above); NOT a PASS." >&2
        GATE=1
        ;;
      *)
        echo "ERROR: #137 av-restart-sync-gate did NOT pass — see its verdict above (FAIL = A/V offset drifted beyond ${AV_RESTART_TOLERANCE_MS}ms and lipsync would break; UNKNOWN = a measurement was untrustworthy, never a confirmed pass)." >&2
        GATE=1
        ;;
    esac
  else
    echo "    [av-restart-sync-gate] both partial JSONs not yet on dev1 — the win-stream-snv holder"
    echo "    must run the two decode plans above, then run the gate command printed above by hand."
  fi
  exit "$GATE"
fi

# ============================================================================
# #109 OPTIONAL MODE — ZERO-LOSS restart-survival gate. OFF by default.
# ============================================================================
# #105 Step 4: the zero-loss + stable-latency proof is not trustworthy until it survives BOTH
# an OBS restart and a PC restart of strih+stream. `recording-verdict --json` already computes
# the run's single trustworthy binary delivery verdict (#186) — `overall_pass` +
# `full_chain.zero_loss`/`real_drops`/`burn_unreadable`. This mode runs the SAME per-box
# decode-in-place + merge pipeline [8/8a]..[8/8c] use (recording-verdict-on-strih.sh /
# recording-verdict-on-stream.sh) TWICE — once as a BEFORE baseline, once as an AFTER
# measurement bracketing a real restart — then gates the pair via the strict Tier-0 kernel
# (single source of truth: camera_box::zero_loss_restart_survival::classify) run by the thin
# `zero-loss-restart-gate` CLI. PASS iff BOTH measurements are a genuine zero-loss
# recording-verdict PASS; FAIL if either is not; UNKNOWN (fail-closed) on any
# internally-inconsistent JSON — never a false PASS.
#
# It is a MODE, not a sub-step — like #137's AV_RESTART_GATE it reuses the rig set up by
# [0/8]..[4d/8], runs its OWN record->restart->record->gate flow INSTEAD of the normal
# [5/8]..[8/8] single-pass verdict, and EXITS. Must live here (before [5/8] / the
# VERDICT_ON_STREAM=1 early exit inside [8/8]) for the same reachability reason as #137.
#
# OFF by default (mirrors AV_RESTART_GATE) so a normal zero-loss run is COMPLETELY UNCHANGED.
# Set ZERO_LOSS_RESTART_GATE=1 to opt in.
#
# SCOPE: this gate covers the #186 DELIVERY signal only (frame-drop zero-loss) — the exact
# "final test" #105 Step 4 names. Colour (#364) and A/V-sync (#137, its OWN restart-survival
# gate above) have their own dedicated gates; this step's per-box extract omits --colour-gate
# and the painter/cam1-capture-stats sidecars to keep the restart-survival pair minimal and
# fast — add them by hand (mirroring [8/8a]/[8/8b]) if a fuller pair is wanted.
#
# The restart(s) themselves are OPERATOR/SUPERVISOR ACTIONS: this script PRINTS the exact
# steps — an OBS restart (stop/start via scripts/launch-obs-genlock.sh) and, per #109's "PC
# restart" requirement, a host reboot of strih+stream — and BLOCKS until confirmed; it NEVER
# stops/starts OBS or reboots a host itself (#109 scope: this PR ships the gate + wiring; the
# live restart-survival rig proof, including the approval-gated PC reboot, is
# supervisor-driven — see the #109 2026-07-02 comment: rebooting THIS dev rig is
# standing-approved work the supervisor performs directly, this unattended script simply never
# triggers it on its own).
#
# ONE invocation brackets ONE restart window (whatever the operator performs inside it — an
# OBS restart, a PC reboot, or both back-to-back all count as "the restart" for this pair).
# #109's full 3-pass protocol (baseline -> post-OBS-restart -> post-PC-restart) runs this SAME
# opt-in step TWICE in sequence: once with an OBS restart in the confirmation window, once more
# — reusing this run's just-produced 'after' JSON as the second pass's baseline via
# ZERO_LOSS_RESTART_BEFORE_JSON (skips re-recording an already-clean baseline) — with a PC
# reboot in the second window. Never re-implemented per-restart-type in this script.
if [ "${ZERO_LOSS_RESTART_GATE:-0}" = "1" ]; then
  GATE=0  # this mode owns the exit code (the normal [8/8] GATE assignment is skipped)
  ZERO_LOSS_RESTART_RECORD_SECS="${ZERO_LOSS_RESTART_RECORD_SECS:-360}"
  # Validate the one env var used in bash arithmetic-adjacent sleeps, mirroring #137's
  # AV_RESTART_RECORD_SECS guard — a non-integer override fails with a CLEAR diagnostic
  # instead of an opaque `set -euo pipefail` error mid-run.
  case "$ZERO_LOSS_RESTART_RECORD_SECS" in
    '' | *[!0-9]*)
      echo "ERROR: #109 ZERO_LOSS_RESTART_RECORD_SECS='$ZERO_LOSS_RESTART_RECORD_SECS' must be a positive integer (seconds)." >&2
      exit 2
      ;;
  esac
  # 360s clears recording-verdict's --min-secs 300 analyzed-span floor (#373) with margin for
  # start/stop settling — the SAME floor the normal [8/8c] merge uses below.
  ZERO_LOSS_RESTART_GATE_BIN="${ZERO_LOSS_RESTART_GATE_BIN:-$PROBE_BIN_DIR/zero-loss-restart-gate}"
  VERDICT_EXE_WIN="${VERDICT_EXE_WIN:-C:\\camera-box\\recording-verdict.exe}"
  OUT_DIR_WIN="${OUT_DIR_WIN:-C:\\camera-box\\verdict-out}"
  ZL_BURN_STRIH_RUN_ID="${BURN_STRIH_RUN_ID:-911002}"
  ZL_BURN_STREAM_RUN_ID="${BURN_STREAM_RUN_ID:-911004}"

  # $1 = label ("before" | "after"). Records strih+stream for ZERO_LOSS_RESTART_RECORD_SECS,
  # then emits the SAME per-box decode-in-place + merge plan [8/8a]..[8/8c] use (bash cannot
  # scp/exec on Windows — #208/#193), writing this pass's zero-loss verdict JSON to
  # $OUTDIR/zero-loss-restart-<label>-<RUN_ID>.json instead of the normal $REPORT_JSON path.
  zero_loss_record_and_emit_plan() {
    local label="$1"
    echo "    [zero-loss-restart/$label] recording ${ZERO_LOSS_RESTART_RECORD_SECS}s on strih+stream (program = certified prod scene)"
    # #627: record --action start self-verifies liveness (see the [5/8] call site above) and
    # aborts loud under set -e if the output is dead-on-arrival.
    python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action start
    python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start
    sleep "$ZERO_LOSS_RESTART_RECORD_SECS"
    local strih_host_path stream_host_path
    strih_host_path=$(python3 "$HERE/obs_phase2.py" record --host "$STRIH" --action stop) \
      || { echo "WARNING: [zero-loss-restart/$label] strih StopRecord returned non-zero (continuing; recording may already be stopped)" >&2; strih_host_path=""; }
    stream_host_path=$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop) \
      || { echo "WARNING: [zero-loss-restart/$label] stream StopRecord returned non-zero (continuing; recording may already be stopped)" >&2; stream_host_path=""; }
    echo "    [zero-loss-restart/$label] strih recording:  ${strih_host_path:-<unknown>} (on strih box)"
    echo "    [zero-loss-restart/$label] stream recording: ${stream_host_path:-<unknown>} (on stream box)"

    local strih_rec_win="${strih_host_path:-<the ${label} strih recording, as it lives on the strih box>}"
    local stream_rec_win="${stream_host_path:-<the ${label} stream recording, as it lives on the stream box>}"
    local strih_partial_win="$OUT_DIR_WIN\\zero-loss-restart-${label}-strih-partial-${RUN_ID}.json"
    local stream_partial_win="$OUT_DIR_WIN\\zero-loss-restart-${label}-stream-partial-${RUN_ID}.json"
    local strih_partial="$OUTDIR/zero-loss-restart-${label}-strih-partial-${RUN_ID}.json"
    local stream_partial="$OUTDIR/zero-loss-restart-${label}-stream-partial-${RUN_ID}.json"

    # NOTE on `.sh"` continuation style: unlike the normal [8/8a]/[8/8b] planner calls below,
    # these two calls put the first flag on the SAME line as the script path (no bare
    # `.sh" \` line-continuation right after the filename) — a deliberate style difference so
    # `harness_recording_e2e_paths.rs`'s `.find("recording-verdict-on-strih.sh\" \\")` anchor
    # keeps landing on the NORMAL [8/8a] invocation (the one it actually guards), not this
    # earlier restart-survival-mode call to the same planner.
    echo "    --- [$label 8a] extract the STRIH partial ON the strih box (win-strih), in place ---"
    "$HERE/recording-verdict-on-strih.sh" --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" \
      --strih-rec "$strih_rec_win" \
      -- --extract-partial strih --strih "$strih_rec_win" --capture-fps "$STRIH_CAPTURE_FPS" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$ZL_BURN_STRIH_RUN_ID" \
         --out "$strih_partial_win"
    echo "    pull back to dev1: $strih_partial  (win-strih FileDownload $strih_partial_win -> $strih_partial)"

    echo "    --- [$label 8b] extract the STREAM partial ON the stream box (win-stream-snv), in place ---"
    "$HERE/recording-verdict-on-stream.sh" --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" \
      --stream-rec "$stream_rec_win" \
      -- --extract-partial stream --stream "$stream_rec_win" --capture-fps "$STREAM_CAPTURE_FPS" \
         --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" \
         --cam2-run-id "$RUN_ID" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$ZL_BURN_STRIH_RUN_ID" \
         --burn-stream-run-id "$ZL_BURN_STREAM_RUN_ID" \
         --out "$stream_partial_win"
    echo "    pull back to dev1: $stream_partial  (win-stream-snv FileDownload $stream_partial_win -> $stream_partial)"

    local out_json="$OUTDIR/zero-loss-restart-${label}-${RUN_ID}.json"
    local merge_bin
    merge_bin="$(cd "$PROBE_BIN_DIR" && pwd)/recording-verdict"
    echo "    --- [$label 8c] MERGE the two small partials ON dev1 -> the '$label' zero-loss verdict JSON ---"
    printf '      %q --merge-partials %q --merge-partials %q --min-secs 300 --capture-fps %q --strih-emit-fps %q --stream-capture-fps %q --cam2-run-id %q --burn-cam1-run-id %q --burn-strih-run-id %q --burn-stream-run-id %q --json %q\n' \
      "$merge_bin" "strih=$strih_partial" "stream=$stream_partial" "$STRIH_CAPTURE_FPS" \
      "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "$RUN_ID" \
      "$BURN_CAM1_RUN_ID" "$ZL_BURN_STRIH_RUN_ID" "$ZL_BURN_STREAM_RUN_ID" "$out_json"
    echo "    -> once pulled back + merged, writes the '$label' zero-loss verdict JSON: $out_json"
  }

  echo "[Z1/Z3] #109 baseline zero-loss measurement (BEFORE the restart)"
  if [ -n "${ZERO_LOSS_RESTART_BEFORE_JSON:-}" ] && [ -f "${ZERO_LOSS_RESTART_BEFORE_JSON}" ]; then
    echo "    ZERO_LOSS_RESTART_BEFORE_JSON=$ZERO_LOSS_RESTART_BEFORE_JSON — reusing an already-measured"
    echo "    baseline (e.g. a previous pass's 'after' JSON) instead of re-recording."
    BEFORE_JSON="$ZERO_LOSS_RESTART_BEFORE_JSON"
  else
    zero_loss_record_and_emit_plan before
    BEFORE_JSON="$OUTDIR/zero-loss-restart-before-${RUN_ID}.json"
  fi

  echo "[Z2/Z3] #109 restart — OPERATOR/SUPERVISOR ACTION (this script does NOT execute it)"
  echo "    Perform the restart under test now:"
  echo "      OBS restart: stop then start OBS on strih AND stream (scripts/launch-obs-genlock.sh),"
  echo "      PC restart:  reboot the strih/stream host(s) (approval-gated — get the user's explicit"
  echo "                   go-ahead first; this dev rig's reboot is standing-approved WORK, never"
  echo "                   auto-executed by this unattended script) — then relaunch OBS the same way."
  echo "    After either/both, verify re-lock from primary sources BEFORE continuing: dantesync LOCK"
  echo "    (scripts/dantesync-gate.sh log, not timedatectl), genlock render-tick ENABLED, NDI"
  echo "    re-bound, program on the probe scene."
  # The restart MUST be confirmed before the 'after' measurement — otherwise before/after are
  # near-identical and the gate reports a SPURIOUS PASS, masking the very regression this step
  # exists to catch. Same interactive-TTY-or-explicit-confirm shape as #137's AV_RESTART_GATE.
  if [ "${ZERO_LOSS_RESTART_CONFIRM:-}" = "1" ]; then
    echo "    ZERO_LOSS_RESTART_CONFIRM=1 — trusting that the operator/supervisor already restarted + re-verified."
  elif [ -t 0 ]; then
    read -r -p "    Press ENTER once the restart is done and re-lock is verified... " _
  else
    echo "ERROR: #109 ZERO_LOSS_RESTART_GATE cannot confirm the restart happened — stdin is not a TTY" >&2
    echo "       and ZERO_LOSS_RESTART_CONFIRM=1 is not set. Refusing to take the 'after' measurement" >&2
    echo "       without a real restart (that would spuriously PASS and mask a real #109 regression)." >&2
    echo "       Perform the restart manually, then re-run interactively OR set ZERO_LOSS_RESTART_CONFIRM=1." >&2
    exit 1
  fi

  echo "[Z3/Z3] #109 post-restart zero-loss measurement (AFTER the restart) + gate"
  zero_loss_record_and_emit_plan after
  AFTER_JSON="$OUTDIR/zero-loss-restart-after-${RUN_ID}.json"

  echo "    Once both verdict JSONs are pulled back + merged on dev1, run the strict gate"
  echo "    (single source of truth: camera_box::zero_loss_restart_survival::classify):"
  printf '      %q %q %q\n' "$ZERO_LOSS_RESTART_GATE_BIN" "$BEFORE_JSON" "$AFTER_JSON"
  if [ -f "$BEFORE_JSON" ] && [ -f "$AFTER_JSON" ]; then
    # The gate binary PRINTS its own accurate verdict (PASS / FAIL / UNKNOWN + reasons) to
    # stdout; capture its exit code and surface an HONEST wrapper line per code — never
    # overstate an UNKNOWN (an inconsistent measurement — not proof of a regression) or a
    # bad/missing-JSON error (exit 2) as a confirmed zero-loss regression (no-overstatement).
    zl_rc=0
    "$ZERO_LOSS_RESTART_GATE_BIN" "$BEFORE_JSON" "$AFTER_JSON" || zl_rc=$?
    case "$zl_rc" in
      0)
        echo "    [zero-loss-restart-gate] PASS — zero-loss delivery held across the restart"
        ;;
      2)
        echo "ERROR: #109 zero-loss-restart-gate could NOT evaluate (bad/missing measurement JSON — see its error above); NOT a PASS." >&2
        GATE=1
        ;;
      *)
        echo "ERROR: #109 zero-loss-restart-gate did NOT pass — see its verdict above (FAIL = the restart broke zero-loss delivery, or the baseline itself was never clean; UNKNOWN = a measurement was internally inconsistent, never a confirmed pass)." >&2
        GATE=1
        ;;
    esac
  else
    echo "    [zero-loss-restart-gate] both verdict JSONs not yet on dev1 — the win-strih/win-stream-snv"
    echo "    holder must run the decode+merge plans above for BOTH passes, then run the gate command"
    echo "    printed above by hand."
  fi
  exit "$GATE"
fi

echo "[5/8] StartRecord on strih + stream (program = certified prod scene) + imag (#462 — program set by rig-mode.sh test beforehand)"
# #627: `record --action start` now polls GetRecordStatus itself right after StartRecord and
# raises (nonzero exit) if the output isn't genuinely active + writing growing bytes — a
# dead-on-arrival recording (StartRecord reports success but writes 0 bytes) is caught within
# seconds instead of silently discovered only when the file is fetched at the end of the run.
# `set -euo pipefail` (top of this script) makes that nonzero exit abort this run immediately;
# no extra guard needed at this call site.
python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action start
python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start
python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action start

# #312 Phase-2 ALL-CAMBOX SWEEP (opt-in via ALL_CAMBOX=1). Instead of one steady-state hold on a
# single cambox, sequentially cut EACH active cambox into strih PROGRAM for ~SEGMENT_SECS, cycling
# the sweep until the total reaches DURATION, while the ONE continuous stream recording keeps
# running. All boxes capture the SAME cam2-painted tick through the HDMI splitter, so per-segment
# painted-tick continuity == per-box zero-loss. Each switch's wall-clock epoch-ns (the burn
# gen_ts_ns timeline — dev1 CLOCK_REALTIME, DanteSync-slaved to the painter) is captured as a
# window boundary; the switch schedule is written for recording-verdict --switch-schedule (step
# [8/8]). The strih/stream PROGRAM-OUTPUT burns (911002/911004) ride across scene switches, so the
# [4b/8] burn-ON gate is unaffected. The DEFAULT path (no ALL_CAMBOX) is the unchanged single hold.
ALL_CAMBOX="${ALL_CAMBOX:-0}"
# scene:label pairs, per the CANONICAL #399 strih NDI-input->camera mapping (set-ndi-mapping.py
# DEFAULT_MAP; scene names follow the input labels 1:1, .claude/skills/genlock/SKILL.md):
#   'Cam 5'->CAM1(.61)  'Cam 1'->CAM3(.63)  'Cam 3'->CAM4(.64)  'Cam 2'->CAM2(.62)
#   'Cam 4'->CAM5(.65)  'Cam 6'->CAM6(.66)
# #24/#399: CAM3 is back in the default — its original exclusion (#301, cam3 SSH down) closed
# 2026-06-30, and #399 later re-pinned 'Cam 1' from CAM4 to CAM3 (a prior default here still said
# 'Cam 1'->CAM4, silently mis-attributing CAM3's frames to the "CAM4" label — see
# tests/python/test_cambox_sweep_mapping.py, which cross-checks this default against DEFAULT_MAP
# so a future re-map can't desync it again).
#
# #312 CORRECTS the #333 painter exclusion: this default used to sweep ONLY cam1/cam3/cam4,
# excluding CAM2 on the theory that "while painting the monitor it does NOT capture/emit its OWN
# camera NDI" (#179). That reasoning went STALE the moment #291 (closed 2026-06-28) landed:
# cam2's camera-box daemon keeps CAPTURING + EMITTING its own NDI feed throughout a TEST run
# (only its framebuffer is freed for the separate frame-probe painter process, via
# CAMERA_BOX_NO_DISPLAY=1 — see the `[2b/8]` deploy loop below). cam2's OWN chain is therefore
# JUST AS MEASURABLE as cam1/cam3/cam4/cam5/cam6's, via the SAME digital capture-burn mechanism
# (recording-verdict.rs's CAMERA_UNDER_TEST_NODES) — this default now includes it. cam5/cam6
# (fleet growth 4→6, #451) are added the same way cam3/cam4 were by #624.
CAMBOX_SWEEP="${CAMBOX_SWEEP:-Cam 5:CAM1 Cam 1:CAM3 Cam 3:CAM4 Cam 2:CAM2 Cam 4:CAM5 Cam 6:CAM6}"
SEGMENT_SECS="${SEGMENT_SECS:-30}"
if [ "$ALL_CAMBOX" = "1" ]; then
  # #332: the all-cambox sweep now runs on the DEFAULT decode-on-stream path (VERDICT_ON_STREAM=1,
  # #193 — decode where the video lives, never pull the multi-GB recordings to dev1). The per-box
  # `--merge-partials` step consumes `--switch-schedule` (appended to MERGE_ARGS below), so the
  # per-cambox `all_cambox_continuity` is computed in the merge ON the stream box — the SAME shared
  # verdict builder the fused path uses. (The old guard that FORCED VERDICT_ON_STREAM=0 — pulling the
  # decode onto dev1 because the merge path didn't take --switch-schedule — is gone; that follow-up
  # IS this issue.) The legacy decode-on-dev1 path (VERDICT_ON_STREAM=0) still wires it via
  # VERDICT_ARGS for a box with no uploaded verdict.exe.
  echo "[6/8] ALL-CAMBOX sweep: cut each cambox into strih program ${SEGMENT_SECS}s, cycling '$CAMBOX_SWEEP' until >=${DURATION}s (run_id=$RUN_ID)"
  # Build the per-segment cut plan (scene + label), cycling the sweep to cover DURATION. Python owns
  # the colon-pair parsing (scene names contain spaces, e.g. 'Cam 5'), so bash never word-splits it.
  mapfile -t _SWEEP_PLAN < <(python3 "$HERE/switch_schedule.py" plan \
    --sweep "$CAMBOX_SWEEP" --segment-secs "$SEGMENT_SECS" --duration "$DURATION")
  if [ "${#_SWEEP_PLAN[@]}" -eq 0 ]; then
    echo "ERROR: empty cambox sweep plan from CAMBOX_SWEEP='$CAMBOX_SWEEP' — fix it, then re-run." >&2
    exit 1
  fi
  _SWITCH_START_NS=""        # window[0].start_ns — the very FIRST switch opens window 0
  _SEG_BOUNDARIES=()         # epoch-ns CLOSING each segment (the next switch, then the final stop)
  _seg_i=0
  _seg_n="${#_SWEEP_PLAN[@]}"
  for _seg in "${_SWEEP_PLAN[@]}"; do
    _scene="${_seg%%$'\t'*}"; _label="${_seg##*$'\t'}"
    # Cut strih PROGRAM to this cambox's scene; the subcommand prints the switch epoch-ns
    # (time.time_ns()) on stdout and fails loud if the scene renders black (dead cambox).
    _switch_ns="$(python3 "$HERE/obs_phase2.py" switch --host "$STRIH" --program-scene "$_scene")"
    echo "    [seg $((_seg_i+1))/${_seg_n}] $_label via '$_scene' switched at ${_switch_ns} ns"
    if [ -z "$_SWITCH_START_NS" ]; then
      _SWITCH_START_NS="$_switch_ns"          # first switch = window 0 start
    else
      _SEG_BOUNDARIES+=("$_switch_ns")        # each later switch CLOSES the previous segment
    fi
    sleep "$SEGMENT_SECS"
    _seg_i=$((_seg_i+1))
  done
  _SEG_BOUNDARIES+=("$(date +%s%N)")          # final boundary = end of the last segment (≈ stop)
  # Assemble + validate the ordered, non-overlapping schedule JSON from the captured boundaries.
  python3 "$HERE/switch_schedule.py" build \
    --sweep "$CAMBOX_SWEEP" --segment-secs "$SEGMENT_SECS" --duration "$DURATION" \
    --start-ns "$_SWITCH_START_NS" \
    --boundaries "$(IFS=,; echo "${_SEG_BOUNDARIES[*]}")" \
    > "$SWITCH_SCHEDULE_JSON"
  echo "    wrote switch schedule -> $SWITCH_SCHEDULE_JSON"
else
  # #11/#373 RECORD_PAD: the verdict trims the recording's lead/tail edge frames, so a window of
  # exactly DURATION can NEVER satisfy the --min-secs DURATION floor (run 7020001: analyzed span
  # 299.9 s < 300.0). Record DURATION + RECORD_PAD so the ANALYZED span reaches the floor.
  RECORD_PAD="${RECORD_PAD:-10}"
  echo "[6/8] steady-state run: ${DURATION}s + ${RECORD_PAD}s pad (run_id=$RUN_ID)"
  sleep "$(( DURATION + RECORD_PAD ))"
fi

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
IMAG_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action stop) \
  || echo "WARNING: imag StopRecord returned non-zero (continuing; recording may already be stopped)" >&2
echo "    strih host file:  ${STRIH_HOST_PATH:-<unknown>}"
echo "    stream host file: ${STREAM_HOST_PATH:-<unknown>}"
echo "    imag host file:   ${IMAG_HOST_PATH:-<unknown>}  (#462 — stays ON imag, decoded in place below)"
# #359: do NOT kill the painter early. frame-probe writes the ground-truth CSV ONLY on its clean
# --duration-secs self-exit (src/probe/run.rs) — the old unconditional `pkill -x frame-probe` here
# fired at ~DURATION, BEFORE the painter's DURATION+60 self-exit, so it never wrote a fresh CSV and
# a STALE leftover got pulled → a fake catastrophic FAIL (run 354002). WAIT for the painter to
# self-exit: poll until its PROCESS is gone AND a non-empty /tmp/painter.csv freshly written THIS
# run exists (remote mtime >= run start), bounded by its --duration-secs deadline + grace. A
# backstop kill only fires if it overran, so the painter can never be left holding /dev/fb0.
PAINTER_EXIT_DEADLINE=$(( PAINTER_LAUNCH_EPOCH + DURATION + 60 ))
PAINTER_WAIT_UNTIL=$(( PAINTER_EXIT_DEADLINE + 45 ))   # 45s grace past the painter self-exit
echo "    #359 waiting for the cam2 painter to self-exit + write a fresh CSV (until $(date -d "@$PAINTER_WAIT_UNTIL" '+%H:%M:%S' 2>/dev/null || echo "$PAINTER_WAIT_UNTIL"))"
while [ "$(date +%s)" -lt "$PAINTER_WAIT_UNTIL" ]; do
  if sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
       root@"$PAINTER_IP" \
       "! pgrep -x frame-probe >/dev/null 2>&1 && [ -s /tmp/painter.csv ] \
        && [ \"\$(stat -c %Y /tmp/painter.csv 2>/dev/null || echo 0)\" -ge $RUN_START_EPOCH ]" \
       2>/dev/null; then
    break
  fi
  sleep 5
done
# Backstop: if the painter somehow overran its self-exit window, stop it so it never holds /dev/fb0.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" "pkill -x frame-probe 2>/dev/null; true"
# cam1: send SIGINT (graceful) so camera-box's shutdown handler runs and writes the
# cam2→cam1 LOSS sidecar (CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt — cam1's V4L2
# capture-drop count). Give it a moment to flush, then SIGKILL any straggler.
# #626: digit-anchored pattern — see the cleanup() comment above for why a bare
# 'camera-box-burn-' self-matches the invoking remote shell's own cmdline and kills it before
# the rest of the command runs.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "pkill -INT -f 'camera-box-burn-[0-9]' 2>/dev/null; pkill -INT -x camera-box 2>/dev/null; \
   sleep 3; pkill -9 -f 'camera-box-burn-[0-9]' 2>/dev/null; pkill -9 -x camera-box 2>/dev/null; true"

# Download the cam2 painter ground-truth CSV (tick,gen_ts_ns) for the honest cam→strih
# optical assessment. (cam2→cam1 latency no longer needs it — #179 reads cam2's paint-ts
# CO-LOCATED from the cam2 QR next to the cam1 burn IN the stream recording.)
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$PAINTER_IP":/tmp/painter.csv "$PAINTER_CSV" 2>/dev/null || \
  echo "WARNING: could not fetch painter CSV (cam→strih assessment omitted)" >&2
# #312 item 2 (PR A): download the cam2 continuous QPSK A/V-sync marker log (ALL_CAMBOX=1 only —
# [3/8] never emits it on the plain single-camera path). Best-effort: a missing/failed fetch
# degrades this run to loss+latency-only (all_cambox_av_sync simply omitted), never aborts the
# zero-loss proof this far into the run.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
    root@"$PAINTER_IP":/tmp/av-markers.csv "$MARKER_CSV" 2>/dev/null || \
    echo "WARNING: could not fetch cam2 A/V-sync marker log (all_cambox_av_sync will be absent this run)" >&2
fi
# #359: FAIL LOUD if the pulled painter ground-truth is stale/missing — NEVER run the verdict
# against stale ground truth (a stale /tmp/painter.csv produced a fake 14.9h-offset catastrophic
# FAIL on run 354002). The CSV (header `tick,gen_ts_ns,flip_ts_ns`; gen_ts_ns = CLOCK_REALTIME
# epoch ns) must be present+non-empty, span ≈ DURATION (not a tiny ~40s stale file), and its
# gen_ts_ns must overlap THIS run's wall clock (not hours off from RUN_START_EPOCH). set +e is
# active here, so the gate exits non-zero EXPLICITLY (the EXIT trap still restores the rig). The
# verdict logic lives in the pure, unit-tested painter_csv_freshness() (lib sourced above).
read -r PAINTER_VERDICT PAINTER_SPAN PAINTER_OFFSET <<EOF
$(painter_csv_freshness "$PAINTER_CSV" "$RUN_START_EPOCH" "$DURATION")
EOF
if [ "$PAINTER_VERDICT" != "OK" ]; then
  echo "FATAL #359: painter ground-truth CSV not fresh ($PAINTER_VERDICT): span=${PAINTER_SPAN}s" >&2
  echo "            (expected ≈ ${DURATION}s), gen_ts offset from run start=${PAINTER_OFFSET}s." >&2
  echo "            A stale/absent ground truth yields a fake catastrophic FAIL — refusing to run" >&2
  echo "            the verdict. The painter did not write a fresh /tmp/painter.csv for this run." >&2
  exit 1
fi
echo "    #359 painter ground-truth FRESH: span=${PAINTER_SPAN}s offset=${PAINTER_OFFSET}s (OK)"
# Download the SOURCE camera's V4L2 capture-drop sidecar (the cam2→SOURCE LOSS — the camera
# leg; #24: whichever of cam1/cam3/cam4 was resolved). The verdict reports v4l2_dropped as
# cam2→SOURCE loss (NOT a painter-tick compare). Best effort: absent ⇒ the verdict simply
# omits the cam2→SOURCE loss line.
CAM1_CAPTURE_STATS="$OUTDIR/cam1-capture-stats.txt"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  root@"$CAM1_IP":/tmp/cam1-capture-stats.txt "$CAM1_CAPTURE_STATS" 2>/dev/null || \
  echo "WARNING: could not fetch $CAMERA_NAME capture-stats sidecar (cam2→$CAMERA_NAME loss omitted)" >&2
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
# #364/#377 — the per-camera COLOUR gate (one definition for BOTH the per-box and the legacy paths).
# ON by default: rig TEST mode paints the #367 colour scale (frame-probe --colour-scale), so every
# recording carries it and `--colour-gate` HARD-fails the headline on a grayscale / hue-shifted /
# white-balance-cast camera that the delivery-only verdict would pass. Set COLOUR_GATE=0 for a
# delivery-only run whose painter does NOT paint the scale (extract would otherwise abort: scale
# missing). In the per-box path each box samples its OWN recording during extract and carries the
# summary in its partial (#377 cross-box carry-through); in the legacy fused path the gate samples
# directly on dev1 where the recordings live.
CG=""
if [ "${COLOUR_GATE:-1}" = "1" ]; then CG="--colour-gate"; fi
VERDICT_ARGS=(--strih "$STRIH_REC" --min-secs 300 --capture-fps "$STRIH_CAPTURE_FPS" \
  --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" --cam2-run-id "$RUN_ID" \
  --burn-strih-run-id "$BURN_STRIH_RUN_ID" --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
  --burn-cam1-run-id "$BURN_CAM1_RUN_ID" \
  --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
if [ -n "$CG" ]; then VERDICT_ARGS+=("$CG"); fi
# #178: use `if` blocks for the optional verdict inputs (NOT a `test && append` one-liner) —
# a FALSE file-test returns non-zero and would `set -e`-abort the script before the verdict;
# an `if` condition is exempt, so an absent optional recording degrades gracefully (the
# verdict simply omits that input).
if [ -f "$STREAM_REC" ]; then VERDICT_ARGS+=(--stream "$STREAM_REC"); fi
if [ -f "$PAINTER_CSV" ]; then VERDICT_ARGS+=(--painter "$PAINTER_CSV"); fi
if [ -f "$CAM1_CAPTURE_STATS" ]; then VERDICT_ARGS+=(--cam1-capture-stats "$CAM1_CAPTURE_STATS"); fi
# #312 Phase-2: in the all-cambox sweep, feed the per-segment switch schedule so the verdict
# partitions the SINGLE continuous stream recording into per-cambox windows (by burn gen_ts_ns,
# minus the 1s transition guard) and gates each box's painted-tick continuity. Needs --stream
# (appended above); the legacy decode-on-dev1 path (VERDICT_ON_STREAM=0) consumes VERDICT_ARGS
# directly. `if`-form (NOT `[ -f ] && ...`) so a missing file never set -e-aborts (#178).
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$SWITCH_SCHEDULE_JSON" ]; then
  VERDICT_ARGS+=(--switch-schedule "$SWITCH_SCHEDULE_JSON")
  echo "    #312 all-cambox: --switch-schedule $SWITCH_SCHEDULE_JSON"
fi
# #312 item 2 (PR A): the LEGACY decode-on-dev1 fused path (VERDICT_ON_STREAM=0) has `--stream`
# pointing at a LOCAL recording, so recording-verdict can decode the marker log directly —
# `--av-marker-log` is enough, no partial/carry machinery needed. The default VERDICT_ON_STREAM=1
# path wires this differently, at [8/8b] below (the stream box extracts + carries it).
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$MARKER_CSV" ]; then
  VERDICT_ARGS+=(--av-marker-log "$MARKER_CSV")
  echo "    #312 item 2: --av-marker-log $MARKER_CSV (fused all_cambox_av_sync)"
fi
# #624 deliverable 4 / #312 item 2 PR B: the +/-20ms per-camera A/V-offset gate measures each
# camera's DEVIATION from AV_EXPECTED_MS (default 0 -- the operator's live #398 dock dialed to
# ~0 in practice), not from a hardcoded 0. Override when the dock is intentionally dialed to a
# nonzero value. Always passed (matches the CLI's own default) so the gate is explicit in the
# printed command, not silently implicit.
AV_EXPECTED_MS="${AV_EXPECTED_MS:-0}"
VERDICT_ARGS+=(--av-expected-ms "$AV_EXPECTED_MS")

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
  # $CG (--colour-gate, ON by default unless COLOUR_GATE=0) is defined once above, before
  # VERDICT_ARGS — see the #364/#377 comment there. Each box's extract samples its OWN recording's
  # colour and carries the summary in its partial; the dev1 merge applies it (strih rec → cam1,
  # stream rec → strih+stream) and FAILS the headline on any wrong colour.
  # #462: resolved HERE (before the imag deploy step below needs it too) — the SAME Linux binary
  # this dev1 process would otherwise merge with; imag-nb (x86_64 Ubuntu) runs it unmodified.
  VERDICT_BIN="$(cd "$PROBE_BIN_DIR" && pwd)/recording-verdict"
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
       $CG --out "$STRIH_PARTIAL_WIN"
  echo "    pull back to dev1: $STRIH_PARTIAL  AND the #186 pixel-proof dir $STRIH_PIXELS"
  echo "      (win-strih FileDownload $STRIH_PARTIAL_WIN -> $STRIH_PARTIAL;"
  echo "       win-strih FileDownload $STRIH_PIXELS_WIN -> $STRIH_PIXELS  [absent on a clean run])"

  # #312 item 2 (PR A): the cam2 continuous A/V-sync marker log lives on dev1 (pulled from cam2
  # above, a plain Linux scp) but the stream recording — the ONLY recording that co-locates the
  # marker's audio track with the cam2 dual-QR video — lives on the WINDOWS stream box. scp/ssh TO
  # Windows is DENIED on this rig (same constraint every other cross-box transfer here hits), so
  # this PUSHES the small marker CSV via the win-stream-snv MCP (FileUpload), mirroring the exact
  # PLAN convention `av_sync_calibrate.py`'s REMOTE PUSH plan already uses. `--extract-partial
  # stream` then decodes it ON-BOX (alongside the burns) and carries the result through the small
  # partial JSON to the dev1 merge — never the recording itself.
  AV_MARKER_WIN="${AV_MARKER_WIN:-$OUT_DIR_WIN\\av-markers-${RUN_ID}.csv}"
  _av_marker_args=""
  if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$MARKER_CSV" ]; then
    echo "    --- [8/8b-pre] PUSH the cam2 A/V-sync marker log to the stream box (win-stream-snv, scp-to-Windows denied) ---"
    echo "      win-stream-snv FileUpload $MARKER_CSV -> $AV_MARKER_WIN"
    _av_marker_args="--av-marker-log $AV_MARKER_WIN"
  fi

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
       $_av_marker_args \
       $CG --out "$STREAM_PARTIAL_WIN"
  echo "    pull back to dev1: $STREAM_PARTIAL  AND the #186 pixel-proof dir $STREAM_PIXELS"
  echo "      (win-stream-snv FileDownload $STREAM_PARTIAL_WIN -> $STREAM_PARTIAL;"
  echo "       win-stream-snv FileDownload $STREAM_PIXELS_WIN -> $STREAM_PIXELS  [absent on a clean run])"

  # #462 (EPIC #466): extract the IMAG partial ON imag-nb — UNLIKE 8/8a/8/8b above, this step
  # ACTUALLY RUNS NOW (imag-nb is a plain Linux box reachable over ssh/scp, same access class as
  # cam1/cam2 — no win-* MCP "paste this" dance needed, per the #462 issue text). By the time this
  # returns, $IMAG_PARTIAL already exists on dev1 — ready for the merge command printed below.
  IMAG_PARTIAL="$OUTDIR/imag-partial-${RUN_ID}.json"          # pulled back to dev1 (already, by now)
  IMAG_PIXELS="$OUTDIR/imag-partial-${RUN_ID}-pixels"          # #186 pixel proofs (absent on a clean run)
  IMAG_REMOTE_OUT_DIR="${IMAG_REMOTE_OUT_DIR:-/home/newlevel/verdict-out}"
  IMAG_REMOTE_PARTIAL="$IMAG_REMOTE_OUT_DIR/imag-partial-${RUN_ID}.json"
  echo "    --- [8/8c] extract the IMAG partial ON imag-nb (${IMAG_IP}, plain ssh — #462) ---"
  # #178 resilience (same discipline as the StopRecord→verdict region): this runs under `set -e`
  # (re-enabled at the top of this VERDICT_ON_STREAM=1 branch), so an UNGUARDED failure here (imag
  # unreachable, a stale/missing deployed binary, a transient ssh hiccup) would set -e-abort the
  # WHOLE script — including the strih/stream plan the operator still needs to run below. `|| {
  # WARNING; }` degrades gracefully instead: the imag leg is skipped, $IMAG_PARTIAL stays absent,
  # and the merge command below (guarded by `if [ -f "$IMAG_PARTIAL" ]`) simply omits it.
  if [ -n "${IMAG_HOST_PATH:-}" ]; then
    "$HERE/recording-verdict-on-imag.sh" \
      --verdict-bin "$VERDICT_BIN" --out-dir "$IMAG_REMOTE_OUT_DIR" --local-out-dir "$OUTDIR" \
      --imag-rec "$IMAG_HOST_PATH" \
      -- --extract-partial imag --imag "$IMAG_HOST_PATH" --imag-capture-fps "$IMAG_CAPTURE_FPS" \
         --out "$IMAG_REMOTE_PARTIAL" \
    && echo "    pulled back to dev1: $IMAG_PARTIAL  (+ the #186 pixel-proof dir $IMAG_PIXELS, if any)" \
    || echo "WARNING: #462 recording-verdict-on-imag.sh failed (imag unreachable / stale binary / ssh hiccup) — \
continuing WITHOUT the imag partial; the merge below will omit --merge-partials imag=... (cam→imag proof skipped this run)." >&2
  else
    echo "WARNING: #462 no imag recording path (StopRecord returned none) — imag partial NOT produced;" >&2
    echo "         the merge below will run WITHOUT --merge-partials imag=... (cam→imag proof skipped)." >&2
  fi

  echo "    --- [8/8d] MERGE the small partials ON dev1 (no recording on dev1) ---"
  echo "    After pulling both partials (+ their <partial>-pixels dirs) to dev1, run the merge:"
  # The merge reads ONLY the small JSONs (+ the small painter CSV / capture-stats already on dev1)
  # and produces the SAME full-chain verdict the fused path would — equivalent fields + PASS.
  MERGE_ARGS=(--merge-partials "strih=$STRIH_PARTIAL" --merge-partials "stream=$STREAM_PARTIAL" \
    --min-secs 300 --capture-fps "$STRIH_CAPTURE_FPS" \
    --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" \
    --imag-capture-fps "$IMAG_CAPTURE_FPS" --cam2-run-id "$RUN_ID" \
    --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-cam2-run-id "$BURN_CAM2_RUN_ID" \
    --burn-cam3-run-id "$BURN_CAM3_RUN_ID" --burn-cam4-run-id "$BURN_CAM4_RUN_ID" \
    --burn-cam5-run-id "$BURN_CAM5_RUN_ID" --burn-cam6-run-id "$BURN_CAM6_RUN_ID" \
    --burn-strih-run-id "$BURN_STRIH_RUN_ID" --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
    --av-expected-ms "$AV_EXPECTED_MS" \
    --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
  # #462: fold in the imag partial WHEN [8/8c] actually produced one (it runs directly above, not
  # merely printed) — `if`-form so a missing/failed imag extract never `set -e`-aborts the merge of
  # the other two nodes (#178 resilience — degrade gracefully, never abort the whole proof).
  if [ -f "$IMAG_PARTIAL" ]; then
    MERGE_ARGS+=(--merge-partials "imag=$IMAG_PARTIAL")
  fi
  # #377 — pass --colour-gate to the merge too (defense in depth): with it set, a partial that
  # LACKS its carried colour summary ERRORS LOUDLY ("re-run extract with --colour-gate") instead of
  # silently skipping a requested gate. The carried summary is honored regardless; this just catches
  # a stale/forgotten extract. Empty $CG (COLOUR_GATE=0) adds nothing.
  if [ -n "$CG" ]; then MERGE_ARGS+=("$CG"); fi
  if [ -f "$PAINTER_CSV" ]; then MERGE_ARGS+=(--painter "$PAINTER_CSV"); fi
  if [ -f "$CAM1_CAPTURE_STATS" ]; then MERGE_ARGS+=(--cam1-capture-stats "$CAM1_CAPTURE_STATS"); fi
  # #332 all-cambox: feed the per-segment switch schedule into the MERGE step so the per-cambox
  # `all_cambox_continuity` is computed ON the stream box (this default decode-on-stream path),
  # NOT forced onto dev1. The merge reads the stream partial's per-frame ticks + gen_ts and the
  # schedule's window boundaries — the SAME computation the fused/legacy path produces. `if`-form
  # (NOT `[ -f ] && ...`) so a missing schedule never set -e-aborts (#178). Needs --cam2-run-id
  # (already above, for the optical anchor) + the stream partial (the all-cambox segmentation reads
  # the SINGLE continuous stream recording's frames, carried in stream=$STREAM_PARTIAL).
  if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$SWITCH_SCHEDULE_JSON" ]; then
    MERGE_ARGS+=(--switch-schedule "$SWITCH_SCHEDULE_JSON")
    echo "    #332 all-cambox: --switch-schedule $SWITCH_SCHEDULE_JSON (per-cambox continuity in the merge, ON the stream box)"
  fi
  printf '      %q ' "$VERDICT_BIN" "${MERGE_ARGS[@]}"; echo
  echo "    The win-* MCP holder runs 8/8a + 8/8b on strih+stream (imag's 8/8c ALREADY ran above —"
  echo "    #462, plain ssh, no MCP needed), pulls the strih+stream partials (+ their <partial>-pixels"
  echo "    #186 proof dirs) to dev1, then runs the 8/8d merge above on dev1. A recording is NEVER"
  echo "    copied box-to-box nor to dev1 — only the small partial JSONs (+ the painter CSV + the"
  echo "    handful of flagged-frame PNGs) move (#208/#186/#462)."
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
