#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the guarded
# win-ssh-exec.sh source below, a no-op when the caller already sourced it) — matches the
# sibling scripts/lib/*.sh convention (cold-cut-step.sh, camera-box-restart-verify.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, and
# recording-e2e.sh (the only caller) already sets it. Every function ALWAYS `return 0` on its
# best-effort (runtime) paths so a no-op / failed branch can never trip the caller's `set -e`
# (the sourced-`set -e`-leak class, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cg-chain-e2e.sh — #1301/#1302 opt-in CG_CHAIN=1 E2E profile for the
# SongPlayer-originated content chain (SongPlayer -> cg OBS (RESOLUME-SNV) -> strih -> stream).
# OFF BY DEFAULT (CG_CHAIN unset/0 ⇒ every function the harness calls is a pure no-op, so a normal
# E2E run is byte-for-byte inert). Invoked from recording-e2e.sh with the #675 sourced-lib pattern —
# the CG_CHAIN-guarded call lines are added AFTER existing anchored lines, never editing one.
#
# What the profile does when CG_CHAIN=1 (ALL best-effort + loud, never a camera-chain abort):
#   - at [5/8]: turn the SongPlayer output burn ON through the SHIPPED API (POST
#     {base}/api/v1/ndi/burn {"output":"SP-fast","on":true}) and READ IT BACK from
#     {base}/api/v1/ndi/health (`burn_on` of that output), cut cg OBS program to the scene carrying
#     that output (`sp-fast`), and StartRecord cg OBS over OBS-WS.
#   - after the camera sweep/hold, BEFORE [7/8] StopRecord: ONE tail CG window — strih program is
#     cut to the scene carrying the `CG-obs` input (only that item shown), so the strih + stream
#     recordings carry the CG chain. It is a TAIL window on purpose: a window inside
#     switch-schedule.json would have no cam2 tick (a zero-frame cambox window FAILS) and a mid-run
#     cut would land inside the optical span — the camera-chain verdict must stay untouched.
#   - right after [7/8] StopRecord: strih's scene + program are restored.
#   - at [8/8d]: StopRecord cg OBS (keeping the StopRecord host path) and scp that exact file to
#     dev1 (or run the operator's CG_CHAIN_PULL_CMD), fed to the merge as `--cg <path>`, which emits
#     the REPORT-ONLY cg_chain section (src/cg_chain_gate.rs; never changes overall_pass).
#   - in cleanup(): the #246/#844 leak-guard — burn OFF (verified on /health, retried, a loud LEAK
#     line if it never reads false), StopRecord cg OBS, and every scene snapshot restored, even on
#     an early abort.

# win_ssh_scp_source_path (the backslash-to-slash scp source fix) lives in win-ssh-exec.sh, which
# recording-e2e.sh sources earlier; source it here too when a caller (a test) did not.
if ! declare -F win_ssh_scp_source_path >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/win-ssh-exec.sh
  . "$(dirname "${BASH_SOURCE[0]}")/win-ssh-exec.sh"
fi

# True iff the CG_CHAIN profile is enabled for this run. Pure, no side effects.
cg_chain_enabled() {
  [ "${CG_CHAIN:-0}" = "1" ]
}

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

# Toggle the SongPlayer output burn $1 (on|off) and VERIFY it on the health endpoint. Up to
# CG_CHAIN_BURN_ATTEMPTS (default 3) POST + read-back rounds, CG_CHAIN_BURN_RETRY_SLEEP seconds
# apart (default 1). A verified toggle prints one VERIFIED line. An ON that never reads back true is
# a loud WARNING (the cg_chain section then proves nothing). An OFF that never reads back false is a
# loud LEAK line naming the manual off command — the burn must NEVER stay on the LED wall. ALWAYS
# returns 0 (never aborts the run or cleanup()).
cg_chain_songplayer_burn() {
  local action="$1" url body want attempts i state=unknown
  url="$(cg_chain_songplayer_burn_url)"
  if ! body="$(cg_chain_songplayer_burn_body "$action")"; then
    echo "[cg_chain] WARNING: SongPlayer burn action '$action' is not on|off — nothing sent" >&2
    return 0
  fi
  if [ "$action" = on ]; then want=true; else want=false; fi
  attempts="${CG_CHAIN_BURN_ATTEMPTS:-3}"
  case "$attempts" in '' | *[!0-9]* | 0) attempts=3 ;; esac
  for ((i = 1; i <= attempts; i++)); do
    curl -fsS -m "${CG_CHAIN_BURN_TIMEOUT:-10}" -X POST -H 'Content-Type: application/json' \
      -d "$body" "$url" >/dev/null 2>&1 || true
    state="$(cg_chain_songplayer_burn_state)"
    if [ "$state" = "$want" ]; then
      echo "[cg_chain] SongPlayer burn $action VERIFIED (burn_on=$state on $(cg_chain_songplayer_output), $(cg_chain_songplayer_health_url))"
      return 0
    fi
    if [ "$i" -lt "$attempts" ]; then sleep "${CG_CHAIN_BURN_RETRY_SLEEP:-1}"; fi
  done
  if [ "$action" = on ]; then
    echo "[cg_chain] WARNING: SongPlayer burn ON not confirmed after $attempts attempt(s) (burn_on=$state on $(cg_chain_songplayer_output)) — the cg_chain section will prove nothing this run" >&2
  else
    echo "[cg_chain] LEAK: SongPlayer burn still not OFF after $attempts attempt(s) (burn_on=$state on $(cg_chain_songplayer_output)) — the burn may still be on the LED wall; turn it off: curl -X POST -H 'Content-Type: application/json' -d '$body' $url" >&2
  fi
  return 0
}

# ---- hosts, scenes, state files ----------------------------------------------------------------

# Resolve the cg OBS (RESOLUME-SNV) host IP from DNS. PRINTS the IP on success; returns nonzero +
# a loud stderr line on failure (the caller treats a resolve failure as "skip the cg leg", never
# an abort). `$1` overrides the name (default resolume.lan).
cg_chain_resolve_host() {
  local name="${1:-${CG_CHAIN_HOST:-resolume.lan}}"
  local ip
  ip="$(getent hosts "$name" 2>/dev/null | awk '{print $1; exit}')"
  if [ -z "$ip" ]; then
    echo "[cg_chain] WARNING: could not resolve cg OBS host '$name' (getent hosts) — skipping the CG leg this run" >&2
    return 1
  fi
  printf '%s' "$ip"
}

# The cg OBS scene carrying the SongPlayer output (env CG_CHAIN_CG_SCENE, default the lower-cased
# output name — `SP-fast` -> `sp-fast`, the live cg_scenes naming). Pure.
cg_chain_cg_scene() {
  local out
  out="$(cg_chain_songplayer_output)"
  printf '%s' "${CG_CHAIN_CG_SCENE:-${out,,}}"
}

# The restore-snapshot path for kind $1 (cg-program | strih-scene), in CG_CHAIN_STATE_DIR (the
# run's OUTDIR; falls back to TMPDIR). Pure.
cg_chain_state_file() {
  printf '%s/cg-chain-%s-state.json' "${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}" "$1"
}

# The scene helper that sits next to obs_phase2.py ($1 = the obs_phase2.py path). Pure.
cg_chain_scene_py() {
  printf '%s/cg_chain_scene.py' "$(dirname "$1")"
}

# ---- cg OBS record leg ---------------------------------------------------------------------------

# Cut cg OBS program to the scene carrying the SongPlayer output, snapshotting the previous program
# first (restored in cleanup()). $1=host-ip $2=path-to-obs_phase2.py $3=timeout-secs. BEST-EFFORT +
# loud; ALWAYS return 0.
cg_chain_cg_program_select() {
  local host="$1" py="$2" tmo="${3:-30}" scene
  scene="$(cg_chain_cg_scene)"
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" program --host "$host" --scene "$scene" \
    --state-file "$(cg_chain_state_file cg-program)" >/dev/null; then
    echo "[cg_chain] cg OBS ($host) program -> '$scene' OK"
  else
    echo "[cg_chain] WARNING: cg OBS ($host) program cut to '$scene' failed — the cg recording may not carry the SongPlayer burn" >&2
  fi
  return 0
}

# Cut cg OBS program to the SongPlayer scene, then StartRecord cg OBS over OBS-WS
# (obs_phase2.py record --host <ip> --action start). Args: $1=host-ip $2=path-to-obs_phase2.py
# $3=timeout-secs. Returns 0 on a started recording (caller then sets CG_RECORDING_STARTED=1 so
# cleanup() StopRecords this box), 1 on failure. MUST be called from an `if` (the nonzero return is
# the failure signal, not an abort — never call it bare under set -e).
cg_chain_record_start() {
  local host="$1" py="$2" tmo="${3:-30}"
  cg_chain_cg_program_select "$host" "$py" "$tmo"
  if timeout "$tmo" python3 "$py" record --host "$host" --action start >/dev/null 2>&1; then
    echo "[cg_chain] cg OBS ($host) StartRecord OK"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS ($host) StartRecord failed — the cg_chain section will be omitted this run" >&2
  return 1
}

# StopRecord cg OBS over OBS-WS and KEEP the StopRecord host path (obs_phase2.py prints it as its
# only stdout line) in CG_HOST_RECORDING_PATH for the pull. An empty answer (already stopped — the
# cleanup() pass after [8/8d]) never clears a path an earlier stop recorded. Args: $1=host-ip
# $2=path-to-obs_phase2.py $3=timeout-secs. BEST-EFFORT + loud; ALWAYS return 0.
cg_chain_record_stop() {
  local host="$1" py="$2" tmo="${3:-30}" path
  if path="$(timeout "$tmo" python3 "$py" record --host "$host" --action stop 2>/dev/null)"; then
    path="$(printf '%s\n' "$path" | tail -n 1)"
    if [ -n "$path" ]; then CG_HOST_RECORDING_PATH="$path"; fi
    echo "[cg_chain] cg OBS ($host) StopRecord OK${path:+ -> $path}"
  else
    echo "[cg_chain] WARNING: cg OBS ($host) StopRecord failed (best-effort)" >&2
  fi
  return 0
}

# ---- (b) the cg OBS recording pull --------------------------------------------------------------

# The scp SOURCE spec for a Windows host path: `<user>@<host>:<path with / separators>` (the
# win_ssh_scp_source_path fix — a backslash scp source reads "No such file"). Spaces stay as they
# are: the arg reaches scp as ONE argv entry. Pure.
cg_chain_pull_source_spec() {
  printf '%s@%s:%s' "$1" "$2" "$(win_ssh_scp_source_path "$3")"
}

# Pull the cg OBS recording to the local path $2. With CG_CHAIN_PULL_CMD set, that operator command
# runs (with CG_HOST_IP, CG_HOST_PATH and CG_RECORDING exported). Otherwise the DEFAULT: scp the
# exact StopRecord file (CG_HOST_RECORDING_PATH) from `${CG_CHAIN_USER:-newlevel}@$1`, bounded by
# CG_CHAIN_PULL_TIMEOUT (default 900 s). $1=cg-host-ip $2=local-dest-path. Returns 0 iff the
# destination file exists afterwards. MUST be called from an `if` (nonzero = "no cg recording this
# run, omit --cg").
cg_chain_pull_recording() {
  local host="$1" dest="$2"
  local cmd="${CG_CHAIN_PULL_CMD:-}" hostpath="${CG_HOST_RECORDING_PATH:-}" spec
  if [ -n "$cmd" ]; then
    if CG_HOST_IP="$host" CG_HOST_PATH="$hostpath" CG_RECORDING="$dest" bash -c "$cmd" >/dev/null 2>&1 && [ -f "$dest" ]; then
      echo "[cg_chain] cg OBS recording pulled to $dest (CG_CHAIN_PULL_CMD)"
      return 0
    fi
    echo "[cg_chain] WARNING: cg OBS recording pull failed (CG_CHAIN_PULL_CMD) — omitting --cg this run" >&2
    return 1
  fi
  if [ -z "$hostpath" ]; then
    echo "[cg_chain] WARNING: no cg OBS recording path from StopRecord — nothing to pull, omitting --cg this run" >&2
    return 1
  fi
  spec="$(cg_chain_pull_source_spec "${CG_CHAIN_USER:-newlevel}" "$host" "$hostpath")"
  if timeout "${CG_CHAIN_PULL_TIMEOUT:-900}" sshpass -p "${CG_CHAIN_PW:-newlevel}" scp \
    -o StrictHostKeyChecking=no -o ConnectTimeout=10 "$spec" "$dest" >/dev/null 2>&1 && [ -f "$dest" ]; then
    echo "[cg_chain] cg OBS recording pulled to $dest ($(du -h "$dest" 2>/dev/null | cut -f1) from $spec)"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS recording scp failed ($spec) — omitting --cg this run" >&2
  return 1
}

# ---- (c) the ONE tail CG window on strih --------------------------------------------------------

# True iff the tail CG window should run: the profile is on AND the cg OBS recording started (no
# cg recording = nothing to judge, so strih is never cut). Pure.
cg_chain_window_due() {
  cg_chain_enabled && [ "${CG_RECORDING_STARTED:-0}" = 1 ]
}

# The CG window length in seconds (env CG_CHAIN_WINDOW_SECS, default 30; a non-positive or
# non-integer value falls back to 30). Pure.
cg_chain_window_secs() {
  case "${CG_CHAIN_WINDOW_SECS:-}" in
    '' | *[!0-9]* | 0) printf '30' ;;
    *) printf '%s' "$CG_CHAIN_WINDOW_SECS" ;;
  esac
}

# The ONE CG window record: {"kind":"cg","scene":…,"input":…,"start_ns":…,"end_ns":…}. Returns 1
# (prints nothing) unless start_ns < end_ns are integers. Pure.
cg_chain_window_json() {
  python3 -c '
import json, sys
scene, inp, s, e = sys.argv[1:5]
try:
    s, e = int(s), int(e)
except ValueError:
    sys.exit(1)
if s >= e:
    sys.exit(1)
print(json.dumps({"kind": "cg", "scene": scene, "input": inp, "start_ns": s, "end_ns": e}, separators=(",", ":")))
' "$1" "$2" "$3" "$4"
}

# The tail CG window: cut strih program to the scene carrying CG_CHAIN_STRIH_INPUT (default
# `CG-obs`; CG_CHAIN_STRIH_SCENE picks one when several carry it) with only that item shown, hold
# it for cg_chain_window_secs, and write the window to $CG_CHAIN_STATE_DIR/cg-window.json. The
# strih snapshot is restored by cg_chain_strih_restore (right after [7/8] StopRecord) and again by
# cleanup(). $1=strih-host $2=path-to-obs_phase2.py $3=timeout-secs. A pure no-op unless
# cg_chain_window_due; BEST-EFFORT + loud; ALWAYS return 0.
cg_chain_window() {
  cg_chain_window_due || return 0
  local strih="$1" py="$2" tmo="${3:-30}" input out start_ns scene secs end_ns json
  input="${CG_CHAIN_STRIH_INPUT:-CG-obs}"
  secs="$(cg_chain_window_secs)"
  if ! out="$(timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" strih-solo --host "$strih" \
    --input "$input" --scene "${CG_CHAIN_STRIH_SCENE:-}" --state-file "$(cg_chain_state_file strih-scene)")"; then
    echo "[cg_chain] WARNING: strih CG cut failed (input '$input') — strih/stream will not carry the CG chain this run" >&2
    return 0
  fi
  start_ns="${out%%$'\t'*}"
  scene="${out#*$'\t'}"
  echo "[6/8] #1302 CG window: strih program -> '$scene' ('$input' only) for ${secs}s, cut at ${start_ns} ns"
  if declare -F interruptible_sleep >/dev/null 2>&1; then interruptible_sleep "$secs"; else sleep "$secs"; fi
  end_ns="$(date +%s%N)"
  if json="$(cg_chain_window_json "$scene" "$input" "$start_ns" "$end_ns")"; then
    printf '%s\n' "$json" >"${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}/cg-window.json" \
      && echo "    wrote CG window -> ${CG_CHAIN_STATE_DIR:-${TMPDIR:-/tmp}}/cg-window.json"
  else
    echo "[cg_chain] WARNING: bad CG window bounds (start=$start_ns end=$end_ns) — window record not written" >&2
  fi
  return 0
}

# Restore one snapshot kind $1 through the scene helper ($2 = obs_phase2.py path, $3 = timeout).
# No snapshot file = nothing to do. ALWAYS return 0.
cg_chain_restore_snapshot() {
  local kind="$1" py="$2" tmo="${3:-30}" state
  state="$(cg_chain_state_file "$kind")"
  [ -f "$state" ] || return 0
  if timeout "$tmo" python3 "$(cg_chain_scene_py "$py")" restore --state-file "$state" >/dev/null; then
    echo "[cg_chain] $kind restored"
  else
    echo "[cg_chain] WARNING: $kind restore failed — snapshot kept at $state (restore by hand: python3 $(cg_chain_scene_py "$py") restore --state-file $state)" >&2
  fi
  return 0
}

# Restore strih's CG-window scene + program right after [7/8] StopRecord (cleanup() retries it).
# $1=path-to-obs_phase2.py $2=timeout-secs. A pure no-op unless CG_CHAIN=1; ALWAYS return 0.
cg_chain_strih_restore() {
  cg_chain_enabled || return 0
  cg_chain_restore_snapshot strih-scene "$1" "${2:-30}"
}

# cleanup() leak-guard: burn OFF (verified), StopRecord cg OBS, and every scene snapshot restored,
# even on an early abort. A pure no-op when CG_CHAIN is not enabled (so it is safe to call
# unconditionally from cleanup()). Args: $1=cg-host-ip-or-empty $2=path-to-obs_phase2.py
# $3=timeout-secs. ALWAYS return 0.
cg_chain_cleanup() {
  cg_chain_enabled || return 0
  local host="$1" py="$2" tmo="${3:-30}"
  cg_chain_songplayer_burn off
  if [ -n "$host" ]; then
    cg_chain_record_stop "$host" "$py" "$tmo"
  fi
  cg_chain_restore_snapshot strih-scene "$py" "$tmo"
  cg_chain_restore_snapshot cg-program "$py" "$tmo"
  return 0
}
