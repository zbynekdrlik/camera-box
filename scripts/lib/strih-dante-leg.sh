#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-leg.sh -- issue 1485 (owner 2.11.2026: each application is its own Dante
# device; strih-snv only, VBAN stays). The Dante variant of the leg in scripts/lib/strih-vban-leg.sh,
# selected by the box fact STRIH_DANTE_DEVICES (`none` = that lib renders byte-identically to issue 1439).
#
# The address: a real /24 with `ipv4.route-table` = the leg's table, so the connected route sits in
# that table and only sockets bound to the leg's address reach it (`from <addr> lookup <table>`). A
# plain /24 in main would pull everything this box sends to the audio VLAN from its rig address (the
# dantesync NTP replies to mbc and fohabl) onto the leg, into the output drop.
#
# The filter, read from dstack's source (head of 2.11.2026): a device's sockets are bound to the
# interface address; its ARC, flow and mDNS instance CLIENT sockets bind port 0, so a peer's reply comes
# from the peer's service port to an ephemeral port (the second input rule, bounded to the ephemeral
# range: the source port alone must never open 319 or the VBAN ports). The peer list is the defaults plus
# this box's own devices, so a peer device on other ports needs its port added. Multicast: mDNS, the info
# group 224.0.0.231:8702, the heartbeat group 224.0.0.233:8708. IGMP goes both ways or a snooping switch
# ages the groups out. No conntrack: its hooks would see every NDI packet of this box. avahi stays off
# the leg by its uid (an avahi restart risks the NDI announcement).
#
# PTP (ROZHODNUTE 6628785864, owner 4.11.2026): every dstack device here follows Dante PTP IN PROCESS
# (dsync's follower in dstack v0.6.23: UDP 319 event + 320 general on INADDR_ANY, joined to 224.0.1.129
# on the leg, receive only, it never sends). So one input rule admits exactly that group to 319/320 from
# the /24; no output rule (IGMP already passes). dsync's own 31900 stays shut. The dsync service stays
# the only writer of the system clock; Linux's default IP_MULTICAST_ALL also hands the group's packets to
# its own INADDR_ANY:319 sockets, and its gm_allowlist keeps it on video-clock.lan.

# The rule is rendered only once a pinned device's dstack follows in process (strih_dante_ptp_leg_open,
# strih-dante-ptp.sh): before that a dstack that reached 319 would start its old follower beside dsync.
if ! declare -F strih_dante_ptp_leg_open >/dev/null; then
  # shellcheck source=scripts/lib/strih-dante-ptp.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-dante-ptp.sh"
fi

# the PTP multicast group + the two ports dsync's follower binds (dsync-ptp net_linux.rs)
STRIH_DANTE_PTP_GROUP=224.0.1.129
STRIH_DANTE_PTP_PORTS="319, 320"

STRIH_DANTE_MDNS_PORT=5353
STRIH_DANTE_INFO_PORT=8700
# the info + heartbeat multicast ports
STRIH_DANTE_GROUP_PORTS="8702 8708"
STRIH_DANTE_GROUPS="224.0.0.231 224.0.0.233 224.0.0.251"
# a peer's ARC / flow control / CMC ports over dstack's device slots: the base (dstack arc::PORT,
# flows::PORT, cmc::PORT) + k for slot k = 0..31. A peer PC's device can sit in any slot (issue 1508:
# mbc-snv answered from 4456 after mbc's restart and the base-only set dropped it for 18 h). Stateless on
# purpose: the leg has no conntrack (dstack two-networks: replies by the slot ranges, never base ports only).
STRIH_DANTE_PEER_DEFAULT_PORTS="4440-4471 4455-4486 8800-8831"
# net.ipv4.ip_local_port_range's Linux default, where a port-0 client socket lands
STRIH_DANTE_EPHEMERAL_PORTS="32768-60999"
# issue 1517 (owner 13.11.2026, airuleset#1313): ssh + the remoteos MCP agent answer on the Dante address
# too, from the leg's /24 only (remoteos-mcp#96).
STRIH_DANTE_MGMT_TCP_PORTS="22, 8092"

# strih_dante_leg_declared -> rc 0 when the LOADED box declares Dante devices on the leg.
strih_dante_leg_declared() {
  local v
  v="$(strih_lx_dante_devices 2>/dev/null)" || return 1
  [ -n "$v" ] && [ "$v" != none ]
}

# _strih_dante_merged_set ITEM... -> the same with touching ports and ranges merged: `nft list` prints an
# interval set that way, and the setup + verify read-back compares text.
_strih_dante_merged_set() {
  printf '%s\n' "$@" | sort -t- -k1,1n -k2,2n | awk -F- '
    function put() { out = out (out == "" ? "" : ", ") (lo == hi ? lo : lo "-" hi) }
    { f = $1 + 0; l = (NF > 1 ? $2 + 0 : f)
      if (NR == 1) { lo = f; hi = l } else if (f <= hi + 1) { if (l > hi) hi = l } else { put(); lo = f; hi = l } }
    END { put(); print out }'
}

# strih_dante_service_ports DEVICES -> every device's ARC, CMC and flow port, space-joined (pure).
strih_dante_service_ports() {
  local d ports out=""
  for d in $1; do
    ports="${d#*=}"
    out="${out}${out:+ }$(cut -d: -f1-3 <<<"$ports" | tr : ' ')"
  done
  printf '%s\n' "$out"
}

# strih_dante_in_ports DEVICES -> the nft set of the ports the devices receive on (pure).
strih_dante_in_ports() {
  local d ranges=()
  for d in $1; do ranges+=("${d##*:}"); done
  # shellcheck disable=SC2046,SC2086  # the lists split on purpose
  _strih_dante_merged_set $(strih_dante_service_ports "$1") "${ranges[@]}" \
    "$STRIH_DANTE_MDNS_PORT" "$STRIH_DANTE_INFO_PORT" $STRIH_DANTE_GROUP_PORTS
}

# strih_dante_peer_ports DEVICES -> the nft set of the service ports a peer answers from (pure), merged
# the way `nft list` prints an interval set.
strih_dante_peer_ports() {
  # shellcheck disable=SC2046,SC2086  # the lists split on purpose
  _strih_dante_merged_set $(strih_dante_service_ports "$1") $STRIH_DANTE_PEER_DEFAULT_PORTS \
    "$STRIH_DANTE_MDNS_PORT" "$STRIH_DANTE_INFO_PORT"
}

# strih_dante_nft_input IF NET DEVICES [TX] -> the input rules (a leg's table, before its drop). TX = the box
# fact STRIH_DANTE_TX (`none` / empty = receive only). Issue 1513 (dstack, 13.11.2026): a TX flow socket binds
# an EPHEMERAL port and the receiver's keepalives come from ITS ephemeral port, so neither rule above admits
# them; the flow expired after 4 s. A TX box admits UDP from the /24 to the ephemeral range (stopgap until
# dstack's fixed TX flow port range, then the rule names those ports).
strih_dante_nft_input() {
  local ifc="$1" net="$2" dev="$3" tx="${4:-none}"
  printf '        iifname "%s" ip saddr %s udp dport { %s } counter accept\n' "$ifc" "$net" "$(strih_dante_in_ports "$dev")"
  printf '        iifname "%s" ip saddr %s udp sport { %s } udp dport %s counter accept\n' "$ifc" "$net" \
    "$(strih_dante_peer_ports "$dev")" "$STRIH_DANTE_EPHEMERAL_PORTS"
  if [ -n "$tx" ] && [ "$tx" != none ]; then
    printf '        iifname "%s" ip saddr %s udp dport %s counter accept\n' "$ifc" "$net" "$STRIH_DANTE_EPHEMERAL_PORTS"
  fi
  if strih_dante_ptp_leg_open; then
    printf '        iifname "%s" ip saddr %s ip daddr %s udp dport { %s } counter accept\n' "$ifc" "$net" \
      "$STRIH_DANTE_PTP_GROUP" "$STRIH_DANTE_PTP_PORTS"
  fi
  printf '        iifname "%s" ip protocol igmp counter accept\n' "$ifc"
  printf '        iifname "%s" ip saddr %s tcp dport { %s } counter accept\n' "$ifc" "$net" "$STRIH_DANTE_MGMT_TCP_PORTS"
}

# _strih_dante_avahi_uid -> the avahi user's uid ('' when there is none). nft stores a uid and lists the
# number, so the rule carries it (STRIH_DANTE_AVAHI_UID = the test seam).
_strih_dante_avahi_uid() { printf '%s' "${STRIH_DANTE_AVAHI_UID-$(id -u avahi 2>/dev/null || true)}"; }

# strih_dante_nft_output IF ADDR NET -> the output rules (before the leg's drop). The avahi drop comes
# first: avahi's sockets are unbound and a multicast of theirs would carry the leg's address.
strih_dante_nft_output() {
  local ifc="$1" addr="$2" net="$3" groups uid
  # shellcheck disable=SC2086  # the list splits on purpose
  groups="$(printf ', %s' $STRIH_DANTE_GROUPS)"
  uid="$(_strih_dante_avahi_uid)"
  if [ -n "$uid" ]; then printf '        oifname "%s" meta skuid %s counter drop\n' "$ifc" "$uid"; fi
  printf '        oifname "%s" ip protocol igmp counter accept\n' "$ifc"
  printf '        oifname "%s" ip saddr %s ip daddr { %s%s } meta l4proto udp counter accept\n' "$ifc" "$addr" "$net" "$groups"
  printf '        oifname "%s" ip saddr %s ip daddr %s tcp sport { %s } counter accept\n' "$ifc" "$addr" "$net" "$STRIH_DANTE_MGMT_TCP_PORTS"
}
