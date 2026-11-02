#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, sets no shell options: sourcing runs it
# in the CALLER's shell -- recording-e2e.sh under `set -euo pipefail` -- which owns the strict mode).
#
# scripts/lib/jxs-link-continuity.sh -- issue 1387 slice 2 (design 6558387590): the BLOCKING JPEG XS
# link continuity gate of the full-path E2E, built like the issue-1439 program-audio continuity gate.
# On the JPEG XS path (program-inputs.sh: `! program_path_ndi`) every measured camera link (strih
# `JXS camN src`), the strih program output (`camera-box JPEG XS program`) and the stream program
# input (`JXS PGM src`) must lose nothing over the recording window: a lost, concealed or undecodable
# slice, a dropped or foreign datagram, a window with no frame, a hole in the windows, a busy-dropped,
# refused or unsent output frame = RED; UNKNOWN fails too. The decision is the pure
# scripts/jxs_link_continuity.py; this lib only takes the marks and reads the logs. On the NDI path
# both functions do nothing but say so.
#
#   jxs_link_continuity_mark LABEL DIR STRIH STREAM
#       the window mark of both boxes: `<newest log name>\t<its size in bytes>\t<newest HH:MM:SS.mmm>`
#       into DIR/jxs-link-continuity-<strih|stream>-LABEL.mark (LABEL start = just before StartRecord,
#       stop = right after StopRecord). Fail-open, rc 0 on every path: a missing mark makes that box
#       UNKNOWN, which fails the run at the gate (fail-closed there, never here).
#   jxs_link_continuity_gate HERE STRIH STREAM DIR [VERDICT_JSON]
#       reads each box's log from JXS_LINK_CONTINUITY_LOOKBACK bytes before the start mark to
#       JXS_LINK_CONTINUITY_CLOSE bytes past the stop mark (its JPEG XS lines only, filtered on the
#       box), decides, waits (bounded) while a closing window is not in the log yet, merges the block
#       into VERDICT_JSON as `jxs_link_continuity` (the Discord report names it) and prints it.
#       rc 0 = PASS (or the NDI path); 1 = FAIL or UNKNOWN (the caller folds it into $GATE).
#
# BOUNDED (owner 1.11.2026: an ssh read of the whole 100+ MB OBS log on a broadcast box dropped frames
# on the JPEG XS program link): a mark is a `stat` + a 64 KiB `tail -c` seek, never `wc -l`; the read
# is `tail -c +N | head -c LEN` (a seek, then only the window's bytes) filtered by `grep` on the box at
# nice 19, so only the JPEG XS lines (with their byte offsets, `grep -b`) leave the box.
#
# Which inputs: an ALL_CAMBOX run grades every active camera on the strih's JPEG XS set
# (camera_active_excluding, the acked-offline cameras left out, camera_strih_input), a single-camera
# run the one on strih program (STRIH_PROG_SOURCE); stream: STREAM_PROG_SOURCE. The camera senders'
# own `jxs send` counters are not read (an ssh per camera the run does not make otherwise).
#
# Transport: stream_box_exec (scripts/lib/stream-box.sh, plain ssh by the box's obs-fleet class) for
# BOTH boxes -- the JPEG XS path exists only on the Linux OBS boxes; a Windows-class box reads UNKNOWN
# before any contact. Bounded by `timeout`. Credentials: STRIH_USER/STRIH_PW, STREAM_USER/STREAM_PW.
#
# Knobs: JXS_LINK_CONTINUITY_LOOKBACK (262144 bytes before the start mark: the window line before the
# start), _CLOSE (262144 bytes past the stop mark: the closing window), _WAIT_S (60) / _STEP_S (10)
# for a closing window not in the log yet, _READ_TIMEOUT_S (60 per read). Test seam:
# JXS_LINK_CONTINUITY_READ_CMD (`CMD HOST REMOTE_TEXT` prints what the box would).

_JXS_LINK_CONTINUITY_LIB="${BASH_SOURCE[0]%/*}"
command -v stream_box_platform >/dev/null 2>&1 || . "$_JXS_LINK_CONTINUITY_LIB/stream-box.sh"
command -v program_path_ndi >/dev/null 2>&1 || . "$_JXS_LINK_CONTINUITY_LIB/program-inputs.sh"

# A log name the box may hand back: OBS names its logs `YYYY-MM-DD HH-MM-SS.txt` (spelled-out digit
# sets: a locale bracket range is wider).
_jxs_link_continuity_name_ok() {
  [[ "${1-}" =~ ^[0123456789]{4}-[0123456789]{2}-[0123456789]{2}\ [0123456789]{2}-[0123456789]{2}-[0123456789]{2}\.txt$ ]]
}

_jxs_link_continuity_count_ok() {
  [[ "${1-}" =~ ^[0123456789]{1,12}$ ]]
}

_jxs_link_continuity_time_ok() {
  [[ "${1-}" =~ ^[0123456789]{2}:[0123456789]{2}:[0123456789]{2}\.[0123456789]{3}$ ]]
}

# The remote texts (bash on the box). NAME is validated, FROM / LEN numeric.
_jxs_link_continuity_mark_sh() {
  # shellcheck disable=SC2016  # expands on the box
  printf '%s' 'd="$HOME/.config/obs-studio/logs"; n="$(ls -t -- "$d"/*.txt 2>/dev/null | head -n 1)"; [ -n "$n" ] || exit 3; s="$(stat -c %s -- "$n")" || exit 3; h="$(nice -n 19 tail -c 65536 -- "$n" | grep -a -o -E "^[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}" | tail -n 1)"; printf "%s\t%s\t%s\n" "${n##*/}" "$s" "$h"'
}

_jxs_link_continuity_slice_sh() {
  local name="$1" from="$2" len="$3"
  # shellcheck disable=SC2016  # expands on the box
  printf '%s' 'd="$HOME/.config/obs-studio/logs"; n="$(ls -t -- "$d"/*.txt 2>/dev/null | head -n 1)"; f="$d/'"${name}"'"; [ -n "$n" ] && [ -f "$f" ] || exit 3; printf "#newest\t%s\t%s\n" "${n##*/}" "$(stat -c %s -- "$f")"; nice -n 19 tail -c +'"$((from + 1))"' -- "$f" | head -c '"${len}"' | nice -n 19 grep -a -b -E "^[0-9:.]+: \[distroav\] jxs-(source|output) " || true'
}

# _jxs_link_continuity_read HOST USER PW TEXT -> what the box prints (rc = the read's).
_jxs_link_continuity_read() {
  local host="$1" user="$2" pw="$3" text="$4" t="${JXS_LINK_CONTINUITY_READ_TIMEOUT_S:-60}"
  _jxs_link_continuity_count_ok "$t" || t=60
  if [ -n "${JXS_LINK_CONTINUITY_READ_CMD:-}" ]; then
    timeout "$t" "$JXS_LINK_CONTINUITY_READ_CMD" "$host" "$text"
    return
  fi
  # shellcheck disable=SC2016  # the sourced lib's path and the text are positional arguments
  timeout "$t" bash -c '. "$1"; shift; stream_box_exec "$@"' _ \
    "$_JXS_LINK_CONTINUITY_LIB/stream-box.sh" "$user" "$pw" "$host" "" "$text"
}

# _jxs_link_continuity_box_creds ROLE -> sets _jxs_user / _jxs_pw for the box.
_jxs_link_continuity_box_creds() {
  if [ "${1-}" = strih ]; then
    _jxs_user="${STRIH_USER:-newlevel}" _jxs_pw="${STRIH_PW:-}"
  else
    _jxs_user="${STREAM_USER:-newlevel}" _jxs_pw="${STREAM_PW:-}"
  fi
}

jxs_link_continuity_mark() {
  local label="${1-}" dir="${2-}" strih="${3-}" stream="${4-}" role host out name size tod _jxs_user _jxs_pw
  program_path_ndi && return 0
  [ -n "$label" ] && [ -n "$dir" ] || return 0
  for role in strih stream; do
    if [ "$role" = strih ]; then host="$strih"; else host="$stream"; fi
    rm -f "$dir/jxs-link-continuity-${role}-${label}.mark" 2>/dev/null || true
    if [ "$(stream_box_platform "$host")" != linux ]; then
      echo "    WARNING: [jxs-link] issue 1387: the ${role} box ${host} is not a Linux OBS box -- no window ${label} mark, the JPEG XS link gate will read UNKNOWN" >&2
      continue
    fi
    _jxs_link_continuity_box_creds "$role"
    out="$(_jxs_link_continuity_read "$host" "$_jxs_user" "$_jxs_pw" "$(_jxs_link_continuity_mark_sh)" 2>/dev/null \
      | tr -d '\r' | head -n 1)" || out=""
    name="" size="" tod=""
    IFS=$'\t' read -r name size tod <<<"$out" || true
    if _jxs_link_continuity_name_ok "$name" && _jxs_link_continuity_count_ok "$size"; then
      _jxs_link_continuity_time_ok "$tod" || tod=""
      printf '%s\t%s\t%s\n' "$name" "$size" "$tod" >"$dir/jxs-link-continuity-${role}-${label}.mark" 2>/dev/null || true
      echo "    [jxs-link] issue 1387: window ${label} mark = byte ${size} (${tod:-no time}) of the ${role} OBS log '${name}'"
    else
      echo "    WARNING: [jxs-link] issue 1387: no window ${label} mark from the ${role} box ${host} (read: '${out}') -- the JPEG XS link gate will read UNKNOWN and fail the run" >&2
    fi
  done
  return 0
}

# _jxs_link_continuity_inputs ROLE -> the measured JPEG XS inputs of the box, one per line.
_jxs_link_continuity_inputs() {
  local cam
  if [ "${1-}" = stream ]; then
    [ -z "${STREAM_PROG_SOURCE:-}" ] || printf '%s\n' "$STREAM_PROG_SOURCE"
    return 0
  fi
  if [ "${ALL_CAMBOX:-0}" != 1 ]; then
    [ -z "${STRIH_PROG_SOURCE:-}" ] || printf '%s\n' "$STRIH_PROG_SOURCE"
    return 0
  fi
  declare -F camera_active_excluding >/dev/null && declare -F camera_strih_input >/dev/null || return 0
  for cam in $(camera_active_excluding "${PREFLIGHT_EXCLUDED_CAMS:-}"); do
    case " ${CAMERA_STRIH_JXS_SET:-} " in
      *" ${cam} "*) camera_strih_input "$cam" && printf '\n' ;;
      *) echo "    NOTE: [jxs-link] ${cam} is not on the strih's JPEG XS set -- no JPEG XS link to grade for it" >&2 ;;
    esac
  done
  return 0
}

# _jxs_link_continuity_box ROLE HOST DIR PY OUT -> one box's result JSON at OUT (always written).
_jxs_link_continuity_box() {
  local role="$1" host="$2" dir="$3" py="$4" out="$5" sname="" ssize="" stime="" ename="" esize="" etime=""
  local back close from len reason="" slice="$dir/jxs-link-continuity-${role}-slice.txt" _jxs_user _jxs_pw line
  local -a args=(--box "$role")
  local n_in=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    args+=(--input "$line")
    n_in=$((n_in + 1))
  done < <(_jxs_link_continuity_inputs "$role")
  [ "$role" != strih ] || args+=(--program-output)
  IFS=$'\t' read -r sname ssize stime 2>/dev/null <"$dir/jxs-link-continuity-${role}-start.mark" || true
  IFS=$'\t' read -r ename esize etime 2>/dev/null <"$dir/jxs-link-continuity-${role}-stop.mark" || true
  if [ "$n_in" = 0 ]; then
    reason="no measured ${role} JPEG XS input resolved (the active cameras on the JPEG XS set / STRIH_PROG_SOURCE / STREAM_PROG_SOURCE)"
  elif [ "$(stream_box_platform "$host")" != linux ]; then
    reason="the ${role} box ${host} is not a Linux OBS box (the JPEG XS path runs on the Linux boxes only)"
  elif ! _jxs_link_continuity_name_ok "$sname" || ! _jxs_link_continuity_count_ok "$ssize"; then
    reason="no window START mark (the ${role} OBS log was not readable before StartRecord)"
  elif ! _jxs_link_continuity_name_ok "$ename" || ! _jxs_link_continuity_count_ok "$esize"; then
    reason="no window STOP mark (the ${role} OBS log was not readable after StopRecord)"
  elif [ "$sname" != "$ename" ] || [ "$esize" -lt "$ssize" ]; then
    reason="the ${role} OBS log changed inside the window ('${sname}' byte ${ssize} -> '${ename}' byte ${esize}): OBS restarted during the recording"
  fi
  if [ -n "$reason" ]; then
    python3 "$py" "${args[@]}" --unknown "$reason" --box-out "$out" >/dev/null 2>&1 || true
    return 0
  fi
  back="${JXS_LINK_CONTINUITY_LOOKBACK:-262144}"
  close="${JXS_LINK_CONTINUITY_CLOSE:-262144}"
  _jxs_link_continuity_count_ok "$back" || back=262144
  _jxs_link_continuity_count_ok "$close" || close=262144
  from=$((ssize - back))
  [ "$from" -ge 0 ] || from=0
  len=$((esize - from + close))
  _jxs_link_continuity_box_creds "$role"
  if ! _jxs_link_continuity_read "$host" "$_jxs_user" "$_jxs_pw" "$(_jxs_link_continuity_slice_sh "$sname" "$from" "$len")" \
       >"$slice" 2>/dev/null || ! [ -s "$slice" ]; then
    python3 "$py" "${args[@]}" --unknown "the ${role} OBS log could not be read from ${host}" \
      --box-out "$out" >/dev/null 2>&1 || true
    return 0
  fi
  python3 "$py" "${args[@]}" --slice "$slice" --name "$sname" --from "$from" --len "$len" --start "$ssize" \
    --stop "$esize" --start-time "${stime:-}" --stop-time "${etime:-}" --box-out "$out" >/dev/null 2>&1 || true
  return 0
}

jxs_link_continuity_gate() {
  local here="${1-}" strih="${2-}" stream="${3-}" dir="${4-}" verdict="${5-}"
  local py="$here/jxs_link_continuity.py" block="$dir/jxs-link-continuity.json" step wmax tries i rc=0
  local sbox="$dir/jxs-link-continuity-strih.json" pbox="$dir/jxs-link-continuity-stream.json"
  if program_path_ndi; then
    echo "[jxs-link] issue 1387 JPEG XS link continuity -- SKIPPED (the NDI path)"
    return 0
  fi
  echo "[jxs-link] issue 1387: JPEG XS link continuity over the recording window (strih ${strih}, stream ${stream}) -- BLOCKING"
  step="${JXS_LINK_CONTINUITY_STEP_S:-10}"
  wmax="${JXS_LINK_CONTINUITY_WAIT_S:-60}"
  _jxs_link_continuity_count_ok "$step" || step=10
  _jxs_link_continuity_count_ok "$wmax" || wmax=60
  tries=$((wmax / (step > 0 ? step : 1) + 1))
  for ((i = 1; i <= tries; i++)); do
    rm -f "$block" "$sbox" "$pbox" 2>/dev/null || true
    _jxs_link_continuity_box strih "$strih" "$dir" "$py" "$sbox"
    _jxs_link_continuity_box stream "$stream" "$dir" "$py" "$pbox"
    rc=0
    python3 "$py" --combine "$sbox" "$pbox" --out "$block" >/dev/null 2>&1 || rc=$?
    if [ "$rc" = 2 ] && [ "$i" -lt "$tries" ] \
       && python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("retryable") else 1)' "$block" 2>/dev/null; then
      echo "    [jxs-link] a closing window is not in the log yet -- reading again in ${step} s (${i}/${tries})"
      sleep "$step"
      continue
    fi
    break
  done
  [ -s "$block" ] || python3 "$py" --combine "$sbox" "$pbox" --out "$block" >/dev/null 2>&1 || true
  if [ -n "$verdict" ] && [ -f "$verdict" ]; then
    python3 "$py" --from-block "$block" --merge-into "$verdict" 2>&1 | sed 's/^/    /' || true
    python3 -c 'import json,sys; sys.exit(0 if "jxs_link_continuity" in json.load(open(sys.argv[1])) else 1)' \
      "$verdict" 2>/dev/null \
      || echo "ERROR: [jxs-link] issue 1387: the JPEG XS link block could not be merged into ${verdict} -- the E2E report will NOT show it; the verdict is in ${block}" >&2
  else
    python3 "$py" --from-block "$block" 2>&1 | sed 's/^/    /' || true
  fi
  rc=0
  python3 "$py" --from-block "$block" >/dev/null 2>&1 || rc=$?
  if [ "$rc" = 0 ]; then
    echo "    ok: every JPEG XS link lost nothing in the recording window (issue 1387)"
    return 0
  fi
  echo "ERROR: [jxs-link] issue 1387: a JPEG XS link is NOT proven loss-free over the recording window (a concealed slice is a lost slice, UNKNOWN fails) -- this run is RED" >&2
  return 1
}
