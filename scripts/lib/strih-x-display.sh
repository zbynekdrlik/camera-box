#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements that
# act) -- deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh,
# verify-strih.sh and the Tier-0 harnesses), and strict mode here would leak into all of them.
#
# scripts/lib/strih-x-display.sh -- issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez
# monitora aj po zapojeni monitora aj po zapnuti!!!"; main design 6386467303, layer 1): X never depends on
# a monitor and never holds a fixed HDMI output.
#
# WHY: strih-pp booted 24.10.2026 with only the Multiview monitor (HDMI-0, a fixed vk-direct output of the
# drm-output config) on. With no Screen options, the NVIDIA X driver put HDMI-0 into its layout
# (`Validated MetaModes: "DFP-0:nvidia-auto-select"`, the boot display), the autostart turned it off and
# left an 8x8 screen, and OBS's vkAcquireXlibDisplayEXT('HDMI-0') read -13 -- with no X CRTC on it (no
# `still in the X layout` line). Every boot with the operator's NEC on (DP) acquired both outputs.
#
# WHAT: ONE xorg.conf.d OutputClass for the NVIDIA driver (the mechanism the driver package's own
# 10-nvidia.conf uses for AllowEmptyInitialConfiguration):
#   * `MetaModes` names ONLY the operator outputs (every NVIDIA connector X lists, minus the fixed ones),
#     each `nvidia-auto-select` -- a display a MetaMode does not name is off in it, and a listed one that
#     is not connected is dropped -- so X never sets a mode on a fixed output, at any monitor state;
#   * `AllowEmptyInitialConfiguration` (already on through the package default) is stated explicitly:
#     with no operator monitor X starts with no display at all, and the session display watch
#     (scripts/strih_display_watch.py) keeps that screen usable.
# The NVIDIA README (Display Device Names) accepts connector names (`DP-0`, `HDMI-0`) in MetaModes. NVIDIA
# connector names are one word + a number (`DP-0`, `HDMI-1`, `DVI-D-0`); an iGPU sink's outputs (`eDP-1-2`
# on strih-snv) carry a provider suffix and are not the NVIDIA driver's, so they are left out.
# The file applies at the next X start; it is never followed by an X restart here.
#
#   strih_x_display_nvidia_outputs     stdin `xrandr` -> the NVIDIA connector names, one per line
#   strih_x_display_conf_text FIXED OUTPUTS -> the file text (rc 1, nothing printed, when it cannot be built)
#   strih_x_display_fixed SCRIPTS_DIR CONF -> the armed connectors of the drm-output config
#   strih_x_display_provision ...      setup-strih step 6 (strih_drm_output_provision calls it)
#   strih_x_display_grade_row ...      verify-strih's (display-x-config) row (the display watch's rows)
#
# Test seams: STRIH_X_CONF_DIR (/etc/X11/xorg.conf.d), STRIH_X_XORG_LOG (/var/log/Xorg.0.log).

STRIH_X_DISPLAY_CONF_NAME=15-camera-box-fixed-hdmi.conf

strih_x_display_conf_path() {
  printf '%s/%s' "${STRIH_X_CONF_DIR:-/etc/X11/xorg.conf.d}" "$STRIH_X_DISPLAY_CONF_NAME"
}

strih_x_display_xorg_log() { printf '%s' "${STRIH_X_XORG_LOG:-/var/log/Xorg.0.log}"; }

# strih_x_display_name_ok NAME -> 0 iff NAME is a plain connector name (letters, digits, '.', '-'; 1..63
# bytes): nothing that could break out of a quoted xorg.conf value.
strih_x_display_name_ok() {
  # the sets spelled out (a bracket RANGE follows the locale), held in locals (one 32+ char literal reads
  # as a secret to the commit scan)
  local LC_ALL=C n="${1-}" upper='ABCDEFGHIJKLMNOPQRSTUVWXYZ' lower='abcdefghijklmnopqrstuvwxyz' digit='0123456789'
  [ "${#n}" -ge 1 ] && [ "${#n}" -le 63 ] || return 1
  case "$n" in
    *[!${upper}${lower}${digit}.-]*) return 1 ;;
  esac
  return 0
}

# strih_x_display_nvidia_outputs (stdin = `xrandr --query` / `--current`) -> every output X lists whose
# name is one word + a number (the NVIDIA connector names), one per line, in X's order. Reads all of
# stdin; always rc 0.
strih_x_display_nvidia_outputs() {
  LC_ALL=C awk '($2 == "connected" || $2 == "disconnected") && $1 ~ /^[A-Za-z]+(-[A-Za-z]+)*-[0-9]+$/ { print $1 }' || true
}

# strih_x_display_metamodes FIXED OUTPUTS -> `DP-0: nvidia-auto-select, ...` over OUTPUTS minus FIXED
# (both space-separated). rc 1 (nothing printed) on a bad name or when no operator output remains.
strih_x_display_metamodes() {
  local fixed=" ${1-} " o out=""
  for o in ${2-}; do
    strih_x_display_name_ok "$o" || return 1
    case "$fixed" in *" $o "*) continue ;; esac
    out="${out:+$out, }${o}: nvidia-auto-select"
  done
  [ -n "$out" ] || return 1
  printf '%s' "$out"
}

# strih_x_display_conf_text FIXED OUTPUTS -> the xorg.conf.d file. rc 1 (nothing printed) when FIXED is
# empty, a name is not plain, or no operator output remains.
strih_x_display_conf_text() {
  local f mm
  [ -n "${1// /}" ] || return 1
  for f in ${1-}; do strih_x_display_name_ok "$f" || return 1; done
  mm="$(strih_x_display_metamodes "${1-}" "${2-}")" || return 1
  cat <<EOF
# WRITTEN BY setup-strih.sh step 6 (camera-box issue 1424, design 6386467303 layer 1). Do not hand-edit.
# The fixed HDMI outputs of the drm-output config (${1}) never enter the X layout: X starts with only the
# operator outputs named below that are connected, and with an empty screen when none is (the display
# watch keeps that screen usable), so OBS's vk-direct acquire finds them free at every monitor state.
# Applies at the next X start.
Section "OutputClass"
    Identifier "camera-box-fixed-hdmi"
    MatchDriver "nvidia-drm"
    Option "AllowEmptyInitialConfiguration" "true"
    Option "MetaModes" "${mm}"
EndSection
EOF
}

# strih_x_display_fixed SCRIPTS_DIR CONF -> the connectors CONF arms, space-separated ("" when CONF is
# absent or disabled), read with the C module's grammar (drm_output_config.py from SCRIPTS_DIR, else the
# installed copy). rc 1 when CONF cannot be classified.
strih_x_display_fixed() {
  python3 - "${1-}" "${2-}" 2>/dev/null <<'PY'
import sys
sys.path[:0] = [sys.argv[1], "/usr/local/bin"]
import drm_output_config as c
text = c.drm_output_config_text(sys.argv[2])
if not text.strip():
    print("")
    sys.exit(0)
verdict, outs = c.drm_output_instances(text)
if verdict == "ok":
    print(" ".join(o["connector"] for o in outs))
elif verdict == "disabled":
    print("")
else:
    sys.exit(1)
PY
}

# strih_x_display_provision USER_HOME DESKTOP_USER SCRIPTS_DIR CONF -- setup-strih step 6 (root), right
# after the drm-output config: write the file when it differs, remove it when the config arms nothing.
# Never restarts X. Uses the caller's warn and strih_drm_xrandr_query (strih-drm-output.sh). Always rc 0.
strih_x_display_provision() {
  local home="$1" user="$2" sd="$3" conf="$4" path fixed xr outs text
  path="$(strih_x_display_conf_path)"
  if ! fixed="$(strih_x_display_fixed "$sd" "$conf")"; then
    warn "  issue 1424: ${conf} cannot be classified -- the X config ${path} is left as it is"
    return 0
  fi
  if [ -z "$fixed" ]; then
    if [ -e "$path" ] || [ -L "$path" ]; then
      rm -f "$path" && echo "  removed ${path} (no fixed HDMI output armed; applies at the next X start)"
    fi
    return 0
  fi
  xr="$(strih_drm_xrandr_query "$home" "$user")"
  outs="$(printf '%s\n' "$xr" | strih_x_display_nvidia_outputs | tr '\n' ' ')"
  if [ -z "${outs// /}" ]; then
    warn "  SKIP issue 1424: xrandr on :0 named no output (Xorg :0 not up yet?) -- ${path} NOT written; re-run setup-strih.sh after the kiosk session is up"
    return 0
  fi
  if ! text="$(strih_x_display_conf_text "$fixed" "$outs")"; then
    warn "  SKIP issue 1424: no operator output besides the fixed '${fixed}' among '${outs% }' (or a name is not plain) -- ${path} NOT written"
    return 0
  fi
  if [ -f "$path" ] && [ ! -L "$path" ] && [ "$(cat "$path" 2>/dev/null || true)" = "$text" ]; then
    echo "  ${path} unchanged (fixed HDMI outputs ${fixed} stay out of X)"
    return 0
  fi
  if install -d -m 0755 "$(dirname "$path")" && rm -f "$path" && printf '%s\n' "$text" | install -m 0644 /dev/stdin "$path"; then
    echo "  wrote ${path} (fixed HDMI outputs ${fixed} stay out of X, operator outputs: $(strih_x_display_metamodes "$fixed" "$outs"); applies at the next X start, never restarted here)"
  else
    warn "  issue 1424: could not write ${path} -- verify-strih's (display-x-config) row FAILs until it is written"
  fi
  return 0
}

# strih_x_display_grade_row REPO USER_HOME DESKTOP_USER -> ONE row `OK|text` / `FAIL|text` / `NOTE|text`
# (display-x-config): the file matches what step 6 writes for the box's drm-output config and the X
# outputs, the running X applied it (its log carries the MetaModes option, else a NOTE: it applies at the
# next X start), and no fixed output has an X mode now. Reads X through strih_kiosk_x_run
# (strih-session-apps.sh). Always rc 0.
strih_x_display_grade_row() {
  local repo="$1" home="$2" user="$3" conf path fixed xr outs want mm f inx=""
  conf="${home}/.camera-box/drm-output.json"
  path="$(strih_x_display_conf_path)"
  if ! fixed="$(strih_x_display_fixed "${repo}/scripts" "$conf")"; then
    printf 'FAIL|(display-x-config) %s cannot be classified -- re-run setup-strih.sh step 6\n' "$conf"
    return 0
  fi
  if [ -z "$fixed" ]; then
    if [ -e "$path" ]; then
      printf 'FAIL|(display-x-config) %s is present but no fixed HDMI output is armed -- re-run setup-strih.sh step 6 (it removes it)\n' "$path"
    else
      printf 'OK|(display-x-config) no fixed HDMI output armed -- no X config needed\n'
    fi
    return 0
  fi
  xr="$(strih_kiosk_x_run "${STRIH_SESSION_APPS_EUID:-$(id -u)}" "$user" "$home" 10 xrandr --current 2>/dev/null || true)"
  outs="$(printf '%s\n' "$xr" | strih_x_display_nvidia_outputs | tr '\n' ' ')"
  if [ -z "${outs// /}" ]; then
    printf 'FAIL|(display-x-config) xrandr on :0 named no output (X not answering for %s) -- cannot grade %s\n' "$user" "$path"
    return 0
  fi
  if ! want="$(strih_x_display_conf_text "$fixed" "$outs")"; then
    printf 'FAIL|(display-x-config) no operator output besides the fixed %s among %s -- no X config can keep them out of X\n' "$fixed" "${outs% }"
    return 0
  fi
  if [ ! -f "$path" ] || [ "$(cat "$path" 2>/dev/null || true)" != "$want" ]; then
    printf 'FAIL|(display-x-config) %s %s -- re-run setup-strih.sh step 6\n' "$path" \
      "$([ -f "$path" ] && echo 'differs from what step 6 writes' || echo missing)"
    return 0
  fi
  for f in $fixed; do
    printf '%s\n' "$xr" | LC_ALL=C awk -v n="$f" '$1 == n && $3 ~ /^[0-9]+x[0-9]+\+/ { f = 1 } $1 == n && $4 ~ /^[0-9]+x[0-9]+\+/ { f = 1 } END { exit !f }' \
      && inx="${inx:+$inx }$f"
  done
  if [ -n "$inx" ]; then
    printf 'FAIL|(display-x-config) the fixed output(s) %s are in the X layout now -- the display watch turns them off; read journalctl --user -u strih-display-watch\n' "$inx"
    return 0
  fi
  mm="$(strih_x_display_metamodes "$fixed" "$outs")"
  if grep -aqF "Option \"MetaModes\" \"${mm}\"" "$(strih_x_display_xorg_log)" 2>/dev/null; then
    printf 'OK|(display-x-config) %s: fixed %s kept out of X, MetaModes %s live in the running X\n' "$path" "$fixed" "$mm"
  else
    printf 'NOTE|(display-x-config) %s is written (fixed %s kept out of X) but the running X started without it -- it applies at the next X start\n' "$path" "$fixed"
  fi
  return 0
}
