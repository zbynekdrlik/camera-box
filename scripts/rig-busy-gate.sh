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

# --------------------------------------------------------------------------------------------
# #830: shared cross-repo rig lease -- acquire BEFORE this gate does anything else (even before
# polling OBS state below), so a foreign CI run (restreamer, once it participates via
# zbynekdrlik/restreamer#349) can never have its E2E corrupted by us rerouting program scenes
# underneath it, and this gate can report WHO holds the rig instead of an ambiguous RIG_BUSY. The
# OBS-state busy-check loop below is UNCHANGED and stays wired in as the FALLBACK for a busy rig
# with no lease participant on the other side yet. See scripts/lib/rig-lease.sh for the full
# design + rationale (owner-settled lockdir-on-dev1 design, gh issue #830).
# --------------------------------------------------------------------------------------------
# shellcheck source=lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"

RIG_LEASE_REPO="${RIG_LEASE_REPO:-${GITHUB_REPOSITORY:-camera-box-local}}"
RIG_LEASE_RUN_ID="${RIG_LEASE_RUN_ID:-${GITHUB_RUN_ID:-local-$$}}"
RIG_LEASE_RUN_URL="${RIG_LEASE_RUN_URL:-}"
RIG_LEASE_JOB="${RIG_LEASE_JOB:-${GITHUB_JOB:-full-path}}"
# How long we expect to HOLD the rig once acquired (this gate's own busy-wait budget PLUS the
# recording step that follows it in the same CI job) -- written into holder.json so a foreign
# gate can decide whether to wait us out or fail fast.
RIG_LEASE_HOLD_SECS="${RIG_LEASE_HOLD_SECS:-2700}"
# Stale threshold comfortably ABOVE this job's own `timeout-minutes: 75` cap (full-path-e2e.yml)
# so a genuinely-still-running holder is NEVER reclaimed on heartbeat age alone -- a dead/crashed
# holder is still bounded-reclaimable, just not instantly (mirrors the #657 self-heal principle).
RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}"
# How long WE are willing to wait for a live foreign holder to release before giving up. Defaults
# to this gate's own OBS-busy budget (MAX_ITERATIONS * SLEEP_SECS below) so both loops share one
# mental model of "how long is this gate allowed to wait".
RIG_LEASE_MAX_WAIT_SECS="${RIG_LEASE_MAX_WAIT_SECS:-$((MAX_ITERATIONS * SLEEP_SECS))}"

# Release the lease on ANY exit path EXCEPT the "rig is free, proceeding" success path (which
# leaves it held for the recording step that runs later in the SAME job -- that step, and
# cancellation, are covered by the workflow's own separate `always()` release step, see
# scripts/rig-lease-release.sh). Safe to call unconditionally: rig_lease_release() itself is a
# no-op unless we are still the lease's current holder.
RIG_LEASE_PROCEEDING=0
rig_lease_cleanup_on_exit() {
  if [ "$RIG_LEASE_PROCEEDING" -ne 1 ]; then
    rig_lease_release "$RIG_LEASE_RUN_ID"
  fi
}
trap rig_lease_cleanup_on_exit EXIT

# rig_lease_epoch_of ISO8601 -> echo its epoch seconds, or "" if unparseable.
rig_lease_epoch_of() {
  date -u -d "$1" +%s 2>/dev/null || echo ""
}

# rig_lease_acquire_or_wait -- try to acquire the shared lease. If it is held by a genuinely LIVE
# foreign holder, either WAIT for it (retrying on the same iteration/sleep cadence as the OBS
# busy-check loop below) or FAIL FAST when the holder's own expected_release_at is further away
# than our whole wait budget -- no point grinding a budget against a soak that will obviously
# outlast it (the exact #830 complaint: 30 minutes burnt against a 45-minute restreamer soak).
rig_lease_acquire_or_wait() {
  local expected_release_at
  expected_release_at="$(date -u -d "+${RIG_LEASE_HOLD_SECS} seconds" +%Y-%m-%dT%H:%M:%SZ)"
  # Bounded by an explicit iteration count (never an unbounded while-true) -- one attempt per
  # sleep tick over the whole wait budget, plus one extra attempt for headroom. A zero SLEEP_SECS
  # (fast tests) would otherwise divide-by-zero; treat it as 1 tick's worth of spacing only for
  # sizing this bound, the actual `sleep "$SLEEP_SECS"` below still sleeps 0.
  local wait_tick=$(( SLEEP_SECS > 0 ? SLEEP_SECS : 1 ))
  local max_wait_attempts=$(( RIG_LEASE_MAX_WAIT_SECS / wait_tick + 2 ))

  for ((_lease_attempt = 0; _lease_attempt <= max_wait_attempts; _lease_attempt++)); do
    local acquire_out acquire_rc
    set +e
    acquire_out=$(rig_lease_acquire "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID" "$RIG_LEASE_RUN_URL" \
      "$RIG_LEASE_JOB" "$expected_release_at" "$RIG_LEASE_STALE_SECS")
    acquire_rc=$?
    set -e
    echo "[rig-busy-gate] $acquire_out"
    if [ "$acquire_rc" -eq 0 ]; then
      return 0
    fi

    local held_by exp_at now_epoch exp_epoch remaining
    held_by=$(printf '%s' "$acquire_out" | sed -n 's/^RIG_LEASE_HELD_BY=\([^ ]*\).*/\1/p')
    exp_at=$(printf '%s' "$acquire_out" | sed -n 's/.*expected_release_at=\([^ ]*\)$/\1/p')
    now_epoch=$(date -u +%s)
    exp_epoch="$(rig_lease_epoch_of "$exp_at")"
    if [ -n "$exp_epoch" ]; then
      remaining=$(( exp_epoch - now_epoch ))
    else
      remaining=0
    fi

    if [ "$remaining" -gt "$RIG_LEASE_MAX_WAIT_SECS" ]; then
      echo "::error title=RIG LEASE HELD (#830)::rig lease held by ${held_by}, expected free in ${remaining}s (~$(( remaining / 60 )) min) -- exceeds our max-wait budget (${RIG_LEASE_MAX_WAIT_SECS}s). Failing FAST rather than grinding a budget against a soak that will obviously outlast it."
      report_outcome "OUTCOME=RIG_LEASE_HELD RIG_HELD_BY=${held_by} expected_release_at=${exp_at}"
      exit 44
    fi

    if [ "$_lease_attempt" -ge "$max_wait_attempts" ]; then
      echo "::error title=RIG LEASE HELD (#830)::rig lease still held by ${held_by} after waiting ${RIG_LEASE_MAX_WAIT_SECS}s."
      report_outcome "OUTCOME=RIG_LEASE_HELD RIG_HELD_BY=${held_by} expected_release_at=${exp_at}"
      exit 44
    fi
    echo "[rig-busy-gate] rig lease held by ${held_by} (expected free ~${exp_at}) -- waiting..."
    sleep "$SLEEP_SECS"
  done

  # Unreachable in practice (the loop body always either returns or exits) -- defensive fail
  # closed rather than falling through silently if that invariant is ever broken.
  echo "::error title=RIG LEASE HELD (#830)::rig lease wait loop exhausted unexpectedly."
  report_outcome "OUTCOME=RIG_LEASE_HELD"
  exit 44
}

rig_lease_acquire_or_wait

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
    # #830: this gate keeps the lease HELD across the exit — the recording step that follows in
    # the same CI job needs it. The workflow's own separate `always()` step (see
    # scripts/rig-lease-release.sh) releases it later, on success, on a later step's failure, AND
    # on cancellation.
    RIG_LEASE_PROCEEDING=1
    report_outcome "OUTCOME=RIG_FREE"
    exit 0
  fi

  # #830: keep our own lease heartbeat fresh across a long busy-wait (comfortably unnecessary at
  # the default budgets -- MAX_ITERATIONS*SLEEP_SECS <= RIG_LEASE_STALE_SECS -- but cheap and
  # correct regardless of overrides).
  rig_lease_heartbeat_touch

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
    if grep -q ",${_stray_label}," <<<",${STRAY_HOSTS_CSV},"; then
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
