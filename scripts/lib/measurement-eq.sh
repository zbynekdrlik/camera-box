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

# #1124 item 3: POST-record stomp re-check (report-only). Re-verify the measurement pins/hold are
# STILL in force after StopRecord (they are only restored later, by cleanup()'s teardown), so a
# mid-recording writer that stomped them surfaces as a LOUD diagnostic instead of an opaque
# A/V-gate result. NEVER gates -- a mismatch prints a WARNING and returns 0. Reuses the SAME
# obs_phase2.py verify-measurement-pins command the pre-record [4h/8eq] block uses (its docstring
# already names this deferred post-record re-call). Args: profile strih_host strih_pw stream_host
# stream_pw. The whole re-check is best-effort: an unreachable box / WS hiccup is itself only a
# report-only NOTE (the measurement already happened; this is diagnosis on top).
measurement_eq_post_record_stomp_recheck() {
  local _prof="$1" _sh="$2" _spw="$3" _mh="$4" _mpw="$5" _sd
  _sd="$(_meq_scripts_dir)"
  echo "[7/8 meq] #1124 POST-record stomp re-check (report-only) — are the measurement pins/hold STILL in force after StopRecord? (a mid-run writer stomp would make the A/V-gate result reflect the WRONG config)"
  # A non-zero exit here means verify could NOT confirm the pins in force — which is EITHER a real
  # mid-run stomp OR an unreachable box / WS hiccup (indistinguishable by exit code). So the WARNING
  # names BOTH causes and points at the [meq-stomp …] detail above (never asserts a definite stomp,
  # which could wrongly discredit a valid run). Report-only, gate unaffected.
  python3 "$_sd/obs_phase2.py" verify-measurement-pins --host "$_sh" --password "$_spw" \
    --profile "$_prof" --role strih 2>&1 | sed 's/^/    [meq-stomp strih] /' \
    || echo "WARNING #1124: strih measurement pins could NOT be confirmed in force after StopRecord (a mid-run writer stomp, OR the box was unreachable / a WS hiccup — see the [meq-stomp strih] detail above). If a genuine stomp, this run's A/V-gate result reflects the WRONG config. Report-only, gate unaffected." >&2
  python3 "$_sd/obs_phase2.py" verify-measurement-pins --host "$_mh" --password "$_mpw" \
    --profile "$_prof" --role stream 2>&1 | sed 's/^/    [meq-stomp stream] /' \
    || echo "WARNING #1124: stream hold could NOT be confirmed in force after StopRecord (a mid-run writer stomp, OR the box was unreachable / a WS hiccup — see the [meq-stomp stream] detail above). If a genuine stomp, this run's A/V-gate result reflects the WRONG config. Report-only, gate unaffected." >&2
  # #1133: report-only helper called as a bare statement MUST return 0 on every path, or a caller
  # under `set -euo pipefail` would abort the whole run on a benign non-zero (this one is in the
  # [7/8] `set +e` region today, but return 0 keeps it safe if the call ever moves).
  return 0
}

# #1124 items 1+2: POST-verdict report-only diagnostics off the run's full verdict JSON. Item 1 --
# staleness: feed the verdict's all_cambox_delivery_latency into the pure staleness decision so a
# profile that no longer matches the rig's transports surfaces "profile STALE — re-derive". Item 2
# -- edge-oscillation: ONLY when the run FAILED zero-loss ($gate != 0), classify the uniform
# copies~=gaps FIFO limit-cycle signature so a phase-edge flake reads as the known #757-Corr-2
# class, not a regression. Both NEVER gate. Args: profile verdict_json gate. A missing verdict
# JSON (planner mode never produced one on dev1) is a report-only NOTE, never an error.
measurement_eq_post_verdict_diagnostics() {
  local _prof="$1" _verdict="$2" _gate="${3:-0}" _sd
  _sd="$(_meq_scripts_dir)"
  if [ ! -f "$_verdict" ]; then
    echo "    [8/8 meq] #1124 no verdict JSON on dev1 ($_verdict) — skipping report-only staleness/edge diagnostics (planner mode / merge ran elsewhere)"
    return 0
  fi
  echo "[8/8 meq] #1124 staleness report (report-only) — is the checked-in measurement profile still current vs this run's measured delivery?"
  python3 "$_sd/e2e_measurement_pins.py" staleness-from-verdict --profile "$_prof" --verdict "$_verdict" 2>&1 | sed 's/^/    [meq-staleness] /' || true
  if [ "$_gate" != "0" ]; then
    echo "[8/8 meq] #1124 edge-oscillation classifier (report-only) — a FAILED profile run: is it the known FIFO-edge flake or a genuine regression?"
    python3 "$_sd/e2e_measurement_pins.py" edge-oscillation --verdict "$_verdict" 2>&1 | sed 's/^/    [meq-edge] /' || true
  fi
  # #1133: this helper is called as a bare statement in the [8/8] region, which runs under the
  # caller's re-enabled `set -euo pipefail` — return 0 unconditionally so a benign non-zero (an
  # empty read, a sed/pipefail hiccup) can NEVER set -e-abort the run before its own `exit $GATE`.
  return 0
}
