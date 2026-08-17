#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the scripts/lib/audio-presence-preflight.sh / capture-rate-guard.sh
# convention (no top-level `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/cbox-burn-log-persist.sh -- persist each cam-box burn-run fps log to dev1 (#716).
#
# #716 root cause (found while confirming a capture-rate telemetry question): a cam-box burn run's
# own `Streaming: X fps emitted / Y fps captured` telemetry is written FILE-ONLY. recording-e2e.sh
# launches the burn systemd-run unit with `--property=StandardOutput=append:/tmp/cbox-burn.log`
# (cam1) / `/tmp/cbox-burn-<cn>.log` (each ALL_CAMBOX secondary), so the fine-grained per-window
# fps output goes STRAIGHT to that file and never reaches journald. The NEXT gate run's deploy step
# then `rm -f`s the file before its own burn, and the harness only ever scp's back the coarse
# end-of-run summary (`cam1-capture-stats.txt`) -- it never copies the fps log to the run's $OUTDIR.
# Net: at any moment only the LATEST run's fine-grained fps log survives on the box, so correlating
# cam-box capture-rate against a SPECIFIC past recording window is impossible unless it happens to
# be the very latest run.
#
# This lib mirrors the proven `cam1-capture-stats.txt` per-run scp-back sidecar: a few KB per run
# turns "only the latest run is inspectable" into "every archived /tmp/recording-e2e-<RUN_ID>/ on
# dev1 carries its own cam-box capture-rate ground truth".
#
# The DECISION LOGIC (which path each box writes, the per-run OUTDIR filename) lives here as pure
# functions so it is Tier-0 unit-testable (tests/harness_cbox_burn_log_persist.rs); the
# recording-e2e.sh step is a thin caller. Source-only: no side effects at source time.

# cbox_burn_log_remote_path CAM -> the box's own /tmp burn-log path. cam1 (the source-camera burn,
# whatever camera $CAMERA_NAME resolved to -- the file is named `cam1-*` by convention, like
# cam1-capture-stats.txt) writes the BARE /tmp/cbox-burn.log; every ALL_CAMBOX secondary writes the
# -<cn>-infixed /tmp/cbox-burn-<cn>.log -- matching recording-e2e.sh's own StandardOutput=append:
# targets at the [2/8] / [2b/8] deploy sites.
cbox_burn_log_remote_path() {
  local cam="$1"
  if [ "$cam" = "cam1" ]; then
    printf '/tmp/cbox-burn.log\n'
  else
    printf '/tmp/cbox-burn-%s.log\n' "$cam"
  fi
}

# cbox_burn_log_dest_name CAM RUN_ID -> the per-run OUTDIR filename for that box's burn log,
# mirroring the existing cam1-capture-stats.txt <cam>-* sidecar naming (e.g. cam1-cbox-burn-<id>.log).
cbox_burn_log_dest_name() {
  printf '%s-cbox-burn-%s.log\n' "$1" "$2"
}

# cbox_burn_log_persist CAM_PW CAM_IP CAM RUN_ID OUTDIR -> scp the box's burn-run fps log back to
# $OUTDIR/<cam>-cbox-burn-<run_id>.log on dev1. BEST-EFFORT: a missing/failed fetch only WARNs and
# STILL returns 0. This runs this far into a real ~300s gate run (right beside the cam1-capture-stats
# scp), where aborting on a transient scp failure would waste the whole cycle -- same tolerance as
# the sidecar it sits beside. ConnectTimeout bounds a genuinely-dead box so it never hangs the run.
cbox_burn_log_persist() {
  local cam_pw="$1" cam_ip="$2" cam="$3" run_id="$4" outdir="$5"
  local remote dest
  remote="$(cbox_burn_log_remote_path "$cam")"
  dest="$outdir/$(cbox_burn_log_dest_name "$cam" "$run_id")"
  sshpass -p "$cam_pw" scp -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    root@"$cam_ip":"$remote" "$dest" 2>/dev/null \
    || echo "WARNING: could not fetch $cam burn-run fps log ($remote) -- capture-rate forensics for this run omitted (#716)" >&2
  return 0
}
