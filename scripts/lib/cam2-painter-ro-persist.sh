#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-handoff.sh, cam2-painter-deadman.sh,
# rig-test-dropin.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so imposing strict mode here would leak into whichever caller
# sources it. scripts/rig-mode.sh (the only caller) already sets it.
#
# scripts/lib/cam2-painter-ro-persist.sh -- SINGLE SOURCE OF TRUTH for changing cam2-painter.service's
# PERSISTENT enable-state on cam2's READ-ONLY root safely (#1175).
#
# WHY (#1175): cam2's root is read-only (the appliance hardening every deploy path already handles
# with `mount -o remount,rw /`). `systemctl enable`/`disable` writes/removes an enable symlink under
# /etc/systemd/system, which FAILS `Read-only file system` on that root. Both rig-mode.sh call sites
# used `systemctl <enable|disable> cam2-painter.service 2>/dev/null || true`, which SWALLOWED that
# failure:
#   - EVENT `disable` (cam2_painter_service_disable_cmds): the unit stayed `enabled`, so a bare reboot
#     re-armed the QR painter on the LIVE broadcast, while painter_stop_remote's PASS line still
#     claimed "stopped+disabled (no QR can return, including across a reboot)" (#892 hazard, live
#     2026-08-23 ~05:20).
#   - TEST `enable --now` (cam2_painter_steady_state_handoff_cmds): `--now` started it at runtime so
#     the active/painting checks passed, but the `enable` symlink never landed -> the unit was NOT
#     enabled and died at the next reboot, while the handoff claimed "enabled + survives reboot".
#
# The fix: open a remount-rw window (the canonical scripts/deploy-fleet.sh:111 /
# scripts/bkshading-deploy-relay.sh:137 pattern), run the change FAIL-LOUD (no `2>/dev/null || true`
# swallow), ALWAYS restore the root to read-only (even on failure), then VERIFY the persistent
# `is-enabled` state actually changed -- the enable-state read-back the PASS condition now depends on.
#
# Source-only: a pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase.

# cam2_painter_persist_state_cmds MODE -> REMOTE bash (embed via `$(cam2_painter_persist_state_cmds
# enable-now|disable)` inside a remote-command heredoc that runs under the caller's `set -e`). MODE:
#   enable-now -> `systemctl enable --now cam2-painter.service`, verify the unit ends up `enabled`.
#   disable    -> `systemctl disable cam2-painter.service`,      verify the unit is no longer `enabled`.
# FAIL LOUD (exit 1) on a remount failure, a non-zero systemctl, or a post-change state mismatch --
# the enclosing `cam_ssh` then returns non-zero, which is exactly the intended #1175 behaviour: a
# persistent state that was CLAIMED but did not actually land must never be reported as done.
cam2_painter_persist_state_cmds() {
  local mode="${1:-}" action want
  case "$mode" in
  enable-now)
    action="enable --now"
    want="enabled"
    ;;
  disable)
    action="disable"
    want="not-enabled"
    ;;
  *)
    printf 'echo "FAIL: [#1175] cam2_painter_persist_state_cmds: unknown mode %s (expected enable-now|disable)" >&2; exit 1\n' "${mode:-<empty>}"
    return 0
    ;;
  esac
  # First heredoc (expanded): the remount + the systemctl change; $action is a build-time value,
  # every RUNTIME var is \$-escaped so it survives into the emitted remote script.
  cat <<CMDS
# #1175: cam2's root is READ-ONLY; the '$action' of cam2-painter.service cannot write the
#        /etc/systemd/system enable symlink and FAILS 'Read-only file system'. Remount rw (the
#        canonical deploy-fleet.sh / bkshading-deploy-relay.sh pattern), change FAIL-LOUD, restore ro,
#        then VERIFY the persistent enable-state actually changed.
if ! mount -o remount,rw / 2>/dev/null; then
  echo "FAIL: [#1175] could not remount / read-write to persist 'systemctl $action cam2-painter.service' (cam2 has a read-only root)." >&2
  exit 1
fi
_pss_rc=0
systemctl $action cam2-painter.service || _pss_rc=\$?
mount -o remount,ro / 2>/dev/null || true
if [ "\$_pss_rc" -ne 0 ]; then
  echo "FAIL: [#1175] 'systemctl $action cam2-painter.service' failed (rc=\$_pss_rc) even inside the remount-rw window." >&2
  exit 1
fi
_pss_state="\$(systemctl is-enabled cam2-painter.service 2>/dev/null || true)"
CMDS
  # Second heredoc (literal): the read-back verify. $_pss_state is a RUNTIME var, kept literal.
  if [ "$want" = "enabled" ]; then
    cat <<'CMDS'
if [ "$_pss_state" != "enabled" ]; then
  echo "FAIL: [#1175] cam2-painter.service is-enabled='$_pss_state' after enable (expected 'enabled') -- it will NOT survive a reboot." >&2
  exit 1
fi
echo "[#1175] cam2-painter.service ENABLED + persisted (is-enabled=enabled; survives reboot) via a remount-rw window."
CMDS
  else
    cat <<'CMDS'
if [ "$_pss_state" = "enabled" ]; then
  echo "FAIL: [#1175] cam2-painter.service still is-enabled='enabled' after disable -- a reboot would re-arm the QR painter on the live broadcast." >&2
  exit 1
fi
echo "[#1175] cam2-painter.service DISABLED + persisted (is-enabled='$_pss_state'; a reboot cannot re-arm the QR) via a remount-rw window."
CMDS
  fi
}
