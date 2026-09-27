---
paths:
  - "vendor/obs-studio/libobs/obs-genlock-render-tick-pin.h"
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

The pin lives in `vendor/obs-studio/libobs/obs-genlock-render-tick-pin.h`. obs-video.c includes it
ONCE, inside its Linux guard. The header holds static state for the one graphics thread, so no other
file may include it. obs-video.c keeps three calls: `genlock_pin_render_tick_thread()` at startup and
the two `video_sleep` calls.

- **The pin cores are `isolated` AND `nohz_full`** (`genlock_render_tick_pin_set`, pure).
  - Either list empty = no pin, and one line: `render-tick thread not pinned: no isolated cores`.
  - There is no fallback set.
  - The shared baseline grader's `affinity` row FAILs kernel isolation, so today NO OBS box pins.
    The code only arms on a box built with real isolated nohz_full cores on purpose.
- **The pin is held only while the tick SLEEPS.**
  - `video_sleep` calls `genlock_tick_pin_sleep_begin(t)` right before `os_sleepto_ns(t)`: narrow the
    mask, then raise FIFO. It returns whether it pinned.
  - It calls `genlock_tick_pin_sleep_end(tick_pinned)` right after: drop FIFO, then restore the saved
    startup mask.
  - A sleeping thread creates nothing, so no thread is ever created under the pin.
  - "Pin after spawning" is impossible here: there is no last spawn.
  - A late tick (`t` already past) does not sleep, so it skips the pin: no syscalls.
  - FIFO carries `SCHED_RESET_ON_FORK` as a second guard, the same lesson as the cam-box capture
    thread (`realtime-isolation.md`).
  - An unprivileged thread cannot clear that flag, so the leave call passes it too
    (`SCHED_OTHER | SCHED_RESET_ON_FORK`). A call without it fails EPERM.
  - The flag stays on the thread. A child it creates therefore starts at nice 0, which is harmless:
    the tick itself runs at nice 0.
- **What the pin buys is honest and small.** Only the wake-up happens on a quiet tickless core under
  FIFO. The render work after it runs SCHED_OTHER on the shared process mask. The cost is four
  syscalls and a migration per tick, and only on a box that pins.
- **Order matters.** Narrow before FIFO, drop FIFO before widening, so the tick is never FIFO on a
  shared core.
- **Failure handling:**
  - Startup decides once. A getaffinity or setaffinity failure leaves the tick unpinned (WARNING).
  - A FIFO failure keeps the pin and skips FIFO on every tick.
  - A per-tick failure (enter or leave), or a failed startup restore, restores what it can,
    disarms, and warns ONCE. Later ticks make no syscalls.
  - When that restore fails too, the thread may stay on the pin cores (and FIFO). The disarm then
    logs ONE `LOG_ERROR` naming what may have stayed, never the reassuring "continuing SCHED_OTHER on
    the process mask" warning.
  - Never abort, never retry-loop.
- **Log lines drift-guard reads (`genlock_rt_pin_from_log`) keep their wording:**
  - `render-tick thread set SCHED_FIFO prio N on the isolated core` -> `ok`
  - `could NOT set render-tick thread SCHED_FIFO` -> `failed`. Since issue 1357 this grades **OK**,
    not DRIFT: no box grants rtprio, so a pinned SCHED_OTHER tick is expected. The #572 DRIFT is
    retired.
  - `not pinned: no isolated cores` -> `unpinned` (OK, by design)
  - `could NOT pin render-tick thread` / `could NOT read the render-tick thread's CPU mask` ->
    `pin_failed` (OK, reported with its reason: the tick runs unpinned)

## rtprio stays OFF on every box

- `obs_box_rtprio_off` (shared baseline, both setup scripts) removes every retired
  `95-<box>-genlock-rtprio.conf` grant: imag's #484 one and strih's retired 11c one.
- The grader row `rtprio` (after `affinity`) FAILs while one exists. It also FAILs while the RUNNING
  OBS still holds a non-zero realtime soft limit (`/proc/<obs>/limits`): a grant removed after OBS
  started stays in force until OBS restarts in a fresh session.
  - The gather emits `obs_running` (empty when `pgrep` is missing) and `obs_rtprio_limit`.
  - No OBS running grades nothing there. A running OBS whose limit cannot be read, or no `pgrep`,
    FAILs: unreadable is never a pass.
- verify-imag `(bb)` and verify-strih item 32 both grade the row. verify-strih's own item 33 is gone.
- A grant "gated on isolated cores present" was rejected: the `affinity` row forbids kernel
  isolation, so the gate could never open on a box that passes the baseline. It would be dead code.

## Verification (Tier-0, no cargo)

`tests/genlock_render_tick_pin_1357.rs` is std-only. The Rust authority `src/genlock_render_tick_pin.rs`
is included via `#[path]`. It lifts the header's block between `/* camera-box issue 1357 render-tick
pin BEGIN` and its END marker and compiles it three ways:

1. **Parity.** `genlock_render_tick_pin_set` over hand-picked and xorshift vectors must equal
   `render_tick_pin_cores`. The Rust parser mirrors the SHIPPED C parser on malformed input (it
   stops at the first non-separator, non-digit), so it does NOT reuse `affinity::parse_cpulist`.
   It saturates at `CPU_SETSIZE` where the C stops accumulating; the output is the same, and there
   are no equivalent mutants left.
2. **Syscall trace.**
   - `pthread_setaffinity_np` / `pthread_getaffinity_np` / `sched_setscheduler` are
     macro-substituted with recording stubs.
   - `GENLOCK_SYSFS_ISOLATED` / `GENLOCK_SYSFS_NOHZ_FULL` (`#ifndef` seams) point at fixture files.
   - `h_fail_aff_at` / `h_fail_sched_at` fail the Nth call: startup trial = 1, startup restore = 2,
     first tick enter = 3, first tick leave = 4. `h_sticky` makes every later call fail too (a
     restore that keeps failing).
   - A prelude `os_gettime_ns` stub (`h_now`) drives the late-tick case.
   - The exact startup and per-tick sequences are asserted, including the disarm path.
3. **Real threads.** A tick thread pins to the highest CPU of the test's own mask. A child created
   inside the window must inherit the pin (the CONTROL, proving the harness sees inheritance). A
   child created after the wake must carry the full mask and policy 0. The gate needs at least 2
   CPUs and FAILs, never skips, on a 1-CPU runner.

Run it: `CARGO_MANIFEST_DIR=<wt> rustc --edition 2021 --test tests/genlock_render_tick_pin_1357.rs -o t && ./t`.

Gotchas:

- glibc's `CPU_SET`/`CPU_ISSET` assign their `int` argument to a `size_t` inside the macro, and
  `-Wconversion`'s sign half fires at every call site. The gate adds `-Wno-sign-conversion`; that is
  the libc macro, not the lifted code.
- Keep the block's helpers `static inline`, so a harness that uses only the pure decision does not
  fail `-Werror=unused-function`.
- Mutation proof: 19 mutants of the header block were all killed, run on a scratch copy. The list:
  - the fallback restored, OR for AND;
  - the sleep-end no-op, the leave order swapped, the reset flag dropped, startup left pinned;
  - the affinity or FIFO startup failure mishandled;
  - disarm keeping `armed`, disarm skipping the restore, a failed restore logged as a warning,
    begin/end ignoring a failure;
  - the late-tick skip removed or inverted;
  - the parser separator and cap, and the silent not-pinned line.
- The real `obs-video.c` + the header pass a syntax-only compile against the in-tree headers: the
  `obs-drm-output.md` recipe (a stub `obsconfig.h` + `-Ivendor/obs-studio/deps/libcaption`).
- `clippy-driver --edition 2021 --test -D warnings` over the std-only test files caught a dead
  helper (`run_block` in `strih_provision_pure_functions.rs`) left behind when its last caller moved.
  Run it on every std-only test file you touch.

## Deploy + live check (supervisor)

The header is part of libobs, so strih-lx needs the Linux genlock bundle. Two checks after the deploy:

- The OBS log shows `render-tick thread not pinned: no isolated cores`.
- `for t in /proc/$(pgrep -x obs)/task/*; do awk '/Cpus_allowed_list/{print $2}' $t/status; done | sort | uniq -c`
  shows ONE mask. There is no 10-11 group any more.
