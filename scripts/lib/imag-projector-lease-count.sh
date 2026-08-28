#!/bin/bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (imag-projector-heal.sh, imag-scene-route.sh)
# of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the
# CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
#
# issue 1152 M4 lease-tolerance slice — the #756 projector-count preflight (scripts/recording-e2e.sh)
# must expect the DRM-LEASE projector shape (1 Multiview + 0 Program) instead of the dormant shape
# (1 Multiview + 1 Program) once ~/.camera-box/drm-output.json arms the in-OBS DRM-lease HDMI
# output (.claude/rules/obs-drm-output.md). In lease mode the Program is drawn by the vendored OBS
# DRM output directly onto the leased CRTC — never an X window — so counting exactly 1 X "Projector
# - Program" window is structurally the wrong expectation in that mode, not a bug in the counting
# mechanism itself.
#
# PURE decision only — no ssh/python I/O here — so it is Tier-0-testable (source + call) with zero
# cargo compile (#557). The caller (recording-e2e.sh) is responsible for gathering LEASE_CONNECTOR
# (via imag_scenes.drm_output_lease_connector — the ONE decision grammar every other lease-aware
# caller already consults, obs_phase2.py::_drm_lease_connector_for_host / imag-obs-start.sh) and the
# live wmctrl MV_COUNT/PGM_COUNT, then branches on this function's verdict.

# imag_projector_lease_count_verdict LEASE_CONNECTOR MV_COUNT PGM_COUNT
# Echoes exactly one of: ok-dormant | ok-lease | fail-dormant | fail-lease
#   LEASE_CONNECTOR: the non-empty connector name (drm-output lease ENABLED) or "" (dormant).
#   MV_COUNT / PGM_COUNT: the wmctrl-counted window totals. An empty/non-numeric read (an ssh
#     hiccup, a missing tool) is treated as "not the expected count" — never a silent pass.
# Dormant expects exactly 1 Multiview + 1 Program (the pre-#1152 #756 contract, byte-identical).
# Lease-enabled expects exactly 1 Multiview + 0 Program — a Program window STILL present in lease
# mode is a genuinely inconsistent state (the connector never actually left the X layout, or a
# stray reappeared) and must fail loud, never be silently tolerated as "extra is fine".
imag_projector_lease_count_verdict() {
  local connector="${1:-}" mv="${2:-}" pgm="${3:-}"
  case "$mv" in '' | *[!0-9]*) mv=-1 ;; esac
  case "$pgm" in '' | *[!0-9]*) pgm=-1 ;; esac
  if [ -n "$connector" ]; then
    if [ "$mv" -eq 1 ] && [ "$pgm" -eq 0 ]; then
      printf 'ok-lease\n'
    else
      printf 'fail-lease\n'
    fi
  else
    if [ "$mv" -eq 1 ] && [ "$pgm" -eq 1 ]; then
      printf 'ok-dormant\n'
    else
      printf 'fail-dormant\n'
    fi
  fi
}
