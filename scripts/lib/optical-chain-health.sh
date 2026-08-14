#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions + string builders, no top-level
# statements) -- matches the sibling scripts/lib/*.sh convention (obs-watchdog-decision.sh,
# imag-power-envelope.sh, cambox-parallel-restore.sh) of deliberately NOT setting `set -euo
# pipefail` here: sourcing this file executes it in the CALLER's shell, so imposing strict mode
# here would leak into whichever caller sources it. The callers (optical-chain-alert-watchdog.sh,
# optical-chain-preflight.sh) set their own strict mode.
#
# scripts/lib/optical-chain-health.sh -- #860: the SHARED, PURE decision core for the cam2
# optical-injection leg (painter -> cam2 monitor -> cam1 camera). No I/O, no ssh, no OBS, no MCP,
# so it can be unit-tested exhaustively (mirrors scripts/lib/obs-watchdog-decision.sh /
# imag-power-envelope.sh). TWO dev1-side surfaces consume it: the standing
# optical-chain-alert-watchdog.sh (systemd timer) AND the recording-e2e.sh [0/8] preflight.
#
# WHY (#860, live incident 2026-08-14): a chain of FAILED E2E runs whose cleanups each logged
# `WARNING #712: cam2/painter restore failed/timed out` left the painter DEAD -- cam2's monitor
# pitch black -- and the next gate run's optical hop reported UNAVAILABLE / breached the
# undecodable floor, with NO alert firing anywhere. A dead painter must page immediately (the
# standing rig-degradation-alert rule) and fail-fast the harness before a ~40-min run is burned.
#
# TEST/EVENT-mode discriminator: `painter_expected` = the cam2 painter pidfile is present OR the
# permanent cam2-painter.service is enabled. This is the DURABLE, NON-staling state rig-mode.sh
# already maintains -- since #1008/#937 TEST mode's STEADY STATE is the ENABLED permanent
# cam2-painter.service (the transient pidfile /run/rig-painter.pid exists only during the
# at-mode-set verification window and is REMOVED at handoff), so the service-enabled arm is the
# durable TEST signal; EVENT mode REMOVES the pidfile (painter_stop_remote) AND disables the
# service (#892). So a black
# monitor in EVENT mode -> painter_expected=0 -> SKIP (never a false alert); a dead painter in
# TEST mode -> alert. Deliberately reuses the pidfile/service lifecycle instead of a second,
# drift-prone marker file, and instead of the #281 rig-heartbeat (which is stale-after-10-min by
# design -- the wrong gate for a standing 2-h TEST painter).
#
# Source-only: pure functions + string builders, no side effects at source time.

# optical_chain_classify_nonblack_probe <rc> <output> -> stdout: OK | BLACK | UNKNOWN
#   Classify the outcome of `obs_phase2.py assert-program-nonblack` (#901): rc 0 = PASS = OK; a
#   non-zero rc whose output carries the BLACK verdict ("renders BLACK") = BLACK; ANY OTHER
#   non-zero (WS unreachable, PIL missing, connection refused) = UNKNOWN. A probe that could not
#   run is "nothing to decide", NEVER proof of a dark monitor -- the imag-power-envelope
#   "connectivity failure = nothing to decide" discipline.
optical_chain_classify_nonblack_probe() {
  local rc="${1:-1}" output="${2:-}"
  if [ "$rc" = "0" ]; then
    printf 'OK\n'
    return 0
  fi
  case "$output" in
    *"renders BLACK"*) printf 'BLACK\n' ;;
    *) printf 'UNKNOWN\n' ;;
  esac
}

# _optical_chain_snapshot_field <snapshot> <KEY> -> stdout: the value after `KEY|` (last match), or
# empty. Internal helper; the snapshot is the cam2 painter probe's `KEY|value` lines.
_optical_chain_snapshot_field() {
  local snapshot="$1" key="$2"
  printf '%s\n' "$snapshot" | sed -n "s/^${key}|//p" | tail -1
}

# optical_chain_painter_expected_from_snapshot <snapshot> -> stdout: 1 | 0
#   A painter is EXPECTED (TEST-mode intent) iff the pidfile is present (PID_PRESENT|1) OR the
#   permanent service is enabled (SVC_ENABLED|1). EVENT mode leaves neither -> 0.
optical_chain_painter_expected_from_snapshot() {
  local snapshot="$1" pid_present svc_enabled
  pid_present="$(_optical_chain_snapshot_field "$snapshot" PID_PRESENT)"
  svc_enabled="$(_optical_chain_snapshot_field "$snapshot" SVC_ENABLED)"
  if [ "$pid_present" = "1" ] || [ "$svc_enabled" = "1" ]; then
    printf '1\n'
  else
    printf '0\n'
  fi
}

# optical_chain_painter_alive_from_snapshot <snapshot> -> stdout: 1 | 0
#   A painter is ALIVE iff the pidfile's PID is alive (PID_ALIVE|1) OR the permanent service is
#   active (SVC_ACTIVE|1). A present-but-dead pidfile (crashed painter) -> 0.
optical_chain_painter_alive_from_snapshot() {
  local snapshot="$1" pid_alive svc_active
  pid_alive="$(_optical_chain_snapshot_field "$snapshot" PID_ALIVE)"
  svc_active="$(_optical_chain_snapshot_field "$snapshot" SVC_ACTIVE)"
  if [ "$pid_alive" = "1" ] || [ "$svc_active" = "1" ]; then
    printf '1\n'
  else
    printf '0\n'
  fi
}

# optical_chain_alert_condition <painter_expected 0|1> <painter_alive 0|1> <optical OK|BLACK|UNKNOWN>
#   -> stdout: skip | alert:PAINTER-DEAD | alert:OPTICAL-BLACK | healthy | healthy-unverified
#   The pure verdict both surfaces act on. Any value other than 1 for expected/alive is treated as
#   0/not-alive (defensive). Unrecognized optical token is treated as UNKNOWN.
optical_chain_alert_condition() {
  local expected="${1:-0}" alive="${2:-0}" optical="${3:-UNKNOWN}"
  if [ "$expected" != "1" ]; then
    printf 'skip\n'
    return 0
  fi
  if [ "$alive" != "1" ]; then
    printf 'alert:PAINTER-DEAD\n'
    return 0
  fi
  case "$optical" in
    BLACK) printf 'alert:OPTICAL-BLACK\n' ;;
    OK)    printf 'healthy\n' ;;
    *)     printf 'healthy-unverified\n' ;;
  esac
}

# optical_chain_painter_probe_remote_snippet [PIDFILE] [SERVICE] -> stdout: REMOTE bash for cam2
#   that emits exactly four `KEY|value` lines the pure functions above parse:
#     PID_PRESENT|<0|1>  pidfile exists
#     PID_ALIVE|<0|1>    pidfile's PID is alive (kill -0)
#     SVC_ENABLED|<0|1>  the permanent painter service is enabled (systemctl is-enabled == enabled)
#     SVC_ACTIVE|<0|1>   the permanent painter service is active
#   A box without the unit installed reports SVC_ENABLED|0 / SVC_ACTIVE|0 (never an error). Emit
#   this INSIDE an ssh command string to cam2: `ssh root@cam2 "$(optical_chain_painter_probe_remote_snippet)"`.
optical_chain_painter_probe_remote_snippet() {
  local pidfile="${1:-/run/rig-painter.pid}" service="${2:-cam2-painter.service}"
  cat <<REMOTE
_pf='$pidfile'
if [ -f "\$_pf" ]; then
  echo "PID_PRESENT|1"
  _pid="\$(cat "\$_pf" 2>/dev/null || true)"
  if [ -n "\$_pid" ] && kill -0 "\$_pid" 2>/dev/null; then echo "PID_ALIVE|1"; else echo "PID_ALIVE|0"; fi
else
  echo "PID_PRESENT|0"
  echo "PID_ALIVE|0"
fi
if [ "\$(systemctl is-enabled '$service' 2>/dev/null)" = "enabled" ]; then echo "SVC_ENABLED|1"; else echo "SVC_ENABLED|0"; fi
if [ "\$(systemctl is-active '$service' 2>/dev/null)" = "active" ]; then echo "SVC_ACTIVE|1"; else echo "SVC_ACTIVE|0"; fi
REMOTE
}
