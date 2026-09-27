---
paths:
  - "vendor/obs-studio/libobs/obs-video.c"
  - "src/genlock_render_tick_pin.rs"
  - "tests/genlock_render_tick_pin_1357.rs"
  - "tests/genlock_rt_pin.rs"
  - "scripts/lib/obs-box-baseline.sh"
  - "scripts/lib/obs-box-baseline-verify.sh"
---

# The genlock render-tick CPU pin (issue 1357)

The libobs graphics thread drives the genlock render tick (`video_sleep` -> `genlock_next_deadline`
in `vendor/obs-studio/libobs/obs-video.c`). #484 pinned it onto imag's reserved cores under a LOW
SCHED_FIFO priority. Issue 1357 found it harming strih-lx twice.

## What went wrong (live, strih-lx)

- 23.9.2026, with an rtprio grant: the pin put the tick on SCHED_FIFO prio 10, pinned to cores 10-11.
  28 NDI threads inherited FIFO and the pin, and the release E2E failed `[4i/8align]`.
- 27.9.2026, no grant: `isolated=[]` and `nohz_full=[]`, yet the log said "pinned to the isolated
  nohz_full cores". 40 threads carried `Cpus_allowed_list 10-11` while the rest were 2-11. Among them
  were 12 `ndi:io` and 7 unnamed `libobs: graphic` children.

Two causes:

1. The code read only `nohz_full` and fell back to a hardcoded `{10,11}`.
2. The thread stayed pinned for its whole life. A new thread inherits its creator's affinity and
   policy, and the graphics thread creates threads for its whole life: NDI receivers on
   activate/show, and driver threads.

## The rule now

- **The pin cores are `isolated` AND `nohz_full`** (`genlock_render_tick_pin_set`, pure).
  - Either list empty = no pin, and one line: `render-tick thread not pinned: no isolated cores`.
  - There is no fallback set.
  - The shared baseline grader's `affinity` row FAILs kernel isolation, so today NO OBS box pins.
    The code only matters on a box that someday has real isolated nohz_full cores.
- **The pin is held only while the tick SLEEPS.**
  - `video_sleep` calls `genlock_tick_pin_sleep_begin()` right before `os_sleepto_ns` (narrow the
    mask, then raise FIFO).
  - It calls `genlock_tick_pin_sleep_end()` right after (drop FIFO, then restore the saved startup
    mask).
  - A sleeping thread creates nothing, so no thread is ever created under the pin.
  - "Pin after spawning" is impossible here: there is no last spawn.
  - FIFO carries `SCHED_RESET_ON_FORK` as a second guard, the same lesson as the cam-box capture
    thread (`realtime-isolation.md`).
  - An unprivileged thread cannot clear that flag, so the leave call passes it too
    (`SCHED_OTHER | SCHED_RESET_ON_FORK`); a call without it fails EPERM.
- **Order matters.** Narrow before FIFO, drop FIFO before widening, so the tick is never FIFO on a
  shared core.
- **Failure handling:**
  - Startup decides once. A getaffinity or setaffinity failure leaves the tick unpinned (WARNING).
  - A FIFO failure keeps the pin and skips FIFO on every tick.
  - A per-tick failure restores what it can, disarms, and warns ONCE.
  - Never abort, never retry-loop.
- **Cost:** four syscalls and a migration per tick, only on a box that really pins.
- **Log lines drift-guard reads keep their wording:**
  - `render-tick thread set SCHED_FIFO prio N on the isolated core` -> `ok`
  - `could NOT set render-tick thread SCHED_FIFO` -> `failed` (DRIFT)
  - the new `not pinned: no isolated cores` -> `unpinned` (OK, by design)

## rtprio stays OFF on every box

- `obs_box_rtprio_off` (shared baseline, both setup scripts) removes every retired
  `95-<box>-genlock-rtprio.conf` grant: imag's #484 one and strih's retired 11c one.
- The grader row `rtprio` (after `affinity`) FAILs while one exists. verify-imag `(bb)` and
  verify-strih item 32 both grade it. verify-strih's own item 33 is gone.
- A grant "gated on isolated cores present" was rejected: the `affinity` row forbids kernel isolation,
  so the gate could never open on a box that passes the baseline. It would be dead code.

## Verification (Tier-0, no cargo)

`tests/genlock_render_tick_pin_1357.rs` is std-only. The Rust authority `src/genlock_render_tick_pin.rs`
is included via `#[path]`. It lifts the whole block between `/* camera-box issue 1357 render-tick pin
BEGIN` and its END marker and compiles it three ways:

1. **Parity.** `genlock_render_tick_pin_set` over hand-picked and xorshift vectors must equal
   `render_tick_pin_cores`. The Rust parser mirrors the SHIPPED C parser byte-for-byte on malformed
   input (it stops at the first non-separator, non-digit), so it does NOT reuse
   `affinity::parse_cpulist`.
2. **Syscall trace.** `pthread_setaffinity_np` / `pthread_getaffinity_np` / `sched_setscheduler`
   are macro-substituted with recording stubs, and `GENLOCK_SYSFS_ISOLATED` /
   `GENLOCK_SYSFS_NOHZ_FULL` (`#ifndef` seams) point at fixture files. The exact startup and
   per-tick sequences are asserted.
3. **Real threads.** A tick thread pins to the highest CPU of the test's own mask. A child created
   inside the window must inherit the pin (the CONTROL, proving the harness sees inheritance). A
   child created after the wake must carry the full mask and policy 0. The gate needs at least 2
   CPUs and FAILs, never skips, on a 1-CPU runner.

Run it: `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_render_tick_pin_1357.rs -o t && ./t`.

Gotchas:

- glibc's `CPU_SET`/`CPU_ISSET` assign their `int` argument to a `size_t` inside the macro, and
  `-Wconversion`'s sign half fires at every call site. The gate adds `-Wno-sign-conversion`; that is
  the libc macro, not the lifted code.
- The parity harness must take the address of the thread-side functions (`(void)&fn;`), or
  `-Werror=unused-function` rejects the block.
- Mutation proof: 12 mutants of the block were all killed (fallback restored, OR for AND, the
  sleep-end no-op, the leave order swapped, the reset flag dropped, startup left pinned, the
  affinity or FIFO failure mishandled, and the parser separator and cap).
- The real `obs-video.c` passes a syntax-only compile against the in-tree headers: the
  `obs-drm-output.md` recipe (a stub `obsconfig.h` + `-Ivendor/obs-studio/deps/libcaption`).

## Deploy + live check (supervisor)

obs-video.c is libobs, so strih-lx needs the Linux genlock bundle. Two checks after the deploy:

- The OBS log shows `render-tick thread not pinned: no isolated cores`.
- `for t in /proc/$(pgrep -x obs)/task/*; do awk '/Cpus_allowed_list/{print $2}' $t/status; done | sort | uniq -c`
  shows ONE mask. There is no 10-11 group any more.
