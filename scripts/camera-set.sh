#!/usr/bin/env bash
# Single source of truth for the camera-box FLEET (#24; grew to cam1-7 by #451/#753).
#
# The frame-loss orchestrators (scripts/loopback-e2e.sh, scripts/recording-e2e.sh) source
# this and resolve a camera NAME to its device IP and NDI source name, instead of baking
# cam2 in. The map is authoritative per CLAUDE.md / targets.md:
#
#   cam1 -> 10.77.9.61 / "CAM1 (usb)"
#   cam2 -> 10.77.9.62 / "CAM2 (usb)"   (the off-air development rig; the default everywhere)
#   cam3 -> 10.77.9.63 / "CAM3 (usb)"
#   cam4 -> 10.77.9.64 / "CAM4 (usb)"
#   cam5 -> 10.77.9.65 / "CAM5 (usb)"   (#451 — fleet growing 4->6)
#   cam6 -> 10.77.9.66 / "CAM6 (usb)"
#   cam7 -> 10.77.9.67 / "CAM7 (usb)"   (#753 — real box built + provisioned 2026-07-14,
#                                        fleet growing 6->7, Elgato 4K S grabber)
#
# #827 (2026-07-27) — cam5/cam6/cam7 were RETIRED from the ACTIVE fleet: their USB grabber cards
# were returned to their owner and those boxes were powered off. Per BINDING owner directive
# (2026-07-27, posted on #827): the retirement had to be trivially REVERSIBLE — "dufam ze to
# odobratie cam5 az cam7 urobis tak aby ked zasa budu k dispozicii si to vedel znova lahko
# povolit" (when those boxes come back, re-enabling them must be a one-line change, never
# archaeology through a deleted diff). So EVERY per-camera fact for cam5/cam6/cam7 (IP, NDI
# source name, genlock fps, strih scene/NDI-input route) stayed intact below, fully resolvable —
# retirement was expressed ONLY as membership in `CAMERA_ACTIVE_SET`, never as a deleted case arm.
# This history is HISTORICAL now — see issue 1216 below, which is exactly that promised reversal.
#
# #898 (2026-07-31) — cam3 ALSO RETIRED from the ACTIVE fleet, same mechanism: its USB grabber
# card was physically DESTROYED (a 12V USB-C brick put 12V on VBUS during the #728/#688 power
# test, destroying cam1's original card + a powered hub; cam3's card was then moved into cam1 as
# recovery, per #898). cam3 has zero capture hardware today and no replacement exists yet. Its
# facts (IP, NDI source, genlock fps, strih scene/NDI-input route) stay intact below exactly like
# cam5/cam6/cam7 — retirement is membership-only here too.
#
# **CAMERA_ACTIVE_SET is the ONE declared list of cameras physically installed and active TODAY.**
# Every fleet-wide consumer (recording-e2e.sh's preflight/sweep/burn-target loops,
# set-ndi-mapping.py's enforced pins via its own --active flag, deploy-fleet.sh/verify-fleet.sh/
# upgrade-fleet-ndi.sh's CAMERA_SET fallback, .github/workflows/full-path-e2e.yml indirectly via
# recording-e2e.sh) DERIVES its working set from this ONE line — never a second hardcoded
# enumeration of "which cams exist right now".
#
# **RE-ENABLE PROCEDURE (the whole point of this design):** a retired camera (e.g. cam3, once a
# replacement grabber card is fitted, or cam4 once its capture-leg wedge is resolved) coming back
# online is re-activated by adding its name back to CAMERA_ACTIVE_SET below (or overriding the
# env var for a one-off run:
# `CAMERA_ACTIVE_SET="cam1 cam2 cam3 cam4" ...`). Nothing else needs to change —
# camera_resolve/camera_strih_route already know it fully, and every consumer that derives from
# CAMERA_ACTIVE_SET (or camera_active_secondary_set below) picks it up automatically on its next
# run. See tests/harness_camera_set.rs's `camera_active_set_env_override_reactivates_a_retired_camera`
# / `camera_set_cam3_reactivated_939_resolves_and_is_active_by_default` for the proof this
# actually works.
#
# cam4 RETIRED 2026-08-02 (issue 947): its NZXT Signal HD60 grabber wedges the capture leg within
# 3-9 minutes of every start — three reproductions in 75 minutes, each preceded by a uvcvideo -71
# burst, once with a USB re-enumeration. camera-box keeps the node open and its NDI ports bound
# while emitting nothing, so the box looks alive and the E2E gate aborts at [1/8] FROZEN. The
# hardware call (reseat/replace the grabber, or power-cycle the box) is an OPEN question on issue
# 947 and needs someone at the rig; until then cam4 is membership-retired here exactly like
# cam3/cam5/cam6/cam7 so the fleet runs on cam1+cam2 and A/V work is not blocked by it.
# Re-activation is the one-line add-back below (cam1 holds the 3ms phase-sync floor either way, so
# no re-anchor is needed to REMOVE cam4 — but re-ADDING it must run the calibrator, see
# .claude/rules/phase-sync-calibrator-testing.md).
#
# cam3 RE-ACTIVATED 2026-08-13 (issue 939, user order 2026-08-11): the destroyed Genki grabber was
# replaced with an Elgato Cam Link 4K — 60.0fps captured/emitted, 0 corrupted, colour chroma
# verified live; strih's `NDI cam3` is bound 1:1 to `CAM3 (usb)` and the `Cam 3` scene item is
# enabled. Its per-source phase-sync hold sits at the 3 ms floor; recalibrate from the first green
# E2E run's own verdict JSON if the measured delivery spread demands it (see
# .claude/rules/phase-sync-calibrator-testing.md).
#
# cam1 RE-RETIRED 2026-08-22 (issue 1110): its ShadowCast USB grabber is HARDWARE-DEFECTIVE beyond
# software compensation — chronic over-rate (61.5 -> 63.1 fps captured, climbing), constant 4
# corrupted/5s, -EPROTO; USB re-auth does NOT cure it, and the emit-side fixes (issue 1145 v3 /
# issue 1167) mitigate but do not eliminate. The leg-health gate (issue 1133) rightly REFUSES every
# E2E run while cam1 is in the set, and cam1 alone oscillates -2..-11 id in [4i/8align] while
# cam2/cam3/cam4 sit within 0-2 id — the SINGLE deterministic blocker of the green E2E series. The
# card swap is delivered to the owner (needs-answer on issue 1110); until the grabber is physically
# swapped, cam1 is retired from the MEASURED fleet so the rest of the stack (emit fixes, pin-mover,
# preempt=full kernel) can be proven on healthy cameras.
#
# HISTORY: cam1 was first retired 2026-08-19 (issue 1134, owner order #1130) and RETURNED the same
# day (issue 1130 read the judder as an emit-gate regression in the deployed binary, not the card).
# That return is now superseded — the grabber IS the fault, confirmed over 2026-08-21's E2E series.
#
# cam1 was ALSO the hard-pinned PRIMARY/source node of the E2E chain (cam2 paints -> SOURCE films
# cam2's monitor -> strih -> stream), so its retirement is the first that had to move the SOURCE
# role, not just drop a secondary. #1134 extended this file's retirement=membership doctrine to the
# source role via camera_source_box() below: the source is now the FIRST strih-routable member of
# CAMERA_ACTIVE_SET, so dropping cam1 from the default set here moves the source to cam3 with no
# other edit. Membership-retired exactly like cam4/cam5/cam6/cam7 — cam1's IP / NDI source / strih
# route stay fully resolvable below. cam1 is ALSO acked in rig-fleet.txt
# (grabber-hw-defect-swap-pending-issue-1110); a box OUTSIDE the active set is never a preflight
# target, so the stale-ack guard never fires on it (the cam4 precedent).
# RE-ENABLE (once the grabber is swapped): add "cam1" back to CAMERA_ACTIVE_SET (and CAMERA_ALIGN_SET
# below) AND delete cam1's rig-fleet.txt ack line — nothing else (the source role follows
# automatically, cam1 being the first strih-routable member of a cam1-first set again).
#
# cam2 CAMERA-UNDER-TEST RETIRED 2026-08-24 (issue 1170, owner order): cam2's ShadowCast grabber
# captures imag-nb's HDMI output, so cam2's leg measures the imag PROJECTION path (issue 781), not a
# splitter camera feed — and that grabber's cure-decay collapsed from ~2h to ~7min (issue 1193
# canary journal), so its capture leg cannot survive a 40-min run. cam2 is dropped from the active
# set so it is no longer a MEASURED camera-under-test: no [2b/8] burn deploy, no capture-leg health
# check, no sweep window, no verdict window, no alignment. Its PAINTER role stays UNCONDITIONAL
# (keyed off PAINTER_IP, NOT this set): cam2 still paints the dual-QR + emits the QPSK marker, the
# source camera still films its monitor, and its DanteSync clock is still gated (the whole run's
# timebase). Its facts (IP / NDI source) stay fully resolvable below exactly like the other retired
# cameras — retirement is membership-only. The hardware swap is tracked on issue 1198.
# RE-ENABLE (once the card is swapped): add "cam2" back to CAMERA_ACTIVE_SET — every camera-under-test
# facet (deploy, leg-health, sweep, verdict, and CAMERA_ALIGN_SET membership via camera_is_active)
# flows back automatically, one line, no other edit (see tests/harness_cam2_camera_under_test_gating_1170.rs).
#
# cam1 + cam2 RESTORED 2026-08-27 (issue 1198, owner ruling — SUPERSEDES both retirement
# diagnoses above): the owner refused the physical card swap outright — verbatim "tie dve karty su
# vpohode nebudem ich menit za ine lebo su uplne funkcne" ("those two cards are fine, I will not
# replace them, they are fully functional") — and a live read-only journal check on ALL FOUR cam
# boxes (2026-08-27 14:59 UTC, production running) confirmed it:
#   cam1  60.0 fps emitted / 61.4 fps captured (4 capture-dropped/5s, 1 corrupted)  colour
#   cam2  60.2 fps emitted / 60.0 fps captured (0 capture-dropped/5s, 1 corrupted)  colour
#   cam3  60.0 fps emitted / 60.0 fps captured (2 capture-dropped/5s, 0 corrupted)  colour
#   cam4  60.0 fps emitted / 60.0 fps captured (0 capture-dropped/5s, 4 corrupted)  colour
# with NO STUCK / self-heal / LATCH marker on any box in the ~400 preceding journal lines. Both
# "hardware-defective" diagnoses above were built from EPISODES (cam1's issue 1110 chronic
# over-rate window, cam2's issue 1193 cure-decay collapse to ~7min), never a permanent card
# state — today's steady health falsifies "systematically dying model, 2 of 2" outright.
#
# The historical episodes (issue 1110 cam1 ingest churn, issue 1193 cam2 over-rate self-heal,
# issue 1200 cam3 latch-halving) stay REAL and are not explained away — their root cause is now
# tracked as OUTSIDE the capture card itself (USB port/hub, cable, power, the HDMI splitter port,
# kernel/uvcvideo, or thermal), never the model of card. That investigation continues on issue
# 1198 from a full green E2E run's own verdict + live journals, not from a further edit here.
#
# cam5 + cam6 + cam7 RESTORED 2026-08-28 (issue 1216, owner request — "pridal som ti cam 5 6 7 uz
# mame vacsi spliter cize ich updatni a zarad ich do developmentu"): a bigger splitter is fitted
# and cam5/cam6/cam7 are physically wired back in, exactly the reversal the #827 retirement
# above always promised. Supervisor-verified live (2026-08-28 ~11:00 CEST) BEFORE this membership
# flip: all three boxes updated dev.362 -> dev.569, dantesync 1.8.20 -> 1.8.52 with
# phase_slew_enabled:true (systemd-timesyncd masked), capture steady 59.9-60.1 fps on all three
# (cam5 grayscale -- a SEPARATE ticket, unrelated to membership), strih OBS carries `NDI
# cam5/cam6/cam7` inputs + `Cam 5/6/7` scenes already (live GetInputList/GetSceneList read), and
# cam7's burn-id integration (911012) was already complete from its original #753 build-out. So
# the ONLY missing piece was exactly this membership line -- nothing else needed to change,
# proving the #827 design held. cam4 alone stays out (issue 947, its own unrelated capture-leg
# wedge) -- see tests/harness_camera_set.rs's
# `camera_active_set_default_is_exactly_cam1_cam2_cam3_cam5_cam6_cam7_1216` and
# tests/harness_qr_align_step_1003.rs's `align_set_extends_to_cam5_cam6_cam7_when_active_1216`
# for the round-trip proof.
#
# cam5 OUT AGAIN 2026-08-28 (issue 1217, same day as its own #1216 restoration) — a DEAD_PORT
# leg, not a card fault: post-deploy, cam5's capture chroma reads a flat static frame
# (`rough=0.1`, healthy baseline 7.1-8.0) while cam6/cam7 on the SAME new splitter read colour
# (`rough=8.7-9.5`/`8.8`) in the same minute — the exact proven-good-sibling DEAD_PORT signature
# from `.claude/rules/splitter-port-health-watchdog.md`. Live E2E run 33163294977's [1/8]
# frozen-camera-gate FAILED on NDI cam5 (identical pixel hash across both 3.5s samples) while
# every other camera changed, so leaving it active blocks the WHOLE fleet's E2E gate on one dead
# leg. The box itself is healthy (60.0 fps captured, card registers clean) and reachable, so a
# rig-fleet.txt ack alone would trip the stale-ack guard (`healthy + acked -> stale`) — the
# correct shape is the cam4 precedent again: membership-only removal from BOTH
# CAMERA_ACTIVE_SET and the CAMERA_ALIGN_SET derivation below (cam6/cam7 stay in both), plus an
# ack line documenting "healthy box, outside the measured set" (rig-fleet.txt). cam5's facts
# (IP, NDI source, genlock fps, strih route) stay fully resolvable below — retirement is
# membership-only, exactly like every camera before it.
# RE-ENABLE (once the splitter cable/port is fixed): verify `capture chroma` on cam5 reads
# `-> colour` (rough >= ~7), then add "cam5" back to CAMERA_ACTIVE_SET (and the
# CAMERA_ALIGN_SET loop below) AND delete cam5's rig-fleet.txt ack line — nothing else.
# cam4 + cam5 RESTORED 2026-08-30 (issue 1216 completion, owner directive verbatim: "kamery od
# 1-7 bezia" -- cameras 1 through 7 are running): the owner physically reseated cables on the
# rig, and a live check (2026-08-30 ~12:30 CEST) confirms BOTH re-entry conditions this umbrella
# ticket was waiting on are now met:
#   cam4: svc active, 60.0 fps captured, capture chroma "u_dev=6.4 v_dev=7.5 rough=2.9 -> colour"
#         -- its own #947 capture-leg wedge (uvcvideo -71 bursts within minutes of every start)
#         is a SEPARATE symptom from the frame content itself; the box now captures a real colour
#         image steadily, so the membership-only exclusion is no longer warranted. If the #947
#         wedge symptom reproduces again, that is tracked as its own fresh episode, not a reason
#         to keep this membership exclusion standing on stale evidence.
#   cam5: svc active, capture chroma "u_dev=5.8 v_dev=7.8 rough=7.8 -> colour" -- clears the
#         ~7 healthy-baseline bar from `.claude/rules/splitter-port-health-watchdog.md` (the
#         DEAD_PORT signature that retired it on issue 1217 was rough=0.1, a flat static frame;
#         7.8 is squarely in the same 7.1-8.0 range cam6/cam7 read healthy at).
# cam2 was ALSO rebooted and confirmed healthy after the same cable reseat (already active, no
# membership change needed for it). Per the RE-ENABLE procedures both cameras' own retirement
# comments above promised: add both names back to CAMERA_ACTIVE_SET, add cam5 back to the
# CAMERA_ALIGN_SET derivation loop below (cam4's align membership was already unconditional --
# see the `_align_out="cam3 cam4"` base below, untouched by this change), and delete both
# `rig-fleet.txt` ack lines (`cam4:on-air-but-outside-measured-set-2026-08-07` and
# `cam5:healthy-box-dead-splitter-leg-2026-08-28`) -- nothing else. This is, for the first time,
# the FULL seven-camera fleet active simultaneously.
CAMERA_ACTIVE_SET="${CAMERA_ACTIVE_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}"

# CAMERA_ALIGN_SET — the on-air strih cameras that the #1003 floor-3 per-run aligner keeps phase-
# aligned. It is a SUPERSET of the MEASURED set: cam4 stays here UNCONDITIONALLY (the explicit
# `_align_out="cam3 cam4"` base below, untouched regardless of cam4's own CAMERA_ACTIVE_SET
# membership — the owner's rework mandate, issue 1003, 2026-08-20; the offline-ack
# "outside-measured-set" covers only E2E measurement, never production alignment), and cam3 is
# always included as the explicit on-air base. cam1's membership DERIVES from CAMERA_ACTIVE_SET
# (issue 1170 introduced the derivation for cam2; issue 1198, 2026-08-27, generalized it to cam1;
# issue 1216, 2026-08-28, then removed cam2 from the derivation OUTRIGHT — see the probe-path
# comment right above the derivation line). issue 1216 (2026-08-28) extended the SAME derivation
# to cam5/cam6/cam7 — a trailing loop, each appended only when it is a word in
# CAMERA_ACTIVE_SET, so the resolved order stays cam1..cam7. issue 1217 (same day) DROPPED cam5
# out of that trailing loop (its leg was a DEAD_PORT at the time, delivering no real content) —
# and cam4+cam5 RESTORED 2026-08-30 (issue 1216 completion) puts cam5 BACK into the trailing loop:
# its leg now reads colour (see the CAMERA_ACTIVE_SET header comment above), so aligning it is
# worthwhile again, exactly the RE-ENABLE procedure cam5's own retirement comment always promised
# ("add cam5 back to CAMERA_ACTIVE_SET AND the CAMERA_ALIGN_SET loop below"). cam1/cam5/cam6/cam7
# ALL derive from CAMERA_ACTIVE_SET now — none is hardcoded true; shrinking the active set drops
# each of them from the align set again, one line, no other edit. With today's default (the full
# seven-camera fleet active) the resolved set is "cam1 cam3 cam4 cam5 cam6 cam7" — every
# alignable on-air camera (cam2 = the projection probe whose view is structurally behind the
# splitter family, deliberately excluded below regardless of its own active-set membership).
# Override to match the on-air reality if the fleet changes: CAMERA_ALIGN_SET="cam1 cam3 cam4".
# The inline case matches are word-exact on the space-padded set (same #39-injection-safe posture
# as camera_is_active — it never evals the value); cam3/cam4 are the explicit always-on-air base.
# cam2 NEVER derives into the align set (issue 1216/1152 rig-model correction, 2026-08-28):
# cam2 is the PROJECTION PROBE -- its grabber captures imag-nb's HDMI output, so its view of the
# painter QR arrives through painter -> cam1 camera -> strih -> imag -> HDMI -> grabber,
# structurally ~8 painter ids (~130 ms) behind the direct splitter family. The floor-3 MUTUAL
# align cannot equalize it by design, and its bimodal decode (twice-rescaled optical image)
# flips the measured spread, failing the stability criterion (run 33166543288 [4i/8align]).
# Its CAMERA_ACTIVE_SET membership (E2E leg, burn 911009, probe role) is untouched.
CAMERA_ALIGN_SET="${CAMERA_ALIGN_SET:-$(_align_out="cam3 cam4"; case " $CAMERA_ACTIVE_SET " in *" cam1 "*) _align_out="cam1 $_align_out" ;; esac; for _align_cam in cam5 cam6 cam7; do case " $CAMERA_ACTIVE_SET " in *" $_align_cam "*) _align_out="$_align_out $_align_cam" ;; esac; done; printf '%s' "$_align_out")}"

# This file is meant to be SOURCED, not executed — it defines functions and a default, and
# performs no side effects on its own. Direct execution prints the resolved default set.
#
# Injection safety (#39 threat model): the camera name flows from a workflow_dispatch input,
# so the resolver MUST NOT eval / word-split / index an array with the raw value. A plain
# `case` match on a literal set never executes the value — an unknown/hostile name simply
# falls through to the `*)` reject arm and returns nonzero.

# CAMERA_SET = the ordered list a "drive the whole set" loop iterates over (deploy-fleet.sh,
# verify-fleet.sh, upgrade-fleet-ndi.sh). Override to run a subset, e.g. `CAMERA_SET="cam1 cam3"`,
# or to include a retired-but-still-defined camera for a one-off manual op, e.g.
# `CAMERA_SET="$CAMERA_ACTIVE_SET cam5"`. Defaults to the ACTIVE set (#827) — never a second,
# independently-maintained camera list.
CAMERA_SET="${CAMERA_SET:-$CAMERA_ACTIVE_SET}"

# GENLOCK_FPS = the genlock/broadcast emit rate the harness starts the manual camera-box
# sender at, so it wall-paces EXACTLY like the deployed camera-box service (#66). The deployed
# cam1 gets this from the systemd drop-in
# `/etc/systemd/system/camera-box.service.d/genlock.conf` = `CAMERA_BOX_GENLOCK_FPS=60` — cam
# boxes are UNAFFECTED by the strih topology move (#459, EPIC #466): cam1 still emits 60fps NDI.
# Topology v2 (#459, was #11 mixed 60/30): strih is now cut-to-stream only at 30fps and
# DECIMATES that 60fps camera feed to its own 30fps canvas on ingest (the 60fps LED-wall IMAG
# role moved to the separate imag-nb box, #458/#463); strih→stream is now a plain 30→30
# pass-through. The harness must mirror the 60 emit rate or the manually-launched sender
# free-runs / paces at the wrong rate and the downstream genlock FIFO in OBS (one frame
# per render tick) drops frames or renders black. Single source of truth, env-overridable (set
# GENLOCK_FPS to match the live drop-in if the emit rate ever changes). Default 60 = the pinned
# camera emit rate matching the deployed genlock.conf drop-in — deploy-fleet.sh does NOT write
# that drop-in (it only ships the binary via scp + systemctl restart), so a default of 30 here
# would only mismatch the HARNESS's own manually-launched sender against the rate actually
# deployed on the box, not "shadow back" any config deploy-fleet.sh itself controls.
GENLOCK_FPS="${GENLOCK_FPS:-60}"

# camera_resolve <name>
# On success: sets CAMERA_NAME / CAMERA_IP / CAMERA_SOURCE / CAMERA_GENLOCK_FPS and returns 0.
# On an unknown/empty name: prints an error to stderr and returns 1 (fail loudly — never silently
# fall back to cam2 and certify the wrong box).
#
# This resolves EVERY camera the fleet has ever wired (cam1-cam7), REGARDLESS of
# CAMERA_ACTIVE_SET — resolution is a FACT lookup, not a policy decision. Whether a resolved
# camera is currently part of the active fleet is `camera_is_active`'s job (below), consumed by
# callers that care (recording-e2e.sh's SOURCE-camera selection, the ALL_CAMBOX secondary sweep).
# This split is deliberate (#827, binding owner directive): retiring cam5/cam6/cam7 from the
# active fleet must never require deleting their facts here.
#
# CAMERA_GENLOCK_FPS (#451) is the AUTHORITATIVE per-camera genlock emit rate table — distinct
# from the global harness-only GENLOCK_FPS above. Every camera in the program-feeding fleet
# emits at 60fps today; this per-name table is the single place a future per-camera divergence
# would be recorded, and is what #450's provisioning drop-in generation is meant to read.
#
# #528 design pivot (2026-07-08): this table used to ALSO carry a per-camera HDMI
# cameraman-preview NDI source (CAMERA_DISPLAY_SOURCE / CAMERA_DISPLAY_EXECSTART_SOURCE, #556/
# #562) that setup-device.sh wired into either config.toml's [display] section or a baked
# ExecStart --display flag. The owner rejected that whole per-box-config approach: camboxes have
# no keyboard/mouse, and the preview monitor gets physically MOVED between cameras during an
# event, so a static per-box table can never track it. The HDMI cameraman preview is now
# UNCONDITIONAL and fleet-wide, baked directly into the binary's default
# (`DEFAULT_DISPLAY_SOURCE` in src/main.rs) — every cambox previews the same source with zero
# provisioning, and the existing ~1s DRM-connector poll (src/ndi_display.rs) handles plug/unplug/
# move for free. Nothing about the preview source lives in this table any more.
camera_resolve() {
  local name="${1:-}"
  case "$name" in
    cam1) CAMERA_IP=10.77.9.61; CAMERA_SOURCE="CAM1 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam2) CAMERA_IP=10.77.9.62; CAMERA_SOURCE="CAM2 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam3) CAMERA_IP=10.77.9.63; CAMERA_SOURCE="CAM3 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam4) CAMERA_IP=10.77.9.64; CAMERA_SOURCE="CAM4 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    # #827 (2026-07-27): cam5/cam6/cam7 are RETIRED from CAMERA_ACTIVE_SET (grabber cards
    # returned, boxes powered off) but their FACTS stay fully resolvable here — see this file's
    # header note. Re-enabling one of them is adding its name back to CAMERA_ACTIVE_SET, never
    # restoring a deleted case arm.
    cam5) CAMERA_IP=10.77.9.65; CAMERA_SOURCE="CAM5 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam6) CAMERA_IP=10.77.9.66; CAMERA_SOURCE="CAM6 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam7) CAMERA_IP=10.77.9.67; CAMERA_SOURCE="CAM7 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    *)
      echo "camera-set: unknown camera '${name}' (expected one of: cam1 cam2 cam3 cam4 cam5 cam6 cam7)" >&2
      return 1
      ;;
  esac
  CAMERA_NAME="$name"
  return 0
}

# camera_is_active <name> -> returns 0 iff NAME is a word in CAMERA_ACTIVE_SET, 1 otherwise.
# This is the ONLY place "is this camera currently part of the live fleet?" is decided — callers
# NEVER re-derive it by checking CAMERA_ACTIVE_SET's text themselves (a substring check would
# wrongly match e.g. "cam1" inside a hypothetical "cam10"). Word-exact match via a `case` over a
# space-padded haystack — same injection-safety posture as camera_resolve (#39): NAME is never
# eval'd or word-split as a command.
camera_is_active() {
  local name="${1:-}"
  case " ${CAMERA_ACTIVE_SET} " in
    *" ${name} "*) return 0 ;;
    *) return 1 ;;
  esac
}

# camera_source_box -> prints (stdout) the box currently filling the SOURCE-camera role (the
# "cam1 role": films cam2's monitor, carries the #174 render-time capture burn, is routed onto the
# strih PROGRAM as a camera-under-test). #1134: this is the PRIMARY analogue of the
# retirement=membership doctrine — the source is DERIVED, never hard-pinned to a name:
#   * CAMERA_SOURCE_BOX (env) wins outright if set — the operator's explicit one-off override,
#     same trust model as recording-e2e.sh's CAM= (a member of CAMERA_ACTIVE_SET is expected). An
#     override that is NOT a strih-routable camera is printed as-is here (not validated in this
#     function), and fails loud downstream at recording-e2e.sh's camera_resolve/camera_strih_route
#     under set -e — never a silent wrong-box certification.
#   * otherwise: the FIRST member of CAMERA_ACTIVE_SET that camera_strih_route accepts. cam2 (the
#     fixed painter) is NOT strih-routable and is skipped automatically, so a default set of
#     "cam2 cam3" resolves to cam3, while any legacy cam1-first set still resolves to cam1
#     (byte-identical to the pre-#1134 hard-pin — full back-compat).
# Fails loudly (nonzero, stderr) when NO member is strih-routable (e.g. a painter-only set) —
# never silently certify a chain with no source. The camera_strih_route probe runs in a SUBSHELL
# so it cannot leak CAMERA_STRIH_SCENE/CAMERA_STRIH_SOURCE into the caller (the caller re-resolves
# those itself via its own camera_strih_route call). Reuses camera_strih_route as the single
# authority on source-eligibility — never a second cam list (the 1:1 mapping is decided once).
camera_source_box() {
  if [ -n "${CAMERA_SOURCE_BOX:-}" ]; then
    printf '%s' "$CAMERA_SOURCE_BOX"
    return 0
  fi
  local cam
  for cam in $CAMERA_ACTIVE_SET; do
    if ( camera_strih_route "$cam" ) >/dev/null 2>&1; then
      printf '%s' "$cam"
      return 0
    fi
  done
  echo "camera-set: no strih-routable SOURCE camera in CAMERA_ACTIVE_SET='${CAMERA_ACTIVE_SET}' (set CAMERA_SOURCE_BOX, or add a source-eligible camera to the active set)" >&2
  return 1
}

# camera_active_secondary_set -> prints (space-separated, stdout) CAMERA_ACTIVE_SET with cam1 and
# cam2 removed — the "other camera-under-test boxes" the ALL_CAMBOX sweep cuts into strih program
# alongside the resolved SOURCE camera (the "cam1 role", #1134: DERIVED via camera_source_box, no
# longer the literal cam1) and the fixed painter (cam2). This is the ONE
# place recording-e2e.sh's preflight/deploy/restore/transport-sampler loops derive their
# secondary-camera membership from — adding/removing a camera from CAMERA_ACTIVE_SET flows
# through here automatically, never a second independently-maintained list.
camera_active_secondary_set() {
  local cam src out=""
  # #1134: exclude whichever box camera_source_box resolves as the SOURCE (the "cam1 role"), not
  # the literal cam1 — so a cam1-first legacy set excludes cam1 exactly as before, while the
  # cam2 cam3 default (source=cam3) correctly excludes cam3 and yields an empty secondary set.
  src="$(camera_source_box 2>/dev/null)" || src=""
  for cam in $CAMERA_ACTIVE_SET; do
    [ "$cam" = cam2 ] && continue
    [ "$cam" = "$src" ] && continue
    out="${out:+$out }$cam"
  done
  printf '%s' "$out"
}

# camera_active_sweep_pairs -> prints (space-separated, stdout) "Cam N:CAMN" pairs for EVERY
# camera in CAMERA_ACTIVE_SET, in its own order — the canonical scene:label format
# scripts/recording-e2e.sh's CAMBOX_SWEEP default derives from (#827). Mirrors the #753 1:1
# mapping (scene "Cam N" shows camera camN) exactly — never re-derive that mapping separately.
camera_active_sweep_pairs() {
  local cam n out=""
  for cam in $CAMERA_ACTIVE_SET; do
    n="${cam#cam}"
    out="${out:+$out }Cam ${n}:CAM${n}"
  done
  printf '%s' "$out"
}

# camera_active_ndi_sources_csv -> prints (stdout) "NDI cam1,NDI cam2,..." -- a comma-joined
# strih NDI-input list, one per camera in CAMERA_ACTIVE_SET. Used as the frozen-camera-gate's
# bash-level fallback default (scripts/recording-e2e.sh) when its own python-derived list comes
# back empty -- so that fallback also derives from CAMERA_ACTIVE_SET, never a second literal.
camera_active_ndi_sources_csv() {
  local cam out=""
  for cam in $CAMERA_ACTIVE_SET; do
    out="${out:+$out,}NDI $cam"
  done
  printf '%s' "$out"
}

# camera_align_ndi_sources_csv -> prints (stdout) "NDI cam1,NDI cam2,NDI cam3,NDI cam4" -- the
# comma-joined strih NDI-input list for CAMERA_ALIGN_SET (the on-air alignment superset, incl.
# cam4). Used by the #1003 floor-3 per-run aligner (scripts/qr_align_pins.py --sources) so the
# alignment set derives from CAMERA_ALIGN_SET, never a literal cam range. The 1:1 "NDI cam<N>"
# convention matches camera_strih_route / set-ndi-mapping.py.
camera_align_ndi_sources_csv() {
  local cam out=""
  for cam in $CAMERA_ALIGN_SET; do
    out="${out:+$out,}NDI $cam"
  done
  printf '%s' "$out"
}

# camera_align_ndi_sources_excluding_csv <excluded> -> the align CSV with any word in EXCLUDED
# (space-separated cam names) removed -- the offline-ack sibling of camera_align_ndi_sources_csv,
# mirroring camera_active_ndi_sources_excluding_csv. The floor-3 aligner (#1003 review 🟡) passes
# recording-e2e.sh's PREFLIGHT_EXCLUDED_CAMS here so an acked-OFFLINE on-air camera (a temporarily
# wedged cam4) is NOT required to decode a painter QR -- otherwise one acked box would abort the
# WHOLE run. cam4 is aligned when HEALTHY (it stays in CAMERA_ALIGN_SET); it is dropped only while
# explicitly acked offline, exactly like the freeze-watch's active-set exclusion. Word-exact match.
camera_align_ndi_sources_excluding_csv() {
  local excluded="${1:-}" cam out=""
  for cam in $CAMERA_ALIGN_SET; do
    case " ${excluded} " in
      *" ${cam} "*) continue ;;
    esac
    out="${out:+$out,}NDI $cam"
  done
  printf '%s' "$out"
}

# camera_active_excluding <excluded> -> prints (space-separated, stdout) every camera in
# CAMERA_ACTIVE_SET that is NOT a word in EXCLUDED (space-separated cam names, e.g. the
# recording-e2e.sh `[0/8]` fleet preflight's own $PREFLIGHT_EXCLUDED_CAMS -- boxes TEMPORARILY
# acked-offline via CAMBOX_OFFLINE_ACK, distinct from PERMANENT retirement via CAMERA_ACTIVE_SET
# membership). Word-exact match, same discipline as camera_is_active above.
#
# #827 follow-up (2026-07-28): live hardware gate run 30310110884 proved a retired camera (never
# a member of CAMERA_ACTIVE_SET at all) can still leak into a sampled/checked list when a
# consumer enumerates the fleet via a literal range instead of deriving from CAMERA_ACTIVE_SET --
# this is the ONE place that "active minus acked-offline" derivation happens, so a retired camera
# can never reappear just because a NEW call site forgot to intersect with CAMERA_ACTIVE_SET.
camera_active_excluding() {
  local excluded="${1:-}" cam out=""
  for cam in $CAMERA_ACTIVE_SET; do
    case " ${excluded} " in
      *" ${cam} "*) continue ;;
    esac
    out="${out:+$out }$cam"
  done
  printf '%s' "$out"
}

# camera_active_ndi_sources_excluding_csv <excluded> -> prints (stdout) "NDI cam1,NDI cam2,..."
# for every camera in camera_active_excluding's result -- the CSV-shaped sibling of the function
# above, used by consumers that pass a comma-joined strih-NDI-input list to a Python harness
# (frozen-camera-gate.py's --sources, live-freeze-watch's source arg) rather than iterating cam
# names one at a time.
camera_active_ndi_sources_excluding_csv() {
  local excluded="${1:-}" cam out=""
  for cam in $(camera_active_excluding "$excluded"); do
    out="${out:+$out,}NDI $cam"
  done
  printf '%s' "$out"
}

# camera_strih_route <name>
# On success: sets CAMERA_STRIH_SCENE / CAMERA_STRIH_SOURCE -- the strih OBS scene, and its
# underlying NDI-input name, that shows this physical camera's feed on the certified prod
# program -- and returns 0. On any camera NOT wired as a strih-routed SOURCE camera (an unknown
# name, or cam2 -- see below) prints an error to stderr and returns 1.
#
# Like camera_resolve above, this is a FACT lookup (cam1/cam3/cam4/cam5/cam6/cam7 all resolve,
# REGARDLESS of CAMERA_ACTIVE_SET) — never a policy decision. #827: retiring cam5/cam6/cam7 from
# the active fleet does not remove their strih routes here.
#
# #24 item 1: extracted so scripts/recording-e2e.sh's single-node full-path launch can drive
# cam1, cam3, cam4, cam5, cam6, OR cam7 as the dedicated SOURCE camera (the box filming cam2's
# monitor, carrying the #174 render-time capture burn) instead of being hard-coded to cam1. #312
# (fleet growth 4->6, #451) extends this to cam5/cam6; #753 to cam7.
#
# cam2 is DELIBERATELY NOT a case here, and never should be: recording-e2e.sh's `$CAMERA_NAME`
# default IS "cam2" (back-compat, see below) while `$PAINTER_IP` is ALSO hardcoded to cam2's
# physical IP -- if this function accepted "cam2" as a valid SOURCE route, an un-overridden
# recording-e2e.sh run would try to deploy the SOURCE-camera capture-burn binary AND the painter
# process to the SAME physical box simultaneously (a real /dev/video0 + /dev/fb0 device
# conflict), instead of failing loudly here as designed. #312 DOES make cam2 a measurable
# "camera under test" for the ALL-CAMBOX sweep's digital-burn contiguity check (see
# `recording-verdict.rs`'s `CAMERA_UNDER_TEST_NODES` + its own scene "Cam 2"/"NDI cam2" pin) --
# but that wiring is deliberately kept OUT of this function and lives directly in
# scripts/recording-e2e.sh's `CAMBOX_SWEEP` default + its `[2b/8]` deploy loop (keyed off
# `$PAINTER_IP`, never through a `camera_strih_route "cam2"` call), so the single-SOURCE-camera
# path above can never accidentally select cam2.
#
# The scene/source pins mirror scripts/set-ndi-mapping.py's fixed, Claude-owned genlock mapping
# EXACTLY (never re-derive it separately -- that mapping is the single place it is decided).
#
# #753 PIVOT (2026-07-14, binding user directive): the mapping is now 1:1 -- "chcem aby uz bolo
# ze cam 1 je cam1 ndi source, nie pomenene" (cam N IS the camN NDI source, not relabeled). The
# pre-2026-07-14 offset table (cam1->"Cam 5"/"NDI cam5", cam3->"Cam 1"/"NDI cam1", etc) is HISTORY
# -- see set-ndi-mapping.py's module docstring for the full pre/post record.
#   NDI cam1 -> CAM1 (usb)   =>  cam1 shows on scene "Cam 1" / source "NDI cam1"
#   NDI cam3 -> CAM3 (usb)   =>  cam3 shows on scene "Cam 3" / source "NDI cam3"
#   NDI cam4 -> CAM4 (usb)   =>  cam4 shows on scene "Cam 4" / source "NDI cam4"
#   NDI cam5 -> CAM5 (usb)   =>  cam5 shows on scene "Cam 5" / source "NDI cam5"
#   NDI cam6 -> CAM6 (usb)   =>  cam6 shows on scene "Cam 6" / source "NDI cam6"
#   NDI cam7 -> CAM7 (usb)   =>  cam7 shows on scene "Cam 7" / source "NDI cam7"
# Literal `case` match (#39 injection-safe, same threat model as camera_resolve above) --
# an unknown/hostile name runs no command, it just falls through to the reject arm.
camera_strih_route() {
  local name="${1:-}"
  case "$name" in
    cam1) CAMERA_STRIH_SCENE="Cam 1"; CAMERA_STRIH_SOURCE="NDI cam1" ;;
    cam3) CAMERA_STRIH_SCENE="Cam 3"; CAMERA_STRIH_SOURCE="NDI cam3" ;;
    cam4) CAMERA_STRIH_SCENE="Cam 4"; CAMERA_STRIH_SOURCE="NDI cam4" ;;
    cam5) CAMERA_STRIH_SCENE="Cam 5"; CAMERA_STRIH_SOURCE="NDI cam5" ;;
    cam6) CAMERA_STRIH_SCENE="Cam 6"; CAMERA_STRIH_SOURCE="NDI cam6" ;;
    cam7) CAMERA_STRIH_SCENE="Cam 7"; CAMERA_STRIH_SOURCE="NDI cam7" ;;
    *)
      echo "camera-set: '${name}' is not a strih-routed SOURCE camera (expected one of: cam1 cam3 cam4 cam5 cam6 cam7)" >&2
      return 1
      ;;
  esac
  return 0
}

# The default camera for back-compat: every orchestrator certified cam2 before #24, so the
# unset default stays cam2 and existing CI/behaviour is unchanged.
CAMERA="${CAMERA:-cam2}"

# When executed directly (not sourced), print the resolved default — a quick self-check.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  camera_resolve "$CAMERA"
  printf 'CAMERA=%s IP=%s SOURCE=%q FPS=%s\n' "$CAMERA_NAME" "$CAMERA_IP" "$CAMERA_SOURCE" "$CAMERA_GENLOCK_FPS"
  # #24: also self-check the strih route -- only when the default camera is SOURCE-eligible
  # (cam2's default is NOT -- it is the fixed painter, never routed through strih as a
  # camera-under-test, so camera_strih_route rejects it; that is expected, not an error here).
  if camera_strih_route "$CAMERA" 2>/dev/null; then
    printf 'STRIH_SCENE=%q STRIH_SOURCE=%q\n' "$CAMERA_STRIH_SCENE" "$CAMERA_STRIH_SOURCE"
  fi
  printf 'CAMERA_ACTIVE_SET=%q\n' "$CAMERA_ACTIVE_SET"
  printf 'CAMERA_SOURCE_BOX=%q\n' "$(camera_source_box 2>/dev/null || echo '<none>')"
  printf 'CAMERA_ACTIVE_SECONDARY_SET=%q\n' "$(camera_active_secondary_set)"
fi
