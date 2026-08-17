#!/usr/bin/env bash
# airuleset:script-ok source-only lib — it is sourced INTO the caller's shell, so a `set -euo
# pipefail` here would LEAK `-e` into the caller (see .claude/rules/ci-testing-gotchas.md); the
# caller (the watchdog) owns its own `set -uo pipefail`. Same convention as
# scripts/lib/obs-watchdog-decision.sh / scripts/lib/optical-chain-health.sh.
#
# scripts/lib/cg-bridge-health.sh — the PURE decision heart of the #1006 CG-bridge republish-black
# alert. No I/O, no ssh, no OBS, no MCP — pure so it can be unit-tested exhaustively.
#
# WHY (#1006): strih's `CG bridge` scene renders fully black on air when Resolume Arena's
# "CG_Bridge light" composition output is black WHILE its upstream NDI feed is live — with no alarm
# anywhere (Arena up, plugin up, sender registered). The actual differential decision (upstream live
# but republished black) lives in `obs_phase2.py republish-black-check`, whose exit code encodes the
# verdict; this lib only maps that probe's rc to the watchdog's incident classification, so the
# confirm/throttle/page flow stays identical to every sibling dev1-side alert watchdog.
#
# Source-only: defines cg_bridge_classify_probe(); runs nothing.
#
# cg_bridge_classify_probe <probe_rc>
#   -> stdout: one of
#        alert:republish-black   probe exit 3 — upstream LIVE but the Spout republish is BLACK
#        healthy                 probe exit 0 — OK (both live) or IDLE (upstream itself idle)
#        unknown                 any other rc (4 = unreadable screenshot, or a transport/timeout
#                                failure) — "nothing to decide this pass", NEVER a false alert
cg_bridge_classify_probe() {
  local rc="${1:-}"
  case "$rc" in
    3) printf 'alert:republish-black\n' ;;
    0) printf 'healthy\n' ;;
    *) printf 'unknown\n' ;;
  esac
}
