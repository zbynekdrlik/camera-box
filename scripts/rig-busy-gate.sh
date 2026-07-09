#!/usr/bin/env bash
set -euo pipefail

# #406/#312 item5: the automatic pull_request-triggered full-path-e2e CI gate must NEVER reroute
# strih/stream's production OBS program scenes while a REAL broadcast (streaming and/or recording)
# is in progress — that would be a genuine production incident, not just a wasted CI run.
#
# Polls scripts/obs_phase2.py rig-busy-check every RIG_BUSY_GATE_SLEEP_SECS (default 60s) for up to
# RIG_BUSY_GATE_ITERATIONS (default 30) checks — a 30-minute budget by default — and:
#   - as soon as the rig reports free           -> print OUTCOME=RIG_FREE, exit 0 (proceed).
#   - if the rig is unreachable (WS down)       -> ::error RIG UNREACHABLE, OUTCOME=RIG_UNREACHABLE,
#                                                   exit 43 (fail CLOSED — never assume "free").
#   - if still busy after the whole budget      -> ::error RIG BUSY, OUTCOME=RIG_BUSY, exit 42.
#
# These three distinct outcomes/exit codes let a human or agent tell "rig busy, re-run later" apart
# from "runner/rig unreachable, fail-closed" apart from a genuine code regression in the E2E step
# that follows this gate in the same job.
#
# Env overrides (tests + operators):
#   STRIH_HOST / STREAM_HOST         - rig OBS WebSocket hosts (default 10.77.9.202 / .204)
#   OBS_PASSWORD                     - OBS WebSocket password (default "" — matches recording-e2e.sh)
#   OBS_PHASE2_PY                    - path to obs_phase2.py (default: sibling of this script)
#   RIG_BUSY_GATE_ITERATIONS         - max poll count (default 30)
#   RIG_BUSY_GATE_SLEEP_SECS         - seconds between polls (default 60)
#   GITHUB_STEP_SUMMARY              - if set (GitHub Actions), OUTCOME=... is appended there too

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_PASSWORD="${OBS_PASSWORD:-}"
OBS_PHASE2_PY="${OBS_PHASE2_PY:-$HERE/obs_phase2.py}"
MAX_ITERATIONS="${RIG_BUSY_GATE_ITERATIONS:-30}"
SLEEP_SECS="${RIG_BUSY_GATE_SLEEP_SECS:-60}"

# Always echo the outcome to stdout (so it shows in the plain job log, and is asserted by the
# harness tests) AND append it to GITHUB_STEP_SUMMARY when running under GitHub Actions.
report_outcome() {
  echo "$1"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
}

LAST_OUTPUT=""
for ((i = 1; i <= MAX_ITERATIONS; i++)); do
  echo "[rig-busy-gate] check ${i}/${MAX_ITERATIONS} — querying strih ($STRIH_HOST) + stream ($STREAM_HOST) OBS WebSocket..."
  set +e
  OUTPUT=$(python3 "$OBS_PHASE2_PY" rig-busy-check \
    --strih-host "$STRIH_HOST" --stream-host "$STREAM_HOST" \
    --password "$OBS_PASSWORD" 2>&1)
  RC=$?
  set -e
  echo "[rig-busy-gate] $OUTPUT"
  LAST_OUTPUT="$OUTPUT"

  if [ "$RC" -eq 3 ]; then
    echo "::error title=RIG UNREACHABLE - gate not run::obs_phase2.py rig-busy-check could not reach strih and/or stream OBS WebSocket: $OUTPUT"
    report_outcome "OUTCOME=RIG_UNREACHABLE"
    exit 43
  fi
  if [ "$RC" -ne 0 ]; then
    echo "::error title=RIG UNREACHABLE - gate not run::obs_phase2.py rig-busy-check exited unexpectedly ($RC): $OUTPUT"
    report_outcome "OUTCOME=RIG_UNREACHABLE"
    exit 43
  fi

  BUSY=$(printf '%s' "$OUTPUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["busy"])')
  if [ "$BUSY" = "False" ]; then
    echo "[rig-busy-gate] rig is free — proceeding."
    report_outcome "OUTCOME=RIG_FREE"
    exit 0
  fi

  echo "[rig-busy-gate] rig busy — will retry."
  if [ "$i" -lt "$MAX_ITERATIONS" ]; then
    sleep "$SLEEP_SECS"
  fi
done

echo "::error title=RIG BUSY - real broadcast detected, gate not run::rig still busy after ${MAX_ITERATIONS} checks (~$((MAX_ITERATIONS * SLEEP_SECS / 60)) min budget): $LAST_OUTPUT"
report_outcome "OUTCOME=RIG_BUSY"
exit 42
