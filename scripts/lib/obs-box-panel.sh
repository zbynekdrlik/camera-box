#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the kiosk panel's config text, install and grade; no
# top-level statements besides constants) -- the scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-panel.sh -- issue 1420: the kiosk panel of the Linux OBS boxes. Owner 13.10.2026:
# "a nevieme mat pod linuxom nejaku listu s tray ikonkami/appkami atd...?", then on the strih boxes
# "Chcem to vidiet aj na strih a rozhodnut sa podla toho ako sa to bude spravat a vizerat" -- every
# role gets it as a trial (gate `16h-panel` in scripts/lib/obs-box-role.sh).
#
# The panel is tint2 (the distro package) at the bottom of :0: the running windows (a click switches,
# beside Alt-Tab; a right click does nothing, so no window is closed from the panel), the XEmbed system
# tray (the Restreamer's appindicator icon falls back to XEmbed without a StatusNotifier host) and a
# clock. It runs as the --user unit systemd/obs-box-panel.service (Restart=always), started by the kiosk
# autostart after the DISPLAY import.
#   * obs_box_panel_tint2rc       -- the ONE declared config text (~/.config/tint2/tint2rc).
#   * obs_box_panel_install       -- setup-strih step 16h: the package, the config and the unit written
#                                    only on a difference, the unit enabled; never a start (the autostart
#                                    starts it at login; a changed config is picked up by a running panel
#                                    on its next restart, so a running unit is try-restarted).
#   * obs_box_panel_grade_report  -- verify-strih item 44, through the caller's ok / bad / note:
#       (panel-pkg)    tint2 installed;
#       (panel-config) the config equals the declared text;
#       (panel-unit)   the unit file equals the checkout, enabled, active, its main process /usr/bin/tint2;
#       (panel-window) a tint2 window on :0 (`wmctrl -lx` class tint2.Tint2); NOTE when X has no
#                      client list to read.
#     A box the gate leaves out prints ONE note naming why.

OBS_BOX_PANEL_PACKAGE=tint2
OBS_BOX_PANEL_UNIT=obs-box-panel.service
OBS_BOX_PANEL_BIN=/usr/bin/tint2
OBS_BOX_PANEL_WM_CLASS=tint2.Tint2
OBS_BOX_PANEL_ROWS=4

# obs_box_panel_tint2rc -> the declared tint2 config: one bottom panel 30 px high over every monitor,
# taskbar (T) + system tray (S) + clock (C), no launcher, no window closing from the panel.
obs_box_panel_tint2rc() {
  cat <<'TINT2RC'
# obs-box-panel (issue 1420) -- WRITTEN BY setup-strih.sh step 16h; do not hand-edit.
# Background 1: the panel
rounded = 0
border_width = 0
background_color = #1c1c1c 100
border_color = #000000 0
# Background 2: the active window's button
rounded = 3
border_width = 1
background_color = #2f5d8a 100
border_color = #5b8fc4 100
# Background 3: an inactive window's button
rounded = 3
border_width = 1
background_color = #2a2a2a 100
border_color = #3a3a3a 100

panel_items = TSC
panel_size = 100% 30
panel_margin = 0 0
panel_padding = 4 2 6
panel_background_id = 1
panel_position = bottom center horizontal
panel_layer = top
panel_monitor = all
strut_policy = follow_size
autohide = 0
wm_menu = 0
panel_dock = 0

taskbar_mode = single_desktop
taskbar_padding = 0 0 4
taskbar_background_id = 0
taskbar_name = 0

task_text = 1
task_icon = 1
task_centered = 1
task_maximum_size = 260 26
task_padding = 6 2 4
task_font = Sans 10
task_font_color = #ffffff 100
task_background_id = 3
task_active_background_id = 2
task_iconified_background_id = 3
mouse_left = toggle_iconify
mouse_middle = none
mouse_right = none
mouse_scroll_up = none
mouse_scroll_down = none

systray_padding = 6 2 6
systray_background_id = 0
systray_sort = ascending
systray_icon_size = 22

time1_format = %H:%M:%S
time2_format = %d.%m.%Y
time1_font = Sans Bold 11
time2_font = Sans 8
clock_font_color = #ffffff 100
clock_padding = 8 0
clock_background_id = 0
TINT2RC
}

# obs_box_panel_config_path USER_HOME -> the config path the unit reads.
obs_box_panel_config_path() { printf '%s/.config/tint2/tint2rc' "${1:?user home required}"; }

# _obs_box_panel_put_text TEXT DST -> write TEXT to DST (mode 0644) only when it differs; prints
# `unchanged` / `written`. rc 1 on a write failure.
_obs_box_panel_put_text() {
  local text="$1" dst="$2"
  if [ -f "$dst" ] && [ "$(cat "$dst")" = "$text" ]; then
    printf 'unchanged'
    return 0
  fi
  printf '%s\n' "$text" >"${dst}.new" && chmod 0644 "${dst}.new" && mv -f "${dst}.new" "$dst" \
    || { echo "obs-box-panel: cannot write ${dst}" >&2; return 1; }
  printf 'written'
}

# obs_box_panel_install REPO USER_HOME DESKTOP_USER -> setup-strih step 16h. rc 1 + a reason on failure.
obs_box_panel_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local cfg unitdir w changed=0
  [ -f "${repo}/systemd/${OBS_BOX_PANEL_UNIT}" ] \
    || { echo "obs-box-panel: systemd/${OBS_BOX_PANEL_UNIT} not found under ${repo}" >&2; return 1; }
  if [ "$(dpkg-query -W -f='${Status}' "$OBS_BOX_PANEL_PACKAGE" 2>/dev/null || true)" != "install ok installed" ]; then
    echo "  installing ${OBS_BOX_PANEL_PACKAGE}"
    DEBIAN_FRONTEND=noninteractive apt-get install -y "$OBS_BOX_PANEL_PACKAGE" \
      || { echo "obs-box-panel: apt-get install ${OBS_BOX_PANEL_PACKAGE} failed" >&2; return 1; }
  fi
  cfg="$(obs_box_panel_config_path "$home")"
  unitdir="${home}/.config/systemd/user"
  strih_user_home_dir "$home" "$user" "$(dirname "$cfg")" \
    || { echo "obs-box-panel: cannot create $(dirname "$cfg")" >&2; return 1; }
  strih_user_home_dir "$home" "$user" "$unitdir" || { echo "obs-box-panel: cannot create ${unitdir}" >&2; return 1; }
  w="$(_obs_box_panel_put_text "$(obs_box_panel_tint2rc)" "$cfg")" || return 1
  chown "$user:$user" "$cfg" 2>/dev/null || true
  echo "  ${cfg} ${w}"
  [ "$w" = written ] && changed=1
  w="$(strih_session_apps_put "${repo}/systemd/${OBS_BOX_PANEL_UNIT}" "${unitdir}/${OBS_BOX_PANEL_UNIT}" 0644)" || return 1
  chown "$user:$user" "${unitdir}/${OBS_BOX_PANEL_UNIT}" 2>/dev/null || true
  echo "  ${unitdir}/${OBS_BOX_PANEL_UNIT} ${w}"
  [ "$w" = written ] && changed=1
  strih_session_apps_user_systemctl "$user" daemon-reload \
    || { echo "obs-box-panel: systemctl --user daemon-reload failed" >&2; return 1; }
  strih_session_apps_user_systemctl "$user" enable "$OBS_BOX_PANEL_UNIT" \
    || { echo "obs-box-panel: systemctl --user enable ${OBS_BOX_PANEL_UNIT} failed" >&2; return 1; }
  if [ "$changed" = 1 ]; then
    # only a RUNNING panel is restarted onto the new files; the autostart starts a stopped one at login
    strih_session_apps_user_systemctl "$user" try-restart "$OBS_BOX_PANEL_UNIT" \
      || { echo "obs-box-panel: systemctl --user try-restart ${OBS_BOX_PANEL_UNIT} failed" >&2; return 1; }
  fi
  echo "  ${OBS_BOX_PANEL_UNIT} enabled (started by the kiosk autostart at login, never from setup)"
}

# obs_box_panel_grade_rows REPO USER_HOME DESKTOP_USER -> OBS_BOX_PANEL_ROWS lines `OK|text` / `FAIL|text`
# / `NOTE|text`. Read-only; always rc 0.
obs_box_panel_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local cfg unit pid exe en act wins
  if [ "$(dpkg-query -W -f='${Status}' "$OBS_BOX_PANEL_PACKAGE" 2>/dev/null || true)" = "install ok installed" ]; then
    printf 'OK|(panel-pkg) %s installed\n' "$OBS_BOX_PANEL_PACKAGE"
  else
    printf 'FAIL|(panel-pkg) %s not installed -- re-run setup-strih.sh step 16h (issue 1420)\n' "$OBS_BOX_PANEL_PACKAGE"
  fi
  cfg="$(obs_box_panel_config_path "$home")"
  if [ -f "$cfg" ] && [ "$(cat "$cfg")" = "$(obs_box_panel_tint2rc)" ]; then
    printf 'OK|(panel-config) %s = the declared panel config\n' "$cfg"
  else
    printf 'FAIL|(panel-config) %s %s -- re-run setup-strih.sh step 16h\n' "$cfg" "$([ -f "$cfg" ] && echo 'differs from the declared config' || echo missing)"
  fi
  unit="${home}/.config/systemd/user/${OBS_BOX_PANEL_UNIT}"
  en="$(strih_session_apps_user_systemctl "$user" is-enabled "$OBS_BOX_PANEL_UNIT" 2>/dev/null || true)"
  act="$(strih_session_apps_user_systemctl "$user" is-active "$OBS_BOX_PANEL_UNIT" 2>/dev/null || true)"
  pid="$(strih_session_apps_user_systemctl "$user" show -p MainPID --value "$OBS_BOX_PANEL_UNIT" 2>/dev/null || true)"
  exe=""
  if [ -n "$pid" ] && [ "$pid" != 0 ]; then exe="$(readlink "${OBS_BOX_PANEL_PROC:-/proc}/${pid}/exe" 2>/dev/null || true)"; fi
  if ! cmp -s "${repo}/systemd/${OBS_BOX_PANEL_UNIT}" "$unit" 2>/dev/null; then
    printf 'FAIL|(panel-unit) %s missing or differs from the checkout -- re-run setup-strih.sh step 16h\n' "$unit"
  elif [ "$en" != enabled ] || [ "$act" != active ] || [ "$exe" != "$OBS_BOX_PANEL_BIN" ]; then
    printf 'FAIL|(panel-unit) %s enabled=%s active=%s main process=%s (want enabled, active, %s) -- systemctl --user start %s; journalctl --user -u %s\n' \
      "$OBS_BOX_PANEL_UNIT" "${en:-unknown}" "${act:-unknown}" "${exe:-none}" "$OBS_BOX_PANEL_BIN" "$OBS_BOX_PANEL_UNIT" "$OBS_BOX_PANEL_UNIT"
  else
    printf 'OK|(panel-unit) %s enabled + active, main process %s\n' "$OBS_BOX_PANEL_UNIT" "$exe"
  fi
  wins="$(strih_session_apps_wmctrl "$user" "$home")"
  if [ -z "$wins" ]; then
    printf 'NOTE|(panel-window) not graded: wmctrl on :0 answered nothing (X down or no client list)\n'
  elif awk -v c="$OBS_BOX_PANEL_WM_CLASS" '$3 == c {f = 1} END {exit !f}' <<<"$wins"; then
    printf 'OK|(panel-window) the tint2 panel window is on :0\n'
  else
    printf 'FAIL|(panel-window) no %s window on :0 -- the panel is not shown\n' "$OBS_BOX_PANEL_WM_CLASS"
  fi
  return 0
}

# obs_box_panel_grade_report REPO USER_HOME DESKTOP_USER -> verify-strih item 44 through the caller's ok /
# bad / note; on a box the gate leaves out ONE note. rc 1 unless every row was printed.
obs_box_panel_grade_report() {
  local why st d rows=0
  if ! why="$(obs_box_step_applies 16h-panel)"; then
    note "(panel) not graded: ${why}"
    return 0
  fi
  while IFS='|' read -r st d; do
    case "$st" in OK) ok "$d" ;; NOTE) note "$d" ;; *) bad "$d" ;; esac
    rows=$((rows + 1))
  done < <(obs_box_panel_grade_rows "$@")
  [ "$rows" = "$OBS_BOX_PANEL_ROWS" ]
}
