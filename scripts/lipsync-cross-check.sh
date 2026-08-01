#!/usr/bin/env bash
# lipsync-cross-check.sh -- issue 930: given a lipsync-test-mode stream recording and a paired
# QR/QPSK TEST-mode stream recording (SAME rig state, recorded back-to-back), compute BOTH
# offsets and print the cross-check verdict.
#
set -euo pipefail
#
# WHY / WHAT this script does NOT do (deliberate scope boundary, issue 930's own worker
# dispatch): capturing the two recordings themselves (StartRecord/StopRecord on the stream OBS,
# switching cam2 between lipsync-test-mode.sh and rig-mode.sh TEST) reuses EXISTING, already
# battle-tested tooling (recording-e2e.sh's own StartRecord/StopRecord + fetch machinery,
# scripts/lipsync-test-mode.sh for the lipsync-mode swap, rig-mode.sh for TEST-mode restore) --
# this script does NOT reinvent that orchestration. It is the NEW glue issue 930 actually
# introduces: given the two ALREADY-CAPTURED stream recordings (paths as CLI args) plus the
# lipsync recording's QPSK-marker-log-equivalent, it (1) segments the lipsync recording into
# ~20s chunks (matching scripts/av_sync_measure.py's own --secs 20 convention), (2) runs
# av_sync_measure.py --media on each chunk + av_sync_calibrate.py --calibrate to get the
# SEM-shrunk SyncNet offset (issue 917's engine, issue 805's aggregator -- zero new Python math),
# (3) runs `recording-verdict --av-sync --syncnet-offset-ms <that>` on the QR/QPSK recording to
# get the cross-check verdict, and (4) prints both offsets + the delta (the ticket's own
# acceptance criterion 1).
#
# Usage:
#   lipsync-cross-check.sh \
#     --lipsync-recording  <path to the lipsync-test-mode stream recording> \
#     --qrqpsk-recording   <path to the paired QR/QPSK TEST-mode stream recording> \
#     --qrqpsk-marker-log  <path to that recording's cam2 QPSK emit-log CSV> \
#     --verdict-bin <path to the recording-verdict binary> \
#     [--workdir <scratch dir, default: a mktemp -d>]
#
# `--verdict-bin` is REQUIRED, with NO local-build default -- recording-verdict needs `--features
# probe` to even exist as a binary, and this repo's Local Build Policy forbids building that
# locally (Tier 0 -- CI builds it, download the probe-tools-linux-amd64 artifact). Mirrors
# recording-verdict-on-imag.sh's own --verdict-bin (no baked-in default there either).
#
# Prints the final JSON (recording-verdict's own --av-sync + lipsync_cross_check output) to
# stdout and a one-line human summary to stderr.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"

# --------------------------------------------------------------------------------------------- #
# PURE functions -- sourced + unit-tested by tests/harness_lipsync_cross_check.rs, zero network,
# zero real recording decode.
# --------------------------------------------------------------------------------------------- #

# lipsync_segment_cmd <input> <segment_secs> <out_pattern> -- the ffmpeg segment-muxer command
# that splits <input> into ~<segment_secs>-long chunks (issue 930's SyncNet-side windowing,
# matching av_sync_measure.py's own --secs 20 default). `-c copy` (no re-encode -- the trimmed
# asset is already a fixed, known H.264/AAC geometry, see PROVENANCE.md) keeps this instant.
lipsync_segment_cmd() {
  local input="$1" secs="$2" out_pattern="$3"
  printf 'ffmpeg -y -i %q -c copy -map 0 -f segment -segment_time %q -reset_timestamps 1 %q' \
    "$input" "$secs" "$out_pattern"
}

# lipsync_measure_chunk_cmd <python> <av_sync_measure.py> <chunk> <calibration_log> -- one
# SyncNet measurement of a single ~20s chunk, appending to the shared calibration-log JSONL
# (issue 917's engine; the JSONL is what av_sync_calibrate.py --calibrate then aggregates).
lipsync_measure_chunk_cmd() {
  local py="$1" script="$2" chunk="$3" calibration_log="$4"
  printf '%s %s --media %q --calibration-log %q' \
    "$(printf '%q' "$py")" "$(printf '%q' "$script")" "$chunk" "$calibration_log"
}

# lipsync_aggregate_cmd <python> <av_sync_calibrate.py> <calibration_log> <report_json> -- the
# SEM-shrunk aggregate over every measured chunk (issue 805's engine, already unit-tested --
# zero new math here).
lipsync_aggregate_cmd() {
  local py="$1" script="$2" calibration_log="$3" report_json="$4"
  printf '%s %s --calibrate %q --report-json %q' \
    "$(printf '%q' "$py")" "$(printf '%q' "$script")" "$calibration_log" "$report_json"
}

# lipsync_verdict_cmd <verdict_bin> <qrqpsk_recording> <marker_log> <syncnet_offset_ms> -- the
# final recording-verdict --av-sync call carrying the aggregated SyncNet offset (issue 930's own
# --syncnet-offset-ms wiring), producing the printed cross-check JSON.
lipsync_verdict_cmd() {
  local bin="$1" recording="$2" marker_log="$3" syncnet_offset_ms="$4"
  printf '%s --av-sync %q --av-marker-log %q --syncnet-offset-ms %q' \
    "$(printf '%q' "$bin")" "$recording" "$marker_log" "$syncnet_offset_ms"
}

# lipsync_mean_offset_from_report_json <report_json_text> -- pull `mean_offset_ms` out of
# av_sync_calibrate.py --calibrate's --report-json output (a tiny JSON object: {n, n_total,
# mean_offset_ms, stdev_ms, ci95_ms}). Uses python3 (already a hard dependency of this whole
# toolchain) rather than adding a jq dependency.
lipsync_mean_offset_from_report_json() {
  local json_text="$1"
  python3 -c 'import json,sys; print(json.loads(sys.argv[1])["mean_offset_ms"])' "$json_text"
}

# --------------------------------------------------------------------------------------------- #
# Orchestration (real network/filesystem effects) -- exercised end-to-end only by the supervisor
# on the real rig, per issue 930's own scope boundary (this worker's dispatch delivers the code +
# the pure-function tests, never the long paired-run evidence itself).
# --------------------------------------------------------------------------------------------- #

main() {
  local lipsync_recording="" qrqpsk_recording="" qrqpsk_marker_log="" verdict_bin=""
  local workdir=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --lipsync-recording) lipsync_recording="$2"; shift 2 ;;
      --qrqpsk-recording) qrqpsk_recording="$2"; shift 2 ;;
      --qrqpsk-marker-log) qrqpsk_marker_log="$2"; shift 2 ;;
      --verdict-bin) verdict_bin="$2"; shift 2 ;;
      --workdir) workdir="$2"; shift 2 ;;
      *) echo "usage: $0 --lipsync-recording <p> --qrqpsk-recording <p> --qrqpsk-marker-log <p> --verdict-bin <p> [--workdir <d>]" >&2; exit 2 ;;
    esac
  done
  [ -n "$lipsync_recording" ] || { echo "FAIL: --lipsync-recording is required" >&2; exit 2; }
  [ -n "$qrqpsk_recording" ] || { echo "FAIL: --qrqpsk-recording is required" >&2; exit 2; }
  [ -n "$qrqpsk_marker_log" ] || { echo "FAIL: --qrqpsk-marker-log is required" >&2; exit 2; }
  [ -n "$verdict_bin" ] || { echo "FAIL: --verdict-bin is required -- download the CI probe-tools-linux-amd64 artifact's recording-verdict binary (Tier 0: never build --features probe locally)" >&2; exit 2; }
  [ -f "$lipsync_recording" ] || { echo "FAIL: $lipsync_recording not found" >&2; exit 1; }
  [ -f "$qrqpsk_recording" ] || { echo "FAIL: $qrqpsk_recording not found" >&2; exit 1; }
  [ -f "$qrqpsk_marker_log" ] || { echo "FAIL: $qrqpsk_marker_log not found" >&2; exit 1; }
  [ -x "$verdict_bin" ] || { echo "FAIL: $verdict_bin not found/executable -- download the CI probe-tools-linux-amd64 artifact first" >&2; exit 1; }

  local made_workdir=false
  if [ -z "$workdir" ]; then
    workdir="$(mktemp -d)"
    made_workdir=true
  fi
  mkdir -p "$workdir"

  echo "[lipsync-cross-check] segmenting $lipsync_recording into ~20s chunks -> $workdir" >&2
  eval "$(lipsync_segment_cmd "$lipsync_recording" 20 "$workdir/chunk-%03d.mp4")" -loglevel error

  local calibration_log="$workdir/lipsync-calibration.jsonl"
  rm -f "$calibration_log"
  local py="${LIPSYNC_PYTHON:-python3}"
  local measure_script="${LIPSYNC_AV_SYNC_MEASURE:-$REPO_ROOT/scripts/av_sync_measure.py}"
  local calibrate_script="${LIPSYNC_AV_SYNC_CALIBRATE:-$REPO_ROOT/scripts/av_sync_calibrate.py}"
  for chunk in "$workdir"/chunk-*.mp4; do
    [ -f "$chunk" ] || continue
    echo "[lipsync-cross-check] measuring $chunk" >&2
    eval "$(lipsync_measure_chunk_cmd "$py" "$measure_script" "$chunk" "$calibration_log")" || true
  done

  local report_json="$workdir/lipsync-syncnet-agg.json"
  echo "[lipsync-cross-check] aggregating $calibration_log -> $report_json" >&2
  eval "$(lipsync_aggregate_cmd "$py" "$calibrate_script" "$calibration_log" "$report_json")"
  local syncnet_offset_ms
  syncnet_offset_ms="$(lipsync_mean_offset_from_report_json "$(cat "$report_json")")"
  echo "[lipsync-cross-check] SyncNet aggregated offset: ${syncnet_offset_ms}ms" >&2

  echo "[lipsync-cross-check] cross-checking against the paired QR/QPSK recording" >&2
  eval "$(lipsync_verdict_cmd "$verdict_bin" "$qrqpsk_recording" "$qrqpsk_marker_log" "$syncnet_offset_ms")"

  if [ "$made_workdir" = true ]; then
    rm -rf "$workdir"
  fi
}

# Run main only when EXECUTED, not when SOURCED (tests/harness_lipsync_cross_check.rs sources
# this file and calls the pure functions directly, never triggering a real orchestration run).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
