#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (mirrors rig-test-dropin.sh /
# presenter-liveness-check.sh / marker-device-resolve.sh / rig-test-ledger.sh) -- string
# builders only; `set -euo pipefail` must NEVER be set here because sourcing this file mutates
# the CALLING script's (rig-mode.sh) own shell options.
#
# scripts/lib/event-assert.sh — #722: the REMOTE-command builders for the two fleet-wide checks
# the EVENT-mode CONTRACT's assert phase needs that have no existing tool: (1) per-box paint-
# process count + service-active + stray-unit status (item 1 + part of item 5), and (2) the
# artifacts-existing check (item 8). Everything else the assert phase needs already has a tool
# (scripts/obs_burn_filter.py check, scripts/obs_phase2.py record/stream-status/latency-check,
# scripts/set-ndi-mapping.py --verify-only, scripts/qr_screenshot_check.py) -- rig-mode.sh's
# event_mode_assert() (see rig-mode.sh) orchestrates all of it into one facts JSON for
# scripts/event_assert.py's pure decision + aggregation layer.
#
# WHY (2026-07-12 incident, #721): rig-mode event + a manual supervisor checklist BOTH said
# "clean" while a QR was live on air. "EVENT mode" must never again depend on someone
# remembering what to check, or on any single step's own exit code being trusted blindly.
#
# Sourced by scripts/rig-mode.sh.

# event_assert_fleet_check_cmds -> the REMOTE bash run on EACH cam box (over ssh) that reports,
# in one round trip:
#   PAINT_COUNT=<n>            — pgrep match count for any --paint-only process (item 1; the
#                                 pattern-based check survives a RENAMED binary, per #723's own
#                                 -f -- --paint-only convention, never a bare process-name match)
#   SERVICE_ACTIVE=<state>     — `systemctl is-active camera-box` (item 5)
#   STRAY_UNITS=<comma-list>   — any camera-box-burn-* systemd unit still present (item 5; empty
#                                 when clean)
# Pure string (no dev1-side vars to interpolate) — safe to call identically for every box.
event_assert_fleet_check_cmds() {
  cat <<'REMOTE'
PAINT_COUNT=$(pgrep -c -f -- --paint-only 2>/dev/null || echo 0)
SERVICE_ACTIVE=$(systemctl is-active camera-box 2>/dev/null || echo unknown)
STRAY_UNITS=$(systemctl list-units --all --plain --no-legend 'camera-box-burn-*' 2>/dev/null | awk '{print $1}' | paste -sd, -)
echo "PAINT_COUNT=$PAINT_COUNT SERVICE_ACTIVE=$SERVICE_ACTIVE STRAY_UNITS=$STRAY_UNITS"
REMOTE
}

# event_assert_artifacts_check_cmds PATH... -> the REMOTE (or local) bash that prints, one per
# line, every given PATH that STILL EXISTS (item 8 — test artifacts cleared). Empty output means
# everything is cleared. Works identically whether run over ssh on a cam box or directly on
# dev1 (the rig-active heartbeat lives on dev1; the pidfile/marker-log live on cam2) — the
# caller decides which paths to pass for which target.
event_assert_artifacts_check_cmds() {
  local p out=""
  out="for p in"
  for p in "$@"; do
    out="$out '$p'"
  done
  # `true` at the end: this is a REPORT, not a pass/fail test on its own (the decision happens
  # later, in event_assert.py) -- it must always exit 0 regardless of whether any path existed,
  # so a caller's own `set -e` never aborts just because nothing was found.
  out="$out; do [ -e \"\$p\" ] && echo \"\$p\"; done; true"
  printf '%s\n' "$out"
}
