---
paths:
  - "src/genlock_pacing.rs"
  - "src/capture_stall.rs"
  - "src/dupe_decimation/**"
---

# Classifying a `#707 genlock emit-gate SKIPPED` event as LOSSY vs BENIGN (issue 1131)

A `#707 genlock emit-gate SKIPPED boundaries … totalling N boundary interval(s)` WARN does
**NOT** by itself mean N frames were lost. The number is the boundary-COUNTER leap from
`genlock_pacing::boundary_skip_count` when `genlock_emit_gate`'s resync branch fires
(`lag_intervals > GENLOCK_MAX_CATCHUP_INTERVALS(8) && !queue_had_frame`). That branch is the
HONEST clock-STEP / empty-queue path (#131 cold-boot resync): the loop genuinely waited for the
triggering frame, so the skipped boundaries had **no captured content** — leaping them loses
nothing. A CLOCK_REALTIME step re-latches the epoch-relative grid and the counter jumps, while
capture (monotonic) keeps flowing 60 fps and every captured frame is still emitted.

The `queue_had_frame=true` (buffered-drain) case NEVER reaches resync — it catches up ONE
interval per poll, so it can't skip >1 boundary while unemitted captured frames exist. That IS
the issue-1131 invariant, and it is already enforced in `genlock_pacing.rs`. Empty-queue
starvation slots get a bounded last-frame REPEAT fill (issue 1167 v4, `dupe_decimation/gate.rs`).

## The triage procedure (do this before ever "fixing" a #707 skip — it is usually benign)

Two independent correlations, from an E2E run's own durable artifacts under
`/tmp/recording-e2e-<id>/` (or downloaded from the green run's artifacts):

1. **SOURCE side — the decisive one.** In the cambox burn log (`camN-cbox-burn-<id>.log`) each
   `#707 emit-1s: [..] cap-1s: [..]` status line (~every 5 s, 5 sliding 1-s buckets, oldest
   first) is the ground truth. Compute `cap − emit` per bucket AT the skip timestamp and in the
   next line. A **genuine N-frame drop shows a ~N deficit in some bucket**; a benign grid
   re-latch shows deficit 0 (a transient +1 that recovers to 0 in the next 5-s line is just a
   sliding-bucket boundary, not a loss). Also scan the WHOLE run's worst per-bucket deficit —
   if it never approaches N and never lands at a skip time, no skip dropped frames. (A steady
   `cap−emit` of 1–2 with cap at 61–62 is CORRECT decimation of an over-rate ShadowCast grabber,
   #909 — not loss.)
2. **RECORD side.** In `verdict-<id>.json`, `all_cambox_continuity.segments[]` are the per-cambox
   ON-PROGRAM windows (`start_ns`/`end_ns` epoch-ns; a cam has segments only while it was on
   program). Convert the skip UTC to epoch and check whether it even falls in that cam's segment.
   A skip outside the window never reached the recording. Inside a segment, a benign residual
   shows `gaps` with the diffuse `issue 883 fallback: … no delta above the outlier ceiling (10)`
   reason (the rig's ordinary steady-state residual), NOT a single `residual` with large
   `missing_slots` (the multi-slot-leap signature). Note the outlier ceiling is 10, so a 9-leap
   would hide in the 883-fallback bucket — the SOURCE emit==cap check (1) is what disambiguates.

Reusable scripts: see the issue-1131 close comment (issuecomment-5492634259) — a ~40-line python
that mines all skip events × emit/cap deficit × verdict segment across several runs.

**Re-open condition for the nearest-frame degrade** (issue 1131, closed overcome 2026-09-01):
a skip with `cap−emit ≥ 2` SUSTAINED across ≥2 consecutive 5-s buckets, or a verdict cam segment
with one `residual missing_slots` well above the diffuse 883-fallback profile. Until then a
nearest-frame degrade would over-emit on benign clock re-latches (there are no captured frames
for the skipped boundaries to degrade to).
