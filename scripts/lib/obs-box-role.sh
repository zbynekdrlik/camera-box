#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the per-box step gate, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into
# it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-role.sh -- issue 1412: which setup-strih steps / verify-strih items run on the
# LOADED box. ONE provisioner serves every Linux OBS box (the unified-design ruling, umbrella issue
# 1357), so a step that does not apply to a box is skipped by a box FACT here, never by a box name and
# never by a copy of the script. setup-strih.sh and verify-strih.sh each carry one gate line per step:
#
#   if WHY="$(obs_box_step_applies <step>)"; then <the step> else <a SKIP line naming WHY>; fi
#
# The facts come from scripts/strih-boxes/<box>.env through scripts/lib/strih-box-facts.sh.

# obs_box_step_applies STEP -> rc 0 when the loaded box runs STEP; rc 1 with the reason on stdout when
# it skips it. A step with no rule here always runs.
#   11b -- the NIC IRQ unit resolves the rig NIC's XHCI interrupt, so it exists only for a USB rig NIC
#          (STRIH_NIC_MIN_USB_MBPS != none). On a PCIe NIC (strih-pp's RTL8125) the walk to a usbN root
#          would fail the unit at every boot (gap G7).
#   16e -- the YouTube-guard stream program-audio sampler (issue 1404) reads the SNV stream output, so
#          only a box with STRIH_PROGRAM_AUDIO_SAMPLER=on runs it (gap G11).
obs_box_step_applies() {
  local step="${1:?step required}" v
  case "$step" in
    11b)
      v="$(strih_lx_nic_min_usb_mbps)" || return 0
      if [ "$v" = none ]; then
        printf 'the rig NIC is not a USB device (STRIH_NIC_MIN_USB_MBPS=none) -- no xhci IRQ to place'
        return 1
      fi ;;
    16e)
      v="$(strih_lx_program_audio_sampler)" || return 0
      if [ "$v" != on ]; then
        printf 'STRIH_PROGRAM_AUDIO_SAMPLER=%s -- this box runs no stream program-audio sampler' "$v"
        return 1
      fi ;;
  esac
  return 0
}
