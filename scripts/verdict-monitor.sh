#!/usr/bin/env bash
# verdict-monitor.sh (#166) — LIVENESS/TIMEOUT guard for a long-running verdict.
#
# The recording-verdict process can decode multi-GB recordings for minutes. If it
# CRASHES (the #166 night: it died silently after >1h of single-threaded decode)
# or HANGS, a naive monitor that only waits for a completion marker would wait
# FOREVER — that crashed process never writes the marker, so the whole e2e run
# hung all night with no result.
#
# This monitor detects BOTH failure modes and FAILS LOUDLY instead of hanging:
#
#   1. DEAD  — the verdict PID is gone but no exit marker was written (it crashed
#              / was OOM-killed / segfaulted before recording its exit code).
#   2. STALL — the verdict is alive but its output file has not grown for
#              `--stall-timeout` seconds (it is wedged / deadlocked, not working).
#
# On either, the monitor prints a clear diagnostic (tail of the output) and exits
# non-zero. On clean completion it returns the verdict's own exit code (read from
# the exit marker the wrapper writes).
#
# This script ONLY monitors; the caller runs the verdict in the background and
# writes the exit marker. See `run_verdict_guarded` in recording-e2e.sh for the
# canonical wrapper, and the unit tests in tests/verdict_monitor.sh.
#
# Usage:
#   verdict-monitor.sh --pid <PID> --output <FILE> --exit-marker <FILE> \
#       [--stall-timeout <SECS>] [--poll <SECS>] [--label <NAME>]
#
# Exit code: the verdict's exit code on clean completion; 124 on STALL; 137-style
# non-zero (CODE_DEAD=126) on a silent death; 2 on bad args.
set -euo pipefail

CODE_STALL=124   # mirrors GNU timeout's "timed out" convention
CODE_DEAD=126    # process died without writing its exit marker
CODE_ARGS=2

PID=""
OUTPUT=""
EXIT_MARKER=""
STALL_TIMEOUT="${VERDICT_STALL_TIMEOUT:-300}"   # default 5 min of no progress = stalled
POLL="${VERDICT_POLL:-5}"
LABEL="verdict"

while [ $# -gt 0 ]; do
  case "$1" in
    --pid)          PID="$2"; shift 2;;
    --output)       OUTPUT="$2"; shift 2;;
    --exit-marker)  EXIT_MARKER="$2"; shift 2;;
    --stall-timeout) STALL_TIMEOUT="$2"; shift 2;;
    --poll)         POLL="$2"; shift 2;;
    --label)        LABEL="$2"; shift 2;;
    *) echo "verdict-monitor: unknown arg: $1" >&2; exit "$CODE_ARGS";;
  esac
done

if [ -z "$PID" ] || [ -z "$OUTPUT" ] || [ -z "$EXIT_MARKER" ]; then
  echo "verdict-monitor: --pid, --output and --exit-marker are required" >&2
  exit "$CODE_ARGS"
fi

# Current size (bytes) of the output file, or 0 if it does not exist yet.
out_size() {
  if [ -f "$OUTPUT" ]; then
    wc -c < "$OUTPUT" 2>/dev/null | tr -d ' '
  else
    echo 0
  fi
}
# Is the monitored process still alive?
alive() { kill -0 "$PID" 2>/dev/null; }
# Dump a diagnostic tail of the output so the failure is debuggable from the log.
diag_tail() {
  echo "    --- last 25 lines of $LABEL output ($OUTPUT) ---" >&2
  [ -f "$OUTPUT" ] && tail -n 25 "$OUTPUT" >&2 || echo "    (no output file)" >&2
  echo "    ------------------------------------------------" >&2
}

echo "[$LABEL-monitor] watching pid=$PID output=$OUTPUT stall-timeout=${STALL_TIMEOUT}s poll=${POLL}s"
last_size="$(out_size)"
last_progress_ts="$(date +%s)"

while true; do
  # 1. Clean completion: the wrapper wrote the exit marker. Trust it over the PID
  #    check (the PID may already be reaped). Read and return the verdict's code.
  if [ -f "$EXIT_MARKER" ]; then
    code="$(tr -dc '0-9' < "$EXIT_MARKER" 2>/dev/null)"
    code="${code:-0}"
    echo "[$LABEL-monitor] $LABEL finished cleanly (exit marker code=$code)"
    exit "$code"
  fi

  # 2. DEAD: the process is gone but left NO exit marker → it crashed silently.
  #    This is the exact #166 failure that must FAIL FAST, never hang.
  if ! alive; then
    # Grace: the wrapper writes the marker just after the process exits; give it a
    # couple of poll cycles to appear before declaring a silent death.
    for _ in 1 2 3; do
      sleep 1
      if [ -f "$EXIT_MARKER" ]; then
        code="$(tr -dc '0-9' < "$EXIT_MARKER" 2>/dev/null)"; code="${code:-0}"
        echo "[$LABEL-monitor] $LABEL finished (exit marker code=$code, post-death)"
        exit "$code"
      fi
    done
    echo "ERROR: $LABEL process (pid=$PID) DIED with NO exit marker — it crashed/was killed" \
         "before recording its result. FAILING the run instead of waiting forever (#166)." >&2
    diag_tail
    exit "$CODE_DEAD"
  fi

  # 3. STALL: alive but no output growth for >= stall-timeout → wedged, not working.
  now="$(date +%s)"
  size="$(out_size)"
  if [ "$size" != "$last_size" ]; then
    last_size="$size"
    last_progress_ts="$now"
  else
    idle=$(( now - last_progress_ts ))
    if [ "$idle" -ge "$STALL_TIMEOUT" ]; then
      echo "ERROR: $LABEL process (pid=$PID) STALLED — no output progress for ${idle}s" \
           "(>= ${STALL_TIMEOUT}s). It is wedged, not working. FAILING the run (#166)." >&2
      diag_tail
      kill "$PID" 2>/dev/null || true
      exit "$CODE_STALL"
    fi
  fi

  sleep "$POLL"
done
