---
paths:
  - "src/genlock_n1_depth.rs"
  - "src/genlock_grid_bench.rs"
  - "tests/genlock_relock_selection_parity.rs"
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
| rounded depth | `n1_depth_frames` | `genlock_n1_depth_frames` | |
| deep guard | `n1_is_deep_source(floor)` | `genlock_n1_is_deep_source` | |
| SHED | `n1_shed_due` | `genlock_n1_shed_due`, called by the SOURCE wrapper `genlock_should_converge_phase` for `n < 2 && last_known_n < 2` | probe `ReleaseCadence::should_converge_phase`, bench |
| HOLD | `should_hold_n1_phase` | `genlock_n1_hold_due` via `genlock_should_hold_n1_phase`, at the head of the N==1 STEADY branch | counts `n1_grows=` on the audit line |

`genlock_phase_converge_due` / `genlock_backlog::should_converge_phase` are BYTE-IDENTICAL to their
pre-1367 text (`if (n < 2) return false;`): every N>=2 decision is the old code path, proven by a
20 000-vector comparison against a verbatim pre-1367 copy (`converge_decisions_are_byte_identical_to_before_1367`).

## The three things that went wrong first (do not undo them)

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

Plus the 1 µs pin tolerance (a whole-frame pin like 1000 ms must not count one frame too many).

## Known limits (measured, documented, not bugs)

- **Early schedule phase past the pin's headroom.** An EARLY tick moves the release deadline
  (`wall − reserve`, floored to the grid) one frame earlier once the phase exceeds
  `ceil(pin / I) · I − pin`; the resync depth is then `base + 1` and each sender gap/dup costs a
  hold/shed pair (bench: pins 963/999 at a sustained −10 ms). OBS never ticks early except the few
  ticks after a BACKWARD wall step (`GENLOCK_MAX_SLEW_NS` 2 ms/tick). Late phases are safe at every
  deep pin (`a_late_schedule_phase_never_corrects_any_deep_pin_1367`).
- **The drain still reads queue length.** A tick late by > ~1.4 intervals inflates the queue by
  two frames and the untouched settle-back drain sheds; before issue 1367 that was absorbing (30
  forever), now the hold regrows it within the throttle window — exactly one `n1_grows` per drain
  (bench: ~5/h at a 60 ms late-tick tail, 0 at 45 ms and at the live 10–30 ms tail).

## Verification recipe (Tier-0, no cargo compile)

- Bench + authority: one `lib.rs` with `#[path]` mods for `genlock_grid`, `genlock_backlog`,
  `genlock_n1_depth`, `genlock_grid_bench`; `clippy-driver --edition 2021 --test -D warnings`, run.
- Probe mirror: the same plus `pub mod probe { #[path] pub mod genlock; }`.
- Parity gate: build a stub `camera_box` rlib from those three modules, compile
  `tests/genlock_relock_selection_parity.rs` with `--extern camera_box=…`, `CARGO_MANIFEST_DIR` +
  `CARGO_TARGET_TMPDIR` set. Mutation-proof it by pointing `CARGO_MANIFEST_DIR` at a scratch repo
  with a mutated obs-source.c (11 mutations, all RED at landing).
- The C wrappers (`genlock_n1_tick_wall_now`, the routing, the hold call site) are not in the
  parity lift: lift them with stub `obs` / `os_gettime_ns` / `obs_source_t` and `gcc -Wall -Wextra
  -Wformat=2 -Wconversion -Werror`, and drive a late tick (scheduled read inert, processing read
  sheds).
- pwsh mirrors: check every `Escape('…')` literal against the `\s+`-squished C offline.

## Live acceptance (supervisor)

Full-bundle deploy on stream (and strih-lx, same vendored tree), then ≥ 6 strih OBS restarts with
the pin unchanged. After each settle the stream `NDI 2ME PGM` audit `ts_head_skew_ms` must return
to ONE value (≈ 31 frames at 987); `n1_grows=` / `converge_sheds=` may step by a few right after a
restart and must stay flat in steady windows. Then the release E2E A/V gate.
