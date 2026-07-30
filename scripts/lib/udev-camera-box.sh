#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time) -- mirrors
# scripts/lib/startup-self-heal.sh / v4l2-neutral.sh convention of deliberately NOT setting
# `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so imposing
# strict mode here would leak into whichever caller sources it (setup-device.sh, create-usb-linux.sh's
# chroot, verify-device.sh, recording-e2e.sh all already set their own).
#
# scripts/lib/udev-camera-box.sh -- SINGLE SOURCE OF TRUTH for the #894 fix: the udev rule that
# restarts production camera-box.service on a video4linux hot-plug must never fight an in-flight
# E2E burn measurement, and USB autosuspend must stay OFF across a re-enumeration, not just at boot.
#
# WHY (#894): the fleet's `/etc/udev/rules.d/99-camera-box.rules` (traced to the retired
# scripts/setup.sh, #563 -- the rule never made it into setup-device.sh/create-usb-linux.sh during
# that migration, which is why only cam1 still carries it live) was UNCONDITIONAL:
#   ACTION=="add", SUBSYSTEM=="video4linux", RUN+="/bin/systemctl restart camera-box.service"
# recording-e2e.sh deliberately stops camera-box.service and runs its own probe-featured
# camera-box-burn-<RUN_ID>.service during a run. Any video4linux "add" event (a benign USB
# re-enumeration) restarts PRODUCTION, which steals /dev/videoN back from the burn unit --
# 77/NOPERM, then a restart-loop into 1/FAILURE. recording-verdict.rs then reports this as
# `frozen_leg` on the camera (it looks identical to a genuinely stuck capture from the outside),
# which cost a session two full gate runs chasing the wrong hypothesis. Live evidence, gate run
# 30554124753: cam1's grabber re-enumerated 3s into the CAM1 segment window; camera-box.service
# restarted; the burn unit's next few open() attempts got NOPERM.
#
# Second defect found while root-causing the first (same file, same mechanism): USB autosuspend
# is disabled ONLY by a one-shot /etc/rc.local loop at boot. A device that re-enumerates later
# comes back at the kernel default `auto`, and nothing re-applies `on` -- measured live across the
# fleet (#894 comment): the box still at `on` (cam3) had ZERO re-enumerations that day; the two
# that had drifted to `auto` (cam1, cam2) had 5 and 1, an amplifying feedback loop
# (autosuspend_delay_ms=2000 is enough idle time to suspend a live capture device during the exact
# window between camera-box.service stopping and the burn unit opening the node).
#
# THE FIX: ONE video4linux "add" rule, scoped by construction to the grabber that actually fired
# the event (never a blanket SUBSYSTEM=="usb" match, which would also catch every keyboard/hub/
# mouse) -- RUN+= a small helper script that:
#   1. walks up from the firing device's OWN sysfs path ($DEVPATH, set by udev) to its USB
#      ancestor (the nearest parent carrying idVendor -- the real usb_device node, not an
#      interface sub-node) and re-asserts power/control=on there;
#   2. restarts production camera-box.service UNLESS a camera-box-burn-*.service unit is
#      currently active (an E2E run owns the device -- restarting production here is exactly the
#      steal this ticket fixes).
#
# Testing without a rig (Tier 0, default cargo features): tests/harness_udev_camera_box_894.rs
# sources this file for the PURE decision/parser functions below, and separately re-execs the
# GENERATED helper-script content under a nested, PATH-restricted bash with fake `systemctl`/
# sysfs stand-ins (the imag-ssh-remote-tool-preflight.md "fake the remote, not the ssh" pattern)
# to prove the generated script's actual behavior, not just that it contains the right substrings.

# ── (A) the udev rules file content -- written by setup-device.sh + create-usb-linux.sh's chroot,
# and read back (content-checked) by verify-device.sh's new (w) acceptance check. ────────────────
udev_camera_box_rules_content() {
  cat <<'EOF'
# camera-box udev rules (#894): on every video4linux "add" (a capture grabber (re-)enumerating),
# re-assert USB autosuspend=off on its own USB ancestor, then restart production
# camera-box.service -- but ONLY when no camera-box-burn-*.service (an in-flight E2E measurement
# run's own unit) currently owns the device. See /usr/local/bin/camera-box-udev-video-add.sh.
ACTION=="add", SUBSYSTEM=="video4linux", RUN+="/usr/local/bin/camera-box-udev-video-add.sh"
EOF
}

# ── (B) the RUN+= helper script content -- written to /usr/local/bin/camera-box-udev-video-add.sh,
# chmod +x, by the same two provisioning paths. ───────────────────────────────────────────────────
udev_camera_box_helper_script_content() {
  cat <<'EOF'
#!/bin/bash
# camera-box: udev RUN+= handler for ACTION=="add", SUBSYSTEM=="video4linux" (#894).
#
# Two jobs, both scoped to the SPECIFIC grabber that just (re-)enumerated -- never a blanket
# SUBSYSTEM=="usb" match, which would also fire for every keyboard/hub/mouse on the box:
#  1. Re-assert USB autosuspend=off on the grabber's own USB ancestor. /etc/rc.local only applies
#     this once at boot; a later re-enumeration silently reverts to the kernel default `auto`, and
#     autosuspend_delay_ms=2000 is enough idle time to suspend a LIVE capture device -- measured
#     fleet-wide (#894): the box still at `on` had zero re-enumerations; the two that had drifted
#     to `auto` had 5 and 1, an amplifying feedback loop.
#  2. Restart production camera-box.service -- but ONLY when no camera-box-burn-*.service (the
#     E2E harness's own probe-featured measurement unit, scripts/recording-e2e.sh) is currently
#     active. That harness deliberately stops production and holds the device itself during a
#     run; restarting production here steals the device back (77/NOPERM in the burn unit, #894).
set -u

# _CBX_SYS_ROOT / _CBX_SYSTEMCTL: overridable ONLY so tests/harness_udev_camera_box_894.rs can
# point these at a throwaway fake sysfs tree + a stub systemctl instead of the real /sys (never
# writable/fakeable by a normal test process) and the real systemd -- production udev invocations
# never set either, so real behavior is byte-for-byte the same as the hardcoded defaults (an
# ABSOLUTE systemctl path, matching the fleet's original rule -- a udev RUN+= worker's PATH is
# minimal/unreliable, so this intentionally never resolves systemctl via PATH in production).
_CBX_SYS_ROOT="${_CBX_SYS_ROOT:-/sys}"
_CBX_SYSTEMCTL="${_CBX_SYSTEMCTL:-/bin/systemctl}"

# (1) walk up from THIS video4linux device's own sysfs path ($DEVPATH, set by udev) to its USB
# ancestor (the first parent directory carrying idVendor -- the real usb_device node, not an
# interface sub-node) and set power/control=on there.
if [ -n "${DEVPATH:-}" ]; then
  _d="${_CBX_SYS_ROOT}${DEVPATH}"
  while [ -n "$_d" ] && [ "$_d" != "$_CBX_SYS_ROOT" ] && [ "$_d" != "/" ]; do
    if [ -f "$_d/idVendor" ] && [ -f "$_d/power/control" ]; then
      echo on > "$_d/power/control" 2>/dev/null || true
      break
    fi
    _d="$(dirname "$_d")"
  done
fi

# (2) never fight an in-flight E2E burn measurement.
if "$_CBX_SYSTEMCTL" list-units --type=service --state=active --no-legend --plain 'camera-box-burn-*' 2>/dev/null | grep -q .; then
  exit 0
fi
exec "$_CBX_SYSTEMCTL" restart camera-box.service
EOF
}

# ── (C) verify-device.sh's (w) content check -- 0 iff RULE_TEXT wires the video4linux add event
# to our guarded helper script rather than the OLD bare unconditional restart. ───────────────────
udev_camera_box_rule_is_burn_gated() {
  local rule_text="${1:-}"
  case "$rule_text" in
    *'SUBSYSTEM=="video4linux"'*'camera-box-udev-video-add.sh'*) return 0 ;;
    *) return 1 ;;
  esac
}

# udev_camera_box_helper_has_burn_guard SCRIPT_TEXT -> 0 iff the helper script content itself
# actually checks for an active camera-box-burn-*.service before restarting production (guards
# against a rules file that points at the right path while the script's own body regressed back
# to an unconditional restart).
udev_camera_box_helper_has_burn_guard() {
  local script_text="${1:-}"
  case "$script_text" in
    *'camera-box-burn-'*'restart camera-box.service'*) return 0 ;;
    *) return 1 ;;
  esac
}

# ── (D) live power/control drift read -- REMOTE bash TEXT (embed via `$(...)`), assumes
# $V4L2_NEUTRAL_NODE is ALREADY resolved (v4l2_neutral_resolve_node_cmd, scripts/lib/v4l2-neutral.sh,
# embedded immediately before this in the same ssh command string). Walks the SAME sysfs ancestry
# as the helper script above, but starting from the resolved /dev/videoN's own `device` symlink
# instead of udev's $DEVPATH (verify-device.sh has no udev event to read DEVPATH from). Prints
# exactly one line so the caller can grep it out of the ssh output; empty value means "no USB
# ancestor with idVendor found" (e.g. cam4, which has no grabber fitted at all, #828).
#
# CRITICAL: ends with an explicit `;` on its last statement -- the same command-substitution
# trailing-newline-strip trap v4l2-neutral.sh's own comment documents (#744/#746) -- so this can be
# safely embedded ahead of more literal text at the call site.
udev_camera_box_grabber_power_control_read_cmd() {
  printf '%s\n' \
    '_cbx_d="$(readlink -f "/sys/class/video4linux/$(basename "$V4L2_NEUTRAL_NODE")/device" 2>/dev/null)"' \
    'CAMERA_BOX_GRABBER_POWER_CONTROL=""' \
    'while [ -n "$_cbx_d" ] && [ "$_cbx_d" != "/sys" ] && [ "$_cbx_d" != "/" ]; do' \
    '  if [ -f "$_cbx_d/idVendor" ] && [ -f "$_cbx_d/power/control" ]; then' \
    '    CAMERA_BOX_GRABBER_POWER_CONTROL="$(cat "$_cbx_d/power/control" 2>/dev/null)"' \
    '    break' \
    '  fi' \
    '  _cbx_d="$(dirname "$_cbx_d")"' \
    'done' \
    'echo "CAMERA_BOX_GRABBER_POWER_CONTROL=$CAMERA_BOX_GRABBER_POWER_CONTROL";'
}

# udev_camera_box_grabber_power_control_from_output SSH_OUTPUT_TEXT -> the value captured by the
# cmd above ("" | "on" | "auto" | ...), pure parse (no I/O). "" means either no USB ancestor was
# found (no grabber fitted) or the ssh call itself failed to produce the line at all -- the caller
# distinguishes those two by checking whether ANY video4linux device exists first.
udev_camera_box_grabber_power_control_from_output() {
  printf '%s\n' "$1" | sed -n 's/^CAMERA_BOX_GRABBER_POWER_CONTROL=//p' | tail -n1
}

# udev_camera_box_power_control_is_on VALUE -> 0 iff VALUE is exactly "on" (trimmed).
udev_camera_box_power_control_is_on() {
  [ "$(printf '%s' "${1:-}" | tr -d '[:space:]')" = "on" ]
}

# ── (E) burn-unit health -- did a deployed camera-box-burn-*.service actually stay ACTIVE through
# the recording window, or did it die (e.g. NOPERM from exactly this ticket's device-steal race)?
# recording-e2e.sh calls this right after StopRecord ([7/8]), BEFORE the merge/verdict computes
# its own (unrelated) frozen_leg verdict -- so a dead burn unit is surfaced as its OWN loud,
# distinctly-labeled run-integrity failure instead of being silently indistinguishable from a
# genuinely frozen camera in the log. ──────────────────────────────────────────────────────────

# udev_camera_box_burn_unit_state_cmd UNIT -> REMOTE bash TEXT (embed via `$(...)`, standalone --
# does not depend on any preceding embedded snippet) that prints "BURN_UNIT_STATE=<state>" for the
# named systemd unit (`systemctl is-active` -- active | failed | inactive | activating | ...).
# Ends with an explicit `;` for the same reason as the functions above.
udev_camera_box_burn_unit_state_cmd() {
  local unit="${1:?udev_camera_box_burn_unit_state_cmd: unit required}"
  printf 'BURN_UNIT_STATE=$(systemctl is-active %q 2>/dev/null); echo "BURN_UNIT_STATE=$BURN_UNIT_STATE";\n' "$unit"
}

# udev_camera_box_burn_unit_state_from_output SSH_OUTPUT_TEXT -> the captured state, pure parse.
udev_camera_box_burn_unit_state_from_output() {
  printf '%s\n' "$1" | sed -n 's/^BURN_UNIT_STATE=//p' | tail -n1
}

# udev_camera_box_burn_unit_is_healthy STATE -> 0 iff STATE is exactly "active". Anything else
# (failed, inactive, activating, an empty/unreadable ssh result) is UNHEALTHY -- fail-closed, same
# discipline as every other "unreachable = FAIL, never a silent pass" check in this repo.
udev_camera_box_burn_unit_is_healthy() {
  [ "${1:-}" = "active" ]
}

# udev_camera_box_burn_unit_integrity_message CAMBOX UNIT STATE -> the loud, distinctly-labeled
# run-integrity line printed to stderr when a burn unit is unhealthy (pure formatting, no I/O).
# "RUN-INTEGRITY FAILURE" is the intentionally grep-able prefix -- a human or CI log reader must
# never mistake this for a genuinely frozen camera.
udev_camera_box_burn_unit_integrity_message() {
  local cambox="${1:-?}" unit="${2:-?}" state="${3:-<unreadable>}"
  printf 'RUN-INTEGRITY FAILURE: %s burn unit %s is NOT ACTIVE (state=%s) -- the device was likely stolen by a udev-triggered production restart (#894), NOT a frozen camera. Any accompanying frozen_leg on %s is this artifact, not a real freeze.\n' \
    "$cambox" "$unit" "$state" "$cambox"
}
