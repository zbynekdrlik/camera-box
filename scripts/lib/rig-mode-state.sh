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
# (#892) and removes /run/rig-painter.pid; rig-mode.sh test enable-`--now`s it. That is the
# "painter_expected"/"painter_alive" signal scripts/lib/optical-chain-health.sh already owns -- this
# lib self-sources it (ONE definition each, ONE cam2 probe snippet) and adds a REACHABILITY SENTINEL
# so the verdict is 3-state:
#   UNKNOWN : the cam2 probe carried no RIG_MODE_PROBE_OK sentinel (ssh failed / box off / empty), OR
#             any of the four painter fields is MISSING (a truncated/partial ssh read) or is `?` (a
#             systemctl hiccup -- manager unresponsive). The mode is UNREADABLE -> the caller behaves
#             EXACTLY as today (fail-safe: an unreadable mode must NEVER silence a real TEST-mode
#             fault, and a partial/hiccup read must NEVER be misread as a provable EVENT).
#   TEST    : reachable AND the painter is EXPECTED (pidfile present OR cam2-painter.service enabled)
#             OR ALIVE (pidfile PID alive OR cam2-painter.service active). A running painter -- even
#             an active-but-DISABLED one (an E2E `systemctl start` on a rig last set to EVENT leaves
#             the unit active+disabled until a reboot / `rig-mode.sh test`) -- is positive evidence of
#             NOT a clean broadcast (#892: EVENT never leaves a painter running), so it reads TEST.
#   EVENT   : reachable AND the painter is NEITHER expected NOR alive (all four fields a definite 0).
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
#   The sentinel is echoed FIRST purely so ssh connectivity is unambiguous. Its PRESENCE alone is
#   NOT read as "provable state": rig_mode_from_painter_snapshot ALSO requires all four painter
#   fields to be present with a definite 0/1 value, so a truncated read (sentinel present, systemctl
#   lines lost) or a `?` hiccup is UNKNOWN, never EVENT. Passed as the ssh command's LAST argument,
#   so the $()-strip of its trailing newline is harmless (nothing follows it -- the #744/#746
#   mid-string-glue trap does not apply).
rig_mode_state_probe_remote_snippet() {
  local pidfile="${1:-/run/rig-painter.pid}" service="${2:-cam2-painter.service}"
  printf 'echo RIG_MODE_PROBE_OK\n'
  optical_chain_painter_probe_remote_snippet "$pidfile" "$service"
}

# rig_mode_from_painter_snapshot <snapshot> -> stdout: EVENT | TEST | UNKNOWN
#   Pure. The snapshot is rig_mode_state_probe_remote_snippet's stdout (the sentinel + KEY|value
#   lines). Fail-safe toward UNKNOWN in every ambiguous case:
#     - no RIG_MODE_PROBE_OK sentinel (empty / ssh failed)                     -> UNKNOWN
#     - any of the four painter fields MISSING (truncated/partial ssh read)    -> UNKNOWN
#     - any painter field is `?` (a systemctl no-answer hiccup)                -> UNKNOWN
#   Only when the sentinel is present AND all four fields are a definite 0/1 does it decide:
#     - painter EXPECTED (pidfile OR service enabled) OR ALIVE (pid alive OR service active) -> TEST
#     - none of the four set                                                                 -> EVENT
#   The `expected OR alive` is what makes an active-but-DISABLED painter read TEST (a running painter
#   is never a clean broadcast, #892) instead of a false EVENT that would silence real DEAD_PORTs.
rig_mode_from_painter_snapshot() {
  local snapshot="${1:-}"
  case "$snapshot" in
    *RIG_MODE_PROBE_OK*) : ;;
    *)
      printf 'UNKNOWN\n'
      return 0
      ;;
  esac
  # Every painter field must be present AND a definite 0/1 -- a missing line or a `?` -> UNKNOWN.
  local f v
  for f in PID_PRESENT PID_ALIVE SVC_ENABLED SVC_ACTIVE; do
    v="$(_optical_chain_snapshot_field "$snapshot" "$f")"
    case "$v" in
      0 | 1) : ;;
      *)
        printf 'UNKNOWN\n'
        return 0
        ;;
    esac
  done
  local expected alive
  expected="$(optical_chain_painter_expected_from_snapshot "$snapshot")"
  alive="$(optical_chain_painter_alive_from_snapshot "$snapshot")"
  if [ "$expected" = "1" ] || [ "$alive" = "1" ]; then
    printf 'TEST\n'
  else
    printf 'EVENT\n'
  fi
}
