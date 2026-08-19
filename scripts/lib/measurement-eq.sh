#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the scripts/lib/cbox-burn-log-persist.sh convention (no top-level
# `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/measurement-eq.sh -- #1003 measurement-window per-camera equalization helpers.
#
# Thin wrappers over scripts/e2e_measurement_pins.py (the PURE resolver, Tier-0 unit-tested) so
# recording-e2e.sh's OWN text gains only a source line + a few plain function-call lines (the
# #675/#716 sourced-helper anchor-safety pattern -- the static-anchor tests read recording-e2e.sh's
# text, never a sourced lib, so these bodies are invisible to them). Every function derives the
# scripts dir from its OWN location, so it works whether the caller has $HERE set or not.

_meq_scripts_dir() {
  # scripts/lib/measurement-eq.sh -> scripts/
  (cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
}

# ON iff the opt-in flag is set. Default OFF (cautious #757 precedent): the supervisor enables it
# for the live validation E2E; promotion to default is a follow-up once N clean runs confirm.
measurement_eq_enabled() {
  [ "${MEASUREMENT_EQ:-0}" = "1" ]
}

# Resolve the profile ONCE -> the plan JSON on stdout. Non-zero (with the resolver's own loud
# stderr) when the profile is missing / malformed / INCOHERENT -- the caller must fail the run.
measurement_eq_plan_json() {
  python3 "$(_meq_scripts_dir)/e2e_measurement_pins.py" resolve --profile "$1"
}

# Scalar extractors over the plan JSON (arg 1 = the JSON string). python only (no jq dependency).
measurement_eq_hold_ms() {
  printf '%s' "$1" | python3 -c 'import sys,json; print(int(json.load(sys.stdin)["stream_hold_ms"]))'
}
measurement_eq_prod_hold_ms() {
  printf '%s' "$1" | python3 -c 'import sys,json; print(int(json.load(sys.stdin)["production"]["stream_hold_ms"]))'
}
measurement_eq_av_expected_ms() {
  printf '%s' "$1" | python3 -c 'import sys,json; print(int(round(float(json.load(sys.stdin)["av_expected_ms"]))))'
}

# leftover_slack_ms is a PROFILE field (not in the resolved plan) -- read it from the profile file
# (arg 1 = profile path), defaulting to 40 when absent.
measurement_eq_slack_ms() {
  python3 -c 'import sys,json; print(int(json.load(open(sys.argv[1])).get("leftover_slack_ms",40)))' "$1"
}
