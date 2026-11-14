#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-leg.sh -- issue 1485 (owner 2.11.2026: each application is its own Dante
# device). The Dante variant of the leg in scripts/lib/strih-vban-leg.sh, selected by the box fact
# STRIH_DANTE_DEVICES (`none` = that lib renders byte-identically to issue 1439).
#
# The address: a real /24 with `ipv4.route-table` = the leg's table, so the connected route sits in
# that table and only sockets bound to the leg's address reach it (`from <addr> lookup <table>`). A
# plain /24 in main would pull everything this box sends to the audio VLAN from its rig address (the
# dantesync NTP replies to mbc and fohabl) onto the leg.
#
# The filter (issue 1519, dstack #264): dstack's GENERIC Dante-leg filter, a pinned artifact
# (scripts/lib/strih-dante-leg-filter.sh), never a per-box ruleset of ours. Every dstack device binds fixed
# ports from its slot (dstack >= 0.6.117), so the file names no device: Dante's ports, the flow range
# 29800-31847, PTP 319/320, the multicast groups, ssh + the MCP agent (issue 1517). VBAN on the Dante leg is
# retired (the loader refuses a leg VBAN sender on a Dante box).

if ! declare -F strih_dante_leg_filter_apply >/dev/null; then
  # shellcheck source=scripts/lib/strih-dante-leg-filter.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-dante-leg-filter.sh"
fi

# strih_dante_leg_declared -> rc 0 when the LOADED box declares Dante devices on the leg.
strih_dante_leg_declared() {
  local v
  v="$(strih_lx_dante_devices 2>/dev/null)" || return 1
  [ -n "$v" ] && [ "$v" != none ]
}
