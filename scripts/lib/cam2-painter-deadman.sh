#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines two pure functions, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, camera-box-
# restart-verify.sh, rig-test-dropin.sh, audio-marker-check.sh) of deliberately NOT setting
# `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so imposing
# strict mode here would leak into whichever caller sources it. scripts/recording-e2e.sh (the
# only caller today) already sets -euo pipefail itself.
#
# scripts/lib/cam2-painter-deadman.sh -- SINGLE SOURCE OF TRUTH for the ON-BOX dead-man restart
# of the PERMANENT cam2-painter.service (#872).
#
# WHY (#872): recording-e2e.sh stops that unit at THREE sites (`_cam2_prep` on both arms of the
# ALL_CAMBOX branch, and the `[3/8]` fb0-free step) and restarts it at exactly ONE -- cleanup(),
# the bash EXIT trap. SIGKILL is uncatchable, so on a killed run the trap never runs and cam2's
# interkom return monitor stays dark indefinitely. That is a ROUTINE event here, not an edge
# case: full-path-e2e.yml's concurrency group is `cancel-in-progress: true`, so any push to `dev`
# cancels an in-flight hardware run, and GitHub SIGKILLs the runner's process tree. Live evidence
# 2026-07-29: stopped 21:31:56, still `inactive`/`enabled` at 01:03 -- 3.5 hours dark, across
# three subsequent runs, noticed only by accident.
#
# The recovery deliberately lives ON THE CAMERA BOX rather than on dev1: a dev1-side retry (a
# longer trap, a second restore path, a polling watchdog) still runs on the machine that is being
# killed, and .claude/rules/rig-standing-services.md documents this rig's recurring pattern of
# standing services silently going quiet. A transient systemd timer armed by the same code that
# does the stopping has neither failure mode.
#
# This is NOT a preventive reboot and NOT a symptom-hider: it restores a service THIS harness
# stopped seconds earlier, which the harness is contractually responsible for returning. The root
# cause is the trap-only restore; this is its fix.
#
# Source-only: pure string builders, no ssh, no side effects at source time -- mirrors every
# other _cmds builder in this codebase.

# Transient unit name shared by the arm/disarm pair. `systemd-run --unit=NAME --on-active=...`
# creates NAME.timer plus NAME.service; both are cleaned up by the disarm below.
CAM2_PAINTER_DEADMAN_UNIT="${CAM2_PAINTER_DEADMAN_UNIT:-cam2-painter-deadman}"
# The self-heal window, as a PERIODIC re-fire (#1072, superseding the #872 one-shot 90-min delay).
# #872 chose a 90-min ONE-SHOT `--on-active` timer precisely so it could not fire MID-RUN (a live
# run holds the painter stopped for 25-35 min): with a one-shot, a long delay was the ONLY thing
# keeping it clear of the run. But a one-shot fires exactly once -- so a run SIGKILLed after it
# already fired (or before, then the run outlives... ) leaves the standing painter dark for up to
# the full window. #1072's requirement: a standing TEST painter must never be dark longer than
# ~5 min on ANY exit path. The fix is a PERIODIC re-fire on a SHORT window (below + --on-unit-active
# in the arm): mid-run safety comes from the action's own RUN-AWARE guards, not a long delay --
# every fire during a live run is a no-op. Once a killed run's device is free, the next fire within
# ~5 min brings the painter back; thereafter the guard sees the painter's OWN frame-probe and
# no-ops, and the unit's Restart=always keeps it up.
#
# #1246 -- the run-aware guard must cover BOTH on-box owners of the measured devices, not just fb0:
# the ORIGINAL guard checked ONLY `pgrep -x frame-probe` (the fb0 painter). Its premise -- "a live
# run always has a frame-probe" -- has a GAP: in the ALL_CAMBOX [2b/8] path the harness stops the
# deployed cam2-painter (its frame-probe exits) and starts the `camera-box-burn-cam2-<id>` capture
# burn MANY seconds/minutes before it launches its own [3/8] /tmp/frame-probe painter. A periodic
# fire in that window saw no frame-probe, passed the guard, and ran `systemctl start cam2-painter`
# -- whose `Wants=camera-box.service` pulls production camera-box, whose ExecStartPre
# camera-box-free-capture-device.sh stops every camera-box-burn-* unit -- KILLING the live burn
# mid-measurement (live 2026-08-31 run 1635844760: burn Started 19:01:02.787, deadman fired
# 19:01:49.544 in the no-frame-probe gap, burn Stopped 19:01:49.624; the [7b/8] integrity check
# correctly failed an otherwise-green verdict). The action now ALSO no-ops when a live capture burn
# owns /dev/video -- `pgrep -x camera-box-burn` (the exact 15-char comm free-capture-device.sh
# itself keys on) OR an active/activating camera-box-burn-* systemd unit (blip-robust against the
# burn's Restart=on-failure). The #281 rig heartbeat is written on DEV1 and is unreadable from this
# ON-BOX timer, so the burn presence is the on-box equivalent "a measurement claims the device".
# TRADE-OFF: on a SIGKILLed run the stray burn persists, so this deadman DEFERS cam2-painter
# recovery to the #772 camera-box-deadman (armed at the same sites) clearing the stray burn first --
# ~15 min later than the old frame-probe-self-exit path, but a corrupted live verdict is far worse,
# and cam2-painter cannot coexist with a burn anyway (both conflict on the device via camera-box).
# The systemd-run arm sets --description=cam2-painter-deadman AND the burn check uses a unit-NAME
# pattern arg ("camera-box-burn-*"), never a DESCRIPTION-column grep -- otherwise the check would
# self-match THIS deadman's own transient unit (systemd-run without --description sets the unit
# Description to the command line, which contains "camera-box-burn"), permanently disarming it
# (#1246 review, empirically reproduced -- the exact #772 pattern-argument idiom avoids it).
# RESIDUAL (#1246, fail-loud not silent): the two guards cover the burn->frame-probe gap (a burn
# owns the device) but NOT the brief stop->burn-start gap (deployed painter stopped, burn not yet
# started). Each stop site re-arms a fresh CAM2_PAINTER_DEADMAN_MINUTES delay, so that gap is
# normally clear of a fire; only a cam2 prep taking longer than the window between the painter stop
# and the burn start would let a fire seize /dev/video before the burn exists -- and that fails
# LOUD (the later burn start fails / the [7b/8] run-integrity check catches it), never a silently
# corrupted verdict.
CAM2_PAINTER_DEADMAN_MINUTES="${CAM2_PAINTER_DEADMAN_MINUTES:-5}"

# cam2_painter_deadman_arm_cmds -> REMOTE bash (embed via `$(cam2_painter_deadman_arm_cmds)`
# IMMEDIATELY BEFORE a `systemctl stop cam2-painter` in the same remote ssh command string).
# Arms a transient PERIODIC timer (#1072) that re-fires every CAM2_PAINTER_DEADMAN_MINUTES and
# `systemctl start cam2-painter` whenever the device is free -- i.e. no frame-probe (fb0) AND no
# live camera-box-burn (capture device, #1246) -- until cleanup() disarms it.
# A box without the unit installed is a guarded no-op (#440's existing guard convention), never
# a failure. Re-arming is idempotent: any previous transient unit is cleared first, so repeated
# runs never collide on the unit name.
#
# NOTE the trailing `;` on the last statement -- `$(...)` strips trailing newlines, so without it
# whatever the caller concatenates next is swallowed as extra argv (the documented CLAUDE.md
# gotcha that produced the #744/#746 `unknown arguments: rm` incident).
cam2_painter_deadman_arm_cmds() {
  cat <<ARM
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  systemctl stop ${CAM2_PAINTER_DEADMAN_UNIT}.timer 2>/dev/null || true
  systemctl reset-failed ${CAM2_PAINTER_DEADMAN_UNIT}.service 2>/dev/null || true
  systemd-run --quiet --description=cam2-painter-deadman --on-active=${CAM2_PAINTER_DEADMAN_MINUTES}min --on-unit-active=${CAM2_PAINTER_DEADMAN_MINUTES}min --unit=${CAM2_PAINTER_DEADMAN_UNIT} \\
    /bin/bash -c 'pgrep -x frame-probe >/dev/null && exit 0; pgrep -x camera-box-burn >/dev/null && exit 0; systemctl list-units --state=active,activating --plain --no-legend --type=service "camera-box-burn-*" 2>/dev/null | grep -q . && exit 0; systemctl start cam2-painter' 2>/dev/null \\
    && echo "[#872/#1072] cam2-painter dead-man armed (periodic, every ${CAM2_PAINTER_DEADMAN_MINUTES}min) -- a killed run self-heals on this box within ~${CAM2_PAINTER_DEADMAN_MINUTES}min" \\
    || echo "WARNING #872: could not arm the cam2-painter dead-man -- a killed run WILL leave the monitor dark" >&2
fi;
ARM
}

# cam2_painter_deadman_disarm_cmds -> REMOTE bash (embed via
# `$(cam2_painter_deadman_disarm_cmds)` in cleanup()'s remote command string, AFTER the existing
# `systemctl start cam2-painter`). Cancels the timer now that cleanup has done the restore
# itself, so it cannot later restart an already-running unit.
#
# Same trailing-`;` rule as above.
cam2_painter_deadman_disarm_cmds() {
  cat <<DISARM
if systemctl list-unit-files cam2-painter.service >/dev/null 2>&1; then
  systemctl stop ${CAM2_PAINTER_DEADMAN_UNIT}.timer 2>/dev/null || true
  systemctl reset-failed ${CAM2_PAINTER_DEADMAN_UNIT}.service 2>/dev/null || true
  echo "[#872] cam2-painter dead-man disarmed"
fi;
DISARM
}
