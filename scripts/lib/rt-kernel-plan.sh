#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (mirrors event-assert.sh /
# stale-artifact-guard.sh / camera-box-restart-verify.sh) -- pure decision/string builders only;
# `set -euo pipefail` must NEVER be set here because sourcing this file executes it in the CALLER's
# shell and would leak strict mode into whichever script sources it. Every function is pure: it
# reads only its args + emits to stdout, no side effects, no globals, no I/O.
#
# scripts/lib/rt-kernel-plan.sh -- issue 899: the low-latency kernel provisioning DECISION logic.
# Lane 1 (merged: src/affinity.rs capture-IRQ routing) fixed defect 3; defect 1 (the fleet runs a
# stock PREEMPT_DYNAMIC kernel with no full preemption) remains.
#
# OWNER DECISION (2026-08-20): Ubuntu Pro / subscription is REJECTED ("linux has its own free
# realtime compiles, I will not pay Ubuntu"). The plan is now:
#   STEP 1 (this lane) -- linux-lowlatency-hwe-24.04, the official Ubuntu MAIN-archive low-latency
#     meta (FREE, no subscription). On 24.04 it is a CONFIG meta: it depends on the generic HWE
#     image + the `lowlatency-kernel` config package, which drops
#     /etc/default/grub.d/99-lowlatency.cfg = GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full
#     rcu_nocbs=all" -- full preemption + a higher-HZ timer. Precedent: imag-nb already runs it
#     (setup-imag.sh step 7, issue 482). It gives most of the practical benefit for zero cost.
#   STEP 2 (only if a live measurement shows STEP 1 is not enough) -- a custom PREEMPT_RT kernel
#     built in CI (mainline >=6.12 has RT merged). Runbook/rule doc only in this lane, NOT code.
#
# preempt=full is NOT full PREEMPT_RT: it still does not thread hardirq/softirq handlers, so a
# hardirq can still preempt the prio-90 grab. That honest gap is why STEP 2 exists -- but STEP 1 is
# the free, low-risk first move, so it ships first and is measured (docs/runbooks/899-...).
#
# Applying STEP 1 is a reboot-class per-box change on a `ro`-root appliance with the issue-295/547
# brick-hardening + the single-kernel invariant (verify-device.sh check `(k)`), so it is the
# SUPERVISOR's coordinated deploy step (docs/runbooks/899-realtime-isolation.md), NEVER done from a
# code lane. This library only PLANS + describes the sequence; scripts/rt-kernel-upgrade.sh prints
# it per box (dry-run, read-only). Reboot/apply is unverified here by design.
#
# Pure functions (source + call directly; the twin is tests/rt_kernel_provision.rs, run_sourced):
#   rt_kernel_flavour                    -> the decided kernel package (single source of truth)
#   rt_kernel_readiness_verdict RUN CAND
#   rt_kernel_upgrade_plan RUN INST GEN GRUBDEF [CAND]
#   rt_kernel_step_command TOKEN

# The one decided kernel flavour -- referenced by every other function + the driver + the runbook.
# STEP 1: the free official-archive low-latency meta (no Ubuntu Pro), matching the imag-nb precedent.
rt_kernel_flavour() { printf 'linux-lowlatency-hwe-24.04'; }

# _rt_truthy VALUE -> exit 0 iff VALUE is a truthy token (1/yes/true), else exit 1. Set-e-safe:
# used only as an `if` condition, never bare.
_rt_truthy() {
  case "${1:-}" in
    1|y|yes|true|Y|YES|TRUE) return 0 ;;
    *) return 1 ;;
  esac
}

# rt_kernel_readiness_verdict RUNNING_LOWLAT LOWLAT_INSTALLED CANDIDATE_PRESENT -> one verdict token.
# Describes whether a box CAN be upgraded to the low-latency profile right now. There is NO Ubuntu
# Pro axis any more (the free main-archive package needs no subscription). The verdict is kept
# CONSISTENT with rt_kernel_upgrade_plan's own blocked condition (`!inst && !cand`) by taking the
# INSTALLED axis too: a box that already has the config package installed is `ready` to finish the
# upgrade even if the apt candidate has since aged out -- so the readiness header can NEVER read
# `no-rt-candidate` while the plan below correctly proceeds because it is already installed.
#   already-lowlatency : the box already runs the low-latency profile (preempt=full active)
#   ready              : the config package is installed OR a linux-lowlatency-hwe-24.04 candidate is
#                        apt-resolvable (free, no Pro)
#   no-rt-candidate    : NOT installed AND no apt candidate (fail-closed -- kept from the pro-attach
#                        design for the genuinely-missing-package case)
rt_kernel_readiness_verdict() {
  local run="${1:-}" inst="${2:-}" cand="${3:-}"
  if _rt_truthy "$run"; then printf 'already-lowlatency'; return 0; fi
  if ! _rt_truthy "$inst" && ! _rt_truthy "$cand"; then printf 'no-rt-candidate'; return 0; fi
  printf 'ready'
}

# rt_kernel_upgrade_plan RUNNING_LOWLAT LOWLAT_INSTALLED SUPERSEDED_GENERIC GRUB_DEFAULT [RT_CANDIDATE]
# -> the ORDERED atomic per-box plan, one token per line, OR a single noop:/blocked: token.
#
# Axes (all 1/0 except GRUB_DEFAULT which is `saved` or a number):
#   RUNNING_LOWLAT      : preempt=full is already the ACTIVE boot mode (nothing to do).
#   LOWLAT_INSTALLED    : the `lowlatency-kernel` config package is already installed (skip install).
#   SUPERSEDED_GENERIC  : the install will pull a NEW generic HWE image alongside the running one, so
#                         after reboot the OLD image is superseded and must be purged to restore the
#                         single-kernel invariant. True on today's cam fleet (the HWE generic meta is
#                         NOT installed -> a new HWE image comes in); FALSE on an imag-like box that
#                         already tracks the HWE meta (config-only install, no new image, no purge).
#   RT_CANDIDATE (5th, default 1 = present): whether linux-lowlatency-hwe-24.04 is apt-resolvable.
#                         Not installed AND no candidate -> `blocked:no-rt-candidate` (the fail-closed
#                         shape kept from the pro-attach design), so the plan agrees with readiness.
#
# The order is the SAFE atomic sequence: install the config meta (which drops preempt=full) + pin
# GRUB to the new image + regen (initrd-guaranteed, #295) + reboot INTO it, CONFIRM preempt=full is
# active, and only THEN purge the now-superseded old generic image and re-check the single-kernel
# invariant. Never purge the kernel you are still running. Per-box GRUB drift is honoured: `saved`
# boxes pin via grub-set-default, a numeric GRUB_DEFAULT pins the new menuentry.
rt_kernel_upgrade_plan() {
  local run="${1:-}" inst="${2:-}" gen="${3:-}" grubdef="${4:-}" cand="${5:-1}"
  if _rt_truthy "$run"; then printf 'noop:already-lowlatency\n'; return 0; fi
  if ! _rt_truthy "$inst" && ! _rt_truthy "$cand"; then
    printf 'blocked:no-rt-candidate\n'; return 0
  fi
  if ! _rt_truthy "$inst"; then printf 'install-lowlatency\n'; fi
  printf 'verify-lowlatency-config\n'
  if [ "$grubdef" = "saved" ]; then printf 'grub-pin:saved\n'; else printf 'grub-pin:menuentry\n'; fi
  printf 'safe-grub-regen\n'
  printf 'reboot-into-lowlatency\n'
  printf 'confirm-running-lowlatency\n'
  if _rt_truthy "$gen"; then printf 'purge-superseded-generic\n'; fi
  printf 'verify-single-kernel\n'
  printf 'post-verify\n'
}

# rt_kernel_step_command TOKEN -> the concrete shell the SUPERVISOR runs for one plan token, or a
# `# SUPERVISOR:` note for the reboot-class / post-reboot gates. Mutating commands wrap the `ro`
# root remount themselves so each token is self-contained and copy-pasteable. `unknown-token` for
# anything unrecognised (fail-loud, never a silent empty command).
rt_kernel_step_command() {
  case "${1:-}" in
    install-lowlatency)
      # --allow-change-held-packages: the cam boxes hold their kernel packages (issue 295/487),
      # and the lowlatency meta depends on the HWE packages, so the install must be allowed to move
      # the hold -- exactly as setup-imag.sh step 7 does (#820). This ADDS the lowlatency config
      # (preempt=full) and, on a box without the HWE generic meta, a new generic HWE image.
      printf 'mount -o remount,rw / && apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-change-held-packages linux-lowlatency-hwe-24.04 && mount -o remount,ro /' ;;
    verify-lowlatency-config)
      printf '# SUPERVISOR: assert the config package landed -- test -f /etc/default/grub.d/99-lowlatency.cfg AND grep -q preempt=full /etc/default/grub.d/99-lowlatency.cfg (refuse to trust it otherwise, mirrors setup-imag.sh step 7)' ;;
    grub-pin:saved)
      printf '# SUPERVISOR: GRUB_DEFAULT=saved box (cam2/cam3) -- pick the NEW HWE generic entry id in /boot/grub/grub.cfg (the newest kernel version), then: mount -o remount,rw / && grub-set-default "<Advanced...>the new kernel>" && mount -o remount,ro /' ;;
    grub-pin:menuentry)
      printf '# SUPERVISOR: numeric GRUB_DEFAULT box (cam1: =0) -- entry 0 is grub-sorted newest-first, so update-grub makes the new HWE image default; confirm GRUB_DEFAULT still points at it in /etc/default/grub' ;;
    safe-grub-regen)
      # The #295 safe pattern: guarantee every installed kernel has an initrd BEFORE update-grub,
      # then update-grub once (the preempt=full grub.d drop applies to every entry).
      printf 'mount -o remount,rw / && for v in /boot/vmlinuz-*; do k="${v#/boot/vmlinuz-}"; [ -e "/boot/initrd.img-$k" ] || update-initramfs -c -k "$k"; done && update-grub && mount -o remount,ro /   # #295: initrd-guarantee before grub' ;;
    reboot-into-lowlatency)
      printf '# SUPERVISOR: reboot the box (reboot-class, one box at a time, in a window with NO live E2E; the old generic entry stays in GRUB as rollback)' ;;
    confirm-running-lowlatency)
      printf '# SUPERVISOR: after reboot, confirm preempt=full is ACTIVE: grep -qw preempt=full /proc/cmdline AND (cat /sys/kernel/debug/sched/preempt shows "(full)"); uname -r is still *-generic (the config meta keeps the generic image), NOT *-lowlatency' ;;
    purge-superseded-generic)
      # NEVER a wildcard generic purge -- the NEW running kernel is ALSO a generic image, so a glob
      # would remove the kernel the box is running. Purge only the SPECIFIC pre-upgrade version,
      # recorded before the upgrade.
      printf '# SUPERVISOR: restore single-kernel (check (k)) -- purge ONLY the specific pre-upgrade image (the old uname -r noted before the upgrade), e.g.: mount -o remount,rw / && apt-get purge -y --allow-change-held-packages "linux-image-<OLD_VER>" "linux-modules-<OLD_VER>" && mount -o remount,ro / . NEVER a wildcard generic purge (that removes the new running kernel).' ;;
    verify-single-kernel)
      printf '# SUPERVISOR: re-run verify-device.sh -- check (k) single-kernel invariant is restored; check (ac) still WARNs "not PREEMPT_RT" (EXPECTED -- preempt=full is STEP 1, full RT is STEP 2)' ;;
    post-verify)
      printf '# SUPERVISOR: run the full verify-device.sh acceptance gate + a full E2E, AND the before/after 10-min emit-jitter+underrun measurement (docs/runbooks/899-realtime-isolation.md) to decide whether STEP 2 (custom PREEMPT_RT) is needed' ;;
    noop:already-lowlatency)
      printf '# nothing to do -- box already runs the low-latency profile (preempt=full active)' ;;
    blocked:no-rt-candidate)
      printf '# BLOCKED: linux-lowlatency-hwe-24.04 has no apt candidate -- run: mount -o remount,rw / && apt-get update && mount -o remount,ro /, confirm the main archive is reachable, then re-plan' ;;
    *)
      printf 'unknown-token' ;;
  esac
}
