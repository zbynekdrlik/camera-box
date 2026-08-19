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

# #1003 finding 2: the LIVE #1035 cam->strih p99 latency bound must rise in profile mode by the
# marker camera's (cam2, the painter) pin delta -- raising cam2's pin +N raises the measured
# cam2-paint->strih-program latency +N, so the fixed 400ms bound would else false-fail by
# construction. arg1 = plan JSON, arg2 = the base bound (recording-verdict's default 400). Falls
# back to the base bound if "NDI cam2" is absent from the plan (loud on stderr).
measurement_eq_cam_strih_bound_ms() {
  printf '%s' "$1" | python3 -c 'import sys,json
p=json.load(sys.stdin); base=float(sys.argv[1]); m="NDI cam2"
t=p["strih_pins"].get(m); q=p["production"]["strih_pins"].get(m)
if t is None or q is None:
    sys.stderr.write("[measurement-eq] WARN: marker camera %r absent from plan; cam-strih bound left at base %g\n"%(m,base)); print(int(round(base))); sys.exit(0)
print(int(round(base + (t - q))))' "$2"
}

# #1003 finding 10: which CAMERA_ACTIVE_SET cameras are NOT covered by the profile (arg1 = plan
# JSON, arg2 = the space-separated active set, e.g. "cam1 cam2 cam3"). Prints the missing cam names
# (space-joined); empty output == fully covered. The #900 re-anchor this replaces had an explicit
# coverage-fail, so a future active-set change can never silently measure an unequalized camera.
measurement_eq_missing_active() {
  printf '%s' "$1" | python3 -c 'import sys,json
p=json.load(sys.stdin); active=sys.argv[1].split()
have=set(p["strih_pins"])
print(" ".join(c for c in active if ("NDI %s"%c) not in have))' "$2"
}
