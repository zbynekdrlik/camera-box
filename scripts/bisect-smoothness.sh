#!/usr/bin/env bash
# scripts/bisect-smoothness.sh — driver for the #1150 controlled visible-smoothness bisect.
set -euo pipefail
#
# WHY (issue 1150, owner's mandate via issue 1130 point 4). Visible juddering on strih/stream/imag
# worsened over the last weeks; the owner WITHDREW the hardware conclusion and the working hypothesis
# is a REGRESSION in the emit/receive stack. This driver finds the breaking commit RIDANE (not by
# guessing): for each candidate history point it deploys that point's historical CI binary to
# CAM1+CAM2 ONLY, leaving CAM3 on the current build as the measurement CONTROL, then STOPS. The E2E
# run + per-box uniformity read-out + the owner's visual confirmation are done by the SUPERVISOR by
# hand BETWEEN points (issue 1130 point 1: visual acceptance is the final gate) — this driver never
# runs the E2E and never touches CAM3.
#
# SAFETY:
#   * DRY-RUN is the DEFAULT — nothing is deployed until --execute is passed.
#   * The deploy set is the literal "cam1 cam2" (bisect_deploy_plan) — cam3 is NEVER redeployed.
#   * The actual deploy REUSES scripts/deploy-fleet.sh (--run <id>): stop -> remount,rw -> scp ->
#     start -> remount,ro -> sha256 byte-verify -> version read-back -> genlock-emit check. No new
#     deploy code, no new credential.
#   * State is DURABLE in the marker log (~/.camera-box/bisect-smoothness.log, never tmpfs) so a
#     dead session / compaction resumes exactly where it stopped.
#
# USAGE:
#   scripts/bisect-smoothness.sh                     # DRY-RUN: show the next pending point's plan + runbook
#   scripts/bisect-smoothness.sh --list              # show all points + each point's latest status
#   scripts/bisect-smoothness.sh --point P3-bad462   # DRY-RUN a specific point
#   scripts/bisect-smoothness.sh --execute           # DEPLOY the next pending point to CAM1+CAM2, then STOP
#   scripts/bisect-smoothness.sh --execute --point P5-post1111   # deploy a specific point
#   scripts/bisect-smoothness.sh --record-result P3-bad462 "uniformity cam1=0.71 cam3=0.99 ..."
#   ENV: BISECT_POINTS_FILE (default scripts/bisect-smoothness-points.tsv),
#        BISECT_LOG (default ~/.camera-box/bisect-smoothness.log)

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bisect-smoothness.sh
. "$HERE/lib/bisect-smoothness.sh"

BISECT_POINTS_FILE="${BISECT_POINTS_FILE:-$HERE/bisect-smoothness-points.tsv}"
BISECT_LOG="${BISECT_LOG:-$HOME/.camera-box/bisect-smoothness.log}"

_read_log() { [ -f "$BISECT_LOG" ] && cat "$BISECT_LOG" || printf ''; }

# _point_fields LABEL -> "RUN_ID<TAB>VERSION<TAB>NOTE" for LABEL from the points file (rc!=0 if absent)
_point_fields() {
  local want="$1" line parsed
  # `|| [ -n "$line" ]` reads a final line lacking a trailing newline; no `2>/dev/null` so a
  # malformed line (rc=2) shouts instead of vanishing (issue 1150 review 🟡3/🟡4).
  while IFS= read -r line || [ -n "$line" ]; do
    parsed="$(bisect_parse_point_line "$line")" || continue
    case "$parsed" in
      "$want"$'\t'*) printf '%s' "${parsed#*$'\t'}"; return 0 ;;
    esac
  done < "$BISECT_POINTS_FILE"
  return 1
}

_print_runbook() {
  local label="$1" run="$2" ver="$3"
  cat <<RB

------------------------------------------------------------------------
STOP — E2E + uniformity read-out is the SUPERVISOR's manual step (this
driver never runs the E2E and never touches cam3). Run the bisect E2E
LOCALLY on dev1 (NOT via the PR gate) for point $label ($ver, run $run):

  1. Protect the staged build from the ci.yml post-merge auto-deploy:
     ack CAM1+CAM2 offline in rig-fleet.txt for the bisect window (a
     dev->main merge would otherwise redeploy the fleet to main's build).
  2. Neutralize the [0/8] camera-box version-parity gate (it refuses a
     mixed fleet, exit 20). Verified-real options:
       (a) CAMERA_ACTIVE_SET="cam1 cam2" + \\
           CAMERA_BOX_VERSION_GATE_MAIN_PIN=$ver
           (honest reads; measure cam3 control in a separate run), OR
       (b) per-node CAMERA_BOX_VERSION_GATE_VERSION_<cam> fixtures +
           CAMERA_BOX_VERSION_GATE_MAIN_PIN=<one uniform version>
           (single mixed-fleet run; gate stops verifying real versions).
  3. Run recording-e2e.sh locally with:
       E2E_EXECUTE_VERDICT=1 WIN_VERDICT_EXE_LOCAL=<recording-verdict.exe>
     (recording-e2e.sh:4253-4264 — the local decode path, not the PR gate).
  4. Read per-box cadence uniformity + copies/gaps; CONTROL cam3 must stay
     0.99+ (else the run measured the environment, not the build).
  5. Record it:  scripts/bisect-smoothness.sh --record-result $label "<verdict>"
------------------------------------------------------------------------
RB
}

_cmd_list() {
  local markers line parsed label rest st
  markers="$(_read_log)"
  printf '%-16s %-14s %-16s %-10s %s\n' POINT RUN_ID VERSION STATUS NOTE
  while IFS= read -r line || [ -n "$line" ]; do
    parsed="$(bisect_parse_point_line "$line")" || continue
    label="${parsed%%$'\t'*}"; rest="${parsed#*$'\t'}"
    st="$(bisect_latest_status "$label" "$markers")"; [ -n "$st" ] || st="-"
    printf '%-16s %-14s %-16s %-10s %s\n' "$label" "${rest%%$'\t'*}" "$(printf '%s' "$rest" | cut -f2)" "$st" "$(printf '%s' "$rest" | cut -f3-)"
  done < "$BISECT_POINTS_FILE"
}

_resolve_label() { # echo target label (explicit --point or next pending); rc!=0 if none
  local explicit="$1"
  if [ -n "$explicit" ]; then printf '%s' "$explicit"; return 0; fi
  bisect_next_pending "$(cat "$BISECT_POINTS_FILE")" "$(_read_log)"
}

main() {
  local execute=0 explicit_point="" record_label="" record_text="" do_list=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --execute) execute=1; shift ;;
      --point) explicit_point="${2:?--point needs a LABEL}"; shift 2 ;;
      --list) do_list=1; shift ;;
      --record-result) record_label="${2:?--record-result needs a LABEL}"; record_text="${3:?--record-result needs a verdict string}"; shift 3 ;;
      -h|--help) sed -n '2,40p' "$HERE/bisect-smoothness.sh"; return 0 ;;
      *) echo "unknown arg: $1" >&2; return 1 ;;
    esac
  done

  [ -f "$BISECT_POINTS_FILE" ] || { echo "points file not found: $BISECT_POINTS_FILE" >&2; return 1; }
  mkdir -p "$(dirname "$BISECT_LOG")"

  if [ "$do_list" = "1" ]; then _cmd_list; return 0; fi

  if [ -n "$record_label" ]; then
    local rf; rf="$(_point_fields "$record_label")" || { echo "no such point: $record_label" >&2; return 1; }
    local run="${rf%%$'\t'*}" ver; ver="$(printf '%s' "$rf" | cut -f2)"
    bisect_marker_line "$record_label" "$run" "$ver" result "$record_text" >> "$BISECT_LOG"
    echo "recorded result for $record_label -> $BISECT_LOG"
    return 0
  fi

  local label; label="$(_resolve_label "$explicit_point")" || { echo "all points already measured (a 'result' marker exists for each) — bisect complete." ; return 0; }
  local rf; rf="$(_point_fields "$label")" || { echo "no such point: $label" >&2; return 1; }
  local run="${rf%%$'\t'*}" ver; ver="$(printf '%s' "$rf" | cut -f2)"
  local plan; plan="$(bisect_deploy_plan "$label" "$run" "$ver")"

  echo "== bisect point: $label  (version $ver, ci run $run) =="
  echo "deploy CAM1+CAM2 (cam3 = control, untouched):"
  echo "    $plan"

  if [ "$execute" != "1" ]; then
    echo ""
    echo "DRY-RUN (default) — nothing deployed. Re-run with --execute to deploy this point."
    _print_runbook "$label" "$run" "$ver"
    return 0
  fi

  local camset; camset="$(bisect_camera_set)"
  echo ""
  echo "--execute: deploying $label to $camset via deploy-fleet.sh ..."
  # Single-sourced CAMERA_SET (bisect_camera_set) — the SAME value the tested deploy plan prints, so
  # the literal that actually deploys is the one under test (issue 1150 review 🔴2). On a partial
  # deploy failure record a durable 'deploy-failed' marker + a loud message before set -e aborts, so
  # the operator has a trace and does NOT mistake a half-deployed fleet for a clean point (🔵6).
  if ! CAMERA_SET="$camset" "$HERE/deploy-fleet.sh" --run "$run"; then
    bisect_marker_line "$label" "$run" "$ver" deploy-failed "$camset" >> "$BISECT_LOG"
    echo "ERROR: deploy of $label to $camset FAILED — see deploy-fleet.sh output above. Marked deploy-failed in $BISECT_LOG." >&2
    echo "       The fleet may be half-deployed; do NOT run the E2E for this point until it is re-deployed cleanly." >&2
    return 1
  fi
  bisect_marker_line "$label" "$run" "$ver" deployed "$camset" >> "$BISECT_LOG"
  echo "marker written: $label deployed -> $BISECT_LOG"
  _print_runbook "$label" "$run" "$ver"
}

# Pure-planner guard: sourcing this file (tests) must NOT run main.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
