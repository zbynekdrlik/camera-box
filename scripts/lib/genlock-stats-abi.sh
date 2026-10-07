#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + printers, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`.
#
# scripts/lib/genlock-stats-abi.sh -- issue 1302: the stats-ABI gate on a FAST genlock deploy.
# RED stub: every function exists and does nothing yet (the fast deploy is not gated).

genlock_stats_abi_is_version() { return 1; }
genlock_stats_abi_from_obs_h() { cat >/dev/null; return 1; }
genlock_stats_abi_at_sha() { return 1; }
genlock_fast_abi_verdict() { echo "OK"; return 0; }
genlock_fast_abi_gate_ps() { return 0; }
genlock_stats_abi_marker_ps() { return 0; }
genlock_stats_abi_stage() { return 0; }
genlock_stats_abi_resolve() { return 0; }
