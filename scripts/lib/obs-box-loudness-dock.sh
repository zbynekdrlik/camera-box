#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no top-level statements): sourcing runs in
# the CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-loudness-dock.sh -- issue 1436 (owner 4.11.2026): the LUFS meter for the stream
# operator. The Windows stream OBS had one; the Linux migration lost it. The pinned obs release
# (scripts/obs-release.pin, v0.4.13 and later) carries norihiro's obs-loudness-dock 0.5.1:
# `lib/x86_64-linux-gnu/obs-plugins/loudness-dock.so` + `share/obs/obs-plugins/loudness-dock/locale/`,
# so setup-strih step 4's bundle install puts it on every Linux OBS box -- no install path of its own.
#
# This file is the dock's SPEC over the ONE engine scripts/lib/obs-box-plugin-dock.sh (the issue-1470
# A/V-sync dock seed). The meter is the stream operator's, so the seed and the layout row run on the
# stream role only (`obs_box_step_applies loudness-dock`); the plugin rows are graded on every role (the
# strih role installs the same bundle).
#
# setup-strih step 16g: obs_box_loudness_dock_seed USER_INI DESKTOP_USER, right AFTER the A/V-sync dock
#   seed: an absent meter is appended to the right dock area, so it lands under the A/V-sync dock. Its
#   first seed decision writes `[CameraBox] LoudnessDockSeeded=true` (its own key: the A/V-sync dock's
#   marker never makes the meter's hidden + floating default "the operator's"). WARN-only, rc 0.
# verify-strih item 47: obs_box_loudness_dock_grade_report LIBDIR SHAREDIR LOG USER_INI -> 3 rows,
#   (loudness-dock) / (loudness-dock-loaded) / (loudness-dock-layout), graded like the A/V-sync dock's.

if ! declare -F obs_box_plugin_dock_seed >/dev/null; then
  # shellcheck source=scripts/lib/obs-box-plugin-dock.sh
  . "$(dirname "${BASH_SOURCE[0]}")/obs-box-plugin-dock.sh"
fi

# The module (the bundle manifest's `obs-plugins/loudness-dock.so`) and the dock's object name
# (norihiro obs-loudness-dock: ID_PREFIX "net.nagater.obs-loudness-dock." + ".main", two dots).
OBS_BOX_LOUDNESS_DOCK_MODULE="loudness-dock"
OBS_BOX_LOUDNESS_DOCK_ID="net.nagater.obs-loudness-dock..main"
# shellcheck disable=SC2034  # the engine reads the spec by name (indirect expansion)
{
  OBS_BOX_DOCK_LOUDNESS_MODULE="$OBS_BOX_LOUDNESS_DOCK_MODULE"
  OBS_BOX_DOCK_LOUDNESS_ID="$OBS_BOX_LOUDNESS_DOCK_ID"
  OBS_BOX_DOCK_LOUDNESS_MARKER="LoudnessDockSeeded"
  OBS_BOX_DOCK_LOUDNESS_LABEL="LUFS meter (loudness dock)"
  OBS_BOX_DOCK_LOUDNESS_TAG="loudness-dock"
  OBS_BOX_DOCK_LOUDNESS_STEP="loudness-dock"
  OBS_BOX_DOCK_LOUDNESS_ISSUE="issue 1436"
  OBS_BOX_DOCK_LOUDNESS_INSTALL="the pinned obs release bundle carries it since v0.4.13 (scripts/obs-release.pin): deploy a bundle at or after that pin (setup-strih.sh step 4)"
}

obs_box_loudness_dock_seed() { obs_box_plugin_dock_seed LOUDNESS "$@"; }
obs_box_loudness_dock_loaded_verdict() { obs_box_plugin_dock_loaded_verdict "$OBS_BOX_LOUDNESS_DOCK_MODULE"; }
obs_box_loudness_dock_grade_rows() { obs_box_plugin_dock_grade_rows LOUDNESS "$@"; }
obs_box_loudness_dock_grade_report() { obs_box_plugin_dock_grade_report LOUDNESS "$@"; }
