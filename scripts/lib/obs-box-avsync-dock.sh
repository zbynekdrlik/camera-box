#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no top-level statements): sourcing runs in
# the CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-avsync-dock.sh -- issue 1470: the camera-box A/V-sync dock (vendor/av-sync-dock:
# the lock / offset / latency display, the `mbc` ASRC readout, the diag + `LOCK-CORRECT SUGGESTED` lines
# the av-step watchdog reads) on the Linux OBS boxes. The Windows bundle always carried it; the Linux
# strih-variant bundle (linux-genlock.yml) builds + stages it since issue 1470, so setup-strih step 4's
# bundle install puts it next to obs-vban.so -- no install path of its own.
#
# This file is the dock's SPEC over the ONE engine scripts/lib/obs-box-plugin-dock.sh (issue 1436 moved
# the seed + grade there, byte-identical, when the LUFS meter became the second seeded dock).
#
# The dock reads the measurement audio on the STREAM box (CAMERA_BOX_MEASURE_SOURCE_NAME "mbc"), so the
# layout seed and the layout row are the stream role's (`obs_box_step_applies avsync-dock`,
# scripts/lib/obs-box-role.sh). The strih role installs the same bundle, so its OBS loads the plugin too:
# the plugin rows are graded on every role.
#
# setup-strih step 16g: obs_box_avsync_dock_seed USER_INI DESKTOP_USER -- the dock seeded visible +
#   docked right; its first seed decision writes `[CameraBox] AvsyncDockSeeded=true` into the same
#   user.ini (OBS saves the same hidden + floating state when the operator floats the dock and closes
#   it, which every later run must keep, and a replaced user.ini loses the marker with its layout).
#   WARN-only, rc 0.
# verify-strih item 47: obs_box_avsync_dock_grade_report LIBDIR SHAREDIR LOG USER_INI -> 3 rows:
#   (avsync-dock)         the plugin .so + its locale in the /usr prefix OBS loads;
#   (avsync-dock-loaded)  the newest OBS log's `Loaded Modules:` list names the .so (a running OBS keeps
#                         the plugin set it started with); NOTE with no log / no list yet;
#   (avsync-dock-layout)  stream role: the saved DockState holds the dock visible; FAIL absent or in
#                         OBS's hidden default before the first seed; NOTE when the operator closed it.
#                         Other roles: a NOTE naming why it is not graded.
#
# Test seams: those of scripts/lib/obs-box-plugin-dock.sh.

if ! declare -F obs_box_plugin_dock_seed >/dev/null; then
  # shellcheck source=scripts/lib/obs-box-plugin-dock.sh
  . "$(dirname "${BASH_SOURCE[0]}")/obs-box-plugin-dock.sh"
fi

# The plugin's module name (vendor/av-sync-dock CMakeLists.txt `project(...)`, setup_plugin_target's
# PREFIX "" -> <name>.so) and the dock's object name (ID_PREFIX + ".main", plugin-main.c), test-pinned.
OBS_BOX_AVSYNC_DOCK_MODULE="obs-audio-video-sync-dock"
OBS_BOX_AVSYNC_DOCK_ID="net.nagater.obs-audio-video-sync-dock..main"
# shellcheck disable=SC2034  # the engine reads the spec by name (indirect expansion)
{
  OBS_BOX_DOCK_AVSYNC_MODULE="$OBS_BOX_AVSYNC_DOCK_MODULE"
  OBS_BOX_DOCK_AVSYNC_ID="$OBS_BOX_AVSYNC_DOCK_ID"
  OBS_BOX_DOCK_AVSYNC_MARKER="AvsyncDockSeeded"
  OBS_BOX_DOCK_AVSYNC_LABEL="A/V-sync dock"
  OBS_BOX_DOCK_AVSYNC_TAG="avsync-dock"
  OBS_BOX_DOCK_AVSYNC_STEP="avsync-dock"
  OBS_BOX_DOCK_AVSYNC_ISSUE="issue 1470"
  OBS_BOX_DOCK_AVSYNC_INSTALL="the Linux genlock strih bundle carries it since issue 1470: deploy a bundle built after it (setup-strih.sh step 4)"
}

# obs_box_avsync_dock_seed USER_INI DESKTOP_USER -> setup-strih step 16g on the stream role. Always rc 0.
obs_box_avsync_dock_seed() { obs_box_plugin_dock_seed AVSYNC "$@"; }

# obs_box_avsync_dock_loaded_verdict (stdin = an OBS log) -> loaded | not-loaded | no-module-list.
obs_box_avsync_dock_loaded_verdict() { obs_box_plugin_dock_loaded_verdict "$OBS_BOX_AVSYNC_DOCK_MODULE"; }

# obs_box_avsync_dock_grade_rows LIBDIR SHAREDIR LOG USER_INI -> the 3 `OK|`/`FAIL|`/`NOTE|` rows.
obs_box_avsync_dock_grade_rows() { obs_box_plugin_dock_grade_rows AVSYNC "$@"; }

# obs_box_avsync_dock_grade_report LIBDIR SHAREDIR LOG USER_INI -> verify-strih item 47 (the caller's
# ok / bad / note). rc 1 when the grader did not print all of its rows.
obs_box_avsync_dock_grade_report() { obs_box_plugin_dock_grade_report AVSYNC "$@"; }
