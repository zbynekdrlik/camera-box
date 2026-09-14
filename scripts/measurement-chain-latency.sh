#!/usr/bin/env bash
# scripts/measurement-chain-latency.sh -- mbc measurement-chain latency probe (extended header below).
set -euo pipefail
#
# #1312: the standalone, READ-ONLY orchestrator for the "development" handover check's 14th item
# (avlatency: meracia zvukova cesta -- latencia oproti baseline). It proves, between productions and
# WITHOUT an E2E run, that the mbc measurement-audio chain (cam2 painter QPSK marker -> HDMI speaker ->
# measurement mic -> mbc Ableton -> Dante -> stream OBS `mbc`) is still ALIGNED with the video within
# the E2E's +/-90 ms gate -- NOT merely audible (that is #1310's job).
#
# It is thin I/O: (1) sample the stream `mbc` peak over ~30 s via the OBS-WS InputVolumeMeters event
# (scripts/measurement_chain_latency_probe.py) -> timestamped burst-onset samples on dev1's wall clock;
# (2) read the cam2 painter marker log (/run/rig-qpsk-markers.csv, index,frame_id,emit_ts_ns) over ssh
# -- AFTER the meter sample so the just-emitted markers are present; (3) hand both to the PURE kernel
# scripts/measurement_chain_latency.py, which detects onsets at the SAME -60 dB bar, pairs each onset to
# its emit, takes the median latency and compares it to a persisted baseline. NO recording, NO disk, NO
# rig mutation. The -60 dB bar is single-sourced from scripts/lib/audio-presence-preflight.sh (never
# retyped). All decision logic + all tests live in the pure kernel (pytest Tier-0, #557).
#
# WALL-CLOCK NOTE (the #1312 STEP-0 finding): the permanent cam2 painter emits monotonic-since-start
# emit_ts (setup-device.sh's unit has no --wall-clock), NOT the DanteSync wall clock. The pure kernel
# GUARDS on that -- a monotonic emit_ts is never paired and reads UNKNOWN (reason monotonic-emit),
# never a false drift. The item goes green-capable once the painter is switched to --wall-clock
# (a SAFE no-op for the A/V verdict path -- a supervisor follow-up).
#
# Usage:
#   scripts/measurement-chain-latency.sh              # measure, print key=value + a log line
#   scripts/measurement-chain-latency.sh --baseline   # ALSO persist the measured latency as baseline
#                                                      # (supervisor, right after a green E2E)
#   scripts/measurement-chain-latency.sh --help
#
# Env:
#   OBS_PASSWORD                 stream OBS-WS password (default empty; LAN no-auth).
#   STREAM_HOST / CAM2_HOST      box addresses (defaults below).
#   CAM_PW                       cam2 ssh password (default newlevel).
#   MC_BASELINE_FILE             baseline JSON path (default ~/.camera-box/measurement-chain-latency-baseline.json).
#   MC_TOLERANCE_MS / MC_MAX_PAIR_MS / MC_MIN_PAIRED / MC_SAMPLE_S / MC_MARKER_TAIL_N   knobs.
#   MC_SSH_TIMEOUT / MC_METER_TIMEOUT   per-step bounds.
#   MC_MARKER_CSV_FILE           Tier-0 seam: cat this file instead of ssh-reading the marker log.
#   MC_METER_FILE                Tier-0 seam: cat this file instead of running the WS meter sampler.
#   MC_BOX_REACHABLE             Tier-0 seam: force box-reachable (default 1 with MC_METER_FILE).
#   MC_MARKER_CSV_CMD / MC_METER_CMD   override the whole marker-read / meter-sample command.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BASELINE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --baseline) BASELINE=1 ;;
    --help | -h)
      sed -n '5,49p' "${BASH_SOURCE[0]}"  # description + Usage + Env (skip shebang/summary/set line)
      exit 0
      ;;
    *)
      echo "measurement-chain-latency: unknown arg '$1' (try --help)" >&2
      exit 2
      ;;
  esac
  shift
done

STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
CAM2_HOST="${CAM2_HOST:-10.77.9.62}"
CAM_PW="${CAM_PW:-newlevel}"
export OBS_PASSWORD="${OBS_PASSWORD:-}"
export MEASUREMENT_AUDIO_WS_PASSWORD="${OBS_PASSWORD:-}"

BASELINE_FILE="${MC_BASELINE_FILE:-$HOME/.camera-box/measurement-chain-latency-baseline.json}"
TOLERANCE_MS="${MC_TOLERANCE_MS:-90}"
MAX_PAIR_MS="${MC_MAX_PAIR_MS:-2000}"
MIN_PAIRED="${MC_MIN_PAIRED:-3}"
SAMPLE_S="${MC_SAMPLE_S:-32}"
MARKER_TAIL_N="${MC_MARKER_TAIL_N:-400}"
SSH_TIMEOUT="${MC_SSH_TIMEOUT:-20}"
METER_TIMEOUT="${MC_METER_TIMEOUT:-60}"
MARKER_LOG="${MC_MARKER_LOG:-/run/rig-qpsk-markers.csv}"

DECIDE="${MC_DECIDE:-$HERE/measurement_chain_latency.py}"
METER_PROBE="${MC_METER_PROBE:-$HERE/measurement_chain_latency_probe.py}"
PREFLIGHT_LIB="${MC_PREFLIGHT_LIB:-$HERE/lib/audio-presence-preflight.sh}"

# --- the -60 dB onset bar, single-sourced (never retyped) ----------------------------------------
THRESH="-60"
if [ -e "$PREFLIGHT_LIB" ]; then
  # shellcheck source=scripts/lib/audio-presence-preflight.sh
  . "$PREFLIGHT_LIB"
  THRESH="$(audio_preflight_default_threshold_db)"
fi

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/measurement-chain-latency.XXXXXX")"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT
MARKER_FILE="$WORKDIR/markers.txt"
METER_FILE="$WORKDIR/meter.txt"
: >"$MARKER_FILE"
: >"$METER_FILE"

# --- 1) meter samples (~30 s of mbc peak) + box-reachable ---------------------------------------
BOX_REACHABLE=1
if [ -n "${MC_METER_FILE:-}" ]; then
  cat "$MC_METER_FILE" >"$METER_FILE" 2>/dev/null || : >"$METER_FILE"
  BOX_REACHABLE="${MC_BOX_REACHABLE:-1}"
elif [ -n "${MC_METER_CMD:-}" ]; then
  if eval "$MC_METER_CMD" >"$METER_FILE" 2>/dev/null; then BOX_REACHABLE=1; else BOX_REACHABLE=0; fi
  BOX_REACHABLE="${MC_BOX_REACHABLE:-$BOX_REACHABLE}"
else
  if timeout "$METER_TIMEOUT" python3 "$METER_PROBE" "$STREAM_HOST" --sample-s "$SAMPLE_S" \
       >"$METER_FILE" 2>/dev/null; then BOX_REACHABLE=1; else BOX_REACHABLE=0; fi
fi

# --- 2) cam2 marker log (read AFTER the meter sample so the just-emitted rows are present) --------
if [ -n "${MC_MARKER_CSV_FILE:-}" ]; then
  cat "$MC_MARKER_CSV_FILE" >"$MARKER_FILE" 2>/dev/null || : >"$MARKER_FILE"
elif [ -n "${MC_MARKER_CSV_CMD:-}" ]; then
  eval "$MC_MARKER_CSV_CMD" >"$MARKER_FILE" 2>/dev/null || : >"$MARKER_FILE"
else
  sshpass -p "$CAM_PW" timeout "$SSH_TIMEOUT" ssh \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o BatchMode=no \
    "root@$CAM2_HOST" "tail -n $MARKER_TAIL_N $MARKER_LOG" \
    >"$MARKER_FILE" 2>/dev/null || : >"$MARKER_FILE"
fi

# --- 3) decide (the PURE kernel; --baseline persists the measured latency) ------------------------
WRITE_FLAG=()
[ "$BASELINE" -eq 1 ] && WRITE_FLAG=(--write-baseline)
OUT="$(python3 "$DECIDE" classify \
  --marker-file "$MARKER_FILE" --meter-file "$METER_FILE" \
  --box-reachable "$BOX_REACHABLE" --threshold-db "$THRESH" \
  --max-pair-ms "$MAX_PAIR_MS" --baseline-file "$BASELINE_FILE" \
  --tolerance-ms "$TOLERANCE_MS" --min-paired "$MIN_PAIRED" "${WRITE_FLAG[@]}")"

# the key=value block (carries the single `verdict=` token the handover check parses) -> stdout
printf '%s\n' "$OUT"

# a human log line -> stderr (uses `result=`, NOT `verdict=`, so exactly ONE verdict token is captured)
VTOKEN="$(sed -n 's/^verdict=//p' <<<"$OUT")"
LAT="$(sed -n 's/^latency_ms=//p' <<<"$OUT")"
BASE="$(sed -n 's/^baseline_ms=//p' <<<"$OUT")"
PAIRED="$(sed -n 's/^paired=//p' <<<"$OUT")"
REASONV="$(sed -n 's/^reason=//p' <<<"$OUT")"
printf '%s [measurement-chain-latency] stream (%s): reachable=%s result=%s latency_ms=%s baseline_ms=%s paired=%s reason=%s\n' \
  "$(date -u +%FT%TZ)" "$STREAM_HOST" "$BOX_REACHABLE" "$VTOKEN" "$LAT" "$BASE" "$PAIRED" "$REASONV" >&2
