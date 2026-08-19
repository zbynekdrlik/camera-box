# Runbook — realtime isolation (issue 899)

**Status: STAGED, not applied.** The autopilot worker for issue 899 ran in a worktree lane and
must not apply reboot-class or live-RT-model changes to the fleet (an E2E may be running on the
rig; a fleet reboot / kernel swap / scheduling-model change is the supervisor's coordinated step).
This runbook is the hand-off for those live steps. The **code/config-stageable** part (the
capture-IRQ fix + the honest service comment + the verify-device.sh lock) is already in the branch.

## Background — the three defects (re-validated live on cam1, 2026-08-18)

The fleet's realtime setup was only half-built. Measured on cam1 (10.77.9.61, N150-class, 4 cores,
`isolcpus=3`):

1. **Kernel is not PREEMPT_RT.** `uname -r` = `6.8.0-134-generic`, `/proc/version` =
   `PREEMPT_DYNAMIC` (voluntary), `/sys/kernel/realtime` absent. SCHED_FIFO orders our own threads,
   but hardirq/softirq handlers are NOT threaded, so an interrupt preempts even the prio-90 grab.
2. **The reserved core is shared by many FIFO threads; the service comment was false.**
   `CPUSchedulingPolicy=fifo` in the unit applies to the WHOLE process, so all 27 threads on core 3
   inherit FIFO 50 — not `SCHED_OTHER` as the old comment claimed. The grab is FIFO 90 (siblings
   can't preempt it), but the comment documented a state the fleet has never been in.
3. **The xhci capture IRQ shares the grab core.** IRQ 125 → core 3, ~2.9 billion non-preemptible
   interrupts. `ExecStartPre=... --setup-irq-affinity` (from `systemd/camera-box.service`, baked
   into the USB image by `build-image.sh`) actively routes it there.

These interact — the fix is one coherent decision, not three independent patches.

## What this branch already does (code/config, no live change)

- **Defect 3 fixed in code.** `src/affinity.rs` gains `kernel_is_preempt_rt()` and
  `select_irq_target_cores(is_rt, capture_core, online)`. `setup_irq_affinity()` now routes the
  capture IRQ **off** the grab core (onto the general cores) on a stock kernel, and only co-locates
  it on the isolated core on a real PREEMPT_RT kernel. This is strictly better on today's non-RT
  fleet AND forward-compatible: if the fleet is later upgraded to PREEMPT_RT (below), the same code
  automatically flips to the co-located routing #289 intended. **Takes effect on the next fleet
  redeploy** (the ExecStartPre invocation runs the new binary) — a supervisor deploy step, not a
  reboot.
- **Defect 2's false comment corrected.** `systemd/camera-box.service` now states the honest state
  (whole-process FIFO 50; grab raised to 90; a hardirq still preempts on non-RT) and points here.
- **verify-device.sh surfaces the drift** (check `(ac)`, WARN-only for now): it reports the
  PREEMPT_RT status (defect 1 — informational, the fleet is not RT yet) and whether the xhci capture
  IRQ is routed OFF the isolated grab core on a stock kernel (defect 3). A box still running the
  pre-899 binary WARNs (`capture IRQ … is on the isolated grab core … — redeploy the issue-899
  binary`); a redeployed box reads `ok-off-grab`. It is WARN-only deliberately so it never
  red-fails the current fleet's acceptance gate before the coordinated redeploy — the flip to a
  hard FAIL is a documented follow-up gated on that redeploy (mirroring the repo's established
  report-only→blocking seam pattern). On a PREEMPT_RT kernel it instead expects the IRQ co-located
  on the grab core (`ok-on-grab`).

## STAGED step A — PREEMPT_RT kernel, fleet-wide (reboot-class — supervisor only)

This is the path that makes the FIFO priorities AND the IRQ-on-core-3 routing mean what #289
already assumes. It is a real piece of work, not a package install:

1. **Ubuntu Pro.** Ubuntu 24.04 ships the realtime kernel as `linux-image-realtime` under Ubuntu
   Pro (`pro attach <token>` + `pro enable realtime-kernel`). Confirm the fleet's Pro entitlement
   first; without it there is no supported RT kernel.
2. **The single-kernel invariant.** `verify-device.sh` check `(k)` (#547) enforces EXACTLY ONE
   installed kernel equal to the running one, and the appliances are `ro`-root with the #295/#547
   brick-hardening (GRUB pinned to a known-good initrd-bearing entry). Installing a second kernel
   violates check `(k)` until the OLD generic kernel is purged and GRUB re-pinned to the RT entry.
   Do this atomically per box: install RT kernel → `update-grub` with the RT entry pinned as
   `GRUB_DEFAULT=saved` → verify the initrd exists for it (the #295 guard) → purge the generic
   kernel → re-run `verify-device.sh` check `(k)`.
3. **The cmdline is unchanged** — `isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2` still apply
   (they are orthogonal to the preemption model). On the RT kernel the ExecStartPre will co-locate
   the xhci IRQ on core 3 automatically (the code above), which is now defensible.
4. **Canary first, then fleet.** Upgrade ONE cam box, reboot, run `verify-device.sh` (all checks
   incl. `(r)`), then a full E2E, and confirm the grab thread's isolation is honest (an SSH login
   no longer perturbs capture — the original #728 symptom). Only then roll the rest, ONE box at a
   time, each behind its own reboot + verify.
5. **Rollback:** the pinned known-good generic kernel entry stays in GRUB until the RT kernel is
   proven on a box; if a box misbehaves, `grub-reboot` the generic entry and reboot.

**Do NOT apply step A from a code lane.** It is a coordinated fleet reboot; the worker's branch
carries only the forward-compatible code that reads correctly on both kernels.

## STAGED step B — honest per-thread stock policy (needs a live A/V/emit measurement)

If the fleet stays on the stock kernel, defect 2 can be made honest instead of merely documented:

- Drop the process-wide `CPUSchedulingPolicy=fifo` / `CPUSchedulingPriority=50` from the unit so
  non-re-pinned threads start `SCHED_OTHER`.
- Set `CPUAffinity=0-2` (general cores) instead of `3`, and have the binary pin ONLY the grab
  thread onto core 3 at FIFO 90 — giving the grab the isolated core alone.

**Why this is staged, not shipped blind:** the NDI send/recv/resend threads (`ndis:*`) today run
FIFO 50 on core 3, right next to their consumer. Moving them to the loaded general cores as
`SCHED_OTHER` MAY regress emit latency — the exact thing the isolation exists to protect. This is
an RT-tuning change to the live emit path and must be validated with a real capture/emit
measurement (the #728-style zero-SSH method: compare emit-dip / rate-step under box load before and
after), on the rig, by the supervisor's deploy-and-measure step — never merged blind from a code
lane.

## The decision to surface to the owner/supervisor

Step A (PREEMPT_RT) and Step B (honest stock) are two coherent end-states. Step A is a bigger
commitment (Ubuntu Pro dependency + fleet reboots) but makes the whole #289 design honest as
written; Step B keeps the stock kernel but reworks the live scheduling model. The capture-IRQ fix
in this branch is strictly better under EITHER and is forward-compatible with both, so it ships
now; the choice between A and B is the coordinated live step this runbook stages.
