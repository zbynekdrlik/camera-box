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
  for a genuinely-missing package). Plan tokens: `install-lowlatency` → `verify-lowlatency-config`
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
- Defect 2 (the process-wide `CPUSchedulingPolicy=fifo` puts all threads FIFO-50 on the grab core,
  not `SCHED_OTHER`) is runbook **Step B** — a stock per-thread rework that needs a live emit
  measurement (#728-style), independent of the kernel choice; the unit comment is already corrected.
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
