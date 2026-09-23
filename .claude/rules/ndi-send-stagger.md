---
paths:
  - "src/send_stagger.rs"
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

## What it does

`src/send_stagger.rs` (pure, crate-root, std-only):

- `camera_number_from_hostname("CAM7") == Some(7)`: the OS hostname set by `setup-device.sh` STEP 1.
  It is the same `gethostname(2)` value the NDI SDK publishes as the source's machine name. Anything
  else (`camera-box`, `CAM0`, `CAM1-lx`, >99) returns `None`.
- `slot_interval_us(genlock_fps, cap_num, cap_den)` is the SHORTER of the send and capture intervals.
  Genlock off returns 0.
- `send_offset_us(n, slot) = (n-1) × STAGGER_US`, clamped to `MAX_OFFSET_SLOT_PERCENT` (45 %) of
  the slot. `STAGGER_US = 1200`, so CAM1…CAM7 get 0, 1.2, … 7.2 ms.
- `plan(hostname, genlock_fps, cap_num, cap_den)` returns the offset plus the ONE startup line:
  - `NDI send stagger: camN offset=<us> us (#1242)` normally;
  - `... — genlock off, no emit grid to stagger` when genlock is off;
  - a WARN naming the hostname when there is no CAM<N> identity.
- `should_sleep(offset, frame_backlogged)` returns false when the frame came from an already
  non-empty V4L2 queue.
- `StaggerWindow` + `window_summary` give the per-5 s
  `#1242 send stagger: offset=… us, N slept / K skipped (frame already backlogged) …, max per-frame work W ms + offset O ms vs capture interval C ms`
  line. It is a WARN on any skip, or when `W + O ≥ C` (`OVER BUDGET`).

`src/main.rs` `run_capture_loop` wires it in:

1. `plan()` at startup, logged once.
2. At the top of each loop iteration, BEFORE `process_frame`, it takes the previous sleep:
   `mem::take(&mut last_stagger_sleep_ms)`. A corrupted buffer never reaches the callback, so taking
   it inside the callback would pair it with the wrong dequeue.
3. `frame_backlogged = queue_had_frame`, where queue_had_frame is computed from
   `idle_wait_ms(dequeue, previous_sleep)`.
4. `stagger_anchor = Instant::now()` right after the decimation gate passes.
5. After `capture_timecode_100ns` and BEFORE the starvation-repeat loop + `emit_one`: if
   `should_sleep`, it sleeps `remaining_sleep(offset, anchor.elapsed())`; otherwise it counts a skip.
   This is ONE delay per iteration, and the same site covers the production zero-copy send and the
   probe burn ring hand-off.
6. After the last `emit_one`, it records the per-frame work (callback time minus the sleep).
   `window_summary` rides the 5 s Streaming report (only when the offset is non-zero).

## The arithmetic (re-check before changing STAGGER_US)

- **Wire time:** a cambox NIC is 1 GbE (live: cam1 `enp3s0` = 1000 Mb/s), so one frame takes
  2.4–2.8 ms on the wire.
- **Drain:** the 2.5 G egress drains one frame in ~1.0–1.1 ms.
- **Train rate:** with spacing `s` the train runs at about `wire_time / s × 1 Gb/s`. That stays at or
  below 2.5 Gb/s once `s ≥ 0.96–1.12 ms`. At 1.2 ms the train peaks at 2.0–2.33 Gb/s.
- **Upper bound:** the send is synchronous on the capture thread, which is also the only V4L2
  drainer. The sleep shortens the next dequeue, and the `idle_wait_ms` add-back only stays sound
  while the sleep is under `BUFFERED_DEQUEUE_FRACTION` (0.5) of a CAPTURE interval. That is why the
  clamp uses the shorter interval. `src/lib.rs` has a `const _: () = assert!(...)` against the REAL
  fraction.
- **Fleet size:** seven cameras need `6 × s ≤ 7.5 ms`, so `s ≤ 1.25 ms`. More cameras on the same
  port would need a smaller `s`; otherwise the clamp makes the last cameras share the 7.5 ms slot.
- **Budget:** CAM7 must fit its per-frame work (hashes + conversion + the synchronous send) in
  16.67 − 7.2 ≈ 9.5 ms. The backlog skip guarantees the stagger never costs captured frames: a
  behind loop sends immediately and the skip is counted. The 5 s line shows the margin on every box.

## Invariants — do not break

- **The timecode never moves.** It is computed BEFORE the sleep, and `src/ndi.rs` does not know about
  the stagger (`tests/harness_send_stagger_1242.rs` pins both).
- **The emit grid never moves.** `decimation_gate.poll(wall_clock_ns(), …)` runs before the anchor,
  and the next boundary never derives from the send instant.
- **What DOES move is the arrival at strih:** up to 7.2 ms for CAM7. The per-run `[4i/8align]`
  (relative, floor-3 pins) re-equalises presentation. Until it has, the offset counts against the
  blocking delivery-spread budget (`SPREAD_THRESHOLD_MS`, 24 ms).
- **`gen_ts_ns` / the grab tee stay stamped at the emit instant, before the sleep.**
- **E2E blind spot:** in burn mode the capture thread only does `ring.submit` (the NDI encode runs on
  the burn thread). So an E2E run proves the send TIMING but NOT the production capture-loop budget.
  Read the production journal's 5 s stagger line for that.
- **Unaffected while the budget holds:** #944 emit-freeze (15 s), #945 capture wedge, #707 emit-1s
  buckets, the capture-stall WARN (the raw dequeue only gets shorter), and the self-heal triggers.
  A later camera with new capture-dropped / `#1145` drains / stagger skips is the budget, not the
  grabber.

## Verifying a change (Tier-0)

- The module is std-only: `rustc --edition 2021 --test -D warnings <copy of src/send_stagger.rs>`,
  run it, and `clippy-driver` the same copy.
- The harness reads only `src/main.rs` / `src/ndi.rs` text:
  `CARGO_MANIFEST_DIR=<worktree> rustc --edition 2021 --test tests/harness_send_stagger_1242.rs`,
  then run it from the worktree root. Put this in a script FILE (the worktree guard refuses the
  env-prefixed form inline).
- The lib.rs float const-assert shape compiles standalone (float compares are legal in a `const`
  item).
- `main.rs` itself compiles first on CI.

## Live acceptance (supervisor)

1. Deploy the camera-box binary to the fleet (never a cambox reboot).
2. Read each box's startup line: `journalctl -u camera-box | grep 'NDI send stagger'`.
3. Read CAM7's 5 s `#1242 send stagger:` line: 0 skipped and no `OVER BUDGET`. Check that its
   `Streaming:` captured fps / capture-dropped are unchanged.
4. Compare the `foh1_video ether2` `tx-drop-queue1-packet` delta over 30 min against the pre-change
   rate (31 bursts / 9.3 min, max +2552). It should be near zero.
5. Run two release E2E runs with no multi-frame copies/gaps burst.
6. Expect the `[4i/8align]` strih pins to shift by up to ~7 ms, since CAM7 is now the latest arrival.
