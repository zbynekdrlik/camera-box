#!/usr/bin/env bash
# Fleet-wide drift-guard loop for the camera-box fleet (cam1-7, #552 — remaining #547 work).
#
# scripts/verify-device.sh certifies ONE box at a time (the post-reboot acceptance gate, #454).
# The fleet stays converged only if every box keeps passing it — #547's goal #6 asked for the
# "keeping it converged" piece: a wrapper that runs verify-device.sh across the WHOLE fleet in
# one pass and reports any box that drifted, instead of re-deriving each box's state by hand.
#
# This script is a VERIFY loop, not a deploy loop (contrast scripts/deploy-fleet.sh, which pushes
# a binary) — it composes the ALREADY-TESTED scripts/verify-device.sh per box and rolls the
# per-box PASS/FAIL/SKIPPED verdicts up into one fleet-wide report + exit status.
#
# An OFFLINE box (unreachable over SSH — e.g. cam7 during the 2026-07-06 fleet convergence) is
# reported SKIPPED, never a hard FAIL: an offline box could simply be mid-reboot/deploy, and
# verify-device.sh's own per-CHECK "unreachable = FAIL" posture is right for a box that SHOULD be
# up; at the FLEET level, a box that's plain not there yet is a different signal from a box that
# IS there and failing its acceptance checks.
#
# Usage:
#   scripts/verify-fleet.sh                       # verify cam1-7 (or camera-set.sh's CAMERA_SET)
#   CAMERA_SET="cam1 cam3" scripts/verify-fleet.sh   # verify a subset
#   scripts/verify-fleet.sh --help
#
# Env:
#   SSH_USER     SSH user for the reachability probe (default: root)
#   CAM_PW       box root password (default: newlevel — same fallback as verify-device.sh /
#                deploy-fleet.sh / clock-offset-guard.sh)
#   SSH_TIMEOUT  SSH connect timeout in seconds for the reachability probe (default: 10)
#   VERIFY_CMD   the per-box verify command (default: scripts/verify-device.sh, resolved next to
#                this script) — overridable so tests can point it at a stub instead of driving
#                real SSH sessions against fake boxes
#
# Exit: 0 iff no reachable box FAILed verify-device.sh (an all-SKIPPED or all-PASS fleet exits 0).
# Nonzero iff at least one reachable box FAILed.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve() -- NAME -> IP (#24/#451)

SSH_USER="${SSH_USER:-root}"
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"
SET="${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}"
VERIFY_CMD="${VERIFY_CMD:-$HERE/verify-device.sh}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# =================================================================================================
# PURE function (no network, no SSH -- unit-tested from tests/harness_verify_fleet.rs by sourcing
# this file; the BASH_SOURCE guard below skips the live SSH flow when sourced. Same convention as
# scripts/verify-device.sh / scripts/setup-device.sh.)
# =================================================================================================

# box_status REACHABLE_RC VERIFY_RC -> "PASS" | "FAIL" | "SKIPPED".
# REACHABLE_RC is the exit status of the SSH reachability probe (0 = reachable). An unreachable
# box is SKIPPED regardless of VERIFY_RC (never run verify-device.sh against a box that isn't
# there). A reachable box PASSes iff VERIFY_RC is 0, else FAILs.
box_status() {
  local reachable_rc="$1" verify_rc="$2"
  if [ "$reachable_rc" -ne 0 ]; then
    echo "SKIPPED"
    return 0
  fi
  if [ "$verify_rc" -eq 0 ]; then
    echo "PASS"
  else
    echo "FAIL"
  fi
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the live SSH flow below.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

usage() {
  cat <<EOF
verify-fleet.sh -- fleet-wide drift-guard loop over scripts/verify-device.sh (#552).

Usage:
  scripts/verify-fleet.sh
  scripts/verify-fleet.sh --help

CAMERA_SET (env, default cam1-7, from scripts/camera-set.sh) selects the boxes to check. An
offline box is reported SKIPPED, never a hard FAIL. Exit: 0 iff no reachable box FAILed.
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac

command -v sshpass >/dev/null 2>&1 || { err "sshpass is required (apt-get install sshpass)"; exit 1; }

log "Verifying fleet: $SET"
echo ""

declare -a PASSED=() FAILED=() SKIPPED=()
for cam in $SET; do
  if ! camera_resolve "$cam"; then
    FAILED+=("$cam(invalid)"); continue
  fi
  ip="$CAMERA_IP"
  echo "================================================================"
  echo ">> [$cam] $ip"
  echo "================================================================"

  reachable_rc=0
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    -o BatchMode=no "$SSH_USER@$ip" true >/dev/null 2>&1 || reachable_rc=$?

  verify_rc=0
  if [ "$reachable_rc" -eq 0 ]; then
    "$VERIFY_CMD" "$cam" || verify_rc=$?
  fi

  status="$(box_status "$reachable_rc" "$verify_rc")"
  case "$status" in
    PASS)    log "[$cam] PASS"; PASSED+=("$cam") ;;
    FAIL)    err "[$cam] FAIL"; FAILED+=("$cam") ;;
    SKIPPED) warn "[$cam] OFFLINE/unreachable -- SKIPPED (not treated as a fleet failure)"; SKIPPED+=("$cam") ;;
  esac
  echo ""
done

echo "================================================================"
info "PASS:    ${PASSED[*]:-none}"
info "FAIL:    ${FAILED[*]:-none}"
info "SKIPPED: ${SKIPPED[*]:-none}"

if [ "${#FAILED[@]}" -eq 0 ]; then
  log "FLEET CONVERGED -- no reachable box failed verify-device.sh (#552)"
  exit 0
fi
err "FLEET DRIFT -- ${#FAILED[@]} box(es) failed: ${FAILED[*]}"
exit 1
