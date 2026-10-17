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
# issue 1404 rig race (14.10.2026): one idle read is not a free rig. restreamer's YouTube disconnect
# test republishes stream OBS with a deliberate gap (up to ~45 s), and run 38520662080 was admitted
# on a read inside a 10 s gap. A run is admitted only after strih + stream read idle CONTINUOUSLY for
# IDLE_HOLD_SECS: one read every IDLE_STEP_SECS of the hold (IDLE_POLL_SECS apart; it follows
# SLEEP_SECS down to 0 so the offline tests stay instant). The lease is released at the first read
# that sees a box streaming, so a foreign broadcast is never failed by our lease. Since issue 1432 (ROZHODNUTÉ
# 6182938785) the hold runs UNDER the lease: every restreamer stream-lan job takes the lease for its
# whole job (restreamer PR 400), so its republish gaps happen under its own lease, and releasing
# during the hold let restreamer's next job take the rig (run 38667117544 starved).
IDLE_HOLD_SECS="${RIG_BUSY_GATE_IDLE_HOLD_SECS:-60}"
IDLE_STEP_SECS=5
IDLE_POLL_SECS="${RIG_BUSY_GATE_IDLE_POLL_SECS:-$((SLEEP_SECS < IDLE_STEP_SECS ? SLEEP_SECS : IDLE_STEP_SECS))}"

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

# issue 1450: the boxes this run touches (RIG_LEASE_RESOURCES, a CSV of the atoms / groups of
# scripts/rig-lease-resources.txt; each workflow step that runs this gate declares it). Every
# rig_lease_acquire below reads it from the env. Checked ONCE here, before any lease or OBS read: a
# typo is a configuration error that no wait can cure, so it fails fast (exit 45), never a retry
# loop and never "no protection". Unset = the whole rig, as before (nothing printed).
if [ -n "${RIG_LEASE_RESOURCES:-}" ]; then
  _lease_resources_rc=0
  _lease_resources="$(rig_lease_resources_resolve "$RIG_LEASE_RESOURCES")" || _lease_resources_rc=$?
  if [ "$_lease_resources_rc" -ne 0 ] || [ -z "$_lease_resources" ]; then
    echo "::error title=RIG LEASE BAD RESOURCES (issue 1450)::RIG_LEASE_RESOURCES='${RIG_LEASE_RESOURCES}' does not resolve (rc ${_lease_resources_rc}: ${_lease_resources:-the table is unreadable}) -- the names are the atoms and groups of scripts/rig-lease-resources.txt. Refusing before any lease or OBS read."
    report_outcome "OUTCOME=RIG_LEASE_BAD_RESOURCES RIG_LEASE_BAD_RESOURCES=${_lease_resources:-unresolvable}"
    exit 45
  fi
  echo "[rig-busy-gate] rig lease resources: ${RIG_LEASE_RESOURCES} -> ${_lease_resources}"
fi

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

# rig_busy_read -> one rig-busy-check over strih + stream: sets OUTPUT, LAST_OUTPUT and BUSY
# (True / False); an unreachable rig ends the gate here (exit 43, unchanged).
rig_busy_read() {
  local rc
  set +e
  OUTPUT=$(python3 "$OBS_PHASE2_PY" rig-busy-check \
    --strih-host "$STRIH_HOST" --stream-host "$STREAM_HOST" \
    --password "$OBS_PASSWORD" 2>&1)
  rc=$?
  set -e
  echo "[rig-busy-gate] $OUTPUT"
  LAST_OUTPUT="$OUTPUT"

  if [ "$rc" -eq 3 ]; then
    echo "::error title=RIG UNREACHABLE - gate not run::obs_phase2.py rig-busy-check could not reach strih and/or stream OBS WebSocket: $OUTPUT"
    report_outcome "OUTCOME=RIG_UNREACHABLE"
    exit 43
  fi
  if [ "$rc" -ne 0 ]; then
    echo "::error title=RIG UNREACHABLE - gate not run::obs_phase2.py rig-busy-check exited unexpectedly ($rc): $OUTPUT"
    report_outcome "OUTCOME=RIG_UNREACHABLE"
    exit 43
  fi
  BUSY=$(printf '%s' "$OUTPUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["busy"])')
}

# rig_busy_any_streaming -> 0 iff the last read names a box that is STREAMING (a broadcast, ours never).
rig_busy_any_streaming() {
  [ "$(printf '%s' "$OUTPUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("yes" if any(x.get("streaming") for x in d.get("diagnostics", [])) else "")' 2>/dev/null || true)" = yes ]
}

# rig_busy_idle_hold -> 0 iff every read over the next IDLE_HOLD_SECS is idle (issue 1404). Runs
# with our lease held (issue 1432); returns 1 at the first busy read, with OUTPUT/BUSY holding that read.
rig_busy_idle_hold() {
  local n reads=$(((IDLE_HOLD_SECS + IDLE_STEP_SECS - 1) / IDLE_STEP_SECS))
  echo "[rig-busy-gate] idle read -- confirming the rig stays idle for ${IDLE_HOLD_SECS} s (${reads} reads, issue 1404: a stream republish gap is not a free rig); our lease stays held meanwhile (issue 1432)"
  for ((n = 1; n <= reads; n++)); do
    sleep "$IDLE_POLL_SECS"
    rig_busy_read
    if [ "$BUSY" != "False" ]; then
      echo "[rig-busy-gate] the rig went busy again after $((n * IDLE_STEP_SECS)) s of the idle hold -- not a free rig"
      return 1
    fi
  done
  return 0
}

LAST_OUTPUT=""
LEASE_HELD=1   # rig_lease_acquire_or_wait above returned with the lease held
for ((i = 1; i <= MAX_ITERATIONS; i++)); do
  echo "[rig-busy-gate] check ${i}/${MAX_ITERATIONS} — querying strih ($STRIH_HOST) + stream ($STREAM_HOST) OBS WebSocket..."
  rig_busy_read

  if [ "$BUSY" = "False" ]; then
    # issue 1432: take the lease back (it was released while a box streamed) BEFORE the hold, and
    # keep it through the hold, so no other holder can take the rig between the hold and the run.
    if [ "$LEASE_HELD" -eq 0 ]; then
      rig_lease_acquire_or_wait
      LEASE_HELD=1
    fi
    if rig_busy_idle_hold; then
      # beat the lease (nothing refreshed it over the hold) and confirm it is still ours: never
      # admit on a lease another holder took over meanwhile (rc 1). rc 2 (a filesystem error on
      # the beat) and rc 3 (past the hold ceiling, only reachable after a >75 min gate wait) leave
      # the lease ours, so they are admitted, logged.
      _refresh_rc=0
      rig_lease_refresh_if_mine "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID" >/dev/null || _refresh_rc=$?
      [ "$_refresh_rc" -eq 0 ] || [ "$_refresh_rc" -eq 1 ] \
        || echo "[rig-busy-gate] WARNING: lease beat after the idle hold returned rc ${_refresh_rc} (the lease is still ours) -- admitting"
      if [ "$_refresh_rc" -eq 1 ]; then
        echo "[rig-busy-gate] our rig lease was taken over during the idle hold -- not admitted (issue 1432)"
        LEASE_HELD=0
        BUSY="True"
      else
        echo "[rig-busy-gate] idle for the whole hold -- final read under the lease"
        rig_busy_read
      fi
    fi
  fi
  if [ "$BUSY" = "False" ]; then
    echo "[rig-busy-gate] rig is free — proceeding."
    # #830: this gate keeps the lease HELD across the exit — the recording step that follows in
    # the same CI job needs it. The workflow's own separate `always()` step (see
    # scripts/rig-lease-release.sh) releases it later, on success, on a later step's failure, AND
    # on cancellation.
    RIG_LEASE_PROCEEDING=1
    # issue 1383: keep the held lease visibly alive until the recording step's own heartbeat
    # refresher takes over (the verdict-exe fetch step + the first preflight ran ~18 min unbeaten).
    # Detached, all fds on /dev/null; it ends on release, a foreign holder or the hold ceiling.
    rig_lease_keepalive_spawn "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID"
    report_outcome "OUTCOME=RIG_FREE"
    exit 0
  fi

  # #830: keep our own lease heartbeat fresh across a long busy-wait (comfortably unnecessary at
  # the default budgets -- MAX_ITERATIONS*SLEEP_SECS <= RIG_LEASE_STALE_SECS -- but cheap and
  # correct regardless of overrides). issue 1383: through the one holder keep-alive, which never
  # bumps a foreign holder's heartbeat and rolls the declared release time while we wait.
  # issue 1404: never hold the lease while a box STREAMS (a foreign broadcast in progress, e.g.
  # restreamer's YouTube run, whose own restart refuses a held lease); it is taken again on the
  # next idle read, before the idle hold (issue 1432). A stray recording alone keeps it: the #657 self-heal below mutates the rig.
  if rig_busy_any_streaming; then
    if [ "$LEASE_HELD" -eq 1 ]; then
      echo "[rig-busy-gate] a box is streaming -- releasing our rig lease while we wait (issue 1404)"
      rig_lease_release "$RIG_LEASE_RUN_ID"
      LEASE_HELD=0
    fi
  elif [ "$LEASE_HELD" -eq 1 ]; then
    rig_lease_refresh_if_mine "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID" >/dev/null || true
  else
    rig_lease_acquire_or_wait
    LEASE_HELD=1
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
