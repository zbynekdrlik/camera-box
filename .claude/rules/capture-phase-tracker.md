---
paths:
  - "src/capture_phase.rs"
  - "src/capture_phase/**"
  - "src/capture_phase_bench.rs"
  - "src/dupe_decimation/stamp.rs"
  - "src/dupe_decimation/stamp/**"
  - "src/dupe_decimation/gate.rs"
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
    frame the device started, so a dropped USB frame is a sequence step of 2 and costs nothing.
  - A sample more than `RESEED_JITTER_MULTIPLE` (8) x the fit RMS off, and at least
    `RESEED_FLOOR_NS` (1 ms), is not folded; its stamp is the prediction.
    `RESEED_CONSECUTIVE_OUTLIERS` (3) in a row is a real phase step and re-seeds.
  - A backward/stalled sequence (a device re-open) or a step above `MAX_SEQ_ADVANCE` (8) re-seeds.
  - LOCKED at `LOCK_MIN_FRAMES` (120) samples with an RMS residual at most `LOCK_MAX_JITTER_NS`
    (1 ms). The prediction error at the newest frame is `2 sigma / sqrt(n)`, 1/8 of the raw jitter.
- **`SlotHysteresis`** places the smoothed realtime instant in a grid slot that advances by
  exactly the SEQUENCE advance (a drop expects +2, never a false crossing). A slot one earlier or
  one later is accepted only once the instant is `SLOT_HYSTERESIS_NS` (500 us) past the edge: one
  duplicate slot per fast crossing, one missing slot per slow one. The stamp stays within 500 us
  of the smoothed instant (up to 500 us AFTER it on the fast side, never near the send instant,
  which is ≥ 9 ms later).
- **`CapturePhase::stamp_frame`** returns the slot only while the fit is locked AND the camera runs
  within `STAMP_MODE_MAX_RATE_PPM` (2000) of the emit rate. `None` otherwise, and the capture loop
  keeps today's raw stamp + poll-time gate byte-identically. That is the fail-safe.
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
  continues from the poll-time one without a false gap; a poll-time poll after the stamp path
  re-latches on its own slot (its boundary sat a dequeue latency behind).

## Why the 1:1 band (over-rate grabbers keep today's gate)

The burst is a near-1:1 phenomenon: its length is the jitter width over the phase drift per frame.
A 61.5 fps ShadowCast drifts 0.4 ms per frame, so a flip touches at most one frame. Its surplus and
its send-bound queue residence are what the `dupe_decimation` #1145/#1167 machinery absorbs
(retire, depth drain, fast drain, starvation fill), tuned against the poll time on those boxes.
A stamp-driven drain there would turn every residence drop into a gap fill (an extra send on a
send-bound loop). The band (2000 ppm) covers a free-running camera and 59.94-into-60 (-1000 ppm)
and stays below the gate's over-rate takt threshold (60.3 fps, ~4975 ppm). The `lib.rs`
const-assert pins that, so a stamp-driven stream never reads as over-rate.

## Unchanged (keep it that way)

- The issue-1131 rule: on the stamp path a frame drained late from a backlogged queue still emits
  (its slot is new). A re-latch needs a stamp jump beyond 8 slots, which is not buffered content.
- The #1242 send stagger (it sleeps after the timecode and after the gate), the buffered-queue
  signal, `emit_one`, and the starvation-repeat timecodes (`starvation_repeat_timecode_100ns`).
- Every self-heal trigger and its byte-anchored log text.
- The `#707 SKIPPED` line for a real clock step: the stamp path sets the boundary to the slot after
  the emitted one and folds the fill into the intentional extra advance, so a forward step logs
  one SKIP (the bench: a +700 ms step = 42 slots, one line).
- The mono->real offset re-sample every 100 frames. The tracker is MONOTONIC, so a realtime step
  never re-seeds it; the step shows as one stamp jump at the next re-sample.
- `harness_send_stagger_1242.rs` pins `let emit = decimation_gate.poll(` and friends as UNIQUE
  text in main.rs. That is why the slot is STAGED (`note_stamp_slot`) instead of a second poll call.

## Observability

The 5 s `#707 emit-1s: [..] cap-1s: [..] (1-second buckets, oldest first)` line appends
` phase_lock=seed|band|stamp phase_ppm=+15.9 jitter_us=41 crossings=2 reseeds=0`:

- `phase_lock`: `seed` = seeding (raw path), `band` = locked but outside the 1:1 band (raw path),
  `stamp` = stamp-driven.
- `phase_ppm`: the camera frame RATE offset from the emit rate (positive = faster = its crossings
  drop a duplicate slot). The LS slope noise is ~3 ppm (1 sigma) at 60 us jitter, so read a trend,
  not one line.
- `jitter_us`: the fit RMS (the raw V4L2 timestamp jitter).
- `crossings` / `reseeds`: cumulative since the process start.

All keys are mutually non-substring with each other and with `emit-1s:` / `cap-1s:`
(`status_tokens_are_parseable_and_mutually_non_substring`). The existing parsers match the prefix:
`residual_churn_attribution._B707_RE` (pinned by
`test_b707_line_with_the_capture_phase_tokens_still_parses_1367`) and the leg-health
`cap-1s: \[..\]` grep.

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
  - A dropped frame = 1 repeat for its own slot, no re-seed. A +-700 ms realtime step = 1 re-latch
    (+700: one 42-slot SKIP line), 0 re-seeds. A re-open (1.2 s hole, new phase, sequence from 0)
    = 1 re-seed + one SKIP line. Same timestamps give the same stamps.

## Verifying a change (Tier-0)

- The pure modules + the bench compile standalone: a scratch crate whose `lib.rs` declares
  `genlock_grid`, `genlock_pacing`, `genlock_stamp`, `dupe_decimation`, `capture_phase` and
  `capture_phase_bench` as plain `mod` items with the source files SYMLINKED in (so the child
  modules `capture_phase/tests.rs` and `dupe_decimation/stamp/tests.rs` resolve normally). Add a
  stub `ndi.rs` holding the real `floor_boundary_100ns` (awk it out of `src/ndi.rs`) and copy the
  `lib.rs` const-assert. Then `rustc --edition 2021 --test -D warnings lib.rs` and run it, and
  `clippy-driver --edition 2021 --test -D warnings lib.rs`. The whole replica runs 141 tests in
  ~1 s. Put the steps in a script file: the worktree guard refuses variable-driven one-liners.
- `tests/harness_send_stagger_1242.rs` reads main.rs text only: build it with plain `rustc
  --test` and run it from the worktree root with `CARGO_MANIFEST_DIR` set (again from a script).
- A module moved between files loses the intra-doc links its old glob import resolved. Adding a
  `use super::*` to keep them trips `unused_imports` (doc links do not count as uses; checked with
  rustc 1.97). Add link reference definitions instead (`/// [`X`]: crate::path::X`), as
  `dupe_decimation/shed_log.rs` does.
- `main.rs` compiles first on CI.

## Live acceptance (supervisor)

1. Deploy the camera-box binary to cam5 first (a Cam Link, 1:1). Read its `#707 emit-1s` line:
   `phase_lock=stamp` within ~2 s of start, `phase_ppm` ~ the camera's drift, `jitter_us` in the
   tens, `reseeds=0`.
2. Watch at least 2 crossings (~35 min). Per crossing: `crossings=` +1; the `(#889)` line shows ONE
   blind-pacing shed (fast camera) or ONE starvation repeat (slow) instead of a 15-20 s burst
   (success: sheds + repeats <= 2 per crossing). strih `genlock-fifo audit 'NDI cam5'`
   `stamp_gap=` + `stamp_dup=` <= 1 per crossing, 0 new relocks, `n2_early=` unchanged.
3. Then the fleet. cam1/cam2 (over-rate ShadowCast) must read `phase_lock=band`: today's path.
4. Then an E2E and the restart-matrix `cambox` kind, recording each window's position in the
   17 min cycle (finding 5881791402).

## Known limits

- The cross-model latency offset (cam4 NZXT sits 6.8 ms off the Cam Links) is untouched. Removing
  it needs grabber-side timestamps (design Approach 3, per model on the rig).
- The one-frame wrap every ~17 min is physics for a free-running camera; D2 makes it one clean
  event.
- A seeding or re-seeding stream (2 s after a start or a re-open) runs today's path.
