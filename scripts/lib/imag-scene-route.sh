#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (rig-test-dropin.sh, camera-box-restart-
# verify.sh, ...) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so imposing strict mode here would leak into whichever
# caller sources it. recording-e2e.sh (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/imag-scene-route.sh -- SINGLE SOURCE OF TRUTH for mapping a resolved SOURCE
# camera name (cam1/cam3/cam4/cam5/cam6 -- camera-set.sh's camera_resolve()/camera_strih_route(),
# also cam2 for completeness even though it is never routed as a SOURCE camera) to imag-nb's OWN
# OBS scene name for that same physical camera.
#
# WHY (#682): recording-e2e.sh never set imag's program scene -- whatever a PRIOR session left
# on program silently decided which camera imag's leg measured. Live incident (2026-07-11,
# RUN_ID 1573931971, CAM=cam1): imag's program was still 'Cam 4' (leftover from an earlier
# #674 restart experiment) while the run certified cam1 -- the imag leg FAILed (the 911003 burn
# configured on 'NDI CAM1' was absent, fail-closed #585) even though cam1->strih->stream was
# ZERO loss end-to-end.
#
# imag's mapping is a DIRECT 1:1 (unlike strih's OFFSET mapping in camera_strih_route() --
# camera-set.sh's own doc explains why strih is offset and imag is not): imag-nb's Phase 1 setup
# (setup-imag.sh, #458) pins 'NDI CAM1'..'NDI CAM6' -> 'CAMx (usb)' 1:1, the same #399-style
# mapping recording-e2e.sh's own IMAG_PROG_SOURCE="NDI CAM${N}" constant already assumes, and
# rig-mode.sh's set_imag_test_program() (which hardcodes IMAG_PROG_SCENE="Cam 1" -- ONLY correct
# for the CAM=cam1 default, never for a dedicated cam3/cam4/cam5/cam6 cert) already relies on for
# its own single, unconditional cam1 route. Scene names verified LIVE on imag-nb
# (10.77.9.182:4455), 2026-07-11: 'Cam 1' .. 'Cam 6' all exist.
#
# Source-only: defines a PURE function, runs nothing on its own (no python, no OBS call, no I/O)
# -- safe to source from recording-e2e.sh and from unit tests.

# imag_scene_for_camera NAME -> imag-nb's own OBS scene name for that physical camera
# ("cam1" -> "Cam 1", "cam3" -> "Cam 3", ...). Prints "" and returns 1 on an unrecognised name --
# NEVER silently guesses a scene name for a camera outside the known cam1-cam6 fleet (#451).
# Literal `case` match (mirrors camera_resolve()/camera_strih_route()'s own injection-safe style,
# camera-set.sh's #39 threat-model note) -- an unknown/hostile name runs no command, it just
# falls through to the reject arm.
imag_scene_for_camera() {
  local name="${1:-}" n
  case "$name" in
    cam1|cam2|cam3|cam4|cam5|cam6)
      n="${name#cam}"
      printf 'Cam %s' "$n"
      return 0
      ;;
    *)
      printf ''
      return 1
      ;;
  esac
}

# imag_source_for_camera NAME -> imag-nb's own NDI INPUT name for that physical camera
# ("cam1" -> "NDI CAM1", "cam3" -> "NDI CAM3", ...) -- the burn-target / program-feeding input,
# the sibling of imag_scene_for_camera above (which gives the SCENE). Prints "" and returns 1 on
# an unrecognised name (#1135): NEVER guesses an input for a camera outside the known cam1-cam6
# fleet, and stays in lock-step with imag_scene_for_camera's SAME case set so a resolved source box
# either resolves BOTH (scene + input) or fails loud on BOTH -- never a half-resolved imag route.
# The mapping is the SAME 1:1 pin imag_scenes.py seeds (f"NDI CAM{n}" -> "CAMx (usb)", #458) and
# rig-mode.sh's IMAG_PROG_SOURCE default already assumed as the literal "NDI CAM1" (cam1-only);
# #1135 derives it per resolved source box instead. Injection-safe literal `case` (same #39
# threat-model style as imag_scene_for_camera / camera_strih_route).
imag_source_for_camera() {
  local name="${1:-}" n
  case "$name" in
    cam1|cam2|cam3|cam4|cam5|cam6)
      n="${name#cam}"
      printf 'NDI CAM%s' "$n"
      return 0
      ;;
    *)
      printf ''
      return 1
      ;;
  esac
}
