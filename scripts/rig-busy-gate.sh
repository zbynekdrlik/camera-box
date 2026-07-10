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
# #657: even with the recording-e2e.sh cleanup-trap + interruptible-sleep hardening (belt), this
# gate ALSO self-heals as a suspenders: obs_phase2.py's rig-busy-check already reports a
# "stray_hosts" list — boxes matching EXACTLY "our own stray recording" (recording ON, streaming
# OFF; a real broadcast always streams+records together, so this signature can never be a real
# broadcast). After STRAY_HEAL_THRESHOLD CONSECUTIVE polls showing a box in that stray_hosts list,
# this gate StopRecords that box itself (StopRecord only — keeps the file, never touches program
# routing) and logs loudly, converting what used to be a PERMANENT self-deadlock (every later gate
# run dying RIG_BUSY on our own leftover until a human manually intervened, #657 live incident:
# 4+ runs failed in one session, ~45 min of rig time lost) into a self-healing one.
#
# Env overrides (tests + operators):
#   STRIH_HOST / STREAM_HOST         - rig OBS WebSocket hosts (default 10.77.9.202 / .204)
#   OBS_PASSWORD                     - OBS WebSocket password (default "" — matches recording-e2e.sh)
#   OBS_PHASE2_PY                    - path to obs_phase2.py (default: sibling of this script)
#   RIG_BUSY_GATE_ITERATIONS         - max poll count (default 30)
#   RIG_BUSY_GATE_SLEEP_SECS         - seconds between polls (default 60)
#   STRAY_HEAL_THRESHOLD             - consecutive stray-only polls before self-heal (default 3, #657)
#   GITHUB_STEP_SUMMARY              - if set (GitHub Actions), OUTCOME=... is appended there too

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_PASSWORD="${OBS_PASSWORD:-}"
OBS_PHASE2_PY="${OBS_PHASE2_PY:-$HERE/obs_phase2.py}"
MAX_ITERATIONS="${RIG_BUSY_GATE_ITERATIONS:-30}"
SLEEP_SECS="${RIG_BUSY_GATE_SLEEP_SECS:-60}"
STRAY_HEAL_THRESHOLD="${STRAY_HEAL_THRESHOLD:-3}"

# Always echo the outcome to stdout (so it shows in the plain job log, and is asserted by the
# harness tests) AND append it to GITHUB_STEP_SUMMARY when running under GitHub Actions.
report_outcome() {
  echo "$1"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
}

# #657: per-host CONSECUTIVE stray-poll counters, reset to 0 the instant a poll does NOT show
# that host in stray_hosts (a real broadcast starting mid-wait, or the rig going genuinely free,
# must never inherit a stale streak from an earlier, unrelated stray episode).
declare -A STRAY_STREAK
STRAY_STREAK[strih]=0
STRAY_STREAK[stream]=0

# #657: StopRecord ONE box (via obs_phase2.py's existing `record --action stop` — the same call
# recording-e2e.sh's cleanup() and rig-mode.sh's stray-recording guard already use) once its
# consecutive stray-poll streak reaches STRAY_HEAL_THRESHOLD, then reset the streak. Never called
# for a box NOT in stray_hosts — obs_phase2.py's _stray_recording_hosts already guarantees
# stray_hosts never includes a box that is also streaming, so this can never touch a real
# broadcast.
self_heal_stray_box() {
  local label="$1" host="$2"
  if [ "${STRAY_STREAK[$label]}" -ge "$STRAY_HEAL_THRESHOLD" ]; then
    echo "::warning title=SELF-HEAL StopRecord (#657)::${label} (${host}) has shown OUR OWN stray recording (recording ON, streaming OFF) for ${STRAY_STREAK[$label]} consecutive polls -- StopRecording it now (file kept, program routing untouched) so this gate can proceed. Never done for a box that is also streaming."
    if python3 "$OBS_PHASE2_PY" record --host "$host" --action stop; then
      echo "[rig-busy-gate] #657 self-heal: ${label} StopRecord ok"
    else
      echo "::warning::#657 self-heal StopRecord failed for ${label} -- will retry on a later poll"
    fi
    STRAY_STREAK[$label]=0
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

  # #649 item 3: surface obs_phase2.py's plain-English diagnostic hint (per-box streaming vs
  # recording state -> stray-test-recording-vs-real-broadcast) as its OWN log line, not just
  # buried inside the raw JSON above — a future RIG_BUSY incident should be a 2-minute log read,
  # not a manual SSH+OBS inspection.
  HINT=$(printf '%s' "$OUTPUT" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("hint",""))' 2>/dev/null || true)
  if [ -n "$HINT" ]; then
    echo "[rig-busy-gate] HINT: $HINT"
  fi

  # #657: update each box's consecutive-stray-poll streak from this check's stray_hosts list,
  # then self-heal any box that just crossed the threshold. A box NOT in stray_hosts this poll
  # (free, or now a real broadcast, or the OTHER box) has its streak reset to 0 — a streak only
  # ever counts truly CONSECUTIVE stray polls.
  STRAY_HOSTS_CSV=$(printf '%s' "$OUTPUT" | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin).get("stray_hosts", [])))' 2>/dev/null || true)
  for _stray_label in strih stream; do
    if printf ',%s,' "$STRAY_HOSTS_CSV" | grep -q ",${_stray_label},"; then
      STRAY_STREAK[$_stray_label]=$(( STRAY_STREAK[$_stray_label] + 1 ))
    else
      STRAY_STREAK[$_stray_label]=0
    fi
  done
  self_heal_stray_box strih "$STRIH_HOST"
  self_heal_stray_box stream "$STREAM_HOST"

  echo "[rig-busy-gate] rig busy — will retry."
  if [ "$i" -lt "$MAX_ITERATIONS" ]; then
    sleep "$SLEEP_SECS"
  fi
done

echo "::error title=RIG BUSY - real broadcast detected, gate not run::rig still busy after ${MAX_ITERATIONS} checks (~$((MAX_ITERATIONS * SLEEP_SECS / 60)) min budget): $LAST_OUTPUT"
report_outcome "OUTCOME=RIG_BUSY"
exit 42
