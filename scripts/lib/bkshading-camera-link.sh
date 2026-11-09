#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no side effects at source time) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/bkshading-camera-link.sh -- issue 1452 (shading#6, owner 9.11.2026): the Blackmagic camera's
# USB-C network link. Since shading v0.2.x the bkshading relay drives the camera over its REST API on the
# camera's own USB CDC-NCM link (the kernel names it enx<mac>, driver cdc_ncm). The camera is the DHCP
# server of a /30; the relay reads its address from systemd-networkd's lease of that link. camera-box
# installs the ONE .network file the shading contract (docs/CONTRACT.md v0.2.2) specifies, byte for byte,
# on every box that runs the relay: every cambox (setup-device, bkshading-deploy-relay.sh) and the handheld
# SBC (bkshading-provision-sbc.sh --install).
#
# Why systemd-networkd: it is already the cambox's network manager (setup-device STEP 2 writes the netplan
# with `renderer: networkd`, the LAN stanza matching only `enp*`, issue 1155), so a file in
# /etc/systemd/network takes effect without touching netplan. `05-` sorts before netplan's
# /run/systemd/network/10-netplan-*.network. The match is the driver AND Blackmagic's USB vendor id, so it
# never claims the cabled LAN NIC (enp*, the issue-1439 rule) nor a cdc_ncm uplink dongle, and the link
# gets no default route, no DNS, no NTP: the box never routes through the camera.
#
# Contents: the path + text, the install (write when different, enable systemd-networkd, never a start),
# the read-only remote gather + the pure verdict verify-device (bb) and the SBC --check grade, and the
# live-box apply program (`bkshading_camera_link_apply_program`, piped into `ssh root@<box> bash -s`: one
# verified rw window, networkd reloaded only after the root reads read-only again).
# Test seam: BKSHADING_CAMERA_LINK_PATH (the file on the box).

if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/ro-window.sh
  . "${BASH_SOURCE[0]%/*}/ro-window.sh"
fi

# The contract's file, verbatim (tests/fixtures/shading_pinned_rest/CONTRACT.md pins it byte for byte).
bkshading_camera_link_network_path() { printf '%s\n' "${BKSHADING_CAMERA_LINK_PATH:-/etc/systemd/network/05-bkshading-camera.network}"; }

bkshading_camera_link_network_text() {
  cat <<'NETWORK'
# /etc/systemd/network/05-bkshading-camera.network
# shading#6: the Blackmagic camera's USB-C network link (CDC-NCM). The camera is the DHCP server
# of a /30; the bkshading relay reads its address from this link's lease.
[Match]
Driver=cdc_ncm
Property=ID_VENDOR_ID=1edb

[Link]
RequiredForOnline=no

[Network]
DHCP=ipv4
LinkLocalAddressing=no
IPv6AcceptRA=no

[DHCPv4]
UseRoutes=no
UseGateway=no
UseDNS=no
UseNTP=no
UseHostname=no
UseDomains=no
NETWORK
}

# bkshading_camera_link_sha -> the sha256 of the file text (the gather compares the box's file to it).
bkshading_camera_link_sha() { bkshading_camera_link_network_text | sha256sum | cut -d' ' -f1; }

# bkshading_camera_link_install [ROOT] -> writes the file under ROOT (default /) when it is missing or
# differs (a temp file + rename, mode 0644) and enables systemd-networkd. ENABLE-only, never a start or a
# reload (provisioning-scripts.md: setup-device defers to the next boot; a live box is brought over by
# bkshading-deploy-relay.sh, which reloads networkd only after its verified ro close). The caller holds
# the rw window. Prints `CAMERA_LINK_WRITTEN=1|0` last; rc 1 + a named line on any failure.
# Seam: BKSHADING_CAMERA_LINK_SYSTEMCTL (tests).
bkshading_camera_link_install() {
  local root="${1:-}" path dest tmp written=0 sc="${BKSHADING_CAMERA_LINK_SYSTEMCTL:-systemctl}"
  path="$(bkshading_camera_link_network_path)"
  dest="${root%/}${path}"
  if [ "$(sha256sum "$dest" 2>/dev/null | cut -d' ' -f1)" != "$(bkshading_camera_link_sha)" ]; then
    mkdir -p "$(dirname "$dest")" || { echo "  issue 1452: cannot create $(dirname "$dest")" >&2; return 1; }
    tmp="${dest}.new.$$"
    if ! bkshading_camera_link_network_text >"$tmp" || ! chmod 0644 "$tmp" || ! mv -f "$tmp" "$dest"; then
      rm -f "$tmp"
      echo "  issue 1452: could not write $dest" >&2
      return 1
    fi
    written=1
    echo "  issue 1452: wrote $dest (the camera's USB-C link: DHCP from the camera, no route, no DNS)"
  else
    echo "  issue 1452: $dest already the contract's file"
  fi
  if [ "$("$sc" is-enabled systemd-networkd.service 2>/dev/null || true)" != enabled ]; then
    "$sc" enable systemd-networkd.service >/dev/null 2>&1 \
      || { echo "  issue 1452: could not enable systemd-networkd (the camera link needs it)" >&2; return 1; }
    echo "  issue 1452: systemd-networkd enabled (not started here)"
  fi
  printf 'CAMERA_LINK_WRITTEN=%s\n' "$written"
}

# bkshading_camera_link_gather_snippet -> REMOTE sh text (read-only) printing the KEY=value lines the
# verdict grades: the file's sha, networkd's enable state, and how many default routes / DNS servers sit
# on an enx* link (the camera link must carry neither).
bkshading_camera_link_gather_snippet() {
  local path
  path="$(bkshading_camera_link_network_path)"
  cat <<SNIP
printf 'LINK_NET_SHA=%s\n' "\$(sha256sum $path 2>/dev/null | cut -d' ' -f1)"
printf 'LINK_NETWORKD=%s\n' "\$(systemctl is-enabled systemd-networkd.service 2>/dev/null)"
printf 'LINK_ENX_DEFAULT=%s\n' "\$(ip -4 route show default 2>/dev/null | grep -c ' dev enx' || true)"
printf 'LINK_ENX_DNS=%s\n' "\$(for _l in /sys/class/net/enx*; do [ -e "\$_l" ] || continue; resolvectl dns "\${_l##*/}" 2>/dev/null | sed 's/^[^:]*://' ; done | tr -s ' ' | grep -c '[0-9]' || true)"
SNIP
}

# The tools that put the file on a box, named in every FAIL line.
bkshading_camera_link_fix_hint() {
  printf '%s\n' "a live cambox: bash scripts/lib/bkshading-camera-link.sh --apply-program | ssh root@<box> bash -s (a REST-pin relay deploy runs it too); else setup-device.sh / bkshading-provision-sbc.sh --install"
}

_bkcl_field() { printf '%s\n' "${1:-}" | sed -n "s/^$2=//p" | tail -n 1; }

# bkshading_camera_link_verdict BLOCK -> `ok` | one `FAIL: <reason>` per line. Pure.
bkshading_camera_link_verdict() {
  local block="${1:-}" out="" v
  if [ -z "$block" ]; then
    printf '%s\n' "FAIL: empty camera-link read -- cannot certify the camera's USB-C link file"
    return 0
  fi
  v="$(_bkcl_field "$block" LINK_NET_SHA)"
  if [ -z "$v" ]; then
    out="${out}FAIL: $(bkshading_camera_link_network_path) missing -- the relay cannot find the camera ($(bkshading_camera_link_fix_hint))"$'\n'
  elif [ "$v" != "$(bkshading_camera_link_sha)" ]; then
    out="${out}FAIL: $(bkshading_camera_link_network_path) differs from the shading contract's file ($(bkshading_camera_link_fix_hint))"$'\n'
  fi
  v="$(_bkcl_field "$block" LINK_NETWORKD)"
  [ "$v" = enabled ] || out="${out}FAIL: systemd-networkd is-enabled='${v:-<none>}' -- the camera link file has no daemon to apply it"$'\n'
  v="$(_bkcl_field "$block" LINK_ENX_DEFAULT)"
  case "$v" in
    0) ;;
    '' | *[!0-9]*) out="${out}FAIL: the default routes could not be read"$'\n' ;;
    *) out="${out}FAIL: ${v} default route(s) on an enx* link -- the box must never route through the camera (issue 1155/1439)"$'\n' ;;
  esac
  v="$(_bkcl_field "$block" LINK_ENX_DNS)"
  case "$v" in
    0 | '') ;;
    *[!0-9]*) out="${out}FAIL: the DNS servers of the enx* links could not be read"$'\n' ;;
    *) out="${out}FAIL: an enx* link carries DNS servers -- the camera must never be the box's resolver"$'\n' ;;
  esac
  if [ -z "$out" ]; then printf '%s\n' ok; else printf '%s' "$out"; fi
}

# bkshading_camera_link_apply_program -> the REMOTE program (root, `ssh root@<box> bash -s`) that puts the
# camera link file on a LIVE cambox without a setup-device re-run. Everything runs inside ONE function
# called with </dev/null (the program arrives on bash's stdin). 1. read: the root mode, the file, networkd's
# enable state; 2. the rw window, only when something needs a write OR the root does not read ro: the file
# (temp + rename), `systemctl enable systemd-networkd`, then the ONE verified close (an EXIT trap runs the
# same close on any failure inside); 3. after the close only: `networkctl reload` when the file changed (the
# camera link comes up; networkd reconfigures only the links whose .network changed, the LAN NIC's netplan
# file is untouched), then the link list read back. Exit 0 only when every step held.
bkshading_camera_link_apply_program() {
  cat <<'PROG'
# scripts/lib/bkshading-camera-link.sh (issue 1452): the camera's USB-C link file on this box.
_bkcl_main() {
set -eu
PROG
  printf '_bkcl_f=%q\n' "$(bkshading_camera_link_network_path)"
  printf '_bkcl_body=%q\n' "$(bkshading_camera_link_network_text)"
  printf '_bkcl_want=%q\n' "$(bkshading_camera_link_sha)"
  printf '%s\n' "$(declare -f ro_root_mount_mode)"
  cat <<'PROG'
_bkcl_box="$(hostname 2>/dev/null || echo this box)"
_bkcl_sha() { if [ -e "$1" ]; then _bkcl_s="$(sha256sum <"$1")"; echo "${_bkcl_s%% *}"; else echo __ABSENT__; fi; }
_bkcl_mode="$(ro_root_mount_mode "$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)")"
_bkcl_do_f=0
[ "$(_bkcl_sha "$_bkcl_f")" = "$_bkcl_want" ] || _bkcl_do_f=1
_bkcl_do_en=0
[ "$(systemctl is-enabled systemd-networkd.service 2>/dev/null || true)" = enabled ] || _bkcl_do_en=1
echo "PLAN ($_bkcl_box): root $_bkcl_mode; camera link file $([ "$_bkcl_do_f" = 1 ] && echo write || echo in-place); systemd-networkd $([ "$_bkcl_do_en" = 1 ] && echo enable || echo enabled)"
_bkcl_rc=0
_bkcl_open=0
_bkcl_close() {
  [ "$_bkcl_open" = 1 ] || return 0
  _bkcl_open=0
PROG
  ro_window_close_cmds "issue 1452" "\$_bkcl_box" \
    "The camera link file is on disk, but the root stays read-WRITE, so networkd is not reloaded." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then run this program again (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_bkcl_close' EXIT
if [ "$_bkcl_do_f" = 1 ] || [ "$_bkcl_do_en" = 1 ] || [ "$_bkcl_mode" != ro ]; then
  if [ "$_bkcl_mode" = ro ] || [ "$_bkcl_mode" = unknown ]; then
    if ! mount -o remount,rw /; then
      echo "FAIL: [issue 1452] could not remount / read-write on $_bkcl_box -- nothing was written" >&2
      exit 1
    fi
  fi
  _bkcl_open=1
  if [ "$_bkcl_do_f" = 1 ]; then
    if ! { mkdir -p "${_bkcl_f%/*}" && printf '%s\n' "$_bkcl_body" >"$_bkcl_f.new" && chmod 0644 "$_bkcl_f.new" \
      && mv -f "$_bkcl_f.new" "$_bkcl_f"; }; then
      rm -f "$_bkcl_f.new"
      echo "FAIL: [issue 1452] could not write $_bkcl_f on $_bkcl_box" >&2
      exit 1
    fi
    echo "OK: wrote $_bkcl_f"
  fi
  if [ "$_bkcl_do_en" = 1 ]; then
    systemctl enable systemd-networkd.service >/dev/null 2>&1 \
      || { echo "FAIL: [issue 1452] could not enable systemd-networkd on $_bkcl_box" >&2; exit 1; }
    echo "OK: systemd-networkd enabled"
  fi
  _bkcl_close
fi
trap - EXIT
if [ "$_bkcl_do_f" = 1 ]; then
  if networkctl reload; then
    echo "OK: networkctl reload -- the camera link (if a camera is on USB) takes its DHCP lease now"
  else
    echo "FAIL: [issue 1452] networkctl reload failed on $_bkcl_box -- the file applies at the next boot" >&2
    _bkcl_rc=1
  fi
fi
echo "LINKS ($_bkcl_box):"
networkctl list --no-legend 2>/dev/null || true
ip -4 route show default 2>/dev/null || true
return "$_bkcl_rc"
}
_bkcl_main </dev/null
PROG
}

# CLI: `bash scripts/lib/bkshading-camera-link.sh --apply-program` prints the live-box program (pipe it into
# `ssh root@<box> bash -s` while no E2E holds the rig lease). Sourcing defines the functions only.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  case "${1:-}" in
    --apply-program) bkshading_camera_link_apply_program ;;
    *) echo "usage: $0 --apply-program" >&2; exit 2 ;;
  esac
fi
