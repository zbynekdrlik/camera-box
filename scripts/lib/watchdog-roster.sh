#!/usr/bin/env bash
# scripts/lib/watchdog-roster.sh -- issue 1319: the ONE source of truth for the dev1 `--user`
# production-critical alert-watchdog TIMERS the development-handover check
# (scripts/rig-dev-handover-check.sh item 16) verifies are enabled + active + fired recently.
#
# WHY a roster lib: the owner's 15.9. complaint was that av-step + avsync-lineup timers had NEVER
# been installed on dev1 and NOTHING reported it. The `_PRODUCTION_CRITICAL_TIME_BUCKETED` set in
# tests/python/test_notify_dedup_key_sweep_1206.py is a TEST artefact (it asserts a notify-dedup
# property), NOT a runtime roster -- so this file is the runtime source of truth the check reads.
# Keep the two lists conceptually aligned, but this one is authoritative for the handover check.
#
# Format: each WATCHDOG_TIMERS entry is "<timer-unit>:<scope>" where scope is:
#   core -- always required
#   imag -- imag-nb scoped; ignorable once imag is retired (issue 1316). Marked here so the check
#           can drop it via RDH_IMAG_RETIRED / classify_watchdogs(imag_retired=True) without a code
#           change. imag-nb currently returns next year, so it is LIVE (counted) today.
# This file is SOURCED (no `set -euo pipefail`, no execution) -- it only declares the array.

# shellcheck disable=SC2034  # consumed by the sourcing orchestrator, not this file
WATCHDOG_TIMERS=(
  dantesync-clock-alert-watchdog.timer:core     # issue 1307 -- dante-clock loss / DNS / GM-move
  network-reach-alert-watchdog.timer:core       # issue 1001 -- strih/stream unreachable
  bundle-state-alert-watchdog.timer:core        # issue 732  -- :8899 bundle-state server down
  obs-liveness-watchdog.timer:core              # issue 391  -- broadcast-OBS render wedge
  audio-lag-alert-watchdog.timer:core           # issue 1226 -- OBS audio-timeline lag / band drift
  measurement-audio-alert-watchdog.timer:core   # issue 1310 -- mbc measurement-audio digital silence
  genlock-lock-alert-watchdog.timer:core        # issue 1299 -- fleet genlock LOCK facet
  render-freeze-alert-watchdog.timer:core       # issue 1320 -- PROGRAM render freeze / relock storm
  av-step-alert-watchdog.timer:core             # issue 1319 -- absolute A/V-offset STEP + BAND arm
  avsync-lineup-alert-watchdog.timer:core       # issue 1319 -- A/V line-up
  avsync-heartbeat-alert-watchdog.timer:core    # issue 812  -- A/V-sync heartbeat stale
  frozen-input-alert-watchdog.timer:core        # issue 1052 -- frozen NDI input
  frozen-strih-input-alert-watchdog.timer:core  # issue 1052 -- frozen strih input
  optical-chain-alert-watchdog.timer:core       # issue 860  -- cam2 optical-injection dead
  splitter-port-alert-watchdog.timer:core       # issue 739  -- HDMI splitter-port no-signal
  grabber-stuck-alert-watchdog.timer:core       # issue 1128 -- fast-capture grabber STUCK
  imag-obs-alert-watchdog.timer:imag            # issue 882  -- imag OBS down / drift (imag-scoped)
)
