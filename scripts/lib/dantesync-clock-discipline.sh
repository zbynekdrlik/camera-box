#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + one default, no top-level statements) --
# sourced by scripts/clock-offset-guard.sh, whose callers own their own strictness; a
# `set -euo pipefail` here would leak into every caller (the scripts/lib/obs-fleet.sh convention).
#
# scripts/lib/dantesync-clock-discipline.sh -- issue 1372: the ONE dantesync CLOCK-DISCIPLINE
# classifier and the ONE fleet DATE-MASTER verdict every clock consumer grades through.
#
# WHY: dantesync 1.9.0 (dantesync#117) made `ptp_phase_lock` the default discipline -- the rate AND
# phase come from the Dante PTP tick only, NTP only moves the DATE, and phase slew is not used
# (`phase_slew_enabled` reads false BY DESIGN). `clock_discipline="legacy"` restores the pre-1.9.0
# servo, where phase_slew is again the cure for the NTP step storm (#1215/#1130). The bare phase_slew
# flag therefore no longer tells a healthy node from a broken one; the node's DISCIPLINE does. The
# first 1.9.0 fleet roll made every E2E [0/8] gate refuse (rc=20, every node PHASE-SLEW DISABLED).
# Under 1.9.0 the NTP master (`date_authority=master`) also owns the fleet DATE (dantesync#88): it
# lets the fleet line sit up to `date_step_bound_ms` (default 50) off UTC and then announces a
# COORDINATED fleet step, so its own ntp_offset_us (that fleet-line error, -25..-36 ms live) is
# graded on the step bound, never the 2 ms UTC bound.
# dantesync 1.11.0 (PR 121) keeps the fleet date within ~2-3 ms by 500 us micro-corrections and
# reports date_micro_active / date_micro_last_us / date_correction_falling_behind /
# date_micro_paused. A master that carries date_correction_falling_behind is graded on the micro
# bound (DATE_MASTER_MICRO_BOUND_MS, default 5 ms) + margin; falling behind is OUT, paused is its own
# PAUSED verdict. A master without the fields (1.9.0 / 1.10.0) keeps the step-bound grade byte for
# byte, so a mixed fleet grades each master by what it reports during a roll.
# dantesync 1.12.0 (PR 123) corrects the fleet date ONCE A NIGHT by default: a master whose
# date_correction_mode is "daily" corrects nothing by day, so its date_offset_error_ms grows to a
# day's drift (~1.5 s on the rig) until one coordinated step at the nightly window. It still
# carries date_correction_falling_behind (always false), so the daily branch is decided FIRST, on
# date_correction_mode. It is graded on its schedule (the next window within (now, now + 24 h 30 min],
# the last nightly step within the last 26 h or null = none seen yet, a NOTE) and |error| <=
# DATE_MASTER_DAILY_BOUND_MS (default 3000 ms) + margin. "micro" and "" keep the 1.11 grade. The
# pure decision takes "now" as an argument; the public wrappers read it once (date_master_now_s).
#
# CONSUMERS (they source scripts/clock-offset-guard.sh, which sources this lib): dantesync-gate.sh
# (the E2E [0/8] gate), verify-imag.sh (l), verify-strih.sh check 6, dantesync-maintenance-gate.sh.
# No consumer re-parses the discipline fields; the python twin is scripts/dantesync_fleet.py
# (classify_clock_discipline / clock_discipline_unlocked / date_master_verdict). Both sides are
# pinned by ONE table, tests/fixtures/dantesync_clock_discipline_1372.tsv, whose @ rows are /status
# captured read-only from the live 1.9.0 fleet (tests/python/test_clock_discipline_1372.py).
#
# DEPENDS on clock-offset-guard.sh's own parsers (resolved at CALL time): phase_slew_enabled_from_
# pipe_json, abs_int, _freshest_ntp_offset_line, dantesync_offset_verdict.

# DATE_MASTER_MARGIN_US -- the ONE margin (us) on the date master's own step bound, read by every
# consumer (the gate, verify-imag, verify-strih). DANTESYNC_DATE_MARGIN_US overrides it.
# shellcheck disable=SC2034  # read by the scripts that source this lib
DATE_MASTER_MARGIN_US="${DANTESYNC_DATE_MARGIN_US:-1000}"

# DATE_MASTER_MICRO_BOUND_MS -- the ONE bound (ms, before the margin) a dantesync 1.11.0 date master
# is graded on: 1.11.0 (dantesync PR 121) holds the fleet date within ~2-3 ms by 500 us
# micro-corrections (2 ms dead band, 20 s interval), so the 50 ms step bound is ~17x too loose for it.
# DANTESYNC_DATE_MICRO_BOUND_MS overrides it (a positive plain decimal; anything else, 0 included,
# is unreadable). 5 ms + the margin is deliberately stricter than dantesync's own falling-behind
# alarm (raised past 10 ms), so a 6-10 ms catch-up transient reads OUT here while dantesync is quiet.
# shellcheck disable=SC2034  # read by the scripts that source this lib
DATE_MASTER_MICRO_BOUND_MS="${DANTESYNC_DATE_MICRO_BOUND_MS:-5}"

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

# --- CLOCK DISCIPLINE ---------------------------------------------------------------------------

# clock_discipline_from_pipe_json TEXT -> the `"clock_discipline"` STRING value; "" when the field
# is absent or null (an older build's blob has none; 1.9.0 serves "" before the controller
# publishes). A field present with a NON-string value (a number, a boolean) prints the marker
# `<not a string>`, so it is graded UNKNOWN, never mistaken for an older build (the python twin
# does the same). `|| true` survives a no-match under set -e/pipefail.
clock_discipline_from_pipe_json() {
  local raw
  raw="$(printf '%s' "$1" | grep -oE '"clock_discipline"[[:space:]]*:[[:space:]]*("[^"]*"|[^,}[:space:]]+)' \
    | tail -1 | sed 's/^"clock_discipline"[[:space:]]*:[[:space:]]*//' || true)"
  case "$raw" in
    ""|null) printf '' ;;
    \"*\") raw="${raw#\"}"; printf '%s' "${raw%\"}" ;;
    *) printf '<not a string>' ;;
  esac
}

# ptp_phase_locked_from_pipe_json TEXT -> "true"/"false" (the JSON boolean `"ptp_phase_locked"`),
# "" if absent or not a JSON boolean (a quoted "true" string is unread, never a guessed lock).
ptp_phase_locked_from_pipe_json() {
  printf '%s' "$1" | grep -oE '"ptp_phase_locked"[[:space:]]*:[[:space:]]*(true|false)' \
    | sed -n 's/.*:[[:space:]]*\(true\|false\).*/\1/p' | tail -1 || true
}

# clock_discipline_class TEXT -> PTP_PHASE_LOCK | LEGACY_SLEW | LEGACY_NO_SLEW | UNKNOWN.
#   clock_discipline=ptp_phase_lock AND ptp_phase_locked=true      -> PTP_PHASE_LOCK
#   clock_discipline absent/""/null (older build) or "legacy":
#     phase_slew_enabled=true                                       -> LEGACY_SLEW
#     phase_slew_enabled=false                                      -> LEGACY_NO_SLEW (#1215)
#   anything else (an unknown value, an unread lock or slew flag)   -> UNKNOWN
# A phase lock that is not locked is UNKNOWN here and clock_discipline_unlocked says "yes", so the
# check names it and FAILS it (rc 2) instead of reading it as merely incomplete.
clock_discipline_class() {
  local text="$1" disc
  disc="$(clock_discipline_from_pipe_json "$text")"
  case "$disc" in
    ptp_phase_lock)
      if [ "$(ptp_phase_locked_from_pipe_json "$text")" = true ]; then
        printf 'PTP_PHASE_LOCK'
      else
        printf 'UNKNOWN'
      fi ;;
    ""|legacy)
      case "$(phase_slew_enabled_from_pipe_json "$text")" in
        true) printf 'LEGACY_SLEW' ;;
        false) printf 'LEGACY_NO_SLEW' ;;
        *) printf 'UNKNOWN' ;;
      esac ;;
    *) printf 'UNKNOWN' ;;
  esac
}

# clock_discipline_unlocked TEXT -> "yes" iff clock_discipline=ptp_phase_lock AND the node reports
# ptp_phase_locked=false (the phase lock does not own the clock -- a READ, wrong state); else "no".
# The ONE place the named PTP-PHASE UNLOCKED failure is decided; consumers never re-read the fields.
clock_discipline_unlocked() {
  if [ "$(clock_discipline_from_pipe_json "$1")" = ptp_phase_lock ] \
     && [ "$(ptp_phase_locked_from_pipe_json "$1")" = false ]; then
    printf 'yes'
  else
    printf 'no'
  fi
}

# clock_discipline_check LABEL TEXT -> prints ONE status line; returns 0 OK / 2 BAD / 3 UNKNOWN.
#   PTP_PHASE_LOCK / LEGACY_SLEW -> 0 (the two healthy disciplines)
#   LEGACY_NO_SLEW               -> 2 (the #1215 stepping node; line keeps "PHASE-SLEW DISABLED")
#   clock_discipline_unlocked    -> 2, named PTP-PHASE UNLOCKED
#   anything else                -> 3 (unreadable is never OK -- the gm_check contract)
# The legacy lines keep the pre-1.9.0 "PHASE-SLEW ENABLED/DISABLED/UNKNOWN" words, so an older
# node reads exactly as it did before this classifier existed.
clock_discipline_check() {
  local label="$1" text="$2" err disc
  case "$(clock_discipline_class "$text")" in
    PTP_PHASE_LOCK)
      err="$(printf '%s' "$text" | grep -oE '"ptp_phase_error_us"[[:space:]]*:[[:space:]]*-?[0-9.]+' \
        | sed -n 's/.*:[[:space:]]*//p' | tail -1 || true)"
      printf '  %-14s CLOCK PTP-PHASE-LOCK (dantesync 1.9.0: rate and phase from the PTP tick, phase locked%s -- #1372)\n' \
        "$label" "$([ -n "$err" ] && printf ', error %sus' "$err")"
      return 0 ;;
    LEGACY_SLEW)
      printf '  %-14s CLOCK LEGACY PHASE-SLEW ENABLED  (pre-1.9.0 discipline, slews phase error, never steps -- #1215)\n' "$label"
      return 0 ;;
    LEGACY_NO_SLEW)
      printf '  %-14s CLOCK LEGACY PHASE-SLEW DISABLED (pre-1.9.0 discipline without phase slew: dantesync will STEP the clock -- #1215)\n' "$label"
      return 2 ;;
  esac
  if [ "$(clock_discipline_unlocked "$text")" = yes ]; then
    printf '  %-14s CLOCK PTP-PHASE UNLOCKED (clock_discipline=ptp_phase_lock but ptp_phase_locked=false: the phase lock does not own the clock -- #1372)\n' "$label"
    return 2
  fi
  disc="$(clock_discipline_from_pipe_json "$text")"
  printf '  %-14s CLOCK-DISCIPLINE UNKNOWN (clock_discipline=%s unreadable or unknown; PHASE-SLEW UNKNOWN -- status incomplete, #1372)\n' \
    "$label" "${disc:-<absent>}"
  return 3
}

# --- DATE MASTER (dantesync#88) -----------------------------------------------------------------

# _pipe_json_number_raw TEXT KEY -> the raw text of a numeric-or-null JSON value for KEY ("" when
# absent). Numbers may carry a fraction/exponent (the date fields are f64 milliseconds). KEY is
# always a literal field name from this file, never caller data.
_pipe_json_number_raw() {
  printf '%s' "$1" \
    | grep -oE "\"$2\"[[:space:]]*:[[:space:]]*(null|-?[0-9]+(\\.[0-9]+)?([eE][+-]?[0-9]+)?)" \
    | sed -n 's/.*:[[:space:]]*//p' | tail -1 || true
}

# _ms_to_us RAW -> the integer microseconds of a numeric millisecond value (half-even rounding,
# the same as the python twin's round()); "" for null/absent/non-numeric, and "" for a non-finite
# or absurd value (|us| >= 1e15, i.e. more than 15 digits) that bash integer arithmetic cannot
# grade -- the python twin applies the same limit, so both read it as unreadable.
_ms_to_us() {
  local us
  case "$1" in
    ""|null) printf ''; return 0 ;;
  esac
  us="$(awk -v v="$1" 'BEGIN { printf "%.0f", v * 1000 }' 2>/dev/null || true)"
  if grep -qE '^-?[0-9]{1,15}$' <<<"$us"; then
    printf '%s' "$us"
  else
    printf ''
  fi
}

# date_authority_from_pipe_json TEXT -> "master" / "follower" / "local" / "" (the legacy or not-yet-
# anchored blob).
date_authority_from_pipe_json() {
  printf '%s' "$1" | grep -oE '"date_authority"[[:space:]]*:[[:space:]]*"[^"]*"' \
    | sed -n 's/.*"date_authority"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | tail -1 || true
}

# _pipe_json_bool_raw TEXT KEY -> "true"/"false" for a JSON boolean, "" when KEY is absent, and the
# marker `<not a bool>` when KEY is present with any other value (null, a quoted "false", a number),
# so a present-but-unreadable flag is graded unknown, never guessed (the python twin does the same).
# KEY is always a literal field name from this file, never caller data.
_pipe_json_bool_raw() {
  local raw
  raw="$(printf '%s' "$1" | grep -oE "\"$2\"[[:space:]]*:[[:space:]]*(\"[^\"]*\"|[^,}[:space:]]+)" \
    | tail -1 | sed "s/^\"$2\"[[:space:]]*:[[:space:]]*//" || true)"
  case "$raw" in
    "") printf '' ;;
    true|false) printf '%s' "$raw" ;;
    *) printf '<not a bool>' ;;
  esac
}

# date_master_micro_capable TEXT -> "yes" iff the /status carries `date_correction_falling_behind`
# (any value): a dantesync 1.11.0+ node, whose date master is graded on its micro-corrections. The
# capability is read from the status itself, never from a version string (a mixed fleet during a
# roll grades each master by what it reports). "no" for a 1.9.0/1.10.0 blob.
date_master_micro_capable() {
  if [ -n "$(_pipe_json_bool_raw "$1" date_correction_falling_behind)" ]; then
    printf 'yes'
  else
    printf 'no'
  fi
}

# _positive_bound_us BOUND_MS -> a date bound (the micro or the daily one) in integer us; "" unless
# BOUND_MS is a positive plain decimal (digits, an optional fraction) -- the python twin applies the
# same shape.
_positive_bound_us() {
  local us
  [[ $1 =~ ^[0-9]+(\.[0-9]+)?$ ]] || { printf ''; return 0; }
  us="$(_ms_to_us "$1")"
  if [ -n "$us" ] && [ "$us" -gt 0 ]; then
    printf '%s' "$us"
  fi
}

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

# _pipe_json_raw_value TEXT KEY -> the raw JSON token of KEY's value: `"..."` for a string, `null`,
# a number or boolean as written; "" when KEY is absent. KEY is always a literal field name from
# this file, never caller data.
_pipe_json_raw_value() {
  printf '%s' "$1" | grep -oE "\"$2\"[[:space:]]*:[[:space:]]*(\"[^\"]*\"|[^,}[:space:]]+)" \
    | tail -1 | sed "s/^\"$2\"[[:space:]]*:[[:space:]]*//" || true
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
_date_daily_last_state() {
  local raw now="$2" age
  [[ $now =~ ^[0-9]{1,15}$ ]] || { printf 'unknown'; return 0; }
  raw="$(_pipe_json_raw_value "$1" date_daily_last_step_ts)"
  [ "$raw" = null ] && { printf 'none'; return 0; }
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
  if [ -z "$err_us" ] || [ -z "$bound_us" ]; then
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

# _date_master_micro_verdict TEXT MARGIN_US MICRO_BOUND_MS -> ok | out | paused | unknown for a
# micro-capable date master (dantesync 1.11.0):
#   out     -- date_correction_falling_behind=true (the micro-corrections cannot hold the date;
#              wins over everything else), or |date_offset_error_ms| > micro bound + MARGIN_US
#   paused  -- date_micro_paused=true: no UTC reading for over a minute, the fleet date runs free
#   ok      -- both flags false and |date_offset_error_ms| <= micro bound + MARGIN_US
#   unknown -- a flag that is not a JSON boolean, an unreadable error, or an unreadable micro bound
# date_step_bound_ms is not read here: it only sets the abnormal-error step threshold on 1.11.0.
_date_master_micro_verdict() {
  local text="$1" margin="$2" micro="$3" behind paused err_us bound_us
  behind="$(_pipe_json_bool_raw "$text" date_correction_falling_behind)"
  paused="$(_pipe_json_bool_raw "$text" date_micro_paused)"
  [ "$behind" = true ] && { printf 'out'; return 0; }
  case "$behind/$paused" in
    false/true) printf 'paused'; return 0 ;;
    false/false) ;;
    *) printf 'unknown'; return 0 ;;
  esac
  err_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_offset_error_ms)")"
  bound_us="$(_positive_bound_us "$micro")"
  if [ -z "$err_us" ] || [ -z "$bound_us" ]; then
    printf 'unknown'
  elif [ "$(abs_int "$err_us")" -le $((bound_us + 10#$margin)) ]; then
    printf 'ok'
  else
    printf 'out'
  fi
}

# date_master_verdict TEXT MARGIN_US [MICRO_BOUND_MS] -> none | ok | out | paused | unknown.
#   none    -- not the date master (follower, local, legacy "", absent)
#   a master in date_correction_mode "daily" (dantesync 1.12.0, date_master_mode_class) ->
#     _date_master_daily_verdict on DATE_MASTER_DAILY_BOUND_MS at date_master_now_s; decided FIRST,
#     because a 1.12.0 master also carries date_correction_falling_behind
#   an unknown date_correction_mode on a master -> unknown
#   a micro-capable master (dantesync 1.11.0, date_master_micro_capable) -> _date_master_micro_verdict
#     on MICRO_BOUND_MS (default DATE_MASTER_MICRO_BOUND_MS); the only source of `paused`
#   any other master (1.9.0 / 1.10.0) -- graded exactly as before on its own step bound:
#   ok      -- master, |date_offset_error_ms| <= date_step_bound_ms + MARGIN_US
#   out     -- master, past that bound (a date the master failed to step)
#   unknown -- master, but the error or a positive step bound is unreadable (never an OK), or
#              MARGIN_US is not a non-negative integer
date_master_verdict() {
  local text="$1" margin="$2" micro="${3:-$DATE_MASTER_MICRO_BOUND_MS}" err_us bound_us
  [ "$(date_authority_from_pipe_json "$text")" = master ] || { printf 'none'; return 0; }
  grep -qE '^[0-9]+$' <<<"$margin" || { printf 'unknown'; return 0; }
  case "$(date_master_mode_class "$text")" in
    daily)
      _date_master_daily_verdict "$text" "$margin" "$DATE_MASTER_DAILY_BOUND_MS" "$(date_master_now_s)"
      return 0 ;;
    unknown) printf 'unknown'; return 0 ;;
  esac
  if [ "$(date_master_micro_capable "$text")" = yes ]; then
    _date_master_micro_verdict "$text" "$margin" "$micro"
    return 0
  fi
  err_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_offset_error_ms)")"
  bound_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_step_bound_ms)")"
  if [ -z "$err_us" ] || [ -z "$bound_us" ] || [ "$bound_us" -le 0 ]; then
    printf 'unknown'
    return 0
  fi
  if [ "$(abs_int "$err_us")" -le $((bound_us + 10#$margin)) ]; then
    printf 'ok'
  else
    printf 'out'
  fi
}

# date_master_effective_bound_us TEXT BOUND_US MARGIN_US [MICRO_BOUND_MS] -> the bound the date
# master's own ntp_offset_us median is graded on:
#   a daily-mode master (1.12.0): max(BOUND_US, DATE_MASTER_DAILY_BOUND_MS*1000 + MARGIN_US) when
#     the daily bound is readable, else BOUND_US (its median IS the day's drift);
#   a master with an unknown date_correction_mode: BOUND_US unchanged (its verdict is unknown);
#   a micro-capable master (1.11.0): max(BOUND_US, micro bound*1000 + MARGIN_US), the micro bound
#     being MICRO_BOUND_MS (default DATE_MASTER_MICRO_BOUND_MS) when readable, else BOUND_US;
#   any other master: max(BOUND_US, date_step_bound_ms*1000 + MARGIN_US) with a readable positive
#     step bound, else BOUND_US;
#   every other node: BOUND_US unchanged.
# So the bound a consumer prints is the one date_master_verdict grades the date on.
date_master_effective_bound_us() {
  local text="$1" bound="$2" margin="$3" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" step_us mode
  if ! grep -qE '^[0-9]+$' <<<"$bound" || ! grep -qE '^[0-9]+$' <<<"$margin" \
     || [ "$(date_authority_from_pipe_json "$text")" != master ]; then
    printf '%s' "$bound"
    return 0
  fi
  mode="$(date_master_mode_class "$text")"
  if [ "$mode" = daily ]; then
    step_us="$(_positive_bound_us "$DATE_MASTER_DAILY_BOUND_MS")"
  elif [ "$mode" = unknown ]; then
    step_us=""
  elif [ "$(date_master_micro_capable "$text")" = yes ]; then
    step_us="$(_positive_bound_us "$micro")"
  else
    step_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_step_bound_ms)")"
  fi
  if [ -z "$step_us" ] || [ "$step_us" -le 0 ] || [ $((step_us + 10#$margin)) -le "$bound" ]; then
    printf '%s' "$bound"
  else
    printf '%s' "$((step_us + 10#$margin))"
  fi
}

# date_master_check LABEL TEXT MARGIN_US [MICRO_BOUND_MS] -> prints ONE line for a date master only;
# returns 0 OK (or not a master, silently) / 2 OUT / 3 UNKNOWN / 4 PAUSED. PAUSED (a 1.11.0 master
# with no UTC reading) is WARN-level: each consumer decides -- the E2E [0/8] gate (dantesync-gate.sh)
# refuses it as UNKNOWN, verify-imag reports a warning. A pre-1.11.0 master prints exactly the
# step-bound lines it always did. A dantesync 1.12.0 daily-mode master prints its nightly schedule
# (_date_master_daily_check, 0 OK / 2 OUT / 3 UNKNOWN); an unknown date_correction_mode is UNKNOWN.
date_master_check() {
  local label="$1" text="$2" margin="$3" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" err bound mode
  if [ "$(date_authority_from_pipe_json "$text")" = master ]; then
    mode="$(date_master_mode_class "$text")"
    if [ "$mode" = daily ]; then
      _date_master_daily_check "$label" "$text" "$margin"
      return
    elif [ "$mode" = unknown ]; then
      mode="$(_pipe_json_raw_value "$text" date_correction_mode)"
      mode="${mode#\"}"
      printf '  %-14s DATE MASTER UNKNOWN (date_correction_mode=%s is not a mode this grading knows ("daily", "micro" or "") -- status incomplete, #1372)\n' \
        "$label" "${mode%\"}"
      return 3
    elif [ "$(date_master_micro_capable "$text")" = yes ]; then
      _date_master_micro_check "$label" "$text" "$margin" "$micro"
      return
    fi
  fi
  err="$(_pipe_json_number_raw "$text" date_offset_error_ms)"
  bound="$(_pipe_json_number_raw "$text" date_step_bound_ms)"
  case "$(date_master_verdict "$text" "$margin")" in
    none) return 0 ;;
    ok)
      printf '  %-14s DATE MASTER OK      (fleet date %sms off UTC <= step bound %sms + %sus margin -- the master steps the fleet past it, dantesync#88/#1372)\n' \
        "$label" "$err" "$bound" "$margin"
      return 0 ;;
    out)
      printf '  %-14s DATE MASTER OUT     (fleet date %sms off UTC > step bound %sms + %sus margin -- the master did not step the fleet date, #1372)\n' \
        "$label" "$err" "$bound" "$margin"
      return 2 ;;
    *)
      printf '  %-14s DATE MASTER UNKNOWN (date_offset_error_ms=%s date_step_bound_ms=%s margin=%sus unreadable -- status incomplete, #1372)\n' \
        "$label" "${err:-<absent>}" "${bound:-<absent>}" "$margin"
      return 3 ;;
  esac
}

# _date_master_micro_check LABEL TEXT MARGIN_US MICRO_BOUND_MS -> date_master_check's line + rc for a
# micro-capable (dantesync 1.11.0) date master: 0 OK / 2 OUT / 3 UNKNOWN / 4 PAUSED.
_date_master_micro_check() {
  local label="$1" text="$2" margin="$3" micro="$4" err behind paused last active note=""
  err="$(_pipe_json_number_raw "$text" date_offset_error_ms)"
  behind="$(_pipe_json_bool_raw "$text" date_correction_falling_behind)"
  paused="$(_pipe_json_bool_raw "$text" date_micro_paused)"
  case "$(date_master_verdict "$text" "$margin" "$micro")" in
    ok)
      last="$(_pipe_json_number_raw "$text" date_micro_last_us)"
      active="$(_pipe_json_bool_raw "$text" date_micro_active)"
      [ "$active" = true ] && note=", a correction in flight"
      [ -n "$last" ] && [ "$last" != null ] && note="${note}, last correction ${last}us"
      printf '  %-14s DATE MASTER OK      (fleet date %sms off UTC <= micro bound %sms + %sus margin -- dantesync 1.11.0 micro-corrections hold the fleet date%s, #1372)\n' \
        "$label" "$err" "$micro" "$margin" "$note"
      return 0 ;;
    out)
      if [ "$behind" = true ]; then
        printf '  %-14s DATE MASTER OUT     (dantesync 1.11.0 reports date_correction_falling_behind=true: the micro-corrections cannot hold the fleet date, %sms off UTC -- #1372)\n' \
          "$label" "${err:-<absent>}"
      else
        printf '  %-14s DATE MASTER OUT     (fleet date %sms off UTC > micro bound %sms + %sus margin -- the dantesync 1.11.0 micro-corrections did not hold the fleet date, #1372)\n' \
          "$label" "$err" "$micro" "$margin"
      fi
      return 2 ;;
    paused)
      printf '  %-14s DATE MASTER PAUSED  (dantesync 1.11.0 reports date_micro_paused=true: no UTC reading for over a minute, the micro-corrections are paused and the fleet date runs free at the grandmaster rate until UTC is back -- WARN, the E2E [0/8] gate refuses it, #1372)\n' \
        "$label"
      return 4 ;;
    *)
      printf '  %-14s DATE MASTER UNKNOWN (date_offset_error_ms=%s date_correction_falling_behind=%s date_micro_paused=%s micro bound=%sms margin=%sus unreadable -- status incomplete, #1372)\n' \
        "$label" "${err:-<absent>}" "${behind:-<absent>}" "${paused:-<absent>}" "$micro" "$margin"
      return 3 ;;
  esac
}

# _date_daily_schedule_problems TEXT NOW_S -> the "; "-joined reasons a daily-mode master's nightly
# schedule is not OK (empty when it is). It reads the SAME states _date_daily_schedule_verdict
# decides on, so the text never disagrees with the verdict.
_date_daily_schedule_problems() {
  local text="$1" now="$2" out="" next last nraw lraw
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
    past|far) out="${out}; next nightly window ${nraw} is not within the next 24 h 30 min" ;;
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
  if [ -z "$err_us" ] || [ -z "$bound_us" ]; then
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

# --- the JOURNAL path (a node read from journald, graded with its own /status when readable) -----
#
# A dantesync 1.11.x date master still logs `[NTP] offset:-2140us (date authority, fleet line
# -2140us, step bound 50000us)`: the journal line carries no capability, so the journal alone cannot
# tell a 1.11.x master (micro-corrections, ~2-3 ms) from a 1.10.0 one (steps at 50 ms). The node's
# own /status does (date_master_micro_capable), so every journal consumer passes it when it has it
# (verify-strih check 6 reads 127.0.0.1:8898 on the box). Without a readable /status the journal
# keeps the step bound and dantesync_journal_date_note says so -- never a silent loosening.
# dantesync 1.12.0 (daily mode) logs the same line shape: its /status decides that too, through the
# nightly schedule (_date_daily_schedule_verdict) and the daily bound, at date_master_now_s.

# date_step_bound_us_from_journal JOURNAL -> the step bound (us) carried by the FRESHEST
# `[NTP] offset:` line when it is the master's date-authority shape
#   [NTP] offset:-25217us (date authority, fleet line -25217us, step bound 50000us)
# "" when the freshest offset line is any other shape (a client's `(threshold:..)` line).
date_step_bound_us_from_journal() {
  _freshest_ntp_offset_line "$1" \
    | sed -n 's/.*\[NTP\] offset:[+-]\{0,1\}[0-9][0-9]*us (date authority,.*step bound \([0-9][0-9]*\)us).*/\1/p' \
    | tail -1 || true
}

# journal_date_grade_from_step STEP_US MARGIN_US STATUS [MICRO_BOUND_MS] -> how a date-authority
# journal line whose step bound is STEP_US is graded, given the SAME node's /status (python twin:
# scripts/dantesync_fleet.py journal_date_grade; both pinned by the journal_grade column of
# tests/fixtures/dantesync_clock_discipline_1372.tsv):
#   none              -- STEP_US is not a positive integer or MARGIN_US not a non-negative integer
#                        (each at most 15 digits, so bash integer arithmetic never wraps):
#                        not a date-authority line, grade the journal the ordinary way
#   micro:<us>        -- STATUS is a micro-capable (1.11.x) date master with both flags false:
#                        median-only on micro bound (MICRO_BOUND_MS, default
#                        DATE_MASTER_MICRO_BOUND_MS) + MARGIN_US
#   out               -- that master reports date_correction_falling_behind=true (wins over paused)
#   paused            -- that master reports date_micro_paused=true
#   unknown           -- that master's flags are not JSON booleans, or the micro bound is unreadable
#   step:<us>         -- STATUS carries a date_authority but is not a micro-capable master (1.9.0 /
#                        1.10.0, or a follower): STEP_US + MARGIN_US
#   step-unread:<us>  -- STATUS carries no date_authority (empty, unreachable, not a /status, a
#                        pre-1.9.0 blob): STEP_US + MARGIN_US, the looser bound, named by the note
#   daily:<us>        -- STATUS is a dantesync 1.12.0 date master in date_correction_mode "daily"
#                        with a live nightly schedule: median-only on DATE_MASTER_DAILY_BOUND_MS +
#                        MARGIN_US (its median IS the day's drift)
#   daily-out         -- that master's schedule is out (no / past / far next window, a stale or far
#                        ahead last nightly step)
#   daily-unknown     -- that master's schedule fields, its date_correction_mode (an unknown value)
#                        or the daily bound are unreadable
# The flag order is _date_master_micro_verdict's, so the journal and /status paths agree; the daily
# schedule is _date_daily_schedule_verdict's.
journal_date_grade_from_step() {
  local step="$1" margin="$2" text="$3" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" auth behind paused bound_us mode
  if [[ ! $step =~ ^[0-9]{1,15}$ ]] || [ "$((10#$step))" -le 0 ] || [[ ! $margin =~ ^[0-9]{1,15}$ ]]; then
    printf 'none'
    return 0
  fi
  auth="$(date_authority_from_pipe_json "$text")"
  if [ -z "$auth" ]; then
    printf 'step-unread:%s' "$((10#$step + 10#$margin))"
    return 0
  fi
  if [ "$auth" = master ]; then
    mode="$(date_master_mode_class "$text")"
    if [ "$mode" = daily ]; then
      bound_us="$(_positive_bound_us "$DATE_MASTER_DAILY_BOUND_MS")"
      case "$(_date_daily_schedule_verdict "$text" "$(date_master_now_s)")" in
        out) printf 'daily-out' ;;
        ok)
          if [ -n "$bound_us" ]; then
            printf 'daily:%s' "$((bound_us + 10#$margin))"
          else
            printf 'daily-unknown'
          fi ;;
        *) printf 'daily-unknown' ;;
      esac
      return 0
    elif [ "$mode" = unknown ]; then
      printf 'daily-unknown'
      return 0
    fi
  fi
  if [ "$auth" != master ] || [ "$(date_master_micro_capable "$text")" != yes ]; then
    printf 'step:%s' "$((10#$step + 10#$margin))"
    return 0
  fi
  behind="$(_pipe_json_bool_raw "$text" date_correction_falling_behind)"
  paused="$(_pipe_json_bool_raw "$text" date_micro_paused)"
  [ "$behind" = true ] && { printf 'out'; return 0; }
  case "$behind/$paused" in
    false/true) printf 'paused'; return 0 ;;
    false/false) ;;
    *) printf 'unknown'; return 0 ;;
  esac
  bound_us="$(_positive_bound_us "$micro")"
  if [ -z "$bound_us" ]; then
    printf 'unknown'
  else
    printf 'micro:%s' "$((bound_us + 10#$margin))"
  fi
}

# dantesync_journal_date_grade JOURNAL MARGIN_US [STATUS] [MICRO_BOUND_MS] -> the grade of the
# JOURNAL's freshest offset line (journal_date_grade_from_step on its step bound); `none` for an
# ordinary line. STATUS omitted = no readable /status.
dantesync_journal_date_grade() {
  journal_date_grade_from_step "$(date_step_bound_us_from_journal "$1")" "$2" "${3:-}" "${4:-}"
}

# dantesync_journal_date_bound_us JOURNAL MARGIN_US [STATUS] [MICRO_BOUND_MS] -> the bound (us) a
# date-authority journal line is graded on (micro bound + margin for a micro-capable master /status,
# the daily bound + margin for a daily-mode 1.12.0 master /status with a live schedule, else the
# line's own step bound + margin); "" for an ordinary line, and for a grade that is decided by a flag
# or the schedule rather than a bound (out / paused / unknown / daily-out / daily-unknown).
dantesync_journal_date_bound_us() {
  local grade
  grade="$(dantesync_journal_date_grade "$1" "$2" "${3:-}" "${4:-}")"
  case "$grade" in
    micro:*|step:*|step-unread:*|daily:*) printf '%s' "${grade#*:}" ;;
  esac
}

# dantesync_journal_date_note JOURNAL MARGIN_US [STATUS] [MICRO_BOUND_MS] -> the text a consumer
# prints for what the date master's journal was graded on (the SAME decision the verdict uses); ""
# for an ordinary line. verify-strih check 6 and the gate's journal fallback print it.
dantesync_journal_date_note() {
  local journal="$1" margin="$2" text="${3:-}" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" grade step
  grade="$(dantesync_journal_date_grade "$journal" "$margin" "$text" "$micro")"
  step="$(date_step_bound_us_from_journal "$journal")"
  case "$grade" in
    micro:*)
      printf 'date master micro bound %sms + %sus margin = %sus, median-only (its /status is a micro-capable dantesync 1.11.x date master, #1372)' \
        "$micro" "$margin" "${grade#*:}" ;;
    step:*)
      printf 'date master step bound %sus + %sus margin = %sus, median-only (its /status is not a micro-capable 1.11.x date master, dantesync#88/#1372)' \
        "$step" "$margin" "${grade#*:}" ;;
    step-unread:*)
      printf 'date master step bound %sus + %sus margin = %sus, median-only (no readable /status: micro-corrections not graded, the looser step bound, #1372)' \
        "$step" "$margin" "${grade#*:}" ;;
    out)
      printf 'date master /status reports date_correction_falling_behind=true: the micro-corrections cannot hold the fleet date (#1372)' ;;
    paused)
      printf 'date master /status reports date_micro_paused=true: no UTC reading for over a minute, the fleet date runs free (#1372)' ;;
    unknown)
      printf 'date master /status micro-correction flags or micro bound (%sms) unreadable (#1372)' "$micro" ;;
    daily:*)
      printf 'date master daily bound %sms + %sus margin = %sus, median-only (its /status is a dantesync 1.12.0 daily-mode date master with a live nightly schedule: the fleet date drifts all day and is stepped once a night, #1372)' \
        "$DATE_MASTER_DAILY_BOUND_MS" "$margin" "${grade#*:}" ;;
    daily-out)
      printf 'date master /status nightly schedule OUT: %s (dantesync 1.12.0 daily mode, #1372)' \
        "$(_date_daily_journal_problems "$text")" ;;
    daily-unknown)
      printf 'date master /status nightly schedule UNKNOWN: %s (dantesync 1.12.0 daily mode, #1372)' \
        "$(_date_daily_journal_problems "$text")" ;;
  esac
}

# _date_daily_journal_problems STATUS -> the reasons behind a daily-out / daily-unknown journal
# grade: the schedule reasons at date_master_now_s (the journal median, not the /status error field,
# is what the journal path grades), an unknown date_correction_mode, or an unreadable daily bound.
_date_daily_journal_problems() {
  local text="$1" mode reasons
  if [ "$(date_master_mode_class "$text")" = unknown ]; then
    mode="$(_pipe_json_raw_value "$text" date_correction_mode)"
    mode="${mode#\"}"
    printf 'date_correction_mode=%s is not a mode this grading knows' "${mode%\"}"
    return 0
  fi
  reasons="$(_date_daily_schedule_problems "$text" "$(date_master_now_s)")"
  printf '%s' "${reasons:-the daily bound ${DATE_MASTER_DAILY_BOUND_MS}ms unreadable}"
}

# dantesync_journal_clock_verdict JOURNAL FRESHNESS_S BOUND_US STABILITY_US DATE_MARGIN_US [STATUS]
# [MICRO_BOUND_MS] -> the verdict word for a node read from its JOURNAL (verify-strih check 6, the
# verify-imag fallback, the gate's linux journal fallback). A date-authority freshest line is graded
# per dantesync_journal_date_grade:
#   micro:/step:/step-unread:<us> -> dantesync_offset_verdict MEDIAN-ONLY on that bound -- the fleet
#     line walks by design and a coordinated (or micro) step moves every sample at once, so the
#     spread is not a health signal (the gate's HTTP master is median-only for the same reason, #1014)
#   out -> falling_behind | paused -> paused | unknown -> unknown (the /status flags decide)
#   daily:<us> -> MEDIAN-ONLY on the daily bound + margin (dantesync 1.12.0 daily mode)
#   daily-out -> date_out | daily-unknown -> unknown (the nightly schedule decides)
# Any other journal is graded exactly as before (BOUND_US + STABILITY_US); STATUS is not read.
dantesync_journal_clock_verdict() {
  local journal="$1" fresh="$2" bound="$3" stability="$4" margin="$5" grade
  grade="$(dantesync_journal_date_grade "$journal" "$margin" "${6:-}" "${7:-}")"
  case "$grade" in
    micro:*|step:*|step-unread:*|daily:*) dantesync_offset_verdict "$journal" "$fresh" "${grade#*:}" ;;
    out) printf 'falling_behind' ;;
    daily-out) printf 'date_out' ;;
    daily-unknown) printf 'unknown' ;;
    paused) printf 'paused' ;;
    unknown) printf 'unknown' ;;
    *) dantesync_offset_verdict "$journal" "$fresh" "$bound" "$stability" ;;
  esac
}
