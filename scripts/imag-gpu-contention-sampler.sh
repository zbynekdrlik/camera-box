#!/usr/bin/env bash
set -euo pipefail
# #674 — one-shot, bounded-duration GPU sampler for imag-nb, used to test the "GPU/encode
# contention rising over a recording" hypothesis (the #674 addendum, 2026-07-12): imag is itself
# RECORDING (NVENC) during exactly the windows its own #588 optical-judder density rises, and
# #709 already proved this box's GPU is a resource that degrades under load (VRAM leak -> NVENC
# OOM). This script samples `nvidia-smi` (utilization, VRAM used, encoder session count) at a
# short, fixed cadence for a bounded duration, so a #674 gate-run dispatch can arm it BEFORE
# triggering ONE `full-path-e2e` ALL_CAMBOX run and correlate the samples OFFLINE against that
# same run's `all_cambox_continuity.imag.segments[].optical_stuck_density` per-window values.
#
# Ruled out ALREADY on #674 before this script was written (see the issue's own thread — don't
# re-litigate these): restart-alone trigger, #707 shared root cause (wrong correlation signature:
# imag's own judder is time-elapsed-correlated, not per-camera-correlated), NDI-reception FIFO
# starvation (a run with near-zero underruns still showed the full judder pattern). GPU/encode
# contention during imag's OWN recording is the next, not-yet-tested suspect.
#
# Usage: imag-gpu-contention-sampler.sh --duration-secs N [--interval-secs 1.5] [--out PATH]
# Writes one CSV row per sample: epoch_s,gpu_util_pct,mem_used_mib,encoder_sessions
# (epoch_s = `date +%s.%3N` at the START of that sample's nvidia-smi call — millisecond
# resolution is enough to line up against ~30s schedule windows; nvidia-smi's own --query-gpu
# call typically takes tens of ms, negligible against the 1-2s cadence).
#
# Fails LOUD (non-zero exit, no silent partial data) if nvidia-smi is not on PATH — a missing GPU
# tool must never produce an empty-but-"successful" sample file that looks like "zero contention".

DURATION_SECS="${IMAG_GPU_SAMPLE_DURATION_SECS:-400}"
INTERVAL_SECS="${IMAG_GPU_SAMPLE_INTERVAL_SECS:-1.5}"
OUT="${IMAG_GPU_SAMPLE_OUT:-$HOME/imag-gpu-contention-674.csv}"

while [ $# -gt 0 ]; do
  case "$1" in
    --duration-secs) DURATION_SECS="$2"; shift 2 ;;
    --interval-secs) INTERVAL_SECS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if ! command -v nvidia-smi >/dev/null 2>&1; then
  echo "FATAL #674: nvidia-smi not found on PATH -- cannot sample GPU state, refusing to write a" \
       "fake-empty/misleading sample file" >&2
  exit 1
fi

echo "epoch_s,gpu_util_pct,mem_used_mib,encoder_sessions" > "$OUT"

start_epoch="$(date +%s)"
end_epoch=$((start_epoch + DURATION_SECS))
n=0
while [ "$(date +%s)" -lt "$end_epoch" ]; do
  now="$(date +%s.%3N)"
  line="$(nvidia-smi --query-gpu=utilization.gpu,memory.used,encoder.stats.sessionCount \
    --format=csv,noheader,nounits 2>/dev/null || echo ",,")"
  echo "${now},${line}" >> "$OUT"
  n=$((n + 1))
  sleep "$INTERVAL_SECS"
done

echo "#674 GPU sampler: wrote $n samples over ~${DURATION_SECS}s to $OUT" >&2
