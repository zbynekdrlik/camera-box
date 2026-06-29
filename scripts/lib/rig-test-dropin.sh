#!/usr/bin/env bash
# scripts/lib/rig-test-dropin.sh — SINGLE SOURCE OF TRUTH for the #291 rig-test no-display systemd
# drop-in: the PATH constant + the "clear it on restore" logic. Sourced by:
#   - scripts/rig-mode.sh        (INSTALLS the drop-in in `test`, REMOVES it in `event`)
#   - scripts/recording-e2e.sh   (cam2 camera-box restore in cleanup())
#   - scripts/loopback-e2e.sh    (remote camera-box restore in cleanup())
#
# WHY (#309, a #246-class leak): `rig-mode.sh test` installs a TRANSIENT drop-in that overrides
# ExecStart to run camera-box WITHOUT --display (frees /dev/fb0 for the QR painter). If an operator
# then runs a sibling e2e harness STANDALONE, that harness's camera-box "restore" used to
# `restart`/`start` camera-box with NO knowledge of the drop-in — bringing it back in NO-DISPLAY mode
# (the interkom return monitor stays dark) while the operator believes broadcast is fully restored.
# Every camera-box restore must therefore CLEAR the drop-in first. Keeping the path AND the clear
# logic HERE means the three scripts can never desync — the path string lives in exactly ONE place.
#
# Source-only: this file defines a constant + a pure string builder and runs nothing on its own
# (no main, no ssh), so it is safe to source from any of the above scripts and from the unit tests.

# The TRANSIENT no-display drop-in path (#291). In /run (tmpfs) so a reboot auto-reverts to the
# deployed --display unit. Overridable for a non-default rig; this default IS the single source.
RIG_TEST_DROPIN="${RIG_TEST_DROPIN:-/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf}"

# rig_test_dropin_clear_cmds [DROPIN] -> the REMOTE bash that REMOVES the rig-test no-display drop-in
# (and its now-empty drop-in dir) + `daemon-reload`s, so a following `systemctl restart/start
# camera-box` brings camera-box back WITH --display (interkom return monitor restored). PURE string
# builder (no ssh) so the SAME commands can be either injected into a dev1-side ssh command line
# (recording-e2e.sh) OR carried into a remote heredoc and eval'd (loopback-e2e.sh) — ONE source for
# both the path and the logic. Fully idempotent: rm -f / rmdir / daemon-reload are no-ops/safe when
# no drop-in is present, so wiring this into every restore can never change a normal restart. The
# trailing `|| true` keeps each line safe under `set -e` (a transient daemon-reload hiccup must not
# abort the restore — the subsequent restart's own is-active/--display check is the fail-loud gate)
# AND on a non-systemd test host (CI).
rig_test_dropin_clear_cmds() {
  local dropin="${1:-$RIG_TEST_DROPIN}"
  local dropin_dir
  dropin_dir="$(dirname "$dropin")"
  cat <<CLEAR
rm -f "$dropin" 2>/dev/null || true
rmdir "$dropin_dir" 2>/dev/null || true
systemctl daemon-reload 2>/dev/null || true
CLEAR
}
