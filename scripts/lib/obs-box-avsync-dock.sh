#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no top-level statements): sourcing runs in
# the CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-avsync-dock.sh -- issue 1470: the camera-box A/V-sync dock (vendor/av-sync-dock:
# the lock / offset / latency display, the `mbc` ASRC readout, the diag + `LOCK-CORRECT SUGGESTED` lines
# the av-step watchdog reads) on the Linux OBS boxes. The Windows bundle always carried it; the Linux
# strih-variant bundle (linux-genlock.yml) builds + stages it since issue 1470, so setup-strih step 4's
# bundle install puts it next to obs-vban.so -- no install path of its own.
#
# The dock reads the measurement audio on the STREAM box (CAMERA_BOX_MEASURE_SOURCE_NAME "mbc"), so the
# items below run on the stream role only (`obs_box_step_applies avsync-dock`, scripts/lib/obs-box-role.sh);
# the strih role loads the same plugin from the same bundle and grades nothing here.
#
# setup-strih step 16g: obs_box_avsync_dock_seed USER_INI DESKTOP_USER. OBS hides a dock a plugin adds
#   (OBSStudioAPI.cpp obs_frontend_add_dock_by_id: setVisible(false) + setFloating(true)) and the saved
#   [BasicWindow] DockState, restored after the plugin load, decides. scripts/obs_dock_state.py seeds the
#   dock visible + docked right when that state has no entry for it, or holds it in OBS's own hidden +
#   floating default; an operator's placement (visible, or closed while docked) is kept. Never while OBS
#   runs (OBS rewrites user.ini from memory on exit), never over an unreadable file. WARN-only, rc 0.
# verify-strih item 47: obs_box_avsync_dock_grade_report LIBDIR SHAREDIR LOG USER_INI -> 3 rows:
#   (avsync-dock)         the plugin .so + its locale in the /usr prefix OBS loads;
#   (avsync-dock-loaded)  the newest OBS log's `Loaded Modules:` list names the .so (a running OBS keeps
#                         the plugin set it started with); NOTE with no log / no list yet;
#   (avsync-dock-layout)  the saved DockState holds the dock visible; FAIL absent or in OBS's hidden
#                         default; NOTE when the operator closed it docked.
#
# Test seams (tests/python/test_obs_box_avsync_dock_1470.py): OBS_BOX_AVSYNC_DOCK_OBS_RUNNING_CMD
# (`pgrep -x obs`), OBS_BOX_AVSYNC_DOCK_PY (scripts/obs_dock_state.py).

# The plugin's module name (vendor/av-sync-dock CMakeLists.txt `project(...)`, setup_plugin_target's
# PREFIX "" -> <name>.so) and the dock's object name (ID_PREFIX + ".main", plugin-main.c), test-pinned.
OBS_BOX_AVSYNC_DOCK_MODULE="obs-audio-video-sync-dock"
OBS_BOX_AVSYNC_DOCK_ID="net.nagater.obs-audio-video-sync-dock..main"
OBS_BOX_AVSYNC_DOCK_ROWS=3

_obs_box_avsync_dock_py() {
  printf '%s' "${OBS_BOX_AVSYNC_DOCK_PY:-$(dirname "${BASH_SOURCE[0]}")/../obs_dock_state.py}"
}

# _obs_box_avsync_dock_obs_running -> rc 0 when an OBS process runs (the seam word-split, else pgrep).
_obs_box_avsync_dock_obs_running() {
  local -a cmd
  read -r -a cmd <<<"${OBS_BOX_AVSYNC_DOCK_OBS_RUNNING_CMD:-pgrep -x obs}"
  "${cmd[@]}" >/dev/null 2>&1
}

# obs_box_avsync_dock_seed USER_INI DESKTOP_USER -> setup-strih step 16g on the stream role. Always rc 0.
obs_box_avsync_dock_seed() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}" why out rc=0
  if ! why="$(obs_box_step_applies avsync-dock)"; then
    echo "  A/V-sync dock layout: SKIP -- ${why}"
    return 0
  fi
  if _obs_box_avsync_dock_obs_running; then
    echo "  WARN: OBS is running -- it rewrites ${ini} from memory on exit, so the A/V-sync dock was NOT seeded into its saved layout; stop strih-obs.service and re-run setup-strih.sh (issue 1470)"
    return 0
  fi
  out="$(python3 "$(_obs_box_avsync_dock_py)" seed --ini "$ini" --dock "$OBS_BOX_AVSYNC_DOCK_ID" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "  WARN: the A/V-sync dock could not be seeded into ${ini} (left untouched): ${out} (issue 1470)"
    return 0
  fi
  case "$out" in
    seeded | redocked)
      chown -h "$user":"$user" "$ini" 2>/dev/null || true
      echo "  A/V-sync dock ${out} visible + docked right in the saved OBS layout (${ini}, issue 1470)" ;;
    present) echo "  A/V-sync dock: the saved OBS layout already places it (the operator's layout, kept)" ;;
    no-state | no-ini) echo "  WARN: ${ini} has no saved OBS dock layout yet -- start OBS once, stop it, re-run setup-strih.sh step 16g (issue 1470)" ;;
    *) echo "  WARN: the A/V-sync dock seed printed '${out}' (issue 1470)" ;;
  esac
  return 0
}

# obs_box_avsync_dock_loaded_verdict (stdin = an OBS log) -> loaded | not-loaded | no-module-list.
# Reads to EOF (drain-safe under the caller's pipefail).
obs_box_avsync_dock_loaded_verdict() {
  awk -v so="${OBS_BOX_AVSYNC_DOCK_MODULE}.so" '
    /Loaded Modules:/ { inlist = 1; seen = 1; next }
    inlist && /-----/ { inlist = 0; next }
    inlist { n = split($0, f, " "); if (n > 0 && f[n] == so) found = 1 }
    END { if (found) print "loaded"; else if (seen) print "not-loaded"; else print "no-module-list" }'
}

# obs_box_avsync_dock_grade_rows LIBDIR SHAREDIR LOG USER_INI -> the 3 `OK|`/`FAIL|`/`NOTE|` rows.
obs_box_avsync_dock_grade_rows() {
  local libdir="${1:?libdir required}" sharedir="${2:?sharedir required}" log="${3-}" ini="${4:?user.ini required}"
  local so loc missing="" v
  so="${libdir%/}/obs-plugins/${OBS_BOX_AVSYNC_DOCK_MODULE}.so"
  loc="${sharedir%/}/obs/obs-plugins/${OBS_BOX_AVSYNC_DOCK_MODULE}/locale/en-US.ini"
  [ -f "$so" ] || missing="$so"
  [ -f "$loc" ] || missing="${missing:+${missing} + }${loc}"
  if [ -z "$missing" ]; then
    printf 'OK|(avsync-dock) the A/V-sync dock plugin is installed: %s + its locale\n' "$so"
  else
    printf 'FAIL|(avsync-dock) the A/V-sync dock plugin is not installed (missing %s) -- the Linux genlock strih bundle carries it since issue 1470: deploy a bundle built after it (setup-strih.sh step 4)\n' "$missing"
  fi
  if [ -z "$log" ] || [ ! -f "$log" ]; then
    printf 'NOTE|(avsync-dock-loaded) no OBS log yet -- start strih-obs.service and re-run verify-strih.sh\n'
  else
    v="$(obs_box_avsync_dock_loaded_verdict <"$log")"
    case "$v" in
      loaded) printf 'OK|(avsync-dock-loaded) the newest OBS log (%s) lists %s.so in its Loaded Modules\n' "$log" "$OBS_BOX_AVSYNC_DOCK_MODULE" ;;
      not-loaded) printf 'FAIL|(avsync-dock-loaded) the newest OBS log (%s) does not list %s.so -- restart strih-obs.service after the bundle install (a running OBS keeps the plugins it started with)\n' "$log" "$OBS_BOX_AVSYNC_DOCK_MODULE" ;;
      *) printf 'NOTE|(avsync-dock-loaded) the newest OBS log (%s) has no Loaded Modules list yet (OBS still starting?) -- re-run verify-strih.sh\n' "$log" ;;
    esac
  fi
  v="$(python3 "$(_obs_box_avsync_dock_py)" check --ini "$ini" --dock "$OBS_BOX_AVSYNC_DOCK_ID" 2>/dev/null)" || v=unreadable
  case "$v" in
    visible) printf 'OK|(avsync-dock-layout) the saved OBS layout (%s) shows the A/V-sync dock\n' "$ini" ;;
    hidden) printf 'NOTE|(avsync-dock-layout) the operator closed the A/V-sync dock (docked, hidden in %s) -- kept; Docks menu to show it\n' "$ini" ;;
    absent | default-hidden) printf 'FAIL|(avsync-dock-layout) the saved OBS layout (%s) %s, so OBS opens it hidden -- stop strih-obs.service and re-run setup-strih.sh step 16g (issue 1470)\n' "$ini" "$([ "$v" = absent ] && echo 'has no A/V-sync dock' || echo 'holds the A/V-sync dock in its hidden + floating default')" ;;
    no-state | no-ini) printf 'FAIL|(avsync-dock-layout) OBS has saved no dock layout in %s yet -- start OBS once, stop it, re-run setup-strih.sh step 16g (issue 1470)\n' "$ini" ;;
    *) printf 'FAIL|(avsync-dock-layout) the saved OBS layout in %s could not be read (%s) -- fix the file, then re-run setup-strih.sh step 16g (issue 1470)\n' "$ini" "${v:-no answer}" ;;
  esac
}

# obs_box_avsync_dock_grade_report LIBDIR SHAREDIR LOG USER_INI -> verify-strih item 47 (the caller's
# ok / bad / note). rc 1 when the grader did not print all of its rows.
obs_box_avsync_dock_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(obs_box_avsync_dock_grade_rows "$@")
  [ "$rows" = "$OBS_BOX_AVSYNC_DOCK_ROWS" ]
}
