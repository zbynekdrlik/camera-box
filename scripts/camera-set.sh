#!/usr/bin/env bash
# Single source of truth for the cam1-7 set (#24; extended to cam5-7 by #451).
#
# The frame-loss orchestrators (scripts/loopback-e2e.sh, scripts/recording-e2e.sh) source
# this and resolve a camera NAME (cam1..cam7) to its device IP and NDI source name, instead
# of baking cam2 in. The map is authoritative per CLAUDE.md / targets.md:
#
#   cam1 -> 10.77.9.61 / "CAM1 (usb)"   (HDMI preview -> "STRIH-SNV (interkom)", #528)
#   cam2 -> 10.77.9.62 / "CAM2 (usb)"   (the off-air development rig; the default everywhere)
#   cam3 -> 10.77.9.63 / "CAM3 (usb)"
#   cam4 -> 10.77.9.64 / "CAM4 (usb)"
#   cam5 -> 10.77.9.65 / "CAM5 (usb)"   (#451 — fleet growing 4->7)
#   cam6 -> 10.77.9.66 / "CAM6 (usb)"
#   cam7 -> 10.77.9.67 / "CAM7 (usb)"
#
# This file is meant to be SOURCED, not executed — it defines functions and a default, and
# performs no side effects on its own. Direct execution prints the resolved default set.
#
# Injection safety (#39 threat model): the camera name flows from a workflow_dispatch input,
# so the resolver MUST NOT eval / word-split / index an array with the raw value. A plain
# `case` match on a literal set never executes the value — an unknown/hostile name simply
# falls through to the `*)` reject arm and returns nonzero.

# CAMERA_SET = the ordered list a "drive the whole set" loop iterates over. Override to run a
# subset, e.g. `CAMERA_SET="cam1 cam3 cam4"`. Defaults to all seven cameras (#451).
CAMERA_SET="${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}"

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
# On success: sets CAMERA_NAME / CAMERA_IP / CAMERA_SOURCE / CAMERA_GENLOCK_FPS /
# CAMERA_DISPLAY_SOURCE and returns 0. On an unknown/empty name: prints an error to stderr and
# returns 1 (fail loudly — never silently fall back to cam2 and certify the wrong box).
#
# CAMERA_GENLOCK_FPS (#451) is the AUTHORITATIVE per-camera genlock emit rate table — distinct
# from the global harness-only GENLOCK_FPS above. Every camera in the program-feeding fleet
# emits at 60fps today; this per-name table is the single place a future per-camera divergence
# would be recorded, and is what #450's provisioning drop-in generation is meant to read.
#
# CAMERA_DISPLAY_SOURCE (#528) is the per-camera HDMI cameraman-preview NDI source table — the
# fleet's single source of truth for "which NDI source does this box's --display render". EMPTY
# (never unset — every case arm assigns it) for a box with no configured preview, so `set -u`
# callers can test it directly instead of tripping on an unbound variable. cam1 had NO preview at
# all (setup-device.sh wrote a bare ExecStart, #528 event finding) -- it gets the interkom/
# return-monitor source here so a re-provision keeps it instead of needing a manual SSH edit.
#
# cam2 is DELIBERATELY left EMPTY here, even though its live box already runs with the same
# interkom preview baked into ExecStart as a manual edit -- scripts/rig-mode.sh's TEST/EVENT mode
# toggle (used by the QR-painter E2E harness) specifically flips cam2's `--display` CLI flag via a
# systemd drop-in override and verifies restoration by grepping ExecStart for `--display`
# (rig-mode.sh:248/353). Camera-box's config.toml `[display]` section (what this table drives) is
# read INDEPENDENTLY of any ExecStart flag (src/main.rs's CLI-overrides-config precedence) -- so
# giving cam2 a table entry would make a FUTURE re-provision (config.toml keeps the [display]
# section regardless of the ExecStart drop-in) silently break rig-mode.sh's fb0-arbitration
# checks: TEST mode's no-display override would stop working (config.toml still supplies the
# source) and EVENT mode's restore-check would false-FAIL forever after (no --display in ExecStart
# to find, even though the preview genuinely still works via config.toml). Until rig-mode.sh is
# taught to recognize BOTH mechanisms, cam2's preview stays a manual, non-provisioner-persistent
# ExecStart edit, same as today -- no regression, just deferred (tracked as a follow-up).
camera_resolve() {
  local name="${1:-}"
  case "$name" in
    cam1) CAMERA_IP=10.77.9.61; CAMERA_SOURCE="CAM1 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="STRIH-SNV (interkom)" ;;
    cam2) CAMERA_IP=10.77.9.62; CAMERA_SOURCE="CAM2 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    cam3) CAMERA_IP=10.77.9.63; CAMERA_SOURCE="CAM3 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    cam4) CAMERA_IP=10.77.9.64; CAMERA_SOURCE="CAM4 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    cam5) CAMERA_IP=10.77.9.65; CAMERA_SOURCE="CAM5 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    cam6) CAMERA_IP=10.77.9.66; CAMERA_SOURCE="CAM6 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    cam7) CAMERA_IP=10.77.9.67; CAMERA_SOURCE="CAM7 (usb)"; CAMERA_GENLOCK_FPS=60; CAMERA_DISPLAY_SOURCE="" ;;
    *)
      echo "camera-set: unknown camera '${name}' (expected one of: cam1 cam2 cam3 cam4 cam5 cam6 cam7)" >&2
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
  printf 'CAMERA=%s IP=%s SOURCE=%q FPS=%s DISPLAY=%q\n' "$CAMERA_NAME" "$CAMERA_IP" "$CAMERA_SOURCE" "$CAMERA_GENLOCK_FPS" "$CAMERA_DISPLAY_SOURCE"
fi
