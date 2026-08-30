#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors the
# sibling scripts/lib/ndi-name-selfheal.sh convention which is also `set -euo pipefail`-free for the
# same reason.
#
# scripts/lib/imag-active-cams-state.sh — issue 1218: the imag active-set NDI idle enforce pass.
#
# WHY this exists: imag-nb thermal-throttles because it decodes camera NDI feeds OUTSIDE the active
# set for nothing (an inactive camera's `NDI CAM{n}` receiver runs a full 1080p60 decode). The
# durable fix is the on-box --bootstrap seed reading the provisioned state file, but a dev1 pass
# (the E2E [0/8] preflight, rig-mode TEST entry) should ALSO enforce the policy immediately over WS
# so a ~40-min run never thermal-throttles mid-flight from a stale-but-still-decoding inactive leg
# (the exact render-health-preflight false-fail in the ticket). imag_scenes.py --enforce-ndi-policy
# idles every inactive camera's receiver (ndi_source_name "" + genlock_fifo off) + (re)enforces the
# active ones (discoverability-gated) AND writes a fresh copy of the one-line state file to the box.
#
# Source-only: this file defines a function and performs no side effects on its own. The CALLER
# sources it via the #675 prevention pattern (a new lib, not an edit to an anchored line) so the
# static-anchor test suites reading recording-e2e.sh / rig-mode.sh never see this logic.

# imag_enforce_ndi_active_policy <imag_host> <active_set> <scripts_dir>
#   Apply the active-set idle policy on imag over WS and refresh its state file. Best-effort:
#   ALWAYS returns 0 (an idle-policy failure is never fatal to the run — the box's own boot seed is
#   the durable enforcement; this is the immediate belt-and-suspenders). imag's OBS WebSocket is
#   passwordless (verify-imag.sh drives imag_scenes.py the same way, no --password).
#   Env seam IMAG_ACTIVE_CAMS_ENFORCE_CMD substitutes the invocation for Tier-0 tests (it runs with
#   IMAG_ACTIVE_CAMS_ENFORCE_HOST / _ACTIVE / _SCRIPTS exported so a fake can assert what it was
#   called with), keeping the caller's decision flow testable with zero OBS/network.
imag_enforce_ndi_active_policy() {
  local host="$1" active_set="$2" scripts_dir="$3"
  if [ -n "${IMAG_ACTIVE_CAMS_ENFORCE_CMD:-}" ]; then
    IMAG_ACTIVE_CAMS_ENFORCE_HOST="$host" IMAG_ACTIVE_CAMS_ENFORCE_ACTIVE="$active_set" \
      IMAG_ACTIVE_CAMS_ENFORCE_SCRIPTS="$scripts_dir" bash -c "$IMAG_ACTIVE_CAMS_ENFORCE_CMD" || true
    return 0
  fi
  python3 "$scripts_dir/imag_scenes.py" --host "$host" \
    --enforce-ndi-policy --active-cams "$active_set" 2>&1 || true
  return 0
}
