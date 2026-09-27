#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + defaults, no top-level statements) --
# sourced by scripts/lib/dantesync-clock-discipline.sh, whose callers own their own strictness; a
# `set -euo pipefail` here would leak into every caller (the scripts/lib/obs-fleet.sh convention).
#
# scripts/lib/dantesync-date-daily.sh -- issue 1372: the dantesync 1.12.0 NIGHTLY date mode.
#
# WHY: dantesync 1.12.0 (PR 123) corrects the fleet date ONCE A NIGHT by default. A master whose
# date_correction_mode is "daily" corrects nothing by day, so its date_offset_error_ms grows to a
# day's drift (~1.5 s on the rig) until one coordinated step at the nightly window (02:00 UTC). It
# is graded on its schedule -- the next window (date_daily_next_utc) within (now, now + 24 h 30 min],
# the last nightly step (date_daily_last_step_ts) within the last 26 h or null (none seen since the
# master started, a NOTE) -- and |date_offset_error_ms| <= DATE_MASTER_DAILY_BOUND_MS + margin
# (design: issue 1372 comment 5850538767, Approach 1).
#
# The ONE date-master verdict (date_master_verdict / date_master_effective_bound_us /
# date_master_check / journal_date_grade_from_step in dantesync-clock-discipline.sh) routes a
# daily-mode master here. The python twin is scripts/dantesync_fleet.py (date_master_mode_class /
# _date_master_daily_verdict), both pinned by tests/fixtures/dantesync_clock_discipline_1372.tsv.
# The pure decisions take "now" as an argument; date_master_now_s is the ONE clock read.
#
# DEPENDS on dantesync-clock-discipline.sh's parsers (resolved at CALL time): _pipe_json_raw_value,
# _pipe_json_number_raw, _ms_to_us, _positive_bound_us; and clock-offset-guard.sh's abs_int.

# DATE_MASTER_DAILY_BOUND_MS -- the ONE bound (ms, before the margin) a dantesync 1.12.0 date master
# in date_correction_mode "daily" is graded on. It corrects the fleet date once a night, so by day
# the error is a day's drift (~1.5 s on the rig at +17.6 ppm); 3000 ms sits above that and below
# dantesync's own 5000 ms emergency cap (daily_emergency_ms), past which it steps at once.
# DANTESYNC_DATE_DAILY_BOUND_MS overrides it (a positive plain decimal, else unreadable).
# shellcheck disable=SC2034  # read by the scripts that source this lib
DATE_MASTER_DAILY_BOUND_MS="${DANTESYNC_DATE_DAILY_BOUND_MS:-3000}"

# The nightly schedule's windows (seconds), fixed by the design (issue 1372 comment 5850538767):
# the next window must lie within (now, now + 24 h + 30 min] -- one night ahead plus the window's
# own 30 min UTC wait; the last nightly step within the last 26 h -- a day plus that wait plus
# slack; and up to 30 min AHEAD of now, because dantesync records a nightly step when it announces
# it and the step lands two leads later.
DATE_DAILY_NEXT_MAX_S=88200
DATE_DAILY_LAST_MAX_AGE_S=93600
DATE_DAILY_LAST_MAX_AHEAD_S=1800
# How long dantesync keeps a nightly window open while it has no fresh UTC reading (DAILY_WINDOW_NS):
# a window this recent that has not stepped is waiting for UTC; an older one is a stuck scheduler.
DATE_DAILY_WINDOW_S=1800

# --- the dantesync 1.12.0 NIGHTLY date mode (date_correction_mode "daily") --------------------

# date_master_now_s -> "now" (Unix seconds) for the nightly-schedule grade: DANTESYNC_DATE_NOW_S when
# set (tests pin it), else the wall clock. The ONLY place the daily grade reads the clock; the
# decisions below take it as an argument.
date_master_now_s() {
  if [ -n "${DANTESYNC_DATE_NOW_S:-}" ]; then
    printf '%s' "$DANTESYNC_DATE_NOW_S"
  else
    date +%s 2>/dev/null || true
  fi
}

# date_master_daily_bound_us -> DATE_MASTER_DAILY_BOUND_MS in integer us; "" when it is unreadable
# (a typo in DANTESYNC_DATE_DAILY_BOUND_MS -- every consumer then grades the date UNKNOWN).
date_master_daily_bound_us() {
  _positive_bound_us "$DATE_MASTER_DAILY_BOUND_MS"
}

# date_master_mode_text TEXT -> `date_correction_mode` as printed in a line: the string without its
# quotes, or the raw token of a non-string value; "" when absent.
date_master_mode_text() {
  local mode
  mode="$(_pipe_json_raw_value "$1" date_correction_mode)"
  mode="${mode#\"}"
  printf '%s' "${mode%\"}"
}

# date_master_mode_class TEXT -> daily | other | unknown, from `date_correction_mode`:
#   "daily"                                -> daily   (dantesync 1.12.0's nightly step)
#   absent / null / "" / "micro"           -> other   (1.12.0 micro mode, a follower, a pre-1.12 blob:
#                                                      graded exactly as before this mode existed)
#   any other string, or a non-string      -> unknown (the python twin does the same)
date_master_mode_class() {
  case "$(_pipe_json_raw_value "$1" date_correction_mode)" in
    '"daily"') printf 'daily' ;;
    ''|null|'""'|'"micro"') printf 'other' ;;
    *) printf 'unknown' ;;
  esac
}

# _date_daily_utc_epoch VALUE -> the Unix second of an RFC 3339 UTC second `YYYY-MM-DDTHH:MM:SSZ`
# (the one shape dantesync's format_utc_rfc3339 writes); "" for any other shape or an impossible date
# (GNU date refuses 2026-02-30, the python twin's strptime too). Pure: parses, never reads the clock.
_date_daily_utc_epoch() {
  local v="$1" epoch
  [[ $v =~ ^[1-9][0-9]{3}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z$ ]] \
    || { printf ''; return 0; }
  epoch="$(date -u -d "$v" +%s 2>/dev/null || true)"
  [[ $epoch =~ ^[0-9]{1,15}$ ]] && printf '%s' "$epoch"
  return 0
}

# _date_daily_next_state TEXT NOW_S -> ok | missing | past | far | unknown for `date_daily_next_utc`,
# the start of the next nightly window on the fleet wall (the open window while it has not stepped):
#   missing -- absent or null (no schedule)          past -- at or before NOW_S
#   far     -- more than DATE_DAILY_NEXT_MAX_S ahead  unknown -- not a string, not RFC 3339 UTC, or
#   ok      -- within (NOW_S, NOW_S + 24 h 30 min]               NOW_S not a plain integer
_date_daily_next_state() {
  local raw epoch now="$2"
  [[ $now =~ ^[0-9]{1,15}$ ]] || { printf 'unknown'; return 0; }
  raw="$(_pipe_json_raw_value "$1" date_daily_next_utc)"
  case "$raw" in
    ''|null) printf 'missing'; return 0 ;;
    \"*\") raw="${raw#\"}"; raw="${raw%\"}" ;;
    *) printf 'unknown'; return 0 ;;
  esac
  epoch="$(_date_daily_utc_epoch "$raw")"
  if [ -z "$epoch" ]; then
    printf 'unknown'
  elif [ "$epoch" -le "$((10#$now))" ]; then
    printf 'past'
  elif [ "$((epoch - 10#$now))" -gt "$DATE_DAILY_NEXT_MAX_S" ]; then
    printf 'far'
  else
    printf 'ok'
  fi
}

# _date_daily_last_state TEXT NOW_S -> ok | none | stale | ahead | unknown for
# `date_daily_last_step_ts`, the fleet-wall second the last nightly step landed on:
#   none    -- null: no nightly step since the master started (a NOTE, not a fault)
#   stale   -- more than DATE_DAILY_LAST_MAX_AGE_S (26 h) before NOW_S
#   ahead   -- more than DATE_DAILY_LAST_MAX_AHEAD_S (30 min) after NOW_S (an announced step lands
#              two leads after it is recorded, so a few seconds ahead is normal; this far is not)
#   unknown -- absent, not a non-negative integer, or NOW_S not a plain integer
# A JSON `-0` is the integer 0 (python's json reads it so), never an unreadable value.
_date_daily_last_state() {
  local raw now="$2" age
  [[ $now =~ ^[0-9]{1,15}$ ]] || { printf 'unknown'; return 0; }
  raw="$(_pipe_json_raw_value "$1" date_daily_last_step_ts)"
  [ "$raw" = null ] && { printf 'none'; return 0; }
  [ "$raw" = -0 ] && raw=0
  [[ $raw =~ ^[0-9]{1,15}$ ]] || { printf 'unknown'; return 0; }
  age=$((10#$now - 10#$raw))
  if [ "$age" -gt "$DATE_DAILY_LAST_MAX_AGE_S" ]; then
    printf 'stale'
  elif [ "$age" -lt "-$DATE_DAILY_LAST_MAX_AHEAD_S" ]; then
    printf 'ahead'
  else
    printf 'ok'
  fi
}

# _date_daily_schedule_verdict TEXT NOW_S -> ok | out | unknown: is the nightly scheduler alive?
# out when the next window is missing / past / far or the last step is stale / ahead (a read, wrong
# field wins); else unknown when either is unreadable; else ok (a null last step is ok).
_date_daily_schedule_verdict() {
  local next last
  next="$(_date_daily_next_state "$1" "$2")"
  last="$(_date_daily_last_state "$1" "$2")"
  case "$next/$last" in
    missing/*|past/*|far/*|*/stale|*/ahead) printf 'out' ;;
    unknown/*|*/unknown) printf 'unknown' ;;
    *) printf 'ok' ;;
  esac
}

# _date_master_daily_verdict TEXT MARGIN_US DAILY_BOUND_MS NOW_S -> ok | out | unknown for a date
# master in date_correction_mode "daily" (dantesync 1.12.0). PURE: NOW_S is an argument.
#   out     -- the schedule is out (_date_daily_schedule_verdict), or |date_offset_error_ms| >
#              DAILY_BOUND_MS + MARGIN_US (a read, wrong field wins over an unreadable one)
#   unknown -- a schedule field, the error or the daily bound unreadable
#   ok      -- a live schedule and the error within the daily bound + margin
# date_correction_falling_behind / date_micro_paused are not read: daily mode never raises them.
_date_master_daily_verdict() {
  local text="$1" margin="$2" daily="$3" now="$4" sched err_us bound_us err_v
  sched="$(_date_daily_schedule_verdict "$text" "$now")"
  err_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_offset_error_ms)")"
  bound_us="$(_positive_bound_us "$daily")"
  # date_master_check hands MARGIN_US straight here, so it is validated here too (bash arithmetic
  # on "abc" would print errors, not a verdict).
  if [[ ! $margin =~ ^[0-9]{1,15}$ ]] || [ -z "$err_us" ] || [ -z "$bound_us" ]; then
    err_v=unknown
  elif [ "$(abs_int "$err_us")" -le $((bound_us + 10#$margin)) ]; then
    err_v=ok
  else
    err_v=out
  fi
  case "$sched/$err_v" in
    out/*|*/out) printf 'out' ;;
    unknown/*|*/unknown) printf 'unknown' ;;
    *) printf 'ok' ;;
  esac
}

# _date_daily_schedule_problems TEXT NOW_S -> the "; "-joined reasons a daily-mode master's nightly
# schedule is not OK (empty when it is). It reads the SAME states _date_daily_schedule_verdict
# decides on, so the text never disagrees with the verdict.
_date_daily_schedule_problems() {
  local text="$1" now="$2" out="" next last nraw lraw ago
  if [[ ! $now =~ ^[0-9]{1,15}$ ]]; then
    printf 'now=%s (DANTESYNC_DATE_NOW_S or the wall clock) unreadable' "${now:-<empty>}"
    return 0
  fi
  nraw="$(_pipe_json_raw_value "$text" date_daily_next_utc)"
  nraw="${nraw#\"}"; nraw="${nraw%\"}"
  lraw="$(_pipe_json_raw_value "$text" date_daily_last_step_ts)"
  next="$(_date_daily_next_state "$text" "$now")"
  last="$(_date_daily_last_state "$text" "$now")"
  case "$next" in
    missing) out="${out}; no next nightly window (date_daily_next_utc ${nraw:-absent})" ;;
    past)
      # The open window reads "past" until the scheduler steps: normally seconds, up to the 30 min
      # the window stays open without UTC. Longer ago, the scheduler did not advance at all.
      ago=$((10#$now - $(_date_daily_utc_epoch "$nraw")))
      if [ "$ago" -le "$DATE_DAILY_WINDOW_S" ]; then
        out="${out}; next nightly window ${nraw} is open and has not stepped yet (waiting for UTC? the window stays open up to 30 min)"
      else
        out="${out}; next nightly window ${nraw} is ${ago} s in the past: the nightly scheduler did not advance"
      fi ;;
    far) out="${out}; next nightly window ${nraw} is more than 24 h 30 min ahead" ;;
    unknown) out="${out}; date_daily_next_utc=${nraw:-<absent>} unreadable" ;;
  esac
  case "$last" in
    stale) out="${out}; last nightly step at ${lraw} is not within the last 26 h" ;;
    ahead) out="${out}; last nightly step at ${lraw} is more than 30 min ahead of now ${now}" ;;
    unknown) out="${out}; date_daily_last_step_ts=${lraw:-<absent>} unreadable" ;;
  esac
  printf '%s' "${out#; }"
}

# _date_daily_problems TEXT MARGIN_US DAILY_BOUND_MS NOW_S -> the schedule reasons plus the error
# clause (the error past, or unreadable against, the daily bound + margin): what date_master_check
# prints for a daily-mode master that is not OK. Empty when it is OK.
_date_daily_problems() {
  local text="$1" margin="$2" daily="$3" now="$4" out err err_us bound_us
  out="$(_date_daily_schedule_problems "$text" "$now")"
  [ -n "$out" ] && out="; ${out}"
  err="$(_pipe_json_number_raw "$text" date_offset_error_ms)"
  err_us="$(_ms_to_us "$err")"
  bound_us="$(_positive_bound_us "$daily")"
  if [[ ! $margin =~ ^[0-9]{1,15}$ ]]; then
    out="${out}; margin=${margin}us unreadable"
  elif [ -z "$err_us" ] || [ -z "$bound_us" ]; then
    out="${out}; date_offset_error_ms=${err:-<absent>} or the daily bound ${daily}ms unreadable"
  elif [ "$(abs_int "$err_us")" -gt $((bound_us + 10#$margin)) ]; then
    out="${out}; fleet date ${err}ms off UTC > daily bound ${daily}ms + ${margin}us margin"
  fi
  printf '%s' "${out#; }"
}

# _date_master_daily_check LABEL TEXT MARGIN_US -> date_master_check's line + rc for a dantesync
# 1.12.0 date master in date_correction_mode "daily": 0 OK / 2 OUT / 3 UNKNOWN. "now" is read ONCE
# (date_master_now_s) and handed to the verdict and the reasons alike.
_date_master_daily_check() {
  local label="$1" text="$2" margin="$3" daily="$DATE_MASTER_DAILY_BOUND_MS" now err nraw lraw lms note
  now="$(date_master_now_s)"
  case "$(_date_master_daily_verdict "$text" "$margin" "$daily" "$now")" in
    ok)
      err="$(_pipe_json_number_raw "$text" date_offset_error_ms)"
      nraw="$(_pipe_json_raw_value "$text" date_daily_next_utc)"
      nraw="${nraw#\"}"
      lraw="$(_pipe_json_raw_value "$text" date_daily_last_step_ts)"
      lms="$(_pipe_json_number_raw "$text" date_daily_last_step_ms)"
      if [ "$lraw" = null ]; then
        note="NOTE: no nightly step seen since the master started"
      else
        note="last nightly step ${lms:-?}ms at ${lraw}"
      fi
      printf '  %-14s DATE MASTER OK      (daily mode: fleet date %sms off UTC <= daily bound %sms + %sus margin; next nightly window %s, %s -- dantesync 1.12.0 corrects the fleet date once a night, #1372)\n' \
        "$label" "$err" "$daily" "$margin" "${nraw%\"}" "$note"
      return 0 ;;
    out)
      printf '  %-14s DATE MASTER OUT     (daily mode: %s -- the dantesync 1.12.0 nightly date step is not holding the fleet date, #1372)\n' \
        "$label" "$(_date_daily_problems "$text" "$margin" "$daily" "$now")"
      return 2 ;;
    *)
      printf '  %-14s DATE MASTER UNKNOWN (daily mode: %s -- status incomplete, #1372)\n' \
        "$label" "$(_date_daily_problems "$text" "$margin" "$daily" "$now")"
      return 3 ;;
  esac
}

# _date_daily_journal_note GRADE STATUS -> dantesync_journal_date_note's text for a daily-out /
# daily-unknown journal grade: an unknown date_correction_mode is named as such (it is not the daily
# mode); otherwise the schedule reasons at date_master_now_s (the journal median, not the /status
# error field, is what the journal path grades), or the unreadable daily bound.
_date_daily_journal_note() {
  local grade="$1" text="$2" reasons
  if [ "$(date_master_mode_class "$text")" = unknown ]; then
    printf 'date master /status date_correction_mode=%s is not a mode this grading knows ("daily", "micro" or ""): UNKNOWN (#1372)' \
      "$(date_master_mode_text "$text")"
    return 0
  fi
  reasons="$(_date_daily_schedule_problems "$text" "$(date_master_now_s)")"
  printf 'date master /status nightly schedule %s: %s (dantesync 1.12.0 daily mode, #1372)' \
    "$([ "$grade" = daily-out ] && printf 'OUT' || printf 'UNKNOWN')" \
    "${reasons:-the daily bound ${DATE_MASTER_DAILY_BOUND_MS}ms unreadable}"
}
