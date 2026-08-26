#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors the
# sibling scripts/lib/leg-health-guard.sh / capture-rate-guard.sh convention which are also
# `set -euo pipefail`-free for the same reason.
#
# scripts/lib/ndi-name-selfheal.sh — #1158: the [4c/8] frozen-camera-gate self-heal for an NDI input
# whose ndi_source_name was EMPTIED or DRIFTED off its #399 baseline.
#
# WHY this exists (live-confirmed on strih 2026-08-20): the E2E harness reattach (the #1114
# CLEAR-then-SET) can leave a strih 'NDI camN' input's ndi_source_name = "" when the sender vanishes
# from the DistroAV finder mid-settle; a force-kill OBS restart can also reload a drifted saved-scene
# name. An EMPTY name STOPS the DistroAV receiver thread ("No NDI Source selected; Requesting Source
# Thread Stop"), so the in-loop #767/#1096 auto-rebind watchdogs can NEVER revive it — a PERMANENT
# wedge until a name is re-applied (the owner had to open Properties / re-run set-ndi-mapping by
# hand; "nesmie sa to stat"). strih_mv_scenes.reattach() is the producer-side fix (it no longer
# leaves ""); THIS is the harness safety net that recovers an empty/drifted name from ANY cause,
# on a FROZEN verdict, before the run fails.
#
# It delegates to set-ndi-mapping.py --heal (the SINGLE #399 baseline authority + the shared
# obs_phase2.reenforce_ndi_name policy: discoverability-gated + read-back-verified; correct inputs
# untouched; an offline-baseline drifted input is left as-is + logged LOUD). Exit 0 iff --heal
# HEALED >=1 input (the caller re-samples a revived leg); non-zero otherwise (nothing healable /
# offline baseline / verify-fail / error — the caller proceeds with its normal retry+abort, the
# #1158 lines already surfaced the reason).
#
# Source-only: this file defines a function and performs no side effects on its own.

# ndi_name_selfheal_run <strih_host> <active_set> <scripts_dir>
#   Returns 0 iff set-ndi-mapping.py --heal healed >=1 emptied/drifted active input.
#   The OBS_PASSWORD env (already exported by the [4c/8] frozen-gate context) authenticates the WS.
#   Env seam: NDI_NAME_SELFHEAL_CMD substitutes the heal invocation for Tier-0 tests (it runs with
#   NDI_NAME_SELFHEAL_HOST / _ACTIVE / _SCRIPTS exported so a fake can assert what it was called with
#   and simulate any exit code). This keeps the [4c/8] decision flow testable with zero OBS/network.
ndi_name_selfheal_run() {
  local host="$1" active_set="$2" scripts_dir="$3"
  if [ -n "${NDI_NAME_SELFHEAL_CMD:-}" ]; then
    NDI_NAME_SELFHEAL_HOST="$host" NDI_NAME_SELFHEAL_ACTIVE="$active_set" \
      NDI_NAME_SELFHEAL_SCRIPTS="$scripts_dir" bash -c "$NDI_NAME_SELFHEAL_CMD"
    return $?
  fi
  python3 "$scripts_dir/set-ndi-mapping.py" --host "$host" --password "${OBS_PASSWORD:-}" \
    --active "$active_set" --heal
}
