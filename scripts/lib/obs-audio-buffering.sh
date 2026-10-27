#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell (setup-strih / verify-strih /
# recording-e2e.sh, all `set -euo pipefail`), so strict mode here would leak into it. Every function
# is safe as a bare statement under the caller's errexit: no grep / pipeline whose no-match exit leaks.
#
# scripts/lib/obs-audio-buffering.sh -- issue 1425 (design 6443440513, ROZHODNUTE 6436368009): the
# per-box FIXED OBS audio buffering, box side.
#
# The box fact STRIH_AUDIO_BUFFERING_MS (scripts/strih-boxes/<box>.env, 0..960 ms) is written by
# setup-strih step 5 to ~/.camera-box/genlock-audio-buffering-ms of the OBS user. The genlock libobs
# reads that file once per audio reset (obs.c genlock_audio_buffering_read_box_ms): the buffering is
# then exactly that value from the first tick and never grows (a late mixed member is re-anchored,
# MIXED_LATE), and the log carries ONE line
#   genlock-audio-buffering: FIXED <ms> ms (<ticks> ticks) from ~/.camera-box/genlock-audio-buffering-ms (issue 1425)
# A box without a valid file logs `genlock-audio-buffering: NOT deterministic ...` instead.
#
# Public API:
#   obs_audio_buffering_ms_ok VALUE             -> 0 iff an integer 0..960 (ASCII digits, spelled out)
#   obs_audio_buffering_install HOME USER MS    -> setup-strih step 5: write the marker (compare-then-write)
#   obs_audio_buffering_verdict FACT MARKER LOG -> pure grade: `ok <detail>` rc 0 | `FAIL: <reason>` rc 1
#   obs_audio_buffering_verify_row FACT HOME LOGFILE -> verify-strih item 48 (calls the caller's ok/bad)
#   obs_audio_buffering_e2e_gate_all STRIH SUSER SPW STREAM TUSER TPW -> the recording-e2e [0/8] row
#
# The grade (verify-strih item 48 and the E2E [0/8] row share it): the fact is valid; the marker file
# equals the fact; the RUNNING OBS log (the newest log) carries the FIXED line with that value; after
# it no `ABOVE the floor` growth line and every `audio-telemetry #800: total_buffering=` reads the FIXED
# line's tick count. Ticks, not ms: a fact that is not a whole number of 21.3 ms ticks is rounded UP to
# whole ticks, and the telemetry prints the rounded ms (85 -> 4 ticks = 85 ms, 50 -> 3 ticks = 64 ms).

OBS_AUDIO_BUFFERING_MARKER_REL=".camera-box/genlock-audio-buffering-ms"
OBS_AUDIO_BUFFERING_MAX_MS=960
# The token a reader prints for a marker file that does not exist.
OBS_AUDIO_BUFFERING_ABSENT="__ABSENT__"
# The OBS log lines the grade reads (one grep, never the whole log over ssh).
OBS_AUDIO_BUFFERING_LOG_ERE='genlock-audio-buffering: |ABOVE the floor|audio-telemetry #800: total_buffering='

# obs_audio_buffering_ms_ok VALUE -> 0 iff VALUE is 1..3 ASCII digits whose value is 0..960. The digit
# set is spelled out: a bracket RANGE follows the locale (ci-testing-gotchas.md, issue 1302).
obs_audio_buffering_ms_ok() {
  local v="${1-}"
  [[ "$v" =~ ^[0123456789]{1,3}$ ]] || return 1
  [ "$((10#$v))" -le "$OBS_AUDIO_BUFFERING_MAX_MS" ]
}

# obs_audio_buffering_install HOME USER MS -> write MS + "\n" to HOME/.camera-box/genlock-audio-buffering-ms,
# 0644, owned by USER, through a temp file + rename, only when the content, mode or owner differ (logged
# `unchanged` / `written`). The directory goes through strih_user_home_dir (every level the user's, no
# symlinked level). A symlinked marker refuses: setup runs as root in a user-writable tree. OBS reads the
# file at its next start; this never restarts OBS. rc 1 + a stderr line on any refusal or failure.
obs_audio_buffering_install() {
  local home="${1-}" user="${2-}" ms="${3-}" dir file tmp want cur_mode cur_owner
  if ! obs_audio_buffering_ms_ok "$ms"; then
    echo "obs-audio-buffering: STRIH_AUDIO_BUFFERING_MS '${ms}' is not an integer 0..${OBS_AUDIO_BUFFERING_MAX_MS} ms (issue 1425)" >&2
    return 1
  fi
  [ -n "$home" ] && [ -n "$user" ] || { echo "obs-audio-buffering: HOME and USER are required" >&2; return 1; }
  if ! declare -F strih_user_home_dir >/dev/null; then
    # shellcheck source=scripts/lib/strih-user-home.sh
    . "$(dirname "${BASH_SOURCE[0]}")/strih-user-home.sh" || return 1
  fi
  dir="${home%/}/.camera-box"
  file="${home%/}/${OBS_AUDIO_BUFFERING_MARKER_REL}"
  strih_user_home_dir "$home" "$user" "$dir" || return 1
  if [ -L "$file" ] || { [ -e "$file" ] && [ ! -f "$file" ]; }; then
    echo "obs-audio-buffering: ${file} is a symlink or not a plain file -- refusing to write it (issue 1425)" >&2
    return 1
  fi
  want="$((10#$ms))"
  if [ -f "$file" ]; then
    cur_mode="$(stat -c '%a' "$file" 2>/dev/null)" || cur_mode=""
    cur_owner="$(stat -c '%U' "$file" 2>/dev/null)" || cur_owner=""
    if [ "$(cat "$file" 2>/dev/null)" = "$want" ] && [ "$(wc -c < "$file")" -eq "$((${#want} + 1))" ] \
      && [ "$cur_mode" = 644 ] && [ "$cur_owner" = "$user" ]; then
      echo "  ${file}: unchanged (${want} ms)"
      return 0
    fi
  fi
  tmp="$(mktemp "${dir}/.genlock-audio-buffering-ms.XXXXXX")" || { echo "obs-audio-buffering: mktemp in ${dir} failed" >&2; return 1; }
  if ! { printf '%s\n' "$want" > "$tmp" && chmod 0644 "$tmp" && chown "${user}:" "$tmp" && mv -f "$tmp" "$file"; }; then
    rm -f "$tmp"
    echo "obs-audio-buffering: could not write ${file} (issue 1425)" >&2
    return 1
  fi
  echo "  ${file}: written (${want} ms) -- OBS reads it at its next start (restart strih-obs.service to apply)"
}

# obs_audio_buffering_verdict FACT MARKER LOG_TEXT -> the grade (see the header). MARKER = the marker
# file's content with its trailing newline removed, or OBS_AUDIO_BUFFERING_ABSENT. LOG_TEXT = the newest
# OBS log's lines (all of them, or only those OBS_AUDIO_BUFFERING_LOG_ERE matches; empty = no log).
# Prints ONE line and returns 0 (`ok ...`) or 1 (`FAIL: ...`). Pure: no file or network access.
obs_audio_buffering_verdict() {
  local fact="${1-}" marker="${2-}" log="${3-}" line fixed_re tel_re above_re
  local fixed_ms="" fixed_ticks="" seen=0 tel_n=0 tel_bad="" above="" warn=""
  fixed_re='genlock-audio-buffering: FIXED ([0123456789]+) ms \(([0123456789]+) ticks\)'
  tel_re='audio-telemetry #800: total_buffering=([0123456789]+) ms \(ticks=([0123456789]+)/'
  above_re='total audio buffering is now ([0123456789]+) milliseconds'
  if ! obs_audio_buffering_ms_ok "$fact"; then
    echo "FAIL: the box fact STRIH_AUDIO_BUFFERING_MS '${fact}' is not an integer 0..${OBS_AUDIO_BUFFERING_MAX_MS} ms"
    return 1
  fi
  fact="$((10#$fact))"
  if [ "$marker" = "$OBS_AUDIO_BUFFERING_ABSENT" ]; then
    echo "FAIL: ~/${OBS_AUDIO_BUFFERING_MARKER_REL} is missing (the fact is ${fact} ms) -- re-run setup-strih step 5, then restart strih-obs.service"
    return 1
  fi
  if [ "$marker" != "$fact" ]; then
    echo "FAIL: ~/${OBS_AUDIO_BUFFERING_MARKER_REL} reads '${marker}', the box fact STRIH_AUDIO_BUFFERING_MS is ${fact} ms -- re-run setup-strih step 5, then restart strih-obs.service"
    return 1
  fi
  if [ -z "$log" ]; then
    echo "FAIL: no OBS log to read the running OBS's audio buffering from"
    return 1
  fi
  # The LAST genlock-audio-buffering line is the running OBS's latest audio reset; grade what follows it.
  while IFS= read -r line; do
    if [[ "$line" == *"genlock-audio-buffering: "* ]]; then
      seen=1 tel_n=0 tel_bad="" above="" warn=""
      if [[ "$line" =~ $fixed_re ]]; then
        fixed_ms="${BASH_REMATCH[1]}" fixed_ticks="${BASH_REMATCH[2]}"
      else
        fixed_ms="" fixed_ticks="" warn="${line#*genlock-audio-buffering: }"
      fi
    elif [ "$seen" = 1 ] && [[ "$line" == *"ABOVE the floor"* ]]; then
      if [[ "$line" =~ $above_re ]]; then above="${BASH_REMATCH[1]} ms"; else above="a growth"; fi
    elif [ "$seen" = 1 ] && [[ "$line" =~ $tel_re ]]; then
      tel_n=$((tel_n + 1))
      [ -n "$fixed_ticks" ] && [ "${BASH_REMATCH[2]}" != "$fixed_ticks" ] && tel_bad="${BASH_REMATCH[1]} ms (${BASH_REMATCH[2]} ticks)"
    fi
  done <<< "$log"
  if [ "$seen" = 0 ]; then
    echo "FAIL: the running OBS log has no genlock-audio-buffering line (a bundle before issue 1425, or OBS has not started since)"
    return 1
  fi
  if [ -z "$fixed_ms" ]; then
    echo "FAIL: the running OBS logged '${warn:0:120}' -- it started before the marker was right, restart strih-obs.service"
    return 1
  fi
  if [ "$((10#$fixed_ms))" != "$fact" ]; then
    echo "FAIL: the running OBS is FIXED at ${fixed_ms} ms, the box fact is ${fact} ms -- restart strih-obs.service"
    return 1
  fi
  if [ -n "$above" ]; then
    echo "FAIL: the running OBS grew its audio buffering to ${above} after the FIXED ${fact} ms line"
    return 1
  fi
  if [ -n "$tel_bad" ]; then
    echo "FAIL: audio-telemetry #800 read total_buffering=${tel_bad}, the FIXED line is ${fixed_ticks} ticks"
    return 1
  fi
  echo "ok FIXED ${fact} ms (${fixed_ticks} ticks) in the running OBS log, the marker equals the fact, ${tel_n} telemetry read(s) at ${fixed_ticks} ticks, no growth"
}

# _obs_audio_buffering_read_marker FILE -> the marker's content without its trailing newline(s), or
# OBS_AUDIO_BUFFERING_ABSENT. At most 16 bytes (a larger file is wrong anyway).
_obs_audio_buffering_read_marker() {
  local f="${1-}" v
  if [ -f "$f" ]; then
    v="$(head -c 16 "$f" 2>/dev/null)" || v=""
    printf '%s' "$v"
  else
    printf '%s' "$OBS_AUDIO_BUFFERING_ABSENT"
  fi
}

# obs_audio_buffering_verify_row FACT HOME LOGFILE -> verify-strih item 48: ONE `ok`/`bad` row through
# the CALLER's ok / bad functions. Read-only.
obs_audio_buffering_verify_row() {
  local fact="${1-}" home="${2-}" logf="${3-}" marker log="" v
  marker="$(_obs_audio_buffering_read_marker "${home%/}/${OBS_AUDIO_BUFFERING_MARKER_REL}")"
  if [ -n "$logf" ] && [ -f "$logf" ]; then
    log="$(grep -a -E "$OBS_AUDIO_BUFFERING_LOG_ERE" "$logf" 2>/dev/null || true)"
    [ -n "$log" ] || log="(no audio-buffering line)"
  fi
  if v="$(obs_audio_buffering_verdict "$fact" "$marker" "$log")"; then
    ok "(audio-buffering) ${v#ok }"
  else
    bad "(audio-buffering) ${v#FAIL: } (issue 1425)"
  fi
}

# obs_audio_buffering_remote_cmd -> the ONE bash text the E2E row runs on a Linux OBS box over ssh:
# `MARKER=<content|__ABSENT__>`, `LOG=yes|none`, then the newest OBS log's matching lines.
obs_audio_buffering_remote_cmd() {
  # shellcheck disable=SC2016  # the $-words are expanded on the box, not here
  printf 'm="$HOME/%s"; if [ -f "$m" ]; then printf "MARKER=%%s\\n" "$(head -c 16 "$m")"; else echo "MARKER=%s"; fi; f="$(ls -1t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -1)"; if [ -n "$f" ]; then echo LOG=yes; grep -a -E %s "$f" || true; else echo LOG=none; fi' \
    "$OBS_AUDIO_BUFFERING_MARKER_REL" "$OBS_AUDIO_BUFFERING_ABSENT" "'${OBS_AUDIO_BUFFERING_LOG_ERE}'"
}

# obs_audio_buffering_box_fact HOST -> the STRIH_AUDIO_BUFFERING_MS of the box HOST is (its obs-fleet
# name's fact file, loaded in a SUBSHELL so the caller never gets a loaded box). rc 1 + a stderr line
# when HOST has no fleet row or the facts do not load.
obs_audio_buffering_box_fact() {
  local host="${1-}" dir="${BASH_SOURCE[0]%/*}" name out
  name="$(obs_fleet_name_for_host "$host" 2>/dev/null)" || name=""
  if [ -z "$name" ]; then
    echo "obs-audio-buffering: no obs-fleet row names ${host} -- cannot read its box fact (issue 1425)" >&2
    return 1
  fi
  if ! out="$(
    command -v strih_box_load >/dev/null 2>&1 || . "${dir}/strih-box-facts.sh"
    strih_box_load "$name" >/dev/null || exit 3
    strih_box_fact STRIH_AUDIO_BUFFERING_MS
  )"; then
    echo "obs-audio-buffering: the box facts of '${name}' did not load (scripts/strih-boxes/${name}.env -- the loader's line above says why) (issue 1425)" >&2
    return 1
  fi
  printf '%s\n' "$out"
}

# obs_audio_buffering_e2e_gate HOST USER PW -> the [0/8] grade of ONE OBS box. A box whose obs-fleet
# class is not linux-genlock has no box fact: one NOTE, rc 0. Otherwise the fact (dev1's checkout), the
# box's marker + newest OBS log over plain ssh (sshpass outermost, the ssh bounded by
# OBS_AUDIO_BUFFERING_SSH_TIMEOUT, default 20 s), and the verdict: `ok` line rc 0, or an ERROR line rc 1.
# An unreadable box is a FAIL (fail closed), never a pass.
obs_audio_buffering_e2e_gate() {
  local host="${1-}" user="${2-}" pw="${3-}" name fact raw marker="" log="" have_log="" line v
  name="$(obs_fleet_name_for_host "$host" 2>/dev/null)" || name="$host"
  if [ "$(obs_fleet_class_for_host "$host" 2>/dev/null || true)" != "linux-genlock" ]; then
    echo "    NOTE: ${name}: not a linux-genlock box -- no box fact STRIH_AUDIO_BUFFERING_MS, audio buffering not graded (issue 1425)"
    return 0
  fi
  if ! fact="$(obs_audio_buffering_box_fact "$host")"; then
    echo "ERROR: [0/8] ${name}: the audio buffering cannot be graded -- its box fact did not load (issue 1425)" >&2
    return 1
  fi
  raw="$(sshpass -p "$pw" timeout "${OBS_AUDIO_BUFFERING_SSH_TIMEOUT:-20}" ssh -n -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 \
    "${user}@${host}" "$(obs_audio_buffering_remote_cmd)" 2>/dev/null)" || raw=""
  while IFS= read -r line; do
    case "$line" in
      MARKER=*) [ -z "$have_log" ] && marker="${line#MARKER=}" ;;
      LOG=yes) have_log=yes ;;
      LOG=none) have_log=none ;;
      *) [ "$have_log" = yes ] && log="${log}${line}"$'\n' ;;
    esac
  done <<< "$raw"
  if [ -z "$have_log" ]; then
    echo "ERROR: [0/8] ${name} (${host}): could not read its audio-buffering marker + OBS log over ssh -- not graded, refusing (issue 1425)" >&2
    return 1
  fi
  [ "$have_log" = yes ] && [ -z "$log" ] && log="(no audio-buffering line)"
  if v="$(obs_audio_buffering_verdict "$fact" "$marker" "$log")"; then
    echo "    ok: ${name} audio buffering ${v#ok }"
    return 0
  fi
  echo "ERROR: [0/8] ${name} (${host}) audio buffering NOT deterministic: ${v#FAIL: } (issue 1425)" >&2
  return 1
}

# obs_audio_buffering_e2e_gate_all STRIH SUSER SPW STREAM TUSER TPW -> the BLOCKING recording-e2e [0/8]
# row: both OBS boxes graded (both always run, so one refusal names every box at once); rc 1 when either
# fails.
obs_audio_buffering_e2e_gate_all() {
  local rc=0
  echo "[0/8] OBS audio buffering is the box's FIXED value on strih + stream (issue 1425, BLOCKING)"
  obs_audio_buffering_e2e_gate "${1-}" "${2-}" "${3-}" || rc=1
  obs_audio_buffering_e2e_gate "${4-}" "${5-}" "${6-}" || rc=1
  return "$rc"
}
