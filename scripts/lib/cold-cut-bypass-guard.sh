#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements) — same
# convention as scripts/lib/cold-cut-step.sh: sourcing runs in the CALLER's shell (the CI
# arm-check step), which sets its own `set -euo pipefail`; no top-level statements here, and every
# function ALWAYS `return`s so a no-op branch can never trip the caller's `set -e`.
#
# scripts/lib/cold-cut-bypass-guard.sh — #1086 ARM-TIME guard + LOUD banner for the deliberate
# keepalive-bypass cold cut (the sibling runtime lib is scripts/lib/cold-cut-step.sh).
#
# WHY: full-path-e2e.yml sources COLD_CUT_BYPASS_CAM / COLD_CUT_BYPASS_INPUT from REPOSITORY
# VARIABLES so a genuine-cold run is a variable flip with no code change. A repository variable is
# GLOBAL — it applies to EVERY dev→main PR gate run until cleared — so a stuck/typo'd value would
# silently idle a LIVE strih receiver run after run. This guard, called by a CI step BEFORE the
# ~30-min recording step, makes an armed bypass LOUD and fail-CLOSED before any rig time is spent:
#   - both variables empty (the natural unset state) ⇒ a SILENT no-op, exit 0 — a normal gate run
#     is byte-for-byte unaffected, and the guard can NEVER fire when the variable is empty.
#   - COLD_CUT_BYPASS_CAM set ⇒ a LOUD ARMED banner naming both values, then REJECT (exit 1,
#     ::error::) a target outside the current-sweep 2nd-cut set — the cold-cut onset is measured on
#     a cambox's SECOND program cut, and only those cameras get a second cut, so any other target
#     would idle a receiver whose sweep never re-cuts to it (NO genuine cold-cut measured, a live
#     input left torn down). It also rejects a set CAM with an empty INPUT early (mirrors
#     cold-cut-step.sh's cold_cut_reset_state, but before the recording starts, not mid-run).
#   - COLD_CUT_BYPASS_INPUT set but COLD_CUT_BYPASS_CAM empty ⇒ the bypass is INERT (cold-cut-step.sh
#     keys arming on COLD_CUT_BYPASS_CAM), so warn LOUDLY that it is NOT armed and exit 0 (an inert
#     bypass idles nothing — safe, not a hard error).

# The canonical set of sweep labels whose receiver the bypass may idle: the CURRENT all-cambox
# sweep only re-cuts to (i.e. gives a SECOND program cut to) CAM1/CAM2/CAM3, and the cold-cut onset
# is measured on that second cut (issue 1086 acceptance). ONE source of truth for both the guard
# and its tests; env-overridable so a future sweep change that gives more boxes a 2nd cut is a
# one-line widen here (COLD_CUT_BYPASS_VALID_TARGETS), never a code hunt.
cold_cut_bypass_valid_targets() { printf '%s' "${COLD_CUT_BYPASS_VALID_TARGETS:-CAM1 CAM2 CAM3}"; }

# Return 0 iff $1 is one of the valid bypass targets (exact whole-token match, never a substring).
cold_cut_bypass_target_valid() {
  local want="$1" t
  for t in $(cold_cut_bypass_valid_targets); do
    [ "$t" = "$want" ] && return 0
  done
  return 1
}

# Arm-time guard + loud banner. Called by full-path-e2e.yml's arm-check step BEFORE the recording
# step. See the header for the full state table. ALWAYS returns 0 on a safe state (both empty, or
# INERT), and returns non-zero (with a printed ::error::) on a genuinely misconfigured ARMED state.
cold_cut_bypass_arm_check() {
  local cam="${COLD_CUT_BYPASS_CAM:-}" input="${COLD_CUT_BYPASS_INPUT:-}"

  # OFF BY DEFAULT: both variables empty ⇒ silent no-op. The guard NEVER fires when unset.
  if [ -z "$cam" ] && [ -z "$input" ]; then
    return 0
  fi

  # INPUT set but CAM empty: half-configured. cold-cut-step.sh keys arming on COLD_CUT_BYPASS_CAM,
  # so the bypass is INERT (it idles nothing). Warn LOUDLY so nobody believes it is armed; safe ⇒ 0.
  if [ -z "$cam" ]; then
    echo "::warning::#1086 cold-cut keepalive-bypass: COLD_CUT_BYPASS_INPUT='${input}' is set but COLD_CUT_BYPASS_CAM is empty — the bypass is INERT (NOT armed; it idles nothing). Set COLD_CUT_BYPASS_CAM to a valid target ($(cold_cut_bypass_valid_targets)) to arm it, or clear COLD_CUT_BYPASS_INPUT."
    return 0
  fi

  # Armed. Print the LOUD banner naming BOTH values first (visible even when we then reject).
  echo ">>> #1086 cold-cut keepalive-bypass ARMED <<< COLD_CUT_BYPASS_CAM='${cam}' COLD_CUT_BYPASS_INPUT='${input}' — this run will deliberately idle that strih NDI receiver COLD and restore it before its 2nd program cut, to measure a GENUINE cold-cut onset. Clear BOTH repository variables to disarm."

  if ! cold_cut_bypass_target_valid "$cam"; then
    echo "::error::#1086 cold-cut keepalive-bypass: COLD_CUT_BYPASS_CAM='${cam}' is NOT a valid bypass target. Only the current-sweep 2nd-cut cameras ($(cold_cut_bypass_valid_targets)) get a second program cut, so only those yield a genuine cold-cut onset — refusing to idle a receiver whose sweep never re-cuts to it. Set COLD_CUT_BYPASS_CAM to one of those, or clear it to disarm." >&2
    return 1
  fi

  if [ -z "$input" ]; then
    echo "::error::#1086 cold-cut keepalive-bypass: COLD_CUT_BYPASS_CAM='${cam}' is set but COLD_CUT_BYPASS_INPUT is empty — refusing to guess which strih NDI receiver to idle. Set COLD_CUT_BYPASS_INPUT (e.g. 'NDI cam1'), then re-run." >&2
    return 1
  fi

  echo "    #1086 cold-cut keepalive-bypass target '${cam}' is valid; recording-e2e.sh will arm the bypass on strih input '${input}'."
  return 0
}
