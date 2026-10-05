#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-handoff.sh, cam2-painter-deadman.sh,
# rig-test-dropin.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so imposing strict mode here would leak into whichever caller
# sources it. scripts/rig-mode.sh (the only caller) already sets it.
#
# scripts/lib/cam2-painter-ro-persist.sh -- SINGLE SOURCE OF TRUTH for changing cam2-painter.service's
# PERSISTENT enable-state on cam2's READ-ONLY root safely (#1175), and for leaving that root
# READ-ONLY again before the painter starts (issue 1405).
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
# WHY (issue 1405): the #1175 window ran `systemctl enable --now` INSIDE the rw window, then
# `mount -o remount,ro / 2>/dev/null || true`. The ro remount failed EBUSY, the `|| true` hid it,
# and nothing read the mount state afterwards: cam2 ran on a WRITABLE root until the next reboot
# (live 4.10.2026: the last `r/w` remount at 10:36:54 had no `ro` after it, and the painter became
# active the same second). That is exactly the stick-wear state the read-only appliance
# (setup-device STEP 18) exists to prevent. That the START opened the blocking writer is INFERRED
# from that timing, not proven: cam2-painter.service itself writes only /run, and the one writer
# seen live (a second systemd-journald) is not explained by this unit. Either way, a start on a
# writable root is what this emitter no longer does, and the root mode is now READ, not assumed.
#
# The fix: open a remount-rw window, run ONLY the enable-state change FAIL-LOUD (no
# `2>/dev/null || true` swallow, never `--now`), ALWAYS remount the root read-only (even on
# failure), then VERIFY with `findmnt -no OPTIONS /` (read through the shared ro-root canon's
# ro_root_mount_mode, scripts/lib/ro-root.sh) that the root reads `ro` -- the checked close
# scripts/bkshading-deploy-relay.sh's remount_ro_checked also does. If it does not, FAIL LOUD naming
# the holders (the processes with a file open for WRITING on / from `fuser -vm /`, plus the
# deleted-but-open files from the shared issue-808 holder probe) and never start the painter. Then
# verify the persistent `is-enabled` state actually changed, and only in enable-now mode start the
# painter -- on a root already proven read-only. No retry loop: a writer that keeps the root busy
# keeps it busy on every retry, so a retry only delays the same failure.
#
# Every emitted statement ends with `;` (the CLAUDE.md `$(...)` trailing-newline-strip gotcha): the
# callers embed this text via `$(...)`, so its last statement must still end cleanly whatever text
# the caller puts right after it.
#
# Source-only: a pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase.

# issue 1405: the root-mode reading is the ONE first-token reading of the shared ro-root canon.
# Lazy-source it (rig-mode.sh does not source ro-root.sh itself) -- the same lazy-source pattern the
# handoff lib uses for its own helpers. The function's definition is emitted INTO the remote text.
command -v ro_root_mount_mode >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/ro-root.sh"
# issue 1405: the holders of deleted-but-open files are named by the repo's ONE probe for that
# EBUSY cause (issue 808, bkshading_deploy_ro_holder_probe_cmd); its text is emitted into the
# failure branch. Lazy-sourced the same way (the lib is pure and side-effect free).
command -v bkshading_deploy_ro_holder_probe_cmd >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/bkshading-deploy-runtime.sh"

# cam2_painter_persist_state_cmds MODE -> REMOTE bash (embed via `$(cam2_painter_persist_state_cmds
# enable-now|disable)` inside a remote-command heredoc that runs under the caller's `set -e`). MODE:
#   enable-now -> `systemctl enable cam2-painter.service` inside the window, verify the root reads
#                 `ro` again, verify the unit ends up `enabled`, THEN start it (issue 1405).
#   disable    -> `systemctl disable cam2-painter.service` inside the window, verify the root reads
#                 `ro` again, verify the unit is no longer `enabled`. Never starts anything (#892).
# FAIL LOUD (exit 1) on a remount failure, a non-zero systemctl, a root left not read-only, a
# post-change state mismatch, or (enable-now) a failed start -- the enclosing `cam_ssh` then returns
# non-zero, which is exactly the intended behaviour: a persistent state that was CLAIMED but did not
# actually land, or a writable cambox root, must never be reported as done.
cam2_painter_persist_state_cmds() {
  local mode="${1:-}" action="" want="" rig_mode="" refusal=""
  case "$mode" in
  enable-now)
    action="enable"
    want="enabled"
    rig_mode="test"
    refusal="cam2-painter.service is NOT started"
    ;;
  disable)
    action="disable"
    want="not-enabled"
    rig_mode="event"
    refusal="the EVENT switch stops here"
    ;;
  *)
    printf 'echo "FAIL: [#1175] cam2_painter_persist_state_cmds: unknown mode %s (expected enable-now|disable)" >&2; exit 1;\n' "${mode:-<empty>}"
    return 0
    ;;
  esac
  # (1) the shared root-mode reader, defined on the box (a statement of its own, ';'-terminated).
  printf '%s;\n' "$(declare -f ro_root_mount_mode)"
  # (2) the window: remount rw, the change, the ro remount. $action is a build-time value; every
  #     RUNTIME var is \$-escaped so it survives into the emitted remote script.
  cat <<CMDS
# #1175: cam2's root is READ-ONLY; the '$action' of cam2-painter.service cannot write the
#        /etc/systemd/system enable symlink and FAILS 'Read-only file system'. Remount rw, change
#        FAIL-LOUD, restore ro.
# issue 1405: only the enable-state change runs inside the window (never a start), and the root
#        must READ ro afterwards (findmnt), else FAIL LOUD naming the holders -- never a cambox on a
#        writable root.
if ! mount -o remount,rw / 2>/dev/null; then
  echo "FAIL: [#1175] could not remount / read-write to persist 'systemctl $action cam2-painter.service' (cam2 has a read-only root)." >&2;
  exit 1;
fi;
_pss_rc=0;
systemctl $action cam2-painter.service || _pss_rc=\$?;
_pss_ro_rc=0;
_pss_ro_err="\$(mount -o remount,ro / 2>&1)" || _pss_ro_rc=\$?;
_pss_opts="\$(findmnt -no OPTIONS / 2>/dev/null || awk '\$2=="/"{print \$4; exit}' /proc/mounts 2>/dev/null || true)";
_pss_root="\$(ro_root_mount_mode "\$_pss_opts")";
if [ "\$_pss_root" != "ro" ]; then
  echo "FAIL: [#1405] cam2's root is NOT read-only after the remount-rw window ('findmnt -no OPTIONS /' = '\$_pss_opts' -> \$_pss_root; 'mount -o remount,ro /' rc=\$_pss_ro_rc\${_pss_ro_err:+: \$_pss_ro_err}). A cambox must never run on a writable root, so $refusal. cam2-painter.service is-enabled now: '\$(systemctl is-enabled cam2-painter.service 2>/dev/null || true)'." >&2;
  echo "FAIL: [#1405] processes with a file open for WRITING on / ('fuser -vm /', ACCESS F):" >&2;
CMDS
  # (2b) the holders, in the failure branch only. The writer filter keeps the header and every line
  #      whose ACCESS field (the one right after the PID) carries F: 'fuser -vm /' lists PID 1 and
  #      the kernel threads first, so a cut at N lines hides a writer with a high PID. No header
  #      line = fuser printed no listing (missing, failed): say so, never "none". Literal heredoc:
  #      every $ is awk's.
  cat <<'CMDS'
  { fuser -vm / 2>&1 || true; } | awk '/USER/ && /PID/ && /ACCESS/ { print; h = 1; next } { for (i = 1; i < NF; i++) if ($i ~ /^[0-9]+$/) { if ($(i + 1) ~ /F/) { print; n++ } break } } END { if (!h) print "  (fuser printed no listing -- is psmisc installed? check by hand: fuser -vm /)"; else if (n == 0) print "  (none: no process holds a file open for writing on /)" }' >&2 || true;
  echo "FAIL: [#1405] holders of deleted-but-open files on / (lsof +L1, else the /proc fd scan):" >&2;
  {
CMDS
  bkshading_deploy_ro_holder_probe_cmd
  cat <<CMDS
  } >&2 || true;
  echo "FAIL: [#1405] stop that holder on cam2, run 'mount -o remount,ro /' there until 'findmnt -no OPTIONS /' reads ro, then re-run rig-mode.sh $rig_mode (never reboot a cambox remotely)." >&2;
  exit 1;
fi;
if [ "\$_pss_rc" -ne 0 ]; then
  echo "FAIL: [#1175] 'systemctl $action cam2-painter.service' failed (rc=\$_pss_rc) even inside the remount-rw window (the root is back read-only)." >&2;
  exit 1;
fi;
_pss_state="\$(systemctl is-enabled cam2-painter.service 2>/dev/null || true)";
CMDS
  # (3) the read-back verify (literal: $_pss_state / $_pss_opts are RUNTIME vars) and, in
  #     enable-now mode only, the start on the proven read-only root.
  if [ "$want" = "enabled" ]; then
    cat <<'CMDS'
if [ "$_pss_state" != "enabled" ]; then
  echo "FAIL: [#1175] cam2-painter.service is-enabled='$_pss_state' after enable (expected 'enabled') -- it will NOT survive a reboot." >&2;
  exit 1;
fi;
echo "[#1175] cam2-painter.service ENABLED + persisted (is-enabled=enabled; survives reboot) via a remount-rw window; root back read-only (findmnt: $_pss_opts, issue 1405).";
if ! systemctl start cam2-painter.service; then
  echo "FAIL: [#1405] cam2-painter.service is enabled + persisted and cam2's root is read-only, but 'systemctl start cam2-painter.service' failed -- the TEST painter is NOT running." >&2;
  exit 1;
fi;
echo "[#1405] cam2-painter.service started on a root verified read-only.";
CMDS
  else
    cat <<'CMDS'
if [ "$_pss_state" = "enabled" ]; then
  echo "FAIL: [#1175] cam2-painter.service still is-enabled='enabled' after disable -- a reboot would re-arm the QR painter on the live broadcast." >&2;
  exit 1;
fi;
echo "[#1175] cam2-painter.service DISABLED + persisted (is-enabled='$_pss_state'; a reboot cannot re-arm the QR) via a remount-rw window; root back read-only (findmnt: $_pss_opts, issue 1405).";
CMDS
  fi
}
