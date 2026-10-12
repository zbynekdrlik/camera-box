#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih operator-surface constants, the setup-strih
# step 16g install and the verify-strih item 43 grader; no top-level statements besides constants) --
# the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. setup-strih.sh / verify-strih.sh set their own.
# Every function checks its own return codes.
#
# scripts/lib/strih-operator.sh -- issue 1418 (owner 12.10.2026, 30 min before the SNV event): the strih
# notebook's X server stopped answering (OBS kept rendering, the panel froze), the Bluetooth mouse could
# not pair, the touchpad did not click, clicking the Multiview did not cut and the Stream Deck was stuck.
# verify-strih had passed: it graded services, OBS, genlock and NDI, never what the OPERATOR touches.
# ROZHODNUTE 6126870183: every hand fix on a notebook lands in setup + verify the same day. Design
# 6127043932 (approach 1): this ONE lib is the operator surface.
#
#   * strih_operator_install -- setup-strih step 16g (as root), every role:
#     - the OBS user.ini [BasicWindow] keys STRIH_OPERATOR_INI_KEYS: SaveProjectors=true +
#       ProjectorAlwaysOnTop=false (issue 1346, moved here from step 7: the operator's laptop Multiview
#       projector re-opens on every launch and never stays above the other windows) and
#       MultiviewMouseSwitch=true (clicking a Multiview tile cuts to that scene; it read false on
#       12.10.2026, most likely from the 11.10 user.ini backup restore). A RawConfigParser upsert that
#       never clobbers an unparseable file, and NEVER while OBS runs: OBS rewrites user.ini from memory
#       on exit, so a key written under a running OBS is lost (a WARN names it; verify grades it);
#     - every OTHER wired NetworkManager profile on the rig subnet: connection.autoconnect no, and down
#       when it is active (issue 1411: the onboard port's DHCP profile on the rig /23 won the routes and
#       the fleet clock broke for ~22 h). The subnet rule is strih_nic_rig_subnet_others (the ONE rule
#       verify item 36's (nic-homed) row uses). The rig NIC's own profile (its active device or its
#       connection.interface-name) is never touched, and nothing is touched while the rig address is
#       not on the rig NIC (the subnet is then unknown). An interface on the rig subnet that no
#       NetworkManager connection runs is named in a WARN (item 36 FAILs it).
#
# Test seams (tests/python/test_strih_operator_1418.py): STRIH_OPERATOR_OBS_RUNNING_CMD (`pgrep -x obs`),
# STRIH_OPERATOR_NMCLI (nmcli), STRIH_OPERATOR_IP_ADDR_CMD (`ip -o -4 addr show`), STRIH_OPERATOR_SYSFS
# (/sys); STRIH_NIC_IFACE overrides the rig NIC like everywhere else.

# The subnet rule + the rig-NIC verdict live in the NIC lib (verify item 36).
if ! declare -F strih_nic_rig_subnet_others >/dev/null; then
  # shellcheck source=scripts/lib/strih-nic-driver.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-nic-driver.sh"
fi

STRIH_OPERATOR_INI_KEYS=(SaveProjectors=true ProjectorAlwaysOnTop=false MultiviewMouseSwitch=true)

strih_operator_sysfs() { printf '%s' "${STRIH_OPERATOR_SYSFS:-/sys}"; }
_strih_operator_nmcli() { "${STRIH_OPERATOR_NMCLI:-nmcli}" "$@"; }

# _strih_operator_cmd VAR DEFAULT -> run the command a seam variable names (word-split), else DEFAULT.
_strih_operator_cmd() {
  local -a cmd
  read -r -a cmd <<<"${1:-$2}"
  "${cmd[@]}"
}

# strih_operator_obs_running -> rc 0 when an OBS process runs.
strih_operator_obs_running() {
  _strih_operator_cmd "${STRIH_OPERATOR_OBS_RUNNING_CMD:-}" "pgrep -x obs" >/dev/null 2>&1
}

# _strih_operator_ip_addrs -> `ip -o -4 addr show` (always rc 0).
_strih_operator_ip_addrs() {
  _strih_operator_cmd "${STRIH_OPERATOR_IP_ADDR_CMD:-}" "ip -o -4 addr show" 2>/dev/null || true
}

# --- setup-strih step 16g --------------------------------------------------------------------------

# strih_operator_user_ini_seed USER_INI DESKTOP_USER -> upsert STRIH_OPERATOR_INI_KEYS into [BasicWindow]
# of USER_INI (owned by DESKTOP_USER afterwards). Never while OBS runs, never over an unparseable file:
# both print a WARN line. Always rc 0 (verify item 43's (mv-click) row grades the result).
strih_operator_user_ini_seed() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}"
  if strih_operator_obs_running; then
    echo "  WARN: OBS is running -- it rewrites ${ini} from memory on exit, so [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} were NOT seeded; stop strih-obs.service and re-run setup-strih.sh (issue 1418)"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "  WARN: python3 absent -- cannot seed [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini}"
    return 0
  fi
  if python3 - "$ini" "${STRIH_OPERATOR_INI_KEYS[@]}" <<'PY'
import configparser, os, sys
path = sys.argv[1]
# strict=False tolerates duplicate keys/sections OBS may write; RawConfigParser avoids % interpolation.
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
if os.path.exists(path):
    try:
        cp.read(path)
    except Exception as e:
        # NEVER clobber a user.ini that cannot be parsed: a rewrite would DISCARD every other OBS setting.
        sys.stderr.write("user.ini parse failed (%s) -- leaving it untouched\n" % e)
        sys.exit(3)
if not cp.has_section("BasicWindow"):
    cp.add_section("BasicWindow")
for kv in sys.argv[2:]:
    k, v = kv.split("=", 1)
    cp.set("BasicWindow", k, v)
with open(path, "w") as fh:
    cp.write(fh, space_around_delimiters=False)
PY
  then
    chown "$user":"$user" "$ini" 2>/dev/null || true
    echo "  seeded [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini}"
  else
    echo "  WARN: could not seed [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini} (left untouched, above) -- fix the file and re-run setup-strih.sh"
  fi
  return 0
}

# strih_operator_nic_isolate -> take every OTHER wired NetworkManager profile off the rig subnet (see the
# header). rc 1 + a stderr line when nmcli cannot list or change a profile; rc 0 otherwise (a rig NIC or
# rig address that cannot be read is a WARN: nothing is touched blind).
strih_operator_nic_isolate() {
  local ip addrs nic pfx others ifcs="" covered=" " rows u t dev ifn name ac a on_rig changed=0 ifc addr
  ip="$(strih_lx_ip)" || return 1
  addrs="$(_strih_operator_ip_addrs)"
  if ! nic="$(strih_lx_rig_nic "$(strih_operator_sysfs)" "$addrs" "$ip" 2>/dev/null)"; then
    echo "  WARN: the rig NIC cannot be resolved -- no other profile taken off the rig subnet (verify item 36 grades it)"
    return 0
  fi
  if ! pfx="$(strih_nic_rig_prefix "$nic" "$ip" "$addrs")"; then
    echo "  WARN: ${ip} is not on the rig NIC ${nic} right now -- the rig subnet is unknown, no other profile touched (verify item 36 grades it)"
    return 0
  fi
  others="$(strih_nic_rig_subnet_others "$nic" "$ip" "$addrs")"
  while read -r ifc addr; do
    if [ -n "$ifc" ]; then ifcs="${ifcs} ${ifc}"; fi
  done <<<"$others"
  if ! rows="$(_strih_operator_nmcli -g UUID,TYPE,DEVICE connection show 2>/dev/null)"; then
    echo "strih-operator: nmcli cannot list the NetworkManager connections" >&2
    return 1
  fi
  while IFS=: read -r u t dev; do
    [ -n "$u" ] && [ "$dev" != "$nic" ] || continue
    on_rig=0
    if [ -n "$dev" ]; then
      case " ${ifcs} " in *" ${dev} "*) on_rig=1; covered="${covered}${dev} " ;; esac
    fi
    if [ "$t" != 802-3-ethernet ]; then
      if [ "$on_rig" = 1 ]; then
        echo "  WARN: ${dev} carries an address on the rig subnet ${ip}/${pfx} through the ${t} connection ${u} -- not a wired profile, left as it is; take it off the rig subnet by hand (verify item 36 FAILs it)"
      fi
      continue
    fi
    ifn="$(_strih_operator_nmcli -g connection.interface-name connection show "$u" 2>/dev/null || true)"
    [ "$ifn" != "$nic" ] || continue
    for a in $(_strih_operator_nmcli -g ipv4.addresses connection show "$u" 2>/dev/null | tr ',' ' ' || true); do
      if strih_nic_on_subnet "$ip" "$pfx" "$a"; then on_rig=1; fi
    done
    [ "$on_rig" = 1 ] || continue
    name="$(_strih_operator_nmcli -g connection.id connection show "$u" 2>/dev/null || true)"
    ac="$(_strih_operator_nmcli -g connection.autoconnect connection show "$u" 2>/dev/null || true)"
    if [ "$ac" != no ]; then
      _strih_operator_nmcli connection modify "$u" connection.autoconnect no || {
        echo "strih-operator: nmcli could not set connection.autoconnect no on '${name}' (${u})" >&2; return 1; }
      echo "  '${name}' (${u}): connection.autoconnect no -- it is on the rig subnet ${ip}/${pfx} next to the rig NIC ${nic}"
      changed=1
    fi
    if [ -n "$dev" ]; then
      _strih_operator_nmcli connection down "$u" >/dev/null || {
        echo "strih-operator: nmcli could not bring '${name}' (${u}) down on ${dev}" >&2; return 1; }
      echo "  '${name}' (${u}): brought down on ${dev} (it carried the rig subnet next to ${nic})"
      changed=1
    fi
  done <<<"$rows"
  for ifc in $ifcs; do
    case "$covered" in *" ${ifc} "*) ;; *)
      echo "  WARN: ${ifc} carries an address on the rig subnet ${ip}/${pfx} but no NetworkManager connection runs on it -- take it down by hand (verify item 36 FAILs it)" ;;
    esac
  done
  if [ "$changed" = 0 ]; then
    echo "  no other wired profile on the rig subnet ${ip}/${pfx} next to ${nic} needed a change -- unchanged"
  fi
  return 0
}

# strih_operator_install USER_INI DESKTOP_USER -> setup-strih step 16g: the user.ini seed (WARN-only)
# and the rig-subnet isolation (rc 1 on an nmcli failure).
strih_operator_install() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}"
  strih_operator_user_ini_seed "$ini" "$user"
  strih_operator_nic_isolate
}
