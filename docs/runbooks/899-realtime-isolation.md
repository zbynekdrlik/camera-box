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

## STAGED step A — PREEMPT_RT kernel, fleet-wide (reboot-class — supervisor only) — CHOSEN end-state

**Kernel choice DECIDED (issue 899 lane 2, 2026-08-20): PREEMPT_RT (`linux-image-realtime`).**
It is the only option that makes the whole isolation design honest as written: it threads the
hardirq/softirq handlers, so the xhci capture IRQ becomes a schedulable kthread whose default RT
priority (50) is below the grab (90), and the merged RT-conditional affinity code
(`src/affinity.rs::select_irq_target_cores`) automatically flips to co-locating that IRQ on the
isolated core, which is now defensible. `linux-image-lowlatency` was rejected — it is CONFIG_PREEMPT
(not RT), so it does NOT thread hardirqs and would not satisfy the isolation assumption; staying on
the stock kernel (already the merged floor) leaves defect 1 unfixed. Trade-off accepted: an Ubuntu
Pro dependency + coordinated per-box reboots; these boxes are latency-bound not throughput-bound, so
the RT kernel's slightly lower max throughput is a non-issue.

This is the path that makes the FIFO priorities AND the IRQ-on-core-3 routing mean what the isolation
design already assumes. It is a real piece of work, not a package install:

**Live state (read-only, 2026-08-20, cam1/cam2/cam3):** all three still run `6.8.0-134-generic`
(PREEMPT_DYNAMIC); `linux-image-realtime` candidate `6.8.1-1015.16` is apt-resolvable but NOT
installed; **Ubuntu Pro is NOT attached** (so today every box plans `blocked:need-pro-attach` — see
the planner below); per-box drift the pin step must respect: `GRUB_DEFAULT=0` on cam1 vs `saved` on
cam2/cam3; cam2 additionally has the `linux-image-generic` meta installed (cam1/cam3 do not).

1. **Ubuntu Pro.** Ubuntu 24.04 ships the realtime kernel as `linux-image-realtime` under Ubuntu
   Pro (`pro attach <token>` + `pro enable realtime-kernel`). Confirm the fleet's Pro entitlement
   first; without it there is no supported RT kernel.
2. **The single-kernel invariant.** `verify-device.sh` check `(k)` (#547) enforces EXACTLY ONE
   installed kernel equal to the running one, and the appliances are `ro`-root with the #295/#547
   brick-hardening (GRUB pinned to a known-good initrd-bearing entry). Installing a second kernel
   violates check `(k)` until the OLD generic kernel is purged and GRUB re-pinned to the RT entry.
   Do this atomically per box, in the SAFE order (reboot INTO the RT kernel BEFORE purging generic —
   never purge the kernel you are still running, which would strip its own modules): install RT
   kernel → verify the initrd exists for it (the #295 guard) → pin GRUB to the RT entry
   (`grub-set-default` on a `GRUB_DEFAULT=saved` box, or edit `GRUB_DEFAULT` to the RT menuentry on a
   numeric-default box) → `update-grub` → **reboot** → confirm the box is running the RT kernel →
   THEN purge the generic kernel → re-run `verify-device.sh` check `(k)`. This exact ordered sequence
   is emitted, drift-aware, by the planner in the next section.
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

### The mechanical planner (issue 899 lane 2) — DRY-RUN, read-only, never mutates a box

Instead of improvising the reboot-class sequence by hand, the supervisor runs
`scripts/rt-kernel-upgrade.sh` per box. It reads the box's state READ-ONLY over ssh and prints the
exact, drift-aware, atomic plan from the pure + unit-tested decision logic in
`scripts/lib/rt-kernel-plan.sh` (`tests/rt_kernel_provision.rs`). There is deliberately NO apply
mode — the code PLANS, the supervisor APPLIES (enable-only doctrine).

```
scripts/rt-kernel-upgrade.sh --box <ip>              # print the box's plan (readiness + ordered steps)
scripts/rt-kernel-upgrade.sh --box <ip> --commands   # also print the concrete shell per step
```

The plan honours per-box drift automatically: a `GRUB_DEFAULT=saved` box gets `grub-pin:saved`, a
numeric-default box gets `grub-pin:menuentry`; `purge-generic` is emitted only when the generic meta
is present; `install-rt-kernel` is skipped if the RT kernel is already installed. When Ubuntu Pro is
not attached (today's fleet) the plan is the single token `blocked:need-pro-attach` — attach Pro
(`pro attach <token> && pro enable realtime-kernel`) first, then re-plan. **Order of operations per
box, one box at a time:** run `--box` to get the plan → apply each step → reboot → confirm RT →
purge generic → `verify-device.sh` (check `(k)` + check `(ac)` must now read PREEMPT_RT) → a full
E2E → confirm an SSH login no longer perturbs capture (the original zero-SSH-method symptom). Only
then move to the next box; the generic entry stays in GRUB as rollback until the RT kernel is proven.

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

## The decision — chosen, and what is left for the supervisor

**Step A (PREEMPT_RT) is the CHOSEN end-state** (issue 899 lane 2). It is the only option that makes
the isolation design honest as written, and the merged capture-IRQ code is already forward-compatible
with it. What is left is purely the reboot-class APPLICATION, which a code lane cannot do on a live
`ro`-root fleet during an E2E:

- **Prerequisite:** attach Ubuntu Pro on the fleet (not attached today) and `pro enable
  realtime-kernel`. Until then every box's plan is `blocked:need-pro-attach`.
- **Apply per box** using `scripts/rt-kernel-upgrade.sh --box <ip>`: canary one box, prove it, then
  roll the rest one at a time, each behind its own reboot + verify + E2E.

Step B (honest stock per-thread policy) is retained ONLY as a fallback if Pro entitlement turns out
to be unavailable — and even then it must be validated with a live emit measurement (below), never
merged blind. The capture-IRQ fix already merged is strictly better under either kernel and ships
regardless.
