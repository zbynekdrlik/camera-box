---
paths:
  - "src/capture_phase.rs"
  - "src/capture_phase/**"
  - "src/capture_phase_bench.rs"
  - "src/dupe_decimation/stamp.rs"
  - "src/dupe_decimation/stamp/**"
  - "src/dupe_decimation/gate.rs"
  - "src/genlock_stamp.rs"
  - "src/main.rs"
---

# Capture phase tracker + the stamp-driven emit gate (issue 1367 slice D2)

## The burst it removes

A cambox camera free-runs against the per-second genlock grid (the Cam Links read ~16 ppm), so
its capture phase slides through a slot edge once every ~17 min. Before D2 each frame was stamped
at the floor of its OWN V4L2 timestamp (uvcvideo stamps the host time of the first USB packet) and
the emit gate decided on the poll wall clock after the dequeue. Both carry jitter, so for 15-20 s
around each edge the frames landed on either side at random. Every flip is a shed plus a repeat:
one unique frame lost, the next shown twice. Live CAM5 29.9.2026: 36 blind sheds + 35 starvation
repeats in one burst. strih saw 34-68 `stamp_gap` per crossing. The physical truth is ONE extra or
ONE missing frame per crossing.

## The pieces

- **`CapturePhaseTracker`** (`src/capture_phase.rs`) fits `t = a + P * seq` over the last
  `FIT_WINDOW_FRAMES` (256) frames by least squares.
  - Exact `i128` running sums, re-anchored on the oldest sample at every pop, so the same
    timestamps always give the same fit (the bench's determinism test). No floats until the result.
  - The fit runs over the V4L2 SEQUENCE number (`FrameInfo::sequence`, new). uvcvideo counts every
    frame the DEVICE sent (buffer free or not), so a frame the HOST dropped is a sequence step of 2
    and costs nothing.
  - A frame the DEVICE skipped leaves no sequence step: it arrives a whole number of periods late.
    Its residual is re-read as `k` hidden frames: `seq_advance` and the sequence position grow by
    `k`, `hidden_drops()` counts it, and the gate fills the missing slot with one starvation repeat,
    exactly what today's poll gate does (review round 1, the bench's hidden-skip case). Before
    that fix the two frames after such a skip were stamped one slot early from the old prediction.
    A lone timestamp a whole period LATE (a USB completion delayed by a frame) reads the same; the
    next on-time frame's residual of minus one period then re-seeds. Accepted cost: one repeat, one
    duplicate stamp on the wire (the re-seed frame goes out on the raw path into the slot the late
    frame took: strih `stamp_dup` +1) and ~2 s on today's raw path (pinned by
    `a_single_timestamp_one_period_late_costs_one_hidden_drop_then_one_reseed`); `hidden_drops=`
    on the status line makes it visible.
  - From `OUTLIER_MIN_FRAMES` (30) samples on, a sample more than `RESEED_JITTER_MULTIPLE` (8) x the
    fit RMS off, and at least `RESEED_FLOOR_NS` (1 ms), is not folded. Under half a period it is
    stamped from the prediction and the lock is kept: HOLD, NEVER FOLLOW (issue 1371). Only
    `RESEED_CONSECUTIVE_OUTLIERS` (30, ~0.5 s at 60 fps) in a row re-seed, a real phase step (a
    grabber re-lock); half a period or more re-seeds at once (never a stamp from the old phase).
    A Cam Link on a busy USB root port delivers frames +1..+7 ms LATE for a few frames while the
    sensor keeps its phase. The 14.10.2026 re-anchor (move the fit by the median of 3 agreeing
    outliers) followed those delays: on cam5 eight shifts of +1..+7 ms in 3 s, +30 ms summed, and
    the E2E window read 14/14 (run 1922653182, design 6176639966). The older 3-in-a-row re-seed put
    the stream ~2 s on the raw path = the 1/1 residual. Never move the fit onto a delivery delay.
    The re-seed logs `#1371 capture phase re-seed: reason=outlier-run resid_us=[..]` with the
    whole run.
  - **A constant-offset run is a card re-timing STEP, absorbed, never a re-seed (issue 1482,
    ROZHODNUTÉ 6445636799).** When the run reaches its 30 frames AND every residual lies within
    `max(4 x fit RMS, RESEED_FLOOR_NS / 2)` (500 us on a clean stream) of the run's median
    (`constant_step_ns`), the tracker moves every window sample's time by that median
    (`absorb_step`) and folds the run's 30 samples on the new phase. The `FitSums` are relative to
    the oldest sample, which moves with the rest, so every sum stays exactly what it was: only the
    intercept moves; the slope (the card's rate), the jitter and the lock are kept. One
    `PhaseNote::PhaseStep` and one line `#1482 capture phase step: <+us> us (card re-timing), lock
    kept`; `phase_steps=` counts them. Still a re-seed: a ramp or scattered run, a half-period
    step, a sequence back/jump, a time stall, a gap. Runs shorter than 30 still HOLD (the
    issue-1371 delivery-delay rule is untouched). The cost of a step: during its 30 held frames
    (~0.5 s) the stamps still come from the old phase, then the step reaches the stamps in one
    move; the slot chooser keeps the slot when the moved instant stays inside the 500 us
    hysteresis and makes exactly one duplicate or one missing slot when it crosses an edge.
  - **Why (the owner's ruling):** no capture card, camera, monitor or HDMI source is ever assumed
    to run on the genlock clock, and no fix asks for replacement hardware. cam2's Cam Link reads
    +45 ppm against the SAME splitter signal cam3/cam5 read at +18 ppm: it re-times the frames on
    its own clock and catches up in constant +1.8..+2.6 ms steps (once -6.3 ms) at 1.4..75 s intervals, one per
    ~20 s on average (live
    27.10.2026: 2329 re-seeds on cam2 vs 10 on cam3/cam5). The cambox absorbs it, so the card's
    clock never travels further than the cambox.
  - A backward/stalled sequence (a device re-open), a step above `MAX_SEQ_ADVANCE` (8, hidden
    frames included), a timestamp that does not advance, or a gap longer than
    (sequence step + 1) x `MAX_FRAME_PERIOD_NS` (100 ms) re-seeds at any sample count (the `+ 1`
    is slack for a slow stream's own jitter; a shorter pause folded while seeding only delays the
    lock until that sample leaves the window).
  - **The `i128` range (review round 2).** Two unfolded outliers may sit between pushed samples,
    so the window's sequence span reaches 256 x 3 x 8 = 6144 and its time span ~1.2e12 ns. Then
    `nb^2` can reach ~1.5e40, past `i128::MAX` (~1.7e38): a debug build panicked, a release build
    would have wrapped silently. `nb_sq_over_d` computes `floor(nb^2 / d)` exactly as
    `q * nb + r * nb / d` (`nb = q * d + r`) and never forms `nb^2`; every other product stays
    below ~1e32 (the `FitSums` doc has the arithmetic,
    `a_slow_stream_with_outlier_runs_never_overflows_the_fit` reproduces the overflow).
  - LOCKED (sticky) at `LOCK_MIN_FRAMES` (120) samples with an RMS residual at most
    `LOCK_MAX_JITTER_NS` (1 ms) AND white residuals. It lets go only above `LOCK_EXIT_JITTER_NS`
    (2 ms), so a stream near the bound does not switch paths back and forth. White: no quarter of
    the window may sit more than `WHITENESS_SIGMAS` (5) standard errors off the line (floor 20 us). The standard error uses the
    LOCAL jitter (RMS of consecutive residual differences over sqrt 2), because a phase step folded
    while seeding inflates the fit RMS and would widen its own bound. The O(window) check runs only
    while unlocked, every `WHITENESS_CHECK_EVERY` (16) frames after a failure. The prediction
    error at the newest frame is `2 sigma / sqrt(n)`, 1/8 of the raw jitter.
- **`SourcePhase`** (`src/capture_phase/source_phase.rs`, issue 1482 step 2, design 6449255649):
  the long-horizon SOURCE phase, so a re-timing card's sawtooth never reaches the stamp.
  - Step 1 kept the lock through every card step, but the short fit (~4 s) follows each TOOTH:
    cam2's card runs +45 ppm between steps and catches up later by +1.0..+2.9 ms, ~2 ms peak to
    peak, four times the 500 us hysteresis. Live after step 1 (comment 6449236338): 11 crossings
    + 4 hidden drops in 30 min vs cam3's 2 + 0 on the same signal.
  - A least-squares `t = a + P * x` over the last `SOURCE_FIT_SECONDS` (180 s) of the samples the
    short fit FOLDS, RAW: the card's steps are not removed, so the slope is the source's long-term
    rate and the intercept its mean phase (the sawtooth averaged). The sequence position `x` is the
    short fit's own (hidden drops already re-indexed). One point per `SOURCE_DECIMATION_FRAMES`
    (10) of `x`, at most `SOURCE_FIT_MAX_POINTS` (4096): ~1080 points at 60 fps. The same exact
    `i128` `FitSums` + the shared `line_predict_ns`; the range arithmetic is in the module doc.
  - Fed from `CapturePhaseTracker::push` (so an absorbed run's raw samples reach it and a held
    delivery delay never does), reset from `seed()`: every seed and re-seed (half-period step,
    an outlier run that is no constant step, sequence back/jump, time stall, gap) starts it over;
    an absorbed card step does NOT.
  - `stamp_frame`: while the short fit is locked AND the source fit spans
    `SOURCE_FIT_MIN_SECONDS` (60 s), the smoothed instant is the SOURCE prediction at the frame's
    `x`; before that the short fit's, as before step 2. The slot chooser, the gate, the band check
    (still on the short fit's rate) and the sender path are unchanged.
  - A pass-through card (cam3/cam5): both fits agree within jitter (the unit test bounds the
    difference under 60 us), so its stamps are today's.
  - Cost: for the first 60 s after a start or a real re-seed a stepping card still stamps from
    the short fit and can flip at an edge, as with step 1 only.
- **`SlotHysteresis`** places the smoothed realtime instant in a grid slot that advances by
  exactly the SEQUENCE advance (a drop expects +2, never a false crossing). A slot one earlier or
  one later is accepted only once the instant is `SLOT_HYSTERESIS_NS` (500 us) past the edge: one
  duplicate slot per fast crossing, one missing slot per slow one. The stamp stays within 500 us
  of the smoothed instant: up to 500 us AFTER it on the fast side. The frame still has to finish
  its USB transfer and be dequeued before it is sent (the bench models 11 ms), so the stamp stays
  at or before the send instant, as the sender contract §4 requires.
- **`CapturePhase::stamp_frame`** returns the slot only while the fit is locked AND the camera runs
  within `STAMP_MODE_MAX_RATE_PPM` (2000) of the emit rate; once stamp-driven it stays up to
  `STAMP_MODE_EXIT_RATE_PPM` (2500), the band's hysteresis. `None` otherwise, and the capture loop
  keeps today's raw stamp + poll-time gate byte-identically. That is the fail-safe.
  `capture_phase::stamp_instant_100ns` is the ONE helper main.rs floors into the timecode (the
  slot middle while driven, else today's raw capture instant).
  - **A realtime clock step is not a crossing (review round 2).** When the mono->real offset moves
    by `CLOCK_STEP_NS` (1 ms) or more between two stamp-driven frames, the slot chooser re-anchors
    on the new floor (a `Start`), so `crossings=` counts only the camera drift, and a step that
    lands the phase just past an edge never leaves the stamp one slot behind the realtime floor
    (`a_realtime_step_landing_inside_the_hysteresis_follows_the_new_floor`, review round 3). A
    slewing clock never trips it: dantesync's `adjtimex` frequency and offset slews move
    `CLOCK_MONOTONIC` and `CLOCK_REALTIME` together, so the offset moves only on a step. The gate
    still sees a real step as a slot jump and fills or skips it like the poll-time gate.
  - **The offset changes epoch in the frame the wall steps (issue 1372, ROZHODNUTÉ 6033853074).**
    `main.rs` holds it in `genlock_stamp::StampOffset`: seeded before the loop, then fed ONE
    bracketed read per captured frame (`read_mono_wall_mono_ns`: mono, wall, mono, judged by
    `genlock_wall_step::wall_offset_ns`) at the top of the callback, before `stamp_frame`. A
    difference of 2 ms or more against the cached offset (`OFFSET_STEP_RESAMPLE_NS`, the render
    tick's `WALL_STEP_MIN_NS`) is adopted at once and logged once (`#1372 stamp offset re-sampled
    on a wall step of <ms>`); a smaller one waits for the 100-frame cadence (the tracker re-anchors
    there, as before); a preempted read (bracket over 100 µs) is never adopted, so it can never
    re-anchor (review round 4's double re-anchor on a preempted read is gone). The bench:
    `a_whole_slot_date_step_re_anchors_by_whole_slots_with_no_crossing_1372` switches the stamp
    epoch with the first frame READ after the step (captured 9 ms before it and dequeued 2 ms
    after: the bench steps the clock at the read; was 924 ms late), still 97 / -95 / 13 whole
    slots with 0 off-slot intervals; `a_preempted_clock_read_never_moves_the_stamps_1372`
    keeps the stamps one slot per frame across an 8 ms preempted read 2.5 ms before an edge.
- **The gate** (`DecimationGate::note_stamp_slot` before the unchanged `poll` call) decides on the
  slot alone via the pure `dupe_decimation::stamp_slot_action`:

  | vs the last EMITTED slot | action |
  |---|---|
  | none yet | `Latch`: emit |
  | +1 | `Advance`: emit |
  | same | `Duplicate`: drop (counted as a blind shed) |
  | +2..+9 | `Gap`: emit after the existing starvation repeats fill the missing slots (consecutive cap `STARVATION_REPEAT_MAX` kept, so a half-rate leg still looks down) |
  | further, or backward | `Resync`: emit, re-latch, no fill |

  The poll wall clock, the queue signals and the unique-rate windows are not read.
  `note_emitted_stamp_100ns` records every emitted stamp on BOTH paths, so the stamp path
  continues from the poll-time one without a false gap. Back on the poll-time gate
  (`relatch_after_stamp_path`), the boundary stays on the capture grid while the queue still
  holds frames, so the issue-1131 catch-up emits every buffered frame one slot each (review round
  2: re-latching at once on the poll slot blind-shed the rest of the backlog). At the first frame
  the loop waited for, it re-latches on that poll's own slot; the slots between the frame's own
  capture slot (the poll instant minus its queue residence) and the poll slot are counted as
  intentional (`last_poll_relatch_extra`), so leaving the stamp path never logs a phantom
  `#707 SKIPPED` (review round 1).
  - **When a stamp-driven stream logs `#707 SKIPPED`:** only for missing slots nothing filled. That
    is a forward jump past 8 slots (a clock step, a device hole: all of it), the rest of a 5..8-slot
    gap beyond the 4 repeats, or any gap once the consecutive repeat budget is spent (a half-rate
    leg). A forward clock step of 2..8 slots is filled with repeats, as today's gate does.
  - **The gap fill matches today's gate, not only "one slot".** The design says "fill only a real
    one-slot gap"; the stamp path fills gaps of 1..8 missing slots with up to 4 repeats, exactly
    the poll-time starvation fill it replaces, so a device hole costs the same on both paths. A
    crossing itself is always a one-slot gap. Recorded on issue 1367 with the band decision.

## Why the 1:1 band (over-rate grabbers keep today's gate)

This limit is an implementation decision within the design's fail-safe; the reasons are recorded
on issue 1367 (comment 5882620373). D2 does not extend the stamp-driven gate to the over-rate
regime.

The burst is a near-1:1 phenomenon: its length is the jitter width over the phase drift per frame.
A 61.5 fps ShadowCast drifts 0.4 ms per frame, so a flip touches at most one frame. Its surplus and
its send-bound queue residence are what the `dupe_decimation` #1145/#1167 machinery absorbs
(retire, depth drain, fast drain, starvation fill), tuned against the poll time on those boxes.
A stamp-driven drain there would turn every residence drop into a gap fill (an extra send on a
send-bound loop). The band (2000 ppm to enter, 2500 to leave) covers a free-running camera and
59.94-into-60 (-1000 ppm) and stays below the gate's over-rate takt threshold (60.3 fps, ~4975
ppm). The `lib.rs` const-assert pins the exit edge, so a stamp-driven stream never reads as
over-rate.

## Unchanged (keep it that way)

- The issue-1131 rule: on the stamp path a frame drained late from a backlogged queue still emits
  (its slot is new). A re-latch needs a stamp jump beyond 8 slots, which is not buffered content.
  On the way back to the poll-time gate the boundary waits on the capture grid until the queue is
  empty (`buffered_frames_after_the_stamp_path_all_emit`).
- The #1242 send stagger (it sleeps after the timecode and after the gate), the buffered-queue
  signal, `emit_one`, and the starvation-repeat timecodes (`starvation_repeat_timecode_100ns`).
- Every self-heal trigger and its byte-anchored log text.
- The `#707 SKIPPED` line for a real clock step: the stamp path sets the boundary to the slot after
  the emitted one and folds the fill into the intentional extra advance, so a forward step logs
  one SKIP (the bench: a +700 ms step = 42 slots, one line).
- The mono->real offset re-sample every 100 frames (now inside `genlock_stamp::StampOffset`),
  plus the issue-1372 wall-step re-sample in the step frame. The tracker is MONOTONIC, so a
  realtime step never re-seeds it; the step shows as one stamp jump in the step frame (the slot
  chooser re-anchors, no crossing is counted).
- `harness_send_stagger_1242.rs` pins `let emit = decimation_gate.poll(` and friends as UNIQUE
  text in main.rs. That is why the slot is STAGED (`note_stamp_slot`) instead of a second poll call.

## Observability

The 5 s `#707 emit-1s: [..] cap-1s: [..] (1-second buckets, oldest first)` line appends
` phase_lock=seed|band|stamp phase_ppm=+15.9 src_ppm=+15.7 jitter_us=41 crossings=2 reseeds=0
hidden_drops=0 phase_steps=0`:

- `phase_lock`: `seed` = seeding (raw path), `band` = locked but outside the 1:1 band (raw path),
  `stamp` = stamp-driven, `off` = genlock off (no emit grid).
- `phase_ppm`: the camera frame RATE offset from the emit rate (positive = faster = its crossings
  drop a duplicate slot). The LS slope noise is ~3 ppm (1 sigma) at 60 us jitter, so read a trend,
  not one line.
- `src_ppm` (issue 1482 step 2): the SOURCE fit's rate, same sign as `phase_ppm`; `na` until
  the source fit spans its 60 s. On a pass-through card it reads `phase_ppm` within noise; on a
  re-timing card it reads the source's rate while `phase_ppm` keeps the card's local rate (cam2:
  `phase_ppm` ~+45, `src_ppm` ~ what cam3's `phase_ppm` reads on the same signal).
- `jitter_us`: the fit RMS (the raw V4L2 timestamp jitter).
- `crossings` / `reseeds` / `hidden_drops`: cumulative since the process start. `crossings` counts
  camera-edge crossings only (a realtime clock step re-anchors, uncounted); `hidden_drops` counts
  frames the device skipped without a sequence step (each costs one starvation repeat), so a lone
  repeat with no `crossings` increase is attributable.
- `phase_steps`: cumulative card re-timing steps absorbed with the lock kept (issue 1482). A
  re-timing card (cam2, one step per ~20 s live) grows it ~3/min while `reseeds=` stays flat; a
  pass-through card stays at 0.

Every absorbed card step logs ONE `#1482 capture phase step: <+us> us (card re-timing), lock kept`
line (`PhaseNote::log_line()`, the whole line main.rs writes). Every re-seed and every hidden drop
also logs ONE line with what caused it (issue 1371),
`#1371 capture phase re-seed: reason=<r> ...` or `#1371 capture phase hidden drop: frames=<k>
resid_us=<+n>`. Reasons: `outlier-run resid_us=[a,b,c]` (the 3 residuals, oldest first),
`half-period resid_us=<n>`, and `sequence-back` / `sequence-jump` / `time-stall` / `gap` with
`seq_step=` + `dt_us=`. Residuals are signed us, positive = the frame arrived LATE. The tracker
stores it as `PhaseNote` (`CapturePhase::note()`, cleared by the next frame); `describe()` builds
the text. Use it to tell a delivery delayed on a shared USB bus (cam5: the bkshading relay polls a
BMPCC on the Cam Link's bus 4, re-seeds ~1 per 10 s relay cycle) from a real phase step.

All keys are mutually non-substring with each other and with `emit-1s:` / `cap-1s:`
(`status_tokens_parse_and_their_keys_are_mutually_non_substring` parses the EMITTED line). The
existing parsers match the prefix:
`residual_churn_attribution._B707_RE` (pinned by
`test_b707_line_with_the_capture_phase_tokens_still_parses_1367`) and the leg-health
`cap-1s: \[..\]` grep (`cap1s_band_warn_reads_the_line_with_the_capture_phase_tokens_1367`).
`tests/harness_capture_phase_wiring_1367.rs` pins the main.rs wiring (tracking before the gate,
the staged slot before the ONE poll, the stamp instant, the emitted-stamp record, the tokens).

## The bench (`src/capture_phase_bench.rs`)

A +-16 ppm camera, 2300 s (two crossings), driven frame by frame through the REAL
`DecimationGate::poll` (raw) and through `CapturePhase` + `note_stamp_slot` (tracked), exactly as
`main.rs` wires them. Every emitted stamp feeds the receiver's own `genlock_grid::StampTrack`.

- **Jitter model** (the only calibrated knobs): the timestamp and the dequeue each get a core
  Gaussian of 8 us plus, on 5 % of frames, a 100 us one. With i.i.d. Gaussian noise the flips per
  crossing are `~1.13 sigma / drift-per-frame`: hundreds at the measured burst width. The live
  counts need a narrow core with a sparse wide tail: the tail sets the width, the core and tail
  rate set the count. Re-derive from new journal bursts, never to make a change pass.
- **Numbers** (seed `0x1367_d2be`):
  - +16 ppm raw: cambox 39 / 41 / 45 shed+repeat events per poll-time crossing (6-16 s wide);
    strih 71 / 57 stamp flips per stamp crossing (19-26 s wide). Tracked: exactly 1 blind shed
    per crossing, 0 strih flips.
  - -16 ppm raw: cambox 47 / 51, strih 81 / 69. Tracked: exactly 1 starvation repeat per
    crossing, 0 strih flips.
  - The tracked crossing lands ~31 s after the true one (500 us of hysteresis at 16 us/s).
  - A dropped frame = 1 repeat for its own slot, no re-seed. A frame the device skipped (no
    sequence step) = 1 repeat, exactly like today's gate. A hidden 1.5 s pause = 1 re-seed + one
    SKIP line. A +-700 ms realtime step = 1 re-latch (+700: one 42-slot SKIP line), 0 re-seeds. A
    re-open (1.2 s hole, new phase, sequence from 0) = 1 re-seed + one SKIP line. Same timestamps
    give the same stamps.
  - At the design's 0.3 ms i.i.d. Gaussian timestamp jitter the tracker still gives exactly 1 per
    crossing (2 crossings, 0 receiver flips, 0 re-seeds).
  - **Re-timing card (issue 1482)**, cam2's live numbers: a +45 ppm card clock, 48 seeded catch-up
    steps of +1.8..+2.6 ms (the 8th -6.3 ms) at random 2..75 s intervals (the design's band, one
    per ~38 s on average, half the live rate) over 30 min, seed
    `0x1482_ca2d`. With step 1: 48 `phase_steps`, 0 re-seeds, 5 crossings for the 5 edge crossings
    of the card's stepped instants = 2 blind sheds + 3 starvation repeats, 0 receiver dup/gap, 0
    SKIPs. With step 2 the slot follows this schedule's source line (~-9 ppm): 1 crossing, 1
    starvation repeat, pinned. Before it (the
    RED commit): 48 re-seeds, 3 receiver gaps, 3 SKIPs; the raw path alone 109 sheds + 110 repeats
    and 48 dup + 101 gap at the receiver.

  - **Sawtooth card (issue 1482 step 2)**, `a_sawtooth_card_stamps_the_source_phase_1482`: a
    self-consistent model of cam2's live sawtooth. The card's clock runs +45 ppm, the source
    -59 ppm, and the card catches up LATER by a step drawn from +1.0..+2.9 ms each time its offset
    to the source reaches 0. The sawtooth stays bounded (0..2.9 ms) and the step contribution is
    exactly the rate difference: +104 ppm, ~95 steps in 30 min, one per ~19 s. So the source rate
    = the local rate minus the step contribution (45 - 104). The live figures (+45 local, +104 of
    steps, +18 on cam3) cannot all hold at once under that identity; the bench takes the card's two
    measured numbers and derives the source. 30 min through the REAL `DecimationGate`, seeds
    `0x1482_5a7e` / `0x1482_0001` / `0x1482_0002`: 6 source edge crossings each (16-18 tooth
    crossings). Step 1 only (the RED commit): 6 / 6 / 10 tracked crossings, the third = 2 blind
    sheds + 8 repeats (a tooth crossing an edge back and forth); raw path 35-46 sheds + 41-52
    repeats, 68-77 dup + 90-105 gap at the receiver. With step 2: 6 / 6 / 6, every one a
    starvation repeat (the source is slower than the grid), 0 receiver dup/gap, 0 re-seeds, 0 SKIPs.
    The same card over a SLOW source, +18 ppm as cam3/cam5 read live
    (`a_sawtooth_card_over_a_slow_source_stamps_the_source_phase_1482`, ~+27 ppm of steps): the
    source line lingers near each edge for minutes. Step 1 only: 2 / 6 / 8 tracked crossings for 2
    source edges (blind sheds AND repeats: back and forth). With step 2: 2 / 2 / 2, each one blind
    shed (the source is faster), 0 receiver dup/gap, 0 re-seeds.

## Verifying a change

- **A bench must FAIL on the old code, and one seed may not show it** (issue 1482 step 2): the
  live-shaped sawtooth (-59 ppm source) separated step 1 from step 2 on only 1 of 3 seeds (6/6/10
  vs 6/6/6); a SLOW source (+18 ppm, the line lingers at an edge) separated it on 2 of 3 (2/6/8 vs
  2/2/2). Sweep the rate and a few seeds against the OLD tree before picking the bench case (a
  throwaway exploration test appended to the bench, removed before commit), and record the old
  numbers in the test doc.
- A module moved between files loses the intra-doc links its old glob import resolved. Adding a
  `use super::*` to keep them trips `unused_imports` (doc links do not count as uses; checked with
  rustc 1.97). Add link reference definitions instead (`/// [`X`]: crate::path::X`), as
  `dupe_decimation/shed_log.rs` does.
- **A NEW test against the OLD code** (proving a test would have caught a bug after the fix already
  landed): never check out another commit in the worktree; overwrite only the files under test in a
  scratch copy with `git -C <worktree> show <old-sha>:<path>` (plus the old `lib.rs`, whose
  const-assert names the old constants). Round 3 proved the clock-step-inside-the-hysteresis test this way.
- A debug build (`cargo test`, no `--release`) has overflow checks on, so an `i128` product that
  would wrap silently in the release binary panics there: build the worst-case window in a test
  (`a_slow_stream_with_outlier_runs_never_overflows_the_fit`) instead of trusting a bound comment.
- **A bench clock step applies at the instant the code READS the clock** (issue 1372 review round 1):
  the capture loop reads the wall at the dequeue, ~11 ms after the capture, so a frame captured
  just before a step and dequeued after it already reads the stepped wall. Keying the step on the
  capture time made the bench's "exactly as main.rs" read unfaithful and hid that early frame. Step
  every modelled read (the per-frame bracket and the poll) at its own instant.

## Live acceptance (supervisor)

1. Deploy the camera-box binary to cam5 first (a Cam Link, 1:1). Read its `#707 emit-1s` line:
   `phase_lock=stamp` within ~2 s of start, `phase_ppm` ~ the camera's drift, `jitter_us` in the
   tens, `reseeds=0`, `hidden_drops=0` (a rise with a starvation repeat is a device skip).
2. Watch at least 2 crossings (~35 min). Per crossing: `crossings=` +1; the `(#889)` line shows ONE
   blind-pacing shed (fast camera) or ONE starvation repeat (slow) instead of a 15-20 s burst
   (success: sheds + repeats <= 2 per crossing). strih `genlock-fifo audit 'NDI cam5'`
   `stamp_gap=` + `stamp_dup=` <= 1 per crossing, 0 new relocks, `n2_early=` unchanged.
3. Then the fleet. cam1/cam2 (over-rate ShadowCast) must read `phase_lock=band`: today's path.
4. Issue 1482 step 2, cam2 (the re-timing card), 30 min: `reseeds=0`, `crossings=` and
   `hidden_drops=` at cam3's level (one per real ~17-min source edge), `src_ppm` ~ cam3's
   `phase_ppm` while cam2's `phase_ppm` keeps its card's ~+45.
5. Then an E2E and the restart-matrix `cambox` kind, recording each window's position in the
   17 min cycle (finding 5881791402).

## Known limits

- The cross-model latency offset (cam4 NZXT sits 6.8 ms off the Cam Links) is untouched. Removing
  it needs grabber-side timestamps (design Approach 3, per model on the rig).
- The one-frame wrap every ~17 min is physics for a free-running camera; D2 makes it one clean
  event.
- A seeding or re-seeding stream (2 s after a start or a re-open) runs today's path.
- Issue 1482 step 2: the source fit needs 60 s after every start or real re-seed; until then a
  re-timing card stamps from the short fit and may flip at an edge. The switch itself moves the
  smoothed instant from the short fit's tooth onto the source line (up to about half a tooth), so
  near an edge it can make one slot event, like a tooth return.
- A LONE absorbed step (live once: -6.3 ms among cam2's catch-ups, or a constant delivery delay
  absorbed and later reversed) is NOT reset away: the source line walks onto the new phase over the
  180 s window, its least-squares end value overshooting by up to a third of the step. Pinned by
  `a_lone_absorbed_step_walks_the_source_onto_the_new_phase_in_one_crossing_1482`: a -6.3 ms step
  that moves the camera one slot = exactly one crossing, the estimate at most the step after the
  capture (6.3 ms, inside the ~11 ms capture-to-send margin) and 2.1 ms before it. A guard (reset
  the source fit on a step beyond the sawtooth band) was not added: the design keeps every absorbed
  step in the fit. A card whose source rate
  itself changes within 180 s (not seen live) would be followed with the window's lag.
- Issue 1482's constant-offset test has a floor of 0.5 ms around the run's median, so a run whose
  residuals span 1 ms or less is absorbed as ONE step even when it is a slow ramp (the next run of
  the ramp is absorbed again, step-wise); only a wider ramp re-seeds. Pinned by
  `the_constant_offset_tolerance_edges_1482`.
- A delivery delay that stays CONSTANT for 30 frames or more is now absorbed as a step, and the
  return to the true phase as a second step of the opposite sign. Each direction costs ~0.5 s of
  stamps on the stale phase and, near an edge, one dup or one gap that `crossings=` counts like
  camera drift. Read a `phase_steps=` pair of opposite sign close together as a delivery delay,
  not as card behaviour (a re-timing card steps one way, the other only rarely).
