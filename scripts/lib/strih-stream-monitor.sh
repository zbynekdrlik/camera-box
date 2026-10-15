#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the stream-role headphone monitor's constants + its three
# verify-strih rows; no top-level statements besides constants) -- the sibling scripts/lib/*.sh convention
# of NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict mode would leak
# into it. setup-strih.sh / verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/strih-stream-monitor.sh -- issue 1436 (owner 15.10.2026: the SNV way, one unified PP/SNV
# design; issue 1437: never through PipeWire): the stream box's headphones play the REAL outgoing stream.
# The session-app entry strih-stream-monitor.service (scripts/strih_stream_monitor.py) is installed by
# setup-strih step 16d like every session app (scripts/lib/strih-session-apps.sh), on the STREAM role only
# (obs_box_role_apply). This lib holds what is specific to it:
#
#   * STRIH_STREAM_MONITOR_PACKAGES -- ffmpeg (the RTMP audio decode) + alsa-utils (aplay), added to step
#     16d's package install when the entry is in the list (strih_session_apps_packages);
#   * strih_stream_monitor_grade_rows -- verify-strih's three extra rows of the entry, printed by
#     strih_session_apps_grade_rows (STRIH_STREAM_MONITOR_ROWS of them):
#       (stream-monitor-pass) the player's own --check-state: a pass within 30 s, waiting or playing, and
#                             the headphone PCM free or held by its aplay -- PipeWire on it FAILs;
#       (stream-monitor-key)  the stream OBS profile's service.json = rtmp_custom to the local restreamer
#                             with the box name as the key (setup step 3 writes it), and the short
#                             hostname (the unit's %l) is that same name -- else the monitor plays a key
#                             OBS never publishes;
#       (stream-monitor-obs)  the program-audio input (`Zvuk z mixu`) is on Monitor Off in the running OBS
#                             (the seeder sets it) -- a NOTE while OBS does not answer.
#
# Test seams (tests/python/test_strih_stream_monitor_1436.py): STRIH_STREAM_MONITOR_HOSTNAME (the short
# hostname), STRIH_STREAM_MONITOR_SCENES (strih_scenes.py), STRIH_STREAM_MONITOR_MANIFEST (the seed
# manifest); the player itself reads STRIH_STREAM_MONITOR_ASOUND / _PROC.

STRIH_STREAM_MONITOR_UNIT=strih-stream-monitor.service
# The player's state file, under the user's runtime dir (the unit passes --state-file %t/<this>).
STRIH_STREAM_MONITOR_STATE_FILE=strih-stream-monitor.json
# shellcheck disable=SC2034  # read cross-file by scripts/lib/strih-session-apps.sh (strih_session_apps_packages)
STRIH_STREAM_MONITOR_PACKAGES=(ffmpeg alsa-utils)
# How many rows strih_stream_monitor_grade_rows prints.
# shellcheck disable=SC2034  # read cross-file by scripts/lib/strih-session-apps.sh
STRIH_STREAM_MONITOR_ROWS=3

# strih_stream_monitor_short_hostname -> the box's short hostname (what the unit's %l expands to).
strih_stream_monitor_short_hostname() {
  if [ -n "${STRIH_STREAM_MONITOR_HOSTNAME:-}" ]; then
    printf '%s' "$STRIH_STREAM_MONITOR_HOSTNAME"
  else
    hostname -s 2>/dev/null || true
  fi
}

# strih_stream_monitor_key_row REPO USER_HOME -> the (stream-monitor-key) row. Always rc 0.
strih_stream_monitor_key_row() {
  local repo="$1" home="$2" key profile dir drift host
  if ! key="$(strih_lx_hostname 2>/dev/null)" || [ -z "$key" ] || ! profile="$(strih_lx_obs_profile 2>/dev/null)"; then
    printf 'FAIL|(stream-monitor-key) the box facts (STRIH_HOSTNAME / STRIH_OBS_PROFILE) could not be read\n'
    return 0
  fi
  dir="${home}/.config/obs-studio/basic/profiles/${profile}"
  drift="$(python3 "${repo}/scripts/obs_box_stream_profile.py" --grade-key "$dir" --key "$key" 2>&1 || true)"
  host="$(strih_stream_monitor_short_hostname)"
  if [ -n "$drift" ]; then
    printf 'FAIL|(stream-monitor-key) %s: %s -- re-run setup-strih.sh step 3 with OBS stopped (it writes the box name as the key when none is set; an operator key is kept, so remove it first)\n' "$dir" "${drift%%$'\n'*}"
  elif [ "$host" != "$key" ]; then
    printf "FAIL|(stream-monitor-key) the short hostname is '%s', not the box name '%s': the monitor plays rtmp://127.0.0.1:1234/live/%%l -- re-run setup-strih.sh step 1\n" "$host" "$key"
  else
    printf 'OK|(stream-monitor-key) OBS publishes rtmp://127.0.0.1:1234/live/%s and the monitor plays the same (hostname %s)\n' "$key" "$host"
  fi
  return 0
}

# strih_stream_monitor_obs_row REPO -> the (stream-monitor-obs) row. Always rc 0.
strih_stream_monitor_obs_row() {
  local repo="$1" name out rc=0
  name="${OBS_BOX_STREAM_AUDIO_INPUT:-Zvuk z mixu}"
  out="$(timeout 20 python3 "${STRIH_STREAM_MONITOR_SCENES:-${repo}/scripts/strih_scenes.py}" --audio-monitor-type \
    --host "${OBS_WS_HOST:-127.0.0.1}" --manifest "${STRIH_STREAM_MONITOR_MANIFEST:-/opt/camera-box/strih-seed.json}" 2>&1)" || rc=$?
  if [ "$rc" = 2 ] || [ "$rc" = 124 ]; then
    printf 'NOTE|(stream-monitor-obs) OBS websocket not answering (%s) -- the monitoring of %s is graded while OBS runs\n' "${out%%$'\n'*}" "$name"
  elif [ "$rc" != 0 ]; then
    printf 'FAIL|(stream-monitor-obs) the monitoring reader failed (rc %s: %s) -- run strih_scenes.py --audio-monitor-type by hand\n' "$rc" "${out##*$'\n'}"
  elif [ "$out" = OBS_MONITORING_TYPE_NONE ]; then
    printf 'OK|(stream-monitor-obs) %s is on Monitor Off: the headphones hear only the outgoing stream\n' "$name"
  elif [ "$out" = absent ]; then
    printf 'FAIL|(stream-monitor-obs) the program-audio input %s is absent from the OBS collection -- port the collection (obs_collection_port.py --preset stream-pp) or let the seeder create it at the next OBS start\n' "$name"
  else
    printf 'FAIL|(stream-monitor-obs) %s monitoring reads %s -- the seeder turns it off at the next OBS start (strih-obs-start.sh runs strih_scenes.py --bootstrap), or set Monitor Off by hand\n' "$name" "${out%%$'\n'*}"
  fi
  return 0
}

# strih_stream_monitor_grade_rows REPO USER_HOME DESKTOP_USER -> the three rows (OK|/FAIL|/NOTE| text).
# Always rc 0.
strih_stream_monitor_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  strih_session_app_pass_row stream-monitor strih_stream_monitor.py "$STRIH_STREAM_MONITOR_STATE_FILE" \
    "$STRIH_STREAM_MONITOR_UNIT" "$repo" "$user"
  strih_stream_monitor_key_row "$repo" "$home"
  strih_stream_monitor_obs_row "$repo"
  return 0
}
