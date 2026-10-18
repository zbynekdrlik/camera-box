---
paths:
  - "src/affinity.rs"
  - "scripts/lib/rt-kernel-plan.sh"
  - "scripts/rt-kernel-upgrade.sh"
  - "tests/rt_kernel_provision.rs"
  - "docs/runbooks/899-realtime-isolation.md"
  - "scripts/lib/display-gpu.sh"
  - "tests/python/test_display_gpu_1446.py"
---

# Realtime isolation on the cam fleet (issue 899)

The cam-box fleet's realtime isolation is ONE coherent subsystem, split across a merged code layer
and a staged reboot-class kernel layer. Read this before touching affinity/IRQ/kernel work.

**Owner decision (2026-08-20): Ubuntu Pro is REJECTED — the kernel path is FREE, two-step.**
STEP 1 = `linux-lowlatency-hwe-24.04` (official main archive, preempt=full, the imag-nb precedent).
STEP 2 = a custom PREEMPT_RT kernel built in CI, ONLY if a live measurement shows STEP 1 is not
enough. An earlier lane's `linux-image-realtime`/`pro attach` plan is SUPERSEDED — do not restore it.

## What is DONE (merged, live on the fleet)

- **`src/affinity.rs`** — `kernel_is_preempt_rt()` + `select_irq_target_cores(is_rt, capture_core,
  online)`. `setup_irq_affinity()` routes the xhci capture IRQ **off** the isolated grab core on a
  stock/lowlatency (non-RT) kernel, and only co-locates it on the grab core on a real PREEMPT_RT
  kernel. RT-conditional + forward-compatible: it flips automatically once the fleet is on an RT
  kernel (STEP 2). Verified live (all cam boxes, 2026-08-20): xhci IRQ 125 `smp_affinity_list=0-2`.
  - **issue 1198 refinement:** the non-RT capture IRQ no longer goes onto ALL general cores; it is
    dedicated to ONE reserved general core — the highest online non-capture core
    (`select_irq_reserved_core`) — and `select_painter_cores(capture_core, reserved_irq_core, online)`
    excludes BOTH the capture core AND that reserved IRQ core. So on the 4-core cam boxes the split is
    capture=3 / capture-IRQ=2 / painter=[0,1] instead of the old capture=3 / IRQ+painter both [0,1,2].
    WHY: before it, the non-preemptible xhci hardirq shared cores with the painter/display threads, and
    cam1's #528 HDMI-preview 1080p scale on those shared cores delayed URB delivery → dropped frames
    (58 fps captured vs 60 emitted; 0 drops with the preview off — single-variable proof). Degradation
    rungs keep the old behaviour when a box is too small to reserve a core without stranding the
    painter (2-core → shared; 1-core → capture core), and the RT path (`[capture_core]`) is unchanged.
    `setup_irq_affinity()` logs a `#1198 core split: capture=… capture-IRQ=… painter/display=…` INFO
    line at startup. **Live confirmation on the rig is a supervisor deploy step AFTER integration** —
    the `smp_affinity_list=0-2` above becomes a single core (2) once the fixed binary is deployed.
- **`verify-device.sh` check `(ac)`** — WARN-only, surfaces the PREEMPT_RT status + whether the
  capture IRQ is off the grab core. Deliberately never red-fails the current non-RT fleet. NOTE:
  after STEP 1 (lowlatency/preempt=full) this check STILL WARNs "not PREEMPT_RT" — that is EXPECTED
  and correct (preempt=full is not full RT). It only reads "PREEMPT_RT" after STEP 2.
- **Defect 2 — per-thread SCHED_FIFO (code-fixed in this lane, replaces the old runbook Step B).**
  The process-wide `CPUSchedulingPolicy=fifo` is DROPPED from BOTH appliance unit copies
  (`scripts/setup-device.sh` STEP 7 heredoc + the checked-in `systemd/camera-box.service` that
  `build-image.sh` bakes) — it forced every thread (tokio/NDI ndis:*/disc:*/self-heal) onto
  SCHED_FIFO prio 50 on the isolated core (27 on cam1). The binary now raises SCHED_FIFO prio 90
  PER THREAD only on the capture+emit hot path — `src/affinity.rs::set_current_thread_realtime`
  (pure decision `realtime_fifo_priority(RtThreadRole)`) called from the production capture loop
  and the cam1-burn emit thread; every auxiliary thread (painter/display/intercom/qpsk)
  stays SCHED_OTHER. NO thread moves cores (this is why it is safe without a live measurement,
  unlike the rejected Approach 2 below). Needs `CAP_SYS_NICE` — the STEP 9 setcap already grants
  `cap_sys_nice,cap_ipc_lock`. Locked by `verify-device.sh` check `(ah)` (HARD FAIL): the unit must
  carry no process-wide `CPUSchedulingPolicy` AND the live camera-box process must show only a
  handful of SCHED_FIFO threads (not ~27). **Live per-box redeploy of the new unit+binary + the
  `verify-device.sh` proof are a supervisor step** — cam1 gets it via its re-provisioning run.
  The DEEPER "also move NDI emit threads off core 3" rework (the rejected Approach 2) still needs a
  live #728-style emit measurement and stays staged; per-thread FIFO here does NOT do that.

- **issue 1367 — the reserved capture-IRQ core must be QUIET, not only reserved.** Three things
  still ran on core 2 after the 1198 split, each measured on cam5 (15.10.2026) as capture-phase
  re-seeds of the leg:
  - **systemd PID 1 and every unit start** (65 of 102 re-seeds within 1 s of a unit start). Fix: the
    manager `CPUAffinity` drop-in from `scripts/lib/cambox-housekeeping-cpus.sh` (housekeeping =
    online − isolated − reserved). systemd 255 applies it to itself only at boot; a live box needs
    `taskset -pc <set> 1` (the lib's apply program), a `daemon-reexec` does not move PID 1.
  - **the NIC IRQ** (`enp1s0`, allowed on 0-2 by `irqaffinity=0-2`): moving it off cut the re-seeds
    from 22-31 per run to 2. camera-box's `setup_irq_affinity` now moves every non-capture device IRQ
    off the reserved core (pure plan `src/irq_offload.rs`) and sets `/proc/irq/default_smp_affinity`
    to the same set, so an IRQ allocated later (a NIC re-open) does not land there.
  - **the split never ran on the fleet at all:** setup-device's unit had no
    `ExecStartPre=... --setup-irq-affinity` (only build-image's unit did). setup-device STEP 7 now
    writes `camera-box.service.d/irq-affinity.conf` with `ExecStartPre=-+...` ('+' runs it outside
    `ProtectSystem=strict`/`ReadOnlyPaths=/`, which make /proc/irq read-only). verify-device `(au)`
    grades the LOADED unit (`systemctl show ExecStartPreEx` flags `privileged`), a 2 s
    /proc/interrupts sample (a non-capture IRQ that FIRED on the reserved core fails; an idle IRQ
    allowed there does not) and the default affinity.

## What is STAGED (reboot-class — SUPERVISOR only, never from a code lane)

- **STEP 1 — `linux-lowlatency-hwe-24.04`** (the free main-archive low-latency meta, no Pro). On
  24.04 it is a CONFIG meta: it depends on the generic HWE image + `lowlatency-kernel`, which drops
  `/etc/default/grub.d/99-lowlatency.cfg` (`preempt=full rcu_nocbs=all`). `uname -r` stays
  `*-generic`; the win is the `preempt=full` boot mode + higher-HZ timer. Precedent: imag-nb
  (`setup-imag.sh` step 7, issue 482). It does NOT thread hardirqs (that is STEP 2), but gives most
  of the practical benefit for zero cost.
- **`scripts/lib/rt-kernel-plan.sh`** (pure, `run_sourced`-tested) + **`scripts/rt-kernel-upgrade.sh`**
  (DRY-RUN driver, read-only ssh, `--box`/`--facts`/`--commands`, **NO apply mode** — enable-only:
  the code PLANS, the supervisor APPLIES). Full runbook: `docs/runbooks/899-realtime-isolation.md`.
  Verdict tokens: `already-lowlatency` / `ready` / `no-rt-candidate` (the fail-closed shape kept
  for a genuinely-missing package). Every mutating step's `--commands` text is ONE pasteable
  program (`_rt_window_program`, issue 1407): `bash -s <<'RT_KERNEL_STEP'`, the rw open, the
  work with its rc kept, the shared verified ro close from `scripts/lib/ro-window.sh` run whatever
  the work did, then the work's own FAIL/OK line. Never print a bare `&& mount -o remount,ro /`
  again (`.claude/rules/ro-window.md`); the step's apt/grub/TMPDIR text lives in the WORK argument,
  so the multi-word `apt-get install` anchor below still holds. Plan tokens: `install-lowlatency` → `verify-lowlatency-config`
  → `grub-pin:*` → `safe-grub-regen` → `reboot-into-lowlatency` → `confirm-running-lowlatency` →
  `purge-superseded-generic` → `verify-single-kernel` → `post-verify`.
- **SAFE atomic order** (the planner enforces): install → verify config → grub-pin → grub-regen →
  **reboot INTO lowlatency → confirm preempt=full → THEN purge the superseded generic** → verify
  check `(k)`. Never purge the kernel you are still running, and NEVER a wildcard `-generic` purge
  (the new running kernel is also a `-generic` image — purge only the specific pre-upgrade version).
- **Drift the planner reads live (2026-08-20):** the candidate is apt-resolvable with NO Pro;
  `GRUB_DEFAULT=0` on cam1 vs `saved` on cam2/cam3 (→ `grub-pin:menuentry` vs `grub-pin:saved`);
  the HWE generic meta is NOT installed on any cam box, so the install pulls a NEW HWE image
  alongside the GA `6.8.0-134-generic` (a second kernel → the `superseded_generic` axis = purge
  needed); kernel apt-holds differ per box (→ `--allow-change-held-packages`); root is `ro`.
- **STEP 2 — custom PREEMPT_RT kernel via CI** (escalation, only with measurement data). Mainline
  ≥6.12 has RT merged; a CI build job → a `.deb` artifact → the same deploy path + the SAME atomic
  reboot-class sequence as STEP 1. Maintenance-heavier (self-owned kernel), so gated on the STEP-1
  before/after measurement. Runbook/rule only in the current lane — NOT built here.

## The N150 boxes need the HWE kernel for a GPU driver (issue 1446)

- **cam5, cam6 and cam7 carry the N150 (Twin Lake) GPU `8086:46d4`.** The fleet kernel
  6.8.0-142's i915 lists only `46D0 46D1 46D2` (`modinfo -F alias i915`), so that GPU has NO driver:
  no `/dev/dri`, no `/sys/class/drm`. The cameraman HDMI preview then lives only on the firmware
  framebuffer, which the UEFI firmware sets up only when a monitor is connected and on at POST. A
  monitor plugged in later never lights (cam5, 16.10.2026). cam1-4 carry `8086:46d1`: i915 is
  bound there, and the framebuffer follows hotplug.
- **Both HWE kernels list `46D3`/`46D4`** (read from the i915.ko in each package's linux-modules
  .deb): `linux-image-generic-hwe-24.04` -> 7.0.0-38, and the lowlatency 6.11.0-1016 image.
- **`linux-lowlatency-hwe-24.04` (STEP 1 above) now resolves to the GENERIC HWE kernel 7.0.0-38 +
  `lowlatency-kernel`** (`/etc/default/grub.d/99-lowlatency.cfg`: `preempt=full rcu_nocbs=all`),
  not the 6.11 lowlatency image the archive's separate meta still names (read on cam5, 16.10.2026).
  So the STEP 1 install also gives an N150 box its GPU driver: one kernel line on the fleet, the
  same `scripts/rt-kernel-upgrade.sh` planner and the same reboot-before-purge order.
- **The install pulls a libc6 upgrade, and the root stays writable until the next boot.** Running
  processes (avahi, cron, dantesync, dbus-daemon, camera-box, old shells) keep the deleted old
  libc mapped, so the step's verified ro close FAILs by name (`.claude/rules/ro-window.md`), and
  dbus cannot be restarted safely. So install right before a planned PHYSICAL power cycle: a short
  power-button press (a clean ACPI poweroff), then power on. Never a remote reboot of a cambox.
- **A reflashed box starts ON the line (issue 1446, provisioning half).** create-usb-linux.sh's
  chroot sources `rt-kernel-plan.sh` (copied to `/tmp` with `ro-window.sh` + `ro-root.sh`, which it
  lazy-sources from its own dir; all three are in `check_required_files`), installs
  `"$(rt_kernel_flavour)"` instead of the GA `linux-image-generic`, holds
  `$(rt_kernel_line_held_metas)` (flavour, HWE image + headers metas, `lowlatency-kernel`) and FAILs
  the build when the flavour is missing or grub.cfg lacks `preempt=full` (lowlatency-kernel's
  `/etc/default/grub.d/99-lowlatency.cfg` appends it; `update-grub` runs after the
  `/etc/default/grub` rewrite, so it survives). setup-device.sh is unchanged: a live box moves only
  through the planner. **verify-device `(aw)` "fleet kernel line"** (HARD) grades it with
  `rt_kernel_line_verdict` over one `rt_kernel_line_gather_remote_snippet` read: the flavour
  `install|hold ok installed`, `preempt=full` a whole word of `/proc/cmdline`, and `uname -r` = the
  ONE `linux-image-<digit>...` entry of the installed `linux-image-generic-hwe-24.04` meta's
  `Depends` (the 7.0 meta also lists `linux-modules-*` / `linux-main-modules-*`, skipped). Its FAIL
  names `scripts/rt-kernel-upgrade.sh --box <ip>`.
- **verify-device `(av)` "display GPU driven" is the gate** (`scripts/lib/display-gpu.sh`): it
  FAILs every PCI display controller (class `0x03*`) with no driver bound, naming its
  vendor:device id and the running kernel. `/dev/fb0` is not the signal: a driven box with no
  monitor has none, and an undriven box whose monitor was on at boot has one.

## Gotchas

- **The lowlatency meta is CONFIG-only vs an IMAGE swap depending on the box.** On a box that
  already tracks the HWE generic meta (imag-like) it adds only `lowlatency-kernel` (preempt=full),
  no new image. On the cam boxes (HWE meta absent) it ALSO pulls a new HWE generic image → a second
  kernel → single-kernel-invariant handling (purge the old one after reboot). The planner's
  `superseded_generic` axis (gather_facts: HWE meta absent → 1) distinguishes them; do not assume
  config-only on the cam fleet.
- **The purge keys on the OBSERVED stale set on an already-lowlatency box, NOT the prediction
  (`GEN`) — the cam5 2026-09-03 gap.** `GEN` (gather_facts: HWE meta absent → 1) is a PRE-install
  PREDICTION and drives the purge ONLY in the pre-install (run=0) branch, where `uname -r` is still
  the OLD kernel so "installed image != uname -r" is not yet a valid stale signal. Once a box is
  ALREADY running preempt=full (run=1), the planner instead reads an OBSERVATION — gather_facts's
  6th `superseded_installed` field (installed `linux-image-<ver>-generic` whose `<ver>` != `uname
  -r`, plus the `linux-image-generic` meta) — and emits `purge-superseded-generic` +
  `verify-single-kernel` whenever it is non-empty, instead of collapsing to `noop`. cam5
  (10.77.9.65) ran preempt=full with `GEN=0` (HWE meta present) yet a stale `6.8.0-134-generic`
  (+ modules/-extra) and the generic meta still installed → the old prediction-only planner printed
  `noop:already-lowlatency` and silently left the single-kernel invariant (check `(k)`) violated
  until a hand purge. Never re-collapse the run=1 branch to a bare noop on the prediction; the
  observed set is the fail-closed signal (early-gate-pin doctrine: observe, don't predict).
- **`preempt=full` ≠ PREEMPT_RT.** Check `(ac)` keeps WARNing "not PREEMPT_RT" after STEP 1 — that
  is correct, not a regression. Only STEP 2 makes it read PREEMPT_RT.
- **Measure before escalating.** STEP 2 (a self-owned kernel) is only justified by data — the
  runbook's 10-min before/after emit-jitter + underrun measurement (existing `Streaming:` /
  `emit-1s:` / `genlock-fifo audit underruns=` journal lines, no new tools) is the go/no-go.
- The lowlatency-kernel install is reboot-class on `ro`-root appliances with the issue-295/547
  brick-hardening — one box at a time, canary first, in a window with NO live E2E, generic stays
  pinned in GRUB as rollback until preempt=full is proven. A supervisor deploy step, UNVERIFIED
  from a worktree code lane.
- Defect 2 (the process-wide `CPUSchedulingPolicy=fifo` put all threads FIFO-50 on the grab core,
  not `SCHED_OTHER`) is **CODE-FIXED** (see the per-thread SCHED_FIFO bullet in "What is DONE"),
  NOT the old runbook Step B any more. The stock per-thread FIFO raise moves NO thread between
  cores, so it needed no live emit measurement; the DEEPER rework that DOES move NDI emit threads
  off core 3 (the rejected Approach 2) is the piece that still needs a #728-style measurement and
  stays staged. The unit comment now describes the per-thread reality (no process-wide policy).
- **Appliance tmpfs layout — a provisioning install/regen must redirect cache + TMPDIR to the
  ROOTFS.** On the cam boxes `/var/cache` (512M), `/tmp` (100M) AND `/var/tmp` (50M) are ALL tmpfs;
  only `/` (rootfs, ~51G) is ample. The lowlatency-hwe install (~242MB archives + a new HWE image)
  overflows `/var/cache` and the in-postinst/`safe-grub-regen` initramfs build (~78MB initrd)
  overflows `/tmp`. The planner's `install-lowlatency`/`safe-grub-regen` commands therefore carry
  `apt-get install -o Dir::Cache::archives=/root/apt-tmp` + `export TMPDIR=/root/tmpbig` (both on the
  rootfs, `mkdir -p` first). `/var/tmp` is NOT a safe fallback — it is a 50M tmpfs, smaller than
  `/var/cache`. A `_apt`-sandbox `W: Download is performed unsandboxed as root` warning from a
  root-owned cache dir under `/root` (0700) is NON-blocking (the install succeeds; the whole step is
  root-run on a ro-root appliance), so `/root/apt-tmp` is the proven value — do not "fix" it into a
  tmpfs path. Supervisor-proven live on cam1/2/3 (issue-899 comment 2026-08-22), re-verified on cam4.
- **Static-anchor self-collision on a MULTI-WORD substring (a sharper #832 case).**
  `tests/rt_kernel_provision.rs` asserts the generated commands via `out.contains("apt-get install")`
  — a MULTI-WORD literal. Inserting an apt option BETWEEN the words (`apt-get -o ... install`) breaks
  that assertion even though `apt-get` and `install` are both still present. Keep any inserted flag
  AFTER the verb (`apt-get install -o Dir::Cache::archives=... -y ...`; apt accepts it identically).
  CRITICAL for the Tier-0 bash-source RED→GREEN check (no cargo compile here): verify the EXACT
  literal substrings each `.contains(...)` uses — multi-word included, with `grep -F "apt-get
  install"` — NEVER the tokens separately (`grep -q apt-get && grep -q install`), which passes while
  the real single-`assert!` is RED. A separate-token check is exactly how a self-collision slips past
  a "GREEN" bash-source pass (caught in #899 review, 2026-09-01).
- The OBS boxes hit the same inheritance class in the vendored genlock render tick (issue 1357): its
  affinity leaked too, which `SCHED_RESET_ON_FORK` does not reset, so the tick holds its pin only while
  it sleeps (`genlock-render-tick-pin.md`).
- **The per-thread FIFO raise MUST carry `SCHED_RESET_ON_FORK`, or children inherit it (issue 899,
  2026-09-15).** `set_current_thread_realtime(CaptureEmit)` runs on a tokio runtime WORKER thread. A
  Linux thread's scheduling policy is INHERITED across `clone()` unless `SCHED_RESET_ON_FORK` is set,
  so raising the worker to a bare `SCHED_FIFO` silently pushed every thread it later spawned — the NDI
  SDK threads (`ndis:recv`/`ndis:send`/`ndir:reconn`) and the tokio blocking pool — onto SCHED_FIFO
  prio 90 too, re-creating the process-wide-FIFO regression this ticket removed. It was
  NONDETERMINISTIC (measured on cam2: 19 FIFO threads at boot vs 1 after a restart — depends which
  worker spawns the NDI threads), so verify `(ah)` `rt_fifo_thread_ceiling` failed at random. The fix
  is the policy WORD `SCHED_FIFO | SCHED_RESET_ON_FORK` (`0x40000000`, supported by
  `sched_setscheduler` since Linux >= 2.6.32) via the pure `capture_emit_sched_policy_word()` (crate
  root, numeric literals so a std-only replica mirrors it under Tier-0; a `#[cfg(test)]` parity test
  ties the literals to `libc`). The hot-path thread keeps FIFO 90; its forked children fall back to
  SCHED_OTHER. NO thread moves cores. Any FUTURE per-thread FIFO raise site must ALSO use this policy
  word, never a bare `SCHED_FIFO`.
- **Follow-up (Finding A, still open):** the running fleet units on cam1/3/4/5/6/7 still carry the
  process-wide FIFO until each box is RE-PROVISIONED — nothing in `deploy-fleet.sh` refreshes
  `camera-box.service`; the M.2 migration re-provisions box by box, and check `(ah)` will only read
  the fixed per-thread reality after each box's re-provision + redeploy (a supervisor step).
