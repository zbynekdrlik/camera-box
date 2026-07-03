#!/usr/bin/env bash
# Single source of truth for the cam1-4 set (#24).
#
# The frame-loss orchestrators (scripts/loopback-e2e.sh, scripts/recording-e2e.sh) source
# this and resolve a camera NAME (cam1..cam4) to its device IP and NDI source name, instead
# of baking cam2 in. The map is authoritative per CLAUDE.md / targets.md:
#
#   cam1 -> 10.77.9.61 / "CAM1 (usb)"
#   cam2 -> 10.77.9.62 / "CAM2 (usb)"   (the off-air development rig; the default everywhere)
#   cam3 -> 10.77.9.63 / "CAM3 (usb)"
#   cam4 -> 10.77.9.64 / "CAM4 (usb)"
#
# This file is meant to be SOURCED, not executed — it defines functions and a default, and
# performs no side effects on its own. Direct execution prints the resolved default set.
#
# Injection safety (#39 threat model): the camera name flows from a workflow_dispatch input,
# so the resolver MUST NOT eval / word-split / index an array with the raw value. A plain
# `case` match on a literal set never executes the value — an unknown/hostile name simply
# falls through to the `*)` reject arm and returns nonzero.

# CAMERA_SET = the ordered list a "drive the whole set" loop iterates over. Override to run a
# subset, e.g. `CAMERA_SET="cam1 cam3 cam4"`. Defaults to the four cameras.
CAMERA_SET="${CAMERA_SET:-cam1 cam2 cam3 cam4}"

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
# camera emit rate (this is what deploy-fleet.sh re-pins cam1's emit to — a default of 30 would
# shadow it back).
GENLOCK_FPS="${GENLOCK_FPS:-60}"

# camera_resolve <name>
# On success: sets CAMERA_NAME / CAMERA_IP / CAMERA_SOURCE and returns 0.
# On an unknown/empty name: prints an error to stderr and returns 1 (fail loudly — never
# silently fall back to cam2 and certify the wrong box).
camera_resolve() {
  local name="${1:-}"
  case "$name" in
    cam1) CAMERA_IP=10.77.9.61; CAMERA_SOURCE="CAM1 (usb)" ;;
    cam2) CAMERA_IP=10.77.9.62; CAMERA_SOURCE="CAM2 (usb)" ;;
    cam3) CAMERA_IP=10.77.9.63; CAMERA_SOURCE="CAM3 (usb)" ;;
    cam4) CAMERA_IP=10.77.9.64; CAMERA_SOURCE="CAM4 (usb)" ;;
    *)
      echo "camera-set: unknown camera '${name}' (expected one of: cam1 cam2 cam3 cam4)" >&2
      return 1
      ;;
  esac
  CAMERA_NAME="$name"
  return 0
}

# The default camera for back-compat: every orchestrator certified cam2 before #24, so the
# unset default stays cam2 and existing CI/behaviour is unchanged.
CAMERA="${CAMERA:-cam2}"

# When executed directly (not sourced), print the resolved default — a quick self-check.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  camera_resolve "$CAMERA"
  printf 'CAMERA=%s IP=%s SOURCE=%q\n' "$CAMERA_NAME" "$CAMERA_IP" "$CAMERA_SOURCE"
fi
