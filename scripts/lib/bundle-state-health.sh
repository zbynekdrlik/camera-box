#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (network-reach-health.sh, obs-watchdog-decision.sh,
# optical-chain-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (bundle-state-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/bundle-state-health.sh -- #732: the SHARED, PURE decision core for the dev1-side
# strih/stream `:8899` BundleStateServer active health-check watchdog. No I/O, no ping, no curl, no
# ssh, so it can be unit-tested exhaustively (mirrors network-reach-health.sh / obs-watchdog-decision.sh).
#
# WHY (#732, four live recurrences through 2026-08-13): the strih/stream `BundleStateServer`
# Scheduled Task dies with `SCHED_S_TASK_TERMINATED` (`0x40010004`) -- an informational/SUCCESS class
# -- on session/parent teardown (dominant post-reboot), so Windows Task Scheduler's
# restart-on-failure (`RestartCount`) never engages; it also failed to restart a real `0xC000013A`
# crash (silent 3 days) and cannot cover a cold-start that never fired at all. Nothing off the box
# probes `:8899`, so the version-integrity E2E gate reads the box UNKNOWN and blames itself. A passive
# Task-Scheduler policy can never cover a non-failure termination -- this needs an ACTIVE external
# prober (dev1) that restarts the task regardless of its last exit code.
#
# The dev1-side network-reachability watchdog (#1001) already probes `:8899`, but ONLY as one of
# three OR-signals for "is the box on the network at all", so a `:8899`-only death while the box is
# otherwise fully up (ping + OBS-WS :4455 answering) classifies the box REACHABLE and never pages.
# THIS watchdog closes that specific gap: box up but `:8899` down -> restart the task + a throttled
# alert. A fully-unreachable box is left to the #1001 watchdog (no double-paging, no pointless
# restart against a dark box).
#
# Source-only: pure functions, no side effects at source time.

# bundle_state_box_reachable <ping_ok 0|1> <ws_ok 0|1> -> stdout: 1 | 0
#   Is the BOX itself up, INDEPENDENT of :8899? 1 iff ping OR the OBS-WS :4455 TCP connect answered.
#   Deliberately EXCLUDES the :8899 signal -- that is the service THIS watchdog tests, so it can never
#   count toward "the box is up" (otherwise a dead :8899 could never be distinguished from a dead
#   box, and the whole watchdog would collapse into the #1001 reachability check). Any value other
#   than "1" counts as a failed signal (defensive: an empty/garbage probe result never a false up).
bundle_state_box_reachable() {
  local ping_ok="${1:-0}" ws_ok="${2:-0}"
  if [ "$ping_ok" = "1" ] || [ "$ws_ok" = "1" ]; then
    printf '1\n'
  else
    printf '0\n'
  fi
}

# bundle_state_classify <box_reachable 0|1> <bundle_healthy 0|1>
#   -> stdout: HEALTHY | DOWN | BOX_UNREACHABLE
#   HEALTHY        : the box is up AND :8899 answered 200 with a JSON body -- nothing to do.
#   DOWN           : the box is up but :8899 did NOT answer -- the exact target failure
#                    (SCHED_S_TASK_TERMINATED / cold-start-never-happened / wedged-but-listening).
#                    THIS watchdog acts (confirm -> restart + throttled alert).
#   BOX_UNREACHABLE: the box itself is down -- defer to the #1001 network-reachability watchdog;
#                    a restart against a dark box is pointless and a page would double up. Nothing to
#                    decide here.
#   Any value other than "1" counts as 0 (defensive: a garbage probe result never a false HEALTHY,
#   and a not-provably-up box defers rather than restarting).
bundle_state_classify() {
  local box_reachable="${1:-0}" bundle_healthy="${2:-0}"
  if [ "$box_reachable" != "1" ]; then
    printf 'BOX_UNREACHABLE\n'
  elif [ "$bundle_healthy" = "1" ]; then
    printf 'HEALTHY\n'
  else
    printf 'DOWN\n'
  fi
}

# bundle_state_alert_detail <box> <ip> <ping_ok 0|1> <ws_ok 0|1> <bundle_ok 0|1> -> one human line
#   naming the box+ip and the up/DOWN state of each signal, for the Discord alert body. `DOWN`
#   (upper) marks a failed signal so it reads at a glance on a phone; `up` marks a live one. Mirrors
#   net_reach_alert_detail's format so the two watchdogs' alerts read consistently.
bundle_state_alert_detail() {
  local box="${1:-?}" ip="${2:-?}" ping_ok="${3:-0}" ws_ok="${4:-0}" bundle_ok="${5:-0}"
  local p w b
  [ "$ping_ok" = "1" ] && p="up" || p="DOWN"
  [ "$ws_ok" = "1" ] && w="up" || w="DOWN"
  [ "$bundle_ok" = "1" ] && b="up" || b="DOWN"
  printf '%s (%s): ping %s, OBS-WS:4455 %s, bundle-state:8899 %s\n' "$box" "$ip" "$p" "$w" "$b"
}

# bundle_state_restart_remote_cmd -> stdout: the exact remote command to (re)start the task.
#   Pure string (no network) so the session-agnostic-safety property is pinned by a Tier-0 test:
#   `schtasks /run` starts the HIDDEN, headless bundle-state supervisor task (verified live: the task
#   action is `powershell ... -WindowStyle Hidden -File run-bundle-state-server.ps1`) -- session-
#   agnostic per .claude/rules/win-ssh-vs-mcp.md. NEVER the `/it` interactive form (a desktop-session
#   op the headless dev1 watchdog must never issue, and a documented DEAD END on these boxes). Runs
#   over the boxes' default cmd.exe ssh shell, so no PowerShell wrapper is needed. Trailing `;` so it
#   composes safely if a caller ever embeds it mid-string (the $()-newline-strip gotcha).
bundle_state_restart_remote_cmd() {
  printf 'schtasks /run /tn "BundleStateServer";\n'
}
