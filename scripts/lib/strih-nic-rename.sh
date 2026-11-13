#!/usr/bin/env bash
# airuleset:script-ok source-only lib (the rig-NIC rename: a systemd .link file, its verify rows and the
# live switch program; no top-level statements besides constants and the CLI guard at the end) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell. setup-strih.sh / verify-strih.sh set their own. Every function checks its own return
# codes, because a caller runs it in an `||` list, where errexit is off.
#
# scripts/lib/strih-nic-rename.sh -- issue 1502: a stable SHORT name for the rig NIC.
#
# Every rig-NIC consumer derives its names from the rig NIC's kernel name (strih_lx_rig_nic): the
# VLAN leg `<rig nic>.<id>` (the kernel caps a name at 15 characters), dsync's --interface, the
# dante-input TOML. A USB adapter gets the predictable name enx<mac> (15 characters), so its VLAN leg
# `enx00e04c680185.70` cannot exist. The box fact STRIH_NIC_RENAME = `<mac>=<name>` | `none` names
# the rig NIC through the standard systemd mechanism: a .link file udev's net_setup_link builtin
# applies when the device is ADDED (systemd.link(5)).
#
#   * strih_nic_rename_link_text (PURE) -- the .link text. [Match] carries the MAC AND the driver: a
#     VLAN (or a bridge / bond) inherits its parent's MAC, so a MAC-only match would also try to name
#     `<name>.<id>` `<name>` when it is created; a VLAN's driver is never the NIC driver.
#   * strih_nic_rename_apply -- setup-strih step 1c: write the file when it differs (header-marked,
#     temp + rename), remove OUR file when the fact is none, never touch an operator's file, never
#     rename a live interface (the NOTE names the re-plug / rebind that applies it).
#   * strih_nic_rename_grade_rows -- verify-strih item 36's `(nic-rename)` rows.
#   * strih_nic_rename_switch_program -- the detached live switch of a box's two NetworkManager
#     profiles onto the renamed NIC with an automatic rollback (the supervisor runbook in
#     .claude/rules/strih-linux-provisioning.md); CLI: `bash strih-nic-rename.sh --switch-program ...`.
#
# Test seams (tests/python/test_strih_nic_rename_1502.py): STRIH_NIC_RENAME_LINK (the .link path),
# STRIH_NIC_RENAME_SYSROOT (/sys), STRIH_NIC_RENAME_NM_RETIRED (the issue-1495 retired-NIC
# NetworkManager drop-in); `udevadm` is looked up on PATH.

# The first line of every .link file this lib writes: the ownership marker (an operator's file never
# carries it, so it is never replaced or removed).
STRIH_NIC_RENAME_MARKER="# camera-box issue 1502: the rig NIC's stable name (scripts/lib/strih-nic-rename.sh)"

strih_nic_rename_link_path() { printf '%s' "${STRIH_NIC_RENAME_LINK:-/etc/systemd/network/10-camera-box-rig-nic.link}"; }
strih_nic_rename_sysroot() { printf '%s' "${STRIH_NIC_RENAME_SYSROOT:-/sys}"; }
strih_nic_rename_retired_conf() {
  printf '%s' "${STRIH_NIC_RENAME_NM_RETIRED:-/etc/NetworkManager/conf.d/90-vban-dante70-retired.conf}"
}

# strih_nic_rename_mac VALUE / strih_nic_rename_name VALUE -> the two halves of a `<mac>=<name>` fact.
strih_nic_rename_mac() { printf '%s' "${1%%=*}"; }
strih_nic_rename_name() { printf '%s' "${1#*=}"; }

# strih_nic_rename_link_text MAC NAME DRIVER -> the .link file (PURE).
strih_nic_rename_link_text() {
  local mac="${1:?mac required}" name="${2:?name required}" drv="${3:?driver required}"
  printf '%s\n' "$STRIH_NIC_RENAME_MARKER" \
    "# Written by setup-strih step 1c from the box fact STRIH_NIC_RENAME. udev applies it when the NIC is" \
    "# ADDED (a re-plug, a boot, a USB rebind); a live interface is never renamed by setup." \
    "# Driver= keeps a VLAN on the NIC (same MAC, driver 802.1Q) from matching." \
    "[Match]" "MACAddress=${mac}" "Driver=${drv}" "" "[Link]" "Name=${name}"
}

# strih_nic_rename_iface_of_mac SYSROOT MAC -> the PHYSICAL interface (one with a device link) whose
# address is MAC, rc 0; nothing + rc 1 when none; the names + rc 2 when several (never guessed).
strih_nic_rename_iface_of_mac() {
  local sysroot="${1-}" mac="${2-}" f d a found=""
  for f in "$sysroot"/class/net/*/address; do
    [ -r "$f" ] || continue
    d="${f%/address}"
    [ -e "${d}/device" ] || continue
    a="$({ tr '[:upper:]' '[:lower:]' <"$f"; } 2>/dev/null || true)"
    if [ "$a" = "$mac" ]; then found="${found}${found:+ }${d##*/}"; fi
  done
  [ -n "$found" ] || return 1
  printf '%s' "$found"
  case "$found" in *" "*) return 2 ;; esac
  return 0
}

# strih_nic_rename_apply_hint SYSROOT IFACE -> the exact command that makes udev ADD the device again
# (so the .link names it): a USB device's unbind + bind, else a synthetic add event.
strih_nic_rename_apply_hint() {
  local sysroot="${1-}" ifc="${2-}" dev usb
  dev="$(readlink -f "${sysroot}/class/net/${ifc}/device" 2>/dev/null || true)"
  usb="$(dirname "${dev:-/}")"
  if [ -n "$dev" ] && [ -f "${usb}/idVendor" ]; then
    usb="${usb##*/}"
    printf "sudo sh -c 'echo %s > /sys/bus/usb/drivers/usb/unbind && echo %s > /sys/bus/usb/drivers/usb/bind' (or re-plug it)" "$usb" "$usb"
  else
    printf 'sudo udevadm trigger --action=add --settle /sys/class/net/%s (with its link down)' "$ifc"
  fi
}

# _strih_nic_rename_ours FILE -> rc 0 when FILE starts with the ownership marker.
_strih_nic_rename_ours() {
  local first=""
  IFS= read -r first <"$1" 2>/dev/null || true
  [ "$first" = "$STRIH_NIC_RENAME_MARKER" ]
}

# _strih_nic_rename_reload -> `udevadm control --reload` (udev re-reads .link files); a failure is a WARN.
_strih_nic_rename_reload() {
  udevadm control --reload </dev/null >/dev/null 2>&1 \
    || echo "  WARN: udevadm control --reload failed -- udev reads the .link file at its next start"
}

# strih_nic_rename_apply VALUE DRIVER -> setup-strih step 1c (see the header). rc 1 + a named stderr line
# when an operator's file holds the path or the write fails; rc 0 otherwise.
strih_nic_rename_apply() {
  local val="${1-}" drv="${2-}" f mac name text tmp ifc rc=0 sysroot
  f="$(strih_nic_rename_link_path)"; sysroot="$(strih_nic_rename_sysroot)"
  if [ "$val" = none ]; then
    if [ ! -e "$f" ]; then
      echo "  no rig-NIC rename declared (STRIH_NIC_RENAME=none)"
    elif _strih_nic_rename_ours "$f"; then
      rm -f -- "$f" || { echo "strih-nic-rename: could not remove ${f}" >&2; return 1; }
      _strih_nic_rename_reload
      echo "  ${f}: removed (STRIH_NIC_RENAME=none); the NIC keeps its current name until it is added again"
    else
      echo "  NOTE: ${f} is not camera-box's (no issue-1502 marker) -- left alone"
    fi
    return 0
  fi
  mac="$(strih_nic_rename_mac "$val")"; name="$(strih_nic_rename_name "$val")"
  if [ -e "$f" ] && ! _strih_nic_rename_ours "$f"; then
    echo "strih-nic-rename: ${f} exists and is not camera-box's (no issue-1502 marker) -- refusing to replace it; move it aside" >&2
    return 1
  fi
  text="$(strih_nic_rename_link_text "$mac" "$name" "$drv")" || return 1
  if [ -f "$f" ] && [ "$(cat "$f" 2>/dev/null)" = "$text" ]; then
    echo "  ${f}: unchanged (${mac} -> ${name})"
  else
    mkdir -p -- "$(dirname "$f")" || { echo "strih-nic-rename: could not create $(dirname "$f")" >&2; return 1; }
    tmp="$(mktemp "$(dirname "$f")/.10-camera-box-rig-nic.XXXXXX")" \
      || { echo "strih-nic-rename: could not create a temp file next to ${f}" >&2; return 1; }
    if ! { printf '%s\n' "$text" >"$tmp" && chmod 0644 "$tmp" && mv -f -- "$tmp" "$f"; }; then
      rm -f -- "$tmp"
      echo "strih-nic-rename: could not write ${f}" >&2; return 1
    fi
    _strih_nic_rename_reload
    echo "  ${f}: written (${mac} -> ${name})"
  fi
  ifc="$(strih_nic_rename_iface_of_mac "$sysroot" "$mac")" || rc=$?
  if [ "$rc" = 1 ]; then
    echo "  NOTE: no interface carries ${mac} now (not plugged in) -- it comes up as ${name} when it is plugged in"
  elif [ "$rc" != 0 ]; then
    echo "  WARN: several interfaces carry ${mac}: ${ifc} -- verify item 36 FAILs until one does"
  elif [ "$ifc" = "$name" ]; then
    echo "  the rig NIC is ${name} (${mac})"
  else
    echo "  NOTE: ${mac} is still ${ifc}: setup never renames a live interface. Move its NetworkManager profiles off it first (the issue-1502 runbook), then: $(strih_nic_rename_apply_hint "$sysroot" "$ifc")"
  fi
  return 0
}

# strih_nic_rename_grade_rows SYSROOT RIG_NIC [VALUE [DRIVER]] -> verify-strih item 36's `(nic-rename)`
# rows: `OK|` / `FAIL|` / `NOTE|` lines. VALUE / DRIVER default to the loaded box facts. RIG_NIC is what
# strih_lx_rig_nic resolved (empty when it could not). Read-only; always rc 0 (the rows are the verdict).
strih_nic_rename_grade_rows() {
  local sysroot="${1:-/sys}" rig="${2-}" val="${3-}" drv="${4-}" f mac name ifc rc=0 conf m lm hit=""
  if [ -z "$val" ]; then val="$(strih_lx_nic_rename 2>/dev/null)" || { printf 'FAIL|(nic-rename) the box fact STRIH_NIC_RENAME is not readable\n'; return 0; }; fi
  if [ -z "$drv" ]; then drv="$(strih_lx_nic_driver 2>/dev/null)" || { printf 'FAIL|(nic-rename) the box fact STRIH_NIC_DRIVER is not readable\n'; return 0; }; fi
  f="$(strih_nic_rename_link_path)"
  if [ "$val" = none ]; then
    if [ -e "$f" ] && _strih_nic_rename_ours "$f"; then
      printf 'FAIL|(nic-rename) %s is left over while STRIH_NIC_RENAME is none -- re-run setup-strih.sh step 1c\n' "$f"
    else
      printf 'NOTE|(nic-rename) no rig-NIC rename declared (STRIH_NIC_RENAME=none)\n'
    fi
    return 0
  fi
  mac="$(strih_nic_rename_mac "$val")"; name="$(strih_nic_rename_name "$val")"
  if [ -f "$f" ] && [ "$(cat "$f" 2>/dev/null)" = "$(strih_nic_rename_link_text "$mac" "$name" "$drv")" ]; then
    printf 'OK|(nic-rename) %s names %s %s\n' "$f" "$mac" "$name"
  else
    printf 'FAIL|(nic-rename) %s is missing or differs from the box fact (%s -> %s) -- re-run setup-strih.sh step 1c\n' "$f" "$mac" "$name"
  fi
  ifc="$(strih_nic_rename_iface_of_mac "$sysroot" "$mac")" || rc=$?
  if [ "$rc" = 0 ] && [ "$ifc" = "$name" ]; then
    printf 'OK|(nic-rename) %s carries %s\n' "$name" "$mac"
  elif [ "$rc" = 1 ]; then
    printf 'FAIL|(nic-rename) no interface carries %s -- the rig NIC %s is not plugged in\n' "$mac" "$name"
  elif [ "$rc" = 2 ]; then
    printf 'FAIL|(nic-rename) several interfaces carry %s: %s\n' "$mac" "$ifc"
  else
    printf 'FAIL|(nic-rename) %s is still %s, not %s -- the .link applies when the device is added: %s\n' "$mac" "$ifc" "$name" "$(strih_nic_rename_apply_hint "$sysroot" "$ifc")"
  fi
  if [ "$rig" != "$name" ]; then
    printf 'FAIL|(nic-rename) the rig NIC resolves to %s, not %s (STRIH_NIC_DRIVER %s / STRIH_NIC_IFACE)\n' "${rig:-<unresolved>}" "$name" "$drv"
  fi
  conf="$(strih_nic_rename_retired_conf)"
  if [ -f "$conf" ]; then
    while read -r m; do
      [ -n "$m" ] || continue
      lm="$(tr '[:upper:]' '[:lower:]' <<<"$m")"
      case "$lm" in "mac:${mac}" | "interface-name:${name}") hit="${hit}${hit:+;}${m}" ;; esac
    done < <(sed -n 's/^match-device=//p' "$conf" 2>/dev/null | head -n 1 | tr ';,' '\n\n')
    if [ -n "$hit" ]; then
      printf 'FAIL|(nic-rename) %s still unmanages the rig NIC (%s) -- NetworkManager will not activate it; take the entries out (the issue-1502 switch program does)\n' "$conf" "$hit"
    fi
  fi
  return 0
}

# strih_nic_rename_switch_program OLD NEW LAN_PROFILE LEG_PROFILE VLAN_ID GATEWAY PEER RIG_ADDR LEG_ADDR
# [WAIT_S] -> a self-contained bash program (stdout) that moves the box's rig LAN profile
# (connection.interface-name) and its VLAN leg profile (vlan.parent + connection.interface-name
# NEW.VLAN_ID) from OLD onto NEW, takes NEW's MAC out of the issue-1495 retired-NIC drop-in, brings both
# up, then waits up to WAIT_S (60) for the gateway to answer a ping on NEW AND the leg peer an ARP on
# NEW.VLAN_ID (ARP: the leg's nft filter drops inbound echo replies; ARP is not IP).
#   * ok: OLD is disconnected + link down (it would answer ARP for RIG_ADDR on the same L2, and
#     NetworkManager could put a DHCP profile on it), and RIG_ADDR / LEG_ADDR are announced (unsolicited
#     ARP) on NEW / NEW.VLAN_ID so no neighbour keeps the old MAC.
#   * no answer: NEW goes unmanaged again (when it was retired) + link down, the drop-in is restored, both
#     profiles go back to OLD and up; a rollback that cannot bring OLD up is `rollback-FAILED` (exit 2).
# It refuses before any change when NEW is absent, when arping is not iputils' (other builds take other
# flags), or when the drop-in has more than one match-device line. The outcome lands in
# RIG_NIC_SWITCH_RESULT (/run/rig-nic-switch.result: `ok ...` | `rolled-back ...` | `rollback-FAILED ...` |
# `refused ...`). Run it DETACHED (systemd-run): the ssh session rides OLD.
strih_nic_rename_switch_program() {
  local old="${1:?old NIC}" new="${2:?new NIC}" lan="${3:?LAN profile}" leg="${4:?leg profile}"
  local vid="${5:?VLAN id}" gw="${6:?gateway}" peer="${7:?leg peer}" rig="${8:?rig address}"
  local legaddr="${9:?leg address}" wait="${10:-60}" a
  local LC_ALL=C up='ABCDEFGHIJKLMNOPQRSTUVWXYZ' lo='abcdefghijklmnopqrstuvwxyz' dg='0123456789'
  for a in "$old" "$new" "$lan" "$leg" "$vid" "$gw" "$peer" "$rig" "$legaddr" "$wait"; do
    case "$a" in '' | -* | *[!${up}${lo}${dg}._:-]*)
      echo "strih-nic-rename: '${a}' is not a plain name / address" >&2; return 1 ;;
    esac
  done
  for a in "$vid" "$wait"; do
    [[ "$a" =~ ^[123456789][0123456789]*$ ]] || { echo "strih-nic-rename: '${a}' is not a whole number" >&2; return 1; }
  done
  [ "$vid" -le 4094 ] || { echo "strih-nic-rename: VLAN id ${vid} is over 4094" >&2; return 1; }
  printf '%s\n' '#!/usr/bin/env bash' \
    "# issue 1502: the rig NIC switch ${old} -> ${new} (scripts/lib/strih-nic-rename.sh). Run detached." \
    'set -uo pipefail' \
    "OLD=${old} NEW=${new} LAN=${lan} LEG=${leg} VID=${vid} GW=${gw} PEER=${peer}" \
    "RIG_ADDR=${rig} LEG_ADDR=${legaddr} WAIT=${wait}"
  cat <<'PROGRAM'
RESULT="${RIG_NIC_SWITCH_RESULT:-/run/rig-nic-switch.result}"
RETIRED="${RIG_NIC_SWITCH_RETIRED:-/etc/NetworkManager/conf.d/90-vban-dante70-retired.conf}"
SYSROOT="${RIG_NIC_SWITCH_SYSROOT:-/sys}"
log() { printf 'rig-nic-switch: %s\n' "$*"; }
finish() { printf '%s\n' "$1" >"$RESULT" 2>/dev/null || log "could not write ${RESULT}"; log "$1"; exit "$2"; }
point() {
  nmcli connection modify "$LAN" connection.interface-name "$1" </dev/null \
    && nmcli connection modify "$LEG" vlan.parent "$1" connection.interface-name "$1.$VID" </dev/null
}
up() {
  nmcli --wait 30 connection up "$LAN" </dev/null >/dev/null && nmcli --wait 30 connection up "$LEG" </dev/null >/dev/null
}
healthy() {
  ping -c 1 -W 1 -I "$NEW" "$GW" >/dev/null 2>&1 && arping -q -c 1 -w 2 -I "$NEW.$VID" "$PEER" >/dev/null 2>&1
}
announce() {
  arping -q -U -c 2 -I "$1" "$2" >/dev/null 2>&1 || log "WARNING: the unsolicited ARP for $2 on $1 failed -- neighbours wait for their entry to expire"
}
[ -e "${SYSROOT}/class/net/${NEW}" ] || finish "refused ${NEW} does not exist (rename it first) -- nothing changed" 1
case "$(arping -V 2>&1 || true)" in *iputils*) ;; *) finish "refused arping is missing or not iputils' (apt-get install iputils-arping) -- nothing changed" 1 ;; esac
[ -e "${SYSROOT}/class/net/${OLD}" ] || log "WARNING: ${OLD} is gone -- a rollback cannot restore the link"
MAC="$({ tr '[:upper:]' '[:lower:]' <"${SYSROOT}/class/net/${NEW}/address"; } 2>/dev/null || true)"
BACKUP=""
if [ -n "$MAC" ] && [ -f "$RETIRED" ]; then
  [ "$(grep -c '^match-device=' "$RETIRED" || true)" -le 1 ] || finish "refused ${RETIRED} has more than one match-device line -- edit it by hand; nothing changed" 1
  keep=""; hit=""
  while read -r m; do
    [ -n "$m" ] || continue
    lm="$(tr '[:upper:]' '[:lower:]' <<<"$m")"
    case "$lm" in "mac:${MAC}" | "interface-name:${NEW}" | "interface-name:enx${MAC//:/}") hit=1 ;; *) keep="${keep}${keep:+;}${m}" ;; esac
  done < <(sed -n 's/^match-device=//p' "$RETIRED" | tr ';,' '\n\n')
  if [ -n "$hit" ]; then
    BACKUP="${RETIRED}.rig-nic-switch.bak"
    cp -p -- "$RETIRED" "$BACKUP" || finish "refused could not back up ${RETIRED} -- nothing changed" 1
    if [ -n "$keep" ]; then
      tmp="${RETIRED}.rig-nic-switch.new"
      while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in match-device=*) printf 'match-device=%s\n' "$keep" ;; *) printf '%s\n' "$line" ;; esac
      done <"$RETIRED" >"$tmp" && mv -f -- "$tmp" "$RETIRED" || finish "refused could not rewrite ${RETIRED}" 1
    else
      rm -f -- "$RETIRED" || finish "refused could not remove ${RETIRED}" 1
    fi
    nmcli general reload conf </dev/null >/dev/null 2>&1 || true
    nmcli device set "$NEW" managed yes </dev/null >/dev/null 2>&1 || true
    log "${MAC} taken out of ${RETIRED} (backup ${BACKUP})"
  fi
fi
rollback() {
  log "rolling back to ${OLD}: $1"
  if [ -n "$BACKUP" ]; then
    mv -f -- "$BACKUP" "$RETIRED" && nmcli general reload conf </dev/null >/dev/null 2>&1
    nmcli device set "$NEW" managed no </dev/null >/dev/null 2>&1 || log "WARNING: ${NEW} stays managed until NetworkManager's next start"
  fi
  ip link set dev "$NEW" down || log "WARNING: could not take ${NEW}'s link down"
  { point "$OLD" && up; } || finish "rollback-FAILED $1 -- ${OLD} did not come back up: the box may be off the network (console needed)" 2
  finish "rolled-back $1" 1
}
point "$NEW" || rollback "nmcli could not point the profiles at ${NEW}"
up || log "a connection up on ${NEW} failed -- checking reachability anyway"
end=$(( $(date +%s) + WAIT ))
while :; do
  if healthy; then
    nmcli device disconnect "$OLD" </dev/null >/dev/null 2>&1 || log "WARNING: nmcli could not disconnect ${OLD}"
    ip link set dev "$OLD" down || log "WARNING: could not take ${OLD}'s link down -- it may still answer ARP for ${RIG_ADDR}"
    announce "$NEW" "$RIG_ADDR"
    announce "$NEW.$VID" "$LEG_ADDR"
    finish "ok ${LAN} on ${NEW}, ${LEG} on ${NEW}.${VID}: ${GW} pings, ${PEER} answers ARP; ${OLD} down, ${RIG_ADDR} + ${LEG_ADDR} announced" 0
  fi
  [ "$(date +%s)" -lt "$end" ] || break
  sleep 2
done
rollback "${GW} (ping on ${NEW}) or ${PEER} (ARP on ${NEW}.${VID}) did not answer within ${WAIT} s"
PROGRAM
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  case "${1-}" in
    --switch-program) shift; strih_nic_rename_switch_program "$@" ;;
    *) echo "usage: $0 --switch-program OLD NEW LAN_PROFILE LEG_PROFILE VLAN_ID GATEWAY PEER RIG_ADDR LEG_ADDR [WAIT_S]" >&2; exit 2 ;;
  esac
fi
