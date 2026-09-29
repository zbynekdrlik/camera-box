#!/usr/bin/env bash
# shellcheck disable=SC2034  # the CG_SP_BURN_ON / CG_SP_BURN_OWED flags are read by cg-chain-e2e.sh
# airuleset:script-ok source-only lib (defines functions only, no top-level statement) — the
# scripts/lib/*.sh convention of NOT setting `set -euo pipefail` in a sourced file: it runs in the
# CALLER's shell (recording-e2e.sh, which sets it). Every function returns 0 on its best-effort paths
# unless its comment says "MUST be called from an `if`".
#
# scripts/lib/cg-chain-songplayer.sh — issue 1302: the SongPlayer half of the opt-in CG_CHAIN=1 E2E
# profile, sourced by scripts/lib/cg-chain-e2e.sh (never on its own; its functions call that lib's
# cg_chain_cg_scene / cg_chain_state_file / cg_chain_scene_py / cg_chain_burn_obs_timeout at run
# time). Two parts:
#   (a)  the SongPlayer burn API: the toggle POST (HTTP code + body logged), the /health read-back
#        polled by wall time, 404 / 409 named and never retried;
#   (b2) SongPlayer's OWN program through its obs-websocket facade (:4456): snapshot, press, BOTH
#        programs read back, and the restore after the recording.
# The rule: .claude/rules/cg-burn-node-role.md.

# ---- (a) the SongPlayer burn API ---------------------------------------------------------------

# The SongPlayer API base (env CG_CHAIN_SONGPLAYER_API, default http://resolume.lan:8920 — the
# SongPlayer app on RESOLUME-SNV), with any trailing slash dropped. Pure.
cg_chain_songplayer_api_base() {
  local base="${CG_CHAIN_SONGPLAYER_API:-http://resolume.lan:8920}"
  printf '%s' "${base%/}"
}

# The SongPlayer output whose burn this run toggles (env CG_CHAIN_SONGPLAYER_OUTPUT). Pure.
cg_chain_songplayer_output() {
  printf '%s' "${CG_CHAIN_SONGPLAYER_OUTPUT:-SP-fast}"
}

# The burn-toggle URL. on/off travels in the JSON body, never the URL. Pure.
cg_chain_songplayer_burn_url() {
  printf '%s/api/v1/ndi/burn' "$(cg_chain_songplayer_api_base)"
}

# The health URL whose per-output `burn_on` confirms a toggle. Pure.
cg_chain_songplayer_health_url() {
  printf '%s/api/v1/ndi/health' "$(cg_chain_songplayer_api_base)"
}

# The burn-toggle JSON body for $1 (on|off): {"output":"<out>","on":true|false}. Returns 1 (and
# prints nothing) on any other action, so a typo can never POST. Pure (python json.dumps escapes the
# output name correctly).
cg_chain_songplayer_burn_body() {
  case "$1" in
    on | off) ;;
    *) return 1 ;;
  esac
  python3 -c 'import json,sys; print(json.dumps({"output": sys.argv[1], "on": sys.argv[2] == "on"}, separators=(",", ":")))' \
    "$(cg_chain_songplayer_output)" "$1"
}

# Read a /api/v1/ndi/health JSON document on STDIN and print the `burn_on` of output $1:
# `true` / `false`, or `unknown` when the JSON does not parse, the output is absent, or its
# `burn_on` is missing / not a boolean. Never fails the caller (ALWAYS returns 0).
cg_chain_health_burn_on() {
  python3 -c '
import json, sys
out = sys.argv[1]
try:
    doc = json.load(sys.stdin)
except Exception:
    print("unknown", end="")
    sys.exit(0)
rows = doc if isinstance(doc, list) else []
for row in rows:
    if isinstance(row, dict) and row.get("ndi_name") == out:
        v = row.get("burn_on")
        print("true" if v is True else "false" if v is False else "unknown", end="")
        sys.exit(0)
print("unknown", end="")
' "$1" 2>/dev/null || printf 'unknown'
  return 0
}

# The CURRENT `burn_on` of the configured output, read live from the health endpoint:
# `true` / `false` / `unknown` (an unreachable endpoint reads `unknown`). ALWAYS returns 0.
cg_chain_songplayer_burn_state() {
  local body
  body="$(curl -fsS -m "${CG_CHAIN_BURN_TIMEOUT:-10}" "$(cg_chain_songplayer_health_url)" 2>/dev/null || true)"
  printf '%s' "$body" | cg_chain_health_burn_on "$(cg_chain_songplayer_output)"
  return 0
}

# ---- bounded polls (issue 1302 slice 3) ----------------------------------------------------------
#
# SongPlayer confirms a burn toggle on /health "within 1 s", and its obs-websocket facade mirrors a
# program cut to cg OBS asynchronously, so one read right after a request misses a state that is
# only late. Every read-back here is a POLL bounded by wall time, never a fixed number of reads.

# A non-negative integer env value $1, else the default $2. Pure.
_cg_chain_uint_or() {
  case "${1:-}" in
    '' | *[!0-9]*) printf '%s' "$2" ;;
    *) printf '%s' "$((10#$1))" ;;
  esac
}

# The burn read-back budget in seconds after an accepted toggle (env CG_CHAIN_BURN_READBACK_S,
# default 3 = three times SongPlayer's own "within 1 s"; 0 = one read). Pure.
cg_chain_burn_readback_secs() {
  _cg_chain_uint_or "${CG_CHAIN_BURN_READBACK_S:-}" 3
}

# The program read-back budget in seconds after a program cut (env CG_CHAIN_PROGRAM_READBACK_S,
# default 5: the facade's mirror to cg OBS is not awaited by SongPlayer; 0 = one read). Pure.
cg_chain_program_readback_secs() {
  _cg_chain_uint_or "${CG_CHAIN_PROGRAM_READBACK_S:-}" 5
}

# The interval of both polls in ms (env CG_CHAIN_READBACK_POLL_MS, default 500; 0 or garbage = 500).
# Pure.
cg_chain_readback_poll_ms() {
  local ms
  ms="$(_cg_chain_uint_or "${CG_CHAIN_READBACK_POLL_MS:-}" 500)"
  if [ "$ms" = 0 ]; then ms=500; fi
  printf '%s' "$ms"
}

# The wall clock in ms. Pure (reads the clock).
_cg_chain_now_ms() {
  date +%s%3N
}

# One more poll round fits the budget: returns 0 after sleeping the interval when the next read,
# $3 ms from now, still lands within $2 seconds of the start $1 (ms); returns 1 (no sleep) when it
# would not. Callers loop `read; match && break; _cg_chain_poll_next ... || break`.
_cg_chain_poll_next() {
  local start="$1" budget_s="$2" poll_ms="$3" now
  now="$(_cg_chain_now_ms)"
  [ $((now - start + poll_ms)) -le $((budget_s * 1000)) ] || return 1
  sleep "$((poll_ms / 1000)).$(printf '%03d' $((poll_ms % 1000)))"
  return 0
}

# Poll the configured output's `burn_on` until it reads $1 (true|false) or the burn read-back
# budget runs out. At least one read. Prints the last state read. ALWAYS returns 0.
cg_chain_songplayer_burn_poll() {
  local want="$1" budget poll start state
  budget="$(cg_chain_burn_readback_secs)"
  poll="$(cg_chain_readback_poll_ms)"
  start="$(_cg_chain_now_ms)"
  while :; do
    state="$(cg_chain_songplayer_burn_state)"
    [ "$state" != "$want" ] || break
    _cg_chain_poll_next "$start" "$budget" "$poll" || break
  done
  printf '%s' "$state"
  return 0
}

# The response body on STDIN as ONE log line: CR/LF folded to spaces, at most 200 characters. Pure.
cg_chain_http_body_line() {
  local s
  s="$(tr '\r\n' '  ')"
  printf '%s' "${s:0:200}"
}

# POST the burn toggle body $1 to $2 and print `<http code><TAB><body line>`. The code is `000`
# when no HTTP answer came back, and the body line then carries curl's own error. The body is kept
# for every code (no `-f`): a 404 / 409 says why. ALWAYS returns 0.
cg_chain_songplayer_burn_post() {
  local body="$1" url="$2" tmp code line
  if ! tmp="$(mktemp "${TMPDIR:-/tmp}/cg-sp-burn.XXXXXX")"; then
    printf '000\tcould not create a temp file for the answer'
    return 0
  fi
  code="$(curl -sS -o "$tmp" -w '%{http_code}' -m "${CG_CHAIN_BURN_TIMEOUT:-10}" -X POST \
    -H 'Content-Type: application/json' -d "$body" "$url" 2>"$tmp.err")" || true
  case "$code" in [0-9][0-9][0-9]) ;; *) code=000 ;; esac
  if [ "$code" = 000 ]; then
    line="$(cg_chain_http_body_line <"$tmp.err")"
  else
    line="$(cg_chain_http_body_line <"$tmp")"
  fi
  rm -f -- "$tmp" "$tmp.err"
  printf '%s\t%s' "$code" "$line"
  return 0
}

# The named reason of a burn toggle SongPlayer refused (HTTP $1), or nothing for any other code.
# 404 / 409 are final answers, never retried (SongPlayer's burn registry: 404 = no pipeline has that
# NDI output, 409 = the output is not paced, and a burn is painted only on the paced path). Pure.
cg_chain_burn_refusal_reason() {
  local out
  out="$(cg_chain_songplayer_output)"
  case "$1" in
    404) printf "HTTP 404: SongPlayer has no output '%s'" "$out" ;;
    409) printf "HTTP 409: pacing is disabled on '%s' (SongPlayer paints the burn only on the paced path)" "$out" ;;
  esac
  return 0
}

# Toggle the SongPlayer output burn $1 (on|off) and VERIFY it on the health endpoint. Up to
# CG_CHAIN_BURN_ATTEMPTS (default 3) POST rounds, CG_CHAIN_BURN_RETRY_SLEEP seconds apart (default
# 1). Every POST prints its own line: `[cg_chain] SongPlayer burn <on|off> attempt k: HTTP <code>
# <body>`. After a 2xx the health endpoint is POLLED (cg_chain_songplayer_burn_poll) until it reads
# the wanted state; after any other code one read decides (the state is authoritative over the
# answer); after no answer (000) nothing is read. A 404 / 409 is a named failure and is not retried.
# A verified toggle prints one VERIFIED line. An ON that never reads back true is a loud WARNING
# (the cg_chain section then proves nothing). An OFF that never reads back false is a loud LEAK line
# naming the manual off command — the burn must NEVER stay on the LED wall; an OFF answered 404
# owes nothing (no pipeline has that output, so no burn is painted anywhere). ALWAYS returns 0
# (never aborts the run or cleanup()).
cg_chain_songplayer_burn() {
  local action="$1" url body want attempts i tries=0 state=unknown resp code line reason=""
  url="$(cg_chain_songplayer_burn_url)"
  if ! body="$(cg_chain_songplayer_burn_body "$action")"; then
    echo "[cg_chain] WARNING: SongPlayer burn action '$action' is not on|off — nothing sent" >&2
    return 0
  fi
  # CG_SP_BURN_ON = 1 only after a VERIFIED ON (cg_chain_record_start turns the cg OBS hop burn on
  # only then); every other outcome of a sent toggle leaves it 0. CG_SP_BURN_OWED = "this run owes the
  # SongPlayer burn an OFF" (cleanup()'s first pass keys on it): 1 from the moment an ON is sent,
  # cleared only by a verified OFF, set again by an OFF that never verified.
  CG_SP_BURN_ON=0
  if [ "$action" = on ]; then
    CG_SP_BURN_OWED=1
    want=true
  else
    want=false
  fi
  attempts="${CG_CHAIN_BURN_ATTEMPTS:-3}"
  case "$attempts" in '' | *[!0-9]* | 0) attempts=3 ;; esac
  for ((i = 1; i <= attempts; i++)); do
    tries="$i"
    resp="$(cg_chain_songplayer_burn_post "$body" "$url")"
    code="${resp%%$'\t'*}"
    line="${resp#*$'\t'}"
    echo "[cg_chain] SongPlayer burn $action attempt $i: HTTP $code${line:+ $line}"
    reason="$(cg_chain_burn_refusal_reason "$code")"
    case "$code" in
      2??) state="$(cg_chain_songplayer_burn_poll "$want")" ;;
      000) state=unknown ;;
      *) state="$(cg_chain_songplayer_burn_state)" ;;
    esac
    if [ "$state" = "$want" ]; then
      if [ "$action" = on ]; then CG_SP_BURN_ON=1; else CG_SP_BURN_OWED=0; fi
      echo "[cg_chain] SongPlayer burn $action VERIFIED (burn_on=$state on $(cg_chain_songplayer_output), $(cg_chain_songplayer_health_url))"
      return 0
    fi
    if [ -n "$reason" ]; then break; fi
    if [ "$i" -lt "$attempts" ]; then sleep "${CG_CHAIN_BURN_RETRY_SLEEP:-1}"; fi
  done
  if [ "$action" = on ]; then
    if [ -n "$reason" ]; then
      echo "[cg_chain] WARNING: SongPlayer refused the burn ON — $reason; not retried (burn_on=$state) — the cg_chain section will prove nothing this run" >&2
    else
      echo "[cg_chain] WARNING: SongPlayer burn ON not confirmed after $tries attempt(s) (burn_on=$state on $(cg_chain_songplayer_output)) — the cg_chain section will prove nothing this run" >&2
    fi
  elif [ "$code" = 404 ]; then
    CG_SP_BURN_OWED=0
    echo "[cg_chain] SongPlayer burn off: nothing to turn off — $reason, so no burn is painted on it"
  else
    CG_SP_BURN_OWED=1
    echo "[cg_chain] LEAK: SongPlayer burn still not OFF after $tries attempt(s)${reason:+ ($reason; not retried)} (burn_on=$state on $(cg_chain_songplayer_output)) — the burn may still be on the LED wall; turn it off: curl -X POST -H 'Content-Type: application/json' -d '$body' $url" >&2
  fi
  return 0
}

# ---- (b2) SongPlayer's OWN program, cut through its obs-websocket facade (issue 1302 slice 3) ----
#
# Since SongPlayer 221 L4b (live 29.9.2026) SongPlayer plays what is on ITS OWN program: a cg OBS
# program cut over :4455 no longer starts or pauses anything in SongPlayer. Its obs-websocket FACADE
# (:4456, the same `SetCurrentProgramScene` Companion presses) is the way in: SongPlayer cuts
# SP-program to that playlist FIRST, then mirrors the same scene to cg OBS (queued, not awaited). So
# the record start cuts cg OBS over :4455 first (the hard cut + its own snapshot, unchanged), then
# presses the facade, then reads BOTH programs back; the cleanup restores SongPlayer's program
# through the facade before the cg OBS :4455 restore puts cg OBS back exactly as it was.

# SongPlayer's program API: GET = `source` (the playlist id on SP-program) + `remote.program_scene`
# (the scene name the facade answers with). Pure.
cg_chain_songplayer_program_url() {
  printf '%s/api/v1/program' "$(cg_chain_songplayer_api_base)"
}

# The facade port (env CG_CHAIN_SP_FACADE_PORT, default 4456). Pure.
cg_chain_sp_facade_port() {
  local p
  p="$(_cg_chain_uint_or "${CG_CHAIN_SP_FACADE_PORT:-}" 4456)"
  if [ "$p" = 0 ]; then p=4456; fi
  printf '%s' "$p"
}

# Read a GET /api/v1/program document on STDIN and print `<scene><TAB><source>`: the
# `remote.program_scene` ('' when null / absent) and the `source` ('' when null / absent). A
# document that does not parse as a JSON object prints nothing and returns 1. Pure.
cg_chain_program_fields() {
  python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(1)
if not isinstance(doc, dict):
    sys.exit(1)
remote = doc.get("remote") if isinstance(doc.get("remote"), dict) else {}
scene = remote.get("program_scene")
source = doc.get("source")
scene = scene if isinstance(scene, str) else ""
source = str(source) if isinstance(source, int) and not isinstance(source, bool) else ""
print(scene + "\t" + source, end="")
' 2>/dev/null
}

# SongPlayer's live program as `<scene><TAB><source>`; returns 1 (prints nothing) when the program
# API is unreachable or answers something unreadable. MUST be called from an `if`.
cg_chain_songplayer_program_read() {
  local doc
  doc="$(curl -fsS -m "${CG_CHAIN_BURN_TIMEOUT:-10}" "$(cg_chain_songplayer_program_url)" 2>/dev/null)" || return 1
  printf '%s' "$doc" | cg_chain_program_fields
}

# The facade session's password is CG_CHAIN_SP_FACADE_PASSWORD, read from the environment by the
# scene helper itself (never on an argv).

# Put SongPlayer's program on the cg scene through the facade. SongPlayer's program is read FIRST
# and snapshotted (cg_chain_state_file sp-program: host, port, scene, source) before the press; an
# unreadable program = no press (a mutation is never made without a snapshot to undo it); a program
# already on the scene = no press and no snapshot (nothing to undo). $1=cg-host $2=obs_phase2.py
# $3=timeout-secs. Returns 0 when SongPlayer's program is on (or was pressed to) the scene, 1 with a
# WARNING otherwise. MUST be called from an `if`.
cg_chain_sp_program_select() {
  local host="$1" py="$2" tmo="${3:-30}" scene port state prog prev_scene prev_source
  scene="$(cg_chain_cg_scene)"
  port="$(cg_chain_sp_facade_port)"
  state="$(cg_chain_state_file sp-program)"
  if ! prog="$(cg_chain_songplayer_program_read)"; then
    echo "[cg_chain] WARNING: could not read SongPlayer's program ($(cg_chain_songplayer_program_url)) — not cut through its facade (no snapshot to restore from)" >&2
    return 1
  fi
  prev_scene="${prog%%$'\t'*}"
  prev_source="${prog#*$'\t'}"
  if [ "$prev_scene" = "$scene" ]; then
    echo "[cg_chain] SongPlayer program already on '$scene' (source ${prev_source:-none}) — no facade cut"
    return 0
  fi
  if ! python3 -c '
import json, sys
host, port, scene, source, path = sys.argv[1:6]
state = {"host": host, "port": int(port), "scene": scene or None,
         "source": int(source) if source else None}
tmp = path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(state, f)
import os
os.replace(tmp, path)
' "$host" "$port" "$prev_scene" "$prev_source" "$state"; then
    echo "[cg_chain] WARNING: could not write the SongPlayer program snapshot ($state) — not cut through its facade" >&2
    return 1
  fi
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" facade-program --host "$host" --port "$port" \
    --scene "$scene" >/dev/null; then
    echo "[cg_chain] SongPlayer facade ($host:$port) program -> '$scene' OK (was '${prev_scene:-none}', source ${prev_source:-none})"
    return 0
  fi
  echo "[cg_chain] WARNING: SongPlayer facade ($host:$port) cut to '$scene' failed — SongPlayer may not play '$scene' in the cg recording" >&2
  return 1
}

# Read BOTH programs back after the cuts until they agree with the cg scene: SongPlayer's
# `remote.program_scene` (GET /api/v1/program) and cg OBS's program scene over :4455
# (obs_phase2.py program-scene, each call under cg_chain_burn_obs_timeout). Polled every
# cg_chain_readback_poll_ms for up to cg_chain_program_readback_secs (the facade's mirror to cg OBS
# is not awaited by SongPlayer). $1=cg-host $2=obs_phase2.py. Returns 0 on a match; 1 with a loud
# WARNING naming both reads otherwise — the caller keeps the cg burn OFF. MUST be called from an `if`.
cg_chain_program_readback() {
  local host="$1" py="$2" scene budget poll start prog sp cg pw=()
  scene="$(cg_chain_cg_scene)"
  budget="$(cg_chain_program_readback_secs)"
  poll="$(cg_chain_readback_poll_ms)"
  if [ -n "${CG_CHAIN_OBS_PASSWORD:-}" ]; then pw=(--password "$CG_CHAIN_OBS_PASSWORD"); fi
  start="$(_cg_chain_now_ms)"
  while :; do
    sp=""
    if prog="$(cg_chain_songplayer_program_read)"; then sp="${prog%%$'\t'*}"; fi
    cg="$(timeout "$(cg_chain_burn_obs_timeout)" python3 "$py" program-scene --host "$host" \
      ${pw[@]+"${pw[@]}"} 2>/dev/null | tail -n 1)" || cg=""
    if [ "$sp" = "$scene" ] && [ "$cg" = "$scene" ]; then
      echo "[cg_chain] programs read back: SongPlayer '$sp' + cg OBS ($host) '$cg'"
      return 0
    fi
    _cg_chain_poll_next "$start" "$budget" "$poll" || break
  done
  echo "[cg_chain] WARNING: program mismatch after the cut — SongPlayer program '${sp:-unreadable}', cg OBS program '${cg:-unreadable}', want '$scene' — the cg burn stays OFF this run" >&2
  return 1
}

# Restore SongPlayer's program from this run's snapshot through the facade and retire the snapshot
# (renamed `.restored`, so the cleanup() second pass is a no-op). No snapshot = nothing to do; a
# program already back on the snapshotted scene is not pressed again. A snapshot with no scene
# (nothing on SP-program, or a playlist whose catalog names no scene) cannot be pressed through the
# facade: a WARNING naming the dashboard cut that restores it, snapshot kept. A failed press keeps
# the snapshot and names the manual command. $1=obs_phase2.py $2=timeout-secs. ALWAYS returns 0.
cg_chain_sp_program_restore() {
  local py="$1" tmo="${2:-30}" state fields host port scene source prog
  state="$(cg_chain_state_file sp-program)"
  [ -f "$state" ] || return 0
  if ! fields="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
src = d.get("source")
print("\x1f".join([str(d["host"]), str(d["port"]), d.get("scene") or "", "" if src is None else str(src)]), end="")
' "$state" 2>/dev/null)"; then
    echo "[cg_chain] WARNING: SongPlayer program snapshot $state is unreadable — SongPlayer's program NOT restored; check it at $(cg_chain_songplayer_program_url)" >&2
    return 0
  fi
  # A unit separator, never a tab: a tab is IFS whitespace, so an empty scene would collapse.
  IFS=$'\x1f' read -r host port scene source <<<"$fields"
  if [ -z "$scene" ]; then
    echo "[cg_chain] WARNING: SongPlayer had no scene on its program before this run (source ${source:-none}) — the facade cannot put it back; restore it from the dashboard: curl -X POST -H 'Content-Type: application/json' -d '{\"source\":${source:-<playlist id>}}' $(cg_chain_songplayer_api_base)/api/v1/program/cut (snapshot kept at $state)" >&2
    return 0
  fi
  if prog="$(cg_chain_songplayer_program_read)" && [ "${prog%%$'\t'*}" = "$scene" ]; then
    mv -f -- "$state" "$state.restored"
    echo "[cg_chain] SongPlayer program already back on '$scene'"
    return 0
  fi
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" facade-program --host "$host" --port "$port" \
    --scene "$scene" >/dev/null; then
    mv -f -- "$state" "$state.restored"
    echo "[cg_chain] SongPlayer program restored -> '$scene' (facade $host:$port)"
  else
    echo "[cg_chain] WARNING: SongPlayer program restore through the facade failed — snapshot kept at $state (restore by hand: python3 $(cg_chain_scene_py "$py") facade-program --host $host --port $port --scene '$scene')" >&2
  fi
  return 0
}
