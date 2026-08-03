---
paths:
  - "src/capture_wedge.rs"
  - "src/painter_wedge.rs"
  - "src/main.rs"
  - "src/probe/run.rs"
  - "src/probe/painter.rs"
  - "src/probe/qpsk_emit.rs"
---

# Heartbeat wedge-watchdog — the reusable recipe for a blocking-hardware call that can hang forever (#945, #936)

## The recurring failure shape

A thread's ONLY job per loop iteration is one blocking hardware/kernel syscall (V4L2
`VIDIOC_DQBUF`, a DRM `page_flip()` ioctl, ...). Under a genuine driver/hardware fault the kernel
can park that thread in `TASK_UNINTERRUPTIBLE` ("D state") — by Linux kernel design **no signal,
not even SIGKILL, can preempt a D-state thread until the blocking call itself returns.** This has
now hit TWICE in this codebase on two completely different call sites:

- **#945** (2026-08-02): a `-71` USB isochronous fault wedged `VideoCapture::process_frame`'s
  `VIDIOC_DQBUF` dequeue — 48 CONSECUTIVE minutes of total silence, every existing in-loop
  observability check (stats tick, `capture_stall`, `capture_rate_health`/`selfheal`) frozen along
  with it, because they all only run AFTER the blocking call returns.
- **#936** (2026-08-03): the SAME class of bug on `KmsPresenter::present()`'s `page_flip()` ioctl —
  the painter survived both SIGTERM and a follow-up SIGKILL escalation (`e14bfc432`) while wedged.

**If you find a THIRD blocking-hardware call site that can hang, do NOT invent a new mitigation —
apply this exact recipe.** Wrapping the ioctl in a helper-thread + bounded join was considered and
REJECTED both times: the owning type usually isn't cheaply `Send`+`Sync` across a helper thread,
and it doesn't beat the external-watchdog approach on detection speed anyway.

## The recipe (4 pieces, always in this shape)

1. **A shared `Arc<AtomicU64>` heartbeat**, stamped with `start.elapsed().as_nanos() as u64`
   (monotonic, NOT wall-clock) by the loop **immediately after** the blocking call returns —
   unconditional on the call's Ok/Err outcome or any "was this interesting" filter. That
   unconditional stamp is the discriminator: a genuine no-signal condition (unplugged HDMI, no
   frames) still returns from the syscall regularly, so the heartbeat keeps advancing; only a
   TRUE wedge (the syscall never returning at all) lets it go stale.
2. **A pure decision module** (`src/capture_wedge.rs` is the canonical one — REUSE its
   `evaluate_wedge(seconds_since_last_progress, threshold_s) -> WedgeVerdict` and `WedgeVerdict`
   enum verbatim for a new site; the threshold math is fully generic, nothing capture-specific is
   baked in. `src/painter_wedge.rs` mirrors this exact non-duplication choice). Each NEW site gets
   its own tiny module with just: a threshold constant (justify it against whatever
   ALREADY-EXISTING internal timeout the site has — see below), a distinct exit code constant, and
   a `..._message(seconds, threshold) -> String` pure formatter naming the ticket number, the
   CRITICAL keyword, and the exit code (so journal/forensics greps never confuse two different
   wedge events — the `.claude/rules/self-heal-frozen-leg-attribution.md` discipline).
3. **A genuinely SEPARATE watchdog thread** (can never itself be blocked by the same call) that
   polls the heartbeat on an interval well inside the threshold, breaks its own loop the instant it
   sees `stop`/`running` go false (so normal shutdown is never misreported as a wedge — check
   `stop` BEFORE computing anything, not after), and on a real wedge: logs the CRITICAL message via
   `tracing::error!` then calls `std::process::exit(THE_DISTINCT_CODE)` immediately. There is no
   graceful in-process teardown path available — the wedged thread cannot be joined, so exiting
   from the watchdog thread (which the kernel CAN still terminate, since it isn't the stuck one) is
   the most the process can do. Recovery is external: `systemd Restart=always` where the unit has
   it (`cam2-painter.service`, `camera-box.service`), or the calling script's own retry.
4. **Threshold sizing — always relate it to an EXISTING internal timeout on the same call path if
   one exists**, never pick a number in isolation. `#936`'s `PAINTER_WEDGE_THRESHOLD_S = 3.0` is
   ~6x `KmsPresenter::wait_flip_complete()`'s own 500ms event-poll timeout specifically so this
   watchdog never double-reports a stall that mechanism ALREADY handles (that inner timeout returns
   an `Err` and unwinds the thread cleanly well under 1s) — this watchdog exists only for the
   HARDER case: a block that happens before the inner timeout's own code is ever reached (e.g. in
   the ioctl issuance itself, not the event wait after it). `#945`'s `CAPTURE_WEDGE_THRESHOLD_S =
   25.0` is sized against the existing 5s "Streaming:" stats-tick cadence (~5 ticks) instead, since
   that call site has no comparable inner timeout.

## A gotcha specific to a THRESHOLD test on a `pub const`

A runtime `assert!(SOME_CONST >= X, "...")` test trips
`clippy::assertions_on_constants` under `cargo clippy --all-targets -- -D warnings` (this repo's
CI gate) because the compiler can already prove the assertion's truth value at compile time. Move
that exact guarantee to a `const _: () = assert!(SOME_CONST >= X);` placed right after the
constant's own definition instead — it is STRONGER (fails the BUILD, not just a test run) and
clippy-clean. Do not write a runtime test for a compile-time-known fact about a `pub const`.

## Wiring cost when the guarded function's signature changes

Adding the heartbeat parameter to a shared function (`run_painter`, `process_frame`) means
updating EVERY call site — check `run_painter`'s two callers (`run()` the Phase-1 loopback AND
`run_paint_only()` the rig path) both got the SAME watchdog wiring for symmetry, not just the one
site the live incident happened to hit; an asymmetric fix (only the affected path gets protected)
leaves the other call site silently exposed to the identical bug.
