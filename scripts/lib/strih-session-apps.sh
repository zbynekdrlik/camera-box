#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih session-app constants, the setup-strih step 16d
# install and the verify-strih items 37/38 grader; no top-level statements besides constants) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. setup-strih.sh / verify-strih.sh set their own.
# Every function checks its own return codes, because a caller runs it in an `||` list, where errexit
# is off.
#
# scripts/lib/strih-session-apps.sh -- issue 1399 (owner 4.10.2026: "toto je produkcny pocitac vsetko ma
# bezat vzdy a stale"): two supervised --user units in the strih-lx operator session, beside
# strih-obs.service (design comment 5977447339, Approach 1):
#
#   * strih-browser-keeper.service -> scripts/strih_browser_keeper.py: refreshes each OBS browser source
#     once after every obs-websocket (re)connect as soon as its page server answers, and again when the
#     page server comes back; never a working page.
#   * bkshading-panel-app.service -> scripts/bkshading_panel_app.py: the bkshading panel (:8770) in a
#     WebKitGTK window titled "Shading" -- a NORMAL window that openbox Alt+Tab reaches.
#
# Both units are WantedBy=graphical-session.target like strih-obs.service, and like it they are STARTED
# by the kiosk openbox autostart (strih_openbox_autostart_text calls strih_session_apps_autostart_lines):
# openbox never reaches graphical-session.target, and a default.target unit would also start on a
# user-manager start with no X session (an ssh login), where the panel window cannot open. ONE start
# path, the one OBS uses.
#
#   * strih_session_apps_install -- setup-strih step 16d: the packages (python3-gi + WebKit2 4.1 +
#     python3-websocket) and an import preflight, both programs into the bin dir, both units into the
#     operator's ~/.config/systemd/user, the 4.10.2026 hand-made panel stopgap removed (its
#     WantedBy=default.target link + its ~/.local/bin copy; the unit file itself is overwritten under
#     the same name), `systemctl --user enable` -- never a start (enable-only, the kiosk pattern).
#   * strih_session_apps_grade_rows / _report -- verify-strih items 37 (keeper) + 38 (panel): each unit
#     installed byte-identical to the checkout with its program, enabled, started by the autostart and
#     active; the keeper's last pass recent and connected; a window titled "Shading" on :0.
#
# Test seams (tests/python/test_strih_session_apps_1399.py): STRIH_SESSION_APPS_BIN_DIR (/usr/local/bin),
# STRIH_SESSION_APPS_RUNTIME_DIR (/run/user/<uid>), STRIH_SESSION_APPS_EUID (the running uid),
# STRIH_SESSION_APPS_PYTHON (/usr/bin/python3, the import preflight), STRIH_SESSION_APPS_WMCTRL
# (wmctrl); `dpkg-query`, `apt-get`, `sudo`, `systemctl` and `id` are looked up on PATH.

# unit -> program pairs (the program is installed into the bin dir; the unit's ExecStart runs it there).
STRIH_SESSION_APP_UNITS=(strih-browser-keeper.service bkshading-panel-app.service)
STRIH_SESSION_APP_PACKAGES=(python3-gi gir1.2-webkit2-4.1 python3-websocket)
STRIH_SESSION_APP_WANTS_TARGET=graphical-session.target
STRIH_PANEL_WINDOW_TITLE=Shading
# The keeper's state file, under the user's runtime dir (the unit passes --state-file %t/<this>).
STRIH_BROWSER_KEEPER_STATE_FILE=strih-browser-keeper.json
# The 4.10.2026 hand-made stopgap (issue comment 5977460330): unit WantedBy=default.target + a
# ~/.local/bin copy of the app. Paths relative to the operator's home.
STRIH_PANEL_STOPGAP_WANTS=.config/systemd/user/default.target.wants/bkshading-panel-app.service
STRIH_PANEL_STOPGAP_BIN=.local/bin/bkshading-panel-app

# --- constants + path seams ------------------------------------------------------------------------

# strih_session_app_script UNIT -> the program the unit runs (rc 1 for an unknown unit).
strih_session_app_script() {
  case "${1-}" in
    strih-browser-keeper.service) printf 'strih_browser_keeper.py' ;;
    bkshading-panel-app.service) printf 'bkshading_panel_app.py' ;;
    *) return 1 ;;
  esac
}

# strih_session_app_item UNIT -> the verify-strih row label of the unit's item.
strih_session_app_item() {
  case "${1-}" in
    strih-browser-keeper.service) printf 'browser-keeper' ;;
    bkshading-panel-app.service) printf 'shading-app' ;;
    *) return 1 ;;
  esac
}

strih_session_apps_bin_dir() { printf '%s' "${STRIH_SESSION_APPS_BIN_DIR:-/usr/local/bin}"; }

# strih_session_apps_autostart_lines -> the kiosk openbox autostart lines that START both units at every
# login, one per unit (a unit that is not installed yet never blocks the next line).
strih_session_apps_autostart_lines() {
  local u
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    printf 'systemctl --user start %s || true\n' "$u"
  done
}

# strih_session_apps_missing_packages -> each package of STRIH_SESSION_APP_PACKAGES that dpkg does not
# report "install ok installed", one per line. Always rc 0.
strih_session_apps_missing_packages() {
  local p
  for p in "${STRIH_SESSION_APP_PACKAGES[@]}"; do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] || printf '%s\n' "$p"
  done
  return 0
}

# strih_session_apps_import_check -> the python the import preflight runs: everything the two programs
# import at runtime (the keeper: websocket; the panel: Gtk 3 + WebKit2 4.1 through gi).
strih_session_apps_import_check() {
  printf '%s' 'import websocket, gi; gi.require_version("Gtk", "3.0"); gi.require_version("WebKit2", "4.1"); from gi.repository import GLib, Gtk, WebKit2'
}

# strih_session_apps_user_systemctl USER ARGS... -> `systemctl --user ARGS` in USER's session: as root
# through `sudo -u USER XDG_RUNTIME_DIR=/run/user/<uid>` (setup-strih and the step-17 verify run as root,
# whose own --user manager is not the operator's), else inline. Returns systemctl's rc.
strih_session_apps_user_systemctl() {
  local user="${1:?user required}" uid
  shift
  if [ "${STRIH_SESSION_APPS_EUID:-$(id -u)}" = 0 ]; then
    uid="$(id -u "$user" 2>/dev/null)" || return 1
    sudo -u "$user" XDG_RUNTIME_DIR="/run/user/${uid}" systemctl --user "$@"
  else
    systemctl --user "$@"
  fi
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

# strih_session_apps_install REPO USER_HOME DESKTOP_USER -> install both session apps (see the header).
# rc 1 + a stderr line on a missing source file, a package that will not install, a failed import
# preflight or a failed file install; a `systemctl --user enable` that cannot reach the user bus (a box
# provisioned with nobody logged in) only WARNs -- the autostart starts the units regardless.
strih_session_apps_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local bindir unitdir u script missing py
  bindir="$(strih_session_apps_bin_dir)"
  unitdir="${home}/.config/systemd/user"
  py="${STRIH_SESSION_APPS_PYTHON:-/usr/bin/python3}"
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    script="$(strih_session_app_script "$u")" || return 1
    [ -f "${repo}/systemd/${u}" ] || { echo "strih-session-apps: systemd/${u} not found under ${repo}" >&2; return 1; }
    [ -f "${repo}/scripts/${script}" ] || { echo "strih-session-apps: scripts/${script} not found under ${repo}" >&2; return 1; }
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
  echo "  packages present + importable: ${STRIH_SESSION_APP_PACKAGES[*]}"

  install -d -m 0755 "$bindir" || { echo "strih-session-apps: cannot create ${bindir}" >&2; return 1; }
  install -d -m 0755 "$unitdir" || { echo "strih-session-apps: cannot create ${unitdir}" >&2; return 1; }
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    script="$(strih_session_app_script "$u")" || return 1
    install -m 0755 "${repo}/scripts/${script}" "${bindir}/${script}" || {
      echo "strih-session-apps: cannot install ${bindir}/${script}" >&2; return 1; }
    install -m 0644 "${repo}/systemd/${u}" "${unitdir}/${u}" || {
      echo "strih-session-apps: cannot install ${unitdir}/${u}" >&2; return 1; }
    echo "  installed ${u} -> ${unitdir}/${u} (runs ${bindir}/${script})"
  done

  # The stopgap's WantedBy=default.target link would also start the panel on a non-graphical login.
  if [ -L "${home}/${STRIH_PANEL_STOPGAP_WANTS}" ] || [ -e "${home}/${STRIH_PANEL_STOPGAP_WANTS}" ]; then
    rm -f "${home}/${STRIH_PANEL_STOPGAP_WANTS}" || { echo "strih-session-apps: cannot remove ${home}/${STRIH_PANEL_STOPGAP_WANTS}" >&2; return 1; }
    echo "  removed the 4.10.2026 stopgap link ~/${STRIH_PANEL_STOPGAP_WANTS} (the panel starts from the kiosk autostart)"
  fi
  if [ -e "${home}/${STRIH_PANEL_STOPGAP_BIN}" ]; then
    rm -f "${home}/${STRIH_PANEL_STOPGAP_BIN}" || { echo "strih-session-apps: cannot remove ${home}/${STRIH_PANEL_STOPGAP_BIN}" >&2; return 1; }
    echo "  removed the stopgap copy ~/${STRIH_PANEL_STOPGAP_BIN} (the unit runs ${bindir}/bkshading_panel_app.py)"
  fi
  chown -R "${user}:${user}" "$unitdir" 2>/dev/null || echo "  WARN: could not chown ${unitdir} to ${user}"

  if strih_session_apps_user_systemctl "$user" enable "${STRIH_SESSION_APP_UNITS[@]}" >/dev/null 2>&1; then
    echo "  enabled ${STRIH_SESSION_APP_UNITS[*]} (NOT started now: the kiosk openbox autostart starts both at every login)"
  else
    echo "  WARN: could not reach ${user}'s user bus -- enable by hand once logged in: systemctl --user enable ${STRIH_SESSION_APP_UNITS[*]} (the autostart starts both regardless)"
  fi
  return 0
}

# --- verify-strih items 37 + 38 ------------------------------------------------------------------------

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

# strih_session_app_unit_verdict UNIT_STATE SCRIPT_STATE ENABLED AUTOSTART ACTIVE -> ONE verdict token,
# rc 0 iff ok. UNIT_STATE / SCRIPT_STATE: ok | absent | differs; ENABLED / AUTOSTART: 1 | 0; ACTIVE: the
# `systemctl --user is-active` word (empty = unreadable). Fail-closed order: unit-absent -> unit-differs
# -> script-absent -> script-differs -> not-enabled -> no-autostart -> not-active:<word> -> ok.
strih_session_app_unit_verdict() {
  local unit="${1-}" script="${2-}" enabled="${3-}" autostart="${4-}" active="${5-}"
  case "$unit" in ok) ;; absent) printf 'unit-absent'; return 1 ;; *) printf 'unit-differs'; return 1 ;; esac
  case "$script" in ok) ;; absent) printf 'script-absent'; return 1 ;; *) printf 'script-differs'; return 1 ;; esac
  [ "$enabled" = 1 ] || { printf 'not-enabled'; return 1; }
  [ "$autostart" = 1 ] || { printf 'no-autostart'; return 1; }
  [ "$active" = active ] || { printf 'not-active:%s' "${active:-unreadable}"; return 1; }
  printf 'ok'
}

# strih_session_app_remedy TOKEN UNIT -> the remediation text for a failed unit verdict.
strih_session_app_remedy() {
  case "${1-}" in
    unit-*|script-*) printf 're-run setup-strih.sh step 16d (issue 1399)' ;;
    not-enabled) printf 'systemctl --user enable %s (or re-run setup-strih.sh step 16d)' "${2-}" ;;
    no-autostart) printf 're-run setup-strih.sh step 15 (the kiosk openbox autostart starts it at login)' ;;
    *) printf 'systemctl --user restart %s; read journalctl --user -u %s' "${2-}" "${2-}" ;;
  esac
}

# strih_shading_window_present [TITLE] (stdin = `wmctrl -l`) -> rc 0 iff a window's title (field 4 on)
# is exactly TITLE (default "Shading"). Reads ALL of stdin (no early exit -> no SIGPIPE upstream).
strih_shading_window_present() {
  awk -v want="${1:-$STRIH_PANEL_WINDOW_TITLE}" '
    { t = ""; for (i = 4; i <= NF; i++) t = t (i > 4 ? " " : "") $i; if (t == want) found = 1 }
    END { exit found ? 0 : 1 }'
}

# strih_session_apps_wmctrl USER USER_HOME -> `wmctrl -l` of the kiosk display :0 with the operator's
# Xauthority (as root through sudo -u, else inline), bounded; prints nothing when X does not answer.
strih_session_apps_wmctrl() {
  local user="${1:?user required}" home="${2:?user home required}" wm
  wm="$(strih_session_apps_wmctrl_bin)"
  if [ "${STRIH_SESSION_APPS_EUID:-$(id -u)}" = 0 ]; then
    sudo -u "$user" env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout 10 "$wm" -l 2>/dev/null || true
  else
    env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout 10 "$wm" -l 2>/dev/null || true
  fi
}

# strih_session_apps_wmctrl_bin -> the window-list tool (STRIH_SESSION_APPS_WMCTRL is the test seam).
strih_session_apps_wmctrl_bin() { printf '%s' "${STRIH_SESSION_APPS_WMCTRL:-wmctrl}"; }

# strih_session_apps_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|text` / `FAIL|text`:
# one unit row per session app, the keeper's last-pass row and the panel window row. Always rc 0.
strih_session_apps_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local bindir unitdir u item script ustate sstate enabled autostart active verdict state_file line rc wm
  bindir="$(strih_session_apps_bin_dir)"
  unitdir="${home}/.config/systemd/user"
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    item="$(strih_session_app_item "$u")" || continue
    script="$(strih_session_app_script "$u")" || continue
    ustate="$(strih_session_app_file_state "${unitdir}/${u}" "${repo}/systemd/${u}")"
    sstate="$(strih_session_app_file_state "${bindir}/${script}" "${repo}/scripts/${script}")"
    enabled=0; [ -L "${unitdir}/${STRIH_SESSION_APP_WANTS_TARGET}.wants/${u}" ] && enabled=1
    autostart=0; grep -qxF "systemctl --user start ${u} || true" "${home}/.config/openbox/autostart" 2>/dev/null && autostart=1
    active="$(strih_session_apps_user_systemctl "$user" is-active "$u" 2>/dev/null || true)"
    active="${active%%$'\n'*}"
    if verdict="$(strih_session_app_unit_verdict "$ustate" "$sstate" "$enabled" "$autostart" "$active")"; then
      printf 'OK|(%s) %s: unit + %s match the checkout, enabled, started by the kiosk autostart, active\n' "$item" "$u" "${bindir}/${script}"
    else
      printf 'FAIL|(%s) %s: %s (unit=%s program=%s enabled=%s autostart=%s active=%s) -- %s\n' "$item" "$u" "$verdict" \
        "$ustate" "$sstate" "$enabled" "$autostart" "${active:-unreadable}" "$(strih_session_app_remedy "$verdict" "$u")"
    fi
  done

  if state_file="$(strih_session_apps_runtime_dir "$user")/${STRIH_BROWSER_KEEPER_STATE_FILE}"; then
    rc=0
    line="$(python3 "${repo}/scripts/strih_browser_keeper.py" --check-state "$state_file" 2>&1)" || rc=$?
    line="${line%%$'\n'*}"
    if [ "$rc" = 0 ]; then
      printf 'OK|(browser-keeper-pass) %s\n' "$line"
    else
      printf 'FAIL|(browser-keeper-pass) %s -- read journalctl --user -u strih-browser-keeper.service\n' "${line:-no verdict from the keeper}"
    fi
  else
    printf 'FAIL|(browser-keeper-pass) cannot resolve the runtime dir of %s (no uid) -- the keeper state is unreadable\n' "$user"
  fi

  if ! command -v "$(strih_session_apps_wmctrl_bin)" >/dev/null 2>&1; then
    printf 'FAIL|(shading-app-window) wmctrl missing -- re-run setup-strih.sh step 11 (the shared kiosk installs it)\n'
  else
    wm="$(strih_session_apps_wmctrl "$user" "$home")"
    if [ -z "$wm" ]; then
      printf 'FAIL|(shading-app-window) the kiosk display :0 listed no window (X not answering for %s) -- is the operator session up?\n' "$user"
    elif printf '%s\n' "$wm" | strih_shading_window_present "$STRIH_PANEL_WINDOW_TITLE"; then
      printf 'OK|(shading-app-window) a window titled "%s" is open on :0\n' "$STRIH_PANEL_WINDOW_TITLE"
    else
      printf 'FAIL|(shading-app-window) no window titled "%s" on :0 -- systemctl --user restart bkshading-panel-app.service; read journalctl --user -u bkshading-panel-app.service\n' "$STRIH_PANEL_WINDOW_TITLE"
    fi
  fi
  return 0
}

# strih_session_apps_grade_report REPO USER_HOME DESKTOP_USER -> print the rows through the CALLER's ok /
# bad functions (verify-strih). rc 1 when the grader printed no row at all -- a silent grader is a FAIL.
strih_session_apps_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_session_apps_grade_rows "$@")
  [ "$rows" -gt 0 ]
}
