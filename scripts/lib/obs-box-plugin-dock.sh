#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no top-level statements): sourcing runs in
# the CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-plugin-dock.sh -- the ONE engine that seeds and grades a plugin dock in the saved
# OBS layout on the Linux OBS boxes (issue 1470 the A/V-sync dock; issue 1436 the LUFS meter). Each dock
# is a SPEC: a few variables named OBS_BOX_DOCK_<KEY>_* that its own lib declares
# (scripts/lib/obs-box-avsync-dock.sh, scripts/lib/obs-box-loudness-dock.sh), read here by indirect
# expansion:
#   _MODULE   the plugin's module name (<name>.so in obs-plugins, its data dir in share/obs/obs-plugins)
#   _ID       the dock's Qt object name (obs_frontend_add_dock_by_id's id)
#   _MARKER   its first-seed key in user.ini's [CameraBox] section (scripts/obs_dock_state.py --marker)
#   _LABEL    the dock's name in every message
#   _TAG      the verify row tag: (<tag>), (<tag>-loaded), (<tag>-layout)
#   _STEP     the obs_box_step_applies key that gates the seed + the layout row (scripts/lib/obs-box-role.sh)
#   _ISSUE    the issue named in the messages
#   _INSTALL  how the plugin reaches the box, named by the not-installed FAIL row
#
# OBS hides a dock a plugin adds (OBSStudioAPI.cpp obs_frontend_add_dock_by_id: setVisible(false) +
# setFloating(true)); the saved [BasicWindow] DockState, restored after the plugins load, decides.
# scripts/obs_dock_state.py (the one Qt 6 saveState parser) seeds the dock visible + docked at the end of
# the right area when the state has no entry for it, or -- before this dock's first seed only -- holds
# it in OBS's own hidden + floating default; an operator's placement (visible, or closed) is kept. Each
# dock's first seed decision writes ITS marker into the same user.ini. Never while OBS runs (OBS rewrites
# user.ini from memory on exit; the ONE probe strih_operator_obs_running), never over an unreadable file.
#
# Test seams: STRIH_OPERATOR_OBS_RUNNING_CMD (the strih-operator.sh probe), OBS_BOX_PLUGIN_DOCK_PY
# (scripts/obs_dock_state.py).

OBS_BOX_PLUGIN_DOCK_ROWS=3

_obs_box_plugin_dock_py() {
  printf '%s' "${OBS_BOX_PLUGIN_DOCK_PY:-$(dirname "${BASH_SOURCE[0]}")/../obs_dock_state.py}"
}

# _obs_box_plugin_dock_spec KEY -> sets the caller's locals module id marker label tag step issue install
# (the caller declares them `local`); rc 1 naming a missing field.
_obs_box_plugin_dock_spec() {
  local key="${1:?dock key required}" f v
  case "$key" in
    '' | *[!ABCDEFGHIJKLMNOPQRSTUVWXYZ]*) echo "obs-box-plugin-dock: dock key '${key}' is not upper-case letters" >&2; return 1 ;;
  esac
  for f in MODULE ID MARKER LABEL TAG STEP ISSUE INSTALL; do
    v="OBS_BOX_DOCK_${key}_${f}"
    if [ -z "${!v:-}" ]; then
      echo "obs-box-plugin-dock: ${v} is not set" >&2
      return 1
    fi
  done
  v="OBS_BOX_DOCK_${key}_MODULE"; module="${!v}"
  v="OBS_BOX_DOCK_${key}_ID"; id="${!v}"
  v="OBS_BOX_DOCK_${key}_MARKER"; marker="${!v}"
  v="OBS_BOX_DOCK_${key}_LABEL"; label="${!v}"
  v="OBS_BOX_DOCK_${key}_TAG"; tag="${!v}"
  v="OBS_BOX_DOCK_${key}_STEP"; step="${!v}"
  v="OBS_BOX_DOCK_${key}_ISSUE"; issue="${!v}"
  v="OBS_BOX_DOCK_${key}_INSTALL"; install="${!v}"
}

# obs_box_plugin_dock_seed KEY USER_INI DESKTOP_USER -> setup-strih step 16g for one dock. Always rc 0.
obs_box_plugin_dock_seed() {
  local key="${1:?dock key required}" ini="${2:?user.ini path required}" user="${3:?desktop user required}"
  local module id marker label tag step issue install why out rc=0 tok
  _obs_box_plugin_dock_spec "$key" || { echo "  WARN: dock spec ${key} is incomplete (above) -- nothing seeded"; return 0; }
  if ! why="$(obs_box_step_applies "$step")"; then
    echo "  ${label} layout: SKIP -- ${why}"
    return 0
  fi
  if strih_operator_obs_running; then
    echo "  WARN: OBS is running -- it rewrites ${ini} from memory on exit, so the ${label} was NOT seeded into its saved layout; stop strih-obs.service and re-run setup-strih.sh (${issue})"
    return 0
  fi
  out="$(python3 "$(_obs_box_plugin_dock_py)" seed --ini "$ini" --dock "$id" --marker "$marker" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "  WARN: the ${label} could not be seeded into ${ini} (left untouched): ${out} (${issue})"
    return 0
  fi
  # The token is the last line; anything before it is the tool's own output, shown as it is.
  tok="${out##*$'\n'}"
  [ "$tok" = "$out" ] || echo "  ${out%$'\n'*}"
  case "$tok" in
    seeded | redocked | present)
      # the tool rewrote user.ini (a seed, or only the first-seed marker): the desktop user's file again
      chown -h "$user":"$user" "$ini" 2>/dev/null || true ;;
  esac
  case "$tok" in
    seeded | redocked) echo "  ${label} ${tok} visible + docked right in the saved OBS layout (${ini}, ${issue})" ;;
    present) echo "  ${label}: the saved OBS layout already places it (the operator's layout, kept)" ;;
    no-state | no-ini) echo "  WARN: ${ini} has no saved OBS dock layout yet -- start OBS once, stop it, re-run setup-strih.sh step 16g (${issue})" ;;
    *) echo "  WARN: the ${label} seed printed '${tok}' (${issue})" ;;
  esac
  return 0
}

# obs_box_plugin_dock_loaded_verdict MODULE (stdin = an OBS log) -> loaded | not-loaded | no-module-list.
# The module matches the list entry's whole name. Reads to EOF (drain-safe under the caller's pipefail).
obs_box_plugin_dock_loaded_verdict() {
  awk -v so="${1:?module required}.so" '
    /Loaded Modules:/ { inlist = 1; seen = 1; next }
    inlist && /-----/ { inlist = 0; next }
    inlist { n = split($0, f, " "); if (n > 0 && f[n] == so) found = 1 }
    END { if (found) print "loaded"; else if (seen) print "not-loaded"; else print "no-module-list" }'
}

# obs_box_plugin_dock_grade_rows KEY LIBDIR SHAREDIR LOG USER_INI -> the 3 `OK|`/`FAIL|`/`NOTE|` rows.
obs_box_plugin_dock_grade_rows() {
  local key="${1:?dock key required}" libdir="${2:?libdir required}" sharedir="${3:?sharedir required}"
  local log="${4-}" ini="${5:?user.ini required}"
  local module id marker label tag step issue install so loc missing="" v why
  _obs_box_plugin_dock_spec "$key" || return 0
  so="${libdir%/}/obs-plugins/${module}.so"
  loc="${sharedir%/}/obs/obs-plugins/${module}/locale/en-US.ini"
  [ -f "$so" ] || missing="$so"
  [ -f "$loc" ] || missing="${missing:+${missing} + }${loc}"
  if [ -z "$missing" ]; then
    printf 'OK|(%s) the %s plugin is installed: %s + its locale\n' "$tag" "$label" "$so"
  else
    printf 'FAIL|(%s) the %s plugin is not installed (missing %s) -- %s\n' "$tag" "$label" "$missing" "$install"
  fi
  if [ -z "$log" ] || [ ! -f "$log" ]; then
    printf 'NOTE|(%s-loaded) no OBS log yet -- start strih-obs.service and re-run verify-strih.sh\n' "$tag"
  else
    v="$(obs_box_plugin_dock_loaded_verdict "$module" <"$log")"
    case "$v" in
      loaded) printf 'OK|(%s-loaded) the newest OBS log (%s) lists %s.so in its Loaded Modules\n' "$tag" "$log" "$module" ;;
      not-loaded) printf 'FAIL|(%s-loaded) the newest OBS log (%s) does not list %s.so -- restart strih-obs.service after the bundle install (a running OBS keeps the plugins it started with); if it still does not load, grep that log for %s / Failed to load\n' "$tag" "$log" "$module" "$module" ;;
      *) printf 'NOTE|(%s-loaded) the newest OBS log (%s) has no Loaded Modules list yet (OBS still starting?) -- re-run verify-strih.sh\n' "$tag" "$log" ;;
    esac
  fi
  if ! why="$(obs_box_step_applies "$step")"; then
    printf 'NOTE|(%s-layout) not graded: %s\n' "$tag" "$why"
    return 0
  fi
  v="$(python3 "$(_obs_box_plugin_dock_py)" check --ini "$ini" --dock "$id" --marker "$marker" 2>/dev/null)" || v=unreadable
  case "$v" in
    visible) printf 'OK|(%s-layout) the saved OBS layout (%s) shows the %s\n' "$tag" "$ini" "$label" ;;
    hidden) printf 'NOTE|(%s-layout) the %s is hidden in %s (closed by the operator, or an OBS dock reset) -- kept; Docks menu to show it\n' "$tag" "$label" "$ini" ;;
    absent | default-hidden) printf 'FAIL|(%s-layout) the saved OBS layout (%s) %s, so OBS opens it hidden -- stop strih-obs.service and re-run setup-strih.sh step 16g (%s)\n' "$tag" "$ini" "$([ "$v" = absent ] && echo "has no ${label}" || echo "holds the ${label} in its hidden + floating default")" "$issue" ;;
    no-state | no-ini) printf 'FAIL|(%s-layout) OBS has saved no dock layout in %s yet -- start OBS once, stop it, re-run setup-strih.sh step 16g (%s)\n' "$tag" "$ini" "$issue" ;;
    *) printf 'FAIL|(%s-layout) the saved OBS layout in %s could not be read (%s) -- fix the file, then re-run setup-strih.sh step 16g (%s)\n' "$tag" "$ini" "${v:-no answer}" "$issue" ;;
  esac
}

# obs_box_plugin_dock_grade_report KEY LIBDIR SHAREDIR LOG USER_INI -> verify-strih item 47 for one dock
# (the caller's ok / bad / note). rc 1 when the grader did not print all of its rows.
obs_box_plugin_dock_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(obs_box_plugin_dock_grade_rows "$@")
  [ "$rows" = "$OBS_BOX_PLUGIN_DOCK_ROWS" ]
}
