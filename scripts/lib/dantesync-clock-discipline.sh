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

# _micro_bound_us MICRO_BOUND_MS -> the micro bound in integer us; "" unless MICRO_BOUND_MS is a
# positive plain decimal (digits, an optional fraction) -- the python twin applies the same shape.
_micro_bound_us() {
  local us
  [[ $1 =~ ^[0-9]+(\.[0-9]+)?$ ]] || { printf ''; return 0; }
  us="$(_ms_to_us "$1")"
  if [ -n "$us" ] && [ "$us" -gt 0 ]; then
    printf '%s' "$us"
  fi
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
  bound_us="$(_micro_bound_us "$micro")"
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
#   a micro-capable master (1.11.0): max(BOUND_US, micro bound*1000 + MARGIN_US), the micro bound
#     being MICRO_BOUND_MS (default DATE_MASTER_MICRO_BOUND_MS) when readable, else BOUND_US;
#   any other master: max(BOUND_US, date_step_bound_ms*1000 + MARGIN_US) with a readable positive
#     step bound, else BOUND_US;
#   every other node: BOUND_US unchanged.
# So the bound a consumer prints is the one date_master_verdict grades the date on.
date_master_effective_bound_us() {
  local text="$1" bound="$2" margin="$3" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" step_us
  if ! grep -qE '^[0-9]+$' <<<"$bound" || ! grep -qE '^[0-9]+$' <<<"$margin" \
     || [ "$(date_authority_from_pipe_json "$text")" != master ]; then
    printf '%s' "$bound"
    return 0
  fi
  if [ "$(date_master_micro_capable "$text")" = yes ]; then
    step_us="$(_micro_bound_us "$micro")"
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
# step-bound lines it always did.
date_master_check() {
  local label="$1" text="$2" margin="$3" micro="${4:-$DATE_MASTER_MICRO_BOUND_MS}" err bound
  if [ "$(date_authority_from_pipe_json "$text")" = master ] \
     && [ "$(date_master_micro_capable "$text")" = yes ]; then
    _date_master_micro_check "$label" "$text" "$margin" "$micro"
    return
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
