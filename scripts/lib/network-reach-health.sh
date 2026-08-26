#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (obs-watchdog-decision.sh, optical-chain-health.sh,
# imag-power-envelope.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (network-reach-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/network-reach-health.sh -- #1001: the SHARED, PURE decision core for the dev1-side
# strih/stream network-reachability alert watchdog. No I/O, no ping, no ssh, no OBS, so it can be
# unit-tested exhaustively (mirrors scripts/lib/obs-watchdog-decision.sh / optical-chain-health.sh).
#
# WHY (#1001, live 2026-08-06 50-min outage + 2026-08-13 recurrence): strih's optical NIC died and
# the box fell fully off the network -- no DHCP, ARP INCOMPLETE, OBS-WS + ssh + MCP all dead -- while
# stream's `NDI 2ME PGM` silently held the last frozen frame. NO Discord alert fired: every existing
# watchdog probes a box it assumes is UP (OBS-WS GetStats, ssh INTO the box) and treats a total
# network outage as `no probe output -> nothing to decide`. The reachability question can only be
# answered by a prober that is UP while the target is DOWN -- dev1 -- probing the box from OUTSIDE
# with a MULTI-SIGNAL check (ping OR :4455 OBS-WS OR :8899 bundle-state), so a single dropped ping or
# a Windows box that firewalls ICMP-but-answers-TCP is never a false outage.
#
# Source-only: pure functions, no side effects at source time.

# net_reach_classify_box <ping_ok 0|1> <ws_ok 0|1> <bundle_ok 0|1> -> stdout: REACHABLE | UNREACHABLE
#   A box is REACHABLE iff ANY signal succeeded (ping OR the OBS-WS :4455 TCP connect OR the
#   bundle-state :8899 TCP connect). Only when ALL THREE fail is it UNREACHABLE -- the exact real
#   incident ("No route to host" on every probe). ANY value other than "1" counts as a failed signal
#   (defensive: an empty/garbage probe result is never a false REACHABLE).
net_reach_classify_box() {
  local ping_ok="${1:-0}" ws_ok="${2:-0}" bundle_ok="${3:-0}"
  if [ "$ping_ok" = "1" ] || [ "$ws_ok" = "1" ] || [ "$bundle_ok" = "1" ]; then
    printf 'REACHABLE\n'
  else
    printf 'UNREACHABLE\n'
  fi
}

# net_reach_any_reachable <flag...> -> stdout: 1 | 0
#   The reference-anchor aggregate (dev1-side-outage guard): given the reachability flag of each
#   REFERENCE rig node (cam1/cam2/imag-nb -- nodes that share the rig's network fate), returns 1 iff
#   AT LEAST ONE is reachable. When it returns 0, NO reference node answered -> dev1's own path to the
#   rig subnet is down (or the whole rig link stalled, e.g. an event-day mobile uplink) -> the pass is
#   "nothing to decide", never a false "both OBS boxes down". Zero args -> 0 (nothing to anchor on).
#   Any value other than "1" counts as not-reachable.
net_reach_any_reachable() {
  local flag
  for flag in "$@"; do
    [ "$flag" = "1" ] && { printf '1\n'; return 0; }
  done
  printf '0\n'
}

# net_reach_recovery_decision <was_alerted 0|1> <now_reachable 0|1> -> stdout: recover=0 | recover=1
#   Fire ONE recovery ("reachable again") ping only when a box we actually PAGED for (was_alerted=1)
#   returns to reachable (now_reachable=1). A box that was down but never confirmed/paged, or one that
#   was healthy all along, never emits a recovery ping. Any value other than "1" counts as 0.
net_reach_recovery_decision() {
  local was_alerted="${1:-0}" now_reachable="${2:-0}"
  if [ "$was_alerted" = "1" ] && [ "$now_reachable" = "1" ]; then
    printf 'recover=1\n'
  else
    printf 'recover=0\n'
  fi
}

# net_reach_alert_detail <box> <ping_ok 0|1> <ws_ok 0|1> <bundle_ok 0|1> -> stdout: one human line
#   naming the box and the up/DOWN state of each of the three signals, for the Discord alert body.
#   `DOWN` (upper) marks a failed signal so it reads at a glance on a phone; `up` marks a live one.
net_reach_alert_detail() {
  local box="${1:-?}" ping_ok="${2:-0}" ws_ok="${3:-0}" bundle_ok="${4:-0}"
  local p w b
  [ "$ping_ok" = "1" ] && p="up" || p="DOWN"
  [ "$ws_ok" = "1" ] && w="up" || w="DOWN"
  [ "$bundle_ok" = "1" ] && b="up" || b="DOWN"
  printf '%s: ping %s, OBS-WS:4455 %s, bundle-state:8899 %s\n' "$box" "$p" "$w" "$b"
}

# net_reach_box_is_report_only <box> <report_only_boxes> -> stdout: report_only=1 | report_only=0
#   A box NAMED in the space-separated <report_only_boxes> list is REPORT-ONLY: it is probed,
#   classified, logged and per-box state-tracked exactly like a paging box, but a report-only box
#   NEVER fires a Discord page (nor a recovery ping) -- its verdict is log-only. This is for a
#   TRAVELING box (resolume, #811) whose absence is the NORMAL state (powered off / away between
#   events), so paging on its unreachability would be pure false-alarm noise. A supervisor "flips it
#   required" by REMOVING its name from the list (leaving it only in the paging BOXES roster), at
#   which point it pages like strih/stream with all confirm/throttle/recovery state already warm.
#   Whole-word match on the box NAME (so "resolume" never matches "resolume-alt" unless that name is
#   also listed). Empty list / not a member -> report_only=0 (pages normally -- the strih/stream
#   default). Always returns 0 so it is safe to call as a bare statement under a caller's set -e.
net_reach_box_is_report_only() {
  local box="${1:-}" list="${2:-}" b
  for b in $list; do
    [ "$b" = "$box" ] && { printf 'report_only=1\n'; return 0; }
  done
  printf 'report_only=0\n'
}
