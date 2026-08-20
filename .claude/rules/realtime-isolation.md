---
paths:
  - "src/affinity.rs"
  - "scripts/lib/rt-kernel-plan.sh"
  - "scripts/rt-kernel-upgrade.sh"
  - "tests/rt_kernel_provision.rs"
  - "docs/runbooks/899-realtime-isolation.md"
---

# Realtime isolation on the cam fleet (issue 899)

The cam-box fleet's realtime isolation is ONE coherent subsystem, split across a merged code layer
and a staged reboot-class kernel layer. Read this before touching affinity/IRQ/kernel work.

## What is DONE (merged, live on the fleet)

- **`src/affinity.rs`** — `kernel_is_preempt_rt()` + `select_irq_target_cores(is_rt, capture_core,
  online)`. `setup_irq_affinity()` routes the xhci capture IRQ **off** the isolated grab core on a
  stock (non-RT) kernel, and only co-locates it on the grab core on a real PREEMPT_RT kernel. This
  is RT-conditional and forward-compatible: it flips automatically once the fleet is on an RT
  kernel. Verified live (all cam boxes): xhci IRQ 125 `smp_affinity_list=0-2` (off the grab core 3).
- **`verify-device.sh` check `(ac)`** — WARN-only, surfaces the PREEMPT_RT status + whether the
  capture IRQ is off the grab core. Deliberately never red-fails the current non-RT fleet; the flip
  to a hard FAIL is a documented follow-up gated on the RT redeploy.

## What is STAGED (reboot-class — SUPERVISOR only, never from a code lane)

- **Kernel choice DECIDED: PREEMPT_RT (`linux-image-realtime`)** — the only option that threads
  hardirq/softirq handlers so the isolation design (issue 289/303) is honest as written.
  `linux-image-lowlatency` is CONFIG_PREEMPT (not RT), does NOT thread hardirqs → rejected.
- **`scripts/lib/rt-kernel-plan.sh`** (pure, `run_sourced`-tested) + **`scripts/rt-kernel-upgrade.sh`**
  (DRY-RUN driver, read-only ssh, `--box`/`--facts`/`--commands`, **NO apply mode** — enable-only:
  the code PLANS, the supervisor APPLIES). Full runbook: `docs/runbooks/899-realtime-isolation.md`.
- **SAFE atomic order** (an improvement the planner enforces): install → verify initrd → grub-pin →
  update-grub → **reboot INTO rt → confirm running rt → THEN purge generic** → verify check `(k)`.
  Never purge the generic kernel while still running it (strips its own modules).
- **Blockers/drift the planner reads live (as of 2026-08-20):** Ubuntu Pro is NOT attached → every
  box plans `blocked:need-pro-attach` (`pro attach` + `pro enable realtime-kernel` first);
  `GRUB_DEFAULT=0` on cam1 vs `saved` on cam2/cam3 (→ `grub-pin:menuentry` vs `grub-pin:saved`);
  cam2 additionally has `linux-image-generic` meta (cam1/cam3 do not); root is `ro`; a second kernel
  violates the single-kernel invariant (verify check `(k)`, issue 547) until generic is purged.

## Gotchas

- The RT-kernel install is reboot-class on `ro`-root appliances with the issue-295/547
  brick-hardening — one box at a time, canary first, generic stays pinned in GRUB as rollback until
  RT is proven. This is a supervisor deploy step, verified UNVERIFIED from a worktree code lane.
- Defect 2 (the process-wide `CPUSchedulingPolicy=fifo` puts all threads FIFO-50 on the grab core,
  not `SCHED_OTHER`) is runbook **Step B** — a stock per-thread rework that needs a live emit
  measurement (#728-style), so it is NOT merged blind; the unit comment is already corrected.
