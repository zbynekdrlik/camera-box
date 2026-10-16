#!/usr/bin/env bash
# scripts/lib/strih-transition.sh -- the E2E harness owns the strih program transition (sourced).
set -euo pipefail
#
# issue 1367 (16.10.2026): the operator left strih on "Fade 500 ms" after a production, and every
# E2E cut then mixed the outgoing and incoming camera (blended QR frames, a ~0.73 s hole in the new
# camera's ticks right after the verdict's 1 s guard), so the release gate failed on the transition,
# not on the cameras. The harness therefore records the operator's transition, selects Cut for its
# own program switches, and restores the original in cleanup(). The save is durable
# (~/.camera-box/strih-transition-saved.json): an aborted run's next start keeps restoring the
# operator's real choice, never a stale Cut.
#
# strih_transition_force_cut HERE HOST PASSWORD [SAVE]
#   Fail-loud (exit 1): a measurement on a blending transition is not a measurement.
# strih_transition_restore HERE HOST PASSWORD [SAVE]
#   Best effort for cleanup(): a failure prints a WARNING naming the manual step; always returns 0.

strih_transition_save_path() {
  printf '%s\n' "${STRIH_TRANSITION_SAVE:-$HOME/.camera-box/strih-transition-saved.json}"
}

strih_transition_force_cut() {
  local here="$1" host="$2" pw="$3" save="${4:-$(strih_transition_save_path)}"
  echo "      issue 1367: strih program transition -> Cut for the measurement (operator's choice saved)"
  if ! python3 "$here/obs_phase2.py" transition-cut --host "$host" --password "$pw" --save "$save"; then
    echo "ERROR: could not set the strih program transition to Cut (issue 1367) -- a blending transition mixes two cameras on every cut and the continuity verdict is meaningless; fix strih OBS-WS, then re-run." >&2
    exit 1
  fi
}

strih_transition_restore() {
  local here="$1" host="$2" pw="$3" save="${4:-$(strih_transition_save_path)}"
  if ! timeout "${OBS_CLEANUP_TIMEOUT:-30}" python3 "$here/obs_phase2.py" transition-restore \
      --host "$host" --password "$pw" --save "$save"; then
    echo "WARNING: could not restore the operator's strih program transition (issue 1367); it stays saved in $save -- the next E2E run restores it, or run: python3 scripts/obs_phase2.py transition-restore --host $host" >&2
  fi
  return 0
}
