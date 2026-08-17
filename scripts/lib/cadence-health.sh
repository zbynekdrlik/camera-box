#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (frozen-input-health.sh, network-reach-health.sh,
# obs-watchdog-decision.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (cadence-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/cadence-health.sh -- #794: the SHARED, PURE decision core for the dev1-side NON-60
# SOURCE-CADENCE alert watchdog. A fourth sibling of the dev1 alert-watchdog family (network-reach
# issue 1001 / frozen-input issue 1052 / bundle-state issue 732). No I/O, no ssh, no OBS, so it can
# be unit-tested exhaustively (mirrors scripts/lib/frozen-input-health.sh).
#
# WHY (#794, live 2026-07-17): a camera set to 50 fps feeding the 60 fps chain went undetected for
# weeks. Every fps counter reads the CANVAS rate (60/30), and a grabber that upconverts 50->60 by
# DUPLICATION delivers a genuine 60 NDI frames/s -- so the canonical figures are all clean. The ONE
# per-source signal that reflects the true NDI DELIVERY rate is the genlock-fifo `received=`
# cumulative counter on the receiver (strih, for camera inputs): a camera genuinely delivering
# 50/43 fps advances `received=` at 50/43 per second. The dev1-side watchdog samples the newest
# `received=` PLUS the audit line's OWN log timestamp per source each pass, PERSISTS them, and turns
# two consecutive samples into a measured fps -- this lib is the pure kernel of that turn.
#
# THE LOAD-BEARING DECISION -- the issue-797 "phantom 50.1 fps" avoidance: the rate's numerator
# (received delta) AND its denominator (elapsed seconds) BOTH come from the SAME two matched audit
# lines' OWN log timestamps -- NEVER a wall-clock / pass-interval divisor. The audit line appends
# every ~5.017 s; a naive `received-delta / 6s-wall-sleep` spans ~one tick (+301 frames at a true
# 60) and prints 301/6 = 50.17 -- a phantom "50" at EVERY true-60 source (the retracted "OBS caps at
# 50" thesis that cost two days). cadence_measure_fps divides the received delta by the two lines'
# OWN elapsed seconds, so a single-tick window reads 301/5.017 = ~60, not ~50.
#
# The confirm-counter + alert throttle stay the SAME shared obs_watchdog_confirm /
# obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh) the sibling watchdogs use.
#
# Source-only: pure functions, no side effects at source time.

# cadence_ts_to_seconds <HH:MM:SS[.mmm][:]> -> stdout: seconds-of-day as a float, or EMPTY when the
#   input does not strictly parse as a clock time. Accepts the OBS log prefix form with a trailing
#   ':' (e.g. `14:00:05.017:`). Empty output upstream means "no timestamp" -> UNMEASURABLE, never a
#   guessed value.
cadence_ts_to_seconds() {
  local ts="${1:-}"
  ts="${ts%:}" # strip the OBS-log-prefix trailing colon, if any
  case "$ts" in
    [0-9][0-9]:[0-9][0-9]:[0-9][0-9] | [0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9]*) : ;;
    *) return 0 ;; # unparseable -> empty
  esac
  LC_ALL=C awk -v t="$ts" 'BEGIN{
    n = split(t, a, ":");
    if (n != 3) { exit 0 }
    h = a[1] + 0; m = a[2] + 0; s = a[3] + 0;
    if (h > 23 || m > 59 || s >= 60) { exit 0 }  # not a real clock time -> empty
    printf "%.3f", h * 3600 + m * 60 + s;
  }'
}

# cadence_measure_fps <prev_ts> <prev_received> <curr_ts> <curr_received>
#   -> stdout: `fps=<F> window_s=<W> advanced=<0|1>` (all present) when a rate could be measured, or
#      `fps= window_s= advanced=` (all empty) when it could NOT be (reseed upstream, never page).
#   The rate is `(curr_received - prev_received) / (curr_seconds - prev_seconds)` where BOTH seconds
#   come from the audit lines' OWN timestamps -- the issue-797 phantom-50 avoidance (see the header).
#     unmeasurable when: either received value is not a non-negative integer; curr_received <
#       prev_received (the cumulative counter reset -- OBS restarted); either timestamp does not
#       parse; the window is <= 0 (after a date-less midnight-wrap correction) or larger than
#       CADENCE_MAX_WINDOW_S (a stale prev sample from a watchdog that was down -- reseed, never a
#       rate over an hours-old span).
#     advanced=0 (received delta == 0) is a genuine measurement of a FROZEN source (issue-1052's
#       job) -- cadence_classify turns advanced != 1 into UNKNOWN, never a "wrong 0 fps" page.
cadence_measure_fps() {
  local prev_ts="${1:-}" prev_recv="${2:-}" curr_ts="${3:-}" curr_recv="${4:-}"
  local unmeasurable='fps= window_s= advanced='

  case "$prev_recv" in '' | *[!0-9]*) printf '%s\n' "$unmeasurable"; return 0 ;; esac
  case "$curr_recv" in '' | *[!0-9]*) printf '%s\n' "$unmeasurable"; return 0 ;; esac
  # Cumulative counter reset (OBS restarted) -> reseed, never a rate.
  if [ "$curr_recv" -lt "$prev_recv" ]; then printf '%s\n' "$unmeasurable"; return 0; fi

  local prev_sec curr_sec
  prev_sec="$(cadence_ts_to_seconds "$prev_ts")"
  curr_sec="$(cadence_ts_to_seconds "$curr_ts")"
  if [ -z "$prev_sec" ] || [ -z "$curr_sec" ]; then printf '%s\n' "$unmeasurable"; return 0; fi

  local dreceived=$((curr_recv - prev_recv))
  LC_ALL=C awk \
    -v ps="$prev_sec" -v cs="$curr_sec" -v dr="$dreceived" \
    -v maxw="${CADENCE_MAX_WINDOW_S:-21600}" 'BEGIN{
      w = cs - ps;
      if (w < 0) w += 86400;                 # date-less OBS log: a window straddling midnight
      if (w <= 0 || w > maxw + 0) { print "fps= window_s= advanced="; exit 0 }
      adv = (dr > 0) ? 1 : 0;
      printf "fps=%.3f window_s=%.3f advanced=%d\n", dr / w, w, adv;
    }'
}

# cadence_classify <fps> <window_s> <advanced 0|1> <expected_fps> <tolerance_fps> <min_window_s>
#                  <expected_live 0|1> <box_reachable 0|1>
#   -> stdout: OK | WRONG | UNKNOWN | SKIP
#     SKIP    -- out of scope this pass: the source is not expected live, OR the box is not reachable
#                (issue-1001 already owns that page). Checked FIRST. Any flag != "1" is out-of-scope.
#     UNKNOWN -- nothing trustable to judge: unmeasurable fps/window, a source not advancing
#                (advanced != 1 -- a frozen source is issue-1052's job), or a window shorter than the
#                minimum trustable span. Reseed; NEVER page on a shaky reading.
#     WRONG   -- |fps - expected_fps| > tolerance_fps (band edge inclusive): the source is delivering
#                a sustained non-60 cadence.
#     OK      -- inside the tolerance band.
cadence_classify() {
  local fps="${1:-}" window_s="${2:-}" advanced="${3:-}" expected="${4:-60}" tol="${5:-3}" \
    min_window="${6:-60}" expected_live="${7:-0}" box_reachable="${8:-0}"

  if [ "$expected_live" != "1" ] || [ "$box_reachable" != "1" ]; then printf 'SKIP\n'; return 0; fi

  case "$fps" in '' | *[!0-9.]*) printf 'UNKNOWN\n'; return 0 ;; esac
  case "$window_s" in '' | *[!0-9.]*) printf 'UNKNOWN\n'; return 0 ;; esac
  if [ "$advanced" != "1" ]; then printf 'UNKNOWN\n'; return 0; fi

  local win_ok
  win_ok="$(LC_ALL=C awk -v w="$window_s" -v m="$min_window" 'BEGIN{print (w+0 >= m+0) ? 1 : 0}')"
  if [ "$win_ok" != "1" ]; then printf 'UNKNOWN\n'; return 0; fi

  local wrong
  wrong="$(LC_ALL=C awk -v f="$fps" -v e="$expected" -v t="$tol" \
    'BEGIN{d = f - e; if (d < 0) d = -d; print (d > t + 0) ? 1 : 0}')"
  if [ "$wrong" = "1" ]; then printf 'WRONG\n'; else printf 'OK\n'; fi
}

# cadence_recovery_decision <was_alerted 0|1> <now_ok 0|1> -> stdout: recover=0 | recover=1
#   Fire ONE recovery ("cadence back to ~60") ping only when a source we actually PAGED for
#   (was_alerted=1) returns to an OK cadence (now_ok=1). Sibling of frozen_input_recovery_decision.
#   Any value other than "1" counts as 0.
cadence_recovery_decision() {
  local was_alerted="${1:-0}" now_ok="${2:-0}"
  if [ "$was_alerted" = "1" ] && [ "$now_ok" = "1" ]; then
    printf 'recover=1\n'
  else
    printf 'recover=0\n'
  fi
}

# cadence_alert_detail <source> <measured_fps> <expected_fps> -> stdout: one human line for the
#   Discord alert body, naming the source and its measured-vs-expected cadence.
cadence_alert_detail() {
  local source="${1:-?}" measured="${2:-?}" expected="${3:-60}"
  printf "%s: source cadence measured %s fps, expected %s fps (camera likely set to a non-60 rate)\n" \
    "$source" "$measured" "$expected"
}
