#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) —
# matches the sibling scripts/lib/*.sh convention (rig-test-dropin.sh, camera-box-restart-verify.sh)
# of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the
# CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
# recording-e2e.sh (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/cambox-parallel-restore.sh — #712: launch a cambox device-restore ssh call PER BOX
# CONCURRENTLY (not sequentially) and wait for all of them, reporting each box's outcome BY NAME.
#
# WHY: a GitHub Actions run cancellation kills the runner PROCESS directly -- it does not wait
# for a bash trap's SEQUENTIAL per-cambox restore loop (one ssh round-trip per box, each up to
# CLEANUP_SSH_TIMEOUT=30s) to finish every iteration before the hard kill lands. Live incident
# (2026-07-12): cancelling an ALL_CAMBOX `recording-e2e.sh` run mid-sweep left cam3/4/5/6
# stranded -- their `camera-box-burn-*` binaries still held `/dev/videoN`, `camera-box.service`
# crash-looping ("Device or resource busy"), and burns left ON on every strih NDI input (a
# visible-QR risk on a live broadcast) -- while cam1/cam2 (restored outside this loop) were
# already clean by the time the cancellation landed. Backgrounding every box's restore call
# bounds the WHOLE loop's wall-clock by the SLOWEST single box instead of the SUM of all of
# them, so the loop fits inside a short cancellation grace window regardless of how many
# camboxes are active in CAMBOX_SWEEP.
#
# #713 EXTENSION (2026-07-12): the SAME live-incident class recurred after #712 shipped -- a
# cancellation landing after the (now-parallel) cam3/4/5/6 loop still stranded cam2, because
# cam1's restore (BEFORE the loop) and cam2/painter's restore (AFTER the loop) were still two
# separate, sequential, un-backgrounded calls outside #712's own parallel group. cleanup() now
# backgrounds ALL of cam1 + the cam3/4/5/6 loop (when ALL_CAMBOX=1) + cam2/painter into this SAME
# group, with ONE shared cambox_parallel_wait_and_report call at the very end of the whole
# device-restore phase -- up to 6 boxes concurrently, not just 4.
#
# Usage (see recording-e2e.sh's cleanup(), the whole device-restore phase -- cam1, the cam3/4/5/6
# ALL_CAMBOX loop, and cam2/painter all append into ONE shared pair of arrays):
#   CAMBOX_PARALLEL_PIDS=()
#   CAMBOX_PARALLEL_LABELS=()
#   ( timeout "$CLEANUP_SSH_TIMEOUT" sshpass ... ssh ... root@"$CAM1_IP" "..." ) &
#   CAMBOX_PARALLEL_PIDS+=("$!")
#   CAMBOX_PARALLEL_LABELS+=("$CAMERA_NAME (source, $CAM1_IP)")
#   for _cip in "$CAM3_IP" "$CAM4_IP" "$CAM5_IP" "$CAM6_IP"; do
#     ( timeout "$CLEANUP_SSH_TIMEOUT" sshpass ... ssh ... root@"$_cip" "..." ) &
#     CAMBOX_PARALLEL_PIDS+=("$!")
#     CAMBOX_PARALLEL_LABELS+=("$_ccn ($_cip)")
#   done
#   ( timeout "$CLEANUP_SSH_TIMEOUT" sshpass ... ssh ... root@"$PAINTER_IP" "..." ) &
#   CAMBOX_PARALLEL_PIDS+=("$!")
#   CAMBOX_PARALLEL_LABELS+=("cam2/painter, $PAINTER_IP")
#   cambox_parallel_wait_and_report
#
# cambox_parallel_wait_and_report waits for EVERY collected PID INDIVIDUALLY (never a bare
# `wait` with no arguments, which reports only an aggregate success/fail and loses which PID
# belongs to which box) and echoes each box's outcome under its OWN label — a passing box's
# success is never conflated with a failing box's warning, and vice versa. Mirrors the existing
# #649/#675 warn-don't-abort discipline already used elsewhere in cleanup(): it NEVER `exit`s,
# so one box's restore failure can never kill the trap before it reaches the rest of cleanup().
# Consumes (and clears) the CAMBOX_PARALLEL_PIDS/CAMBOX_PARALLEL_LABELS arrays the caller filled
# in; returns 1 if ANY box failed/timed out (the caller MAY act on this, but is never required
# to — the loud per-box WARNING #712 line is itself the fail-loud signal).
# cambox_parallel_label_ip LABEL -> print the SINGLE IPv4 embedded in a box label (#715). The
# labels the caller records carry the box IP, e.g. "cam1 (source, 10.77.9.61)", "cam3
# (10.77.9.63)", "cam2/painter, 10.77.9.62" -- each exactly one IP. FAIL-OPEN: a label with ZERO
# or MORE THAN ONE IPv4 prints nothing and returns 1, so the retry below skips that box and leaves
# it to the #684 FINAL pass rather than guessing a target. The label->IP coupling is a deliberate,
# documented interim: the root-cause follow-up will pass an explicit IP array from the launch
# sites and this parser retires with it.
cambox_parallel_label_ip() {
  local _label="$1"
  local _ips _n
  _ips="$(printf '%s\n' "$_label" | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}')" || true
  _n="$(printf '%s' "$_ips" | grep -c .)" || true
  if [ "${_n:-0}" = 1 ]; then
    printf '%s\n' "$_ips"
    return 0
  fi
  return 1
}

# _cambox_retry_remote_cmd [painter] -> print the REMOTE bash text for a single sequential
# recovery attempt (#715). Generic: free the capture device (the #626 self-match-safe anchored
# burn pkill), restart camera-box, then poll `systemctl is-active` (~16s); the final test sets the
# ssh status so the caller's `if timeout ... ssh ...; then` branches on genuine recovery. For a
# "painter" box it ALSO restarts cam2-painter.service and requires BOTH units active -- a generic
# camera-box restart alone leaves cam2's monitor BLACK (the #860 trap), so its recovery must be
# VERIFIED before the caller prunes it from the failed set. Single-quoted heredocs: every remote
# `$`/`$(...)` is literal (evaluated on the cam box, never locally on dev1).
_cambox_retry_remote_cmd() {
  if [ "${1:-}" = painter ]; then
    cat <<'CBOX715P'
pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null || true; pkill -x camera-box 2>/dev/null || true; sleep 1; systemctl restart camera-box 2>/dev/null || true; _cbr=0; while [ "$(systemctl is-active camera-box 2>/dev/null)" != active ] && [ $_cbr -lt 16 ]; do sleep 1; _cbr=$((_cbr+1)); done; if [ "$(systemctl is-active camera-box 2>/dev/null)" = active ]; then systemctl start cam2-painter 2>/dev/null || true; _cbp=0; while [ "$(systemctl is-active cam2-painter 2>/dev/null)" != active ] && [ $_cbp -lt 16 ]; do sleep 1; _cbp=$((_cbp+1)); done; [ "$(systemctl is-active cam2-painter 2>/dev/null)" = active ]; else false; fi
CBOX715P
  else
    cat <<'CBOX715G'
pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null || true; pkill -x camera-box 2>/dev/null || true; sleep 1; systemctl restart camera-box 2>/dev/null || true; _cbr=0; while [ "$(systemctl is-active camera-box 2>/dev/null)" != active ] && [ $_cbr -lt 16 ]; do sleep 1; _cbr=$((_cbr+1)); done; [ "$(systemctl is-active camera-box 2>/dev/null)" = active ]
CBOX715G
  fi
}

# cambox_parallel_retry_failed -> (#715) sequentially retry every box the parallel burst left in
# CAMBOX_PARALLEL_FAILED_LABELS, ONE AT A TIME. The parallel restore group fails ~100% at the
# current fleet because 3 ssh sessions launched in the same instant are connection-rejected within
# ~2s (a dev1-side burst, confirmed by exact CI timing); the identical command run one-at-a-time
# (as the later #684 FINAL pass proves every run) succeeds. So this pass runs AFTER the burst has
# drained, waits a short settle, and retries each failed box IN SEQUENCE, pruning the ones that
# genuinely recover so cambox_parallel_surface_painter_failure only surfaces still-dead boxes.
# Strictly sequential -> never itself bursts. Gated on CAM_PW so a no-credential context (unit
# tests) never touches the rig. Always returns 0 and keeps every ssh inside an `if ...; then` --
# cleanup()'s trap must always run to completion (the #649/#675/#712 warn-only discipline).
cambox_parallel_retry_failed() {
  [ -n "${CAM_PW:-}" ] || return 0
  [ "${#CAMBOX_PARALLEL_FAILED_LABELS[@]}" -gt 0 ] || return 0
  local _delay="${CAMBOX_PARALLEL_RETRY_DELAY:-2}"
  # settle: let any sshd/dev1 burst-penalty state from the parallel group decay before the first
  # retry (the retries below are one-at-a-time, so this pass never bursts by construction).
  sleep "$_delay" 2>/dev/null || true
  local _lbl _ip _remote _kind
  local _still=()
  for _lbl in "${CAMBOX_PARALLEL_FAILED_LABELS[@]}"; do
    _ip="$(cambox_parallel_label_ip "$_lbl")" || _ip=""
    if [ -z "$_ip" ]; then
      echo "    WARNING #715: cannot parse a single retry-target IP from \"$_lbl\" -- leaving it for the #684 FINAL pass" >&2
      _still+=("$_lbl")
      continue
    fi
    case "$_lbl" in
      *painter*) _kind=painter ;;
      *)         _kind=generic ;;
    esac
    _remote="$(_cambox_retry_remote_cmd "$_kind")"
    if timeout "${CLEANUP_SSH_TIMEOUT:-30}" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_ip" "$_remote"; then
      echo "    [cleanup] #715: $_lbl recovered on sequential retry (after the parallel burst drained)"
    else
      echo "    WARNING #715: $_lbl still down after a sequential retry -- leaving it for the #684 FINAL pass" >&2
      _still+=("$_lbl")
    fi
  done
  if [ "${#_still[@]}" -gt 0 ]; then
    CAMBOX_PARALLEL_FAILED_LABELS=("${_still[@]}")
  else
    CAMBOX_PARALLEL_FAILED_LABELS=()
  fi
  return 0
}

cambox_parallel_wait_and_report() {
  local _i=0
  local _rc=0
  local _pid
  # #860: reset the failed-label ledger at the START of every wait (never at the end -- the caller
  # reads it AFTER this returns to surface a cam2/painter failure loudly, see
  # cambox_parallel_surface_painter_failure below). Additive to the existing per-box WARNING #712 +
  # non-zero return; never a signature change.
  CAMBOX_PARALLEL_FAILED_LABELS=()
  for _pid in "${CAMBOX_PARALLEL_PIDS[@]}"; do
    if wait "$_pid"; then
      echo "    [cleanup] ${CAMBOX_PARALLEL_LABELS[$_i]} restore ok"
    else
      echo "    WARNING #712: ${CAMBOX_PARALLEL_LABELS[$_i]} restore failed/timed out in parallel cleanup — check it manually" >&2
      CAMBOX_PARALLEL_FAILED_LABELS+=("${CAMBOX_PARALLEL_LABELS[$_i]}")
      _rc=1
    fi
    _i=$((_i + 1))
  done
  # #715: the parallel group fails ~100% at the current fleet -- 3 ssh sessions launched in the
  # same instant are connection-rejected within ~2s (a dev1-side burst; confirmed by exact CI
  # timing). Now that the burst has drained, retry each failed box ONE AT A TIME -- the identical
  # command run sequentially succeeds (the #684 FINAL pass proves this) -- and prune the ones that
  # genuinely recover. The original per-box WARNING #712 telemetry above is preserved either way
  # (evidence for the root-cause follow-up: stagger/bound the launch concurrency at
  # recording-e2e.sh's launch sites). _rc reflects the FINAL (pruned) failed set.
  cambox_parallel_retry_failed
  if [ "${#CAMBOX_PARALLEL_FAILED_LABELS[@]}" -gt 0 ]; then _rc=1; else _rc=0; fi
  CAMBOX_PARALLEL_PIDS=()
  CAMBOX_PARALLEL_LABELS=()
  return "$_rc"
}

# cambox_parallel_surface_painter_failure -> surface the MOST RECENT cambox_parallel_wait_and_report's
# failed labels (CAMBOX_PARALLEL_FAILED_LABELS) as GitHub Actions annotations, so a restore failure
# is VISIBLE in the run summary instead of buried in stderr (#860). The cam2/painter box -- a dead
# painter leaves cam2's monitor BLACK and silently poisons the next gate run's optical hop (the
# 2026-08-14 incident) -- escalates to `::error::`; any OTHER box stays a milder `::warning::`. NEVER
# `exit`s (cleanup()'s trap must always run to completion, the #649/#675/#712 warn-only discipline);
# the standing optical-chain watchdog (#860) owns the throttled Discord paging.
cambox_parallel_surface_painter_failure() {
  local _lbl
  for _lbl in "${CAMBOX_PARALLEL_FAILED_LABELS[@]:-}"; do
    [ -n "$_lbl" ] || continue
    case "$_lbl" in
      *cam2/painter*)
        echo "::error title=cam2 painter restore FAILED (issue 860 / WARNING 712)::${_lbl} restore failed/timed out in cleanup — cam2's monitor may be left BLACK, silently poisoning the next gate run's cam2->cam1 optical hop. Restore with scripts/rig-mode.sh test; the standing optical-chain watchdog also pages this." >&2
        ;;
      *)
        echo "::warning title=cambox restore FAILED (WARNING 712)::${_lbl} restore failed/timed out in cleanup — check it manually." >&2
        ;;
    esac
  done
  return 0
}
