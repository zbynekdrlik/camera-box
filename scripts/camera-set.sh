#!/usr/bin/env bash
# Single source of truth for the cam1-6 set (#24; extended to cam5-6 by #451).
#
# The frame-loss orchestrators (scripts/loopback-e2e.sh, scripts/recording-e2e.sh) source
# this and resolve a camera NAME (cam1..cam6) to its device IP and NDI source name, instead
# of baking cam2 in. The map is authoritative per CLAUDE.md / targets.md:
#
#   cam1 -> 10.77.9.61 / "CAM1 (usb)"
#   cam2 -> 10.77.9.62 / "CAM2 (usb)"   (the off-air development rig; the default everywhere)
#   cam3 -> 10.77.9.63 / "CAM3 (usb)"
#   cam4 -> 10.77.9.64 / "CAM4 (usb)"
#   cam5 -> 10.77.9.65 / "CAM5 (usb)"   (#451 — fleet growing 4->6)
#   cam6 -> 10.77.9.66 / "CAM6 (usb)"
#
#   cam7 does NOT exist yet (#593) -- the user only expressed FUTURE interest in a 7th camera,
#   no box was ever built/connected. Do NOT add a cam7 case arm / CAMERA_SET entry until a real
#   cam7 box exists; adding it back is a one-line change when it does (uncomment + fill in its
#   real IP below, mirroring the pattern of the six real entries above).
#
# This file is meant to be SOURCED, not executed — it defines functions and a default, and
# performs no side effects on its own. Direct execution prints the resolved default set.
#
# Injection safety (#39 threat model): the camera name flows from a workflow_dispatch input,
# so the resolver MUST NOT eval / word-split / index an array with the raw value. A plain
# `case` match on a literal set never executes the value — an unknown/hostile name simply
# falls through to the `*)` reject arm and returns nonzero.

# CAMERA_SET = the ordered list a "drive the whole set" loop iterates over. Override to run a
# subset, e.g. `CAMERA_SET="cam1 cam3 cam4"`. Defaults to all six real cameras (#451; #593 —
# cam7 was never built and must NOT appear in the default set).
CAMERA_SET="${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6}"

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
    cam5) CAMERA_IP=10.77.9.65; CAMERA_SOURCE="CAM5 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    cam6) CAMERA_IP=10.77.9.66; CAMERA_SOURCE="CAM6 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    # cam7 not yet built (#593) -- uncomment + fill in its real IP/source when a 7th box exists:
    # cam7) CAMERA_IP=10.77.9.67; CAMERA_SOURCE="CAM7 (usb)"; CAMERA_GENLOCK_FPS=60 ;;
    *)
      echo "camera-set: unknown camera '${name}' (expected one of: cam1 cam2 cam3 cam4 cam5 cam6)" >&2
      return 1
      ;;
  esac
  CAMERA_NAME="$name"
  return 0
}

# camera_strih_route <name>
# On success: sets CAMERA_STRIH_SCENE / CAMERA_STRIH_SOURCE -- the strih OBS scene, and its
# underlying NDI-input name, that shows this physical camera's feed on the certified prod
# program -- and returns 0. On any camera NOT wired as a strih-routed "camera under test"
# (an unknown name, cam2 -- the fixed painter/monitor box, or cam5/cam6 -- not yet reserved a
# #174 capture-burn id or a strih scene) prints an error to stderr and returns 1.
#
# #24 item 1: extracted so scripts/recording-e2e.sh's single-node full-path launch can drive
# cam1, cam3, OR cam4 as the dedicated SOURCE camera (the box filming cam2's monitor, carrying
# the #174 render-time capture burn) instead of being hard-coded to cam1. The scene/source
# pins mirror scripts/set-ndi-mapping.py's fixed, Claude-owned genlock mapping EXACTLY (never
# re-derive it separately -- that mapping is the single place it is decided):
#   NDI cam5 -> CAM1 (usb)   =>  cam1 shows on scene "Cam 5" / source "NDI cam5"
#   NDI cam1 -> CAM3 (usb)   =>  cam3 shows on scene "Cam 1" / source "NDI cam1"
#   NDI cam3 -> CAM4 (usb)   =>  cam4 shows on scene "Cam 3" / source "NDI cam3"
# Literal `case` match (#39 injection-safe, same threat model as camera_resolve above) --
# an unknown/hostile name runs no command, it just falls through to the reject arm.
camera_strih_route() {
  local name="${1:-}"
  case "$name" in
    cam1) CAMERA_STRIH_SCENE="Cam 5"; CAMERA_STRIH_SOURCE="NDI cam5" ;;
    cam3) CAMERA_STRIH_SCENE="Cam 1"; CAMERA_STRIH_SOURCE="NDI cam1" ;;
    cam4) CAMERA_STRIH_SCENE="Cam 3"; CAMERA_STRIH_SOURCE="NDI cam3" ;;
    *)
      echo "camera-set: '${name}' is not a strih-routed camera-under-test (expected one of: cam1 cam3 cam4)" >&2
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
fi
