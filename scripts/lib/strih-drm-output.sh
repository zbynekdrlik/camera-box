#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh,
# verify-strih.sh and the Tier-0 harnesses), and strict mode here would leak into all of them.
#
# scripts/lib/strih-drm-output.sh -- issue 1346 (owner 24.9.2026): the strih-lx HDMI output is the
# SAME fixed hardware output imag has -- the in-OBS DRM-lease output (issue 1152, the vendored
# libobs module; .claude/rules/obs-drm-output.md) -- selectable between Program and the BUILT-IN
# Multiview. It is never an OBS projector window and never the desktop.
#
# The module's activation contract is ONE file, ~/.camera-box/drm-output.json of the OBS user, a
# single machine-written line: {"enabled":true,"connector":"<X RandR name>","argb":2105376,
# "view":"multiview"}. The connector is the X RandR OUTPUT name (xrandr prints HDMI-0 on the
# NVIDIA-primary strih-lx, HDMI-1 under modesetting) -- never the kernel connector name HDMI-A-1.
# The operator switches the view in OBS (Tools menu) and the choice is written back into the file;
# `strih_scenes.py --projector program|multiview` is the scripted twin.
#
# Pure helpers (unit-tested by tests/strih_drm_output_provision_1346.rs):
#   strih_drm_hdmi_connected [SYSFS_DIR]    0 iff a kernel HDMI connector reads `connected`
#   strih_drm_hdmi_output_from_xrandr       stdin `xrandr --query` -> the first connected HDMI name
#   strih_drm_output_config_json CONN [VIEW] the one-line config (refuses a bad name/view)
#   strih_drm_legacy_view TEXT              the retired strih-lx-projector.json type -> initial view
#   strih_drm_output_verdict ...            verify-strih's drm-output item verdict

# The M1 dark-grey solid (0x202020): the image before the first rendered frame + the fail-open one.
STRIH_DRM_OUTPUT_ARGB=2105376

# strih_drm_hdmi_connected [SYSFS_DIR] -> 0 iff some <dir>/card*-HDMI*/status reads `connected`.
# The KERNEL status is the truth for "a monitor is plugged in": after a lease X RandR can stick at
# `disconnected` while the kernel stays `connected` (obs-drm-output.md M1 runbook gotcha).
strih_drm_hdmi_connected() {
  local dir="${1:-/sys/class/drm}" st
  for st in "$dir"/card*-HDMI*/status; do
    [ -f "$st" ] || continue
    if [ "$(cat "$st" 2>/dev/null || true)" = connected ]; then
      return 0
    fi
  done
  return 1
}

# strih_drm_hdmi_output_from_xrandr (stdin = `xrandr --query`) -> print the FIRST connected HDMI
# output's X RandR name (nothing when none). Always returns 0 and reads ALL of stdin (no early
# exit, so an upstream writer never takes a SIGPIPE under the caller's pipefail).
strih_drm_hdmi_output_from_xrandr() {
  awk '$1 ~ /^HDMI/ && $2 == "connected" && !done { print $1; done = 1 }' || true
}

# strih_drm_output_config_json CONNECTOR [VIEW] -> the one-line config the C module and the Python
# classifier both arm on. VIEW defaults to multiview (owner ROZHODNUTE). Refuses (prints nothing,
# returns 1) an empty / non-[A-Za-z0-9._-] connector or an unknown view -- a config the C would
# ignore must never be written.
strih_drm_output_config_json() {
  local conn="${1:-}" view="${2:-multiview}"
  case "$conn" in
    '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  case "$view" in
    program | multiview) ;;
    *) return 1 ;;
  esac
  printf '{"enabled":true,"connector":"%s","argb":%s,"view":"%s"}\n' "$conn" "$STRIH_DRM_OUTPUT_ARGB" "$view"
}

# strih_drm_legacy_view TEXT -> the initial view carried over from the retired
# /opt/camera-box/strih-lx-projector.json ({"type":"program"} -> program; anything else ->
# multiview, the owner's default). Always returns 0.
strih_drm_legacy_view() {
  local text="${1:-}" re='"type"[[:space:]]*:[[:space:]]*"program"'
  if [[ $text =~ $re ]]; then
    printf 'program'
  else
    printf 'multiview'
  fi
  return 0
}

# strih_drm_output_verdict HDMI_CONNECTED ARMED_CONNECTOR VIEW LIVE_SCANOUT LIVE_MULTIVIEW
#   HDMI_CONNECTED   1 iff strih_drm_hdmi_connected
#   ARMED_CONNECTOR  the connector the config arms ("-" / empty = dormant: absent, disabled, bad;
#                    "?" = the classifier itself could not run, e.g. the strih_scenes import failed)
#   VIEW             program | multiview | unknown (the config's "view" token)
#   LIVE_SCANOUT     1 iff the newest OBS log has `drm-output: program scanout LIVE`
#   LIVE_MULTIVIEW   1 iff the newest OBS log has `drm-output: multiview bind LIVE`
# Prints ONE token; return code 0 = PASS, 2 = NOTE (skip / report), 1 = FAIL:
#   skip-no-hdmi        (2) no HDMI monitor and no armed config -- today's eDP-only strih-lx
#   hdmi-unplugged      (2) the config is armed but no HDMI monitor is plugged in
#   classify-failed     (1) an HDMI monitor is plugged in but the config could not be classified
#   config-missing      (1) an HDMI monitor is plugged in but no armed config -> setup-strih step 6
#   view-invalid        (1) the "view" value is neither program nor multiview (the C runs Program)
#   lease-not-live      (1) armed + plugged, but the OBS log never reached the scanout
#   multiview-not-live  (1) view multiview, scanout live, but the built-in Multiview never bound
#   ok                  (0)
strih_drm_output_verdict() {
  local hdmi="${1:-0}" conn="${2:-}" view="${3:-program}" live="${4:-0}" mv="${5:-0}"
  [ "$conn" = "-" ] && conn=""
  if [ "$hdmi" != 1 ]; then
    if [ -n "$conn" ] && [ "$conn" != "?" ]; then
      printf 'hdmi-unplugged'
    else
      printf 'skip-no-hdmi'
    fi
    return 2
  fi
  if [ "$conn" = "?" ]; then
    printf 'classify-failed'
    return 1
  fi
  if [ -z "$conn" ]; then
    printf 'config-missing'
    return 1
  fi
  case "$view" in
    program | multiview) ;;
    *)
      printf 'view-invalid'
      return 1
      ;;
  esac
  if [ "$live" != 1 ]; then
    printf 'lease-not-live'
    return 1
  fi
  if [ "$view" = multiview ] && [ "$mv" != 1 ]; then
    printf 'multiview-not-live'
    return 1
  fi
  printf 'ok'
  return 0
}
