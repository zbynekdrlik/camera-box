#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the kiosk panel's config text, install and grade; no
# top-level statements besides constants) -- the scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-panel.sh -- issue 1420: the kiosk panel of the Linux OBS boxes. Owner 13.10.2026:
# "a nevieme mat pod linuxom nejaku listu s tray ikonkami/appkami atd...?", then on the strih boxes
# "Chcem to vidiet aj na strih a rozhodnut sa podla toho ako sa to bude spravat a vizerat" -- every
# role gets it, and after seeing it live: "1 mame mat jeden typ linux pracovnych zariadeni ako su
# nasetupovane mas to doplnit do nasho skriptu!!" -- one Linux workstation type (gate `16h-panel`).
#
# Owner 14.10.2026: "aplikacie v liste na linuxe ked zavriem tak tam maju byt aby som ich aj otvorit vedel"
# -- a LAUNCHER button per app the box has (OBS, Shading, Restreamer, RustDesk where present), in front
# of the window list; an app the box supervises starts through its own unit (a click on a running app
# does nothing, never a second copy).
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
#       (panel-launchers) every declared launcher .desktop file exists with the declared text;
#       (panel-unit)   the unit file equals the checkout, enabled, active, its main process /usr/bin/tint2;
#       (panel-discord) issue 1448: Discord installed on the stream role (setup installs the pinned
#                      .deb); NOTE on the strih role (production-only);
#       (panel-kbd)    issue 1441: the keyboard layout helper installed = the checkout, and its live
#                      `print` on :0 names a layout (NOTE when X answers nothing);
#       (panel-files)  issue 1458: pcmanfm installed, the recordings helper = the checkout, and
#                      pcmanfm's libfm.conf use_trash=0 (a delete frees the disk, after a confirmation);
#       (panel-power)  issue 1464: the power program installed = the checkout, and logind answers
#                      CanPowerOff + CanReboot = yes for the desktop user in the user manager (the
#                      launcher's own context), so the button powers off without a password;
#       (panel-window) a tint2 window on :0 (`wmctrl -lx` class tint2.Tint2); NOTE when X has no
#                      client list to read.
#     A box the gate leaves out prints ONE note naming why.

OBS_BOX_PANEL_PACKAGE=tint2
OBS_BOX_PANEL_UNIT=obs-box-panel.service
OBS_BOX_PANEL_BIN=/usr/bin/tint2
OBS_BOX_PANEL_WM_CLASS=tint2.Tint2
OBS_BOX_PANEL_ROWS=9
# issue 1441 (owner 18.10.2026: "da sa v liste prepinat??"): the keyboard layout item. tint2 runs the
# helper as an executor (`watch` prints US / SK on every change, a left click runs `next`); it reads and
# locks the XKB group through libX11 (Ubuntu 26.04 ships no xkb-switch). The panel always runs it with
# /usr/bin/python3; OBS_BOX_PANEL_KBD_PYTHON is only the verify row's interpreter seam for the tests.
OBS_BOX_PANEL_KBD_HELPER="${OBS_BOX_PANEL_KBD_HELPER:-/usr/local/lib/camera-box/obs_box_kbd_layout.py}"
OBS_BOX_PANEL_KBD_PYTHON="${OBS_BOX_PANEL_KBD_PYTHON:-/usr/bin/python3}"
# issue 1458 (owner 21.10.2026: "file explorer aby vedeli vymazat nahravky ktore robi obs"): the
# `Nahrávky` launcher runs the recordings helper, which opens pcmanfm in the ACTIVE OBS profile's record
# folder at click time. The binary path is the launcher's presence seam for the tests.
OBS_BOX_PANEL_FILES_PACKAGE=pcmanfm
OBS_BOX_PANEL_FILES_BIN="${OBS_BOX_PANEL_FILES_BIN:-/usr/bin/pcmanfm}"
OBS_BOX_PANEL_FILES_HELPER="${OBS_BOX_PANEL_FILES_HELPER:-/usr/local/lib/camera-box/obs_box_recordings.py}"
# issue 1464 (owner 23.10.2026: "ikonkna ka lavo dole v liste na vypnutie a restert pc ze si otvorim a
# potvrdim co chcem"): the `Vypnúť` launcher at the LEFT end runs the power program, a Slovak confirm
# dialog (Vypnúť / Reštartovať / Zrušiť) that runs `systemctl poweroff` / `reboot` only on a confirmed
# choice. Installed by step 16h like the other panel helpers. The logind read of the (panel-power) row
# runs as the desktop user inside the user manager (`systemd-run --user`), where tint2 starts the
# launcher, so polkit judges it in the same context as the real button. OBS_BOX_PANEL_LOGIND_TIMEOUT
# bounds that read (seconds).
OBS_BOX_PANEL_POWER_HELPER="${OBS_BOX_PANEL_POWER_HELPER:-/usr/local/bin/obs-box-power}"
OBS_BOX_PANEL_LOGIND_TIMEOUT="${OBS_BOX_PANEL_LOGIND_TIMEOUT:-15}"
# issue 1464 part 2 (owner 23.10.2026: "na stream-snv nevidim v liste chrome"): Google Chrome is installed
# on the stream role (OBS_BOX_RESTREAMER_HOST_PACKAGES) but never had a launcher. The binary path is the
# presence seam; the strih role never gets it (owner 24.9.2026: strih is a production-only computer).
OBS_BOX_PANEL_CHROME_BIN="${OBS_BOX_PANEL_CHROME_BIN:-/usr/bin/google-chrome-stable}"
# issue 1465 item 3 revision (owner 24.10.2026, ROZHODNUTE 6383739315: "musi byt zrozumitelne ako si ma operator
# regulovat hlasitost sluchatiek ... aj tu globalnu"): the stream operator's GLOBAL headphone volume. The item
# `Slúchadlá NN %` / `Slúchadlá stlmené` (wheel = +-5 %, left click = mute) and the notebook's volume keys run
# the SAME helper (scripts/stream_operator_volume.py), which acts on the one sink stream-operator-headphones
# only. Declared here (the item, the helper path, the keybind lines); setup-strih step 12 installs the helper
# and merges the keys (scripts/lib/stream-operator-audio.sh), verify item 9b grades them. Stream role only.
OBS_BOX_PANEL_HEADPHONES_HELPER="${OBS_BOX_PANEL_HEADPHONES_HELPER:-/usr/local/bin/obs-box-headphones}"
# The system user-unit dir (a distro package's --user unit, e.g. restreamer.service) and the RustDesk
# binary -- seams for the offline tests.
OBS_BOX_PANEL_SYS_UNIT_DIR="${OBS_BOX_PANEL_SYS_UNIT_DIR:-/usr/lib/systemd/user}"
OBS_BOX_PANEL_RUSTDESK_BIN="${OBS_BOX_PANEL_RUSTDESK_BIN:-/usr/bin/rustdesk}"
OBS_BOX_PANEL_DISCORD_BIN="${OBS_BOX_PANEL_DISCORD_BIN:-/usr/bin/discord}"
# issue 1448 part 2: the stream role installs Discord from the PINNED official .deb (read 17.10.2026);
# Discord updates its own app host under ~/.config/discord, so the pin only sets the bootstrap.
OBS_BOX_PANEL_DISCORD_URL="${OBS_BOX_PANEL_DISCORD_URL:-https://stable.dl2.discordapp.net/apps/linux/1.0.162/discord-1.0.162.deb}"
OBS_BOX_PANEL_DISCORD_SHA256="${OBS_BOX_PANEL_DISCORD_SHA256:-e7f346569784ef47eaa530c058d810bef686c7c063efcee6493d56d138f139cc}"

# obs_box_panel_launchers USER_HOME -> one `id|Name|Exec|Icon` line per app this box has, in panel order:
#   power      the power program, FIRST = the left end of the panel, on every box (issue 1464)
#   obs        strih-obs.service in the user's unit dir (every Linux OBS box; on stream-pp it is the stream OBS)
#   files      pcmanfm in the OBS record folder, through the recordings helper (issue 1458)
#   shading    bkshading-panel-app.service in the user's unit dir (the strih role)
#   restreamer restreamer.service (the restreamer package's --user unit) in the user's or the system unit dir
#   chrome     Google Chrome, on the stream role only and only where the binary is installed (issue 1464)
#   rustdesk   the RustDesk binary
#   discord    the Discord binary (issue 1448)
# A supervised app starts through its unit, never its binary: the unit runs the box's own wrapper
# (strih-obs-start.sh) and `start` on a running unit does nothing.
obs_box_panel_launchers() {
  local home="${1:?user home required}" ud
  ud="${home}/.config/systemd/user"
  printf 'power|Vypnúť|/usr/bin/python3 %s|%s\n' "$OBS_BOX_PANEL_POWER_HELPER" "$(obs_box_panel_power_icon_path "$home")"
  [ -f "${ud}/strih-obs.service" ] \
    && printf 'obs|OBS Studio|systemctl --user start strih-obs.service|%s\n' "$(obs_box_panel_obs_icon_path "$home")"
  [ -x "$OBS_BOX_PANEL_FILES_BIN" ] \
    && printf 'files|Nahrávky|/usr/bin/python3 %s open|system-file-manager\n' "$OBS_BOX_PANEL_FILES_HELPER"
  [ -f "${ud}/bkshading-panel-app.service" ] \
    && printf 'shading|Shading|systemctl --user start bkshading-panel-app.service|video-display\n'
  if [ -f "${ud}/restreamer.service" ] || [ -f "${OBS_BOX_PANEL_SYS_UNIT_DIR}/restreamer.service" ]; then
    printf 'restreamer|Restreamer|systemctl --user start restreamer.service|Restreamer\n'
  fi
  if [ -x "$OBS_BOX_PANEL_CHROME_BIN" ] && [ "$(strih_box_role)" = stream ]; then
    # issue 1469: the kiosk autologin never unlocks the GNOME login keyring, so Chrome keeps its own
    # basic password store and never pops "Unlock Login Keyring".
    printf 'chrome|Chrome|%s --password-store=basic|google-chrome\n' "$OBS_BOX_PANEL_CHROME_BIN"
  fi
  [ -x "$OBS_BOX_PANEL_RUSTDESK_BIN" ] && printf 'rustdesk|RustDesk|%s|rustdesk\n' "$OBS_BOX_PANEL_RUSTDESK_BIN"
  # issue 1448 (owner 16.10.2026, "ma byt na lavo discord v liste"): Discord where it is installed. It
  # is single-instance, so a click on a running one focuses its window, never a second copy.
  [ -x "$OBS_BOX_PANEL_DISCORD_BIN" ] && printf 'discord|Discord|%s|discord\n' "$OBS_BOX_PANEL_DISCORD_BIN"
  return 0
}

# obs_box_panel_obs_icon_path USER_HOME -> where step 16h puts the OBS logo (the vendored OBS tree's
# obs-logo-128.png, copied into this lib's dir so the staged checkout carries it).
obs_box_panel_obs_icon_path() { printf '%s/.local/share/icons/obs-box-panel/obs.png' "${1:?user home required}"; }

# obs_box_panel_power_icon_path USER_HOME -> where step 16h puts the power button's icon (issue 1464:
# scripts/lib/obs-box-panel-power.svg, shipped with the panel because `system-shutdown` exists only in
# some icon themes, so a minimal kiosk could show a blank button; tint2 renders SVG through librsvg).
obs_box_panel_power_icon_path() { printf '%s/.local/share/icons/obs-box-panel/power.svg' "${1:?user home required}"; }

# obs_box_panel_libfm_conf USER_HOME -> pcmanfm's libfm.conf (issue 1458: use_trash=0, confirm_del=1).
obs_box_panel_libfm_conf() { printf '%s/.config/libfm/libfm.conf' "${1:?user home required}"; }

# obs_box_panel_desktop_path USER_HOME ID -> the launcher's .desktop file.
obs_box_panel_desktop_path() {
  printf '%s/.local/share/applications/obs-box-panel-%s.desktop' "${1:?user home required}" "${2:?id required}"
}

# obs_box_panel_desktop_text NAME EXEC ICON -> the declared .desktop text of one launcher.
obs_box_panel_desktop_text() {
  printf '[Desktop Entry]\n# obs-box-panel launcher (issue 1420) -- WRITTEN BY setup-strih.sh step 16h; do not hand-edit.\n'
  printf 'Type=Application\nName=%s\nExec=%s\nIcon=%s\nTerminal=false\nNoDisplay=true\n' "$1" "$2" "$3"
}

# obs_box_panel_tint2rc [USER_HOME] -> the declared tint2 config: one bottom panel 30 px high over every
# monitor, launchers (L, one per app obs_box_panel_launchers finds for USER_HOME) + taskbar (T) + system
# tray (S) + on the stream role the headphone volume item (E, issue 1465) + the keyboard layout item (E,
# issue 1441) + clock (C), no window closing from the panel. No USER_HOME (or no app): no launcher. Each E
# is the next `execp = new` block in order, so the headphone block is printed before the keyboard one.
obs_box_panel_tint2rc() {
  local home="${1:-}" items=TSEC launchers="" id rest headphones=0
  if [ -n "$home" ]; then
    while IFS='|' read -r id rest; do
      [ -n "$id" ] || continue
      launchers="${launchers}launcher_item_app = $(obs_box_panel_desktop_path "$home" "$id")"$'\n'
    done < <(obs_box_panel_launchers "$home")
  fi
  if [ "$(strih_box_role)" = stream ]; then headphones=1; items=TSEEC; fi
  [ -n "$launchers" ] && items="L${items}"
  _obs_box_panel_tint2rc_base | sed "s/^panel_items = TSC\$/panel_items = ${items}/"
  if [ "$headphones" = 1 ]; then _obs_box_panel_tint2rc_headphones; fi
  _obs_box_panel_tint2rc_kbd
  if [ -n "$launchers" ]; then
    printf '\nlauncher_padding = 4 2 6\nlauncher_background_id = 0\nlauncher_icon_background_id = 0\n'
    printf 'launcher_icon_size = 22\nlauncher_tooltip = 1\nstartup_notifications = 0\n'
    printf '%s' "$launchers"
  fi
}

_obs_box_panel_tint2rc_base() {
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

# _obs_box_panel_tint2rc_kbd -> the executor block of the keyboard layout item (issue 1441): the
# continuous `watch` shows the active layout, a left click locks the next one.
_obs_box_panel_tint2rc_kbd() {
  printf '\n# the keyboard layout item (issue 1441): US / SK, a click switches (Alt+Shift still works)\n'
  printf 'execp = new\n'
  printf 'execp_command = /usr/bin/python3 %s watch\n' "$OBS_BOX_PANEL_KBD_HELPER"
  printf 'execp_interval = 2\nexecp_continuous = 1\nexecp_has_icon = 0\nexecp_centered = 1\n'
  printf 'execp_lclick_command = /usr/bin/python3 %s next\n' "$OBS_BOX_PANEL_KBD_HELPER"
  printf 'execp_tooltip = Klavesnica US / SK -- klik prepne (aj Alt+Shift)\n'
  printf 'execp_font = Sans Bold 10\nexecp_font_color = #ffffff 100\nexecp_padding = 8 0\nexecp_background_id = 0\n'
}

# _obs_box_panel_tint2rc_headphones -> the executor block of the stream operator's headphone volume item
# (issue 1465 item 3 revision): the continuous `watch` shows the level (and puts the kept level back, unmuted,
# when the sink appears), the wheel steps it, a left click mutes / unmutes.
_obs_box_panel_tint2rc_headphones() {
  local h="$OBS_BOX_PANEL_HEADPHONES_HELPER"
  printf '\n# the headphone volume item (issue 1465): Slúchadlá NN %% -- wheel = louder / quieter, click = mute\n'
  printf 'execp = new\n'
  printf 'execp_command = /usr/bin/python3 %s watch\n' "$h"
  printf 'execp_interval = 2\nexecp_continuous = 1\nexecp_has_icon = 0\nexecp_centered = 1\n'
  printf 'execp_lclick_command = /usr/bin/python3 %s toggle\n' "$h"
  printf 'execp_uwheel_command = /usr/bin/python3 %s up\n' "$h"
  printf 'execp_dwheel_command = /usr/bin/python3 %s down\n' "$h"
  printf 'execp_tooltip = Slúchadlá -- celková hlasitosť: koliesko myši hore / dole, klik = stlmiť / zapnúť (aj klávesy hlasitosti)\n'
  printf 'execp_font = Sans Bold 10\nexecp_font_color = #ffffff 100\nexecp_padding = 8 0\nexecp_background_id = 0\n'
}

# obs_box_panel_headphones_keybinds_xml -> the openbox rc.xml lines of the notebook's volume keys (a comment
# + three keybinds, indented as openbox's own <keyboard> entries), running the same helper as the panel item.
# ONE source for the merge (setup-strih step 12, through the kiosk's rc.xml filter) AND the grader.
obs_box_panel_headphones_keybinds_xml() {
  local h="$OBS_BOX_PANEL_HEADPHONES_HELPER"
  printf '  <!-- OBS-box kiosk: the stream operator headphone volume keys (issue 1465) -->\n'
  printf '  <keybind key="XF86AudioRaiseVolume"><action name="Execute"><command>/usr/bin/python3 %s up</command></action></keybind>\n' "$h"
  printf '  <keybind key="XF86AudioLowerVolume"><action name="Execute"><command>/usr/bin/python3 %s down</command></action></keybind>\n' "$h"
  printf '  <keybind key="XF86AudioMute"><action name="Execute"><command>/usr/bin/python3 %s toggle</command></action></keybind>\n' "$h"
}

# obs_box_panel_kbd_row REPO USER_HOME DESKTOP_USER -> the (panel-kbd) row: the installed helper equals the
# checkout, and its `print` as DESKTOP_USER on :0 names a layout. Read-only; always rc 0.
obs_box_panel_kbd_row() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local state label
  state="$(strih_session_app_file_state "$OBS_BOX_PANEL_KBD_HELPER" "${repo}/scripts/obs_box_kbd_layout.py")"
  if [ "$state" != ok ]; then
    printf 'FAIL|(panel-kbd) %s %s -- re-run setup-strih.sh step 16h (issue 1441)\n' "$OBS_BOX_PANEL_KBD_HELPER" \
      "$([ "$state" = absent ] && echo missing || echo 'differs from the checkout')"
    return 0
  fi
  label="$(strih_kiosk_x_run "${STRIH_SESSION_APPS_EUID:-$(id -u)}" "$user" "$home" 10 \
    "$OBS_BOX_PANEL_KBD_PYTHON" "$OBS_BOX_PANEL_KBD_HELPER" print 2>/dev/null || true)"
  label="${label%%$'\n'*}"
  if [ -z "$label" ]; then
    printf 'NOTE|(panel-kbd) helper installed; the live layout not read: X on :0 answered nothing\n'
  elif [ "$label" = "?" ]; then
    printf 'FAIL|(panel-kbd) the helper reads a layout group with no name (?) -- check setxkbmap -query on :0 (issue 1441)\n'
  else
    printf 'OK|(panel-kbd) helper installed, the panel shows layout %s\n' "$label"
  fi
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

# obs_box_panel_discord_install -> issue 1448: on the stream role, install Discord from the pinned .deb
# (sha256-verified before apt) when the package is not installed; the strih role is production-only and
# never gets it. rc 1 + a reason on failure; nothing installed on a sha mismatch.
obs_box_panel_discord_install() {
  local deb got
  [ "$(strih_box_role)" = stream ] || return 0
  [ "$(dpkg-query -W -f='${Status}' discord 2>/dev/null || true)" = "install ok installed" ] && return 0
  echo "  installing Discord from ${OBS_BOX_PANEL_DISCORD_URL}"
  deb="$(mktemp --suffix=.deb)" || { echo "obs-box-panel: mktemp failed" >&2; return 1; }
  if ! curl -fsSL -o "$deb" "$OBS_BOX_PANEL_DISCORD_URL"; then
    rm -f "$deb"; echo "obs-box-panel: Discord download failed (${OBS_BOX_PANEL_DISCORD_URL})" >&2; return 1
  fi
  got="$(sha256sum "$deb" | awk '{print $1}')"
  if [ "$got" != "$OBS_BOX_PANEL_DISCORD_SHA256" ]; then
    rm -f "$deb"
    echo "obs-box-panel: Discord .deb sha256 mismatch (want ${OBS_BOX_PANEL_DISCORD_SHA256}, got ${got}) from ${OBS_BOX_PANEL_DISCORD_URL} -- refusing to install" >&2
    return 1
  fi
  if ! DEBIAN_FRONTEND=noninteractive apt-get install -y "$deb"; then
    rm -f "$deb"; echo "obs-box-panel: apt-get install of the Discord .deb failed" >&2; return 1
  fi
  rm -f "$deb"
}

# obs_box_panel_discord_row -> the (panel-discord) grade row: stream role OK installed / FAIL missing,
# strih role NOTE (not wanted). Read-only.
obs_box_panel_discord_row() {
  if [ "$(strih_box_role)" != stream ]; then
    printf 'NOTE|(panel-discord) not graded: Discord is installed on the stream role only (the strih role is production-only)\n'
  elif [ "$(dpkg-query -W -f='${Status}' discord 2>/dev/null || true)" = "install ok installed" ]; then
    printf 'OK|(panel-discord) discord installed\n'
  else
    printf 'FAIL|(panel-discord) discord not installed -- re-run setup-strih.sh step 16h (issue 1448)\n'
  fi
}

# obs_box_panel_install REPO USER_HOME DESKTOP_USER -> setup-strih step 16h. rc 1 + a reason on failure.
obs_box_panel_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local cfg unitdir w changed=0
  [ -f "${repo}/systemd/${OBS_BOX_PANEL_UNIT}" ] \
    || { echo "obs-box-panel: systemd/${OBS_BOX_PANEL_UNIT} not found under ${repo}" >&2; return 1; }
  local pkg
  for pkg in "$OBS_BOX_PANEL_PACKAGE" "$OBS_BOX_PANEL_FILES_PACKAGE"; do
    if [ "$(dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null || true)" != "install ok installed" ]; then
      echo "  installing ${pkg}"
      DEBIAN_FRONTEND=noninteractive apt-get install -y "$pkg" \
        || { echo "obs-box-panel: apt-get install ${pkg} failed" >&2; return 1; }
    fi
  done
  cfg="$(obs_box_panel_config_path "$home")"
  unitdir="${home}/.config/systemd/user"
  strih_user_home_dir "$home" "$user" "$(dirname "$cfg")" \
    || { echo "obs-box-panel: cannot create $(dirname "$cfg")" >&2; return 1; }
  strih_user_home_dir "$home" "$user" "$unitdir" || { echo "obs-box-panel: cannot create ${unitdir}" >&2; return 1; }
  obs_box_panel_discord_install || return 1
  local id name ex icon dst appdir icondir
  appdir="${home}/.local/share/applications"
  icondir="$(dirname "$(obs_box_panel_obs_icon_path "$home")")"
  strih_user_home_dir "$home" "$user" "$appdir" || { echo "obs-box-panel: cannot create ${appdir}" >&2; return 1; }
  strih_user_home_dir "$home" "$user" "$icondir" || { echo "obs-box-panel: cannot create ${icondir}" >&2; return 1; }
  w="$(strih_session_apps_put "${repo}/scripts/lib/obs-box-panel-obs.png" "$(obs_box_panel_obs_icon_path "$home")" 0644)" || return 1
  chown "$user:$user" "$(obs_box_panel_obs_icon_path "$home")" 2>/dev/null || true
  [ "$w" = written ] && changed=1
  w="$(strih_session_apps_put "${repo}/scripts/lib/obs-box-panel-power.svg" "$(obs_box_panel_power_icon_path "$home")" 0644)" || return 1
  chown "$user:$user" "$(obs_box_panel_power_icon_path "$home")" 2>/dev/null || true
  [ "$w" = written ] && changed=1
  install -d -m 0755 "$(dirname "$OBS_BOX_PANEL_KBD_HELPER")" \
    || { echo "obs-box-panel: cannot create $(dirname "$OBS_BOX_PANEL_KBD_HELPER")" >&2; return 1; }
  w="$(strih_session_apps_put "${repo}/scripts/obs_box_kbd_layout.py" "$OBS_BOX_PANEL_KBD_HELPER" 0755)" || return 1
  echo "  ${OBS_BOX_PANEL_KBD_HELPER} ${w} (the keyboard layout item, issue 1441)"
  [ "$w" = written ] && changed=1
  install -d -m 0755 "$(dirname "$OBS_BOX_PANEL_FILES_HELPER")" \
    || { echo "obs-box-panel: cannot create $(dirname "$OBS_BOX_PANEL_FILES_HELPER")" >&2; return 1; }
  w="$(strih_session_apps_put "${repo}/scripts/obs_box_recordings.py" "$OBS_BOX_PANEL_FILES_HELPER" 0755)" || return 1
  echo "  ${OBS_BOX_PANEL_FILES_HELPER} ${w} (the recordings item, issue 1458)"
  [ "$w" = written ] && changed=1
  install -d -m 0755 "$(dirname "$OBS_BOX_PANEL_POWER_HELPER")" \
    || { echo "obs-box-panel: cannot create $(dirname "$OBS_BOX_PANEL_POWER_HELPER")" >&2; return 1; }
  w="$(strih_session_apps_put "${repo}/scripts/obs_box_power.py" "$OBS_BOX_PANEL_POWER_HELPER" 0755)" || return 1
  echo "  ${OBS_BOX_PANEL_POWER_HELPER} ${w} (the power button, issue 1464)"
  local libfm
  libfm="$(obs_box_panel_libfm_conf "$home")"
  strih_user_home_dir "$home" "$user" "$(dirname "$libfm")" \
    || { echo "obs-box-panel: cannot create $(dirname "$libfm")" >&2; return 1; }
  w="$(python3 "${repo}/scripts/obs_box_recordings.py" seed-libfm "$libfm")" \
    || { echo "obs-box-panel: cannot write ${libfm}" >&2; return 1; }
  chown "$user:$user" "$libfm" 2>/dev/null || true
  echo "  ${libfm} ${w} (pcmanfm: a delete frees the disk, after a confirmation)"
  while IFS='|' read -r id name ex icon; do
    [ -n "$id" ] || continue
    dst="$(obs_box_panel_desktop_path "$home" "$id")"
    w="$(_obs_box_panel_put_text "$(obs_box_panel_desktop_text "$name" "$ex" "$icon")" "$dst")" || return 1
    chown "$user:$user" "$dst" 2>/dev/null || true
    echo "  ${dst} ${w} (launcher: ${name})"
    [ "$w" = written ] && changed=1
  done < <(obs_box_panel_launchers "$home")
  w="$(_obs_box_panel_put_text "$(obs_box_panel_tint2rc "$home")" "$cfg")" || return 1
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
  if [ -f "$cfg" ] && [ "$(cat "$cfg")" = "$(obs_box_panel_tint2rc "$home")" ]; then
    printf 'OK|(panel-config) %s = the declared panel config\n' "$cfg"
  else
    printf 'FAIL|(panel-config) %s %s -- re-run setup-strih.sh step 16h\n' "$cfg" "$([ -f "$cfg" ] && echo 'differs from the declared config' || echo missing)"
  fi
  local id name ex icon dst badl="" nl=0
  while IFS='|' read -r id name ex icon; do
    [ -n "$id" ] || continue
    nl=$((nl + 1))
    dst="$(obs_box_panel_desktop_path "$home" "$id")"
    [ -f "$dst" ] && [ "$(cat "$dst")" = "$(obs_box_panel_desktop_text "$name" "$ex" "$icon")" ] || badl="${badl:+$badl, }${dst}"
  done < <(obs_box_panel_launchers "$home")
  if [ -n "$badl" ]; then
    printf 'FAIL|(panel-launchers) missing or not the declared launcher: %s -- re-run setup-strih.sh step 16h\n' "$badl"
  else
    printf 'OK|(panel-launchers) %s launcher(s), each the declared .desktop file\n' "$nl"
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
  obs_box_panel_discord_row
  obs_box_panel_kbd_row "$repo" "$home" "$user"
  obs_box_panel_files_row "$repo" "$home"
  obs_box_panel_power_row "$repo" "$user"
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

# obs_box_panel_files_row REPO USER_HOME -> the (panel-files) row (issue 1458): pcmanfm installed, the
# recordings helper = the checkout, libfm.conf use_trash=0.
obs_box_panel_files_row() {
  local repo="$1" home="$2" why
  if [ "$(dpkg-query -W -f='${Status}' "$OBS_BOX_PANEL_FILES_PACKAGE" 2>/dev/null || true)" != "install ok installed" ]; then
    printf 'FAIL|(panel-files) %s not installed -- re-run setup-strih.sh step 16h (issue 1458)\n' "$OBS_BOX_PANEL_FILES_PACKAGE"
  elif ! cmp -s "${repo}/scripts/obs_box_recordings.py" "$OBS_BOX_PANEL_FILES_HELPER" 2>/dev/null; then
    printf 'FAIL|(panel-files) %s missing or differs from the checkout -- re-run setup-strih.sh step 16h\n' "$OBS_BOX_PANEL_FILES_HELPER"
  elif ! why="$(python3 "${repo}/scripts/obs_box_recordings.py" check-libfm "$(obs_box_panel_libfm_conf "$home")" 2>&1)"; then
    printf 'FAIL|(panel-files) %s -- re-run setup-strih.sh step 16h\n' "$why"
  else
    printf 'OK|(panel-files) pcmanfm + the recordings launcher, %s\n' "$why"
  fi
}

# obs_box_panel_logind_can USER VERB -> logind's answer to Can<VERB> (PowerOff / Reboot) as USER inside
# USER's user manager (`systemd-run --user`, the context tint2 starts the launcher in; as root through
# strih_session_apps_user_run), bounded; prints the bare word (yes / no / challenge / na) or nothing when
# it did not answer. Always rc 0.
obs_box_panel_logind_can() {
  local user="${1:?user required}" verb="${2:?verb required}" out=""
  # busctl bounds its own call too: with --pipe the transient unit holds this read's stdout, so a busctl
  # still waiting after `timeout` killed systemd-run would keep the command substitution open
  local -a run=(systemd-run --user --wait --pipe --quiet --collect
    busctl --timeout=10 call org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager "Can${verb}")
  # stdin /dev/null: systemd-run --pipe hands its stdin to the transient unit, never verify's own
  # (as root `timeout` runs inside sudo: it bounds the read, a root `sudo -u` does not block)
  out="$(strih_session_apps_user_run "$user" timeout "$OBS_BOX_PANEL_LOGIND_TIMEOUT" "${run[@]}" \
    </dev/null 2>/dev/null || true)"
  # busctl prints one line: s "yes"
  out="${out%%$'\n'*}"
  case "$out" in
    's "'*'"') out="${out#s \"}"; printf '%s' "${out%\"}" ;;
  esac
  return 0
}

# obs_box_panel_power_row REPO DESKTOP_USER -> the (panel-power) row (issue 1464): the power program =
# the checkout, and logind lets DESKTOP_USER power off and reboot without a password (both `yes`).
# Read-only; always rc 0.
obs_box_panel_power_row() {
  local repo="$1" user="$2" off reb
  if ! cmp -s "${repo}/scripts/obs_box_power.py" "$OBS_BOX_PANEL_POWER_HELPER" 2>/dev/null; then
    printf 'FAIL|(panel-power) %s missing or differs from the checkout -- re-run setup-strih.sh step 16h (issue 1464)\n' "$OBS_BOX_PANEL_POWER_HELPER"
    return 0
  fi
  off="$(obs_box_panel_logind_can "$user" PowerOff)"
  reb="$(obs_box_panel_logind_can "$user" Reboot)"
  if [ "$off" = yes ] && [ "$reb" = yes ]; then
    printf 'OK|(panel-power) power program installed; logind lets %s power off + reboot without a password\n' "$user"
  else
    printf 'FAIL|(panel-power) logind as %s in the user manager: CanPowerOff=%s CanReboot=%s (want yes, yes) -- the panel button would be refused; check that the seat0 session is active and local (loginctl), else a polkit rule for org.freedesktop.login1.power-off / reboot (issue 1464)\n' \
      "$user" "${off:-no answer}" "${reb:-no answer}"
  fi
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
