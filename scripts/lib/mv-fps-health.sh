#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (frozen-input-health.sh, network-reach-health.sh,
# obs-watchdog-decision.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (mv-fps-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/mv-fps-health.sh -- #1083 (continuation of #771): the SHARED, PURE decision core for
# the dev1-side MV-fps alert watchdog. No I/O, no ssh, no OBS, so it can be unit-tested exhaustively
# (mirrors scripts/lib/frozen-input-health.sh / network-reach-health.sh).
#
# WHY (#1083): #771 shipped the observability CORE -- vendored libobs `render_display()` emits
# `multiview-audit: monitor=N divisor=D rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per
# throttleable Multiview projector, `src/mv_audit.rs` parses+gates it, and the `mv-fps-gate` bin is
# an E2E-preflight / drift-guard consumer. But NOTHING read it LIVE, so a MV render collapse
# (measured live: imag monitor-3 to ~12fps for 5 min, strih 4K MV to 9-11fps under contention) went
# unalarmed unless someone ran `mv-fps-gate` by hand. The dev1-side watchdog reads each box's newest
# OBS log, runs `mv-fps-gate` (each projector's recent-window MEDIAN cadence, #1212), and pages on a
# sustained below-floor collapse.
# This lib turns the gate's exit code (+ a per-box OBS-log identity) into the watchdog's decisions,
# so they are correct regardless of any live rig. The confirm-counter + alert throttle stay the SAME
# shared obs_watchdog_confirm / obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh)
# the #391/#1001/#1052 siblings use -- no second alert mechanism.
#
# Source-only: pure functions, no side effects at source time.

# mv_fps_verdict <gate_exit> -> stdout: PASS | BELOW | UNKNOWN
#   Maps the `mv-fps-gate` bin's exit code to the watchdog's verdict:
#     0     -> PASS    (every projector's recent-window median cadence is at or above its floor, #1212)
#     1     -> BELOW   (>=1 projector's recent-window median fell below its own printed floor -> collapse)
#     other -> UNKNOWN (2 = no `multiview-audit:` line / read error, or an empty / non-numeric code)
#   UNKNOWN is fail-safe: a box whose OBS log could not be read, or that emits no audit line (OBS
#   down / a pre-#771 build / the ssh read failed), NEVER pages here -- the reachability (#1001) and
#   imag-OBS-liveness (#882) watchdogs own a genuinely-down box.
mv_fps_verdict() {
  case "${1:-}" in
    0) printf 'PASS\n' ;;
    1) printf 'BELOW\n' ;;
    *) printf 'UNKNOWN\n' ;;
  esac
}

# mv_fps_restart_reset <prev_logid> <curr_logid> -> stdout: reset=0 | reset=1
#   Autostart-aware freshness reset (the #391/#799 "counter reset -> None -> NEVER page" fail-safe,
#   adapted to a log-file identity since the audit line carries no cumulative counter). reset=1 iff
#   a PRIOR log identity existed AND the current one is a real, DIFFERENT identity -- i.e. OBS
#   restarted (a fresh `~/.config/obs-studio/logs/*.txt` / `%APPDATA%\obs-studio\logs\*.txt`), so a
#   fresh instance's warm-up below-floor read must not inherit the previous instance's pending
#   below-floor confirmation. A first-ever pass (empty prev) or a FAILED current read (empty curr)
#   never resets (there is nothing to reset, and a failed read is not a restart).
mv_fps_restart_reset() {
  local prev="${1:-}" curr="${2:-}"
  if [ -n "$prev" ] && [ -n "$curr" ] && [ "$prev" != "$curr" ]; then
    printf 'reset=1\n'
  else
    printf 'reset=0\n'
  fi
}

# mv_fps_recovery_decision <was_alerted 0|1> <now_pass 0|1> -> stdout: recover=0 | recover=1
#   Fire ONE recovery ("MV fps recovered") ping only when a box we actually PAGED for (was_alerted=1)
#   is PASS again (now_pass=1). A box that dipped but was never confirmed/paged, or one healthy all
#   along, never emits a recovery ping. Any value other than "1" counts as 0. Sibling of
#   frozen_input_recovery_decision / net_reach_recovery_decision.
mv_fps_recovery_decision() {
  local was_alerted="${1:-0}" now_pass="${2:-0}"
  if [ "$was_alerted" = "1" ] && [ "$now_pass" = "1" ]; then
    printf 'recover=1\n'
  else
    printf 'recover=0\n'
  fi
}

# mv_fps_alert_detail <box> <gate_output> -> stdout: one human line for the Discord alert body,
#   naming the box and every `FAIL monitor=…` line the gate printed (the collapsed projector(s),
#   their rendered_fps and floor). If the gate printed no FAIL line, falls back to a bare box name so
#   the body is never empty.
mv_fps_alert_detail() {
  local box="${1:-?}" gate_output="${2:-}" fails
  # Join every `FAIL …` line with a literal "; " separator. NOT `paste -sd'; '` -- paste's -d takes
  # a CYCLED delimiter LIST, so '; ' would alternate ';' and ' ' between fields, not separate with
  # "; ". awk emits the separator only BETWEEN records.
  fails="$(printf '%s\n' "$gate_output" | grep '^FAIL ' | sed 's/^FAIL //' \
    | awk 'NR>1{printf "; "} {printf "%s", $0} END{if (NR>0) printf "\n"}')"
  if [ -n "$fails" ]; then
    printf '%s MV render collapsed -- %s\n' "$box" "$fails"
  else
    printf '%s MV render collapsed below floor\n' "$box"
  fi
}
