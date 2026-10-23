#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, sets no shell options: sourcing runs it
# in the CALLER's shell -- recording-e2e.sh under `set -euo pipefail` -- which owns the strict mode).
#
# scripts/lib/program-audio-continuity.sh -- issue 1439 (ROZHODNUTE 6359382787, owner 23.10.2026):
# the BLOCKING program-audio continuity gate of the full-path E2E. Audio has the same zero-frame-loss
# bar as video: a lost VBAN slot (silent OR concealed), a late packet, a stale drop, a re-anchor, a
# slot handed after its stamp, a buffering-guard re-anchor of the program input in the OBS mix or an
# ASRC placement jump inside the recording window makes the run RED. The decision is the pure
# scripts/program_audio_continuity.py; this lib only takes the window marks and reads the log.
#
#   program_audio_continuity_mark LABEL DIR [HOST]
#       the window mark: `<newest log name>\t<line count>` of the stream box's newest OBS log, written
#       to DIR/program-audio-continuity-LABEL.mark (LABEL start = just before StartRecord, stop =
#       right after StopRecord). Fail-open, rc 0 on every path: a missing mark makes the gate UNKNOWN,
#       which fails the run (fail-closed there, never here, so a mark never aborts the recording).
#   program_audio_continuity_gate HERE HOST DIR [VERDICT_JSON]
#       reads the log from (start mark - PROGRAM_AUDIO_CONTINUITY_LOOKBACK lines) to its end, runs the
#       decision, waits (bounded) while the closing status / ASRC / telemetry line is not in the log yet, merges
#       the block into VERDICT_JSON as `program_audio_continuity` (the Discord report names it) and
#       prints it. rc 0 = PASS only; 1 = FAIL or UNKNOWN (the caller folds it into $GATE).
#
# The program-audio input and the box come from the box facts: a Linux stream box (stream-snv) via
# stream_mbc_vban_expect (scripts/lib/stream-box.sh, STRIH_PROGRAM_AUDIO_INPUT, `mbc`); a Windows
# stream box has no fact file and no obs-vban source, its program audio is the Dante input `mbc`
# (PROGRAM_AUDIO_CONTINUITY_WIN_INPUT), graded on the buffering-guard + ASRC checks only (`--no-vban`,
# the block says the obs-vban check is n/a). The transport is stream_box_exec (bash over plain ssh on
# Linux, PowerShell through win_ssh_run on Windows), bounded by `timeout`.
#
# Knobs: PROGRAM_AUDIO_CONTINUITY_LOOKBACK (2000 lines before the start mark: the opening status line
# is at most 10 s back, the ASRC line 60 s), _WAIT_S (90 s for the closing lines) / _STEP_S (10 s),
# _READ_TIMEOUT_S (60 s per read). Test seams: PROGRAM_AUDIO_CONTINUITY_READ_CMD (`CMD mark|slice HOST
# NAME FROM` prints what the box would), PROGRAM_AUDIO_CONTINUITY_PLATFORM (linux|windows).

_PROGRAM_AUDIO_CONTINUITY_LIB="${BASH_SOURCE[0]%/*}"
command -v stream_box_platform >/dev/null 2>&1 || . "$_PROGRAM_AUDIO_CONTINUITY_LIB/stream-box.sh"

# A log name the box may hand back: OBS names its logs `YYYY-MM-DD HH-MM-SS.txt`. Anything else is
# refused before it reaches a remote command (spelled-out digit sets: a locale bracket range is wider).
_program_audio_continuity_name_ok() {
  [[ "${1-}" =~ ^[0123456789]{4}-[0123456789]{2}-[0123456789]{2}\ [0123456789]{2}-[0123456789]{2}-[0123456789]{2}\.txt$ ]]
}

_program_audio_continuity_count_ok() {
  [[ "${1-}" =~ ^[0123456789]{1,9}$ ]]
}

_program_audio_continuity_platform() {
  local p="${PROGRAM_AUDIO_CONTINUITY_PLATFORM:-}"
  case "$p" in linux | windows) printf '%s' "$p" ;; *) stream_box_platform "${1-}" ;; esac
}

# The remote texts (bash for Linux, PowerShell for Windows). NAME is validated, FROM numeric.
_program_audio_continuity_sh() {
  local mode="$1" name="${2-}" from="${3-}"
  local d='d="$HOME/.config/obs-studio/logs"; '
  local newest='n="$(ls -t -- "$d"/*.txt 2>/dev/null | head -n 1)"; '
  if [ "$mode" = mark ]; then
    printf '%s' "${d}${newest}"'[ -n "$n" ] || exit 3; printf '"'"'%s\t%s\n'"'"' "${n##*/}" "$(wc -l < "$n")"'
  else
    printf '%s' "${d}${newest}"'printf '"'"'#newest\t%s\n'"'"' "${n##*/}"; tail -n +'"${from}"' -- "$d"/'"'${name}'"
  fi
}

_program_audio_continuity_ps() {
  local mode="$1" name="${2-}" from="${3-}"
  local head='$ErrorActionPreference = "Stop"
$dir = Join-Path $env:APPDATA "obs-studio\logs"
$f = Get-ChildItem -LiteralPath $dir -Filter "*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $f) { exit 3 }
'
  if [ "$mode" = mark ]; then
    printf '%s' "$head"'$fs = [System.IO.FileStream]::new($f.FullName, "Open", "Read", "ReadWrite")
$r = [System.IO.StreamReader]::new($fs)
$n = 0
while ($null -ne $r.ReadLine()) { $n++ }
$r.Close()
Write-Output ("{0}`t{1}" -f $f.Name, $n)'
  else
    printf '%s' "$head"'Write-Output ("#newest`t{0}" -f $f.Name)
$fs = [System.IO.FileStream]::new((Join-Path $dir "'"${name}"'"), "Open", "Read", "ReadWrite")
$r = [System.IO.StreamReader]::new($fs)
$i = 0
while ($null -ne ($l = $r.ReadLine())) { $i++; if ($i -ge '"${from}"') { Write-Output $l } }
$r.Close()'
  fi
}

# _program_audio_continuity_read MODE HOST [NAME FROM] -> what the box prints (rc = the read's).
_program_audio_continuity_read() {
  local mode="$1" host="$2" name="${3-}" from="${4-}" t="${PROGRAM_AUDIO_CONTINUITY_READ_TIMEOUT_S:-60}"
  _program_audio_continuity_count_ok "$t" || t=60
  if [ -n "${PROGRAM_AUDIO_CONTINUITY_READ_CMD:-}" ]; then
    timeout "$t" "$PROGRAM_AUDIO_CONTINUITY_READ_CMD" "$mode" "$host" "$name" "$from"
    return
  fi
  # shellcheck disable=SC2016  # the sourced libs' paths and the texts are positional arguments
  timeout "$t" bash -c '. "$1"; . "$2"; shift 2; stream_box_exec "$@"' _ \
    "$_PROGRAM_AUDIO_CONTINUITY_LIB/win-ssh-exec.sh" "$_PROGRAM_AUDIO_CONTINUITY_LIB/stream-box.sh" \
    "${STREAM_USER:-newlevel}" "${STREAM_PW:-newlevel}" "$host" \
    "$(_program_audio_continuity_ps "$mode" "$name" "$from")" \
    "$(_program_audio_continuity_sh "$mode" "$name" "$from")"
}

program_audio_continuity_mark() {
  local label="${1-}" dir="${2-}" host="${3:-${STREAM:-}}" out="" name="" count=""
  [ -n "$label" ] && [ -n "$dir" ] || return 0
  rm -f "$dir/program-audio-continuity-${label}.mark" 2>/dev/null || true
  out="$(_program_audio_continuity_read mark "$host" 2>/dev/null | tr -d '\r' | head -n 1)" || out=""
  IFS=$'\t' read -r name count <<<"$out" || true
  if _program_audio_continuity_name_ok "$name" && _program_audio_continuity_count_ok "$count"; then
    printf '%s\t%s\n' "$name" "$count" >"$dir/program-audio-continuity-${label}.mark" 2>/dev/null || true
    echo "    [program-audio] issue 1439: window ${label} mark = line ${count} of the stream OBS log '${name}'"
  else
    echo "    WARNING: [program-audio] issue 1439: no window ${label} mark from the stream box ${host} (read: '${out}') -- the program-audio continuity gate will read UNKNOWN and fail the run" >&2
  fi
  return 0
}

program_audio_continuity_gate() {
  local here="${1-}" host="${2-}" dir="${3-}" verdict="${4-}"
  local py="$here/program_audio_continuity.py" block="$dir/program-audio-continuity.json"
  local log="$dir/program-audio-continuity-log.txt" platform input="" expect="" reason="" rc=0
  local sname="" scount="" ename="" ecount="" newest="" from tries step wmax back raw i
  local -a vban=()
  rm -f "$block" "$log" 2>/dev/null || true
  platform="$(_program_audio_continuity_platform "$host")"
  echo "[program-audio] issue 1439: program-audio continuity over the recording window (stream box ${host}, ${platform}) -- BLOCKING"
  if [ "$platform" = linux ]; then
    if expect="$(stream_mbc_vban_expect "$host" 2>&1)"; then
      read -r _ _ input <<<"$expect"
    else
      reason="the stream box's facts name no program-audio input: ${expect}"
    fi
  else
    input="${PROGRAM_AUDIO_CONTINUITY_WIN_INPUT:-mbc}"
    vban=(--no-vban)
  fi
  if [ -z "$reason" ]; then
    IFS=$'\t' read -r sname scount <"$dir/program-audio-continuity-start.mark" 2>/dev/null || true
    IFS=$'\t' read -r ename ecount <"$dir/program-audio-continuity-stop.mark" 2>/dev/null || true
    if ! _program_audio_continuity_name_ok "$sname" || ! _program_audio_continuity_count_ok "$scount"; then
      reason="no window START mark (the stream OBS log was not readable before StartRecord)"
    elif ! _program_audio_continuity_name_ok "$ename" || ! _program_audio_continuity_count_ok "$ecount"; then
      reason="no window STOP mark (the stream OBS log was not readable after StopRecord)"
    elif [ "$sname" != "$ename" ] || [ "$ecount" -lt "$scount" ]; then
      reason="the stream OBS log changed inside the window ('${sname}' line ${scount} -> '${ename}' line ${ecount}): OBS restarted during the recording"
    fi
  fi
  if [ -n "$reason" ]; then
    python3 "$py" --unknown "$reason" --input "$input" "${vban[@]}" --out "$block" >/dev/null 2>&1 || true
  else
    back="${PROGRAM_AUDIO_CONTINUITY_LOOKBACK:-2000}"
    step="${PROGRAM_AUDIO_CONTINUITY_STEP_S:-10}"
    wmax="${PROGRAM_AUDIO_CONTINUITY_WAIT_S:-90}"
    _program_audio_continuity_count_ok "$back" || back=2000
    _program_audio_continuity_count_ok "$step" || step=10
    _program_audio_continuity_count_ok "$wmax" || wmax=90
    from=$((scount - back))
    [ "$from" -ge 1 ] || from=1
    tries=$((wmax / (step > 0 ? step : 1) + 1))
    for ((i = 1; i <= tries; i++)); do
      if raw="$(_program_audio_continuity_read slice "$host" "$sname" "$from" 2>/dev/null | tr -d '\r')" \
         && [ -n "$raw" ]; then
        newest="$(head -n 1 <<<"$raw")"
        newest="${newest#\#newest$'\t'}"
        if [ "$newest" != "$sname" ]; then
          python3 "$py" --unknown "OBS started a new log ('${newest}') after the window start ('${sname}'): OBS restarted, the window cannot be proven" \
            --input "$input" "${vban[@]}" --out "$block" >/dev/null 2>&1 || true
          break
        fi
        tail -n +2 <<<"$raw" >"$log"
        rc=0
        python3 "$py" --log "$log" --input "$input" "${vban[@]}" --first-line "$from" \
          --start-line "$scount" --end-line "$ecount" --out "$block" >/dev/null 2>&1 || rc=$?
        # an opening line older than the lookback (a relock burst writes many lines): read the
        # whole log once, never report UNKNOWN for a read that started too late (review round 1)
        if [ "$rc" = 2 ] && [ "$from" -gt 1 ] \
           && python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("opening_missing") else 1)' "$block" 2>/dev/null; then
          echo "    [program-audio] no opening line in the ${back} lines before the start mark -- reading the log again from the first line"
          from=1
          i=$((i - 1))
          continue
        fi
        if [ "$rc" = 2 ] && [ "$i" -lt "$tries" ] \
           && python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("retryable") else 1)' "$block" 2>/dev/null; then
          echo "    [program-audio] the closing status / ASRC / telemetry line is not in the log yet -- reading again in ${step} s (${i}/${tries})"
          sleep "$step"
          continue
        fi
        break
      fi
      rm -f "$block"
      if [ "$i" -lt "$tries" ]; then
        echo "    [program-audio] the stream OBS log read failed -- again in ${step} s (${i}/${tries})" >&2
        sleep "$step"
      else
        python3 "$py" --unknown "the stream OBS log could not be read from ${host} (${tries} tries)" \
          --input "$input" "${vban[@]}" --out "$block" >/dev/null 2>&1 || true
      fi
    done
  fi
  [ -s "$block" ] || python3 "$py" --unknown "the continuity decision wrote no result" --input "$input" \
    "${vban[@]}" --out "$block" >/dev/null 2>&1 || true
  rc=0
  if [ -n "$verdict" ] && [ -f "$verdict" ]; then
    python3 "$py" --from-block "$block" --input "$input" --merge-into "$verdict" 2>&1 | sed 's/^/    /' || true
    # review round 1: a block the verdict JSON does not carry is a red run the report would call green
    python3 -c 'import json,sys; sys.exit(0 if "program_audio_continuity" in json.load(open(sys.argv[1])) else 1)' \
      "$verdict" 2>/dev/null \
      || echo "ERROR: [program-audio] issue 1439: the continuity block could not be merged into ${verdict} -- the E2E report will NOT show it; the verdict is in ${block}" >&2
  else
    python3 "$py" --from-block "$block" --input "$input" 2>&1 | sed 's/^/    /' || true
  fi
  python3 "$py" --from-block "$block" --input "$input" >/dev/null 2>&1 || rc=$?
  if [ "$rc" = 0 ]; then
    echo "    ok: the program audio lost nothing in the recording window (issue 1439)"
    return 0
  fi
  echo "ERROR: [program-audio] issue 1439: the program audio is NOT proven continuous over the recording window (a concealed slot is a lost slot, UNKNOWN fails) -- this run is RED" >&2
  return 1
}
