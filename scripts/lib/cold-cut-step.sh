#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements) — matches the
# sibling scripts/lib/*.sh convention (camera-box-restart-verify.sh, cambox-parallel-restore.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, and
# recording-e2e.sh (the only caller) already sets it. Every function called from the sweep loop
# ALWAYS `return 0` so a no-op branch can never trip the caller's `set -e` (the #ci-testing-gotchas
# sourced-`set -e`-leak class).
#
# scripts/lib/cold-cut-step.sh — #1086 deliberate keepalive-bypass COLD CUT step for the all-cambox
# sweep. OPT-IN, OFF BY DEFAULT (COLD_CUT_BYPASS_CAM empty ⇒ every function is a pure no-op, so a
# normal E2E run is byte-for-byte inert).
#
# WHY (issue 768 acceptance criterion, deferred to this phase-2 ticket): under the #767 keep-alive
# DistroAV build (PROP_BEHAVIOR_KEEP_ACTIVE) every strih NDI receiver keeps decoding off-program, so
# every natural sweep "cold cut" is actually WARM — a revert of issue 767 (a receiver that never
# rebinds from cold) would NOT redden the issue-768 report-only onset seam. This step temporarily
# bypasses keep-alive for ONE camera: after that camera's FIRST sweep appearance it IDLES its
# receiver (obs_phase2.py idle-receiver clears ndi_source_name ⇒ DistroAV tears it down cold — the
# SAME idle discipline _quiesce_probe_input/teardown use), holds it GENUINELY cold for the whole
# hidden window (>= COLD_CUT_HOLD_SECS, topping up with a short sleep only if the sweep cadence was
# shorter), then RESTORES it right before the sweep's NEXT cut to it ⇒ the issue-768 seam measures a
# real cold-cut onset. A 767 regression then reliably reddens the (report-only) gate.
#
# LIVE CALIBRATION of the LIVE gate flip (cold_cut::gates_overall_pass) stays on the ticket for a
# full-authority session; this only PRODUCES the genuine-cold measurement.
#
# GATING / SAFETY: when active, COLD_CUT_BYPASS_INPUT MUST name the strih NDI input to idle (e.g.
# "NDI cam1") — required, never guessed (a wrong input would idle the wrong receiver on a live box).
# The idle-receiver primitive uses overlay:True, so ONLY ndi_source_name + genlock_fifo change and
# the per-source genlock latency pin is preserved; restore re-points the receiver and re-enables the
# genlock FIFO, leaving the input exactly as it started.

# The sweep LABEL to make genuinely cold (empty = disabled). e.g. COLD_CUT_BYPASS_CAM=CAM1
cold_cut_bypass_cam() { printf '%s' "${COLD_CUT_BYPASS_CAM:-}"; }

# Active iff a bypass label is set.
cold_cut_bypass_active() { [ -n "${COLD_CUT_BYPASS_CAM:-}" ]; }

# The strih NDI input to idle/restore (required when active). No default guess.
cold_cut_bypass_input() { printf '%s' "${COLD_CUT_BYPASS_INPUT:-}"; }

# Minimum genuine-cold hold in seconds: >= 60 (the issue-768 bar), default 62, never below 60.
cold_cut_hold_secs() {
  local h="${COLD_CUT_HOLD_SECS:-62}"
  case "$h" in '' | *[!0-9]*) h=62 ;; esac
  [ "$h" -lt 60 ] && h=60
  printf '%s' "$h"
}

# The per-run state file (phase + captured prev ndi name + idle epoch). Override for tests.
cold_cut_state_file() { printf '%s' "${COLD_CUT_STATE_FILE:-/tmp/cold-cut-bypass.state}"; }

_cold_cut_get() {
  # $1 = key; prints its value (last write wins) or empty.
  sed -n "s/^$1=//p" "$(cold_cut_state_file)" 2>/dev/null | tail -1
}

_cold_cut_set() {
  # $1 = key, $2 = value; rewrites that key in the state file (create if missing).
  local f k v tmp
  f="$(cold_cut_state_file)"; k="$1"; v="$2"
  tmp="${f}.tmp.$$"
  { grep -v "^${k}=" "$f" 2>/dev/null || true; printf '%s=%s\n' "$k" "$v"; } > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$f" 2>/dev/null || true
}

# Initialise the state machine (phase=none). Call ONCE before the sweep loop. No-op when inactive.
cold_cut_reset_state() {
  cold_cut_bypass_active || return 0
  local f
  f="$(cold_cut_state_file)"
  : > "$f" 2>/dev/null || true
  _cold_cut_set phase none
  if [ -z "$(cold_cut_bypass_input)" ]; then
    echo "ERROR #1086: COLD_CUT_BYPASS_CAM='$(cold_cut_bypass_cam)' is set but COLD_CUT_BYPASS_INPUT is empty — refusing to guess which strih NDI receiver to idle. Set COLD_CUT_BYPASS_INPUT (e.g. 'NDI cam1'), then re-run." >&2
    return 1
  fi
  echo "[6c/8] #1086 keepalive-bypass cold cut ARMED: label='$(cold_cut_bypass_cam)' input='$(cold_cut_bypass_input)' hold>=$(cold_cut_hold_secs)s (idle after 1st appearance, restore before the 2nd cut)"
  return 0
}

# Called at the TOP of each sweep iteration, BEFORE the switch to $1's scene. If $1 is the target
# and its receiver is currently idled, top up the cold hold to COLD_CUT_HOLD_SECS then RESTORE it so
# the imminent cut lands on a receiver re-created from cold. ALWAYS returns 0.
#   $1 label  $2 host  $3 password  $4 obs_phase2.py path
cold_cut_before_segment() {
  cold_cut_bypass_active || return 0
  local label="$1" host="$2" pw="$3" obs_py="$4"
  [ "$label" = "$(cold_cut_bypass_cam)" ] || return 0
  [ "$(_cold_cut_get phase)" = "idled" ] || return 0
  local idle_ts hold now waited
  idle_ts="$(_cold_cut_get idle_ts)"
  hold="$(cold_cut_hold_secs)"
  now="$(date +%s)"
  if [ -n "$idle_ts" ]; then
    waited=$(( now - idle_ts ))
    if [ "$waited" -lt "$hold" ]; then
      echo "    #1086: '$(cold_cut_bypass_input)' cold for ${waited}s < ${hold}s — topping up $(( hold - waited ))s before the cold cut"
      interruptible_sleep "$(( hold - waited ))" 2>/dev/null || sleep "$(( hold - waited ))" || true
    fi
  fi
  _cold_cut_do_restore "$host" "$pw" "$obs_py" "for the genuine cold cut to ${label}"
  return 0
}

# Restore the idled target receiver to its captured prev ndi name (shared by before_segment's
# pre-cut restore AND cleanup's abort/single-appearance restore). GUARD: never restore to an EMPTY
# name — `idle-receiver --restore ""` is falsy and would RE-IDLE (leaving the input black); if the
# capture was empty (a failed idle-time read / a genuinely source-less input) warn LOUDLY and mark
# the run skipped instead. Marks phase=restored on a real restore. ALWAYS returns 0.
#   $1 host  $2 password  $3 obs_phase2.py path  $4 reason (for the log line)
_cold_cut_do_restore() {
  local host="$1" pw="$2" obs_py="$3" reason="$4" prev_ndi
  prev_ndi="$(_cold_cut_get prev_ndi)"
  if [ -z "$prev_ndi" ]; then
    echo "    WARNING #1086: no captured prev ndi_source_name for '$(cold_cut_bypass_input)' — NOT restoring (restoring to an empty name would re-idle it black); the input may need a manual OBS re-point. Marking the bypass skipped." >&2
    _cold_cut_set phase skipped
    return 0
  fi
  echo "    #1086: RESTORING '$(cold_cut_bypass_input)' -> '${prev_ndi}' ${reason}"
  python3 "$obs_py" idle-receiver --host "$host" --password "$pw" --input "$(cold_cut_bypass_input)" --restore "$prev_ndi" 2>&1 | sed 's/^/      [cold-cut restore] /' || true
  _cold_cut_set phase restored
  return 0
}

# Best-effort FINAL restore, for recording-e2e.sh's cleanup() EXIT trap: if the target receiver was
# idled but never restored (the run was interrupted during the cold hold, or the sweep cut to the
# target only ONCE so before_segment's restore never fired), re-point it so a live strih input is
# never left torn down black. A NO-OP when the bypass is off, or when the machine already reached
# restored/skipped. ALWAYS returns 0 (a cleanup trap must never abort).
#   $1 host  $2 password  $3 obs_phase2.py path
cold_cut_cleanup_restore() {
  cold_cut_bypass_active || return 0
  [ "$(_cold_cut_get phase)" = "idled" ] || return 0
  echo "[cleanup] #1086: the keepalive-bypass target '$(cold_cut_bypass_input)' was left idled (run interrupted, or single-appearance sweep) — restoring its receiver"
  _cold_cut_do_restore "$1" "$2" "$3" "(cleanup: restore an idled-but-never-restored receiver)"
  return 0
}

# Called at the BOTTOM of each sweep iteration, AFTER the switch to $1's scene. Tracks the target's
# first appearance; the FIRST time the sweep is on a NON-target segment after the target has
# appeared, IDLES the target's receiver (captures its prev ndi_source_name) so it goes cold for the
# hidden window. ALWAYS returns 0.
#   $1 label  $2 host  $3 password  $4 obs_phase2.py path
cold_cut_after_segment() {
  cold_cut_bypass_active || return 0
  local label="$1" host="$2" pw="$3" obs_py="$4" phase
  phase="$(_cold_cut_get phase)"
  if [ "$label" = "$(cold_cut_bypass_cam)" ]; then
    [ "$phase" = "none" ] && _cold_cut_set phase appeared
    return 0
  fi
  # A non-target segment: idle the target once, after it has appeared, and only until restored.
  [ "$phase" = "appeared" ] || return 0
  echo "    #1086: IDLING '$(cold_cut_bypass_input)' (target ${label} off-program) — tearing its receiver down COLD to bypass keep-alive"
  local out prev_ndi
  out="$(python3 "$obs_py" idle-receiver --host "$host" --password "$pw" --input "$(cold_cut_bypass_input)" 2>&1)" || true
  printf '%s\n' "$out" | sed 's/^/      [cold-cut idle] /'
  prev_ndi="$(printf '%s\n' "$out" | sed -n 's/^PREV_NDI_NAME=//p' | tail -1)"
  if [ -z "$prev_ndi" ]; then
    echo "    WARNING #1086: idled '$(cold_cut_bypass_input)' but captured an EMPTY prev ndi_source_name (source-less input, or the idle-time read failed) — the receiver cannot be auto-restored; a manual OBS re-point may be needed." >&2
  fi
  _cold_cut_set prev_ndi "$prev_ndi"
  _cold_cut_set idle_ts "$(date +%s)"
  _cold_cut_set phase idled
  return 0
}
