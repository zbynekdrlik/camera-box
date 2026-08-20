#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (mirrors event-assert.sh /
# stale-artifact-guard.sh / camera-box-restart-verify.sh) -- pure decision/string builders only;
# `set -euo pipefail` must NEVER be set here because sourcing this file executes it in the CALLER's
# shell and would leak strict mode into whichever script sources it. Every function is pure: it
# reads only its args + emits to stdout, no side effects, no globals, no I/O.
#
# scripts/lib/rt-kernel-plan.sh -- issue 899 (lane 2): the PREEMPT_RT kernel provisioning DECISION
# logic. Lane 1 (merged: src/affinity.rs capture-IRQ routing) fixed defect 3; defect 1 (the fleet
# runs a stock PREEMPT_DYNAMIC kernel, not PREEMPT_RT) remains. The chosen kernel is
# linux-image-realtime (Ubuntu 24.04 realtime kernel under Ubuntu Pro): it threads hardirq/softirq
# handlers so the xhci IRQ becomes a schedulable kthread below the prio-90 grab -- exactly what the
# issue-289 isolation design already assumes and the merged RT-conditional affinity code flips to.
#
# Applying it is a reboot-class per-box change on a `ro`-root appliance with the issue-295/547
# brick-hardening + the single-kernel invariant (verify-device.sh check `(k)`), so it is the
# SUPERVISOR's coordinated deploy step (docs/runbooks/899-realtime-isolation.md), NEVER done from a
# code lane. This library only PLANS + describes the sequence; scripts/rt-kernel-upgrade.sh prints
# it per box (dry-run, read-only). Reboot/apply is unverified here by design.
#
# Pure functions (source + call directly; the twin is tests/rt_kernel_provision.rs, run_sourced):
#   rt_kernel_flavour                    -> the decided kernel package (single source of truth)
#   rt_kernel_readiness_verdict RUN CAND PRO
#   rt_kernel_upgrade_plan RUN INST PRO GEN GRUBDEF
#   rt_kernel_step_command TOKEN

# The one decided kernel flavour -- referenced by every other function + the driver + the runbook.
rt_kernel_flavour() { printf 'linux-image-realtime'; }

# _rt_truthy VALUE -> exit 0 iff VALUE is a truthy token (1/yes/true), else exit 1. Set-e-safe:
# used only as an `if` condition, never bare.
_rt_truthy() {
  case "${1:-}" in
    1|y|yes|true|Y|YES|TRUE) return 0 ;;
    *) return 1 ;;
  esac
}

# rt_kernel_readiness_verdict RUNNING_IS_RT CANDIDATE_PRESENT PRO_ATTACHED -> one verdict token.
# Describes whether a box CAN be upgraded to the RT kernel right now:
#   already-realtime  : the box already runs a PREEMPT_RT kernel (nothing to do)
#   ready             : RT package candidate is available AND Ubuntu Pro is attached
#   needs-pro-attach  : candidate available but Pro not attached (`pro attach` first)
#   no-rt-candidate   : the linux-image-realtime package is not resolvable from apt at all
rt_kernel_readiness_verdict() {
  local run="${1:-}" cand="${2:-}" pro="${3:-}"
  if _rt_truthy "$run"; then printf 'already-realtime'; return 0; fi
  if ! _rt_truthy "$cand"; then printf 'no-rt-candidate'; return 0; fi
  if _rt_truthy "$pro"; then printf 'ready'; else printf 'needs-pro-attach'; fi
}

# rt_kernel_upgrade_plan RUNNING_IS_RT RT_INSTALLED PRO_ATTACHED GENERIC_PRESENT GRUB_DEFAULT
# -> the ORDERED atomic per-box plan, one token per line, OR a single noop:/blocked: token.
#
# The order is the SAFE atomic sequence (an improvement over the merged runbook prose, which
# purged the generic kernel BEFORE rebooting into RT -- removing the running kernel's own modules):
# install + pin + reboot INTO RT first, CONFIRM it is running, and only THEN purge the now-unused
# generic kernel and re-check the single-kernel invariant. Per-box GRUB drift is honoured:
# `saved` boxes pin via grub-set-default, a numeric GRUB_DEFAULT pins the RT menuentry.
rt_kernel_upgrade_plan() {
  local run="${1:-}" inst="${2:-}" pro="${3:-}" gen="${4:-}" grubdef="${5:-}"
  if _rt_truthy "$run"; then printf 'noop:already-realtime\n'; return 0; fi
  if ! _rt_truthy "$inst" && ! _rt_truthy "$pro"; then
    printf 'blocked:need-pro-attach\n'; return 0
  fi
  if ! _rt_truthy "$inst"; then printf 'install-rt-kernel\n'; fi
  printf 'verify-rt-initrd\n'
  if [ "$grubdef" = "saved" ]; then printf 'grub-pin:saved\n'; else printf 'grub-pin:menuentry\n'; fi
  printf 'update-grub\n'
  printf 'reboot-into-rt\n'
  printf 'confirm-running-realtime\n'
  if _rt_truthy "$gen"; then printf 'purge-generic\n'; fi
  printf 'verify-single-kernel\n'
  printf 'post-verify\n'
}

# rt_kernel_step_command TOKEN -> the concrete shell the SUPERVISOR runs for one plan token, or a
# `# SUPERVISOR:` note for the reboot-class / post-reboot gates. Mutating commands wrap the `ro`
# root remount themselves so each token is self-contained and copy-pasteable. `unknown-token` for
# anything unrecognised (fail-loud, never a silent empty command).
rt_kernel_step_command() {
  case "${1:-}" in
    install-rt-kernel)
      printf 'mount -o remount,rw / && apt-get update && apt-get install -y linux-image-realtime && mount -o remount,ro /' ;;
    verify-rt-initrd)
      printf 'ls -l /boot/initrd.img-*realtime*   # issue 295/547 brick-hardening: the RT entry MUST have an initrd before it is pinned' ;;
    grub-pin:saved)
      printf '# SUPERVISOR: GRUB_DEFAULT=saved box (cam2/cam3) -- find the RT menuentry id in /boot/grub/grub.cfg (grep realtime), then: mount -o remount,rw / && grub-set-default "<Advanced...>realtime id>" && update-grub && mount -o remount,ro /' ;;
    grub-pin:menuentry)
      printf '# SUPERVISOR: numeric GRUB_DEFAULT box (cam1: =0) -- set GRUB_DEFAULT to the RT submenu entry in /etc/default/grub (e.g. "Advanced options for Ubuntu>...realtime"), then: mount -o remount,rw / && update-grub && mount -o remount,ro /' ;;
    update-grub)
      printf 'mount -o remount,rw / && update-grub && mount -o remount,ro /' ;;
    reboot-into-rt)
      printf '# SUPERVISOR: reboot the box (reboot-class, one box at a time, generic entry stays as rollback)' ;;
    confirm-running-realtime)
      printf '# SUPERVISOR: after reboot, confirm: uname -r shows *-realtime AND /proc/version has PREEMPT_RT AND /sys/kernel/realtime=1' ;;
    purge-generic)
      printf 'mount -o remount,rw / && apt-get purge -y "linux-image-*generic" "linux-headers-*generic" && mount -o remount,ro /   # safe: box is now running RT; restores the single-kernel invariant' ;;
    verify-single-kernel)
      printf '# SUPERVISOR: re-run verify-device.sh -- check (k) single-kernel invariant + check (ac) must now read kernel is PREEMPT_RT' ;;
    post-verify)
      printf '# SUPERVISOR: run the full verify-device.sh acceptance gate + a full E2E; confirm an SSH login no longer perturbs capture (the issue-728 symptom)' ;;
    noop:already-realtime)
      printf '# nothing to do -- box already runs a PREEMPT_RT kernel' ;;
    blocked:need-pro-attach)
      printf '# BLOCKED: attach Ubuntu Pro first (pro attach <token> && pro enable realtime-kernel), then re-plan' ;;
    *)
      printf 'unknown-token' ;;
  esac
}
