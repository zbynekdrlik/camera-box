---
paths:
  - "src/genlock_n1_depth.rs"
  - "src/genlock_grid_bench.rs"
  - "src/genlock_grid_bench_tests.rs"
  - "tests/genlock_relock_selection_parity.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
  - "src/probe/genlock.rs"
---

# The N==1 PIN-DERIVED DEPTH (issue 1367) — one depth after every restart

## What and where

A deep N==1 genlock input (the stream `NDI 2ME PGM`, 30-into-30, pin 987 ms) had two absorbing
depths. A strih OBS restart empties the FIFO; the release keeps its locked boundary, GAP-RESYNCs
onto the first post-restart frame at `base = ceil((pin − 1 µs) / interval)` frames (30), and each
startup-stall DUPLICATE stamp is presented one tick later on STEADY (+1 each). The settle-back
drain only fires above `ceil + 2`, so 30 / 31 / 32 were all stable. Live: four restarts landed on
32 / 31 / 32 / 31 — a whole-frame A/V step per restart at a constant pin.

The rule: settle to `target = base + 1` (the depth the healthy sender gap/dup tail produces
anyway — neutral there). Deeper → SHED one frame; shallower → HOLD one tick. Both share the drain
throttle (30 ticks). Active only on a DEEP source (`floor_frames + 2 ≤ base`, floor = `wall −
newest queued stamp`); a shallow source (cg feeds, imag cameras at 3 ms) is untouched.

| Piece | Rust authority | C (obs-source.c) | Also |
|---|---|---|---|
| scheduled instant | `n1_tick_wall_ns` | `genlock_n1_tick_wall_ns` (pure) + `genlock_n1_tick_wall_now` (reads `os_gettime_ns()` + `obs->video.video_time`) | |
| on the grid | `n1_tick_on_grid` (pure) + `n1_tick_is_on_grid` | `genlock_n1_tick_on_grid` (pure) + `genlock_n1_tick_is_on_grid` (`genlock_grid_floor_ns`), `GENLOCK_N1_ON_GRID_NS` 2 ms | both wrappers `return genlock_n1_tick_is_on_grid(tick_wall, interval) && …` |
| rounded depth | `n1_depth_frames` | `genlock_n1_depth_frames` | |
| deep guard | `n1_is_deep_source(floor)` | `genlock_n1_is_deep_source` | |
| SHED | `n1_shed_due` | `genlock_n1_shed_due`, called by the SOURCE wrapper `genlock_should_converge_phase` for `n < 2 && last_known_n < 2` | probe `ReleaseCadence::should_converge_phase`, bench |
| HOLD | `should_hold_n1_phase` | `genlock_n1_hold_due` via `genlock_should_hold_n1_phase`, at the head of the N==1 STEADY branch | counts `n1_grows=` on the audit line |

`genlock_phase_converge_due` is byte-identical to its pre-1367 text and
`genlock_backlog::should_converge_phase` code-identical (one comment added), both with
`if (n < 2) return false;`: every N>=2 decision is the old code path, proven by a 20 000-vector
comparison against a verbatim pre-1367 copy (`converge_decisions_are_byte_identical_to_before_1367`).

## The four things that went wrong first (do not undo them)

1. **Queue length is not depth.** Lowering the drain hysteresis to one frame churned (10 sheds/h,
   17–21 flips/h): a render tick later than the ~21.7 ms sender skew already holds the NEXT frame,
   so the queue reads one frame deep at the correct state. Depth = the presented AGE.
2. **The processing wall is not the tick.** `video_sleep` (obs-video.c) CATCHES UP a tick that
   overran by less than two intervals (`count = 1`, `video_time` = the next slot, no sleep), so no
   slot is lost. The age read at the processing wall of such a tick is one frame too deep for the
   whole overrun — a margin on the processing wall only moves the misfire band (review rounds 1–2).
   Read at the SCHEDULED instant (`video_time`, the `sys_time` `async_tick` passes down) the age has
   no lateness. An overrun of ≥ 2 intervals skips slots and `video_time` jumps with them, so the
   deeper depth read then is real.
3. **A shared edge limit-cycles.** Hold and shed on one depth edge cycled ~1100 each per hour when
   the tick phase sat on it. Both read the ROUNDED depth; shed needs `> target`, hold `< target`:
   a one-frame dead-band.
4. **A wall-clock step moves the scheduled tick off the grid** (review round 3). After a step of
   δ the ticks sit δ off the per-second grid and `genlock_next_deadline` slews them back only
   `GENLOCK_MAX_SLEW_NS` (2 ms) per tick; read there, a settled conveyor read one frame deep after
   a forward step of more than half a frame (a shed) and one frame shallow once back (a hold).
   Both halves act only while the tick is within 2 ms of a grid point and defer otherwise.
   Normal and caught-up late ticks are scheduled on their slot or at most 2 ms after it (a tick
   that overran the next slot by under 2 ms sleeps to that slot + the clamped 2 ms), so they are
   on the grid; at that exact edge the read order can defer one tick, never more.

Plus the 1 µs pin tolerance (a whole-frame pin like 1000 ms must not count one frame too many).

## Known limits (measured)

- **A tick held OFF the grid disables the rule.** A constant schedule phase beyond ±2 ms (a
  stress bound; a real tick is off the grid only while a wall step slews back) defers every N==1
  correction, so the conveyor behaves exactly like the pre-1367 code (bench: the ±10 ms constant
  phase sweep is identical with and without the rule). Inside ±2 ms at pins whose frame headroom is
  under 2 ms (999: 1 ms; 1000: 0) an EARLY tick still moves the release deadline a frame, but OBS
  never ticks early outside the last one or two ticks of a backward-step slew.
- **The settle-back drain still reads queue length** — owned by the drain, which the design kept
  unchanged, and proposed to the supervisor as a follow-up. A tick late by more than ~1.4 intervals
  inflates the queue by two frames and the drain sheds; before issue 1367 that was absorbing (30
  forever), now the hold regrows it within the throttle window, exactly one `n1_grows` per drain.
  That is a skip plus a duplicate per such tick (bench: 5–7 drains/h at a 60 ms late-tick tail,
  none at 45 ms or at the live 10–30 ms tail).

## Verification recipe (Tier-0, no cargo compile)

- Bench + authority: one `lib.rs` with `#[path]` mods for `genlock_grid`, `genlock_backlog`,
  `genlock_n1_depth`, `genlock_grid_bench`; `clippy-driver --edition 2021 --test -D warnings`, run.
- Probe mirror: the same plus `pub mod probe { #[path] pub mod genlock; }`.
- Parity gate: build a stub `camera_box` rlib from those three modules, compile
  `tests/genlock_relock_selection_parity.rs` with `--extern camera_box=…`, `CARGO_MANIFEST_DIR` +
  `CARGO_TARGET_TMPDIR` set. Mutation-proof it by pointing `CARGO_MANIFEST_DIR` at a scratch repo
  with a mutated obs-source.c (14 mutations, all RED at landing).
- The C wrappers (`genlock_n1_tick_wall_now`, `genlock_n1_tick_is_on_grid`, the routing, the hold
  call site) are not in the parity lift: lift them with stub `obs` / `os_gettime_ns` /
  `obs_source_t`, `#include` the real `obs-genlock-grid.h`, compile with `gcc -Wall -Wextra
  -Wformat=2 -Wconversion -Werror`, and drive a late tick (scheduled read inert, processing read
  sheds) and a wall step (20 ms off the grid defers, 1 ms off sheds).
- pwsh mirrors: check every `Escape('…')` literal against the `\s+`-squished C offline.

## Live acceptance (supervisor)

Full-bundle deploy on stream (and strih-lx, same vendored tree), then ≥ 6 strih OBS restarts with
the pin unchanged. After each settle the stream `NDI 2ME PGM` audit `ts_head_skew_ms` must return
to ONE value (≈ 31 frames at 987); `n1_grows=` / `converge_sheds=` may step by a few right after a
restart and must stay flat in steady windows. Then the release E2E A/V gate.
