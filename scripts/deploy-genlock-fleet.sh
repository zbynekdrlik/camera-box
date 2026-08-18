#!/usr/bin/env bash
# scripts/deploy-genlock-fleet.sh — (STUB, filled in the GREEN commit) one deploy path for the OBS
# genlock build across strih + stream + imag from ONE CI run id (issue 789 residual, bod 4 + bod 5).
set -euo pipefail
# The real planner/orchestrator (pure builders + source-guard + main) is added in the GREEN commit.

# --- source-guard: when sourced (the unit tests), stop here (no functions yet in the stub) --------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi
