#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (imag-scene-route.sh, rig-test-dropin.sh, ...)
# of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the
# CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
# recording-e2e.sh (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/imag-burn-verify.sh -- issue 1204: fail-closed cross-check that imag's BURN TARGET
# (IMAG_PROG_SOURCE) is the input imag ACTUALLY renders in program after [4a/8] routes the scene.
#
# WHY (run 32908274448 / verdict 518418121): recording-e2e.sh derived imag's PROGRAM SCENE from
# the camera-under-test (imag_scene_for_camera "$CAMERA_NAME" -> 'Cam 3' -> renders 'NDI CAM3')
# but the imag BURN TARGET was hard-pinned to 'NDI CAM1'. With cam1 offline-acked and the active
# set = cam3 the two diverged, the burn landed on a NON-program input, and the imag recording
# carried zero 911003 anchors. The IMAG_PROG_SOURCE derivation is now fixed (imag_source_for_camera
# "$CAMERA_NAME", the SAME resolution the scene uses) -- these functions are the belt-and-braces
# read-back cross-check that PROVES the derived target equals what imag genuinely renders, so any
# FUTURE divergence source (a manual override, a mislabeled scene, the #938 sweep re-targeting)
# fails LOUD before recording rather than silently wasting the run (the strih/stream #901
# "burn what's actually rendered" philosophy applied to imag).
#
# Source-only: defines PURE functions, runs nothing on its own (no python, no OBS call, no I/O) --
# safe to source from recording-e2e.sh and from unit tests. The caller does the live
# `obs_phase2.py program-rendered-input` read and passes the result in.

# imag_burn_target_matches_program RENDERED TARGET -> 0 iff RENDERED == TARGET and NEITHER is empty.
# An empty RENDERED (could not read program-rendered-input: OBS unreachable / no enabled scene
# item) or an empty TARGET is a MISMATCH (fail-closed) -- never a silent "match". Pure string
# comparison, no I/O.
imag_burn_target_matches_program() {
  local rendered="${1:-}" target="${2:-}"
  [ -n "$rendered" ] && [ -n "$target" ] && [ "$rendered" = "$target" ]
}

# imag_burn_mismatch_message RENDERED TARGET SCENE -> the loud one-line diagnostic for a mismatch,
# printed to be echoed to stderr by the caller before it exits 1. Distinguishes the two failure
# shapes: an EMPTY rendered input (could not read what imag renders) vs a genuine wrong-target
# (rendered a DIFFERENT input than the burn target). Single printf, safe to embed in $(...).
imag_burn_mismatch_message() {
  local rendered="${1:-}" target="${2:-}" scene="${3:-}"
  if [ -z "$rendered" ]; then
    printf 'imag burn-target cross-check (issue 1204): could not read imag program-rendered-input for scene %s -- refusing to record (would burn an unverifiable input, leaving the recording with no 911003 anchors). Confirm imag OBS is up and the scene has an enabled renderable item.' "'$scene'"
  else
    printf 'imag burn-target cross-check (issue 1204): burn target %s is NOT the input imag actually renders in program (scene %s renders %s). Burning a non-program input leaves the imag recording with zero 911003 anchors (the run 32908274448 failure). Fix the IMAG_PROG_SOURCE derivation (imag_source_for_camera) or the imag scene route.' "'$target'" "'$scene'" "'$rendered'"
  fi
}
