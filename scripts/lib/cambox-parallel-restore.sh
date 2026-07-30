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
cambox_parallel_wait_and_report() {
  local _i=0
  local _rc=0
  local _pid
  for _pid in "${CAMBOX_PARALLEL_PIDS[@]}"; do
    if wait "$_pid"; then
      echo "    [cleanup] ${CAMBOX_PARALLEL_LABELS[$_i]} restore ok"
    else
      echo "    WARNING #712: ${CAMBOX_PARALLEL_LABELS[$_i]} restore failed/timed out in parallel cleanup — check it manually" >&2
      _rc=1
    fi
    _i=$((_i + 1))
  done
  CAMBOX_PARALLEL_PIDS=()
  CAMBOX_PARALLEL_LABELS=()
  return "$_rc"
}
