#!/usr/bin/env bash
# scripts/lib/bisect-smoothness.sh — pure helpers for the #1150 smoothness-bisect driver.
set -euo pipefail
#
# PURE (Tier-0 bash-testable, no rig / no cargo) helpers sourced by scripts/bisect-smoothness.sh.
# They do NO I/O against the fleet: parsing the declarative points file, building the EXACT deploy
# command (always CAM1+CAM2, NEVER cam3 — the control box), formatting a durable marker-log line,
# and computing the next un-measured point from the points list + the marker log. The driver's own
# main() (deploy over ssh via deploy-fleet.sh, marker-log writes) is guarded behind BASH_SOURCE!=$0
# so tests/bisect-smoothness.test.sh can source THIS lib and assert the builders in isolation — the
# same pure-planner model as tests/rig_mode.rs / tests/launch_obs_genlock.rs.
#
# The load-bearing safety property is in bisect_deploy_plan: the emitted CAMERA_SET is the literal
# string "cam1 cam2", so cam3 (issue 1150's measurement CONTROL, which MUST stay on the current
# build) can never be redeployed by this driver.

# bisect_parse_point_line LINE
#   Parse ONE points-file line "LABEL<TAB>RUN_ID<TAB>VERSION<TAB>NOTE".
#   - blank / '#'-comment line -> return 1 (skip, no output)
#   - RUN_ID not all-digits, or empty LABEL -> return 2 (error to stderr)
#   - valid -> echo the 4 fields tab-joined, return 0
bisect_parse_point_line() {
  local line="${1-}"
  # skip blank / whitespace-only
  if [ -z "${line//[[:space:]]/}" ]; then return 1; fi
  # skip comment lines (first non-space char is '#')
  case "${line#"${line%%[![:space:]]*}"}" in
    '#'*) return 1 ;;
  esac
  local label run ver note
  IFS=$'\t' read -r label run ver note <<<"$line"
  note="${note-}"
  if [ -z "$label" ]; then
    echo "bisect_parse_point_line: empty LABEL in: $line" >&2; return 2
  fi
  case "$run" in
    ''|*[!0-9]*) echo "bisect_parse_point_line: RUN_ID not numeric ('$run') in: $line" >&2; return 2 ;;
  esac
  printf '%s\t%s\t%s\t%s' "$label" "$run" "$ver" "$note"
}

# bisect_deploy_plan LABEL RUN_ID VERSION -> the EXACT deploy command string.
#   ALWAYS CAMERA_SET="cam1 cam2" — cam3 (control) is never touched by the driver.
bisect_deploy_plan() {
  local _label="${1-}" run="${2-}" _ver="${3-}"
  printf 'CAMERA_SET="cam1 cam2" scripts/deploy-fleet.sh --run %s' "$run"
}

# bisect_marker_line LABEL RUN_ID VERSION STATUS [EXTRA] -> a tab-separated durable marker line.
#   Timestamp is BISECT_NOW when set (deterministic tests), else UTC now.
bisect_marker_line() {
  local label="${1-}" run="${2-}" ver="${3-}" status="${4-}" extra="${5-}"
  local ts="${BISECT_NOW:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
  printf '%s\t%s\t%s\t%s\t%s\t%s' "$ts" "$label" "$run" "$ver" "$status" "$extra"
}

# bisect_latest_status LABEL MARKER_TEXT -> the status of the LAST marker line for LABEL ("" if none).
bisect_latest_status() {
  local label="${1-}" markers="${2-}"
  printf '%s\n' "$markers" | awk -F'\t' -v l="$label" '$2==l{s=$5} END{if(s!="")print s}'
}

# bisect_next_pending POINTS_TEXT MARKER_TEXT
#   Echo the LABEL of the first points-file entry whose latest marker status is not "result"
#   (i.e. not yet E2E-measured). Return 1 when every point already has a "result" marker.
bisect_next_pending() {
  local points="${1-}" markers="${2-}"
  local line parsed label st
  while IFS= read -r line; do
    parsed="$(bisect_parse_point_line "$line" 2>/dev/null)" || continue
    label="${parsed%%$'\t'*}"
    st="$(bisect_latest_status "$label" "$markers")"
    if [ "$st" != "result" ]; then
      printf '%s' "$label"
      return 0
    fi
  done <<<"$points"
  return 1
}
