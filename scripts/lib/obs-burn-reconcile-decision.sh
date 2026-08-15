#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no unconditional side effects at source
# time), mirrors scripts/lib/obs-watchdog-decision.sh / rig-lease.sh / rig-heartbeat.sh which are
# also `set -euo pipefail`-free for the same reason (sourcing must never mutate the CALLING
# script's own shell options -- see .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/obs-burn-reconcile-decision.sh -- the PURE heart of the #1060 dev1-side
# fresh-OBS-start burn-reconcile watchdog. No I/O, no ssh, no OBS, no MCP -- pure so it can be
# unit-tested exhaustively (mirrors scripts/lib/obs-watchdog-decision.sh).
#
# WHY (#1060): issue 1057 closed the burn-resurrection window for the DELIBERATE dev1-driven
# relaunch (launch-obs-genlock.sh's PLAN now directs a post-launch obs_burn_filter.py sweep-off).
# Still open -- the UNATTENDED strih/stream OBS start paths (box boot autostart, NL_STARTUP.ahk
# obs64 auto-respawn, the issue-411 self-heal Task-Scheduler relaunch, all reusing
# launch-obs-genlock.sh's emitted PowerShell which never touches the burn). On any of those a saved
# genlock_burn=true reloads and renders the QR measurement burn onto the LIVE program, and the
# Windows box has no on-box python/WS client to clear it locally.
#
# THE LOAD-BEARING DISCRIMINATOR: a FRESH OBS START, never merely "a burn is present". A persistent
# TEST-mode burn on strih/stream is a LEGITIMATE, deliberately-persistent operator state (the rig
# "TEST mode must stay alive" convention: QR visible on the rig) whose rig-active heartbeat (#281)
# goes STALE after ~10 min while the burn should remain -- so "burn present + stale heartbeat" is
# NOT a leak, it is idle TEST mode, and clearing it would be a false-clear of deliberate operator
# state. Only at a fresh OBS restart is a reloaded saved burn definitively a resurrection (no
# gate/operator has had the chance to set one at the instant OBS comes up; a gate sets its burn
# only AFTER launch+verify). And even at a fresh start we DEFER while a live gate/TEST harness is
# coordinating (a fresh #281 heartbeat OR a held #830 rig lease) so the watchdog never clears a
# burn a live gate deliberately set mid-run.
#
# Source-only: defines obs_burn_reconcile_is_fresh_start() + obs_burn_reconcile_decide(); runs
# nothing.

# obs_burn_reconcile_is_fresh_start <prev_render_total_frames> <cur_render_total_frames>
#   -> exit 0 (a fresh OBS start since the last pass) / 1 (same OBS session, or undetermined).
#
#   GetStats.renderTotalFrames is monotone since OBS process start and RESETS on restart, so a DROP
#   vs the persisted baseline = a restart since the last pass. Rules:
#     - prev unknown / non-numeric (first pass, or a lost state file) -> FRESH (reconcile once; the
#       full decision below still only SWEEPs when uncoordinated AND a burn renders, so this is
#       safe -- it also catches a box that already leaked a burn before the watchdog first ran).
#     - cur non-numeric / empty (an unreadable probe this pass) -> NOT fresh (a bad read can never
#       PROVE a restart; the watchdog treats an unreadable probe as "nothing to decide this pass"
#       separately, and must not advance its baseline off it).
#     - both numeric: cur < prev -> FRESH (counter reset); cur >= prev -> NOT fresh (same session).
obs_burn_reconcile_is_fresh_start() {
  local prev="${1:-}" cur="${2:-}"
  # A bad/empty CURRENT read cannot prove a restart.
  case "$cur" in
    "" | *[!0-9]*) return 1 ;;
  esac
  # An unknown/corrupt PREVIOUS baseline -> reconcile once (treat as fresh).
  case "$prev" in
    "" | *[!0-9]*) return 0 ;;
  esac
  [ "$cur" -lt "$prev" ]
}

# obs_burn_reconcile_decide <fresh_start 0|1> <coordinated 0|1> <burn_present 0|1>
#   -> stdout: exactly one verdict word:
#        NOOP  -- not a fresh start: persistent state (incl. a legitimate TEST-mode burn) untouched.
#        DEFER -- a fresh start, but a live gate/TEST harness is coordinating (it owns burn state).
#        SWEEP -- a fresh start, uncoordinated, and a burn renders: a resurrected saved burn -> clear.
#        CLEAN -- a fresh start, uncoordinated, no burn renders: nothing to clear (log only).
#   PURE: no side effects. Any non-1 arg is treated as its safe/false reading (0).
obs_burn_reconcile_decide() {
  local fresh="${1:-0}" coordinated="${2:-0}" burn_present="${3:-0}"
  if [ "$fresh" != "1" ]; then
    printf 'NOOP\n'
    return 0
  fi
  if [ "$coordinated" = "1" ]; then
    printf 'DEFER\n'
    return 0
  fi
  if [ "$burn_present" = "1" ]; then
    printf 'SWEEP\n'
  else
    printf 'CLEAN\n'
  fi
  return 0
}
