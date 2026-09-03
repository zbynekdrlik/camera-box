#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + a string builder + one sibling-lib source,
# no other top-level statements) -- matches the sibling scripts/lib/*.sh convention
# (optical-chain-health.sh / splitter-health.sh / obs-watchdog-decision.sh) of deliberately NOT
# setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so strict
# mode here would leak into whichever caller sources it. The caller (splitter-port-alert-watchdog.sh)
# sets its own strict mode.
#
# scripts/lib/rig-mode-state.sh -- #1290: the SHARED, 3-state rig TEST/EVENT-mode discriminator for
# the dev1-side TEST-premise watchdogs (the splitter-port #739 family). A TEST-premise watchdog --
# one whose verdict assumes the TEST-rig topology (ONE camera through an HDMI splitter to EVERY
# cambox) -- must NOT page in provable EVENT/production mode, where each cambox has its OWN camera.
#
# WHY (#1290, live 2026-09-03): splitter-port-alert-watchdog.sh paged the owner's phone 5x during a
# LIVE show because a cambox with no camera connected reads grayscale while a sibling reads colour,
# so its sibling-anchor DEAD_PORT discriminator (a TEST-rig premise) fired. The watchdog had no
# notion of rig mode.
#
# The DURABLE EVENT signal (do NOT use the #281 rig-heartbeat -- stale-after-10-min by design, the
# wrong gate for an idle-but-in-TEST rig): rig-mode.sh event STOPS+DISABLES cam2-painter.service
# (#892) and removes /run/rig-painter.pid; rig-mode.sh test enable-`--now`s it. That is EXACTLY the
# "painter_expected" signal scripts/lib/optical-chain-health.sh already owns -- this lib self-sources
# it (ONE definition of painter_expected, ONE cam2 probe snippet) and adds a REACHABILITY SENTINEL so
# the verdict is 3-state:
#   UNKNOWN : the cam2 probe was empty / carried no RIG_MODE_PROBE_OK sentinel (ssh failed / box off)
#             -> the mode is UNREADABLE -> the caller behaves EXACTLY as today (fail-safe: an
#             unreadable mode must NEVER silence a real TEST-mode fault, and NEVER be read as EVENT).
#   TEST    : reachable AND painter_expected (pidfile present OR cam2-painter.service enabled).
#   EVENT   : reachable AND NOT painter_expected (rig-mode.sh event disabled + removed the painter).
#
# Source-only: pure functions + a string builder; the ONE top-level statement self-sources the
# sibling optical-chain-health.sh lib (in a $() subshell so it never changes the caller's cwd).

# Self-source optical-chain-health.sh so painter_expected + the cam2 probe snippet have ONE
# definition each (never a second drift-prone copy). BASH_SOURCE[0] resolves to THIS lib regardless
# of who sources it; the $() subshell keeps the caller's cwd unchanged.
_RIG_MODE_STATE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/optical-chain-health.sh
. "$_RIG_MODE_STATE_LIB_DIR/optical-chain-health.sh"

# rig_mode_state_probe_remote_snippet [PIDFILE] [SERVICE] -> stdout: REMOTE bash for cam2 that emits
#   a reachability sentinel line (RIG_MODE_PROBE_OK) FOLLOWED by the four painter KEY|value lines
#   optical_chain_painter_probe_remote_snippet emits (PID_PRESENT/PID_ALIVE/SVC_ENABLED/SVC_ACTIVE).
#   Embed INSIDE an ssh command string to cam2:
#     ssh root@cam2 "$(rig_mode_state_probe_remote_snippet)"
#   The sentinel is echoed FIRST, so if ssh connected at all it is present even if the systemctl
#   reads hiccup -- which is what makes an EMPTY snapshot mean "unreachable" (UNKNOWN), never EVENT.
#   Passed as the ssh command's LAST argument, so the $()-strip of its trailing newline is harmless
#   (nothing follows it -- the #744/#746 mid-string-glue trap does not apply).
rig_mode_state_probe_remote_snippet() {
  local pidfile="${1:-/run/rig-painter.pid}" service="${2:-cam2-painter.service}"
  printf 'echo RIG_MODE_PROBE_OK\n'
  optical_chain_painter_probe_remote_snippet "$pidfile" "$service"
}

# rig_mode_from_painter_snapshot <snapshot> -> stdout: EVENT | TEST | UNKNOWN
#   Pure. The snapshot is rig_mode_state_probe_remote_snippet's stdout (the sentinel + KEY|value
#   lines). No sentinel (empty / partial ssh output) -> UNKNOWN (the caller behaves as today). Else
#   reuse optical_chain_painter_expected_from_snapshot: painter expected -> TEST, else -> EVENT.
rig_mode_from_painter_snapshot() {
  local snapshot="${1:-}"
  case "$snapshot" in
    *RIG_MODE_PROBE_OK*) : ;;
    *)
      printf 'UNKNOWN\n'
      return 0
      ;;
  esac
  local expected
  expected="$(optical_chain_painter_expected_from_snapshot "$snapshot")"
  if [ "$expected" = "1" ]; then
    printf 'TEST\n'
  else
    printf 'EVENT\n'
  fi
}

# rig_mode_is_event <verdict> -> exit 0 iff verdict == EVENT. Convenience boolean gate for a caller
# that prefers `if rig_mode_is_event "$m"; then ...` over a string compare.
rig_mode_is_event() {
  [ "${1:-}" = "EVENT" ]
}
