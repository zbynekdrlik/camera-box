#!/usr/bin/env bash
# airuleset:script-ok sourced library (not executed directly) — deliberately does NOT set -e at
# top level, same convention as scripts/camera-set.sh: sourcing must never change the calling
# shell's own options as a side effect. `set -euo pipefail` only applies inside the
# executed-directly self-check guard at the bottom of this file.
#
# Single source of truth for WHICH physical notebook drives the imag-nb role (#832; the imag
# notebook swap umbrella is #791).
#
# Same design as scripts/camera-set.sh's CAMERA_ACTIVE_SET (#827): a `case` FACT lookup that
# resolves EVERY known imag box regardless of which one is active today, plus ONE selector that
# decides which fact every consumer actually reads. Retirement/activation of a box is ONLY
# membership in that ONE selector — a box's own facts (its IP) are NEVER deleted, so swapping
# back is always a one-line change here, never archaeology through a deleted diff or hunting a
# literal across half a dozen scripts.
#
# Two known imag-nb boxes. NOTE the 2026-07-29 IP SWAP (user directive): the imag ROLE keeps its
# historical address .182 permanently, so a box swap never again drifts the address every consumer
# and every operator has memorised. The replacement notebook was moved ONTO .182 and the retired
# incumbent was moved OFF it, rather than repointing the whole rig at a new number:
#   replacement -> 10.77.9.182 (ACTIVE: the box physically wired to the wall's HDMI since
#                                2026-07-27; fully provisioned — OBS 32.1.2 genlock build,
#                                scenes 6/6 + Multiview 6/6, obs-websocket :4455, dantesync PTP
#                                LOCKED, zero failed units — #816/#819-#825. Held .187 from
#                                2026-07-27 until the 2026-07-29 swap.)
#   incumbent   -> 10.77.9.189 (RETIRED reference box, hostname imag-pc; its HDMI is disconnected
#                                (#831) so it drives nothing. Moved off .182 on 2026-07-29 and its
#                                OBS autostart disabled, so it can never again be mistaken for the
#                                live imag. Kept reachable only as a verification reference.)
#
# IMAG_HOST_ACTIVE selects which fact is the one every consumer (scripts/recording-e2e.sh,
# scripts/rig-mode.sh) derives IMAG_IP from. Swapping the rig's imag role back to the incumbent
# (should the replacement have to come out) is changing ONE line below — or overriding the env
# var for a one-off run: `IMAG_HOST_ACTIVE=incumbent ...`. Nothing else needs to change; every
# consumer sourcing this file picks it up automatically on its next run. See
# tests/harness_imag_host.rs's `imag_host_active_env_override_swaps_the_incumbent_back_in` for
# the proof this actually works.
IMAG_HOST_ACTIVE="${IMAG_HOST_ACTIVE:-replacement}"

# This file is meant to be SOURCED, not executed — it defines a function + a default and
# performs no side effects beyond resolving IMAG_IP for the sourcing shell. Direct execution
# prints the resolved state (a quick self-check).
#
# Injection safety (same #39 threat model camera-set.sh documents): IMAG_HOST_ACTIVE is only ever
# read from local env/defaults in this repo (never a remote workflow_dispatch input at the time
# of writing), but the resolver still never eval's / word-splits the value — an unknown value
# simply falls through to the `*)` reject arm and returns nonzero.

# imag_host_resolve <name> -> sets IMAG_HOST_IP, returns 0. Unknown name -> loud stderr + 1
# (never silently guess an address and certify the wrong box). FACT lookup — resolves BOTH boxes
# regardless of IMAG_HOST_ACTIVE, exactly like camera-set.sh's camera_resolve().
imag_host_resolve() {
  local name="${1:-}"
  case "$name" in
    incumbent)   IMAG_HOST_IP=10.77.9.189 ;;
    replacement) IMAG_HOST_IP=10.77.9.182 ;;
    *)
      echo "imag-host: unknown imag host '${name}' (expected one of: incumbent replacement)" >&2
      return 1
      ;;
  esac
  IMAG_HOST_NAME="$name"
  return 0
}

if ! imag_host_resolve "$IMAG_HOST_ACTIVE"; then
  echo "imag-host: IMAG_HOST_ACTIVE='${IMAG_HOST_ACTIVE}' is not a known imag host -- refusing to guess an address" >&2
  return 1 2>/dev/null || exit 1
fi

# IMAG_IP is the ONE derived variable every existing consumer already reads (recording-e2e.sh,
# rig-mode.sh) — still directly env-overridable on its own, for an ad-hoc address that isn't one
# of the two named facts above (e.g. a THIRD box for a one-off test), without touching this file
# or IMAG_HOST_ACTIVE at all. An explicit IMAG_IP always wins over the resolved active host.
IMAG_IP="${IMAG_IP:-$IMAG_HOST_IP}"

# When executed directly (not sourced), print the resolved state — same convention as
# camera-set.sh's own self-check.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  printf 'IMAG_HOST_ACTIVE=%s IMAG_HOST_NAME=%s IMAG_HOST_IP=%s IMAG_IP=%s\n' \
    "$IMAG_HOST_ACTIVE" "$IMAG_HOST_NAME" "$IMAG_HOST_IP" "$IMAG_IP"
fi
