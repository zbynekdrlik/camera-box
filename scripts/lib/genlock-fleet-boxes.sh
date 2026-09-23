#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure per-box constant functions, no top-level statements
# besides sourcing its sibling obs-fleet.sh) -- the scripts/lib/*.sh convention of NOT setting
# `set -euo pipefail` here: sourcing executes it in the CALLER's shell, and each caller
# (deploy-genlock-fleet.sh, launch-obs-genlock.sh, obs-self-heal-install.sh) sets its own strict mode.
#
# scripts/lib/genlock-fleet-boxes.sh -- issue 1317 part 3: the ONE per-box constant table of the
# genlock OBS planners. Before it, deploy-genlock-fleet.sh (then at 999 lines) carried the MCP/IP/AHK
# table inline and launch-obs-genlock.sh + obs-self-heal-install.sh each kept their own copy of the
# same facts, so retiring the Windows strih PC (M4 cut-over 20.9.2026 -- its address belongs to the
# Linux strih-lx now) meant three edits with nothing checking they agreed. Now every planner reads these
# functions, and the box ADDRESSES + the AHK fact come from the fleet list (scripts/lib/obs-fleet.sh:
# obs_fleet_host / obs_fleet_has_ahk), so a box is registered once.
#
# There is NO Windows `strih` entry any more: the production strih is strih-lx (a linux-genlock box,
# deployed by setup-strih.sh, launched by its strih-obs.service systemd user unit), and every Windows
# function below returns 2 for it -- a Windows program can never be emitted for the Linux strih.

# shellcheck source=scripts/lib/obs-fleet.sh
. "$(dirname "${BASH_SOURCE[0]}")/obs-fleet.sh"

# fleet_box_mcp BOX -> the win-* MCP server that drives a WINDOWS genlock box (stream, resolume);
# rc 2 for any other name (a Linux box has no win-* MCP).
fleet_box_mcp() { case "${1:-}" in stream) echo "win-stream-snv" ;; resolume) echo "win-resolume" ;; *) return 2 ;; esac; }

# fleet_box_ip BOX -> the address a planner prints/dials. stream + resolume come from the fleet list
# (resolume's is the HOSTNAME resolume.lan -- NEVER a pinned literal IP: it is DHCP-drifting and
# currently collides with `bridge` at .201, targets.md, so a plan resolves + identity-confirms it
# live). imag keeps its ssh alias `imag`; strih-lx dials STRIH_LX_IP else the fleet host
# (strih-lx.lan has no DNS entry on dev1). rc 2 for an unknown name.
fleet_box_ip() {
  case "${1:-}" in
    stream|resolume) obs_fleet_host "$1" && echo || return 2 ;;
    imag)            echo "imag" ;;
    strih-lx)        fleet_strih_lx_ip ;;
    *)               return 2 ;;
  esac
}
# strih-lx: STRIH_LX_IP, else the obs-fleet host; no row = rc 2 (fail closed, never a guessed name).
fleet_strih_lx_ip() { local h="${STRIH_LX_IP:-}"; [ -n "$h" ] || h="$(obs_fleet_host strih-lx)" || return 2; [ -n "$h" ] && echo "$h" || return 2; }

# fleet_box_has_ahk BOX -> 1|0, the ONE obs-fleet AHK fact (resolume runs NL_STARTUP.ahk).
fleet_box_has_ahk() { obs_fleet_has_ahk "${1:-}"; echo; }

# fleet_box_ahk_script / fleet_box_ahk_prefer BOX -> the PER-BOX AHK relaunch identity passed into
# the shared scripts/lib/ahk-watchdog.sh primitive (issue 1295). resolume (RESOLUME-SNV) runs
# AutoHotkey v2 with its OWN NL_STARTUP.ahk (the path has a SPACE -- the relaunch PS wraps it in
# double quotes) and PREFERS the Startup .lnk ('lnk') so a path move on the traveling box cannot
# break the relaunch. Only meaningful when fleet_box_has_ahk BOX = 1; rc 2 for a box with no AHK
# watcher, so a caller asking for an identity that does not exist fails loud instead of silently
# relaunching another box's script.
fleet_box_ahk_script() { case "${1:-}" in resolume) echo 'C:\Users\Resolume\Documents\_NLMEDIA resolume\_APPS\NL_STARTUP.ahk' ;; *) return 2 ;; esac; }
fleet_box_ahk_prefer() { case "${1:-}" in resolume) echo "lnk" ;; *) return 2 ;; esac; }

# fleet_resolume_identity_confirm_note -> the IDENTITY-CONFIRM preamble the resolume deploy plan
# prints (issue 1295). RESOLUME-SNV is a TRAVELING box addressed by HOSTNAME, and resolume.lan
# currently resolves to 10.77.9.201 -- the SAME IP `bridge` lists in targets.md (an event-LAN DHCP
# collision) -- so before uploading/deploying to it the supervisor MUST resolve it live AND confirm
# the box IDENTITY (its cg OBS profile), never "the shared OBS-WS password worked" (targets.md /
# rig-state-inspection.md §2). The PLANNER only EMITS this step; the supervisor runs it in the
# win-resolume MCP before STEP 0. PURE (no I/O).
fleet_resolume_identity_confirm_note() {
  cat <<'NOTE'
# STEP -1 (resolume ONLY -- box IDENTITY confirm, issue 1295): resolume.lan is a TRAVELING box on a
#         DHCP lease that currently resolves to 10.77.9.201 -- the SAME IP `bridge` lists in
#         targets.md (event-LAN collision). Resolve it LIVE and confirm it is REALLY the CG box
#         before touching it (never a pinned IP, never "the OBS-WS password worked" -- targets.md /
#         rig-state-inspection.md §2):
#           1. on dev1:           getent hosts resolume.lan      # the live address
#           2. in win-resolume MCP Shell, confirm the cg OBS identity (ONE of):
#                (gci "$env:APPDATA\obs-studio\basic\profiles" -Directory).Name   # expect 'cg'
#                # or over OBS-WS: GetVersion + the 'cg' profile / cg_scenes collection
#         Proceed to STEP 0 ONLY once the resolved address is confirmed to be RESOLUME-SNV.
NOTE
}

# fleet_execute_boxes CSV -> the boxes deploy-genlock-fleet.sh EXECUTE mode actually deploys (and so
# the ONLY ones its durable fleet log may record): the normalized list minus strih-lx, whose execute
# arm is still a follow-up (issue 1317 part 3 -- the default fleet is strih-lx,stream, so a raw
# default run must never log "strih-lx deployed at <sha>"). Empty output = nothing to execute. Pure.
fleet_execute_boxes() {
  local csv="${1:-}" b out=""
  local IFS=','
  for b in $csv; do
    if [ -n "$b" ] && [ "$b" != "strih-lx" ]; then out="${out:+$out,}$b"; fi
  done
  printf '%s' "$out"
}

# fleet_box_keepalive_tasks BOX -> the OBS keep-alive SCHEDULED-TASK names the deploy must disable so
# NONE of them respawns obs64 while the bytes are being copied (#1140). Per-box + CURATED, never all
# of a box's ~nine scheduled tasks: the stream box runs the #812 avsync-keepalive (~10 min) AND the
# #411 obs-self-heal (~2 min) -- avsync-keepalive is the named minimum, but the actual obs64 respawner
# is obs-self-heal (avsync-keepalive only relaunches the two avsync monitor scripts), so both must be
# named. resolume's respawner IS its AHK watcher (the has_ahk stop/restart path), so it lists none.
# The emitted program disables+restores ONLY a task that is PRESENT and ENABLED, so a name absent (or
# deliberately disabled) on the box is a harmless skip -- adding another box's keep-alive here later
# is a one-line change, not a hardcoded pile inline at the call site.
# Task names MUST be whitespace-free: the emitter word-splits this space-separated list.
fleet_box_keepalive_tasks() {
  case "${1:-}" in
    stream) echo 'avsync-keepalive camera-box-obs-self-heal-stream' ;;
    *)      echo '' ;;
  esac
}
