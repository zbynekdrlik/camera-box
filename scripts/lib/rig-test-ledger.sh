#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (mirrors rig-test-dropin.sh /
# presenter-liveness-check.sh / marker-device-resolve.sh) -- functions use `return 1` as their
# normal calling convention, and `set -euo pipefail` must NEVER be set here because sourcing
# this file mutates the CALLING script's (rig-mode.sh / recording-e2e.sh) own shell options.
#
# scripts/lib/rig-test-ledger.sh — #723: the rig-test LEDGER. Anything a test/worker starts on
# the rig (a painter, a burn, a scene override) registers durably here; `rig-mode.sh event`
# cleans BY LEDGER — never by guessing a process NAME PATTERN.
#
# WHY (#721, the 2026-07-12 live incident's root link): a worker launched a RENAMED painter
# (`cam2-painter`, 24h duration) that EVERY name-based cleanup (`pkill -x frame-probe`,
# `pgrep -f -- --paint-only`) silently missed, because it matched neither pattern. A process
# tracked by PID cannot be evaded by a rename — `kill $PID` does not care what the process calls
# itself. The ledger is the durable record of "what did we start + its PID + how long it's
# allowed to live", so event mode (or an orphan sweep) can find and kill EVERYTHING regardless of
# name.
#
# Sourced by:
#   - scripts/rig-mode.sh        (painter_launch_remote registers; do_event cleans)
#   - scripts/recording-e2e.sh   (its own painter launch registers too)
#
# Design: registration + termination EXECUTE on the box that hosts the process (cam2 today — the
# only box that runs a painter) via the ssh-embeddable REMOTE bash snippets below (same
# `_cmds`-suffix convention as audio-marker-check.sh / rig-test-dropin.sh). Reading + deciding
# what to clean happens on dev1 (the orchestrator), where jq is available; the cam-box appliance
# itself needs no JSON tooling — the JSONL line is built with plain `printf`.

# rig_test_ledger_default_max_duration_secs -> the safety-net cap (1h) applied to any ledger
# entry that does not supply an explicit override reason. #721 was a 24h-duration renamed
# painter nothing ever caught — this cap is what makes an UN-REASONED long-lived registration
# impossible.
rig_test_ledger_default_max_duration_secs() {
  echo 3600
}

# rig_test_ledger_effective_max_duration REQUESTED [REASON] -> the max_duration to actually
# register: REQUESTED verbatim when REASON is non-empty (an explicit, JUSTIFIED override — e.g.
# rig-mode TEST mode's intentional 2h measurement window); otherwise clamped to
# rig_test_ledger_default_max_duration_secs (min(REQUESTED, cap)).
rig_test_ledger_effective_max_duration() {
  local requested="$1" reason="${2:-}" cap
  cap="$(rig_test_ledger_default_max_duration_secs)"
  if [ -n "$reason" ]; then
    echo "$requested"
  elif [ "$requested" -gt "$cap" ]; then
    echo "$cap"
  else
    echo "$requested"
  fi
}

# rig_test_ledger_is_expired START_EPOCH MAX_DURATION NOW_EPOCH -> "1" if
# NOW_EPOCH >= START_EPOCH + MAX_DURATION, else "0". Pure arithmetic — no I/O.
rig_test_ledger_is_expired() {
  local start="$1" max_dur="$2" now="$3"
  if [ "$now" -ge "$((start + max_dur))" ]; then echo 1; else echo 0; fi
}

# rig_test_ledger_entry_json WHAT PID_OR_UNIT BOX STARTED_BY MAX_DURATION_SECS START_EPOCH ->
# one-line JSON object (the JSONL row shape). Minimal manual escaping (backslash + double-quote)
# — every caller passes internal, script-controlled string values (script/component names, PIDs
# or unit names, box hostnames), never arbitrary user input.
rig_test_ledger_entry_json() {
  local what="$1" pidunit="$2" box="$3" started_by="$4" max_duration="$5" start_epoch="$6"
  what="${what//\\/\\\\}"; what="${what//\"/\\\"}"
  pidunit="${pidunit//\\/\\\\}"; pidunit="${pidunit//\"/\\\"}"
  box="${box//\\/\\\\}"; box="${box//\"/\\\"}"
  started_by="${started_by//\\/\\\\}"; started_by="${started_by//\"/\\\"}"
  printf '{"what":"%s","pid_or_unit":"%s","box":"%s","started_by":"%s","max_duration_secs":%s,"start_epoch":%s}\n' \
    "$what" "$pidunit" "$box" "$started_by" "$max_duration" "$start_epoch"
}

# RIG_TEST_LEDGER_PATH_DEFAULT — the per-box ledger path (/run is tmpfs: auto-cleared on reboot,
# which is fine — a reboot already kills every registered process too).
RIG_TEST_LEDGER_PATH_DEFAULT="/run/camera-box-rig-tests.jsonl"

# rig_test_ledger_register_remote_cmds WHAT PID_OR_UNIT BOX STARTED_BY MAX_DURATION_SECS
#   [LEDGER_PATH] -> the REMOTE bash snippet that appends ONE JSONL entry (rig_test_ledger_entry_json's
# shape) to LEDGER_PATH, using the REMOTE box's OWN clock for start_epoch (consistent with expiry
# checks later reading that same box's clock). Creates the ledger's directory first (defensive —
# /run already exists on every box, but a custom LEDGER_PATH might not).
rig_test_ledger_register_remote_cmds() {
  local what="$1" pidunit="$2" box="$3" started_by="$4" max_duration="$5"
  local ledger="${6:-$RIG_TEST_LEDGER_PATH_DEFAULT}"
  cat <<REMOTE
mkdir -p "\$(dirname "$ledger")" 2>/dev/null || true
NOW_EPOCH=\$(date +%s)
printf '{"what":"$what","pid_or_unit":"$pidunit","box":"$box","started_by":"$started_by","max_duration_secs":$max_duration,"start_epoch":'"\$NOW_EPOCH"'}\n' >> "$ledger"
echo "[#723] registered ledger entry: what=$what pid_or_unit=$pidunit box=$box max_duration=${max_duration}s (\$(basename "$ledger"))"
REMOTE
}

# rig_test_ledger_read_remote_cmds [LEDGER_PATH] -> REMOTE bash: print the ledger's raw JSONL
# content (empty output, never an error, when the file doesn't exist yet).
rig_test_ledger_read_remote_cmds() {
  local ledger="${1:-$RIG_TEST_LEDGER_PATH_DEFAULT}"
  echo "cat \"$ledger\" 2>/dev/null || true"
}

# rig_test_ledger_clear_remote_cmds [LEDGER_PATH] -> REMOTE bash: EMPTY the ledger file (never
# delete it — a subsequent registration just appends to a fresh empty file; a missing directory
# would otherwise make the next registration's `>>` fail).
rig_test_ledger_clear_remote_cmds() {
  local ledger="${1:-$RIG_TEST_LEDGER_PATH_DEFAULT}"
  echo ": > \"$ledger\" 2>/dev/null || true"
}

# rig_test_ledger_terminate_entry_cmds PID_OR_UNIT KIND -> REMOTE bash that terminates a SINGLE
# ledger entry: SIGTERM, wait up to ~5s, escalate to SIGKILL if still alive. KIND is "pid" (a
# bare numeric PID — kill/kill -9, immune to any process rename: this is the #721 fix) or "unit"
# (a systemd unit name — `systemctl stop`, which SIGTERMs then SIGKILLs internally per its own
# TimeoutStopSec). Prints "KILL_NEEDED=1" on stdout iff SIGKILL was actually required (graceful
# SIGTERM did not stop it in time) — the caller uses this to decide whether the #660 clean-paint
# fallback (below) is warranted; prints "KILL_NEEDED=0" otherwise. Always exits 0 — a
# process/unit that is already gone is a SUCCESSFUL cleanup, not a failure; this function's job
# is "make sure it's gone", never "prove it was alive".
rig_test_ledger_terminate_entry_cmds() {
  local target="$1" kind="$2"
  case "$kind" in
    unit)
      cat <<REMOTE
if systemctl is-active --quiet '$target' 2>/dev/null; then
  systemctl stop '$target' 2>/dev/null || true
fi
if systemctl is-active --quiet '$target' 2>/dev/null; then echo "KILL_NEEDED=1"; else echo "KILL_NEEDED=0"; fi
REMOTE
      ;;
    *)
      cat <<REMOTE
if kill -0 "$target" 2>/dev/null; then
  kill "$target" 2>/dev/null || true
  ti=0; while kill -0 "$target" 2>/dev/null && [ \$ti -lt 10 ]; do sleep 0.5; ti=\$((ti+1)); done
  if kill -0 "$target" 2>/dev/null; then
    kill -9 "$target" 2>/dev/null || true
    sleep 0.5
    echo "KILL_NEEDED=1"
  else
    echo "KILL_NEEDED=0"
  fi
else
  echo "KILL_NEEDED=0"
fi
REMOTE
      ;;
  esac
}

# rig_test_ledger_clean_paint_fallback_cmds [FB_DEVICE] -> REMOTE bash: the #660 fallback
# screen-clear — when an entry's process had to be SIGKILLed, it never got the chance to run its
# own graceful teardown blank (src/probe/fb.rs::blank_fbdev, called from KmsPresenter's Drop,
# which SIGKILL bypasses entirely). Best-effort raw-zero write of the fbdev's frame buffer so a
# frozen QR frame does not linger on the broadcast monitor for the few seconds until camera-box
# restarts and re-grabs it. This is a stopgap EXTERNAL clear (no real fbdev-geometry awareness),
# not a substitute for the process's own blank — but it needs no probe-feature Rust build to
# deploy, and never fails the caller (`|| true`).
rig_test_ledger_clean_paint_fallback_cmds() {
  local fb_device="${1:-/dev/fb0}"
  echo "dd if=/dev/zero of=\"$fb_device\" bs=1M count=8 2>/dev/null || true"
}
