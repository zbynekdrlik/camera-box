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
# The leg sits on the console's audio VLAN, box facts STRIH_PROGRAM_VBAN_LEG / _LEG_ADDR /
# STRIH_PROGRAM_VBAN_SENDER (the port is STRIH_PROGRAM_VBAN_PORT). Two kinds:
#   mac:<MAC>  a second NIC (stream-snv's USB-C r8152), its kernel name resolved by the MAC of a PHYSICAL
#              interface (a device link; a VLAN on it shares the MAC), never guessed;
#   vlan:<id>  a TAGGED VLAN on the rig NIC (strih-snv: VLAN 70 on enp7s0 since 23.10.2026 10:31, the
#              switch port carries the rig VLAN 89 untagged + 70 tagged), interface `<rig nic>.<id>`.
# Four parts, every one idempotent:
#   1. the NetworkManager profile `vban-dante70` pinned to the leg: a /32 (no connected route, so
#      nothing to the audio VLAN leaves through it), no gateway, never-default, no auto routes / DNS, no
#      IPv6, plus ONE policy route so a ping reply leaves through the leg (route `<leg /24> table 70` +
#      rule `priority 100 from <addr> table 70`). A changed profile is `device reapply`d in place.
#      - mac: the profile on the NIC (a fresh install brings it up by DHCP, with a second default route)
#        is ADOPTED and every other profile on it deleted; a refused reapply falls back to `connection
#        up` (the leg is not the ssh link).
#      - vlan, the HARD trap (supervisor 23.10.2026): the atlantic driver RESETS the rig link for ~8 s
#        (`link change old 10000 new 0`, carrier lost) the FIRST time a VLAN enters its HW filter
#        (`8021q: adding VLAN 0 to HW filter`) -- NDI, PTP and ssh on the production box go with it. So
#        the VLAN connection is created ONLY when absent, after a loud WARNING, created down and brought
#        up exactly once (then setup waits, bounded, for the reset to come and go: carrier_changes; a
#        failed first up is never armed); it is NEVER deleted,
#        re-created, brought down or up again: an address / route / rule change is modify + `device
#        reapply`, a refused reapply FAILS by name, an identity change (vlan.parent, vlan.id, the interface
#        name) and an existing INACTIVE VLAN profile are refused untouched -- arming its autoconnect would
#        make NetworkManager activate it at once (a maintenance-window job).
#   2. nftables `table inet vban_dante70` (the house pattern of scripts/lib/dscp-nft.sh: a dedicated
#      table, `table` / `delete table` atomic replace, never `flush ruleset`, applied by its own boot
#      oneshot, never the distro nftables.service): IN on the leg only the sender's VBAN + echo
#      requests, OUT on the leg only echo replies (owner: ping "aby sme mali aspon nejake info ze to
#      bezi"), everything else dropped and counted; ARP is not IP and still answers. With a sender, one
#      counter-only rule (no verdict) of the SENDER's VBAN arriving on any OTHER interface (a routed packet
#      keeps its source address): verify's "VBAN no longer comes through the router" proof. Without one (STRIH_PROGRAM_VBAN_SENDER=none, strih-snv until
#      phase 2) the leg passes only ping. dantesync keeps video-clock.lan on the rig NIC and never sees
#      audio-VLAN PTP (the input drop); avahi is never touched (the output drop keeps mDNS off the leg).
#   3. the per-interface ARP sysctls (arp_ignore=1, arp_announce=2) in /etc/sysctl.d, slash form keyed
#      on the kernel name (a VLAN name has a dot: net/ipv4/conf/enp7s0.70/arp_ignore): systemd's
#      99-systemd.rules runs `systemd-sysctl --prefix=/net/ipv4/conf/$name` on every net device add, so
#      they apply at boot, on a USB re-plug and when NM creates the VLAN (read on stream-snv 23.10.2026).
#   4. the filter goes in BEFORE the leg takes its address.
#
# setup-strih step 13 (every role): strih_vban_leg_install. verify-strih item 9 (every role):
# strih_vban_leg_grade, read-only (never the link), through the caller's ok / bad / note (each rc 0).
# Run verify as root (nft).
#
# Seams (tests/python/test_vban_leg_1439.py): STRIH_VBAN_LEG_SYSFS (/sys), STRIH_VBAN_LEG_PROC_SYS
# (/proc/sys), STRIH_VBAN_LEG_NFT_FILE, STRIH_VBAN_LEG_UNIT_FILE, STRIH_VBAN_LEG_SYSCTL_FILE,
# STRIH_VBAN_LEG_WAIT_S (the counter window, 3 s), STRIH_VBAN_LEG_LINK_WAIT_S (30 s) +
# STRIH_VBAN_LEG_RESET_SEEN_S (10 s) (the rig-link wait); nmcli / ip / nft / systemctl / sysctl from PATH;
# STRIH_NIC_IFACE overrides the rig NIC like everywhere else.

STRIH_VBAN_LEG_PROFILE=vban-dante70
STRIH_VBAN_LEG_NFT_TABLE=vban_dante70
STRIH_VBAN_LEG_UNIT=vban-dante70-filter.service
STRIH_VBAN_LEG_ROUTE_TABLE=70
STRIH_VBAN_LEG_RULE_PRIORITY=100

_strih_vban_leg_nft_file() { printf '%s' "${STRIH_VBAN_LEG_NFT_FILE:-/etc/nftables.d/vban-dante70.nft}"; }
_strih_vban_leg_unit_file() { printf '%s' "${STRIH_VBAN_LEG_UNIT_FILE:-/etc/systemd/system/${STRIH_VBAN_LEG_UNIT}}"; }
_strih_vban_leg_sysctl_file() { printf '%s' "${STRIH_VBAN_LEG_SYSCTL_FILE:-/etc/sysctl.d/60-vban-dante70.conf}"; }
_strih_vban_leg_sysfs() { printf '%s' "${STRIH_VBAN_LEG_SYSFS:-/sys}"; }

# strih_vban_leg_kind -> mac | vlan | none (the loaded box's STRIH_PROGRAM_VBAN_LEG).
strih_vban_leg_kind() {
  local v
  v="$(strih_lx_program_vban_leg)" || return 1
  case "$v" in mac:*) echo mac ;; vlan:*) echo vlan ;; *) echo none ;; esac
}

# strih_vban_leg_net ADDR -> the leg's /24 (A.B.C.0/24): the policy route for the ping replies.
strih_vban_leg_net() { printf '%s.0/24' "${1%.*}"; }

# strih_vban_leg_iface SYSFS MAC -> the kernel name of the PHYSICAL interface (one with a device link)
# whose address is MAC (rc 0); nothing + rc 1 when none has it; every name + rc 2 when several do.
strih_vban_leg_iface() {
  local sysfs="${1-}" mac="${2-}" f a d found=""
  for f in "$sysfs"/class/net/*/address; do
    [ -r "$f" ] || continue
    d="${f%/address}"
    [ -e "${d}/device" ] || continue
    a="$(tr '[:upper:]' '[:lower:]' <"$f" 2>/dev/null)" || continue
    if [ "$a" = "$mac" ]; then found="${found}${found:+ }${d##*/}"; fi
  done
  [ -n "$found" ] || return 1
  printf '%s\n' "$found"
  case "$found" in *" "*) return 2 ;; esac
  return 0
}

# strih_vban_leg_resolve -> `IFACE PARENT VLAN_ID` of the loaded box's leg on stdout (PARENT / VLAN_ID `-`
# for the mac kind), rc 0; rc 1 + one stderr line naming what is missing (the MAC, the rig NIC, a name
# over the kernel's 15 characters). Reads only.
strih_vban_leg_resolve() {
  local leg mac id parent ifc rc=0 sysfs
  leg="$(strih_lx_program_vban_leg)" || return 1
  sysfs="$(_strih_vban_leg_sysfs)"
  case "$leg" in
    mac:*)
      mac="${leg#mac:}"
      ifc="$(strih_vban_leg_iface "$sysfs" "$mac")" || rc=$?
      if [ "$rc" = 1 ]; then
        echo "no network interface has the MAC ${mac} (fact STRIH_PROGRAM_VBAN_LEG: the VBAN-only leg's NIC) -- plug it in" >&2; return 1
      elif [ "$rc" != 0 ]; then
        echo "several interfaces carry the MAC ${mac}: ${ifc}" >&2; return 1
      fi
      printf '%s - -\n' "$ifc" ;;
    vlan:*)
      id="${leg#vlan:}"
      parent="$(strih_lx_rig_nic "$sysfs" "$(ip -o -4 addr show 2>/dev/null || true)" "$(strih_lx_ip)" 2>/dev/null)" \
        || { echo "the rig NIC cannot be resolved (driver $(strih_lx_nic_driver 2>/dev/null || echo '?'), STRIH_IP $(strih_lx_ip 2>/dev/null || echo '?')) -- the VLAN ${id} leg rides on it; set STRIH_NIC_IFACE" >&2; return 1; }
      ifc="${parent}.${id}"
      [ "${#ifc}" -le 15 ] || { echo "the VLAN interface name ${ifc} is over the kernel's 15 characters" >&2; return 1; }
      printf '%s %s %s\n' "$ifc" "$parent" "$id" ;;
    *) echo "no VBAN-only leg declared" >&2; return 1 ;;
  esac
}

# strih_vban_leg_nft_text IF SENDER PORT -> the ruleset file (nft -f), see the header; SENDER `none` = only
# ping. Its rule text is exactly what nft 1.0.9 lists back (pinned against a netns render and the live
# strih-snv table by the pytest).
strih_vban_leg_nft_text() {
  local ifc="${1:?interface required}" sender="${2:?sender required}" port="${3:?port required}"
  local vin="" voff="" what="only echo requests (no VBAN sender declared yet)"
  if [ "$sender" != none ]; then
    vin="        iifname \"${ifc}\" ip saddr ${sender} udp dport ${port} counter accept"$'\n'
    voff="        iifname != \"${ifc}\" iifname != \"lo\" ip saddr ${sender} udp dport ${port} counter"$'\n'
    what="only ${sender}'s VBAN to UDP ${port} and echo requests"
  fi
  printf '%s\n' "#!/usr/sbin/nft -f" \
    "# issue 1439 (ROZHODNUTE 6360417876): the VBAN-only leg ${ifc}. In: ${what};" \
    "# out: only echo replies; everything else dropped. A counter-only input rule (with a sender) counts" \
    "# the sender's VBAN arriving on any other interface (verify-strih item 9: none may come routed)." \
    "# Written by setup-strih step 13 (scripts/lib/strih-vban-leg.sh), applied by ${STRIH_VBAN_LEG_UNIT}." \
    "table inet ${STRIH_VBAN_LEG_NFT_TABLE}" \
    "delete table inet ${STRIH_VBAN_LEG_NFT_TABLE}" \
    "table inet ${STRIH_VBAN_LEG_NFT_TABLE} {" \
    "    chain input {" \
    "        type filter hook input priority filter; policy accept;"
  printf '%s' "$vin"
  printf '%s\n' "        iifname \"${ifc}\" icmp type echo-request counter accept" \
    "        iifname \"${ifc}\" counter drop"
  printf '%s' "$voff"
  printf '%s\n' "    }" \
    "    chain output {" \
    "        type filter hook output priority filter; policy accept;" \
    "        oifname \"${ifc}\" icmp type echo-reply counter accept" \
    "        oifname \"${ifc}\" counter drop" \
    "    }" \
    "}"
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
Description=VBAN-only leg filter: only the program VBAN and ping on the leg (issue 1439)
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

# strih_vban_leg_sysctl_text IF -> the sysctl.d drop-in (slash form: a VLAN name carries a dot).
strih_vban_leg_sysctl_text() {
  local ifc="${1:?interface required}"
  printf '# issue 1439: the VBAN-only leg %s answers ARP only for its own address, from it\n' "$ifc"
  printf 'net/ipv4/conf/%s/arp_ignore = 1\nnet/ipv4/conf/%s/arp_announce = 2\n' "$ifc" "$ifc"
}

# strih_vban_leg_nm_props IF ADDR [PARENT VLAN_ID] -> `property<TAB>value` lines: the profile, as
# `nmcli -g` reads it back (the vlan kind adds vlan.parent + vlan.id after the interface name).
strih_vban_leg_nm_props() {
  local ifc="${1:?interface required}" addr="${2:?address required}" parent="${3:--}" id="${4:--}"
  printf '%s\t%s\n' connection.id "$STRIH_VBAN_LEG_PROFILE" connection.interface-name "$ifc"
  if [ "$parent" != - ]; then printf '%s\t%s\n' vlan.parent "$parent" vlan.id "$id"; fi
  printf '%s\t%s\n' \
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

# _strih_vban_leg_nm_scan IF MAC PARENT VLAN_ID -> one `UUID NAMED ON_LEG ACTIVE` line per profile of the
# leg's type that is ours by name or bound to the leg (mac: its interface-name, its MAC, or active on it;
# vlan: its interface-name, active on it, or the same parent + id); rc 1 when nmcli cannot list.
_strih_vban_leg_nm_scan() {
  local ifc="$1" mac="$2" parent="$3" id="$4" want=802-3-ethernet rows u t dev ifn m named on act
  [ "$parent" = - ] || want=vlan
  rows="$(nmcli -g UUID,TYPE,DEVICE connection show </dev/null 2>/dev/null)" || return 1
  while IFS=: read -r u t dev; do
    [ -n "$u" ] || continue
    [ "$t" = "$want" ] || continue
    named=0; on=0; act=0
    [ "$(_strih_vban_leg_nm_get "$u" connection.id || true)" != "$STRIH_VBAN_LEG_PROFILE" ] || named=1
    [ "$dev" != "$ifc" ] || act=1
    ifn="$(_strih_vban_leg_nm_get "$u" connection.interface-name || true)"
    if [ "$act" = 1 ] || [ "$ifn" = "$ifc" ]; then on=1
    elif [ "$want" = vlan ]; then
      if [ "$(_strih_vban_leg_nm_get "$u" vlan.parent || true)" = "$parent" ] \
         && [ "$(_strih_vban_leg_nm_get "$u" vlan.id || true)" = "$id" ]; then on=1; fi
    else
      m="$(_strih_vban_leg_nm_get "$u" 802-3-ethernet.mac-address | tr '[:upper:]' '[:lower:]' || true)"
      if [ -n "$m" ] && [ "$m" = "$mac" ]; then on=1; fi
    fi
    if [ "$named" = 1 ] || [ "$on" = 1 ]; then printf '%s %s %s %s\n' "$u" "$named" "$on" "$act"; fi
  done <<<"$rows"
}

# _strih_vban_leg_nm_diff UUID IF ADDR PARENT VLAN_ID -> one `prop: is 'X', wants 'Y'` line per property
# that differs.
_strih_vban_leg_nm_diff() {
  local u="$1" p want got
  while IFS=$'\t' read -r p want; do
    got="$(_strih_vban_leg_nm_get "$u" "$p")" || got="<unreadable>"
    [ "$got" = "$want" ] || printf "%s: is '%s', wants '%s'\n" "$p" "$got" "$want"
  done < <(strih_vban_leg_nm_props "$2" "$3" "$4" "$5")
}

# _strih_vban_leg_put PATH MODE TEXT -> `written` (temp file + rename in PATH's dir) or `unchanged` on
# stdout; rc 1 on a write error. Content, mode and (as root) root:root ownership compared. Not
# obs_box_write_if_changed: the caller needs the written/unchanged answer (a written ruleset reloads the
# table) and its own named error, and that helper `fail`s the whole script (setup's fail is not defined
# where this lib is tested).
_strih_vban_leg_put() {
  local dest="$1" mode="$2" text="$3" tmp
  if [ -f "$dest" ] && [ "$(stat -c %a "$dest" 2>/dev/null)" = "${mode#0}" ] \
     && { [ "$(id -u)" != 0 ] || [ "$(stat -c %U:%G "$dest" 2>/dev/null)" = root:root ]; } \
     && cmp -s "$dest" <(printf '%s\n' "$text"); then
    echo unchanged; return 0
  fi
  mkdir -p "${dest%/*}" || return 1
  tmp="$(mktemp "${dest%/*}/.${dest##*/}.XXXXXX")" || return 1
  if [ "$(id -u)" = 0 ]; then chown root:root "$tmp" || { rm -f "$tmp"; return 1; }; fi
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
  if [ "$w2" = written ]; then
    systemctl daemon-reload || { echo "strih-vban-leg: systemctl daemon-reload failed after writing ${uf}" >&2; return 1; }
  fi
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
    echo "  table inet ${STRIH_VBAN_LEG_NFT_TABLE} (re)loaded on ${ifc}"
  else
    echo "  table inet ${STRIH_VBAN_LEG_NFT_TABLE} live and unchanged"
  fi
}

# _strih_vban_leg_sysctl_apply IF -> the drop-in, applied (when the interface exists), read back.
_strih_vban_leg_sysctl_apply() {
  local ifc="$1" sf w d
  sf="$(_strih_vban_leg_sysctl_file)"
  w="$(_strih_vban_leg_put "$sf" 0644 "$(strih_vban_leg_sysctl_text "$ifc")")" \
    || { echo "strih-vban-leg: could not write ${sf}" >&2; return 1; }
  d="${STRIH_VBAN_LEG_PROC_SYS:-/proc/sys}/net/ipv4/conf/${ifc}"
  if [ ! -e "$(_strih_vban_leg_sysfs)/class/net/${ifc}" ]; then
    echo "  ${sf}: ${w} (${ifc} does not exist yet: udev applies it when it appears)"
    return 0
  fi
  sysctl -q -p "$sf" || { echo "strih-vban-leg: sysctl -p ${sf} failed" >&2; return 1; }
  if [ "$(cat "${d}/arp_ignore" 2>/dev/null)" != 1 ] || [ "$(cat "${d}/arp_announce" 2>/dev/null)" != 2 ]; then
    echo "strih-vban-leg: ${ifc} arp_ignore / arp_announce do not read back 1 / 2 after sysctl -p ${sf}" >&2; return 1
  fi
  echo "  ${sf}: ${w}; ${ifc} arp_ignore=1 arp_announce=2"
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

# _strih_vban_leg_carrier_changes PARENT -> the rig NIC's carrier_changes counter ('' when unreadable).
_strih_vban_leg_carrier_changes() {
  cat "$(_strih_vban_leg_sysfs)/class/net/${1}/carrier_changes" 2>/dev/null || true
}

# _strih_vban_leg_rig_link_wait PARENT CHANGES_BEFORE -> after a first VLAN activation, wait (bounded,
# STRIH_VBAN_LEG_LINK_WAIT_S, 30 s) for the atlantic reset to COME AND GO: carrier_changes up by at least 2
# since CHANGES_BEFORE (read before the activation: down + up), carrier 1 and a default route through the
# rig NIC. atlantic reports the carrier drop from its link poll up to ~1 s late and NetworkManager holds
# the route a moment longer, so the first read after the activation can look healthy (review round 2).
# A NIC whose counter does not move within STRIH_VBAN_LEG_RESET_SEEN_S (10 s) while its link is up had no
# reset: one note, return. A link that stays down: one WARNING at the bound. Always rc 0.
_strih_vban_leg_rig_link_wait() {
  local parent="$1" before="${2-}" n=0 max="${STRIH_VBAN_LEG_LINK_WAIT_S:-30}" seen="${STRIH_VBAN_LEG_RESET_SEEN_S:-10}"
  local t now up
  [[ "$max" =~ ^[0123456789]+$ ]] || max=30
  [[ "$seen" =~ ^[0123456789]+$ ]] || seen=10
  while :; do
    t="$(ip -4 route show default 2>/dev/null || true)"
    now="$(_strih_vban_leg_carrier_changes "$parent")"
    up=0
    if [ "$(cat "$(_strih_vban_leg_sysfs)/class/net/${parent}/carrier" 2>/dev/null || true)" = 1 ] \
       && grep -F -- " dev ${parent} " <<<"$t" >/dev/null; then up=1; fi
    if [ "$up" = 1 ] && [[ "$before" =~ ^[0123456789]+$ ]] && [[ "$now" =~ ^[0123456789]+$ ]] \
       && [ "$((now - before))" -ge 2 ]; then
      echo "  the rig link ${parent} is up again (reset seen: carrier_changes ${before} -> ${now}, carrier + default route) after ${n} s"
      return 0
    fi
    if [ "$up" = 1 ] && [ "$n" -ge "$seen" ] && { [ -z "$now" ] || [ "$now" = "$before" ]; }; then
      echo "  NOTE: no carrier change on the rig link ${parent} in ${seen} s after the VLAN came up (carrier_changes ${before:-?}): no reset on this NIC, the link is up"
      return 0
    fi
    if [ "$n" -ge "$max" ]; then
      echo "  WARNING: the rig link ${parent} is not back after ${max} s (carrier_changes ${before:-?} -> ${now:-?}, carrier or default route missing) -- the next steps may fail on the network; re-run setup-strih when it is"
      return 0
    fi
    sleep 1
    n=$((n + 1))
  done
}

# _strih_vban_leg_vlan_warning PARENT ID -> the loud line printed before a first VLAN creation.
_strih_vban_leg_vlan_warning() {
  echo "  WARNING: creating VLAN ${2} on the rig NIC ${1} -- the atlantic driver resets the rig link for ~8 s the first time a VLAN enters its HW filter (issue 1439, 23.10.2026): NDI, PTP and ssh on this box drop for that time; done once, never again on a re-run"
}

# _strih_vban_leg_active_on IF -> the UUID of the profile active on IF ('' when none); rc 1 + a named
# stderr line when nmcli cannot list the active connections (never read as "not active").
_strih_vban_leg_active_on() {
  local rows u p
  rows="$(nmcli -g UUID,DEVICE connection show --active </dev/null 2>/dev/null)" \
    || { echo "strih-vban-leg: nmcli cannot list the active connections -- nothing touched" >&2; return 1; }
  while IFS=: read -r u p; do
    if [ "$p" = "$1" ]; then echo "$u"; return 0; fi
  done <<<"$rows"
  return 0
}

# _strih_vban_leg_nm_apply IF ADDR MAC PARENT VLAN_ID -> the profile adopted / created, the extra ones
# deleted, applied in place, read back (see the header for the vlan kind's rules). rc 1 on an nmcli failure
# or a wrong read-back; an uncabled leg (activation refused) is a WARN, rc 0.
_strih_vban_leg_nm_apply() {
  local ifc="$1" addr="$2" mac="$3" parent="$4" id="$5" scan u named on act keep="" adopt="" first=""
  local reapply=0 diff p cc0
  local -a extra=() kv=() addkv=() upargs=()
  scan="$(_strih_vban_leg_nm_scan "$ifc" "$mac" "$parent" "$id")" \
    || { echo "strih-vban-leg: nmcli cannot list the NetworkManager connections" >&2; return 1; }
  while read -r u named on act; do
    [ -n "$u" ] || continue
    if [ "$named" = 1 ] && [ -z "$keep" ]; then keep="$u"; fi
    if [ "$act" = 1 ] && [ -z "$adopt" ]; then adopt="$u"; fi
    if [ -z "$first" ]; then first="$u"; fi
  done <<<"$scan"
  # vlan: the ACTIVE profile is kept whatever its name -- never a second activation of the VLAN.
  if [ "$parent" != - ] && [ -n "$adopt" ]; then keep="$adopt"; fi
  [ -n "$keep" ] || keep="${adopt:-$first}"
  while read -r u named on act; do
    if [ -n "$u" ] && [ "$u" != "$keep" ]; then extra+=("$u"); fi
  done <<<"$scan"
  while IFS=$'\t' read -r u named; do
    kv+=("$u" "$named")
    case "$u" in connection.id|connection.interface-name|connection.autoconnect) ;; *) addkv+=("$u" "$named") ;; esac
  done < <(strih_vban_leg_nm_props "$ifc" "$addr" "$parent" "$id")
  if [ -z "$keep" ]; then
    # Created DOWN and already static: a fresh profile never has a DHCP moment, and the up below is its
    # ONE activation (an autoconnect racing it would activate a VLAN twice).
    if [ "$parent" = - ]; then
      nmcli connection add type ethernet con-name "$STRIH_VBAN_LEG_PROFILE" ifname "$ifc" "${addkv[@]}" \
        connection.autoconnect no </dev/null >/dev/null \
        || { echo "strih-vban-leg: nmcli could not create the profile ${STRIH_VBAN_LEG_PROFILE} on ${ifc}" >&2; return 1; }
    else
      _strih_vban_leg_vlan_warning "$parent" "$id"
      nmcli connection add type vlan con-name "$STRIH_VBAN_LEG_PROFILE" ifname "$ifc" "${addkv[@]}" \
        connection.autoconnect no </dev/null >/dev/null \
        || { echo "strih-vban-leg: nmcli could not create the VLAN ${id} profile on ${parent}" >&2; return 1; }
    fi
    keep="$(_strih_vban_leg_nm_scan "$ifc" "$mac" "$parent" "$id" | while read -r u named on act; do [ "$named" = 1 ] && echo "$u"; done | head -n 1 || true)"
    [ -n "$keep" ] || { echo "strih-vban-leg: the new profile ${STRIH_VBAN_LEG_PROFILE} is not listed after nmcli add" >&2; return 1; }
    echo "  NM profile ${STRIH_VBAN_LEG_PROFILE} created for ${ifc}"
    # a VLAN's device does not exist before this activation: the profile names it, so no ifname
    if [ "$parent" = - ]; then upargs=(ifname "$ifc"); else upargs=(); fi
    cc0="$([ "$parent" = - ] || _strih_vban_leg_carrier_changes "$parent")"
    if nmcli --wait 30 connection up "$keep" "${upargs[@]}" </dev/null >/dev/null; then
      echo "  ${ifc}: ${STRIH_VBAN_LEG_PROFILE} brought up"
    elif [ "$parent" != - ]; then
      # never armed: NetworkManager would activate it later, in the middle of the next steps
      echo "strih-vban-leg: the new VLAN profile ${keep} did not come up on ${ifc} -- left with autoconnect off (never armed: NetworkManager would activate it later, resetting the atlantic rig link ~8 s while nobody waits): nmcli connection up ${keep} in a maintenance window" >&2
      return 1
    else
      echo "  WARN: ${ifc} did not come up with ${STRIH_VBAN_LEG_PROFILE} (is the leg cabled to the audio VLAN?) -- it comes up with the link (autoconnect); verify-strih item 9 grades it"
    fi
    if [ "$parent" != - ]; then _strih_vban_leg_rig_link_wait "$parent" "$cc0"; fi
    nmcli connection modify "$keep" connection.autoconnect yes </dev/null \
      || { echo "strih-vban-leg: nmcli could not set connection.autoconnect yes on ${keep}" >&2; return 1; }
    [ -n "$(ip -4 -o addr show dev "$ifc" 2>/dev/null || true)" ] || return 0
  else
    act="$(_strih_vban_leg_active_on "$ifc")" || return 1
    if [ "$parent" != - ] && [ "$act" != "$keep" ]; then
      # arming it (autoconnect yes) or any up would activate the VLAN NOW: the atlantic link reset
      echo "strih-vban-leg: the VLAN profile ${keep} exists but ${ifc} is not active on it -- NOT touched (an activation can reset the atlantic rig link ~8 s): nmcli connection up ${keep} in a maintenance window" >&2
      return 1
    fi
    diff="$(_strih_vban_leg_nm_diff "$keep" "$ifc" "$addr" "$parent" "$id")"
    if [ "$parent" != - ] && grep -E '^(connection\.interface-name|vlan\.parent|vlan\.id):' <<<"$diff" >/dev/null; then
      echo "strih-vban-leg: the VLAN profile ${keep} has another identity ($(grep -E '^(connection\.interface-name|vlan\.parent|vlan\.id):' <<<"$diff" | tr '\n' ';')) -- NOT changed: re-creating the VLAN resets the atlantic rig link (~8 s); do it by hand in a maintenance window" >&2
      return 1
    fi
    if [ -n "$diff" ]; then
      nmcli connection modify "$keep" "${kv[@]}" </dev/null \
        || { echo "strih-vban-leg: nmcli could not set the profile ${keep} for ${ifc}" >&2; return 1; }
      echo "  NM profile ${keep} -> ${STRIH_VBAN_LEG_PROFILE} for ${ifc} (was: $(tr '\n' ';' <<<"$diff"))"
      # connection.id / connection.autoconnect touch no device; everything else is applied
      if grep -v -E '^connection\.(id|autoconnect):' <<<"$diff" >/dev/null; then reapply=1; fi
    else
      echo "  NM profile ${STRIH_VBAN_LEG_PROFILE} (${keep}) for ${ifc} unchanged"
    fi
  fi
  for u in "${extra[@]}"; do
    if [ "$parent" != - ] && [ "$(nmcli -g UUID,DEVICE connection show --active </dev/null 2>/dev/null | grep -c -F -- "${u}:" || true)" != 0 ]; then
      echo "  WARN: a second ACTIVE VLAN profile ${u} -- left alone (taking it down would touch the rig link)"; continue
    fi
    nmcli connection delete "$u" </dev/null >/dev/null \
      || { echo "strih-vban-leg: nmcli could not delete the second profile ${u} on ${ifc}" >&2; return 1; }
    echo "  NM profile ${u} on ${ifc} deleted (one profile on the leg)"
  done
  act="$(_strih_vban_leg_active_on "$ifc")" || return 1
  if [ "$act" = "$keep" ] && [ "$reapply" = 0 ]; then
    :
  elif [ "$act" = "$keep" ] && nmcli device reapply "$ifc" </dev/null >/dev/null 2>&1; then
    echo "  ${ifc}: the profile reapplied in place (no link bounce)"
  elif [ "$parent" != - ]; then
    if [ "$act" = "$keep" ]; then
      echo "strih-vban-leg: NetworkManager refused to reapply ${STRIH_VBAN_LEG_PROFILE} on ${ifc} -- NOT re-activated (a re-activated VLAN resets the atlantic rig link ~8 s); the profile is saved, apply it by hand in a maintenance window" >&2
    else
      echo "strih-vban-leg: the VLAN profile ${keep} exists but ${ifc} is not active on it -- NOT activated here (a VLAN activation can reset the atlantic rig link ~8 s): nmcli connection up ${keep} in a maintenance window" >&2
    fi
    return 1
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

# strih_vban_leg_install -> setup-strih step 13 (every role). rc 0 (also a SKIP without a leg), rc 1 + stderr
# naming what failed. Order: the filter, the ARP sysctls, then the profile (the leg never carries its
# address unfiltered).
strih_vban_leg_install() {
  local leg addr sender port res ifc parent id mac=-
  if [ "$(strih_vban_leg_kind)" = none ]; then
    echo "  SKIP: no VBAN-only leg declared (STRIH_PROGRAM_VBAN_LEG=none) -- no audio-VLAN leg on this box"
    return 0
  fi
  leg="$(strih_lx_program_vban_leg)"; addr="$(strih_lx_program_vban_leg_addr)"
  sender="$(strih_lx_program_vban_sender)"; port="$(strih_lx_program_vban_port)"
  [ "${leg%%:*}" != mac ] || mac="${leg#mac:}"
  # an unplugged leg NIC never aborts setup (it runs inside a genlock deploy after OBS stopped): a WARN
  # naming the MAC, nothing changed; verify item 9 FAILs it.
  if [ "$mac" != - ]; then
    local irc=0
    strih_vban_leg_iface "$(_strih_vban_leg_sysfs)" "$mac" >/dev/null 2>&1 || irc=$?
    if [ "$irc" = 1 ]; then
      echo "  WARN: no network interface has the MAC ${mac} (the VBAN-only leg's NIC) -- plug it in and re-run setup; nothing changed, verify-strih item 9 FAILs it"
      return 0
    fi
  fi
  if ! res="$(strih_vban_leg_resolve 2>/dev/null)"; then
    echo "strih-vban-leg: the leg ${leg}: $(strih_vban_leg_resolve 2>&1 >/dev/null || true) -- nothing changed" >&2
    return 1
  fi
  read -r ifc parent id <<<"${res##*$'\n'}"
  echo "  the VBAN-only leg ${leg}: ${ifc} -> ${addr}/32, $([ "$sender" = none ] && echo 'only ping (no sender declared yet)' || echo "only ${sender}'s VBAN to UDP ${port} + ping")"
  _strih_vban_leg_filter_apply "$ifc" "$sender" "$port" || return 1
  _strih_vban_leg_sysctl_apply "$ifc" || return 1
  _strih_vban_leg_nm_apply "$ifc" "$addr" "$mac" "$parent" "$id" || return 1
  # a VLAN created just now did not exist at the sysctl step above (udev applies the drop-in too)
  [ ! -e "$(_strih_vban_leg_sysfs)/class/net/${ifc}" ] || _strih_vban_leg_sysctl_apply "$ifc" >/dev/null || return 1
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
# Read-only: it never creates, activates or reapplies anything (the vlan kind's link-reset trap).
strih_vban_leg_grade() {
  local leg addr sender port stream ifc parent id mac=- res scan u named on act keep="" others="" diff p d ai aa
  local want l1 l2 acc_rule off_rule a1 a2 o1 o2 wait="${STRIH_VBAN_LEG_WAIT_S:-3}"
  if [ "$(strih_vban_leg_kind)" = none ]; then
    note "(vban-leg) no VBAN-only leg declared (STRIH_PROGRAM_VBAN_LEG=none): no audio-VLAN leg on this box"
    return 0
  fi
  leg="$(strih_lx_program_vban_leg)"; addr="$(strih_lx_program_vban_leg_addr)"
  sender="$(strih_lx_program_vban_sender)"; port="$(strih_lx_program_vban_port)"
  stream="$(strih_lx_program_vban_streams)"
  [ "${leg%%:*}" != mac ] || mac="${leg#mac:}"
  if ! res="$(strih_vban_leg_resolve 2>/dev/null)"; then
    bad "(vban-leg) ${leg}: $(strih_vban_leg_resolve 2>&1 >/dev/null || true) -- the program VBAN cannot arrive"
    return 0
  fi
  read -r ifc parent id <<<"${res##*$'\n'}"
  if [ "$parent" != - ]; then
    if [ ! -e "$(_strih_vban_leg_sysfs)/class/net/${ifc}" ]; then
      bad "(vban-leg) interface: ${ifc} (VLAN ${id} on the rig NIC ${parent}) does not exist -- setup-strih.sh step 13 creates it once: the first VLAN resets the atlantic rig link ~8 s, so run it in a quiet moment"
      return 0
    fi
    ok "(vban-leg) interface: ${ifc} = VLAN ${id} tagged on the rig NIC ${parent}"
  else
    ok "(vban-leg) interface: ${ifc} = the leg NIC (MAC ${mac})"
  fi
  # the NetworkManager profile: ours, as provisioned, the only one on the leg
  if ! scan="$(_strih_vban_leg_nm_scan "$ifc" "$mac" "$parent" "$id")"; then
    bad "(vban-leg) profile: nmcli cannot list the NetworkManager connections"
  else
    while read -r u named on act; do
      [ -n "$u" ] || continue
      if [ "$named" = 1 ] && [ -z "$keep" ]; then keep="$u"; else others="${others}${others:+, }$(_strih_vban_leg_nm_get "$u" connection.id || true) (${u})"; fi
    done <<<"$scan"
    if [ -z "$keep" ]; then
      bad "(vban-leg) profile: no NetworkManager profile ${STRIH_VBAN_LEG_PROFILE} for ${ifc}${others:+ (on it: ${others})} -- re-run setup-strih.sh step 13"
    else
      diff="$(_strih_vban_leg_nm_diff "$keep" "$ifc" "$addr" "$parent" "$id")"
      if [ -n "$diff" ] || [ -n "$others" ]; then
        bad "(vban-leg) profile ${STRIH_VBAN_LEG_PROFILE}: $(tr '\n' ';' <<<"$diff")${others:+ other profiles on ${ifc}: ${others}} -- re-run setup-strih.sh step 13"
      else
        ok "(vban-leg) profile ${STRIH_VBAN_LEG_PROFILE} for ${ifc}: ${addr}/32, no gateway, never-default, the ping-reply route in table ${STRIH_VBAN_LEG_ROUTE_TABLE}"
      fi
    fi
  fi
  # the kernel's view: the /32, no route through the leg but the policy one
  if p="$(_strih_vban_leg_live_problems "$ifc" "$addr")"; then
    bad "(vban-leg) routes: $(tr '\n' ';' <<<"$p") -- only the leg's own traffic may use it (re-run setup-strih.sh step 13)"
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
    ok "(vban-leg) filter: ${STRIH_VBAN_LEG_UNIT} enabled + active, table inet ${STRIH_VBAN_LEG_NFT_TABLE} = $([ "$sender" = none ] && echo 'in only ping' || echo "in only ${sender}'s VBAN to UDP ${port} + ping"), out only echo replies"
  fi
  if [ "$sender" = none ]; then
    note "(vban-leg) vban: no VBAN sender declared yet (STRIH_PROGRAM_VBAN_SENDER=none): the leg passes only ping"
    return 0
  fi
  acc_rule="iifname \"${ifc}\" ip saddr ${sender} udp dport ${port} counter accept"
  off_rule="iifname != \"${ifc}\" iifname != \"lo\" ip saddr ${sender} udp dport ${port} counter"
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
