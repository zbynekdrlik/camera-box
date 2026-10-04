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
#     once per OBS run (a real OBS restart, never a keeper restart or a WS reconnect) as soon as its page
#     server answers, and again when the page server comes back; never a working page.
#   * bkshading-panel-app.service -> scripts/bkshading_panel_app.py: the bkshading panel (:8770) in a
#     WebKitGTK window titled "Shading" -- a NORMAL window that openbox Alt+Tab reaches.
#
# Both units are WantedBy=graphical-session.target like strih-obs.service, and like it they are STARTED
# by the kiosk openbox autostart (strih_openbox_autostart_text calls strih_session_apps_autostart_lines):
# openbox never reaches graphical-session.target, and a default.target unit would also start on a
# user-manager start with no X session (an ssh login), where the panel window cannot open. ONE login
# start path, the one OBS uses; the strih-lx genlock deploy also starts both after strih-obs, into the
# running session (scripts/lib/strih-lx-deploy.sh strih_lx_remote_start_cmd).
#
#   * strih_session_apps_install -- setup-strih step 16d: the packages (python3-gi + WebKit2 4.1 +
#     python3-websocket) and an import preflight, both programs into the bin dir, both units into the
#     operator's ~/.config/systemd/user (each written only when its bytes or mode differ), the
#     4.10.2026 hand-made panel stopgap removed (its WantedBy=default.target link + its ~/.local/bin
#     copy; the unit file itself is overwritten under the same name), `systemctl --user enable`, and a
#     `try-restart` of the units whose files changed -- a stopped unit is never started (enable-only,
#     the kiosk pattern), a running one is moved onto the new files.
#   * strih_session_apps_grade_rows / _report -- verify-strih items 37 (keeper) + 38 (panel): each unit
#     installed byte-identical to the checkout with its program, enabled, started by the autostart,
#     active, and its main process running the installed program and started after the files were
#     written; the keeper's last pass recent and connected; the panel window (its WM_CLASS + the title
#     "Shading") on :0.
#
# Test seams (tests/python/test_strih_session_apps_1399.py): STRIH_SESSION_APPS_BIN_DIR (/usr/local/bin),
# STRIH_SESSION_APPS_RUNTIME_DIR (/run/user/<uid>), STRIH_SESSION_APPS_EUID (the running uid),
# STRIH_SESSION_APPS_PYTHON (/usr/bin/python3, the import preflight), STRIH_SESSION_APPS_WMCTRL
# (wmctrl), STRIH_SESSION_APPS_PROC (/proc); `dpkg-query`, `apt-get`, `sudo`, `systemctl` and `id` are
# looked up on PATH.

# unit -> program pairs (the program is installed into the bin dir; the unit's ExecStart runs it there).
STRIH_SESSION_APP_UNITS=(strih-browser-keeper.service bkshading-panel-app.service)
STRIH_SESSION_APP_PACKAGES=(python3-gi gir1.2-webkit2-4.1 python3-websocket)
STRIH_SESSION_APP_WANTS_TARGET=graphical-session.target
STRIH_PANEL_WINDOW_TITLE=Shading
# `res_name.res_class` of the panel window as `wmctrl -lx` prints it: GDK takes both from the program
# name the app sets (GLib.set_prgname, PRGNAME in scripts/bkshading_panel_app.py; read live under Xvfb).
STRIH_PANEL_WM_CLASS=bkshading-panel-app.Bkshading-panel-app
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
  local bindir unitdir u script missing py w1 w2 err unit_written
  local -a changed
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
  changed=()
  unit_written=0
  for u in "${STRIH_SESSION_APP_UNITS[@]}"; do
    script="$(strih_session_app_script "$u")" || return 1
    w1="$(strih_session_apps_put "${repo}/scripts/${script}" "${bindir}/${script}" 0755)" || return 1
    w2="$(strih_session_apps_put "${repo}/systemd/${u}" "${unitdir}/${u}" 0644)" || return 1
    [ "$w2" = written ] && unit_written=1
    if [ "$w1" = written ] || [ "$w2" = written ]; then
      changed+=("$u")
      echo "  installed ${u} -> ${unitdir}/${u} (runs ${bindir}/${script}): program ${w1}, unit ${w2}"
    else
      echo "  ${u} + ${bindir}/${script} unchanged"
    fi
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

  if ! err="$(strih_session_apps_user_systemctl "$user" enable "${STRIH_SESSION_APP_UNITS[@]}" 2>&1)"; then
    # No user bus = no running unit either: nothing to reload or restart.
    echo "  WARN: systemctl --user enable failed for ${user} (${err//$'\n'/ }) -- enable by hand once logged in: systemctl --user enable ${STRIH_SESSION_APP_UNITS[*]} (the autostart starts both regardless)"
    return 0
  fi
  echo "  enabled ${STRIH_SESSION_APP_UNITS[*]} (a stopped unit is NOT started: the kiosk openbox autostart starts both at every login)"
  # A RUNNING unit whose program or unit changed is restarted, so the new code runs (try-restart never
  # starts a stopped unit: still enable-only). A written unit file is reloaded explicitly first, so a
  # restart never runs a cached old definition. In the genlock deploy OBS is stopped here, so a keeper
  # restart costs no extra refresh.
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

# strih_session_app_process_state PROGRAM CMDLINE START NEWEST -> ok | unreadable | wrong-program |
# stale, rc 0 iff ok. Whether the unit's RUNNING process runs the installed files: CMDLINE is the
# process argv with spaces for the NULs (/proc/<MainPID>/cmdline), its second word must be PROGRAM
# (`/usr/bin/python3 <PROGRAM> ...`); START is the unit's ExecMainStartTimestamp in unix seconds (a
# leading @ allowed); NEWEST is the newest mtime of the installed program + unit. A process started
# before the files were last written still runs the old code (stale) -- an installed-but-not-running
# change must never read as live.
strih_session_app_process_state() {
  local program="${1-}" cmdline="${2-}" start="${3#@}" newest="${4-}" argv1
  [ -n "$cmdline" ] || { printf 'unreadable'; return 1; }
  read -r _ argv1 _ <<<"$cmdline"
  [ "$argv1" = "$program" ] || { printf 'wrong-program'; return 1; }
  if ! [[ "$start" =~ ^[0-9]+$ ]] || ! [[ "$newest" =~ ^[0-9]+$ ]]; then
    printf 'unreadable'
    return 1
  fi
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
  case "${1-}" in
    unit-*|script-*) printf 're-run setup-strih.sh step 16d (issue 1399)' ;;
    not-enabled) printf 'systemctl --user enable %s (or re-run setup-strih.sh step 16d)' "${2-}" ;;
    no-autostart) printf 're-run setup-strih.sh step 15 (the kiosk openbox autostart starts it at login)' ;;
    process:stale|process:wrong-program)
      printf 'the running process predates the installed files: systemctl --user restart %s' "${2-}" ;;
    *) printf 'systemctl --user restart %s; read journalctl --user -u %s' "${2-}" "${2-}" ;;
  esac
}

# strih_session_app_process_facts USER UNIT -> `<cmdline with spaces>|<start>` of the unit's main process,
# read in USER's session (both empty when unreadable). STRIH_SESSION_APPS_PROC is the /proc test seam.
strih_session_app_process_facts() {
  local user="$1" unit="$2" pid start cmd=""
  pid="$(strih_session_apps_user_systemctl "$user" show -p MainPID --value "$unit" 2>/dev/null || true)"
  start="$(strih_session_apps_user_systemctl "$user" show -p ExecMainStartTimestamp --value --timestamp=unix "$unit" 2>/dev/null || true)"
  if [[ "${pid%%$'\n'*}" =~ ^[1-9][0-9]*$ ]]; then
    # the group carries the 2>/dev/null: a redirect fails before a command's own 2>/dev/null applies,
    # and a main process that exited between `show` and this read must not print on verify's stderr.
    cmd="$( { tr '\0' ' ' < "${STRIH_SESSION_APPS_PROC:-/proc}/${pid%%$'\n'*}/cmdline"; } 2>/dev/null || true)"
  fi
  printf '%s|%s' "$cmd" "${start%%$'\n'*}"
}

# strih_shading_window_present [CLASS] [TITLE] (stdin = `wmctrl -lx`) -> rc 0 iff a window has the panel
# app's WM_CLASS (field 3, `res_name.res_class`) AND the exact title (field 5 on). Reads ALL of stdin (no
# early exit -> no SIGPIPE upstream).
strih_shading_window_present() {
  awk -v cls="${1:-$STRIH_PANEL_WM_CLASS}" -v want="${2:-$STRIH_PANEL_WINDOW_TITLE}" '
    { t = ""; for (i = 5; i <= NF; i++) t = t (i > 5 ? " " : "") $i; if ($3 == cls && t == want) found = 1 }
    END { exit found ? 0 : 1 }'
}

# strih_session_apps_wmctrl USER USER_HOME -> `wmctrl -lx` of the kiosk display :0 with the operator's
# Xauthority (as root through sudo -u, else inline), bounded; prints nothing when X does not answer.
strih_session_apps_wmctrl() {
  local user="${1:?user required}" home="${2:?user home required}" wm
  wm="$(strih_session_apps_wmctrl_bin)"
  if [ "${STRIH_SESSION_APPS_EUID:-$(id -u)}" = 0 ]; then
    sudo -u "$user" env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout 10 "$wm" -lx 2>/dev/null || true
  else
    env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout 10 "$wm" -lx 2>/dev/null || true
  fi
}

# strih_session_apps_wmctrl_bin -> the window-list tool (STRIH_SESSION_APPS_WMCTRL is the test seam).
strih_session_apps_wmctrl_bin() { printf '%s' "${STRIH_SESSION_APPS_WMCTRL:-wmctrl}"; }

# strih_session_apps_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|text` / `FAIL|text`:
# one unit row per session app, the keeper's last-pass row and the panel window row
# (strih_session_apps_row_count of them). Always rc 0.
strih_session_apps_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local bindir unitdir u item script ustate sstate enabled autostart active process facts newest verdict
  local state_file line rc wm
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
    facts="$(strih_session_app_process_facts "$user" "$u")"
    newest="$(stat -c %Y "${unitdir}/${u}" "${bindir}/${script}" 2>/dev/null | sort -n | tail -n 1 || true)"
    process="$(strih_session_app_process_state "${bindir}/${script}" "${facts%|*}" "${facts##*|}" "$newest" || true)"
    if verdict="$(strih_session_app_unit_verdict "$ustate" "$sstate" "$enabled" "$autostart" "$active" "$process")"; then
      printf 'OK|(%s) %s: unit + %s match the checkout, enabled, started by the kiosk autostart, active, its process started after the files were installed\n' "$item" "$u" "${bindir}/${script}"
    else
      printf 'FAIL|(%s) %s: %s (unit=%s program=%s enabled=%s autostart=%s active=%s process=%s) -- %s\n' "$item" "$u" "$verdict" \
        "$ustate" "$sstate" "$enabled" "$autostart" "${active:-unreadable}" "$process" "$(strih_session_app_remedy "$verdict" "$u")"
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
    elif printf '%s\n' "$wm" | strih_shading_window_present "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"; then
      printf 'OK|(shading-app-window) the panel window (%s, "%s") is open on :0\n' "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"
    else
      printf 'FAIL|(shading-app-window) no panel window (%s, "%s") on :0 -- systemctl --user restart bkshading-panel-app.service; read journalctl --user -u bkshading-panel-app.service\n' "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"
    fi
  fi
  return 0
}

# strih_session_apps_row_count -> how many rows strih_session_apps_grade_rows prints (one per unit +
# the keeper's last pass + the panel window).
strih_session_apps_row_count() { printf '%s' "$(( ${#STRIH_SESSION_APP_UNITS[@]} + 2 ))"; }

# strih_session_apps_grade_report REPO USER_HOME DESKTOP_USER -> print the rows through the CALLER's ok /
# bad functions (verify-strih). rc 1 unless the grader printed every row: a grader that stopped part way
# is a FAIL, never a quietly shorter list.
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
  [ "$rows" = "$(strih_session_apps_row_count)" ]
}
