#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions, no top-level statements) -- matches
# the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, cam2-painter-restore-
# retry.sh, cambox-parallel-restore.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak into
# whichever caller sources it. scripts/recording-e2e.sh (the only caller) already sets -euo pipefail.
#
# scripts/lib/cam2-painter-restore-recheck.sh -- #1126: ONE final, dedicated genuine-painting
# re-check of cam2-painter, run in cleanup() AFTER cambox_parallel_wait_and_report (+ its #715
# retry) has already recorded the failed set, and BEFORE cambox_parallel_surface_painter_failure
# emits its GitHub annotation.
#
# WHY (#1126, live run 1104689227, 2026-08-19): the cam2/painter restore runs a LOT of serial work
# inside ONE CLEANUP_SSH_TIMEOUT(=30s)-bounded ssh (stop burns + restart camera-box + is-active/
# painting verify + retry). On a slow-restart run that combined ssh hits the 30s wall and `timeout`
# SIGKILLs it a hair (~47ms) BEFORE cam2-painter.service actually reports active -- so the subshell
# exits non-zero, cam2/painter lands in CAMBOX_PARALLEL_FAILED_LABELS, and (since the #715 retry
# NEVER prunes a painter) a false ::error:: reds a run whose recording verdict was overall_pass=true.
# The restore genuinely SUCCEEDED; only the verify window lost the race by ~50ms.
#
# The fix is the owner-sanctioned "one final re-check after the poll deadline": a SEPARATE, short,
# bounded ssh (its OWN timeout -- it never extends the tight 30s parallel-restore budget, so the
# #712/#713 GH-Actions cancellation-grace guarantee is preserved) that re-checks the SAME presenter-
# aware genuine-painting signal cam2_painter_restore_verify_cmds uses (a KMS DRM page-flip presenter
# with its DRM device held + a 'vblank-locked' journal line, OR the fbdev fallback holding /dev/fb0
# -- NOT bare is-active, so it can NEVER mask a BLACK monitor, the #863/#860 discipline). Only if the
# painter is genuinely painting NOW does it PRUNE cam2/painter (and its lockstep FAILED_IPS entry)
# from the failed ledger, so cambox_parallel_surface_painter_failure no longer fires a false
# ::error:: and cambox_parallel_wait_and_report's non-zero return no longer reflects a phantom
# failure. A genuinely dead painter is LEFT in the set -> the #860 ::error:: fires legitimately.
#
# NOT a blind timeout bump: it does NOT widen CLEANUP_SSH_TIMEOUT (which would weaken cancellation
# grace for every box); it adds a short confirmation ONLY when a box actually failed, and only for
# the painter (the one box #715 can never prune).
#
# Source-only: pure functions, no ssh / no side effects at source time.

# cam2_painter_genuine_paint_check_cmd -> REMOTE bash: EXIT 0 iff cam2-painter.service is active AND
# genuinely painting (the SAME presenter-aware signal as cam2_painter_restore_verify_cmds), else
# EXIT 1. Short bounded poll (~a few seconds) -- the painter is EXPECTED to already be active by the
# time this runs (it went active a hair after the combined restore ssh's deadline), so this is a
# confirmation, not a restart. Single-quoted heredoc: every remote $/$(...) is literal (evaluated on
# the cam box).
#
# #1126 review 🟡-1: this exit code drives a PRUNE decision (the caller removes cam2/painter from the
# failed ledger + suppresses the #860 ::error:: on EXIT 0), which is STRONGER semantics than the
# WARN-only cam2_painter_restore_verify_cmds. So it may ONLY exit 0 on a POSITIVE paint signal --
# "unit not installed" (or a transient list-unit-files hiccup, which `if ! ...` also catches) EXITs
# 1, NOT 0: absence-of-painter is exactly the #863 black-monitor case a prune must never mask.
# #1148: the paint SIGNAL is now the shared `_cb_paint_signal` (scripts/lib/cam2-paint-signal.sh);
# lazy-source it and emit its definition before the poll wrapper below.
command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"

cam2_painter_genuine_paint_check_cmd() {
  cam2_paint_signal_remote_fn
  cat <<'PAINTCHK'
if ! systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then exit 1; fi
_pc=0
while [ "$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ] && [ $_pc -lt 6 ]; do sleep 1; _pc=$((_pc+1)); done
[ "$(systemctl is-active cam2-painter.service 2>/dev/null)" = "active" ] || exit 1
_pp=0
while [ $_pp -lt 6 ]; do
  _pj="$(journalctl -u cam2-painter.service -n 100 --no-pager 2>/dev/null || true)"
  if printf '%s\n' "$_pj" | _cb_paint_signal >/dev/null 2>&1; then exit 0; fi
  sleep 1; _pp=$((_pp+1))
done
exit 1
PAINTCHK
}

# cam2_painter_restore_final_recheck PAINTER_IP -> if cam2/painter is in CAMBOX_PARALLEL_FAILED_LABELS,
# run ONE dedicated bounded genuine-painting re-check ssh against PAINTER_IP; on confirmed painting,
# PRUNE cam2/painter from BOTH CAMBOX_PARALLEL_FAILED_LABELS and (in lockstep) CAMBOX_PARALLEL_FAILED_IPS,
# so the subsequent cambox_parallel_surface_painter_failure sees a truthful ledger. Guarded: a no-op
# unless CAM_PW is set (a no-credential / unit-test context never touches the rig) AND cam2/painter is
# actually in the failed set. Never `exit`s, always returns 0 (cleanup()'s trap must always run to
# completion -- the #649/#675/#712 warn-only discipline).
cam2_painter_restore_final_recheck() {
  local _pip="$1"
  [ -n "${CAM_PW:-}" ] || return 0
  local _idx _found=""
  for _idx in "${!CAMBOX_PARALLEL_FAILED_LABELS[@]}"; do
    # #1126 review 🔵-4: match the exact cam2/painter label (recorded as "cam2/painter, <ip>"),
    # not a broad *painter* — precise + future-proof if another box label ever carries "painter".
    case "${CAMBOX_PARALLEL_FAILED_LABELS[$_idx]}" in
      *cam2/painter*) _found="$_idx"; break ;;
    esac
  done
  [ -n "$_found" ] || return 0
  local _to="${CAM2_PAINTER_RECHECK_TIMEOUT:-25}"
  if timeout "$_to" sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$_pip" "$(cam2_painter_genuine_paint_check_cmd)"; then
    echo "    [cleanup] #1126: cam2/painter confirmed genuinely painting on a final re-check -- the parallel-restore verify window lost the race (restore succeeded ~50ms after the deadline); pruning it from the failed set (no false ::error::)"
    local _nl=() _ni=() _j
    for _j in "${!CAMBOX_PARALLEL_FAILED_LABELS[@]}"; do
      [ "$_j" = "$_found" ] && continue
      _nl+=("${CAMBOX_PARALLEL_FAILED_LABELS[$_j]}")
      _ni+=("${CAMBOX_PARALLEL_FAILED_IPS[$_j]:-}")
    done
    if [ "${#_nl[@]}" -gt 0 ]; then
      CAMBOX_PARALLEL_FAILED_LABELS=("${_nl[@]}")
      CAMBOX_PARALLEL_FAILED_IPS=("${_ni[@]}")
    else
      CAMBOX_PARALLEL_FAILED_LABELS=()
      CAMBOX_PARALLEL_FAILED_IPS=()
    fi
  else
    echo "    WARNING #1126: cam2/painter final re-check did NOT confirm genuine painting -- leaving it in the failed set; the #860 ::error:: fires legitimately" >&2
  fi
  return 0
}
