#!/usr/bin/env bash
# Drive the single-node loopback frame-loss E2E across a camera SET (#24).
#
# Runs scripts/loopback-e2e.sh once per camera in CAMERA_SET, resolving each camera's IP +
# NDI source from scripts/camera-set.sh. Aggregates the verdicts and exits non-zero if ANY
# camera fails — so one orchestrator certifies cam1-4 instead of cam2 alone.
#
# Defaults to cam2 ONLY (back-compat: the same single-camera run the project did before #24).
# To certify the live cameras, name them explicitly, e.g.:
#   CAMERA_SET="cam1 cam3 cam4" scripts/loopback-e2e-all.sh
#   CAMERA_SET="cam1 cam2 cam3 cam4" scripts/loopback-e2e-all.sh
#
# IMPORTANT (per the #24 acceptance criteria + approval-scope): cam1/cam3/cam4 are the LIVE
# production cameras. Running the probe on a live camera takes it OFF-AIR briefly and needs
# an explicit maintenance window + operator approval. This script does NOT enforce that —
# the operator who names a live camera in CAMERA_SET is the guard. Per-run env (MODE,
# DURATION_SECS, the gate bounds, etc.) is passed straight through to loopback-e2e.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"

# Default the set to cam2 alone unless the operator widened it (back-compat). Note this
# overrides camera-set.sh's cam1-4 default ON PURPOSE: a bare invocation must not silently
# take the live cameras off-air.
SET="${CAMERA_SET:-cam2}"

echo ">> loopback-e2e across camera set: $SET"

declare -a FAILED=()
for cam in $SET; do
  # Validate the name up front so a typo fails loudly before any device is touched.
  if ! camera_resolve "$cam"; then
    echo "!! skipping invalid camera '$cam'"
    FAILED+=("$cam(invalid)")
    continue
  fi
  echo ""
  echo "================================================================"
  echo ">> [$cam] $CAMERA_IP / '$CAMERA_SOURCE'"
  echo "================================================================"
  # Per-camera artifact so runs don't clobber each other.
  if CAM="$cam" LOCAL_OUT="${LOCAL_OUT:-./frame-probe-${MODE:-coverage}-${cam}.json}" \
      bash "$HERE/loopback-e2e.sh"; then
    echo ">> [$cam] PASS"
  else
    rc=$?
    echo "!! [$cam] FAILED (rc=$rc)"
    FAILED+=("$cam")
  fi
done

echo ""
echo "================================================================"
if [ "${#FAILED[@]}" -eq 0 ]; then
  echo ">> ALL CAMERAS PASSED: $SET"
  exit 0
fi
echo "!! CAMERAS FAILED: ${FAILED[*]}"
exit 1
