#!/usr/bin/env bash
# scripts/lib/painter-csv-freshness.sh — the PURE, testable heart of the #359 painter-CSV
# freshness gate.
#
# WHY (#359): recording-e2e.sh silently pulled a STALE cam2 painter ground-truth CSV (run 354002:
# 14.9h gen_ts offset, 40s span) and ran recording-verdict against it → a fake catastrophic FAIL
# that was a measurement artifact, not real frame loss. The harness must FAIL LOUD instead of
# trusting stale ground truth. Extracting the verdict logic here (no I/O beyond reading the CSV,
# no ssh, no OBS) makes the actual span/offset thresholds unit-testable — recording-e2e.sh sources
# this and prints the FATAL message + exits non-zero on a non-OK verdict.
#
# Source-only: defines painter_csv_freshness() and runs nothing on its own.
#
# painter_csv_freshness <csv_path> <run_start_epoch_s> <duration_s>
#   The CSV is the frame-probe paint log: header `tick,gen_ts_ns,flip_ts_ns`, then one data row
#   per painted tick; gen_ts_ns (column 2) is CLOCK_REALTIME epoch ns (the painter runs --wall-clock).
#   Echoes ONE line: "<verdict> <span_s> <offset_s>", verdict ∈
#     MISSING — file absent or empty
#     EMPTY   — header only / fewer than 2 data rows (the painter painted ~nothing)
#     SHORT   — painted span < duration/2 (a truncated/stale tiny file, e.g. the 40s leftover)
#     OFFSET  — |first gen_ts − run_start| > duration + 600s (ground truth is hours off, not this run)
#     OK      — fresh: span ≈ duration AND gen_ts overlaps this run's wall clock
#   Returns 0 iff verdict == OK, else 1 — so a caller may `if painter_csv_freshness ...; then`.
painter_csv_freshness() {
  local csv="$1" run_start="$2" dur="$3" out
  if [ ! -s "$csv" ]; then
    echo "MISSING 0 0"
    return 1
  fi
  out=$(awk -F, -v run_start="$run_start" -v dur="$dur" '
    NR==1 { next }                                  # skip the "tick,gen_ts_ns,flip_ts_ns" header
    $2 ~ /^[0-9]+$/ { if (first=="") first=$2; last=$2; n++ }
    END {
      if (n < 2) { print "EMPTY 0 0"; exit }
      span   = (last - first) / 1e9                 # seconds spanned by the painted ticks
      offset = (first / 1e9) - run_start; if (offset < 0) offset = -offset
      verdict = (span < dur * 0.5) ? "SHORT" : ((offset > dur + 600) ? "OFFSET" : "OK")
      printf "%s %d %d\n", verdict, span, offset
    }' "$csv")
  echo "$out"
  case "$out" in
    OK\ *) return 0 ;;
    *) return 1 ;;
  esac
}
