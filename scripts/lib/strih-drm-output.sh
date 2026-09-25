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
#   strih_drm_hdmi_connected [SYSFS_DIR] [BACKEND] [XRANDR_TEXT]
#                                           0 iff an HDMI monitor is connected: the kernel status
#                                           (lease), the X RandR view (vk-direct)
#   strih_drm_hdmi_output_from_xrandr       stdin `xrandr --query` -> the first connected HDMI name
#   strih_drm_xrandr_query USER_HOME USER   (gatherer) `xrandr --query` of display :0, or nothing
#   strih_drm_output_config_json CONN [VIEW] [BACKEND] the one-line config (refuses a bad name/view/backend)
#   strih_drm_legacy_view TEXT              the retired strih-lx-projector.json type -> initial view
#   strih_drm_output_verdict ...            verify-strih's drm-output item verdict
#   strih_drm_vk_present_dead               stdin = an OBS log -> 0 iff the vk-direct present loop died
#
# Provisioning ACTION (root, setup-strih.sh step 6; uses the caller's warn/fail):
#   strih_drm_output_provision USER_HOME DESKTOP_USER SCRIPTS_DIR BACKEND

# The M1 dark-grey solid (0x202020): the image before the first rendered frame + the fail-open one.
STRIH_DRM_OUTPUT_ARGB=2105376

# strih_drm_hdmi_connected [SYSFS_DIR] [BACKEND] [XRANDR_QUERY_TEXT] -> 0 iff an HDMI monitor is
# plugged in, read the way the box's HDMI output backend (the fact STRIH_HDMI_OUTPUT_BACKEND) needs:
#   lease (the default, also for an absent BACKEND): some <dir>/card*-HDMI*/status reads `connected`.
#     The KERNEL status is the truth there: after a lease X RandR can stick at `disconnected` while
#     the kernel stays `connected` (obs-drm-output.md M1 runbook gotcha). The xrandr text is ignored.
#   vk-direct: the X RandR view (XRANDR_QUERY_TEXT = `xrandr --query`, from strih_drm_xrandr_query)
#     names a connected HDMI output. The NVIDIA X driver does not drive the KMS connector status, so
#     sysfs reads `disconnected` with the monitor plugged in (main design 5840508308). A connected
#     output with NO mode/CRTC counts too -- that is how xrandr lists HDMI-0 while vk-direct holds it.
#     An empty text (X not up) is never connected.
strih_drm_hdmi_connected() {
  local dir="${1:-/sys/class/drm}" backend="${2:-lease}" xr="${3:-}" st
  if [ "$backend" = vk-direct ]; then
    [ -n "$(printf '%s\n' "$xr" | strih_drm_hdmi_output_from_xrandr)" ]
    return
  fi
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

# strih_drm_xrandr_query USER_HOME DESKTOP_USER -> print `xrandr --query` of the desktop user's X
# session :0 (with that user's Xauthority), or nothing when X does not answer. As root it runs as the
# desktop user (the setup-strih step-6 shape); otherwise inline (verify-strih run by the operator).
# Bounded (a wedged X never hangs setup/verify) and always returns 0, so a set -e caller survives.
strih_drm_xrandr_query() {
  local user_home="$1" desktop_user="$2"
  if [ "$(id -u)" = 0 ]; then
    sudo -u "$desktop_user" env DISPLAY=:0 XAUTHORITY="${user_home}/.Xauthority" timeout 10 xrandr --query 2>/dev/null || true
  else
    env DISPLAY=:0 XAUTHORITY="${user_home}/.Xauthority" timeout 10 xrandr --query 2>/dev/null || true
  fi
}

# strih_drm_output_config_json CONNECTOR [VIEW] [BACKEND] -> the one-line config the C module and the
# Python classifier both arm on. VIEW defaults to multiview (owner ROZHODNUTE). BACKEND (issue 1346)
# defaults to lease = NO "backend" key, so a lease config stays byte-identical to the pre-backend one;
# vk-direct (the NVIDIA Vulkan direct-display backend) is written explicitly. Refuses (prints nothing,
# returns 1) an empty / non-[A-Za-z0-9._-] connector, an unknown view or an unknown backend -- a config
# the C would ignore or keep dormant must never be written.
strih_drm_output_config_json() {
  local conn="${1:-}" view="${2:-multiview}" backend="${3:-lease}" extra=""
  case "$conn" in
    '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  case "$view" in
    program | multiview) ;;
    *) return 1 ;;
  esac
  case "$backend" in
    lease) ;;
    vk-direct) extra=',"backend":"vk-direct"' ;;
    *) return 1 ;;
  esac
  printf '{"enabled":true,"connector":"%s","argb":%s,"view":"%s"%s}\n' "$conn" "$STRIH_DRM_OUTPUT_ARGB" "$view" "$extra"
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

# strih_drm_output_verdict HDMI_CONNECTED ARMED_CONNECTOR VIEW LIVE_SCANOUT LIVE_MULTIVIEW [BACKEND FACT [DEAD]]
#   HDMI_CONNECTED   1 iff strih_drm_hdmi_connected
#   ARMED_CONNECTOR  the connector the config arms ("-" / empty = dormant: absent, disabled, bad;
#                    "?" = the classifier itself could not run, e.g. the strih_scenes import failed)
#   VIEW             program | multiview | unknown (the config's "view" token)
#   LIVE_SCANOUT     1 iff the newest OBS log has `drm-output: program scanout LIVE`
#   LIVE_MULTIVIEW   1 iff the newest OBS log has `drm-output: multiview bind LIVE`
#   BACKEND          issue 1346: the config's backend token (lease | vk-direct | unknown; "?" or empty
#                    = not read -- the check is skipped)
#   FACT             the box fact STRIH_HDMI_OUTPUT_BACKEND ("?" or empty = not read -- skipped)
#   DEAD             1 iff strih_drm_vk_present_dead on the newest OBS log (the vk present loop died)
# Prints ONE token; return code 0 = PASS, 2 = NOTE (skip / report), 1 = FAIL:
#   skip-no-hdmi        (2) no HDMI monitor and no armed config -- today's eDP-only strih-lx
#   hdmi-unplugged      (2) the config is armed but no HDMI monitor is plugged in
#   classify-failed     (1) an HDMI monitor is plugged in but the config could not be classified
#   config-missing      (1) an HDMI monitor is plugged in but no armed config -> setup-strih step 6
#   view-invalid        (1) the "view" value is neither program nor multiview (the C runs Program)
#   backend-invalid     (1) the "backend" value is neither lease nor vk-direct (the C stays dormant)
#   backend-drift       (1) the config's backend is not the box fact (setup-strih step 6 re-writes it)
#   present-dead        (1) the vk-direct present loop died after going live (the HDMI is dead)
#   lease-not-live      (1) armed + plugged, but the OBS log never reached the scanout
#   multiview-not-live  (1) view multiview, scanout live, but the built-in Multiview never bound
#   ok                  (0)
strih_drm_output_verdict() {
  local hdmi="${1:-0}" conn="${2:-}" view="${3:-program}" live="${4:-0}" mv="${5:-0}"
  local backend="${6:-?}" fact="${7:-?}" dead="${8:-0}"
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
  if [ "$backend" = unknown ]; then
    printf 'backend-invalid'
    return 1
  fi
  if [ -n "$backend" ] && [ "$backend" != "?" ] && [ -n "$fact" ] && [ "$fact" != "?" ] && [ "$backend" != "$fact" ]; then
    printf 'backend-drift'
    return 1
  fi
  if [ "$dead" = 1 ]; then
    printf 'present-dead'
    return 1
  fi
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

# strih_drm_vk_present_dead (stdin = the newest OBS log) -> 0 iff the vk-direct present loop EXITED
# (`drm-output: vk-direct present loop exited`) with no later `drm-output: stopped (vk-direct` -- the loop
# died on its own (a display the bounded rebuild could not bring back, a lost device) while OBS kept
# running, so the HDMI is dead although the log once reached `program scanout LIVE` (review round 1).
# A clean stop (halt -> exit line -> stopped line) and a log without the loop are not dead. Reads all of
# stdin (no early exit); always returns 0/1, never aborts a set -e caller.
strih_drm_vk_present_dead() {
  local verdict
  verdict="$(LC_ALL=C awk '
    index($0, "drm-output: vk-direct present loop exited") { dead = 1 }
    index($0, "drm-output: stopped (vk-direct") { dead = 0 }
    END { print dead ? "dead" : "alive" }' || true)"
  [ "$verdict" = dead ]
}

# strih_drm_output_provision USER_HOME DESKTOP_USER SCRIPTS_DIR BACKEND -- setup-strih.sh step 6 (root):
# the fixed HDMI output's activation contract, ~/.camera-box/drm-output.json of the OBS (desktop) user.
#   * the retired 19.9. projector config is removed; its "type" seeds the initial view (default multiview);
#   * BACKEND is the box fact STRIH_HDMI_OUTPUT_BACKEND (issue 1346): lease = the X RandR lease (an
#     Intel-driven connector), vk-direct = Vulkan direct display (an NVIDIA-driven connector -- the
#     NVIDIA X driver refuses the lease). vk-direct needs the Vulkan loader (installed here, a failed
#     install FAILS the step) and the NVIDIA driver's ICD (a WARN when missing);
#   * a symlinked ~/.camera-box or config is refused (root never writes through it);
#   * an EXISTING config is the operator's view choice and is kept -- only its backend is brought onto
#     the fact, as the desktop user (strih_scenes.write_drm_backend: every other key kept, one line);
#   * a fresh config is written ONLY when an HDMI monitor is connected (the kernel status for lease, the
#     X RandR view for vk-direct -- strih_drm_hdmi_connected) AND X RandR names it (`install -o <desktop
#     user>`, never a root redirect); otherwise a loud SKIP.
# Uses the caller's warn/fail (setup-strih.sh).
strih_drm_output_provision() {
  local user_home="$1" desktop_user="$2" scripts_dir="$3" backend="$4"
  # the module's activation contract: <user_home>/.camera-box/drm-output.json of the desktop user
  local DRM_CONF_DIR="${user_home}/.camera-box"
  local DRM_CONF="${DRM_CONF_DIR}/drm-output.json"
  local LEGACY_PROJ=/opt/camera-box/strih-lx-projector.json
  local DRM_VIEW0 DRM_CONN DRM_LINE DRM_XRANDR=""
  case "$backend" in
    lease | vk-direct) ;;
    *) fail "issue 1346: the HDMI output backend fact is '${backend}' (want lease or vk-direct) -- fix the box fact STRIH_HDMI_OUTPUT_BACKEND" ;;
  esac
  DRM_VIEW0="$(strih_drm_legacy_view "$(cat "$LEGACY_PROJ" 2>/dev/null || true)")"
  if [ "$backend" = vk-direct ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y libvulkan1 \
      || fail "issue 1346: apt-get install libvulkan1 failed -- the vk-direct HDMI output cannot start without the Vulkan loader"
    [ -f /usr/share/vulkan/icd.d/nvidia_icd.json ] \
      || warn "  issue 1346: no NVIDIA Vulkan ICD (/usr/share/vulkan/icd.d/nvidia_icd.json) -- the vk-direct HDMI output needs the NVIDIA driver's Vulkan"
    # The NVIDIA connector's kernel status stays `disconnected`; vk-direct detects the monitor through
    # the X RandR view (main design 5840508308).
    DRM_XRANDR="$(strih_drm_xrandr_query "$user_home" "$desktop_user")"
  fi
  if [ -L "$DRM_CONF_DIR" ] || [ -L "$DRM_CONF" ]; then
    warn "  SKIP issue 1346: ${DRM_CONF_DIR} or ${DRM_CONF} is a symlink -- refusing to write through it as root; remove it and re-run"
  elif [ -f "$DRM_CONF" ]; then
    echo "  ${DRM_CONF} already present -- leaving the operator's HDMI output choice"
    if sudo -u "$desktop_user" python3 -c '
import sys
sys.path[:0] = [sys.argv[1], "/usr/local/bin"]
try:
    import strih_scenes as s
except ImportError as e:
    sys.exit("cannot import strih_scenes (%s) -- setup-strih.sh installs python3-websocket at step 11; re-run it" % e)
try:
    s.write_drm_backend(sys.argv[2], sys.argv[3])
except ValueError as e:
    sys.exit(str(e))
' "$scripts_dir" "$backend" "$DRM_CONF"; then
      echo "  ${DRM_CONF}: backend ${backend} (the box fact STRIH_HDMI_OUTPUT_BACKEND)"
    else
      warn "  issue 1346: could not write backend ${backend} into ${DRM_CONF} (the reason is above) -- verify-strih item 4c reports it as backend-drift"
    fi
  elif [ "$backend" = vk-direct ] && [ -z "$DRM_XRANDR" ]; then
    warn "  SKIP issue 1346: vk-direct detects the HDMI monitor through X RandR, but xrandr on :0 answered nothing (Xorg :0 not up yet?) -- ${DRM_CONF} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
  elif strih_drm_hdmi_connected /sys/class/drm "$backend" "$DRM_XRANDR"; then
    [ -n "$DRM_XRANDR" ] || DRM_XRANDR="$(strih_drm_xrandr_query "$user_home" "$desktop_user")"
    DRM_CONN="$(printf '%s\n' "$DRM_XRANDR" | strih_drm_hdmi_output_from_xrandr)"
    if [ -n "$DRM_CONN" ] && DRM_LINE="$(strih_drm_output_config_json "$DRM_CONN" "$DRM_VIEW0" "$backend")"; then
      install -d -o "$desktop_user" -g "$desktop_user" "$DRM_CONF_DIR"
      printf '%s\n' "$DRM_LINE" | install -m 0644 -o "$desktop_user" -g "$desktop_user" /dev/stdin "$DRM_CONF"
      echo "  wrote ${DRM_CONF} (HDMI output ${DRM_CONN}, backend ${backend}, view ${DRM_VIEW0}; takes effect at the next OBS start)"
    else
      warn "  SKIP issue 1346: an HDMI monitor is connected but X RandR could not name it (Xorg :0 not up yet?) -- ${DRM_CONF} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
    fi
  else
    warn "  SKIP issue 1346: no HDMI monitor connected -- ${DRM_CONF} NOT provisioned (the fixed HDMI output stays dormant); attach the HDMI monitor and re-run setup-strih.sh"
  fi
  rm -f "$LEGACY_PROJ"
}
