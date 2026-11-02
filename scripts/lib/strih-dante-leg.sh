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
# the leg by its uid (an avahi restart risks the NDI announcement). UDP 319/320 and dsync's 31900 are
# never admitted: dantesync stays the box's only PTP owner.

STRIH_DANTE_MDNS_PORT=5353
STRIH_DANTE_INFO_PORT=8700
# the info + heartbeat multicast ports
STRIH_DANTE_GROUP_PORTS="8702 8708"
STRIH_DANTE_GROUPS="224.0.0.231 224.0.0.233 224.0.0.251"
# a peer's default ARC / flow control / CMC ports (dstack arc::PORT, flows::PORT, cmc::PORT)
STRIH_DANTE_PEER_DEFAULT_PORTS="4440 4455 8800"
# net.ipv4.ip_local_port_range's Linux default, where a port-0 client socket lands
STRIH_DANTE_EPHEMERAL_PORTS="32768-60999"

# strih_dante_leg_declared -> rc 0 when the LOADED box declares Dante devices on the leg.
strih_dante_leg_declared() {
  local v
  v="$(strih_lx_dante_devices 2>/dev/null)" || return 1
  [ -n "$v" ] && [ "$v" != none ]
}

# _strih_dante_sorted_set ITEM... -> the items sorted by their first number, unique, `, `-joined: the
# order `nft list` prints an anonymous set in.
_strih_dante_sorted_set() {
  printf '%s\n' "$@" | sort -t- -k1,1n -u | paste -s -d, - | sed 's/,/, /g'
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
  local d rx=()
  for d in $1; do rx+=("${d##*:}"); done
  # shellcheck disable=SC2046,SC2086  # the lists split on purpose
  _strih_dante_merged_set $(strih_dante_service_ports "$1") "${rx[@]}" \
    "$STRIH_DANTE_MDNS_PORT" "$STRIH_DANTE_INFO_PORT" $STRIH_DANTE_GROUP_PORTS
}

# strih_dante_peer_ports DEVICES -> the nft set of the service ports a peer answers from (pure).
strih_dante_peer_ports() {
  # shellcheck disable=SC2046,SC2086  # the lists split on purpose
  _strih_dante_sorted_set $(strih_dante_service_ports "$1") $STRIH_DANTE_PEER_DEFAULT_PORTS \
    "$STRIH_DANTE_MDNS_PORT" "$STRIH_DANTE_INFO_PORT"
}

# strih_dante_nft_input IF NET DEVICES -> the input rules (a leg's table, before its drop).
strih_dante_nft_input() {
  local ifc="$1" net="$2" dev="$3"
  printf '        iifname "%s" ip saddr %s udp dport { %s } counter accept\n' "$ifc" "$net" "$(strih_dante_in_ports "$dev")"
  printf '        iifname "%s" ip saddr %s udp sport { %s } udp dport %s counter accept\n' "$ifc" "$net" \
    "$(strih_dante_peer_ports "$dev")" "$STRIH_DANTE_EPHEMERAL_PORTS"
  printf '        iifname "%s" ip protocol igmp counter accept\n' "$ifc"
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
}
