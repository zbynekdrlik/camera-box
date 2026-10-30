#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only, no top-level statements besides defaults) --
# sourcing executes it in the caller's shell, so strict mode here would leak into the dev1 watchdogs
# that source it (they set their own options, ci-testing-gotchas.md).
#
# scripts/lib/avsync-production.sh -- issue 1441 (owner 28.10.2026: "do buducna chodi hrozne vela
# spamu do alert-snv discord o sync obraz zvuk ... len podstatne hlasky"): is a venue in a PRODUCTION
# right now? The A/V report and the A/V alarms post only for a production.
#
#   * the rig venue (AVSYNC_RIG_MODE_VENUE, default snv): the rig TEST/EVENT mode read off the cam2
#     painter (scripts/lib/rig-mode-state.sh). EVENT = yes, TEST = no (development streams run all
#     day there, 20 pages in 48 h before this). An unreadable mode falls back to the stream state.
#   * every other venue: its stream box streams (OBS-WS outputActive) = yes, not = no.
#   * anything unreadable = unknown. Every caller treats unknown like a production: a broken probe
#     must never silence a real broadcast.
#   * AVSYNC_PRODUCTION (yes|no|unknown) overrides the reading (tests, an operator's one-off).
#
# Requires scripts/lib/rig-mode-state.sh sourced first (rig_mode_state_probe_remote_snippet,
# rig_mode_from_painter_snapshot).

AVSYNC_RIG_MODE_VENUE="${AVSYNC_RIG_MODE_VENUE:-snv}"
RIG_MODE_PAINTER_IP="${RIG_MODE_PAINTER_IP:-10.77.9.62}"
RIG_MODE_PROBE_TIMEOUT="${RIG_MODE_PROBE_TIMEOUT:-20}"
AVSYNC_CAM_SSH_USER="${AVSYNC_CAM_SSH_USER:-root}"

# avsync_production_from <venue> <rig_mode EVENT|TEST|UNKNOWN|""> <stream_active true|false|null|"">
#   -> stdout: yes | no | unknown. Pure.
avsync_production_from() {
  local venue="$1" mode="$2" active="$3"
  if [ "$venue" = "$AVSYNC_RIG_MODE_VENUE" ]; then
    case "$mode" in
      EVENT) printf 'yes\n'; return 0 ;;
      TEST) printf 'no\n'; return 0 ;;
    esac
  fi
  case "$active" in
    true) printf 'yes\n' ;;
    false) printf 'no\n' ;;
    *) printf 'unknown\n' ;;
  esac
}

# avsync_rig_mode -> stdout: EVENT | TEST | UNKNOWN, one bounded ssh to cam2. sshpass stays the
# OUTER command, so a test's sshpass stub intercepts the whole call (ci-testing-gotchas.md: a
# `timeout sshpass` would exec the real sshpass and reach the live rig from a unit test).
avsync_rig_mode() {
  local snapshot
  snapshot="$(sshpass -p "${CAM_PW:-newlevel}" timeout "$RIG_MODE_PROBE_TIMEOUT" \
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o BatchMode=no \
    "${AVSYNC_CAM_SSH_USER}@${RIG_MODE_PAINTER_IP}" \
    "$(rig_mode_state_probe_remote_snippet)" 2>/dev/null || true)"
  rig_mode_from_painter_snapshot "$snapshot"
}

# avsync_venue_production <venue> <stream_active true|false|null|""> -> stdout: yes | no | unknown.
#   The rig mode is probed only for the rig venue, and only when no override is set.
avsync_venue_production() {
  local venue="$1" active="${2:-}" mode=""
  if [ -n "${AVSYNC_PRODUCTION:-}" ]; then
    case "$AVSYNC_PRODUCTION" in
      yes | no) printf '%s\n' "$AVSYNC_PRODUCTION" ;;
      *) printf 'unknown\n' ;;
    esac
    return 0
  fi
  [ "$venue" = "$AVSYNC_RIG_MODE_VENUE" ] && mode="$(avsync_rig_mode)"
  avsync_production_from "$venue" "$mode" "$active"
}

# avsync_stream_active <ws_host> -> stdout: true | false | null, the stream box OBS outputActive read
# through obs_phase2.py stream-status (the lineup watchdog's read). An unreachable OBS-WS = null.
avsync_stream_active() {
  local host="$1" out active
  local -a args=(stream-status --host "$host")
  local pw="${STREAM_OBS_WS_PW:-${OBS_WS_PASSWORD:-${OBS_PASSWORD:-}}}"
  [ -n "$pw" ] && args+=(--password "$pw")
  out="$(timeout 20 python3 "${AVSYNC_OBS_PHASE2:-$(dirname "${BASH_SOURCE[0]}")/../obs_phase2.py}" \
    "${args[@]}" 2>/dev/null || true)"
  active="$(printf '%s\n' "$out" | sed -n 's/^active=\([A-Za-z0-9]*\).*/\1/p' | tail -1)"
  case "$active" in
    True | true | 1) printf 'true\n' ;;
    False | false | 0) printf 'false\n' ;;
    *) printf 'null\n' ;;
  esac
}

# avsync_venue_production_live <venue> <ws_host> -> stdout: yes | no | unknown. Like
#   avsync_venue_production, but reads the stream state itself, and only when the answer needs it
#   (no override, and not decided by the rig mode).
avsync_venue_production_live() {
  local venue="$1" host="$2" mode=""
  if [ -n "${AVSYNC_PRODUCTION:-}" ]; then
    avsync_venue_production "$venue" ""
    return 0
  fi
  [ "$venue" = "$AVSYNC_RIG_MODE_VENUE" ] && mode="$(avsync_rig_mode)"
  case "$mode" in
    EVENT | TEST) avsync_production_from "$venue" "$mode" "" ;;
    *) avsync_production_from "$venue" "$mode" "$(avsync_stream_active "$host")" ;;
  esac
}
