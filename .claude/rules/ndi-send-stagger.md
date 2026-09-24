---
paths:
  - "src/send_stagger.rs"
  - "src/send_handoff.rs"
  - "src/ndi_send_thread.rs"
  - "src/frame_buffer_pool.rs"
  - "tests/harness_send_stagger_1242.rs"
  - "src/capture_stall.rs"
---

# Per-camera NDI SEND stagger (#1242)

## Why it exists

All camboxes emit on the same 60 Hz genlock grid, and on the splitter rig they all capture the
SAME HDMI signal, so their frames reached the NDI SDK within ~1 ms of each other. Each 1080p60
SpeedHQ frame is ~300–350 KB; the seven trains cross the 10 G trunks together and converge on
strih-lx's ONE 2.5 GbE uplink (`foh1_video ether2`, a USB RTL8156 adapter). The switch tail-drops
whole packet runs (`tx-drop-queue1` in `scripts/netcfg-audit.sh --check`), and a ~2500-packet run
costs 2–5 cameras ~380 ms of video at once: strih `recv-timing` n≈277, genlock FIFO underruns
(repeats), then converge sheds (gaps). The evidence chain is on issue 1242, and the netcfg side is in
`.claude/rules/netcfg-audit.md`.

Measured A/B on the live rig: stagger ON ≈ 48 ether2 drops/min (most minutes 0), stagger OFF
(1.7.0-dev.668) ≈ 790/min, max 2 110/min.

## History — why the wait moved off the capture loop

The first version (1.7.0-dev.666) SLEPT the offset inside the capture callback, right before the
synchronous send. The capture thread is the only V4L2 drainer, so the sleep was dead capture time.
A ~15 ms per-frame work spike plus CAM7's 7.2 ms offset overran the 16.7 ms slot: `OVER BUDGET`
windows, late frames, then a 112-relock burst on the strih-lx `NDI cam7` receiver (24.9.2026, 01:31).
1.7.0-dev.668 shipped it OFF (`STAGGER_ACTIVE=false`). The current design (Approach 1 on issue 1242)
moves the wait to the thread that owns the send, and the stagger is ON again.

## What it does

`src/send_stagger.rs` (pure, std-only) decides the OFFSET only:

- `camera_number_from_hostname("CAM7") == Some(7)`: the OS hostname set by `setup-device.sh` STEP 1.
  It is the same `gethostname(2)` value the NDI SDK publishes as the source's machine name. Anything
  else (`camera-box`, `CAM0`, `CAM1-lx`, >99) returns `None`.
- `slot_interval_us(genlock_fps, cap_num, cap_den)` is the SHORTER of the send and capture intervals.
  Genlock off returns 0.
- `send_offset_us(n, slot) = (n-1) × STAGGER_US`, clamped to `MAX_OFFSET_SLOT_PERCENT` (45 %) of
  the slot. `STAGGER_US = 1200`, so CAM1…CAM7 get 0, 1.2, … 7.2 ms.
- `plan()` = `plan_gated(STAGGER_ACTIVE, …)` returns the offset plus the ONE startup line:
  `NDI send stagger: camN offset=<us> us (#1242)`; `… — genlock off, no emit grid to stagger`; a WARN
  naming a non-CAM<N> hostname; and, with the switch off, `… offset=0 us (#1242) — stagger disabled
  (STAGGER_ACTIVE=false)`. `STAGGER_ACTIVE` is the one-constant rollback; both positions are tested.

`src/send_handoff.rs` (pure, std-only, generic over the frame) owns the WAIT:

- `SendJob { frame, timecodes, deadline }`: every timecode of one emitted iteration (the #1167
  starvation repeats earliest first, then the current frame) and ONE absolute deadline.
- `send_deadline(anchor, offset) = anchor + offset`. The anchor is the emit-gate decision instant as
  a monotonic `Instant`: a DanteSync wall-clock step cannot stretch the wait, and the per-frame work
  after the gate never shifts the send. Do NOT re-anchor to the wall-clock grid slot: the capture
  phase relative to the grid is arbitrary, and `max(arrival, slot + offset)` collapses the spread
  whenever a frame arrives later than its camera's offset.
- `HandoffSlot`: ONE slot, newest wins. `offer` never waits on the send thread. An UNTAKEN older job
  is handed back as `Offer::Replaced` so the caller recycles its buffer and counts it.
- `run_send_loop(slot, window, idle, &mut sink)`: take → if the deadline is ahead,
  `wait_until(deadline)` (returns early when a NEWER job arrives → the held one goes out at once,
  counted `expedited`, so a catch-up burst is sent in order and never overwritten) → send every
  timecode in order → record the window → `after_job` → `housekeeping`.
- `sleep_until(deadline)`: the same wait for the ring-fed E2E burn thread.
- `CaptureWindow` (capture side) + `SendWindow` (send side, shared through a mutex) +
  `window_summary` → the per-5 s line, now printed on EVERY box (CAM1 included):
  `#1242 send stagger: offset=… us, capture loop max work W ms vs capture interval C ms; send thread
  J job(s) (a waited for the offset / b already past it / c expedited by a newer frame), max lateness
  L ms, max send S ms, R replaced`. It is a WARN on:
  - `OVER BUDGET`: `W ≥ C` (the offset is no longer part of the capture budget);
  - `SEND OVER BUDGET`: one job's sends took `≥ C` (the send thread cannot keep up at that rate);
  - `LATE`: `L ≥ LATE_WARN_FRACTION (0.5) × C`;
  - `REPLACED`: any job replaced unsent (newest wins because the send thread fell behind).

`src/ndi_send_thread.rs` is the NDI glue (CI-compiled only; linux-gated):

- `NdiSendThread::spawn(sender, window, emit_heartbeat_ns, epoch)` moves the `NdiSender` onto an
  `ndi-send` thread, pinned to the isolated capture core and raised to SCHED_FIFO like the burn thread.
- The sink sends with the SYNCHRONOUS `send_frame_zero_copy` (`NDIlib_send_send_video_v2`), so the
  SDK is done with the buffer when the call returns. The buffer goes back to the pool only then:
  no use-after-free window. Do not switch to `send_video_async_v2` without keeping each buffer
  alive until the NEXT send call (the async contract).
- It stamps the #944 emit heartbeat only after a CONFIRMED send (a wedged or failing send now trips
  emit-freeze, exit 81) and runs the #297 re-announce after each job and on each 500 ms idle tick.
  A failed send logs the same `Failed to send frame:` line as before.
- `hand_off(data, info, timecodes, deadline)` copies the frame off the V4L2 mmap into a pooled
  buffer (`src/frame_buffer_pool.rs`, the #280 `BufferPool` moved to the crate root and re-exported
  from `probe::genlock`), offers the job, then `yield_now()`: the send thread shares the isolated
  core at the same SCHED_FIFO priority and cannot preempt the capture thread, so the yield lets it
  take the job (and park on the deadline) before the capture loop drains the next buffered frame.

`src/main.rs` `run_capture_loop` wires it in:

1. `plan()` at startup, logged once. The ndi-send thread is spawned whenever the burn thread did not
   take the sender.
2. In the callback, after the emit gate: `stagger_anchor = Instant::now()`, `send_deadline =
   send_deadline(stagger_anchor, send_stagger_offset)`.
3. `emit_one` only COLLECTS `production_timecodes` (plus the counters, the grab tee, the 30p tee).
   After the last `emit_one`, ONE `send_thread.hand_off(...)` carries all of them; a replacement is
   `capture_window.note_replaced`. `capture_window.note_work(callback time)` follows.
4. The #1131 buffered-queue signal reads the raw `dequeue_duration_ms` (nothing to add back).
5. The burn path puts the same `send_deadline` in `BurnJob`; the burn thread renders the QR, then
   `sleep_until(job.send_deadline)`, then sends, and records into the same `SendWindow`.
6. Shutdown: `send_thread.shutdown()` closes the slot (the last pending frame still drains) and joins.

## The arithmetic (re-check before changing STAGGER_US)

- **Wire time:** a cambox NIC is 1 GbE (live: cam1 `enp3s0` = 1000 Mb/s), so one frame takes
  2.4–2.8 ms on the wire.
- **Drain:** the 2.5 G egress drains one frame in ~1.0–1.1 ms.
- **Train rate:** with spacing `s` the train runs at about `wire_time / s × 1 Gb/s`. That stays at or
  below 2.5 Gb/s once `s ≥ 0.96–1.12 ms`. At 1.2 ms the train peaks at 2.0–2.33 Gb/s.
- **Fleet size:** seven cameras need `6 × s ≤ 7.5 ms`, so `s ≤ 1.25 ms`. More cameras on the same
  port would need a smaller `s`; otherwise the clamp makes the last cameras share the 7.5 ms slot.
- **Budget:** the capture thread now does only hashing + the frame copy per emitted frame; the SpeedHQ
  encode + send run on the send thread (same core). The send thread keeps up while one job's send
  stays under a capture interval (`SEND OVER BUDGET` is that signal). The offset only delays WHEN the
  send starts inside its slot; the next frame's deadline is one interval later, so the offset never
  eats the budget. A long send delays the next capture callback (same core, no preemption) — the V4L2
  queue buffers it, exactly as the old inline send did.

## Invariants — do not break

- **The timecode never moves.** Every timecode is computed on the capture thread before the hand-off
  and carried verbatim; `src/ndi.rs` knows nothing about the stagger (the harness pins both).
- **The emit grid never moves.** `decimation_gate.poll(wall_clock_ns(), …)` runs before the anchor,
  and the next boundary never derives from the send instant.
- **The capture callback never sleeps** (the harness asserts no `thread::sleep` between
  `capture.process_frame(` and the #945 heartbeat store).
- **One job per emitted iteration**: the starvation repeats and the current frame share one deadline
  and one slot entry. Handing them over one by one would let the single slot replace the repeats.
- **What DOES move is the arrival at strih:** up to 7.2 ms for CAM7. The per-run `[4i/8align]`
  (relative, floor-3 pins) re-equalises presentation. Until it has, the offset counts against the
  blocking delivery-spread budget (`SPREAD_THRESHOLD_MS`, 24 ms).
- **`gen_ts_ns` / the grab tee stay stamped at the emit instant.**
- **E2E blind spot:** in burn mode the capture thread only does `ring.submit` and the burn thread
  waits for the same deadline. An E2E run proves the send TIMING but NOT the production send thread
  (the sender lives on the burn thread there). Read the production journal's 5 s line for that.
- **Genlock off** (legacy, not on the fleet): the sender self-paces inside the send on the send thread,
  so a slow boundary wait can show as `REPLACED` instead of the old capture back-pressure.

## Verifying a change (Tier-0)

- `send_stagger.rs`, `send_handoff.rs` and `frame_buffer_pool.rs` are std-only:
  `rustc --edition 2021 --test -D warnings <file>`, run it, and `clippy-driver --edition 2021 --test
  -D warnings <file>` (+ `--crate-type lib`). The hand-off tests drive the REAL `run_send_loop` with a
  fake `SendSink`, so deadline, order, expedite, replacement and heartbeat semantics are proven
  without NDI. Their timing margins are generous (100 ms–1.5 s) for a loaded CI runner.
- `ndi_send_thread.rs` needs the NDI sender: build a replica crate root that `#[path]`-includes the
  REAL `ndi_send_thread.rs` + `send_handoff.rs` + `frame_buffer_pool.rs` next to stand-in `capture`
  / `ndi` / `affinity` modules and a stub `tracing` rlib (`#[macro_export]` error/warn/info that
  `format!` their args), then `rustc` + `clippy-driver -D warnings` + run one end-to-end hand-off.
- The harness reads only source text: `CARGO_MANIFEST_DIR=<worktree> rustc --edition 2021 --test
  tests/harness_send_stagger_1242.rs`, run it from the worktree root (put this in a script FILE —
  the worktree guard refuses the env-prefixed form inline).
- `main.rs` itself compiles first on CI; `cargo fmt --all --check` is the only local parse of it.

## Live acceptance (supervisor)

1. Deploy the camera-box binary to the fleet (service restart, never a cambox reboot).
2. Read each box's startup line: `journalctl -u camera-box | grep 'NDI send stagger'` →
   `camN offset=(N-1)×1200 us (#1242)`.
3. Read every box's 5 s `#1242 send stagger:` line for 30 min: no `OVER BUDGET` / `SEND OVER BUDGET`
   / `LATE` / `REPLACED`; note the capture-loop max work, the max lateness and the max send.
   Check that `Streaming:` captured fps / capture-dropped are unchanged.
4. Compare the `foh1_video ether2` `tx-drop-queue1-packet` rate over 30 min: it should be back near the
   stagger-ON level (≈ 50/min), not the stagger-OFF ≈ 790/min.
5. strih-lx `genlock-fifo audit` for every `NDI camN`: no relock burst (the 24.9. cam7 1→113).
6. Then a release E2E with no multi-camera copies/gaps burst window; expect the `[4i/8align]` strih
   pins to shift by up to ~7 ms, since CAM7 is now the latest arrival.
