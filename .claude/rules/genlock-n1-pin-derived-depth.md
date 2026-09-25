---
paths:
  - "src/genlock_n1_depth.rs"
  - "src/genlock_n1_depth_tests.rs"
  - "src/genlock_grid_bench.rs"
  - "src/genlock_grid_bench_tests.rs"
  - "tests/genlock_relock_selection_parity.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
  - "src/probe/genlock.rs"
  - "src/probe/genlock_n1_tests.rs"
  - "src/genlock_shallow_av_bench.rs"
  - "tests/genlock_shallow_depth_wiring_1367.rs"
  - "tests/genlock_shallow_depth_parity_1367.rs"
  - "tests/genlock_n1_lift/mod.rs"
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
newest queued stamp`). A shallow source (cg feeds, imag cameras at 3 ms) has its own PER-LOCK
depth since ROZHODNUTÉ 5827497952 — see the section at the end.

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
- Probe mirror: the same plus `pub mod probe { #[path] pub mod genlock; }` (its N==1 tests live in
  `src/probe/genlock_n1_tests.rs`, a `#[path]` child of `genlock`).
- Parity gate: build a stub `camera_box` rlib from those three modules, compile
  `tests/genlock_relock_selection_parity.rs` with `--extern camera_box=…`, `CARGO_MANIFEST_DIR` +
  `CARGO_TARGET_TMPDIR` set. Mutation-proof it by pointing `CARGO_MANIFEST_DIR` at a scratch repo
  with a mutated obs-source.c AND a copy of `obs-genlock-grid.h` (the on-grid case `#include`s it via `CARGO_MANIFEST_DIR`; without it every mutation reads RED for the wrong reason) (17 mutations, all RED at landing). The source-side
  `genlock_n1_tick_is_on_grid` has its own parity case against the REAL `obs-genlock-grid.h`.
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

## The SHALLOW per-lock depth (ROZHODNUTÉ 5827497952)

**Why.** A shallow N==1 source (resolume `sp-*_video`, strih-lx `CG-obs`, pin 3 ms, base 1) is
never deep, so the pin rule above never acted and its depth FLOATED with arrival: live
`sp-slow_video` stepped 66 / 100 / 133 ms between 5 s audits, and the audio (which follows the
video delay, `genlock-audio-pairing.md`) was re-placed 72 times in 42 min — the songplayer gate's
33 ms dropout.

**The rule.** After each LOCK, measure the ARRIVAL FLOOR (the ROUNDED age of the newest queued
frame at the tick's SCHEDULED instant, `n1_depth_frames(tick_wall, newest)`) over
`N1_SHALLOW_SETTLE_TICKS` (90) on-grid N==1 PRESENT ticks, then latch
`D = max(base, floor_max) + 1` (`n1_shallow_track(&mut ShallowDepth, ShallowTick)` /
`n1_shallow_target_frames(base, floor_max, deep, min_latency)`). D holds until the next relock
(or a re-measure, below); the same one-frame hold / shed keeps the presented depth on it
(`n1_shallow_hold_due` / `n1_shallow_shed_due`, the shared #859 throttle, the one-frame dead band).

| Piece | Rust (`src/genlock_n1_depth.rs`) | C (`obs-source.c`) |
|---|---|---|
| latch target + imag cap | `n1_shallow_target_frames(base, floor_max, deep, min_latency)` → `(D, capped)`, `D = 0` when capped | `genlock_n1_shallow_target_frames(…, deep, min_latency_box, bool *capped)` |
| relock gap | `n1_shallow_gap_is_relock` (≥ 1 s) | `genlock_n1_shallow_gap_is_relock`, `GENLOCK_N1_SHALLOW_RELOCK_GAP_NS` |
| window + re-measure | `n1_shallow_rearm` / `n1_shallow_track` (`ShallowDepth` incl. `over_ticks` + `deep_ticks`, per-tick `ShallowTick`) | `genlock_n1_shallow_rearm` / `genlock_n1_shallow_track` (the seven `genlock_shallow_*` state fields) |
| the window's deep verdict | `n1_shallow_window_deep` (strict majority) | `genlock_n1_shallow_window_deep` |
| the present-tail latch | — | `genlock_shallow_latch(source, tick_wall, wall_now, interval, reserve_ms, relock)` (computes `deep`, calls the tracker, logs) |
| governs (not deep) | `n1_shallow_governs` | `genlock_n1_shallow_governs` + the source wrapper `genlock_n1_shallow_governs_now` |
| SHED / HOLD | `n1_shallow_shed_due` / `n1_shallow_hold_due` | ORed next to the deep halves in `genlock_should_converge_phase` / `genlock_should_hold_n1_phase` |

What carries it (do not undo):
- **The rounded floor, not a raw `ceil`.** Stamps and scheduled ticks share the per-second grid, so
  the rounded newest-frame age already IS `ceil(arrival lag / interval)`; a raw `ceil` of the ns
  value jumps a frame on a 1 ns phase. The max over the window absorbs a late tick (a late tick only
  UNDER-reads the floor at the scheduled instant).
- **Sampled at the PRESENT TAIL only**, after the audio tracker, through the one helper
  `genlock_shallow_latch` (it reuses `genlock_delay_tick_wall`, so
  `genlock_n1_tick_wall_now(wall_now)` stays count-3; the helper keeps `genlock_release_tick`
  from growing). A tick with an empty queue never reaches
  `genlock_release_tick`, so a hold/underrun has no floor to read anyway.
- **Relock = ACQUIRE (`genlock_shallow_relock = boundary == 0` at the top of the tick) or a GAP RESYNC
  whose missing-stamp gap is ≥ 1 s** (a sender restart; a single lost frame is not). A pin change
  re-arms it in `obs_source_set_genlock_latency_ms` (a new base) — only for an N==1 source
  (`genlock_last_known_n < 2`; an N>=2 source's state is cleared anyway). During the new window
  the OLD D is still maintained, so a relock that finds the same floor changes nothing — video or
  audio.
- **A rising arrival re-measures without a relock** (review round 1): a latched source whose
  floor sits AT or OVER D for a whole window (`over_ticks` reaching 90 on-grid ticks) opens a new
  window and latches the deeper D; the audio then slews once. A floor that only touches D for a
  few ticks resets `over_ticks` and changes nothing.
- **An N==1 source with no depth, no window and no cap opens a window by itself** — a source that
  became N==1 without an ACQUIRE (its canvas-rate ratio changed) still gets a D.
- **Deep sources keep the pin rule.** The shallow halves act only while `!n1_is_deep_source`, and a
  deep source LATCHES the pin rule's own `base + 1` (the `deep` flag of the target), never a
  floor-derived D, so the audio lock is harmless there (2ME PGM carries no audio anyway). The deep
  flag is the window's strict MAJORITY (`deep_ticks * 2 > window_ticks`, review round 2): read on
  the one latch tick, a startup stall still running there would latch `floor_max + 1` (~100 ms too
  deep for the audio) on a deep source. An N>=2 tick clears the whole state (`last_known_n < 2`
  gates it). An UNKNOWN N (`last_known_n = 0` after an ACQUIRE / GAP RESYNC, until
  `genlock_effective_source_multiple` re-confirms it — normally the same or the next tick) reads as
  N==1: an N>=2 source then opens a window for that tick, which only freezes its audio tracker
  (PENDING) until the next tick clears it. Harmless, documented at `genlock_shallow_latch`.
- **The #859 queue-length drain stays out while D governs** (`drain_eligible = false` after the N==1
  mark): a queue-length read would shed a correctly deep conveyor on a wide arrival spread, and the
  shallow shed already covers depth > D.
- **imag (item 4):** libobs has no box identity (imag and strih share input names), so the box
  declares it: `setup-imag.sh` step 13 writes `~/.camera-box/genlock-min-latency`, read ONCE by
  `genlock_min_latency_box()`. A floor that asks for more than `base + 1` there is REPORTED and NOT
  applied (review round 1 — a forced shallower D would churn hold/shed against the arrival): the
  latch stores NO depth (`shallow_depth=0`), the rule never holds or sheds that input, the #859
  drain stays on, no auto re-measure churns it until a real relock, and the report is
  `genlock-shallow-lock … wanted_frames=<D> capped=1` at LOG_WARNING + `shallow_capped=1`. A box
  without the marker (every Windows box, strih-lx) reads false. The marker fails OPEN when absent,
  so it is written in TWO places — `setup-imag.sh` step 13 and the fleet deploy's imag leg (step 5a,
  as the desktop user from `getent`, before the supervised restart that loads the new libobs) — and
  gated twice by `verify-imag.sh`: check (bc) FAILs an absent or unreadable marker before check
  (o)'s OBS restart, and check (bd), after that restart, requires the NEW OBS log to say
  `genlock-min-latency: ON` (libobs reads the marker once per process, so the file alone does not
  prove the loaded libobs honours it). Note for the owner: on imag an uncapped governed input is
  held at `base + 1` = 2 frames, one frame deeper than a free conveyor whose floor sits at or below
  base; that is the decided formula, recorded on the ticket.

Observability: one `genlock-shallow-lock '<src>': depth_frames= floor_max_frames= base_frames=
wanted_frames= latency_ms= capped=` line per latch, and `shallow_depth= shallow_capped= shallow_latches=` on the
audit line (audit-line-only).

**Verification (Tier-0).** Authority + the grid-bench port + the two-clock A/V bench: one harness
`lib.rs` with `#[path]` mods for `genlock_grid`, `genlock_backlog`, `genlock_n1_depth`,
`genlock_grid_bench`, `genlock_audio_pairing` (the shallow bench is a `#[path]` child of the audio
pairing bench, which is a child of `genlock_audio_pairing`); `rustc --test` + `clippy-driver --test
-D warnings`. The parity gate `c_n1_shallow_depth_matches_the_rust_authority_1367` (its own file
`tests/genlock_shallow_depth_parity_1367.rs` since review round 2 — the relock parity file was past
the 1000-line budget) lifts the new helpers with the rest of the N==1 block through the shared
directory module `tests/genlock_n1_lift/mod.rs` (`lift_converge_helper` / `converge_defines` /
`compile_and_run_n1_block`, used by both files; the two new `#define`s are in `converge_defines`).
Its tick sequence covers a relock in the middle of an open window, a capped report then a relock,
and a deep window whose latch tick reads not-deep. Mutation
proof: COMPILE each parity test with `CARGO_MANIFEST_DIR=<scratch repo with the mutated C>` —
`env!` is resolved at COMPILE time, so setting it only at run time silently tests the unmutated
file (20/20 RED at landing, 31/31 after review round 2 — the deep flag + its majority, the capped
0, the re-measure, the auto-window, the booking, the fold and the applied audio delay each have a
mutation; 0/20 when only the run-time env was set). The std-only
`tests/genlock_shallow_depth_wiring_1367.rs` pins the wiring, mirrored in both
`windows-genlock*.yml`.

**Bench pitfalls (issue 1367 review rounds 2-3 — both were shipped once and caught later).**
- A scenario that changes the arrival mid-run can pass VACUOUSLY: a later relock (sender or OBS
  restart) re-latches the new D anyway, so a dedup'd latched sequence `[3, 4]` proves nothing about
  WHEN it happened. Assert the time share (`depth_hist[D_new]` covers the span from the change on).
  And the rounded floor is `ceil(lag / interval)`, so a "rise" must put EVERY floor at or over D
  (28–40 → 70–95 ms at D 3), not straddle it (60–80 ms floors at 2 or 3 and never re-measures).
- A transient metric sampled only inside the settled gate silently drops the transient's start (the
  gate reopens seconds after a re-latch; the slew's first seconds are its largest |A/V|). Sample the
  transient on its own, outside the gate, and LOWER-bound its peak so a late start fails.

**The grid-bench port carries the rule** (`Fifo` in `src/genlock_grid_bench.rs`, now `pub(crate)`
for the shallow bench), with `BenchConfig::shallow_depth_rule` (default true; false = the
pre-rule floating conveyor, the anti-tautology) and `min_latency_box`. The deep 2ME PGM bench results
are unchanged. The probe `ReleaseCadence` mirror does NOT carry the shallow rule (a test-only
reference with no scheduled-tick model); the authority + parity + grid-bench port are the proof.

**Live acceptance (supervisor).** Full-bundle deploy on resolume + strih-lx + stream. On resolume
each `sp-*_video` logs one `genlock-shallow-lock` per OBS start (depth 2–3), then its audit
`ts_head_skew_ms` stays on `depth × 33` apart from ≤ 1 s excursions, `shallow_latches=` stays flat
between restarts, `audio_slews=` / `audio_steps=` stay 0. The songplayer gate (±40 ms, 0 dropouts)
passes across two OBS restarts.
