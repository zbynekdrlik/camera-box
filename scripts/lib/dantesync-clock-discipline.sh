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

# date_master_verdict TEXT MARGIN_US -> none | ok | out | unknown.
#   none    -- not the date master (follower, local, legacy "", absent)
#   ok      -- master, |date_offset_error_ms| <= date_step_bound_ms + MARGIN_US
#   out     -- master, past that bound (a date the master failed to step)
#   unknown -- master, but the error or a positive step bound is unreadable (never an OK), or
#              MARGIN_US is not a non-negative integer
date_master_verdict() {
  local text="$1" margin="$2" err_us bound_us
  [ "$(date_authority_from_pipe_json "$text")" = master ] || { printf 'none'; return 0; }
  grep -qE '^[0-9]+$' <<<"$margin" || { printf 'unknown'; return 0; }
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

# date_master_effective_bound_us TEXT BOUND_US MARGIN_US -> the bound the date master's own
# ntp_offset_us median is graded on: max(BOUND_US, date_step_bound_ms*1000 + MARGIN_US) when TEXT is
# a date master with a readable positive step bound, else BOUND_US unchanged (every other node).
date_master_effective_bound_us() {
  local text="$1" bound="$2" margin="$3" step_us
  if ! grep -qE '^[0-9]+$' <<<"$bound" || ! grep -qE '^[0-9]+$' <<<"$margin" \
     || [ "$(date_authority_from_pipe_json "$text")" != master ]; then
    printf '%s' "$bound"
    return 0
  fi
  step_us="$(_ms_to_us "$(_pipe_json_number_raw "$text" date_step_bound_ms)")"
  if [ -z "$step_us" ] || [ "$step_us" -le 0 ] || [ $((step_us + 10#$margin)) -le "$bound" ]; then
    printf '%s' "$bound"
  else
    printf '%s' "$((step_us + 10#$margin))"
  fi
}

# date_master_check LABEL TEXT MARGIN_US -> prints ONE line for a date master only; returns
# 0 OK (or not a master, silently) / 2 OUT / 3 UNKNOWN.
date_master_check() {
  local label="$1" text="$2" margin="$3" err bound
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

# --- the JOURNAL path (a node read from journald, no /status) -----------------------------------

# date_step_bound_us_from_journal JOURNAL -> the step bound (us) carried by the FRESHEST
# `[NTP] offset:` line when it is the master's date-authority shape
#   [NTP] offset:-25217us (date authority, fleet line -25217us, step bound 50000us)
# "" when the freshest offset line is any other shape (a client's `(threshold:..)` line).
date_step_bound_us_from_journal() {
  _freshest_ntp_offset_line "$1" \
    | sed -n 's/.*\[NTP\] offset:[+-]\{0,1\}[0-9][0-9]*us (date authority,.*step bound \([0-9][0-9]*\)us).*/\1/p' \
    | tail -1 || true
}

# dantesync_journal_date_bound_us JOURNAL MARGIN_US -> the date master's own step bound + MARGIN_US
# when the freshest `[NTP] offset:` line has the date-authority shape with a positive bound and
# MARGIN_US is a non-negative integer; "" otherwise (grade the journal the ordinary way). The ONE
# place the journal branch decision lives, so a consumer's printed bound always matches the graded
# one (verify-strih prints it, the gate's journal fallback prints it).
dantesync_journal_date_bound_us() {
  local step margin="$2"
  step="$(date_step_bound_us_from_journal "$1")"
  if [ -n "$step" ] && [ "$step" -gt 0 ] && grep -qE '^[0-9]+$' <<<"$margin"; then
    printf '%s' "$((10#$step + 10#$margin))"
  fi
}

# dantesync_journal_clock_verdict JOURNAL FRESHNESS_S BOUND_US STABILITY_US DATE_MARGIN_US ->
# the dantesync_offset_verdict word for a node read from its JOURNAL (verify-strih check 6, the
# verify-imag fallback, the gate's linux journal fallback). A date-authority freshest line is
# graded MEDIAN-ONLY on dantesync_journal_date_bound_us -- the fleet line walks by design and a
# coordinated step moves every sample at once, so the spread is not a health signal (the gate's
# HTTP master is median-only for the same reason, #1014). Any other journal is graded exactly as
# before (BOUND_US + STABILITY_US).
dantesync_journal_clock_verdict() {
  local journal="$1" fresh="$2" bound="$3" stability="$4" margin="$5" dbound
  dbound="$(dantesync_journal_date_bound_us "$journal" "$margin")"
  if [ -n "$dbound" ]; then
    dantesync_offset_verdict "$journal" "$fresh" "$dbound"
  else
    dantesync_offset_verdict "$journal" "$fresh" "$bound" "$stability"
  fi
}
