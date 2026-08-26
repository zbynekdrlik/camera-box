#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions only, no top-level statements) — same
# convention as scripts/lib/splitter-health.sh / obs-watchdog-decision.sh: deliberately does NOT
# set `set -euo pipefail` here, because sourcing runs this file in the CALLER's shell and strict
# mode would leak into whichever caller sources it. The caller
# (scripts/grabber-stuck-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/grabber-stuck-health.sh — #1128: the SHARED, PURE decision core for the dev1-side
# fast-capture grabber STUCK alert. No I/O, no ssh, no journalctl — pure, so it unit-tests
# exhaustively (mirrors scripts/lib/splitter-health.sh).
#
# WHY (#1128): the GENKI ShadowCast 2 grabber can free-run at ~62.5 fps AND deliver persistent
# corrupted frames — a state a `systemctl restart camera-box` does NOT clear (only a USB
# re-enumeration does). The camera-box appliance's own crate-root detector (src/grabber_stuck.rs)
# decides this and, on a STUCK verdict, logs the report-only marker `#1128 grabber STUCK` every 5s
# to its journal REGARDLESS of whether the in-process re-auth action is enabled. This watchdog is
# the ALERT half: a dev1 timer ssh-reads each ACTIVE cambox's journal for that marker and pages
# ONCE per episode + a recovery ping — keeping ONE source of truth for the verdict (the Rust
# detector decides; this only relays), exactly like the #663 self-heal marker relay. The camboxes
# have no airuleset checkout / Discord creds, so the page MUST come from dev1 (same topology as
# splitter-port / network-reach / optical-chain alert watchdogs).
#
# Source-only: pure functions, no side effects at source time.

# grabber_stuck_parse_probe <raw> -> stdout ONE line: reachable=<0|1> stuck=<0|1>
#   reachable=1 iff the probe echoed the PROBE_OK sentinel (ssh connected); stuck=1 iff a
#   `#1128 grabber STUCK` marker line is present in the probed freshness window. An ssh failure ->
#   empty raw -> reachable=0 = NODATA (never a false STUCK from an unreachable box).
grabber_stuck_parse_probe() {
  local raw="${1:-}"
  local reachable=0 stuck=0
  case "$raw" in *PROBE_OK*) reachable=1 ;; esac
  case "$raw" in *"#1128 grabber STUCK"*) stuck=1 ;; esac
  printf 'reachable=%s stuck=%s\n' "$reachable" "$stuck"
}

# grabber_stuck_classify <reachable 0|1> <stuck 0|1> -> stdout: verdict=<STUCK|OK|NODATA>
#   NODATA (unreachable) is "nothing to decide this pass", NEVER a page — an ssh/journal blip must
#   not read as either a fault or a recovery.
grabber_stuck_classify() {
  local reachable="${1:-0}" stuck="${2:-0}"
  if [ "$reachable" != "1" ]; then
    printf 'verdict=NODATA\n'
    return 0
  fi
  if [ "$stuck" = "1" ]; then
    printf 'verdict=STUCK\n'
  else
    printf 'verdict=OK\n'
  fi
}

# grabber_stuck_marker_fps <raw> -> stdout: the captured fps from the newest `#1128 grabber STUCK`
#   marker line, or "?" when absent/unparseable. The appliance's marker reads:
#   "#1128 grabber STUCK: /dev/videoN captured 62.50 fps (>= 61.5 fps over-rate floor) WITH ...".
#   The device path carries no `NN.NN`, so the first decimal after "captured " is the fps.
grabber_stuck_marker_fps() {
  local raw="${1:-}" line fps
  line="$(printf '%s\n' "$raw" | { grep -F '#1128 grabber STUCK' || true; } | tail -1)"
  fps="$(printf '%s\n' "$line" | { grep -oE 'captured [0-9]+\.[0-9]+ fps' || true; } | { grep -oE '[0-9]+\.[0-9]+' || true; } | tail -1)"
  printf '%s' "${fps:-?}"
}
