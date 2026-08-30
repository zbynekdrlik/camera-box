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
# #1179: opt-in painter display-mode override passthrough. When PAINTER_DISPLAY_MODE is set to a
# valid WxH@RR (e.g. 2560x1080@100 -- issue 881's experiment), every painter this harness LAUNCHES
# gets `--display-mode <mode>`; unset ⇒ byte-identical to today (no flag). Pure helper, embedded at
# the two painter launch sites the same way the existing optional marker-flags variable is (the
# #675 sourced-lib pattern).
# shellcheck source=scripts/lib/painter-display-mode.sh
. "$HERE/lib/painter-display-mode.sh"
# #656 prevention item 2: the "source camera capture-delivery-rate defective" preflight signal
# (pure grep pattern + message formatter — the appliance itself does the fps math, see
# src/capture_rate_health.rs). Used by the [0/8] preflight step below, before any deploy/record.
# shellcheck source=scripts/lib/capture-rate-guard.sh
. "$HERE/lib/capture-rate-guard.sh"
# #1133: the per-box capture-leg-health signal set — the appliance's own DEQUEUE-STALL / emit-gate
# SKIPPED-aggregate / kernel uvcvideo-EPROTO journal signals the #656 fps-only preflight above is
# blind to (grep patterns + windowed read builders + a pure classifier + report-only cap-1s warn).
# Consumed by the new fail-fast step further below (see its own banner).
# shellcheck source=scripts/lib/leg-health-guard.sh
. "$HERE/lib/leg-health-guard.sh"
# #1141: the head-end OPTICAL blur/shutter fail-fast — reads the source box's rough= capture
# telemetry (the #216 slow-shutter class the capture-RATE gate above is blind to). Sourced-lib
# invoked with ONE call line below (the #675 pattern), pinned to src/optical_preflight.rs by
# tests/harness_optical_preflight_1141.rs. shellcheck source=scripts/lib/optical-preflight.sh
. "$HERE/lib/optical-preflight.sh"
# #675: cleanup()'s cam1/cam3/cam4/cam2 `systemctl restart camera-box` restore used to
# be a bare `2>/dev/null; true`/`|| true` that silently swallowed a failed restart, leaving the
# rig's production camera-box.service down and undetected — poll+retry+loud-warn builder.
# shellcheck source=scripts/lib/camera-box-restart-verify.sh
. "$HERE/lib/camera-box-restart-verify.sh"
# #682: imag never had its program scene set by THIS harness -- a prior session's leftover scene
# silently decided which camera imag's leg measured. imag_scene_for_camera maps the resolved
# SOURCE camera to imag's own "Cam N" scene name (the SAME 1:1 pattern imag_scenes.py seeds).
# shellcheck source=scripts/lib/imag-scene-route.sh
. "$HERE/lib/imag-scene-route.sh"
# issue 1204: the fail-closed cross-check that imag's burn target IS the input imag renders in
# program -- used at [4a/8] after the scene route, so a burn can never land on a non-program input
# (the run 32908274448 failure). (Function names are deliberately NOT spelled here so the [4a/8]
# call site stays the sole `.find()` anchor for its wiring test, per the #832/#716 comment-collision
# lesson.)
# shellcheck source=scripts/lib/imag-burn-verify.sh
. "$HERE/lib/imag-burn-verify.sh"
# #723: SINGLE SOURCE OF TRUTH for the rig-test LEDGER — this harness's own cam2 painter launch
# registers into it too, so rig-mode.sh event's cleanup sweep can find + kill it BY PID even if a
# run is abandoned mid-flight (never just a name-pattern guess). See scripts/lib/rig-test-ledger.sh.
# shellcheck source=scripts/lib/rig-test-ledger.sh
. "$HERE/lib/rig-test-ledger.sh"
# #703: shared ssh/scp helpers for EXECUTING recording-verdict directly on strih/stream (the
# [8/8] E2E_EXECUTE_VERDICT=1 path — #701 proved ssh/scp works on this rig for these boxes).
# shellcheck source=scripts/lib/win-ssh-exec.sh
. "$HERE/lib/win-ssh-exec.sh"
# #977/#978/#979: obs64/AHK Windows-session-visibility probe + pure message parser (issue 958) --
# shared with the #979 dev1 watchdog, never a second detector for the same signal.
# shellcheck source=scripts/lib/obs-session-visibility.sh
. "$HERE/lib/obs-session-visibility.sh"
# #863: WARN-only (never `exit`) verification that the PERMANENT cam2-painter.service genuinely
# came back active + painting after cleanup() restarts it below -- a fire-and-forget restart call
# that used to be a silent no-op (the permanent painter unit was never installed, see #863).
# shellcheck source=scripts/lib/cam2-painter-restore-verify.sh
. "$HERE/lib/cam2-painter-restore-verify.sh"
# #872: ON-BOX dead-man restart for that same permanent painter. The restore above lives in
# cleanup(), the bash EXIT trap -- structurally unreachable on SIGKILL, which this workflow's
# `cancel-in-progress: true` concurrency group makes routine (any push to `dev` kills an
# in-flight hardware run). Arming a transient timer on the camera box before each stop means a
# killed run self-heals there, with no dev1 involvement.
# shellcheck source=scripts/lib/cam2-painter-deadman.sh
. "$HERE/lib/cam2-painter-deadman.sh"
# #772: the SAME on-box dead-man idea for the PRODUCTION camera-box.service (not the fb0 painter).
# recording-e2e.sh stops camera-box + launches a forever-running burn unit at four sites and
# restarts production only in cleanup() -- unreachable on a cancel-in-progress SIGKILL, so a killed
# run leaves camera-box stopped + a stray burn holding /dev/video (operator MV frozen between runs).
# Armed on the box before each stop; a killed run restores production there with no dev1 involvement.
# shellcheck source=scripts/lib/camera-box-deadman.sh
. "$HERE/lib/camera-box-deadman.sh"
# #1072: turn cleanup()'s one-shot painter restore into a bounded RETRY (fail-loud) that exposes a
# success flag (_cprr_ok) so the dead-man is disarmed ONLY when the painter genuinely came back --
# a failed restore leaves the (now periodic ~5-min) dead-man armed to self-heal.
# shellcheck source=scripts/lib/cam2-painter-restore-retry.sh
. "$HERE/lib/cam2-painter-restore-retry.sh"
# #1126: ONE final bounded genuine-painting re-check AFTER the parallel-restore wait, to prune
# cam2/painter from the failed ledger when its restore actually SUCCEEDED but the combined 30s
# restore ssh's verify window lost the race by ~50ms (a false ::error:: on a GREEN-verdict run).
# shellcheck source=scripts/lib/cam2-painter-restore-recheck.sh
. "$HERE/lib/cam2-painter-restore-recheck.sh"
# #1093: ORDERING PROOF (cam2-painter must be PAINTING before the cam-pixel probe -- cam1's picture
# IS the painter's HDMI) + RECEIVER-WEDGE ESCALATION (issue 1096: strih's DistroAV never re-locks
# after a sender bounce -> restart strih OBS once, re-check once). All logic lives in the lib; this
# script gains only the source line, one painter-up wait before cam1's probe, and two call-site
# swaps (preflight_mv_reverify -> mv_reverify_or_escalate at the deploy sites; the #675 pattern).
# shellcheck source=scripts/lib/mv-reverify-escalate.sh
. "$HERE/lib/mv-reverify-escalate.sh"
# #860: the SHARED pure optical-chain decision core + its [0/8] preflight fail-fast (the #675
# sourced-lib pattern -- the preflight is invoked with ONE line below, no anchored line edited).
# shellcheck source=scripts/lib/optical-chain-health.sh
. "$HERE/lib/optical-chain-health.sh"
# shellcheck source=scripts/lib/optical-chain-preflight.sh
. "$HERE/lib/optical-chain-preflight.sh"
# #780: the SHARED imag display-path drift gather + verdict (picom off, iGPU max-freq pin, tap conf)
# -- the SAME lib scripts/drift-guard.sh's --check-imag facet uses. Its [0/8] preflight fail-fast is
# invoked with ONE line below (the #675 sourced-lib pattern -- no anchored line edited).
# shellcheck source=scripts/lib/imag-display-path.sh
. "$HERE/lib/imag-display-path.sh"
# issue 1105: the SHARED imag kernel-cmdline ISOLATION drift gather + verdict (isolcpus/nohz_full/
# scoped-rcu_nocbs) -- the SAME issue-784 lib scripts/drift-guard.sh's --check-imag facet uses. Its
# [0/8] preflight fail-fast is invoked with ONE guarded block below (the #675 sourced-lib pattern --
# no anchored line edited).
# shellcheck source=scripts/lib/imag-cmdline-isolation.sh
. "$HERE/lib/imag-cmdline-isolation.sh"
# #878 (same family as #844/#869/#872): the PURE decision for the STARTUP self-heal below -- a
# dead harness only ever restores rig state inside cleanup() (the bash EXIT trap), which SIGKILL
# never reaches, so the leftover camera-box.service/painter/burn state strands until a human
# clears it by hand. The repair-vs-skip decision this file provides is derived from the SAME
# durable rig_e2e_marker_present() evidence rig-restore-watchdog.sh already trusts (#353) -- never a proxy
# for "is the box currently healthy", which stays entirely the [0/8] fleet preflight's own call.
# shellcheck source=scripts/lib/startup-self-heal.sh
. "$HERE/lib/startup-self-heal.sh"
# #709: imag-nb GPU VRAM headroom preflight (pure query-cmd builder + parser + message
# formatters) — a long-uptime OBS render pipeline on imag-nb can leak GPU VRAM until StartRecord's
# NVENC encoder init fails with NV_ENC_ERR_OUT_OF_MEMORY; catch it BEFORE StartRecord, not via the
# opaque #627 liveness-check failure it would otherwise produce.
# shellcheck source=scripts/lib/imag-gpu-guard.sh
. "$HERE/lib/imag-gpu-guard.sh"
# #748: pre-record measurement-audio presence preflight (pure parser + silent/audible classifier +
# operator messages + remote command builders) — the mbc measurement chain feeds the whole A/V-sync
# leg; a silent chain must FAIL the run loudly, never burn a cycle reported as a quiet av_sync
# "unknown, candidates: 0" (the run 237189640 silence that went unnoticed for a week).
# shellcheck source=scripts/lib/audio-presence-preflight.sh
. "$HERE/lib/audio-presence-preflight.sh"
# #711: Discord full-report sender (fail-open, reuses the existing bot-token #notifications
# path — never a second sender) — called once the merge verdict is genuinely computed, in the
# E2E_EXECUTE_VERDICT=1 branch of [8/8] below.
# shellcheck source=scripts/lib/e2e-discord-report.sh
. "$HERE/lib/e2e-discord-report.sh"
# #716: cam-box burn-run fps-log persistence to dev1 (the box overwrites its own
# /tmp/cbox-burn*.log on the NEXT deploy, and journald never sees it) — pure path/name builders +
# a best-effort scp-back runner, Tier-0 unit-tested (tests/harness_cbox_burn_log_persist.rs). The
# actual persist call site is tagged with its own distinct marker below (after the run).
# shellcheck source=scripts/lib/cbox-burn-log-persist.sh
. "$HERE/lib/cbox-burn-log-persist.sh"
# issue 798: emit ONE loud, greppable IMAG-LEG-VERIFIED / IMAG-LEG-NOT-VERIFIED run-log marker at
# the imag extract, naming WHY the leg was skipped when it is. Pure source-only lib (one function);
# the real call site is tagged with its own distinct marker after the extract runs below.
# shellcheck source=scripts/lib/imag-leg-marker.sh
. "$HERE/lib/imag-leg-marker.sh"
# #887: imag's zero-loss proof used to stop at OBS's own self-reported compositor stats. A
# REPORT-ONLY (never touches $GATE) independent check now compares the compositor's own
# produced-frame count (imag_produced_frame_check.py, GetStats) against the i915 kernel's
# per-CRTC CRC debugfs counter on the connector actually driving HDMI-A-1, sampled once during
# the recording window (see the lib's own header for why this is a bounded SAMPLE, not a
# whole-window capture).
# shellcheck source=scripts/lib/imag-presented-frame-check.sh
. "$HERE/lib/imag-presented-frame-check.sh"
# #712: cleanup()'s cam3/4 ALL_CAMBOX restore loop used to ssh into each box SEQUENTIALLY —
# a GH Actions cancellation kills the runner process directly, it does not wait for a 4-box
# sequential loop to finish. cambox_parallel_wait_and_report waits for all 4 backgrounded
# restores at once, bounding the loop's wall-clock by the slowest single box.
# shellcheck source=scripts/lib/cambox-parallel-restore.sh
. "$HERE/lib/cambox-parallel-restore.sh"
# #744: reset a capture card's saturation/contrast to ITS OWN --list-ctrls default (never a
# foreign literal calibrated for a different card, and never a hardcoded /dev/videoN -- USB
# grabber nodes renumber, #728). Used by the [0/8] preflight and the [2/8]/[2b/8] deploy sites
# below, which previously hardcoded a literal saturation=50 / contrast=50 pair on `/dev/video0` --
# harmless on the ShadowCast 2 (its own 0-100 default) but a dark/chroma-muted picture on the
# 0-255 Elgato 4K S cards (their own default is 128).
# shellcheck source=scripts/lib/v4l2-neutral.sh
. "$HERE/lib/v4l2-neutral.sh"
# #707: give the [2/8]/[2b/8] burn-mode systemd-run unit the SAME CPU affinity mask production's
# camera-box.service carries (issue 289's CPUAffinity= drop-in only ever applies to a unit
# literally named camera-box.service -- the transient burn unit got none at all). Derived from
# the box's own /sys/devices/system/cpu/isolated at deploy time, never a hardcoded core number.
# shellcheck source=scripts/lib/cpu-affinity-burn.sh
. "$HERE/lib/cpu-affinity-burn.sh"
# #749: sweep stale /tmp/camera-box-burn-* binaries a prior run's own cleanup() failed to remove
# (a flaky ssh round-trip, #737) BEFORE this run's own scp deploy -- each box's /tmp is a 100MB
# tmpfs that CAN fill outright (CAM1 + CAM6 both hit 100% live) and fail the deploy hard.
# shellcheck source=scripts/lib/tmp-burn-sweep.sh
. "$HERE/lib/tmp-burn-sweep.sh"
# #1086: the deliberate keepalive-bypass COLD CUT step for the all-cambox sweep (OPT-IN, OFF by
# default via COLD_CUT_BYPASS_CAM). Pure gating + a small state machine; the two sweep-loop call
# sites below are inert no-ops unless COLD_CUT_BYPASS_CAM names a sweep label, so a normal run is
# byte-for-byte unchanged.
# shellcheck source=scripts/lib/cold-cut-step.sh
. "$HERE/lib/cold-cut-step.sh"
# #707 B1 (freeze+jump discriminator, second prong): the per-cambox TCP-transport + NIC sampler.
# Pure REMOTE-COMMAND-STRING builders (no ssh at source time) — launched in [5b/8], harvested in
# [7c/8]. See the lib header for WHY (record Send-Q/retrans/NIC counters during the window so the
# next ~2.7s FREEZE's discriminator — link vs box-emit vs NDI SDK — can be READ, not guessed).
# shellcheck source=scripts/lib/transport-sampler.sh
. "$HERE/lib/transport-sampler.sh"
# issue 1202: pre-[0/8]-gate auto-align of the active cam fleet to THIS run's candidate camera-box
# build. When the fleet is uniformly on ONE stale build != candidate, deploy the candidate before
# the version-parity gate so its existing candidate-pin accept passes (no manual deploy-fleet on the
# treadmill). Mixed/unread fleets are NEVER auto-deployed — the gate keeps deciding those.
# shellcheck source=scripts/lib/camera-box-parity-align.sh
. "$HERE/lib/camera-box-parity-align.sh"
# shellcheck source=scripts/lib/frame-probe-parity-align.sh
. "$HERE/lib/frame-probe-parity-align.sh"   # #1138: pre-gate auto-align of cam2's steady-state painter to the candidate
# #758 item 1 — the fleet-wide minute-0 preflight: a named, loud, self-expiring exclusion for a
# box that's known-offline for a reason outside this harness's control (cambox-offline-ack.sh),
# plus the per-box service-active/emitter-count/stray-unit check (preflight-fleet-check.sh).
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"
# issue 1013: imag-nb's OFFLINE-ACK leg-skip note. imag is wired into the SAME cambox-offline-ack
# mechanism above (it is just another "box" name); when it is acked-offline the gate sets
# IMAG_OFFLINE_ACKED=1 (in the [0/8] reachability preflight below) and SKIPS every imag step,
# emitting this loud, greppable, report-only NOTE per skipped step so a green run never reads back
# as if the imag leg had passed (ONE full test, no partials, issue 798).
# shellcheck source=scripts/lib/imag-offline-ack.sh
. "$HERE/lib/imag-offline-ack.sh"
# shellcheck source=scripts/lib/preflight-fleet-check.sh
. "$HERE/lib/preflight-fleet-check.sh"
# #827: the CI workflow has no way to feed CAMBOX_OFFLINE_ACK for the automatic pull_request gate
# (it carries no workflow_dispatch inputs) -- so an explicit env value (a manual dispatch's
# offline_ack input, or an operator's hand-set env var) still wins outright, but an EMPTY one now
# falls back to the checked-in repo-level default file below. Computed once, here, before the
# [0/8] fleet preflight reads CAMBOX_OFFLINE_ACK.
RIG_FLEET_ACK_FILE="${RIG_FLEET_ACK_FILE:-$HERE/../rig-fleet.txt}"
CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$RIG_FLEET_ACK_FILE")"
# issue 1013: the imag-nb offline-ack gate flag + reason. imag is acked exactly like a cam box —
# `imag:<reason>` in CAMBOX_OFFLINE_ACK / rig-fleet.txt. The flag starts at 0 and is flipped to 1
# ONLY by the [0/8] reachability preflight below, when imag is BOTH acked AND genuinely unreachable
# (an acked-but-reachable imag is a STALE ack that fails there instead). Every downstream imag step
# consults IMAG_OFFLINE_ACKED to skip itself with a loud report-only note (imag_leg_skip_note).
IMAG_OFFLINE_ACKED=0
IMAG_OFFLINE_ACK_REASON="$(cambox_offline_ack_reason "imag")"
# #758 item 3 — the in-run freeze watch: polls the SAME MV-clone mechanism DURING the recording
# window (StartRecord through StopRecord) so a mid-run freeze fails the run within ~30s of onset,
# not at decode time.
# shellcheck source=scripts/lib/live-freeze-watch.sh
. "$HERE/lib/live-freeze-watch.sh"
# #882/#1232: restart-and-settle for the [1/8] imag render-health sweep -- the leading windows
# (right after a fresh OBS start) can measure a real, transient warm-up dip that is not a
# regression; a settle-adaptive PHASE (bounded by a wall budget) absorbs however many leading
# windows it actually takes, then the same strict windows as before must all pass. The pure
# decision lives here so classify() itself (src/render_budget.rs) stays untouched/strict.
# shellcheck source=scripts/lib/render-health-warmup.sh
. "$HERE/lib/render-health-warmup.sh"
# issue 1091 (issue 771 point 3): synchronous MV-fps floor preflight — read each OBS box's newest
# log's recent `multiview-audit:` window median and fail loud (only on a CONFIRMED sustained collapse) before
# the run, so the gate never wastes a ~40-min recording on a box whose Multiview render already
# collapsed. Consumes the same mv-fps-gate binary + mv_audit::gate_log the issue-1083 live watchdog
# uses; the #675 sourced-lib pattern (source + ONE call line below, no anchored line edited).
# shellcheck source=scripts/lib/mv-fps-preflight.sh
. "$HERE/lib/mv-fps-preflight.sh"
# #833: a MISSING tool on imag-nb (wmctrl, nm) must never be read as a MEASURED zero/empty
# result -- the #756 projector-count preflight and the [1/8] nm divisor-capability check both
# shell a remote helper on imag-nb; an absent helper used to be silently misread as "0 projectors"
# / "capability missing (#756 regression)" instead of "the tool is not installed" (#822 class).
# shellcheck source=scripts/lib/imag-require-remote-tool.sh
. "$HERE/lib/imag-require-remote-tool.sh"
# #882: distinguish "OBS process absent" / "port 4455 not listening" from a deeper projector-open
# failure -- the same class as #833's missing-tool check above, applied to OBS liveness itself.
# shellcheck source=scripts/lib/imag-obs-reachability.sh
. "$HERE/lib/imag-obs-reachability.sh"
# #1151: the SHARED reader of the issue-1146 `projector-vsync: present-vsync ARMED` OBS-log marker --
# the SAME lib scripts/drift-guard.sh's --check-imag facet uses. Its [0/8] preflight surface is a
# REPORT-ONLY line invoked below AFTER the Program projector is opened (the marker is emitted at
# projector open), via the $(fn) gather-snippet embed (the #675 sourced-lib pattern); NEVER fails run.
# shellcheck source=scripts/lib/obs-projector-vsync.sh
. "$HERE/lib/obs-projector-vsync.sh"
# #835: a dante-*.json file already sitting in $OUTDIR that this harness did not write is the
# artifact of a stale manual pre-fetch runbook (removed by #648, but nothing warned when someone
# still followed it) or a reused RUN_ID whose dir was never cleaned -- must announce itself, not
# lurk silently next to a gate that already fetches DanteSync status live over HTTP.
# shellcheck source=scripts/lib/stale-artifact-guard.sh
. "$HERE/lib/stale-artifact-guard.sh"
# #894: udev_camera_box_burn_unit_state_cmd/_from_output/_is_healthy/_integrity_message -- the
# post-StopRecord run-integrity assertion below (a burn unit that died mid-run must be surfaced as
# ITS OWN loud, distinctly-labeled failure, never silently indistinguishable from a genuinely
# frozen camera in recording-verdict.rs's frozen_leg).
# shellcheck source=scripts/lib/udev-camera-box.sh
. "$HERE/lib/udev-camera-box.sh"
# #895: self_heal_reset_window_journalctl_cmd/_events_from_output/_scan_message -- the
# post-StopRecord self-heal-RESET scan below (a capture_rate_selfheal USB reset firing during the
# recording must be attributed to self_heal_reset, never silently misread as frozen_leg on the
# camera).
# shellcheck source=scripts/lib/self-heal-attribution.sh
. "$HERE/lib/self-heal-attribution.sh"
# #1134: the SOURCE-camera role (the "cam1 role") is no longer hard-pinned to cam1 -- it is the
# first strih-routable member of CAMERA_ACTIVE_SET (camera_source_box, scripts/camera-set.sh), so
# retiring cam1 from the active set (its USB grabber hw-faulted -- #1110 -EPROTO, owner order
# #1130) moves the source role to the next healthy box (cam3 today) with zero edit here. CAM= (a
# one-off recording-e2e override) and CAMERA_SOURCE_BOX (the fleet-level override in camera-set.sh)
# both still win over the derivation. Bare (set -e) so an active set with no strih-routable source
# fails loudly here, never silently certifies the wrong box.
E2E_SOURCE_BOX="$(camera_source_box)"
camera_resolve "${CAM:-$E2E_SOURCE_BOX}"
# #24 item 1: this harness's SOURCE-camera role (the physical box filming cam2's monitor via
# the optical loopback + carrying the #174 render-time capture burn) is one of
# cam1/cam3/cam4/cam5/cam6/cam7 (camera_strih_route() resolves any of them — a pure FACT lookup,
# #827: retiring a camera from CAMERA_ACTIVE_SET does not remove its SOURCE route) — cam2 is
# deliberately EXCLUDED from this role: it is the fixed painter (its own monitor + /dev/fb0), and
# camera_strih_route() rejects it by design so it can never be selected as SOURCE (see that
# function's own doc for why — the device conflict with $PAINTER_IP). cam2 IS separately wired as
# a "camera under test" for the ALL-CAMBOX sweep's digital-burn contiguity check
# (recording-verdict.rs's CAMERA_UNDER_TEST_NODES) via its own dedicated scene "Cam 2"/"NDI cam2"
# and burn id, keyed off $PAINTER_IP directly in the [2b/8] deploy loop below — NEVER through
# this SOURCE-camera resolution. camera_strih_route() (camera-set.sh) fails loudly (via `set -e`,
# mirroring camera_resolve's own bare-call style above) on any unsupported CAM rather than
# silently certifying the wrong box; on success it sets
# CAMERA_STRIH_SCENE/CAMERA_STRIH_SOURCE, consumed below.
camera_strih_route "$CAMERA_NAME"
# ALL_CAMBOX=1's OWN secondary-camera deploy loop ([2b/8] below) unconditionally deploys cam2 +
# every camera in camera_active_secondary_set() (camera-set.sh, #827) at their FIXED physical
# IPs. Picking a non-default SOURCE camera at the same time is not supported at all — it would
# risk double-deploying a physical box under two different burn binaries (a real device/process
# conflict). Reject the combination loudly instead.
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ "$CAMERA_NAME" != "$E2E_SOURCE_BOX" ]; then
  echo "ERROR: CAM='$CAMERA_NAME' + ALL_CAMBOX=1 is not supported — ALL_CAMBOX's own [2b/8]" >&2
  echo "       loop already deploys cam2 + every active secondary camera at their fixed IPs" >&2
  echo "       alongside the primary; picking a non-default SOURCE camera too risks" >&2
  echo "       double-deploying the same physical box. Run CAM=<name> WITHOUT" >&2
  echo "       ALL_CAMBOX for a dedicated single-node source-camera certification (#24)." >&2
  exit 1
fi

CAM1_IP="${CAM1_IP:-$CAMERA_IP}"      # the SOURCE camera (films cam2's monitor, emits NDI w/ #174 burn); resolved via CAM=/camera_resolve above (#24) — despite the name, this is whichever camera was selected
PAINTER_IP="${PAINTER_IP:-10.77.9.62}" # cam2 — the box with the physical monitor cam1 films; #312: ALSO deployed as its OWN camera-under-test node ([2b/8] below), keyed off this same IP
# #624/#312: the OTHER camera-under-test boxes the ALL_CAMBOX sweep cuts into strih program
# (cam2's own chain + every camera in camera_active_secondary_set(), #827). Only used (deployed
# to / restored) when ALL_CAMBOX=1 — the default single-camera path never touches them. Same
# physical IPs camera-set.sh / cam-disk-guard.sh / rig-restore-watchdog.sh use.
#
# #827 (2026-07-27, binding owner directive) — REVERSIBILITY: cam5/cam6/cam7's IPs stay declared
# here as FACTS even though they are retired from CAMERA_ACTIVE_SET today (grabber cards returned
# to their owner, boxes powered off). Which of CAM3_IP..CAM7_IP actually get DEPLOYED/preflighted
# under ALL_CAMBOX=1 is decided ENTIRELY by camera_active_secondary_set() (scripts/camera-set.sh)
# — re-enabling a retired camera is adding its name back to CAMERA_ACTIVE_SET there, never
# touching this file. Re-enable procedure: cam5 back? add "cam5" to CAMERA_ACTIVE_SET in
# scripts/camera-set.sh (or export CAMERA_ACTIVE_SET="cam1 cam2 cam3 cam4 cam5" for a one-off
# run), then rerun the gate — nothing here needs to change.
CAM3_IP="${CAM3_IP:-10.77.9.63}"
CAM4_IP="${CAM4_IP:-10.77.9.64}"
CAM5_IP="${CAM5_IP:-10.77.9.65}"
CAM6_IP="${CAM6_IP:-10.77.9.66}"
CAM7_IP="${CAM7_IP:-10.77.9.67}"
# camera_secondary_ip NAME -> the fixed IP var for a non-source, non-painter camera name (facts
# mirror camera-set.sh's camera_resolve() exactly). The ONLY place a name from
# camera_active_secondary_set() is turned into an IP -- every loop below goes through this, never
# a second hand-maintained name->IP table.
camera_secondary_ip() {
  case "$1" in
    cam3) printf '%s' "$CAM3_IP" ;;
    cam4) printf '%s' "$CAM4_IP" ;;
    cam5) printf '%s' "$CAM5_IP" ;;
    cam6) printf '%s' "$CAM6_IP" ;;
    cam7) printf '%s' "$CAM7_IP" ;;
    *) echo "camera_secondary_ip: unknown secondary camera '$1'" >&2; return 1 ;;
  esac
}
STRIH=10.77.9.202
STREAM=10.77.9.204
# #462 (EPIC #466 Topology v2): imag-nb — the NEW 60fps low-latency IMAG cutter of all 6 NDI
# cameras (Linux, own recorded program). A THIRD recorded+decoded node alongside strih+stream —
# its zero-loss proof is the cam2 OPTICAL tick's own contiguity (60fps, no beat) ANDed with its
# own 911003 digital corner burn (#463) when present.
#
# #832: IMAG_IP is now DERIVED from scripts/imag-host.sh — the ONE declared imag host (mirrors
# camera-set.sh's CAMERA_ACTIVE_SET design, #827) — instead of an independent literal here.
# Swapping the rig's imag role (incumbent .182 <-> replacement .187) is a one-line change in that
# ONE file (or IMAG_HOST_ACTIVE=incumbent for a one-off run), never a hunt through this script.
# shellcheck source=scripts/imag-host.sh
. "$HERE/imag-host.sh"
# #845: imag_has_discrete_nvidia (the SAME hardware-detector setup-imag.sh/verify-imag.sh already
# use, #816) -- picks which [4e/8] headroom preflight variant applies to whichever imag box is
# active. Sourcing (not re-deriving) it here is the established reuse pattern
# (verify-imag.sh sources this same file for imag_cpu_isolation_plan/imag_has_discrete_nvidia).
# shellcheck source=scripts/setup-imag.sh
. "$HERE/setup-imag.sh"
CAM_PW=newlevel
# #703: strih/stream ssh creds for the E2E_EXECUTE_VERDICT=1 path (win-ssh-exec.sh helpers) —
# same convention as CAM_PW/IMAG_PW, per targets.md's "SSH: newlevel/newlevel" rows.
STRIH_USER="${STRIH_USER:-newlevel}"
STRIH_PW="${STRIH_PW:-newlevel}"
STREAM_USER="${STREAM_USER:-newlevel}"
STREAM_PW="${STREAM_PW:-newlevel}"
RUN_ID="${RUN_ID:-$(( (RANDOM << 16) | RANDOM ))}"
# #703: surface RUN_ID to the CI workflow (when running under GH Actions) so a downstream
# workflow step (the fail-closed structural guard) can locate THIS run's verdict JSON
# directly — /tmp/recording-e2e-${RUN_ID}/verdict-${RUN_ID}.json — without a fragile
# "most-recently-modified /tmp dir" heuristic.
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "RECORDING_E2E_RUN_ID=$RUN_ID" >> "$GITHUB_ENV"
fi
DURATION="${DURATION:-1800}"
if [ "$DURATION" -lt 300 ]; then
  echo "ERROR: DURATION=${DURATION} below the 300 s zero-loss floor (default 1800)." >&2
  exit 1
fi
# #772: the camera-box dead-man's FIRST fire must land only AFTER this run's ENTIRE window, so it
# can never restore production DURING a live measurement (the safety-critical invariant -- worst
# case is a slower recovery, never a corrupted verdict). = ceil(DURATION/60) + a generous overhead
# margin covering the worst-case PRE-record overhead (deploy + mv_reverify/frozen-gate bounded
# retries, ~12-15 min absolute worst per the :359+ accounting) so the first fire is guaranteed to
# land after StopRecord. The margin is env-tunable, but the invariant HANGS on it -- so a bad /
# too-small override is CLAMPED up to a hard floor (never trusted to be sane), never allowed to
# silently re-enable a mid-run fire. FLOOR = the worst-case pre-record overhead itself; below it the
# first fire could precede StopRecord.
CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN=15
CAMERA_BOX_DEADMAN_OVERHEAD_MIN="${CAMERA_BOX_DEADMAN_OVERHEAD_MIN:-20}"
case "$CAMERA_BOX_DEADMAN_OVERHEAD_MIN" in
  ''|*[!0-9]*)
    echo "WARNING #772: CAMERA_BOX_DEADMAN_OVERHEAD_MIN='$CAMERA_BOX_DEADMAN_OVERHEAD_MIN' is not a non-negative integer -- clamping to the safe floor ${CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN}min so the dead-man cannot fire mid-recording" >&2
    CAMERA_BOX_DEADMAN_OVERHEAD_MIN="$CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN" ;;
esac
if [ "$CAMERA_BOX_DEADMAN_OVERHEAD_MIN" -lt "$CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN" ]; then
  echo "WARNING #772: CAMERA_BOX_DEADMAN_OVERHEAD_MIN=${CAMERA_BOX_DEADMAN_OVERHEAD_MIN} below the safe floor -- raising to ${CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN}min (a smaller margin could let the first fire precede StopRecord)" >&2
  CAMERA_BOX_DEADMAN_OVERHEAD_MIN="$CAMERA_BOX_DEADMAN_OVERHEAD_FLOOR_MIN"
fi
CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN=$(( (DURATION + 59) / 60 + CAMERA_BOX_DEADMAN_OVERHEAD_MIN ))
# #747: the cam2 painter (launched below, before the recording) must stay ALIVE from its launch,
# through the pre-record warm-up/gate budget AND the whole DURATION recording, self-exiting
# (writing its ground-truth CSV, #359) only AFTER the recording is stopped. The old fixed +60s
# slack was sized BEFORE the #747 frozen-camera-gate warm-up and scene warm-up phases were inserted
# between the painter launch and the recording start; with them the painter self-exited ~47s before
# the recording ended and the last ~1.5 verdict windows went dark (windows 8-9 all-undecodable).
# Size the slack to cover the WORST-CASE pre-record budget that still records: the frozen-camera
# gate can burn up to its full attempts-times-retry-sleep (~4x45s) before it passes, plus routing/
# burn/scene-warm plus record start+stop ~40s. Used LOCK-STEP by BOTH the painter --duration-secs
# AND the PAINTER_EXIT_DEADLINE self-exit wait (a drift makes the wait give up before self-exit,
# pulling a stale CSV). The painter-CSV freshness gate has NO upper span bound (it fails only on
# span < DURATION/2), so a larger margin is safe.
# #1223: 240s stopped being enough margin -- two of three overnight E2E aborts (2026-08-29/30)
# were the painter expiring BEFORE the recording even started, because today's worst-case
# pre-record budget grew past it: the frozen-camera gate above, PLUS a new settle-wait between the
# align pins and the record step (its own budget, up to its own multi-minute ceiling), PLUS the
# render/multiview gates and the align/heal/routing steps that already ran before it, together
# exceeded 9 minutes on a degraded attempt. Every future gate added to the pre-record path only
# grows this budget further, so the slack is sized with generous headroom rather than tight to a
# single measured worst case. A companion fix sends the painter a graceful shutdown right after
# the recording is stopped (see the #359 comment below) so this larger slack never actually
# lengthens a normal run's tail -- only a genuinely slow/degraded pre-record phase consumes it.
# (Comment deliberately avoids the bracketed phase labels + FROZEN_CAM_* identifiers other
# tests string-anchor on -- the recording-e2e.sh static-anchor GOTCHA, project CLAUDE.md.)
PAINTER_PRE_RECORD_SLACK_SECS="${PAINTER_PRE_RECORD_SLACK_SECS:-1200}"
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
# #755: cam7 capture-burn run_id (fleet growth 6→7, #753), deployed ONLY under ALL_CAMBOX=1.
# Match recording-verdict's BURN_RUN_ID_CAM7 (911012) default exactly.
BURN_CAM7_RUN_ID="${BURN_CAM7_RUN_ID:-911012}"
# #827 (2026-07-27, binding owner directive) — REVERSIBILITY: BURN_CAM{5,6,7}_RUN_ID stay
# declared as FACTS even though cam5/cam6/cam7 are retired from CAMERA_ACTIVE_SET today. Whether
# they are actually DEPLOYED under ALL_CAMBOX=1 is decided entirely by
# camera_active_secondary_set() (scripts/camera-set.sh) — see that function + CAM3_IP..CAM7_IP's
# own comment above for the re-enable procedure.
# camera_secondary_burn_run_id NAME -> the reserved BURN_CAM<N>_RUN_ID for a secondary camera
# name -- mirrors camera_secondary_ip's shape, single lookup site.
camera_secondary_burn_run_id() {
  case "$1" in
    cam3) printf '%s' "$BURN_CAM3_RUN_ID" ;;
    cam4) printf '%s' "$BURN_CAM4_RUN_ID" ;;
    cam5) printf '%s' "$BURN_CAM5_RUN_ID" ;;
    cam6) printf '%s' "$BURN_CAM6_RUN_ID" ;;
    cam7) printf '%s' "$BURN_CAM7_RUN_ID" ;;
    *) echo "camera_secondary_burn_run_id: unknown secondary camera '$1'" >&2; return 1 ;;
  esac
}
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
  cam7) SRC_BURN_RUN_ID="$BURN_CAM7_RUN_ID" ;;
esac
OUTDIR="${OUTDIR:-/tmp/recording-e2e-${RUN_ID}}"
mkdir -p "$OUTDIR"
stale_dante_artifact_warn "$OUTDIR"
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

# issue 808 (bkshading epic): automated report-only check of the checklist above, via the
# bkshading-relay running on the SOURCE cambox (USB-PTP/gphoto2 -> the camera BODY, see
# .claude/rules/bkshading.md). REPORT-ONLY by design (owner M3 decision on issue 808) -- an
# unreachable relay or an absent camera (the shading camera is ONE portable unit, cabled to
# only one box at a time) is a quiet skip, never an abort; a genuinely slow shutter is a loud
# WARNING, never a hard gate. Never fails the run (bkshading_preflight_report always returns 0).
# shellcheck source=scripts/lib/bkshading-preflight.sh
. "$HERE/lib/bkshading-preflight.sh"
bkshading_preflight_report "$CAMERA_NAME" "$CAM1_IP"

# issue 808 (bkshading epic): PAUSE bkshading-relay on the two MEASUREMENT-CRITICAL camboxes
# (the SOURCE camera + cam2/painter) for the duration of THIS run -- causally proven to degrade
# capture/emit on both (cam1 Cam Link 58.6 vs 60.0 fps stop/start isolation; cam2 dual-QR window
# quality correlation, a 3-core box already running camera-box RT + the painter). Declared BEFORE
# cleanup()'s own EXIT trap (`trap ... EXIT HUP INT TERM`) installs further below (recording-e2e-cleanup-composition.md's own
# convention) so a cleanup() fired by an EARLY abort still reads the safe pre-trap default (0 =
# do not restore) instead of an uninitialized variable. Records each box's PRIOR active state so
# cleanup() only re-starts the relay where THIS run actually found it running -- never on a box
# the operator deliberately silenced (the current interim manual mitigation on cam1/cam2).
BKSH_PAUSE_CAM1_WAS_ACTIVE=0
BKSH_PAUSE_PAINTER_WAS_ACTIVE=0
# shellcheck source=scripts/lib/bkshading-e2e-pause.sh
. "$HERE/lib/bkshading-e2e-pause.sh"
BKSH_PAUSE_CAM1_WAS_ACTIVE="$(bkshading_e2e_pause_stop "$CAMERA_NAME" "$CAM1_IP" "$CAM_PW")"
echo "    bkshading-relay pause ($CAMERA_NAME, $CAM1_IP): was-active=$BKSH_PAUSE_CAM1_WAS_ACTIVE, now stopped for the run"
BKSH_PAUSE_PAINTER_WAS_ACTIVE="$(bkshading_e2e_pause_stop cam2 "$PAINTER_IP" "$CAM_PW")"
echo "    bkshading-relay pause (cam2/painter, $PAINTER_IP): was-active=$BKSH_PAUSE_PAINTER_WAS_ACTIVE, now stopped for the run"
# review finding (issue 808): cleanup()'s own EXIT trap does not install until far below (the
# ONLY other `trap ... EXIT` statement in this file) -- the many ordinary `exit 1` sites in the
# `[0/8]` preflight gates between here and there (reachability, optical-injection-leg, DanteSync,
# version/parity, clock-offset, leg-health, and more) would otherwise leave the relay stopped
# with NO restore for the rest of THIS run. Installing a SECOND `trap ... EXIT` completely
# REPLACES the handler for that signal (standard bash semantics) -- so this TEMPORARY trap is
# automatically superseded the instant cleanup()'s own real `trap ... EXIT HUP INT TERM` installs
# later, and covers every ordinary exit in between until then. (A genuine SIGKILL of the whole harness
# stays structurally untrappable by ANY mechanism, same accepted residual risk this file already
# carries for other pre-trap state, e.g. the #878 startup self-heal comment above -- the NEXT
# run's own preflight is where that class of loss is caught, not this one.)
# ANCHOR NOTE: this temporary handler is deliberately INDENTED one space -- ten-plus
# static-anchor tests (harness_cam2_painter_coordination.rs and siblings) slice the cleanup body
# via a newline-plus-column-0 keyword search, i.e. an UNINDENTED first word here is this file's
# reserved marker for the REAL install of the cleanup handler far below. The leading space keeps
# this TEMPORARY handler out of that namespace (plain valid bash) so those extractions still land
# on the real install. Do NOT de-indent it. (Wording here also deliberately avoids the literal
# keyword-plus-function-name adjacency other tests anchor on -- the CLAUDE.md anchor gotcha.)
 trap '
  bkshading_e2e_pause_restore "$CAMERA_NAME" "$CAM1_IP" "$CAM_PW" "${BKSH_PAUSE_CAM1_WAS_ACTIVE:-0}"
  bkshading_e2e_pause_restore cam2 "$PAINTER_IP" "$CAM_PW" "${BKSH_PAUSE_PAINTER_WAS_ACTIVE:-0}"
' EXIT HUP INT TERM

echo "[0/8] reachability preflight ($CAMERA_NAME source, cam2 painter, strih, stream, imag — #462)"
for hp in "$CAMERA_NAME=$CAM1_IP" "cam2(painter)=$PAINTER_IP" "strih=$STRIH" "stream=$STREAM" "imag=$IMAG_IP"; do
  _name="${hp%%=*}"; _ip="${hp#*=}"
  # issue 1013: imag (and ONLY imag) is ackable in this loop, via the SAME cambox-offline-ack
  # mechanism the cam boxes use (#758/#827). An operator-acknowledged absent notebook
  # (imag:<reason> in CAMBOX_OFFLINE_ACK / rig-fleet.txt) SKIPS the imag leg instead of aborting;
  # source/painter/strih/stream stay unconditionally mandatory (they have no ack path by design).
  if [ "$_name" = "imag" ] && cambox_offline_ack_is_acked "imag"; then
    if ping -c1 -W2 "$_ip" >/dev/null 2>&1; then
      # acked but REACHABLE -> STALE ack (the box is back; the ack must be removed) -> fail loud.
      cambox_offline_ack_stale_message "imag" >&2; exit 1
    fi
    cambox_offline_ack_note "imag"
    IMAG_OFFLINE_ACKED=1
    echo "    skipped: imag ($_ip) — operator-acknowledged offline; the imag leg is SKIPPED this run (issue 1013)"
    continue
  fi
  if ping -c1 -W2 "$_ip" >/dev/null 2>&1; then echo "    ok: $_name ($_ip)"; else
    echo "ERROR: $_name ($_ip) UNREACHABLE from dev1 — fix route/host, then re-run." >&2; exit 1; fi
done

# #860: optical-injection-leg fail-fast. A STANDING cam2 painter (rig-mode.sh test's
# /run/rig-painter.pid, or the permanent cam2-painter.service) that a previous run's cleanup left
# DEAD poisons this run's cam2->cam1 hop (the 2026-08-14 incident: painter dead, next gate wasted a
# ~40-min recording before its verdict reported the optical hop UNAVAILABLE). Abort LOUD now.
# Narrow abort policy (painter EXPECTED-but-DEAD only, no OBS dependency) so a CI gate is never
# false-aborted; a strih-BLACK read is a WARN (the standing optical-chain watchdog owns paging).
# Plain statement (never $()/pipeline) so its `exit 1` propagates to the harness.
echo "[0/8] optical injection leg preflight — a standing dead cam2 painter must fail-fast, not waste a run (#860)"
optical_chain_preflight_assert "$PAINTER_IP" root "$CAM_PW" "$STRIH" "${OBS_PASSWORD:-}" "$HERE"

# #780: imag display-path config drift preflight — picom running (a compositor breaks the tear-free
# direct scanout), the #841 iGPU max-freq pin down, or the #779 tap conf gone are DETERMINISTIC
# config states that live BELOW every other measurement in this gate (OBS render / recording verdict
# / screenshots all end before the display path). Fail-fast HERE, at minute 0, instead of projecting
# a laggy/torn 40-min run. UNKNOWN facets (an SSH hiccup — the earlier fleet-reachability step above,
# imag included, owns genuine unreachability) only WARN; a proven DRIFT aborts. Same shared verdict
# lib the --check-imag facet runs, so the two can never diverge.
echo "[0/8] imag display-path config preflight — a compositor / idle-GPU / lost-tap drift must fail-fast, not waste a run (#780)"
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[0/8] imag display-path config preflight (#780)" "$IMAG_OFFLINE_ACK_REASON"
else
  imag_display_path_preflight_assert "$IMAG_IP" "${IMAG_USER:-newlevel}" || exit 1
fi

# issue 1105: imag kernel-cmdline ISOLATION drift preflight — the SAME shared issue-784 lib the
# --check-imag facet uses. isolcpus=/nohz_full=/scoped-rcu_nocbs on /proc/cmdline (the issue-784/842
# footgun re-appearing via a stray grub.d drop-in or hand-edit) strips CPUs from the scheduler
# load-balancing domain and piles OBS's ~119-thread pool onto one core → NDI 60→~53fps, underruns.
# Fail-fast HERE at minute 0 instead of projecting a starved ~40-min run. UNKNOWN (an SSH hiccup —
# the fleet-reachability step above, imag included, owns genuine unreachability) only WARNs; a proven
# DRIFT aborts. New imag hard-abort site → wrapped in the IMAG_OFFLINE_ACKED guard
# (.claude/rules/imag-offline-ack.md), exactly like the display-path preflight above.
echo "[0/8] imag kernel-cmdline isolation preflight — an isolcpus/nohz_full/scoped-rcu_nocbs drift must fail-fast, not starve a run (issue 1105 · issue-784 follow-up)"
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[0/8] imag kernel-cmdline isolation preflight (issue 1105)" "$IMAG_OFFLINE_ACK_REASON"
else
  imag_cmdline_isolation_preflight_assert "$IMAG_IP" "${IMAG_USER:-newlevel}" || exit 1
fi

# #977/#958: obs64/AHK Windows-session-visibility gate. A session-0 obs64 (launched via
# ssh+Invoke-CimMethod) answers OBS WebSocket, serves NDI, and writes a normal log -- so it sails
# through every OTHER [0/8] term below while being fully invisible to the operator on the console.
# The real incident sat like this for ~3.5h before the user found it manually. FAIL LOUD (never a
# silent pass) on strih OR stream being invisible, including an ssh/connectivity failure at this
# check itself (unlike #979's dev1 watchdog, which treats that case as "nothing to decide" -- a
# per-PR CI gate has the opposite correct default for a probe failure this late in the preflight).
echo "[0/8] obs64/AHK session-visibility gate — fail when strih/stream OBS (or strih AHK) is INVISIBLE to the operator (#977/#958)"
# win_ssh_run BLOCKS until the remote command exits (its own doc comment: "the CALLER must bound
# it with an outer timeout if a wedge must not hang forever") -- this step runs FIRST, before
# every other gate, so an unbounded call here could hang up to ServerAliveInterval*
# ServerAliveCountMax=600s per box. Same wrapper shape as the established [4b2/8] audio-preflight
# precedent below (`timeout` execvp()s its command directly, so it cannot invoke a shell FUNCTION
# like win_ssh_run -- route through `bash -c`, re-sourcing the lib inside that subshell).
SVG_SSH_TIMEOUT="${SVG_SSH_TIMEOUT:-30}"
_svg_strih_out="$(timeout "$SVG_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
  "$HERE/lib/win-ssh-exec.sh" "$STRIH_USER" "$STRIH_PW" "$STRIH" "$(obs_session_visibility_probe_ps 1)" 2>/dev/null || true)"
_svg_strih_msg="$(obs_session_visibility_message "$_svg_strih_out" 1)"
if [ -n "$_svg_strih_msg" ]; then
  echo "ERROR: [0/8] strih INVISIBLE: $_svg_strih_msg" >&2
  echo "       Recovery: bash scripts/launch-obs-genlock.sh --box strih --force   # paste into the win-strih MCP Shell (session 1, never ssh+CIM — issue 958)" >&2
  exit 1
fi
echo "    ok: strih obs64/AHK visible on the console (SessionId=1, window present)"
_svg_stream_out="$(timeout "$SVG_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
  "$HERE/lib/win-ssh-exec.sh" "$STREAM_USER" "$STREAM_PW" "$STREAM" "$(obs_session_visibility_probe_ps 0)" 2>/dev/null || true)"
_svg_stream_msg="$(obs_session_visibility_message "$_svg_stream_out" 0)"
if [ -n "$_svg_stream_msg" ]; then
  echo "ERROR: [0/8] stream INVISIBLE: $_svg_stream_msg" >&2
  echo "       Recovery: bash scripts/launch-obs-genlock.sh --box stream --force   # paste into the win-stream-snv MCP Shell (session 1, never ssh+CIM — issue 958)" >&2
  exit 1
fi
echo "    ok: stream obs64 visible on the console (SessionId=1, window present)"

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
# proceed to recording. The Linux cams are read over SSH; the Windows boxes are
# queried LIVE over HTTP from dantesync#47's own network status endpoint
# (http://<box>:8898/status, #648) via dantesync-gate.sh's --win-http — no win-* MCP, no human
# pre-fetch, so this gate is fully unattended. (Superseded the pre-#648 flow: this script used
# to curl each box's status to a LOCAL FILE and hand the gate --win-status FILE, which needed a
# human/agent with win-* MCP access to backfill on a fetch failure — the automatic
# pull_request-triggered CI run on dev1 has neither. dantesync-gate.sh's own
# DANTESYNC_GATE_WIN_HTTP_<NAME> env var is the offline/fixture test seam now, mirroring its
# existing DANTESYNC_GATE_LINUX_JOURNAL_<NAME> convention for the Linux nodes.)
echo "[0/8] DanteSync NTP+PTP gate — $CAMERA_NAME, cam2, strih, stream must ALL be synced+locked (#7/#8/#648)"
# Enforce grandmaster IDENTITY (was report-first per issue 834). gm_check ships report-only in
# dantesync-gate.sh (DANTESYNC_GATE_GM_ENFORCE default 0); every fleet node now holds the rig
# grandmaster 10.77.9.184 (dantesync election + PTP-interface fix v1.8.42-1.8.46), so a node
# PTP-locked to a foreign/unreadable GM now HARD-fails here (FOREIGN->20, UNKNOWN->11) instead of
# only being reported — the stream-on-a-foreign-GM false-green issue 834/1073 is about.
DANTESYNC_GATE_GM_ENFORCE=1 "$HERE/dantesync-gate.sh" \
  --bound-us "${CLOCK_GUARD_BOUND_US:-2000}" \
  --win-http-port "${WIN_DANTE_PORT:-8898}" \
  --linux "$CAMERA_NAME=$CAM1_IP cam2=$PAINTER_IP" \
  --win-http "strih=$STRIH" \
  --win-http "stream=$STREAM"

# Version-integrity precondition gate (#123) — THE OTHER hard step, alongside DanteSync. The whole
# test is worthless unless the LIVE strih+stream OBS stack is the PINNED build (a randomly-deployed /
# drifted / stock-OBS build silently produces a false result — that is #119). So before bringing up
# the rig, gather each Windows box's observed stack state and run drift-guard --compare against the
# pinned set (vendor/README.md); REFUSE (non-zero) on DRIFT (20) or UNKNOWN (11). Same Windows-box
# access pattern as the DanteSync gate above: each box's state JSON is fetched over
# its standing http.server (a helper exposes the read-only /drift-guard observed values as
# /bundle-state.json), falling back to a caller-pre-fetched file (the win-* MCP holder writes it).
# Optionally pass VERSION_GATE_MANIFEST=<BUNDLE_MANIFEST.json> to also assert the build SHAs.
echo "[0/8] version-integrity gate — live strih+stream stack MUST match the pinned set (#123/#119)"
WIN_BUNDLE_STATE_PORT="${WIN_BUNDLE_STATE_PORT:-8899}"
VERSION_STRIH_STATE="${VERSION_STRIH_STATE:-$OUTDIR/version-strih.json}"
VERSION_STREAM_STATE="${VERSION_STREAM_STATE:-$OUTDIR/version-stream.json}"
# Try to fetch each Windows box's stack-state JSON over its http.server; a failure leaves the file
# absent -> the gate reports that box UNKNOWN and refuses, unless the caller already placed a state
# file there via the win-* MCP. (The DanteSync gate above no longer uses this file-relay pattern —
# it queries strih/stream LIVE over HTTP via dantesync-gate.sh's --win-http, #648 — but this
# version-integrity gate still does; #123/#119 is unrelated, separate scope.)
# shellcheck source=scripts/lib/bundle-state-selfheal.sh
. "$HERE/lib/bundle-state-selfheal.sh"   # #817: gate-time self-heal for a dead :8899 BundleStateServer
fetch_box_state() {
  local host="$1" dest="$2" _bs_user _bs_pw
  # #817: resolve per-box ssh creds so a fetch failure can trigger the SAME session-agnostic restart
  # the dev1 issue-732 watchdog uses (schtasks /run), then re-fetch — before refusing the whole run.
  case "$host" in
    "$STREAM") _bs_user="$STREAM_USER"; _bs_pw="$STREAM_PW" ;;
    *)         _bs_user="$STRIH_USER";  _bs_pw="$STRIH_PW" ;;
  esac
  [ -s "$dest" ] && { echo "    using pre-fetched version-integrity state: $dest"; return 0; }
  if curl -fsS --max-time 30 -o "$dest" "http://${host}:${WIN_BUNDLE_STATE_PORT}/bundle-state.json" 2>/dev/null; then
    echo "    fetched version-integrity state from ${host}:${WIN_BUNDLE_STATE_PORT} -> $dest"
  else
    # #817: :8899 is not answering — self-heal (schtasks /run over ssh) + a bounded re-fetch, then an
    # HONEST one-line fault if it stays down, instead of the pre-#817 misleading version-drift note.
    bundle_state_selfheal_fetch "$host" "$dest" "$WIN_BUNDLE_STATE_PORT" "$_bs_user" "$_bs_pw" \
      && echo "    self-healed bundle-state-server on ${host}: re-fetched -> $dest"
  fi
}
fetch_box_state "$STRIH"  "$VERSION_STRIH_STATE"  || true
fetch_box_state "$STREAM" "$VERSION_STREAM_STATE" || true

# #652: disk-budget preflight WARN (never fail — informational only). The harness's own E2E test
# recordings had silently accumulated to ~500 GB on strih / 139 GB on stream (back to
# 2026-06-17, including a single 266 GB stray), invisible until the disk nearly filled (17 GB
# free). Best-effort: an unreachable bundle-state-server (the box's :8899 /record-dir-stats.json,
# same standing service fetch_box_state above already relies on, #650) just skips the check —
# this is a WARN, never a gate.
RECORDINGS_BUDGET_GB="${RECORDINGS_BUDGET_GB:-50}"
check_recordings_budget() {
  local label="$1" host="$2" stats total_gb
  stats=$(curl -fsS --max-time 30 "http://${host}:${WIN_BUNDLE_STATE_PORT}/record-dir-stats.json" 2>/dev/null) || {
    echo "    NOTE: could not fetch $label recordings-dir stats (bundle-state-server unreachable) — skipping disk-budget check" >&2
    return 0
  }
  total_gb=$(printf '%s' "$stats" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("total_bytes",0)/1e9)' 2>/dev/null) || return 0
  echo "    $label recordings dir: $(printf '%.1f' "$total_gb") GB (budget ${RECORDINGS_BUDGET_GB} GB)"
  if python3 -c "import sys; sys.exit(0 if float('$total_gb') > float('$RECORDINGS_BUDGET_GB') else 1)" 2>/dev/null; then
    echo "WARNING #652: $label's OBS recordings directory holds ~$(printf '%.1f' "$total_gb") GB of accumulated recordings (budget ${RECORDINGS_BUDGET_GB} GB) — old E2E test recordings may be piling up; see the cleanup plan this run prints at [8/8] (KEEP_RECORDINGS=1 opts out), or clean up manually via the win-* MCP." >&2
  fi
}
check_recordings_budget strih  "$STRIH"
check_recordings_budget stream "$STREAM"
# #756 — imag is SSH-reachable, so read its deployed genlock build SHA directly (the Windows boxes'
# SHAs flow in via their --win-state bundle-state JSON) and hand it to the gate's CROSS-BOX parity
# assert. Best-effort: an unreachable imag yields "" -> the parity facet stays dormant until >=2
# boxes report a SHA (opt-in rollout), never a spurious refuse. Mirrors the [4d/8] imag ssh (l.1412).
# issue 1164: when imag is acked offline (physically absent, issue 1013), SKIP the imag ssh gathers
# entirely (no ssh to $IMAG_IP) and leave IMAG_GENLOCK_SHA empty -- the gate is invoked with
# --imag-acked-offline below (instead of the imag SHA / manifest / bytes args), so an acked-absent
# imag does NOT UNKNOWN-refuse the whole run. The empty IMAG_GENLOCK_SHA also skips the .so gather
# block below (its `if [ -n "$IMAG_GENLOCK_SHA" ]` guard). Non-acked path byte-identical.
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[0/8] version-integrity gate imag facets (genlock sha + .so bytes)" "$IMAG_OFFLINE_ACK_REASON"
  IMAG_GENLOCK_SHA=""
else
IMAG_GENLOCK_SHA="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
  "${IMAG_USER:-newlevel}@$IMAG_IP" 'cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null | head -1' 2>/dev/null | tr -d '[:space:]' || true)"
fi
# #1082 -- best-effort BYTE-parity inputs for the version-integrity gate, so the [0/8] byte facet
# compares each box's DEPLOYED bytes against CI truth, not just the hand-written marker. All
# best-effort via scripts/lib/manifest-autosource.sh: any fetch/gather failure yields "" -> the arg
# is OMITTED -> the facet stays DORMANT (opt-in), never a spurious refuse (the ENFORCE flip is
# deferred, #1082). shellcheck source=scripts/lib/manifest-autosource.sh
. "$HERE/lib/manifest-autosource.sh"
VERSION_GATE_REPO="${VERSION_GATE_REPO:-zbynekdrlik/camera-box}"
# Windows FAST manifest (strih+stream share ONE build -> one manifest), keyed on strih's OWN reported
# marker SHA. The gate applies this GLOBAL --manifest to BOTH --win-state boxes (strih AND stream), and
# a manifest activates the obs_dll_sha256 byte compare AND the genlock_capability check on each box.
# ENFORCED (#1100, the #758-shape second step): the auto-source runs UNCONDITIONALLY -- the #1082 opt-in
# guard (which required BOTH boxes to ALREADY report obs_dll_sha256 + genlock_capability before sourcing
# the manifest) is REMOVED, so a box that stops reporting its deployed obs.dll sha now flips to a
# gate-blocking UNKNOWN (drift-guard compares a manifest sha vs an empty observed) instead of a silent
# skip -- every box is REQUIRED to report its bytes. Precondition verified LIVE before the flip:
# strih+stream both serve obs_dll_sha256 + distroav_dll_sha256 + genlock_capability on :8899, and the CI
# FAST manifest at the fleet marker SHA carries the same obs.dll (the #770 on-box byte gather is now
# deployed fleet-wide -- the #1067 port4455 class). The fetch stays best-effort: a gh/network failure
# (or an unresolvable marker SHA) yields "" -> the manifest is omitted for THAT run (a fetch outage is
# not a box-drift signal), never a false refuse. VERSION_GATE_MANIFEST (if pre-set) wins.
AUTO_WIN_MANIFEST="${VERSION_GATE_MANIFEST:-}"
if [ -z "$AUTO_WIN_MANIFEST" ]; then
  AUTO_WIN_MANIFEST="$(manifest_autosource_fetch "$VERSION_GATE_REPO" windows-genlock-fast.yml obs-genlock-fast-dll \
    "$(genlock_build_sha_state_read "$VERSION_STRIH_STATE")" "$OUTDIR/win-fast-manifest.json")"
fi
# imag linux .so byte gather (ssh) + its own CI manifest, keyed on imag's marker SHA. The manifest is
# fetched only when the .so gather actually returned SHAs, so a failed gather leaves the facet dormant.
AUTO_IMAG_MANIFEST=""
IMAG_SO_CSV=""
if [ -n "$IMAG_GENLOCK_SHA" ]; then
  IMAG_SO_PROBE="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_so_gather_cmd)" 2>/dev/null || true)"
  IMAG_SO_CSV="$(imag_so_bytes_csv "$IMAG_SO_PROBE")"
  [ -n "$IMAG_SO_CSV" ] && AUTO_IMAG_MANIFEST="$(manifest_autosource_fetch "$VERSION_GATE_REPO" linux-genlock.yml obs-genlock-linux-x86_64 \
    "$IMAG_GENLOCK_SHA" "$OUTDIR/imag-linux-manifest.json")"
fi
# ALWAYS pass --win-state for strih AND stream (NOT conditional on the file existing): an absent file
# is UNKNOWN -> the gate REFUSES, never a silent pass with a box's build unverified.
# issue 1164: when imag is acked offline, invoke the gate WITHOUT the imag SHA / manifest / bytes
# args and WITH --imag-acked-offline, so the gate skips the imag .so byte facet (loud SKIPPED,
# counted ok) and drops imag from the cross-box parity (which then certifies strih+stream) -- never
# an UNKNOWN-refuse on the physically-absent, acked box. The else branch is byte-identical to before.
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  "$HERE/version-integrity-gate.sh" \
    ${AUTO_WIN_MANIFEST:+--manifest "$AUTO_WIN_MANIFEST"} \
    --win-state "strih=$VERSION_STRIH_STATE" \
    --win-state "stream=$VERSION_STREAM_STATE" \
    --imag-acked-offline "$IMAG_OFFLINE_ACK_REASON"
else
"$HERE/version-integrity-gate.sh" \
  ${AUTO_WIN_MANIFEST:+--manifest "$AUTO_WIN_MANIFEST"} \
  --win-state "strih=$VERSION_STRIH_STATE" \
  --win-state "stream=$VERSION_STREAM_STATE" \
  --genlock-sha "imag=$IMAG_GENLOCK_SHA" \
  ${AUTO_IMAG_MANIFEST:+--imag-manifest "$AUTO_IMAG_MANIFEST"} \
  ${IMAG_SO_CSV:+--imag-bytes "imag=$IMAG_SO_CSV"}
fi

# dantesync fleet-wide VERSION-PARITY gate (#862) — alongside the DanteSync NTP+PTP gate (#7) and
# the version-integrity gate (#123) above. Those two measure LIVE BEHAVIOUR (offset/lock, OBS/
# DistroAV/NDI versions); neither ever checks the dantesync DAEMON's own version, so a fleet
# running a pre-#53-burst-filter dantesync (a strictly WORSE measurement instrument) passes both
# and can still silently corrupt every downstream latency/timestamp number (#836/#851's imag-nb/
# dev1 drift, discovered only by post-mortem — exactly what this gate exists to prevent). Every
# node dantesync runs on is checked: the active cam fleet (camera_active_excluding — never a
# literal range, see .claude/rules/camera-active-set.md), imag-nb, dev1 ITSELF (the box running
# this very gate — #862 point 2: the harness's own host is never exempt), and strih/stream over
# SSH exactly like every other node (#862 follow-up, 2026-07-30: the ORIGINAL bundle-state-backed
# read for strih/stream was half-wired and always UNKNOWN in practice — every node, incl.
# Windows, is now read the SAME uniform way via `dantesync --version`, no bundle-state
# involvement). An unreachable node never silently passes — it is either read, or explicitly
# excluded via the SAME CAMBOX_OFFLINE_ACK/rig-fleet.txt mechanism the fleet preflight already
# uses (#758/#827), never a silent skip.
echo "[0/8] dantesync version-parity gate — every managed node must run the pinned dantesync (#862)"
DANTESYNC_VERSION_LINUX="$CAMERA_NAME=root@$CAM1_IP cam2=root@$PAINTER_IP"
for _dv_cn in $(camera_active_excluding "$CAMERA_NAME cam2"); do
  DANTESYNC_VERSION_LINUX="$DANTESYNC_VERSION_LINUX ${_dv_cn}=root@$(camera_secondary_ip "$_dv_cn")"
done
# issue 1013: DROP imag-nb from the dantesync version pin gate when imag is acked-offline. That
# gate treats an UNREAD node as UNKNOWN and REFUSES the whole run (exit 11); its own ack-exclusion
# keys on the node name, and here imag is named "imag-nb" (not the "imag" the ack uses), so the
# exclusion would never match — dropping the entry is the correct, matching-free skip.
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[0/8] dantesync version pin gate (imag-nb dropped from the gate)" "$IMAG_OFFLINE_ACK_REASON"
else
  DANTESYNC_VERSION_LINUX="$DANTESYNC_VERSION_LINUX imag-nb=${IMAG_USER:-newlevel}@$IMAG_IP"
fi
"$HERE/dantesync-version-gate.sh" \
  --linux "$DANTESYNC_VERSION_LINUX" \
  --local dev1 \
  --win "strih=${WIN_SSH_USER:-newlevel}@$STRIH stream=${WIN_SSH_USER:-newlevel}@$STREAM"

# camera-box binary CROSS-BOX version-parity gate (issue 875) — the follow-up split from the
# dantesync version-parity gate above. Where that gate checks the dantesync DAEMON against a fixed
# PIN, this checks the continuously-deployed camera-box BINARY across the active cam fleet with a
# RELATIVE cross-box parity model (NO pin — a dev build `1.7.0-dev.NNN` has no canonical value to
# pin against; the only checkable invariant is that every active box AGREES, mirroring
# drift-guard.sh's genlock_build_sha parity engine). A single box on a stale build silently runs
# objectively different behaviour than the rest (issue 875: cam4 once lacked the publish-30p fix,
# unnoticed because dantesync happened to be uniform so a daemon-only gate stayed green). camera-box
# runs ONLY on the cam boxes, so the node list is JUST the active cam fleet (camera_active_excluding
# — never a literal range, .claude/rules/camera-active-set.md). An unreachable box is either read or
# explicitly excluded via the SAME CAMBOX_OFFLINE_ACK/rig-fleet.txt mechanism the fleet preflight and
# the version-parity gate above already use, never a silent skip.
echo "[0/8] camera-box version-parity gate — every active cam box must run the SAME camera-box build (issue 875)"
# issue 1170 (derivation generalized to cam1 by issue 1198): cam2's camera-box BINARY is
# version-gated ONLY while cam2 is a MEASURED camera — gated on the SAME set-membership check the
# [2b/8] deploy uses, currently TRUE (cam2 is in the default CAMERA_ACTIVE_SET as of issue 1198).
# WHEN cam2 sits outside the active set it is painter-only and NOT redeployed for the run (it stays
# on the fleet-deployed build, not this run's candidate) — grading it unconditionally would refuse
# every run on a spurious version mismatch against that stale build. Its PAINTER-role clock is
# gated UNCONDITIONALLY either way (the dantesync clock version gate above keeps cam2 regardless —
# that pins the run's timebase). Dropping "cam2" from CAMERA_ACTIVE_SET drops this grading too, one
# line, no other edit needed.
CAMBOX_VERSION_LINUX="$CAMERA_NAME=root@$CAM1_IP"
if camera_is_active cam2; then
  CAMBOX_VERSION_LINUX="$CAMBOX_VERSION_LINUX cam2=root@$PAINTER_IP"
fi
for _cb_cn in $(camera_active_excluding "$CAMERA_NAME cam2"); do
  CAMBOX_VERSION_LINUX="$CAMBOX_VERSION_LINUX ${_cb_cn}=root@$(camera_secondary_ip "$_cb_cn")"
done
# issue 1202: BEFORE the gate, auto-align the SAME node set to this run's candidate when the fleet
# is uniformly stale-vs-candidate (deploys the candidate to /usr/local/bin/camera-box so the gate's
# candidate-pin accept passes). Best-effort: mixed/unread fleets are never auto-deployed, and the
# gate below is the authority (it REFUSES if the fleet is not on the candidate).
cambox_parity_align_before_gate "$CAMBOX_VERSION_LINUX"
"$HERE/camera-box-version-gate.sh" \
  --linux "$CAMBOX_VERSION_LINUX" \
  --candidate-pin "$(sed -n 's/^version = "\(.*\)"$/\1/p' "$HERE/../Cargo.toml" | head -1)"

# issue 1138: pre-gate auto-align of the cam2 STEADY-STATE painter (/usr/local/bin/frame-probe,
# cam2-painter.service) to THIS run's candidate build — the frame-probe sibling of the camera-box
# parity align above. Between dev->main merges the painter is auto-deployed by NOTHING (ci.yml
# deploy-fleet is main-only), so it silently LAGS the current build (the 2026-08-29 incident: an
# uncompensated QPSK marker + a dark aux tick until a manual redeploy). This deploys the candidate
# frame-probe (the clean probe-tools CI artifact) to cam2 when stale, so pin+deploy advance together
# (orphan-PROOF), and exports FRAME_PROBE_ALIGN_CI_BIN so the [1/8] pin below verifies against the
# SAME artifact bytes. Best-effort (ALWAYS returns 0); the report-only [1/8] pin is the loud signal.
# cam2-only (frame-probe lives only on the painter box) and unconditional (cam2 is the painter
# regardless of active-set membership); honours CAMBOX_OFFLINE_ACK + the --no-main-pin soak escape.
frame_probe_parity_align_before_gate "cam2=root@$PAINTER_IP"

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

# #878 (same family as #844/#869/#872): STARTUP self-heal, BEFORE the fleet preflight below
# asserts anything about the fleet. A dead harness (SIGKILLed / GH Actions cancelled under
# full-path-e2e.yml's `cancel-in-progress: true` concurrency group -- a ROUTINE event, any push to
# `dev` kills an in-flight hardware run) only ever restores rig state inside cleanup(), the bash
# EXIT trap -- structurally unreachable on SIGKILL. The NEXT run then fails at [0/8] preflight on a
# leftover precondition (camera-box.service inactive, painter dark, a leaked genlock_burn #844)
# instead of a measurement: four consecutive runs died this way live, 2026-07-30, ten seconds
# apart on cam2/cam3/cam4 -- the ALL_CAMBOX sweep walking the fleet, not independent failures.
#
# Gated STRICTLY on rig_e2e_marker_present() (#353, scripts/lib/rig-heartbeat.sh) -- the SAME
# durable "a harness entered a test state and did not clean up" evidence rig-restore-watchdog.sh
# already trusts as its PRIMARY stranded signal. scripts/lib/startup-self-heal.sh turns that
# evidence into repair-vs-skip, never on a guess (unrecognized input skips + logs the ambiguity,
# see that file's own functions). This step is deliberately narrow: it repairs
# ONLY what THIS harness can prove it owns, and it does NOT change the [0/8] fleet preflight's own
# pass/fail policy below -- an inactive box with no marker evidence still hard-fails exactly as
# before. Whether the preflight itself should ALSO self-heal an unproven case is the OPEN #878
# question left for the user; this change does not resolve it either way. ALL_CAMBOX only, same
# scope as the fleet preflight it precedes -- the bug is specific to the sweep touching cam3/cam4.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  _shr_marker=0
  rig_e2e_marker_present && _shr_marker=1
  _shr_action="$(startup_self_heal_decision "$_shr_marker")"
  _shr_reason="$(startup_self_heal_reason "$_shr_marker")"
  echo "[0/8] startup self-heal (#878): ${_shr_action} -- ${_shr_reason}"
  if [ "$_shr_action" = "repair" ]; then
    # camera-box.service on every box THIS harness's own EXIT-trap teardown restarts -- cam1
    # (source), cam2 (painter), and every camera_active_secondary_set() member (#827, never a
    # literal cam-number range). Reuses the SAME restart+verify primitive that teardown's own
    # FINAL verify pass uses (via the startup_self_heal_cambox_verify_cmds/_painter_verify_cmds
    # wrappers, scripts/lib/startup-self-heal.sh -- see that file for why the indirection exists)
    # STANDALONE: idempotent and cheap on an already-healthy box, a genuine restart+retry attempt
    # on one the previous run left down.
    echo "    restoring camera-box.service on every fleet box (idempotent restart+verify, #675/#684)"
    timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
      "$(startup_self_heal_cambox_verify_cmds "$CAMERA_NAME (source, $CAM1_IP) STARTUP-#878")" || true
    for _shr_cn in $(camera_active_secondary_set); do
      _shr_cip="$(camera_secondary_ip "$_shr_cn")"
      timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_shr_cip" \
        "$(startup_self_heal_cambox_verify_cmds "$_shr_cn ($_shr_cip) STARTUP-#878")" || true
    done
    timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$PAINTER_IP" \
      "$(startup_self_heal_cambox_verify_cmds "cam2/painter, $PAINTER_IP STARTUP-#878")
$(startup_self_heal_painter_verify_cmds)" || true
    # #844: clear any genlock_burn leaked ON by a run that never reached cleanup(). obs_burn_filter.py
    # remove is idempotent (a no-op if already off -- the same mechanism cleanup()'s own #246/#257
    # clear-loop uses); scope derives from camera_active_excluding (#827, never a literal range).
    echo "    clearing any leaked genlock_burn on strih's active-fleet NDI inputs (idempotent, #246/#257/#844)"
    for _shr_bn in $(camera_active_excluding ""); do
      _shr_bnum="${_shr_bn#cam}"
      python3 "$HERE/obs_burn_filter.py" remove --host "$STRIH" --input "NDI cam${_shr_bnum}" 2>&1 \
        | sed "s/^/    [startup-self-heal burn-clear ${_shr_bn}] /" || true
    done
  fi
fi

# #758 item 1 — fleet-wide minute-0 preflight: a dirty/degraded rig must fail LOUDLY here, before
# the 4-minute build/arm phase, never after a 40-minute recording ("Nechapem dokedy bude trvat ze
# si ci test urobi najprv preflight checky..." — the user's binding demand, 2026-07-14). ALL_CAMBOX
# sweep ONLY — the plain single-camera path only ever touches its own SOURCE camera, already
# pinged above.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  # #827: the preflight target list is cam1 (source) + cam2 (painter) + every camera in
  # camera_active_secondary_set() (camera-set.sh) — the ONE place fleet membership is declared.
  # Re-enabling a retired camera (e.g. cam5) is adding it to CAMERA_ACTIVE_SET there; this loop
  # picks it up automatically, no change needed here.
  # #1134: label the SOURCE node with $CAMERA_NAME (the resolved source, camera_source_box), never
  # the literal cam1 -- with cam1 acked in rig-fleet.txt, labelling the resolved source IP "cam1"
  # would make the stale-ack guard fire on a healthy cam3.
  PREFLIGHT_TARGETS=("$CAMERA_NAME=$CAM1_IP" "cam2=$PAINTER_IP")
  PREFLIGHT_TARGET_NAMES="$CAMERA_NAME cam2"
  for _pf_cn in $(camera_active_secondary_set); do
    PREFLIGHT_TARGETS+=("${_pf_cn}=$(camera_secondary_ip "$_pf_cn")")
    PREFLIGHT_TARGET_NAMES="$PREFLIGHT_TARGET_NAMES $_pf_cn"
  done
  echo "[0/8] fleet preflight — ${PREFLIGHT_TARGET_NAMES} must each be reachable-or-acked and genuinely ready (#758/#827)"
  PREFLIGHT_EXCLUDED_CAMS=""
  PREFLIGHT_DANTESYNC_LINUX=""
  for _pf in "${PREFLIGHT_TARGETS[@]}"; do
    _pfbox="${_pf%%=*}"
    _pfip="${_pf#*=}"
    _pfacked=0
    if cambox_offline_ack_is_acked "$_pfbox"; then _pfacked=1; fi
    if ping -c1 -W2 "$_pfip" >/dev/null 2>&1; then
      # Reachable — but reachable is NOT the same as healthy (#827): a box whose OS/network is up
      # but whose camera-box.service never leaves e.g. "activating" (a physically-removed grabber
      # card, 2026-07-27) must still be excludable via an ack — so the health check now always
      # runs here, and cambox_offline_ack_decide (not a raw ping result) makes the call.
      #
      # Self-heal routine leftover junk from a prior run BEFORE checking (stop stray
      # camera-box-burn-* units + sweep stale /tmp binaries, #749/#758) — then assert the box is
      # genuinely ready: the sweep fixes what a mere leftover explains, this check fails loud on
      # what a leftover does NOT explain (an inactive service, a wrong emitter count, a unit that
      # SURVIVED the sweep).
      sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_pfip" \
        "$(tmp_burn_sweep_stale_units_cmds) $(tmp_burn_sweep_stale_cmds)" 2>/dev/null || true
      _pfline="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_pfip" \
        "$(preflight_fleet_check_cmds)" 2>/dev/null || true)"
      _pfverdict="$(preflight_fleet_check_verdict "$_pfline")"
      if [ -n "$_pfverdict" ]; then _pfstatus="unhealthy"; else _pfstatus="healthy"; fi
      case "$(cambox_offline_ack_decide "$_pfstatus" "$_pfacked")" in
      stale)
        cambox_offline_ack_stale_message "$_pfbox" >&2
        exit 1
        ;;
      exclude)
        # Reachable but genuinely unhealthy, and acked — proceed WITHOUT it, named + loud (never
        # a silent drop, per strict-test-mandate-no-gate-weakening).
        cambox_offline_ack_note "$_pfbox"
        echo "    excluded: ${_pfbox} (${_pfip}) reachable but unhealthy — ${_pfverdict}"
        PREFLIGHT_EXCLUDED_CAMS="${PREFLIGHT_EXCLUDED_CAMS:+$PREFLIGHT_EXCLUDED_CAMS }${_pfbox}"
        ;;
      fail)
        echo "ERROR: [preflight] FAIL: ${_pfbox} (${_pfip}): ${_pfverdict} — ssh in and investigate (systemctl status camera-box; systemctl restart camera-box if needed)." >&2
        exit 1
        ;;
      *)
        echo "    ok: ${_pfbox} (${_pfip}) — ${_pfline}"
        PREFLIGHT_DANTESYNC_LINUX="${PREFLIGHT_DANTESYNC_LINUX:+$PREFLIGHT_DANTESYNC_LINUX }${_pfbox}=${_pfip}"
        ;;
      esac
    else
      # Unreachable — acked (a NAMED, operator-acknowledged outage) → NOTE + exclude; unacked →
      # loud FAIL (never a silent drop, per strict-test-mandate-no-gate-weakening).
      case "$(cambox_offline_ack_decide unreachable "$_pfacked")" in
      exclude)
        cambox_offline_ack_note "$_pfbox"
        PREFLIGHT_EXCLUDED_CAMS="${PREFLIGHT_EXCLUDED_CAMS:+$PREFLIGHT_EXCLUDED_CAMS }${_pfbox}"
        ;;
      *)
        echo "ERROR: [preflight] FAIL: ${_pfbox} (${_pfip}): unreachable from dev1 — fix connectivity/power, or if this is a KNOWN operator-acknowledged outage set CAMBOX_OFFLINE_ACK=\"${_pfbox}:<reason>\" to proceed without it (#758)." >&2
        exit 1
        ;;
      esac
    fi
  done
  echo "    excluded (acked-offline): ${PREFLIGHT_EXCLUDED_CAMS:-none}"

  # #947: dantesync freshest-offset sanity for the active SECONDARY cameras only (cam1/cam2 are
  # already covered by the main DanteSync gate above) -- reuses the SAME dantesync-gate.sh this
  # harness already trusts. Membership is derived from camera_active_secondary_set()
  # (camera-set.sh, #827), filtered down to whatever actually came back healthy in
  # PREFLIGHT_DANTESYNC_LINUX above (so a box acked-offline above is correctly excluded too) --
  # NEVER a literal cam[3-7] range. The old `grep -oE 'cam[3-7]=...'` pattern tested the WRONG
  # thing: it filtered PREFLIGHT_DANTESYNC_LINUX (which always holds cam1/cam2, already gated
  # above) by a hardcoded number range instead of testing "is this box a SECONDARY camera" via
  # CAMERA_ACTIVE_SET. With CAMERA_ACTIVE_SET="cam1 cam2" (cam4 retired, #947) the secondary set
  # is EMPTY, so the old `if [ -n "$PREFLIGHT_DANTESYNC_LINUX" ]` guard passed (cam1/cam2 are
  # always in there) while the grep filter yielded nothing -- dantesync-gate.sh correctly refused
  # a zero-node --linux ("no nodes to gate"), failing the whole preflight even though cam1+cam2
  # were both already gated clean above (run 30761247629). Deriving membership from
  # camera_active_secondary_set() means this never needs editing again when CAMERA_ACTIVE_SET
  # changes -- re-enabling a retired camera just flows through automatically.
  PREFLIGHT_DANTESYNC_SECONDARY=""
  PREFLIGHT_DANTESYNC_SECONDARY_NAMES=""
  # Resolve the secondary membership ONCE -- it is a pure function of CAMERA_ACTIVE_SET and cannot
  # change mid-loop, so re-invoking it per candidate would just re-parse the same set in a fresh
  # subshell each iteration (the sibling cleanup() loops resolve it once, as their loop head, for
  # the same reason -- do NOT repeat their loop-head text here, it is a pinned static anchor).
  _pfdsl_secondary=" $(camera_active_secondary_set) "
  for _pfdsl in $PREFLIGHT_DANTESYNC_LINUX; do
    _pfdslbox="${_pfdsl%%=*}"
    case "$_pfdsl_secondary" in
      *" ${_pfdslbox} "*)
        PREFLIGHT_DANTESYNC_SECONDARY="${PREFLIGHT_DANTESYNC_SECONDARY:+$PREFLIGHT_DANTESYNC_SECONDARY }${_pfdsl}"
        PREFLIGHT_DANTESYNC_SECONDARY_NAMES="${PREFLIGHT_DANTESYNC_SECONDARY_NAMES:+$PREFLIGHT_DANTESYNC_SECONDARY_NAMES }${_pfdslbox}"
        ;;
    esac
  done
  if [ -n "$PREFLIGHT_DANTESYNC_SECONDARY" ]; then
    echo "[0/8] dantesync freshest-offset sanity — ${PREFLIGHT_DANTESYNC_SECONDARY_NAMES} (#758/#827/#947)"
    # issue 1022 follow-up (live run 31669664399): the gate's step-chase machinery (client bound
    # widened by the master's own ntp_deadband_us envelope + the bimodal chase-signature
    # exclusion) engages ONLY when the NTP master is among the call's configured nodes -- without
    # strih here, a secondary camera is graded against the BARE bound and false-fails on the
    # master's routine ~2.5ms step propagation. Passing strih also re-grades strih itself in this
    # call (harmless, already proven clean by the main gate above).
    # Enforce grandmaster identity here too (issue 1073): this call grades strih (the NTP master),
    # whose grandmaster identity must be enforced exactly like the main gate above.
    DANTESYNC_GATE_GM_ENFORCE=1 "$HERE/dantesync-gate.sh" \
      --bound-us "${CLOCK_GUARD_BOUND_US:-2000}" \
      --win-http-port "${WIN_DANTE_PORT:-8898}" \
      --win-http "strih=$STRIH" \
      --linux "$PREFLIGHT_DANTESYNC_SECONDARY"
  else
    echo "[0/8] dantesync freshest-offset sanity — skipped: no secondary cameras in CAMERA_ACTIVE_SET, cam1+cam2 are already fully covered by the main DanteSync gate above (#947)"
  fi

  # #924 (user directive, 2026-08-01): the rig's DEFAULT state, whenever the user has NOT asked
  # for EVENT mode, is TEST mode -- burns available, QR painting, the sync marker audible. A
  # harness starting on a rig in its own default state must SET UP whatever state it needs itself
  # (exactly like [2/8]/[4b/8] below already turn burns ON with the correct per-run run_ids) --
  # never abort and demand a human normalize the rig first. A leaked genlock_burn=true from a run
  # that never reached cleanup() (the #246/#844 leak class -- e.g. a GH Actions concurrency-group
  # cancel, see .claude/rules/ci-testing-gotchas.md) is exactly such a state: THIS harness can
  # clear it itself, the same idempotent `obs_burn_filter.py remove` the #844/#878 startup
  # self-heal above already trusts. NORMALIZE, don't abort -- strih/stream must still not already
  # be recording/streaming below (a stray session this harness cannot safely take over itself).
  echo "[0/8] OBS pre-run state — normalizing genlock_burn OFF on every strih NDI input (#924), no stray recording/streaming (#758)"
  # #827 follow-up: derive the checked camera list from CAMERA_ACTIVE_SET (camera-set.sh) minus
  # any acked-offline box -- never a literal 1..7 range (a retired camera must never be checked
  # here, regardless of what its strih OBS input still looks like).
  for _cam in $(camera_active_excluding "$PREFLIGHT_EXCLUDED_CAMS"); do
    _n="${_cam#cam}"
    _pfburn="$(python3 "$HERE/obs_burn_filter.py" check --host "$STRIH" --input "NDI cam${_n}" 2>/dev/null || true)"
    case "$_pfburn" in
      *burn_on=True*)
        echo "    normalizing: strih NDI cam${_n} had genlock_burn ON from a prior run — clearing it (#924)"
        python3 "$HERE/obs_burn_filter.py" remove --host "$STRIH" --input "NDI cam${_n}" 2>&1 \
          | sed "s/^/    [normalize cam${_n}] /" || true
        ;;
    esac
  done
  # #938/#1011: the CAMERA_ACTIVE_SET-derived loop above normalizes ONLY strih's active-fleet cam
  # inputs — an out-of-set / non-cam input left genlock_burn ON (strih 'NDI cam4'/'NDI cam3',
  # stream 'phase2-probe-src') stays ON into this run. Follow it with the shared EXHAUSTIVE sweep
  # (obs_burn_filter.py sweep-off — GetInputList over WS) on strih AND stream so no leaked burn
  # survives regardless of which input carries it, on strih/stream/imag (guard class issue 246/844).
  for _nsip in "$STRIH" "$STREAM" "$IMAG_IP"; do
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" sweep-off --host "$_nsip" 2>&1 \
      | sed "s/^/    [normalize sweep] /" || true
  done
  for _pfhs in "strih=$STRIH" "stream=$STREAM"; do
    _pfhbox="${_pfhs%%=*}"
    _pfhip="${_pfhs#*=}"
    _pfrec="$(python3 "$HERE/obs_phase2.py" record --action status --host "$_pfhip" 2>/dev/null || true)"
    case "$_pfrec" in
      *active=True*)
        echo "ERROR: [preflight] FAIL: ${_pfhbox}: OBS is ALREADY recording (a stray session from an aborted prior run) — stop it before starting: ${_pfrec}" >&2
        exit 1
        ;;
    esac
    _pfstream="$(python3 "$HERE/obs_phase2.py" stream-status --host "$_pfhip" 2>/dev/null || true)"
    case "$_pfstream" in
      *active=True*)
        echo "ERROR: [preflight] FAIL: ${_pfhbox}: OBS is ALREADY streaming (a stray session from an aborted prior run) — stop it before starting: ${_pfstream}" >&2
        exit 1
        ;;
    esac
  done

  # #882: distinguish process-absent / port-not-listening BEFORE ever attempting to open the
  # projectors. The 2026-07-30 outage left every subsequent preflight failure reading a WRONG
  # generic message (hardcoding a connector pair that isn't even present on this box) even when
  # the true cause was "OBS was not running at all" -- a one-line honest diagnosis here replaces
  # what was ~30 minutes of investigation.
  # issue 1013: skip the whole imag OBS-prep leg (reachability probe / projectors / wmctrl / heal /
  # studio-mode) when imag is acked-offline — every step below hard-aborts (exit 1) on an absent
  # box. The body stays at its original indent under this guard (bash-legal; the static-anchor
  # tests match substrings, not indentation) to keep the diff minimal in this anchor-dense region.
  if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
    imag_leg_skip_note "[0/8] imag OBS-prep (reachability probe / projectors / wmctrl / studio-mode)" "$IMAG_OFFLINE_ACK_REASON"
  else
  echo "[0/8] imag-nb OBS reachability probe (process/port) — #882"
  _imag_reach_probe="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_obs_reachability_probe_cmd)" 2>/dev/null || true)"
  _imag_reach_msg="$(imag_obs_reachability_message "$_imag_reach_probe")"
  if [ -n "$_imag_reach_msg" ]; then
    echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}): ${_imag_reach_msg}" >&2
    exit 1
  fi

  # imag-nb's Multiview AND Program projectors must be OPEN before ANY run starts — the user's
  # explicit, binding requirement ("MULTIVIEW MUSI BYT ZAPNUTE ako podmienka preflight pred tym
  # nez sa rozbehne akykolvek test"). obs-websocket has no "is it open" query, so this ALWAYS
  # (idempotently) opens both — a failed open is a loud preflight FAIL, never a silent skip. By
  # this point the #882 reachability probe above already ruled out process-absent/port-closed, so
  # a failure HERE is either a WS handshake/auth problem or no matching monitor -- open_projectors
  # itself now labels those two cases distinctly (scripts/obs_phase2.py); never re-assert a
  # hardcoded connector name on top of its already-accurate message.
  echo "[0/8] imag-nb Multiview + Program projectors must be OPEN (#758)"
  if ! python3 "$HERE/obs_phase2.py" open-projectors --host "$IMAG_IP" 2>&1 | sed 's/^/    [imag projectors] /'; then
    echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}): open-projectors failed — see the [imag projectors] output above for the exact cause (#882)." >&2
    exit 1
  fi

  # #833: preflight imag-nb's OWN tooling BEFORE either the #769 heal or the #756 count check
  # below shells out to `wmctrl` — a MISSING tool must fail loud BY NAME here, never be silently
  # misread downstream as "0 projectors" (a freshly (re)provisioned box without wmctrl installed,
  # #791, cost three wasted hardware-gate re-runs chasing a false "stray windows" diagnosis).
  _imag_wmctrl_probe="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_require_remote_tool_cmd wmctrl)" 2>/dev/null || true)"
  _imag_wmctrl_missing="$(imag_remote_tool_probe_missing "$_imag_wmctrl_probe")"
  if [ -n "$_imag_wmctrl_missing" ]; then
    echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}): required tool(s) not installed: ${_imag_wmctrl_missing} (apt-get install -y wmctrl) — refusing to run the #756 projector-count check that cannot execute without it (#822/#833 class)." >&2
    exit 1
  fi

  # camera-box #756: OpenVideoMixProjector (called unconditionally by the block above, on EVERY
  # single recording-e2e.sh run) only REPLACES the same-monitor projector instead of stacking a
  # new one on top when the OBS user-config key BasicWindow.CloseExistingProjectors is true
  # (vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp OpenProjector()) — that key has
  # NO compiled-in default (a missing key reads as false), and imag's global.ini never carried
  # it until setup-imag.sh's #756 seed. Live-caught (2026-07-15): with the key unset, imag had
  # accumulated 7 stray Multiview + 7 stray Program projector windows (confirmed via
  # `wmctrl -l` over SSH) — seven independently-throttled Multiview renders, each STILL costing
  # real graphics-thread time regardless of the #276/#278/#293 per-display divisor mechanism
  # (which IS confirmed correctly compiled in — see the nm -D -u capability check below), fully
  # explains the render-health preflight's intermittent sub-58fps failures. setup-imag.sh now
  # seeds CloseExistingProjectors=true so every open-projectors call above self-corrects to
  # exactly one window per monitor — but PROVE it every run, never just hope the config landed
  # (a reprovision that forgets the seed, or an OBS upgrade changing the default, must be caught
  # immediately, not silently degrade render health run after run): hard-fail loud if imag
  # shows more than one Multiview or Program projector AFTER the narrow #769 heal below.

  # #769: windowed-stray heal FIRST — OBS's launch-restore can recreate a projector WINDOWED
  # (internal monitor=-1); the CloseExistingProjectors replace loop matches GetMonitor()==target
  # only, so that stray is invisible to it and every ensure-open above stacks one more window
  # (live ping-pong: 3 gate refusals in one afternoon, 2026-07-15). Keep the NEWEST window per
  # kind (the one ensure-open just opened on the proper monitor), close older strays; the count
  # check below stays the LOUD backstop when this does not converge (a genuinely regressed
  # config must still refuse the rig, #756).
  # shellcheck source=lib/imag-projector-heal.sh
  . "$HERE/lib/imag-projector-heal.sh"
  sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_projector_heal_cmds)" 2>/dev/null \
    | sed 's/^/    [imag projector-heal] /' || true

  echo "[0/8] imag-nb projector count must be EXACTLY 1 Multiview + 1 Program — no stray accumulation (#756)"
  _mv_count="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" \
    "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Multiview' || true" \
    2>/dev/null || true)"
  _pgm_count="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" \
    "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Program' || true" \
    2>/dev/null || true)"
  # issue 1152 M4 lease-tolerance slice: in DRM-lease mode the Program is drawn by the vendored
  # OBS DRM output directly onto the leased CRTC (.claude/rules/obs-drm-output.md), never an X
  # window -- so the expected Program window count is 0, not 1, while Multiview stays required
  # at exactly 1 either way. Consult the box's OWN lease config via imag_scenes' shared
  # classifier (the SAME grammar open_projectors/imag-obs-start.sh already use, per the M4
  # follow-up rule doc) rather than a second, divergent config reader.
  _lease_connector="$(python3 -c "
import sys
sys.path.insert(0, '$HERE')
import imag_scenes
print(imag_scenes.drm_output_lease_connector(imag_scenes._drm_output_config_text('$IMAG_IP')))
" 2>/dev/null || true)"
  # shellcheck source=lib/imag-projector-lease-count.sh
  . "$HERE/lib/imag-projector-lease-count.sh"
  _lease_verdict="$(imag_projector_lease_count_verdict "$_lease_connector" "${_mv_count:-}" "${_pgm_count:-}")"
  case "$_lease_verdict" in
    ok-lease)
      echo "    ok: imag-nb drm-output lease ENABLED for '${_lease_connector}' -- exactly 1 Multiview projector, 0 X Program windows (Program is on the DRM-leased scanout, issue 1152)"
      ;;
    ok-dormant)
      echo "    ok: imag-nb shows exactly 1 Multiview + 1 Program projector"
      ;;
    fail-lease)
      echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}) drm-output lease ENABLED for '${_lease_connector}' but projector count is Multiview=${_mv_count:-0} Program=${_pgm_count:-0}, expected exactly 1 Multiview + 0 Program (issue 1152 -- the Program is DRM scanout; an X Program window here means the connector never actually left the X layout, or a stray reappeared)." >&2
      exit 1
      ;;
    *)
      echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}) projector count is Multiview=${_mv_count:-0} Program=${_pgm_count:-0}, expected exactly 1+1 — stray projector windows are accumulating (check BasicWindow.CloseExistingProjectors=true in ~/.config/obs-studio/{global,user}.ini on imag-nb, or close the extras: DISPLAY=:0 wmctrl -l | grep Projector)." >&2
      exit 1
      ;;
  esac

  # imag Studio Mode must be ON — INVERTED from the former #758 force-OFF step (user hard rule,
  # 2026-07-15: without Studio Mode the multiview's Preview cell is DEAD, so Studio is "MUST BE"
  # on EVERY broadcast box, imag included — never a render knob to toggle by mood). The old
  # force-OFF was written when Studio ON collapsed imag's render (38-42fps/~23ms) — root cause
  # was the pre-#767 distroav.so receiver teardown churn, NOT the preview pass: with the #767
  # keep-alive build imag measures 60.0fps/~1.8ms WITH Studio ON + MV + 7 cams + overlays
  # (2026-07-15). Forcing ON here also makes the render-health preflight below measure the REAL
  # production state — a Studio-ON render regression must fail the gate, not hide behind a
  # temporarily-toggled-off preview.
  echo "[0/8] imag Studio Mode must be ON (production parity — Preview cell needs it, #767)"
  python3 "$HERE/obs_phase2.py" ensure-studio-mode-on --host "$IMAG_IP" 2>&1 | sed 's/^/    [imag studio-mode] /'

  # #1151 REPORT-ONLY: confirm the issue-1146 `projector-vsync: present-vsync ARMED` marker is armed
  # in imag's OBS log. Placed AFTER the projector-open step (the marker is emitted at Program-projector
  # OPEN, one-shot-on-change, so a check before the projectors are opened reads nothing after a restart).
  # Reads the newest OBS .txt log via the SHARED projector_vsync_gather_remote_snippet ($(fn) embed,
  # the #675 sourced-lib pattern), formats via the SHARED projector_vsync_report_line. NEVER fails the
  # run (issue 781: proves the tear-free present MECHANISM is engaged, never that scanout tearing is
  # gone — objective proof needs the physical HDMI tap). Same lib the drift-guard --check-imag facet
  # uses, so the marker string lives in ONE place.
  echo "[0/8] imag Program present-vsync marker check (report-only — #1151, issue 1146/1107)"
  _imag_vsync_log="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(projector_vsync_gather_remote_snippet)" 2>/dev/null || true)"
  echo "    [imag projector-vsync] $(projector_vsync_report_line "$_imag_vsync_log")"
  fi   # issue 1013: end of the IMAG_OFFLINE_ACKED skip-guard over the imag OBS-prep leg
fi

# Capture-delivery-rate preflight (#656 prevention item 2): the appliance's OWN capture loop
# (src/capture_rate_health.rs) already WARNs when $CAMERA_NAME's captured fps has sustained a
# >1% deviation from its negotiated rate for 6 consecutive 5s report windows — the exact #656
# root cause (cam1's ShadowCast 2 silently delivering ~64fps instead of 60fps, producing a
# persistent ~4Hz content-duplicate judder only caught after the fact via tick-pattern
# archaeology on a full 6-minute recording, fixed live via a USB reset). Read the SOURCE
# camera's recent journal for that WARN and fail FAST — before burning a doomed 30-minute run —
# rather than re-deriving the fps math a second time here (single source of truth stays in Rust).
echo "[0/8] capture-delivery-rate preflight — $CAMERA_NAME must not show a sustained rate defect (#656)"
# #693: resolve the CURRENT camera-box.service InvocationID first, so the journal read below is
# scoped to THIS process instance only -- a stale WARN from a prior instance (killed by a routine
# cleanup() restart) must never leak into the lookback window. Empty on failure (older systemd /
# transient ssh hiccup) -- capture_rate_journalctl_cmd falls back to the unscoped read then.
CAPTURE_RATE_INVOCATION_ID="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"
CAPTURE_RATE_DEFECT_LINE="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(capture_rate_journalctl_cmd "$CAPTURE_RATE_INVOCATION_ID") | grep -E '$(capture_rate_defect_grep_pattern)' | tail -1" \
  2>/dev/null || true)"
if [ -n "$CAPTURE_RATE_DEFECT_LINE" ]; then
  echo "ERROR: $(capture_rate_preflight_message "$CAMERA_NAME" "$CAPTURE_RATE_DEFECT_LINE")" >&2
  echo "       matched journal line: $CAPTURE_RATE_DEFECT_LINE" >&2
  exit 1
fi
echo "    ok: no sustained capture-rate defect in $CAMERA_NAME's recent journal"

# Leg-health preflight (#1133): the #656 preflight ABOVE only fires on the appliance's own fps
# DEFECTIVE WARN, which never fires for the #1130/#1110-class defect (cam1 delivered 61-63fps —
# inside the ShadowCast 2 tolerance — while it stalled/skipped/EPROTO'd for HOURS, and the #656
# preflight still said "ok"). This step reads the OTHER, orthogonal degradation signals the box
# genuinely emits and ABORTS the run naming the box + signal, so a sick capture leg is a named
# escalation (drop the box from CAMERA_ACTIVE_SET / CAMBOX_OFFLINE_ACK, or fix the leg) rather than
# a 30-minute run measured on a broken tool. The HARD signals are: sustained CAPTURE FRAME LOSS
# (sent-vs-captured from the `Streaming:` lines, calibrated 1.90% gate with a sustain guard — the
# #1133 replacement for the DEQUEUE STALL gate), emit-gate SKIPPED aggregates (last 5 min), and
# kernel uvcvideo -EPROTO (last hour). REPORT-ONLY (never abort): the #707 DEQUEUE STALL COUNT
# (issue 1198 proved it ANTI-correlated with real frame loss — VIDIOC_DQBUF is a blocking wait, so
# a well-protected fast thread reports MORE stalls than a lossy one; its old "replace cable/port/
# grabber" wording was a misattribution) and the cap-1s over-rate (chronic ShadowCast over-rate is
# absorbed by the genlock decimation, issue #909 — hard-failing it would recreate that mistake).
# All logic lives in the sourced scripts/lib/leg-health-guard.sh (frame-loss threshold calibrated
# from the supervisor's 2026-08-20 live-fleet read; the classifiers + patterns are unit-tested in
# tests/harness_leg_health_guard_1133.rs).
echo "[0/8] leg-health preflight — every active capture leg must be free of sustained frame loss / emit-gate skips / USB EPROTO (DQBUF stall count is now report-only, issue 1198) (#1133)"
# Always check the SOURCE camera (its feed drives the whole verdict); under ALL_CAMBOX also check
# the reachable+healthy+non-acked secondaries the fleet preflight already vetted
# (PREFLIGHT_DANTESYNC_LINUX = "box=ip" pairs, so an acked/unreachable box is already absent).
LEG_HEALTH_TARGETS="$CAMERA_NAME=$CAM1_IP"
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -n "${PREFLIGHT_DANTESYNC_LINUX:-}" ]; then
  for _lhpair in $PREFLIGHT_DANTESYNC_LINUX; do
    _lhb="${_lhpair%%=*}"
    [ "$_lhb" = "$CAMERA_NAME" ] && continue # source already in the list
    LEG_HEALTH_TARGETS="$LEG_HEALTH_TARGETS $_lhpair"
  done
fi
LEG_HEALTH_NOW="$(date +%s)"
LEG_HEALTH_SINCE=$((LEG_HEALTH_NOW - $(leg_health_journal_window_secs)))
LEG_HEALTH_EP_SINCE=$((LEG_HEALTH_NOW - $(leg_health_eproto_window_secs)))
for _lht in $LEG_HEALTH_TARGETS; do
  _lhbox="${_lht%%=*}"
  _lhip="${_lht#*=}"
  # Respect offline-ack: an operator-acked box is not checked (belt-and-braces — a secondary from
  # PREFLIGHT_DANTESYNC_LINUX is already non-acked, but the source is added unconditionally).
  if cambox_offline_ack_is_acked "$_lhbox"; then
    echo "    skip: $_lhbox — operator-acknowledged offline, leg-health not checked"
    continue
  fi
  # issue 1170: cam2's CAPTURE leg is checked ONLY while cam2 is a MEASURED camera (gated on
  # `camera_is_active cam2`, currently TRUE — cam2 is back in the default CAMERA_ACTIVE_SET as of
  # issue 1198; the owner ruled the grabber "hardware-defective" diagnosis wrong and refused the
  # card swap the ticket originally tracked). cam2 stays a PAINTER (its reachability + DanteSync
  # clock are ALWAYS gated above, regardless of this leg's status). Should a REAL capture-leg
  # regression reproduce on cam2 in the future, dropping it from CAMERA_ACTIVE_SET again skips this
  # check automatically (never a hardcoded literal here) — cam2 is only a leg-health target because
  # it is a hardcoded painter reachability entry that flows into the healthy-node list.
  if [ "$_lhbox" = cam2 ] && ! camera_is_active cam2; then
    echo "    skip: cam2 — capture leg excluded from measurement (issue 1170, not in CAMERA_ACTIVE_SET); painter clock/reachability still gated"
    continue
  fi
  # Resolve THIS box's CURRENT camera-box.service InvocationID so the journal read is scoped to
  # the running instance only (#693 — a killed prior instance's sick lines never leak in). The
  # source reuses the id already resolved by the #656 preflight above.
  if [ "$_lhbox" = "$CAMERA_NAME" ]; then
    _lhinv="$CAPTURE_RATE_INVOCATION_ID"
  else
    _lhinv="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_lhip" \
      "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"
  fi
  _lhout="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_lhip" \
    "$(leg_health_read_all_cmd "$_lhinv" "$LEG_HEALTH_SINCE" "$LEG_HEALTH_NOW" "$LEG_HEALTH_EP_SINCE" "$LEG_HEALTH_NOW")" \
    2>/dev/null || true)"
  _lhstall="$(leg_health_extract STALL "$_lhout")"
  _lhskip="$(leg_health_extract SKIP "$_lhout")"
  _lheproto="$(leg_health_extract EPROTO "$_lhout")"
  _lhstreaming="$(leg_health_extract_streaming "$_lhout")"
  # HARD signal A (count-based): emit-gate SKIP aggregates + kernel uvcvideo -EPROTO. #1133 DROPPED
  # the DEQUEUE STALL count from this classify — it gated on a quantity ANTI-correlated with real
  # frame loss (issue 1198: the arm losing 4.5x FEWER frames reported MORE stalls).
  if ! _lhmsg="$(leg_health_classify "$_lhbox" "$_lhskip" "$_lheproto")"; then
    echo "ERROR: $_lhmsg" >&2
    exit 1
  fi
  # HARD signal B (#1133): sustained CAPTURE FRAME LOSS (sent-vs-captured from the `Streaming:`
  # lines) — the quantity the DEQUEUE STALL count only correlated with backwards. Calibrated 1.90%
  # gate with a sustain guard (leg_health_frame_loss_*); over-rate is benign (issue #909).
  if ! _lhmsg="$(leg_health_frame_loss_classify "$_lhbox" "$_lhstreaming")"; then
    echo "ERROR: $_lhmsg" >&2
    exit 1
  fi
  # Report-only: surface a sustained cap-1s over-rate for diagnostics (never aborts, issue #909).
  leg_health_cap1s_band_warn "$_lhbox" "$(leg_health_extract_cap1s "$_lhout")"
  # Report-only (#1133): the DEQUEUE STALL count is now diagnostics-only (anti-correlated with real
  # frame loss, issue 1198 — never aborts).
  leg_health_dequeue_stall_report "$_lhbox" "$_lhstall"
  echo "    ok: $_lhbox capture leg healthy (loss-ok skip=$_lhskip eproto=$_lheproto, stall=$_lhstall report-only in-window)"
done
# #1141: head-end OPTICAL blur/shutter fail-fast. The capture-RATE gate above (#656) proves the
# source camera captures at the right RATE, but is BLIND to a camera capturing at that rate yet
# BLURRED (slow shutter 1/60 / anti-flicker — the #216 class: 16.7 ms exposure smears the moving
# dual-QR → optically undecodable → a 175 s optical-read gap, no frame loss the rate check can see).
# Read the source box's head-end rough= capture telemetry (src/capture.rs luma_roughness, #1079) and
# fail-fast NAMED (Slovak) when it is SUSTAINED below the calibrated floor. Plain statement (never
# $()/pipeline) so its `exit 1` propagates. NOTEs (never aborts) on thin data / an ssh hiccup — the
# fleet reachability gate owns genuine unreachability, and the owner's hardest constraint is that a
# CI gate is never FALSE-aborted. Immune to the imag x264 observer effect (#1130): capture chain,
# before the recorder.
echo "[0/8] optical head-end blur/shutter preflight — $CAMERA_NAME must not capture BLURRED (slow shutter/anti-flicker, #1141)"
optical_preflight_assert "$CAM1_IP" root "$CAM_PW" "$CAMERA_NAME"

# cam1 v4l2 capture controls (#338/#312, range-aware since #744): apply the device's OWN
# --list-ctrls default for saturation+contrast BEFORE the run. The old "sharp set" (saturation=0,
# contrast=75) was meant to aid the optical dual-QR decode but HURT it (#312 run 312005:
# the ShadowCast box with the sharp set read ~50% undecodable while the NZXT card on
# device defaults read the SAME monitor clean). Device defaults decode fine; saturation=0
# also tinted/greyed the picture. #744: a hardcoded literal saturation=50 / contrast=50 pair is
# only correct on the ShadowCast 2's own 0-100 range -- on the 0-255 Elgato 4K S cards the SAME literal
# is ~39% of THEIR default (128), producing a dark/chroma-muted picture (live 2026-07-13). Each
# card now gets its OWN reported default via scripts/lib/v4l2-neutral.sh, resolving the capture
# node dynamically (never a hardcoded /dev/video0 -- USB nodes renumber, #728). The [2/8] deploy
# step re-applies the same reset at open; this is the belt-and-braces preflight the harness owns
# regardless.
echo "[0/8] apply device-own-default colour controls (#338/#312/#744)"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(v4l2_neutral_apply_cmds)" \
  || echo "WARNING: could not pre-apply $CAMERA_NAME v4l2 controls (the [2/8] deploy step re-applies them)" >&2

# #758 item 2 — sender-bounce liveness re-verify: after a box's [2/8]/[2b/8] service->burn-unit
# swap, re-verify its "NDI cam<N>" main input still delivers CHANGING frames. A swap can leave
# the box emitting NDI again while strih's receiver never re-locks on its own; ONE auto
# re-attach (strih_mv_scenes.py --reattach, re-applying the input's OWN current
# ndi_source_name) is tried, THEN the harness gives the reconnect real SETTLE time across a
# bounded number of re-checks before failing loud — never StartRecord, never a cleanup that
# ends with a leg still dead.
#
# #761 (2026-07-15, user-directed, KEPT): strih's "MV Cam N" scenes were switched to
# SAME-SOURCE — the old "MV NDI cam<N>" low-bandwidth clone items are now DISABLED in those
# scenes, so probing them always reads frozen regardless of camera health. This re-verify (and
# strih_mv_scenes.py's reattach()) now targets the MAIN "NDI cam<N>" input instead — the SAME
# per-box split as the [1/8] preflight above. It works without frozen-camera-gate.py's own
# #747 PREVIEW warm-up (still --warm-settle 0, unchanged) because the built-in OBS Multiview
# grid projector renders every "Cam N" scene's thumbnail continuously while open, keeping every
# "NDI cam<N>" main input always-active regardless of program/preview state. IMAG stays on the
# low-bandwidth clone model (#763 tracks unifying the two boxes later) — this function is
# strih-only (called with a camera name/number, always resolves against $STRIH).
#
# Retry/settle budget calibrated from a LIVE measurement (2026-07-14), not guessed: this
# mechanism's first two real CI exercises both failed cam1 with the ORIGINAL tight budget (a
# single ~3.5s check, one re-attach, a 2s settle, one more ~3.5s check -- ~13s total after the
# caller's own upfront `sleep 4`). A direct timed `systemctl restart camera-box` + repeated
# frozen-camera-gate polling against the SAME "MV NDI cam1" clone (the pre-#761 target)
# measured genuine recovery at t+11.4s -- inside the OLD budget's margin, but only barely,
# matching the two real failures. The mid-run [3/8]-area frozen-camera-gate retry loop below
# (FROZEN_CAM_ATTEMPTS=4 x FROZEN_CAM_RETRY_SLEEP=30s) already establishes this repo's OWN
# precedent for how long a genuine post-deploy NDI reconnect can take -- this preflight check
# reuses the SAME "attempts x settle" shape, just with a smaller per-camera budget (it fires up
# to 7x in an ALL_CAMBOX sweep, vs. the mid-run gate's one-shot use), sized comfortably above
# the measured 11.4s.
preflight_mv_reverify() {
  local box="$1" cam_n="$2"
  [ "${ALL_CAMBOX:-0}" = "1" ] || return 0
  case " $PREFLIGHT_EXCLUDED_CAMS " in *" $box "*) return 0 ;; esac
  local attempts="${PREFLIGHT_MV_REVERIFY_ATTEMPTS:-3}"
  local settle_s="${PREFLIGHT_MV_REVERIFY_SETTLE_S:-6}"
  # #759 review: bound each OBS-touching python call by `timeout` (the #328 discipline — a hung
  # obs-websocket op must NEVER block, least of all the cleanup trap this is now called from). The
  # calls are already internally bounded (create_connection timeout + per-RPC timeout); this is the
  # belt-and-suspenders outer bound, harmless in the happy path (both complete well under it).
  local call_timeout="${PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT:-30}"
  local a
  for a in $(seq 1 "$attempts"); do
    if timeout "$call_timeout" python3 "$HERE/frozen-camera-gate.py" --host "$STRIH" --password "" \
        --sources "NDI cam${cam_n}" --samples 2 --cadence 3.5 --threshold 1 --warm-settle "${PREFLIGHT_MV_REVERIFY_WARM_SETTLE:-0}" \
        --verdict-bin "$PROBE_BIN_DIR/frozen-camera-gate" >/dev/null 2>&1; then
      if [ "$a" -gt 1 ]; then
        echo "    [sender-bounce] ${box} recovered on attempt ${a}/${attempts}" >&2
      fi
      return 0
    fi
    if [ "$a" -eq 1 ]; then
      echo "    [sender-bounce] ${box} (NDI cam${cam_n}) shows no pixel change right after its deploy — attempting re-attach (#758 item 2)" >&2
      timeout "$call_timeout" python3 "$HERE/strih_mv_scenes.py" --host "$STRIH" --password "" --reattach "$cam_n" >&2 || true
      # #1114: the re-attach above is a CLEAR-then-SET receiver reset (the merged WS-side fix) that
      # tears down + rebuilds strih's NDI receiver — so its fresh DistroAV
      # finder must re-resolve the live post-bounce burn sender by URL, MEASURED at up to ~2 min on
      # the live rig. Give it its OWN bounded re-resolve window right after the kick (mv_reverify_
      # resolve_wait: a real poll that exits the instant a pixel changes, so a fast re-lock costs ~0
      # extra), so the short attempt budget below cannot false-fail a genuinely-live-but-still-
      # resolving leg into the destructive receiver-wedge escalation. Deploy context only — the
      # cleanup caller (attempts=1) must stay fast enough never to outlast a GH-Actions cancellation
      # grace window.
      if [ "${PREFLIGHT_MV_REVERIFY_CONTEXT:-preflight}" != "cleanup" ]; then
        if mv_reverify_resolve_wait "$box" "$cam_n" "$call_timeout"; then
          return 0
        fi
      fi
    fi
    if [ "$a" -lt "$attempts" ]; then
      echo "    [sender-bounce] ${box} attempt ${a}/${attempts} still no pixel change — settling ${settle_s}s for the NDI reconnect, then re-sampling" >&2
      sleep "$settle_s"
    fi
  done
  # #759 review: in the WARN-only cleanup context the run is already ending and nothing is being
  # aborted, so DON'T emit the deploy-time "ERROR ... this run must not proceed" line there (it reads
  # as a spurious run-abort in the teardown log). The cleanup caller emits its own accurate WARNING;
  # the deploy-time callers (|| exit 1) keep the loud ERROR that correctly precedes their abort.
  if [ "${PREFLIGHT_MV_REVERIFY_CONTEXT:-preflight}" != "cleanup" ]; then
    echo "ERROR: [preflight] FAIL: ${box} (NDI cam${cam_n}) still shows no pixel change after ${attempts} attempts (incl. one re-attach, ~$((attempts * 4 + (attempts - 1) * settle_s))s total) — the camera leg is dead right after its own deploy. Investigate the box directly (this run must not proceed with a known-dead leg)." >&2
  fi
  return 1
}

# shellcheck disable=SC2317  # called only from the cleanup trap (SC2317 = "unreachable")
cleanup_mv_reverify_active_boxes() {
  # #759 (#758 item 2, cleanup half): WARN-only sender-bounce re-verify for the cleanup trap. After
  # the device-restore phase above restarts camera-box on every active box (a sender bounce), a
  # camera's NDI leg can come back up while strih's receiver never re-locks on its own -- leaving a
  # dead leg that poisons the NEXT run's [0/8] preflight. Re-check each active camera and nudge it
  # once (one fire-and-forget reattach), WARN-only: this runs inside the EXIT trap and must NEVER
  # abort it (the deploy-time sites correctly fail the run loud; here we only warn), per the
  # #328/#712/#713 cleanup discipline.
  [ "${ALL_CAMBOX:-0}" = "1" ] || return 0
  # Ordering/readiness guard: the trap can fire from an early [0/8] preflight failure BEFORE
  # PROBE_BIN_DIR is set / the probe binaries exist. With no frozen-camera-gate binary there is
  # nothing to verify against, so SKIP -- never fire a spurious reattach against an unset
  # $PROBE_BIN_DIR. (preflight_mv_reverify is now defined ABOVE the trap, so it is always callable
  # here regardless of how early the trap fires.)
  if [ ! -x "${PROBE_BIN_DIR:-}/frozen-camera-gate" ]; then
    echo "[cleanup] #759 sender-bounce reverify SKIPPED -- probe binaries not yet available (trap fired before deploy setup)" >&2
    return 0
  fi
  echo "[cleanup] #759 sender-bounce reverify -- re-checking each active camera's NDI leg re-locked after its restart (WARN-only)" >&2
  local _rvcam
  for _rvcam in $CAMERA_ACTIVE_SET; do
    # attempts=1: one quick check + one fire-and-forget reattach on failure, NO multi-attempt
    # settle loop -- the next run's [0/8] preflight is the real gate; cleanup only nudges + warns,
    # never blocking the trap long enough to outlast a GH-Actions cancellation grace window. The
    # `|| echo` (never a hard fail) keeps a still-dead leg from ever aborting the cleanup trap.
    PREFLIGHT_MV_REVERIFY_ATTEMPTS=1 PREFLIGHT_MV_REVERIFY_CONTEXT=cleanup preflight_mv_reverify "$_rvcam" "${_rvcam#cam}" \
      || echo "    WARNING #759: ${_rvcam} NDI leg still not delivering changing frames after its cleanup restart (reattach nudged) -- the next run's [0/8] preflight will re-check/fail on it if still dead" >&2
  done
}

# shellcheck disable=SC2317  # cleanup() runs via the EXIT/HUP/INT/TERM trap
cleanup() {
  set +e
  # #657: cleanup() can now be invoked TWICE on an interrupted run — once synchronously via the
  # INT/TERM/HUP trap (fired the instant interruptible_sleep's `wait` is interrupted, BEFORE
  # `wait` even returns control to the caller), and again via the EXIT trap once
  # interruptible_sleep's own `exit` call actually terminates the shell. Both traps point at the
  # SAME function name (armed on EXIT HUP INT TERM further below), so guard re-entry here: only
  # the FIRST invocation does the real teardown work; a second call is a safe, instant no-op.
  if [ "${CLEANUP_HAS_RUN:-0}" = "1" ]; then
    return
  fi
  CLEANUP_HAS_RUN=1
  # #649: StopRecord is the VERY FIRST thing cleanup() does — before EVEN the heartbeat/marker
  # clears below, and well before the #328 cam-device-free block. Root cause of the live incident
  # (2026-07-10): a GitHub Actions cancellation sends SIGINT, then SIGKILLs after a short grace
  # window; the OLD cleanup() reached StopRecord only AFTER the heartbeat/marker clears + the cam1
  # ssh restore + the cam2 ssh restore (each up to CLEANUP_SSH_TIMEOUT=30s) — so a SIGKILL landing
  # inside that grace window killed the trap before it ever stopped strih/stream/imag's recording.
  # The orphaned recording then self-deadlocks EVERY later gate run as RIG_BUSY (rig-busy-gate.sh
  # can't tell "our own leftover" from a real broadcast) until someone manually stops it (#649).
  # StopRecord itself is a single, normally-instant obs-websocket round trip — moving it first
  # costs nothing in the happy path and means it lands even when the grace window is too short to
  # reach anything after it. #328's "free the cam device before OBS" lesson is UNCHANGED for the
  # ssh/OBS-scene work further down (that ordering is what the #312 incident needed); this is a
  # narrower, even-earlier safety-critical action ahead of it.
  #
  # #649 item 2: NEVER blind-StopRecord a box this harness never itself started recording on — an
  # early abort (before step [5/8]'s own StartRecord ever ran) must leave whatever IS recording
  # alone, since blindly stopping it could kill a REAL broadcast's recording (worse than the
  # original bug). Each
  # *_RECORDING_STARTED flag is set to 1 ONLY right after that box's OWN StartRecord call actually
  # succeeded ([5/8] below); a box whose StartRecord never ran (or never succeeded) keeps its
  # flag at the 0 default declared before the trap arms, so this block is a no-op for it.
  echo "[cleanup] #649 StopRecord FIRST (harness-started boxes only, before anything slower)"
  if [ "${STRIH_RECORDING_STARTED:-0}" = "1" ]; then
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$STRIH" --action stop >/dev/null 2>&1 \
      && echo "    [cleanup] strih: StopRecord ok" \
      || echo "    WARNING #649: strih StopRecord failed/timed out in cleanup — recording may still be ON; check GetRecordStatus and stop it manually" >&2
  fi
  if [ "${STREAM_RECORDING_STARTED:-0}" = "1" ]; then
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop >/dev/null 2>&1 \
      && echo "    [cleanup] stream: StopRecord ok" \
      || echo "    WARNING #649: stream StopRecord failed/timed out in cleanup — recording may still be ON; check GetRecordStatus and stop it manually" >&2
  fi
  if [ "${IMAG_RECORDING_STARTED:-0}" = "1" ]; then
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action stop >/dev/null 2>&1 \
      && echo "    [cleanup] imag: StopRecord ok" \
      || echo "    WARNING #649: imag StopRecord failed/timed out in cleanup — recording may still be ON; check GetRecordStatus and stop it manually" >&2
  fi
  # #281 Fix#3: clear the rig-active heartbeat + stop its refresher — before the cam/OBS
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
  # #713: cam1 (SOURCE), cam3/4 (ALL_CAMBOX loop below), and cam2/painter ALL background
  # their ssh restore into ONE shared CAMBOX_PARALLEL_PIDS/LABELS group with ONE
  # cambox_parallel_wait_and_report call at the end of this device-restore phase (after
  # cam2/painter is armed, just before the OBS teardown region begins) -- extends #712's
  # cam3/4-only parallelization to the WHOLE phase. Live incident (2026-07-12, #713): a GH
  # Actions cancellation landing AFTER #712 shipped still stranded cam2 -- it sat OUTSIDE #712's
  # parallel group, sequential after the loop, so its restore never got a chance to run before
  # the SIGKILL. Backgrounding cam1 + cam2 too closes that remaining gap.
  CAMBOX_PARALLEL_PIDS=()
  CAMBOX_PARALLEL_LABELS=()
  # #1085: record each backgrounded restore's EXPLICIT target IP in lockstep with PIDS/LABELS so the
  # sequential retry (cambox_parallel_retry_failed) no longer has to parse the IP out of the display
  # label -- the interim label->IP coupling #715's mitigation introduced is retired here.
  CAMBOX_PARALLEL_IPS=()
  # cam1: FORCE-kill the manual #174 burn binary (pkill -9 -f, its own basename) AND any camera-box,
  # remove the deployed test binary, restore the clean deployed service — reliably frees /dev/video0.
  # #626: the pattern MUST be anchored ('camera-box-burn-[a-z0-9]') — a bare 'camera-box-burn-'
  # is a SELF-MATCH: the remote `sh -c "..."` process invoked BY ssh has this exact substring in
  # its OWN /proc/*/cmdline (it's the literal text of the pkill argument being run), so `pkill -f`
  # kills that shell before it ever reaches `systemctl restart` — a live 3h40m undetected outage
  # on cam1/cam3/cam4 traced to this exact bug (#626). The real target's argv0 always has EITHER a
  # run-id digit immediately after the hyphen (cam1's own /tmp/camera-box-burn-1783530925) OR a
  # camname letter (cam2/cam3/cam4's own #624/#312 ALL_CAMBOX deploy,
  # /tmp/camera-box-burn-cam3-1783530925 — `_cbin="/tmp/camera-box-burn-${_cn}-${RUN_ID}"`); the
  # invoking shell's own cmdline has a `[` bracket character there instead (the regex's own
  # class-open), so the anchored `[a-z0-9]` pattern matches ONLY a real target, never itself.
  # #640 CORRECTION: an earlier version of this comment claimed the DIGIT-only pattern
  # ('camera-box-burn-[0-9]') already matched the camname-infixed form too — it does NOT (the
  # character right after the hyphen there is a LETTER, not a digit). That gap orphaned
  # cam2/cam3/cam4's burn processes across multiple runs, crash-looping camera-box
  # ("Device or resource busy") until manually killed — found live while verifying #312 item 2 PR B.
  # #668: the burn binary now runs under a transient systemd-run unit (Restart=on-failure) so a
  # mid-test #663 self-heal respawns it — stop that unit FIRST (an explicit `systemctl stop` is
  # never followed by a restart, even under Restart=on-failure/always) so the pkill below can
  # never race a respawn back into existence.
  # #713: backgrounded ( ... ) & -- collected+waited in the shared group below, alongside
  # cam3/4 and cam2/painter.
  (
    cambox_parallel_stagger
    timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
      "systemctl stop camera-box-burn-${RUN_ID} 2>/dev/null; systemctl reset-failed camera-box-burn-${RUN_ID} 2>/dev/null; \
       pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
       rm -f /tmp/camera-box-burn-* 2>/dev/null; systemctl restart camera-box 2>/dev/null; true
$(camera_box_verify_active_cmds "$CAMERA_NAME (source, $CAM1_IP)")"
  ) &
  CAMBOX_PARALLEL_PIDS+=("$!")
  CAMBOX_PARALLEL_LABELS+=("$CAMERA_NAME (source, $CAM1_IP)")
  CAMBOX_PARALLEL_IPS+=("$CAM1_IP")
  # #624/#312: every ACTIVE secondary camera (camera_active_secondary_set(), #827) — same restore
  # as cam1, ONLY when the ALL_CAMBOX deploy above actually ran (gated the same way) so a plain
  # single-camera run never touches these boxes at all. #827 (binding owner directive): iterating
  # by NAME (not a fixed IP list) is what makes re-enabling a retired camera a one-line
  # CAMERA_ACTIVE_SET edit — this loop needs no change when the active set grows or shrinks.
  if [ "${ALL_CAMBOX:-0}" = "1" ]; then
    # #712: launch every box's restore CONCURRENTLY (never sequentially) so the whole loop
    # fits inside a GH Actions cancellation's short grace window regardless of how many
    # camboxes are active — see scripts/lib/cambox-parallel-restore.sh for the full incident.
    # #713: the arrays are now initialized ONCE, above (shared with cam1 + cam2/painter) --
    # this loop just keeps appending into them.
    for _ccn in $(camera_active_secondary_set); do
      _cip="$(camera_secondary_ip "$_ccn")"
      # #668: same stop-the-unit-first ordering as cam1 above — never let the pkill race a respawn.
      # #712: backgrounded ( ... ) & — every active secondary box restores IN PARALLEL, collected+waited below.
      (
        cambox_parallel_stagger
        timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
          "systemctl stop camera-box-burn-${_ccn}-${RUN_ID} 2>/dev/null; systemctl reset-failed camera-box-burn-${_ccn}-${RUN_ID} 2>/dev/null; \
           pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null; pkill -x camera-box 2>/dev/null; sleep 1; \
           rm -f /tmp/camera-box-burn-* 2>/dev/null; systemctl restart camera-box 2>/dev/null; true
$(camera_box_verify_active_cmds "$_ccn ($_cip)")"
      ) &
      CAMBOX_PARALLEL_PIDS+=("$!")
      CAMBOX_PARALLEL_LABELS+=("$_ccn ($_cip)")
      CAMBOX_PARALLEL_IPS+=("$_cip")
    done
    # #713: no per-loop wait here any more -- the SHARED wait below (after cam2/painter is
    # armed) covers cam1 + this loop + cam2/painter in one pass.
  fi
  # cam2 (painter): restart it. #309: FIRST clear any leftover #291 rig-mode no-display drop-in
  # (a prior `rig-mode.sh test` would otherwise make this restart bring camera-box back WITHOUT
  # --display — the interkom return monitor stays dark). The clear is single-sourced
  # (rig_test_dropin_clear_cmds) + idempotent (rm -f is a no-op if absent). #312: under
  # ALL_CAMBOX=1, [2b/8] ALSO deployed a probe-featured burn binary here under a transient
  # systemd-run unit (#668) — stopping that unit is harmless (no-op) on the plain single-camera
  # path, where [2b/8] never ran.
  # #713: backgrounded ( ... ) & -- collected+waited in the shared group, same as cam1/cam3-4.
  (
    cambox_parallel_stagger
    timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$PAINTER_IP" "pkill -x frame-probe 2>/dev/null || true
systemctl stop camera-box-burn-cam2-${RUN_ID} 2>/dev/null || true
systemctl reset-failed camera-box-burn-cam2-${RUN_ID} 2>/dev/null || true
pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null || true
rm -f /tmp/camera-box-burn-* 2>/dev/null || true
$(rig_test_dropin_clear_cmds)
systemctl restart camera-box 2>/dev/null || true
$(camera_box_verify_active_cmds "cam2/painter, $PAINTER_IP")
systemctl start cam2-painter 2>/dev/null || true
$(cam2_painter_restore_verify_cmds)
$(cam2_painter_restore_retry_cmds)
if [ -n \"\$_cprr_ok\" ]; then
$(cam2_painter_deadman_disarm_cmds)
else
  echo \"[cleanup] #1072: cam2-painter NOT confirmed active after retry — leaving the on-box dead-man ARMED for the ~5-min periodic self-heal\" >&2
fi"
  ) &
  CAMBOX_PARALLEL_PIDS+=("$!")
  CAMBOX_PARALLEL_LABELS+=("cam2/painter, $PAINTER_IP")
  CAMBOX_PARALLEL_IPS+=("$PAINTER_IP")
  # #713: ONE shared wait for cam1 + (cam3/4 if ALL_CAMBOX) + cam2/painter -- the whole
  # device-restore phase's wall-clock is now bounded by the SLOWEST single box, not the sum of
  # up to 6 sequential ssh round trips.
  cambox_parallel_wait_and_report
  # #1126: ONE final bounded genuine-painting re-check of cam2/painter BEFORE surfacing the failure.
  # The combined 30s restore ssh can SIGKILL a hair (~50ms) before cam2-painter.service reports
  # active on a slow restart; the restore succeeded, only the verify window lost the race, and the
  # #715 retry never prunes a painter — so without this a truthful-but-late success reds a
  # GREEN-verdict run. This re-check (a separate short ssh, never extending the tight restore budget)
  # prunes cam2/painter from CAMBOX_PARALLEL_FAILED_LABELS ONLY when it is genuinely painting NOW
  # (presenter-aware signal, never bare is-active — a dead painter stays and the #860 error fires).
  cam2_painter_restore_final_recheck "$PAINTER_IP"
  # #860: surface a cam2/painter (or any box) restore failure LOUDLY as a GitHub annotation instead
  # of leaving it a buried stderr WARNING #712 — a chain of failed cleanups left the painter dead +
  # silently poisoned consecutive gate runs (2026-08-14). Reads CAMBOX_PARALLEL_FAILED_LABELS the
  # wait above populated; never exits (set +e region), never changes always-runs semantics.
  cambox_parallel_surface_painter_failure
  # #759 (#758 item 2, cleanup half): now that every active box has finished restarting above (a
  # sender bounce), re-verify each camera's "NDI camN" leg re-locked on strih and nudge it once if
  # not — WARN-only, so a restart-left-unlocked leg never poisons the NEXT run's [0/8] preflight.
  # Bounded (attempts=1 per box) + guarded so it can never abort this trap; see the function above.
  cleanup_mv_reverify_active_boxes
  # The cam devices are now freed regardless of what the OBS restore does. #328: bound every OBS
  # call by `timeout` so a hung obs-websocket op (#328) can't block the trap even if it runs.
  # #649: StopRecord itself already ran, FIRST, at the top of this function (harness-started boxes
  # only) — no separate record-stop call belongs here any more. #682 SUPERSEDES the old "imag
  # never had its program scene routed by THIS harness" note (rig-mode.sh test used to be the ONLY
  # thing that ever touched it) — [4a/8] above now routes + saves it, so cleanup() restores it too.
  echo "[cleanup] restore OBS program scenes (each bounded by ${OBS_CLEANUP_TIMEOUT}s — #328)"
  # #1086: best-effort FINAL restore of the keepalive-bypass target's strih receiver, in case the
  # run was interrupted during the cold hold (or a single-appearance sweep) left it idled/black.
  # Inert no-op unless COLD_CUT_BYPASS_CAM was set AND the machine is still at phase=idled.
  cold_cut_cleanup_restore "$STRIH" "${OBS_PASSWORD:-}" "$HERE/obs_phase2.py"
  # #691: pass the calibrated cross-check value through ONLY when the caller supplied one
  # (empty by default — the common unattended-CI case simply skips the check).
  _stream_teardown_args=(teardown --host "$STREAM")
  if [ -n "$AV_SYNC_CALIBRATED_MS" ]; then
    _stream_teardown_args+=(--calibrated-latency-ms "$AV_SYNC_CALIBRATED_MS")
  fi
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" "${_stream_teardown_args[@]}"
  # #856: apply THIS run's own computed rig-wide A/V correction LAST -- strictly AFTER the
  # delivery-verify snapshot/restore call immediately above (which unconditionally restores
  # 'NDI 2ME PGM' genlock_latency_ms_src to whatever it was BEFORE this run started -- see that
  # call's own comment). Applying any earlier (e.g. at [8/8g] itself) would be silently
  # overwritten by that restore a few lines later in this SAME cleanup() -- composing with the
  # restore instead of fighting it, per the #856 issue text. Empty (unset) by default: an early
  # abort, or a run where [8/8g]'s combiner refused (too few measured cameras / spread too
  # wide), never touches the stream box's genlock latency here at all.
  if [ -n "$AV_SYNC_APPLY_OFFSET_MS" ]; then
    echo "[cleanup] #856: applying this run's own computed rig-wide A/V correction (${AV_SYNC_APPLY_OFFSET_MS}ms) to '$STREAM_PROG_SOURCE' on stream (av_sync_calibrate.py --apply, read-back verified)"
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/av_sync_calibrate.py" --host "$STREAM" \
      --password "${OBS_PASSWORD:-}" --source "$STREAM_PROG_SOURCE" \
      --offset-ms "$AV_SYNC_APPLY_OFFSET_MS" --apply \
      --json-path "$OUTDIR/av-sync-last-${RUN_ID}.json" \
      || echo "WARNING: #856 av_sync_calibrate.py --apply failed -- stream genlock latency left at the just-restored prod value; the NEXT run recomputes from its own fresh measurement" >&2
  fi
  timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" teardown --host "$STRIH"
  # #682: restore imag's program scene to whatever it was BEFORE [4a/8] routed it to the
  # camera-under-test. A NO-OP if [4a/8] never ran (IMAG_PREV_SCENE stays its "" pre-trap safe
  # default on an early abort). Best-effort like every other cleanup() restore (warn, never abort
  # the trap) -- a restore failure here must never block the rest of cleanup()'s teardown.
  if [ -n "${IMAG_PREV_SCENE:-}" ]; then
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_phase2.py" switch --host "$IMAG_IP" \
      --program-scene "$IMAG_PREV_SCENE" >/dev/null 2>&1 \
      && echo "    [cleanup] imag: restored program scene to '$IMAG_PREV_SCENE'" \
      || echo "    WARNING #682: could not restore imag's program scene to '$IMAG_PREV_SCENE' -- check manually" >&2
  fi
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
    # issue 1013: an acked-offline imag never had a burn applied ([4b/8] skipped it) and is not
    # reachable to clear/verify — skip its triple so cleanup never WARNs a phantom "burn still on".
    if [ "$_bn" = "imag" ] && [ "$IMAG_OFFLINE_ACKED" = 1 ]; then continue; fi
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" remove --host "$_bip" --input "$_bsrc" 2>&1 \
      | sed "s/^/    [$_bn burn-clear] /" || true
    _vrf="$(timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" check --host "$_bip" --input "$_bsrc" 2>&1 || true)"
    printf '%s\n' "$_vrf" | sed "s/^/    [$_bn burn-verify] /"
    # The block above PROMISES to VERIFY burns OFF; surface a LOUD warning if a burn is still on
    # (e.g. the remove SetInputSettings was swallowed by a transient WS hiccup) so a lingering
    # test-burn onto the live broadcast can't pass silently. (cleanup runs in the EXIT trap, so it
    # WARNS rather than exits non-zero; drift-guard --compare burn_env= is the fail-loud gate.)
    if grep -q 'burn_on=True' <<<"$_vrf"; then
      echo "    [$_bn burn-verify] WARNING #246: genlock_burn still ON after clear — re-clear via" >&2
      echo "        scripts/rig-mode.sh event (or obs_burn_filter.py remove) before any live broadcast." >&2
    fi
  done
  # #938/#1011: the pinned BURN_TARGETS loop above clears only each box's PROGRAM input. An
  # out-of-set input a prior all-cambox/probe run left genlock_burn ON (strih 'NDI cam3'/'NDI cam2',
  # stream 'phase2-probe-src') survives the pinned clear and leaks onto the live broadcast past
  # cleanup (the 2026-08-07 pre-broadcast leak; guard class issue 246/844). Sweep EVERY ndi_source
  # input on each box through the shared exhaustive enumerator (obs_burn_filter.py sweep-off —
  # GetInputList over WS, never a CAMERA_ACTIVE_SET-derived list). timeout-bounded like every other
  # cleanup OBS call so a hung WS op can never block the trap (#328).
  echo "[cleanup] #938/#1011 exhaustive genlock_burn sweep-off on EVERY ndi input (strih/stream/imag)"
  for _swpair in "strih=$STRIH" "stream=$STREAM" "imag=$IMAG_IP"; do
    _swbn="${_swpair%%=*}"; _swbip="${_swpair#*=}"
    timeout "$OBS_CLEANUP_TIMEOUT" python3 "$HERE/obs_burn_filter.py" sweep-off --host "$_swbip" 2>&1 \
      | sed "s/^/    [$_swbn burn-sweep] /" || true
  done
  # #684: FINAL, INDEPENDENT camera-box.service verify -- the LAST thing cleanup() does, for
  # EVERY box this run touched (source cam always; cam2/painter always; cam3-4 when
  # ALL_CAMBOX=1). Live incident (2026-07-11): cam1 was found INACTIVE after BOTH the PR #680 and
  # PR #683 "Full-path E2E" gate runs despite the early-cleanup restore above (#675) -- for the
  # cam1 RUN_ID 1573931971 run (#682) the deploy-launched camera-box-burn-1573931971.service
  # itself deactivated cleanly ~11 min after launch (journalctl on cam1: "Started ... 08:46:00" /
  # "Deactivated successfully ... 08:57:10" -- consistent with THIS cleanup() actually having
  # run), yet camera-box.service was never observed restarting until a human found it down 55 min
  # later and hand-started it -- something between the early restore and the true end of
  # cleanup() left the source cam down with no trace of a second attempt. This pass is a cheap,
  # IDEMPOTENT camera_box_verify_active_cmds call used STANDALONE (no preceding stop/pkill/rm --
  # nothing here tears anything down): a healthy box costs one quick SSH round trip; a box the
  # early restore missed gets a genuine INDEPENDENT restart attempt + the same loud #675 WARNING
  # if that ALSO fails, so it can never again go undetected until a human stumbles on it later.
  echo "[cleanup] #684 FINAL camera-box.service verify (every box this run touched, independent of the restore above)"
  timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
    "$(camera_box_verify_active_cmds "$CAMERA_NAME (source, $CAM1_IP) FINAL")" || true
  if [ "${ALL_CAMBOX:-0}" = "1" ]; then
    for _cfcn in $(camera_active_secondary_set); do
      _cfip="$(camera_secondary_ip "$_cfcn")"
      timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cfip" \
        "$(camera_box_verify_active_cmds "$_cfcn ($_cfip) FINAL")" || true
    done
  fi
  timeout "$CLEANUP_SSH_TIMEOUT" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$PAINTER_IP" \
    "$(camera_box_verify_active_cmds "cam2/painter, $PAINTER_IP FINAL")" || true
  # issue 808: RESTORE bkshading-relay on the two boxes the [0/8] pause step above stopped, but
  # ONLY where it found the relay genuinely ACTIVE beforehand -- never re-activates a relay the
  # operator deliberately silenced. Best-effort (never blocks the trap, #328/#649/#712/#713) and
  # placed LAST so this non-safety-critical restore never delays the device-restore phase above.
  echo "[cleanup] #808 bkshading-relay restore (was-active: $CAMERA_NAME=$BKSH_PAUSE_CAM1_WAS_ACTIVE, cam2=$BKSH_PAUSE_PAINTER_WAS_ACTIVE)"
  bkshading_e2e_pause_restore "$CAMERA_NAME" "$CAM1_IP" "$CAM_PW" "${BKSH_PAUSE_CAM1_WAS_ACTIVE:-0}"
  bkshading_e2e_pause_restore cam2 "$PAINTER_IP" "$CAM_PW" "${BKSH_PAUSE_PAINTER_WAS_ACTIVE:-0}"
}
# #657: a plain foreground `sleep N` defers ALL signal handling — trapped OR default — until
# that `wait4()` syscall returns on its own, i.e. until the sleep completes naturally. This is
# documented bash trap behavior, not an orphaning artifact: empirically confirmed live
# (2026-07-10) against the REAL self-hosted dev1 Actions runner — a `gh run cancel` delivered
# SIGINT, then SIGTERM ~7.5s later, then an untrappable "kill entire process tree" ~2.5s after
# that (the runner's OWN documented escalation, confirmed via its Worker log), while a bash
# process sat inside a bare `sleep 180`, and the EXIT/HUP/INT/TERM trap NEVER got a chance to
# run at all — the whole process was killed by the runner's escalation before the deferred trap
# could ever fire. This is exactly recording-e2e.sh's own recording window (below: [6/8] steady
# state + the ALL_CAMBOX per-segment wait) — a cancellation mid-recording would defer
# cleanup()'s (#649 StopRecord-first) trap for the ENTIRE remaining DURATION (300-1810s), far
# past the runner's ~10s grace window, so the trap effectively never runs on a live cancel.
#
# `wait` (unlike directly awaiting a foreground external command) IS documented — and here
# empirically verified against the real runner — to return immediately once a trapped signal
# arrives, even mid-wait, with an exit status > 128. So: background the sleep and `wait` on it
# instead of blocking on it directly. If `wait` returns EARLY (status > 128), the EXIT/HUP/INT/
# TERM trap has ALREADY run cleanup() (synchronously, before `wait` returns control) — kill the
# now-superfluous background sleep and `exit` immediately, rather than letting the script
# blunder on into the rest of the harness as though the recording had completed normally
# (re-StopRecording an already-stopped box, downloading/decoding a run that was never meant to
# complete).
interruptible_sleep() {
  local secs="$1" pid rc
  sleep "$secs" &
  pid=$!
  wait "$pid"
  rc=$?
  if [ "$rc" -gt 128 ]; then
    kill "$pid" 2>/dev/null || true
    echo "interruptible_sleep: interrupted (signal $((rc - 128))) before ${secs}s elapsed -- exiting (cleanup already ran via trap, #657)" >&2
    exit "$rc"
  fi
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
# #691 belt-and-braces (OPTIONAL, empty by default): the known-good calibrated prod
# genlock_latency_ms_src for $STREAM_PROG_SOURCE, from av-sync-last.json on the stream
# box's own ProgramData — gathered by the operator/agent (this script has no ssh/scp path
# to read the Windows box's filesystem directly) and passed in for cleanup()'s teardown
# call to cross-check the restored value against. Declared HERE (BEFORE the cleanup trap
# installs, same reasoning as the *_PROG_SOURCE vars above) so cleanup() never `set -u`-
# aborts referencing it on an early abort. Empty (unset) = the check is silently skipped —
# never a hard requirement, matches drift-guard.sh's av_sync_calibrated_ms convention for
# the SAME file.
AV_SYNC_CALIBRATED_MS="${AV_SYNC_CALIBRATED_MS:-}"
# #856: THIS run's own computed rig-wide A/V correction (offset in ms), set at [8/8g] once the
# merge verdict's own all_cambox_av_sync measurements are available (E2E_EXECUTE_VERDICT=1
# only). Declared HERE (empty default, BEFORE the cleanup trap installs, same reasoning as
# AV_SYNC_CALIBRATED_MS/IMAG_PREV_SCENE above) so cleanup() never `set -u`-aborts referencing
# it on an early abort -- an early abort naturally never sets it, so cleanup()'s own #856 step
# does nothing extra. See that step (right after the stream teardown restore call) for why the
# apply must happen THERE (last), not at [8/8g] itself: the delivery-verify snapshot/restore
# that ALWAYS runs on exit would otherwise silently overwrite whatever [8/8g] computed.
AV_SYNC_APPLY_OFFSET_MS="${AV_SYNC_APPLY_OFFSET_MS:-}"
# #462 (EPIC #466): imag-nb's program-feeding NDI input — the #399-style 1:1 mapping from Phase 1
# (setup-imag.sh) pins 'NDI CAM1'..'NDI CAM6' -> 'CAMx (usb)' 1:1. issue 1204: DERIVE this per
# camera-under-test via imag_source_for_camera "$CAMERA_NAME" (the SAME resolution IMAG_PROG_SCENE
# uses below), so the burn target ALWAYS matches the input backing imag's routed program scene —
# it was previously hard-pinned to the literal 'NDI CAM1', which diverged from the program route
# the moment CAMERA_NAME != cam1 (cam1 offline-acked, active set = cam3 -> imag recorded zero
# 911003 anchors, run 32908274448). rig-mode.sh TEST mode is what actually routes imag's PROGRAM
# onto that scene + toggles this burn ON; this harness defensively ensures/verifies it too (the
# SAME "single source of truth" BURN_TARGETS array, extended below) AND cross-checks it against the
# genuinely-rendered input at [4a/8].
IMAG_PROG_SOURCE="${IMAG_PROG_SOURCE:-$(imag_source_for_camera "$CAMERA_NAME")}"
# #682: imag's OWN scene showing the camera-under-test -- resolved per-camera (never hardcoded to
# 'Cam 1' like rig-mode.sh's set_imag_test_program(), which only ever routes cam1). Declared here
# (BEFORE the cleanup trap installs, same reasoning as the *_PROG_SOURCE vars above) so cleanup()
# never `set -u`-aborts referencing it on an early abort. IMAG_PREV_SCENE stays "" (its #246-style
# safe default) until [4a/8] actually captures imag's PRE-route scene -- cleanup()'s restore is a
# no-op until then.
IMAG_PROG_SCENE="${IMAG_PROG_SCENE:-$(imag_scene_for_camera "$CAMERA_NAME")}"
IMAG_PREV_SCENE=""
# #252: single source of truth for the host=ip=source burn triples. The #195 pre-record burn-ON
# gate and the #246 cleanup() burn-clear loop iterate the SAME set; keeping it in one array means a
# third box (or a triple-structure change) can never green-light a set the cleanup does not clear
# (the #246 linger-onto-live-broadcast hazard). Defined HERE — after the *_PROG_SOURCE vars and
# BEFORE the cleanup trap is armed — so cleanup()'s array expansion is never an unbound `set -u`
# var on an early abort (same ordering reason the *_PROG_SOURCE vars precede the trap). #462: imag
# is now a THIRD burn target (its own 911003 digital corner burn, #463) — the exact extension this
# array's design already anticipated.
BURN_TARGETS=("strih=$STRIH=$STRIH_PROG_SOURCE" "stream=$STREAM=$STREAM_PROG_SOURCE" "imag=$IMAG_IP=$IMAG_PROG_SOURCE")
# #649: "did THIS harness itself start recording on <box>" flags — default UNSET (0) here, before
# the trap arms, so an EARLY abort (before [5/8] ever runs) leaves cleanup()'s StopRecord-first
# block a no-op on all three boxes. Each flips to 1 ONLY right after that box's OWN StartRecord
# actually succeeds ([5/8] below). cleanup() stops ONLY flagged boxes — a blind StopRecord on a
# box this run never started recording on could kill a REAL broadcast's recording if a run gets
# cancelled while still waiting on an earlier gate (the #649 "worse than the original bug" case).
STRIH_RECORDING_STARTED=0
STREAM_RECORDING_STARTED=0
IMAG_RECORDING_STARTED=0
# #286 ALL_CAMBOX — strih's OWN render-time burn (911002) must be present on WHICHEVER strih
# NDI input the sweep currently has cut into program, not just the single default
# STRIH_PROG_SOURCE (cam1's mapped input under the plain single-camera path). Without this,
# recording-verdict's all_cambox_delivery_latency (#286: strih_burn.gen_ts_ns -
# camera_burn.gen_ts_ns, the metric that actually proves the phase-sync fix, as opposed to the
# SOURCE-side all_cambox_latency/#624 which never touches strih's receiver-side genlock hold)
# only ever measures whichever ONE camera happens to already own STRIH_PROG_SOURCE — every other
# camera's window shows a strih-recorded frame with its OWN capture burn but NO strih burn to
# pair it against, so it reports zero samples (confirmed live, 2026-07-09 #286 re-verification:
# only the STRIH_PROG_SOURCE camera measured, the other 5 all "NO SAMPLES"). Extend the SAME
# single-source-of-truth array the #195 ON-gate and the #246 cleanup() OFF-clear loop already
# iterate — so this fix automatically covers BOTH ends, never a burn left on that cleanup
# forgets to clear. #827 (2026-07-27, binding owner directive): the strih NDI inputs iterated
# here DERIVE from CAMERA_ACTIVE_SET (camera-set.sh) — never a second hardcoded list — so
# re-enabling a retired camera (e.g. cam5) is a one-line CAMERA_ACTIVE_SET edit, picked up here
# automatically. A camera genuinely down that run simply never shows a window, so its burn being
# on-but-unused is harmless (never fabricates a measurement, matching the "never fabricate"
# convention n_camera_strih_samples/spread_verdict already follow).
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _acn in $CAMERA_ACTIVE_SET; do
    _acs="NDI $_acn"
    if [ "$_acs" != "$STRIH_PROG_SOURCE" ]; then
      BURN_TARGETS+=("strih-${_acs// /_}=$STRIH=$_acs")
    fi
  done
fi
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
  # #365/#405/#137/#109/#438/#272/#757: build the default-feature gate binaries (no probe deps,
  # no disk balloon). phase-sync-gate + genlock-jitter-report were MISSING here before #757
  # (confirmed: the #756 Member 3 pins-snapshot script's "recommended_pins" feature has been
  # silently degraded to empty ever since it shipped, because phase-sync-gate was never actually
  # on $PROBE_BIN_DIR -- the pre-Discord-report gather site already treats that as a
  # soft/best-effort failure, so it never surfaced). Adding both here fixes that AND is required
  # by #757's new step 4f (a pre-record phase auto-pin, see further down this file)
  # pre-record phase auto-pin step (phase-sync-gate computes the offsets, genlock-jitter-report
  # parses the calibration window's audit log).
  cargo build --release --bin frozen-camera-gate --bin render-budget-gate --bin av-restart-sync-gate --bin zero-loss-restart-gate --bin phase-sync-gate --bin genlock-jitter-report --bin phase-sync-active-floor-gate  # airuleset:build-ok
fi

# [1/8] frame-probe (cam2 painter) sha-pin report (#1138) — the loud PIN that CONFIRMS the [0/8]
# frame-probe auto-align above. Expected = FRAME_PROBE_ALIGN_CI_BIN (the clean probe-tools CI
# artifact the [0/8] align fetched + deployed to cam2 — the TRUE deploy source of truth), falling
# back to $PROBE_BIN_DIR/frame-probe when the align was skipped (--no-main-pin soak) or could not
# fetch (gh unavailable). Pinning against the CI artifact rather than the dev1 LOCAL build makes the
# sha compare exact (both sides = the same artifact bytes, so no build-reproducibility dependency).
# The report-only mode runs ONLY the report (no second camera-box parity table) and ALWAYS exits 0;
# the `|| true` is belt-and-suspenders — a residually-lagging painter (align could not complete)
# SCREAMS + names the fix but never fails this run (the hard-gate flip is the supervisor's #758
# two-step follow-up once the auto-align is rig-proven). cam2-scoped: frame-probe is installed ONLY
# on the painter box (setup-device.sh STEP 3b, cam2_is_painter_box).
echo "[1/8] frame-probe (cam2 painter) sha-pin report — deployed painter vs the candidate CI build (#1138, report-only, confirms the [0/8] align)"
"$HERE/camera-box-version-gate.sh" \
  --frame-probe-only \
  --frame-probe-expected-bin "${FRAME_PROBE_ALIGN_CI_BIN:-$PROBE_BIN_DIR/frame-probe}" \
  --linux "cam2=root@$PAINTER_IP" || true

# #758 item 1 (continued) — per-camera NDI liveness. Needs frozen-camera-gate (just
# built/fetched above), so it cannot run at true [0/8] — this is still comfortably BEFORE any
# deploy/OBS-touching step (StartRecord is 4 more steps away).
#
# #761/#763 (2026-07-15) — the probe target is now PER-BOX, not a single shared convention:
# STRIH switched its "MV Cam N" scenes to SAME-SOURCE (user-directed, #761 KEPT): they now
# render the MAIN "NDI camN" input (scene-item scaled) and the old "MV NDI camN" low-bandwidth
# CLONE items are DISABLED in those scenes — probing the disabled clones always reads FROZEN
# (a disabled scene item never renders, no projector/warm-up trick can activate it) even on a
# perfectly healthy camera. So on strih this preflight now probes the MAIN "NDI camN" inputs
# instead. This is SAFE without frozen-camera-gate.py's own #747 PREVIEW warm-up (still passed
# --warm-settle 0, i.e. disabled, unchanged): the built-in OBS Multiview grid projector renders
# EVERY scene's thumbnail continuously while open (not gated on program/preview state), so as
# long as that projector stays open (the rig's normal state) every "Cam N" scene — and hence
# its "NDI camN" input — is ALWAYS actively rendering, structurally, with no lazy-activation
# false positive (the exact risk the old "4 frozen inputs" panic this comment used to warn
# about). IMAG was REVERTED to the low-bandwidth "MV CAMN" clone approach (full 1080p x7 does
# not fit its render budget, #763 tracks unifying the two boxes onto one model later) — imag has
# no equivalent frozen-camera-gate call site today, but if one is ever added it must target the
# CLONES, not the mains, mirroring this same per-box split.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  echo "[1/8] per-camera NDI liveness via strih's main NDI inputs (#758/#761)"
  # #827 follow-up (2026-07-28): derive the sampled source list from CAMERA_ACTIVE_SET
  # (camera-set.sh) minus any acked-offline box -- never a literal 1..7 range. This is the exact
  # call site that sampled retired "NDI cam5"/"NDI cam6"/"NDI cam7" and failed FROZEN on live run
  # 30310110884 (their strih OBS inputs still exist, but they are retired and never emit).
  PREFLIGHT_MV_SOURCES="$(camera_active_ndi_sources_excluding_csv "$PREFLIGHT_EXCLUDED_CAMS")"
  if [ -n "$PREFLIGHT_MV_SOURCES" ]; then
    python3 "$HERE/frozen-camera-gate.py" --host "$STRIH" --password "" \
      --sources "$PREFLIGHT_MV_SOURCES" --samples 2 --cadence 3.5 --threshold 1 --warm-settle 0 \
      --verdict-bin "$PROBE_BIN_DIR/frozen-camera-gate" \
      || {
        echo "ERROR: [preflight] FAIL: one or more camera NDI inputs show NO pixel change across ~3.5s — a camera leg looks frozen/dead before this run even started. Investigate the named camera's NDI sender (see the frozen list above)." >&2
        exit 1
      }
  fi

  # #758 preflight item — imag RENDER-HEALTH: with the Multiview projector verified OPEN
  # (the [0/8] step above), the PROGRAM compositor must still hold its OWN 60fps frame budget
  # BEFORE any deploy/recording starts — the user's binding demand ("ako to ze to nezachytili
  # preflight testy!!!!" — because this check did not exist yet). Reuses the EXISTING #405
  # render-budget-gate.py + render-budget-gate Rust binary (render_budget::classify) — the
  # SAME strict verdict the later [4d/8] burns-on gate already applies, just run here earlier
  # (MV open, no burns yet) and SUSTAINED across several independent windows (never one
  # averaged window, so a transient mid-window dip cannot be averaged away). classify()'s own
  # 60fps thresholds (2fps tolerance, 1000/60≈16.67ms budget) already ARE the "activeFps >= 58 /
  # averageFrameRenderTime < 16.7ms" bar from the #758 spec — no new threshold invented here,
  # single source of truth stays the Rust binary.
  # issue 1013: skip the imag render-health + MV-divisor leg when imag is acked-offline (both
  # hard-abort exit 1 on an absent box). Body kept at its original indent under this guard
  # (bash-legal; substring anchors, not indentation), matching the [0/8] imag OBS-prep guard above.
  if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
    imag_leg_skip_note "[1/8] imag render-health + MV-divisor capability" "$IMAG_OFFLINE_ACK_REASON"
  else
  # issue 1230: the #1218 imag active-set NDI idle enforce pass was REMOVED here (owner ruling
  # 2026-08-30: no idle policy). imag now keeps all seven cameras named + alive; name recovery is
  # the on-box --bootstrap seed's enforce_ndi_names + the existing name-heal paths, not an E2E pass.
  # #1143: make VAAPI-tex the LIVE record encoder BEFORE the render-health windows (software x264
  # overloads the render thread → the #1130 observer effect). The make-it-live OBS restart fires
  # only when the disk config drifted from the target (a no-op on the steady state); its settle is
  # absorbed by the #882/#1232 settle-adaptive warm-up phase below. Best-effort — a nonzero return
  # WARNs, never aborts.
  if ! python3 "$HERE/imag_scenes.py" --ensure-rec-encoder --host "$IMAG_IP"; then
    echo "    WARNING: #1143 imag ensure-rec-encoder nonzero (best-effort) — continuing; the render-health preflight below still catches a down OBS, record_render_lagged_pct a stale encoder" >&2
  fi
  echo "[1/8] imag render-health preflight — PROGRAM must hold its 60fps budget with MV open, sustained (#758)"
  # #1232: RENDER_HEALTH_WINDOWS is now the number of STRICT windows required to FOLLOW the
  # settle-adaptive warm-up phase (was: the fixed TOTAL window count, of which window 1 alone was
  # the non-counting warm-up) -- so the default drops from 5 (1 warm-up + 4 strict) to 4 (the same
  # 4 strict windows, now following a phase that can absorb more than one leading failure).
  RENDER_HEALTH_WINDOWS="${RENDER_HEALTH_WINDOWS:-4}"
  RENDER_HEALTH_WINDOW_S="${RENDER_HEALTH_WINDOW_S:-6}"
  RENDER_HEALTH_SETTLE_BUDGET_S="${RENDER_HEALTH_SETTLE_BUDGET_S:-60}"
  _rhw=0
  _rhw_first_pass_seen=0
  _rhw_strict_passed=0
  _rhw_start_s="$(date +%s)"
  while :; do
    _rhw=$((_rhw + 1))
    # #882/#1232: capture the REAL python exit code via PIPESTATUS (not the pipeline's own
    # `sed`-decided status) inside an `if`, which is exempt from `set -e` regardless of AND/OR
    # position -- so a failing warm-up window never aborts the script before
    # render_health_phase_outcome gets to decide.
    if OBS_PASSWORD_IMAG="${OBS_PASSWORD_IMAG:-${OBS_PASSWORD:-}}" \
        python3 "$HERE/render-budget-gate.py" \
        --box "imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}" \
        --window-s "$RENDER_HEALTH_WINDOW_S" --verdict-bin "$PROBE_BIN_DIR/render-budget-gate" \
        2>&1 | sed "s/^/    [imag render-health w${_rhw}] /"; then
      _rhw_rc=0
    else
      _rhw_rc="${PIPESTATUS[0]}"
    fi
    _rhw_elapsed_s=$(( $(date +%s) - _rhw_start_s ))
    # #1232 review finding (🟡): capture the PRE-call phase state so the FAIL branch below can
    # tell apart its two distinct causes (a strict-phase regression vs a warm-up that never
    # settled) -- render_health_phase_outcome overwrites _rhw_first_pass_seen on the next line.
    _rhw_pre_seen="$_rhw_first_pass_seen"
    _rhw_phase="$(render_health_phase_outcome "$_rhw_rc" "$_rhw_first_pass_seen" "$_rhw_elapsed_s" "$RENDER_HEALTH_SETTLE_BUDGET_S")"
    _rhw_outcome="$(printf '%s\n' "$_rhw_phase" | sed -n 's/^outcome=//p')"
    _rhw_first_pass_seen="$(printf '%s\n' "$_rhw_phase" | sed -n 's/^first_pass_seen=//p')"
    _rhw_counts_as_strict="$(printf '%s\n' "$_rhw_phase" | sed -n 's/^counts_as_strict=//p')"
    case "$_rhw_outcome" in
      PASS)
        if [ "$_rhw_counts_as_strict" = "1" ]; then
          _rhw_strict_passed=$((_rhw_strict_passed + 1))
        fi
        if [ "$_rhw_strict_passed" -ge "$RENDER_HEALTH_WINDOWS" ]; then
          echo "[preflight] imag render-health settled after ${_rhw} total window(s) (${_rhw_strict_passed}/${RENDER_HEALTH_WINDOWS} strict passes, #882/#1232)."
          break
        fi
        ;;
      WARMUP)
        echo "WARN: [preflight] imag render-health window ${_rhw} FAILED but is still inside the settle-adaptive WARM-UP phase (post-restart NDI-lock/shader settle, elapsed ${_rhw_elapsed_s}s of ${RENDER_HEALTH_SETTLE_BUDGET_S}s budget — #882/#1232) — tolerated, continuing until the first PASS." >&2
        ;;
      *)
        # #1232 review finding (🟡): the two FAIL causes get a DIFFERENT diagnostic context --
        # _rhw_pre_seen=1 means a strict window regressed after warm-up already ended cleanly;
        # _rhw_pre_seen=0 means the box never achieved a single PASS before the settle budget ran
        # out. The operator-facing FAIL wording below stays a SINGLE occurrence in this file
        # (tests/harness_render_health_divisor_758.rs anchors it verbatim) -- only the
        # parenthetical context differs at runtime, never the fixed prefix/suffix text.
        if [ "$_rhw_pre_seen" = "1" ]; then
          _rhw_fail_context="window ${_rhw}, strict-phase regression after ${_rhw_strict_passed} clean strict window(s)"
        else
          _rhw_fail_context="window ${_rhw}, box NEVER settled within the ${RENDER_HEALTH_SETTLE_BUDGET_S}s warm-up budget (elapsed ${_rhw_elapsed_s}s)"
        fi
        echo "ERROR: [preflight] FAIL: imag render pod budgetom s MV otvoreným (${_rhw_fail_context} — #882/#1232) — skontroluj divisor/projektory/zataz." >&2
        exit 1
        ;;
    esac
  done

  # #758 preflight item — MV render-DIVISOR CAPABILITY check (a #756 follow-up item). Distinct
  # from BOTH the #756 build-SHA version-parity check above (same build everywhere) and the
  # render-health OUTCOME gate just above (program stays inside its frame budget): this asks
  # whether the deployed imag frontend binary was actually BUILT WITH the #276/#278/#293
  # multiview render-divisor decouple (the exported obs_display_set_render_divisor symbol).
  # obs-websocket's GetStats has no field reporting per-display divisor/throttle state, so the
  # only real evidence available is the SAME nm -D -u symbol check scripts/setup-imag.sh already
  # runs at provisioning time (#499) — a read-only query of the deployed binary over SSH, no
  # config mutation.
  #
  # #756 (2026-07-15): FLIPPED from WARN-only to FAIL-by-default. The earlier "known gap" belief
  # (that the divisor symbol was missing on imag's Linux frontend) was reached by grepping the
  # OBS frontend LOG for #276/#278/divisor/multiview text markers — but no such log line has
  # EVER existed anywhere in the vendored source (obs-display.c / OBSProjector.cpp never blog()
  # the divisor), so "zero markers" was never real evidence. This EXACT nm -D -u check, run live
  # against the currently-deployed imag frontend (GENLOCK_BUILD_SHA.txt=26de1c3c2), shows
  # count=1 — the symbol genuinely IS referenced (the #276/#278/#293 patch, commit a50fa5a18, is
  # an ancestor of every build since). The render-health failures that motivated the original
  # WARN-only stance were a SEPARATE bug (7 stray accumulated Multiview/Program projector
  # windows — see the #756 projector-count preflight above), now fixed independently. A missing
  # symbol on any FUTURE imag build is therefore a real regression, not an expected gap — hard
  # fail. Flipping back to WARN, if ever needed, is exactly this ONE line:
  IMAG_DIVISOR_CAPABILITY_FAIL="${IMAG_DIVISOR_CAPABILITY_FAIL:-1}"
  # #833: preflight the `nm` tool itself BEFORE shelling it below — nm ships in binutils, and a
  # missing binutils (the #822 provisioning gap) must fail loud BY NAME here, never be silently
  # misread downstream as "MV divisor capability MISSING (#756 regression)" when the real cause
  # is that the check could not even run.
  _imag_nm_probe="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_require_remote_tool_cmd nm)" 2>/dev/null || true)"
  _imag_nm_missing="$(imag_remote_tool_probe_missing "$_imag_nm_probe")"
  if [ -n "$_imag_nm_missing" ]; then
    echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}): required tool(s) not installed: ${_imag_nm_missing} (apt-get install -y binutils) — refusing to run the divisor-capability check that cannot execute without it (#822/#833 class)." >&2
    exit 1
  fi
  echo "[1/8] imag MV render-divisor capability check (#756)"
  _divisor_nm_count="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" \
    'nm -D -u /usr/bin/obs 2>/dev/null | grep -c obs_display_set_render_divisor || true' \
    2>/dev/null || true)"
  if [ "${_divisor_nm_count:-0}" -gt 0 ] 2>/dev/null; then
    echo "    ok: imag /usr/bin/obs references obs_display_set_render_divisor — MV render-divisor capability present"
  else
    _divisor_msg="MV divisor capability MISSING on imag (#756 regression) — /usr/bin/obs does not reference obs_display_set_render_divisor; the multiview render-budget decouple is not compiled into imag's Linux frontend"
    if [ "$IMAG_DIVISOR_CAPABILITY_FAIL" = "1" ]; then
      echo "ERROR: [preflight] FAIL: ${_divisor_msg}" >&2
      exit 1
    fi
    echo "[preflight] WARN: ${_divisor_msg}"
  fi
  fi   # issue 1013: end of the IMAG_OFFLINE_ACKED skip-guard over the imag render-health + divisor leg
fi

echo "[2/8] $CAMERA_NAME (${CAM1_IP}) — probe-featured camera-box with the #174 capture BURN (emits NDI w/ $CAMERA_NAME mark, NO grab #179)"
# #174 + #179: deploy the freshly-built PROBE-featured camera-box (carries the #174 capture
# burn) to a $CAMERA_NAME-LOCAL /tmp path and launch THAT — NOT the prod
# /usr/local/bin/camera-box (the clean production binary with no burn). The burn is
# runtime-gated by CAMERA_BOX_BURN_RUN_ID, so it draws the resolved SOURCE camera's own
# run_id (#24: $SRC_BURN_RUN_ID, matching $CAMERA_NAME) + per-emit frame_id + CAPTURE
# wall-clock ts into the EMITTED frame, which rides through NDI → strih → stream. #179: the
# grab-record flags are GONE — the burn mark in the stream recording fully replaces the
# 7.3GB grab, so the SOURCE camera just emits NDI with the burn. Apply the device's OWN
# --list-ctrls colour default directly here (#338/#312: the old sharp set saturation=0/
# contrast=75 hurt the decode and tinted the picture; device defaults read clean). #744: this
# used to be a hardcoded LITERAL saturation=50/contrast=50 -- correct only on the ShadowCast 2's
# own 0-100 range, but ~39% of the Elgato 4K S's own 0-255 default (128), producing a dark/
# chroma-muted picture. scripts/lib/v4l2-neutral.sh resolves each card's OWN default instead, on
# whatever /dev/videoN the kernel currently assigns (never a hardcoded /dev/video0, #728).
# #668: deploy under a TRANSIENT systemd-run unit (Restart=on-failure), not a bare `nohup ... &`.
# A bare nohup'd process has NOTHING watching it — when a real #656/#663 self-heal fires mid-test
# on this SOURCE camera (it exits(77) BY DESIGN, expecting systemd's Restart=always to bring it
# back — src/capture_rate_selfheal.rs), the ad-hoc nohup'd burn process just dies for the rest of
# the recording window, silently losing this camera's digital-burn measurement (live evidence,
# #668: RUN_ID 1783724370 — cam1's burn ids stopped dead at id 4738 the instant the self-heal's
# USB reset exited it, ~290s before the recording ended). Wrapping it in a transient unit with
# Restart=on-failure gives the self-heal's own respawn expectation a REAL systemd unit to hold —
# no change to capture_rate_selfheal.rs itself, the ad-hoc test deploy now behaves exactly like
# the production camera-box.service it's standing in for. cleanup() stops this unit explicitly
# (an explicit `systemctl stop` is never followed by an automatic restart) before the belt-and-
# suspenders pkill, so a stopped test can never leave a unit trying to respawn.
# #894: StartLimitIntervalSec=0 disables systemd's default 5-in-10s restart-burst limit. Without
# it, a device-steal race (the udev hotplug rule restarting production and taking /dev/videoN
# back mid-run, #894's own root cause) exhausts the burst limit within ~15s and the unit gives up
# PERMANENTLY at 1/FAILURE -- even after the device becomes free again. Unlimited retry means the
# burn unit's next scheduled attempt (every RestartSec=3) reclaims the device instead of dying
# forever, once the #894 udev fix stops production from re-stealing it.
# issue 1198: this burn-transient unit must also carry the issue-1193 over-rate self-heal
# env, mirroring cam2's production overrate-selfheal-canary drop-in -- a burn session that
# opens the ShadowCast grabber in its sustained over-rate mode (roughly 61.3 fps) must
# self-heal (USB reset + exit 77 + this unit's own on-failure restart policy) exactly like
# production does, instead of degrading every cadence window for the rest of the run
# (three burn runs on 2026-08-30 stayed degraded the whole session for lack of this env).
CAM1_BURN_BIN="/tmp/camera-box-burn-${RUN_ID}"
CAM1_BURN_UNIT="camera-box-burn-${RUN_ID}"
# #749: sweep stale binaries BEFORE the scp below -- a full /tmp (a prior run's own cleanup()
# never landing) must never block THIS run's own deploy. Best-effort (never fail the harness).
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(tmp_burn_sweep_stale_cmds)" 2>/dev/null || true
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  "$PROBE_BIN_DIR"/camera-box root@"$CAM1_IP":"$CAM1_BURN_BIN"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM1_IP" \
  "$(camera_box_deadman_arm_cmds "$CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN") \
   systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
   chmod +x $CAM1_BURN_BIN; \
   $(v4l2_neutral_resolve_node_cmd) \
   i=0; while fuser -s \$V4L2_NEUTRAL_NODE 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   $(v4l2_neutral_set_default_cmd) \
   $(cpu_affinity_burn_resolve_cmd) \
   rm -f /tmp/cbox-burn.log; \
   systemd-run --unit=$CAM1_BURN_UNIT --collect \
     --property=Restart=on-failure --property=RestartSec=3 --property=StartLimitIntervalSec=0 \
     --property=StandardOutput=append:/tmp/cbox-burn.log --property=StandardError=append:/tmp/cbox-burn.log \
     \$CPU_AFFINITY_BURN_PROPERTY \
     --setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1 --setenv=CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS --setenv=CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID \
     --setenv=CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt --setenv=NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
     $CAM1_BURN_BIN"
sleep 4  # let $CAMERA_NAME's NDI sender (with the burn) become discoverable
# #1093 (a): cam1's picture is cam2-painter's HDMI, so prove the painter is genuinely PAINTING
# before the cam-pixel probe -- a mid-restart painter must not read as a false dead leg (ordering,
# not a longer blind window). ONCE, here only: the ALL_CAMBOX loop below runs while the painter is
# deliberately stopped ([2b/8] -> [3/8]), so it must not wait on it. WARN-only (never blocks).
mv_reverify_painter_up_wait "$CAM_PW" "$PAINTER_IP"
mv_reverify_or_escalate "$CAMERA_NAME" "${CAMERA_NAME#cam}" || exit 1

# #624/#312: the ALL_CAMBOX sweep also cuts cam2/cam3/cam4 into strih program —
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
# every other active secondary camera (unit doesn't exist there, `2>/dev/null || true` swallows
# it) — but is REQUIRED on cam2 to avoid the #328/#440 two-painters-fighting-over-fb0 bug (the
# permanent service and this loop's transient probe-featured binary must never both hold
# fb0/run at once).
#
# #827 (2026-07-27, binding owner directive): the deploy list is cam2 + every camera in
# camera_active_secondary_set() (camera-set.sh) — the ONE place fleet membership is declared.
# Re-enabling a retired camera (e.g. cam5) is adding it to CAMERA_ACTIVE_SET there; this loop
# picks it up automatically, no change needed here.
# issue 1170: cam2 is deployed as a camera-under-test node ONLY while it is a MEASURED camera
# (cam2 in CAMERA_ACTIVE_SET) — gated on the exact same `camera_is_active cam2` check just below,
# currently TRUE (cam2 is back in the default active set as of issue 1198: the owner ruled the
# ShadowCast "hardware-defective" diagnosis wrong and refused the card swap this section used to
# describe). Its PAINTER role (the painter step below, keyed off PAINTER_IP) is UNAFFECTED either
# way. Dropping "cam2" from CAMERA_ACTIVE_SET (camera-set.sh) again would drop this deploy too, one
# line, no other edit needed. The list starts EMPTY and is conditionally seeded so a run whose
# active set excludes cam2 genuinely excludes it here too (before this the cam2 seed was
# unconditional, keyed off PAINTER_IP, so plain set-removal did not exclude it).
CAMBOX_SECONDARY_DEPLOY=()
if camera_is_active cam2; then
  CAMBOX_SECONDARY_DEPLOY+=("cam2=$PAINTER_IP=$BURN_CAM2_RUN_ID")
fi
for _scn in $(camera_active_secondary_set); do
  CAMBOX_SECONDARY_DEPLOY+=("${_scn}=$(camera_secondary_ip "$_scn")=$(camera_secondary_burn_run_id "$_scn")")
done
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _cn_ip_burn in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _cn="${_cn_ip_burn%%=*}"; _crest="${_cn_ip_burn#*=}"; _cip="${_crest%%=*}"; _cburn="${_crest#*=}"
    echo "[2b/8] $_cn (${_cip}) — probe-featured camera-box with its OWN capture BURN (run_id=$_cburn, #624/#312 ALL_CAMBOX)"
    _cbin="/tmp/camera-box-burn-${_cn}-${RUN_ID}"
    # #668: same transient systemd-run unit (Restart=on-failure) as cam1's [2/8] deploy above —
    # a mid-test self-heal on any of these boxes now respawns, instead of silently dying for the
    # rest of the run.
    _cunit="camera-box-burn-${_cn}-${RUN_ID}"
    _cnodisplay_setenv=""
    if [ "$_cn" = "cam2" ]; then _cnodisplay_setenv="--setenv=CAMERA_BOX_NO_DISPLAY=1 "; fi
    # #749: same pre-scp stale-binary sweep as cam1's [2/8] site above -- each ALL_CAMBOX box has
    # its own independent 100MB /tmp tmpfs that can fill the same way.
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
      "$(tmp_burn_sweep_stale_cmds)" 2>/dev/null || true
    sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
      "$PROBE_BIN_DIR"/camera-box root@"$_cip":"$_cbin"
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$_cip" \
      "systemctl stop cam2-painter 2>/dev/null || true; \
       $(camera_box_deadman_arm_cmds "$CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN") \
       systemctl stop camera-box; pkill -x camera-box 2>/dev/null; \
       chmod +x $_cbin; \
       $(v4l2_neutral_resolve_node_cmd) \
       i=0; while fuser -s \$V4L2_NEUTRAL_NODE 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
       $(v4l2_neutral_set_default_cmd) \
       $(cpu_affinity_burn_resolve_cmd) \
       rm -f /tmp/cbox-burn-${_cn}.log; \
       systemd-run --unit=$_cunit --collect \
         --property=Restart=on-failure --property=RestartSec=3 --property=StartLimitIntervalSec=0 \
         --property=StandardOutput=append:/tmp/cbox-burn-${_cn}.log --property=StandardError=append:/tmp/cbox-burn-${_cn}.log \
         \$CPU_AFFINITY_BURN_PROPERTY \
         ${_cnodisplay_setenv}--setenv=CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL=1 --setenv=CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS --setenv=CAMERA_BOX_BURN_RUN_ID=$_cburn \
         --setenv=NDI_RUNTIME_DIR_V6=/usr/lib/ndi \
         $_cbin"
  done
  sleep 4  # let cam2 + every active secondary camera's NDI senders (with their burns) become discoverable
  # #758 item 2 — sender-bounce re-verify each box's own MV clone, right after the shared settle
  # sleep above (a SEPARATE pass over the same box list, so the settle timing above is unchanged).
  for _cn_ip_burn in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _cn="${_cn_ip_burn%%=*}"
    # #1093 (b): a wedged strih receiver here (issue 1096, cam2/cam3 legs too) escalates to the ONE
    # per-run strih-OBS restart + a single re-check; no painter-up wait (the painter is stopped now).
    mv_reverify_or_escalate "$_cn" "${_cn#cam}" || exit 1
  done
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
  # #869: ALSO stop the PERMANENT cam2-painter.service (the #863 always-on devel painter) — the
  # non-sweep arm below has always done this, and the sweep needs it just as much. #734's
  # `pkill -x frame-probe` + death-wait below cannot cover it on its own: that unit is
  # Restart=always / RestartSec=2, so systemd restores its painter INSIDE the ~10s wait window, the
  # wait times out, and this run launches a SECOND painter onto the same /dev/fb0 under a DIFFERENT
  # run-id — verbatim the #440 artifact (the displayed QR alternates between the two painters'
  # run_ids), which makes all_cambox_continuity report held images as copies/gaps on EVERY cambox.
  # Ordered BEFORE the #734 kill so that kill is not racing a restart the stop has not disabled yet.
  # Guarded (`2>/dev/null || true`) so a box without the unit is unaffected. Note this does NOT
  # extend to camera-box itself: #291 keeps it RUNNING on cam2 here (a measured node whose
  # capture+emit must stay alive) — only the painter is the process being replaced.
  # #872: arm the on-box dead-man FIRST, so a kill anywhere after this point still restores the
  # painter without dev1. cleanup() disarms it on every normal exit.
  _cam2_prep="$(cam2_painter_deadman_arm_cmds)
systemctl stop cam2-painter 2>/dev/null || true; rm -f /tmp/painter.csv /tmp/av-markers.csv;"
  _cam2_marker_flags="--audio-marker --audio-marker-device $AV_SYNC_MARKER_DEVICE \
      --audio-marker-cadence-ticks $AV_SYNC_MARKER_CADENCE --marker-log /tmp/av-markers.csv"
  # #420/#431 fail-loud self-check (same mechanism AV_RESTART_GATE uses, scripts/lib/audio-marker-check.sh):
  # confirms the marker is RUNNING *and* the log is actually GROWING before the run proceeds — a
  # broken marker setup is caught in ~20s here, not discovered after a wasted 30-min sweep.
  _cam2_marker_check="$(audio_marker_check_cmds "$AV_SYNC_MARKER_DEVICE" \
    'pkill -x frame-probe 2>/dev/null || true' \
    'all-cambox continuous marker, #312 item 2' '/tmp/av-markers.csv')"
else
  # #872: same dead-man arm on the non-sweep arm -- it stops the permanent painter too.
  # #772: this arm ALSO stops the production camera-box on cam2 (single-camera mode) -- so arm the
  # camera-box dead-man here too, before that stop.
  _cam2_prep="$(cam2_painter_deadman_arm_cmds)
systemctl stop cam2-painter 2>/dev/null || true; $(camera_box_deadman_arm_cmds "$CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN") systemctl stop camera-box; pkill -x camera-box 2>/dev/null; rm -f /tmp/painter.csv;"
fi
# #734: unconditionally kill any PRE-EXISTING frame-probe on cam2 and VERIFY it is actually dead
# (not merely "started a kill") BEFORE waiting for /dev/fb0 / launching this run's own painter. A
# manual `rig-mode.sh test` invocation (or any stale leftover) left running holds BOTH /dev/fb0 AND
# the audio-marker's ALSA device (hw:CARD=...,DEV=N) EXCLUSIVELY. Without this, the fb0-fuser wait
# below merely times out after 15s (busy or not) and launches a SECOND frame-probe anyway — the
# #420 RUNNING check is DEVICE-scoped (reads /proc/asound/.../status, which reports RUNNING from
# the OLD still-alive process regardless of whether the NEW one ever opened the device) while the
# #431 emission-growth check is scoped to THIS run's own --marker-log file, which the new process
# never gets to write if its own --audio-marker open failed on the busy device — exactly the
# PASS-#420/FAIL-#431 split from the live incident (#734, 2026-07-13, reproduced 2/2). NEVER reuse
# a foreign painter process instead of killing it: it paints a DIFFERENT --run-id than $RUN_ID, and
# this run's recording-verdict decode only trusts markers/QR burns carrying ITS OWN $RUN_ID —
# reusing a stale process's output would silently record the WRONG run's content. `pkill -x`
# matches by process COMM name only, so it can never self-match this remote ssh session's own
# cmdline (the established convention throughout this codebase — never `pkill -f`).
_cam2_kill_existing="pkill -x frame-probe 2>/dev/null || true; \
   ki=0; while pgrep -x frame-probe >/dev/null 2>&1 && [ \$ki -lt 20 ]; do sleep 0.5; ki=\$((ki+1)); done;"
# #1179: opt-in painter display-mode override (mode-independent, so computed once outside the
# _cam2_prep branch). Empty unless PAINTER_DISPLAY_MODE is set to a WxH@RR shape; a mis-shaped or
# injection value FAILS LOUD here (VAR="$(...)" under set -e) before the ssh. The authoritative
# range check (dims/refresh > 0, no overflow) is frame-probe's parse_display_mode on the box.
_cam2_display_mode_flag="$(painter_display_mode_args)"
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "$_cam2_prep \
   $_cam2_kill_existing \
   i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
   (nohup /tmp/frame-probe --paint-only --dual-qr --wall-clock --paint-log /tmp/painter.csv \
      --paint-fps $PAINT_FPS --qr-size $QR_SIZE --run-id $RUN_ID --duration-secs $((DURATION + PAINTER_PRE_RECORD_SLACK_SECS)) \
      $_cam2_marker_flags \
      $_cam2_display_mode_flag \
      >/tmp/painter.log 2>&1 &); \
   $_cam2_marker_check"
PAINTER_LAUNCH_EPOCH="$(date +%s)"  # #359: when the painter's --duration-secs lifetime started
sleep 3  # let the painter put the QR on the monitor cam1 films

# #723: register this run's painter in the rig-test LEDGER — the sanctioned registration path,
# so rig-mode.sh event's cleanup sweep (or an orphan sweep) can find + kill it BY PID even if
# this run is abandoned mid-flight and its process later gets renamed/copied elsewhere (the #721
# incident class). Best-effort (never aborts a measurement run over a ledger hiccup): PID
# discovery via a fresh pgrep right after launch — frame-probe was just uniquely killed+
# relaunched above ($_cam2_kill_existing), so exactly one is expected.
_cam2_painter_pid="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "pgrep -x frame-probe 2>/dev/null | head -1" 2>/dev/null || true)"
if [ -n "$_cam2_painter_pid" ]; then
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
    "$(rig_test_ledger_register_remote_cmds "frame-probe --paint-only (recording-e2e run $RUN_ID)" "$_cam2_painter_pid" cam2 "recording-e2e.sh" "$(rig_test_ledger_effective_max_duration "$((DURATION + 60))" "recording-e2e measurement run")")" \
    2>&1 | sed 's/^/    [#723 ledger] /' || true
else
  echo "WARNING: [#723] could not discover cam2 painter PID for ledger registration (best-effort, run proceeds)." >&2
fi

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
# #358/#691: delivery-verify gate — set stream box's 'NDI 2ME PGM' genlock_latency_ms_src
# to a value that exercises the #292 >450ms cap regression for the test window, then
# restore the PRE-TEST value (snapshotted by obs_phase2.py itself) on teardown. The live
# FIFO audit log read-back (latency_ms= field) confirms the FIFO actually HELD the set
# value (the #292 silent-non-apply gate).
#
# #691: GENLOCK_TEST_LATENCY_MS is left EMPTY (unset) by default now — NOT forced to
# 1000ms. obs_phase2.py's `resolve_test_latency_ms` derives the EFFECTIVE value at call
# time from the stream box's OWN current genlock_latency_ms_src: if that's already >=
# 500ms (comfortably exercises the #292 cap — the normal case once the box is calibrated,
# e.g. the live 925ms A/V-align value), it's used AS-IS with NO forced change at all —
# eliminating the #691 stomp risk entirely for a healthy calibrated box. Only when the
# current value is genuinely below 500ms does it fall back to the original 1000ms. Set
# GENLOCK_TEST_LATENCY_MS explicitly to force a literal value instead (still always wins).
GENLOCK_TEST_LATENCY_MS="${GENLOCK_TEST_LATENCY_MS:-}"
GENLOCK_TEST_LATENCY_SOURCE="${GENLOCK_TEST_LATENCY_SOURCE:-$STREAM_PROG_SOURCE}"

# #1003: measurement-window per-camera equalization (opt-in MEASUREMENT_EQ=1, default OFF —
# cautious #757 precedent; the supervisor enables it for the live validation E2E). When ON it
# applies delivery-equalized-deep strih test pins + a coherent stream hold for the MEASUREMENT
# WINDOW ONLY (snapshot-restore; production 3/6/20 + 971 untouched) and makes --av-expected-ms
# coherent. Resolved ONCE here (from the checked-in profile of MEASURED inputs, coherence-checked)
# so the stream prod-scene call below and the strih pin-apply step further down share the values.
. "$HERE/lib/measurement-eq.sh"
MEASUREMENT_EQ="${MEASUREMENT_EQ:-0}"
MEASUREMENT_EQ_PROFILE="${MEASUREMENT_EQ_PROFILE:-$HERE/e2e-measurement-pins.json}"
if measurement_eq_enabled; then
  # #1003 review: measurement-eq is only coherent for the ALL_CAMBOX per-camera run -- the
  # strih pin apply + verify are ALL_CAMBOX-gated, so without it the stream hold would drop while
  # pins stay production (every camera A/V ~-183ms -> a guaranteed-fail ~25-min run). Refuse early.
  if [ "${ALL_CAMBOX:-0}" != "1" ]; then
    echo "ERROR: [preflight] FAIL: #1003 MEASUREMENT_EQ=1 requires ALL_CAMBOX=1 (per-camera equalization); refusing an incoherent single-camera measurement-eq run." >&2
    exit 1
  fi
  # #1003 review: an EXPLICIT GENLOCK_TEST_LATENCY_MS and the profile hold would BOTH pass
  # --test-latency-ms (argparse last-wins would silently override the operator's explicit value,
  # breaking #691's "explicit always wins"). Refuse the ambiguous combination loudly.
  if [ -n "$GENLOCK_TEST_LATENCY_MS" ]; then
    echo "ERROR: [preflight] FAIL: #1003 MEASUREMENT_EQ=1 and an explicit GENLOCK_TEST_LATENCY_MS are mutually exclusive (both set the stream hold). Unset one." >&2
    exit 1
  fi
  MEQ_PLAN="$(measurement_eq_plan_json "$MEASUREMENT_EQ_PROFILE")" || {
    echo "ERROR: [preflight] FAIL: #1003 measurement-eq profile did not resolve (missing/malformed/INCOHERENT) — $MEASUREMENT_EQ_PROFILE" >&2
    exit 1
  }
  # #1003 review: the profile MUST cover every active camera (the #900 re-anchor it replaces had an
  # explicit coverage-fail) -- else a future CAMERA_ACTIVE_SET change silently measures an
  # unequalized camera against a rebalanced hold.
  MEASUREMENT_EQ_MISSING="$(measurement_eq_missing_active "$MEQ_PLAN" "$CAMERA_ACTIVE_SET")"
  if [ -n "$MEASUREMENT_EQ_MISSING" ]; then
    echo "ERROR: [preflight] FAIL: #1003 measurement-eq profile $MEASUREMENT_EQ_PROFILE does not cover active camera(s): $MEASUREMENT_EQ_MISSING — re-derive the profile for the current CAMERA_ACTIVE_SET." >&2
    exit 1
  fi
  MEASUREMENT_EQ_HOLD="$(measurement_eq_hold_ms "$MEQ_PLAN")"
  MEASUREMENT_EQ_PROD_HOLD="$(measurement_eq_prod_hold_ms "$MEQ_PLAN")"
  MEASUREMENT_EQ_AV_EXPECTED="$(measurement_eq_av_expected_ms "$MEQ_PLAN")"
  MEASUREMENT_EQ_SLACK="$(measurement_eq_slack_ms "$MEASUREMENT_EQ_PROFILE")"
  # #1003 review finding 2: raise the LIVE #1035 cam->strih p99 bound by the marker camera's pin
  # delta so the deep cam2 pin does not false-fail that separate, pin-dependent gate.
  MEASUREMENT_EQ_CAM_STRIH_BOUND="$(measurement_eq_cam_strih_bound_ms "$MEQ_PLAN" 400)"
  echo "[4/8 meq] #1003 measurement-eq ON — strih deep pins + stream hold ${MEASUREMENT_EQ_HOLD}ms (prod ${MEASUREMENT_EQ_PROD_HOLD}), --av-expected ${MEASUREMENT_EQ_AV_EXPECTED}ms, cam-strih p99 bound ${MEASUREMENT_EQ_CAM_STRIH_BOUND}ms; the #900 re-anchor is forced OFF (both write strih pins)"
fi

# #406/#312 item5: belt-and-braces re-check, immediately before rerouting strih/stream's
# PRODUCTION program scenes. The CI-level scripts/rig-busy-gate.sh check (when this harness
# runs under the automatic pull_request-triggered full-path-e2e.yml gate) may have passed
# tens of minutes ago — [1/8]-[3/8] (build/deploy/painter-start) can take a while — so a real
# broadcast could have started in that window. Never reroute a rig that just went live.
echo "[4/8 pre-check] #406 re-verifying the rig is still free right before the prod-scene reroute"
BUSY_RECHECK=$(python3 "$HERE/obs_phase2.py" rig-busy-check \
  --strih-host "$STRIH" --stream-host "$STREAM" --password "${OBS_PASSWORD:-}")
echo "    $BUSY_RECHECK"
if ! printf '%s' "$BUSY_RECHECK" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d["busy"] else 1)'; then
  echo "ERROR: rig went BUSY between the CI busy-gate check and this reroute step — aborting BEFORE touching prod scenes: $BUSY_RECHECK" >&2
  exit 1
fi

echo "[4/8] OBS prod-scene routing — strih program='$STRIH_PROG_SCENE' ($CAMERA_NAME via $STRIH_PROG_SOURCE),"
echo "      stream program='$STREAM_PROG_SCENE' (strih feed via '$STREAM_PROG_SOURCE')"
echo "      #183: forcing genlock_preload=$TEST_PRELOAD on both recorded prod inputs for the test"
if [ -n "$GENLOCK_TEST_LATENCY_MS" ]; then
  echo "      #358: forcing $GENLOCK_TEST_LATENCY_SOURCE genlock_latency_ms_src=$GENLOCK_TEST_LATENCY_MS for delivery-verify (explicit override)"
else
  echo "      #358/#691: $GENLOCK_TEST_LATENCY_SOURCE genlock_latency_ms_src for delivery-verify -- auto (current value if already >= 500ms, else 1000ms fallback)"
fi
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
# #691: --test-latency-ms is passed ONLY when GENLOCK_TEST_LATENCY_MS was explicitly set —
# an unset flag lets obs_phase2.py's resolve_test_latency_ms auto-derive the effective
# value from the box's own current latency at call time (see the #358/#691 block above).
_stream_prod_scene_args=(prod-scene --host "$STREAM" \
  --program-scene "$STREAM_PROG_SCENE" \
  --upstream "$STRIH_OUT" --test-preload "$TEST_PRELOAD" \
  --test-latency-source "$GENLOCK_TEST_LATENCY_SOURCE")
if [ -n "$GENLOCK_TEST_LATENCY_MS" ]; then
  _stream_prod_scene_args+=(--test-latency-ms "$GENLOCK_TEST_LATENCY_MS")
fi
# #1003: in measurement-eq mode set the stream hold to the profile's coherent test hold, with the
# PRODUCTION hold passed as the baseline reference so the snapshot is leftover-anchored (a stuck
# test hold a prior crashed run left is never adopted as production — the 2026-08-19 revert class).
# Restored to production on teardown by the existing `teardown --host STREAM` path.
if measurement_eq_enabled; then
  _stream_prod_scene_args+=(--test-latency-ms "$MEASUREMENT_EQ_HOLD" \
    --test-latency-prod-ref "$MEASUREMENT_EQ_PROD_HOLD" \
    --test-latency-slack "$MEASUREMENT_EQ_SLACK")
fi
STREAM_OUT=$(python3 "$HERE/obs_phase2.py" "${_stream_prod_scene_args[@]}")
echo "    strih program NDI='$STRIH_OUT'  stream program NDI='$STREAM_OUT'"
sleep 6  # let both OBS chains stabilise before recording

# #682: imag never had its program scene routed by THIS harness -- whatever a PRIOR session left
# on program silently decided which camera imag's leg measured (live incident, RUN_ID 1573931971:
# imag stuck on 'Cam 4' from an earlier #674 experiment while this run certified cam1 -- the imag
# leg FAILed even though cam1->strih->stream was ZERO loss end-to-end). Save imag's CURRENT
# program scene (restored in cleanup(), mirroring the strih/stream scene restore there) then route
# it to the camera-under-test's OWN scene via obs_phase2.py's existing `switch` subcommand -- the
# SAME non-black self-check the #312 all-cambox sweep already uses, so a missing/dead imag scene
# FAILS LOUD (bare `set -e` propagation) here instead of silently wasting the whole run.
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[4a/8] imag program-scene routing (#682)" "$IMAG_OFFLINE_ACK_REASON"
else
echo "[4a/8] #682 imag program-scene routing — must show $CAMERA_NAME (the camera under test)"
IMAG_PREV_SCENE="$(python3 "$HERE/obs_phase2.py" program-scene --host "$IMAG_IP")"
echo "    imag was on '$IMAG_PREV_SCENE' — routing to '$IMAG_PROG_SCENE' ($CAMERA_NAME)"
python3 "$HERE/obs_phase2.py" switch --host "$IMAG_IP" --program-scene "$IMAG_PROG_SCENE" >/dev/null
# issue 1204: fail-closed cross-check — the imag BURN TARGET ($IMAG_PROG_SOURCE, now derived from
# the camera-under-test via imag_source_for_camera) MUST be the input imag ACTUALLY renders in the
# program scene we just routed. A stale/divergent derivation (the CAM-default-vs-camera-under-test
# split, run 32908274448) would silently burn a non-program input, leaving the imag recording with
# zero 911003 anchors and failing the imag leg on present/span. Read what imag genuinely renders
# and FAIL LOUD on any mismatch (mirrors the strih/stream #901 "burn what's actually rendered"
# philosophy) — so the [4b/8] burn-check below (which validates $IMAG_PROG_SOURCE) is proven to be
# checking the input that IS in program, not one it merely set itself.
_imag_rendered="$(python3 "$HERE/obs_phase2.py" program-rendered-input --host "$IMAG_IP" || true)"
if ! imag_burn_target_matches_program "$_imag_rendered" "$IMAG_PROG_SOURCE"; then
  echo "ERROR: $(imag_burn_mismatch_message "$_imag_rendered" "$IMAG_PROG_SOURCE" "$IMAG_PROG_SCENE")" >&2
  exit 1
fi
echo "    [4a/8] imag burn-target cross-check OK — program renders '$_imag_rendered' == burn target '$IMAG_PROG_SOURCE' (issue 1204)"
fi

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
  # issue 1013: skip the imag triple when imag is acked-offline — its burn cannot be applied on an
  # absent box, and the burn_on=True check below hard-aborts (exit 1). strih/stream stay mandatory.
  if [ "$_bn" = "imag" ] && [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
    imag_leg_skip_note "[4b/8] imag burn-ON gate (#195/#257)" "$IMAG_OFFLINE_ACK_REASON"; continue
  fi
  # First turn the burn ON over WebSocket (idempotent, no relaunch — #257). `|| true` so a non-zero
  # exit (e.g. OBS unreachable) does not set -e-abort before our own clear diagnostic on the check.
  python3 "$HERE/obs_burn_filter.py" add --host "$_bip" --input "$_bsrc" 2>&1 \
    | sed "s/^/    [$_bn burn-on] /" || true
  _chk="$(python3 "$HERE/obs_burn_filter.py" check --host "$_bip" --input "$_bsrc" 2>&1 || true)"
  echo "    [$_bn burn-check] $_chk"
  if ! grep -q 'burn_on=True' <<<"$_chk"; then
    echo "ERROR: $_bn burn is NOT on (genlock_burn=true) for the recorded input '$_bsrc' — the $_bn" >&2
    echo "       burn would be absent from the recording and the run would be wasted (#195/#257)." >&2
    echo "       Confirm $_bn OBS ($_bip) is up + is the genlock build, then re-run (or scripts/rig-mode.sh test)." >&2
    exit 1
  fi
  echo "    [$_bn burn-check] OK — burns ON (genlock_burn=true on '$_bsrc', runtime, no relaunch)"
done

echo "[4b2/8] #748 audio-presence preflight — the mbc measurement chain MUST be audible before recording"
# The A/V-sync leg reads the mbc measurement audio (speaker -> church PA mic -> mbc -> Dante ->
# stream OBS); a silent chain makes every A/V number meaningless. Make a SHORT probe recording on
# the stream box (the mbc audio rides the stream program recording), run ffmpeg volumedetect on the
# box via win_ssh_run, and FAIL LOUD if the track is silent — so a dead measurement chain never
# again burns a full cycle reported only as a quiet av_sync "unknown, candidates: 0" (#748). Every
# knob is env-overridable like the sibling gates; a short probe guarding the whole run is the
# sanctioned exception to the one-full-test rule (a preflight, not a partial measurement).
AUDIO_PREFLIGHT_ENABLE="${AUDIO_PREFLIGHT_ENABLE:-1}"
AUDIO_PREFLIGHT_THRESHOLD_DB="${AUDIO_PREFLIGHT_THRESHOLD_DB:--60}"
AUDIO_PREFLIGHT_PROBE_SECS="${AUDIO_PREFLIGHT_PROBE_SECS:-15}"
AUDIO_PREFLIGHT_SSH_TIMEOUT="${AUDIO_PREFLIGHT_SSH_TIMEOUT:-90}"
# #748 live finding (run 29282790031): OBS-WS StopRecord's RPC reply lands BEFORE the mp4 muxer
# finalizes the file (moov atom written, handle closed) -- reading the file <1s after StopRecord
# hit "moov atom not found". This never surfaced on the real ~300s recording (plenty of natural
# delay before [7/8]'s later download); the short probe hits the race directly. A BOUNDED RETRY
# (same shape as the [4c/8] frozen-camera gate's reconnect-race retry) absorbs the transient without
# masking a genuine failure -- a real "ffmpeg missing" / wrong-path error fails identically on every
# attempt and still surfaces the same operator message after the attempts are exhausted.
AUDIO_PREFLIGHT_READ_ATTEMPTS="${AUDIO_PREFLIGHT_READ_ATTEMPTS:-4}"
AUDIO_PREFLIGHT_READ_RETRY_SLEEP="${AUDIO_PREFLIGHT_READ_RETRY_SLEEP:-3}"
if [ "$AUDIO_PREFLIGHT_ENABLE" = "1" ]; then
  # Short throwaway probe recording on stream. A leftover from an abort in the ~15s window self-heals:
  # the next run's --action start stops any orphan (obs_phase2.py record) before it re-records.
  python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start >/dev/null
  sleep "$AUDIO_PREFLIGHT_PROBE_SECS"
  _ap_path="$(python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action stop || true)"
  if [ -z "$_ap_path" ]; then
    echo "ERROR: $(audio_preflight_norec_message)" >&2
    exit 1
  fi
  _ap_db=""
  for _ap_attempt in $(seq 1 "$AUDIO_PREFLIGHT_READ_ATTEMPTS"); do
    # `timeout` execvp()s its command directly -- it cannot invoke a shell FUNCTION like win_ssh_run
    # (confirmed live: "timeout: failed to run command 'win_ssh_run': No such file or directory",
    # run 29281776692). Route through `bash -c`, re-sourcing the lib inside that subshell, so bash
    # (not execvp) resolves win_ssh_run as a function -- same fix as every other sibling win_ssh_run
    # call that needs an outer timeout bound.
    _ap_out="$(timeout "$AUDIO_PREFLIGHT_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
      "$HERE/lib/win-ssh-exec.sh" "$STREAM_USER" "$STREAM_PW" "$STREAM" "$(audio_preflight_volumedetect_ps "$_ap_path")" 2>&1 || true)"
    _ap_db="$(audio_preflight_parse_max_db "$_ap_out" || true)"
    if [ -n "$_ap_db" ]; then
      break
    fi
    if [ "$_ap_attempt" -lt "$AUDIO_PREFLIGHT_READ_ATTEMPTS" ]; then
      echo "    [audio-presence preflight] attempt ${_ap_attempt}/${AUDIO_PREFLIGHT_READ_ATTEMPTS} unreadable (mp4 likely still finalizing) — settling ${AUDIO_PREFLIGHT_READ_RETRY_SLEEP}s, re-reading"
      sleep "$AUDIO_PREFLIGHT_READ_RETRY_SLEEP"
    fi
  done
  win_ssh_run "$STREAM_USER" "$STREAM_PW" "$STREAM" "$(audio_preflight_delete_ps "$_ap_path")" >/dev/null 2>&1 || true
  if [ -z "$_ap_db" ]; then
    echo "ERROR: $(audio_preflight_unreadable_message)" >&2
    echo "       raw volumedetect output (last attempt): $_ap_out" >&2
    exit 1
  fi
  if [ "$(audio_preflight_is_silent "$_ap_db" "$AUDIO_PREFLIGHT_THRESHOLD_DB")" = "true" ]; then
    echo "ERROR: $(audio_preflight_silent_message "$_ap_db" "$AUDIO_PREFLIGHT_THRESHOLD_DB")" >&2
    exit 1
  fi
  echo "    ok: mbc measurement audio AUDIBLE (max_volume ${_ap_db} dB >= ${AUDIO_PREFLIGHT_THRESHOLD_DB} dB threshold)"
else
  echo "    [audio-presence preflight] SKIPPED (AUDIO_PREFLIGHT_ENABLE=0)"
fi

echo "[4c/8] #365 frozen-camera gate — every strih raw NDI input must be updating (not a frozen feed)"
# #747: the gate WARMS each camera input itself before sampling — there is NO external
# precondition. The #730/#508 Multiview decoupling removed the last always-on surface for the
# raw main inputs this gate checks (a not-showing DistroAV source does not render at all, which
# is indistinguishable from a genuine freeze); frozen-camera-gate.py now puts each input's
# wrapping 'Cam N' scene on PREVIEW (Studio Mode) and settles (--warm-settle) BEFORE sampling it.
# The stale "keep the Multiview projector open" precondition is gone — the decoupled Multiview
# renders low-bandwidth 'MV Cam N' twins (#730), not these raw main inputs, so it never kept
# them warm either way.
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
# actually matches FROZEN_CAM_EXCLUDE_SENDER at gate time, same as every other input). #753
# (2026-07-14): widened again to seven — cam7's new 'NDI cam7' input joins the checked set the
# same way cam5/cam6 did. #827 (2026-07-27, binding owner directive): the checked input set now
# DERIVES from CAMERA_ACTIVE_SET (camera-set.sh, passed to the python heredoc as an extra arg) —
# never a second hardcoded list. Re-enabling a retired camera is a one-line CAMERA_ACTIVE_SET
# edit, picked up here automatically.
FROZEN_CAM_EXCLUDE_SENDER="${FROZEN_CAM_EXCLUDE_SENDER:-CAM2 (usb)}"
if [ -z "${FROZEN_CAM_SOURCES:-}" ]; then
  FROZEN_CAM_SOURCES="$(python3 - "$STRIH" "$FROZEN_CAM_EXCLUDE_SENDER" "$HERE/obs_phase2.py" "$CAMERA_ACTIVE_SET" <<'PYEOF'
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("o", sys.argv[3])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
host, exclude = sys.argv[1], sys.argv[2]
active_cams = sys.argv[4].split()
ws = m._conn(host, os.environ.get("OBS_PASSWORD", ""))
keep = []
for inp in [f"NDI {cam}" for cam in active_cams]:
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
# #1158 auto-revive self-heal (an emptied/drifted ndi_source_name — e.g. a mid-run reattach clear
# left "" when the sender vanished, or a force-kill OBS restart reloaded a drifted saved scene —
# STOPS/misroutes the DistroAV receiver, which the in-loop #767/#1096 watchdogs can never fix). The
# helper delegates to set-ndi-mapping.py --heal (discoverability-gated + read-back-verified). It is
# ALWAYS called in an `if`-condition, never a bare statement: it exits non-zero when nothing was
# healable, and under this script's `set -euo pipefail` a bare non-zero would abort the whole run
# (the #1133 report-only-probe class); an `if` suppresses `set -e` inside it.
. "$HERE/lib/ndi-name-selfheal.sh"
# #1233: content-INDEPENDENT leg liveness — the abort signal is now strih's `genlock-fifo audit
# received=` counter DELTA per input (the #797/#1052 tap), NOT the old pixel-hash of preview
# screenshots (which false-aborted during the [2b/8] deploy wave: a re-attaching receiver holds the
# last frame → identical hashes even on a live leg). The lib reuses mv_reverify_probe_raw /
# mv_reverify_extract_received (mv-reverify-escalate.sh, already sourced above) + frozen_input_classify.
. "$HERE/lib/frozen-cam-received.sh"
_FROZEN_CAM_SOURCES_EFFECTIVE="${FROZEN_CAM_SOURCES:-$(camera_active_ndi_sources_csv)}"

# #1233 REPORT-ONLY: run the OLD pixel-hash gate (frozen-camera-gate.py) ONCE as a diagnostic line
# only — it warms each input onto PREVIEW (#747 side-effect, preserved) and its FROZEN/PASS verdict
# is logged, but it NEVER aborts (a static-but-live receiver frame reads FROZEN). Bounded by a
# timeout so its per-source warm-up cannot dominate the pre-record budget (#747/#1223 painter slack).
frozen_pixel_verdict=PASS
timeout "${FROZEN_CAM_PIXEL_REPORT_TIMEOUT_S:-90}" python3 "$HERE/frozen-camera-gate.py" \
    --host "$STRIH" \
    --threshold   "${FROZEN_CAM_THRESHOLD:-3}" \
    --samples     "${FROZEN_CAM_SAMPLES:-8}" \
    --sources     "$_FROZEN_CAM_SOURCES_EFFECTIVE" \
    --warm-settle "${FROZEN_CAM_WARM_SETTLE_S:-3}" >/dev/null 2>&1 || frozen_pixel_verdict=FROZEN
echo "    [frozen-camera-gate] #1233 pixel-hash REPORT-ONLY: ${frozen_pixel_verdict} (content-dependent screenshot check — NOT the abort signal)"

# #1233 ABORT SIGNAL: received= delta per input, BOUNDED RETRY (FROZEN_CAM_ATTEMPTS) still covers the
# post-[3/8] / deploy-wave reconnect — a source whose sender is briefly down reads FROZEN/INCONCLUSIVE,
# settles (+#1158 self-heal), then advances; a GENUINELY stuck camera stays FROZEN every attempt.
frozen_ok=0
frozen_recv_verdict=""
for frozen_attempt in $(seq 1 "$FROZEN_CAM_ATTEMPTS"); do
  frozen_recv_verdict="$(frozen_cam_received_read_and_verdict "$STRIH" "$_FROZEN_CAM_SOURCES_EFFECTIVE")"
  case "$frozen_recv_verdict" in
    ALIVE)
      echo "    [frozen-camera-gate] received= ALIVE — every checked strih input advanced across the window (#1233)"
      frozen_ok=1
      break
      ;;
    FROZEN:*)
      echo "    [frozen-camera-gate] received= not advancing on ${frozen_recv_verdict#FROZEN:} (counter stuck — a leg is not delivering)"
      ;;
    *)
      echo "    [frozen-camera-gate] received= ${frozen_recv_verdict} — could not PROVE liveness this attempt (no audit line / unreadable log; not a proven freeze)"
      ;;
  esac
  if [ "$frozen_attempt" -lt "$FROZEN_CAM_ATTEMPTS" ]; then
    if ndi_name_selfheal_run "$STRIH" "$CAMERA_ACTIVE_SET" "$HERE"; then
      echo "    [frozen-camera-gate] #1158 auto-revive re-enforced an emptied/drifted NDI mapping — re-sampling after the settle"
    fi
    echo "    [frozen-camera-gate] attempt ${frozen_attempt}/${FROZEN_CAM_ATTEMPTS} not ALIVE — settling ${FROZEN_CAM_RETRY_SLEEP}s for the post-[3/8] NDI reconnect, then re-sampling"
    sleep "$FROZEN_CAM_RETRY_SLEEP"
  fi
done
if [ "$frozen_ok" -ne 1 ]; then
  case "$(frozen_cam_gate_should_abort "$frozen_ok" "$frozen_recv_verdict")" in
    ABORT)
      echo "    [frozen-camera-gate] received= FROZEN on every one of ${FROZEN_CAM_ATTEMPTS} attempts — a camera is GENUINELY stuck; aborting (#365/#1233)"
      exit 1
      ;;
    *)
      echo "    [frozen-camera-gate] WARN (#1233): could NOT prove leg liveness via received= after ${FROZEN_CAM_ATTEMPTS} attempts (verdict='${frozen_recv_verdict:-none}') — NOT a proven freeze, so NOT aborting (the leg is re-proven downstream by the QR sweep). Investigate the strih OBS-log tap if this recurs." >&2
      ;;
  esac
fi

echo "[4d1/8] #771 MV-fps floor preflight — strih + imag Multiview projectors must not already be rendering below floor (target − tolerance) before we commit a ~40-min run; an unreadable box / a box not yet on the #771 genlock build is report-only, only a CONFIRMED sustained collapse aborts (never false-abort a CI gate)"
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[4d1/8] imag MV-fps floor preflight (#771) — strih still checked" "$IMAG_OFFLINE_ACK_REASON"
  mv_fps_preflight_assert "$PROBE_BIN_DIR/mv-fps-gate" \
    "strih|$STRIH|win|$STRIH_USER|$STRIH_PW"
else
mv_fps_preflight_assert "$PROBE_BIN_DIR/mv-fps-gate" \
  "strih|$STRIH|win|$STRIH_USER|$STRIH_PW" \
  "imag|$IMAG_IP|linux|${IMAG_USER:-newlevel}|${IMAG_PW:-newlevel}"
fi

echo "[4d/8] #405/#406/#462 render-budget gate — with burns ON + Multiview open, strih+stream MUST hold the render frame budget (strih 30fps, stream 30fps — Topology v2, #459: strih's 60fps IMAG role moved to imag-nb, which now carries its own render-budget floor too); imag is measured too (60fps) and is STRICT as well (issue 888) — the step aborts if any of the three boxes misses its budget"
# The 2026-07-02 regression (found when strih was STILL the 60fps LED-wall IMAG box, pre-#459): a
# measurement burn left ON dropped strih RENDER 60->27fps (36ms > 16.6ms/60fps budget) while the
# encoder outputFps stayed a DUPLICATED 60 (green) — and NOTHING
# caught it, because the delivery verdict checks burn-id contiguity (which stays contiguous at
# 27fps) not render fps. This gate snapshots OBS WS GetStats deltas on each box in the exact
# recording state (burns ON from [4b/8], Multiview open) and FAILS FAST if strih/stream misses its
# frame-time budget — so a choked pipeline can never be recorded and then "pass" on delivery.
# STRICT (strict-test mandate) for strih/stream: no warn-only, no override. A fail = fix the root
# cause (an expensive burn is #404's full-frame readback; a render regression is a real regression).
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
      --window-s "${RENDER_GATE_WINDOW_S:-6}"; then
  echo "    [render-budget-gate] strih/stream missed the render frame budget with burns ON — aborting BEFORE recording (#405)." >&2
  echo "    A recording made in this state would judder (encoder duplicates frames) yet pass delivery-contiguity." >&2
  echo "    Root cause is almost always the expensive measurement burn (#404 full-frame readback) or a render regression." >&2
  echo "    Clear burns with scripts/rig-mode.sh event; see EPIC #406." >&2
  exit 1
fi

# issue 888 (RE-GATE, RESTORED to STRICT 2026-08-03): imag's own render-budget term was
# temporarily non-aborting (user-directed 2026-07-30) while its measurement burn cost ~11.5ms of
# imag's 16.67ms (60fps) frame budget, leaving under 1ms of headroom -- a coin-flip gate that
# blocked PR #704 (37 bundled, otherwise-finished tickets) for three consecutive runs. That
# relaxation is now retired: 10 independent `Full-path E2E` gate runs from 2026-07-30 19:20
# through 2026-08-03 (burns confirmed ON via each run's own burn-check log) all PASS this term,
# with imag comfortably at 4.8-6.5ms against the 16.67ms budget -- 10+ms of real headroom, not the
# under-1ms margin that motivated the relaxation. See the design comment on issue 888 for the full
# dataset. This call is strict again: same abort shape as strih/stream, same underlying
# render-budget-gate.py / render_budget::classify threshold (untouched throughout).
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[4d/8] imag render-budget gate (#405/#888)" "$IMAG_OFFLINE_ACK_REASON"
else
if ! OBS_PASSWORD_IMAG="${OBS_PASSWORD_IMAG:-${OBS_PASSWORD:-}}" \
    python3 "$HERE/render-budget-gate.py" \
      --box "imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}" \
      --window-s "${RENDER_GATE_WINDOW_S:-6}"; then
  echo "    [render-budget-gate] imag missed the render frame budget with burns ON — aborting BEFORE recording (#405/#888)." >&2
  echo "    A recording made in this state would judder (encoder duplicates frames) yet pass delivery-contiguity." >&2
  echo "    Root cause is almost always the expensive measurement burn (#404 full-frame readback) or a render regression." >&2
  echo "    This term was temporarily relaxed 2026-07-30..2026-08-03 (issue 888) and was restored to strict" >&2
  echo "    once 10 consecutive runs showed real headroom -- if it is failing again now, that is a genuine" >&2
  echo "    regression, not the original #865/#886 marginal-headroom condition. See issue 888 for history." >&2
  exit 1
fi
fi

if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[4e/8] imag-nb headroom preflight (#709/#845)" "$IMAG_OFFLINE_ACK_REASON"
else
echo "[4e/8] #709/#845 imag-nb headroom preflight — StartRecord's encoder needs real free memory (dGPU VRAM if this box has one, else system RAM on an iGPU-only box)"
# #845: the replacement imag notebook (10.77.9.187, #816) has NO discrete GPU (Intel iGPU / i915
# only) -- the nvidia-smi-based check below is structurally unsatisfiable there and used to abort
# every gate run with "returned an unreadable value" (run 30358343543), wrongly pointing at a
# driver that was never installed by design. Detect dGPU presence the SAME way setup-imag.sh /
# verify-imag.sh already do (imag_has_discrete_nvidia, an lspci display-class match, #816) and run
# the box-appropriate variant. Per #833: a MISSING `lspci` must never be silently misread as "no
# dGPU" (that would be exactly the measured-zero bug class) -- preflight it by name first.
_imag_lspci_probe="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
  "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_require_remote_tool_cmd lspci)" 2>/dev/null || true)"
_imag_lspci_missing="$(imag_remote_tool_probe_missing "$_imag_lspci_probe")"
if [ -n "$_imag_lspci_missing" ]; then
  echo "ERROR: [preflight] FAIL: imag-nb (${IMAG_IP}): required tool(s) not installed: ${_imag_lspci_missing} (apt-get install -y pciutils) — refusing to guess whether a discrete GPU is present (#833/#845 class)." >&2
  exit 1
fi
IMAG_LSPCI_OUT="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
  "${IMAG_USER:-newlevel}@$IMAG_IP" "lspci -nn 2>/dev/null" 2>&1 || true)"
if [ -z "$IMAG_LSPCI_OUT" ]; then
  echo "ERROR: imag-nb lspci query returned nothing (tool confirmed present above) — cannot determine whether a discrete NVIDIA GPU is present. Check SSH connectivity to imag-nb directly." >&2
  exit 1
fi
IMAG_HAS_DGPU=no
if printf '%s\n' "$IMAG_LSPCI_OUT" | imag_has_discrete_nvidia; then IMAG_HAS_DGPU=yes; fi

if [ "$IMAG_HAS_DGPU" = "yes" ]; then
  # --- discrete NVIDIA present: the ORIGINAL #709 nvidia-smi free-VRAM check, unchanged --------
  # A long-uptime OBS render pipeline on imag-nb can leak GPU VRAM (live-diagnosed #709: 6872MiB
  # used out of 8151MiB total after 5 days uptime, leaving ~1058MiB free — NVENC's encoder init then
  # fails NV_ENC_ERR_OUT_OF_MEMORY and StartRecord silently never starts a recording, only caught 4s
  # later by the #627 liveness check with no root-cause hint). Read the box's CURRENT free VRAM and
  # FAIL FAST with an actionable message — before StartRecord ([5/8]) ever runs — rather than burn
  # time discovering an opaque liveness failure. IMAG_GPU_MIN_FREE_MIB default (1500) sits with
  # real margin above the observed failure point (~1058MiB) and well below a freshly-restarted
  # box's healthy free VRAM (~7849MiB, confirmed live post-fix).
  IMAG_GPU_MIN_FREE_MIB="${IMAG_GPU_MIN_FREE_MIB:-1500}"
  IMAG_GPU_QUERY_OUT="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_gpu_query_cmd)" 2>&1 || true)"
  IMAG_GPU_FREE_MIB="$(imag_gpu_free_mib_from_query "$IMAG_GPU_QUERY_OUT" || true)"
  if [ -z "$IMAG_GPU_FREE_MIB" ]; then
    echo "ERROR: $(imag_gpu_unreadable_message)" >&2
    echo "       raw nvidia-smi output: $IMAG_GPU_QUERY_OUT" >&2
    exit 1
  fi
  if [ "$(imag_gpu_headroom_ok "$IMAG_GPU_FREE_MIB" "$IMAG_GPU_MIN_FREE_MIB")" != "true" ]; then
    echo "ERROR: $(imag_gpu_preflight_message "$IMAG_GPU_FREE_MIB" "$IMAG_GPU_MIN_FREE_MIB")" >&2
    exit 1
  fi
  echo "    ok: imag-nb GPU has ${IMAG_GPU_FREE_MIB}MiB VRAM free (>= ${IMAG_GPU_MIN_FREE_MIB}MiB required)"
else
  # --- no discrete GPU (Intel iGPU / i915, this box's CURRENT hardware): system-RAM equivalent -
  # #845: an integrated GPU has no separate VRAM pool -- it draws render/encode buffers from
  # system memory (UMA). /proc/meminfo's MemAvailable is the genuinely meaningful headroom
  # figure on this hardware (live-confirmed on 10.77.9.187, 2026-07-28: no per-GPU memory
  # accounting exists under /sys/class/drm/card*/ -- only clock-scaling gt_*_freq_mhz files).
  # IMAG_MEM_MIN_AVAILABLE_MIB reuses the SAME 1500MiB floor as the dGPU check as a conservative
  # starting point (no #709-equivalent failure has yet been observed on this hardware to
  # calibrate a tighter number against) -- overridable like every sibling *_MIN_* knob here.
  IMAG_MEM_MIN_AVAILABLE_MIB="${IMAG_MEM_MIN_AVAILABLE_MIB:-1500}"
  IMAG_MEM_QUERY_OUT="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_mem_query_cmd)" 2>&1 || true)"
  IMAG_MEM_AVAILABLE_MIB="$(imag_mem_available_mib_from_query "$IMAG_MEM_QUERY_OUT" || true)"
  if [ -z "$IMAG_MEM_AVAILABLE_MIB" ]; then
    echo "ERROR: $(imag_mem_unreadable_message)" >&2
    echo "       raw /proc/meminfo output: $IMAG_MEM_QUERY_OUT" >&2
    exit 1
  fi
  if [ "$(imag_mem_headroom_ok "$IMAG_MEM_AVAILABLE_MIB" "$IMAG_MEM_MIN_AVAILABLE_MIB")" != "true" ]; then
    echo "ERROR: $(imag_mem_preflight_message "$IMAG_MEM_AVAILABLE_MIB" "$IMAG_MEM_MIN_AVAILABLE_MIB")" >&2
    exit 1
  fi
  echo "    ok: imag-nb has no discrete GPU (Intel iGPU / i915, unified memory) -- ${IMAG_MEM_AVAILABLE_MIB}MiB system RAM available (>= ${IMAG_MEM_MIN_AVAILABLE_MIB}MiB required)"
fi
fi   # issue 1013: end of the IMAG_OFFLINE_ACKED skip-guard over the [4e/8] imag headroom preflight

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
# This av-restart-sync path predates #701/#703 (which proved plain scp/ssh reaches strih/stream
# and wired an --execute path into the MAIN verdict below) and has not been migrated to it yet, so
# the recording-verdict --av-sync DECODE step is still EMITTED as a
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
    # #1179: same opt-in painter display-mode override as the [3/8] launch (empty unless
    # PAINTER_DISPLAY_MODE is set to a WxH@RR shape; a mis-shaped/injection value fails loud here
    # under set -e; the box's parse_display_mode does the authoritative range check). Split
    # local-decl / assignment so set -e sees the exit code (a `local X=$(...)` would mask it).
    local _av_display_mode_flag
    _av_display_mode_flag="$(painter_display_mode_args)"
    # #772: cam1's camera-box dead-man was armed at [2/8], but THIS restart-survival mode has an
    # UNBOUNDED operator wait between the before/after pair -- so cam1's stale [2/8] timer could
    # fire mid-"after" measurement and kill cam1's burn (cam1's feed IS the AV video path, filming
    # cam2's marker monitor). Re-arm cam1 here too (idempotent -- clears the old timer, resets its
    # first fire past THIS call's own record + any operator delay), the same way the cam2 ssh below
    # re-arms cam2's own dead-man. Best-effort: a re-arm failure only reverts to the stale [2/8]
    # timer, never worse than before this fix.
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
      "$(camera_box_deadman_arm_cmds "$CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN")" 2>/dev/null || true
    echo "    [av-restart-sync/$label] cam2 painter: dual-QR + QPSK audio marker on $AV_RESTART_MARKER_DEVICE"
    # Free /dev/fb0 the SAME way [3/8] does — stop cam2-painter AND camera-box (which can
    # also hold fb0 via its --display path), kill any leftover frame-probe, then WAIT (bounded)
    # for fb0 to actually release before relaunching. A partial copy that skipped the
    # camera-box stop / the fuser wait would race the framebuffer and silently corrupt the
    # QR + marker paint, degrading the very measurement this gate depends on.
    sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
      "$(cam2_painter_deadman_arm_cmds)
       systemctl stop cam2-painter 2>/dev/null || true; \
       $(camera_box_deadman_arm_cmds "$CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN") \
       systemctl stop camera-box; pkill -x camera-box 2>/dev/null; pkill -x frame-probe 2>/dev/null || true; \
       rm -f /tmp/av-restart-markers.csv; \
       i=0; while fuser -s /dev/fb0 2>/dev/null && [ \$i -lt 30 ]; do sleep 0.5; i=\$((i+1)); done; \
       (nohup /tmp/frame-probe --paint-only --dual-qr --paint-fps $PAINT_FPS --qr-size $QR_SIZE \
          $_av_display_mode_flag \
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

echo "[4f/8] #747 pre-record camera-scene warm-up — cycle every strih 'Cam N' scene onto preview"
# Companion to [4c/8]'s own per-source warm-up: cycle EVERY strih camera scene onto PREVIEW
# briefly (right before StartRecord) so [6/8]'s ALL_CAMBOX sweep's very first program cut to
# each camera is not a cold DistroAV receiver connect. Post-#730/#508 Multiview decoupling, a
# raw NDI main input not currently SHOWING does not render until something puts it on
# program/preview — this is the last chance to do that before the recording actually starts.
# Best-effort: `|| true` so a WS hiccup here never aborts the run (the recording's own
# per-segment cuts will still connect the receiver, just possibly with a cold first second —
# exactly the pre-#747 status quo, not a new failure mode).
python3 "$HERE/warm_cam_scenes.py" --host "$STRIH" --settle "${WARM_CAM_SETTLE_S:-1.5}" 2>&1 \
  | sed 's/^/    /' || true

# ============================================================================
# #757 [4g/8] — PRE-RECORD PHASE AUTO-PIN, STRIH ONLY (opt-out via PRERECORD_PHASE_CALIBRATE=0)
# ============================================================================
# #757: the [2/8]/[2b/8] deploy above RESTARTS every camera-box (systemctl stop/start) on
# EVERY run — each USB capture card's own internal clock free-runs from a phase relative to
# strih's presentation grid that gets effectively RE-RANDOMIZED by that restart. Confirmed
# across 10 consecutive fused-gate runs (2026-07-14/15 night): a camera's own delivery p50
# swings by up to ~one frame period (~16.7-33ms) run-to-run with NO code change — a STATIC pin
# set (however well-calibrated on a past run) only ever removes the FIXED per-camera baseline,
# leaving the full per-restart random re-phase as cross-camera SPREAD error the gate can never
# reliably clear. This step measures THIS run's own phase, right after the deploy/restart above
# (and after the #747 warm-up just above, which leaves every receiver warm for it) and before
# the real recording starts (step 5), and re-pins before anything is scored.
#
# **STRIH ONLY (binding user directive, 2026-07-15).** Per-camera pin EQUALIZATION is a
# STRIH-only concept — imag is the LOW-LATENCY IMAG projection and runs every NDI input pinned
# at the fixed 3ms floor, ALWAYS, self-healed every run by imag_latency_enforce.py below
# (never fed a computed pin). The earlier design here also computed+applied imag pins; that is
# RETIRED — do not resurrect it.
#
# Mechanism (no full recording/decode needed — see scripts/prerecord_phase_calibrate.py's own
# module doc for the derivation): cycle every strih camera scene onto PREVIEW for
# CALIB_DWELL_SECS each (reusing warm_cam_scenes.py's EXACT, already-proven, unchanged
# preview-cycling mechanism — the #747 warm-up just above already calls it with a short
# settle; this is the SAME function, called again with a longer settle for calibration
# purposes). strih's OWN live `genlock-fifo audit` log already carries, per camera, the
# EFFECTIVE pin active during the window (`latency_ms`) and the SIGNED mean deviation of the
# actual arrival from that pin's own release schedule (`mean_head_skew_ms`, #757 — new
# field). Their sum reconstructs each camera's TRUE absolute cam→strih transit latency
# WITHOUT decoding a single frame; feeding that into the EXISTING #286
# `compute_phase_sync_offsets` kernel (via the phase-sync calibrator script, completely
# unchanged) produces the same slowest-anchored relative pin set a full recording-based
# measurement would.
#
# #757 CORRECTION 1 (2026-07-15, live regression on acceptance run 1779172763): the ORIGINAL
# design here cut strih PROGRAM (not preview) through every camera. That disrupted the LIVE
# stream/audio/imag chains for roughly the next two minutes of the SCORED recording that
# followed (elevated imag stuck-density, av_sync clustering collapse) — all self-recovering
# by window 5. #747's own warm-up (right above) proves genlock-fifo audit lines fire for a
# PREVIEW-only-active source too (its own docstring: "so DistroAV's raw NDI receivers... are
# already connected" — vendor/obs-studio/libobs/obs-source.c's genlock_audit_log call sites
# are gated on the source's own tick/showing state, never specifically on "is this the
# program source"). Fixed: this step now NEVER touches strih PROGRAM at all.
#
# #757 CORRECTION 2 (2026-07-15, live regression on acceptance run 1935769027, ZERO-HEADROOM
# EDGE OSCILLATION): equalizing pins EXACTLY at each camera's measured transit (the slowest at
# the literal 3ms floor) left NO margin against ordinary jitter — a frame landing right on the
# ts-align release deadline gets flipped across the slot boundary by the next tick's jitter,
# producing a duplicate on one side and a gap on the other. Live evidence: every camera in
# every segment showed near-EQUAL copies≈gaps (14-21 each), a brand-new pattern that appeared
# ONLY once auto-pin equalization actually ran (the two prior acceptance runs never got that
# far due to Correction 1's UTF-8 crash). Fixed: `phase_sync_calibrate.py --margin-ms` raises
# EVERY camera's pin by a uniform jitter-headroom margin (computed from THIS run's own worst
# observed skew, floored at a safe minimum — see prerecord_phase_calibrate.compute_margin_ms)
# — same relative "slowest lowest, fastest highest" ordering, just shifted off the deadline
# edge. The SAME run also exposed a data-quality bug the margin estimate depends on: fetching
# the OBS log via a blind `-Tail 2000` pulled in ~27 MINUTES of PRIOR history (contaminating
# the jitter measurement with stale/unrelated activity) — fixed by capturing a log LINE-COUNT
# marker before the preview cycle starts and fetching only what was appended since (`Select-
# Object -Skip`), never a blind tail.
#
# #757 CORRECTION 3 (2026-07-15, live regression on acceptance run 1698791093 — Correction 2's
# OWN fix made things WORSE, not better): spread jumped to 128.1ms (cam3 177, cam5 176.7, cam2
# 155.8ms p50) and all_cambox_av_sync offsets shifted to +73..+201ms (cam2 measured +143.65 —
# the margin shifted the WHOLE video path ~130ms late vs the previously-working calibrated
# hold), while the uniform copies≈gaps pattern Correction 2 targeted PERSISTED UNCHANGED
# (9-19 per segment, still uniform). Two consecutive auto-pin iterations degraded results —
# per architecture-first.md's "no circular development", STOP iterating blind on the live
# rig and demote the whole mechanism to OFF-BY-DEFAULT (advisory measurement/report only,
# never applied) until the underlying PREVIEW-based skew measurement itself is understood:
# a margin computed from it shifting the ENTIRE video path by ~130ms is strong evidence that
# `mean_head_skew_ms`/`max_abs_head_skew_ms` measured against a PREVIEW-only-active source
# does NOT reliably proxy the PROGRAM-relevant quantity this mechanism needs — Correction 1's
# reasoning (genlock_audit_log fires for any active view) may be necessary but is evidently
# not SUFFICIENT for the measured skew to mean the same thing as it would for an active
# program source. Re-enable only after that question has a real answer, via
# PRERECORD_PHASE_CALIBRATE=1 (opt-in) — never by flipping this default back blind.
#
# Best-effort throughout (`set +e` for this whole block): a calibration hiccup (an unreachable
# log, zero usable audit lines, an apply failure) must NEVER abort the scored recording below —
# it just means this run keeps whatever pins were already applied (the pre-#757 behavior).
PRERECORD_PHASE_CALIBRATE="${PRERECORD_PHASE_CALIBRATE:-0}"
if [ "$PRERECORD_PHASE_CALIBRATE" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  set +e
  echo "[4g/8] #757 pre-record phase auto-pin (strih only) — measure THIS run's actual per-camera phase, re-pin before the real recording starts"
  CALIB_DWELL_SECS="${CALIB_DWELL_SECS:-10}"
  mkdir -p "$OUTDIR"

  # #757 Correction 2 (time-scoping): capture the CURRENT line count of strih's latest OBS log
  # BEFORE the preview cycle starts, so the fetch below can skip straight past everything
  # older than this calibration window — never a blind `-Tail N` that can silently include
  # many minutes of unrelated prior activity.
  CALIB_LOG_START_LINES="$(win_ssh_run "$STRIH_USER" "$STRIH_PW" "$STRIH" \
    '(Get-Content (Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1)).Count' \
    2>/dev/null | tr -d '[:space:]')"
  case "$CALIB_LOG_START_LINES" in ''|*[!0-9]*) CALIB_LOG_START_LINES=0 ;; esac

  echo "    [calib] cycling every strih camera onto PREVIEW for ${CALIB_DWELL_SECS}s each (program output untouched)"
  python3 "$HERE/warm_cam_scenes.py" --host "$STRIH" --settle "$CALIB_DWELL_SECS" 2>&1 \
    | sed 's/^/    [calib] /'

  CALIB_LOG="$OUTDIR/prerecord-calib-strih-${RUN_ID}.log"
  _calib_fetch_ps='Get-Content (Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1) | Select-Object -Skip '"$CALIB_LOG_START_LINES"
  if win_ssh_run "$STRIH_USER" "$STRIH_PW" "$STRIH" "$_calib_fetch_ps" \
      > "$CALIB_LOG" 2>/dev/null && [ -s "$CALIB_LOG" ]; then
    CALIB_JITTER_JSON="$OUTDIR/prerecord-calib-jitter-${RUN_ID}.json"
    if "$PROBE_BIN_DIR/genlock-jitter-report" --file "$CALIB_LOG" --json > "$CALIB_JITTER_JSON" 2>"$OUTDIR/prerecord-calib-jitter-err-${RUN_ID}.log"; then
      CALIB_STRIH_MEASURED="$OUTDIR/prerecord-calib-strih-measured-${RUN_ID}.json"
      CALIB_MARGIN_FILE="$OUTDIR/prerecord-calib-margin-${RUN_ID}.txt"
      if python3 "$HERE/prerecord_phase_calibrate.py" --jitter-json "$CALIB_JITTER_JSON" \
           --out "$CALIB_STRIH_MEASURED" --margin-out "$CALIB_MARGIN_FILE"; then
        CALIB_MARGIN_MS="$(cat "$CALIB_MARGIN_FILE" 2>/dev/null | tr -d '[:space:]')"
        case "$CALIB_MARGIN_MS" in ''|*[!0-9.]*) CALIB_MARGIN_MS=10 ;; esac
        echo "    [calib] applying corrected pins to strih (mains), jitter-headroom margin=${CALIB_MARGIN_MS}ms…"
        python3 "$HERE/phase_sync_calibrate.py" --host "$STRIH" --password "$STRIH_PW" \
          --measured-json "$CALIB_STRIH_MEASURED" --apply --margin-ms "$CALIB_MARGIN_MS" \
          --gate-bin "$PROBE_BIN_DIR/phase-sync-gate" \
          --json-path "$OUTDIR/prerecord-calib-strih-pins-${RUN_ID}.json" \
          || echo "WARNING: #757 strih pin apply failed — continuing with whatever pins were already set" >&2
      else
        echo "WARNING: #757 no usable per-camera measurement in this run's calibration sweep — skipping pin apply, using whatever was already set" >&2
      fi
    else
      echo "WARNING: #757 genlock-jitter-report found no usable audit lines in the calibration window (see $OUTDIR/prerecord-calib-jitter-err-${RUN_ID}.log) — skipping pin apply" >&2
    fi
  else
    echo "WARNING: #757 could not fetch strih's OBS log for calibration — skipping pin apply" >&2
  fi
  set -e
else
  echo "[4g/8] #757 pre-record phase auto-pin — SKIPPED (PRERECORD_PHASE_CALIBRATE=$PRERECORD_PHASE_CALIBRATE, ALL_CAMBOX=${ALL_CAMBOX:-0})"
fi

# #757 binding user directive (2026-07-15): imag runs EVERY NDI input at the fixed 3ms floor,
# ALWAYS — INDEPENDENT of PRERECORD_PHASE_CALIBRATE (the strih-only auto-pin mechanism above,
# currently off by default per Correction 3). Never gated on that mechanism's own on/off state
# or its success/failure — imag's fixed floor is a standing invariant, not a calibration output.
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  set +e
  if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
    imag_leg_skip_note "[4g/8b] imag 3ms latency-floor enforce (#757)" "$IMAG_OFFLINE_ACK_REASON"
  else
  echo "[4g/8b] #757 enforcing imag's fixed 3ms floor on every NDI input (self-healing, imag never gets per-camera equalization)"
  python3 "$HERE/imag_latency_enforce.py" --host "$IMAG_IP" --password "${IMAG_PW:-newlevel}" 2>&1 \
    | sed 's/^/    [imag-latency] /'
  fi
  set -e
fi

# [4h/8pre] #900 — phase-sync RE-ANCHOR: the establisher the [4h/8] floor gate never had. Re-derive
# the ACTIVE pin set from the ALREADY-persisted per-camera transits (phase-sync-last.json) and apply
# it, so the gate below always has an automatic establisher. This is a RE-ANCHOR, NOT the #757
# RE-MEASUREMENT (which stays off by default): no new measurement, no new kernel -- it reads the
# transits that produced the currently-working pins, restricts them to CAMERA_ACTIVE_SET, and
# re-runs the UNCHANGED compute_phase_sync_offsets kernel. A provable NO-OP when the active set is
# unchanged (same transits -> same pins -> live pins already satisfy the convention -> zero writes);
# a pure constant shift of every surviving pin when a camera leaves/joins. ON by default (opt-out
# PHASE_REANCHOR=0), gated on ALL_CAMBOX like the gate itself, and FAIL-LOUD (never best-effort like
# [4g/8]): a missing/malformed persisted basis, or one that does not cover the active set, is a
# genuine "no calibration basis" state -- exit before StartRecord rather than reach [4h/8] behind
# pins nobody established. It reads phase-sync-last.json but NEVER clobbers it (the applied set is
# recorded only to a run-scoped file).
# #1003: in measurement-eq mode the [4h/8eq] apply below writes the delivery-equalized-deep strih
# pins, which is MUTUALLY EXCLUSIVE with the #900 re-anchor (both write strih pins). ONE flag
# forces the re-anchor off so the two can never disagree; the verbatim default-ON line below is
# unchanged (0 is set+non-empty, so `${PHASE_REANCHOR:-1}` keeps it 0).
measurement_eq_enabled && PHASE_REANCHOR=0
PHASE_REANCHOR="${PHASE_REANCHOR:-1}"
if [ "$PHASE_REANCHOR" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  echo "[4h/8pre] #900 phase-sync re-anchor — establish the ACTIVE floor pin set from persisted transits (no new measurement) before the [4h/8] floor gate"
  python3 "$HERE/phase_sync_reanchor.py" --host "$STRIH" --password "$STRIH_PW" \
    --active-set "$CAMERA_ACTIVE_SET" \
    --gate-bin "$PROBE_BIN_DIR/phase-sync-gate" \
    --out-json "$OUTDIR/reanchor-strih-pins-${RUN_ID}.json" \
    --apply \
    || {
      echo "ERROR: [preflight] FAIL: #900 phase-sync re-anchor could not establish the active pin set from the persisted transits (missing/malformed calibration basis, does not cover the active set, or an apply failure) -- a genuine 'no calibration basis' state. Recalibrate: python3 scripts/phase_sync_calibrate.py --host \$STRIH --measured-json <path> --apply" >&2
      exit 1
    }
fi

# [4h/8eq] #1003 — MEASUREMENT-WINDOW equalization apply + pre-record read-back verify (profile
# mode only). Applies the delivery-equalized-deep per-camera STRIH pins (snapshot-set; restored on
# teardown via the existing `teardown --host STRIH` path), then read-back VERIFIES both boxes' values
# are actually in force before StartRecord. This REPLACES the [4h/8] #893 floor gate below in profile
# mode (the deep pins deliberately violate the min==3ms floor invariant #893 checks, so #893 would
# false-fail a correct profile run); the verify catches a surviving writer / failed apply / wrong
# input name that #893 never could. The stream hold was set at [4/8]; verified here too. FAIL-LOUD
# before StartRecord, never behind a broken measurement config.
if measurement_eq_enabled && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  echo "[4h/8eq] #1003 measurement-eq — apply delivery-equalized-deep strih pins + verify both boxes in force"
  python3 "$HERE/obs_phase2.py" apply-measurement-pins --host "$STRIH" --password "$STRIH_PW" \
    --profile "$MEASUREMENT_EQ_PROFILE" \
    || { echo "ERROR: [preflight] FAIL: #1003 apply-measurement-pins failed on strih" >&2; exit 1; }
  python3 "$HERE/obs_phase2.py" verify-measurement-pins --host "$STRIH" --password "$STRIH_PW" \
    --profile "$MEASUREMENT_EQ_PROFILE" --role strih \
    || { echo "ERROR: [preflight] FAIL: #1003 strih equalized pins not in force before StartRecord" >&2; exit 1; }
  python3 "$HERE/obs_phase2.py" verify-measurement-pins --host "$STREAM" --password "$STREAM_PW" \
    --profile "$MEASUREMENT_EQ_PROFILE" --role stream \
    || { echo "ERROR: [preflight] FAIL: #1003 stream hold not in force before StartRecord" >&2; exit 1; }
fi

# [4h/8] #893 — machine-checked gate: at least one camera in CAMERA_ACTIVE_SET must sit at the
# strih phase-sync floor (min(pin[c] for c in CAMERA_ACTIVE_SET) == 3ms -- the "slowest active
# camera pinned at the floor" convention phase_sync_calibrate.py implements). Reads the LIVE
# pins over OBS WebSocket -- never the persisted phase-sync-last.json, which is exactly what
# let #893's live-vs-file divergence go unnoticed (the file kept showing a healthy 2026-07-09
# calibration while the live pins had all drifted away from it) -- and FAILS the run HERE, before
# StartRecord, rather than after a ~25-minute recording. Owner directive (#893): "nech to je tiez
# v gate ze minimalne jedna aktivna kamera musi mat latenciu 3ms, nech sa tu dalsie tyzdne
# nekrutime vo veciach ktore uz si vedel".
# #1003 review 🔴: SKIP this pre-align floor CHECK when the [4i/8align] floor-3 auto-align OWNS the
# pins (QR_ALIGN=1, the default) — the two are MUTUALLY EXCLUSIVE floor-enforcers. [4i/8align]
# ENFORCES "slowest on-air camera at 3ms" (a stronger, verified re-measure) over the whole align set
# incl. cam4, so this earlier read-only check is redundant AND would otherwise abort a run that
# [4i/8align] would have rescued (or, cross-run, a prior align that legitimately floored cam4 leaves
# no ACTIVE-set camera at 3). Same exclusion shape as the measurement_eq guard beside it.
if [ "${ALL_CAMBOX:-0}" = "1" ] && [ "${QR_ALIGN:-1}" != "1" ] && ! measurement_eq_enabled; then
  echo "[4h/8] #893 phase-sync active-floor gate — at least one ACTIVE strih camera must sit at the 3ms floor"
  python3 "$HERE/phase_sync_active_floor_check.py" --host "$STRIH" --password "$STRIH_PW" \
    --active-set "$CAMERA_ACTIVE_SET" \
    --gate-bin "$PROBE_BIN_DIR/phase-sync-active-floor-gate" \
    || {
      echo "ERROR: [preflight] FAIL: #893 no ACTIVE camera sits at the strih phase-sync floor -- the slowest-active-camera-at-3ms convention has drifted. Recalibrate: python3 scripts/phase_sync_calibrate.py --host \$STRIH --measured-json <path> --apply" >&2
      exit 1
    }
fi

# [4i/8align] #1003 floor-3 per-run camera alignment (owner rework 2026-08-20). The SIMULTANEOUS
# painter-QR screenshot spread across every on-air strih camera (CAMERA_ALIGN_SET, INCLUDING cam4)
# is BOTH the alignment basis AND the owner's acceptance instrument ("ak spravím screenshot, musím
# vidieť rovnaké monotonic a time v KAŽDOM QR"). This BLOCKING preflight measures the spread, applies
# floor-3 pins (the slowest/max-transport camera -> pin 3, every other -> 3 + its relative delivery
# delta from the exact gen_ts_ns difference — RELATIVE-only, never the rejected absolute-depth
# 90/160/184), RE-MEASURES, and ABORTS the run with a per-camera named reason if it stays
# misaligned. strih per-source pins ONLY: the stream NDI 2ME PGM hold (operator A/V-align domain)
# and imag's 3ms floor are never in the align set. The independent SOURCE-side cross-camera spread
# gate (recording-verdict) stays a separate blocking proof, unchanged. Runs on the ALL_CAMBOX path;
# SKIPPED under MEASUREMENT_EQ (that opt-in profile is the OTHER strih-pin writer) and via QR_ALIGN=0.
QR_ALIGN="${QR_ALIGN:-1}"
if [ "$QR_ALIGN" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ] && ! measurement_eq_enabled; then
  echo "[4i/8align] #1003 floor-3 camera alignment via simultaneous painter-QR spread (strih on-air set incl. cam4)"
  . "$HERE/lib/qr-align.sh"
  qr_align_run "$STRIH" "$STRIH_PW" || {
    echo "ERROR: [4i/8align] FAIL: #1003 cameras could not be floor-3 aligned — see the per-camera reason above. The run is ABORTED (owner rework: measure -> align (floor 3) -> verify -> FAIL if it cannot align)." >&2
    exit 1
  }
else
  echo "[4i/8align] #1003 floor-3 camera alignment — SKIPPED (QR_ALIGN=$QR_ALIGN, ALL_CAMBOX=${ALL_CAMBOX:-0}, measurement_eq opt-in profile owns strih pins when on)"
fi

# [4j/8settle] issue 1221 — measured genlock-FIFO settle-wait between the [4i/8align] pin writes and
# the record step below. Each per-source latency pin write in [4i/8align] re-parameterises that
# input's genlock FIFO -> a relock/drain/regain era (the genlock-fifo-limit-cycle class); recording
# immediately after measured that transient, not steady-state (verdict 950927573: per-window
# derived_uniform_fraction 0.644 -> 0.967 monotone convergence, strict-contiguity faults
# concentrated in the head windows, tail already >= the issue-1142 floor). This step POLLS strih's
# genlock-fifo audit relock/underrun/dropped_due/late_hold DELTAS and proceeds once every aligned
# input SEEN in the log has gone quiet for N consecutive passes -- a WAIT ON A MEASURED signal, not
# a blind sleep (no-timeout-band-aids). BOUNDED by a budget and FAIL-OPEN with a loud WARN: it never
# aborts the run and never waits unbounded (downstream gates judge the recording). Runs on the
# ALL_CAMBOX path (same path [4i/8align] + the sweep run on); E2E_GENLOCK_SETTLE=0 disables it.
# Placed BEFORE the freeze-watch arm below so the watcher never logs the pre-record relock era. New
# lines only, via the sourced-helper pattern (issue 675) -- no existing anchor line is edited.
E2E_GENLOCK_SETTLE="${E2E_GENLOCK_SETTLE:-1}"
if [ "$E2E_GENLOCK_SETTLE" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  # shellcheck source=scripts/lib/genlock-settle.sh
  . "$HERE/lib/genlock-settle.sh"
  GENLOCK_SETTLE_WATCHED="$(camera_align_ndi_sources_excluding_csv "${PREFLIGHT_EXCLUDED_CAMS:-}")"
  echo "[4j/8settle] issue 1221 waiting for genlock FIFO to settle after the align pin writes (inputs: ${GENLOCK_SETTLE_WATCHED:-<none>}, budget ${E2E_GENLOCK_SETTLE_BUDGET_S:-180}s)"
  genlock_settle_wait "$STRIH_USER" "$STRIH_PW" "$STRIH" "$GENLOCK_SETTLE_WATCHED" \
    "${E2E_GENLOCK_SETTLE_QUIET_PASSES:-2}" "${E2E_GENLOCK_SETTLE_BUDGET_S:-180}" "${E2E_GENLOCK_SETTLE_POLL_S:-7}"
else
  echo "[4j/8settle] issue 1221 genlock-FIFO settle-wait — SKIPPED (E2E_GENLOCK_SETTLE=$E2E_GENLOCK_SETTLE, ALL_CAMBOX=${ALL_CAMBOX:-0})"
fi

# #758 item 3 — arm the in-run freeze watch for the WHOLE recording window (StartRecord through
# StopRecord), right before StartRecord. ALL_CAMBOX only (mirrors the [0/8]/[1/8] preflight + [2/8]/[2b/8] reverify's
# own gating) — excludes any acked-offline camera.
#
# #761 (2026-07-15): same per-box probe-target split as the [1/8] preflight and the [2/8]/[2b/8]
# sender-bounce reverify above — strih's "MV NDI cam<N>" clone items are now DISABLED (scenes
# switched to same-source), so this watch probes the MAIN "NDI cam<N>" inputs instead, kept
# always-active by the built-in OBS Multiview grid projector. See #763 for imag's separate
# clone-based model.
#
# #757 DIAGNOSTIC KNOB (2026-07-15, temporary, bisecting the uniform copies≈gaps regression):
# this watch's per-poll-cycle work is 7 sources x 3 samples x GetSourceScreenshot on strih's
# FULL-RES main inputs (frozen-camera-gate.py's _capture_timelines) -- GetSourceScreenshot is a
# KNOWN synchronous graphics-thread stall on the requested source. Roughly one screenshot every
# ~2s throughout the ENTIRE recording window is a plausible periodic-disturbance mechanism for
# the uniform ~10-17-pairs-per-30s-segment pattern (#757's own auto-pin/margin mechanism has
# been ruled OUT — the pattern persists with it fully disabled and static pins restored).
# LIVE_FREEZE_WATCH=0 lets a bisect run disable ONLY this mechanism (nothing else) to test that
# hypothesis directly.
#
# #757 KILL-SHOT RESULT (2026-07-15, run 1874027737): pairs PERSIST with freeze-watch fully
# disabled (9-14/segment) -- the mechanism is EXONERATED, it is not the periodic-disturbance
# cause. Default restored to 1 (the #758 safety feature stays ON in normal operation); the
# LIVE_FREEZE_WATCH knob itself is left in place (harmless, still useful for any future
# re-test) but is no longer expected to matter for this bisect.
LIVE_FREEZE_WATCH="${LIVE_FREEZE_WATCH:-1}"
FREEZE_WATCH_PID_FILE="$OUTDIR/freeze-watch.pid"
FREEZE_WATCH_POISON_FILE="$OUTDIR/freeze-watch-poison.txt"
if [ "$LIVE_FREEZE_WATCH" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  # #827 follow-up: derive the watched source list from CAMERA_ACTIVE_SET (camera-set.sh) minus
  # any acked-offline box -- never a literal 1..7 range (same fix as the [0/8]/[1/8] preflight
  # loops above).
  FREEZE_WATCH_SOURCES="$(camera_active_ndi_sources_excluding_csv "${PREFLIGHT_EXCLUDED_CAMS:-}")"
  if [ -n "$FREEZE_WATCH_SOURCES" ]; then
    echo "[5/8 pre] arming in-run freeze watch (#758): $FREEZE_WATCH_SOURCES"
    live_freeze_watch_start "$FREEZE_WATCH_PID_FILE" "$FREEZE_WATCH_POISON_FILE" \
      "$STRIH" "$FREEZE_WATCH_SOURCES" "$PROBE_BIN_DIR"
  fi
else
  echo "[5/8 pre] in-run freeze watch — SKIPPED (LIVE_FREEZE_WATCH=$LIVE_FREEZE_WATCH, ALL_CAMBOX=${ALL_CAMBOX:-0})"
fi

echo "[5/8] StartRecord on strih + stream (program = certified prod scene) + imag (#462 — program routed to the camera under test by [4a/8], #682)"
# #627: `record --action start` now polls GetRecordStatus itself right after StartRecord and
# raises (nonzero exit) if the output isn't genuinely active + writing growing bytes — a
# dead-on-arrival recording (StartRecord reports success but writes 0 bytes) is caught within
# seconds instead of silently discovered only when the file is fetched at the end of the run.
# `set -euo pipefail` (top of this script) makes that nonzero exit abort this run immediately;
# no extra guard needed at this call site.
python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action start
STRIH_RECORDING_STARTED=1   # #649: flag so cleanup()'s StopRecord-first block stops THIS box
python3 "$HERE/obs_phase2.py" record --host "$STREAM" --action start
STREAM_RECORDING_STARTED=1  # #649: same — set only once this box's OWN StartRecord succeeded
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[5/8] imag StartRecord (#462)" "$IMAG_OFFLINE_ACK_REASON"
else
python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action start
IMAG_RECORDING_STARTED=1    # #649: same — set only once this box's OWN StartRecord succeeded
fi

# #705: snapshot the recording window's START epoch (wall clock) — the mid-recording
# capture-rate check ([7b/8] below) bounds its journal read to EXACTLY this window, so a
# #656/#663 defect that recurs DURING the recording gets its own distinct diagnostic instead of
# only surfacing (mis-attributed) via the eventual zero-loss/A-V verdict.
CAPTURE_RATE_WINDOW_START_EPOCH="$(date +%s)"

# [5b/8] #707 B1 (freeze+jump discriminator, SECOND prong) — arm a lightweight per-cambox TCP-to-
# strih + NIC-counter sampler for the WHOLE [5/8]->[7/8] recording window (stopped + harvested in
# [7c/8] below). Prong 1 (the box-side emit_rate_ring 1s WARN, src/emit_rate_ring.rs) answers "did
# the box's OWN emit path dip during the freeze?"; THIS answers "did the box->strih LINK stall at
# the same instant?" — a ballooning Send-Q / retransmit spike / NIC error+drop increment. On the
# next ~2.7s FREEZE the two prongs' CSVs discriminate the unnamed final layer: transport spike ->
# LINK (#688 cable/port + NIC power-mgmt), 1s emit dip -> box emit path (#707), both clean while
# strih freezes -> NDI SDK internal drop. Best-effort: every ssh is guarded so an unreachable box
# never aborts the recording (pure diagnostics — its absence is just a missing CSV). TRANSPORT_SAMPLER=0
# disables it. The remote loop self-terminates after TS_MAX_SECS even if [7c/8]'s stop is skipped,
# so it can never orphan on a box.
TRANSPORT_SAMPLER="${TRANSPORT_SAMPLER:-1}"
if [ "$TRANSPORT_SAMPLER" = "1" ]; then
  TS_MAX_SECS="${TS_MAX_SECS:-$(( DURATION + 120 ))}"   # orphan-safety ceiling >> the window; [7c/8] stops it promptly
  TS_REMOTE_CSV="/tmp/transport-sampler-${RUN_ID}.csv"
  TS_REMOTE_PID="/tmp/transport-sampler-${RUN_ID}.pid"
  # Sample the SOURCE cambox always; under the ALL_CAMBOX sweep also sample cam2(painter) + every
  # ACTIVE secondary camera (camera_active_secondary_set(), #827 — the ONE place fleet membership
  # is declared), each of which is cut into strih program during the sweep. #707 (2026-07-15):
  # cam7 was missing here (fleet grew 6->7, #755, after this list was written) -- LINK could not
  # be auto-ruled-out for its residual events without this coverage.
  TRANSPORT_SAMPLER_BOXES="$CAM1_IP"
  if [ "${ALL_CAMBOX:-0}" = "1" ]; then
    TRANSPORT_SAMPLER_BOXES="$TRANSPORT_SAMPLER_BOXES $PAINTER_IP"
    for _ts_cn in $(camera_active_secondary_set); do
      TRANSPORT_SAMPLER_BOXES="$TRANSPORT_SAMPLER_BOXES $(camera_secondary_ip "$_ts_cn")"
    done
  fi
  echo "[5b/8] transport sampler: arming ss/ip per-box counter sampler (~250ms, ${TS_MAX_SECS}s ceiling) on: $TRANSPORT_SAMPLER_BOXES"
  for _ts_ip in $TRANSPORT_SAMPLER_BOXES; do
    _ts_label="$(transport_sampler_box_label "$_ts_ip")"
    timeout 15 sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "root@${_ts_ip}" \
      "$(transport_sampler_remote_start_cmd "$STRIH" 250 "$TS_REMOTE_CSV" "$TS_MAX_SECS" "$TS_REMOTE_PID")" \
      2>/dev/null | sed "s/^/    ${_ts_label}(${_ts_ip}): /" \
      || echo "    ${_ts_label}(${_ts_ip}): WARNING — transport sampler arm failed (continuing; diagnostics only)"
  done
fi

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
# DEFAULT_MAP; scene names follow the input labels 1:1, .claude/skills/genlock/SKILL.md).
#
# #753 PIVOT (2026-07-14, binding user directive): the mapping is now 1:1 -- "chcem aby uz bolo
# ze cam 1 je cam1 ndi source, nie pomenene" (cam N IS the camN NDI source, not relabeled). Every
# scene:label pair below is now the literal identity — 'Cam N'->CAMN:
#   'Cam 1'->CAM1(.61)  'Cam 2'->CAM2(.62)  'Cam 3'->CAM3(.63)  'Cam 4'->CAM4(.64)
# The pre-2026-07-14 OFFSET table this default used to encode (Cam 5->CAM1, Cam 1->CAM3, Cam
# 3->CAM4, Cam 4->CAM5, unchanged for Cam 2/Cam 6) is HISTORY — see set-ndi-mapping.py's module
# docstring for the full pre/post record; do NOT reintroduce it.
#
# #24/#399 (history): CAM3 was re-added to the default after its #301 SSH-down exclusion closed
# 2026-06-30; #312 corrected the #333 painter exclusion (cam2's camera-box daemon keeps
# CAPTURING+EMITTING its own NDI feed throughout a TEST run — only its framebuffer is freed for
# the separate frame-probe painter process via CAMERA_BOX_NO_DISPLAY=1, see the `[2b/8]` deploy
# loop below — so cam2's own chain is JUST AS MEASURABLE as every other camera's, via the SAME
# digital capture-burn mechanism, recording-verdict.rs's CAMERA_UNDER_TEST_NODES). cam5/cam6
# (fleet growth 4→6, #451) were added the same way cam3/cam4 were by #624; cam7 (fleet growth
# 6→7, #753) the same way again — and #827 (2026-07-27) retired all three (grabber cards
# returned to their owner, boxes powered off), shrinking the default back to cam1-4.
# tests/python/test_cambox_sweep_mapping.py cross-checks this default against DEFAULT_MAP so a
# future re-map can't desync it again.
#
# #708 GOTCHA (2026-07-12, HISTORICAL — mooted by the #753 pivot above, kept for context): before
# 2026-07-14 this scene:label pairing looked like a label->box translation table but wasn't — the
# set-ndi-mapping.py NDI-source-binding INVERSION exactly cancelled it, so
# `all_cambox_continuity.segments[].cambox` label CAMN == physical box camN directly even though
# the scene:label pair itself was offset. Since the pivot the pairing IS a literal identity too —
# same conclusion (CAMN == camN, no translation needed), now for the simpler reason that there is
# no inversion left to cancel. See `.claude/skills/e2e` "CORRECTION (2026-07-12, #708)" for the
# pre-pivot 4-way verification, marked historical there too.
# #827: derived from CAMERA_ACTIVE_SET (camera-set.sh's camera_active_sweep_pairs) — never a
# second hardcoded scene:label list. Re-enabling a retired camera (e.g. cam5) is adding it to
# CAMERA_ACTIVE_SET; this default picks it up automatically.
CAMBOX_SWEEP="${CAMBOX_SWEEP:-$(camera_active_sweep_pairs)}"
SEGMENT_SECS="${SEGMENT_SECS:-30}"

# #887: imag "produced vs presented" independent check -- REPORT-ONLY, never sets $GATE, never
# aborts. See scripts/lib/imag-presented-frame-check.sh's own header for the full rationale +
# honest scoping (every field name says "presented on HDMI-1"/"produced by the compositor",
# never anything about the physical projection surface). Runs here, during the recording window
# (after StartRecord, before the ALL_CAMBOX sweep / steady-state hold step that follows), so
# the sample genuinely overlaps what the
# recording itself is measuring. A box with no resolvable HDMI-A-1 (unplugged, no debugfs CRC
# support) just means this diagnostic has nothing to report -- never a run failure.
IMAG_PRESENTED_SAMPLE_S="${IMAG_PRESENTED_SAMPLE_S:-10}"
echo "[5c/8] #887 imag presented-frame check (report-only) — sampling ${IMAG_PRESENTED_SAMPLE_S}s of HDMI-A-1's DRM CRC counter vs the compositor's own GetStats produced count"
IMAG_PRESENTED_JSON="$OUTDIR/imag-presented-${RUN_ID}.json"
_ipf_resolve_out="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
  "${IMAG_USER:-newlevel}@$IMAG_IP" "$(imag_presented_frame_resolve_cmd "${IMAG_PW:-newlevel}")" 2>/dev/null)" || _ipf_resolve_out=""
_ipf_crtc_dir="$(printf '%s\n' "$_ipf_resolve_out" | sed -n 's/^IMAG_PRESENTED_CRTC_DIR=//p')"
_ipf_card_num="$(printf '%s\n' "$_ipf_resolve_out" | sed -n 's/^IMAG_PRESENTED_CARD_NUM=//p')"
if [ -n "$_ipf_crtc_dir" ] && [ -n "$_ipf_card_num" ]; then
  _ipf_before="$(python3 "$HERE/imag_produced_frame_check.py" --host "$IMAG_IP" 2>/dev/null)" || _ipf_before=""
  _ipf_sample_out="$(sshpass -p "${IMAG_PW:-newlevel}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${IMAG_USER:-newlevel}@$IMAG_IP" \
    "$(imag_presented_frame_sample_cmds "${IMAG_PW:-newlevel}" "$_ipf_card_num" "$_ipf_crtc_dir" "$IMAG_PRESENTED_SAMPLE_S")" 2>/dev/null)" || _ipf_sample_out=""
  _ipf_after="$(python3 "$HERE/imag_produced_frame_check.py" --host "$IMAG_IP" 2>/dev/null)" || _ipf_after=""
  _ipf_presented="$(printf '%s\n' "$_ipf_sample_out" | sed -n 's/.*presented_frame_count=\([0-9]*\).*/\1/p')"
  _ipf_repeated="$(printf '%s\n' "$_ipf_sample_out" | sed -n 's/.*repeated_frame_count=\([0-9]*\).*/\1/p')"
  _ipf_produced_before="$(printf '%s\n' "$_ipf_before" | sed -n 's/.*renderTotalFrames=\([0-9]*\).*/\1/p')"
  _ipf_produced_after="$(printf '%s\n' "$_ipf_after" | sed -n 's/.*renderTotalFrames=\([0-9]*\).*/\1/p')"
  _ipf_produced_delta="null"
  if [ -n "$_ipf_produced_before" ] && [ -n "$_ipf_produced_after" ]; then
    _ipf_produced_delta=$(( _ipf_produced_after - _ipf_produced_before ))
  fi
  echo "    #887: compositor_produced_frames=${_ipf_produced_delta} hdmi1_presented_frames=${_ipf_presented:-n/a} hdmi1_repeated_frames=${_ipf_repeated:-n/a} (sample window ${IMAG_PRESENTED_SAMPLE_S}s)"
  cat > "$IMAG_PRESENTED_JSON" <<JSONEOF
{
  "note": "produced/presented describe imag's OWN compositor pipeline up to its HDMI-A-1 connector ONLY -- this does NOT verify the physical projection surface or anything downstream of the cable (issue 887 option 3, software-only)",
  "sample_window_s": ${IMAG_PRESENTED_SAMPLE_S},
  "compositor_produced_frames": ${_ipf_produced_delta},
  "hdmi1_presented_frames": ${_ipf_presented:-null},
  "hdmi1_repeated_frames": ${_ipf_repeated:-null}
}
JSONEOF
  echo "    wrote $IMAG_PRESENTED_JSON"
else
  echo "    #887: could not resolve imag's HDMI-A-1 connector to a debugfs CRTC dir — skipping (report-only, never a run failure)"
fi

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
  # #1086: arm the deliberate keepalive-bypass cold cut (no-op unless COLD_CUT_BYPASS_CAM is set).
  cold_cut_reset_state
  for _seg in "${_SWEEP_PLAN[@]}"; do
    _scene="${_seg%%$'\t'*}"; _label="${_seg##*$'\t'}"
    # #1086: if this segment is the bypass target and its receiver is idled, RESTORE it (topping up
    # the cold hold) right before the cut so the switch lands on a receiver re-created from cold.
    # Inert no-op unless COLD_CUT_BYPASS_CAM is set; always returns 0 (never trips set -e).
    cold_cut_before_segment "$_label" "$STRIH" "${OBS_PASSWORD:-}" "$HERE/obs_phase2.py"
    # Cut strih PROGRAM to this cambox's scene; the subcommand prints the switch epoch-ns
    # (time.time_ns()) on stdout and fails loud if the scene renders black (dead cambox).
    _switch_ns="$(python3 "$HERE/obs_phase2.py" switch --host "$STRIH" --program-scene "$_scene")"
    echo "    [seg $((_seg_i+1))/${_seg_n}] $_label via '$_scene' switched at ${_switch_ns} ns"
    # #1086: once the target has appeared and the sweep has moved OFF it, IDLE its receiver so it
    # goes genuinely cold for the hidden window. Inert no-op by default; always returns 0.
    cold_cut_after_segment "$_label" "$STRIH" "${OBS_PASSWORD:-}" "$HERE/obs_phase2.py"
    if [ -z "$_SWITCH_START_NS" ]; then
      _SWITCH_START_NS="$_switch_ns"          # first switch = window 0 start
    else
      _SEG_BOUNDARIES+=("$_switch_ns")        # each later switch CLOSES the previous segment
    fi
    interruptible_sleep "$SEGMENT_SECS"
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
  interruptible_sleep "$(( DURATION + RECORD_PAD ))"
fi

echo "[7/8] StopRecord + download strih + stream recordings to dev1 (NO grab #179)"
# #758 item 3 — disarm the in-run freeze watch now that the recording window has ended (its own
# verdict is read further below, at recording-verdict time, from the poison file this leaves behind).
live_freeze_watch_stop "$FREEZE_WATCH_PID_FILE"
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
if [ "$IMAG_OFFLINE_ACKED" = 1 ]; then
  imag_leg_skip_note "[7/8] imag StopRecord (#462)" "$IMAG_OFFLINE_ACK_REASON"
else
IMAG_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$IMAG_IP" --action stop) \
  || echo "WARNING: imag StopRecord returned non-zero (continuing; recording may already be stopped)" >&2
fi
# #705: snapshot the recording window's END epoch — pairs with CAPTURE_RATE_WINDOW_START_EPOCH
# ([5/8] above) to bound the mid-recording capture-rate check immediately below.
CAPTURE_RATE_WINDOW_END_EPOCH="$(date +%s)"
echo "    strih host file:  ${STRIH_HOST_PATH:-<unknown>}"
echo "    stream host file: ${STREAM_HOST_PATH:-<unknown>}"
echo "    imag host file:   ${IMAG_HOST_PATH:-<unknown>}  (#462 — stays ON imag, decoded in place below)"

# #1124 item 3 — POST-record stomp re-check (profile mode only, report-only). Runs HERE, right
# after StopRecord while the measurement pins/hold are STILL in force (cleanup()'s teardown
# restores them only at exit), so a mid-recording writer that stomped them surfaces as a loud
# diagnostic instead of an opaque A/V-gate result. The re-check itself is in the sourced lib
# (#675 anchor-safe pattern); it never gates. Still in the [7/8] `set +e` region.
if measurement_eq_enabled && [ "${ALL_CAMBOX:-0}" = "1" ]; then
  # #1133: report-only, never gates — the trailing `|| true` guarantees no abort even if the region
  # ever leaves `set +e` (the helper also returns 0 internally; belt-and-suspenders).
  measurement_eq_post_record_stomp_recheck "$MEASUREMENT_EQ_PROFILE" "$STRIH" "$STRIH_PW" "$STREAM" "$STREAM_PW" || true
fi

# [7b/8] #894: burn-unit run-integrity check. Runs BEFORE the merge/verdict below, so a burn unit
# that died mid-run (e.g. the exact #894 device-steal race: a hotplug's udev rule restarting
# production, stealing /dev/videoN back with 77/NOPERM) gets its OWN loud, distinctly-labeled
# failure printed here -- never silently indistinguishable from recording-verdict.rs's (unrelated)
# frozen_leg verdict. BURN_UNIT_INTEGRITY_MSG is read by the merge/verdict GATE combinator below.
echo "[7b/8] burn-unit run-integrity check (#894) — did every camera-box-burn-*.service stay ACTIVE through the recording window?"
BURN_UNIT_INTEGRITY_MSG=""
_burn_unit_integrity_check() {  # camname ip unit
  local _bcn="$1" _bip="$2" _bunit="$3" _brc=0 _bout _bstate _bmsg
  _bout="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_bip" \
    "$(udev_camera_box_burn_unit_state_cmd "$_bunit")" 2>/dev/null)" || _brc=$?
  _bstate="$(udev_camera_box_burn_unit_state_from_output "$_bout")"
  if [ "$_brc" -ne 0 ] || ! udev_camera_box_burn_unit_is_healthy "$_bstate"; then
    _bmsg="$(udev_camera_box_burn_unit_integrity_message "$_bcn" "$_bunit" "${_bstate:-<unreachable, ssh rc=$_brc>}")"
    echo "    $_bmsg" >&2
    BURN_UNIT_INTEGRITY_MSG="${BURN_UNIT_INTEGRITY_MSG}${BURN_UNIT_INTEGRITY_MSG:+; }${_bmsg}"
  else
    echo "    $_bcn burn unit ($_bunit) active — OK"
  fi
}
_burn_unit_integrity_check "$CAMERA_NAME" "$CAM1_IP" "$CAM1_BURN_UNIT"
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _cn_ip_burn in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _cn="${_cn_ip_burn%%=*}"; _crest="${_cn_ip_burn#*=}"; _cip="${_crest%%=*}"
    _burn_unit_integrity_check "$_cn" "$_cip" "camera-box-burn-${_cn}-${RUN_ID}"
  done
fi

# [7b/8] #895 + issue 946 + issue 910: run-integrity RESTART-event scan. ONE recognised-event
# table (scripts/lib/self-heal-attribution.sh's restart_event_* functions) keys on the RESET/
# CRITICAL EVENT lines themselves -- the shared "#663 self-heal: USB reset attempt #N succeeded"
# (fired via EITHER the #656 jitter band OR the #717 sustained band), PLUS the issue-945
# capture-wedge (exit 79) and issue-944 emit-freeze (exit 81) watchdog CRITICAL lines -- so all
# three are caught by ONE scan rather than three parallel greps. Read from BOTH sources: (a) the
# box's journald window, scoped to the EXACT recording window via its CURRENT camera-box.service
# InvocationID (re-resolved here -- the [2/8] redeploy restarts camera-box, same #693/#705
# discipline), AND (b) the burn instance's OWN log (issue 910: during an E2E burn the harness
# stops camera-box.service and runs the source/secondary capture as a transient systemd-run unit
# logging to /tmp/cbox-burn*.log, so journald is STRUCTURALLY blind to the recording window --
# mirroring the issue-992 capture-rate burn-log read). Swept across EVERY active camera. Detected
# events are printed loudly NOW (never only in post-hoc forensics) and threaded into
# recording-verdict.rs below via --restart-event KIND:CAMBOX:EPOCH_NS so the pure
# self_heal_attribution module can re-attribute any correlating frozen_leg window -- ALLOWED to
# fire (never suppressed), and never silently swallowed: an event with no correlating window still
# gates the run (#895).
echo "[7b/8] restart-event scan (#895 self-heal reset + issue 945 capture-wedge + issue 944 emit-freeze) — did any active camera restart during the recording window? (checked via BOTH journald AND each camera's burn-instance log, issue 910)"
RESTART_EVENTS=()
_restart_event_scan() {  # camname ip burnlog
  local _scn="$1" _sip="$2" _sblog="$3" _sinv _sjournal _sburn _scombined _sline _skind _sns _sfound=0
  _sinv="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_sip" \
    "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"
  _sjournal="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_sip" \
    "$(self_heal_reset_window_journalctl_cmd "$_sinv" "$CAPTURE_RATE_WINDOW_START_EPOCH" "$CAPTURE_RATE_WINDOW_END_EPOCH")" \
    2>/dev/null || true)"
  _sburn="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_sip" \
    "$(restart_event_burn_log_grep_cmd "$_sblog")" \
    2>/dev/null || true)"
  # KIND:EPOCH_NS from journald AND the burn-instance log (issue 910). During a real E2E burn the
  # two sources are effectively mutually exclusive (camera-box.service is stopped, so journald is
  # blind and only the burn log yields events); the `sort -u` is a defensive dedup for the edge
  # case where the unit genuinely runs and both sources happen to observe the same event.
  _scombined="$( { restart_events_from_journal_output "$_sjournal"; restart_events_from_burn_log_output "$_sburn"; } | sort -u )"
  while IFS= read -r _sline; do
    [ -z "$_sline" ] && continue
    _skind="${_sline%%:*}"; _sns="${_sline#*:}"
    _sfound=1
    echo "    $(restart_event_scan_message "$_skind" "$_scn" "$_sns")"
    RESTART_EVENTS+=("${_skind}:${_scn}:${_sns}")
  done <<< "$_scombined"
  if [ "$_sfound" -eq 0 ]; then
    echo "    $_scn: no restart event in this recording window — OK"
  fi
}
_restart_event_scan "$CAMERA_NAME" "$CAM1_IP" "/tmp/cbox-burn.log"
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _cn_ip_sh in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _cn="${_cn_ip_sh%%=*}"; _crest="${_cn_ip_sh#*=}"; _cip="${_crest%%=*}"
    _restart_event_scan "$_cn" "$_cip" "/tmp/cbox-burn-${_cn}.log"
  done
fi

# [7c/8] #707 B1 transport sampler (second prong) — stop the per-box samplers armed in [5b/8] and
# harvest each per-run CSV into the run dir (scp), so the TCP Send-Q / retransmit / NIC-counter
# timeline lands BESIDE the recordings + verdict for the freeze discriminator. Already inside the
# [7/8] `set +e` region (above), and every ssh/scp is `||`-guarded, so a failed harvest here is a
# warning — never an abort of the run. Reuses the same $TRANSPORT_SAMPLER_BOXES / $TS_REMOTE_*
# vars the [5b/8] arm set (same shell). The remote loop also self-terminates after TS_MAX_SECS, so
# even a fully-skipped stop cannot orphan a sampler on a box.
if [ "${TRANSPORT_SAMPLER:-1}" = "1" ] && [ -n "${TRANSPORT_SAMPLER_BOXES:-}" ]; then
  echo "[7c/8] transport sampler: stopping + harvesting per-box CSVs into $OUTDIR"
  mkdir -p "$OUTDIR"
  for _ts_ip in $TRANSPORT_SAMPLER_BOXES; do
    _ts_label="$(transport_sampler_box_label "$_ts_ip")"
    timeout 15 sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "root@${_ts_ip}" \
      "$(transport_sampler_remote_stop_cmd "$TS_REMOTE_PID")" >/dev/null 2>&1 \
      || echo "    ${_ts_label}(${_ts_ip}): WARNING — transport sampler stop returned non-zero (continuing)"
    _ts_local="$OUTDIR/transport-sampler-${_ts_label}-${RUN_ID}.csv"
    if timeout 20 sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
        "root@${_ts_ip}:$TS_REMOTE_CSV" "$_ts_local" >/dev/null 2>&1; then
      echo "    ${_ts_label}(${_ts_ip}): transport CSV -> $_ts_local ($(wc -l < "$_ts_local" 2>/dev/null || echo 0) rows)"
    else
      echo "    ${_ts_label}(${_ts_ip}): WARNING — no transport CSV harvested (sampler may not have armed on this box)"
    fi
  done
fi

# [7b/8] capture-delivery-rate POST-recording check (#705): the [0/8] preflight above only
# proves $CAMERA_NAME was clean BEFORE the recording started -- the #656/#663 ShadowCast judder
# is confirmed to RECUR mid-session (PR #704's own real-verdict CI run: cam1's own
# recurrence_heal_count=30 at the time of that incident), so a clean preflight does not
# guarantee the recording stayed clean for its whole duration. Re-resolve the CURRENT
# camera-box.service InvocationID here -- NOT the stale $CAPTURE_RATE_INVOCATION_ID the [0/8]
# preflight resolved: [2/8] (which already ran by this point) redeploys + restarts camera-box,
# so that id names a KILLED prior process instance and a query scoped to it would silently see
# NOTHING that happened during the actual recording. Re-query the SAME #656 journal signal,
# this time bounded to the EXACT recording window via journalctl's own native absolute-time
# --since=@epoch/--until=@epoch filtering (capture_rate_window_journalctl_cmd) -- no bash-side
# timestamp math needed. This FAILS the run HERE, before the ~5-10 min decode step below ever
# launches (the recording itself already happened and can't be un-spent, but the decode-time
# portion of the budget is saved), with a diagnostic (capture_rate_recurrence_message) that reads
# distinctly from the preflight's own failure so a human/CI reader never again has to manually
# correlate journalctl timestamps against the recording window by hand (exactly what #703's own
# PR #704 diagnosis required).
#
# (#992 ROZHODNUTÉ -- supervisor, gate rerun 31028767542 evidence, see issue 992 comment
# https://github.com/zbynekdrlik/camera-box/issues/992#issuecomment-5195254731): HARD bands
# (#656 jitter, #971 chronic escalation, #663 self-heal-RESET) still exit 1 unchanged. The #717
# SUSTAINED band is INFORMATIONAL BY DESIGN (issue 909: the genlock decimation gate absorbs this
# over-rate into exact NDI output, which is WHY 909 decoupled it from the USB reset one layer
# down) -- cam1's ShadowCast 2 over-rate is CHRONIC (redevelops ~2min after any fresh device
# open, issue 889), so hard-failing this gate on the same line one layer up would recreate the
# issue-909 mistake: the gate would go permanently red before any verdict is ever computed, so
# the dupe-preferring decimation fix this PR ships (whose entire point is producing clean
# recordings UNDER over-rate) could never be proven -- exactly what happened in gate rerun
# 31028767542. Grep the HARD pattern FIRST, then the SUSTAINED pattern SEPARATELY (never sharing
# one `tail -1` -- a run containing BOTH a reset and a sustained line must never have the reset
# masked by `tail -1` landing on the sustained line instead), at BOTH call sites below.
echo "[7b/8] capture-delivery-rate POST-recording check — $CAMERA_NAME must not have recurred a #656/#663/#971 HARD defect during the recording (checked via BOTH journald AND the burn instance's own log); #717 SUSTAINED band is measured too but is REPORT-ONLY by design (issue 909 -- absorbed by the genlock decimation gate) and never fails this gate (#705/#992)"
CAPTURE_RATE_WINDOW_INVOCATION_ID="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"
CAPTURE_RATE_RECURRENCE_LINE="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(capture_rate_window_journalctl_cmd "$CAPTURE_RATE_WINDOW_INVOCATION_ID" "$CAPTURE_RATE_WINDOW_START_EPOCH" "$CAPTURE_RATE_WINDOW_END_EPOCH") | grep -E '$(capture_rate_defect_grep_pattern_hard)' | tail -1" \
  2>/dev/null || true)"
if [ -n "$CAPTURE_RATE_RECURRENCE_LINE" ]; then
  echo "ERROR: $(capture_rate_recurrence_message "$CAMERA_NAME" "$CAPTURE_RATE_RECURRENCE_LINE")" >&2
  echo "       matched journal line: $CAPTURE_RATE_RECURRENCE_LINE" >&2
  exit 1
fi
# (#992 ROZHODNUTÉ) the #717 SUSTAINED band, checked separately from the HARD pattern above --
# report-only, never exit 1 (see the design-decision comment at the top of this step).
CAPTURE_RATE_WINDOW_SUSTAINED_LINE="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(capture_rate_window_journalctl_cmd "$CAPTURE_RATE_WINDOW_INVOCATION_ID" "$CAPTURE_RATE_WINDOW_START_EPOCH" "$CAPTURE_RATE_WINDOW_END_EPOCH") | grep -E '$(capture_rate_sustained_band_grep_pattern)' | tail -1" \
  2>/dev/null || true)"
if [ -n "$CAPTURE_RATE_WINDOW_SUSTAINED_LINE" ]; then
  echo "$(capture_rate_sustained_band_warn_message "$CAMERA_NAME" "$CAPTURE_RATE_WINDOW_SUSTAINED_LINE")"
fi
# (#992) journald is BLIND to the actual E2E burn instance: the deploy step above
# unconditionally does `systemctl stop camera-box` and launches $CAMERA_NAME's capture as a
# TRANSIENT systemd-run unit whose stdout/stderr are redirected DIRECTLY to /tmp/cbox-burn.log
# (--property=StandardOutput=append:.../StandardError=append:...) -- never through journald at
# all. A clean journald read above therefore only proves the (killed) camera-box.service
# process instance was clean before the stop; it says NOTHING about what actually ran during
# this recording. Read the burn instance's own log file directly -- no epoch window needed here
# (unlike the journalctl read above): the deploy step already does `rm -f /tmp/cbox-burn.log`
# immediately before systemd-run launches THIS run's burn, so the file's entire content is
# already scoped to this exact recording. Same HARD-first/SUSTAINED-second split as above.
CAPTURE_RATE_BURN_LOG_LINE="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(capture_rate_burn_log_grep_cmd "/tmp/cbox-burn.log" "$(capture_rate_defect_grep_pattern_hard)")" \
  2>/dev/null || true)"
if [ -n "$CAPTURE_RATE_BURN_LOG_LINE" ]; then
  echo "ERROR: $(capture_rate_burn_log_recurrence_message "$CAMERA_NAME" "$CAPTURE_RATE_BURN_LOG_LINE")" >&2
  echo "       matched burn-instance log line: $CAPTURE_RATE_BURN_LOG_LINE" >&2
  exit 1
fi
CAPTURE_RATE_BURN_LOG_SUSTAINED_LINE="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$CAM1_IP" \
  "$(capture_rate_burn_log_grep_cmd "/tmp/cbox-burn.log" "$(capture_rate_sustained_band_grep_pattern)")" \
  2>/dev/null || true)"
if [ -n "$CAPTURE_RATE_BURN_LOG_SUSTAINED_LINE" ]; then
  echo "$(capture_rate_burn_log_sustained_band_warn_message "$CAMERA_NAME" "$CAPTURE_RATE_BURN_LOG_SUSTAINED_LINE")"
fi
echo "    ok: no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording window (${CAPTURE_RATE_WINDOW_START_EPOCH}..${CAPTURE_RATE_WINDOW_END_EPOCH}) -- also checked its burn-instance log (/tmp/cbox-burn.log, #992); #717 SUSTAINED band (if any) was report-only and did not fail this gate (issue 909)"

# [7b/8] secondary-camera capture-delivery-rate POST-recording sweep (#994). The source-camera
# step above is the HARD gate that aborts the run. Under ALL_CAMBOX=1 every active secondary
# camera also runs its OWN capture burn ([2b/8], logging to /tmp/cbox-burn-<camname>.log) and is
# cut into strih program, so a capture-rate defect on a SECONDARY during the recording (issue 889:
# cam1 AND cam2 both went over-rate at once -- cam2 is a secondary) is just as real, but was
# invisible: the source-camera step above only ever read $CAMERA_NAME. Sweep every secondary the
# SAME way the issue-910 restart-event scan already does (CAMBOX_SECONDARY_DEPLOY, journald window
# + each box's own burn log, HARD band + #717 SUSTAINED band grepped separately). REPORT-ONLY (a
# loud WARNING #994:, never aborts): a secondary's reset events are already threaded report-only to
# the verdict by the issue-910 scan + issue-914 frozen_leg/self_heal_reset decoupling, and
# hard-failing here on a chronic secondary quirk (cam2 is a secondary) would recreate the exact
# permanently-red-gate mistake issue-909/914 fixed -- a hard secondary gate, if ever wanted, is its
# own ticket (green-gate-first). Option 2 of #994 (the reset-EVENT sweep across secondaries) is
# already delivered by issue 910; this closes option 1 for the capture-rate defect-declaration
# signal. Mirrors the source-camera check's two-source (journald + burn log) / two-band (HARD +
# SUSTAINED) reads, all report-only, reusing the existing camera-parameterized helpers.
_capture_rate_secondary_scan() {  # camname ip burnlog
  local _cn="$1" _cip="$2" _cblog="$3" _cinv _cline
  _cinv="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
    "systemctl show -p InvocationID --value camera-box 2>/dev/null" 2>/dev/null || true)"
  # journald window, HARD band -> report-only WARN
  _cline="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
    "$(capture_rate_window_journalctl_cmd "$_cinv" "$CAPTURE_RATE_WINDOW_START_EPOCH" "$CAPTURE_RATE_WINDOW_END_EPOCH") | grep -E '$(capture_rate_defect_grep_pattern_hard)' | tail -1" \
    2>/dev/null || true)"
  if [ -n "$_cline" ]; then
    echo "    $(capture_rate_secondary_recurrence_warn_message "$_cn" "$_cline")"
  fi
  # journald window, #717 SUSTAINED band -> report-only WARN. Reuses the source-camera #992
  # sustained formatter as-is: its line reads "WARNING #992: <cam> ..." (a #992-labelled line even
  # on a secondary is intentional -- the sustained band's meaning is band-#992, not camera-role;
  # the cam name disambiguates), so no separate #994 sustained formatter is warranted.
  _cline="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
    "$(capture_rate_window_journalctl_cmd "$_cinv" "$CAPTURE_RATE_WINDOW_START_EPOCH" "$CAPTURE_RATE_WINDOW_END_EPOCH") | grep -E '$(capture_rate_sustained_band_grep_pattern)' | tail -1" \
    2>/dev/null || true)"
  if [ -n "$_cline" ]; then
    echo "    $(capture_rate_sustained_band_warn_message "$_cn" "$_cline")"
  fi
  # burn-instance log, HARD band -> report-only WARN (journald-blind sibling, issue 992/910)
  _cline="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
    "$(capture_rate_burn_log_grep_cmd "$_cblog" "$(capture_rate_defect_grep_pattern_hard)")" \
    2>/dev/null || true)"
  if [ -n "$_cline" ]; then
    echo "    $(capture_rate_secondary_burn_log_recurrence_warn_message "$_cn" "$_cline")"
  fi
  # burn-instance log, #717 SUSTAINED band -> report-only WARN (existing #992 formatter, same
  # intentional #992-labelled reuse as the journald sustained read above)
  _cline="$(sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_cip" \
    "$(capture_rate_burn_log_grep_cmd "$_cblog" "$(capture_rate_sustained_band_grep_pattern)")" \
    2>/dev/null || true)"
  if [ -n "$_cline" ]; then
    echo "    $(capture_rate_burn_log_sustained_band_warn_message "$_cn" "$_cline")"
  fi
}
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  echo "[7b/8] secondary-camera capture-rate report-only sweep (#994) — every active secondary camera; the source-camera step above is the hard gate"
  for _cn_ip_crs in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _cn="${_cn_ip_crs%%=*}"; _crest="${_cn_ip_crs#*=}"; _cip="${_crest%%=*}"
    _capture_rate_secondary_scan "$_cn" "$_cip" "/tmp/cbox-burn-${_cn}.log"
  done
  echo "    ok: secondary-camera capture-rate sweep complete (#994, report-only) — any WARNING #994 / WARNING #992 lines above are informational and did not fail this gate"
fi

# #359: an UNCONDITIONAL early kill is still wrong -- frame-probe writes the ground-truth CSV
# ONLY on a clean self-exit or a graceful shutdown (src/probe/run.rs); the old unconditional
# `pkill -x frame-probe` here fired at ~DURATION, BEFORE the painter's own
# DURATION+PAINTER_PRE_RECORD_SLACK_SECS self-exit deadline, so it never wrote a fresh CSV and a
# STALE leftover got pulled → a fake catastrophic FAIL (run 354002). That history is why this
# stayed a pure WAIT for a long time. #1223 revises it: since issue 1186, frame-probe installs a
# SIGTERM handler that runs the SAME teardown as a clean self-exit (writes the CSV + marker log,
# blanks fb0) -- live-proven by a systemd stop of the permanent painter unit producing the
# identical teardown sequence. So a GRACEFUL term sent AFTER the recording has already stopped is
# safe and deliberate (never an early/mid-recording kill, which would still be wrong): it makes
# the wait below succeed within seconds regardless of how large the pre-record slack above is,
# instead of the wait idling out that whole slack on every normal run.
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$PAINTER_IP" \
  "pkill -TERM -x frame-probe 2>/dev/null; true"
# WAIT for the painter to self-exit (whether from the graceful term just sent, or its own
# --duration-secs deadline): poll until its PROCESS is gone AND a non-empty /tmp/painter.csv
# freshly written THIS run exists (remote mtime >= run start), bounded by its --duration-secs
# deadline + grace. A backstop kill only fires if it overran, so the painter can never be left
# holding /dev/fb0.
PAINTER_EXIT_DEADLINE=$(( PAINTER_LAUNCH_EPOCH + DURATION + PAINTER_PRE_RECORD_SLACK_SECS ))
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
# #716: persist each cam-box burn-run fps log to dev1, right beside the cam1-capture-stats sidecar
# above. Each box's own /tmp/cbox-burn*.log (the fine-grained `Streaming: fps emitted/captured`
# telemetry, written FILE-ONLY via StandardOutput=append: and invisible to journald) is `rm -f`'d
# by the NEXT run's deploy, so without this only the LATEST run's fps log ever survives — blocking
# capture-rate forensics against any specific past recording window. Best-effort (WARN, never
# abort); the scp lives in the sourced lib so it never disturbs a static-anchor test's own region.
cbox_burn_log_persist "$CAM_PW" "$CAM1_IP" cam1 "$RUN_ID" "$OUTDIR"
if [ "${ALL_CAMBOX:-0}" = "1" ]; then
  for _bl_entry in "${CAMBOX_SECONDARY_DEPLOY[@]}"; do
    _bl_cn="${_bl_entry%%=*}"; _bl_rest="${_bl_entry#*=}"; _bl_ip="${_bl_rest%%=*}"
    cbox_burn_log_persist "$CAM_PW" "$_bl_ip" "$_bl_cn" "$RUN_ID" "$OUTDIR"
  done
fi
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
  # / http.server. This path predates #701 (plain scp/ssh actually reaches strih/stream, and is
  # the PREFERRED transfer for a multi-GB recording — the win-* MCP FileDownload breaks above a
  # few MB) and has not been migrated; the harness still expects the caller (the
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
# #624 deliverable 4 / #312 item 2 PR B: the per-camera A/V-offset gate measures each camera's
# DEVIATION from AV_EXPECTED_MS -- the EXPECTED MEASURED offset. #1178 RE-DERIVATION (2026-08-29):
# the -92 default was a STALE-PAINTER artifact (issue 1138 class -- an un-pinned cam2 frame-probe
# painter emitted the QPSK marker without its own emit-delay compensation). With the marker delay
# now compensated AT SOURCE (issue-1138 painter redeploy sha f42c66917455), a correctly aligned rig
# MEASURES ~0 again, so the default returns to 0. This value MIRRORS recording-verdict's
# av_window::RIG_VIDEO_LEG_OFFSET_MS (the source of truth; drift-guarded by
# tests/harness_av_expected_calibration_parity_1178.rs). Override for an operator-dialed value, or
# after a rig-verified physical video-chain change re-derives a genuine non-zero leg.
# Always passed so the gate is explicit in the printed command, not silently implicit.
AV_EXPECTED_MS="${AV_EXPECTED_MS:-0}"
# #1003: in measurement-eq mode the A/V gate must expect the value the pin+hold DESIGN implies
# (the profile's coherent av_expected — derived so the equalized-deep pins + rebalanced hold land
# the common A/V level there), NOT a blindly-inherited 0. With the shipped profile this IS 0, but
# a re-derived profile that dials a nonzero expectation carries the gate with it.
measurement_eq_enabled && AV_EXPECTED_MS="$MEASUREMENT_EQ_AV_EXPECTED"
VERDICT_ARGS+=(--av-expected-ms "$AV_EXPECTED_MS")
# #1003 review finding 2: in measurement-eq mode raise the LIVE #1035 cam->strih p99 bound by the
# marker camera's pin delta (else the deep cam2 pin false-fails that separate pin-dependent gate by
# construction). Only the ALL_CAMBOX merge path runs in profile mode, but set it here too for parity.
measurement_eq_enabled && VERDICT_ARGS+=(--max-cam-strih-p99-latency-ms "$MEASUREMENT_EQ_CAM_STRIH_BOUND")
# #855: thread the SAME operator ack (CAMBOX_OFFLINE_ACK, already resolved above from either an
# explicit override or the repo-level rig-fleet.txt default) straight through to recording-verdict
# unchanged -- no shell-side re-parsing. Consumed ONLY by the all_cambox_av_sync gate: an acked
# box is reported EXCLUDED there instead of judged UNKNOWN/FAIL on samples it was never going to
# produce. Harmless no-op (`--offline-ack-cams ""`) when nothing is acked this run.
VERDICT_ARGS+=(--offline-ack-cams "${CAMBOX_OFFLINE_ACK:-}")

# #208 PER-BOX DECODE-IN-PLACE (refines #193): by default decode EACH recording ON ITS OWN BOX —
# the strih recording ON the strih box, the stream recording ON the stream box — and merge the
# SMALL partial JSONs on dev1. A recording is NEVER copied box-to-box (nor to dev1); only the
# small partial JSONs (+ the painter CSV) move. The OLD #193 flow ran a SINGLE fused verdict on
# the stream box, which forced the ~700 MB strih .mkv to be copied strih→stream first — that copy
# is GONE. The harness EMITS the per-box plans (upload recording-verdict.exe → extract the partial
# on each box → pull back ONLY the small JSON); by DEFAULT the agent/operator holding the win-* MCP
# executes them (a human/MCP-pasteable plan). #703's E2E_EXECUTE_VERDICT=1 (below) instead runs
# these plans for real over ssh/scp — #701 proved plain scp/ssh reaches strih/stream directly, no
# MCP paste-step needed — but that opt-in is reserved for the REQUIRED CI gate; a manual/
# workflow_dispatch run still gets the plan-only default here.
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
  # #703: default to the REAL box-local paths OBS's own StopRecord ([7/8] above) already
  # returned ($STRIH_HOST_PATH/$STREAM_HOST_PATH) rather than an unresolved placeholder — this
  # is what EXECUTE mode needs to actually ssh/scp against (a literal "<...>" placeholder would
  # 404 on the box); it also saves the plan-print/MCP operator a manual fill-in step. Still
  # override-able via STRIH_REC_WIN/STREAM_REC_WIN env for a manual/debug dispatch.
  STREAM_REC_WIN="${STREAM_REC_WIN:-${STREAM_HOST_PATH:-<the stream recording AS IT LIVES ON THE STREAM BOX>}}"
  STRIH_REC_WIN="${STRIH_REC_WIN:-${STRIH_HOST_PATH:-<the strih recording AS IT LIVES ON THE STRIH BOX>}}"
  # #703: E2E_EXECUTE_VERDICT=1 (set ONLY by the CI workflow's REQUIRED pull_request gate) makes
  # this harness ACTUALLY RUN the strih+stream decode-in-place over ssh (#701: proven to work
  # with the targets.md creds — retires the old "scp/ssh to Windows is denied" premise for
  # THESE TWO boxes) instead of merely printing the plan for a human/MCP operator to paste.
  # workflow_dispatch / manual operator runs stay at the default 0 — unchanged plan-printing.
  # STRIH_USER/STRIH_PW/STREAM_USER/STREAM_PW are defined near the top of this script (with
  # CAM_PW), reused here.
  E2E_EXECUTE_VERDICT="${E2E_EXECUTE_VERDICT:-0}"
  EXEC_STRIH_ARGS=()
  EXEC_STREAM_ARGS=()
  if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
    if [ -z "${WIN_VERDICT_EXE_LOCAL:-}" ] || [ ! -f "$WIN_VERDICT_EXE_LOCAL" ]; then
      echo "ERROR: #703 E2E_EXECUTE_VERDICT=1 but WIN_VERDICT_EXE_LOCAL is unset/missing — the CI" >&2
      echo "       workflow must download the matching probe-tools-windows-amd64 artifact (built" >&2
      echo "       by ci.yml's windows-probe job for this SAME commit) and export" >&2
      echo "       WIN_VERDICT_EXE_LOCAL before invoking recording-e2e.sh." >&2
      exit 1
    fi
    EXEC_STRIH_ARGS=(--execute --verdict-exe-local "$WIN_VERDICT_EXE_LOCAL" --local-out-dir "$OUTDIR")
    EXEC_STREAM_ARGS=(--execute --verdict-exe-local "$WIN_VERDICT_EXE_LOCAL" --local-out-dir "$OUTDIR")
    echo "    #703: E2E_EXECUTE_VERDICT=1 — strih+stream will be decoded FOR REAL over ssh (not just planned)."
  fi
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
  # #703: wrapped in a function so EXECUTE mode can launch it BACKGROUNDED (parallel with the
  # stream extract below, and with imag's own extract further down) while default plan-print
  # mode still calls it in the FOREGROUND exactly as before (EXEC_STRIH_ARGS is empty there, so
  # the invocation text/behavior is unchanged from the pre-#703 call).
  run_strih_extract() {
    "$HERE/recording-verdict-on-strih.sh" \
      --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" --strih-rec "$STRIH_REC_WIN" \
      "${EXEC_STRIH_ARGS[@]}" \
      -- --extract-partial strih --strih "$STRIH_REC_WIN" --capture-fps "$STRIH_CAPTURE_FPS" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$BURN_STRIH_RUN_ID" \
         $CG --out "$STRIH_PARTIAL_WIN"
  }
  STRIH_EXTRACT_PID=""
  if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
    STRIH_EXTRACT_LOG="$OUTDIR/strih-extract-${RUN_ID}.log"
    run_strih_extract >"$STRIH_EXTRACT_LOG" 2>&1 &
    STRIH_EXTRACT_PID=$!
    echo "    #703: strih extract launched in background (pid $STRIH_EXTRACT_PID, log $STRIH_EXTRACT_LOG)"
  else
    run_strih_extract
  fi
  echo "    pull back to dev1: $STRIH_PARTIAL  AND the #186 pixel-proof dir $STRIH_PIXELS"
  echo "      (win-strih FileDownload $STRIH_PARTIAL_WIN -> $STRIH_PARTIAL;"
  echo "       win-strih FileDownload $STRIH_PIXELS_WIN -> $STRIH_PIXELS  [absent on a clean run])"

  # #312 item 2 (PR A): the cam2 continuous A/V-sync marker log lives on dev1 (pulled from cam2
  # above, a plain Linux scp) but the stream recording — the ONLY recording that co-locates the
  # marker's audio track with the cam2 dual-QR video — lives on the WINDOWS stream box. This
  # plan-only path predates #701 (plain scp/ssh actually reaches strih/stream), so
  # this PUSHES the small marker CSV via the win-stream-snv MCP (FileUpload), mirroring the exact
  # PLAN convention `av_sync_calibrate.py`'s REMOTE PUSH plan already uses. `--extract-partial
  # stream` then decodes it ON-BOX (alongside the burns) and carries the result through the small
  # partial JSON to the dev1 merge — never the recording itself.
  AV_MARKER_WIN="${AV_MARKER_WIN:-$OUT_DIR_WIN\\av-markers-${RUN_ID}.csv}"
  _av_marker_args=""
  if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$MARKER_CSV" ]; then
    echo "    --- [8/8b-pre] PUSH the cam2 A/V-sync marker log to the stream box ---"
    if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
      # #703 (live-CI-run finding, 2026-07-11): this push used to be PRINT-ONLY (the plan text
      # below) — never a problem before, because [8/8] never actually EXECUTED anything, so the
      # stream extract's --av-marker-log arg was never really READ. The FIRST real EXECUTE-mode
      # ALL_CAMBOX=1 run exposed it for real: recording-verdict.exe on stream errored `os error 2
      # — The system cannot find the file specified` trying to read a marker CSV that was never
      # actually there. Actually scp it now.
      echo "      win_ssh_upload $MARKER_CSV -> stream:$AV_MARKER_WIN"
      win_ssh_upload "$STREAM_USER" "$STREAM_PW" "$STREAM" "$MARKER_CSV" "$AV_MARKER_WIN"
    else
      echo "      win-stream-snv FileUpload $MARKER_CSV -> $AV_MARKER_WIN"
    fi
    _av_marker_args="--av-marker-log $AV_MARKER_WIN"
  fi

  # #707 EVENT-FORENSICS: push the SAME switch-schedule JSON the dev1 merge already consumes to
  # the stream box too, so its own --extract-partial can ALSO locate residual copy/gap events
  # (via segment_continuity, same as the merge) and flag their ±2-frame neighbourhoods for #186
  # pixel proof WHILE the recording is still local to this box (the merge, on dev1, never has the
  # recording — see the #208 module doc). Mirrors the --av-marker-log push immediately above.
  SWITCH_SCHEDULE_WIN="${SWITCH_SCHEDULE_WIN:-$OUT_DIR_WIN\\switch-schedule-${RUN_ID}.json}"
  _switch_schedule_args=""
  if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$SWITCH_SCHEDULE_JSON" ]; then
    echo "    --- [8/8b-pre] PUSH the #312 switch schedule to the stream box (#707 event-forensics) ---"
    if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
      echo "      win_ssh_upload $SWITCH_SCHEDULE_JSON -> stream:$SWITCH_SCHEDULE_WIN"
      win_ssh_upload "$STREAM_USER" "$STREAM_PW" "$STREAM" "$SWITCH_SCHEDULE_JSON" "$SWITCH_SCHEDULE_WIN"
    else
      echo "      win-stream-snv FileUpload $SWITCH_SCHEDULE_JSON -> $SWITCH_SCHEDULE_WIN"
    fi
    _switch_schedule_args="--switch-schedule $SWITCH_SCHEDULE_WIN"
  fi

  echo "    --- [8/8b] extract the STREAM partial ON the stream box (win-stream-snv), in place ---"
  # The stream recording carries all three burns; --extract-partial stream decodes it IN PLACE on
  # the stream box. It is passed ONLY its own --stream recording — NEVER the strih recording (the
  # strih recording is decoded on the strih box above), so no box-to-box copy is ever needed.
  # #703: same function-wrapped backgrounding as run_strih_extract above — launched right after
  # strih (so BOTH decodes run concurrently in EXECUTE mode), foreground/unchanged otherwise.
  run_stream_extract() {
    "$HERE/recording-verdict-on-stream.sh" \
      --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" --stream-rec "$STREAM_REC_WIN" \
      "${EXEC_STREAM_ARGS[@]}" \
      -- --extract-partial stream --stream "$STREAM_REC_WIN" --capture-fps "$STREAM_CAPTURE_FPS" \
         --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" \
         --cam2-run-id "$RUN_ID" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$BURN_STRIH_RUN_ID" \
         --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
         $_av_marker_args \
         $_switch_schedule_args \
         $CG --out "$STREAM_PARTIAL_WIN"
  }
  STREAM_EXTRACT_PID=""
  if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
    STREAM_EXTRACT_LOG="$OUTDIR/stream-extract-${RUN_ID}.log"
    run_stream_extract >"$STREAM_EXTRACT_LOG" 2>&1 &
    STREAM_EXTRACT_PID=$!
    echo "    #703: stream extract launched in background (pid $STREAM_EXTRACT_PID, log $STREAM_EXTRACT_LOG)"
  else
    run_stream_extract
  fi
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
    # #1143: capture OBS's OWN record-session render stats (drawn/attempted/lagged frames +
    # lagged_pct + max in-record render ms) from the imag OBS log stop-stats and thread them into
    # the imag extract, so the merged verdict's imag block carries the observer-effect proof
    # REPORT-ONLY (a high lagged_pct ⇒ the RECORDER juddered the chain, not the delivery — #1130).
    # Best-effort: empty on ANY failure, and the flag is added ONLY when non-empty (an empty value
    # would fail the extract's JSON parse). Never aborts the extract (report-only observability).
    IMAG_RECORD_STATS_ARGS=()
    _imag_rrstats="$(python3 "$HERE/imag_record_stats_capture.py" --host "$IMAG_IP" --recording "$IMAG_HOST_PATH" 2>/dev/null || true)"
    [ -n "$_imag_rrstats" ] && IMAG_RECORD_STATS_ARGS=(--record-render-stats "$_imag_rrstats")
    # #832: recording-verdict-on-imag.sh has its OWN independent IMAG_BOX default (it is a
    # standalone tool, also runnable by hand) -- pass the SAME resolved host recording-e2e.sh
    # itself is targeting (scripts/imag-host.sh), so this [8/8c] decode step never silently
    # ssh's to a DIFFERENT imag box than the one the rest of the run just deployed/recorded to.
    IMAG_BOX="$IMAG_IP" "$HERE/recording-verdict-on-imag.sh" \
      --verdict-bin "$VERDICT_BIN" --out-dir "$IMAG_REMOTE_OUT_DIR" --local-out-dir "$OUTDIR" \
      --imag-rec "$IMAG_HOST_PATH" \
      -- --extract-partial imag --imag "$IMAG_HOST_PATH" --imag-capture-fps "$IMAG_CAPTURE_FPS" \
         --out "$IMAG_REMOTE_PARTIAL" "${IMAG_RECORD_STATS_ARGS[@]}" \
    && echo "    pulled back to dev1: $IMAG_PARTIAL  (+ the #186 pixel-proof dir $IMAG_PIXELS, if any)" \
    || echo "WARNING: #462 recording-verdict-on-imag.sh failed (imag unreachable / stale binary / ssh hiccup) — \
continuing WITHOUT the imag partial; the merge below will omit --merge-partials imag=... (cam→imag proof skipped this run)." >&2
  else
    echo "WARNING: #462 no imag recording path (StopRecord returned none) — imag partial NOT produced;" >&2
    echo "         the merge below will run WITHOUT --merge-partials imag=... (cam→imag proof skipped)." >&2
  fi
  # issue 798: LOUD run-log twin of the verdict's full_chain.imag_leg_verified field — one distinct
  # greppable line, emitted the moment the [8/8c] outcome is known, naming the skip REASON. A green
  # run that silently skips the imag leg is a hidden partial (ONE full test, no partials). Report-
  # only — gates nothing. NEW call line only; the imag extract lines above are byte-unchanged (#675).
  # issue 1013: pass the acked-offline reason (3rd arg) so the marker names the true cause when imag
  # was a KNOWN-ABSENT box this run. IMAG_OFFLINE_ACK_REASON is non-empty ONLY when imag was acked
  # (an acked-but-reachable imag exits at the [0/8] reachability preflight, never reaching here), so
  # a normal run passes an empty 3rd arg and keeps the unchanged #798 behaviour.
  echo "    $(imag_leg_run_marker "${IMAG_PARTIAL:-}" "${IMAG_HOST_PATH:-}" "${IMAG_OFFLINE_ACK_REASON:-}")"

  # #703: EXECUTE mode — the strih [8/8a] + stream [8/8b] extracts were launched BACKGROUNDED
  # above (in parallel with each other AND with imag's own synchronous [8/8c] extract just
  # above), so by this point all three have had the SAME wall-clock window to finish instead of
  # serializing (the timing budget this fix's PR body documents). Wait for both now, BEFORE the
  # merge needs their partial JSONs on disk. A failure here is FATAL (unlike imag, which is
  # optional) — the merge cannot compute a real verdict missing either leg.
  if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
    echo "    #703: waiting for the backgrounded strih+stream decode-in-place to finish..."
    STRIH_EXTRACT_RC=0
    if [ -n "$STRIH_EXTRACT_PID" ]; then wait "$STRIH_EXTRACT_PID" || STRIH_EXTRACT_RC=$?; fi
    STREAM_EXTRACT_RC=0
    if [ -n "$STREAM_EXTRACT_PID" ]; then wait "$STREAM_EXTRACT_PID" || STREAM_EXTRACT_RC=$?; fi
    echo "    ----- strih extract log ($STRIH_EXTRACT_LOG) -----"
    cat "$STRIH_EXTRACT_LOG" 2>/dev/null || true
    echo "    ----- stream extract log ($STREAM_EXTRACT_LOG) -----"
    cat "$STREAM_EXTRACT_LOG" 2>/dev/null || true
    echo "    ------------------------------------"
    if [ "$STRIH_EXTRACT_RC" != "0" ] || [ "$STREAM_EXTRACT_RC" != "0" ]; then
      echo "ERROR: #703 strih extract rc=$STRIH_EXTRACT_RC stream extract rc=$STREAM_EXTRACT_RC — cannot" >&2
      echo "       compute the verdict without both partials. See the logs above for the root cause." >&2
      exit 1
    fi
    if [ ! -f "$STRIH_PARTIAL" ] || [ ! -f "$STREAM_PARTIAL" ]; then
      echo "ERROR: #703 extract reported success but a partial JSON is missing (strih=$STRIH_PARTIAL" >&2
      echo "       stream=$STREAM_PARTIAL) — cannot compute the verdict." >&2
      exit 1
    fi
    echo "    #703: both partials present — proceeding to the REAL merge (below)."
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
    --burn-cam7-run-id "$BURN_CAM7_RUN_ID" \
    --burn-strih-run-id "$BURN_STRIH_RUN_ID" --burn-stream-run-id "$BURN_STREAM_RUN_ID" \
    --av-expected-ms "$AV_EXPECTED_MS" \
    --offline-ack-cams "${CAMBOX_OFFLINE_ACK:-}" \
    --out-dir "$OUTDIR/pixel-proof" --json "$REPORT_JSON")
  # #462: fold in the imag partial WHEN [8/8c] actually produced one (it runs directly above, not
  # merely printed) — `if`-form so a missing/failed imag extract never `set -e`-aborts the merge of
  # the other two nodes (#178 resilience — degrade gracefully, never abort the whole proof).
  if [ -f "$IMAG_PARTIAL" ]; then
    MERGE_ARGS+=(--merge-partials "imag=$IMAG_PARTIAL")
  fi
  # #1142 — the full-chain ALL_CAMBOX merge REQUIRES a verified imag leg: a silently-skipped or
  # schema-degraded imag leg REDs the run (owner honesty mandate 2026-08-19). The ONE sanctioned
  # skip is an operator-acknowledged offline imag (issue 1013), already threaded via the ack flag
  # above; the imag per-frame content terms stay report-only regardless (issue 1130 observer
  # effect). NOT set on the strih+stream-only zero-loss-restart merge (a different code path).
  MERGE_ARGS+=(--require-imag-leg)
  # #377 — pass --colour-gate to the merge too (defense in depth): with it set, a partial that
  # LACKS its carried colour summary ERRORS LOUDLY ("re-run extract with --colour-gate") instead of
  # silently skipping a requested gate. The carried summary is honored regardless; this just catches
  # a stale/forgotten extract. Empty $CG (COLOUR_GATE=0) adds nothing.
  if [ -n "$CG" ]; then MERGE_ARGS+=("$CG"); fi
  if [ -f "$PAINTER_CSV" ]; then MERGE_ARGS+=(--painter "$PAINTER_CSV"); fi
  if [ -f "$CAM1_CAPTURE_STATS" ]; then MERGE_ARGS+=(--cam1-capture-stats "$CAM1_CAPTURE_STATS"); fi
  # #1003 review finding 2: raise the LIVE #1035 cam->strih p99 bound by the marker camera's pin
  # delta in measurement-eq mode (the merge path is the one profile mode actually uses).
  if measurement_eq_enabled; then MERGE_ARGS+=(--max-cam-strih-p99-latency-ms "$MEASUREMENT_EQ_CAM_STRIH_BOUND"); fi
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
  # #895 + issue 946 + issue 910: thread every run-integrity RESTART event detected during the
  # recording (the [7b/8] scan above -- self-heal reset, capture-wedge, emit-freeze; from journald
  # AND each camera's burn-instance log) into the merge/verdict call, so the pure
  # self_heal_attribution module can re-attribute any correlating frozen_leg window instead of
  # misreporting it as a camera fault. Tokens are KIND:CAMBOX:EPOCH_NS. An empty array (the common
  # case — nothing restarted) adds nothing.
  if [ "${#RESTART_EVENTS[@]}" -gt 0 ]; then
    for _re_event in "${RESTART_EVENTS[@]}"; do
      MERGE_ARGS+=(--restart-event "$_re_event")
    done
    echo "    restart events: --restart-event x${#RESTART_EVENTS[@]} (${RESTART_EVENTS[*]})"
  fi
  printf '      %q ' "$VERDICT_BIN" "${MERGE_ARGS[@]}"; echo

  # #703: EXECUTE mode runs the merge above FOR REAL right now (never just prints it) and makes
  # THIS SCRIPT'S OWN EXIT CODE the merge recording-verdict's exit code — the actual fix for the
  # bug this issue reports (the required CI gate used to `exit 0` here unconditionally, with NO
  # verdict ever computed). Print mode (default, unchanged) keeps emitting the plan text for a
  # human/MCP operator to run manually and reports its own honest "this is NOT the verdict" note.
  if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then
    echo "    #703: EXECUTING the merge above for real (not just printing it) — this run's own"
    echo "    exit code IS the merge recording-verdict's exit code."
    GATE=0
    "$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?
    # #894: a burn unit that died mid-run (device-steal, see [7b/8] above) is its OWN run-integrity
    # failure, independent of whatever recording-verdict.rs computed for frozen_leg -- force the
    # gate to fail with this EXPLICIT reason so it is never silently indistinguishable from a
    # genuine frozen camera. Only ever TIGHTENS an already-passing $GATE; never downgrades a
    # verdict-computed failure into a softer one.
    if [ -n "$BURN_UNIT_INTEGRITY_MSG" ]; then
      echo "RUN-INTEGRITY: $BURN_UNIT_INTEGRITY_MSG" >&2
      [ "$GATE" -eq 0 ] && GATE=1
    fi
    # #827: merge the fleet-preflight exclusion list into the verdict JSON so a run that excluded
    # boxes (ALL_CAMBOX only — PREFLIGHT_EXCLUDED_CAMS is otherwise unset/empty, and the merge
    # below is then a harmless no-op `excluded_cams: []`) can NEVER be read back as "full-fleet
    # clean" from the JSON alone. Best-effort: a jq failure here never affects $GATE — the
    # exclusion is already loudly printed in the [0/8] preflight log above.
    if [ -f "$REPORT_JSON" ]; then
      _pf_excluded_json="$(cambox_offline_ack_excluded_json "${PREFLIGHT_EXCLUDED_CAMS:-}")"
      _pf_tmp_json="${REPORT_JSON}.tmp"
      if jq --argjson excluded "$_pf_excluded_json" '.excluded_cams = $excluded' \
          "$REPORT_JSON" >"$_pf_tmp_json" 2>/dev/null; then
        mv "$_pf_tmp_json" "$REPORT_JSON"
      else
        echo "WARNING: #827 could not merge excluded_cams into $REPORT_JSON (jq failed) — the exclusion is still visible in the [0/8] preflight log above." >&2
        rm -f "$_pf_tmp_json"
      fi
    fi
    echo "[8/8] render the 2-graph report PNG"
    if [ -f "$REPORT_JSON" ]; then
      python3 "$HERE/recording-e2e-report.py" --json "$REPORT_JSON" --out "$REPORT_PNG" || \
        echo "WARNING: report render failed (non-fatal; JSON at $REPORT_JSON)" >&2
    fi
    # #1124 items 1+2 — POST-verdict report-only diagnostics (profile mode only). Item 1: staleness
    # of the checked-in profile vs THIS run's measured delivery (always). Item 2: edge-oscillation
    # FIFO-limit-cycle classifier, ONLY when the run FAILED ($GATE != 0), so a phase-edge flake
    # reads as the known #757-Corr-2 class not a regression. Both in the sourced lib (#675
    # anchor-safe); neither touches $GATE (report-only, run AFTER $GATE is decided above).
    # Guard is `measurement_eq_enabled` alone (no ALL_CAMBOX, unlike the [7/8] stomp check): the
    # [preflight] refuses MEASUREMENT_EQ=1 without ALL_CAMBOX=1, so profile mode here already implies
    # it; and the helper degrades to a "staleness NOT evaluated" note if the delivery block is absent.
    if measurement_eq_enabled; then
      # #1133: this region runs under the re-enabled `set -euo pipefail`; the trailing `|| true`
      # (plus the helper's own `return 0`) guarantees a report-only diagnostic can never set -e-abort
      # the run before `exit $GATE` below.
      measurement_eq_post_verdict_diagnostics "$MEASUREMENT_EQ_PROFILE" "$REPORT_JSON" "$GATE" || true
    fi
    # ============================================================================
    # #856 [8/8g] -- combine THIS run's own measured per-camera A/V offsets
    # (all_cambox_av_sync) into ONE rig-wide correction. Best-effort: computing the number
    # here never touches $GATE (a FAILED run's own offsets are still real measurements worth
    # correcting FROM for the next run). The actual OBS apply happens LAST, inside cleanup()
    # (see its own #856 step there) -- composing with the delivery-verify snapshot/restore
    # instead of being silently overwritten by it (that restore always runs on exit and would
    # stomp anything applied here).
    # ============================================================================
    AV_SYNC_COMBINE_LOG="$OUTDIR/av-sync-combine-${RUN_ID}.log"
    if AV_SYNC_APPLY_OFFSET_MS="$(python3 "$HERE/av_sync_combine_offsets.py" --verdict-json "$REPORT_JSON" 2>"$AV_SYNC_COMBINE_LOG")"; then
      echo "    [8/8g] #856: rig-wide A/V correction = ${AV_SYNC_APPLY_OFFSET_MS}ms (median of this run's own verdict==\"measured\" cameras) -- applied LAST in cleanup(), after the delivery-verify restore"
    else
      echo "    [8/8g] #856: refusing to compute a rig-wide A/V correction this run (see $AV_SYNC_COMBINE_LOG) -- stream genlock latency left untouched"
      AV_SYNC_APPLY_OFFSET_MS=""
    fi
    # #756 Member 3 — live per-source genlock latency pins + recommended pins, gathered AFTER
    # the verdict JSON exists (it needs this run's OWN delivery-latency table) and BEFORE the
    # Discord report composes (so the pins land in the SAME report, not a follow-up message).
    # Best-effort, fail-open like the report send itself below: a pins-snapshot failure (a box
    # unreachable, phase-sync-gate missing) must never affect $GATE or block the report — the
    # report composer simply omits the pins section when the file is missing/empty (see
    # e2e_discord_report.py's _section_latency_pins: "never fabricated — this run didn't gather
    # a pins snapshot").
    PINS_JSON="/tmp/latency-pins-${RUN_ID}.json"
    echo "    [8/8f-pre] #756: live latency-pins snapshot (strih+imag WS reads + recommended pins from this run's delivery table)"
    if ! python3 "$HERE/latency_pins_snapshot.py" \
        --strih-host "$STRIH" --imag-host "$IMAG_IP" --stream-host "$STREAM" \
        --password "${OBS_PASSWORD:-}" \
        --verdict-json "$REPORT_JSON" --out "$PINS_JSON" 2>&1 | sed 's/^/    [pins-snapshot] /'; then
      echo "WARNING: #756 latency_pins_snapshot.py failed — Discord report will omit the pins section (fail-open, gate unaffected)." >&2
      PINS_JSON=""
    fi
    # #761: per-camera MV-clone-vs-main presentation skew (order-alternated screenshots on imag,
    # painter-QR decode, t_send-compensated median). Best-effort + fail-open exactly like the pins
    # snapshot above: any failure (imag unreachable, no decodable QR) omits the MV-skew section from
    # the Discord report and NEVER touches the run's own verdict/exit code.
    MV_SKEW_JSON="/tmp/mv-skew-${RUN_ID}.json"
    echo "    [8/8f-mv] #761: MV-clone-vs-main skew snapshot (imag WS screenshots, report-only)"
    if ! python3 "$HERE/mv_skew_snapshot.py" \
        --host "$IMAG_IP" --password "${OBS_PASSWORD:-}" \
        --out "$MV_SKEW_JSON" 2>&1 | sed 's/^/    [mv-skew] /'; then
      echo "WARNING: #761 mv_skew_snapshot.py failed — Discord report will omit the MV-skew section (fail-open, gate unaffected)." >&2
      MV_SKEW_JSON=""
    fi
    echo "    [8/8f] #711: Discord full-report (fail-open — never affects \$GATE below)"
    e2e_discord_report_send "$REPORT_JSON" "$RUN_ID" "$GATE" "$DURATION" "$PINS_JSON" "$MV_SKEW_JSON"
    echo "    --- [8/8e] cleanup plan (JSON secured at $REPORT_JSON) ---"
    if [ "${KEEP_RECORDINGS:-0}" = "1" ]; then
      echo "    KEEP_RECORDINGS=1 — skipping the recording-cleanup plan (debugging opt-out, #652)."
    else
      echo "    #652: free rig disk by deleting ONLY this run's own strih+stream recordings (the"
      echo "    verdict + partials + pixel-proofs above are the evidence that is KEPT; the source"
      echo "    recordings are re-derivable by re-running). NEVER a directory sweep — the EXACT"
      echo "    paths StopRecord returned for THIS run only:"
      echo "      win-strih Shell:      Remove-Item -Force -LiteralPath '${STRIH_HOST_PATH:-<unknown>}'"
      echo "      win-stream-snv Shell: Remove-Item -Force -LiteralPath '${STREAM_HOST_PATH:-<unknown>}'"
      echo "    Set KEEP_RECORDINGS=1 to skip this (debugging)."
    fi
    # #758 item 3 — the in-run freeze watch's own verdict: a mid-run freeze the decode-based
    # verdict above may not have caught (or caught only as a generic loss, with no NAMED "this
    # camera froze at this timestamp" diagnosis) HARD-fails this run's own exit code too.
    FREEZE_WATCH_REPORT="$(live_freeze_watch_verdict "$FREEZE_WATCH_POISON_FILE")"
    if [ -n "$FREEZE_WATCH_REPORT" ]; then
      echo "ERROR: [freeze-watch] one or more cameras froze during the recording (#758):" >&2
      echo "$FREEZE_WATCH_REPORT" >&2
      GATE=1
    fi
    echo "    #703: merge recording-verdict exit code = $GATE (this IS the zero-loss/A/V verdict)."
    exit "$GATE"
  fi

  echo "    The win-* MCP holder runs 8/8a + 8/8b on strih+stream (imag's 8/8c ALREADY ran above —"
  echo "    #462, plain ssh, no MCP needed), pulls the strih+stream partials (+ their <partial>-pixels"
  echo "    #186 proof dirs) to dev1, then runs the 8/8d merge above on dev1. A recording is NEVER"
  echo "    copied box-to-box nor to dev1 — only the small partial JSONs (+ the painter CSV + the"
  echo "    handful of flagged-frame PNGs) move (#208/#186/#462)."
  echo "    ============================================================================"
  echo "    NOTE: this exit code is NOT the zero-loss verdict. In per-box PLANNER mode (this path;"
  echo "          E2E_EXECUTE_VERDICT=1 runs it for real over ssh/scp instead, #701/#703) the"
  echo "          harness only EMITS the plan for the win-* MCP holder to run. The"
  echo "          PASS/FAIL is the merge recording-verdict EXIT CODE on dev1 + the pulled-back"
  echo "          JSON — read THOSE, not this script's exit 0."
  echo "    ============================================================================"
  echo "    --- [8/8e] AFTER the merge above reports its verdict (JSON secured at $REPORT_JSON) ---"
  if [ "${KEEP_RECORDINGS:-0}" = "1" ]; then
    echo "    KEEP_RECORDINGS=1 — skipping the recording-cleanup plan (debugging opt-out, #652)."
  else
    echo "    #652: free rig disk by deleting ONLY this run's own strih+stream recordings (the"
    echo "    verdict + partials + pixel-proofs above are the evidence that is KEPT; the source"
    echo "    recordings are re-derivable by re-running). NEVER a directory sweep — the EXACT"
    echo "    paths StopRecord returned for THIS run only:"
    echo "      win-strih Shell:      Remove-Item -Force -LiteralPath '${STRIH_HOST_PATH:-<unknown>}'"
    echo "      win-stream-snv Shell: Remove-Item -Force -LiteralPath '${STREAM_HOST_PATH:-<unknown>}'"
    echo "    Set KEEP_RECORDINGS=1 to skip this (debugging)."
  fi
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

# #652: (LEGACY VERDICT_ON_STREAM=0 path only — the per-box default path's own cleanup plan is
# printed above, inside that branch, before its earlier `exit 0`.) The verdict ran synchronously
# right above, so a non-empty $REPORT_JSON here means the decode+merge actually completed
# (secured evidence) regardless of PASS/FAIL — the partials/pixel-proofs/JSON already captured
# everything needed to diagnose a FAIL, so the multi-GB source recordings are redundant either
# way. A missing/empty JSON (verdict CRASHed/STALLed, GATE=124/126) means nothing was secured —
# skip the cleanup plan entirely rather than suggest deleting evidence of an unresolved failure.
if [ -s "$REPORT_JSON" ]; then
  if [ "${KEEP_RECORDINGS:-0}" = "1" ]; then
    echo "KEEP_RECORDINGS=1 — skipping the recording-cleanup plan (debugging opt-out, #652)."
  else
    echo "#652: verdict JSON secured at $REPORT_JSON — free rig disk by deleting ONLY this run's"
    echo "own strih+stream recordings (never a sweep — the EXACT StopRecord paths for THIS run):"
    echo "  win-strih Shell:      Remove-Item -Force -LiteralPath '${STRIH_HOST_PATH:-<unknown>}'"
    echo "  win-stream-snv Shell: Remove-Item -Force -LiteralPath '${STREAM_HOST_PATH:-<unknown>}'"
  fi
fi

# #758 item 3 — the in-run freeze watch's own verdict (same check as the E2E_EXECUTE_VERDICT=1
# branch's own early exit above; this is the LEGACY/plan-print path's equivalent tail).
FREEZE_WATCH_REPORT="$(live_freeze_watch_verdict "$FREEZE_WATCH_POISON_FILE")"
if [ -n "$FREEZE_WATCH_REPORT" ]; then
  echo "ERROR: [freeze-watch] one or more cameras froze during the recording (#758):" >&2
  echo "$FREEZE_WATCH_REPORT" >&2
  GATE=1
fi

exit "$GATE"
