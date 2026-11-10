#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing caller owns strict mode; the grader must grade every sub-check and never abort mid-way
# win-net-layout.sh (issue 1495) -- a Windows OBS box's declared NETWORK LAYOUT: the read-only gather
# section and a PURE grader, beside the Windows OBS-box baseline (scripts/lib/obs-box-baseline-win.sh,
# which sources this file and puts the section into its ONE gather program).
#
# THE STANDARD: dstack docs/two-networks.md (owner ruling 10.11.2026, ONE standard for every PC with a
# Dante leg, owned by dstack): this item grades its section 6 "Checking a PC" items 1-3 for Windows. Items
# 4-7 (DNS, a TCP connect and an mDNS listen from another host, live flows) are checks from OUTSIDE the PC.
#
# WHY: on 10.11.2026 RESOLUME-SNV's Dante leg moved to a VLAN-70 virtual adapter "Dante70" on a new
# Realtek USB 2.5GbE NIC (dstack#155; the owner asked for a gate row). A swapped NIC / USB port, a
# DHCP Dante address or a second default route would break Dante or the LAN silently -- the same
# class as stream-snv's USB VBAN-leg outage of 8.11.2026 (issue 1495) that nobody was told about.
#
# ONE item, `net_layout`: every declared sub-check is graded and a DRIFT names each failed one.
#   OK      every sub-check holds (or the host declares no layout: "not declared")
#   DRIFT   at least one sub-check fails, all failures named
#   UNKNOWN the section is missing / incomplete (the gather died) -- never OK
# The firewall (dstack's item 4) is NOT graded: the owner's production rule keeps every profile off
# and the decision is open (dstack#155).
#
#   win_net_layout_gather_ps        -> the PowerShell section body (Get-* reads only), `key=value` lines
#   win_net_layout_declared HOST    -> rc 0 iff HOST (the gather's COMPUTERNAME) declares a layout
#   win_net_layout_grade FILE       -> `net_layout <VERDICT> <detail>`; rc 20 DRIFT, 11 UNKNOWN, 0 OK
#
# Gather lines (one per object, `|`-separated fields, CR tolerated):
#   phys=<name>|<description>|<status>|<link speed>|<PnP device id>   (Get-NetAdapter -Physical)
#   virt=<name>|<description>|<status>                                 (the declared Dante adapter)
#   ip=<alias>|<IPv4>|<prefix>|<PrefixOrigin>
#   defroute=<alias>|<next hop>|<route metric>                        (every IPv4 0.0.0.0/0)
#   ifmetric=<alias>|<InterfaceMetric>|<Dhcp>
#   vlan=<alias>|<VlanID>   dnsreg=<alias>|<True|False>   netbios=<alias>|<TcpipNetbiosOptions>
#   binding=<component id>|<True|False>                                (on the Dante adapter)
#   ndi_allowed=<comma list>   (C:\ProgramData\NDI\ndi-config.v1.json adapters.allowed)
# Tests: tests/python/test_win_net_layout_1495.py.

# win_net_layout_declared HOST -> rc 0 iff HOST has a declared layout; sets the WNL_* facts.
win_net_layout_declared() {
    case "${1^^}" in
        RESOLUME-SNV)
            WNL_LAN_ALIAS="Ethernet 4"
            WNL_LAN_DESC="Realtek Gaming USB 2.5GbE Family Controller"
            WNL_LAN_PNP="USB\\VID_0BDA&PID_8156"
            WNL_LAN_SPEED="2.5 Gbps"
            WNL_LAN_IP="10.77.9.201/23"
            WNL_LAN_ORIGIN="Dhcp"
            WNL_LAN_GATEWAY="10.77.8.1"
            WNL_LAN_METRIC="20"
            WNL_DISABLED="Ethernet|Wi-Fi"
            WNL_DANTE_ALIAS="Dante70"
            WNL_DANTE_DESC="Realtek Virtual Adapter"
            WNL_DANTE_VLAN="70"
            WNL_DANTE_IP="10.77.7.107/24"
            WNL_DANTE_METRIC="900"
            WNL_DANTE_UNBOUND="ms_server ms_msclient ms_tcpip6 ms_lltdio ms_rspndr"
            WNL_NDI_ALLOWED="10.77.9.201"
            return 0 ;;
    esac
    return 1
}

# win_net_layout_gather_ps -> the section body for the baseline gather (Write-WbSection 'net_layout').
# Reads only. The Dante alias is the declared one of the gathering host, resolved ON the box from its
# COMPUTERNAME so the one gather program serves every box.
win_net_layout_gather_ps() {
    cat <<'PSNET'
Write-WbSection 'net_layout' {
  $wnlDante = switch ($env:COMPUTERNAME) { 'RESOLUME-SNV' { 'Dante70' } default { '' } }
  # A read that fails prints `err=<what>|<message>`, and the grader reads the layout as UNKNOWN.
  try { foreach ($a in @(Get-NetAdapter -Physical -ErrorAction Stop)) { "phys=$($a.Name)|$($a.InterfaceDescription)|$($a.Status)|$($a.LinkSpeed)|$($a.PnPDeviceID)" } } catch { "err=phys|$($_.Exception.Message)" }
  if ($wnlDante) {
    foreach ($a in @(Get-NetAdapter -Name $wnlDante -ErrorAction SilentlyContinue)) { "virt=$($a.Name)|$($a.InterfaceDescription)|$($a.Status)" }
  }
  try { foreach ($i in @(Get-NetIPAddress -AddressFamily IPv4 -PolicyStore ActiveStore -ErrorAction Stop | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' })) { "ip=$($i.InterfaceAlias)|$($i.IPAddress)|$($i.PrefixLength)|$($i.PrefixOrigin)" } } catch { "err=ip|$($_.Exception.Message)" }
  foreach ($r in @(Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -PolicyStore ActiveStore -ErrorAction SilentlyContinue)) { "defroute=$($r.InterfaceAlias)|$($r.NextHop)|$($r.RouteMetric)" }
  try { foreach ($m in @(Get-NetIPInterface -AddressFamily IPv4 -ErrorAction Stop)) { "ifmetric=$($m.InterfaceAlias)|$($m.InterfaceMetric)|$($m.Dhcp)" } } catch { "err=ifmetric|$($_.Exception.Message)" }
  if ($wnlDante) {
    foreach ($p in @(Get-NetAdapterAdvancedProperty -Name $wnlDante -RegistryKeyword VlanID -ErrorAction SilentlyContinue)) { "vlan=$($p.Name)|$($p.RegistryValue -join ',')" }
    foreach ($d in @(Get-DnsClient -InterfaceAlias $wnlDante -ErrorAction SilentlyContinue)) { "dnsreg=$($d.InterfaceAlias)|$($d.RegisterThisConnectionsAddress)" }
    $wnlDa = Get-NetAdapter -Name $wnlDante -ErrorAction SilentlyContinue
    if ($wnlDa) { foreach ($c in @(Get-CimInstance Win32_NetworkAdapterConfiguration -Filter "InterfaceIndex=$($wnlDa.ifIndex)")) { "netbios=$wnlDante|$($c.TcpipNetbiosOptions)" } }
    foreach ($b in @(Get-NetAdapterBinding -Name $wnlDante -ComponentID ms_server,ms_msclient,ms_tcpip6,ms_lltdio,ms_rspndr -ErrorAction SilentlyContinue)) { "binding=$($b.ComponentID)|$($b.Enabled)" }
  }
  try { $wnlNdi = Get-Content 'C:\ProgramData\NDI\ndi-config.v1.json' -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop; "ndi_allowed=$($wnlNdi.ndi.adapters.allowed -join ',')" } catch { "ndi_allowed_unread=$($_.Exception.Message)" }
}
PSNET
}

# _wnl_host FILE -> the gathering host's name from the BEGIN line (empty when absent).
_wnl_host() {
    [ -r "$1" ] || return 0
    sed -n 's/\r$//; s/^==WINBASELINE-BEGIN== v[0-9]* host=\(.*\)$/\1/p' "$1" | head -n 1
}

# _wnl_grade_body BODY -> the failed sub-checks, one per line (none = every sub-check holds).
# Pure over BODY and the WNL_* facts.
_wnl_grade_body() {
    local body="$1" line key val
    local -a phys=() virt=() ips=() routes=() metrics=() vlan=() dnsreg=() netbios=() bindings=()
    local ndi="" ndi_seen=0
    while IFS= read -r line; do
        line="${line%$'\r'}"
        key="${line%%=*}"; val="${line#*=}"
        [ "$key" = "$line" ] && continue
        case "$key" in
            phys) phys+=("$val") ;;
            virt) virt+=("$val") ;;
            ip) ips+=("$val") ;;
            defroute) routes+=("$val") ;;
            ifmetric) metrics+=("$val") ;;
            vlan) vlan+=("$val") ;;
            dnsreg) dnsreg+=("$val") ;;
            netbios) netbios+=("$val") ;;
            binding) bindings+=("$val") ;;
            ndi_allowed) ndi="$val"; ndi_seen=1 ;;
        esac
    done <<<"$body"

    local p name desc status speed pnp up=0 lan_ok=0
    for p in "${phys[@]}"; do
        IFS='|' read -r name desc status speed pnp <<<"$p"
        if [ "$status" = "Up" ]; then
            up=$((up + 1))
            if [ "$name" = "$WNL_LAN_ALIAS" ]; then
                lan_ok=1
                [ "$desc" = "$WNL_LAN_DESC" ] || echo "lan_nic: '$WNL_LAN_ALIAS' is '$desc' (want '$WNL_LAN_DESC')"
                case "${pnp^^}" in "${WNL_LAN_PNP^^}"*) ;; *) echo "lan_nic_usb: '$WNL_LAN_ALIAS' device $pnp (want $WNL_LAN_PNP)" ;; esac
                [ "$speed" = "$WNL_LAN_SPEED" ] || echo "lan_speed: '$WNL_LAN_ALIAS' links at $speed (want $WNL_LAN_SPEED)"
            else
                echo "extra_nic_up: '$name' ($desc) is Up (want only '$WNL_LAN_ALIAS')"
            fi
        fi
    done
    [ "$lan_ok" = 1 ] || echo "lan_nic_up: '$WNL_LAN_ALIAS' is not an Up physical adapter ($up physical Up)"
    local d found
    local IFS_SAVE="$IFS"; IFS='|'
    for d in $WNL_DISABLED; do
        IFS="$IFS_SAVE"
        found=""
        for p in "${phys[@]}"; do
            IFS='|' read -r name desc status speed pnp <<<"$p"
            [ "$name" = "$d" ] && found="$status"
        done
        [ "$found" = "Disabled" ] || echo "disabled_nic: '$d' is ${found:-absent} (want Disabled)"
        IFS='|'
    done
    IFS="$IFS_SAVE"

    local v vname vdesc vstatus vok=0
    for v in "${virt[@]}"; do
        IFS='|' read -r vname vdesc vstatus <<<"$v"
        [ "$vname" = "$WNL_DANTE_ALIAS" ] || continue
        vok=1
        [ "$vdesc" = "$WNL_DANTE_DESC" ] || echo "dante_adapter: '$WNL_DANTE_ALIAS' is '$vdesc' (want '$WNL_DANTE_DESC')"
        [ "$vstatus" = "Up" ] || echo "dante_up: '$WNL_DANTE_ALIAS' is $vstatus (want Up)"
    done
    [ "$vok" = 1 ] || echo "dante_adapter: '$WNL_DANTE_ALIAS' absent"

    # _wnl_ip ALIAS WANT_CIDR WANT_ORIGIN LABEL -> exactly one matching IPv4 on ALIAS
    _wnl_ip() {
        local alias="$1" want="$2" origin="$3" label="$4" i a addr pre org got=""
        for i in "${ips[@]}"; do
            IFS='|' read -r a addr pre org <<<"$i"
            [ "$a" = "$alias" ] || continue
            got="${got:+$got,}$addr/$pre($org)"
        done
        [ "$got" = "$want($origin)" ] || echo "$label: '$alias' has ${got:-no IPv4} (want $want $origin)"
    }
    _wnl_ip "$WNL_LAN_ALIAS" "$WNL_LAN_IP" "$WNL_LAN_ORIGIN" "lan_ip"
    _wnl_ip "$WNL_DANTE_ALIAS" "$WNL_DANTE_IP" "Manual" "dante_ip"

    local r ra rhop rgot=""
    for r in "${routes[@]}"; do
        IFS="|" read -r ra rhop _ <<<"$r"
        rgot="${rgot:+$rgot,}$ra via $rhop"
    done
    [ "$rgot" = "$WNL_LAN_ALIAS via $WNL_LAN_GATEWAY" ] || echo "default_route: ${rgot:-none} (want only $WNL_LAN_ALIAS via $WNL_LAN_GATEWAY)"

    # _wnl_metric ALIAS WANT LABEL
    _wnl_metric() {
        local m ma mval got=""
        for m in "${metrics[@]}"; do
            IFS="|" read -r ma mval _ <<<"$m"
            [ "$ma" = "$1" ] && got="$mval"
        done
        [ "$got" = "$2" ] || echo "$3: '$1' interface metric ${got:-unread} (want $2)"
    }
    _wnl_metric "$WNL_LAN_ALIAS" "$WNL_LAN_METRIC" "lan_metric"
    _wnl_metric "$WNL_DANTE_ALIAS" "$WNL_DANTE_METRIC" "dante_metric"

    local x xa xv got
    got=""; for x in "${vlan[@]}"; do IFS='|' read -r xa xv <<<"$x"; [ "$xa" = "$WNL_DANTE_ALIAS" ] && got="$xv"; done
    [ "$got" = "$WNL_DANTE_VLAN" ] || echo "dante_vlan: VlanID ${got:-unread} (want $WNL_DANTE_VLAN)"
    got=""; for x in "${dnsreg[@]}"; do IFS='|' read -r xa xv <<<"$x"; [ "$xa" = "$WNL_DANTE_ALIAS" ] && got="$xv"; done
    [ "$got" = "False" ] || echo "dante_dns_register: ${got:-unread} (want False)"
    got=""; for x in "${netbios[@]}"; do IFS='|' read -r xa xv <<<"$x"; [ "$xa" = "$WNL_DANTE_ALIAS" ] && got="$xv"; done
    [ "$got" = "2" ] || echo "dante_netbios: TcpipNetbiosOptions ${got:-unread} (want 2 = off)"
    local comp
    for comp in $WNL_DANTE_UNBOUND; do
        got=""; for x in "${bindings[@]}"; do IFS='|' read -r xa xv <<<"$x"; [ "$xa" = "$comp" ] && got="$xv"; done
        [ "$got" = "False" ] || echo "dante_binding: $comp ${got:-unread} on '$WNL_DANTE_ALIAS' (want unbound)"
    done

    if [ "$ndi_seen" = 0 ]; then
        echo "ndi_allowed: ndi-config.v1.json adapters.allowed unread"
    elif [ "$ndi" != "$WNL_NDI_ALLOWED" ]; then
        echo "ndi_allowed: '${ndi}' (want exactly $WNL_NDI_ALLOWED)"
    fi
}

# win_net_layout_grade FILE -> `net_layout <VERDICT> <detail>`. Pure over FILE (the baseline gather
# output). rc 20 = DRIFT, 11 = UNKNOWN, 0 = OK.
win_net_layout_grade() {
    local file="${1:-}" host body fails
    host="$(_wnl_host "$file")"
    if [ -z "$host" ]; then
        echo "net_layout UNKNOWN gather unread (no BEGIN line naming the host)"
        return 11
    fi
    if ! win_net_layout_declared "$host"; then
        echo "net_layout OK not declared for host $host"
        return 0
    fi
    if ! body="$(_win_baseline_section "$file" net_layout)"; then
        echo "net_layout UNKNOWN $host layout unread (no complete net_layout section)"
        return 11
    fi
    if grep -q '^err=' <<<"${body//$'\r'/}"; then
        echo "net_layout UNKNOWN $host layout read failed: $(grep '^err=' <<<"${body//$'\r'/}" | head -n 3 | paste -sd ';' -)"
        return 11
    fi
    fails="$(_wnl_grade_body "$body")"
    if [ -z "$fails" ]; then
        echo "net_layout OK $host: one NIC '$WNL_LAN_ALIAS' $WNL_LAN_SPEED $WNL_LAN_IP via $WNL_LAN_GATEWAY, '$WNL_DANTE_ALIAS' VLAN $WNL_DANTE_VLAN $WNL_DANTE_IP, NDI $WNL_NDI_ALLOWED"
        return 0
    fi
    echo "net_layout DRIFT $host: $(paste -sd ';' <<<"$fails" | sed 's/;/; /g')"
    return 20
}
