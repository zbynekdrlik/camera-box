#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the stream role's locked operator PipeWire install + its verify
# rows; no top-level statements besides constants) -- the sibling scripts/lib/*.sh convention of NOT setting
# `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict mode would leak into it.
# setup-strih.sh / verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/stream-operator-audio.sh -- issue 1465 item 3 (owner 23.10.2026, ROZHODNUTE: "na toto mozme
# pipewire vyskusat ale nesmie sa babrat do inych veci nez tych ktore mu jasne povolime a definujeme!"): the
# stream operator's headphones through a LOCKED PipeWire, on the STREAM role only (design comment 6363778378).
# What PipeWire may do is declared by scripts/stream_operator_audio.py (the ONE headphone device, rate + quantum,
# no restore, nothing else enabled); this lib installs and grades it:
#
#   * stream_operator_audio_install REPO USER_HOME DESKTOP_USER -- setup-strih step 12 on the stream role:
#     the PipeWire drop-in (~/.config/pipewire/pipewire.conf.d/60-stream-operator-clock.conf) and the
#     WirePlumber drop-in (~/.config/wireplumber/wireplumber.conf.d/60-stream-operator-headphones.conf),
#     compare-then-write; the strih-role leftovers (STREAM_OPERATOR_AUDIO_LEFTOVERS: the hub's program sink +
#     loopback, the quantum-1024 floor, the MiniFuse rules) removed. Both apply at the next start of the
#     operator's PipeWire + WirePlumber (setup never restarts them: the operator's audio, the supervisor
#     restarts it). rc 1 + a stderr line when no headphone device resolves (nothing written or removed);
#   * stream_operator_audio_grade_rows / _report REPO USER_HOME DESKTOP_USER -- verify-strih item 9b on the
#     stream role (STREAM_OPERATOR_AUDIO_ROWS rows): the files, the six read-back rows of the module's grader
#     (pw-dump + pw-metadata in the operator's session), the jack mixer (live + stored), the stream OBS
#     holding no pulse capture input, and the operator's volume control (the helper, the panel item, the keys).
#
# Item 3 REVISION (owner 24.10.2026, ROZHODNUTE 6383739315, design 6383742509): the device is the notebook
# jack on every stream box (never a USB interface); step 12 also writes the jack's declared HDA mixer and
# stores it (`stream_operator_audio.py apply-mixer`: Speaker off, Auto-Mute Enabled, Master / Headphone / PCM
# 0 dB, `alsactl store` -> alsa-restore.service at boot), installs the GLOBAL volume helper
# (scripts/stream_operator_volume.py -> $OBS_BOX_PANEL_HEADPHONES_HELPER, /usr/local/bin/obs-box-headphones)
# and merges the notebook's XF86Audio volume keys into the user's openbox rc.xml through the kiosk's ONE
# rc.xml filter (obs_box_openbox_rc_with_keys); the panel item itself is step 16h's (obs-box-panel.sh).
#
# Test seams (tests/python/test_stream_operator_audio_1465.py): STREAM_OPERATOR_AUDIO_SCENES (strih_scenes.py),
# the module's STREAM_OPERATOR_AUDIO_SYS / _UDEVADM and the monitor's STRIH_STREAM_MONITOR_ASOUND;
# STRIH_SESSION_APPS_EUID (strih_session_apps_user_run: inline vs sudo -u the operator); `pw-dump`,
# `pw-metadata` and `timeout` are looked up on PATH; the module's STREAM_OPERATOR_AUDIO_AMIXER / _ALSACTL /
# _ASOUND_STATE; OBS_BOX_PANEL_HEADPHONES_HELPER (the helper's installed path) and
# STREAM_OPERATOR_AUDIO_OPENBOX_RC_SRC (the stock rc.xml a user without one starts from).

STREAM_OPERATOR_AUDIO_PW_DROPIN=60-stream-operator-clock.conf
STREAM_OPERATOR_AUDIO_WP_DROPIN=60-stream-operator-headphones.conf
# The strih role's operator-session drop-ins (setup step 12's strih branch), removed on the stream role.
STREAM_OPERATOR_AUDIO_LEFTOVERS=(
  pipewire/pipewire.conf.d/51-strih-quantum-1024.conf
  pipewire/pipewire.conf.d/strih-program.conf
  pipewire/pipewire.conf.d/strih-program-loopback.conf
  wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf
  wireplumber/wireplumber.conf.d/51-strih-minifuse.conf
)
# How many rows stream_operator_audio_grade_rows prints (files + the module's six + the jack mixer + OBS +
# the volume control).
# shellcheck disable=SC2034  # read by the report below and the tests
STREAM_OPERATOR_AUDIO_ROWS=10
# The stock openbox rc.xml a user without one starts from (the kiosk's brightness facet reads the same file).
STREAM_OPERATOR_AUDIO_OPENBOX_RC_SRC="${STREAM_OPERATOR_AUDIO_OPENBOX_RC_SRC:-/etc/xdg/openbox/rc.xml}"

# _stream_operator_audio_session USER SECONDS CMD... -> CMD in USER's session (the operator PipeWire), bounded.
_stream_operator_audio_session() {
  local user="$1" t="$2"
  shift 2
  strih_session_apps_user_run "$user" timeout "$t" "$@"
}

# _stream_operator_audio_dump USER FILE -> the operator session's pw-dump into FILE (empty when unreadable).
_stream_operator_audio_dump() {
  _stream_operator_audio_session "$1" 15 pw-dump > "$2" 2>/dev/null || : > "$2"
}

# _stream_operator_audio_pcm_match_dropins USER_HOME -> the monitor drop-ins carrying --pcm-match, one per line.
# Review round 1: the player honours --pcm-match, the locked PipeWire follows the default card rule; with such a
# drop-in the two could pick different cards (the player would sit in no-sink forever). Always rc 0.
_stream_operator_audio_pcm_match_dropins() {
  local f
  for f in "${1}/.config/systemd/user/${STRIH_STREAM_MONITOR_UNIT:-strih-stream-monitor.service}.d"/*.conf; do
    if [ -f "$f" ] && grep -q -e '--pcm-match' "$f"; then printf '%s\n' "$f"; fi
  done
  return 0
}

# stream_operator_audio_install REPO USER_HOME DESKTOP_USER -> see the header. rc 0 / 1.
stream_operator_audio_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local mod="${1}/scripts/stream_operator_audio.py" owner dump wp rel f pm
  pm="$(_stream_operator_audio_pcm_match_dropins "$home")"
  if [ -n "$pm" ]; then
    echo "stream-operator-audio: ${pm//$'\n'/ } gives strih-stream-monitor a --pcm-match; the locked PipeWire follows the default rule (the notebook jack, never a USB interface) -- remove that drop-in (systemctl --user daemon-reload) and re-run" >&2
    return 1
  fi
  owner="${user}:$(id -gn "$user" 2>/dev/null)" || { echo "stream-operator-audio: unknown user ${user}" >&2; return 1; }
  dump="$(mktemp)" || { echo "stream-operator-audio: mktemp failed" >&2; return 1; }
  _stream_operator_audio_dump "$user" "$dump"
  if ! wp="$(python3 "$mod" render-wireplumber --pw-dump "$dump")"; then
    rm -f "$dump"
    echo "stream-operator-audio: the WirePlumber drop-in could not be rendered (above) -- nothing written" >&2
    return 1
  fi
  rm -f "$dump"
  strih_user_home_dir "$home" "$user" "${home}/.config/pipewire/pipewire.conf.d" || return 1
  strih_user_home_dir "$home" "$user" "${home}/.config/wireplumber/wireplumber.conf.d" || return 1
  python3 "$mod" render-pipewire | obs_box_write_if_changed \
    "${home}/.config/pipewire/pipewire.conf.d/${STREAM_OPERATOR_AUDIO_PW_DROPIN}" 0664 "$owner" "operator headphones clock" || return 1
  printf '%s\n' "$wp" | obs_box_write_if_changed \
    "${home}/.config/wireplumber/wireplumber.conf.d/${STREAM_OPERATOR_AUDIO_WP_DROPIN}" 0664 "$owner" "operator headphones device" || return 1
  for rel in "${STREAM_OPERATOR_AUDIO_LEFTOVERS[@]}"; do
    f="${home}/.config/${rel}"
    if [ -e "$f" ] || [ -L "$f" ]; then
      rm -f "$f" || { echo "stream-operator-audio: could not remove ${f}" >&2; return 1; }
      echo "  removed the strih-role leftover ${f}"
    fi
  done
  python3 "$mod" apply-mixer || { echo "stream-operator-audio: the jack's mixer could not be written + stored (above)" >&2; return 1; }
  stream_operator_audio_volume_install "$repo" "$home" "$user" "$owner" || return 1
  echo "  NOTE: the locked operator PipeWire applies at the next start of the operator's audio session: as ${user}"
  echo "        systemctl --user restart wireplumber pipewire pipewire-pulse (or reboot), then restart strih-stream-monitor"
  return 0
}

# stream_operator_audio_volume_install REPO USER_HOME DESKTOP_USER OWNER -> the operator's GLOBAL headphone volume
# (ROZHODNUTE 6383739315): the helper to $OBS_BOX_PANEL_HEADPHONES_HELPER (0755, owned by whoever runs setup:
# root) and the notebook's volume keys merged into the user's openbox rc.xml (seeded from the stock rc.xml when
# the user has none; operator content kept). Both compare-then-write. rc 0 / 1.
stream_operator_audio_volume_install() {
  local repo="$1" home="$2" user="$3" owner="$4" rc_user rc_src merged
  install -d -m 0755 "$(dirname "$OBS_BOX_PANEL_HEADPHONES_HELPER")" \
    || { echo "stream-operator-audio: cannot create $(dirname "$OBS_BOX_PANEL_HEADPHONES_HELPER")" >&2; return 1; }
  obs_box_write_if_changed "$OBS_BOX_PANEL_HEADPHONES_HELPER" 0755 "$(id -un):$(id -gn)" "operator headphone volume helper" \
    < "${repo}/scripts/stream_operator_volume.py" || return 1
  strih_user_home_dir "$home" "$user" "${home}/.config/openbox" || return 1
  rc_user="${home}/.config/openbox/rc.xml"
  if [ -f "$rc_user" ]; then rc_src="$rc_user"; else rc_src="$STREAM_OPERATOR_AUDIO_OPENBOX_RC_SRC"; fi
  [ -f "$rc_src" ] || { echo "stream-operator-audio: no openbox rc.xml to merge the volume keys into (${rc_src} missing)" >&2; return 1; }
  merged="$(obs_box_openbox_rc_with_keys obs_box_panel_headphones_keybinds_xml < "$rc_src")" \
    || { echo "stream-operator-audio: ${rc_src} has no </keyboard> section -- the volume keys cannot be merged" >&2; return 1; }
  printf '%s\n' "$merged" | obs_box_write_if_changed "$rc_user" 0644 "$owner" "openbox rc.xml headphone volume keys" || return 1
  echo "  the volume keys apply at the next login or now: as ${user}, DISPLAY=:0 openbox --reconfigure"
}

# _stream_operator_audio_control_row REPO USER_HOME -> the (operator-audio-control) row: the helper = the checkout,
# every headphone keybind line in the user's openbox rc.xml, every line of the panel item in the tint2 config.
# Always rc 0.
_stream_operator_audio_control_row() {
  local repo="$1" home="$2" rc cfg line bad=""
  rc="${home}/.config/openbox/rc.xml"
  cfg="${home}/.config/tint2/tint2rc"
  cmp -s "${repo}/scripts/stream_operator_volume.py" "$OBS_BOX_PANEL_HEADPHONES_HELPER" 2>/dev/null \
    || bad="${bad} ${OBS_BOX_PANEL_HEADPHONES_HELPER} missing or not the checkout (setup step 12);"
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"
    case "$line" in '<!--'*|'') continue ;; esac
    if ! grep -qF -- "$line" "$rc" 2>/dev/null; then
      line="${line#*key=\"}"
      bad="${bad} ${rc##*/} lacks the ${line%%\"*} key (setup step 12, then openbox --reconfigure);"
    fi
  done < <(obs_box_panel_headphones_keybinds_xml)
  while IFS= read -r line; do
    case "$line" in '#'*|'') continue ;; esac
    if ! grep -qxF -- "$line" "$cfg" 2>/dev/null; then
      bad="${bad} ${cfg##*/} lacks '${line}' (setup step 16h);"
      break
    fi
  done < <(_obs_box_panel_tint2rc_headphones)
  if [ -n "$bad" ]; then
    printf 'FAIL|(operator-audio-control)%s -- the operator has no global headphone volume\n' "$bad"
  else
    printf 'OK|(operator-audio-control) the global headphone volume: panel item Slúchadlá (wheel, click) + the XF86Audio keys -> %s\n' \
      "$OBS_BOX_PANEL_HEADPHONES_HELPER"
  fi
  return 0
}

# _stream_operator_audio_files_row REPO USER_HOME DUMP -> the (operator-audio-files) row. Always rc 0.
_stream_operator_audio_files_row() {
  local repo="$1" home="$2" dump="$3" mod="${1}/scripts/stream_operator_audio.py" pw wp rel bad=""
  pw="${home}/.config/pipewire/pipewire.conf.d/${STREAM_OPERATOR_AUDIO_PW_DROPIN}"
  wp="${home}/.config/wireplumber/wireplumber.conf.d/${STREAM_OPERATOR_AUDIO_WP_DROPIN}"
  [ -f "$pw" ] && cmp -s "$pw" <(python3 "$mod" render-pipewire 2>/dev/null) || bad="${bad} ${pw##*/} missing or drifted;"
  [ -f "$wp" ] && cmp -s "$wp" <(python3 "$mod" render-wireplumber --pw-dump "$dump" 2>/dev/null) \
    || bad="${bad} ${wp##*/} missing or drifted;"
  for rel in "${STREAM_OPERATOR_AUDIO_LEFTOVERS[@]}"; do
    if [ -e "${home}/.config/${rel}" ] || [ -L "${home}/.config/${rel}" ]; then
      bad="${bad} the strih-role ${rel##*/} is back;"
    fi
  done
  while IFS= read -r rel; do
    [ -n "$rel" ] && bad="${bad} ${rel##*/} gives the monitor a --pcm-match (remove it);"
  done < <(_stream_operator_audio_pcm_match_dropins "$home")
  if [ -n "$bad" ]; then
    printf 'FAIL|(operator-audio-files)%s -- re-run setup-strih.sh step 12, then restart the operator PipeWire\n' "$bad"
  else
    printf 'OK|(operator-audio-files) %s + %s as rendered, no strih-role leftover\n' \
      "$STREAM_OPERATOR_AUDIO_PW_DROPIN" "$STREAM_OPERATOR_AUDIO_WP_DROPIN"
  fi
  return 0
}

# _stream_operator_audio_obs_row REPO -> the (operator-audio-obs) row. Always rc 0.
_stream_operator_audio_obs_row() {
  local repo="$1" out rc=0 line
  out="$(timeout 20 python3 "${STREAM_OPERATOR_AUDIO_SCENES:-${repo}/scripts/strih_scenes.py}" --pulse-capture-inputs \
    --host "${OBS_WS_HOST:-127.0.0.1}" 2>&1)" || rc=$?
  line="${out##*$'\n'}"
  if [ "$rc" = 2 ] || [ "$rc" = 124 ]; then
    printf 'NOTE|(operator-audio-obs) OBS websocket not answering (%s) -- the pulse capture inputs are graded while OBS runs\n' "${out%%$'\n'*}"
  elif [ "$rc" != 0 ]; then
    printf 'FAIL|(operator-audio-obs) the pulse capture reader failed (rc %s: %s)\n' "$rc" "$line"
  elif [ "$line" = "pulse capture inputs: none" ]; then
    printf 'OK|(operator-audio-obs) the stream OBS holds no pulse capture input: the operator mix never reaches the program\n'
  else
    printf 'FAIL|(operator-audio-obs) %s -- the launch seed removes scene ones at the next OBS start; a global channel: OBS Settings > Audio > Disabled\n' "$line"
  fi
  return 0
}

# stream_operator_audio_grade_rows REPO USER_HOME DESKTOP_USER -> STREAM_OPERATOR_AUDIO_ROWS rows. Always rc 0.
stream_operator_audio_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local dump="" settings="" sm=""
  if ! dump="$(mktemp)" || ! settings="$(mktemp)" || ! sm="$(mktemp)"; then
    rm -f ${dump:+"$dump"} ${settings:+"$settings"}
    printf 'FAIL|(operator-audio-files) mktemp failed -- nothing graded\n'
    return 0
  fi
  _stream_operator_audio_dump "$user" "$dump"
  _stream_operator_audio_session "$user" 10 pw-metadata -n settings > "$settings" 2>/dev/null || : > "$settings"
  _stream_operator_audio_session "$user" 10 pw-metadata -n sm-settings > "$sm" 2>/dev/null || : > "$sm"
  _stream_operator_audio_files_row "$repo" "$home" "$dump"
  python3 "${repo}/scripts/stream_operator_audio.py" grade --pw-dump "$dump" --settings "$settings" --sm-settings "$sm" \
    2>/dev/null || printf 'FAIL|(operator-audio-sink) the operator audio grader failed\n'
  _stream_operator_audio_obs_row "$repo"
  _stream_operator_audio_control_row "$repo" "$home"
  rm -f "$dump" "$settings" "$sm"
  return 0
}

# stream_operator_audio_grade_report REPO USER_HOME DESKTOP_USER -> the rows through verify-strih's ok/note/bad.
# rc 1 unless every row was printed (a grader that stopped part way is a FAIL, never a shorter list).
stream_operator_audio_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(stream_operator_audio_grade_rows "$@")
  [ "$rows" = "$STREAM_OPERATOR_AUDIO_ROWS" ]
}
