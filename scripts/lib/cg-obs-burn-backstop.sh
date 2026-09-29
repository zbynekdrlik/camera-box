#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the guarded
# obs-fleet.sh source below) — the scripts/lib/*.sh convention of NOT setting `set -euo pipefail` in
# a sourced file: it runs in the CALLER's shell (rig-mode.sh / recording-e2e.sh, which set it).
#
# scripts/lib/cg-obs-burn-backstop.sh — issue 1302 slice 3: the home-gated cg OBS burn backstop.
# Sourced by rig-mode.sh (the EVENT sweeps) and, through scripts/lib/cg-chain-e2e.sh, by
# recording-e2e.sh (the pre-run normalize). The rule: .claude/rules/cg-burn-node-role.md.

# The backstop asks the ONE obs-fleet list whether the traveling cg OBS box is home. Lazy-sourced (the
# e2e-win-baseline.sh idiom), so a caller that already has it is not re-sourced.
if ! declare -F obs_fleet_is_home >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/obs-fleet.sh
  . "$(dirname "${BASH_SOURCE[0]}")/obs-fleet.sh"
fi

# ---- (e) the home-gated cg OBS burn backstop (issue 1302 slice 3) -------------------------------
#
# The cg OBS hop burn (911015) is saved in the cg OBS scene collection, so it survives an OBS / AHK
# respawn, and only the run that turned it on turns it off (after the recording, in cleanup(), and
# cleanup()'s first pass). A runner SIGKILLed before even that first pass leaves it on. So the
# sweeps that already clear every other box -- rig-mode EVENT (sweep-off, then the contract's
# sweep-check) and the E2E pre-run normalize (sweep-off) -- sweep the cg OBS too, through the same
# obs_burn_filter.py sweep-* enumerator, whenever the traveling box is HOME (obs_fleet_is_home:
# resolves + OBS-WS :4455 answers). Away = SKIP, never a failure. These run on EVERY run, not only
# under CG_CHAIN=1: the burn they clear was left by an EARLIER run.

# The obs-fleet name of the cg OBS box (env CG_CHAIN_BACKSTOP_BOX, default resolume). Pure.
cg_chain_backstop_box() {
  printf '%s' "${CG_CHAIN_BACKSTOP_BOX:-resolume}"
}

# The cg OBS burn-sweep target in the rig-mode obs_burn_targets row shape `ip|source|box` -- the
# box's fleet host, `-` (the sweeps enumerate every input, the source field is unused) and the box
# name -- when the box is home; otherwise NOTHING on stdout and one SKIP line on stderr. ALWAYS
# returns 0 (it feeds a `done < <(...)`).
cg_chain_backstop_sweep_targets() {
  local box host
  box="$(cg_chain_backstop_box)"
  if obs_fleet_is_home "$box" && host="$(obs_fleet_host "$box")"; then
    printf '%s|-|%s\n' "$host" "$box"
  else
    echo "    [$box burn-sweep] SKIP: the cg OBS box '$box' is away (obs-fleet home check) -- not swept" >&2
  fi
  return 0
}

# The E2E pre-run cg OBS sweep: clear genlock_burn on EVERY ndi input of the cg OBS while it is home
# (obs_burn_filter.py sweep-off). $1=obs_burn_filter.py $2=per-call timeout (s). Loud, never fatal:
# the camera-chain E2E never aborts on the report-only cg leg, so a sweep that fails is a WARNING
# naming the manual command -- exit 2 = the input enumeration failed (a leaked burn stays
# UNVERIFIED, the fail-closed wording of the burn-enumeration rule), anything else = a burn still
# renders, the connection failed, or the call timed out. CG_CHAIN_OBS_PASSWORD is passed when set.
# ALWAYS returns 0.
cg_chain_backstop_sweep_off() {
  local bf="$1" tmo="${2:-30}" host box out rc pw=()
  if [ -n "${CG_CHAIN_OBS_PASSWORD:-}" ]; then pw=(--password "$CG_CHAIN_OBS_PASSWORD"); fi
  while IFS='|' read -r host _ box; do
    [ -n "$host" ] || continue
    out="$(timeout "$tmo" python3 "$bf" sweep-off --host "$host" ${pw[@]+"${pw[@]}"} 2>&1)" && rc=0 || rc=$?
    if [ -n "$out" ]; then printf '%s\n' "$out" | sed "s/^/    [$box burn-sweep] /"; fi
    case "$rc" in
      0) ;;
      2)
        echo "[cg_chain] WARNING: could not enumerate the cg OBS inputs on $host -- a cg hop burn an earlier run left on stays UNVERIFIED; clear it: python3 $bf sweep-off --host $host" >&2
        ;;
      *)
        echo "[cg_chain] WARNING: the cg OBS burn sweep on $host failed (rc=$rc) -- a cg hop burn may still be ON; clear it: python3 $bf sweep-off --host $host" >&2
        ;;
    esac
  done < <(cg_chain_backstop_sweep_targets)
  return 0
}
