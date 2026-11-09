#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-vban-leg-move.sh -- issue 1495, the follow-ups of a leg move (stream-snv's VBAN/Dante leg
# went from the USB-C NIC to VLAN 70 on its rig NIC, 9.11.2026). Sourced by strih-vban-leg.sh.
#   1. A retired NIC leg is LINK-DOWN, not only without an IP. Live 9.11.2026: the old NIC, parked under a
#      profile with no address, kept its link UP on the switch (VLAN 70 untagged), and mbc kept the old MAC for
#      10.77.7.204 and sent the Dante stream there for ~65 min. Retirement = `nmcli device disconnect` (blocks
#      its autoconnect), the profile deleted, a NetworkManager drop-in `[device-vban-dante70-retired]`
#      `match-device=interface-name:<nic>;mac:<mac>` `managed=0` (NetworkManager.conf(5), "managed": an
#      unmanaged device is never activated, not at an NM restart nor at boot, where the kernel brings the
#      interface up DOWN), `nmcli general reload conf` + `nmcli device set <nic> managed no` for the running
#      daemon, then `ip link set dev <nic> down`. With no NM device there is no automatic DHCP profile either.
#      Never the rig NIC (by name and by MAC). verify's `(vban-leg) leftover` FAILs a retired NIC whose link is
#      admin UP (sysfs flags, IFF_UP) and a 9.11 parking profile left. A `mac:` leg back on such a NIC (the
#      rollback) takes it out of the drop-in first.
#   2. The leg address is announced after a change: iputils `arping -q -U -c 2 -I <leg> <addr>` (unsolicited
#      ARP requests, source = target = the leg address), after the changed leg read back and once after a
#      retirement. The leg runs ipv4.dad-timeout 0 (the handover), so NetworkManager announces nothing, and the
#      kernel's arp_notify fires before NM adds the address. Never the rig NIC or STRIH_IP. A failure is a WARN.
#   3. A leg profile change (`device reapply`, an up, the transition) briefly removes the leg address, and a
#      RUNNING dstack device bound to it stalls (the hub's `strih-snv-intercom`, 9.11.2026 19:04: ENODEV on
#      every send, subscriptions pending until a restart). The leg lib records the change in
#      STRIH_VBAN_LEG_CHANGED (the caller's own shell variable); setup-strih step 13 then runs
#      strih_vban_leg_after_change: a running intercom-hub is try-restarted (never a start), a running OBS on a
#      Dante-program box is named loudly (the genlock deploy stops OBS before setup).
# Seams: STRIH_VBAN_LEG_NM_CONF_FILE (the drop-in), the leg lib's STRIH_VBAN_LEG_SYSFS; nmcli / ip / arping /
# systemctl / pgrep from PATH.

STRIH_VBAN_LEG_RETIRED_SECTION=device-vban-dante70-retired

_strih_vban_leg_nm_conf_file() {
  printf '%s' "${STRIH_VBAN_LEG_NM_CONF_FILE:-/etc/NetworkManager/conf.d/90-vban-dante70-retired.conf}"
}

# _strih_vban_leg_mac_of IF -> IF's MAC in lower case ('' when unreadable).
_strih_vban_leg_mac_of() {
  { tr '[:upper:]' '[:lower:]' <"$(_strih_vban_leg_sysfs)/class/net/${1}/address"; } 2>/dev/null || true
}

# _strih_vban_leg_link_state IF -> up | down (the ADMIN state: IFF_UP of the sysfs flags) | absent | unreadable.
_strih_vban_leg_link_state() {
  local d v
  d="$(_strih_vban_leg_sysfs)/class/net/${1}"
  [ -e "$d" ] || { echo absent; return 0; }
  v="$({ cat "${d}/flags"; } 2>/dev/null || true)"
  if [[ ! "$v" =~ ^0x[0123456789abcdefABCDEF]+$ ]]; then echo unreadable; return 0; fi
  if (( v & 1 )); then echo up; else echo down; fi
}

# strih_vban_leg_retired_conf_text MATCHES -> the NetworkManager drop-in (MATCHES = `;`-joined device specs).
strih_vban_leg_retired_conf_text() {
  printf '%s\n' "# issue 1495: retired VBAN/Dante leg NICs -- never managed, never activated: their link stays DOWN." \
    "# Written by setup-strih step 13 (scripts/lib/strih-vban-leg-move.sh). Remove it to use such a NIC again." \
    "[${STRIH_VBAN_LEG_RETIRED_SECTION}]" "match-device=${1:?device specs required}" "managed=0"
}

# _strih_vban_leg_retired_matches -> the drop-in's device specs, one per line (none without the file).
_strih_vban_leg_retired_matches() {
  local f l
  f="$(_strih_vban_leg_nm_conf_file)"
  [ -f "$f" ] || return 0
  l="$(sed -n 's/^match-device=//p' "$f" 2>/dev/null | head -n 1 || true)"
  tr ';,' '\n\n' <<<"$l" | sed '/^$/d'
}

# _strih_vban_leg_retired_ifaces PARENT -> one line per PRESENT interface the drop-in names or a 9.11 parking
# profile is bound to, never PARENT (the rig NIC).
_strih_vban_leg_retired_ifaces() {
  local parent="$1" m ifc out="" sysfs
  sysfs="$(_strih_vban_leg_sysfs)"
  while read -r m; do
    ifc=""
    case "$m" in
      mac:*) ifc="$(strih_vban_leg_iface "$sysfs" "$(tr '[:upper:]' '[:lower:]' <<<"${m#mac:}")" 2>/dev/null)" || ifc="" ;;
      interface-name:*) ifc="${m#interface-name:}" ;;
    esac
    if [ -n "$ifc" ] && [ "$ifc" != "$parent" ] && [ -e "${sysfs}/class/net/${ifc}" ] \
       && [[ $'\n'"$out" != *$'\n'"${ifc}"$'\n'* ]]; then out="${out}${ifc}"$'\n'; fi
  done <<<"$(_strih_vban_leg_retired_matches)"
  ifc="$(_strih_vban_leg_nm_get "$STRIH_VBAN_LEG_RETIRED_PROFILE" connection.interface-name 2>/dev/null || true)"
  if [ -n "$ifc" ] && [ "$ifc" != "$parent" ] && [ -e "${sysfs}/class/net/${ifc}" ] \
     && [[ $'\n'"$out" != *$'\n'"${ifc}"$'\n'* ]]; then out="${out}${ifc}"$'\n'; fi
  printf '%s' "$out"
}

# _strih_vban_leg_retire_down NIC PARENT -> the drop-in names NIC (its interface name + MAC, added to the
# entries already there), NetworkManager lets go of it, its link goes DOWN. rc 0 (also for NIC `-`); rc 1 + a
# named line for the rig NIC (by name or by MAC: a VLAN shares its parent's MAC) or a failed write / link down.
_strih_vban_leg_retire_down() {
  local nic="${1-}" parent="${2-}" mac rmac f w m merged="" e st
  [ -n "$nic" ] && [ "$nic" != - ] || return 0
  if [ "$nic" = "$parent" ]; then
    echo "strih-vban-leg: refusing to unmanage the rig NIC ${parent} (the ssh link)" >&2; return 1
  fi
  mac="$(_strih_vban_leg_mac_of "$nic")"; rmac="$(_strih_vban_leg_mac_of "$parent")"
  if [ -n "$mac" ] && [ "$mac" = "$rmac" ]; then
    echo "strih-vban-leg: ${nic} carries the rig NIC ${parent}'s MAC ${mac} -- NOT unmanaged (the rig link)" >&2; return 1
  fi
  while read -r m; do
    if [ -n "$m" ]; then merged="${merged}${merged:+;}${m}"; fi
  done <<<"$(_strih_vban_leg_retired_matches)"
  for e in "interface-name:${nic}" ${mac:+"mac:${mac}"}; do
    [[ ";${merged};" == *";${e};"* ]] || merged="${merged}${merged:+;}${e}"
  done
  f="$(_strih_vban_leg_nm_conf_file)"
  w="$(_strih_vban_leg_put "$f" 0644 "$(strih_vban_leg_retired_conf_text "$merged")")" \
    || { echo "strih-vban-leg: could not write ${f} (the retired NIC ${nic} stays managed)" >&2; return 1; }
  [ "$w" = unchanged ] || nmcli general reload conf </dev/null >/dev/null 2>&1 \
    || echo "  WARN: nmcli general reload conf failed -- ${f} takes effect at NetworkManager's next start"
  st="$(_strih_vban_leg_link_state "$nic")"
  if [ "$st" = absent ]; then
    echo "  ${f}: ${w}; ${nic} is not plugged in -- it stays unmanaged and down when it comes back"
    return 0
  fi
  if [ "$w" != unchanged ] || [ "$st" != down ]; then
    nmcli device set "$nic" managed no </dev/null >/dev/null 2>&1 \
      || echo "  WARN: nmcli device set ${nic} managed no failed -- ${f} unmanages it at NetworkManager's next reload"
  fi
  if [ "$st" != down ]; then
    ip link set dev "$nic" down \
      || { echo "strih-vban-leg: could not take the retired NIC ${nic}'s link down -- the switch keeps delivering to its old MAC" >&2; return 1; }
  fi
  echo "  the retired NIC ${nic}: unmanaged (${f}: ${w}), link down -- the switch drops its old MAC (issue 1495)"
}

# _strih_vban_leg_retired_upkeep PARENT IF ADDR -> every 9.11 parking profile `vban-dante70-retired` deleted (its
# device disconnected first) and its NIC retired down, and every retired NIC whose link is up again taken down;
# when anything was retired, the leg IF's ADDR is announced once (a neighbour may still hold the old MAC). NOT
# marked as a leg change: neither action touches the leg address or its rule, so no running Dante device stalls
# and the hub is not restarted (review round 2). rc 1 + a named line on an nmcli or link failure, after the
# announce for what was already retired.
_strih_vban_leg_retired_upkeep() {
  local parent="$1" ifc="${2-}" addr="${3-}" rows u dev id ifn r did="" rd=0
  rows="$(nmcli -g UUID,TYPE,DEVICE connection show </dev/null 2>/dev/null)" \
    || { echo "strih-vban-leg: nmcli cannot list the NetworkManager connections (the retired NIC)" >&2; return 1; }
  while IFS=: read -r u _ dev; do
    [ -n "$u" ] || continue
    id="$(_strih_vban_leg_nm_get "$u" connection.id || true)"
    [ "$id" = "$STRIH_VBAN_LEG_RETIRED_PROFILE" ] || continue
    ifn="${dev:-$(_strih_vban_leg_nm_get "$u" connection.interface-name || true)}"
    if [ "$ifn" = "$parent" ]; then
      echo "  WARN: a ${STRIH_VBAN_LEG_RETIRED_PROFILE} profile (${u}) is bound to the rig NIC ${parent} -- NOT touched"; continue
    fi
    if [ -n "$dev" ]; then nmcli device disconnect "$dev" </dev/null >/dev/null 2>&1 || true; fi
    nmcli connection delete "$u" </dev/null >/dev/null \
      || { echo "strih-vban-leg: nmcli could not delete the parking profile ${u} on ${ifn:-?}" >&2; return 1; }
    echo "  the parking profile ${STRIH_VBAN_LEG_RETIRED_PROFILE} (${u}) on ${ifn:-?} deleted: an active profile kept its link UP"
    did="${did}${did:+, }${ifn:-?}"
    _strih_vban_leg_retire_down "${ifn:--}" "$parent" || rd=1
  done <<<"$rows"
  while read -r r; do
    [ -n "$r" ] || continue
    if [ "$(_strih_vban_leg_link_state "$r")" != down ]; then
      _strih_vban_leg_retire_down "$r" "$parent" || rd=1
      did="${did}${did:+, }${r}"
    fi
  done <<<"$(_strih_vban_leg_retired_ifaces "$parent")"
  if [ -n "$did" ]; then _strih_vban_leg_announce "$ifc" "$addr" "$parent"; fi
  return "$rd"
}

# _strih_vban_leg_unretire NIC -> a `mac:` leg back on a NIC an earlier VLAN move retired (the rollback): its
# entries (interface name, MAC) leave the drop-in (the file goes when none is left), NetworkManager is told
# (`general reload conf`, `device set NIC managed yes`), so the leg's activation is not refused as strictly
# unmanaged. Nothing to do = no output. rc 1 + a named line on a write failure.
_strih_vban_leg_unretire() {
  local nic="${1-}" mac f m lm keep="" hit=""
  f="$(_strih_vban_leg_nm_conf_file)"
  [ -n "$nic" ] && [ -f "$f" ] || return 0
  mac="$(_strih_vban_leg_mac_of "$nic")"
  while read -r m; do
    [ -n "$m" ] || continue
    lm="$(tr '[:upper:]' '[:lower:]' <<<"$m")"
    if [ "$m" = "interface-name:${nic}" ] || { [ -n "$mac" ] && [ "$lm" = "mac:${mac}" ]; }; then
      hit=1
    else
      keep="${keep}${keep:+;}${m}"
    fi
  done <<<"$(_strih_vban_leg_retired_matches)"
  [ -n "$hit" ] || return 0
  if [ -n "$keep" ]; then
    _strih_vban_leg_put "$f" 0644 "$(strih_vban_leg_retired_conf_text "$keep")" >/dev/null \
      || { echo "strih-vban-leg: could not rewrite ${f} without ${nic} (the leg NIC stays unmanaged)" >&2; return 1; }
  else
    rm -f -- "$f" || { echo "strih-vban-leg: could not remove ${f} (the leg NIC ${nic} stays unmanaged)" >&2; return 1; }
  fi
  nmcli general reload conf </dev/null >/dev/null 2>&1 || true
  nmcli device set "$nic" managed yes </dev/null >/dev/null 2>&1 \
    || echo "  WARN: nmcli device set ${nic} managed yes failed -- NetworkManager takes it back at its next start"
  echo "  ${nic}: taken out of ${f} (a VLAN move had retired it; the leg is a mac: leg on it again, issue 1495)"
}

# _strih_vban_leg_mark REASON -> records a change of the leg address on its interface this run (appended to
# STRIH_VBAN_LEG_CHANGED, the caller's shell variable, read by strih_vban_leg_after_change).
_strih_vban_leg_mark() { STRIH_VBAN_LEG_CHANGED="${STRIH_VBAN_LEG_CHANGED:-}${STRIH_VBAN_LEG_CHANGED:+, }${1}"; }

# _strih_vban_leg_announce IF ADDR PARENT -> two unsolicited ARP requests for ADDR on IF (iputils arping), so a
# neighbour holding an old entry for ADDR updates it at once. Never IF = PARENT (the rig NIC) or ADDR = STRIH_IP.
# arping missing = iputils-arping installed; a non-iputils arping or a failure = a WARN. Always rc 0.
_strih_vban_leg_announce() {
  local ifc="${1-}" addr="${2-}" parent="${3-}" rig v rc=0
  rig="$(strih_lx_ip 2>/dev/null || true)"
  if [ -z "$ifc" ] || [ -z "$addr" ] || [ "$ifc" = "$parent" ] || [ "$addr" = "$rig" ]; then
    echo "  NOTE: no gratuitous ARP for ${addr:-?} on ${ifc:-?}: the rig NIC and the rig address are never announced by the leg"
    return 0
  fi
  if ! command -v arping >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends iputils-arping >/dev/null 2>&1 || true
    if ! command -v arping >/dev/null 2>&1; then
      echo "  WARN: no gratuitous ARP for ${addr} on ${ifc}: iputils-arping could not be installed -- a neighbour keeps its old ARP entry until it expires"
      return 0
    fi
  fi
  v="$(arping -V 2>&1 || true)"
  case "$v" in
    *iputils*) ;;
    *) echo "  WARN: no gratuitous ARP for ${addr} on ${ifc}: arping is not the iputils one ('${v%%$'\n'*}') -- install iputils-arping; a neighbour keeps its old ARP entry until it expires"
       return 0 ;;
  esac
  timeout 10 arping -q -U -c 2 -I "$ifc" "$addr" </dev/null >/dev/null 2>&1 || rc=$?
  if [ "$rc" = 0 ]; then
    echo "  ${ifc}: ${addr} announced (gratuitous ARP x2): every neighbour on the leg updates its entry now"
  else
    echo "  WARN: the gratuitous ARP for ${addr} on ${ifc} failed (arping rc ${rc}) -- a neighbour keeps its old ARP entry until it expires"
  fi
  return 0
}

# strih_vban_leg_after_change [USER] -> setup-strih step 13, right after strih_vban_leg_install (also when it
# failed after touching the leg): when the leg changed this run, a RUNNING intercom-hub is try-restarted (its
# dstack device stalls on a reapply), never a stopped one started, then the crash-loop probe and the record of
# the running install (strih-intercom-hub-unit.sh); a running OBS on a Dante-program box is named in a WARNING
# (never restarted here). rc 1 + a named line when the restart fails or the hub crash-loops after it.
strih_vban_leg_after_change() {
  local user="${1-}" why loop dev
  why="${STRIH_VBAN_LEG_CHANGED:-}"
  if [ -z "$why" ]; then
    echo "  the leg did not change this run -- no running Dante device to restart"
    return 0
  fi
  if systemctl is-active -q intercom-hub 2>/dev/null; then
    systemctl try-restart intercom-hub \
      || { echo "strih-vban-leg: systemctl try-restart intercom-hub failed after the leg changed (${why})" >&2; return 1; }
    if declare -F strih_intercom_hub_crash_loop >/dev/null && loop="$(strih_intercom_hub_crash_loop)"; then
      echo "strih-vban-leg: intercom-hub.service ${loop} after its restart on the leg change (${why}) -- journalctl -u intercom-hub" >&2
      return 1
    fi
    if declare -F strih_intercom_hub_record_loaded >/dev/null; then strih_intercom_hub_record_loaded; fi
    echo "  intercom-hub.service restarted: the leg changed (${why}); a running dstack device stalls on it (ENODEV, issue 1495)"
  else
    echo "  intercom-hub.service is not running -- not started here (the leg changed: ${why})"
  fi
  if strih_program_dante_declared 2>/dev/null && pgrep -x obs >/dev/null 2>&1; then
    dev="$(strih_dante_input_device 2>/dev/null || echo '?')"
    echo "  WARNING: OBS is running and its Dante device ${dev} sits on the leg that changed (${why}) -- it stalls like the hub: restart OBS now (as ${user:-the desktop user}: systemctl --user restart strih-obs.service); a genlock deploy stops OBS before setup and never needs this"
  fi
  return 0
}
