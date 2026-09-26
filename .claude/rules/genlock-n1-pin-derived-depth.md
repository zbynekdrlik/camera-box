---
paths:
  - "src/genlock_n1_depth.rs"
  - "src/genlock_n1_depth_tests.rs"
  - "src/genlock_n1_depth_latch_budget_tests.rs"
  - "src/genlock_grid_bench.rs"
  - "src/genlock_grid_bench_tests.rs"
  - "tests/genlock_relock_selection_parity.rs"
  - "vendor/obs-studio/libobs/obs-source.c"
  - "src/probe/genlock.rs"
  - "src/probe/genlock_n1_tests.rs"
  - "src/genlock_shallow_av_bench.rs"
  - "tests/genlock_shallow_depth_wiring_1367.rs"
  - "tests/genlock_shallow_depth_parity_1367.rs"
  - "tests/genlock_shallow_sticky_parity_1367.rs"
  - "src/genlock_n1_depth_sticky_tests.rs"
  - "src/genlock_shallow_av_bench_sticky.rs"
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
  UNDER-reads the floor at the scheduled instant). This RAW tick floor still feeds `floor_max_frames`
  and the rise / fell watch; the latch HISTOGRAM reads the budgeted receive-lag floor (the section
  "The latch budgets the receive-time arrival lag" below).
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
latch_floor_frames= spread_frames= rejects= wanted_frames= latency_ms= capped=` line per latch, one
`genlock-shallow-remeasure '<src>': reason=spread|rise|fell|unreachable|relocks …` line per
re-measure that is not a relock / pin change, and `shallow_depth= shallow_capped= shallow_latches=`
on the audit line (audit-line-only).

## The latch never latches an outlier (design 5830750134)

**Why.** Live 25.9.2026 12:01:17 on resolume: a SongPlayer song change pushed `sp-slow_video`'s
arrival floor to 11 frames inside the settle window. The window MAX latched D 12 (400 ms); the hold
then deepened the queue past the backlog threshold, so the FIFO BACKLOG-relocked ~3 per 5 s and sat
at ~233 ms, and the audio held the latched 400 ms against it (`audio_pairing_offset_ms` walking to
+166, songplayer `audio_corr=0.13`). An OBS relaunch cleared it. The shallow bench reproduces it
exactly on the old code (`latched [3, 3, 12]`, depth stuck at 7, 732 relocks once settled).

| Piece | Rust (`src/genlock_n1_depth.rs`) | C (`obs-source.c`) |
|---|---|---|
| (a) window histogram relative to base, 4 bins (`floor − base`, saturating at 0 and at the over-clamp bin) | `n1_shallow_hist_bin`, `ShallowDepth.hist` | `genlock_n1_shallow_hist_bin`, `genlock_shallow_hist[GENLOCK_SHALLOW_HIST_FIELD_BINS]` (a `_Static_assert` holds it to `GENLOCK_N1_SHALLOW_HIST_BINS`) |
| (a) the latch floor = the window's p90, not its max | `n1_shallow_percentile_bin`, `N1_SHALLOW_LATCH_PERCENTILE` 90 | `genlock_n1_shallow_percentile_bin` |
| (a) p90 − p10 > 1 frame on a non-deep window = a transient: re-measure (old D kept), at most 3 in a row, then latch | `N1_SHALLOW_MAX_SPREAD_FRAMES` 1, `N1_SHALLOW_MAX_REJECTS` 3 | inside `genlock_n1_shallow_track` |
| (b) D ≤ base + 3, an over-clamp D is CLAMPED and reported (`capped`, WARNING); the imag min-latency report-only cap is checked first | `N1_SHALLOW_MAX_EXTRA_FRAMES` 3, `n1_shallow_target_frames` | `genlock_n1_shallow_target_frames` |
| (b) the shed never fires while the newest frame is already more than D frames old | `n1_shallow_shed_due` | `genlock_n1_shallow_shed_due` |
| (b)/(c) the latched watches: floor at/over D a window (rise) — or, clamped, two frames under D a window (fell); realized depth < D for 180 ticks (unreachable); 3 backlog relocks without a 180-tick quiet gap (relocks) | `n1_shallow_watch`, `N1_SHALLOW_UNDER_TICKS`, `N1_SHALLOW_CHURN_RELOCKS`, `N1_SHALLOW_CHURN_QUIET_TICKS` | `genlock_n1_shallow_watch`; the latch helper passes `genlock_n1_depth_frames(tick_wall, source->last_frame_ts, interval)` and the `genlock_relocks` delta (`genlock_shallow_relocks_seen`) |
| (d) the audio hold follows the REALIZED delay under the same lock | `video_delay_track` + `VideoDelayTracker.locked_ms`, `VIDEO_DELAY_FOLLOW_TICKS` 180 (`genlock_audio_pairing.rs`) | `genlock_video_delay_track(…, &locked_ms, …)`, `genlock_video_delay_locked_ms` |

What carries it (do not undo):
- **The cap is sized from the live healthy latches**, not a guess: every `sp-*` / `NDI test` latch of
  25.9.2026 had `floor_max_frames` 1–2 (D − base 1–2); the bench's 50–80 ms straddle band needs
  base + 3.
- **The spread bound is 1, not 2.** On the live 2-frame floor the song change reads p10 = base + 1,
  p90 = the over-clamp bin: spread 2. A bound of 2 let the bench's one-second transient through to
  a clamped latch.
- **The histogram is cleared on the window's FIRST sample**, so the rearm (anchored at the pin setter)
  stays a five-field reset in C.
- **A clamped latch under a genuinely slow arrival must not churn**: the shed guard keeps the conveyor
  off a D the arrival cannot supply, the over-floor watch is replaced by the fell-two-frames watch
  while clamped (no re-measure loop, no WARNING spam), and (d) pairs the audio with the video on air.
- **The downward watches are LATCHED-only.** The realized-under count (180) is twice the settle window
  so the hold's climb onto a clamped D (3 holds × the 30-tick throttle) never trips it; the relock
  count clears after a 180-tick quiet gap so a stall's relock hours apart never re-measures.
- **(d) keeps "a new lock applies at once"**: a clean latch still places the audio once, straight
  onto D (0 slews). Under the same lock the hold follows the smoothed realized delay only after it
  stayed half a frame off for 180 consecutive ticks; leaving a lock clears the count.

Bench (`src/genlock_shallow_av_bench.rs`, `Scenario::burst` = +300 ms on the frames after the sender
restart): a 150 ms / 1 s / 4 s transient all end on D 3 with 0 backlog relocks once settled and
settled |A/V| ≤ 1.97 ms; the 1 s one is rejected (spread) and re-latches the same D (0 slews); the 4 s
one latches the clamp 4 (reported), re-measures once the floor fell and slews twice. Parity: the
shallow sequence adds a short burst, a rejected transient, bounded rejects, a whole-window clamp and
its fall, an unreachable D and a relock storm (latched `… 2 2 4 4 3 3 3`); it runs from C arrays in
one loop (per-tick statements compiled for minutes). Mutation sweep: 22/25 RED at landing, the two
real survivors (the shed guard, a lock left mid-count) closed with new vectors, one equivalent mutant.

**Known limit (review round 1): an unreachable D has no two-clock bench case.** The `unreachable`
and `relocks` watches are proven by unit + parity tests. On a feed where every re-latched D stays
unreachable, a re-measure (and its `genlock-shallow-remeasure` line) repeats about every 270 on-grid
ticks (180 watched + 90 window) — no rate limit; the bench's transient cases never reach it because
the p90 / spread / clamp keep the latched D reachable. `(d)` also FOLLOWS a realized delay above the
lock (a clamp under a slow arrival), a decided extension of "never exceeds".

**Live acceptance (supervisor).** After a SongPlayer song change / pause-resume cycle on resolume:
`shallow_depth ≤ base + 3` (≤ 4 at pin 3), `relocks=` flat, no `genlock-shallow-remeasure
reason=unreachable|relocks` in steady state, and the songplayer gate ±40 ms with 0 dropouts.

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

## A skipped stamp never shortens D — the GAP hold (design 5833339163)

**Why.** Live resolume 25.9.2026 15:29: a SongPlayer song change + operator scene switches made the
sender skip stamps on `sp-slow_video` (D 3). Audit: `stamp_gap` +11 / `underruns` +7 per 5 s,
`video_delay_ms=67 audio_delay_ms=100 audio_pairing_offset_ms=33` for ~10 s, `shallow_depth=3`,
relocks / sheds flat. Mechanism: a skipped stamp puts the head past the locked boundary, so the
tick takes GAP RESYNC, which presented the head AT ONCE at its arrival age — one frame (or more)
under D. The STEADY branch cannot present shallower than D (its head stamp is ≤ the boundary), so
GAP RESYNC is the only path that loses depth. The restore (`n1_shallow_hold_due`) shares the #859
throttle, whose counter only advances on STEADY presents: one frame back per ≥ 1 s of clean
presents, while the next skips re-shallowed it. Not the grid guard, not the rounding, not a
starvation repeat (an empty tick presents nothing).

| Piece | Rust | C (`obs-source.c`) |
|---|---|---|
| pure decision | `n1_shallow_gap_hold_due(tick_wall, head, next, boundary, floor, pin, interval, D)` | `genlock_n1_shallow_gap_hold_due` (in the contiguous N==1 block, parity-lifted) |
| source wrapper | the GAP arm of `Fifo::tick` (`genlock_grid_bench.rs`) | `genlock_should_hold_n1_gap` (N==1 only, on-grid, scheduled instant) |
| call site | — | head of the GAP RESYNC branch: hold → `n1_grows++`, `return false` |

What carries it (do not undo):
- **Unthrottled.** The head ages a frame per tick, so the hold ends within D ticks; it never moves
  the conveyor off D, so it cannot limit-cycle with the shed. The #859 counter is left alone.
- **A normal frame is never held.** Per-second stamps step 33 333 300 / 33 333 400 ns against a
  boundary of presented + 33 333 333, so about one normal frame in three takes GAP RESYNC — but it is
  already D old there.
- **Relock gaps (≥ 1 s) keep the old path** (the window re-measures); deep, unlatched, capped and
  N>=2 sources are untouched.
- **The duplicate guard.** A send-time-stamping sender (strih-lx, the #1355 residual) labels a slow
  frame one slot late and the NEXT frame repeats the stamp: the head is on time. `next ≤ head`
  (the second queued frame) → present. Without it the grid bench's shallow strih-lx case turned 55
  labels into 55 holds + 55 sheds.

**Known limit (review round 1).** The guard only sees a duplicate that is ALREADY queued. When it
has not arrived, the hold fires on the label (a repeat, then a shallow shed): 0 at the live strih-lx
jitter, 22 + 22 per 2 h at σ 4 ms (`a_late_label_without_its_queued_duplicate_costs_a_bounded_repeat_1367`
pins ≤ 30 + 30, paired). The stamps alone cannot separate it from a real skip ("hold only when
two frames short" brings the song-change bug straight back); a real fix needs per-frame arrival
times. A non-30-multiple source (25/50p, 29.97) with a latched D now takes the GAP hold on most
ticks, holding its presented age at ~D — untested, no such source on the rig.

**Bench** (`genlock_shallow_av_bench.rs`, `Scenario::song_change` = for 12 s skip 1 / 2 / 3 stamps
every 500 ms on the 40–64 ms feed): RED 273 of 402 presents under D, video delay 67 vs audio 100,
|A/V| 31.6 ms; GREEN 0 under D, 100 / 100, 1.72 ms. Its metrics sample EVERY presenting tick of the
window (a shallow present is exactly what the settled gate skips). Parity: 12 edge families × 60
ages + a degenerate interval + an unlocked boundary near the epoch (the boundary guard is otherwise
equivalent to the relock check); 8 / 8 C mutations RED. The C wrapper was lifted with a stub
`obs_source_t` and driven (hold / duplicate / nothing queued / N>=2 / unknown N / at D / 20 ms off
the grid / no D / empty queue). Anchors: `genlock_n1_tick_wall_now(wall_now)` count 4, the on-grid
wrapper return count 3, `tests/genlock_shallow_depth_wiring_1367.rs` + both pwsh gates.

**Live acceptance (supervisor).** After a SongPlayer song change / scene switch on resolume:
`video_delay_ms` stays on `shallow_depth × 33` (no 67 against an audio 100), `audio_pairing_offset_ms`
within ±16. `n1_grows=` rises once per real skipped stamp (`stamp_gap=`) with NO `converge_sheds=`
partner; the two climbing TOGETHER on a shallow feed (strih-lx `CG-obs`, `sp-*`) is the late-label
residual above.

## The latch budgets the receive-time arrival lag (ROZHODNUTÉ 5842640404, re-baseline 5842656021)

**Why.** Acceptance read 5842599404 (resolume, after songplayer 147 made idle == playing path):
every song start still logged `genlock-shallow-remeasure reason=rise`, ~3 s later a latch 2 -> 3
frames, `audio_slews` +1 and a +30 ms audio slew over ~35 s. SongPlayer's `send_video_async` costs
+8..11 ms more on playing content than on black, so real frames ARRIVE ~10 ms older while the stamps
do not move. The latch made on idle sat one frame short whenever the idle lag was within ~10 ms under
a frame edge. The tick-read floor cannot budget that: since the per-second grid it is whole frames
(`ceil(lag / interval)` ± 2 ms), and a budget on it adds a frame on EVERY source (the rejected
always-+1 approach) without even stopping the re-measure.

| Piece | Rust | C |
|---|---|---|
| per-source receive lag | `Fifo::receive` records `arrival − stamp` (`genlock_grid_bench.rs`, used by all three benches) | `genlock_rx_arrival_lag_ns` (obs-internal.h), set at the producer push site of `obs_source_output_video_internal` right after `genlock_stamp_track_observe`, under `async_mutex`, `genlock_wall_now_ns() − output->timestamp` saturating at 0; zeroed with `genlock_rx_last_ts` at the explicit flush |
| budgeted latch floor | `n1_shallow_latch_floor_frames(lag, I) = (lag + GENLOCK_N2_JITTER_BUDGET_NS).div_ceil(I)`, 0 for I = 0 | `genlock_n1_shallow_latch_floor_frames`, in the contiguous N==1 block (parity-lifted) |
| where it is read | `ShallowTick.latch_floor_frames` -> the histogram only, and NOT on a min-latency box (the tracker bins `floor_frames` there, ROZHODNUTÉ 5842848307) | the new `latch_floor_frames` scalar of `genlock_n1_shallow_track`, passed by `genlock_shallow_latch`; the bin reads `min_latency_box ? floor_frames : latch_floor_frames` |

What carries it (do not undo):
- **No budget on a min-latency (imag) box (ROZHODNUTÉ 5842848307, option 2 of Design-question
  5842838431).** At a 60p canvas the 15 ms budget is ~90 % of a frame: `ceil((lag + 15) / 16.7) >= 2`
  for any lag over ~1.7 ms, so every shallow 3 ms input would ask for more than base + 1, which the
  imag guard only REPORTS (`capped=1`, no depth, the #859 drain back on). The owner's hard rule is
  minimum latency on imag, so the tracker bins the RAW tick floor there and the input stays governed
  at D 2 exactly as before this slice. The decision lives INSIDE the tracker on its existing
  `min_latency_box` input (no new argument, no second constant).
- **ONE budget constant.** The 15 ms `GENLOCK_N2_JITTER_BUDGET_NS` of the N>=2 conveyor (#1354), no
  second shallow-only constant; the parity lift already lifts its `#define`.
- **The histogram reads the budgeted floor, the watch reads the raw one.** A raw tick floor at or
  over D is a rise of MORE than the budget: after a budgeted latch the margin to a `rise` is >= 15 ms
  by construction. If the watch read the budgeted floor the margin would be ~0 again.
- **Monotone in the lag**, so the histogram's p90 of the budgeted bins IS the budgeted p90 lag.
- **The lag is the LAST RECEIVED frame's**, which is the newest queued frame (the push appends
  under the lock the release tick reads it under). It includes the receive-thread wall read, so a
  `#797 slow output_video` stall (5–17 ms) enters it; the p90 over 90 ticks absorbs an occasional
  one, and a sustained one is exactly the arrival the budget covers. Every received frame overwrites
  it, so no other invalidation seam is needed. A wall step is not gated by `on_grid` here: the p90
  over 90 ticks absorbs a step's transient, and a lasting sender/receiver clock offset biases the
  receive lag and the raw tick floor alike.
- **A lock made while content plays latches on the content lag.** D depends on what the sender is
  doing at the lock (the bench's 18–20 ms content relock is D 3 while its 7–9 ms idle locks are D 2);
  the rise watch only ever moves D up past the budget, never back down while latched.
- **The `wanted_frames=` report mixes floors on a clamped latch.** Past the clamp it falls back to the
  RAW window max (`floor_max_frames`) when that asks for more than the budgeted p90; otherwise it is
  the budgeted `base + p90 + 1`. Report-only.

**Re-baseline (ROZHODNUTÉ 5842656021).** A feed whose p90 lag sits within 15 ms under a frame edge
latches one frame deeper — the feeds a content change would push across it: `NDI test` (22–31 ms)
D 2 -> 3, `sp-slow` (40–64 ms) D 3 -> 4 (100 -> 133 ms, audio follows), the 40–64 ms transient and
song-change cases re-baselined with it. `cg-obs` (28–40 ms, D 3), the 50–80 ms straddle (D 4) and
the restart-band case are unchanged; the 28–40 -> 70–95 ms rise now re-latches onto the base + 3
clamp (reported `capped`). A band far under the edge pays nothing: idle 8 ms stays D 2.

**Bench** (`genlock_shallow_av_bench.rs`, `song_start(idle, content, D)` = the lag band steps at
1500 s): 24–26 -> 35–37 ms latches D 3 on idle, 0 re-measures (`Run.latches` = 3), 0 slews;
7–9 -> 18–20 ms keeps the idle locks on D 2 through the song start (the sender-restart lock made ON the
18–20 ms content lag is 3 — 20 + 15 ms crosses the edge, the same formula); 7–9 -> 59–61 ms
re-measures once onto D 4 and slews once. Parity: latch-floor vectors at both edges, two intervals,
a degenerate interval and the saturation, plus a tick sequence whose latch floor differs from the
raw floor (latched `… 3 4`), then a marker window on the raw floor (`… 2`). Marker path: a 60p unit
case (8 ms lag: raw 1, budgeted 2) latches D 2 governed with the marker and D 3 without; the 30p
two-clock bench at 24–26 ms keeps D 2, never `capped`, on the marker and D 3 without. C mutation
sweep: 9/9 RED (histogram / window max / watch reading the wrong floor, the marker condition dropped
either way or inverted, no ceil, no budget, degenerate interval, saturation).

**Live acceptance (supervisor).** Full-bundle deploy on resolume. Across >= 2 song starts: no
`genlock-shallow-remeasure`, `shallow_latches=` flat, `audio_slews=` flat, `audio_pairing_offset_ms`
0; `sp-*_video` latch lines show `latch_floor_frames=` one over the raw `floor_max_frames=` where the
idle lag is within 15 ms of an edge. On imag (the marker) the latch shows no budget:
`latch_floor_frames=` equals the raw floor, `shallow_capped=0`, D 2 governed; the cap still reports a
genuinely slow input (raw floor over base).

## An idle re-lock keeps the STICKY content floor (design 5844353368)

**Why.** Acceptance read on the budget slice (resolume log `2026-09-26 09-29-12.txt`): the OBS start
at 09:29:22 latched `sp-slow_video` D 4 on playing content (`latch_floor_frames=3`); the idle
SongPlayer re-lock at 09:33:23 re-latched every `sp-*` on black frames at D 2
(`latch_floor_frames=1`); each song start then re-measured (`reason=rise` at 09:34:48 / 09:36:50 /
09:38:20) and slewed the audio. A per-lock latch taken on idle cannot represent content, and every
sender re-lock (song change, E2E restart, reconnect) reset it to the idle level. The idle→content
lag jump is > 15–20 ms (receive-side decode scales with content), beyond the arrival-jitter budget.

| Piece | Rust (`src/genlock_n1_depth.rs`) | C (`obs-source.c`) |
|---|---|---|
| state | `ShallowSticky` (floor, seen instant, block ticks, block histogram) | `genlock_shallow_sticky_{frames,seen_ns,obs_ticks,obs_hist}` (obs-internal.h) |
| observe / decay / clear | `n1_shallow_sticky_track(&mut ShallowSticky, &ShallowTick, audio_flowing, tick_wall)` | `genlock_n1_shallow_sticky_track` (in the lifted N==1 block) |
| decay | `N1_SHALLOW_STICKY_DECAY_NS` (30 min) | `GENLOCK_N1_SHALLOW_STICKY_DECAY_NS` |
| latch reads it | `ShallowTick.sticky_floor_frames`, limited to `base + 2`: D = `max(base, p90, sticky) + 1` | the `sticky_floor_frames` scalar of `genlock_n1_shallow_track` |
| audio flowing | the bench `Fifo.audio_flowing` (the A/V bench: audio leg mode `Timecode`) | `source->genlock_audio_hold_mode == GENLOCK_AUDIO_HOLD_TIMECODE` |

What carries it (do not undo):
- **Observed exactly like the latch measures.** A 90-tick on-grid block of the BUDGETED latch floor
  while the audio flows, read at its p90. It counts only with a p90 − p10 spread within one frame
  (a transient never becomes sticky), a p90 above base (a floor at base asks for nothing — every
  deep source; a pin lowered from a deep value never inherits it) and under the over-clamp bin. A counting block at or above the floor
  raises it or resets its decay clock; a relock or an audio-less tick restarts the block.
- **"Audio flowing" = the hold mode is `timecode`.** libobs has no silence detector; live the mode is
  `timecode` across idle and playing. The floor is a MAX, so an idle block never raises it; it only
  refreshes the clock where the idle lag genuinely sits at that level. A video-only source (cameras)
  never reaches `timecode`, so it keeps no sticky floor and behaves exactly as before.
- **Decay: one frame per 30 min** of scheduled-tick wall time without an observation at the level (a
  wall stepped back never decays it). A genuinely faster sender recovers its shallower D at its next
  lock after the decay.
- **In-process only.** bzalloc'd 0, never persisted; no relock / flush / pin-change seam clears it
  (the wiring gate pins `source->genlock_shallow_sticky_frames =` at 0 occurrences). An OBS restart
  starts without it, so the first song after an OBS start may still re-measure once. An N>=2 tick
  and the min-latency (imag) box clear it (no sticky floor, no budget on the marker box).
- **The latch LIMITS the sticky input to `base + N1_SHALLOW_MAX_EXTRA_FRAMES − 1` (review round 1).**
  The floor is absolute frames, so one recorded at a HIGHER pin would ask for a D over the base + 3
  clamp once the pin is lowered: a capped latch whose `fell` watch re-measures every window until the
  floor decays. Limited, a sticky floor alone never caps a latch (D ≤ base + 3, uncapped); the
  measured p90 keeps its own clamp + report.
- **Unchanged:** the clamp (base + 3), the imag min-latency cap, the rise / fell / unreachable /
  relocks watches. The rise watch still reads the RAW tick floor against D.

Observability: `genlock-shallow-lock … rejects= sticky_floor_frames= wanted_frames= …` — a latch
whose `depth_frames` is above `latch_floor_frames + 1` with `sticky_floor_frames=` at `depth − 1`
is the sticky floor holding the content depth through an idle lock. `wanted_frames=` stays the p90
floor's own ask; `sticky_floor_frames=` is the unlimited floor (0 on the imag marker).

**Bench** (`genlock_shallow_av_bench.rs` `Scenario.songs` + `content_ms`, the scenario in its child
`genlock_shallow_av_bench_sticky.rs`; each song end is a 1.5 s
sender stamp gap = a relock on the idle feed): idle 8–12 ms, three songs at 40–50 ms before the OBS
restart. RED (no sticky): latched `[2, 3, 2, 3, 2, 3, 2, 2, 2]`, 6 audio slews. GREEN: `[2, 3, 3, 3,
3, 2, 2]` (one re-measure at the first song, the three idle re-locks keep D 3), 1 slew, 0 steps.
Every other shallow bench scenario is unchanged with the audio flowing. Parity: the tracker sequence
adds an idle relock over a sticky floor (3), the marker ignoring it (2) and a sticky floor past the
clamp limited to D 4 UNCAPPED (the last latch must not be capped); the sticky sequence
(`tests/genlock_shallow_sticky_parity_1367.rs`) covers the audio / relock / on-grid gates, a spread-2
block whose p90 is UNDER the clamp (review round 1: the earlier transient blocks all sat in the
over-clamp bin, so the spread gate was only caught by `-Werror` on the unused `low`), the base /
over-clamp exclusions, a burst the p90 ignores, the exact decay boundary, a refresh at the level, a
stepped-back wall and both clears. C mutation sweep 19/19 RED, the spread gate now by behaviour
(`<= MAX_SPREAD + 1`, `(void)low`); a dropped `relock`/`on_grid` gate still dies on the harness
`-Werror` unused parameter.

**Live acceptance (supervisor).** Full-bundle deploy on resolume, then the songplayer song-start E2E:
at most one `genlock-shallow-remeasure` per `sp-*` source per OBS session, `audio_slews=` flat on the
second and later song starts, idle re-lock lines with `sticky_floor_frames=` > `latch_floor_frames=`,
and the songplayer A/V gate measurable.
