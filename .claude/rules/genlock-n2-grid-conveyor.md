---
paths:
  - "src/genlock_n2_grid.rs"
  - "src/genlock_n2_grid_bench.rs"
  - "src/probe/genlock_n2_tests.rs"
  - "tests/genlock_n2_grid_parity_1367.rs"
  - "tests/genlock_n2_grid_wiring_1367.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
---

# The grid-exact N>=2 conveyor (issue 1367 slice D1, design 5879332205)

## What and why

Every strih camera is a 60 fps source in a 30 fps canvas (N = 2). Until D1 its release was the
boundary conveyor: each present re-anchored the locked boundary to the presented stamp, so WHICH
frame of each 60 fps pair went on air was whatever the lock picked, and the lock picked by ARRIVAL
(ACQUIRE / backlog relock by the frame nearest the latency among the arrived ones, GAP RESYNC by the
head). The #1049 shed's dead band held both parities. The camera arrival lag on strih-lx sits at
35-50 ms, straddling the 16.7 ms edges, so every strih OBS restart, lock or relock drew a new
33 / 50 / 67 ms presented age per camera (live 28.9.2026: every camera flipped between 50 and 67
within one session, `converge_sheds` 337-1487 per camera per day).

Now an N>=2 source presents, at render tick T, the stamp

    S*(T) = grid_floor(T − GENLOCK_N2_AGE_BASE_NS − pin, canvas_interval / N)

on the per-second SOURCE grid — a pure function of the tick and the pin. Every camera, every
restart, every relock lands on the same frame.

| Piece | Rust authority (`src/genlock_n2_grid.rs`) | C (`obs-source.c`) |
|---|---|---|
| fleet constant | `GENLOCK_N2_AGE_BASE_NS` 50 ms | `#define GENLOCK_N2_AGE_BASE_NS 50000000ULL` |
| source interval | `n2_source_interval_ns(canvas, n)` = canvas / n | `genlock_n2_source_interval_ns` |
| tick instant T | `n2_tick_ns(tick_wall, wall_now, canvas, on_grid)` | `genlock_n2_tick_ns` |
| target stamp | `n2_target_stamp_ns(T, pin_ns, canvas, n)` | `genlock_n2_target_stamp_ns` |
| the pick | `n2_select(queue, target, source_interval)` → `N2Pick { kind, index }` | `genlock_n2_select(source, target, si)` → `struct genlock_n2_pick` |
| the release | probe `ReleaseCadence::tick_n2_grid` | `genlock_release_tick_n2_grid`, called at the TOP of `genlock_release_tick` when `genlock_effective_source_multiple >= 2` |

The C block from `struct genlock_n2_pick {` to the end of `genlock_n2_select` is CONTIGUOUS and
pure; `tests/genlock_n2_grid_parity_1367.rs` lifts it verbatim (+ its four `#define`s) against the
real `obs-genlock-grid.h`.

## The rules the release follows (do not undo)

- **T is the SCHEDULED instant, snapped.** `genlock_n1_tick_wall_now(wall_now)` comes out a few µs
  EARLY; a bare floor would fall one slot back and target a frame already presented. On the grid
  (`genlock_n1_tick_is_on_grid`, ±2 ms) T is the nearest canvas grid point; off the grid (a wall
  step the render tick has not re-gridded yet) T is the canvas grid floor of the PROCESSING wall
  (never a future slot).
- **The pick:** the last frame of the leading queue run stamped ≤ S* + source_interval / 2 (the
  slack covers the 100 ns sender stamp floor, which sits up to 99 ns before its ns grid point, and
  the 1 µs capture offset). Every older frame is erased into `dropped_due`. ON_TARGET when that
  frame is within half a source interval of S*; EARLY when the target has not arrived and an older
  frame is the newest arrived one (any age — two intervals back is only reachable after an early
  or a burst); HOLD with nothing at or before the target.
- **An early present re-anchors NOTHING.** It counts `n2_early=` on the audit line, and the next
  tick targets S*(T + canvas) again. The boundary conveyor it replaces moved the camera one frame
  older on every late second frame until the converge shed pulled it back.
- **HOLD classification:** late when the locked boundary is set and `present_ts` (the reserve
  deadline) has passed it — as on the N==1 path; benign while unlocked (a cold start).
- **What the release still writes:** `genlock_locked_next_boundary_ns` = presented + canvas
  interval (the audit `locked=`, a later N==1 tick and the hold classification read it),
  `genlock_frames_consumed`, `last_frame_ts`, the video-delay tracker at the scheduled tick (the
  audio follows the now-deterministic presented age), `genlock_shallow_latch(…, false)` (an N>=2
  tick clears the N==1 shallow state, as the old N>=2 STEADY present did) and the audit call.
- **What an N>=2 source no longer touches:** the phase anchor, the backlog relock, the #859 drain,
  the #1049 converge shed and the #1161 ACQUIRE bracket. The per-tick erase down to the target
  bounds the queue by itself. Removed as provably dead: the #726 N>=2 STEADY multi-consume
  (`mature_deadline`) and the whole #1161 bracket (helper, `GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS`,
  `genlock_acquire_bracket_ticks` + its five clears, the log line, the Rust `relock_acquire_should_hold`,
  its tests, pin-rise sim and parity test). The setter's pin-RISE boundary zeroing stays (the N==1
  conveyor needs it). `genlock_phase_converge_due` stays: an N==1 tick whose post-erase re-measure
  reads n ≥ 2 can still reach it.
- **The ACQUIRE path stays for an inconclusive first tick** (one queued frame, no confirmed
  multiple): the N==1 conveyor presents one or two young frames, then the next tick measures N = 2
  and the grid takes over (a hold, then the target). The bench bounds the start-up at 2 ticks.
- **N==1 sources are byte-identical** (the stream `NDI 2ME PGM`, the shallow CG latch, imag's
  60-into-60): the only new statements on their path are the top `genlock_effective_source_multiple`
  call, whose latch write equals the one the STEADY branch made anyway.

## Pin arithmetic

Presented age (N = 2, 30 fps canvas) = 16.667 ms × ceil((50 + pin) / 16.667):

| pin (ms) | age |
|---|---|
| 1 … 16 (production 3) | 66.7 ms (4 source frames) |
| 17 … 33 | 83.3 ms |
| 34 … 50 | 100 ms |

One source interval of pin is exactly one frame (the target moves at pin = 16.67 k); a pin inside
the same band keeps the frame. `scripts/qr_align_pins.py` adds a measured present-age delta to the
current pin, so a one-frame delta (≥ 13.7 ms from pin 3) moves exactly one frame. KNOWN GAP (for
the main to decide, not changed in D1): its budget check models the resulting present age as
`arrival_floor + hold` with `arrival_floor = latency + mean head skew`, and on the grid conveyor
the head is always one source frame older than the target, so every camera reads ~86 ms and any
hold over ~8 ms reads over the 94 ms ceiling → BUDGET_BOUND soft-release instead of a pin.

## The budget: arrival lag vs 66.7 ms (measured 28.9.2026)

The newest queued frame's age at the audit tick (`head_skew − (depth + erased − 1) × 16.7 ms`,
30 793 samples of the seven program inputs, `2026-09-28 1*.txt`) brackets the arrival lag within
one slot: p50 49-50 ms on cam1/2/3/5/6/7, 33.7 ms on cam4; p99 52-67 ms; p99.9 67-200 ms (the
stall bursts). A target 66.7 ms old is missed only when the lag exceeds it: 0.024 % of samples in
the 75-95 ms band, 0.039 % in stalls ≥ 95 ms. The bench (`src/genlock_n2_grid_bench.rs`) replays
the per-camera histograms: n2_early 0.089 / 0.143 / 0.417 / 0.143 / 0 / 0.041 / 0.030 % (cam1..7),
0.123 % overall — over the 0.1 % budget on cam2/3/4, driven by the stall tail. Reported, NOT tuned:
`GENLOCK_N2_AGE_BASE_NS` is one fleet constant and changing it is the main's call.

The expected common-mode shift: the old mean presented age per camera (instantaneous, same
samples) was cam1 55.8, cam2 53.4, cam3 58.7, cam4 52.7, cam5 57.7, cam6 57.4, cam7 58.0 ms → all
66.7 ms: every camera's video +8.0…+14.0 ms later (mean +10.4, cam2 +13.3), once.

## Verification (Tier-0, no cargo)

- Authority: a replica `lib.rs` with `#[path]` mods for `genlock_grid` + `genlock_n2_grid`;
  `rustc --edition 2021 --test` + `clippy-driver --test -D warnings`.
- Bench + probe mirror: add `genlock_backlog`, `genlock_n1_depth`, the `#[cfg(test)]` bench and
  `pub mod probe { #[path] pub mod genlock; }` (its N>=2 tests are the `#[path]` child
  `src/probe/genlock_n2_tests.rs`). The probe sims tick on the per-second grid, up to 2 ms LATE,
  never early — the #401-era ±2 ms alternating slew puts every other tick 2 ms early, off the grid,
  which a real render tick never is.
- Parity: build a `camera_box` rlib from that replica, compile the test with `--extern`,
  `CARGO_MANIFEST_DIR` + `CARGO_TARGET_TMPDIR` at compile time. Mutation proof: a scratch tree with
  a mutated `obs-source.c` + a copy of `obs-genlock-grid.h` + the test and `tests/genlock_n1_lift/`,
  recompiled per mutant (9/9 RED at landing: slack, kind compare, pick index, age base, snap,
  off-grid wall, canvas grid, saturation, pin). Rust mutants of `genlock_n2_grid.rs` 10/10 RED on the
  module + bench + probe tests.
- The whole `obs-source.c` `gcc -fsyntax-only` (the `vendored-libobs-change-safety.md` recipe) and
  every std-only reader of it (`genlock_release_cadence`, `genlock_n2_grid_wiring_1367`,
  `genlock_shallow_depth_wiring_1367`, `genlock_audio_timecode_placement_1367`, …).
- pwsh anchors: every `Escape('…')` in both `windows-genlock*.yml` checked against the `\s+`-squished
  C offline — and against the OLD C, where each new one must fail.

Count anchors this slice moved (update them together): `genlock_n1_tick_wall_now(wall_now)` 5,
`converge_eligible = true;` 1, `genlock_video_delay_track(` 3 (definition + two present tails),
`genlock_shallow_latch(source, ` 2.

## Live acceptance (supervisor)

Full-bundle genlock deploy on strih-lx (libobs changed). Then:
1. Every strih camera's `genlock-fifo audit` line: `video_delay_ms=67` (66.7 ms), `n2_early=`
   present, `relocks=` and `converge_sheds=` flat, `dropped_due` +1 per tick (the erased pair frame).
2. Three strih OBS restarts: every camera back on 67 each time; the stream A/V level inside one
   level across the restarts (spread < 5 ms at a constant cambox state); the restart matrix
   strih-obs kind 3/3 PASS.
3. One hour: `n2_early` delta / ticks ≤ 0.1 % per camera — expect cam3 to exceed it from the stall
   tail (bench 0.42 %); report, do not widen the base.
4. Re-read the stream A/V level (expected video ≈ +10 ms later in common mode) and move the stream
   `NDI 2ME PGM` pin by the measured common mode as a recorded step (an N==1 deep source quantizes
   its depth in 33.3 ms steps, so a sub-frame shift may not be realizable through that pin).
