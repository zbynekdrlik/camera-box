#!/usr/bin/env bash
# #802 SRT-tap bench-validation harness -- see the extended header below set -euo pipefail.
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# #802 -- bench-validate the A/V-sync SRT tap BEFORE it ever returns to a prod OBS box.
#
# On 2026-07-19 (LIVE) starting an Aitum Multistream SRT-CALLER output against an unreachable
# listener crashed the whole OBS process (failed mpegts-output start cleanup in vendored
# obs-ffmpeg-mpegts.c). The redesign (`scripts/srt_tap.py`): the OBS-side tap output is an SRT
# LISTENER (`srt://0.0.0.0:9998?mode=listener`) -- a listener bind succeeds immediately with NO
# peer, so the failing-start crash trigger cannot occur; the player (VLC/ffmpeg, caller-capable)
# connects on demand as the CALLER.
#
# This harness runs the AUTOMATABLE safety checks that prove the redesign holds, so the monitor
# flow can be validated on a BENCH OBS (never mid-broadcast on a live box again):
#   1. the launch-path guard REFUSES a crash-prone caller URL (the exact 2026-07-19 shape),
#   2. the guard ACCEPTS a listener URL,
#   3. the recommender emits a canonical listener URL,
#   4. (where local ffmpeg has libsrt) a real SRT-LISTENER mpegts output STARTS AND STAYS ALIVE
#      with NO player connected -- the crux: a missing player can never fail the start,
#   5. the srt_tap + av_sync_measure preflight pytest suites pass (if pytest is present).
#
# Usage: scripts/av-sync-tap-bench.sh [PORT]   (default 19998 -- a dedicated bench port, so it
# never collides with a real 9998 tap on the bench box). Exits non-zero on any FAIL.
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${1:-19998}"
SRT_TAP="${HERE}/srt_tap.py"

fails=0
pass() { printf '  PASS: %s\n' "$1"; }
fail() { printf '  FAIL: %s\n' "$1"; fails=$((fails + 1)); }
skip() { printf '  SKIP: %s\n' "$1"; }

echo "== #802 SRT-tap bench validation (port ${PORT}) =="

# --- Check 1: the guard REFUSES a crash-prone caller URL (exit 2) -------------------------------
echo "[1] launch-path guard refuses a caller tap (the crash shape)"
rc=0
python3 "${SRT_TAP}" --check "srt://127.0.0.1:${PORT}" >/dev/null 2>&1 || rc=$?
if [ "${rc}" -eq 2 ]; then
  pass "caller URL refused (exit 2)"
else
  fail "caller URL not refused (exit ${rc}, expected 2)"
fi

# --- Check 2: the guard ACCEPTS a listener URL (exit 0) -----------------------------------------
echo "[2] launch-path guard accepts a listener tap"
rc=0
python3 "${SRT_TAP}" --check "srt://0.0.0.0:${PORT}?mode=listener" >/dev/null 2>&1 || rc=$?
if [ "${rc}" -eq 0 ]; then
  pass "listener URL accepted (exit 0)"
else
  fail "listener URL not accepted (exit ${rc}, expected 0)"
fi

# --- Check 3: the recommender emits a canonical listener URL ------------------------------------
echo "[3] recommender emits a canonical listener URL"
rec="$(python3 "${SRT_TAP}" --recommend "${PORT}")"
if grep -q 'mode=listener' <<<"${rec}" && grep -q ":${PORT}" <<<"${rec}"; then
  pass "recommended: ${rec}"
else
  fail "recommendation not a listener URL for port ${PORT}: ${rec}"
fi

# --- Check 4: a real SRT-LISTENER mpegts output starts + stays alive with NO player -------------
echo "[4] real ffmpeg SRT-listener output starts + survives with no player"
if ! command -v ffmpeg >/dev/null 2>&1; then
  skip "ffmpeg not on PATH"
elif ! ffmpeg -hide_banner -protocols 2>/dev/null | grep -qw srt; then
  skip "this ffmpeg has no libsrt (run this check on the bench OBS box)"
else
  errlog="$(mktemp)"
  ffmpeg -hide_banner -loglevel error -re \
    -f lavfi -i "testsrc=size=320x240:rate=30" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -f mpegts "srt://0.0.0.0:${PORT}?mode=listener" >/dev/null 2>"${errlog}" &
  ff_pid=$!
  sleep 2
  if kill -0 "${ff_pid}" 2>/dev/null; then
    pass "listener output alive after 2s with no player -- start cannot fail on a missing peer"
    kill "${ff_pid}" 2>/dev/null || true
    wait "${ff_pid}" 2>/dev/null || true
  else
    fail "listener output died with no player connected -- $(tr '\n' ' ' <"${errlog}" | tail -c 200)"
  fi
  rm -f "${errlog}"
fi

# --- Check 5: the pytest suites for the tap module + reader preflight ---------------------------
echo "[5] srt_tap + av_sync_measure preflight pytest suites"
if command -v pytest >/dev/null 2>&1 || python3 -c 'import pytest' >/dev/null 2>&1; then
  if python3 -m pytest -q \
       "${HERE}/../tests/python/test_srt_tap.py" \
       "${HERE}/../tests/python/test_av_sync_measure_tap_preflight.py" >/dev/null 2>&1; then
    pass "pytest suites green"
  else
    fail "pytest suites failed (run them directly for detail)"
  fi
else
  skip "pytest not installed"
fi

echo "== done: ${fails} failure(s) =="
[ "${fails}" -eq 0 ]
