#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements) — matches the
# sibling scripts/lib/*.sh convention (cold-cut-step.sh, camera-box-restart-verify.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, and
# recording-e2e.sh (the only caller) already sets it. Every function ALWAYS `return 0` on its
# best-effort (runtime) paths so a no-op / failed branch can never trip the caller's `set -e`
# (the sourced-`set -e`-leak class, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cg-chain-e2e.sh — #1301 opt-in CG_CHAIN=1 E2E profile for the SongPlayer-originated
# content chain (SongPlayer -> cg OBS (RESOLUME-SNV) -> strih -> stream). OFF BY DEFAULT
# (CG_CHAIN unset/0 ⇒ every function the harness calls is a pure no-op, so a normal E2E run is
# byte-for-byte inert). Invoked from recording-e2e.sh with the #675 sourced-lib pattern — the
# CG_CHAIN-guarded call lines are added AFTER existing anchored lines, never editing one.
#
# What the profile does when CG_CHAIN=1 (ALL best-effort + loud, UNVERIFIED until songplayer#151):
#   - at [5/8]: turn the SongPlayer output burn ON over its API, and StartRecord cg OBS over OBS-WS.
#   - in cleanup(): turn the SongPlayer burn OFF (the #246/#844 leak-guard class — the burn must
#     NEVER stay on the LED wall) and StopRecord cg OBS, even on an early abort.
#   - pull the cg OBS recording to dev1 and feed it to the verdict MERGE as `--cg <path>`, which
#     emits the REPORT-ONLY cg_chain section (src/cg_chain_gate.rs; never changes overall_pass).
#
# UNVERIFIED: the SongPlayer burn half is zbynekdrlik/songplayer#151 (contract #1294 §8) and has
# NOT shipped, so the burn-ON/OFF API is env-configurable and its failure is a LOUD warning, never
# a run abort. A real CG_CHAIN=1 run is a supervisor/rig-ops step once songplayer#151 lands.

# True iff the CG_CHAIN profile is enabled for this run. Pure, no side effects.
cg_chain_enabled() {
  [ "${CG_CHAIN:-0}" = "1" ]
}

# The SongPlayer burn-toggle URL for action $1 (on|off). PURE (no network) so it is Tier-0
# unit-testable. The base is env-configurable (CG_CHAIN_SONGPLAYER_API) because the exact endpoint
# lands with songplayer#151 / contract #1294 §8; the default is a best-effort guess.
cg_chain_songplayer_burn_url() {
  local action="$1"
  local base="${CG_CHAIN_SONGPLAYER_API:-http://songplayer.lan:8099}"
  printf '%s/burn/%s' "$base" "$action"
}

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

# Turn the SongPlayer output burn $1 (on|off) via its API. BEST-EFFORT: a failure is a loud
# warning, never an abort (ALWAYS return 0) — the SongPlayer sender is songplayer#151, unshipped.
cg_chain_songplayer_burn() {
  local action="$1"
  local url
  url="$(cg_chain_songplayer_burn_url "$action")"
  if curl -fsS -m "${CG_CHAIN_BURN_TIMEOUT:-10}" -X POST "$url" >/dev/null 2>&1; then
    echo "[cg_chain] SongPlayer burn $action OK ($url)"
  else
    echo "[cg_chain] WARNING: SongPlayer burn $action failed ($url) — UNVERIFIED until songplayer#151 lands; continuing" >&2
  fi
  return 0
}

# StartRecord cg OBS over OBS-WS (obs_phase2.py record --host <ip> --action start). Args:
# $1=host-ip $2=path-to-obs_phase2.py $3=timeout-secs. Returns 0 on success (caller then sets
# CG_RECORDING_STARTED=1 so cleanup() StopRecords this box), 1 on failure. MUST be called from an
# `if` (the nonzero return is the failure signal, not an abort — never call it bare under set -e).
cg_chain_record_start() {
  local host="$1" py="$2" tmo="${3:-30}"
  if timeout "$tmo" python3 "$py" record --host "$host" --action start >/dev/null 2>&1; then
    echo "[cg_chain] cg OBS ($host) StartRecord OK"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS ($host) StartRecord failed — the cg_chain section will be omitted this run" >&2
  return 1
}

# StopRecord cg OBS over OBS-WS. Args: $1=host-ip $2=path-to-obs_phase2.py $3=timeout-secs.
# BEST-EFFORT + loud; ALWAYS return 0 (cleanup() must never abort on it).
cg_chain_record_stop() {
  local host="$1" py="$2" tmo="${3:-30}"
  if timeout "$tmo" python3 "$py" record --host "$host" --action stop >/dev/null 2>&1; then
    echo "[cg_chain] cg OBS ($host) StopRecord OK"
  else
    echo "[cg_chain] WARNING: cg OBS ($host) StopRecord failed (best-effort)" >&2
  fi
  return 0
}

# Pull the cg OBS recording to the local path $2, by running the operator-configured
# CG_CHAIN_PULL_CMD (the resolume box's recording scp — a supervisor/rig-ops detail that lands with
# the live CG_CHAIN run; UNSET by default, since the resolume recording path + transport are not
# established until songplayer#151). $1=cg-host-ip $2=local-dest-path. The command is run with
# CG_HOST_IP + CG_RECORDING exported so it can reference them. Returns 0 iff the destination file
# exists afterwards. MUST be called from an `if` (nonzero = "no cg recording this run, omit --cg").
cg_chain_pull_recording() {
  local host="$1" dest="$2"
  local cmd="${CG_CHAIN_PULL_CMD:-}"
  if [ -z "$cmd" ]; then
    echo "[cg_chain] cg OBS recording pull not configured (set CG_CHAIN_PULL_CMD to the resolume scp, pending songplayer#151) — omitting --cg this run" >&2
    return 1
  fi
  if CG_HOST_IP="$host" CG_RECORDING="$dest" bash -c "$cmd" >/dev/null 2>&1 && [ -f "$dest" ]; then
    echo "[cg_chain] cg OBS recording pulled to $dest"
    return 0
  fi
  echo "[cg_chain] WARNING: cg OBS recording pull failed (CG_CHAIN_PULL_CMD) — omitting --cg this run" >&2
  return 1
}

# cleanup() leak-guard: turn the SongPlayer burn OFF and StopRecord cg OBS, even on an early abort.
# A pure no-op when CG_CHAIN is not enabled (so it is safe to call unconditionally from cleanup()).
# Args: $1=cg-host-ip-or-empty $2=path-to-obs_phase2.py $3=timeout-secs. ALWAYS return 0.
cg_chain_cleanup() {
  cg_chain_enabled || return 0
  local host="$1" py="$2" tmo="${3:-30}"
  cg_chain_songplayer_burn off
  if [ -n "$host" ]; then
    cg_chain_record_stop "$host" "$py" "$tmo"
  fi
  return 0
}
