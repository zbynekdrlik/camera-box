#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library — sourced by scripts/imag-jitter-monitor.sh
# (#674). Mirrors every other scripts/lib/*.sh sibling (e.g. log-bound.sh): no `set -euo pipefail`
# here, since sourcing a `set -e`-carrying file would silently change the CALLER's shell options.
#
# #674 — imag-nb periodic genlock-FIFO audit delta reporter. A prior worker's live restart-only
# repro attempt found NO reproduction of the imag judder (see the #674 issue history) — the agreed
# next step is telemetry: capture FIFO/underrun deltas continuously so the NEXT NATURAL occurrence
# is caught with data, instead of chasing another blind repro. This file holds the pure byte-offset
# bookkeeping for the "read only the NEW log content since last check" resumable-tail logic, so it
# is Tier-0 unit-testable without touching imag-nb.

# imag_jitter_next_offset STORED_OFFSET CURRENT_SIZE -> the byte offset to resume reading FROM.
# Defends against a rotated/truncated/replaced OBS log: if the stored offset is not a plain
# non-negative integer, OR it now exceeds the file's current size (the file shrank — rotation or a
# fresh OBS process start), resume from 0 (read the WHOLE current file this one run) rather than
# either erroring or silently reading nothing.
imag_jitter_next_offset() {
  local stored="$1" current_size="$2"
  case "$stored" in
    '' | *[!0-9]*)
      echo 0
      return
      ;;
  esac
  if [ "$stored" -gt "$current_size" ]; then
    echo 0
  else
    echo "$stored"
  fi
}
