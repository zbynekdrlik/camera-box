#!/usr/bin/env bash
set -euo pipefail
# #674 — periodic genlock-FIFO audit delta reporter for imag-nb.
#
# Context: imag's #588 optical judder appeared once after a strih+stream OBS restart, but a
# targeted repro (restarting strih+stream in isolation while watching imag's own genlock-fifo
# audit deltas) found ZERO reproduction — the restart alone is not sufficient. Rather than chase
# another blind repro, this ships CONTINUOUS telemetry so the NEXT NATURAL occurrence is captured
# with real data: a periodic delta report of imag's own genlock-FIFO audit counters
# (received/underruns/holds/dropped_due/relocks/late_holds + head-skew jitter), journald-visible,
# so a future investigation can pull `journalctl -t imag-jitter-monitor --since ... --until ...`
# around a reported judder timestamp and see exactly what the FIFO was doing.
#
# Run periodically via the imag-jitter-monitor.timer systemd unit (every 5 minutes) — see
# systemd/imag-jitter-monitor.{service,timer}. Each run reads ONLY the OBS log content written
# since its last run (resumable byte-offset tracking, scripts/lib/imag-jitter-state.sh — safe
# across log rotation/replacement) and feeds that window to `genlock-jitter-report`
# (src/bin/genlock-jitter-report.rs, already exists — the #272 pure delta summarizer).
#
# Restart correlation: `scripts/mark-imag-restart.sh` (run from dev1, which has an SSH key on
# imag-nb, #541) writes a RESTART-MARKER line into the SAME `imag-jitter-monitor` journald stream
# right after a verified strih/stream OBS relaunch — so a future judder report's timestamp can be
# checked against both the periodic FIFO deltas AND any nearby restart marker in one
# `journalctl -t imag-jitter-monitor` view.
#
# Usage: imag-jitter-monitor.sh [--log <obs-log-path>] [--state <state-file>] \
#                                [--jitter-report-bin <path>]
# Env overrides (same names, lower priority than the matching flag):
#   IMAG_JITTER_OBS_LOG, IMAG_JITTER_STATE_FILE, IMAG_JITTER_REPORT_BIN

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/imag-jitter-state.sh
. "$HERE/lib/imag-jitter-state.sh"

OBS_LOG="${IMAG_JITTER_OBS_LOG:-}"
STATE_FILE="${IMAG_JITTER_STATE_FILE:-$HOME/.cache/imag-jitter-monitor.state}"
JITTER_BIN="${IMAG_JITTER_REPORT_BIN:-$HOME/genlock-jitter-report}"

usage() {
  cat <<'USAGE'
Usage: imag-jitter-monitor.sh [--log <obs-log-path>] [--state <state-file>] \
                               [--jitter-report-bin <path>]
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --log)
      OBS_LOG="$2"
      shift 2
      ;;
    --state)
      STATE_FILE="$2"
      shift 2
      ;;
    --jitter-report-bin)
      JITTER_BIN="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ -z "$OBS_LOG" ]; then
  # Auto-discover the CURRENT OBS log (imag's own OBS is rarely restarted, per the genlock skill —
  # the newest file under the logs dir is always the live one).
  OBS_LOG="$(ls -t "$HOME/.config/obs-studio/logs/"*.txt 2>/dev/null | head -1 || true)"
fi
if [ -z "$OBS_LOG" ] || [ ! -f "$OBS_LOG" ]; then
  echo "ERROR: no OBS log found (checked --log / IMAG_JITTER_OBS_LOG / ~/.config/obs-studio/logs/*.txt)" >&2
  exit 1
fi
if [ ! -x "$JITTER_BIN" ]; then
  echo "ERROR: genlock-jitter-report binary not found/executable at $JITTER_BIN" >&2
  exit 1
fi

CURRENT_SIZE="$(stat -c %s "$OBS_LOG")"
STORED_OFFSET="0"
[ -f "$STATE_FILE" ] && STORED_OFFSET="$(cat "$STATE_FILE" 2>/dev/null || echo 0)"
OFFSET="$(imag_jitter_next_offset "$STORED_OFFSET" "$CURRENT_SIZE")"

echo "imag-jitter-monitor: log=$OBS_LOG window=[$OFFSET..$CURRENT_SIZE) bytes"
if [ "$OFFSET" -ge "$CURRENT_SIZE" ]; then
  echo "imag-jitter-monitor: no new log content since last check"
else
  TMP_WINDOW="$(mktemp)"
  trap 'rm -f "$TMP_WINDOW"' EXIT
  tail -c "+$((OFFSET + 1))" "$OBS_LOG" > "$TMP_WINDOW"
  JITTER_RC=0
  "$JITTER_BIN" --file "$TMP_WINDOW" || JITTER_RC=$?
  if [ "$JITTER_RC" -ne 0 ]; then
    echo "imag-jitter-monitor: no genlock-fifo audit lines in this window (rc=$JITTER_RC, harmless if OBS was quiet)"
  fi
fi

mkdir -p "$(dirname "$STATE_FILE")"
echo "$CURRENT_SIZE" > "$STATE_FILE"
