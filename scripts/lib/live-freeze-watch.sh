#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + starts/stops a background PROCESS —
# not side-effect-free at CALL time, but sourcing this file alone does nothing).
#
# scripts/lib/live-freeze-watch.sh — #758 item 3: the in-run freeze watch. A frozen camera today
# is only discovered at [4c/8] (BEFORE recording) or at decode time (AFTER a full 40-minute
# recording) — this watches DURING the recording window ([5/8]..[6/8]) so a mid-run freeze fails
# the run within ~30s of onset instead of poisoning the whole run silently (run 1299588287's own
# forensics: nothing stopped a genuinely dead cam7 early, nothing flagged the degradation loudly
# until [8/8] decode).
#
# Reuses the SAME "MV NDI camN" low-bandwidth-clone + frozen-camera-gate.py mechanism the [0/8]/
# [1/8] preflight and the [2/8]/[2b/8] sender-bounce re-verify already use (one mechanism, three
# call sites) — never a parallel screenshot-diff implementation.
#
# Runs as a BACKGROUND LOOP on dev1 (never inside cleanup() — see #759, the deliberate follow-up
# that keeps this OUT of the safety-critical trap handler for now): live_freeze_watch_start polls
# every POLL_INTERVAL_S seconds and APPENDS one line to POISON_FILE per frozen verdict (never
# stops the recording itself — the harness's [8/8] step reads POISON_FILE and fails the run there);
# live_freeze_watch_stop kills the background loop by its recorded PID.

# live_freeze_watch_start PID_FILE POISON_FILE STRIH SOURCES PROBE_BIN_DIR [POLL_INTERVAL_S]
# -> backgrounds a polling loop, writes its PID to PID_FILE. SOURCES is a comma-separated
# "MV NDI camN,..." list (mirrors frozen-camera-gate.py's own --sources shape). POLL_INTERVAL_S
# defaults to 10 (override for a tighter test loop).
live_freeze_watch_start() {
  local pid_file="$1" poison_file="$2" strih="$3" sources="$4" probe_bin_dir="$5"
  local poll_interval="${6:-10}"
  : > "$poison_file"
  (
    while true; do
      out="$(python3 "$HERE/frozen-camera-gate.py" --host "$strih" --password "" \
        --sources "$sources" --samples 3 --cadence 3 --threshold 1 --warm-settle 0 \
        --verdict-bin "$probe_bin_dir/frozen-camera-gate" 2>&1)"
      rc=$?
      if [ "$rc" = "1" ]; then
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "[freeze-watch] ${ts} FROZEN: ${out}" >>"$poison_file"
      fi
      sleep "$poll_interval"
    done
  ) &
  echo $! >"$pid_file"
}

# live_freeze_watch_stop PID_FILE -> stops the background loop started above (best-effort — a
# missing PID file or an already-dead PID is a silent no-op, never a failure of the caller).
live_freeze_watch_stop() {
  local pid_file="$1"
  if [ -f "$pid_file" ]; then
    kill "$(cat "$pid_file")" 2>/dev/null || true
    rm -f "$pid_file"
  fi
}

# live_freeze_watch_verdict POISON_FILE -> "" (empty = PASS, no freeze detected all run) or the
# full poison-file contents (FAIL — one or more freeze episodes were recorded). Pure read, no I/O
# side effects — the caller (recording-e2e.sh [8/8]) decides how to fail loud.
live_freeze_watch_verdict() {
  local poison_file="$1"
  if [ -s "$poison_file" ]; then
    cat "$poison_file"
  fi
}
