#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih session-app constants, the setup-strih step 16d
# install and the verify-strih items 37-40 grader; no top-level statements besides constants) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. setup-strih.sh / verify-strih.sh set their own.
# Every function checks its own return codes, because a caller runs it in an `||` list, where errexit
# is off.
#
# scripts/lib/strih-session-apps.sh -- issue 1399 (owner 4.10.2026: "toto je produkcny pocitac vsetko ma
# bezat vzdy a stale"): the supervised --user units in the strih-snv operator session, beside
# strih-obs.service (design comment 5977447339, Approach 1; Companion Satellite + its watch joined by
# design comment 5979157737):
#
#   * strih-browser-keeper.service -> scripts/strih_browser_keeper.py: refreshes each OBS browser source
#     once per OBS run (a real OBS restart, never a keeper restart or a WS reconnect) as soon as its page
#     server answers, and again when the page server comes back; never a working page.
#   * bkshading-panel-app.service -> scripts/bkshading_panel_app.py: the bkshading panel (:8770) in a
#     WebKitGTK window titled "Shading" -- a NORMAL window that openbox Alt+Tab reaches.
#   * companion-satellite.service -> the /opt Companion Satellite binary setup-strih step 16 installs (the
#     strih Stream Deck XL agent): Restart=always + journal output instead of a bare autostart launch.
#   * strih-satellite-watch.timer (+ its oneshot strih-satellite-watch.service) ->
#     scripts/strih_satellite_watch.py every 30 s: restarts the Satellite when its own REST shows a
#     sustained fault (not connected while the Companion port answers / no surface while the Stream Deck
#     is on USB), never while Companion is down.
#   * dantesync-tray.service -> the dantesync tray (/usr/local/bin/dantesync-tray, the pinned dantesync
#     release's `dantesync-tray-linux-amd64`, first in v1.18.0) -- the clock state as an icon in the tint2
#     systray, like on the Windows boxes (issue 1429, owner 14.10.2026; design comment 6176755827). Every
#     Linux workstation runs it, the stream role too. Step 16d installs the binary through
#     dantesync_install_tray (scripts/lib/dantesync-install.sh); the unit carries
#     ConditionFileIsExecutable, so while the pin is below 1.18.0 (no binary) its start is a clean skip,
#     never a restart loop.
#   * strih-display-watch.service -> scripts/strih_display_watch.py, every role (issue 1424, owner
#     24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj po zapnuti!!!";
#     design 6386467303 layer 2): an operator monitor that comes on joins the X layout, a fixed HDMI output
#     of the drm-output config stays out of it, and with no operator monitor the screen stays usable
#     (>= 1920x1080), so tint2 and OBS always have one. Its extra row is its last pass (--check-state).
#   * strih-stream-monitor.service -> scripts/strih_stream_monitor.py, the STREAM role only (issue 1436,
#     owner 15.10.2026): the headphones play the audio of the real outgoing RTMP stream straight to the
#     jack's ALSA PCM, never through PipeWire. Its packages + three extra verify rows live in
#     scripts/lib/strih-stream-monitor.sh.
#
# Every entry is WantedBy=graphical-session.target like strih-obs.service, and like it STARTED by the
# kiosk openbox autostart (strih_openbox_autostart_text calls strih_session_apps_autostart_lines):
# openbox never reaches graphical-session.target, and a default.target unit would also start on a
# user-manager start with no X session (an ssh login), where a window cannot open. ONE login start
# path, the one OBS uses; the strih-snv genlock deploy also starts them after strih-obs, into the running
# session (scripts/lib/strih-snv-deploy.sh strih_lx_remote_start_cmd).
#
#   * strih_session_apps_install -- setup-strih step 16d: the packages (python3-gi + WebKit2 4.1 +
#     python3-websocket, and the tray's GTK 3 / appindicator / xdo runtime) and an import preflight, the
#     dantesync tray from the pinned release, the repo programs into the bin dir, every unit file
#     into the operator's ~/.config/systemd/user (each written only when its bytes or mode differ), the
#     4.10.2026 hand-made stopgaps taken over (any WantedBy=default.target link of an entry + the panel's
#     ~/.local/bin copy removed; a unit file itself is overwritten under the same name), `systemctl
#     --user enable`, and a `try-restart` of the entries whose files changed -- a stopped unit is never
#     started (enable-only, the kiosk pattern), a running one is moved onto the new files.
#   * strih_session_apps_grade_rows / _report -- verify-strih items 37 (keeper), 38 (panel), 39
#     (Satellite) + 40 (its watch) + the (dantesync-tray) row: each entry installed byte-identical to the
#     checkout with its program,
#     enabled, started by the kiosk autostart, active, and its main process running the installed program
#     and started after the files were written (for the watch timer: its last run); the keeper's and the
#     watch's last pass recent; the panel window (its WM_CLASS + the title "Shading") on :0.
#
# Test seams (tests/python/strih_session_apps_fakes_1399.py): STRIH_SESSION_APPS_BIN_DIR (/usr/local/bin),
# STRIH_SESSION_APPS_RUNTIME_DIR (/run/user/<uid>), STRIH_SESSION_APPS_EUID (the running uid),
# STRIH_SESSION_APPS_PYTHON (/usr/bin/python3, the import preflight), STRIH_SESSION_APPS_WMCTRL
# (wmctrl), STRIH_SESSION_APPS_PROC (/proc), STRIH_SESSION_APPS_SATELLITE_BIN (the /opt Satellite
# binary), DANTESYNC_INSTALL_TRAY_BIN (the dantesync tray, dantesync-install.sh's seam);
# `dpkg-query`, `apt-get`, `sudo`, `systemctl`, `id` and `curl` are looked up on PATH.

# issue 1412: the user unit dir is created with every level owned by the user (strih_user_home_dir).
if ! declare -F strih_user_home_dir >/dev/null; then
  # shellcheck source=scripts/lib/strih-user-home.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-user-home.sh"
fi
# issue 1436: the stream-role headphone monitor's packages + verify rows.
if ! declare -F strih_stream_monitor_grade_rows >/dev/null; then
  # shellcheck source=scripts/lib/strih-stream-monitor.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-stream-monitor.sh"
fi
# issue 1424: the display watch's X config row (the fixed HDMI outputs kept out of X).
if ! declare -F strih_x_display_grade_row >/dev/null; then
  # shellcheck source=scripts/lib/strih-x-display.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-x-display.sh"
fi
# issue 1429: the dantesync tray binary (install + its path, the pin).
if ! declare -F dantesync_install_tray >/dev/null; then
  # shellcheck source=scripts/lib/dantesync-install.sh
  . "$(dirname "${BASH_SOURCE[0]}")/dantesync-install.sh"
fi

# The session apps, in start order (the kiosk autostart and the deploy start them in this order).
STRIH_SESSION_APP_UNITS=(strih-browser-keeper.service bkshading-panel-app.service companion-satellite.service strih-satellite-watch.timer dantesync-tray.service strih-display-watch.service)
# issue 1429: libgtk-3-0t64 + libayatana-appindicator3-1 + libxdo3 = the dantesync tray's runtime.
STRIH_SESSION_APP_PACKAGES=(python3-gi gir1.2-webkit2-4.1 python3-websocket libgtk-3-0t64 libayatana-appindicator3-1 libxdo3)
STRIH_SESSION_APP_WANTS_TARGET=graphical-session.target
STRIH_PANEL_WINDOW_TITLE=Shading
# `res_name.res_class` of the panel window as `wmctrl -lx` prints it: GDK takes both from the program
# name the app sets (GLib.set_prgname, PRGNAME in scripts/bkshading_panel_app.py; read live under Xvfb).
STRIH_PANEL_WM_CLASS=bkshading-panel-app.Bkshading-panel-app
# The keeper's state file, under the user's runtime dir (the unit passes --state-file %t/<this>).
STRIH_BROWSER_KEEPER_STATE_FILE=strih-browser-keeper.json
# The Satellite watch's state file, under the user's runtime dir (its service passes --state-file %t/<this>).
STRIH_SATELLITE_WATCH_STATE_FILE=strih-satellite-watch.json
# issue 1424: the display watch's state file, under the user's runtime dir (its unit passes --state-file %t/<this>).
STRIH_DISPLAY_WATCH_STATE_FILE=strih-display-watch.json
STRIH_DISPLAY_WATCH_UNIT=strih-display-watch.service
# Companion Satellite: the ONE path of the Electron desktop build setup-strih step 16 installs with the
# tarball's own install.sh --system (strih_companion_satellite_bin prints it), and its unit.
STRIH_COMPANION_SATELLITE_BIN=/opt/companion-satellite/companion-satellite
# shellcheck disable=SC2034  # consumed cross-file by verify-strih.sh's (companion) item
STRIH_COMPANION_SATELLITE_UNIT=companion-satellite.service
# issue 1429: the dantesync tray's unit (its binary: dantesync_install_tray_bin).
STRIH_DANTESYNC_TRAY_UNIT=dantesync-tray.service
# The 4.10.2026 hand-made stopgaps (issue comments 5977460330 panel, 5979039607 Satellite): units
# WantedBy=default.target, and the panel's ~/.local/bin copy of the app. Paths relative to the
# operator's home; the wants dir holds the link of any entry enabled that way.
STRIH_SESSION_APP_STOPGAP_WANTS_DIR=.config/systemd/user/default.target.wants
STRIH_PANEL_STOPGAP_BIN=.local/bin/bkshading-panel-app

# --- constants + path seams ------------------------------------------------------------------------

# strih_session_app_script UNIT -> the repo program (scripts/<name>) the entry installs into the bin dir;
# EMPTY for an entry that runs an installed binary: the Satellite (the /opt binary step 16 installs) and
# the dantesync tray (the release binary step 16d installs). rc 1 for an unknown unit.
strih_session_app_script() {
  case "${1-}" in
    strih-browser-keeper.service) printf 'strih_browser_keeper.py' ;;
    bkshading-panel-app.service) printf 'bkshading_panel_app.py' ;;
    companion-satellite.service) printf '' ;;
    strih-satellite-watch.timer) printf 'strih_satellite_watch.py' ;;
    dantesync-tray.service) printf '' ;;
    strih-display-watch.service) printf 'strih_display_watch.py' ;;
    strih-stream-monitor.service) printf 'strih_stream_monitor.py' ;;
    *) return 1 ;;
  esac
}

# strih_session_app_item UNIT -> the verify-strih row label of the unit's item.
strih_session_app_item() {
  case "${1-}" in
    strih-browser-keeper.service) printf 'browser-keeper' ;;
    bkshading-panel-app.service) printf 'shading-app' ;;
    companion-satellite.service) printf 'companion-satellite' ;;
    strih-satellite-watch.timer) printf 'satellite-watch' ;;
    dantesync-tray.service) printf 'dantesync-tray' ;;
    strih-display-watch.service) printf 'display-watch' ;;
    strih-stream-monitor.service) printf 'stream-monitor' ;;
    *) return 1 ;;
  esac
}

strih_session_apps_bin_dir() { printf '%s' "${STRIH_SESSION_APPS_BIN_DIR:-/usr/local/bin}"; }

# strih_session_app_program UNIT -> the absolute path of the program the entry runs on the box: the bin
# dir copy of its repo program, the /opt Satellite binary (STRIH_SESSION_APPS_SATELLITE_BIN, the test
# seam) or the dantesync tray (dantesync_install_tray_bin, DANTESYNC_INSTALL_TRAY_BIN). rc 1 for an
# unknown unit.
strih_session_app_program() {
  local script
  script="$(strih_session_app_script "${1-}")" || return 1
  if [ -n "$script" ]; then
    printf '%s/%s' "$(strih_session_apps_bin_dir)" "$script"
  elif [ "${1-}" = "$STRIH_DANTESYNC_TRAY_UNIT" ]; then
    dantesync_install_tray_bin
  else
    printf '%s' "${STRIH_SESSION_APPS_SATELLITE_BIN:-$STRIH_COMPANION_SATELLITE_BIN}"
  fi
}

# strih_session_app_binary_installer UNIT -> what installs the binary of an entry that runs one (named
# by the install WARN and the verify remedy).
strih_session_app_binary_installer() {
  case "${1-}" in
    dantesync-tray.service) printf 'setup-strih.sh step 16d (it installs the dantesync tray from the pinned dantesync release, v%s+)' "$DANTESYNC_TRAY_FIRST_VERSION" ;;
    *) printf 'setup-strih.sh step 16 (the Companion Satellite install)' ;;
  esac
}

# strih_session_app_absent_note UNIT -> rc 0 + the NOTE text when an entry's binary is legitimately absent:
# the dantesync tray while the pin is below the first release with a Linux tray. rc 1 otherwise (an
# unreadable pin included: a missing binary is then graded as missing).
strih_session_app_absent_note() {
  local pin
  [ "${1-}" = "$STRIH_DANTESYNC_TRAY_UNIT" ] || return 1
  pin="$(dantesync_install_pinned_version 2>/dev/null)" || return 1
  dantesync_install_version_ge "$pin" "$DANTESYNC_TRAY_FIRST_VERSION" && return 1
  printf 'not installed: the pinned dantesync v%s ships no Linux tray (first in v%s) -- it arrives with the fleet roll to v%s+ and the DANTESYNC_VERSION_PIN bump, then setup-strih.sh step 16d' \
    "$pin" "$DANTESYNC_TRAY_FIRST_VERSION" "$DANTESYNC_TRAY_FIRST_VERSION"
}

# strih_session_app_unit_files UNIT -> every unit file the entry installs, one per line: a timer brings
# the service it runs (the same basename); any other entry is its own unit.
strih_session_app_unit_files() {
  case "${1-}" in
    *.timer) printf '%s\n%s\n' "$1" "${1%.timer}.service" ;;
    *) printf '%s\n' "${1-}" ;;
  esac
}

# strih_session_app_run_unit UNIT -> the unit whose main process runs the program: a timer's service,
# else the unit itself.
strih_session_app_run_unit() {
  case "${1-}" in
    *.timer) printf '%s' "${1%.timer}.service" ;;
    *) printf '%s' "${1-}" ;;
  esac
}

# strih_session_app_autostart_line UNIT -> the ONE kiosk autostart line that starts UNIT (written by
# strih_openbox_autostart_text, grepped by verify-strih items 37-40 and its (companion) item).
strih_session_app_autostart_line() { printf 'systemctl --user start %s || true' "${1-}"; }

# strih_session_apps_autostart_lines -> the kiosk openbox autostart lines that START every entry at
# every login, one per entry (an entry that is not installed yet never blocks the next line).
strih_session_apps_autostart_lines() {
  local u
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    printf '%s\n' "$(strih_session_app_autostart_line "$u")"
  done
}

# strih_session_apps_packages -> the packages step 16d installs, one per line: STRIH_SESSION_APP_PACKAGES
# plus the headphone monitor's (ffmpeg + alsa-utils) when the box runs it (issue 1436, the stream role).
strih_session_apps_packages() {
  printf '%s\n' "${STRIH_SESSION_APP_PACKAGES[@]}"
  if _strih_session_apps_has "$STRIH_STREAM_MONITOR_UNIT"; then
    printf '%s\n' "${STRIH_STREAM_MONITOR_PACKAGES[@]}"
  fi
}

# strih_session_apps_missing_packages -> each package of strih_session_apps_packages that dpkg does not
# report "install ok installed", one per line. Always rc 0.
strih_session_apps_missing_packages() {
  local p
  for p in $(strih_session_apps_packages); do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] || printf '%s\n' "$p"
  done
  return 0
}

# strih_session_apps_import_check -> the python the import preflight runs: everything the two programs
# import at runtime (the keeper: websocket; the panel: Gtk 3 + WebKit2 4.1 through gi).
strih_session_apps_import_check() {
  printf '%s' 'import websocket, gi; gi.require_version("Gtk", "3.0"); gi.require_version("WebKit2", "4.1"); from gi.repository import GLib, Gtk, WebKit2'
}

# strih_session_apps_user_run USER CMD... -> CMD as USER with USER's runtime dir: as root through
# `sudo -u USER XDG_RUNTIME_DIR=/run/user/<uid>` (setup-strih and the step-17 verify run as root, whose
# own --user manager is not the operator's), else inline. Returns CMD's rc (1 when USER has no uid).
strih_session_apps_user_run() {
  local user="${1:?user required}" uid
  shift
  if [ "${STRIH_SESSION_APPS_EUID:-$(id -u)}" = 0 ]; then
    uid="$(id -u "$user" 2>/dev/null)" || return 1
    sudo -u "$user" XDG_RUNTIME_DIR="/run/user/${uid}" "$@"
  else
    "$@"
  fi
}

# strih_session_apps_user_systemctl USER ARGS... -> `systemctl --user ARGS` in USER's session (through
# strih_session_apps_user_run). Returns systemctl's rc.
strih_session_apps_user_systemctl() {
  local user="${1:?user required}"
  shift
  strih_session_apps_user_run "$user" systemctl --user "$@"
}

# strih_session_apps_runtime_dir USER -> the user's runtime dir (/run/user/<uid>); rc 1 when the uid
# cannot be read.
strih_session_apps_runtime_dir() {
  local uid
  if [ -n "${STRIH_SESSION_APPS_RUNTIME_DIR:-}" ]; then
    printf '%s' "$STRIH_SESSION_APPS_RUNTIME_DIR"
    return 0
  fi
  uid="$(id -u "${1:?user required}" 2>/dev/null)" || return 1
  printf '/run/user/%s' "$uid"
}

# --- setup-strih step 16d ----------------------------------------------------------------------------

# strih_session_apps_install REPO USER_HOME DESKTOP_USER -> install every session app (see the header).
# rc 1 + a stderr line on a missing source file, a package that will not install, a failed import
# preflight or a failed file install; a `systemctl --user enable` that cannot reach the user bus (a box
# provisioned with nobody logged in) only WARNs -- the autostart starts the units regardless. A missing
# /opt Satellite binary only WARNs too (step 16 installs it and fails loud itself), and so does a dantesync
# tray that could not be fetched (dantesync_install_tray names why; verify-strih's (dantesync-tray) row
# FAILs it): a GitHub outage must not stop the other session apps, in a deploy that already stopped OBS.
strih_session_apps_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local bindir unitdir u f script program missing py w1 uw w2 err unit_written link before after tray_failed
  local -a changed
  bindir="$(strih_session_apps_bin_dir)"
  unitdir="${home}/.config/systemd/user"
  py="${STRIH_SESSION_APPS_PYTHON:-/usr/bin/python3}"
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    script="$(strih_session_app_script "$u")" || return 1
    while IFS= read -r f; do
      [ -f "${repo}/systemd/${f}" ] || { echo "strih-session-apps: systemd/${f} not found under ${repo}" >&2; return 1; }
    done < <(strih_session_app_unit_files "$u")
    [ -z "$script" ] || [ -f "${repo}/scripts/${script}" ] \
      || { echo "strih-session-apps: scripts/${script} not found under ${repo}" >&2; return 1; }
  done

  missing="$(strih_session_apps_missing_packages)"
  if [ -n "$missing" ]; then
    echo "  installing ${missing//$'\n'/ }"
    # shellcheck disable=SC2086  # one word per package name
    DEBIAN_FRONTEND=noninteractive apt-get install -y $missing || {
      echo "strih-session-apps: apt-get install ${missing//$'\n'/ } failed" >&2; return 1; }
    missing="$(strih_session_apps_missing_packages)"
    [ -z "$missing" ] || { echo "strih-session-apps: still not installed after apt-get: ${missing//$'\n'/ }" >&2; return 1; }
  fi
  "$py" -c "$(strih_session_apps_import_check)" || {
    echo "strih-session-apps: ${py} cannot import websocket + Gtk 3 + WebKit2 4.1 (packages ${STRIH_SESSION_APP_PACKAGES[*]})" >&2
    return 1; }
  echo "  packages present + importable: $(strih_session_apps_packages | tr '\n' ' ')"

  install -d -m 0755 "$bindir" || { echo "strih-session-apps: cannot create ${bindir}" >&2; return 1; }
  strih_user_home_dir "$home" "$user" "$unitdir" || { echo "strih-session-apps: cannot create ${unitdir}" >&2; return 1; }   # issue 1412: every level the user's
  changed=()
  unit_written=0
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    script="$(strih_session_app_script "$u")" || return 1
    program="$(strih_session_app_program "$u")" || return 1
    w1=unchanged
    tray_failed=0
    if [ -n "$script" ]; then
      w1="$(strih_session_apps_put "${repo}/scripts/${script}" "$program" 0755)" || return 1
    elif [ "$u" = "$STRIH_DANTESYNC_TRAY_UNIT" ]; then
      # issue 1429: the tray follows the pin; a replaced binary restarts a running tray (try-restart below)
      before="$(sha256sum "$program" 2>/dev/null | awk '{print $1}' || true)"
      dantesync_install_tray || {
        tray_failed=1
        echo "  WARN: the dantesync tray step failed (above) -- verify-strih (dantesync-tray) names what is missing; re-run setup-strih.sh step 16d"
      }
      after="$(sha256sum "$program" 2>/dev/null | awk '{print $1}' || true)"
      [ "$before" = "$after" ] || w1=written
    fi
    uw=unchanged
    while IFS= read -r f; do
      w2="$(strih_session_apps_put "${repo}/systemd/${f}" "${unitdir}/${f}" 0644)" || return 1
      [ "$w2" = written ] && { uw=written; unit_written=1; }
    done < <(strih_session_app_unit_files "$u")
    if [ "$w1" = written ] || [ "$uw" = written ]; then
      changed+=("$u")
      if [ -n "$script" ]; then
        echo "  installed ${u} -> ${unitdir}/${u} (runs ${program}): program ${w1}, unit ${uw}"
      elif [ "$u" = "$STRIH_DANTESYNC_TRAY_UNIT" ]; then
        echo "  installed ${u} -> ${unitdir}/${u} (runs ${program}): binary ${w1}, unit ${uw}"
      else
        echo "  installed ${u} -> ${unitdir}/${u} (runs ${program}, installed by step 16): unit ${uw}"
      fi
    elif [ -n "$script" ]; then
      echo "  ${u} + ${program} unchanged"
    else
      echo "  ${u} unchanged (runs ${program})"
    fi
    [ -n "$script" ] || [ -x "$program" ] || [ "$tray_failed" = 1 ] || strih_session_app_absent_note "$u" >/dev/null \
      || echo "  WARN: ${program} not installed -- re-run $(strih_session_app_binary_installer "$u"); ${u} cannot run without it"
  done

  # issue 1424 (independent review): the display watch's role policy (HDMI never on the desktop on the
  # strih role); a changed policy restarts a running watch onto it
  if _strih_session_apps_has "$STRIH_DISPLAY_WATCH_UNIT"; then
    w2="$(strih_display_watch_env_put "$home")" || return 1
    echo "  display watch policy ~/${STRIH_DISPLAY_WATCH_ENV_REL}: $(strih_display_watch_env_text) ($([ "$w2" = written ] && echo written || echo kept))"
    if [ "$w2" = written ]; then
      case " ${changed[*]-} " in *" ${STRIH_DISPLAY_WATCH_UNIT} "*) ;; *) changed+=("$STRIH_DISPLAY_WATCH_UNIT") ;; esac
    fi
  fi

  # A stopgap's WantedBy=default.target link would also start its app on a non-graphical login.
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    link="${home}/${STRIH_SESSION_APP_STOPGAP_WANTS_DIR}/${u}"
    if [ -L "$link" ] || [ -e "$link" ]; then
      rm -f "$link" || { echo "strih-session-apps: cannot remove ${link}" >&2; return 1; }
      echo "  removed the 4.10.2026 stopgap link ~/${STRIH_SESSION_APP_STOPGAP_WANTS_DIR}/${u} (the unit starts from the kiosk autostart)"
    fi
  done
  if [ -e "${home}/${STRIH_PANEL_STOPGAP_BIN}" ]; then
    rm -f "${home}/${STRIH_PANEL_STOPGAP_BIN}" || { echo "strih-session-apps: cannot remove ${home}/${STRIH_PANEL_STOPGAP_BIN}" >&2; return 1; }
    echo "  removed the stopgap copy ~/${STRIH_PANEL_STOPGAP_BIN} (the unit runs ${bindir}/bkshading_panel_app.py)"
  fi
  chown -R "${user}:${user}" "$unitdir" 2>/dev/null || echo "  WARN: could not chown ${unitdir} to ${user}"

  if ! err="$(strih_session_apps_user_systemctl "$user" enable "${STRIH_SESSION_APP_UNITS[@]}" 2>&1)"; then
    # No user bus = no running unit either: nothing to reload or restart.
    echo "  WARN: systemctl --user enable failed for ${user} (${err//$'\n'/ }) -- enable by hand once logged in: systemctl --user enable ${STRIH_SESSION_APP_UNITS[*]} (the autostart starts them regardless)"
    return 0
  fi
  echo "  enabled ${STRIH_SESSION_APP_UNITS[*]} (a stopped unit is NOT started: the kiosk openbox autostart starts them at every login)"
  # A RUNNING entry whose program or unit files changed is restarted, so the new code runs (try-restart
  # never starts a stopped unit: still enable-only; the running 4.10.2026 Satellite stopgap moves onto
  # the provisioned unit here). A written unit file is reloaded explicitly first, so a restart never runs
  # a cached old definition. In the genlock deploy OBS is stopped here, so a keeper restart costs no
  # extra refresh.
  if [ "$unit_written" = 1 ] && ! err="$(strih_session_apps_user_systemctl "$user" daemon-reload 2>&1)"; then
    echo "  WARN: systemctl --user daemon-reload failed (${err//$'\n'/ }) -- not restarting onto unit files the manager has not read; run daemon-reload + restart by hand"
    return 0
  fi
  if [ "${#changed[@]}" -gt 0 ]; then
    if err="$(strih_session_apps_user_systemctl "$user" try-restart "${changed[@]}" 2>&1)"; then
      echo "  try-restart ${changed[*]} (only a running unit is restarted onto the new files)"
    else
      echo "  WARN: systemctl --user try-restart ${changed[*]} failed (${err//$'\n'/ }) -- restart them by hand: a running one still runs the old files"
    fi
  fi
  return 0
}

# strih_session_apps_put SRC DST MODE -> install SRC as DST with MODE only when the bytes or the mode
# differ; prints `written` or `unchanged` (an unchanged file keeps its mtime, which verify's
# stale-process check compares against). rc 1 + a stderr line on a failed install.
strih_session_apps_put() {
  local src="$1" dst="$2" mode="$3"
  if [ -f "$dst" ] && cmp -s "$src" "$dst" && [ "$(stat -c %a "$dst" 2>/dev/null)" = "${mode#0}" ]; then
    printf 'unchanged'
    return 0
  fi
  install -m "$mode" "$src" "$dst" || { echo "strih-session-apps: cannot install ${dst}" >&2; return 1; }
  printf 'written'
}

# --- verify-strih items 37-40 -------------------------------------------------------------------------

# strih_session_app_file_state INSTALLED REFERENCE -> ok | absent | differs (byte comparison).
strih_session_app_file_state() {
  if [ ! -f "${1-}" ]; then
    printf 'absent'
  elif cmp -s "$1" "${2-}"; then
    printf 'ok'
  else
    printf 'differs'
  fi
}

# strih_session_app_units_state UNITDIR REFDIR FILE... -> ok | absent | differs over every unit file of
# an entry (a timer + its service): absent if any is absent, else differs if any differs.
strih_session_app_units_state() {
  local dir="${1-}" ref="${2-}" f st out=ok
  shift 2
  for f in "$@"; do
    st="$(strih_session_app_file_state "${dir}/${f}" "${ref}/${f}")"
    case "$st" in
      absent) printf 'absent'; return 0 ;;
      differs) out=differs ;;
    esac
  done
  printf '%s' "$out"
}

# strih_session_app_process_state PROGRAM CMDLINE START NEWEST -> ok | unreadable | wrong-program |
# stale, rc 0 iff ok. Whether a python entry's RUNNING process runs the installed files: CMDLINE is the
# process argv with spaces for the NULs (/proc/<MainPID>/cmdline), its second word must be PROGRAM
# (`/usr/bin/python3 <PROGRAM> ...`); START is the unit's ExecMainStartTimestamp in unix seconds (a
# leading @ allowed); NEWEST is the newest mtime of the installed program + unit. A process started
# before the files were last written still runs the old code (stale) -- an installed-but-not-running
# change must never read as live.
strih_session_app_process_state() {
  local program="${1-}" cmdline="${2-}" start="${3-}" newest="${4-}" argv1
  [ -n "$cmdline" ] || { printf 'unreadable'; return 1; }
  read -r _ argv1 _ <<<"$cmdline"
  [ "$argv1" = "$program" ] || { printf 'wrong-program'; return 1; }
  strih_session_app_start_state "$start" "$newest"
}

# strih_session_app_binary_state PROGRAM EXE START NEWEST -> ok | unreadable | wrong-program | stale, rc 0
# iff ok. The same question for an entry that runs a binary (the Electron Satellite): Chromium rewrites
# its process title, so argv proves nothing -- EXE is `readlink /proc/<MainPID>/exe` and must equal
# PROGRAM resolved (`readlink -f`); a binary replaced under the running process reads `<path> (deleted)`
# = wrong-program. START / NEWEST as for strih_session_app_process_state.
strih_session_app_binary_state() {
  local program="${1-}" exe="${2-}" start="${3-}" newest="${4-}"
  [ -n "$exe" ] || { printf 'unreadable'; return 1; }
  [ "$exe" = "$program" ] || { printf 'wrong-program'; return 1; }
  strih_session_app_start_state "$start" "$newest"
}

# strih_session_app_start_state START NEWEST -> ok | unreadable | stale, rc 0 iff ok: the main process
# started (unix seconds, a leading @ allowed) at or after the newest mtime of the installed files.
strih_session_app_start_state() {
  local start="${1#@}" newest="${2-}"
  if ! [[ "$start" =~ ^[0-9]+$ ]] || ! [[ "$newest" =~ ^[0-9]+$ ]]; then
    printf 'unreadable'
    return 1
  fi
  [ "$((10#$start))" -ge "$((10#$newest))" ] || { printf 'stale'; return 1; }
  printf 'ok'
}

# strih_session_app_last_run_state START NEWEST -> ok | never | unreadable | stale, rc 0 iff ok. A timer
# entry has no long-lived main process: START is its oneshot service's ExecMainStartTimestamp (the last
# run, unix seconds, a leading @ allowed; empty, n/a or 0 = it never ran), and the last run must have
# started at or after NEWEST, the newest mtime of the installed timer + service + program.
strih_session_app_last_run_state() {
  local start="${1#@}" newest="${2-}"
  if ! [[ "$start" =~ ^[0-9]+$ ]] || [ "$((10#$start))" = 0 ]; then
    printf 'never'
    return 1
  fi
  [[ "$newest" =~ ^[0-9]+$ ]] || { printf 'unreadable'; return 1; }
  [ "$((10#$start))" -ge "$((10#$newest))" ] || { printf 'stale'; return 1; }
  printf 'ok'
}

# strih_session_app_unit_verdict UNIT_STATE SCRIPT_STATE ENABLED AUTOSTART ACTIVE PROCESS -> ONE verdict
# token, rc 0 iff ok. UNIT_STATE / SCRIPT_STATE: ok | absent | differs; ENABLED / AUTOSTART: 1 | 0;
# ACTIVE: the `systemctl --user is-active` word (empty = unreadable); PROCESS: the
# strih_session_app_process_state token. Fail-closed order: unit-absent -> unit-differs -> script-absent
# -> script-differs -> not-enabled -> no-autostart -> not-active:<word> -> process:<token> -> ok.
strih_session_app_unit_verdict() {
  local unit="${1-}" script="${2-}" enabled="${3-}" autostart="${4-}" active="${5-}" process="${6-}"
  case "$unit" in ok) ;; absent) printf 'unit-absent'; return 1 ;; *) printf 'unit-differs'; return 1 ;; esac
  case "$script" in ok) ;; absent) printf 'script-absent'; return 1 ;; *) printf 'script-differs'; return 1 ;; esac
  [ "$enabled" = 1 ] || { printf 'not-enabled'; return 1; }
  [ "$autostart" = 1 ] || { printf 'no-autostart'; return 1; }
  [ "$active" = active ] || { printf 'not-active:%s' "${active:-unreadable}"; return 1; }
  [ "$process" = ok ] || { printf 'process:%s' "${process:-unreadable}"; return 1; }
  printf 'ok'
}

# strih_session_app_remedy TOKEN UNIT -> the remediation text for a failed unit verdict.
strih_session_app_remedy() {
  local unit="${2-}" script run
  script="$(strih_session_app_script "$unit" 2>/dev/null || true)"
  run="$(strih_session_app_run_unit "$unit")"
  case "${1-}" in
    script-*)
      if [ -z "$script" ]; then
        printf 're-run %s: it puts %s in place' "$(strih_session_app_binary_installer "$unit")" \
          "$(strih_session_app_program "$unit")"
      else
        printf 're-run setup-strih.sh step 16d (issue 1399)'
      fi ;;
    unit-*) printf 're-run setup-strih.sh step 16d (issue 1399)' ;;
    not-enabled) printf 'systemctl --user enable %s (or re-run setup-strih.sh step 16d)' "$unit" ;;
    no-autostart) printf 're-run setup-strih.sh step 15 (the kiosk openbox autostart starts it at login)' ;;
    process:stale|process:wrong-program)
      if [ "$run" != "$unit" ]; then
        printf 'the timer has not run %s since the files were installed: systemctl --user restart %s' "$run" "$unit"
      else
        printf 'the running process predates the installed files: systemctl --user restart %s' "$unit"
      fi ;;
    process:never)
      printf 'the timer never ran %s: systemctl --user restart %s; read journalctl --user -u %s' "$run" "$unit" "$run" ;;
    *) printf 'systemctl --user restart %s; read journalctl --user -u %s' "$unit" "$run" ;;
  esac
}

# strih_session_app_ok_text UNIT -> the OK row text of an entry (its program named as the box runs it).
strih_session_app_ok_text() {
  local unit="${1-}" script program
  script="$(strih_session_app_script "$unit")" || return 1
  program="$(strih_session_app_program "$unit")" || return 1
  if [ -z "$script" ]; then
    printf 'unit matches the checkout, %s installed, enabled, started by the kiosk autostart, active, its process runs %s and started after the unit was installed' "$program" "$program"
  elif [ "$(strih_session_app_run_unit "$unit")" != "$unit" ]; then
    printf 'units + %s match the checkout, enabled, started by the kiosk autostart, active, last ran the watch after the files were installed' "$program"
  else
    printf 'unit + %s match the checkout, enabled, started by the kiosk autostart, active, its process started after the files were installed' "$program"
  fi
}

# strih_session_app_process_facts USER UNIT -> `<exe>|<start>|<cmdline with spaces>` of the unit's main
# process, read in USER's session (each empty when unreadable; <exe> = readlink /proc/<pid>/exe). The
# free-form cmdline is LAST, so a "|" inside it never shifts the fixed fields. STRIH_SESSION_APPS_PROC is
# the /proc test seam.
strih_session_app_process_facts() {
  local user="$1" unit="$2" pid start cmd="" exe=""
  pid="$(strih_session_apps_user_systemctl "$user" show -p MainPID --value "$unit" 2>/dev/null || true)"
  start="$(strih_session_apps_user_systemctl "$user" show -p ExecMainStartTimestamp --value --timestamp=unix "$unit" 2>/dev/null || true)"
  pid="${pid%%$'\n'*}"
  if [[ "$pid" =~ ^[1-9][0-9]*$ ]]; then
    # the group carries the 2>/dev/null: a redirect fails before a command's own 2>/dev/null applies,
    # and a main process that exited between `show` and this read must not print on verify's stderr.
    cmd="$( { tr '\0' ' ' < "${STRIH_SESSION_APPS_PROC:-/proc}/${pid}/cmdline"; } 2>/dev/null || true)"
    exe="$(readlink "${STRIH_SESSION_APPS_PROC:-/proc}/${pid}/exe" 2>/dev/null || true)"
  fi
  printf '%s|%s|%s' "$exe" "${start%%$'\n'*}" "$cmd"
}

# strih_shading_window_present [CLASS] [TITLE] (stdin = `wmctrl -lx`) -> rc 0 iff a window has the panel
# app's WM_CLASS (field 3, `res_name.res_class`) AND the exact title (field 5 on). Reads ALL of stdin (no
# early exit -> no SIGPIPE upstream).
strih_shading_window_present() {
  awk -v cls="${1:-$STRIH_PANEL_WM_CLASS}" -v want="${2:-$STRIH_PANEL_WINDOW_TITLE}" '
    { t = ""; for (i = 5; i <= NF; i++) t = t (i > 5 ? " " : "") $i; if ($3 == cls && t == want) found = 1 }
    END { exit found ? 0 : 1 }'
}

# strih_kiosk_x_run EUID USER USER_HOME SECONDS CMD... -> CMD on the kiosk display :0 with USER's
# Xauthority (as root, EUID 0, through `sudo -u USER`; else inline), bounded by `timeout SECONDS`. The ONE
# copy for the session apps' window check and the operator grader (issue 1418); strih-drm-output.sh keeps
# its own xrandr reader because its tests source that lib alone.
strih_kiosk_x_run() {
  local euid="$1" user="$2" home="$3" t="$4"
  shift 4
  if [ "$euid" = 0 ]; then
    sudo -u "$user" env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout "$t" "$@"
  else
    env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout "$t" "$@"
  fi
}

# strih_session_apps_wmctrl USER USER_HOME -> `wmctrl -lx` of the kiosk display :0 with the operator's
# Xauthority (as root through sudo -u, else inline), bounded; prints nothing when X does not answer.
strih_session_apps_wmctrl() {
  local user="${1:?user required}" home="${2:?user home required}"
  strih_kiosk_x_run "${STRIH_SESSION_APPS_EUID:-$(id -u)}" "$user" "$home" 10 \
    "$(strih_session_apps_wmctrl_bin)" -lx 2>/dev/null || true
}

# strih_session_apps_wmctrl_bin -> the window-list tool (STRIH_SESSION_APPS_WMCTRL is the test seam).
strih_session_apps_wmctrl_bin() { printf '%s' "${STRIH_SESSION_APPS_WMCTRL:-wmctrl}"; }

# strih_session_app_pass_row ITEM SCRIPT STATE_FILE RUN_UNIT REPO USER -> ONE row `OK|text` / `FAIL|text`:
# the program's own `--check-state` on its state file in USER's runtime dir (the keeper's last pass, the
# Satellite watch's last pass). Always rc 0.
strih_session_app_pass_row() {
  local item="$1" script="$2" file="$3" run="$4" repo="$5" user="$6" state_file line rc=0
  if ! state_file="$(strih_session_apps_runtime_dir "$user")/${file}"; then
    printf 'FAIL|(%s-pass) cannot resolve the runtime dir of %s (no uid) -- the state file is unreadable\n' "$item" "$user"
    return 0
  fi
  line="$(python3 "${repo}/scripts/${script}" --check-state "$state_file" 2>&1)" || rc=$?
  line="${line%%$'\n'*}"
  if [ "$rc" = 0 ]; then
    printf 'OK|(%s-pass) %s\n' "$item" "$line"
  else
    printf 'FAIL|(%s-pass) %s -- read journalctl --user -u %s\n' "$item" "${line:-no verdict from ${script}}" "$run"
  fi
  return 0
}

# strih_session_apps_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|text` / `FAIL|text` /
# `NOTE|text` (only the dantesync tray below its first release):
# one unit row per session app, the keeper's last-pass row, the panel window row and the Satellite
# watch's last-pass row (strih_session_apps_row_count of them). Always rc 0.
strih_session_apps_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local unitdir u item script program run ustate sstate enabled autostart active process facts newest verdict
  local wm cmd exe start absent_note
  local -a files
  unitdir="${home}/.config/systemd/user"
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    item="$(strih_session_app_item "$u")" || continue
    script="$(strih_session_app_script "$u")" || continue
    program="$(strih_session_app_program "$u")" || continue
    run="$(strih_session_app_run_unit "$u")"
    mapfile -t files < <(strih_session_app_unit_files "$u")
    ustate="$(strih_session_app_units_state "$unitdir" "${repo}/systemd" "${files[@]}")"
    if [ -n "$script" ]; then
      sstate="$(strih_session_app_file_state "$program" "${repo}/scripts/${script}")"
    elif [ "$u" = "$STRIH_DANTESYNC_TRAY_UNIT" ]; then
      # issue 1429: the tray binary against the release record of the pin (absent / stale:<why> / ok)
      sstate="$(dantesync_install_tray_state || true)"
    else
      sstate=absent; [ -x "$program" ] && sstate=ok  # the /opt binary step 16 installs: present, no checkout copy
    fi
    enabled=0; [ -L "${unitdir}/${STRIH_SESSION_APP_WANTS_TARGET}.wants/${u}" ] && enabled=1
    autostart=0; grep -qxF "$(strih_session_app_autostart_line "$u")" "${home}/.config/openbox/autostart" 2>/dev/null && autostart=1
    # issue 1429: no tray binary while the pin has no Linux tray = a NOTE, never a FAIL, once the unit, its
    # enablement and its autostart line are in place (an installed binary is graded in full)
    if [ "$sstate" = absent ] && [ -z "$script" ] && absent_note="$(strih_session_app_absent_note "$u")"; then
      if verdict="$(strih_session_app_unit_verdict "$ustate" ok "$enabled" "$autostart" active ok)"; then
        printf 'NOTE|(%s) %s: %s\n' "$item" "$u" "$absent_note"
      else
        printf 'FAIL|(%s) %s: %s (unit=%s enabled=%s autostart=%s; the binary: %s) -- %s\n' "$item" "$u" "$verdict" \
          "$ustate" "$enabled" "$autostart" "$absent_note" "$(strih_session_app_remedy "$verdict" "$u")"
      fi
      continue
    fi
    active="$(strih_session_apps_user_systemctl "$user" is-active "$u" 2>/dev/null || true)"
    active="${active%%$'\n'*}"
    facts="$(strih_session_app_process_facts "$user" "$run")"
    exe="${facts%%|*}"; start="${facts#*|}"; cmd="${start#*|}"; start="${start%%|*}"
    newest="$(stat -c %Y "${files[@]/#/${unitdir}/}" "$program" 2>/dev/null | sort -n | tail -n 1 || true)"
    if [ "$run" != "$u" ]; then
      process="$(strih_session_app_last_run_state "$start" "$newest" || true)"
    elif [ -z "$script" ]; then
      process="$(strih_session_app_binary_state "$(readlink -f "$program" 2>/dev/null || printf '%s' "$program")" \
        "$exe" "$start" "$newest" || true)"
    else
      process="$(strih_session_app_process_state "$program" "$cmd" "$start" "$newest" || true)"
    fi
    if verdict="$(strih_session_app_unit_verdict "$ustate" "$sstate" "$enabled" "$autostart" "$active" "$process")"; then
      printf 'OK|(%s) %s: %s\n' "$item" "$u" "$(strih_session_app_ok_text "$u")"
    else
      printf 'FAIL|(%s) %s: %s (unit=%s program=%s enabled=%s autostart=%s active=%s process=%s) -- %s\n' "$item" "$u" "$verdict" \
        "$ustate" "$sstate" "$enabled" "$autostart" "${active:-unreadable}" "$process" "$(strih_session_app_remedy "$verdict" "$u")"
    fi
  done

  # issue 1412: each extra row only for an entry the box runs (a stream box runs the keeper + the
  # dantesync tray, issue 1429; the tray has no extra row).
  if _strih_session_apps_has strih-browser-keeper.service; then
    strih_session_app_pass_row browser-keeper strih_browser_keeper.py "$STRIH_BROWSER_KEEPER_STATE_FILE" \
      strih-browser-keeper.service "$repo" "$user"
  fi

  if ! _strih_session_apps_has bkshading-panel-app.service; then
    :
  elif ! command -v "$(strih_session_apps_wmctrl_bin)" >/dev/null 2>&1; then
    printf 'FAIL|(shading-app-window) wmctrl missing -- re-run setup-strih.sh step 11 (the shared kiosk installs it)\n'
  else
    wm="$(strih_session_apps_wmctrl "$user" "$home")"
    if [ -z "$wm" ]; then
      printf 'FAIL|(shading-app-window) the kiosk display :0 listed no window (X not answering for %s) -- is the operator session up?\n' "$user"
    elif printf '%s\n' "$wm" | strih_shading_window_present "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"; then
      printf 'OK|(shading-app-window) the panel window (%s, "%s") is open on :0\n' "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"
    else
      printf 'FAIL|(shading-app-window) no panel window (%s, "%s") on :0 -- systemctl --user restart bkshading-panel-app.service; read journalctl --user -u bkshading-panel-app.service\n' "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"
    fi
  fi

  if _strih_session_apps_has strih-satellite-watch.timer; then
    strih_session_app_pass_row satellite-watch strih_satellite_watch.py "$STRIH_SATELLITE_WATCH_STATE_FILE" \
      strih-satellite-watch.service "$repo" "$user"
  fi

  # issue 1424: the display watch's last pass (screen, operator outputs, fixed outputs kept out of X)
  if _strih_session_apps_has "$STRIH_DISPLAY_WATCH_UNIT"; then
    strih_session_app_pass_row display-watch strih_display_watch.py "$STRIH_DISPLAY_WATCH_STATE_FILE" \
      "$STRIH_DISPLAY_WATCH_UNIT" "$repo" "$user"
    # issue 1424 layer 1: the X config that keeps the fixed HDMI outputs out of X (setup-strih step 6)
    strih_x_display_grade_row "$repo" "$home" "$user"
  fi

  # issue 1436: the stream role's headphone monitor -- its last pass + card owner, the OBS key, Monitor Off
  if _strih_session_apps_has "$STRIH_STREAM_MONITOR_UNIT"; then
    strih_stream_monitor_grade_rows "$repo" "$home" "$user"
  fi
  return 0
}

# _strih_session_apps_has UNIT -> 0 when UNIT is one of STRIH_SESSION_APP_UNITS (issue 1412: the stream
# role narrows the list to the keeper + the dantesync tray).
_strih_session_apps_has() {
  local u
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    [ "$u" = "${1-}" ] && return 0
  done
  return 1
}

# strih_session_apps_row_count -> how many rows strih_session_apps_grade_rows prints (one per unit +
# the keeper's last pass + the panel window + the Satellite watch's last pass + the display watch's last
# pass and its X config row (issue 1424) + the stream monitor's
# STRIH_STREAM_MONITOR_ROWS).
strih_session_apps_row_count() {
  local n="${#STRIH_SESSION_APP_UNITS[@]}" u
  for u in strih-browser-keeper.service bkshading-panel-app.service strih-satellite-watch.timer "$STRIH_DISPLAY_WATCH_UNIT"; do
    _strih_session_apps_has "$u" && n=$((n + 1))
  done
  _strih_session_apps_has "$STRIH_DISPLAY_WATCH_UNIT" && n=$((n + 1))  # issue 1424: + its X config row
  _strih_session_apps_has "$STRIH_STREAM_MONITOR_UNIT" && n=$((n + STRIH_STREAM_MONITOR_ROWS))
  printf '%s' "$n"
}

# strih_session_apps_grade_report REPO USER_HOME DESKTOP_USER -> print the rows through the CALLER's ok /
# bad / note functions (verify-strih; a NOTE row is the dantesync tray waiting for its release, issue
# 1429). rc 1 unless the grader printed every row: a grader that stopped part way is a FAIL, never a
# quietly shorter list.
strih_session_apps_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_session_apps_grade_rows "$@")
  [ "$rows" = "$(strih_session_apps_row_count)" ]
}
