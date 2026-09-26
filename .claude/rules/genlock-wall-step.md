---
paths:
  - "src/genlock_wall_step.rs"
  - "src/genlock_wall_step_bench.rs"
  - "vendor/obs-studio/libobs/obs-genlock-wall-step.h"
  - "vendor/obs-studio/libobs/obs-video.c"
  - "tests/genlock_wall_step_parity_1372.rs"
  - "tests/genlock_qpc_wall_step_parity_1372.rs"
---

# A dantesync fleet DATE step: one-tick re-grid, booked qpc step, 1000 ppm recovery (issue 1372)

dantesync 1.9.0 takes the system-clock RATE from the Dante tick and steps the fleet DATE with NTP:
up to ~50 ms, about every 1.8 h, announced (`date_step_pending_ns`) and applied fleet-wide. The
Windows OBS media clock (`os_gettime_ns`, `windows-disciplined-media-clock.md`) follows the rate and
by design NEVER a step. So at a date step the wall clock jumps against the media clock, and every
place that combines the two sees it. The first live step (−51.039 ms, 25.9.2026 23:17:07 UTC) hit
three of them. The decision is ROZHODNUTÉ 5841039244 on the ticket.

## The three consumers and what each does now

| consumer | before | now |
|---|---|---|
| render tick, `genlock_next_deadline` (obs-video.c) | the correction toward the stepped wall grid is clamped to 2 ms/tick: ~9 ticks off phase at 30 fps, and the NDI sender (DistroAV floors the wall clock AT EMIT) stamps off phase meanwhile | ONE detector (`obs-genlock-wall-step.h` ↔ `src/genlock_wall_step.rs`): a bracketed mono/wall/mono read; a wall−media jump > 2 ms vs the previous tick is a step; the deadline takes the wall-grid target UNCLAMPED in that one tick. One `genlock-regrid:` log line. |
| LOCK indicator `qpc_drift` (OBSBasicStatusBar.cpp) | the single-sample jump sat in the 300 s history: DEGRADED for 300 s | `genlock_qpc_wall_step_rebase_ms` (GenlockLockState.hpp ↔ `src/genlock_lock_state.rs`) BOOKS a jump with 33 ms < \|jump\| ≤ 66 ms (two frames; dantesync steps the date at a 50 ms error), at most 1 per window: the history is re-based by it, one `genlock-wall-step:` line, LOCKED. A bigger jump (> 66 ms: a clock set, an NTP-fallback step) or a second step in the window stays in the history and DEGRADES through the unchanged `genlock_qpc_drift_beyond_bound`. |
| ASRC (asrc-compensator.c ↔ `src/asrc_bench.rs`) | the stream `mbc` really lost 44 ms of Dante samples (upstream of OBS); the re-base armed the proportional ±100 ppm restore: 3–4 min | a re-based step the buffer level CONFIRMS is booked as `step_recover_ms` with a setpoint move of the same size, and paid back at `ASRC_STEP_RECOVER_PPM` = 1000 ppm as `step_recover_ppm`, a separate term on top of `applied_ppm`. `recover_ms=` on the `asrc:` line. |

DistroAV needs no change: its video stamp is `floor(wall at emit)`, so once the tick is on the new
grid the stamps are too. Its audio stamp stays raw wall (the sender contract).

## Why the `mbc` part is a recovery, not an offset booking (STEP 0 finding 5841032894)

The first design wanted to book the step into the wall↔media offset so the ASRC never slews. The
code and the log said otherwise: the `mbc` level is a count of real samples (no wall term on that
path; the servo's master is `os_gettime_ns`), the source stamps on the media clock
(`timing_adjust_ms=0`), `ts_lag_ms` stayed flat while `buffered_ms` fell 40 ms, and the compensator's
own step detector fired (`last_step_ms=-43.7`, `restore=1`). The samples never reached OBS. Booking
it away would have frozen a real 44 ms A/V error; the stretch is the correct recovery, it only had
to be fast. Which box drops the Dante samples at a date step (the stream soundcard or the mbc PC's
sender) is the supervisor's rig investigation, not this code.

## Invariants the gates pin

- **The recovery term is NOT booked out of the TS-smoothing timeline** (unlike the #1367 placement
  slew). The lost samples left `next_audio_ts_min` behind the source's own stamps; the stretch is what
  closes that gap. Booking it would leave the timeline 44 ms off and bring the 70 ms smoothing snap
  closer.
- **The setpoint moves with the lost buffer and walks back with every payment**, and the open
  window's level sum moves with it (the `shift_level_target` window rule), so the P term, the restore
  arms and the unreachable bound never read the recovery as an error. It is an INTERNAL move — it
  never goes through `shift_level_target`, which would arm the restore for ≥ 5 ms.
- **Nothing but a confirmed step sets the term**: the #1335 corroboration, SIGNED — the level moved
  the same way as the samples and by at least half of them (`(target − buffered)·loss > 0` and
  `|target − buffered| ≥ 0.5·|loss|`, `loss = −r·1000`). A master-clock-only jump re-bases without
  recovery; a sub-10 ms loss never re-bases.
  `ASRC_MAX_PPM`, the 5 ppm/s slew limit and the ±100 restore clamp are unchanged.
- **A flush or a capture-rule change drops the owed amount** (it was booked against that capture).
- **The corrected advance carries the term** (`raw / (1 + (applied + recover)/1e6)`), so every
  bench plant that models the buffer from the returned advance sees the recovery.
- **The detector trusts only a narrow bracket** (≤ 100 µs): a preempted read decides nothing and does
  not move the reference; every trusted read becomes the reference, so a raw-QPC fallback's slow
  drift (< 1 µs/tick) is never summed into a step.
- **ONE 2 ms definition in the render tick**: `obs-video.c` defines `GENLOCK_MAX_SLEW_NS` as
  `((int)GENLOCK_WALL_STEP_MAX_SLEW_NS)` from the header (the `render tick ENABLED` log still prints
  it); the detector's step threshold is the same constant (pinned by
  `render_tick_uses_the_wall_step_regrid_1372`). The N==1 on-grid window `GENLOCK_N1_ON_GRID_NS`
  (obs-source.c ↔ `src/genlock_n1_depth.rs` `N1_ON_GRID_NS`) is a SEPARATE 2 ms literal with its own
  parity; change the two together.
- **A re-grid stays PENDING until a tick lands on the new grid** (`genlock_wall_step_regrid_due` ↔
  `WallStepState::regrid_due`): if the render that follows the detection stalls past the re-grid
  target, `video_sleep`'s `os_sleepto_ns` fails and falls back to `cur_time + interval · count` on the
  OLD grid; the next tick re-grids again instead of degrading to the 2 ms slew. The pending flag
  clears once the correction is within the 2 ms slew.
- **Frame counters on a stalled re-grid**: the fallback path counts `count` intervals (lagged frames)
  exactly as on any late tick; the re-grid itself never adds a lagged frame when it lands on time.
- **The stamp interval that carries the step is inherent**: after a −51 ms step the next stamp is one
  slot BACKWARD (a receiver shows a repeated frame), after +51 ms it skips a slot. No sender change can
  avoid it (the wall moved); the re-grid only keeps every stamp after it on the grid.
- **The recovery books the MEASURED loss** (the regression residual, which counts the lost samples
  exactly), confirmed by the SIGNED level corroboration (the buffer moved the same way by ≥ half), the
  total owed capped at ±`ASRC_STEP_RECOVER_MAX_MS` = 100 ms. The per-callback level only confirms: it
  reads up to ~10 ms off within a block (the bench's step callback read 35 ms for a 43 ms loss), and
  booking the smaller of the two left the rest to the slow level loop (settled 6 ms off at +60 s).
- **No 2000 ppm stacking**: while the #1367 audio-placement slew is running
  (`genlock_audio_slew_remaining_ns != 0`), obs-source.c holds the payment
  (`asrc_compensator_set_step_recover_hold`); the owed amount waits, it is never dropped.

## Gates and the Tier-0 recipe

- `tests/genlock_wall_step_parity_1372.rs`: `#include`s the real header, compiles it under
  `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, byte-compares every read sequence and deadline with
  the Rust authority, and pins the obs-video.c wiring.
- `tests/genlock_qpc_wall_step_parity_1372.rs` (the widget's booking, lifted from
  GenlockLockState.hpp), `tests/asrc_compensator_parity_1367.rs` (the trace carries `rec=` / `recp=`; the step
  scenario must show a payment at −1000 ppm), `tests/genlock_lock_indicator_guards.rs` (widget wiring:
  the booking is the member `OBSBasicStatusBar::BookGenlockWallStep`, called right before the history
  push; the two new log families mutually non-substring), the pwsh blocks in both
  `windows-genlock*.yml`. The render-tick needles are ONE list (`RENDER_TICK_WIRING` in
  `tests/genlock_wall_step_parity_1372.rs`) that the Rust guard AND
  `windows_workflows_guard_the_same_render_tick_wiring_1372` check against both workflow files — review
  round 2 found the pwsh copy still requiring the round-0 line, which would have failed both Windows
  builds at the guard step.
- `src/genlock_wall_step_bench.rs` (test-only): the logged step against all three. Production: 1
  off-grid tick, 0 duplicate/skipped stamps (also on a 0–30 ms emit spread, where the legacy slew
  stamps a repeat and a skip), a stalled re-grid still landing within one more tick, 0 s DEGRADED,
  the `mbc` level within 2 ms of target from +60 s. Legacy on the same feed: 8 off-grid ticks, 300 s
  DEGRADED, still up to ~36 ms off at +60 s.
- The bench's RECEIVER arm feeds the stepped stamps into the production N==1 FIFO port
  (`genlock_grid_bench::Fifo`) on its own stepped render tick: the deep `2ME PGM` (987 ms) and a
  shallow cg feed (3 ms). Measured as the viewer sees it (held ticks, backward presents, skipped
  slots), never the FIFO `resyncs` counter (on the per-second grid every third stamp interval is 100 ns
  over `interval`, which the port books as a GAP RESYNC ~10×/s with or without a step). Production
  costs ≤ 5 visible events per step and never more than the legacy slew, 0 relocks, 0 underruns; the
  deep FIFO pays strictly less than on the legacy slew. On the shallow 3 ms feed the receiver's own
  re-grid tick costs one held frame the legacy slew spreads out; the sender side pays that back.
- Local, no cargo: a scratch lib.rs with `#[path]` includes of `asrc_bench`, `genlock_grid`,
  `genlock_lock_state`, `genlock_wall_step` (+ the bench under `#[cfg(test)]`), `rustc --test -O`; for
  the parity files a stub `camera_box` rlib of the same modules (+ `genlock_forced_table_audit`) and
  `CARGO_MANIFEST_DIR` + `CARGO_TARGET_TMPDIR` set at COMPILE time; `clippy-driver --test -D warnings`
  on the same inputs. Six scratch C mutants (regrid ignored, `>=` threshold, bracket midpoint, half
  payment, no setpoint move, no book limit) each diverged.

## Live acceptance (supervisor, FULL bundle: libobs + frontend)

At the next fleet date step: one `genlock-regrid:` and one `genlock-wall-step:` line per OBS box
(the Windows boxes and strih-lx alike — a date step moves `CLOCK_REALTIME`, never `CLOCK_MONOTONIC`);
resolume LOCK stays LOCKED; stream `mbc` `recover_ms=` > 0 on the step's `asrc:` line and `level_avg=`
back within ±2 ms of `target=` inside a minute, `applied=` inside its steady band; strih-lx `CG-obs`
and stream `2ME PGM` 0 new relocks at the step.

## Not this path: the resolume `cg-obs` bursts

The `cg-obs` offered 84-then-364 pattern (comment 5841162974) started 6.5 min BEFORE the first date
step. It is the video-io thread lagging (video-io.c cache-full `count += / skipped +=` → dropped
slots then duplicate bursts), not the render tick; see the Design-question 5841262854.
