---
paths:
  - "src/genlock_grid.rs"
  - "src/genlock_grid_bench.rs"
  - "vendor/obs-studio/libobs/obs-genlock-grid.h"
  - "vendor/obs-studio/libobs/obs-video.c"
  - "src/genlock_pacing.rs"
  - "src/dupe_decimation/gate.rs"
---

# ONE per-second genlock grid + the grid-drift bench + the stamp counters (#1355)

## The grid

Every genlocked sender stamps on the PER-SECOND grid: slot `k` of whole second `S` is
`S + floor(k · 1 s / fps)` (`src/ndi.rs floor_boundary_100ns`, DistroAV
`genlock_floor_boundary_100ns`). Until #1355 the receiver floored its ts-align deadline
(`genlock_phase_pin_deadline`) and its render tick (`genlock_next_deadline`) on the grid counted
from 1970, `(t / interval) · interval`. At 30 fps `30 × 33_333_333 = 999_999_990 ns`, so that
grid loses **10 ns per second (0.864 ms/day)** against the stamps (40 ns/s at 60 fps). The stamp
offset walked with the calendar date — ~2 ms on 24.9.2026, mid-frame around 11.10.2026 — and the
deep N==1 `NDI 2ME PGM` FIFO flipped 31 ↔ 32 frames 10–45 ×/h.

Now ONE helper serves both receiver call sites: `vendor/obs-studio/libobs/obs-genlock-grid.h`
(`genlock_grid_floor_ns` / `genlock_grid_next_boundary_ns`, ns units, the sender's
multiply-then-divide order + the #1009 at-most-one-slot promotion). Rust authority:
`src/genlock_grid.rs` (`grid_floor_ns` / `grid_next_boundary_ns`; `genlock_backlog::
phase_pinned_deadline` delegates). Rules for anyone touching it:

- **Integer rates only.** `integer_fps(interval)` accepts `|fps · interval − 1 s| < fps`
  (33_333_333 → 30, 16_666_666/7 → 60); 29.97 (`33_366_666`) has no per-second grid and keeps
  the 1970 arithmetic byte-identical; interval 0 returns `t`.
- **Coincidence, not equality.** The sender stamps in 100 ns units, the receiver grid is in ns:
  a stamp is at most 99 ns BEFORE its own slot's receiver grid point, never after — so a stamp
  is due exactly when the floored deadline reaches its slot. That property (`next(s−1) − s <
  100`) is what the parity gate checks across a WHOLE DAY; short vectors cannot see a 10 ns/s
  drift.
- **The roll-over needs no branch.** For the last slot, `(slot + 1) · 1 s / fps` IS the next whole
  second; a separate `slot + 1 >= fps` branch is an equivalent mutation the gate cannot (and need
  not) see — found while mutation-proving it, removed.
- **Parity gate:** `c_genlock_grid_matches_the_rust_authority_and_the_sender_grid_1355`
  (`tests/genlock_grid_parity_1355.rs`) `#include`s the REAL header (no lift) and lifts the
  DistroAV sender floor verbatim. Mutation-proven: promotion `<=`→`<` (108/3757 vectors), floor on
  the 1970 grid (2495), next slot off by one (31), fractional rate accepted (412).
- **Anchors:** `deadline_and_render_tick_share_the_per_second_grid_1355` +
  `ts_align_deadline_is_phase_pinned_to_the_wall_grid_940` (`tests/genlock_release_cadence.rs`)
  and both `windows-genlock*.yml` pwsh steps pin `return genlock_grid_floor_ns(deadline_ns,
  interval_ns);`, the obs-video.c `next_wall = genlock_grid_next_boundary_ns(wall, interval_ns);`,
  the includes, and forbid the old `wall - (wall % interval_ns) + interval_ns`.
- **A test helper that re-derives the step point by hand encodes the grid.**
  `step_aligned_base_1003` (genlock_backlog.rs tests) used `w % interval`; it now measures the
  shift with `phase_pinned_deadline` + `grid_next_boundary_ns`, byte-identical on the 1970 grid.
  Grep for any other `% interval`/`% I30` in a test that means "where is the deadline grid" before
  changing the grid again.

## The camera emit gate (step 3) — pacing on the stamp grid

The camera stamps on the per-second grid but its emit gate (`src/genlock_pacing.rs`) paced on the
1970 grid (`now % interval`, `boundary + interval`, `(now − boundary) / interval`) — 40 ns/s at the
production 60 fps (`16_666_666 × 60 = 999_999_960`), 8.3 ms apart on 24.9.2026. A capture in that
window crossed the gate boundary of one slot and was stamped into the neighbouring one. Now the
gate grid IS the stamp grid — but the gate still DECIDES on the poll instant (`wall_clock_ns()`
after the dequeue) while the stamp floors the CAPTURE instant, so a capture within the dequeue
latency before a boundary is still stamped one slot early (the free-running grabber's stamp
dup + gap per beat cycle). Step 3 removes the date-walking offset only; deciding on the capture
instant is the open Design-question on #1355 (review round 1 🔴). Now:

- **Latch / #131 re-latch / #707 resync** = `grid_next_boundary_ns`; the re-latch test is
  `next > grid_next(now)` (the exact per-second form of the old `next > now + interval`).
- **Every boundary ADVANCE** = `genlock_pacing::genlock_advance_boundary` (→
  `genlock_grid::grid_advance_ns`) — the gate's own advance AND `dupe_decimation/gate.rs`'s
  starvation fill (`1 + repeats` slots) + FastDrain extra slot. **Never `+ n * interval`:** the
  per-second steps alternate `interval` / `interval + 1` ns, so `+ interval` from a grid point lands
  1 ns short of the next point on a long step and the next poll re-crosses the SAME slot (a doubled
  emit / a phantom skip). The `fast_drain_keeps_…` / `starvation_fill_keeps_…_2026_date_1355` tests
  count off-grid boundaries through every arm at a real date.
- **Lag, the #1131 resync bound, on-time, the #707 skip count** = grid SLOTS via
  `genlock_grid::grid_steps_between` (grid points in `(from, to]`) — never `/ interval`, which calls
  an instant still inside a long slot "one slot late". `grid_steps_between` / `grid_advance_ns` are
  Rust-only (no C mirror — the receiver has no slot-count consumer).
- **Test fixtures that write "N slots late" as `b + N * interval`, or an "exact-rate" capture train
  as `i * floor(1e9 / fps)`, encode the 1970 grid** (the train drifts 40 ns/s and sits a few hundred
  ns BEFORE the per-second points). Write N slots late as `b + N * interval + N` (N slots on either
  grid) and an exact-rate train as `(i as f64 * 1e9 / fps) as u64` (the per-second points at 60).
- Proof: `emit_boundary_equals_the_stamp_boundary_all_day_1355` (1000 latches over a day, 30 + 60
  fps: crossed boundary == grid floor of the emitting instant == its stamp slot, consecutive emits
  one slot apart) + `emit_gate_never_emits_before_the_stamp_boundary_at_the_drift_crossing_1355`
  (the second where the 1970 grid passes the whole second — a few hundred ns apart, invisible to a
  random sample). Log lines, counters, and the #707/#1131/#1145/#1167 semantics are byte-identical;
  only the grid they count on moved. The stamp of a starvation REPEAT (`base − k · (10⁷ / fps)`)
  stays at most `k` × 100 ns above its slot's point — inside the slot, left as is.
- **Tier-0 verify recipe used:** one standalone crate `lib.rs` with `#[path]` mods for
  `genlock_grid` + `genlock_pacing` + `dupe_decimation/mod.rs` (no other `crate::` deps), built with
  plain `rustc --test` and again with `clippy-driver --test -D warnings`; the old-code check =
  archive the RED commit's `src/` files into a scratch tree with the new `tests.rs` copied in.
- **Deploy (supervisor):** camera-box binary only (cambox fleet), no OBS change. Every camera's
  emitted frames move onto the stamp grid — a one-time phase shift of the emit instants (≤ the
  day's offset, 8.3 ms at 60 fps on 24.9.), absorbed by the strih/stream genlock FIFO. Watch each
  cambox's `Streaming:` / `#707` lines stay 299–301 with no new SKIP burst, and the strih
  `genlock-fifo audit` `stamp_dup=` / `stamp_gap=` rate on the camera inputs against its own
  pre-deploy baseline: it must NOT rise; do not expect it to fall to zero — the poll-vs-capture
  latency beat above still produces a dup + gap pair per grabber beat cycle.
- **frame-probe's synth-ndi sender** (`src/bin/frame-probe.rs`, probe-gated) sleeps to
  `genlock_grid::grid_next_boundary_ns` too — it was the last `now % interval` pacer.

## What changes live on deploy (tell the supervisor)

- The render tick of every genlocked OBS moves onto the per-second grid: a one-time shift equal
  to that day's offset (~2 ms on 24.9.), absorbed by the ±2 ms `GENLOCK_MAX_SLEW_NS` clamp in one
  or two ticks.
- The bench shows the 2ME PGM settles in ONE state (31 frames with the live sender tail; 30 with
  none) and never drains. The presented age can therefore end one frame different from the
  pre-deploy mix of 31/32 — re-run the A/V align (the E2E split correction) after the deploy.
  Since issue 1367 every restart settles on `base + 1` (31 at pin 987) whatever the startup stall,
  tail or no tail (`genlock-n1-pin-derived-depth.md`).
- **An EXTERNAL sender that follows an old reading of the sender contract** (`k · interval` from
  1970, in 100 ns units) walks 1 µs/s at 30 fps (4 µs/s at 60) — a whole frame in ~0.4 days —
  against every per-second sender and the receiver. The contract (§3/§4) was corrected to the
  per-second formula in the same change; SongPlayer / the cg OBS must be checked against §4.

## The bench (`src/genlock_grid_bench.rs`)

A tick-by-tick port of the N==1 branches of `genlock_release_tick` (ACQUIRE / BACKLOG relock via
`relock_select_nearest`, STEADY, GAP RESYNC, HOLD / late HOLD, the erase loop, the #859 drain)
driven by a strih-lx sender model (render tick on the grid under test, send delay median 20.7 /
p99 24.9 ms + a late-render tail, per-second FLOOR stamp at SEND time) and a stream receiver
(tick jitter p99 0.5 ms, 0.1 % of ticks 10–30 ms late), sampling the presented age every 150
ticks like the 5 s audit. `GridModel::Legacy1970` keeps a local copy of the pre-fix arithmetic;
`GridModel::Production` calls the real authorities — so the RED commit (production == 1970)
failed on today's arithmetic and the GREEN one passes.

- Numbers (6 h, seed 0x1355_2026_0924): **1970 grid 27.5 flips/h** (states 31/32, 91 late holds,
  91 drains); **production 0.34 flips/h** (one sampled one-tick blip, 0 drains, 0 late holds).
  Date sweep on the 1970 grid: 29.2 (0.3 ms) / 27.5 (2 ms) / 19.8 (5 ms) / 3.7 (11 ms) / 5.4 but
  522 stamp dups/h (16 ms) / **245 flips/h and 18 493 dups/h (22 ms)** / calm at 28–32 ms.
  The production grid gives byte-identical runs at every offset BY CONSTRUCTION (it never sees
  the 1970 offset — the test asserts equality, it does not re-prove a bound per date). What it
  does see is the pin: over pins 950–1024 ms (2 h each) production holds ONE state per pin
  (30 / 31 / 32 frames, 0 flips, 0 drains) while the 1970 grid flips ~20/h at every one. Four
  more seeds: 1970 22.6–30.3/h, production 0.34–0.67/h.
- **The two tail rates are the only calibrated knobs** (sender late-render 300 ppm/frame,
  receiver late ticks 1000 ppm/tick): the design measured the correlation, not a per-frame rate.
  Do not tune them to make a new change pass — change them only from new rig measurements.
- **Residual on the production grid is metadata, not content:** a slow frame still arrives
  stamped one slot late (a gap) followed by an on-time duplicate stamp (~31/h in the bench), but
  the release presents both frames at their normal ticks (a GAP-RESYNC, then STEADY; only the
  stamp-derived age reads one frame short for that one tick) — no hold, no duplicate, no skip, no
  depth change. On the 1970 grid the same pair cost a held frame (a
  visible duplicate), a +1 frame A/V step, and later a drained frame (a visible skip). With the
  10× sender tail the production grid still gives 3.7 flips/h (sampled blips) vs 52.6 on 1970.
- This bench DOES reproduce its target, unlike SimConveyor1049 for the N>=2 ladder
  (`genlock-conveyor-jitter-budget.md`): the N==1 mechanism is fully inside the ported branches.
- **Issue 1367 extended it:** a `SenderRestart` (outage + k-slot startup stall), the N==1 depth
  rule (hold + shed, read at the tick's SCHEDULED instant), a `receiver_tick_offset_ns` schedule
  phase, and a render-tick model that runs serially with CATCH-UP ticks (`video_sleep` counts one
  frame below a two-interval overrun; only >= 2 intervals skips slots — `skipped_ticks`). The
  numbers above were measured before those changes; the issue-1367 tests carry their own.

## The stamp counters (`stamp_dup=` / `stamp_gap=`)

Per-input, on ARRIVAL, under `async_mutex` at the producer push site (AFTER the #99 peak update —
`tests/genlock_preload.rs` pins the peak update within 1400 bytes of `genlock_frames_received++`),
via `genlock_stamp_track_observe` (header) / `StampTrack::observe` (Rust): an equal stamp = dup;
a positive interval in [4 ms, 1 s) updates the source's own step (min-delta, #1042) and above
1.5 steps counts `round(interval / step) − 1` missing intervals; a positive interval below 4 ms
(an off-grid hiccup — review finding: one such interval would otherwise shrink the step until
the next flush and turn every normal interval into ~32 false gaps), backward and ≥ 1 s jumps
count nothing; the
explicit flush resets `genlock_rx_last_ts` + `genlock_rx_min_delta_ns` (counters survive).
Audit-line-only (not in `obs_genlock_stats`), printed right after `wall_qpc_drift_ms=` so the
`(long long)gs.audio_pairing_offset_ms);` last-argument anchor stays; parsed by
`src/jitter_audit.rs` (0 on older lines, `delta_stamp_dup` / `delta_stamp_gap` in the summary,
never in the #757 `--json`). A connection that HALVES while tracked (#1203) reads as one gap per
frame — honest, the sender's stamps really are missing; one that is already halved when the
tracking starts (or after a flush) learns the halved step and counts nothing.

## Post-deploy audit (≥ 30 min on the stream box, before any E2E)

Per 5 s `genlock-fifo audit 'NDI 2ME PGM'` line: `ts_head_skew_ms` must stay in ONE frame state
(no 31↔32 flips — the old 10–45/h), `late_holds` and `dropped_due` deltas 0 (no drain), the line
must carry `stamp_dup=` / `stamp_gap=` (their rate is the sender residual, ~ tens/h, the input
for the sender follow-up), and the relock log's `tick_phase_ns` is now the phase within the
per-second grid. strih-lx's inputs get the counters too; judge `cg` / `CG-obs` like the 2ME PGM,
but expect a steady non-zero baseline on the 60 fps camera inputs (the free-running grabbers beat
against the grid and their stamps key on the CAPTURE instant — the #889 copies/gaps residual),
so compare a camera against its own earlier rate, never against zero.
