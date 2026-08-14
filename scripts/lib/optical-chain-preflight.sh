#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statements) -- matches
# the sibling scripts/lib/*.sh convention (cam2-painter-restore-verify.sh, cambox-parallel-
# restore.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it
# in the CALLER's shell, so imposing strict mode here would leak into whichever caller sources it.
# recording-e2e.sh (the only caller) already sets -euo pipefail itself.
#
# scripts/lib/optical-chain-preflight.sh -- #860: the recording-e2e.sh [0/8] OPTICAL-LEG fail-fast.
# The #675 prevention pattern: a NEW sourced-lib function invoked with ONE line from
# recording-e2e.sh, so NO existing static-anchor line in that file is edited.
#
# WHY (#860, live incident 2026-08-14): a chain of FAILED E2E runs left the cam2 painter DEAD and
# the next gate run burned a ~40-min recording before its verdict reported the cam2->cam1 optical
# hop UNAVAILABLE. A run whose optical injection leg is already dead must abort LOUD at preflight,
# not waste the run.
#
# Uses the SAME shared pure core the standing optical-chain-alert-watchdog uses
# (scripts/lib/optical-chain-health.sh -- the CALLER must have sourced it first, as recording-e2e.sh
# does). Deliberately NARROW abort policy to never false-abort a CI gate (the user's hardest
# constraint): it HARD-ABORTS only on the unambiguous "a standing painter is EXPECTED but DEAD"
# signal from cam2 (no OBS dependency, no false-positive from a legitimately-black program); a
# strih-BLACK read is a loud WARN (the standing watchdog owns the throttled paging), and an
# unreachable cam2 / OBS-WS is "nothing to decide" (the fleet reachability gate owns that).
#
# NOTE on scope: the harness launches its OWN painter later (pgrep-tracked, not this pidfile) with
# its own #464 liveness + #163 prod-scene non-black self-check. This preflight closes the DISTINCT
# gap those do not: a STANDING rig-mode painter (/run/rig-painter.pid) or the permanent
# cam2-painter.service that a previous run left dead, BEFORE this run wastes time.

# optical_chain_preflight_assert <painter_ip> <cam_user> <cam_pw> <strih> <obs_password> <here> [pidfile] [service]
#   Probes the cam2 painter (one ssh) + (only if a live painter is expected) a strih optical proof,
#   runs the shared pure decision, and:
#     - EXITS 1 (loud) when a painter is EXPECTED but DEAD  (the abortable incident),
#     - WARNs   when the painter is alive but strih renders BLACK,
#     - proceeds (ok/NOTE) otherwise.
#   Call it as a PLAIN statement (never in a pipeline/$()) so its `exit 1` propagates to the harness.
optical_chain_preflight_assert() {
  local painter_ip="$1" cam_user="$2" cam_pw="$3" strih="$4" obs_pw="$5" here="$6"
  local pidfile="${7:-/run/rig-painter.pid}" service="${8:-cam2-painter.service}"
  local snapshot expected alive optical out rc

  snapshot="$(timeout 15 sshpass -p "$cam_pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
    "${cam_user}@${painter_ip}" "$(optical_chain_painter_probe_remote_snippet "$pidfile" "$service")" 2>/dev/null || true)"
  if [ -z "$snapshot" ]; then
    echo "    NOTE: [0/8] optical-chain preflight — could not probe cam2 painter over ssh ($painter_ip); the fleet reachability gate owns that condition — skipping the optical-leg fail-fast" >&2
    return 0
  fi

  expected="$(optical_chain_painter_expected_from_snapshot "$snapshot")"
  alive="$(optical_chain_painter_alive_from_snapshot "$snapshot")"

  if [ "$expected" != "1" ]; then
    echo "    ok: [0/8] optical-chain preflight — no standing cam2 painter expected (EVENT mode / none set up); the harness launches + liveness-checks its own painter below"
    return 0
  fi

  if [ "$alive" != "1" ]; then
    echo "ERROR: [0/8] optical injection leg DEAD — a standing cam2 painter is EXPECTED ($pidfile present or $service enabled) but it is NOT alive." >&2
    echo "       cam2's monitor is dark, so the cam2→cam1 optical leg cannot be read; a previous run's cleanup likely left it dead (issue 860 / WARNING #712 class)." >&2
    echo "       Recovery: scripts/rig-mode.sh test   # relaunch the painter + re-verify the chain non-black, then re-run." >&2
    exit 1
  fi

  # Painter expected + alive -> a live optical proof off strih (the #901 read-only check). WARN
  # (never abort) on BLACK: a program legitimately not yet showing a camera would false-abort a gate.
  out="$(timeout 40 python3 "$here/obs_phase2.py" assert-program-nonblack \
    --host "$strih" --password "$obs_pw" --label "#860 [0/8] optical-leg preflight" 2>&1)" && rc=0 || rc=$?
  optical="$(optical_chain_classify_nonblack_probe "$rc" "$out")"
  case "$optical" in
    OK) echo "    ok: [0/8] optical-chain preflight — cam2 painter alive + strih program NON-BLACK" ;;
    BLACK)
      echo "WARNING #860: [0/8] cam2 painter is alive but strih program renders BLACK (process-alive is not QR-on-screen, issue 901/754 class). The standing optical-chain watchdog will page; verify the monitor before trusting this run's optical hop." >&2 ;;
    *)
      echo "    NOTE: [0/8] optical-chain preflight — cam2 painter alive; strih optical proof unavailable (OBS-WS unreadable) — nothing to decide, proceeding" >&2 ;;
  esac
  return 0
}
