#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors the
# sibling scripts/lib/v4l2-neutral.sh / camera-box-restart-verify.sh convention (a function that
# returns a REMOTE COMMAND STRING for the caller to embed via command substitution — this file
# itself performs no ssh, no device I/O).
#
# scripts/lib/tmp-burn-sweep.sh — sweep stale camera-box-burn-<RUN_ID> binaries out of a cam box's
# /tmp BEFORE the next deploy (#749).
#
# WHY: recording-e2e.sh's [2/8]/[2b/8] steps scp the probe-featured camera-box binary to a
# per-RUN_ID-unique /tmp path (~2.3MB) on the SOURCE box (`/tmp/camera-box-burn-<RUN_ID>` on cam1,
# `/tmp/camera-box-burn-<camname>-<RUN_ID>` on cam2/3/4/5/6 under ALL_CAMBOX). cleanup()'s own
# end-of-run `rm -f /tmp/camera-box-burn-*` is best-effort inside a `timeout $CLEANUP_SSH_TIMEOUT`
# -bounded ssh call chained after several other remote commands (systemctl stop/reset-failed/
# pkill/restart) — if the ssh round-trip itself fails to establish, or a real connectivity blip
# drops the channel before that remote shell reaches the `rm -f` line (a real, recurring condition
# on cam boxes with flaky storage/SSH — #737's dying cam2 disk), that run's binary is orphaned
# permanently and cleanup() never gets a second chance at it.
#
# Each box's /tmp is a 100MB tmpfs. At ~2.3MB/run and roughly hourly cadence, ~40-45 orphaned
# binaries fill it outright — CAM1 and CAM6 both hit 100% live (#749) and the NEXT run's scp then
# failed HARD (`scp: write remote ... Failure`) instead of merely wasting space.
#
# FIX: an age-gated sweep run on the box BEFORE its scp, independent of whether the LAST run's own
# end-of-run cleanup succeeded — self-heals any backlog regardless of how it accumulated, and
# never depends on a single fragile ssh round-trip landing at exactly the right moment. `-mmin +60`
# never touches a file from a run still genuinely in flight (a normal recording-e2e.sh run takes a
# few minutes; #749's own worst-case observed gap between consecutive runs is far under 60 min) or
# the file THIS run is about to write (which doesn't exist yet when the sweep runs, since it always
# runs BEFORE this run's own scp).

# tmp_burn_sweep_stale_cmds — the remote shell command that removes stale camera-box-burn-* files
# from /tmp on whichever box it's embedded against. Returns the SAME command text regardless of
# camera name / RUN_ID (the glob + age filter cover every box's own naming scheme uniformly), so
# one function serves cam1's [2/8] site and every box in the [2b/8] ALL_CAMBOX loop alike.
tmp_burn_sweep_stale_cmds() {
  echo "find /tmp -maxdepth 1 -name 'camera-box-burn-*' -mmin +60 -delete 2>/dev/null || true;"
}
