#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-vban-leg.sh -- issue 1439, ROZHODNUTE 6360417876 (owner 23.10.2026): the VBAN-only
# leg. Owner, verbatim: "na stream-snv je druha sietovka usb-c 2.5gbits a ta je pripojena do vlan 70
# audio siete, vies nastavit aby na stream bola ta sietovka na 10.77.7.204 a islo cez nu iba ten vbani
# nic ine aby cez nu ani nic ine neodchadazlo, vsetko ostatne streamy ndi video internet naech ide ce tu
# sietovku klasicku". mbc then sent its VBAN `stream` to that address: the router left the VBAN path.
#
# The leg is a second NIC on the console's audio VLAN (box facts STRIH_PROGRAM_VBAN_LEG_MAC / _LEG_ADDR /
# STRIH_PROGRAM_VBAN_SENDER, the port is STRIH_PROGRAM_VBAN_PORT). Four parts, every one idempotent:
#   1. the NetworkManager profile `vban-dante70` pinned to the NIC's kernel name (resolved by its MAC,
#      never guessed): a /32 (no connected route, so nothing to the audio VLAN leaves through it), no
#      gateway, never-default, no auto routes / DNS, no IPv6, plus ONE policy route so a ping reply
#      leaves through the leg (route `<leg /24> table 70` + rule `priority 100 from <addr> table 70`).
#      An existing profile on the NIC (a fresh install brings it up by DHCP, with a second default route)
#      is ADOPTED, every other profile on it deleted. A working leg is never bounced: a changed profile is
#      `device reapply`d (a refused reapply falls back to `connection up`, the leg is not the ssh link).
#   2. nftables `table inet vban_dante70` (the house pattern of scripts/lib/dscp-nft.sh: a dedicated
#      table, `table` / `delete table` atomic replace, never `flush ruleset`, applied by its own boot
#      oneshot, never the distro nftables.service): IN on the leg only the sender's VBAN + echo
#      requests, OUT on the leg only echo replies (owner: ping "aby sme mali aspon nejake info ze to
#      bezi"), everything else dropped and counted; ARP is not IP and still answers. Plus one
#      counter-only rule (no verdict) of VBAN arriving on any OTHER interface: verify's "VBAN no longer
#      comes through the router" proof. dantesync keeps video-clock.lan on the rig NIC and never sees
#      audio-VLAN PTP (the input drop); avahi is never touched (the output drop keeps mDNS off the leg).
#   3. the per-interface ARP sysctls (arp_ignore=1, arp_announce=2) in /etc/sysctl.d keyed on the kernel
#      name: systemd's 99-systemd.rules runs `systemd-sysctl --prefix=/net/ipv4/conf/$name` on every net
#      device add, so they apply at boot and on a USB re-plug (read on stream-snv 23.10.2026).
#   4. the filter goes in BEFORE the leg takes its address.
#
# setup-strih step 13 (stream role): strih_vban_leg_install. verify-strih item 9 (stream role):
# strih_vban_leg_grade, through the caller's ok / bad / note (each grade rc 0). Run verify as root (nft).
#
# Seams (tests/python/test_vban_leg_1439.py): STRIH_VBAN_LEG_SYSFS (/sys), STRIH_VBAN_LEG_PROC_SYS
# (/proc/sys), STRIH_VBAN_LEG_NFT_FILE, STRIH_VBAN_LEG_UNIT_FILE, STRIH_VBAN_LEG_SYSCTL_FILE,
# STRIH_VBAN_LEG_WAIT_S (the counter window, 3 s); nmcli / ip / nft / systemctl / sysctl from PATH.

STRIH_VBAN_LEG_PROFILE=vban-dante70
STRIH_VBAN_LEG_NFT_TABLE=vban_dante70
STRIH_VBAN_LEG_UNIT=vban-dante70-filter.service
STRIH_VBAN_LEG_ROUTE_TABLE=70
STRIH_VBAN_LEG_RULE_PRIORITY=100

_strih_vban_leg_nft_file() { printf '%s' "${STRIH_VBAN_LEG_NFT_FILE:-/etc/nftables.d/vban-dante70.nft}"; }
_strih_vban_leg_unit_file() { printf '%s' "${STRIH_VBAN_LEG_UNIT_FILE:-/etc/systemd/system/${STRIH_VBAN_LEG_UNIT}}"; }
_strih_vban_leg_sysctl_file() { printf '%s' "${STRIH_VBAN_LEG_SYSCTL_FILE:-/etc/sysctl.d/60-vban-dante70.conf}"; }

# strih_vban_leg_declared -> rc 0 iff the loaded box declares a VBAN-only leg.
strih_vban_leg_declared() {
  local m
  m="$(strih_lx_program_vban_leg_mac)" || return 1
  [ "$m" != none ]
}

# strih_vban_leg_net ADDR -> the leg's /24 (A.B.C.0/24): the policy route for the ping replies.
strih_vban_leg_net() { printf '%s.0/24' "${1%.*}"; }

# strih_vban_leg_iface SYSFS MAC -> the kernel name of the interface whose address is MAC (rc 0); nothing
# + rc 1 when none has it; every name + rc 2 when several do. Never guessed from the MAC's shape.
strih_vban_leg_iface() {
  local sysfs="${1-}" mac="${2-}" f a found=""
  for f in "$sysfs"/class/net/*/address; do
    [ -r "$f" ] || continue
    a="$(tr '[:upper:]' '[:lower:]' <"$f" 2>/dev/null)" || continue
    if [ "$a" = "$mac" ]; then
      f="${f%/address}"
      found="${found}${found:+ }${f##*/}"
    fi
  done
  [ -n "$found" ] || return 1
  printf '%s\n' "$found"
  case "$found" in *" "*) return 2 ;; esac
  return 0
}

# strih_vban_leg_nft_text IF SENDER PORT -> the ruleset file (nft -f), see the header. Its rule text is
# exactly what nft 1.0.9 lists back (pinned against a netns render by the pytest).
strih_vban_leg_nft_text() {
  local ifc="${1:?interface required}" sender="${2:?sender required}" port="${3:?port required}"
  cat <<EOF
#!/usr/sbin/nft -f
# issue 1439 (ROZHODNUTE 6360417876): the VBAN-only leg ${ifc}. In: only ${sender}'s VBAN to UDP ${port}
# and echo requests; out: only echo replies; everything else dropped. The last input rule only COUNTS
# VBAN arriving on any other interface (verify-strih item 9: none may come through the router).
# Written by setup-strih step 13 (scripts/lib/strih-vban-leg.sh), applied by ${STRIH_VBAN_LEG_UNIT}.
table inet ${STRIH_VBAN_LEG_NFT_TABLE}
delete table inet ${STRIH_VBAN_LEG_NFT_TABLE}
table inet ${STRIH_VBAN_LEG_NFT_TABLE} {
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "${ifc}" ip saddr ${sender} udp dport ${port} counter accept
        iifname "${ifc}" icmp type echo-request counter accept
        iifname "${ifc}" counter drop
        iifname != "${ifc}" iifname != "lo" udp dport ${port} counter
    }
    chain output {
        type filter hook output priority filter; policy accept;
        oifname "${ifc}" icmp type echo-reply counter accept
        oifname "${ifc}" counter drop
    }
}
EOF
}

# strih_vban_leg_nft_normalize (stdin) -> the lines of an `nft list table` dump trimmed, every counter's
# values dropped, blank and comment lines gone: the form two dumps (or a dump and the file) compare in.
strih_vban_leg_nft_normalize() {
  LC_ALL=C sed -E -e 's/^[[:space:]]+//' -e 's/[[:space:]]+$//' -e '/^#/d' -e '/^$/d' \
    -e 's/counter packets [0123456789]+ bytes [0123456789]+/counter/'
}

# strih_vban_leg_nft_expected IF SENDER PORT -> what `nft list table inet vban_dante70` must read,
# normalized: the ruleset file's table block (one source for both).
strih_vban_leg_nft_expected() {
  strih_vban_leg_nft_text "$@" | sed -n '/^table .* {$/,$p' | strih_vban_leg_nft_normalize
}

# strih_vban_leg_unit_text NFT_FILE -> the boot oneshot (dscp-nft.sh's dantesync-dscp.service shape).
strih_vban_leg_unit_text() {
  cat <<EOF
[Unit]
Description=VBAN-only leg filter: only the program VBAN and ping on the second NIC (issue 1439)
Wants=network-pre.target
Before=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f ${1:?nft file required}

[Install]
WantedBy=multi-user.target
EOF
}

# strih_vban_leg_sysctl_text IF -> the sysctl.d drop-in (slash form: a kernel name may carry a dot).
strih_vban_leg_sysctl_text() {
  local ifc="${1:?interface required}"
  printf '# issue 1439: the VBAN-only leg %s answers ARP only for its own address, from it\n' "$ifc"
  printf 'net/ipv4/conf/%s/arp_ignore = 1\nnet/ipv4/conf/%s/arp_announce = 2\n' "$ifc" "$ifc"
}

# strih_vban_leg_nm_props IF ADDR -> `property<TAB>value` lines: the profile, as `nmcli -g` reads it back.
strih_vban_leg_nm_props() {
  local ifc="${1:?interface required}" addr="${2:?address required}"
  printf '%s\t%s\n' \
    connection.id "$STRIH_VBAN_LEG_PROFILE" \
    connection.interface-name "$ifc" \
    connection.autoconnect yes \
    ipv4.method manual \
    ipv4.addresses "${addr}/32" \
    ipv4.gateway "" \
    ipv4.never-default yes \
    ipv4.ignore-auto-routes yes \
    ipv4.ignore-auto-dns yes \
    ipv4.dns "" \
    ipv4.routes "$(strih_vban_leg_net "$addr") table=${STRIH_VBAN_LEG_ROUTE_TABLE}" \
    ipv4.routing-rules "priority ${STRIH_VBAN_LEG_RULE_PRIORITY} from ${addr} table ${STRIH_VBAN_LEG_ROUTE_TABLE}" \
    ipv6.method disabled
}

# _strih_vban_leg_nm_get UUID PROP -> the property as `nmcli -g` prints it, backslash escapes removed.
_strih_vban_leg_nm_get() {
  local v
  v="$(nmcli -g "$2" connection show "$1" </dev/null 2>/dev/null)" || return 1
  printf '%s' "${v//\\/}"
}

# _strih_vban_leg_nm_scan IF MAC -> one `UUID NAMED ON_LEG ACTIVE` line per wired profile that is ours
# by name or bound to the leg (its interface-name, its MAC, or active on it); rc 1 when nmcli cannot list.
_strih_vban_leg_nm_scan() {
  local ifc="$1" mac="$2" rows u t dev id ifn m named on act
  rows="$(nmcli -g UUID,TYPE,DEVICE connection show </dev/null 2>/dev/null)" || return 1
  while IFS=: read -r u t dev; do
    [ -n "$u" ] && [ "$t" = 802-3-ethernet ] || continue
    id="$(_strih_vban_leg_nm_get "$u" connection.id)" || id=""
    ifn="$(_strih_vban_leg_nm_get "$u" connection.interface-name)" || ifn=""
    m="$(_strih_vban_leg_nm_get "$u" 802-3-ethernet.mac-address | tr '[:upper:]' '[:lower:]')" || m=""
    named=0; on=0; act=0
    [ "$id" != "$STRIH_VBAN_LEG_PROFILE" ] || named=1
    [ "$dev" != "$ifc" ] || act=1
    if [ "$act" = 1 ] || [ "$ifn" = "$ifc" ] || { [ -n "$m" ] && [ "$m" = "$mac" ]; }; then on=1; fi
    if [ "$named" = 1 ] || [ "$on" = 1 ]; then printf '%s %s %s %s\n' "$u" "$named" "$on" "$act"; fi
  done <<<"$rows"
}

# _strih_vban_leg_nm_diff UUID IF ADDR -> one `prop: is 'X', wants 'Y'` line per property that differs.
_strih_vban_leg_nm_diff() {
  local u="$1" p want got
  while IFS=$'\t' read -r p want; do
    got="$(_strih_vban_leg_nm_get "$u" "$p")" || got="<unreadable>"
    [ "$got" = "$want" ] || printf "%s: is '%s', wants '%s'\n" "$p" "$got" "$want"
  done < <(strih_vban_leg_nm_props "$2" "$3")
}

# _strih_vban_leg_put PATH MODE TEXT -> `written` (temp file + rename in PATH's dir) or `unchanged` on
# stdout; rc 1 on a write error. Content + mode compared (root:root when run as root).
_strih_vban_leg_put() {
  local dest="$1" mode="$2" text="$3" tmp
  if [ -f "$dest" ] && [ "$(stat -c %a "$dest" 2>/dev/null)" = "${mode#0}" ] \
     && cmp -s "$dest" <(printf '%s\n' "$text"); then
    echo unchanged; return 0
  fi
  mkdir -p "${dest%/*}" || return 1
  tmp="$(mktemp "${dest%/*}/.${dest##*/}.XXXXXX")" || return 1
  if ! { printf '%s\n' "$text" >"$tmp" && chmod "$mode" "$tmp" && mv -f "$tmp" "$dest"; }; then
    rm -f "$tmp"; return 1
  fi
  echo written
}

# _strih_vban_leg_filter_apply IF SENDER PORT -> the ruleset file, the oneshot (enabled), the live table.
_strih_vban_leg_filter_apply() {
  local ifc="$1" sender="$2" port="$3" nf uf w1 w2 live want
  nf="$(_strih_vban_leg_nft_file)"; uf="$(_strih_vban_leg_unit_file)"
  if ! command -v nft >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends nftables \
      || { echo "strih-vban-leg: nft is missing and nftables could not be installed" >&2; return 1; }
  fi
  w1="$(_strih_vban_leg_put "$nf" 0644 "$(strih_vban_leg_nft_text "$ifc" "$sender" "$port")")" \
    || { echo "strih-vban-leg: could not write ${nf}" >&2; return 1; }
  w2="$(_strih_vban_leg_put "$uf" 0644 "$(strih_vban_leg_unit_text "$nf")")" \
    || { echo "strih-vban-leg: could not write ${uf}" >&2; return 1; }
  echo "  ${nf}: ${w1}; ${uf}: ${w2}"
  if [ "$w2" = written ]; then systemctl daemon-reload || return 1; fi
  if ! systemctl is-enabled -q "$STRIH_VBAN_LEG_UNIT" 2>/dev/null; then
    systemctl enable "$STRIH_VBAN_LEG_UNIT" || { echo "strih-vban-leg: could not enable ${STRIH_VBAN_LEG_UNIT}" >&2; return 1; }
  fi
  want="$(strih_vban_leg_nft_expected "$ifc" "$sender" "$port")"
  live="$(nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" 2>/dev/null | strih_vban_leg_nft_normalize || true)"
  if [ "$w1" = written ] || [ "$w2" = written ] || [ "$live" != "$want" ] \
     || ! systemctl is-active -q "$STRIH_VBAN_LEG_UNIT" 2>/dev/null; then
    systemctl restart "$STRIH_VBAN_LEG_UNIT" \
      || { echo "strih-vban-leg: ${STRIH_VBAN_LEG_UNIT} failed to load ${nf} (journalctl -u ${STRIH_VBAN_LEG_UNIT})" >&2; return 1; }
    live="$(nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" 2>/dev/null | strih_vban_leg_nft_normalize || true)"
    [ "$live" = "$want" ] \
      || { echo "strih-vban-leg: the live table inet ${STRIH_VBAN_LEG_NFT_TABLE} does not read back as ${nf} after the restart" >&2; return 1; }
    echo "  table inet ${STRIH_VBAN_LEG_NFT_TABLE} (re)loaded: only ${sender}'s VBAN to UDP ${port} + ping on ${ifc}"
  else
    echo "  table inet ${STRIH_VBAN_LEG_NFT_TABLE} live and unchanged"
  fi
}

# _strih_vban_leg_sysctl_apply IF -> the drop-in, applied, read back.
_strih_vban_leg_sysctl_apply() {
  local ifc="$1" sf w root d
  sf="$(_strih_vban_leg_sysctl_file)"; root="${STRIH_VBAN_LEG_PROC_SYS:-/proc/sys}"
  w="$(_strih_vban_leg_put "$sf" 0644 "$(strih_vban_leg_sysctl_text "$ifc")")" \
    || { echo "strih-vban-leg: could not write ${sf}" >&2; return 1; }
  sysctl -q -p "$sf" || { echo "strih-vban-leg: sysctl -p ${sf} failed" >&2; return 1; }
  d="${root}/net/ipv4/conf/${ifc}"
  if [ "$(cat "${d}/arp_ignore" 2>/dev/null)" != 1 ] || [ "$(cat "${d}/arp_announce" 2>/dev/null)" != 2 ]; then
    echo "strih-vban-leg: ${ifc} arp_ignore / arp_announce do not read back 1 / 2 after sysctl -p ${sf}" >&2; return 1
  fi
  echo "  ${sf}: ${w}; ${ifc} arp_ignore=1 arp_announce=2"
}

# _strih_vban_leg_nm_apply IF ADDR MAC -> the profile adopted / created, the extra ones deleted, applied
# without bouncing a correct leg, read back. rc 1 on an nmcli failure or a wrong read-back; an uncabled
# leg (activation refused) is a WARN, rc 0: the saved profile comes up with the cable.
_strih_vban_leg_nm_apply() {
  local ifc="$1" addr="$2" mac="$3" scan u named on act keep="" adopt="" first="" changed=0 diff
  local -a extra=() kv=()
  scan="$(_strih_vban_leg_nm_scan "$ifc" "$mac")" \
    || { echo "strih-vban-leg: nmcli cannot list the NetworkManager connections" >&2; return 1; }
  while read -r u named on act; do
    [ -n "$u" ] || continue
    if [ "$named" = 1 ] && [ -z "$keep" ]; then keep="$u"
    elif [ "$act" = 1 ] && [ -z "$adopt" ]; then adopt="$u"
    elif [ -z "$first" ]; then first="$u"
    fi
  done <<<"$scan"
  [ -n "$keep" ] || keep="${adopt:-$first}"
  while read -r u named on act; do
    if [ -n "$u" ] && [ "$u" != "$keep" ]; then extra+=("$u"); fi
  done <<<"$scan"
  while IFS=$'\t' read -r u named; do kv+=("$u" "$named"); done < <(strih_vban_leg_nm_props "$ifc" "$addr")
  if [ -z "$keep" ]; then
    # Created already static and pinned: a fresh profile never has a DHCP moment on the leg.
    nmcli connection add type ethernet con-name "$STRIH_VBAN_LEG_PROFILE" ifname "$ifc" "${kv[@]:4}" </dev/null >/dev/null \
      || { echo "strih-vban-leg: nmcli could not create the profile ${STRIH_VBAN_LEG_PROFILE} on ${ifc}" >&2; return 1; }
    keep="$(_strih_vban_leg_nm_scan "$ifc" "$mac" | while read -r u named on act; do [ "$named" = 1 ] && echo "$u"; done | head -n 1 || true)"
    [ -n "$keep" ] || { echo "strih-vban-leg: the new profile ${STRIH_VBAN_LEG_PROFILE} is not listed after nmcli add" >&2; return 1; }
    echo "  NM profile ${STRIH_VBAN_LEG_PROFILE} created on ${ifc}"
    changed=1
  else
    diff="$(_strih_vban_leg_nm_diff "$keep" "$ifc" "$addr")"
    if [ -n "$diff" ]; then
      nmcli connection modify "$keep" "${kv[@]}" </dev/null \
        || { echo "strih-vban-leg: nmcli could not set the profile ${keep} on ${ifc}" >&2; return 1; }
      echo "  NM profile ${keep} -> ${STRIH_VBAN_LEG_PROFILE} on ${ifc} (was: $(tr '\n' ';' <<<"$diff"))"
      changed=1
    else
      echo "  NM profile ${STRIH_VBAN_LEG_PROFILE} (${keep}) on ${ifc} unchanged"
    fi
  fi
  for u in "${extra[@]}"; do
    nmcli connection delete "$u" </dev/null >/dev/null \
      || { echo "strih-vban-leg: nmcli could not delete the second profile ${u} on ${ifc}" >&2; return 1; }
    echo "  NM profile ${u} on ${ifc} deleted (one profile on the leg)"
  done
  act="$(nmcli -g UUID,DEVICE connection show --active </dev/null 2>/dev/null | while IFS=: read -r u d; do [ "$d" = "$ifc" ] && echo "$u"; done | head -n 1 || true)"
  if [ "$act" = "$keep" ] && [ "$changed" = 0 ]; then
    :
  elif [ "$act" = "$keep" ] && nmcli device reapply "$ifc" </dev/null >/dev/null 2>&1; then
    echo "  ${ifc}: the profile reapplied in place (no link bounce)"
  elif nmcli --wait 30 connection up "$keep" ifname "$ifc" </dev/null >/dev/null; then
    echo "  ${ifc}: ${STRIH_VBAN_LEG_PROFILE} brought up"
  else
    echo "  WARN: ${ifc} did not come up with ${STRIH_VBAN_LEG_PROFILE} (is the leg cabled to the audio VLAN?) -- the profile is saved and comes up with the link; verify-strih item 9 grades it"
    return 0
  fi
  if diff="$(_strih_vban_leg_live_problems "$ifc" "$addr")"; then
    echo "strih-vban-leg: ${ifc} after applying ${STRIH_VBAN_LEG_PROFILE}: $(tr '\n' ';' <<<"$diff")" >&2
    return 1
  fi
  echo "  ${ifc}: ${addr}/32, no route through it but the ping-reply rule ${STRIH_VBAN_LEG_RULE_PRIORITY} -> table ${STRIH_VBAN_LEG_ROUTE_TABLE}"
}

# _strih_vban_leg_live_problems IF ADDR -> one line per thing the kernel shows wrong on the leg (rc 0 when
# there is one, rc 1 when the leg is right): the addresses, a default route or a main-table route through
# it, the policy rule, the table-70 route.
_strih_vban_leg_live_problems() {
  local ifc="$1" addr="$2" got net t n=0 line
  net="$(strih_vban_leg_net "$addr")"
  got=""
  while read -r line; do
    t="${line#*inet }"; [ "$t" != "$line" ] || continue
    got="${got}${got:+ }${t%% *}"
  done < <(ip -4 -o addr show dev "$ifc" 2>/dev/null || true)
  if [ "$got" != "${addr}/32" ]; then echo "the leg ${ifc} carries '${got:-no address}', not exactly ${addr}/32"; n=1; fi
  # every read is captured first and grepped from a here-string: a grep -q on a pipe can SIGPIPE its
  # producer under the caller's pipefail and read as "no match"
  t="$(ip -4 route show default 2>/dev/null || true)"
  line="$(grep -F -- " dev ${ifc} " <<<"$t" || true)"
  if [ -n "$line" ]; then echo "a default route leaves through the leg ${ifc}: ${line%%$'\n'*}"; n=1; fi
  t="$(ip -4 route show dev "$ifc" 2>/dev/null || true)"
  if [ -n "$t" ]; then echo "the main table routes through the leg ${ifc}: ${t%%$'\n'*}"; n=1; fi
  t="$(ip -4 rule show 2>/dev/null || true)"
  if ! grep -E "^${STRIH_VBAN_LEG_RULE_PRIORITY}:[[:space:]]+from ${addr//./\\.} lookup ${STRIH_VBAN_LEG_ROUTE_TABLE}( |$)" <<<"$t" >/dev/null; then
    echo "no policy rule '${STRIH_VBAN_LEG_RULE_PRIORITY}: from ${addr} lookup ${STRIH_VBAN_LEG_ROUTE_TABLE}' (the ping replies would leave through the rig NIC)"; n=1
  fi
  t="$(ip -4 route show table "$STRIH_VBAN_LEG_ROUTE_TABLE" 2>/dev/null || true)"
  if ! grep -F -- "${net} dev ${ifc} " <<<"$t" >/dev/null; then
    echo "no '${net} dev ${ifc}' route in table ${STRIH_VBAN_LEG_ROUTE_TABLE}"; n=1
  fi
  [ "$n" = 1 ]
}

# strih_vban_leg_install -> setup-strih step 13 on the stream role. rc 0 (also a SKIP without a leg), rc 1 +
# stderr naming what failed. Order: the filter, the ARP sysctls, then the profile (the leg never carries
# its address unfiltered).
strih_vban_leg_install() {
  local mac addr sender port ifc rc=0
  if ! strih_vban_leg_declared; then
    echo "  SKIP: no VBAN-only leg declared (STRIH_PROGRAM_VBAN_LEG_MAC=none) -- the program VBAN reaches this box through the rig NIC"
    return 0
  fi
  mac="$(strih_lx_program_vban_leg_mac)"; addr="$(strih_lx_program_vban_leg_addr)"
  sender="$(strih_lx_program_vban_sender)"; port="$(strih_lx_program_vban_port)"
  ifc="$(strih_vban_leg_iface "${STRIH_VBAN_LEG_SYSFS:-/sys}" "$mac")" || rc=$?
  if [ "$rc" = 1 ]; then
    echo "strih-vban-leg: no network interface has the MAC ${mac} (fact STRIH_PROGRAM_VBAN_LEG_MAC: the VBAN-only leg's USB-C NIC) -- plug it in; nothing changed" >&2
    return 1
  elif [ "$rc" != 0 ]; then
    echo "strih-vban-leg: several interfaces carry the MAC ${mac}: ${ifc} -- nothing changed" >&2
    return 1
  fi
  echo "  the VBAN-only leg: ${ifc} (MAC ${mac}) -> ${addr}/32, only ${sender}'s VBAN to UDP ${port} + ping"
  _strih_vban_leg_filter_apply "$ifc" "$sender" "$port" || return 1
  _strih_vban_leg_sysctl_apply "$ifc" || return 1
  _strih_vban_leg_nm_apply "$ifc" "$addr" "$mac" || return 1
}

# _strih_vban_leg_rule_packets LISTING RULE -> the packet count of the rule whose normalized text is RULE
# in the raw `nft list table` LISTING ('' when absent).
_strih_vban_leg_rule_packets() {
  local line
  while IFS= read -r line; do
    if [ "$(printf '%s\n' "$line" | strih_vban_leg_nft_normalize)" = "$2" ]; then
      line="${line#*counter packets }"
      printf '%s' "${line%% *}"
      return 0
    fi
  done <<<"$1"
}

# strih_vban_leg_grade -> verify-strih item 9's `(vban-leg)` rows (through the caller's ok / bad / note).
strih_vban_leg_grade() {
  local mac addr sender port stream ifc rc=0 scan u named on act keep="" others="" diff p d ai aa
  local want l1 l2 acc_rule off_rule a1 a2 o1 o2 wait="${STRIH_VBAN_LEG_WAIT_S:-3}"
  if ! strih_vban_leg_declared; then
    note "(vban-leg) no VBAN-only leg declared (STRIH_PROGRAM_VBAN_LEG_MAC=none): the program VBAN reaches this box through the rig NIC"
    return 0
  fi
  mac="$(strih_lx_program_vban_leg_mac)"; addr="$(strih_lx_program_vban_leg_addr)"
  sender="$(strih_lx_program_vban_sender)"; port="$(strih_lx_program_vban_port)"
  stream="$(strih_lx_program_vban_streams)"
  ifc="$(strih_vban_leg_iface "${STRIH_VBAN_LEG_SYSFS:-/sys}" "$mac")" || rc=$?
  if [ "$rc" != 0 ]; then
    bad "(vban-leg) ${ifc:+several interfaces (${ifc}) / }no single network interface has the MAC ${mac}: the VBAN-only leg's USB-C NIC is unplugged or dead (fact STRIH_PROGRAM_VBAN_LEG_MAC) -- the program VBAN cannot arrive"
    return 0
  fi
  # the NetworkManager profile: ours, as provisioned, the only one on the leg
  if ! scan="$(_strih_vban_leg_nm_scan "$ifc" "$mac")"; then
    bad "(vban-leg) profile: nmcli cannot list the NetworkManager connections"
  else
    while read -r u named on act; do
      [ -n "$u" ] || continue
      if [ "$named" = 1 ] && [ -z "$keep" ]; then keep="$u"; else others="${others}${others:+, }$(_strih_vban_leg_nm_get "$u" connection.id || true) (${u})"; fi
    done <<<"$scan"
    if [ -z "$keep" ]; then
      bad "(vban-leg) profile: no NetworkManager profile ${STRIH_VBAN_LEG_PROFILE} for ${ifc}${others:+ (on it: ${others})} -- re-run setup-strih.sh step 13"
    else
      diff="$(_strih_vban_leg_nm_diff "$keep" "$ifc" "$addr")"
      if [ -n "$diff" ] || [ -n "$others" ]; then
        bad "(vban-leg) profile ${STRIH_VBAN_LEG_PROFILE}: $(tr '\n' ';' <<<"$diff")${others:+ other profiles on ${ifc}: ${others}} -- re-run setup-strih.sh step 13"
      else
        ok "(vban-leg) profile ${STRIH_VBAN_LEG_PROFILE} on ${ifc}: ${addr}/32, no gateway, never-default, the ping-reply route in table ${STRIH_VBAN_LEG_ROUTE_TABLE}"
      fi
    fi
  fi
  # the kernel's view: the /32, no route through the leg but the policy one
  if p="$(_strih_vban_leg_live_problems "$ifc" "$addr")"; then
    bad "(vban-leg) routes: $(tr '\n' ';' <<<"$p") -- only VBAN may use the leg (re-run setup-strih.sh step 13)"
  else
    ok "(vban-leg) routes: ${ifc} = ${addr}/32 only, no default or main-table route through it, rule ${STRIH_VBAN_LEG_RULE_PRIORITY} -> table ${STRIH_VBAN_LEG_ROUTE_TABLE}"
  fi
  # the ARP sysctls
  d="${STRIH_VBAN_LEG_PROC_SYS:-/proc/sys}/net/ipv4/conf/${ifc}"
  ai="$(cat "${d}/arp_ignore" 2>/dev/null || echo '?')"; aa="$(cat "${d}/arp_announce" 2>/dev/null || echo '?')"
  if [ "$ai" = 1 ] && [ "$aa" = 2 ]; then
    ok "(vban-leg) arp: ${ifc} arp_ignore=1 arp_announce=2"
  else
    bad "(vban-leg) arp: ${ifc} arp_ignore=${ai} arp_announce=${aa}, wants 1 / 2 ($(_strih_vban_leg_sysctl_file), re-run setup-strih.sh step 13)"
  fi
  # the filter: the unit, the live table, then the two counters over the window
  if ! l1="$(nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" 2>/dev/null)"; then
    bad "(vban-leg) filter: cannot read table inet ${STRIH_VBAN_LEG_NFT_TABLE} (missing, or verify-strih not run as root) -- ${STRIH_VBAN_LEG_UNIT} loads it"
    return 0
  fi
  want="$(strih_vban_leg_nft_expected "$ifc" "$sender" "$port")"
  if [ "$(printf '%s\n' "$l1" | strih_vban_leg_nft_normalize)" != "$want" ] \
     || [ "$(systemctl is-enabled "$STRIH_VBAN_LEG_UNIT" 2>/dev/null || true)" != enabled ] \
     || ! systemctl is-active -q "$STRIH_VBAN_LEG_UNIT" 2>/dev/null; then
    bad "(vban-leg) filter: table inet ${STRIH_VBAN_LEG_NFT_TABLE} is not $(_strih_vban_leg_nft_file) live, or ${STRIH_VBAN_LEG_UNIT} is not enabled + active (a reboot would leave the leg open) -- re-run setup-strih.sh step 13"
  else
    ok "(vban-leg) filter: ${STRIH_VBAN_LEG_UNIT} enabled + active, table inet ${STRIH_VBAN_LEG_NFT_TABLE} = in only ${sender}'s VBAN to UDP ${port} + ping, out only echo replies"
  fi
  acc_rule="iifname \"${ifc}\" ip saddr ${sender} udp dport ${port} counter accept"
  off_rule="iifname != \"${ifc}\" iifname != \"lo\" udp dport ${port} counter"
  a1="$(_strih_vban_leg_rule_packets "$l1" "$acc_rule")"; o1="$(_strih_vban_leg_rule_packets "$l1" "$off_rule")"
  sleep "$wait"
  l2="$(nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" 2>/dev/null || true)"
  a2="$(_strih_vban_leg_rule_packets "$l2" "$acc_rule")"; o2="$(_strih_vban_leg_rule_packets "$l2" "$off_rule")"
  if [[ "$a1" =~ ^[0123456789]+$ ]] && [[ "$a2" =~ ^[0123456789]+$ ]]; then
    if [ "$a2" -gt "$a1" ]; then
      ok "(vban-leg) vban: $((a2 - a1)) packets from ${sender} to UDP ${port} on ${ifc} in ${wait} s"
    else
      bad "(vban-leg) vban: no VBAN from ${sender} arrived on ${ifc} in ${wait} s -- is the console ${sender} sending stream '${stream}' to ${addr}:${port}?"
    fi
  fi
  if [[ "$o1" =~ ^[0123456789]+$ ]] && [[ "$o2" =~ ^[0123456789]+$ ]]; then
    if [ "$o2" -gt "$o1" ]; then
      bad "(vban-leg) off-leg: $((o2 - o1)) packets to UDP ${port} arrived off the leg (not on ${ifc}) in ${wait} s -- the console still sends to another address of this box (through the router); point it at ${addr}"
    else
      ok "(vban-leg) off-leg: no packet to UDP ${port} arrived on any other interface in ${wait} s"
    fi
  fi
  return 0
}
