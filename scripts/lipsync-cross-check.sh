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
#     --asset-baseline-ms <the pinned asset's own intrinsic A/V offset, ms> \
#     [--workdir <scratch dir, default: a mktemp -d>]
#
# `--verdict-bin` is REQUIRED, with NO local-build default -- recording-verdict needs `--features
# probe` to even exist as a binary, and this repo's Local Build Policy forbids building that
# locally (Tier 0 -- CI builds it, download the probe-tools-linux-amd64 artifact). Mirrors
# recording-verdict-on-imag.sh's own --verdict-bin (no baked-in default there either).
#
# `--asset-baseline-ms` is REQUIRED (issue 930 supervisor decision, issuecomment-5153948268): the
# pinned lipsync asset itself measures an intrinsic -80ms A/V offset (SyncNet conf 8.0, two
# independent seek methods -- see assets/lipsync/PROVENANCE.md), baked into the SOURCE clip, not
# the rig. The raw aggregated SyncNet-on-rig-recording offset is `intrinsic_asset +
# rig_chain_delta`; this flag's value is subtracted BEFORE the verdict so the cross-check compares
# the RIG-ADDED delta against the QR/QPSK measurement, not a structurally-shifted total.
#
# Prints the final JSON (recording-verdict's own --av-sync + lipsync_cross_check output) to
# stdout and a one-line human summary to stderr.
#
# Env:
#   LIPSYNC_PYTHON              python3 interpreter override (default: python3)
#   LIPSYNC_AV_SYNC_MEASURE     av_sync_measure.py path override
#   LIPSYNC_AV_SYNC_CALIBRATE   av_sync_calibrate.py path override
#   LIPSYNC_SYNCNET_REPO        av_sync_measure.py --repo passthrough (issue 930 tooling finding,
#                                issuecomment-5179960868): points EVERY per-chunk SyncNet
#                                measurement at a non-default syncnet_python checkout (e.g. dev2's
#                                GPU checkout -- dev1's CPU-only wheels are impractical for this
#                                workload). Unset/empty (the default) leaves av_sync_measure.py's
#                                own default checkout path (REPO_ROOT/syncnet_python) in effect.

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

# lipsync_measure_chunk_cmd <python> <av_sync_measure.py> <chunk> <calibration_log> [<repo>] --
# one SyncNet measurement of a single ~20s chunk, appending to the shared calibration-log JSONL
# (issue 917's engine; the JSONL is what av_sync_calibrate.py --calibrate then aggregates).
#
# Optional 5th arg (issue 930 tooling finding, issuecomment-5179960868): av_sync_measure.py
# accepts `--repo DIR` (default REPO_ROOT/syncnet_python) but this call never passed it, so a
# non-default syncnet_python checkout (e.g. dev2's GPU checkout, used live because dev1's CPU-only
# wheels are impractical for this workload) could only be reached by placing/symlinking it at the
# hardcoded default path. When given, becomes a `--repo` flag; omitted/empty (today's default,
# byte-identical to before this knob existed) leaves av_sync_measure.py's own default in effect.
lipsync_measure_chunk_cmd() {
  local py="$1" script="$2" chunk="$3" calibration_log="$4" repo="${5:-}"
  local cmd
  cmd="$(printf '%s %s --media %q --calibration-log %q' \
    "$(printf '%q' "$py")" "$(printf '%q' "$script")" "$chunk" "$calibration_log")"
  if [ -n "$repo" ]; then
    cmd="$cmd $(printf -- '--repo %q' "$repo")"
  fi
  printf '%s' "$cmd"
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
# --syncnet-offset-ms wiring), producing the printed cross-check JSON. Uses the `=` form
# (`--syncnet-offset-ms=<value>`) rather than the space form so a NEGATIVE offset (video earlier
# than audio) still parses even against an older recording-verdict binary that predates
# `allow_negative_numbers` on this flag -- belt-and-braces alongside the clap-side fix.
lipsync_verdict_cmd() {
  local bin="$1" recording="$2" marker_log="$3" syncnet_offset_ms="$4"
  printf '%s --av-sync %q --av-marker-log %q --syncnet-offset-ms=%q' \
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

# lipsync_subtract_baseline <aggregated_ms> <baseline_ms> -- issue 930 supervisor decision
# (issuecomment-5153948268): the pinned lipsync asset itself measures an intrinsic -80ms A/V
# offset (SyncNet conf 8.0, two independent seek methods -- assets/lipsync/PROVENANCE.md), baked
# into the SOURCE clip rather than the rig. `aggregated_ms` (the raw SyncNet-on-rig-recording
# mean) equals `intrinsic_asset + rig_chain_delta`; this returns the RIG-ADDED delta alone, which
# is what the QR/QPSK cross-check verdict must compare against. Plain subtraction -- awk (already
# available everywhere bash is) rather than adding a bc/jq dependency for one arithmetic op.
lipsync_subtract_baseline() {
  local aggregated_ms="$1" baseline_ms="$2"
  awk -v a="$aggregated_ms" -v b="$baseline_ms" 'BEGIN { printf "%.10g\n", (a - b) }'
}

# lipsync_cadence_attribution -- issue 1174: read cam2's `dupe-preferring decimation` summary
# lines (from stdin -- the camera-box journal window captured over the lipsync recording, e.g.
# `journalctl -u camera-box --since ... | grep 'dupe-preferring decimation'`) and classify whether
# the emit path warped the lip-motion cadence during that window.
#
# WHY this is the decisive #1174 signal: SyncNet correlates lip motion against a clean, continuous
# audio track (direct cable -> Dante). The Aug-5 healthy baseline predates the ENTIRE dupe-
# decimation era (#889 onward), so it had ZERO of the motion-WARPING emit events. camera-box's
# `dupe_shed_summary` (src/dupe_decimation/gate.rs) already logs all seven per-window counters;
# this splits them by motion effect:
#   PRESERVING (uniform decimation / true-dup drop, smooth motion): dupe-victim shed, blind-pacing
#     shed.
#   WARPING (freeze/jump, added #1111/#1145/#1167): late-dupe copies emitted, boundaries retired,
#     depth-drained, fast-drained, starvation last-frame repeats.
# ANY nonzero warp count during the lipsync window is cadence damage absent from the Aug-5 baseline
# -> the emit path is a contributing cause. Verdict:
#   CADENCE-WARP     warp events > 0  (emit path warped the lip timeline; confirms suspect #1)
#   CADENCE-CLEAN    matched >=1 line, warp == 0  (emit path exonerated -> look at moire/exposure)
#   CADENCE-UNKNOWN  no summary lines matched  (never a false CLEAN -- no data, not "no warp")
# Grounded in code HISTORY, not a tuned magic threshold; the per-second warp rate is reported as
# magnitude context. awk-only (already a dependency, see lipsync_subtract_baseline above), no grep,
# so it ALWAYS exits 0 -- safe to call as a bare/`|| true` statement under the caller's `set -euo
# pipefail` (the #1133 report-only-helper discipline).
lipsync_cadence_attribution() {
  awk '
    /dupe-preferring decimation/ {
      matched++
      for (i = 2; i <= NF; i++) {
        if      ($i == "dupe-victim")   dupe_shed    += $(i-1) + 0
        else if ($i == "blind-pacing")  blind_shed   += $(i-1) + 0
        else if ($i == "late-dupe")     copies       += $(i-1) + 0
        else if ($i == "boundaries")    retired      += $(i-1) + 0
        else if ($i == "depth-drained") drained      += $(i-1) + 0
        else if ($i == "fast-drained")  fast_drained += $(i-1) + 0
        else if ($i == "starvation")    starvation   += $(i-1) + 0
        else if ($i == "last")          { w = $(i+1); gsub(/[^0-9]/, "", w); window += w + 0 }
      }
    }
    END {
      warp = copies + retired + drained + fast_drained + starvation
      preserving = dupe_shed + blind_shed
      if (matched == 0)   { verdict = "CADENCE-UNKNOWN"; wps = 0 }
      else if (warp == 0) { verdict = "CADENCE-CLEAN";   wps = 0 }
      else                { verdict = "CADENCE-WARP"; wps = (window > 0) ? warp / window : 0 }
      printf "lipsync_cadence: verdict=%s warp_events=%d warp_per_s=%.2f window_s=%d preserving_events=%d copies=%d retired=%d drained=%d fast_drained=%d starvation=%d dupe_shed=%d blind_shed=%d\n", \
             verdict, warp, wps, window, preserving, copies, retired, drained, fast_drained, starvation, dupe_shed, blind_shed
    }
  '
}

# --------------------------------------------------------------------------------------------- #
# Orchestration (real network/filesystem effects) -- exercised end-to-end only by the supervisor
# on the real rig, per issue 930's own scope boundary (this worker's dispatch delivers the code +
# the pure-function tests, never the long paired-run evidence itself).
# --------------------------------------------------------------------------------------------- #

main() {
  local lipsync_recording="" qrqpsk_recording="" qrqpsk_marker_log="" verdict_bin=""
  local asset_baseline_ms=""
  local workdir=""
  # issue 1174: optional cam2 emit-cadence journal (captured over the lipsync recording window) --
  # when given, the same single cross-check command ALSO prints the CADENCE-WARP/CLEAN/UNKNOWN
  # attribution, so the supervisor's one rig run decides suspect #1 (see lipsync_cadence_attribution).
  local lipsync_cadence_log=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --lipsync-recording) lipsync_recording="$2"; shift 2 ;;
      --qrqpsk-recording) qrqpsk_recording="$2"; shift 2 ;;
      --qrqpsk-marker-log) qrqpsk_marker_log="$2"; shift 2 ;;
      --verdict-bin) verdict_bin="$2"; shift 2 ;;
      --asset-baseline-ms) asset_baseline_ms="$2"; shift 2 ;;
      --workdir) workdir="$2"; shift 2 ;;
      --lipsync-cadence-log) lipsync_cadence_log="$2"; shift 2 ;;
      *) echo "usage: $0 --lipsync-recording <p> --qrqpsk-recording <p> --qrqpsk-marker-log <p> --verdict-bin <p> --asset-baseline-ms <ms> [--workdir <d>] [--lipsync-cadence-log <p>]" >&2; exit 2 ;;
    esac
  done
  [ -n "$lipsync_recording" ] || { echo "FAIL: --lipsync-recording is required" >&2; exit 2; }
  [ -n "$qrqpsk_recording" ] || { echo "FAIL: --qrqpsk-recording is required" >&2; exit 2; }
  [ -n "$qrqpsk_marker_log" ] || { echo "FAIL: --qrqpsk-marker-log is required" >&2; exit 2; }
  [ -n "$verdict_bin" ] || { echo "FAIL: --verdict-bin is required -- download the CI probe-tools-linux-amd64 artifact's recording-verdict binary (Tier 0: never build --features probe locally)" >&2; exit 2; }
  [ -n "$asset_baseline_ms" ] || { echo "FAIL: --asset-baseline-ms is required -- the pinned lipsync asset has an intrinsic A/V offset (see assets/lipsync/PROVENANCE.md's Baseline section) that must be subtracted before the verdict (930, issuecomment-5153948268)" >&2; exit 2; }

  # issue 1174: emit-cadence attribution (independent of the SyncNet pipeline). Placed AFTER the
  # required-flag checks but BEFORE the recording-file-existence checks, so it still prints if a
  # recording FILE is missing/unreadable below -- keeping it on the same single command. `|| true`
  # because it is report-only and must never abort the run (the #1133 report-only-helper discipline).
  if [ -n "$lipsync_cadence_log" ]; then
    if [ -r "$lipsync_cadence_log" ]; then
      echo "[lipsync-cross-check] cam2 emit-cadence attribution (issue 1174) from $lipsync_cadence_log:" >&2
      lipsync_cadence_attribution < "$lipsync_cadence_log" >&2 || true
    else
      echo "WARN: [lipsync #1174] --lipsync-cadence-log '$lipsync_cadence_log' not readable -- skipping emit-cadence attribution" >&2
    fi
  fi

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

  # Remove any STALE chunk-*.mp4 left behind by a previous run against the SAME --workdir (an
  # explicit, reused workdir is the only case this matters -- a fresh mktemp -d never has any).
  # Without this, a previous run that produced MORE chunks than this run's segmenting step
  # overwrites would leave leftover chunks in the glob below, silently measuring stale audio/video
  # alongside (or instead of) this run's real data.
  rm -f "$workdir"/chunk-*.mp4

  echo "[lipsync-cross-check] segmenting $lipsync_recording into ~20s chunks -> $workdir" >&2
  eval "$(lipsync_segment_cmd "$lipsync_recording" 20 "$workdir/chunk-%03d.mp4")" -loglevel error

  local calibration_log="$workdir/lipsync-calibration.jsonl"
  rm -f "$calibration_log"
  local py="${LIPSYNC_PYTHON:-python3}"
  local measure_script="${LIPSYNC_AV_SYNC_MEASURE:-$REPO_ROOT/scripts/av_sync_measure.py}"
  local calibrate_script="${LIPSYNC_AV_SYNC_CALIBRATE:-$REPO_ROOT/scripts/av_sync_calibrate.py}"
  # issue 930 tooling finding (issuecomment-5179960868): av_sync_measure.py's own --repo default
  # (REPO_ROOT/syncnet_python) is the only checkout it can otherwise reach. LIPSYNC_SYNCNET_REPO
  # lets a caller point every chunk measurement at a different checkout (e.g. dev2's GPU checkout);
  # empty (the default) omits --repo entirely, leaving av_sync_measure.py's own default in effect.
  local syncnet_repo="${LIPSYNC_SYNCNET_REPO:-}"
  local measured=0 attempted=0
  for chunk in "$workdir"/chunk-*.mp4; do
    [ -f "$chunk" ] || continue
    attempted=$((attempted + 1))
    echo "[lipsync-cross-check] measuring $chunk" >&2
    # av_sync_measure.py's own prose goes to stderr (1>&2) so it never lands on this script's
    # stdout -- the header contract is: ONLY the final recording-verdict JSON reaches stdout.
    if eval "$(lipsync_measure_chunk_cmd "$py" "$measure_script" "$chunk" "$calibration_log" "$syncnet_repo")" 1>&2; then
      measured=$((measured + 1))
    else
      echo "[lipsync-cross-check] WARN: SyncNet measurement failed on $chunk (continuing)" >&2
    fi
  done
  # src/lipsync_cross_check.rs's tolerance math assumes >=2 confident SyncNet windows went into
  # the aggregate -- silently proceeding on 0 or 1 measured chunks (the old `|| true` behavior)
  # would let a near-total measurement failure produce a bogus-precision cross-check verdict.
  if [ "$measured" -lt 2 ]; then
    echo "FAIL: only $measured/$attempted chunk(s) measured successfully -- need >=2 confident SyncNet windows for the aggregate tolerance math" >&2
    exit 1
  fi

  local report_json="$workdir/lipsync-syncnet-agg.json"
  echo "[lipsync-cross-check] aggregating $calibration_log -> $report_json" >&2
  eval "$(lipsync_aggregate_cmd "$py" "$calibrate_script" "$calibration_log" "$report_json")" 1>&2
  local syncnet_offset_ms rig_added_ms
  syncnet_offset_ms="$(lipsync_mean_offset_from_report_json "$(cat "$report_json")")"
  rig_added_ms="$(lipsync_subtract_baseline "$syncnet_offset_ms" "$asset_baseline_ms")"
  echo "[lipsync-cross-check] SyncNet aggregated offset (raw, includes asset baseline): ${syncnet_offset_ms}ms" >&2
  echo "[lipsync-cross-check] asset baseline: ${asset_baseline_ms}ms -> rig-added offset (baseline-corrected): ${rig_added_ms}ms" >&2

  echo "[lipsync-cross-check] cross-checking against the paired QR/QPSK recording" >&2
  eval "$(lipsync_verdict_cmd "$verdict_bin" "$qrqpsk_recording" "$qrqpsk_marker_log" "$rig_added_ms")"

  if [ "$made_workdir" = true ]; then
    rm -rf "$workdir"
  fi
}

# Run main only when EXECUTED, not when SOURCED (tests/harness_lipsync_cross_check.rs sources
# this file and calls the pure functions directly, never triggering a real orchestration run).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
