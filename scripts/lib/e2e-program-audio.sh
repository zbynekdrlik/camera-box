#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, sets no shell options: sourcing runs it
# in the CALLER's shell -- recording-e2e.sh under `set -euo pipefail` -- which owns the strict mode).
#
# scripts/lib/e2e-program-audio.sh -- issue 1421 (owner 13.10.2026: CG/band songs reached the stream
# program next to the measurement signal, and no E2E term refused it): the full-path E2E reads the
# stream program-audio GUARD (scripts/program_audio_guard.py, the exit-code contract over the
# strih-snv sampler's :8891 verdict, issue 1404) before it touches the rig and every few seconds
# while it records. Anything but exit 0 (FOREIGN = non-measurement audio on the program, UNKNOWN =
# the program cannot be proven clean) fails the run, prints ONE operator request
# (E2E_PROGRAM_AUDIO_OPERATOR_MSG, the same text restreamer shows) and names every audible stream
# OBS input (scripts/stream_audio_inventory.py, read-only).
#
#   e2e_program_audio_preflight HERE STREAM_HOST           [0/8], rc 0 clean / 1 refused
#   e2e_program_audio_watch_start HERE STATE_DIR           right after StartRecord: background poll
#   e2e_program_audio_watch_stop HERE STREAM_HOST STATE_DIR  before StopRecord: rc 0 clean / 1 refused
#
# The poll interval (E2E_PROGRAM_AUDIO_POLL_S, 10 s) stays inside the guard's 30 s FOREIGN latch, so
# no FOREIGN window between two polls is missed. The loop is bounded and ends by itself once the
# E2E process is gone (an abort never leaves it polling). Test seams: E2E_PROGRAM_AUDIO_GUARD_CMD,
# E2E_PROGRAM_AUDIO_INVENTORY_CMD.

# The one operator request (Slovak, the rig operator's language). restreamer reuses this exact text.
E2E_PROGRAM_AUDIO_OPERATOR_MSG="ZASTAVENÉ: do stream programu (mbc / stream OBS) ide okrem meracieho signálu aj iný zvuk (hudba, CG, kapela). Stlm v mbc a v stream OBS všetko okrem meracieho signálu a spusti test znova."

_e2e_program_audio_guard() {
  local here="$1"
  if [ -n "${E2E_PROGRAM_AUDIO_GUARD_CMD:-}" ]; then
    timeout 20 "$E2E_PROGRAM_AUDIO_GUARD_CMD" 2>&1
  else
    timeout 20 python3 "$here/program_audio_guard.py" 2>&1
  fi
}

_e2e_program_audio_refuse() {
  local here="$1" stream="$2" line="$3"
  echo "!! issue 1421: stream program audio is NOT the measurement signal alone: ${line}" >&2
  echo "!! ${E2E_PROGRAM_AUDIO_OPERATOR_MSG}" >&2
  echo "   audible stream OBS inputs (peak over 10 s, read-only):" >&2
  if [ -n "${E2E_PROGRAM_AUDIO_INVENTORY_CMD:-}" ]; then
    timeout 40 "$E2E_PROGRAM_AUDIO_INVENTORY_CMD" "$stream" >&2 || echo "   (inventory unavailable)" >&2
  else
    timeout 40 python3 "$here/stream_audio_inventory.py" "$stream" >&2 || echo "   (inventory unavailable)" >&2
  fi
}

e2e_program_audio_preflight() {
  local here="$1" stream="$2" line="" rc=0
  if line="$(_e2e_program_audio_guard "$here")"; then rc=0; else rc=$?; fi
  echo "    ${line}"
  if [ "$rc" != 0 ]; then
    _e2e_program_audio_refuse "$here" "$stream" "$line"
    return 1
  fi
  echo "    ok: the stream program carries the measurement signal only (issue 1421)"
  return 0
}

_e2e_program_audio_ppid() {
  local st=""
  st="$(cat "/proc/$1/stat" 2>/dev/null)" || return 0
  st="${st##*) }"
  set -- $st
  printf '%s' "${2:-}"
}

e2e_program_audio_watch_start() {
  local here="$1" dir="$2" parent="$$"
  local poll="${E2E_PROGRAM_AUDIO_POLL_S:-10}"
  : >"$dir/program-audio-watch.log"
  rm -f "$dir/program-audio-watch.fail"
  (
    set +e
    i=0
    me="$BASHPID"
    # the E2E is gone once this loop is no longer its child (reparented at its exit; a zombie
    # parent still answers kill -0, so the parent link is read, not the pid)
    while [ "$i" -lt 2160 ] && [ "$(_e2e_program_audio_ppid "$me")" = "$parent" ]; do
      line="$(_e2e_program_audio_guard "$here")"
      rc=$?
      ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "${ts} rc=${rc} ${line}" >>"$dir/program-audio-watch.log"
      if [ "$rc" != 0 ] && [ ! -s "$dir/program-audio-watch.fail" ]; then
        echo "${ts} ${line}" >"$dir/program-audio-watch.fail"
      fi
      sleep "$poll"
      i=$((i + 1))
    done
  ) &
  echo $! >"$dir/program-audio-watch.pid"
  echo "    issue 1421: stream program-audio watch started (every ${poll} s, log ${dir}/program-audio-watch.log)"
}

e2e_program_audio_watch_stop() {
  local here="$1" stream="$2" dir="$3" pid="" line="" rc=0
  if [ -f "$dir/program-audio-watch.pid" ]; then
    pid="$(cat "$dir/program-audio-watch.pid")"
    pkill -P "$pid" 2>/dev/null || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    rm -f "$dir/program-audio-watch.pid"
  fi
  if line="$(_e2e_program_audio_guard "$here")"; then rc=0; else rc=$?; fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) rc=${rc} ${line} (final)" >>"$dir/program-audio-watch.log"
  if [ -s "$dir/program-audio-watch.fail" ]; then
    _e2e_program_audio_refuse "$here" "$stream" "first failing poll: $(cat "$dir/program-audio-watch.fail")"
    return 1
  fi
  if [ "$rc" != 0 ]; then
    _e2e_program_audio_refuse "$here" "$stream" "final read: ${line}"
    return 1
  fi
  echo "    ok: the stream program carried the measurement signal only for the whole recording (issue 1421)"
  return 0
}
