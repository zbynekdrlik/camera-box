# Runbook — realtime isolation (issue 899)

**Status: STAGED, not applied.** The autopilot worker for issue 899 ran in a worktree lane and
must not apply reboot-class or live-kernel changes to the fleet (an E2E may be running on the rig;
a fleet reboot / kernel swap / scheduling-model change is the supervisor's coordinated step). This
runbook is the hand-off for those live steps. The **code/config-stageable** part (the capture-IRQ
fix + the honest service comment + the tested dry-run planner) is already in the branch.

**Owner decision (2026-08-20): Ubuntu Pro / subscription is REJECTED** ("linux is open source and
has its own free realtime compiles, I will not pay Ubuntu"). The end-state plan is now two steps:

- **STEP 1 (this runbook's primary path) — `linux-lowlatency` (HWE), the free official Ubuntu
  main-archive low-latency kernel.** `preempt=full` + a higher-HZ timer give most of the practical
  benefit at zero cost. Precedent: imag-nb already runs it (`setup-imag.sh` step 7, issue 482).
  Deploy per box, reboot in a window with NO live E2E, then MEASURE the benefit (below).
- **STEP 2 (only if the measurement shows STEP 1 is not enough) — a custom PREEMPT_RT kernel built
  in CI.** Mainline ≥6.12 has RT merged; a CI build job + artifact + the same deploy path as the
  camera-box binary. It is maintenance-heavier (a self-owned kernel), so it ships only with data.

## Background — the three defects (re-validated live on cam1/cam2/cam3, 2026-08-20)

The fleet's realtime setup was only half-built. Measured on cam1 (10.77.9.61, N150-class, 4 cores,
`isolcpus=3`):

1. **Kernel is not PREEMPT_RT and has no full preemption.** `uname -r` = `6.8.0-134-generic`,
   `/proc/version` = `PREEMPT_DYNAMIC` (voluntary), `/sys/kernel/realtime` absent. SCHED_FIFO
   orders our own threads, but hardirq/softirq handlers are NOT threaded, so an interrupt preempts
   even the prio-90 grab. **This is the one defect STEP 1 + STEP 2 address.**
2. **The reserved core is shared by many FIFO threads; the service comment was false.**
   `CPUSchedulingPolicy=fifo` in the unit applies to the WHOLE process, so all threads on core 3
   inherit FIFO 50 — not `SCHED_OTHER` as the old comment claimed. The grab is FIFO 90 (siblings
   can't preempt it), but the comment documented a state the fleet has never been in. **This is
   STAGED step B** (below), unchanged by the STEP-1/STEP-2 kernel decision.
3. **The xhci capture IRQ shared the grab core.** IRQ 125 → core 3. **FIXED live** — lane 1's
   merged `src/affinity.rs::select_irq_target_cores` routes it OFF the grab core on a stock kernel;
   re-verified 2026-08-20 (`smp_affinity_list=0-2` on all three boxes).

## What this branch already does (code/config, no live change)

- **Defect 3 fixed in code (merged, live).** `src/affinity.rs` routes the capture IRQ **off** the
  grab core on a stock kernel and only co-locates it on the isolated core on a real PREEMPT_RT
  kernel. This is RT-conditional and forward-compatible: it flips automatically once the fleet is
  on a full-RT kernel (STEP 2). On the lowlatency kernel (STEP 1, still not PREEMPT_RT) it keeps
  the IRQ off the grab core — the correct behaviour there too.
- **Defect 2's false comment corrected** in `systemd/camera-box.service` (lane 1). Unchanged here.
- **The tested dry-run planner** (`scripts/lib/rt-kernel-plan.sh` + `scripts/rt-kernel-upgrade.sh`,
  `tests/rt_kernel_provision.rs`) now plans the **lowlatency** upgrade (reworked from the rejected
  pro-attach PREEMPT_RT design). Read-only, enable-only: the code PLANS, the supervisor APPLIES.

## STEP 1 — `linux-lowlatency-hwe-24.04`, per box (reboot-class — supervisor only) — CHOSEN

**Kernel choice: `linux-lowlatency-hwe-24.04`** — the FREE official Ubuntu main-archive low-latency
meta, no subscription. On 24.04 it is a **config meta**: it depends on the generic HWE image + the
`lowlatency-kernel` config package, which drops `/etc/default/grub.d/99-lowlatency.cfg` =
`GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full rcu_nocbs=all"` — full preemption + a higher-HZ timer.
`uname -r` after reboot is still `*-generic` (the meta keeps the generic image); the win is the
`preempt=full` boot mode. Exactly the imag-nb precedent (`setup-imag.sh` step 7, issue 482).

**Honest limit:** `preempt=full` is NOT full PREEMPT_RT — it still does not thread hardirq/softirq
handlers, so a hardirq can still preempt the prio-90 grab. `verify-device.sh` check `(ac)` will
therefore keep WARNing "kernel is NOT PREEMPT_RT" after STEP 1 — that is EXPECTED and correct.
STEP 1 is the free, low-risk first move; STEP 2 closes the remaining gap only if measured to matter.

**Current deployment state (read-only, re-verified 2026-09-03) — STEP 1 is 3/7 done, not 3/4.**
The 2026-09-01 note above was written for a 4-box active fleet; cam5/cam6/cam7 have since returned
to `CAMERA_ACTIVE_SET` and were never upgraded, so the real figure is 3/7 and the active fleet the
fused E2E gate sweeps is currently split across two scheduling models:

```
cam1  7.0.0-30-generic    preempt=(full) lazy          99-lowlatency.cfg=yes
cam2  7.0.0-30-generic    preempt=(full) lazy          99-lowlatency.cfg=yes
cam3  7.0.0-30-generic    preempt=(full) lazy          99-lowlatency.cfg=yes
cam4  6.8.0-134-generic   preempt=none (voluntary)     99-lowlatency.cfg=no
cam5  6.8.0-134-generic   preempt=none (voluntary)     99-lowlatency.cfg=no
cam6  6.8.0-134-generic   preempt=none (voluntary)     99-lowlatency.cfg=no
cam7  6.8.0-134-generic   preempt=none (voluntary)     99-lowlatency.cfg=no
```

cam1/cam2/cam3 run the lowlatency profile — `uname -r` = `7.0.0-30-generic`,
`/sys/kernel/debug/sched/preempt` = `(full)`, `/etc/default/grub.d/99-lowlatency.cfg` present,
single-kernel restored (old `6.8.0-134` purged). **cam4 (10.77.9.64) is deliberately still on the
GA `6.8.0-134-generic` (preempt=none)** — it is the CONTROL box for the issue-1198 grabber-flap
hypothesis and must NOT be upgraded until that hold is lifted (see issue 1198; the 2026-08-30 finding
that cam2's 61.3 fps flap is card-internal, not the kernel, weakens that hold — a supervisor/owner
call, not a code lane's). cam5/cam6/cam7 carry no such hold — they simply re-joined the active set
un-upgraded. The xhci capture-IRQ fix (defect 3) is live on ALL SEVEN active boxes
(`smp_affinity_list` off the grab core). `scripts/rt-kernel-upgrade.sh --box <ip>` correctly reads
`noop:already-lowlatency` for cam1/cam2/cam3 and `ready` → `install-lowlatency /
verify-lowlatency-config / grub-pin:*` for cam4, cam5, cam6 **and** cam7 — the planner itself needed
no change, only this paragraph was behind reality.

**Finishing STEP 1 is no longer only rollout housekeeping — it doubles as the cheap falsification of
a candidate covariate for issue 1168's cross-camera constant offset.** Across 39 local verdict JSONs,
each camera's `residual_offset_ms` (its deviation from that run's own median) averages
cam1 +4.32 / cam2 +2.54 / cam3 +16.82 ms on the three lowlatency (`preempt=full`) boxes against
cam4 −2.16 / cam5 −3.35 / cam6 −5.02 / cam7 −4.41 ms on the four GA (`preempt=none`) boxes, and the
per-run group gap (`mean(cam1..3) − mean(cam4..7)`) is positive in 37 of 39 runs (median +11.0 ms).
This is correlation, not causation: group membership is historical (which boxes happened to get
upgraded first), not random, and cam3 — which also carries its own manual NDI upgrade — contributes
a disproportionate share; drop cam3 and the gap roughly halves, though the sign separation survives.
So a mid-cluster GA→lowlatency upgrade is now worth doing partly to re-measure that box's constant,
not only to finish the rollout.

**Recommended canary: cam5, not cam4** (reboot-class — a recommendation for the supervisor/owner,
per this runbook's own canary-first protocol below; not something a code lane applies). cam5 sits
mid-cluster and carries no open hold, unlike cam4 (the issue-1198 control box). No new procedure is
needed — follow the existing "Canary first, then fleet" step and the "Measuring the benefit"
before/after journal recipe further down this runbook, and additionally note cam5's per-camera
`residual_offset_ms` before and after the reboot alongside the emit-jitter/underrun windows.

**Live state (read-only, 2026-08-20, cam1/cam2/cam3):** all three run `6.8.0-134-generic`
(PREEMPT_DYNAMIC); `linux-lowlatency-hwe-24.04` candidate is apt-resolvable from the main archive
(6.17 on cam1, 7.0 on cam2/cam3) with **no Pro**; `lowlatency-kernel` NOT installed,
`99-lowlatency.cfg` absent, so preempt=full is not active. **Per-box drift the plan respects:**
- `GRUB_DEFAULT=0` on cam1 vs `saved` on cam2/cam3 (→ `grub-pin:menuentry` vs `grub-pin:saved`).
- The HWE generic meta (`linux-image-generic-hwe-24.04`) is **NOT** installed on any cam box, so
  installing the lowlatency meta pulls a NEW HWE generic image alongside the running GA
  `6.8.0-134-generic`. That is a **second kernel** → it violates the single-kernel invariant
  (`verify-device.sh` check `(k)`, issue 547) until the OLD image is purged after the reboot. (On
  an imag-like box that already tracks the HWE meta this would be config-only, no new image, no
  purge — the planner's `superseded_generic` axis distinguishes the two.)
- Kernel apt-holds differ (cam1 holds the specific GA image + modules; cam2/cam3 hold the generic
  metas), so the install needs `--allow-change-held-packages` — same as `setup-imag.sh` step 7.
- Root is `ro` on cam2/cam3 (and normally on cam1). Every mutating step below wraps
  `mount -o remount,rw /` … `mount -o remount,ro /` around itself; the planner's `--commands`
  output already includes those remounts, so a step is copy-paste-safe on a `ro`-root box.

### The mechanical planner — DRY-RUN, read-only, never mutates a box

Instead of improvising the reboot-class sequence by hand, the supervisor runs
`scripts/rt-kernel-upgrade.sh` per box. It reads the box's state READ-ONLY over ssh and prints the
exact, drift-aware, atomic plan from the pure + unit-tested decision logic in
`scripts/lib/rt-kernel-plan.sh` (`tests/rt_kernel_provision.rs`). There is deliberately NO apply
mode — the code PLANS, the supervisor APPLIES (enable-only doctrine).

```
scripts/rt-kernel-upgrade.sh --box <ip>              # print the box's plan (readiness + ordered steps)
scripts/rt-kernel-upgrade.sh --box <ip> --commands   # also print the concrete shell per step
```

The ordered atomic plan (the SAFE order — reboot INTO the new kernel BEFORE purging the superseded
one, never purge the kernel you are still running):

1. `install-lowlatency` — `apt-get install -o Dir::Cache::archives=/root/apt-tmp -y --allow-change-held-packages linux-lowlatency-hwe-24.04`
   (wrapped in a rw/ro remount, with `TMPDIR=/root/tmpbig` exported first — see "Operational gotchas"
   below for the appliance tmpfs reasons; use the exact `--commands` output, do not hand-type this).
   Pulls the `lowlatency-kernel` config (preempt=full) and, on a box without the HWE meta, a new HWE
   generic image.
2. `verify-lowlatency-config` — assert `/etc/default/grub.d/99-lowlatency.cfg` exists AND carries
   `preempt=full` (refuse to trust the config package otherwise — the `setup-imag.sh` step-7 guard).
3. `grub-pin:saved` | `grub-pin:menuentry` — pin GRUB to the new HWE image (drift-aware).
4. `safe-grub-regen` — guarantee every installed kernel has an initrd (issue 295 brick-hardening),
   then `update-grub` once (the preempt=full grub.d drop applies to every entry).
5. `reboot-into-lowlatency` — **reboot the box, in a window with NO live E2E**, one box at a time;
   the old generic entry stays in GRUB as rollback.
6. `confirm-running-lowlatency` — confirm `preempt=full` is ACTIVE: `grep -qw preempt=full
   /proc/cmdline` AND `/sys/kernel/debug/sched/preempt` shows `(full)`. `uname -r` stays `*-generic`.
7. `purge-superseded-generic` — restore single-kernel: purge ONLY the SPECIFIC pre-upgrade image
   (the `uname -r` noted before the upgrade) with `--allow-change-held-packages`. **Never a wildcard
   generic purge** — the new running kernel is also a `-generic` image, so a glob would remove it.
8. `verify-single-kernel` — re-run `verify-device.sh`: check `(k)` restored; check `(ac)` still
   WARNs "not PREEMPT_RT" (EXPECTED — preempt=full is STEP 1, full RT is STEP 2).
9. `post-verify` — the full `verify-device.sh` gate + a full E2E + the before/after measurement below.

**Purge keys on the OBSERVED stale set, not the pre-install prediction (cam5 miss, 2026-09-03).**
The `superseded_generic` axis (`GEN`) is a PRE-install PREDICTION (GEN=1 iff the HWE generic meta is
absent) and drives the purge only in the pre-install branch, where `uname -r` is still the OLD kernel
so "installed image != uname -r" is not yet a valid stale signal. Once a box is ALREADY running
`preempt=full` (the STEP-1 install + reboot are done), the planner instead reads an OBSERVED fact:
`gather_facts` lists the installed `linux-image-<ver>-generic` packages whose `<ver>` != `uname -r`
plus the `linux-image-generic` meta (surfaced as the `superseded_installed=` field on the `# facts:`
line), and `rt_kernel_upgrade_plan` emits `purge-superseded-generic` + `verify-single-kernel`
whenever that observed set is non-empty — naming the exact packages in `--commands` — instead of
collapsing to `noop:already-lowlatency`. cam5 (10.77.9.65, 2026-09-03) sat exactly here: it ran
`preempt=full` with GEN=0 (HWE meta present) yet a stale `6.8.0-134-generic` (+ modules/-extra) and
the `linux-image-generic` meta were still installed; the old planner printed `noop` and never
emitted the purge, silently leaving the single-kernel invariant (check `(k)`) violated until the
supervisor purged by hand. So on a re-planned already-lowlatency box, re-run
`scripts/rt-kernel-upgrade.sh --box <ip> --commands` and apply the purge it now emits.

**Operational gotchas the planner now bakes in (supervisor findings 2026-08-22, cam1/2/3 upgrade).**
These were hit live on all three boxes and are now folded into the planner's generated commands, so a
copy-paste of `--commands` is safe on cam4 (do NOT hand-improvise them again):

- **`/var/cache` is a 512M tmpfs** — the lowlatency-hwe install pulls ~242MB of archives plus a new
  HWE generic image and overflows it. The generated `install-lowlatency` now passes
  `-o Dir::Cache::archives=/root/apt-tmp` so `.deb`s are cached on the ample rootfs (~51G).
- **`/tmp` is a 100M tmpfs** — the ~78MB initrd build (both the in-postinst one during install and
  the `safe-grub-regen` `update-initramfs`/`update-grub`) overflows it. Both generated commands now
  `export TMPDIR=/root/tmpbig` (rootfs) after `mkdir -p`.
- **The first `apt-get update` is mandatory** — the on-box apt index was stale (404 on the
  security.ubuntu.com meta) until refreshed. The generated `install-lowlatency` already runs
  `apt-get update` before the install.
- **GA-meta purge caveat (box-specific).** On a box that ALSO has the GA `linux-generic` /
  `linux-image-generic` / `linux-headers-generic` metas installed (cam2 was such a box), those metas
  BLOCK the old-image purge and must be purged together with the specific old image. cam1/cam3/**cam4**
  do NOT have the GA meta (verified 2026-09-01: cam4 has neither), so cam4's `purge-superseded-generic`
  is the simple specific-image case (`linux-image-6.8.0-134-generic` + `linux-modules-6.8.0-134-generic`,
  both apt-held → `--allow-change-held-packages`). Still NEVER a wildcard `-generic` purge.

**Canary first, then fleet.** Upgrade ONE cam box, reboot, run `verify-device.sh`, then a full E2E,
then the measurement below. Only if it holds, roll the rest one box at a time, each behind its own
reboot + verify + E2E. **Rollback:** the pinned generic entry stays in GRUB until preempt=full is
proven on a box; if a box misbehaves, `grub-reboot` the generic entry and reboot.

**Do NOT apply STEP 1 from a code lane.** It is a coordinated per-box reboot; the worker's branch
carries only the forward-compatible code + the tested planner.

## Measuring the benefit — before/after, 10 minutes, from the journal (no new tools)

Run this on the **canary box** immediately BEFORE its reboot into preempt=full, and again AFTER,
using ONLY existing log lines (no new instrumentation). Compare the two windows to decide whether
STEP 1 is enough or STEP 2 is warranted.

**On the cam box (its own journal) — emit jitter + drop counters:**

```
# 10-minute window of the ~5s emit stats tick and the per-second emit ring:
journalctl -u camera-box --since "10 min ago" | grep -E 'Streaming:|emit-1s' > /tmp/899-before.log
#   * Streaming: X fps emitted / Y fps captured (... N capture-dropped, M corrupted)
#       -> watch emit-fps stability (min/max spread) and the capture-dropped + corrupted deltas.
#   * #707 emit-1s: [60,60,59,60,60] cap-1s: [...]   -> per-second emit; a dip below 60 = a jitter event.
#   * '#707 B1 emit-1s DIP' WARN lines -> COUNT them; each is a sub-5s emit pause on the box.
```

Summarise per window: emit-fps min/max, count of `emit-1s DIP` WARNs, and the delta of
`capture-dropped` / `corrupted` over the 10 min. A lower DIP count + tighter emit-fps spread + zero
drop growth after the reboot = STEP 1 delivered; unchanged or worse = escalate to STEP 2.

**On the downstream OBS box (strih/stream) — the underrun counter for this camera's NDI source:**

```
# The genlock-fifo audit line carries an explicit underruns= counter per NDI input, ~every 5s:
#   genlock-fifo audit 'NDI CAM1': received=300 consumed=300 underruns=0 lagged=0 ts_present=...
# Read 10 min of it for the camera under test (OBS log dir on the box) and compare underruns= /
# lagged= totals before vs after. src/jitter_audit.rs + `genlock-jitter-report` parse this family.
```

Record both windows on the issue (a `MEASUREMENT:` comment) so the STEP-2 go/no-go is data-driven.

## STEP 2 — custom PREEMPT_RT kernel via CI (escalation — ONLY with measurement data)

If the measurement shows preempt=full is not enough (persistent emit dips / underruns under load,
or the #728 SSH-perturbation symptom survives), escalate to a genuine PREEMPT_RT kernel — the only
model that threads hardirq/softirq handlers so the xhci IRQ becomes a schedulable kthread below the
grab (the isolation design's original assumption, which the merged `select_irq_target_cores`
already flips to on an RT kernel). Owner decision keeps this FREE (no Ubuntu Pro): **build it in CI**.

- Mainline ≥6.12 has PREEMPT_RT merged into the tree (no out-of-tree `-rt` patch needed).
- A CI build job produces a `.deb` kernel artifact (image + modules + headers), signed/pinned the
  same way and shipped via the SAME deploy path as the camera-box binary.
- Apply per box with the SAME atomic reboot-class sequence as STEP 1 (install → grub-pin → reboot →
  confirm → purge superseded → verify `(k)` — check `(ac)` now reads PREEMPT_RT), one box at a time.
- Trade-off (why it is gated on data, not default): a self-owned kernel is maintenance-heavier —
  security updates, rebuilds on each base bump, and the single-kernel/brick-hardening dance every
  time. Do it only when the STEP-1 measurement proves it is needed.

## STAGED step B — honest per-thread stock policy (defect 2, needs a live A/V/emit measurement)

Independent of the kernel choice: defect 2 (the process-wide `CPUSchedulingPolicy=fifo` puts all
threads FIFO-50 on the grab core, not `SCHED_OTHER`) can be made honest by dropping the
process-wide policy, setting `CPUAffinity=0-2`, and having the binary pin ONLY the grab thread onto
core 3 at FIFO 90. **Why staged, not shipped blind:** the NDI send/recv/resend threads run FIFO 50
on core 3 next to their consumer today; moving them to the loaded general cores as `SCHED_OTHER`
MAY regress emit latency — the exact thing the isolation protects. It must be validated with a real
capture/emit measurement (the #728-style zero-SSH method) by the supervisor's deploy-and-measure
step, never merged blind from a code lane.

## The decision — chosen, and what is left for the supervisor

**STEP 1 (`linux-lowlatency-hwe-24.04`) is the chosen first move** — free, low-risk, the imag-nb
precedent, and the merged capture-IRQ code is already correct on it. What is left is purely the
reboot-class APPLICATION, which a code lane cannot do on a live `ro`-root fleet during an E2E:

- **Apply per box** using `scripts/rt-kernel-upgrade.sh --box <ip>`: canary one box, reboot in a
  no-E2E window, run the before/after measurement, then roll the rest one at a time.
- **Escalate to STEP 2 (CI-built PREEMPT_RT) only if the measurement warrants it.**

Step B (honest stock per-thread policy) is retained as a separate defect-2 task and, like STEP 2,
needs a live emit measurement before it ships. The capture-IRQ fix already merged is strictly better
under either kernel and ships regardless.
