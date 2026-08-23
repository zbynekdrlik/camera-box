#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (no side effects at source time beyond the
# documented `: "${VAR:=default}"` fallbacks) — mirrors scripts/lib/timesync-authority.sh; a
# sourced lib must NOT impose `set -euo pipefail` on its caller.
# scripts/lib/imag-power-envelope.sh — shared imag-nb power/thermal-envelope gather + verdict +
# guard-decision core (#1040).
#
# Root cause (issues 799/880/1029/1030, live forcewake measurement 2026-08-13): the imag render
# regression is a HARDWARE power clamp. thermald's adaptive DPTF policy programmed the MMIO RAPL
# PL1 long-term constraint to 25 W (MMIO wins over the decorative MSR 200/80 W values), starving
# the iGPU to gt_act_freq 600-850 MHz while every software freq knob sat at 1400. At a sustainable
# 29 W the historic ~5 ms/60fps render class was restored on the ORIGINAL i5 unit (35 W overheated
# it — TCPU 81->90 C in 8 s).
#
# #1162 hardware re-baseline (live calibration on the REPLACEMENT i7-13620H imag-nb, 2026-08-23):
# 29 W STARVES this unit's iGPU (150-450 MHz, 74-88 ms/frame). Its sustainable ceiling is 45 W
# (GPU 1200 MHz, 17-21 ms/frame; ACTUAL package draw plateaus ~36 W at the 93 C chassis thermal
# ceiling), so the default below is re-baselined 29->45 W for it. The step-down (25 W) + guard
# thresholds (93/85 C) are unchanged; the guard's thermal step-down stays armed.
#
# The durable fix pins MMIO PL1 = 45 W + slpc_ignore_eff_freq = 1 at boot (imag-power-envelope.
# service, a root oneshot), PURGES thermald (not masks — it is the actor that programmed 25 W; a
# minimalist appliance purges a competing policy engine, same discipline scripts/lib/timesync-
# authority.sh enforces for competing clock daemons), and supervises the envelope with a LOUD root
# guard (imag-power-envelope-guard.timer): TCPU >= ceiling for 2 consecutive reads -> step PL1
# down to 25 W; TCPU < restore threshold sustained -> restore; a foreign PL1 re-program -> re-
# assert. PROCHOT stays as the hardware backstop; the guard replaces thermald's one useful
# behavior with a version that alerts (dev1-side) instead of silently clamping.
#
# This lib holds the REMOTE gather snippet + the PURE verdicts, SHARED by scripts/drift-guard.sh's
# --check-imag facet and scripts/verify-imag.sh's check (u) so the identity-based zone selection
# and the OK/DRIFT/UNKNOWN verdict never exist as two driftable copies (the SAME extraction
# discipline #596 applied to the timesync verdict). The thermald-absent verdict REUSES the generic
# dpkg_status_installed() / timesync_enabled_state_neutral() from scripts/lib/timesync-authority.sh
# (both callers already source it) rather than re-deriving "is this daemon installed / neutral".
#
# Source-only: this file defines pure functions and performs no side effects on its own beyond the
# documented default-var fallbacks. It does NOT set `set -e` (a sourced lib must never impose
# errexit on its caller).

# The provisioned envelope defaults — ONE place the pin default + the guard thresholds live, so
# setup-imag.sh (the writer), verify-imag.sh (the acceptance gate) and the guard script all agree.
# The STRICT drift-guard gate reads its own authority (vendor/README.md `power_pl1_w_imag`); these
# are the fallbacks a caller uses when it has no README pin in hand. Each is overridable by env.
: "${IMAG_PL1_W:=45}"              # #1162: sustainable long-term MMIO RAPL PL1 (watts) on the i7-13620H
: "${IMAG_PL1_STEPDOWN_W:=25}"     # the safe step-down the guard drops to on a thermal excursion
: "${IMAG_TCPU_STEPDOWN_C:=93}"    # TCPU ceiling (Celsius) — 2 consecutive reads at/above -> step down
: "${IMAG_TCPU_RESTORE_C:=85}"     # TCPU restore threshold — sustained below -> restore full envelope

# The two systemd units the envelope is supervised by (root SYSTEM units — sysfs writes need root).
IMAG_POWER_ENVELOPE_UNIT="imag-power-envelope.service"
IMAG_POWER_GUARD_UNIT="imag-power-envelope-guard.timer"
# The journald tag every guard transition is logged under (retrievable via `journalctl -t ...`).
# shellcheck disable=SC2034  # consumed by scripts/imag-power-envelope-guard.sh which SOURCEs this lib
IMAG_POWER_LOG_TAG="imag-power-envelope"
# The guard's /run state file (streaks + the stepped-down flag). ONE source of truth for the path,
# referenced by BOTH scripts/imag-power-envelope-guard.sh (the writer) and scripts/verify-imag.sh
# (the acceptance gate, which consults STEPPED to tell a LEGITIMATE thermal step-down from foreign
# drift, #1188) so the two can never drift apart on the path.
# shellcheck disable=SC2034  # consumed by the guard + verify-imag, both of which SOURCE this lib
IMAG_POWER_GUARD_STATE_FILE="/run/imag-power-envelope-guard.state"

# imag_pl1_watts_to_uw WATTS -> echoes WATTS * 1_000_000 (RAPL constraints are in micro-watts).
# Exact integer arithmetic; a non-numeric/empty input echoes nothing and returns 1 (never a
# silent 0 that a matcher could read as a real limit).
imag_pl1_watts_to_uw() {
  case "$1" in
    '' | *[!0-9]*) return 1 ;;
    *) printf '%s\n' "$(( $1 * 1000000 ))" ;;
  esac
}

# imag_pl1_uw_matches_pin OBSERVED_UW PINNED_WATTS -> 0 iff OBSERVED_UW (micro-watts) equals
# PINNED_WATTS converted to micro-watts. Empty/non-numeric either side -> 1 (never a false match).
imag_pl1_uw_matches_pin() {
  local observed="$1" pinned_uw
  case "$observed" in '' | *[!0-9]*) return 1 ;; esac
  pinned_uw="$(imag_pl1_watts_to_uw "$2")" || return 1
  [ "$observed" = "$pinned_uw" ]
}

# imag_power_zone_select GATHER -> echoes the `long_term` constraint power_limit_uw of the zone
# whose NAME is `package-0`, selected by name/constraint IDENTITY (never a hardcoded
# intel-rapl-mmio index nor constraint index — the mmio zone ordering and the long_term slot both
# vary across hardware, the RAPL analogue of the presenter-drm cardN renumbering hazard). Empty +
# return 1 when the package-0 long_term constraint is not present in the gather.
imag_power_zone_select() {
  local gather="$1" tag zone idx cname uw
  # `idx` is read only to reach the trailing name/uw fields — the identity match is on NAME.
  # shellcheck disable=SC2034
  while IFS='|' read -r tag zone idx cname uw; do
    if [ "$tag" = "CONSTRAINT" ] && [ "$zone" = "package-0" ] && [ "$cname" = "long_term" ]; then
      [ -n "$uw" ] || return 1
      printf '%s\n' "$uw"
      return 0
    fi
  done <<< "$gather"
  return 1
}

# imag_power_pl1_enabled GATHER -> echoes the `enabled` value of the package-0 zone (the
# ENABLED|package-0|<v> row), or empty + return 1.
imag_power_pl1_enabled() {
  local gather="$1" tag zone val
  while IFS='|' read -r tag zone val; do
    if [ "$tag" = "ENABLED" ] && [ "$zone" = "package-0" ]; then
      printf '%s\n' "$val"
      return 0
    fi
  done <<< "$gather"
  return 1
}

# _imag_power_thermald_field GATHER FIELDNO -> echoes field FIELDNO (1..3 = dpkg|active|enabled) of
# the THERMALD row, or empty + return 1 when there is no THERMALD row. Internal helper.
_imag_power_thermald_field() {
  local gather="$1" fno="$2" tag f1 f2 f3
  while IFS='|' read -r tag f1 f2 f3; do
    if [ "$tag" = "THERMALD" ]; then
      case "$fno" in 1) printf '%s\n' "$f1" ;; 2) printf '%s\n' "$f2" ;; 3) printf '%s\n' "$f3" ;; esac
      return 0
    fi
  done <<< "$gather"
  return 1
}

# imag_power_envelope_verdict GATHER PINNED_WATTS -> echoes one `<facet>|<STATUS>|<detail>` line
# per facet (facets: pl1, slpc, thermald, units; STATUS in OK / DRIFT / UNKNOWN). Both callers
# iterate the lines and map each to their own report style + exit-code contract. An EMPTY gather,
# an unread facet, or a missing pin is UNKNOWN — never a false DRIFT for a mere SSH hiccup.
imag_power_envelope_verdict() {
  local gather="$1" pinned_watts="$2"

  # --- pl1: long_term uW must equal the pinned watts AND the constraint must be enabled ---------
  local pl1_uw pl1_en
  pl1_uw="$(imag_power_zone_select "$gather" || true)"
  pl1_en="$(imag_power_pl1_enabled "$gather" || true)"
  if [ -z "$pl1_uw" ]; then
    printf 'pl1|UNKNOWN|package-0 long_term constraint not gathered\n'
  elif [ -z "$pinned_watts" ]; then
    printf 'pl1|UNKNOWN|no pinned power_pl1_w_imag\n'
  elif imag_pl1_uw_matches_pin "$pl1_uw" "$pinned_watts" && [ "$pl1_en" = "1" ]; then
    printf 'pl1|OK|long_term=%suW (=%sW) enabled=1\n' "$pl1_uw" "$pinned_watts"
  else
    printf 'pl1|DRIFT|long_term=%suW enabled=%s, expected %sW enabled=1\n' \
      "$pl1_uw" "${pl1_en:-<none>}" "$pinned_watts"
  fi

  # --- slpc: every discovered slpc_ignore_eff_freq knob must read 1 -----------------------------
  local tag val slpc_n=0 slpc_bad=0
  while IFS='|' read -r tag val; do
    if [ "$tag" = "SLPC" ]; then
      slpc_n=$((slpc_n + 1))
      [ "$val" = "1" ] || slpc_bad=$((slpc_bad + 1))
    fi
  done <<< "$gather"
  if [ "$slpc_n" -eq 0 ]; then
    printf 'slpc|UNKNOWN|no slpc_ignore_eff_freq knob discovered\n'
  elif [ "$slpc_bad" -eq 0 ]; then
    printf 'slpc|OK|%s knob(s) all read 1\n' "$slpc_n"
  else
    printf 'slpc|DRIFT|%s of %s slpc knob(s) not 1\n' "$slpc_bad" "$slpc_n"
  fi

  # --- thermald: PURGED — not installed (even masked), not active, enabled-state neutral --------
  # Reuses the generic dpkg_status_installed / timesync_enabled_state_neutral (timesync-authority.sh).
  local td_dpkg td_active td_enabled
  if _imag_power_thermald_field "$gather" 1 >/dev/null; then
    td_dpkg="$(_imag_power_thermald_field "$gather" 1)"
    td_active="$(_imag_power_thermald_field "$gather" 2)"
    td_enabled="$(_imag_power_thermald_field "$gather" 3)"
    if dpkg_status_installed "$td_dpkg"; then
      printf 'thermald|DRIFT|INSTALLED (even masked) — purge it, masking is not enough\n'
    elif [ "$(printf '%s' "$td_active" | tr -d '[:space:]')" = "active" ]; then
      printf 'thermald|DRIFT|ACTIVE — its DPTF policy is what programs the 25W clamp\n'
    elif ! timesync_enabled_state_neutral "$td_enabled"; then
      printf 'thermald|DRIFT|enabled (state=%s) — must be purged/absent\n' "${td_enabled:-<none>}"
    else
      printf 'thermald|OK|purged (not installed, inactive, neutral)\n'
    fi
  else
    printf 'thermald|UNKNOWN|thermald state not gathered\n'
  fi

  # --- units: BOTH the oneshot AND the guard timer must be enabled+active ------------------------
  # A correct PL1 with a DEAD guard is the "provisioned but unsupervised" shape (#1015 class).
  local name uen uact seen_env=0 seen_guard=0 units_bad=0 units_seen=0
  while IFS='|' read -r tag name uen uact; do
    [ "$tag" = "UNIT" ] || continue
    case "$name" in
      "$IMAG_POWER_ENVELOPE_UNIT") seen_env=1 ;;
      "$IMAG_POWER_GUARD_UNIT") seen_guard=1 ;;
      *) continue ;;
    esac
    units_seen=$((units_seen + 1))
    if [ "$(printf '%s' "$uen" | tr -d '[:space:]')" != "enabled" ] \
      || [ "$(printf '%s' "$uact" | tr -d '[:space:]')" != "active" ]; then
      units_bad=$((units_bad + 1))
    fi
  done <<< "$gather"
  if [ "$seen_env" -ne 1 ] || [ "$seen_guard" -ne 1 ]; then
    printf 'units|UNKNOWN|envelope unit + guard timer state not both gathered\n'
  elif [ "$units_bad" -eq 0 ]; then
    printf 'units|OK|%s + %s both enabled+active\n' "$IMAG_POWER_ENVELOPE_UNIT" "$IMAG_POWER_GUARD_UNIT"
  else
    printf 'units|DRIFT|%s of %s envelope units not enabled+active — provisioned but unsupervised\n' \
      "$units_bad" "$units_seen"
  fi
}

# imag_power_guard_decision CURRENT_UW EXPECTED_UW STEPDOWN_UW TCPU_C CEIL_C RESTORE_C \
#                           HOT_STREAK COOL_STREAK STEPPED_DOWN
#   -> echoes exactly one action: stepdown | restore | reassert | hold.
# The PURE core of imag-power-envelope-guard.sh. State (HOT_STREAK/COOL_STREAK/STEPPED_DOWN) is
# carried across runs by the guard script's own state file; this function only decides.
#   - TCPU at/above the ceiling with >=1 PRIOR consecutive hot read (this makes 2) AND not already
#     stepped down -> stepdown (never on the FIRST hot read — a single spike is not a trend).
#   - already stepped down, TCPU below the restore threshold with >=1 PRIOR consecutive cool read
#     (sustained) -> restore.
#   - not stepped down, temperature nominal, but the live PL1 no longer equals the expected
#     envelope (a foreign re-program) -> reassert.
#   - anything else, or an unreadable temperature -> hold (a blind step on a missing sensor is
#     forbidden — hold is the safe no-op). STEPDOWN_UW is unused by the decision itself (the guard
#     script applies it) but kept in the signature so the call site reads as the full state.
imag_power_guard_decision() {
  local current_uw="$1" expected_uw="$2" stepdown_uw="$3"
  local tcpu="$4" ceil="$5" restore="$6"
  local hot_streak="$7" cool_streak="$8" stepped_down="$9"
  local tcpu_numeric=0
  case "$tcpu" in '' | *[!0-9-]*) tcpu_numeric=0 ;; *) tcpu_numeric=1 ;; esac
  : "$stepdown_uw"  # documented as part of the guard's state; applied by the script, not here

  # Thermal step-down: only when the temperature is readable, at/above the ceiling, this is the
  # 2nd+ consecutive hot read, and we are not already stepped down.
  if [ "$tcpu_numeric" -eq 1 ] && [ "$stepped_down" != "1" ] \
    && [ "$tcpu" -ge "$ceil" ] && [ "$hot_streak" -ge 1 ]; then
    printf 'stepdown\n'
    return 0
  fi
  # Thermal restore: only when stepped down, the temperature is readably below the restore
  # threshold, and the recovery is sustained (2nd+ consecutive cool read).
  if [ "$stepped_down" = "1" ] && [ "$tcpu_numeric" -eq 1 ] \
    && [ "$tcpu" -lt "$restore" ] && [ "$cool_streak" -ge 1 ]; then
    printf 'restore\n'
    return 0
  fi
  # Foreign re-program: at the nominal envelope (not stepped down) but the live PL1 drifted off
  # the expected value — re-assert it (loudly, via the guard script's marker).
  if [ "$stepped_down" != "1" ] && [ -n "$current_uw" ] && [ -n "$expected_uw" ] \
    && [ "$current_uw" != "$expected_uw" ]; then
    printf 'reassert\n'
    return 0
  fi
  printf 'hold\n'
}

# imag_power_pl1_pin_from_readme_text README_TEXT -> echoes the pinned `power_pl1_w_imag` watts from
# the vendor/README.md pinned-settings table row `| `power_pl1_w_imag` | `N` | ... |`, or empty. Lets
# verify-imag.sh read the SAME authority drift-guard.sh reads (its `pinned_setting`), so the strict
# gate and the acceptance gate never check DIFFERENT wattages after a deliberate re-pin. Pure (takes
# the README text, not a path) so it is testable against a fixture.
imag_power_pl1_pin_from_readme_text() {
  # The backticks below are LITERAL markdown delimiters in the grep/sed patterns, not command
  # substitution -- they sit inside single-quoted patterns (same convention + disable as
  # drift-guard.sh's pinned_setting).
  # shellcheck disable=SC2016
  printf '%s\n' "$1" \
    | grep -aE '\| *`power_pl1_w_imag` *\|' \
    | sed -n 's/^[^|]*|[^|]*|[[:space:]]*`\([^`]*\)`.*/\1/p' | head -1 || true
}

# imag_power_guard_next_streaks ACTION THIS_HOT THIS_COOL HOT COOL STEPPED -> echoes the next
# `HOT COOL STEPPED` guard state (space-separated) after applying ACTION to the prior state. Kept
# PURE + separate from imag_power_guard_decision so the guard script's own streak bookkeeping (the
# "2 consecutive" accounting) is unit-tested, not merely correct-by-inspection: a stepdown/restore
# resets both streaks and flips the stepped flag; a hold/reassert advances the matching streak
# (this read hot -> HOT+1, cool -> COOL+1, neither -> reset that streak) and keeps the flag.
imag_power_guard_next_streaks() {
  local action="$1" this_hot="$2" this_cool="$3" hot="$4" cool="$5" stepped="$6"
  case "$action" in
    stepdown) printf '0 0 1\n' ;;
    restore)  printf '0 0 0\n' ;;
    *)
      local nhot=0 ncool=0
      [ "$this_hot" = "1" ] && nhot=$((hot + 1))
      [ "$this_cool" = "1" ] && ncool=$((cool + 1))
      printf '%s %s %s\n' "$nhot" "$ncool" "${stepped:-0}"
      ;;
  esac
}

# imag_power_guard_stepped_from_state STATE_TEXT -> echoes exactly one of `stepped | not-stepped |
# unknown`, classifying the guard's /run state file BODY (shell KEY=value lines HOT=/COOL=/STEPPED=,
# written by imag_power_guard_next_streaks via scripts/imag-power-envelope-guard.sh). `stepped` iff a
# STEPPED=1 line is present; `not-stepped` iff a STEPPED=<other> line is present; `unknown` for an
# empty/absent body OR one with no STEPPED= line at all (a truncated/corrupt file, or a box whose
# guard has not ticked yet) -- so a consumer (verify-imag.sh's acceptance gate, #1188) NEVER masks a
# genuine foreign drift when it cannot actually CONFIRM the guard stepped down. Co-located here with
# the state PRODUCER so the format's reader and writer never drift. Pure: takes the file TEXT (the
# caller reads the file over SSH), no I/O, and ALWAYS returns 0 (a set -euo pipefail caller invokes
# it inside a `$(...)`, so it must never abort the caller on an empty/malformed read -- the #1133
# class).
imag_power_guard_stepped_from_state() {
  local text="$1" line seen=0 val=""
  [ -n "$text" ] || { printf 'unknown\n'; return 0; }
  while IFS= read -r line; do
    case "$line" in
      STEPPED=*) seen=1; val="${line#STEPPED=}" ;;
    esac
  done <<< "$text"
  [ "$seen" -eq 1 ] || { printf 'unknown\n'; return 0; }
  # keep only digits (a sourced value could carry surrounding whitespace / a stray CR); tr drains
  # fully so there is no SIGPIPE-under-pipefail hazard here.
  val="$(printf '%s' "$val" | tr -cd '0-9')"
  if [ "$val" = "1" ]; then printf 'stepped\n'; else printf 'not-stepped\n'; fi
}

# imag_power_alert_condition JOURNAL -> echoes the concerning-transition marker line(s)
# (STEP-DOWN | RE-ASSERT) found in a `journalctl -t imag-power-envelope` window, or empty if none.
# The dev1-side alert watchdog pages on these — a thermal step-down (the box is being clamped) or a
# foreign re-program (something fought the envelope). RESTORE (recovery) is deliberately NOT paged;
# a clamp that self-heals is informational, not an incident. Used by
# scripts/imag-power-envelope-alert-watchdog.sh with the shared alert throttle.
imag_power_alert_condition() {
  printf '%s\n' "$1" | grep -aE 'STEP-DOWN|RE-ASSERT' || true
}

# imag_power_throttle_alert_condition GATHER -> echoes a THROTTLE-UNDER-FLOOR marker line when a
# MAJORITY (>= IMAG_POWER_THROTTLE_ALERT_PCT %, default 50) of the burst samples show a genuine
# power/thermal clamp holding the ACTUAL iGPU freq BELOW the pinned floor. This is the SECOND #880
# alert path (independent of the guard's STEP-DOWN/RE-ASSERT journal markers): the #841 floor pin
# (gt_min_freq_mhz=1400) is a software REQUEST the punit legally overrides at the MMIO RAPL PL1
# power budget, so under load `Actual` drops below the floor with throttle_reason_pl1=1 and NO guard
# step-down (the guard only steps down on a TCPU excursion) -- silent judder. It keys on
# throttle_reason, NOT raw act_freq: a benign RC6-idle burst (act low/0 but every throttle_reason 0)
# must NEVER page (the exact false positive the ticket body warns of). Input (from
# imag_power_throttle_burst_remote_snippet): one `FLOOR|<mhz>` line + N
# `THROTSAMPLE|<pl1>|<thermal>|<status>|<act>` lines. A "clamp sample" = (pl1=1 OR thermal=1) AND a
# numeric act STRICTLY below the floor. Empty gather / no numeric FLOOR / no THROTSAMPLE lines ->
# empty output (never a false alert on an ssh hiccup). Two-pass so the FLOOR line's position in the
# block does not matter.
# _imag_power_throttle_parse_burst GATHER -> echoes `<total>|<clamped>|<floor>`: total THROTSAMPLE
# lines, how many were genuinely clamped ((pl1=1 OR thermal=1) AND a numeric act STRICTLY below the
# floor), and the numeric FLOOR. Empty + return 1 when there is no numeric FLOOR line (an ssh hiccup
# / a box with no i915 freq surface -> "nothing to decide"). The ONE burst-parse primitive shared by
# imag_power_throttle_alert_condition (the 2-state #880 marker) and imag_power_throttle_state (the
# 3-state #799 discriminator input) so the two can never disagree about what "clamped" means -- no
# two driftable copies. Two-pass so the FLOOR line's position in the block does not matter.
_imag_power_throttle_parse_burst() {
  local gather="$1" floor="" total=0 clamped=0 tag a b d
  # pass 1: the numeric floor (a non-numeric/empty FLOOR is treated as "no floor" -> no decision).
  while IFS='|' read -r tag a; do
    if [ "$tag" = "FLOOR" ]; then
      case "$a" in '' | *[!0-9]*) : ;; *) floor="$a" ;; esac
    fi
  done <<< "$gather"
  [ -n "$floor" ] || return 1
  # pass 2: count burst samples and how many are genuinely clamped below the floor.
  while IFS='|' read -r tag a b _ d; do
    [ "$tag" = "THROTSAMPLE" ] || continue
    total=$((total + 1))
    # a=pl1 b=thermal (status is read into _ and intentionally unused -- pl1/thermal is the precise
    # power/thermal-envelope clamp signal; status can also be prochot/ratl/vr which are not this).
    if [ "$a" = "1" ] || [ "$b" = "1" ]; then
      case "$d" in
        '' | *[!0-9]*) : ;;
        *) [ "$d" -lt "$floor" ] && clamped=$((clamped + 1)) ;;
      esac
    fi
  done <<< "$gather"
  printf '%s|%s|%s\n' "$total" "$clamped" "$floor"
}

imag_power_throttle_alert_condition() {
  local gather="$1" pct="${IMAG_POWER_THROTTLE_ALERT_PCT:-50}" min="${IMAG_POWER_THROTTLE_MIN_SAMPLES:-6}"
  # Validate the tunables the same way floor/act are validated below -- a non-numeric override must
  # not collapse the threshold (a bare $((pct*total)) with pct non-numeric evaluates to 0, firing on
  # a single sample) or the min-sample floor.
  case "$pct" in '' | *[!0-9]*) pct=50 ;; esac
  case "$min" in '' | *[!0-9]*) min=6 ;; esac
  local parsed total clamped floor
  parsed="$(_imag_power_throttle_parse_burst "$gather")" || return 0
  IFS='|' read -r total clamped floor <<< "$parsed"
  # A partial burst (ssh dropped mid-sample) must not read e.g. a 2/2 capture as "sustained" -- the
  # burst emits ~12 samples, so require a minimum before deciding (fewer = "nothing to decide").
  [ "$total" -ge "$min" ] || return 0
  if [ "$clamped" -gt 0 ] && [ "$((clamped * 100))" -ge "$((pct * total))" ]; then
    printf 'THROTTLE-UNDER-FLOOR: %s/%s burst samples held act<%sMHz while PL1/thermal-clamped (threshold %s%%)\n' \
      "$clamped" "$total" "$floor" "$pct"
  fi
}

# imag_power_throttle_state GATHER -> echoes exactly one of `clamped | clean | unknown` -- the
# 3-state refinement the #799 render-cause discriminator needs (the 2-state alert_condition above
# collapses "clean" and "unknown" into one empty output, which cannot tell a healthy GPU apart from
# an unread one). clamped = a MAJORITY of a valid burst is power/thermal-clamped under the floor (the
# GPU is being starved); clean = a valid burst (>= min samples, FLOOR present) with NO clamped
# majority = the GPU has HEADROOM; unknown = no FLOOR / too few samples (an ssh hiccup) -> cannot
# judge (never a false clean/clamped). Same tunables + primitive as alert_condition.
imag_power_throttle_state() {
  local gather="$1" pct="${IMAG_POWER_THROTTLE_ALERT_PCT:-50}" min="${IMAG_POWER_THROTTLE_MIN_SAMPLES:-6}"
  case "$pct" in '' | *[!0-9]*) pct=50 ;; esac
  case "$min" in '' | *[!0-9]*) min=6 ;; esac
  local parsed total clamped floor
  parsed="$(_imag_power_throttle_parse_burst "$gather")" || { printf 'unknown\n'; return 0; }
  IFS='|' read -r total clamped floor <<< "$parsed"
  [ "$total" -ge "$min" ] || { printf 'unknown\n'; return 0; }
  if [ "$clamped" -gt 0 ] && [ "$((clamped * 100))" -ge "$((pct * total))" ]; then
    printf 'clamped\n'
  else
    printf 'clean\n'
  fi
}

# imag_power_throttle_alert_sig MARKER -> echoes a STABLE dedup signature for a throttle-under-floor
# episode. The MARKER line carries a fluctuating <clamped>/<total> count that changes burst-to-burst
# during ONE ongoing clamp (the GPU is pinned busy under render, so nearly every 5-min pass yields a
# different count); embedding that count in the dedup signature would make obs_watchdog_alert_throttle
# see a "new" signature every pass and re-page constantly instead of once-then-suppress. So the sig is
# the STABLE episode identity "under-floor", independent of the count -- the count stays in the alert
# body/detail, never the signature. Pure + separately testable so the stability is proven, not merely
# correct-by-inspection.
imag_power_throttle_alert_sig() {
  # $1 (the marker) is intentionally not interpolated -- the signature is the condition identity, not
  # the fluctuating measurement. Kept in the signature so a future bucketed variant has a seam.
  : "${1:-}"
  printf 'imag-throttle:under-floor\n'
}

# =============================================================================================
# #799 -- the render-degradation CAUSE discriminator.
#
# Two DISTINCT causes produce the same "OBS render budget blown after hours, restart clears it"
# symptom on imag-nb: (a) the issue-880/1043 power/thermal clamp (GPU steered below the pinned
# floor -- imag_power_throttle_state == clamped) and (b) THIS ticket's connection-churn render
# leak (render time creeps while the GPU has HEADROOM -- throttle clean). The salvaged plan:
# read render stats + gt_act_freq + throttle_reason SIMULTANEOUSLY and NAME which cause is active,
# instead of one ambiguous "render degraded" alert. All PURE (no I/O) so drift-guard/verify-imag
# and the Tier-0 test harness exercise them directly.
# =============================================================================================

# imag_render_degraded_from_sample RENDER_LINE -> echoes exactly one of
# `degraded | healthy | stalled | unknown`, classifying a
# `RENDER|<active_fps>|<avg_ms>|<render_skipped_frac>|<render_advanced>` line (emitted by the dev1
# reader scripts/imag-render-stats.py) against the imag 60fps render budget. The thresholds MIRROR
# src/render_budget.rs (the single source of the physical 60fps deadline): budget 1000/60=16.67ms,
# fps floor = target - 2 = 58, render-skip tolerance 5%.
#   - render_advanced=false -> a FULL render-loop stall (activeFps LIES here, #935): that is the
#     #391 obs-liveness FpsZero path's domain, NOT this partial-degrade discriminator -> `stalled`
#     (so the two watchdogs never double-alert one stall).
#   - avg_ms is the trustworthy primary signal (it does NOT lie like activeFps): avg over budget,
#     OR render-skip over tolerance, OR (activeFps < 58 ONLY when advancement is CONFIRMED true --
#     never trust a low activeFps otherwise, #935) -> `degraded`.
#   - malformed / empty / non-numeric avg -> `unknown` (never a false signal on an ssh/WS hiccup).
imag_render_degraded_from_sample() {
  local line="$1" tag afps avg skip adv
  IFS='|' read -r tag afps avg skip adv <<< "$line"
  [ "$tag" = "RENDER" ] || { printf 'unknown\n'; return 0; }
  # avg_ms is the primary trustworthy signal; without a numeric avg we cannot judge.
  case "$avg" in '' | *[!0-9.]*) printf 'unknown\n'; return 0 ;; esac
  # a full render stall is #391's FpsZero domain, not this partial-degrade discriminator.
  [ "$adv" = "false" ] && { printf 'stalled\n'; return 0; }
  # avg over the 60fps frame budget (1000/60 = 16.66666667 ms) -> degraded. awk for the float compare.
  if awk -v a="$avg" 'BEGIN { exit !(a + 0 > 16.66666667) }'; then printf 'degraded\n'; return 0; fi
  # render-skip fraction over the 5% tolerance -> degraded (non-numeric skip = signal absent).
  case "$skip" in
    '' | *[!0-9.]*) : ;;
    *) awk -v s="$skip" 'BEGIN { exit !(s + 0 > 0.05) }' && { printf 'degraded\n'; return 0; } ;;
  esac
  # activeFps below the 58 floor is trustworthy ONLY when renderTotalFrames is confirmed advancing.
  if [ "$adv" = "true" ]; then
    case "$afps" in
      '' | *[!0-9.]*) : ;;
      *) awk -v f="$afps" 'BEGIN { exit !(f + 0 < 58) }' && { printf 'degraded\n'; return 0; } ;;
    esac
  fi
  printf 'healthy\n'
}

# imag_render_cause_from_signals RENDER_LINE BURST -> echoes one `<cause>|<detail>` line naming
# WHICH cause is active, fusing imag_render_degraded_from_sample (render stats) with
# imag_power_throttle_state (the GPU throttle burst). Causes:
#   healthy      -- render within the 60fps budget (no alert)
#   stalled      -- render loop fully stalled (defer to the #391 obs-liveness FpsZero path)
#   unknown      -- render sample unreadable, OR render degraded but the throttle burst is unreadable
#                   (cannot attribute this pass -- never a false churn blame)
#   power-clamp  -- render degraded WHILE the iGPU is power/thermal-clamped (issue 880/1043), NOT a
#                   churn leak (the existing throttle alert already pages the clamp)
#   churn-leak   -- render degraded while the iGPU has HEADROOM (throttle clean): the #799
#                   connection-churn leak an OBS restart clears -- the genuinely new, previously
#                   silent case
imag_render_cause_from_signals() {
  local render_line="$1" burst="$2" rstate tstate
  rstate="$(imag_render_degraded_from_sample "$render_line")"
  case "$rstate" in
    degraded) : ;;
    healthy) printf 'healthy|render within the 60fps budget\n'; return 0 ;;
    stalled) printf 'stalled|render loop fully stalled -- defer to the #391 obs-liveness FpsZero path\n'; return 0 ;;
    *) printf 'unknown|render sample not readable\n'; return 0 ;;
  esac
  tstate="$(imag_power_throttle_state "$burst")"
  case "$tstate" in
    clamped) printf 'power-clamp|render degraded WHILE the iGPU is power/thermal-clamped below the floor -- the issue 880/1043 power/cooling envelope, not a churn leak (see the throttle alert)\n' ;;
    clean) printf 'churn-leak|render degraded while the iGPU has headroom (throttle clean) -- accumulated per-process NDI-receive->texture-upload state (#799); a graceful OBS restart clears it, NOT the power clamp\n' ;;
    *) printf 'unknown|render degraded but the GPU throttle burst is unreadable -- cannot attribute the cause this pass\n' ;;
  esac
}

# imag_power_throttle_render_gate RENDER_LINE -> echoes `page | log-only`. Gates the #880
# throttle-under-floor Discord PAGE on whether OBS render is ACTUALLY suffering. The iGPU sitting
# below the pinned floor is a CHRONIC hardware condition (the punit steers below the software floor
# at the power/thermal envelope; the cooling residual is the technician ticket, issue 1043) — a page
# on it is only ACTIONABLE when it is degrading OBS render. So: `log-only` (no Discord) only when the
# render sample reads a clean, advancing 60fps render (`healthy`); `page` for everything else. This
# FAILS OPEN on `stalled`/`unknown`/unreadable/malformed render — an unreadable render sample must
# NEVER SILENTLY suppress a real clamp alert (the standing rig-alert rule: loud, never silent). Pure:
# reuses the same imag_render_degraded_from_sample classifier the #799 discriminator uses, so there is
# no second WS probe and the two paths can never disagree about what "render healthy" means.
imag_power_throttle_render_gate() {
  case "$(imag_render_degraded_from_sample "${1:-}")" in
    healthy) printf 'log-only\n' ;;
    *)       printf 'page\n' ;;
  esac
}

# imag_power_envelope_gather_remote_snippet -> the REMOTE shell command (a string) both callers run
# over their own transport to collect the observed envelope state into the `|`-delimited block
# imag_power_envelope_verdict parses. Hardware-agnostic (issue 816): a box with no mmio RAPL zone
# emits no ZONE/CONSTRAINT lines (the verdict then reads pl1 UNKNOWN, never a false DRIFT); a box
# that HAS the zone emits its real values. Selection is by NAME identity throughout (never a
# hardcoded intel-rapl-mmio index / cardN — the presenter-drm renumbering hazard).
imag_power_envelope_gather_remote_snippet() {
  cat <<'REMOTE'
# MMIO RAPL zones: emit each zone's name, each constraint's name+limit, and the enabled flag.
for _z in /sys/class/powercap/intel-rapl-mmio:*/; do
  [ -e "${_z}name" ] || continue
  _zn="$(cat "${_z}name" 2>/dev/null || true)"
  printf 'ZONE|%s\n' "$_zn"
  for _cn in "${_z}"constraint_*_name; do
    [ -e "$_cn" ] || continue
    _idx="${_cn##*constraint_}"; _idx="${_idx%_name}"
    _cname="$(cat "$_cn" 2>/dev/null || true)"
    _cuw="$(cat "${_z}constraint_${_idx}_power_limit_uw" 2>/dev/null || true)"
    printf 'CONSTRAINT|%s|%s|%s|%s\n' "$_zn" "$_idx" "$_cname" "$_cuw"
  done
  printf 'ENABLED|%s|%s\n' "$_zn" "$(cat "${_z}enabled" 2>/dev/null || true)"
done
# iGPU SLPC efficient-freq override knob — glob across ALL drm cards (cardN renumbers).
for _s in /sys/class/drm/card*/gt/gt*/slpc_ignore_eff_freq; do
  [ -e "$_s" ] || continue
  printf 'SLPC|%s\n' "$(cat "$_s" 2>/dev/null || true)"
done
# thermald must be PURGED — gather dpkg/active/enabled so the verdict can reject even a masked one.
printf 'THERMALD|%s|%s|%s\n' \
  "$(dpkg -s thermald 2>/dev/null | sed -n 's/^Status: //p' || true)" \
  "$(systemctl is-active thermald 2>/dev/null || true)" \
  "$(systemctl is-enabled thermald 2>/dev/null || true)"
# The two envelope units (enabled+active) — a correct PL1 with a dead guard is unsupervised.
for _u in imag-power-envelope.service imag-power-envelope-guard.timer; do
  printf 'UNIT|%s|%s|%s\n' "$_u" \
    "$(systemctl is-enabled "$_u" 2>/dev/null || true)" \
    "$(systemctl is-active "$_u" 2>/dev/null || true)"
done
# TCPU: the package temperature (x86_pkg_temp thermal zone), in whole Celsius. Selected by TYPE
# identity, never a hardcoded thermal_zoneN. Diagnostic ACTFREQ gathered alongside.
_tcpu=""
for _tz in /sys/class/thermal/thermal_zone*; do
  [ -e "${_tz}/type" ] || continue
  if [ "$(cat "${_tz}/type" 2>/dev/null || true)" = "x86_pkg_temp" ]; then
    _t="$(cat "${_tz}/temp" 2>/dev/null || true)"
    [ -n "$_t" ] && _tcpu=$(( _t / 1000 ))
    break
  fi
done
printf 'TCPU|%s\n' "$_tcpu"
_act=""
for _f in /sys/class/drm/card*/gt/gt*/gt_act_freq_mhz /sys/class/drm/card*/gt_act_freq_mhz; do
  [ -e "$_f" ] || continue
  _act="$(cat "$_f" 2>/dev/null || true)"; [ -n "$_act" ] && break
done
printf 'ACTFREQ|%s\n' "$_act"
REMOTE
}

# imag_power_throttle_burst_remote_snippet -> the REMOTE shell command (a string) the dev1 watchdog
# runs over ssh to sample the throttle+freq state OVER TIME (~6 s), for the #880
# imag_power_throttle_alert_condition. Kept SEPARATE from imag_power_envelope_gather_remote_snippet
# (an instantaneous snapshot) so drift-guard.sh / verify-imag.sh are never slowed by a multi-second
# burst -- only the alert watchdog pays it. Emits one `FLOOR|<rps_min_freq_mhz>` line, then 12
# `THROTSAMPLE|<pl1>|<thermal>|<status>|<act>` lines at 0.5 s spacing. Identity-based throughout:
# glob card* (never a hardcoded cardN -- the presenter-drm renumbering hazard); a box with no i915
# freq surface emits an empty FLOOR + empty samples, which the pure condition reads as "nothing to
# decide" (never a false alert). Hardware-agnostic like the snapshot gather (issue 816).
imag_power_throttle_burst_remote_snippet() {
  cat <<'REMOTE'
# #880 throttle burst: the pinned min-freq FLOOR once, then 12 throttle+act samples over ~6 s.
_floor=""
for _m in /sys/class/drm/card*/gt/gt*/rps_min_freq_mhz /sys/class/drm/card*/gt_min_freq_mhz; do
  [ -e "$_m" ] || continue
  _floor="$(cat "$_m" 2>/dev/null || true)"; [ -n "$_floor" ] && break
done
printf 'FLOOR|%s\n' "$_floor"
_i=0
while [ "$_i" -lt 12 ]; do
  _pl1=""; _th=""; _st=""; _act=""
  for _f in /sys/class/drm/card*/gt/gt*/throttle_reason_pl1; do
    [ -e "$_f" ] && { _pl1="$(cat "$_f" 2>/dev/null || true)"; break; }
  done
  for _f in /sys/class/drm/card*/gt/gt*/throttle_reason_thermal; do
    [ -e "$_f" ] && { _th="$(cat "$_f" 2>/dev/null || true)"; break; }
  done
  for _f in /sys/class/drm/card*/gt/gt*/throttle_reason_status; do
    [ -e "$_f" ] && { _st="$(cat "$_f" 2>/dev/null || true)"; break; }
  done
  for _f in /sys/class/drm/card*/gt/gt*/rps_act_freq_mhz /sys/class/drm/card*/gt_act_freq_mhz; do
    [ -e "$_f" ] && { _act="$(cat "$_f" 2>/dev/null || true)"; break; }
  done
  printf 'THROTSAMPLE|%s|%s|%s|%s\n' "$_pl1" "$_th" "$_st" "$_act"
  _i=$((_i + 1))
  sleep 0.5
done
REMOTE
}
