#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# scripts/lib/event-assert.sh's convention -- `set -euo pipefail` must NEVER be set here since
# sourcing this file mutates the CALLING script's own shell options.
#
# scripts/lib/preflight-fleet-check.sh — #758 item 1: the per-box minute-0 preflight check. Two
# things event_assert_fleet_check_cmds (#722, scripts/lib/event-assert.sh) does NOT already cover:
#
#   1. `PAINT_COUNT` there counts --paint-only PROCESSES (the frame-probe painter) -- this repo's
#      #758 preflight instead needs the CAMERA-BOX EMITTER process count (a different process
#      entirely). `pgrep -cx camera-box` matches the process's exact `comm` name (-x), which
#      NEVER self-matches the enclosing `bash -c "$SCRIPT"` shell that runs this over ssh (unlike
#      `pgrep -f`, whose #722-documented self-match footgun needed a base64-encoded pattern to
#      dodge -- `-x`'s exact-name match has no such risk, so no encoding trick is needed here).
#   2. Reports `SERVICE_ACTIVE`/`EMITTER_COUNT`/`STRAY_UNITS` as ONE preflight-shaped line, so the
#      harness's per-box loop can parse a SINGLE ssh round trip per box.
#
# Sourced by scripts/recording-e2e.sh's [0/8] preflight (ALL_CAMBOX=1 fleet sweep).

# preflight_fleet_check_cmds -> the REMOTE bash run on EACH cam box (over ssh) that reports, in
# one round trip (AFTER the caller has already run tmp_burn_sweep_stale_units_cmds +
# tmp_burn_sweep_stale_cmds to self-heal routine leftover junk from a prior run):
#   SERVICE_ACTIVE=<state>   — `systemctl is-active camera-box`
#   EMITTER_COUNT=<n>        — exact-name pgrep count for the camera-box process itself
#   STRAY_UNITS=<comma-list> — any camera-box-burn-* systemd unit STILL present (empty when clean
#                               — i.e. the self-heal sweep actually worked)
preflight_fleet_check_cmds() {
  cat <<'REMOTE'
SERVICE_ACTIVE=$(systemctl is-active camera-box 2>/dev/null); SERVICE_ACTIVE="${SERVICE_ACTIVE:-unknown}"
EMITTER_COUNT=$(pgrep -cx camera-box 2>/dev/null); EMITTER_COUNT="${EMITTER_COUNT:-0}"
STRAY_UNITS=$(systemctl list-units --all --plain --no-legend 'camera-box-burn-*' 2>/dev/null | awk '{print $1}' | paste -sd, -)
echo "SERVICE_ACTIVE=$SERVICE_ACTIVE EMITTER_COUNT=$EMITTER_COUNT STRAY_UNITS=$STRAY_UNITS"
REMOTE
}

# preflight_fleet_check_verdict OUTPUT_LINE -> "" (empty = PASS) or a human-readable reason string
# (FAIL) — the PURE decision over preflight_fleet_check_cmds's parsed output. No I/O; the caller
# does the ssh round trip and passes the resulting line here to decide pass/fail.
preflight_fleet_check_verdict() {
  local line="$1" service_active emitter_count stray_units
  service_active="$(printf '%s' "$line" | grep -oP 'SERVICE_ACTIVE=\K\S+' || true)"
  emitter_count="$(printf '%s' "$line" | grep -oP 'EMITTER_COUNT=\K[0-9]+' || true)"
  stray_units="$(printf '%s' "$line" | grep -oP 'STRAY_UNITS=\K\S*' || true)"
  if [ "${service_active:-unknown}" != "active" ]; then
    echo "camera-box.service is ${service_active:-unreachable/unknown}, not active"
    return 0
  fi
  if [ "${emitter_count:-0}" != "1" ]; then
    echo "expected exactly ONE camera-box emitter process, found ${emitter_count:-0}"
    return 0
  fi
  if [ -n "${stray_units:-}" ]; then
    echo "stray camera-box-burn-* unit(s) survived the preflight sweep: ${stray_units}"
    return 0
  fi
  echo "" # empty = PASS
}
