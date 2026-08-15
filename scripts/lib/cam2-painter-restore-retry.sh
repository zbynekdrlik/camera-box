#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, cam2-painter-
# deadman.sh, camera-box-restart-verify.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak
# into whichever caller sources it. scripts/recording-e2e.sh (the only caller) already sets
# -euo pipefail itself.
#
# scripts/lib/cam2-painter-restore-retry.sh -- #1072: turn cleanup()'s ONE-SHOT, error-swallowed
# painter restore into a bounded RETRY that is FAIL-LOUD and exposes a success flag so the on-box
# dead-man is disarmed ONLY when the painter genuinely came back.
#
# WHY (#1072, live 2026-08-15): cleanup()'s restore was `systemctl start cam2-painter 2>/dev/null
# || true` -- a single fire-and-forget attempt whose failure was swallowed -- followed by an
# UNCONDITIONAL `cam2_painter_deadman_disarm_cmds`. So when that one start failed, the painter
# stayed dead AND the on-box self-heal net was torn down, leaving cam2's monitor dark until the
# next run's #872 arm (or a manual start). This killed at least three gate runs at the [0/8]
# preflight in one day (runs 31869844646, 31880968359, 31887117614); the supervisor started the
# service by hand before every rerun.
#
# The fix, kept minimal + composable with the existing #863 anchored lines (recording-e2e.sh keeps
# the exact `systemctl start cam2-painter 2>/dev/null || true` first attempt + the adjacent
# `$(cam2_painter_restore_verify_cmds)` -- 5+ tests pin them):
#   - this builder runs AFTER that first attempt + verify: it polls is-active, and if the painter
#     is NOT active it does up to CAM2_PAINTER_RESTORE_RETRIES more `reset-failed` + `start`
#     attempts (each with a short is-active poll), so the restore is a real RETRY, not one-shot;
#   - it is FAIL-LOUD: a final failure prints a WARNING to stderr naming the manual recovery;
#   - it sets the remote shell var `_cprr_ok` (1 on success, empty on failure) which cleanup() uses
#     to gate the dead-man disarm: on failure the dead-man is LEFT ARMED, so the #872 on-box timer
#     (now a periodic ~5-min re-fire, cam2-painter-deadman.sh) heals the painter within ~5 min.
#
# Bounded on purpose (the CLEANUP_SSH_TIMEOUT=30s budget): the common case is the painter already
# active from the first attempt -> the first is-active check passes and the loop exits with ~0
# added time. Only a genuinely dead painter spends the (bounded) retry budget, and that case is
# exactly the one the armed dead-man then covers.
#
# Source-only: pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase.

# How many EXTRA restart attempts (beyond cleanup()'s existing first `systemctl start`) before
# giving up loud + leaving the dead-man armed. Kept small so the whole cleanup ssh stays inside
# CLEANUP_SSH_TIMEOUT; the dead-man is the real guarantee, this is best-effort fast recovery.
CAM2_PAINTER_RESTORE_RETRIES="${CAM2_PAINTER_RESTORE_RETRIES:-3}"

# cam2_painter_restore_retry_cmds -> REMOTE bash (embed via `$(cam2_painter_restore_retry_cmds)`
# in cleanup()'s remote command string, AFTER the existing `systemctl start cam2-painter` +
# `$(cam2_painter_restore_verify_cmds)`). Sets the remote var `_cprr_ok`. A box without the unit
# installed is a guarded no-op that still sets `_cprr_ok=1` (nothing to restore -> disarm proceeds;
# the disarm builder is itself a guarded no-op on such a box).
#
# NOTE the trailing `;` on the last statement -- `$(...)` strips trailing newlines, so without it
# whatever the caller concatenates next is swallowed as extra argv (the documented CLAUDE.md
# #744/#746 gotcha).
cam2_painter_restore_retry_cmds() {
  cat <<RETRY
_cprr_ok=""
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  _cprr=0
  while [ \$_cprr -lt ${CAM2_PAINTER_RESTORE_RETRIES} ]; do
    if [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" = "active" ]; then _cprr_ok=1; break; fi
    systemctl reset-failed cam2-painter.service 2>/dev/null || true
    systemctl start cam2-painter.service 2>/dev/null || true
    _cprw=0
    while [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ] && [ \$_cprw -lt 3 ]; do sleep 1; _cprw=\$((_cprw+1)); done
    _cprr=\$((_cprr+1))
  done
  if [ "\$(systemctl is-active cam2-painter.service 2>/dev/null)" = "active" ]; then _cprr_ok=1; fi
  if [ -n "\$_cprr_ok" ]; then
    echo "[cleanup] cam2-painter.service active after restore (#1072 retry)"
  else
    echo "WARNING #1072: cam2-painter.service FAILED to come active after ${CAM2_PAINTER_RESTORE_RETRIES} restart retries in cleanup -- leaving the on-box dead-man ARMED so it self-heals within ~5 min; verify manually (systemctl status cam2-painter)" >&2
  fi
else
  _cprr_ok=1
fi;
RETRY
}
