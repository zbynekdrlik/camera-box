#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (not executed directly) — same convention as
# scripts/lib/obs-watchdog-decision.sh: no `set -euo pipefail` at top level so sourcing never
# changes the calling shell's own options as a side effect.
#
# scripts/lib/render-health-warmup.sh — #882 restart-and-settle for the imag render-health
# preflight (recording-e2e.sh [1/8]).
#
# WHY: after a fresh OBS (re)start, NDI receivers are still locking and shaders are still warming
# up — the render-health sweep's FIRST window can measure a real, transient dip that has nothing
# to do with a genuine regression. Live incident (2026-07-30, #882): window 1/5 FAILED
# immediately after imag-nb's OBS was restarted at 09:21; the SAME gate binary measured
# 60.00fps/4.47ms/0% skip on the same box twenty minutes later. Do NOT weaken the render-health
# verdict itself (src/render_budget.rs::classify stays exactly as strict) — this only decides
# whether a FAILED window counts toward aborting the sweep. Window 1 is treated as a non-counting
# warm-up: a failure there is logged but does not abort; windows 2..N stay exactly as strict as
# before (a sustained failure across the real windows still fails the gate, loudly).
#
# Chosen over a fixed "minimum OBS uptime before the window opens" gate because obs-websocket's
# GetStats has no process-uptime field to read — "first window doesn't count" self-adapts to
# whatever the real settle time turns out to be, with no guessed magic-number threshold.
#
# Pure so it unit-tests on default features (Tier-0) — sourcing this file performs no I/O.

# render_health_window_outcome <window_index> <rc>
#   window_index — 1-based index of the window just sampled (1 = the warm-up window)
#   rc           — the render-budget-gate.py exit code for THAT window (0 = pass, nonzero = fail)
#   -> stdout: outcome=PASS|WARMUP|FAIL
#   PASS   — rc was 0, regardless of window index.
#   WARMUP — window_index == 1 AND rc != 0: a non-counting warm-up failure, never aborts.
#   FAIL   — any OTHER window (2..N) with rc != 0: a genuine failure, aborts the sweep.
render_health_window_outcome() {
  local idx="${1:-0}" rc="${2:-0}"
  case "$idx" in *[!0-9]* | "") idx=0 ;; esac
  case "$rc" in *[!0-9]* | "") rc=1 ;; esac

  if [ "$rc" -eq 0 ]; then
    printf 'outcome=PASS\n'
  else
    printf 'outcome=FAIL\n'
  fi
}
