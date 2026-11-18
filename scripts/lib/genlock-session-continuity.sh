#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, sets no shell options: sourcing runs it
# in the CALLER's shell -- recording-e2e.sh under `set -euo pipefail` -- which owns the strict mode).
#
# scripts/lib/genlock-session-continuity.sh -- issue 1534 (owner ROZHODNUTE 18.11.2026: the full-path
# E2E is a STRICT zero frame-loss gate for the WHOLE OBS session, not a 4-5 min sample): the BLOCKING
# whole-session video gate, the video twin of the Dante `session` check. On the strih and the stream box
# every genlock input's underruns / relocks after its start settle and every JPEG XS input's lost /
# concealed / undecodable slices since the OBS start must be 0 (scripts/genlock_session_continuity.py
# decides; this lib takes the marks and reads the boxes).
#
#   genlock_session_continuity_mark LABEL DIR STRIH STREAM
#       per box `<newest log name>\t<its size in bytes>` (a `stat`, never a read) into
#       DIR/genlock-session-continuity-<strih|stream>-LABEL.mark. Fail-open, rc 0 on every path: a
#       missing mark makes that box UNKNOWN at the gate (fail-closed there, never here).
#   genlock_session_continuity_gate HERE STRIH STREAM DIR [VERDICT_JSON]
#       reads each box's whole session through the ONE strih log reader's `session` op (strih over its
#       own transport, stream through stream_box_exec with the same text), decides, merges the block
#       into VERDICT_JSON as `genlock_session_continuity` and prints it. rc 0 = PASS; 1 = FAIL or
#       UNKNOWN (the caller folds it into $GATE, never lowering a red gate).
#
# The read covers the whole log up to the moment of the read (after the stop mark): never a miss, a loss
# between the stop and the gate fails the run too. A log that has not reached the stop mark (it never
# should) is read again, bounded. Linux OBS boxes only: a Windows-class box is UNKNOWN before any contact.
#
# Declared inputs (each must have an audit line; every other genlock input in the log is graded too):
# ALL_CAMBOX = `camera_strih_input` of every active camera (minus PREFLIGHT_EXCLUDED_CAMS), a single-
# camera run STRIH_PROG_SOURCE; stream = STREAM_PROG_SOURCE.
#
# Knobs: GENLOCK_SESSION_CONTINUITY_READ_TIMEOUT_S (60 per read), _WAIT_S (30) / _STEP_S (10).
# Test seam: GENLOCK_SESSION_CONTINUITY_READ_CMD (`CMD HOST REMOTE_TEXT` prints what the box would).

_GENLOCK_SESSION_CONTINUITY_LIB="${BASH_SOURCE[0]%/*}"
command -v stream_box_platform >/dev/null 2>&1 || . "$_GENLOCK_SESSION_CONTINUITY_LIB/stream-box.sh"
command -v strih_log_remote_cmd >/dev/null 2>&1 || . "$_GENLOCK_SESSION_CONTINUITY_LIB/strih-log-read.sh"

_gsc_count_ok() {
  [[ "${1-}" =~ ^[0123456789]{1,12}$ ]]
}

_gsc_name_ok() {
  [[ "${1-}" =~ ^[0123456789]{4}-[0123456789]{2}-[0123456789]{2}\ [0123456789]{2}-[0123456789]{2}-[0123456789]{2}\.txt$ ]]
}

# _gsc_read ROLE HOST OP -> what the box prints for OP (ident | session); empty on any failure.
_gsc_read() {
  local role="$1" host="$2" op="$3" t="${GENLOCK_SESSION_CONTINUITY_READ_TIMEOUT_S:-60}" text
  _gsc_count_ok "$t" && [ "$t" -gt 0 ] || t=60
  text="$(strih_log_remote_cmd linux "$op")"
  [ -n "$text" ] || return 0
  if [ -n "${GENLOCK_SESSION_CONTINUITY_READ_CMD:-}" ]; then
    timeout "$t" "$GENLOCK_SESSION_CONTINUITY_READ_CMD" "$host" "$text" || true
    return 0
  fi
  if [ "$role" = strih ]; then
    if [ "$op" = ident ]; then
      strih_log_ident "$host" "${STRIH_USER:-newlevel}" "${STRIH_PW:-}" "$t"
    else
      strih_log_session "$host" "${STRIH_USER:-newlevel}" "${STRIH_PW:-}" "$t"
    fi
    return 0
  fi
  # shellcheck disable=SC2016  # the sourced lib's path and the text are positional arguments
  timeout "$t" bash -c '. "$1"; shift; stream_box_exec "$@"' _ \
    "$_GENLOCK_SESSION_CONTINUITY_LIB/stream-box.sh" "${STREAM_USER:-newlevel}" "${STREAM_PW:-}" \
    "$host" "" "$text" 2>/dev/null || true
  return 0
}

genlock_session_continuity_mark() {
  local label="${1-}" dir="${2-}" strih="${3-}" stream="${4-}" role host out name size
  [ -n "$label" ] && [ -n "$dir" ] || return 0
  for role in strih stream; do
    if [ "$role" = strih ]; then host="$strih"; else host="$stream"; fi
    rm -f "$dir/genlock-session-continuity-${role}-${label}.mark" 2>/dev/null || true
    if [ -z "$host" ] || [ "$(stream_box_platform "$host")" != linux ]; then
      echo "    WARNING: [session-video] issue 1534: the ${role} box '${host}' is not a Linux OBS box -- no window ${label} mark, the whole-session video gate will read UNKNOWN" >&2
      continue
    fi
    out="$(_gsc_read "$role" "$host" ident 2>/dev/null | tr -d '\r' | head -n 1)" || out=""
    name="" size=""
    IFS=$'\t' read -r name size <<<"$out" || true
    if _gsc_name_ok "$name" && _gsc_count_ok "$size"; then
      printf '%s\t%s\n' "$name" "$size" >"$dir/genlock-session-continuity-${role}-${label}.mark" 2>/dev/null || true
      echo "    [session-video] issue 1534: window ${label} mark = byte ${size} of the ${role} OBS log '${name}'"
    else
      echo "    WARNING: [session-video] issue 1534: no window ${label} mark from the ${role} box ${host} (read: '${out}') -- the whole-session video gate will read UNKNOWN and fail the run" >&2
    fi
  done
  return 0
}

# _gsc_inputs ROLE -> the declared inputs of the box, one per line.
_gsc_inputs() {
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
    camera_strih_input "$cam" && printf '\n'
  done
  return 0
}

# _gsc_box ROLE HOST DIR PY OUT -> one box's result JSON at OUT (always written).
_gsc_box() {
  local role="$1" host="$2" dir="$3" py="$4" out="$5" sname="" ssize="" ename="" esize="" reason=""
  local read="$dir/genlock-session-continuity-${role}.read" line step wmax tries i cam
  local -a args=(--box "$role")
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    args+=(--input "$line")
  done < <(_gsc_inputs "$role")
  # the cameras the preflight acked offline are graded report-only (their inputs still write audit lines)
  if [ "$role" = strih ] && declare -F camera_strih_input >/dev/null; then
    for cam in ${PREFLIGHT_EXCLUDED_CAMS:-}; do
      line="$(camera_strih_input "$cam" 2>/dev/null)" || continue
      [ -z "$line" ] || args+=(--exclude "$line")
    done
  fi
  IFS=$'\t' read -r sname ssize 2>/dev/null <"$dir/genlock-session-continuity-${role}-start.mark" || true
  IFS=$'\t' read -r ename esize 2>/dev/null <"$dir/genlock-session-continuity-${role}-stop.mark" || true
  if [ -z "$host" ] || [ "$(stream_box_platform "$host")" != linux ]; then
    reason="the ${role} box '${host}' is not a Linux OBS box (the session read runs on the Linux boxes only)"
  elif ! _gsc_name_ok "$sname" || ! _gsc_count_ok "$ssize"; then
    reason="no window START mark (the ${role} OBS log was not readable before StartRecord)"
  elif ! _gsc_name_ok "$ename" || ! _gsc_count_ok "$esize"; then
    reason="no window STOP mark (the ${role} OBS log was not readable after StopRecord)"
  elif [ "$sname" != "$ename" ] || [ "$esize" -lt "$ssize" ]; then
    reason="the ${role} OBS log changed inside the window ('${sname}' byte ${ssize} -> '${ename}' byte ${esize}): OBS restarted during the recording"
  fi
  if [ -n "$reason" ]; then
    python3 "$py" "${args[@]}" --unknown "$reason" --box-out "$out" >/dev/null 2>&1 || true
    return 0
  fi
  step="${GENLOCK_SESSION_CONTINUITY_STEP_S:-10}"
  wmax="${GENLOCK_SESSION_CONTINUITY_WAIT_S:-30}"
  _gsc_count_ok "$step" || step=10
  _gsc_count_ok "$wmax" || wmax=30
  tries=$((wmax / (step > 0 ? step : 1) + 1))
  for ((i = 1; i <= tries; i++)); do
    _gsc_read "$role" "$host" session 2>/dev/null | tr -d '\r' >"$read" || true
    python3 "$py" "${args[@]}" --read "$read" --log-name "$ename" --start-byte "$ssize" --stop-byte "$esize" \
      --box-out "$out" >/dev/null 2>&1 || true
    if [ "$i" -lt "$tries" ] \
       && python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("retryable") else 1)' "$out" 2>/dev/null; then
      echo "    [session-video] the ${role} OBS log has not reached the stop mark yet -- reading again in ${step} s (${i}/${tries})"
      sleep "$step"
      continue
    fi
    break
  done
  [ -s "$out" ] || python3 "$py" "${args[@]}" --unknown "the ${role} decision wrote no result" --box-out "$out" >/dev/null 2>&1 || true
  return 0
}

genlock_session_continuity_gate() {
  local here="${1-}" strih="${2-}" stream="${3-}" dir="${4-}" verdict="${5-}"
  local py="$here/genlock_session_continuity.py" block="$dir/genlock-session-continuity.json" rc=0
  local sbox="$dir/genlock-session-continuity-strih.json" pbox="$dir/genlock-session-continuity-stream.json"
  echo "[session-video] issue 1534: whole-session video continuity since each OBS start (strih ${strih}, stream ${stream}) -- BLOCKING"
  rm -f "$block" "$sbox" "$pbox" 2>/dev/null || true
  _gsc_box strih "$strih" "$dir" "$py" "$sbox"
  _gsc_box stream "$stream" "$dir" "$py" "$pbox"
  python3 "$py" --combine "$sbox" "$pbox" --out "$block" >/dev/null 2>&1 || true
  if [ -n "$verdict" ] && [ -f "$verdict" ]; then
    python3 "$py" --from-block "$block" --merge-into "$verdict" 2>&1 | sed 's/^/    /' || true
    python3 -c 'import json,sys; sys.exit(0 if "genlock_session_continuity" in json.load(open(sys.argv[1])) else 1)' \
      "$verdict" 2>/dev/null \
      || echo "ERROR: [session-video] issue 1534: the session block could not be merged into ${verdict} -- the E2E report will NOT show it; the verdict is in ${block}" >&2
  else
    python3 "$py" --from-block "$block" 2>&1 | sed 's/^/    /' || true
  fi
  python3 "$py" --from-block "$block" >/dev/null 2>&1 || rc=$?
  if [ "$rc" = 0 ]; then
    echo "    ok: no genlock input repeated a frame and no JPEG XS input lost a slice since the OBS start (issue 1534)"
    return 0
  fi
  echo "ERROR: [session-video] issue 1534: the video is NOT proven loss-free over the whole OBS session (a repeated frame or a lost slice before the window counts too, UNKNOWN fails) -- this run is RED" >&2
  return 1
}
