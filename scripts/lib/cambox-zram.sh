#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure gather/verdict/program builders; the only top-level
# statements are the lazy source of ro-window.sh and two overridable constants) -- deliberately NOT
# `set -euo pipefail`: sourcing runs this file in the CALLER's shell, so strict mode here would leak
# into every caller (the scripts/lib convention, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cambox-zram.sh -- the cambox's swap (issue 1428), in ONE place.
#
# Owner 14.10.2026: every Linux box carries swap ("To ze tam nieje swap je hrube a opakovane
# porusenie pravidiel pre linux boxy"). The OBS boxes get a RAM-sized swapfile on their SSD
# (scripts/lib/obs-box-baseline.sh obs_box_swap). A cambox cannot: its root is READ-ONLY on a USB
# stick, and the sticks already die from wear (never an experiment that can kill a cambox disk). So a
# cambox swaps to compressed RAM: zram, set up at boot by systemd's own zram-generator (Ubuntu
# package systemd-zram-generator, no maintainer scripts), configured by ONE drop-in:
#   - zram-size = ram (the swap reads as large as RAM, matching the OBS-box rule; zstd keeps the
#     real RAM it uses at about a third of what is swapped out);
#   - the package's own /etc/systemd/zram-generator.conf (a conffile, `[zram0]` = min(ram/2, 4 GiB))
#     is never edited, so a package upgrade never asks a conffile question; the drop-in overrides it.
# Nothing is written to the stick at runtime: the generator reads /etc at boot, the device lives in
# RAM.
#
# Consumers:
#   - setup-device.sh `[zram-swap]` (rw window, enable-only like every provisioning step: the swap
#     comes up at the next boot) -- cambox_zram_install_dropin "";
#   - create-usb-linux.sh: the package in the chroot install + cambox_zram_install_dropin "$MOUNT_ROOT";
#   - verify-device.sh (at): cambox_zram_gather_remote_snippet (read-only, over ssh) +
#     cambox_zram_verdict (pure);
#   - a live box without a setup-device re-run: cambox_zram_apply_program, piped into `ssh root@<box>
#     bash -s` by the supervisor while no E2E holds the rig lease. It installs + writes inside ONE rw
#     window closed by the ONE verified close (scripts/lib/ro-window.sh, issue 1407) and starts the
#     swap only after the root reads read-only again.
# Tests: tests/python/test_cambox_zram_1428.py.

if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/ro-window.sh
  . "${BASH_SOURCE[0]%/*}/ro-window.sh"
fi

CAMBOX_ZRAM_PACKAGE="systemd-zram-generator"
# The drop-in on the box. Overridable so a test can point the gather and the program at a temp tree.
CAMBOX_ZRAM_DROPIN="${CAMBOX_ZRAM_DROPIN:-/etc/systemd/zram-generator.conf.d/camera-box.conf}"
# The zram swap must be at least this share of RAM (zram-size = ram gives ~100 %).
CAMBOX_ZRAM_MIN_PERCENT=90

# cambox_zram_dropin_content -> the drop-in, byte-for-byte (setup-device, create-usb and the apply
# write exactly this; verify-device compares its sha).
cambox_zram_dropin_content() {
  cat <<'EOF'
# camera-box issue 1428: swap on every Linux box. A cambox runs a read-only root on a USB stick, so
# its swap is compressed RAM (zram), never a file on the stick. Written through
# scripts/lib/cambox-zram.sh; graded by verify-device.sh (at).
[zram0]
zram-size = ram
compression-algorithm = zstd
swap-priority = 100
EOF
}

# cambox_zram_dropin_sha -> the sha256 hex of the drop-in content.
cambox_zram_dropin_sha() {
  local s
  s="$(cambox_zram_dropin_content | sha256sum)"
  printf '%s\n' "${s%% *}"
}

# cambox_zram_install_dropin ROOT -> writes ROOT$CAMBOX_ZRAM_DROPIN through a temp file + rename
# (ROOT "" = this box, create-usb passes the mounted image). Non-zero on any failure, the temp file
# removed. The package itself is installed by the caller's apt line.
cambox_zram_install_dropin() {
  local root="${1:-}" path
  path="${root}${CAMBOX_ZRAM_DROPIN}"
  if mkdir -p "${path%/*}" && cambox_zram_dropin_content >"$path.new" && chmod 0644 "$path.new" \
    && mv -f "$path.new" "$path"; then
    return 0
  fi
  rm -f "$path.new"
  echo "FAIL: [issue 1428] could not write ${path}" >&2
  return 1
}

# cambox_zram_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh (at): one
# KEY=VALUE line per fact, read-only. The last line is GATHER_DONE=1, so a cut read is told apart.
cambox_zram_gather_remote_snippet() {
  printf '_cz_d=%q\n' "$CAMBOX_ZRAM_DROPIN"
  printf '_cz_p=%q\n' "$CAMBOX_ZRAM_PACKAGE"
  cat <<'REMOTE'
echo "PKG=$(dpkg-query -W -f='${Status}' "$_cz_p" 2>/dev/null || true)"
if [ -e "$_cz_d" ]; then _cz_s="$(sha256sum <"$_cz_d" 2>/dev/null)"; echo "DROPIN_SHA=${_cz_s%% *}"; else echo "DROPIN_SHA=__ABSENT__"; fi
echo "MEM_KB=$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null)"
swapon --show=NAME,SIZE --bytes --noheadings 2>/dev/null | while read -r _cz_n _cz_b; do echo "SWAP=$_cz_n $_cz_b"; done
echo "GATHER_DONE=1"
REMOTE
}

# cambox_zram_verdict BLOCK -> `ok`, or one `FAIL: ...` line per broken facet. BLOCK is the gather's
# output (or a test fixture). Fail-closed: a missing or unreadable fact is a FAIL. Pure bash.
#   - the package is installed (`install ok installed`);
#   - the drop-in is byte-identical to cambox_zram_dropin_content;
#   - the active zram swap is at least CAMBOX_ZRAM_MIN_PERCENT of RAM;
#   - no swap on anything but /dev/zram* (a swapfile or partition on the stick wears it out).
cambox_zram_verdict() {
  local block="${1:-}" line fails="" nl=$'\n' fix
  local pkg="" sha="" mem="" done="" zram=0 name bytes want_sha
  local -a other=()
  while IFS= read -r line; do
    case "$line" in
      PKG=*) pkg="${line#*=}" ;;
      DROPIN_SHA=*) sha="${line#*=}" ;;
      MEM_KB=*) mem="${line#*=}" ;;
      GATHER_DONE=1) done=1 ;;
      SWAP=*)
        read -r name bytes <<<"${line#SWAP=}"
        case "$bytes" in '' | *[!0123456789]*) bytes=0 ;; esac
        case "$name" in
          /dev/zram*) zram=$((zram + bytes)) ;;
          *) other+=("$name") ;;
        esac
        ;;
    esac
  done <<<"$block"
  fix="fix: re-run setup-device.sh, or pipe the cambox_zram_apply_program of scripts/lib/cambox-zram.sh into 'ssh root@<box> bash -s' while no E2E holds the rig (issue 1428)"
  if [ "$done" != 1 ]; then
    printf 'FAIL: the zram swap state could not be read (no GATHER_DONE) -- %s\n' "$fix"
    return 0
  fi
  [ "$pkg" = "install ok installed" ] \
    || fails="${fails}FAIL: ${CAMBOX_ZRAM_PACKAGE} is not installed (dpkg status '${pkg:-<none>}') -- no zram swap at boot; ${fix}${nl}"
  want_sha="$(cambox_zram_dropin_sha)"
  case "$sha" in
    "$want_sha") : ;;
    __ABSENT__) fails="${fails}FAIL: the zram drop-in ${CAMBOX_ZRAM_DROPIN} is missing -- the package default gives min(ram/2, 4 GiB); ${fix}${nl}" ;;
    "") fails="${fails}FAIL: the zram drop-in ${CAMBOX_ZRAM_DROPIN} could not be read; ${fix}${nl}" ;;
    *) fails="${fails}FAIL: the zram drop-in ${CAMBOX_ZRAM_DROPIN} differs from scripts/lib/cambox-zram.sh (sha ${sha:0:12} != ${want_sha:0:12}); ${fix}${nl}" ;;
  esac
  case "$mem" in
    '' | *[!0123456789]*)
      fails="${fails}FAIL: MemTotal could not be read (MEM_KB='${mem}') -- the swap size cannot be graded${nl}"
      ;;
    *)
      local need=$((mem * 1024 / 100 * CAMBOX_ZRAM_MIN_PERCENT))
      if [ "$zram" -eq 0 ]; then
        fails="${fails}FAIL: no zram swap is active (swapon lists no /dev/zram*) -- it comes up at boot once the package + drop-in are in place, or start it with the apply program; ${fix}${nl}"
      elif [ "$zram" -lt "$need" ]; then
        fails="${fails}FAIL: the zram swap is ${zram} bytes, under ${CAMBOX_ZRAM_MIN_PERCENT}% of RAM (${need} bytes) -- the drop-in is not loaded yet (daemon-reload + a swap restart, or the next boot); ${fix}${nl}"
      fi
      ;;
  esac
  if [ "${#other[@]}" -gt 0 ]; then
    fails="${fails}FAIL: swap on ${other[*]} -- a cambox never swaps to its USB stick (it wears out); remove it from /etc/fstab and swapoff it${nl}"
  fi
  if [ -n "$fails" ]; then
    printf '%s' "$fails"
  else
    printf 'ok\n'
  fi
}

# cambox_zram_apply_program -> the REMOTE program (root, `ssh ... bash -s`) that brings a live
# cambox's zram swap up. Everything runs inside ONE function called with </dev/null: the program
# arrives on bash's stdin, so a command that read stdin (apt, dpkg) would eat the rest of it, the
# verified close included (.claude/rules/ro-window.md, the rt-kernel-plan finding).
#   1. read (nothing written): the root mode, whether the package is installed, whether the drop-in
#      matches;
#   2. the rw window, when something needs a write OR the root does not read `ro` (a cambox runs
#      read-only, so a writable root is the stuck state and is put back through the verified close
#      before anything starts): the apt install (a package with no maintainer scripts; its .deb lands
#      in the /var/cache tmpfs), the drop-in (temp file + rename), then the ONE verified close. A
#      failure inside reaches the EXIT trap, which runs the same close;
#   3. after the close: daemon-reload (the generator writes the swap unit), modprobe zram, a swap
#      restart when the drop-in changed under a running zram swap, start dev-zram0.swap, read back
#      swapon. Each failure is named; exit 0 only when every step held.
cambox_zram_apply_program() {
  cat <<'PROG'
# scripts/lib/cambox-zram.sh (issue 1428): the zram swap of this cambox.
_cz_main() {
set -eu
PROG
  printf '_cz_d=%q\n' "$CAMBOX_ZRAM_DROPIN"
  printf '_cz_body=%q\n' "$(cambox_zram_dropin_content)"
  printf '_cz_want=%q\n' "$(cambox_zram_dropin_sha)"
  printf '_cz_p=%q\n' "$CAMBOX_ZRAM_PACKAGE"
  printf '%s\n' "$(declare -f ro_root_mount_mode)"
  cat <<'PROG'
_cz_box="$(hostname 2>/dev/null || echo this cambox)"
_cz_sha() { if [ -e "$1" ]; then _cz_s="$(sha256sum <"$1")"; echo "${_cz_s%% *}"; else echo __ABSENT__; fi; }
# --- 1. what this box needs (read before anything is written) ---
_cz_mode="$(ro_root_mount_mode "$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)")"
_cz_do_pkg=0
[ "$(dpkg-query -W -f='${Status}' "$_cz_p" 2>/dev/null || true)" = "install ok installed" ] || _cz_do_pkg=1
_cz_do_d=0
[ "$(_cz_sha "$_cz_d")" = "$_cz_want" ] || _cz_do_d=1
echo "PLAN ($_cz_box): root $_cz_mode; package $([ "$_cz_do_pkg" = 1 ] && echo install || echo in-place); drop-in $([ "$_cz_do_d" = 1 ] && echo write || echo in-place)"
_cz_rc=0
# --- 2. the rw window, closed by the ONE verified close ---
_cz_open=0
_cz_close() {
  [ "$_cz_open" = 1 ] || return 0
  _cz_open=0
PROG
  ro_window_close_cmds "issue 1428" "\$_cz_box" \
    "Whatever this program wrote is on disk, but the root stays read-WRITE, so it starts no swap." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run the cambox zram apply for this box (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_cz_close' EXIT
if [ "$_cz_do_pkg" = 1 ] || [ "$_cz_do_d" = 1 ] || [ "$_cz_mode" != ro ]; then
  if ! mount -o remount,rw /; then
    echo "FAIL: [issue 1428] could not remount / read-write on $_cz_box -- nothing was written" >&2
    exit 1
  fi
  _cz_open=1
  if [ "$_cz_do_pkg" = 1 ]; then
    if ! DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
      -o Dpkg::Options::=--force-confold "$_cz_p"; then
      echo "FAIL: [issue 1428] apt-get install $_cz_p failed on $_cz_box -- the window is closed again by the verified close" >&2
      exit 1
    fi
  fi
  if [ "$_cz_do_d" = 1 ]; then
    if ! { mkdir -p "${_cz_d%/*}" && printf '%s\n' "$_cz_body" >"$_cz_d.new" && chmod 0644 "$_cz_d.new" && mv -f "$_cz_d.new" "$_cz_d"; }; then
      rm -f "$_cz_d.new"
      echo "FAIL: [issue 1428] could not write $_cz_d on $_cz_box -- the window is closed again by the verified close" >&2
      exit 1
    fi
  fi
fi
_cz_close
trap - EXIT
# --- 3. after the verified close: load, start, read back ---
if ! systemctl daemon-reload; then
  echo "FAIL: [issue 1428] systemctl daemon-reload failed on $_cz_box -- the zram swap unit may not exist yet" >&2
  _cz_rc=1
fi
if ! modprobe zram; then
  echo "FAIL: [issue 1428] modprobe zram failed on $_cz_box" >&2
  _cz_rc=1
fi
if [ "$_cz_do_d" = 1 ] && swapon --show=NAME --noheadings 2>/dev/null | grep -qx /dev/zram0; then
  systemctl stop dev-zram0.swap systemd-zram-setup@zram0.service || _cz_rc=1
fi
if systemctl start dev-zram0.swap; then
  echo "OK: dev-zram0.swap started on $_cz_box"
else
  echo "FAIL: [issue 1428] dev-zram0.swap did not start on $_cz_box -- read: journalctl -u systemd-zram-setup@zram0.service -n 20" >&2
  _cz_rc=1
fi
echo "SWAPS ($_cz_box):"
swapon --show=NAME,SIZE,USED,PRIO --bytes 2>/dev/null || true
return "$_cz_rc"
}
_cz_main </dev/null
PROG
}
