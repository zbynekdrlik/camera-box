#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (not executed directly) — same convention as
# scripts/lib/obs-watchdog-decision.sh: no `set -euo pipefail` at top level so sourcing never
# changes the calling shell's own options as a side effect.
#
# scripts/lib/render-health-warmup.sh — #882/#1232 restart-and-settle for the imag render-health
# preflight (recording-e2e.sh [1/8]).
#
# WHY (#882, 2026-07-30): after a fresh OBS (re)start, NDI receivers are still locking and shaders
# are still warming up — the render-health sweep's FIRST window can measure a real, transient dip
# that has nothing to do with a genuine regression. Live incident: window 1/5 FAILED immediately
# after imag-nb's OBS was restarted at 09:21; the SAME gate binary measured 60.00fps/4.47ms/0%
# skip on the same box twenty minutes later. #882 shipped "window 1 never counts": a fixed ONE
# non-counting warm-up window, followed by 4 strict windows that must all pass.
#
# WHY the fixed single warm-up window stopped being enough (#1232, 2026-08-30): the settle time is
# NOT a constant — it scales with how much the restarted OBS has to re-lock. Live incident: E2E run
# 33308636791 failed at window 2/5 (49.28fps / 19.27ms / 18.6% skip) — a genuine settle overrun, not
# a capacity ceiling, confirmed ~4 minutes later at a clean 60.0fps/2.9-4.5ms with the same MV open.
# The #1143 ensure-rec-encoder step restarts imag's OBS right before these windows; with 5 NDI
# receivers the settle fit inside window 1 (6s), but with 7 active cameras (issue 1216 cam4+cam5
# re-entry) it routinely needs >=2 windows, so the fixed "only window 1 is warm-up" boundary
# measured a still-settling box as a strict, gate-aborting failure.
#
# THE FIX: generalize "window 1 doesn't count" into a settle-ADAPTIVE warm-up PHASE — the leading
# run of FAILED windows from the very start of the sweep up to (and including) the first PASS is
# the non-counting warm-up phase, bounded by a wall-clock budget (default 60s) so a box that is
# genuinely broken (never settles) still fails loudly instead of retrying forever. The window that
# achieves the first PASS ends the warm-up phase but — exactly like the old fixed window 1, whether
# it happened to pass or fail — does NOT itself count toward the required strict total; the strict
# windows are the ones that FOLLOW it (at least 4 of them, all must pass). This is a strict
# generalization: when the box settles inside window 1 (the common case), the phase machine behaves
# byte-for-byte like the old fixed rule (1 non-counting window + 4 strict windows that follow).
# Do NOT weaken the render-health verdict itself (src/render_budget.rs::classify stays exactly as
# strict, #882 principle unchanged) — this only decides whether a FAILED window counts toward
# aborting the sweep, and how long a still-settling box is tolerated before that tolerance itself
# is judged a real failure.
#
# Chosen over a fixed "minimum OBS uptime before the window opens" gate (rejected — no read of
# it possible in the first place: obs-websocket's GetStats has no process-uptime field) and over a
# fixed "N warm-up windows" magic number sized for 7 cameras today (rejected — the #1232 root
# cause IS that the number is not a constant: it varies with the active camera/receiver count and
# the box's own restart cost. "First window doesn't count" already self-adapted to *whatever* the
# real settle time turned out to be with no guessed number; this is that same self-adapting idea,
# extended so the boundary can land on window 2, 3, ... N instead of being pinned to window 1 —
# with a wall-clock budget as the only new magic number, and that number bounds a FAILURE mode
# (a box that never settles), not the expected happy path).
#
# Pure so it unit-tests on default features (Tier-0) — sourcing this file performs no I/O.

# render_health_phase_outcome <rc> <first_pass_seen> <elapsed_s> <budget_s>
#   rc              — the render-budget-gate.py exit code for THIS window (0 = pass, nonzero = fail)
#   first_pass_seen — 1 if some EARLIER window in this sweep already PASSED (i.e. the caller is
#                     already past the warm-up boundary and into the strict phase), else 0
#   elapsed_s       — wall-clock seconds elapsed since the sweep's very first window started
#   budget_s        — the settle wall-clock budget (RENDER_HEALTH_SETTLE_BUDGET_S); once elapsed_s
#                     reaches this while still warming up, a further failure is no longer tolerated
#
#   -> stdout (one `key=value` per line):
#      outcome=PASS|WARMUP|FAIL
#      first_pass_seen=0|1   — the NEW state to carry into the NEXT window's call
#      counts_as_strict=0|1  — whether THIS window's PASS counts toward the required strict total
#
#   PASS   — rc was 0.
#            * first_pass_seen (input) was already 1 → a strict-phase window passed:
#              counts_as_strict=1.
#            * first_pass_seen (input) was 0 → this is the window that ENDS the warm-up phase (the
#              first PASS of the sweep). Like the old fixed window 1, it never counts toward the
#              strict total regardless of it having passed: counts_as_strict=0.
#            Either way first_pass_seen is latched to 1 for every later call.
#   WARMUP — rc != 0, first_pass_seen (input) was 0 (still warming up, no PASS yet), and
#            elapsed_s < budget_s: a non-counting, tolerated warm-up failure — never aborts.
#   FAIL   — any OTHER failing case: rc != 0 with first_pass_seen (input) already 1 (a genuine,
#            never-tolerated strict-window failure — exactly as strict as before), OR rc != 0 while
#            still warming up but elapsed_s has reached/exceeded budget_s (a sustained failure past
#            the whole settle budget aborts loudly, exactly like today — never silently tolerated
#            forever).
render_health_phase_outcome() {
  local rc="${1:-1}" seen="${2:-0}" elapsed="${3:-0}" budget="${4:-0}"
  case "$rc" in *[!0-9]* | "") rc=1 ;; esac
  case "$seen" in 1) seen=1 ;; *) seen=0 ;; esac
  case "$elapsed" in *[!0-9]* | "") elapsed=0 ;; esac
  case "$budget" in *[!0-9]* | "") budget=0 ;; esac

  if [ "$rc" -eq 0 ]; then
    if [ "$seen" -eq 1 ]; then
      printf 'outcome=PASS\nfirst_pass_seen=1\ncounts_as_strict=1\n'
    else
      printf 'outcome=PASS\nfirst_pass_seen=1\ncounts_as_strict=0\n'
    fi
    return 0
  fi

  if [ "$seen" -eq 1 ]; then
    printf 'outcome=FAIL\nfirst_pass_seen=1\ncounts_as_strict=0\n'
    return 0
  fi

  if [ "$elapsed" -lt "$budget" ]; then
    printf 'outcome=WARMUP\nfirst_pass_seen=0\ncounts_as_strict=0\n'
  else
    printf 'outcome=FAIL\nfirst_pass_seen=0\ncounts_as_strict=0\n'
  fi
}
