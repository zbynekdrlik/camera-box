#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-deadman.sh, camera-box-
# restart-verify.sh, tmp-burn-sweep.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so imposing strict mode here would leak
# into whichever caller sources it. scripts/recording-e2e.sh (the only caller today) already sets
# -euo pipefail itself.
#
# scripts/lib/camera-box-deadman.sh -- SINGLE SOURCE OF TRUTH for the ON-BOX dead-man restart of
# the PRODUCTION `camera-box.service` (#772). The direct analogue of cam2-painter-deadman.sh, but
# for the production NDI streaming service every box feeds the operator's multiview from -- NOT the
# cam2 fb0 painter (which its own #872/#1072 dead-man already covers).
#
# WHY (#772): recording-e2e.sh STOPS camera-box and launches a probe-featured capture BURN as a
# transient `systemd-run --unit=camera-box-burn-<RUN_ID> --property=Restart=on-failure` unit at
# four sites (cam1 [2/8], the [2b/8] ALL_CAMBOX loop, cam2 non-sweep [3/8], and AV_RESTART), and
# restarts production ONLY in cleanup() (the bash EXIT trap). SIGKILL is uncatchable, so on a killed
# run the trap never runs -- camera-box stays STOPPED on every box the sweep touched and the burn
# unit (systemd-owned, NOT a runner child; no --duration-secs, so it runs FOREVER) keeps holding
# /dev/videoN. Result: the operator's multiview freezes BETWEEN runs, and the eventual manual /
# next-run `systemctl start camera-box` crash-loops on "Device or resource busy" until the stray
# burn unit is stopped. This is a ROUTINE event, not an edge case: full-path-e2e.yml's concurrency
# group is `cancel-in-progress: true`, so ANY push to `dev` cancels an in-flight hardware run and
# GitHub SIGKILLs the runner tree. Live re-occurrence 2026-08-03: a cancelled run left cam1's
# camera-box.service inactive mid its own cleanup window.
#
# The recovery deliberately lives ON THE CAMERA BOX (same rationale as cam2-painter-deadman): a
# dev1-side retry still runs on the machine being killed. A transient systemd timer armed by the
# same code that does the stopping has neither failure mode.
#
# HOW THIS DIFFERS FROM cam2-painter-deadman (and why): the painter's frame-probe is nohup'd with
# --duration-secs and SELF-TERMINATES, so its dead-man can fire every 5 min harmlessly guarded by
# `pgrep -x frame-probe`. The camera-box BURN has NO self-exit -- it runs forever -- so a
# process-presence guard would keep this dead-man permanently disarmed. Instead the FIRST fire is
# DELAYED past this run's entire window (--on-active, computed by the caller from the real run
# duration + a generous overhead margin), so the dead-man can NEVER fire during a live measurement
# (the safety-critical invariant: worst case is slower recovery, never a corrupted verdict); it then
# re-fires PERIODICALLY (--on-unit-active) so a run killed after the first fire is still recovered,
# and the action SELF-DISARMS once production is genuinely back. No cleanup() disarm wiring is needed
# (a normal run's cleanup restores camera-box; the delayed first fire then finds it active and
# self-disarms), and re-arming is idempotent (any prior transient unit is cleared first), so a
# leftover timer can never accumulate across runs.
#
# It NEVER touches frame-probe -- that is the cam2 fb0 painter, owned entirely by the painter
# dead-man; killing it here would darken the operator's cam2 QR monitor on every camera-box start.
#
# Source-only: a pure string builder, no ssh, no side effects at source time -- mirrors every other
# _cmds builder in this codebase.

# Transient unit name shared by every arm/self-disarm. `systemd-run --unit=NAME ...` creates
# NAME.timer plus NAME.service; both are cleared by the idempotent re-arm below and by the action's
# own self-disarm.
CAMERA_BOX_DEADMAN_UNIT="${CAMERA_BOX_DEADMAN_UNIT:-camera-box-deadman}"
# The PERIODIC re-fire window (minutes) once the delayed first fire has landed -- short, so a run
# killed after the first fire recovers within ~this window. Mid-run safety comes from the DELAYED
# first fire (the arg to camera_box_deadman_arm_cmds), not from this window.
CAMERA_BOX_DEADMAN_REFIRE_MIN="${CAMERA_BOX_DEADMAN_REFIRE_MIN:-5}"

# camera_box_deadman_arm_cmds FIRST_FIRE_MIN -> REMOTE bash (embed via
# `$(camera_box_deadman_arm_cmds "$MIN")` IMMEDIATELY BEFORE a `systemctl stop camera-box` in the
# same remote ssh command string). FIRST_FIRE_MIN is the minutes-from-now of the FIRST fire and MUST
# comfortably exceed THIS run's entire wall-clock window (the caller computes it from the real run
# duration + overhead) so the dead-man can never fire during the measurement. A box without
# camera-box.service installed is a guarded no-op. Re-arming is idempotent: any previous transient
# unit is cleared first.
#
# NOTE the trailing `;` on the last statement -- `$(...)` strips trailing newlines, so without it
# whatever the caller concatenates next is swallowed as extra argv (the documented CLAUDE.md
# #744/#746 gotcha). The action is single-quoted at the systemd-run call so its own `$u`/`&&`/`||`
# are stored literally in the unit and evaluated only at fire time; every `\$` / `\$(...)` below is
# backslash-escaped so THIS heredoc (unquoted, to expand the ${...} params) does not expand them on
# dev1 -- only the ${CAMERA_BOX_DEADMAN_*} params and $first are meant to expand here.
camera_box_deadman_arm_cmds() {
  local first="${1:-45}"
  cat <<ARM
if systemctl list-unit-files camera-box.service >/dev/null 2>&1; then
  systemctl stop ${CAMERA_BOX_DEADMAN_UNIT}.timer 2>/dev/null || true
  systemctl reset-failed ${CAMERA_BOX_DEADMAN_UNIT}.service 2>/dev/null || true
  systemd-run --quiet --on-active=${first}min --on-unit-active=${CAMERA_BOX_DEADMAN_REFIRE_MIN}min --unit=${CAMERA_BOX_DEADMAN_UNIT} \\
    /bin/bash -c 'systemctl list-units --all --plain --no-legend "camera-box-burn-*" 2>/dev/null | while read -r u _; do [ -n "\$u" ] && { systemctl stop "\$u" 2>/dev/null; systemctl reset-failed "\$u" 2>/dev/null; }; done; pkill -9 -x camera-box-burn 2>/dev/null; systemctl is-active --quiet camera-box || systemctl start camera-box; systemctl is-active --quiet camera-box && { systemctl stop ${CAMERA_BOX_DEADMAN_UNIT}.timer 2>/dev/null; systemctl reset-failed ${CAMERA_BOX_DEADMAN_UNIT}.service 2>/dev/null; }; true' 2>/dev/null \\
    && echo "[#772] camera-box dead-man armed (first fire +${first}min, then periodic every ${CAMERA_BOX_DEADMAN_REFIRE_MIN}min) -- a killed run restores production camera-box on this box without dev1" \\
    || echo "WARNING #772: could not arm the camera-box dead-man -- a killed run WILL leave camera-box stopped (operator MV frozen) until the next run" >&2
fi;
ARM
}
