#!/usr/bin/env bash
# airuleset:script-ok source-only lib (the operator session start + the XFCE session of the Linux OBS boxes;
# no top-level statements besides constants) -- the scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-xfce.sh -- issue 1466 (owner ROZHODNUTE 6737530836: "schvalujem postupny prechod na
# xfce"; design 6737548111). The operator desktop of a Linux OBS box is a box FACT, STRIH_DESKTOP = openbox
# (the kiosk + tint2, every box until its turn) | xfce (xfwm4 + xfce4-panel; stream-snv first). Moving a
# box, either way, is that one fact line + setup-strih.
#   * the ONE session start: /usr/local/bin/obs-box-session-start = strih_session_start_text <desktop>
#     (strih-provision.sh), installed on every box. The openbox autostart carries the same text (setup
#     step 15 writes it on every box: the shared baseline grades that file, and a box moved back needs
#     nothing else); XFCE runs the script through ~/.config/autostart/obs-box-session.desktop.
#   * obs_box_session_install REPO HOME USER -- setup-strih step 15: the script on every box; on an xfce box
#     the XDG entry, the Hidden=true overrides of the indexer / update notifier / power manager /
#     screensaver (the box never sleeps; localsearch-3 ran at 133 % CPU, measured 8.11.2026) + the
#     indexer's --user unit masked, xfwm4 compositing OFF, the session never saved on exit, the keys
#     (brightness, Print / Alt+Print -> flameshot, the stream role's headphone keys) and a polkit rule
#     that lets the desktop user power off / reboot without a password (logind answered `challenge` in
#     the XFCE session). An openbox box loses the XDG entry and the polkit rule.
#   * obs_box_xfce_panel_install REPO HOME USER -- run by setup step 16h (obs_box_panel_install): the
#     xfce4-panel layout rendered from the SAME launcher list as tint2 (obs_box_panel_launchers), the
#     standard plugins, the pulseaudio plugin on the stream role only.
#   * obs_box_session_grade_report REPO HOME USER -- verify-strih item 48 (OBS_BOX_XFCE_ROWS rows on an
#     xfce box, the session-start row + one NOTE on an openbox box).
# The xfconf channel files are MERGED and graded by property (scripts/obs_box_xfconf.py): the running XFCE
# apps rewrite them. A file whose declared properties all hold is never touched. While the desktop user's
# xfconfd runs (linger + autologin keep it up), the properties go THROUGH it (xfconf-query on the user's
# bus, each read back), never into its file, which it would overwrite from memory.
#
# Test seams: OBS_BOX_SESSION_START_BIN, OBS_BOX_XFCE_POLKIT_RULE, OBS_BOX_XFCE_XSESSION (paths);
# OBS_BOX_XFCE_PGREP (the process reader).

OBS_BOX_SESSION_START_BIN="${OBS_BOX_SESSION_START_BIN:-/usr/local/bin/obs-box-session-start}"
OBS_BOX_XFCE_POLKIT_RULE="${OBS_BOX_XFCE_POLKIT_RULE:-/etc/polkit-1/rules.d/50-obs-box-power.rules}"
OBS_BOX_XFCE_XSESSION="${OBS_BOX_XFCE_XSESSION:-/usr/share/xsessions/xfce.desktop}"
OBS_BOX_XFCE_PGREP="${OBS_BOX_XFCE_PGREP:-pgrep}"
OBS_BOX_XFCE_SESSION_ENTRY=obs-box-session.desktop
OBS_BOX_XFCE_PANEL_PLUGIN_PKG=xfce4-pulseaudio-plugin
# the rows of obs_box_session_grade_rows on an xfce box (an openbox box prints 2)
OBS_BOX_XFCE_ROWS=8

# obs_box_xfce_hidden_autostarts -> the XDG autostart ids hidden for the desktop user, one per line, each
# with the process it would start (`id process`): the file indexer (133 % CPU live), the update notifier,
# and the power manager + screensaver (absent with --no-install-recommends; hidden anyway so a later
# install can never blank or sleep the box).
obs_box_xfce_hidden_autostarts() {
  printf '%s\n' 'localsearch-3 localsearch-3' 'tracker-miner-fs-3 tracker-miner-fs-3' \
    'update-notifier update-notifier' 'xfce4-power-manager xfce4-power-manager' \
    'xfce4-screensaver xfce4-screensaver' 'light-locker light-locker'
}

# obs_box_xfce_masked_user_units -> the desktop user's --user units masked on an xfce box (the indexer
# can also be D-Bus activated; a mask is a ~/.config/systemd/user/<unit> -> /dev/null link).
obs_box_xfce_masked_user_units() { printf '%s\n' localsearch-3.service; }

# obs_box_xfce_hidden_text ID -> the Hidden=true override of one XDG autostart entry.
obs_box_xfce_hidden_text() {
  printf '[Desktop Entry]\n# issue 1466 -- WRITTEN BY setup-strih.sh step 15: never started on the OBS box; do not hand-edit.\n'
  printf 'Type=Application\nName=%s\nHidden=true\n' "${1:?id required}"
}

# obs_box_xfce_session_entry_text -> ~/.config/autostart/obs-box-session.desktop: the XFCE session runs the
# ONE session start (the openbox kiosk runs the same text from its own autostart).
obs_box_xfce_session_entry_text() {
  printf '[Desktop Entry]\n# issue 1466 -- WRITTEN BY setup-strih.sh step 15: the OBS box session start; do not hand-edit.\n'
  printf 'Type=Application\nName=OBS box session start\nExec=%s\nOnlyShowIn=XFCE;\n' "$OBS_BOX_SESSION_START_BIN"
  printf 'NoDisplay=true\nX-GNOME-Autostart-enabled=true\n'
}

# obs_box_xfce_polkit_rule_text USER -> the polkit rule: USER powers off / reboots without a password
# (the panel's Vypnúť button and the actions plugin). rc 1 on a user name that is not a plain login name.
obs_box_xfce_polkit_rule_text() {
  local user="${1-}" LC_ALL=C
  [[ "$user" =~ ^[abcdefghijklmnopqrstuvwxyz_][abcdefghijklmnopqrstuvwxyz0123456789_-]*$ ]] || return 1
  cat <<EOF
// issue 1466 -- WRITTEN BY setup-strih.sh step 15 (STRIH_DESKTOP=xfce); do not hand-edit.
// The operator powers off / reboots the OBS box from the panel without a password: logind answered
// \`challenge\` in the XFCE session (measured on strih-snv 8.11.2026). Only from the local, active
// session (the panel), never from an ssh login.
polkit.addRule(function(action, subject) {
    if (subject.local && subject.active &&
        (action.id == "org.freedesktop.login1.power-off" ||
         action.id == "org.freedesktop.login1.power-off-multiple-sessions" ||
         action.id == "org.freedesktop.login1.reboot" ||
         action.id == "org.freedesktop.login1.reboot-multiple-sessions") &&
        subject.user == "${user}") {
        return polkit.Result.YES;
    }
});
EOF
}

# obs_box_xfce_channel_path HOME CHANNEL -> the user's xfconf file of CHANNEL.
obs_box_xfce_channel_path() {
  printf '%s/.config/xfce4/xfconf/xfce-perchannel-xml/%s.xml' "${1:?home required}" "${2:?channel required}"
}

# obs_box_xfce_xfwm4_spec / _session_spec / _keys_spec -> the declared properties (scripts/obs_box_xfconf.py
# SPEC lines). xfwm4: no compositor (the projector's present-vsync and the Multiview, like the openbox
# kiosk). The session: never saved on logout (a restored OBS would start beside the supervised one). The
# keys: only ours (override), the same commands as the openbox rc.xml keybinds.
obs_box_xfce_xfwm4_spec() { printf '/general/use_compositing\tbool\tfalse\n'; }
obs_box_xfce_session_spec() { printf '/general/SaveOnExit\tbool\tfalse\n'; }
obs_box_xfce_keys_spec() {
  local shot="${OBS_BOX_PANEL_SHOT_BIN:-/usr/bin/flameshot} gui" h="${OBS_BOX_PANEL_HEADPHONES_HELPER:-/usr/local/bin/obs-box-headphones}"
  printf '/commands/custom\treplace\n'
  printf '/commands/custom/override\tbool\ttrue\n'
  printf '/commands/custom/<Alt>F1\tstring\txfce4-popup-applicationsmenu\n'
  printf '/commands/custom/XF86MonBrightnessUp\tstring\t/usr/local/bin/obs-box-brightness up\n'
  printf '/commands/custom/XF86MonBrightnessDown\tstring\t/usr/local/bin/obs-box-brightness down\n'
  printf '/commands/custom/Print\tstring\t%s\n' "$shot"
  printf '/commands/custom/<Alt>Print\tstring\t%s\n' "$shot"
  if [ "$(strih_box_role)" = stream ]; then
    printf '/commands/custom/XF86AudioRaiseVolume\tstring\t/usr/bin/python3 %s up\n' "$h"
    printf '/commands/custom/XF86AudioLowerVolume\tstring\t/usr/bin/python3 %s down\n' "$h"
    printf '/commands/custom/XF86AudioMute\tstring\t/usr/bin/python3 %s toggle\n' "$h"
  fi
}

# obs_box_xfce_panel_plugins HOME -> the panel's plugins in order, `id|kind|launcher-id` (launcher-id only
# for a launcher): the applications menu, one launcher per obs_box_panel_launchers app (Vypnúť first),
# the task list, an expanding separator, the system tray, the pulseaudio plugin on the stream role, the
# clock, the actions button.
obs_box_xfce_panel_plugins() {
  local home="${1:?home required}" n=1 id rest
  printf '%s|applicationsmenu|\n' "$n"
  while IFS='|' read -r id rest; do
    [ -n "$id" ] || continue
    n=$((n + 1)); printf '%s|launcher|%s\n' "$n" "$id"
  done < <(obs_box_panel_launchers "$home")
  n=$((n + 1)); printf '%s|tasklist|\n' "$n"
  n=$((n + 1)); printf '%s|separator|\n' "$n"
  n=$((n + 1)); printf '%s|systray|\n' "$n"
  if [ "$(strih_box_role)" = stream ]; then n=$((n + 1)); printf '%s|pulseaudio|\n' "$n"; fi
  n=$((n + 1)); printf '%s|clock|\n' "$n"
  n=$((n + 1)); printf '%s|actions|\n' "$n"
}

# obs_box_xfce_launcher_file HOME PLUGIN_ID LAUNCHER_ID -> the launcher's .desktop file in the plugin's own
# folder (the panel reads its launcher items there, so it never copies or renames ours).
obs_box_xfce_launcher_file() {
  printf '%s/.config/xfce4/panel/launcher-%s/obs-box-panel-%s.desktop' "${1:?home required}" "${2:?plugin id required}" "${3:?launcher id required}"
}

# obs_box_xfce_panel_spec HOME -> the declared xfce4-panel properties: ONE bottom panel over the full width
# (p=8 = bottom, length 100 %), locked, 30 px, and exactly the obs_box_xfce_panel_plugins list.
obs_box_xfce_panel_spec() {
  local home="${1:?home required}" pid kind lid ids=""
  printf '/panels\treplace\n/plugins\treplace\n/configver\tint\t2\n/panels\tarray:int\t1\n'
  printf '/panels/panel-1/position\tstring\tp=8;x=0;y=0\n/panels/panel-1/length\tuint\t100\n'
  printf '/panels/panel-1/position-locked\tbool\ttrue\n/panels/panel-1/size\tuint\t30\n/panels/panel-1/icon-size\tuint\t22\n'
  while IFS='|' read -r pid kind lid; do
    ids="${ids:+${ids},}${pid}"
    printf '/plugins/plugin-%s\tstring\t%s\n' "$pid" "$kind"
    case "$kind" in
      launcher) printf '/plugins/plugin-%s/items\tarray:string\tobs-box-panel-%s.desktop\n' "$pid" "$lid" ;;
      separator) printf '/plugins/plugin-%s/expand\tbool\ttrue\n/plugins/plugin-%s/style\tuint\t0\n' "$pid" "$pid" ;;
      systray) printf '/plugins/plugin-%s/square-icons\tbool\ttrue\n' "$pid" ;;
      # our own headphone keys run the helper on the one operator sink (issue 1465), never the plugin's
      pulseaudio) printf '/plugins/plugin-%s/enable-keyboard-shortcuts\tbool\tfalse\n/plugins/plugin-%s/enable-multimedia-keys\tbool\tfalse\n' "$pid" "$pid" ;;
    esac
  done < <(obs_box_xfce_panel_plugins "$home")
  printf '/panels/panel-1/plugin-ids\tarray:int\t%s\n' "$ids"
}

# obs_box_xfce_merge CHANNEL FILE SPEC_FN [ARGS...] -> the channel FILE with SPEC_FN's properties merged in
# (stdout). A renderer for obs_box_write_if_changed_rendered: rc 1 on a failed spec or merge.
obs_box_xfce_merge() {
  local channel="${1:?channel required}" file="${2:?file required}" spec
  shift 2
  spec="$("$@")" || return 1
  printf '%s\n' "$spec" | python3 "$(_obs_box_xfce_repo)/scripts/obs_box_xfconf.py" merge "$channel" "$file" -
}

# obs_box_xfce_check FILE SPEC_FN [ARGS...] -> rc 0 when every declared property holds; prints the
# helper's `ok` or the first difference.
obs_box_xfce_check() {
  local file="${1:?file required}" spec
  shift
  spec="$("$@")" || { echo "the declared properties could not be rendered"; return 1; }
  printf '%s\n' "$spec" | python3 "$(_obs_box_xfce_repo)/scripts/obs_box_xfconf.py" check "$file" -
}

_obs_box_xfce_repo() { (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd); }

# obs_box_xfce_xfconfd_running USER -> rc 0 while the user's xfconfd runs (its channels are then its own).
obs_box_xfce_xfconfd_running() { "$OBS_BOX_XFCE_PGREP" -u "${1:?user required}" -x xfconfd >/dev/null 2>&1; }

# obs_box_xfce_apply_live USER CHANNEL (stdin: the SPEC) -> set the declared properties through USER's RUNNING
# xfconfd (xfconf-query on the user's session bus, as the user when run as root) and read each back.
obs_box_xfce_apply_live() {
  local user="${1:?user required}" channel="${2:?channel required}" uid rt
  local -a runner
  uid="$(id -u "$user")" || return 1
  rt="/run/user/${uid}"
  if [ "${STRIH_SESSION_APPS_EUID:-$(id -u)}" = 0 ]; then
    runner=(sudo -u "$user" env "XDG_RUNTIME_DIR=${rt}" "DBUS_SESSION_BUS_ADDRESS=unix:path=${rt}/bus")
  else
    rt="${XDG_RUNTIME_DIR:-$rt}"
    runner=(env "DBUS_SESSION_BUS_ADDRESS=unix:path=${rt}/bus")
  fi
  python3 "$(_obs_box_xfce_repo)/scripts/obs_box_xfconf.py" apply "$channel" - "${runner[@]}" >/dev/null
}

# obs_box_xfce_write_channel HOME USER CHANNEL SPEC_FN [ARGS...] -> one channel file holds the declared
# properties: nothing is touched when every one already holds (graded by property: xfconfd rewrites the
# file in its own layout); while the user's xfconfd runs they are applied THROUGH it (it owns the file and
# writes it from memory); otherwise the file is merged + written. rc 1 on a failure.
obs_box_xfce_write_channel() {
  local home="$1" user="$2" channel="$3" file merged spec
  shift 3
  file="$(obs_box_xfce_channel_path "$home" "$channel")"
  strih_user_home_dir "$home" "$user" "$(dirname "$file")" || { echo "obs-box-xfce: cannot create $(dirname "$file")" >&2; return 1; }
  if [ -f "$file" ] && obs_box_xfce_check "$file" "$@" >/dev/null; then
    echo "  xfconf ${channel}: ${file} unchanged (every declared property holds)"
    return 0
  fi
  if obs_box_xfce_xfconfd_running "$user"; then
    spec="$("$@")" || { echo "obs-box-xfce: the ${channel} properties could not be rendered" >&2; return 1; }
    printf '%s\n' "$spec" | obs_box_xfce_apply_live "$user" "$channel" \
      || { echo "obs-box-xfce: ${channel} could not be applied through ${user}'s running xfconfd (above)" >&2; return 1; }
    echo "  xfconf ${channel}: applied through ${user}'s running xfconfd and read back (it saves ${file} itself)"
    return 0
  fi
  # the helper prints exactly one trailing newline; the stdin write below puts it back
  merged="$(obs_box_xfce_merge "$channel" "$file" "$@")" || { echo "obs-box-xfce: ${file} could not be merged (above)" >&2; return 1; }
  [ -n "$merged" ] || { echo "obs-box-xfce: ${file}: the merge printed nothing" >&2; return 1; }
  printf '%s\n' "$merged" | obs_box_write_if_changed "$file" 0644 "${user}:$(id -gn "$user")" "xfconf ${channel}" || return 1
}

# obs_box_session_install REPO HOME USER -> setup-strih step 15 (see the header). rc 1 + a reason on failure.
obs_box_session_install() {
  : "${1:?repo required}"
  local home="${2:?home required}" user="${3:?user required}" desktop line id
  local adir unitdir grp
  desktop="$(strih_box_desktop)" || { echo "obs-box-xfce: no STRIH_DESKTOP fact" >&2; return 1; }
  obs_box_write_if_changed_rendered "$OBS_BOX_SESSION_START_BIN" 0755 root:root "session start (${desktop})" \
    strih_session_start_text "$desktop" || { echo "obs-box-xfce: ${OBS_BOX_SESSION_START_BIN} could not be written (above)" >&2; return 1; }
  adir="${home}/.config/autostart"
  if [ "$desktop" != xfce ]; then
    rm -f "${adir}/${OBS_BOX_XFCE_SESSION_ENTRY}" "$OBS_BOX_XFCE_POLKIT_RULE"
    echo "  STRIH_DESKTOP=openbox: the openbox autostart runs the session start; no XFCE entry, no XFCE polkit rule"
    return 0
  fi
  [ -f "$OBS_BOX_XFCE_XSESSION" ] || { echo "obs-box-xfce: ${OBS_BOX_XFCE_XSESSION} missing -- the kiosk step (11) installs xfce4" >&2; return 1; }
  grp="$(id -gn "$user")" || return 1
  strih_user_home_dir "$home" "$user" "$adir" || { echo "obs-box-xfce: cannot create ${adir}" >&2; return 1; }
  obs_box_write_if_changed_rendered "${adir}/${OBS_BOX_XFCE_SESSION_ENTRY}" 0644 "${user}:${grp}" "XFCE session-start entry" \
    obs_box_xfce_session_entry_text || return 1
  while IFS=' ' read -r id _; do
    [ -n "$id" ] || continue
    obs_box_write_if_changed_rendered "${adir}/${id}.desktop" 0644 "${user}:${grp}" "hidden autostart ${id}" \
      obs_box_xfce_hidden_text "$id" || return 1
  done < <(obs_box_xfce_hidden_autostarts)
  unitdir="${home}/.config/systemd/user"
  strih_user_home_dir "$home" "$user" "$unitdir" || { echo "obs-box-xfce: cannot create ${unitdir}" >&2; return 1; }
  while IFS= read -r line; do
    [ -L "${unitdir}/${line}" ] && [ "$(readlink "${unitdir}/${line}")" = /dev/null ] && continue
    ln -sfn /dev/null "${unitdir}/${line}" || { echo "obs-box-xfce: cannot mask ${line}" >&2; return 1; }
    chown -h "${user}:${grp}" "${unitdir}/${line}" 2>/dev/null || true
    echo "  ${line} masked for ${user} (the file indexer stays off)"
  done < <(obs_box_xfce_masked_user_units)
  obs_box_xfce_write_channel "$home" "$user" xfwm4 obs_box_xfce_xfwm4_spec || return 1
  obs_box_xfce_write_channel "$home" "$user" xfce4-session obs_box_xfce_session_spec || return 1
  obs_box_xfce_write_channel "$home" "$user" xfce4-keyboard-shortcuts obs_box_xfce_keys_spec || return 1
  obs_box_write_if_changed_rendered "$OBS_BOX_XFCE_POLKIT_RULE" 0644 root:root "polkit power rule" \
    obs_box_xfce_polkit_rule_text "$user" || { echo "obs-box-xfce: the polkit rule could not be written (above)" >&2; return 1; }
  echo "  XFCE session for ${user}: the session start runs from ${adir}/${OBS_BOX_XFCE_SESSION_ENTRY} at the next login"
}

# obs_box_xfce_panel_install REPO HOME USER -> the xfce4-panel half of step 16h (an xfce box only): the
# pulseaudio plugin on the stream role, each launcher's .desktop in its plugin folder, the panel channel.
obs_box_xfce_panel_install() {
  : "${1:?repo required}"
  local home="${2:?home required}" user="${3:?user required}" pid kind lid name ex icon f grp
  grp="$(id -gn "$user")" || return 1
  if [ "$(strih_box_role)" = stream ] \
    && [ "$(dpkg-query -W -f='${Status}' "$OBS_BOX_XFCE_PANEL_PLUGIN_PKG" 2>/dev/null || true)" != "install ok installed" ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$OBS_BOX_XFCE_PANEL_PLUGIN_PKG" \
      || { echo "obs-box-xfce: apt-get install ${OBS_BOX_XFCE_PANEL_PLUGIN_PKG} failed" >&2; return 1; }
  fi
  while IFS='|' read -r pid kind lid; do
    [ "$kind" = launcher ] || continue
    IFS='|' read -r _ name ex icon < <(obs_box_panel_launchers "$home" | grep "^${lid}|")
    f="$(obs_box_xfce_launcher_file "$home" "$pid" "$lid")"
    strih_user_home_dir "$home" "$user" "$(dirname "$f")" || { echo "obs-box-xfce: cannot create $(dirname "$f")" >&2; return 1; }
    obs_box_write_if_changed_rendered "$f" 0644 "${user}:${grp}" "xfce4-panel launcher ${name}" \
      obs_box_panel_desktop_text "$name" "$ex" "$icon" || return 1
  done < <(obs_box_xfce_panel_plugins "$home")
  obs_box_xfce_write_channel "$home" "$user" xfce4-panel obs_box_xfce_panel_spec "$home"
}

# obs_box_xfce_panel_launchers_ok HOME -> rc 0 when every launcher plugin's .desktop file holds the
# declared text; prints the first bad file otherwise.
obs_box_xfce_panel_launchers_ok() {
  local home="${1:?home required}" pid kind lid name ex icon f
  while IFS='|' read -r pid kind lid; do
    [ "$kind" = launcher ] || continue
    IFS='|' read -r _ name ex icon < <(obs_box_panel_launchers "$home" | grep "^${lid}|")
    f="$(obs_box_xfce_launcher_file "$home" "$pid" "$lid")"
    if ! [ -f "$f" ] || [ "$(cat "$f")" != "$(obs_box_panel_desktop_text "$name" "$ex" "$icon")" ]; then
      printf '%s' "$f"; return 1
    fi
  done < <(obs_box_xfce_panel_plugins "$home")
  return 0
}

# obs_box_session_grade_rows REPO HOME USER -> `OK|text` / `FAIL|text` / `NOTE|text` lines: the session-start
# row on every box, then one NOTE on an openbox box, OBS_BOX_XFCE_ROWS - 1 xfce rows on an xfce box.
# Read-only; always rc 0.
obs_box_session_grade_rows() {
  : "${1:?repo required}"
  local home="${2:?home required}" user="${3:?user required}" desktop id proc why bad="" running="" adir
  desktop="$(strih_box_desktop 2>/dev/null || true)"
  if [ -x "$OBS_BOX_SESSION_START_BIN" ] && [ -n "$desktop" ] \
    && [ "$(cat "$OBS_BOX_SESSION_START_BIN"; printf x)" = "$(strih_session_start_text "$desktop"; printf x)" ]; then
    printf 'OK|(session-start) %s = the %s session start\n' "$OBS_BOX_SESSION_START_BIN" "$desktop"
  else
    printf 'FAIL|(session-start) %s missing, not executable or not the %s session start -- re-run setup-strih.sh step 15 (issue 1466)\n' \
      "$OBS_BOX_SESSION_START_BIN" "${desktop:-<no STRIH_DESKTOP>}"
  fi
  if [ "$desktop" != xfce ]; then
    printf 'NOTE|(xfce) not graded: STRIH_DESKTOP=%s (the openbox kiosk: the baseline kiosk + autostart rows and item 44)\n' "${desktop:-unread}"
    return 0
  fi
  adir="${home}/.config/autostart"
  if [ "$(dpkg-query -W -f='${Status}' xfce4 2>/dev/null || true)" = "install ok installed" ] && [ -f "$OBS_BOX_XFCE_XSESSION" ]; then
    printf 'OK|(xfce-pkg) xfce4 installed, %s present\n' "$OBS_BOX_XFCE_XSESSION"
  else
    printf 'FAIL|(xfce-pkg) xfce4 not installed or %s missing -- re-run setup-strih.sh step 11 (issue 1466)\n' "$OBS_BOX_XFCE_XSESSION"
  fi
  if [ "$(cat "${adir}/${OBS_BOX_XFCE_SESSION_ENTRY}" 2>/dev/null; printf x)" = "$(obs_box_xfce_session_entry_text; printf x)" ]; then
    printf 'OK|(xfce-session-entry) %s runs the session start\n' "${adir}/${OBS_BOX_XFCE_SESSION_ENTRY}"
  else
    printf 'FAIL|(xfce-session-entry) %s missing or not the declared entry -- re-run setup-strih.sh step 15\n' "${adir}/${OBS_BOX_XFCE_SESSION_ENTRY}"
  fi
  while IFS=' ' read -r id proc; do
    [ -n "$id" ] || continue
    [ "$(cat "${adir}/${id}.desktop" 2>/dev/null; printf x)" = "$(obs_box_xfce_hidden_text "$id"; printf x)" ] || bad="${bad:+$bad, }${id}.desktop"
    # -f on the command line, never -x: a process NAME is cut at 15 characters (xfce4-power-manager,
    # xfce4-screensaver, tracker-miner-fs-3 would never match, pgrep refuses the pattern)
    if "$OBS_BOX_XFCE_PGREP" -u "$user" -f "(^|/)${proc}( |\$)" >/dev/null 2>&1; then running="${running:+$running, }${proc}"; fi
  done < <(obs_box_xfce_hidden_autostarts)
  while IFS= read -r id; do
    [ "$(readlink "${home}/.config/systemd/user/${id}" 2>/dev/null)" = /dev/null ] || bad="${bad:+$bad, }${id} not masked"
  done < <(obs_box_xfce_masked_user_units)
  if [ -n "$bad$running" ]; then
    printf 'FAIL|(xfce-hidden) %s%s -- re-run setup-strih.sh step 15 and log in again\n' "${bad:+not hidden: ${bad}}" "${running:+${bad:+; }running: ${running}}"
  else
    printf 'OK|(xfce-hidden) the indexer, update notifier, power manager and screensaver are hidden and not running\n'
  fi
  why=""
  why="$(obs_box_xfce_check "$(obs_box_xfce_channel_path "$home" xfwm4)" obs_box_xfce_xfwm4_spec)" || true
  if [ "$why" = ok ]; then why="$(obs_box_xfce_check "$(obs_box_xfce_channel_path "$home" xfce4-session)" obs_box_xfce_session_spec)" || true; fi
  if [ "$why" = ok ]; then
    printf 'OK|(xfce-settings) xfwm4 compositing off, the session never saved on logout\n'
  else
    printf 'FAIL|(xfce-settings) %s -- re-run setup-strih.sh step 15 with the session logged out\n' "${why:-no answer}"
  fi
  why="$(obs_box_xfce_check "$(obs_box_xfce_channel_path "$home" xfce4-keyboard-shortcuts)" obs_box_xfce_keys_spec)" || true
  if [ "$why" = ok ]; then
    printf 'OK|(xfce-keys) brightness, Print / Alt+Print -> flameshot%s bound\n' "$([ "$(strih_box_role)" = stream ] && echo ', the headphone keys')"
  else
    printf 'FAIL|(xfce-keys) %s -- re-run setup-strih.sh step 15 with the session logged out\n' "${why:-no answer}"
  fi
  if [ "$(cat "$OBS_BOX_XFCE_POLKIT_RULE" 2>/dev/null; printf x)" = "$(obs_box_xfce_polkit_rule_text "$user"; printf x)" ]; then
    printf 'OK|(xfce-polkit) %s lets %s power off + reboot\n' "$OBS_BOX_XFCE_POLKIT_RULE" "$user"
  else
    printf 'FAIL|(xfce-polkit) %s missing or not the declared rule -- re-run setup-strih.sh step 15\n' "$OBS_BOX_XFCE_POLKIT_RULE"
  fi
  if "$OBS_BOX_XFCE_PGREP" -u "$user" -x xfce4-session >/dev/null 2>&1; then
    printf 'OK|(xfce-running) the XFCE session runs for %s\n' "$user"
  else
    printf 'FAIL|(xfce-running) no xfce4-session runs for %s -- the box is not in the session STRIH_DESKTOP names (reboot after setup-strih)\n' "$user"
  fi
  return 0
}

# obs_box_session_grade_report REPO HOME USER -> verify-strih item 48 through the caller's ok / bad / note.
# rc 1 unless every row was printed.
obs_box_session_grade_report() {
  local st d rows=0 want=2
  [ "$(strih_box_desktop 2>/dev/null || true)" = xfce ] && want="$OBS_BOX_XFCE_ROWS"
  while IFS='|' read -r st d; do
    case "$st" in OK) ok "$d" ;; NOTE) note "$d" ;; *) bad "$d" ;; esac
    rows=$((rows + 1))
  done < <(obs_box_session_grade_rows "$@")
  [ "$rows" = "$want" ]
}
