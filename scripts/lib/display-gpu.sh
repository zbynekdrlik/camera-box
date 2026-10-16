#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure gather/verdict builders, no top-level statements) --
# deliberately NOT `set -euo pipefail`: sourcing runs this file in the CALLER's shell, so strict mode
# here would leak into every caller (the scripts/lib convention, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/display-gpu.sh -- does the cambox's display GPU have a kernel driver? (issue 1446)
#
# Owner 16.10.2026: "na cam5 pripojeny operator monitor neukazuje nic preco!!! ma vsetko byt
# failproof a stale fungovat". cam5/cam6/cam7 carry the N150 (Twin Lake) GPU 8086:46d4; the fleet
# kernel 6.8.0-142's i915 lists only 46D0-46D2, so that GPU had NO driver: no /dev/dri, no
# /sys/class/drm. The cameraman HDMI preview (camera_box::ndi_display) then lived only on the
# firmware framebuffer, which the UEFI firmware sets up only when a monitor is connected and on at
# POST -- a monitor plugged in later never lights. With a driver bound (i915 on 46d1, cam1-4), the
# framebuffer follows hotplug. The HWE kernels (6.11 lowlatency, 7.0 generic) list 46D3/46D4.
#
# A healthy box is one whose every PCI display controller has a driver bound. /dev/fb0 is NOT the
# signal: a driven box with no monitor attached has no fb0 (connectors `disconnected`), and an
# undriven box whose monitor was on at boot has one.
#
# Consumers:
#   - verify-device.sh (av): display_gpu_gather_remote_snippet over the existing ssh, then
#     display_gpu_verdict on dev1. HARD FAIL.
# The remedy a FAIL names is the HWE kernel through scripts/rt-kernel-upgrade.sh (the issue-899
# STEP 1 planner, .claude/rules/realtime-isolation.md), then a PHYSICAL power cycle of the box.
# Tests: tests/python/test_display_gpu_1446.py.

# display_gpu_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh (av).
# Read-only. Prints:
#   kernel <uname -r>
#   <pci slot> <vendor>:<device> <driver name, or - when none is bound>   (one per class 0x03* device)
#   end
# The closing `end` row tells a complete read from a cut one. A PCI root that is not a readable
# directory prints `root-unreadable <path>` instead of the device rows. DISPLAY_GPU_PCI_ROOT (default
# /sys/bus/pci/devices) points the gather at a test tree.
display_gpu_gather_remote_snippet() {
  printf '_dg_root=%q\n' "${DISPLAY_GPU_PCI_ROOT:-/sys/bus/pci/devices}"
  cat <<'REMOTE'
echo "kernel $(uname -r 2>/dev/null || true)"
if [ ! -d "$_dg_root" ] || [ ! -r "$_dg_root" ] || [ ! -x "$_dg_root" ]; then echo "root-unreadable $_dg_root"; fi
for _dg_d in "$_dg_root"/*; do
  [ -r "$_dg_d/class" ] || continue
  _dg_c=""; read -r _dg_c <"$_dg_d/class" || true
  case "$_dg_c" in 0x03*) ;; *) continue ;; esac
  _dg_v=""; read -r _dg_v <"$_dg_d/vendor" || true
  _dg_i=""; read -r _dg_i <"$_dg_d/device" || true
  _dg_drv=-
  if [ -L "$_dg_d/driver" ]; then
    _dg_drv="$(readlink "$_dg_d/driver" 2>/dev/null || true)"
    _dg_drv="${_dg_drv##*/}"
    [ -n "$_dg_drv" ] || _dg_drv=-
  fi
  echo "${_dg_d##*/} ${_dg_v#0x}:${_dg_i#0x} ${_dg_drv}"
done
echo end
REMOTE
}

# display_gpu_verdict BLOCK -> prints `OK: <slot> <id> <driver>[; ...] on kernel <k>` and returns 0,
# or one `FAIL: ...` line per problem and returns 1. BLOCK is the gather's output (or a test
# fixture). Fail-closed: a read whose last row is not `end`, a missing kernel, an unreadable PCI
# root, a malformed row and a box with no display controller at all are FAILs, never OK. Pure bash,
# ASCII-only under any locale (the hex set is spelled out, never a bracket range:
# .claude/rules/ci-testing-gotchas.md, issue 1302).
display_gpu_verdict() {
  local block="${1:-}" line kernel="" have_kernel=0 complete=0 n=0 fails="" driven="" nl=$'\n'
  local root_unreadable=""
  local slot id drv extra remedy
  local hex='0123456789abcdef'
  while IFS= read -r line; do
    [ -n "${line// /}" ] || continue
    if [ "$complete" = 1 ]; then
      # anything after `end` means the read is not what the gather prints
      fails="${fails}FAIL: malformed display controller row '${line}' after the end row -- the PCI display state cannot be graded (issue 1446)${nl}"
      continue
    fi
    case "$line" in
      kernel | "kernel "*)
        have_kernel=1
        kernel="${line#kernel}"
        kernel="${kernel# }"
        ;;
      end) complete=1 ;;
      "root-unreadable "*) root_unreadable="${line#root-unreadable }" ;;
      *)
        slot="" id="" drv="" extra=""
        read -r slot id drv extra <<<"$line"
        if [ -n "$extra" ] || [ -z "$drv" ] || ! [[ $id =~ ^[$hex]{4}:[$hex]{4}$ ]] \
          || ! [[ $slot =~ ^[${hex}:.]+$ ]]; then
          fails="${fails}FAIL: malformed display controller row '${line}' -- the PCI display state cannot be graded (issue 1446)${nl}"
          continue
        fi
        n=$((n + 1))
        if [ "$drv" = "-" ]; then
          fails="${fails}__UNDRIVEN__ ${slot} ${id}${nl}"
        else
          driven="${driven:+${driven}; }${slot} ${id} ${drv}"
        fi
        ;;
    esac
  done <<<"$block"
  if [ "$complete" != 1 ]; then
    printf 'FAIL: the PCI display controllers could not be read (a cut or empty read, no end row) -- cannot certify the HDMI preview has a GPU driver (issue 1446)\n'
    return 1
  fi
  if [ "$have_kernel" != 1 ] || [ -z "$kernel" ]; then
    fails="FAIL: the running kernel could not be read (no kernel row) -- the display GPU cannot be graded (issue 1446)${nl}${fails}"
    kernel="<unread>"
  fi
  if [ -n "$root_unreadable" ]; then
    fails="${fails}FAIL: the PCI device directory ${root_unreadable} is not readable -- the display GPU cannot be graded (issue 1446)${nl}"
  fi
  remedy="boot a kernel whose driver knows it: the Ubuntu 24.04 HWE kernel through scripts/rt-kernel-upgrade.sh (.claude/rules/realtime-isolation.md), then a PHYSICAL power cycle of the box (never a remote reboot of a cambox)"
  if [ "$n" -eq 0 ] && [ -z "$fails" ]; then
    fails="FAIL: no PCI display controller (class 0x03*) found on kernel ${kernel} -- every cambox has one, so the HDMI preview has nothing to drive (issue 1446)${nl}"
  fi
  if [ -z "$fails" ]; then
    printf 'OK: %s on kernel %s\n' "$driven" "$kernel"
    return 0
  fi
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      "__UNDRIVEN__ "*)
        read -r slot id <<<"${line#__UNDRIVEN__ }"
        printf 'FAIL: display controller %s (%s) has no kernel driver bound on kernel %s -- the HDMI preview then lives only on the firmware framebuffer, which exists only when a monitor was on at power-on; %s (issue 1446)\n' \
          "$slot" "$id" "$kernel" "$remedy"
        ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <<<"$fails"
  return 1
}
