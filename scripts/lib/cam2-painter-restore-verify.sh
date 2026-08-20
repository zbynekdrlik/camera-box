#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (camera-box-restart-verify.sh, rig-test-
# dropin.sh, audio-marker-check.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak
# into whichever caller sources it. scripts/recording-e2e.sh (the only caller today) already
# sets -euo pipefail itself.
#
# scripts/lib/cam2-painter-restore-verify.sh -- SINGLE SOURCE OF TRUTH for verifying, WARN-ONLY
# (NEVER `exit`), that the PERMANENT cam2-painter.service genuinely came back up and is painting
# after recording-e2e.sh's cleanup() restores it (#863).
#
# WHY (#863): cleanup() already calls `systemctl start cam2-painter` (guarded, `2>/dev/null ||
# true`) but never verified it actually took -- a fire-and-forget call. The #863 ticket's own
# live evidence on cam2 showed exactly this class of failure going unnoticed: the service was
# never even installed for months, and this call was silently a no-op the whole time. Per the
# ticket's ask #4 ("after a measurement ends -- including a cancelled run -- the monitor must
# NOT be left black; a REAL return, verified"), this check closes that gap.
#
# This mirrors scripts/lib/camera-box-restart-verify.sh's SHAPE (poll is-active, WARN loud to
# stderr, NEVER exit/abort -- cleanup() is the bash EXIT trap and must always run to completion,
# see .claude/rules/recording-e2e-cleanup-composition.md + #328/#649) but ALSO checks the painter
# is genuinely PAINTING, not just process-alive -- the SAME presenter-aware distinction
# scripts/lib/presenter-liveness-check.sh's painter_liveness_check_cmds already established for
# the TEST-mode launch (#464): the default `--presenter auto` usually lands on the KMS page-flip
# presenter on cam2's i915, which BY DESIGN never opens /dev/fb0 (only the fbdev fallback does) --
# a bare `fuser -s /dev/fb0` would false-FAIL a healthy KMS run. Unlike painter_liveness_check_
# cmds (used at TEST-mode launch, where a hard `exit 1` correctly aborts the switch), this check
# reads the SERVICE's journal (systemd captures cam2-painter.service's stdout/stderr there, not a
# log file) and NEVER exits: a cleanup()-time restore failure is a loud WARNING for the operator
# to act on, not a reason to flip the harness's own PASS/FAIL exit code (which reflects the E2E
# measurement, not this best-effort restore).
#
# Source-only: pure string builder, no ssh, no side effects at source time -- mirrors every
# other _cmds builder in this codebase.

# cam2_painter_restore_verify_cmds -> REMOTE bash (embed via
# `$(cam2_painter_restore_verify_cmds)` right after an existing `systemctl start cam2-painter`
# call in the same remote ssh command string): if cam2-painter.service is installed, poll
# is-active (~8s), then check the presenter-appropriate painting signal from its journal (~8s),
# WARNING loud to stderr on either failure -- never exit. A box without the unit installed is a
# guarded no-op (#440's existing guard convention), never a failure.

# #1148: the painting SIGNAL itself (KMS-line parse + fuser + vblank-locked + /dev/fb0 fallback) is
# now the SINGLE-SOURCE-OF-TRUTH `_cb_paint_signal` in scripts/lib/cam2-paint-signal.sh (this used
# to be copy-pasted into 5 builders). Lazy-source it (the bundle-state-selfheal.sh idiom); the
# builder emits its definition, then this WARN-only 8x poll pipes the journal into it.
command -v cam2_paint_signal_remote_fn >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/cam2-paint-signal.sh"

cam2_painter_restore_verify_cmds() {
  cam2_paint_signal_remote_fn
  cat <<'VERIFY'
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  _cpv=0
  while [ "$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ] && [ $_cpv -lt 8 ]; do
    sleep 1; _cpv=$((_cpv+1))
  done
  if [ "$(systemctl is-active cam2-painter.service 2>/dev/null)" != "active" ]; then
    echo "WARNING #863: cam2-painter.service FAILED to come back active after cleanup -- cam2 monitor may be left black, verify manually (systemctl status cam2-painter)" >&2
  else
    _cpp=0
    _cpok=""
    while [ $_cpp -lt 8 ]; do
      _cpj="$(journalctl -u cam2-painter.service -n 100 --no-pager 2>/dev/null || true)"
      if printf '%s\n' "$_cpj" | _cb_paint_signal >/dev/null 2>&1; then
        _cpok=1; break
      fi
      sleep 1; _cpp=$((_cpp+1))
    done
    if [ -n "$_cpok" ]; then
      echo "[cleanup] cam2-painter.service active + genuinely painting (#863)"
    else
      echo "WARNING #863: cam2-painter.service active but no painting signal found (neither KMS DRM device held+vblank-locked nor /dev/fb0 held) -- cam2 monitor may be left black, verify manually (journalctl -u cam2-painter.service)" >&2
    fi
  fi
else
  echo "[#863] cam2-painter.service not installed on this box -- nothing to verify"
fi
VERIFY
}
