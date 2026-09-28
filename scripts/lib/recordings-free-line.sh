#!/usr/bin/env bash
# airuleset:script-ok source-only lib (one helper, no top-level statements) -- deliberately NOT
# `set -euo pipefail`: sourcing this into a caller must never change the caller's shell options.
#
# scripts/lib/recordings-free-line.sh -- issue 1386: the recordings-volume free-space line of the E2E
# harness, read through the ONE reader bundle_state_gather.recordings_free_line (the same reader the
# 8 h soak's av_soak_free_space_verdict uses) instead of an inline python copy in recording-e2e.sh.
#
# recordings_free_line_from_stats <stats_json_text> <min_free_gb> <scripts_dir>
#   Prints the reader's one "<VERDICT> <free_gb>" line for a box's /record-dir-stats.json body:
#   "WARN <gb>" when the volume's free_bytes is strictly below <min_free_gb> decimal GB, "OK <gb>"
#   otherwise, "UNKNOWN -1" when free_bytes is null/non-numeric or the body is not a JSON object
#   (never a false WARN). Exit status = python's: non-zero only when python itself (or a non-numeric
#   <min_free_gb>) fails, so the caller keeps its own "could not parse" skip for that case.
#   <scripts_dir> is the directory holding bundle_state_gather.py (the caller's $HERE).
recordings_free_line_from_stats() {
  local stats="$1" min_gb="$2" here="$3"
  printf '%s' "$stats" | PYTHONPATH="$here" python3 -c \
    'import sys, bundle_state_gather as b; print(b.recordings_free_line(sys.stdin.read(), float(sys.argv[1])))' \
    "$min_gb" 2>/dev/null
}
