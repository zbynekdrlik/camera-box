#!/usr/bin/env bash
# airuleset:script-ok source-only lib (content generators + pure parsers, no top-level statements)
# -- mirrors scripts/lib/udev-camera-box.sh, sourced by BOTH setup-device.sh (writes the files) and
# verify-device.sh (checks them). Deliberately NOT setting `set -euo pipefail` here: sourcing this
# file executes it in the CALLER's shell, so imposing strict mode would leak into whichever caller
# sources it.
#
# scripts/lib/camera-box-free-device.sh -- the PROVISIONING bake-in half of #772: a small helper
# script + a camera-box.service drop-in whose ExecStartPre frees /dev/video before camera-box
# (re)starts, from ANY trigger -- the on-box dead-man (camera-box-deadman.sh), cleanup(), the
# next-run [0/8] preflight, OR a MANUAL operator `systemctl restart camera-box` to unfreeze the MV.
#
# WHY: a cancel-in-progress SIGKILL of an E2E run leaves a stray `camera-box-burn-*.service`
# (systemd-owned, Restart=on-failure) holding /dev/video; the next `systemctl start camera-box`
# then crash-loops on "Device or resource busy" (os error 16). The dead-man's own action already
# stops the stray burn before it starts camera-box, but that only covers the dead-man's OWN start.
# Baking the same device-free into camera-box.service's ExecStartPre makes EVERY start path
# self-heal on the box, independent of dev1 and independent of whether this box has been
# re-provisioned with the dead-man yet.
#
# It STOPS the stray burn UNIT (a bare `pkill` just trips its Restart=on-failure respawn), then
# kills any stray burn process. It NEVER touches frame-probe -- that is the cam2 fb0 painter (a
# DIFFERENT device, owned by cam2-painter.service / its own dead-man); killing it here would darken
# the operator's cam2 QR monitor on every camera-box start.
#
# DELIBERATE TRADEOFF (#772): this makes a MID-RUN manual `systemctl start camera-box` (operator
# error -- the only remaining mid-run start path after #894's burn-gated udev rule) KILL the
# in-flight E2E burn and hand the device to production, where before it crash-looped harmlessly
# while the burn kept /dev/video. It is fail-LOUD downstream, never a false PASS: the verdict fails
# on the now-missing burns and the post-StopRecord run-integrity assertion surfaces a burn that
# died mid-run -- a wasted run, not a corrupted one. Freeing the device on EVERY start (so an
# operator can recover a frozen MV between runs) is worth that opt-in-mode operator-error window.

# The absolute path both the helper script is installed to AND the drop-in's ExecStartPre points at.
CAMERA_BOX_FREE_DEVICE_HELPER_PATH="${CAMERA_BOX_FREE_DEVICE_HELPER_PATH:-/usr/local/bin/camera-box-free-capture-device.sh}"

# camera_box_free_capture_device_script_content -> the helper script body written to
# CAMERA_BOX_FREE_DEVICE_HELPER_PATH by setup-device.sh. `set -uo pipefail` (NOT -e): every failure
# is individually tolerated (a stray burn that is already gone, a box with none) and the script must
# always exit 0 so the `-` prefix on the ExecStartPre is never even needed.
camera_box_free_capture_device_script_content() {
  cat <<'HELPER'
#!/usr/bin/env bash
# Managed by setup-device.sh (scripts/lib/camera-box-free-device.sh) -- do not edit by hand (#772).
# Free /dev/video before camera-box starts: stop any stray E2E capture-burn UNIT (Restart=on-failure,
# so a bare pkill would just respawn it), then kill any stray burn process. Deliberately scoped to
# the capture burn ONLY -- it never touches the cam2 painter (a different device: /dev/fb0, not
# /dev/video), which is owned entirely by cam2-painter.service and its own dead-man.
set -uo pipefail
systemctl list-units --all --plain --no-legend "camera-box-burn-*" 2>/dev/null | while read -r u _; do
  [ -n "$u" ] || continue
  systemctl stop "$u" 2>/dev/null || true
  systemctl reset-failed "$u" 2>/dev/null || true
done
pkill -9 -x camera-box-burn 2>/dev/null || true
exit 0
HELPER
}

# camera_box_free_capture_device_dropin_content -> the systemd drop-in text written to
# /etc/systemd/system/camera-box.service.d/free-capture-device.conf. The `-` prefix makes a
# non-zero helper exit non-fatal to camera-box's own start.
camera_box_free_capture_device_dropin_content() {
  cat <<DROPIN
[Service]
# #772: free /dev/video (stop a killed E2E run's stray capture burn) before camera-box starts, so
# production never crash-loops on "Device or resource busy". See ${CAMERA_BOX_FREE_DEVICE_HELPER_PATH}.
ExecStartPre=-${CAMERA_BOX_FREE_DEVICE_HELPER_PATH}
DROPIN
}

# camera_box_free_device_dropin_wired TEXT -> exit 0 iff the drop-in TEXT wires an ExecStartPre to
# the helper path (pure, no I/O -- used by verify-device.sh (y) and its unit test).
camera_box_free_device_dropin_wired() {
  printf '%s' "${1:-}" | grep -Eq "^[[:space:]]*ExecStartPre=-?${CAMERA_BOX_FREE_DEVICE_HELPER_PATH}[[:space:]]*$"
}

# camera_box_free_device_script_is_burn_scoped TEXT -> exit 0 iff the helper TEXT frees the device
# the RIGHT way: it stops the stray burn UNIT (not just a pkill), pkills the stray burn PROCESS, AND
# never references frame-probe (which would kill the cam2 painter). Pure, no I/O.
camera_box_free_device_script_is_burn_scoped() {
  local text="${1:-}"
  grep -q 'systemctl stop' <<<"$text" || return 1
  grep -q 'camera-box-burn-\*' <<<"$text" || return 1
  grep -q 'pkill -9 -x camera-box-burn' <<<"$text" || return 1
  # MUST NOT touch the painter -- a frame-probe reference here is a defect, not a pass.
  if grep -q 'frame-probe' <<<"$text"; then return 1; fi
  return 0
}
