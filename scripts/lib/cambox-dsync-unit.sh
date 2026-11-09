#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the lazy
# source of scripts/lib/dsync-iface.sh) -- deliberately NOT `set -euo pipefail`: sourcing runs in the
# CALLER's shell (setup-device.sh, verify-device.sh, cambox-ro-units.sh), and strict mode would leak.
#
# scripts/lib/cambox-dsync-unit.sh -- issue 1439 part 2 (ROZHODNUTE 1.11.2026: dsync binds PTP to a NIC
# the box declares, never by chance): the cambox dsync unit, rendered and graded in ONE place.
#
# WHY: setup-device STEP 17 wrote `ExecStart=/usr/local/bin/dsync --ntp-server strih.lan`, so dsync
# joined PTP on the first IPv4 interface it listed. On 20.8.2026 the bkshading USB camera link of cam1
# (CDC-NCM, `enx*`) took the multicast join and the box went PTP-deaf for ~5 h. dsync >= 1.21 honours
# `--interface <NIC>` (waits for it, never falls back) and serves `ptp_interface_configured` /
# `ptp_interface` on /status.
#
# Helpers:
#   cambox_lan_nic SYSROOT               the ONE `enp*` under SYSROOT/class/net with a `device` link (the
#                                        netplan LAN rule, `match: name: "enp*"`: a PCI NIC is enp*, the
#                                        USB camera link enx*, a VLAN has no device link); none or two or
#                                        more = rc 1 + a named stderr line, never a guess
#   cambox_dsync_unit_text [NIC]         the STEP 17 unit; without a NIC byte-identical to the pre-1439
#                                        heredoc, with one ` --interface <NIC>` after `--ntp-server
#                                        strih.lan`. A NIC that is not 1-15 of [A-Za-z0-9._-] or starts
#                                        with `-` emits nothing (rc 1) -- strih_dantesync_unit_text's check
#   cambox_dsync_gather_remote_snippet [SYSROOT] [UNIT]
#                                        read-only on-box text for verify-device (ba): the LAN NIC, the
#                                        unit's ExecStart and dsync's own /status, then END
#   cambox_dsync_verify_verdict BLOCK    pure: ok:<nic> | gather-incomplete | nic-unresolved |
#                                        unit-not-bound:<ExecStart> | the dsync_iface_verdict tokens
#
# Consumers: setup-device STEP 17 (the unit), verify-device (ba) (the grade), and the live apply
# cambox_ro_units_apply_program (scripts/lib/cambox-ro-units.sh), which embeds these via `declare -f`.

if ! command -v dsync_iface_verdict >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/dsync-iface.sh
  . "${BASH_SOURCE[0]%/*}/dsync-iface.sh"
fi

cambox_lan_nic() {
  local sys="${1-}" d n found="" count=0
  for d in "$sys"/class/net/*; do
    n="${d##*/}"
    case "$n" in enp*) ;; *) continue ;; esac
    [ -e "$d/device" ] || [ -L "$d/device" ] || continue
    found="${found:+$found }$n"
    count=$((count + 1))
  done
  if [ "$count" -eq 1 ]; then
    printf '%s\n' "$found"
    return 0
  fi
  if [ "$count" -eq 0 ]; then
    echo "cambox_lan_nic: no enp* interface with a device under ${sys}/class/net -- the LAN NIC is unknown, dsync is not bound to a guess" >&2
  else
    echo "cambox_lan_nic: ${count} enp* interfaces with a device under ${sys}/class/net (${found}) -- the LAN NIC is ambiguous, dsync is not bound to a guess" >&2
  fi
  return 1
}

cambox_dsync_unit_text() {
  local iface=""
  if [ "$#" -ge 1 ]; then
    local nic="${1-}" LC_ALL=C up='ABCDEFGHIJKLMNOPQRSTUVWXYZ' lo='abcdefghijklmnopqrstuvwxyz' dg='0123456789'
    [[ "$nic" =~ ^[-._${dg}${up}${lo}]{1,15}$ ]] || return 1
    case "$nic" in -*) return 1 ;; esac
    iface=" --interface ${nic}"
  fi
  cat <<UNIT
[Unit]
Description=dsync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
# Sync to the venue's strih by NAME (strih.lan: strih-snv in SNV, strih-pp in Poprad), NOT the
# dantesync binary's default public NTP pool -- every node must discipline its clock to the SAME
# reference or the wall-clock genlock in src/ndi.rs silently diverges across cameras (#8). Issue 1412:
# the old literal SNV IP left every Poprad cambox with no NTP at all (cam7 ~150 s off strih-pp). The
# CLI value is resolved on every query, so an unresolvable name is a counted NTP failure, never a
# public-pool fallback, and it recovers once DNS answers.
ExecStart=/usr/local/bin/dsync --ntp-server strih.lan${iface}
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
UNIT
}

cambox_dsync_gather_remote_snippet() {
  local sys="${1:-/sys}" unit="${2:-/etc/systemd/system/dsync.service}"
  declare -f cambox_lan_nic dsync_iface_status_fields dsync_iface_read_status
  printf '_d_nic="$(cambox_lan_nic %q 2>/dev/null)" || _d_nic=""\n' "$sys"
  printf 'echo "NIC=$_d_nic"\n'
  printf 'echo "EXEC=$(sed -n '\''s/^ExecStart=//p'\'' %q 2>/dev/null | tail -n 1)"\n' "$unit"
  printf 'if _d_f="$(dsync_iface_read_status)"; then echo "STATUS=1"; echo "FIELDS=$_d_f"; else echo "STATUS=0"; fi\n'
  printf 'echo END\n'
}

cambox_dsync_verify_verdict() {
  local block="${1-}" line nic="" exec_="" status="" fields="" end=0 rest conf joined v
  while IFS= read -r line; do
    case "$line" in
      NIC=*) nic="${line#NIC=}" ;;
      EXEC=*) exec_="${line#EXEC=}" ;;
      STATUS=*) status="${line#STATUS=}" ;;
      FIELDS=*) fields="${line#FIELDS=}" ;;
      END) end=1 ;;
    esac
  done <<<"$block"
  if [ "$end" != 1 ]; then echo gather-incomplete; return 0; fi
  if [ -z "$nic" ]; then echo nic-unresolved; return 0; fi
  case " ${exec_} " in
    *" --interface ${nic} "*) ;;
    *) echo "unit-not-bound:${exec_:-<no ExecStart>}"; return 0 ;;
  esac
  rest="${fields#*$'\t'}"
  conf="${rest%%$'\t'*}"
  joined="${rest#*$'\t'}"
  [ "$status" = 1 ] || { conf=""; joined=""; }
  v="$(dsync_iface_verdict "${status:-0}" "$conf" "$joined" "$nic")"
  echo "$v"
}
