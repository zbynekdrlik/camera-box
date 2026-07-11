#!/usr/bin/env bash
set -euo pipefail
# #674 — write a RESTART-MARKER line into imag-nb's own journald, tagged with the SAME
# `imag-jitter-monitor` syslog identifier the periodic FIFO-delta reporter uses
# (scripts/imag-jitter-monitor.sh + systemd/imag-jitter-monitor.{service,timer}), so a future
# `journalctl -t imag-jitter-monitor` view around a reported judder timestamp shows BOTH the
# periodic delta reports AND any nearby upstream strih/stream OBS restart in one place.
#
# imag-nb pulls its camera feed directly over NDI (never through strih/stream), so a strih/stream
# restart is not directly observable from imag's own OBS log — this script is the explicit,
# deterministic alternative: run it from dev1 (which has an SSH key on imag-nb, #541) right after
# verifying a `scripts/launch-obs-genlock.sh --box {strih|stream} --force` relaunch succeeded (see
# that script's own printed plan, which references this as its final step).
#
# Usage: mark-imag-restart.sh --box strih|stream [--reason <text>]

BOX=""
REASON="manual relaunch"

usage() {
  echo "Usage: $0 --box strih|stream [--reason <text>]"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --box)
      BOX="$2"
      shift 2
      ;;
    --reason)
      REASON="$2"
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

case "$BOX" in
  strih | stream) ;;
  *)
    echo "ERROR: --box must be 'strih' or 'stream' (got '${BOX}')" >&2
    usage >&2
    exit 2
    ;;
esac

IMAG_HOST="${IMAG_HOST:-newlevel@10.77.9.182}"
TS="$(date -u +%FT%TZ)"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$IMAG_HOST" \
  "logger -t imag-jitter-monitor \"RESTART-MARKER box=${BOX} reason='${REASON}' ts=${TS}\""
echo "marked: box=${BOX} reason='${REASON}' ts=${TS} on ${IMAG_HOST} (journalctl -t imag-jitter-monitor)"
