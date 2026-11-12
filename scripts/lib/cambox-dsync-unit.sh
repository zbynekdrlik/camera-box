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
#   cambox_lan_nic SYSROOT               the ONE `enp*` under SYSROOT/class/net with a `device` link AND
#                                        carrier 1 (the netplan LAN rule, `match: name: "enp*"`: a PCI NIC
#                                        is enp*, the USB camera link enx*, a VLAN has no device link; every
#                                        live cambox has TWO onboard ports and the cable sits in either, so
#                                        the cabled one decides); none or two or more = rc 1 + a named
#                                        stderr line, never a guess
#   cambox_dsync_unit_text [NIC]         the STEP 17 unit; without a NIC byte-identical to the pre-1439
#                                        heredoc, with one ` --interface <NIC>` after `--ntp-server
#                                        strih.lan`. A NIC that is not 1-15 of [A-Za-z0-9._-] or starts
#                                        with `-` emits nothing (rc 1) -- strih_dantesync_unit_text's check
#   cambox_dsync_gather_remote_snippet [SYSROOT] [UNIT]
#                                        read-only on-box text for verify-device (ba): the LAN NIC, the
#                                        unit's ExecStart and dsync's own /status, then END
#   cambox_dsync_verify_verdict BLOCK    pure: ok:<nic> | gather-incomplete | nic-unresolved |
#                                        unit-not-bound:<ExecStart> | unreadable:no-python3-on-the-box |
#                                        the dsync_iface_verdict tokens
#   cambox_dsync_apply_prelude / _plan_cmds / _restart_cmds
#                                        the live apply's dsync part (see the section at the end)
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
    # the cabled port: a cambox has two onboard ports and the cable sits in either (issue 1439)
    [ "$(cat "$d/carrier" 2>/dev/null || true)" = 1 ] || continue
    found="${found:+$found }$n"
    count=$((count + 1))
  done
  if [ "$count" -eq 1 ]; then
    printf '%s\n' "$found"
    return 0
  fi
  if [ "$count" -eq 0 ]; then
    echo "cambox_lan_nic: no enp* interface with a device and carrier 1 (a cable) under ${sys}/class/net -- the LAN NIC is unknown, dsync is not bound to a guess" >&2
  else
    echo "cambox_lan_nic: ${count} cabled enp* interfaces (carrier 1) under ${sys}/class/net (${found}) -- the LAN NIC is ambiguous, dsync is not bound to a guess" >&2
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
  printf 'command -v python3 >/dev/null 2>&1 || echo "NOPY=1"\n'
  printf 'if _d_f="$(dsync_iface_read_status)"; then echo "STATUS=1"; echo "FIELDS=$_d_f"; else echo "STATUS=0"; fi\n'
  printf 'echo END\n'
}

cambox_dsync_verify_verdict() {
  local block="${1-}" line nic="" exec_="" status="" fields="" end=0 nopy=0 rest conf joined v
  while IFS= read -r line; do
    case "$line" in
      NOPY=1) nopy=1 ;;
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
  [ "$nopy" != 1 ] || { echo "unreadable:no-python3-on-the-box"; return 0; }
  v="$(dsync_iface_verdict "${status:-0}" "$conf" "$joined" "$nic")"
  echo "$v"
}

# --- the live apply's dsync part (issue 1439) -----------------------------------------------------
# Three emitters scripts/lib/cambox-ro-units.sh cambox_ro_units_apply_program splices into its ONE
# remote program (kept here so that program stays one window and one function). They share its
# shell: `_rou_box`, `_rou_rl_need` (the sha comparator), `_rou_reloaded` (the daemon-reload held),
# and the `_rou_ds_*` names below; the window itself writes the unit with `_rou_write` when
# `_rou_do_ds` = 1 and sets `_rou_ds_written`.
#   cambox_dsync_apply_prelude UNIT SYSROOT WAIT_S POLL_S  the values + the embedded functions; a bad
#                                                          WAIT_S/POLL_S reads as the default 150/5
#   cambox_dsync_apply_plan_cmds     step 1: the NIC resolved ON the box, the wanted unit, the PLAN /
#                                    NOTE line, a named FAIL when the NIC does not resolve
#   cambox_dsync_apply_restart_cmds  step 3: `systemctl restart dsync` only when the unit was written
#                                    and the reload held, then /status read until locked + bound or
#                                    past WAIT_S (at least two reads when WAIT_S > 0), named with the
#                                    reads and the seconds it took

cambox_dsync_apply_prelude() {
  local unit="${1-}" sys="${2-}" wait="${3-}" poll="${4-}"
  [[ "$wait" =~ ^[0123456789]{1,4}$ ]] || wait=150
  [[ "$poll" =~ ^[123456789][0123456789]{0,2}$ ]] || poll=5
  wait=$((10#$wait))
  poll=$((10#$poll))
  # a poll longer than the budget would read once, right after the restart: always a cold-lock FAIL
  if [ "$wait" -gt 0 ] && [ "$poll" -gt "$wait" ]; then poll="$wait"; fi
  printf '_rou_ds_unit=%q\n' "$unit"
  printf '_rou_ds_sys=%q\n' "$sys"
  printf '_rou_ds_wait=%s\n' "$wait"
  printf '_rou_ds_poll=%s\n' "$poll"
  declare -f cambox_lan_nic cambox_dsync_unit_text dsync_iface_verdict dsync_iface_status_fields dsync_iface_read_status
}

cambox_dsync_apply_plan_cmds() {
  cat <<'DSPLAN'
# issue 1439: dsync's unit, bound to the LAN NIC resolved on THIS box; only where dsync.service exists
# (a box without it needs setup-device, like the netconsole unit).
_rou_ds_present=0
[ ! -e "$_rou_ds_unit" ] || _rou_ds_present=1
_rou_ds_nic=""
_rou_ds_body=""
_rou_do_ds=0
_rou_ds_bad=0
if [ "$_rou_ds_present" = 1 ]; then
  if _rou_ds_nic="$(cambox_lan_nic "$_rou_ds_sys")" && _rou_ds_body="$(cambox_dsync_unit_text "$_rou_ds_nic")"; then
    if _rou_rl_need "$_rou_ds_unit" "$_rou_ds_body"; then _rou_do_ds=1; fi
  else
    _rou_ds_bad=1
  fi
fi
if [ "$_rou_ds_present" = 0 ]; then
  echo "NOTE: no $_rou_ds_unit on $_rou_box -- dsync is not provisioned here; re-run setup-device.sh for it (issue 1439)"
elif [ "$_rou_ds_bad" = 1 ]; then
  echo "PLAN ($_rou_box): dsync PTP NIC <unresolved>; unit not written (issue 1439)"
  echo "FAIL: [issue 1394] the LAN NIC of $_rou_box does not resolve (the line above) -- dsync.service is not written, dsync is never bound to a guess (issue 1439)" >&2
else
  echo "PLAN ($_rou_box): dsync PTP NIC $_rou_ds_nic; unit $([ "$_rou_do_ds" = 1 ] && echo write || echo in-place) (issue 1439)"
fi
DSPLAN
}

cambox_dsync_apply_restart_cmds() {
  cat <<'DSRESTART'
if [ "$_rou_ds_written" = 1 ]; then
  if [ "$_rou_reloaded" != 1 ]; then
    echo "FAIL: [issue 1394] dsync not restarted on $_rou_box: the reload failed, so the new unit is not loaded (issue 1439)" >&2
    _rou_rc=1
  elif ! systemctl restart dsync; then
    echo "FAIL: [issue 1394] dsync did not restart on $_rou_box -- read: journalctl -u dsync -n 20 (issue 1439)" >&2
    _rou_rc=1
  else
    _rou_ds_v=""
    _rou_ds_lk=""
    _rou_ds_t0="$(date +%s)"
    _rou_ds_reads=0
    # bounded by the count AND the deadline: a read can take curl's 3 s, so the count alone overran it
    _rou_ds_n=$((_rou_ds_wait / _rou_ds_poll))
    for ((_rou_i = 0; _rou_i <= _rou_ds_n; _rou_i++)); do
      if [ "$_rou_i" != 0 ]; then
        [ $(($(date +%s) - _rou_ds_t0)) -lt "$_rou_ds_wait" ] || break
        sleep "$_rou_ds_poll"
      fi
      _rou_ds_read=0
      _rou_ds_f=""
      if _rou_ds_f="$(dsync_iface_read_status)"; then _rou_ds_read=1; fi
      _rou_ds_reads=$((_rou_ds_reads + 1))
      _rou_ds_lk="${_rou_ds_f%%$'\t'*}"
      _rou_ds_rest="${_rou_ds_f#*$'\t'}"
      _rou_ds_v="$(dsync_iface_verdict "$_rou_ds_read" "${_rou_ds_rest%%$'\t'*}" "${_rou_ds_rest#*$'\t'}" "$_rou_ds_nic")"
      if [ "$_rou_ds_v" = "ok:$_rou_ds_nic" ] && [ "$_rou_ds_lk" = true ]; then break; fi
    done
    _rou_ds_took="${_rou_ds_reads} read(s) in $(($(date +%s) - _rou_ds_t0)) s, budget ${_rou_ds_wait} s"
    if [ "$_rou_ds_v" = unreadable ] && ! command -v python3 >/dev/null 2>&1; then
      echo "FAIL: [issue 1394] dsync's /status on $_rou_box cannot be read: no python3 on the box ($_rou_ds_took, issue 1439)" >&2
      _rou_rc=1
    elif [ "$_rou_ds_v" != "ok:$_rou_ds_nic" ]; then
      echo "FAIL: [issue 1394] dsync on $_rou_box is not bound to $_rou_ds_nic: $_rou_ds_v ($_rou_ds_took, issue 1439)" >&2
      _rou_rc=1
    elif [ "$_rou_ds_lk" != true ]; then
      echo "FAIL: [issue 1394] dsync on $_rou_box is bound to $_rou_ds_nic but not locked ($_rou_ds_took) -- re-read /status, the cold lock takes ~95 s (issue 1439)" >&2
      _rou_rc=1
    else
      echo "OK: dsync locked on $_rou_ds_nic (--interface, configured and joined; $_rou_ds_took; issue 1439)"
    fi
  fi
fi
DSRESTART
}
