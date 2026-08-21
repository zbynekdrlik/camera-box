---
paths:
  - "src/dupe_decimation.rs"
  - "src/genlock_pacing.rs"
  - "src/ndi.rs"
  - "src/emit_skip_log.rs"
---

# Cam-box capture→emit genlock pacing (the #707/#889/#1111 gate)

The cam box captures faster than 60 (ShadowCast free-runs ~61–64 fps) and DECIMATES onto a
wall-clock 60 fps grid before NDI-emitting to the strih genlock-FIFO. Four cooperating pieces,
all pure `cfg(target_os="linux")` logic (NOT probe-gated → Tier-0 testable via `cargo test
--no-run --lib` then running `target/debug/deps/camera_box-*` directly):

The pacing GATE math lives in its own crate-root module `src/genlock_pacing.rs` (issue 1113 —
extracted verbatim out of the then-2555-line `ndi.rs`), gated `#[cfg(target_os="linux")]` in
lock-step with `ndi`. Gotcha when you move doc-commented code between modules like this: BARE-name
intra-doc links (`[`next_boundary_100ns`]`) that resolved inside `ndi.rs` BREAK in the new module —
re-qualify them to `[`crate::ndi::…`]`. `ndi.rs` still owns the NDI-timecode grid
(`next_boundary_100ns` / `fps_from_frame_rate`) the gate complements but does not depend on.

- `genlock_pacing::genlock_emit_gate(now, next_boundary, interval)` → `(would_emit, next)` — the
  wall-clock grid. Emits the first capture at/after each boundary; `#707` resync branch leaps
  forward only when lag > `GENLOCK_MAX_CATCHUP_INTERVALS` (8) = a real clock STEP.
- `genlock_pacing::genlock_emit_on_time(...)` (#1111) → is this an ON-TIME/surplus crossing vs a
  LATE catch-up crossing? Shares `genlock_latched_boundary` with the gate so the two never disagree.
- `dupe_decimation::DecimationGate` (#889) — dupe-preferring victim selection: at an over-rate
  the surplus shed prefers a byte-identical grabber dupe over the unique tick.
- `genlock_pacing::boundary_skip_count` (#707) + `emit_skip_log` (#752) — the `#707 SKIPPED
  boundaries` diagnostic (the WARN is throttled to one aggregate per 5s report).

## GOTCHA — a #889 dupe DEFERRAL must NEVER hold the boundary in the catch-up regime (#1111)

Deferring a dupe (hold the boundary, wait one more capture) is lag-neutral ONLY in the
ON-TIME/surplus regime (the replacement capture lands inside the SAME interval). At a genuine
over-rate a dupe often arrives while the gate is ALREADY LATE (catch-up); deferring THERE holds
the boundary while the wall clock runs on, **ratcheting the gate's lag +1 interval per deferral
until it crosses 8 and trips the #707 resync → ~9 boundaries leapt at once → sub-60 irregular
emit → strih genlock-FIFO relock → visible judder** (issue 1110/1111, live on CAM1). The fix:
gate the deferral on `genlock_emit_on_time`; a LATE dupe is EMITTED instead. Signature of the
symptom in the journal: `#707 ... SKIPPED boundaries ... totalling 9 boundary interval(s)`
repeating every ~10–15 s with `0 capture-dropped` (deterministic beat, NOT random CPU
starvation).

## The over-rate copies ARITHMETIC (not a defect — a mathematical floor)

A grabber at N fps with M byte-identical dupes/sec delivers only **(N−M) UNIQUE fps**. Emitting a
steady 60 from that inherently requires **~2 repeated frames/sec** (58 unique → 60). The receiver
needs steady 60 (no underrun/relock), so those copies are unavoidable — buffering a substitute
frame does NOT help (it also repeats a frame). The alternative (emit <60) is exactly the churn.
So the fix TRADES #707 skips (gaps + relock) for ~2 steady byte-identical copies/s; the copies
land in the E2E verdict `copies` windows and must be re-checked against
`WINDOW_COPIES_GAPS_TOLERANCE` at deploy (that tolerance predates them).

**SUPERSEDED at a GENUINE over-rate by #1145 — the copies are the floor ONLY when the source is
UNIQUE-STARVED (unique rate < 60), never at a plain over-rate.** The arithmetic above conflated two
cases. A grabber over-rating a true-60 source (cam1/cam2 ShadowCast, takt 61.x) delivers ~60 UNIQUE
fps (the over-rate delta IS the dupe rate), so ZERO copies are needed — every dupe can be shed. The
~2 copies/s the pre-#1145 valve emitted there were a BUG: at over-rate the unique rate is exactly 60,
so the emit-gate lag is a driftless random walk and jitter pushes an on-time deferral over the lag==0
hair-trigger, so the next dupe arrives LATE and #1111 copies it (a delta-0 downstream) + a
compensating dropped-unique = the paired "15fps-judder" the #1142 uniformity gate REDs. Only a source
whose UNIQUE rate is genuinely < 60 (a 58-unique grabber, or a 50->60 pulldown padding a sub-60
source by duplication) truly needs copies to hold a steady 60.

## #1145 — stale-boundary RETIREMENT: absorb the over-rate takt without emitting a copy

The FIFTH cooperating piece. A content-dupe crossing an ALREADY-STALE boundary (`lag >= 1` —
`genlock_pacing::genlock_lag_intervals`, the numeric sibling of `genlock_emit_on_time`) is RETIRED
instead of copied: shed the dupe AND advance the stale boundary one interval, emitting NOTHING. The
boundary's downstream hold already happened one interval ago, so retiring it costs no new artifact,
sacrifices no unique, AND drains the dupe-driven lag (the restoring force the bounded-defer variant
lacked — "defer iff lag<BOUND" only postpones the copy, it never CANCELS the debt, so a driftless
walk still eventually trips the resync). Bounded by `RETIRE_MAX_LAG_INTERVALS=4` (<< the resync 8);
above it the #1111 copy valve fires (a panic floor). `genlock_emit_gate` + its resync are UNTOUCHED.

- **Retirement is gated on the UNIQUE rate, NOT the capture takt.** A trailing 2 s `VecDeque` COUNT
  of unique (non-dupe) captures, pruned by `now_ns` on EVERY poll (honest at every instant), is the
  robust "enough distinct content to hold the target without copies" signal — a windowed COUNT reads
  the true unique rate regardless of per-frame jitter / dupe clustering (an interval EMA does NOT: it
  reads local capture spacing during a run of consecutive uniques and leaks). The floor is PARAMETRIC
  (`retire_min_uniques(interval_ns) = WINDOW/interval − RETIRE_UNIQUE_COUNT_MARGIN`, = 114 == 57 fps
  at a 60 fps target). A capture-TAKT gate is WRONG: a takt>60.3 excess-dupe deficit (unique < 60)
  would be wrongly retired, dropping the emit rate + blinding the duplication-masked pulldown detector
  (`dup_cadence.rs`) + tripping the #666 emit-deficit gate. A genuinely starved source below the floor
  (a 50->60 pulldown) stays on the #1111 copy path byte-identical.
- **A 2 s windowed count CANNOT separate a 60-unique-jittery source from a ~58-unique one** (a 3.5 %
  rate difference is within the window's edge/jitter noise: the rig at takt 61.3 j30 dips to
  count ~115, a 57.9-unique 62/period-15 grabber reaches ~117 — they OVERLAP). So the floor is set to
  prioritise the RIG (retire the true-60 case even at heavy jitter) and DELIBERATELY aligned with the
  #666 emit-deficit floor (57 fps): a source ABOVE 57 unique fps emits its honest rate (retired, no
  copies), below it gets copies to hold 60. Consequence: a genuinely-57.9-unique excess-dupe grabber
  now emits the honest ~57.9 (retired) instead of 60-via-copies — the strih FIFO absorbs the gentle
  evenly-spread underrun identically (no lag leap to relock), and `dup_cadence` (pulldown floor 10 %)
  already expects a 6.7 % over-rate dupe rate to be shed. The old #1111 "hold 60 via copies" test was
  updated to this intentional v2 behavior; the load-bearing 0-skips + no-unique-dropped are unchanged.
- **GOTCHA (review-found 🔴, do NOT ship a rate-gated shed without it): a FROZEN source is a distinct
  failure shape.** 100 % content-dupes (a dead painter / wedged upstream — the #1052/#365 class)
  means no unique ever refreshes the window, so its COUNT stays stale-high and retirement fires
  FOREVER, collapsing the NDI emit to ~0 fps (a total BLACKOUT — strictly worse than a frozen picture
  on a broadcast rig). Two guards, both live: (1) prune the window every poll so a dead source's count
  DRAINS to 0 over the window; (2) a FRESHNESS gate (`RETIRE_UNIQUE_FRESH_BOUND_INTERVALS=5` — retire
  only when the most recent unique arrived within ~5 emit intervals) that kills a freeze in ~83 ms and
  ALSO catches a burst-then-freeze the count-drain alone would miss. A freeze then falls back to the
  #1111 copy valve (a frozen PICTURE on a LIVE, FIFO-fed stream — the pre-#1145 behavior).
- Decision is the pure `dupe_decimation::dupe_shed_action(...) -> ShedAction {Emit{copy}, Defer,
  Retire, BlindShed}` (replaced `dupe_preferring_decimate`). `DupeShedLog` gained a `retired` counter
  (the summary line is now 4-count; `main.rs` wires the 4-tuple). Live: retired ≈ over-rate delta,
  copies ≈ 0 on cam1/cam2, all-zero on cam3. Also guard a BACKWARD DanteSync clock step (clear the
  window if its newest entry is in the future — mirrors `genlock_emit_gate`'s #131 re-latch).

## #1145 v2 — queue-DEPTH drain: bound the delivery-latency SAWTOOTH (the SIXTH piece)

The v1 retirement above did NOT fix the owner-visible judder, because it keys on `genlock_lag_intervals`
(BOUNDARY staleness) — and **boundary lag is NOT the queue depth.** When the emit loop is send-bound
(~60 fps: NDI encode+send ≈ one interval) and the card captures 61.x, the loop processes the OLDEST
buffered V4L2 frame each poll and `now` (realtime) lands right on the advancing boundary → `lag` reads
~0 the whole time, so retirement (lag>=1) never fires and its lag-4 ceiling is irrelevant. Meanwhile
the capture→emit QUEUE RESIDENCE grows (over-rate refeeds it faster than the loop drains) and the
4-deep V4L2 buffer periodically overflow-drops in a BURST = the measured delivery-latency sawtooth
(67→167 ms, issue 1110/1130) = the #1142 uniformity RED. v1 is structurally blind to it.

v2 measures the residence DIRECTLY and sheds to bound it:
- **Signal = monotonic queue residence**, `queue_depth_intervals(now_mono, capture_mono, interval)` =
  `(monotonic_clock_ns() − FrameInfo::capture_monotonic_100ns*100) / interval`. It is a DURATION, so
  it uses CLOCK_MONOTONIC (immune to the DanteSync realtime steps the emit BOUNDARY is gridded to — the
  boundary still uses `wall_clock_ns()`, so `poll` now takes BOTH clocks). Guards: capture 0 (the
  FrameInfo no-measurement sentinel) / interval 0 / now≤capture → 0; clamped to
  `QUEUE_DEPTH_SANE_MAX_INTERVALS`=8 so a bogus stamp can't force a runaway shed.
- **Action = `ShedAction::Drain`** (the 5th variant; `DupeShedLog` now a 5-tuple with `drained`,
  summary is 5-count): shed the OLDEST (this) frame + advance the boundary ONE interval, emit nothing —
  a single-slot drop (never a multi-slot skip, so #1131 is preserved). Over-rate + residence ≥
  `QUEUE_DEPTH_SHED_INTERVALS`(2) sheds regardless of dupeness (a controlled drop that pre-empts the
  uncontrolled overflow burst); a DETECTED dupe at residence ≥ `QUEUE_DEPTH_DUPE_SHED_INTERVALS`(1)
  drains one interval earlier (always content-safe).
- **Gated on a capture-TAKT EMA, NOT the unique-rate window.** `note_capture_takt` folds consecutive
  `capture_monotonic` intervals into an integer EMA (`TAKT_EMA_SHIFT`=8, init-seeded);
  `sustained_over_rate()` = EMA interval < `RETIRE_MIN_TAKT_INTERVAL_NS` (1e9/60.3 ≈ 16.584 ms). This
  is the essential discriminator: a 60.00 card reads ~16.667 ms (ABOVE → NOT over-rate → the drain is
  OFF), so it is byte-identical to v1 EVEN through a transient stall (a #1131 buffered-drain, which
  must emit all buffered frames, not shed them) — that is constraint c. Only a >60.3-fps card engages
  the drain. (v1's `enough_unique` window does NOT discriminate here: a stall-recovery on a 60fps card
  also reads unique-rate ~60. The takt is what separates over-rate from stall-recovery.)
- **Gap note:** shedding a non-dupe at over-rate is content-safe because the surplus capture is a
  re-sample carrying the SAME painted frame_id as a neighbour; it only risks a painted-id gap if
  dupe-detection missed a genuine unique, which is far rarer + less visible than the sawtooth it
  replaces (and strictly better than the indiscriminate V4L2 overflow-drop). Report-only fields feed
  the live re-measure; the >=0.95 uniformity acceptance is verified on-rig after deploy, not off-rig.

## #1145 v2.1 — FAST-drain: accelerate a DEEP grid backlog's convergence (the SEVENTH piece)

The v2 depth-drain bounds the STEADY over-rate sawtooth, but it does NOT converge a DEEP backlog
fast. After a refilling event (service restart, receiver reconnect, burn toggles) the emit grid can
fall 12+ intervals behind wall-clock (the delivery latency the owner's painter-QR measures). v2
retires an over-rate dupe only while `lag <= RETIRE_MAX_LAG_INTERVALS` (4); ABOVE that ceiling it
EMITS the late dupe as a #1111 COPY — which does NOT advance the grid — so a deep backlog catches up
ONLY at the tiny send-slack rate (~0.3 frame/s; the owner's live-measured ~35 s for 12 frames). The
grounding sim (the SCRATCH route below, driving the REAL `DecimationGate::poll` with a
realtime/monotonic clock split so a reconnect adds a realtime grid-lag WITHOUT disrupting the
monotonic takt) reproduces exactly this: v2 ~31–54 s at a realistic ~0.3–0.5% send-slack.

v2.1 adds ONE band to the pure `dupe_shed_action` decision:
- **When `sustained_over_rate && enough_unique_to_hold_target && lag > RETIRE_MAX_LAG_INTERVALS`**
  (== 2x the `QUEUE_DEPTH_SHED_INTERVALS` depth target — "residence/backlog exceeds 2x target"),
  return the new **`ShedAction::FastDrain`**: shed the dupe AND advance the boundary by **TWO**
  intervals ("drain up to 2 slots per emit interval"). The extra boundary is ALSO already stale
  (lag > 4 >> 2), so it costs no new downstream gap and is guarded in `poll` to never advance the
  grid into the future (`candidate_next + interval <= now`, else a single-slot fallback). This
  converts the copies v2 emitted (no grid advance) into boundary-advancing retirements at 2x the
  dupe rate → the deep backlog converges in single-digit seconds.
- **DUPES-ONLY, so issue-1131 "never drop a unique while uniques exist" holds** — only a content-dupe
  takes this path; uniques still emit, and the +2 retires a stale boundary rather than dropping an
  extra frame, so the emit rate stays >= the #666 floor (57 fps).
- **Takt-gated → byte-identical below the band.** A healthy 60.00 card is NOT `sustained_over_rate`
  (the whole band is skipped), and steady over-rate WITHOUT a backlog keeps `lag ~0` (< the 2x-target
  band) — both are byte-identical to v2. `DupeShedLog` gained a `fast_drained` counter (now a 6-tuple
  `take()` + a 7-arg `dupe_shed_summary`; `main.rs` wires it) so the live box shows the fast-drain
  engaging distinctly from the v1 retire / v2 depth-drain.
- **Sim (REAL poll, realtime/monotonic split):** 12-frame grid backlog v2 7.3 s → v2.1 5.3 s;
  18 → 11.3/7.3; 24 → 15.3/9.3 (single-digit); emit >= 59.98 fps; uniformity >= 0.997; 60.00 card
  `fast_drained == 0`. On the rig, where v2's grid drain is slack-limited (~0.3 frame/s), the same
  mechanism converts the drain to the dupe-retirement rate — the >=0.95 uniformity + convergence
  acceptance is the live E2E re-measure after deploy (supervisor's step), as with v2.

## GOTCHA — verify pacing changes against the REAL modules, never a hand-simplified re-model (#1145)

The rule below ("faithful Python port") is right that a port reproduces the live behavior — but a
hand-SIMPLIFIED re-model silently DIVERGES. A shortened `DecimationGate`/`Cur` re-write disagreed
with the real #1111 test (it read emit 58 at 62/period-15 where the real code holds ~60 via copies),
which would have mis-designed the fix. The authoritative off-rig check is the CLAUDE.md/#557 SCRATCH
route: copy the ACTUAL `src/genlock_pacing.rs` + `src/dupe_decimation.rs` into a scratch dir with a
`root.rs` that `mod`s both, `sed 's/crate::genlock_pacing:://g'` inside dupe_decimation, then
`rustc --edition 2021 --test root.rs` runs the REAL `DecimationGate::poll` + the real test suite. For
a design sweep, drive that real gate with a synthetic capture stream (periodic isolated dupes at the
over-rate delta + INDEPENDENT timestamp jitter — content-dupeness is a hash property, NOT a
sampling-phase artifact; a source-sampling model that ties dupeness to the jittered timestamp
mis-models the ShadowCast, which stays clean at exactly 60). The downstream uniformity a genlocked
strih sees is the emitted source-tick sequence decimated in-order by 2 (NOT resampled by the jittery
emit timestamps — the emit grid is wall-clock-gridded, the FIFO genlocked).

**Modeling a DEEP grid-backlog convergence (a reconnect/restart/burn-toggle) — the takt-preservation
trap (#1145 v2.1).** The signal that reaches 12 frames is the emit-GRID boundary lag (delivery
latency == the painter-QR lag), NOT the local V4L2 residence (capped at the 4-deep buffer). To inject
that lag in the sim you must fall the emit grid behind WHILE leaving the cam-box's monotonic capture
TAKT continuous — because `sustained_over_rate` is gated on the takt EMA, and if you inject the lag by
jumping the wall clock and resetting the capture index, you punch a multi-second GAP into the
processed-capture-timestamp stream that spikes the takt EMA above the 60.3 threshold and SILENTLY
holds `sustained_over_rate == false` for ~18 s — so the fast-drain never arms and the sim shows ZERO
change (the exact dead-end that cost ~5 iterations here). The faithful model uses a REALTIME/MONOTONIC
CLOCK SPLIT: `poll(now_ns = realtime, …, now_mono, capture_mono)` where a reconnect adds a one-time
REALTIME forward offset (the grid falls behind == delivery lag) while `now_mono`/`capture_mono` stay
continuous (the cam-box kept capturing) — so the takt stays over-rate, residence stays low, only the
grid lag is deep. Calibrated to a realistic ~0.3–0.5% send-slack this reproduces v2's owner-measured
~0.3 frame/s (~35 s for 12 frames) and shows the fast-drain converging in single-digit seconds. Also
model dupes as isolated content-PAIRS (a dupe REPEATS the previous content id) or the gate's byte-hash
`is_dupe` never fires and no dupe path is exercised at all.

## GOTCHA — the #707 resync is QUEUE-BLIND; gate it on the dequeue signal, not just a lag bound (#1131)

`genlock_emit_gate`'s forward-resync (`lag > GENLOCK_MAX_CATCHUP_INTERVALS`) is BLIND to whether the
skipped boundaries actually had captured frames. On a sick/wobbly grabber a single poll's wall-clock
lag can exceed 8 intervals while the V4L2 driver has REAL captured frames buffered (the live
signature: `#707 SKIPPED ... 9 boundary interval(s)` with **0 capture-dropped** = the frames exist,
just delivered late) — the resync leaps past them and they are decimated (discarded in a run) = the
visible multi-frame content judder (issue 1110/1130 P0).

The fix (#1131): thread a per-frame `queue_had_frame` bool — `capture_stall::frame_from_nonempty_queue(dequeue_duration_ms, capture_interval_ms)`,
true when the blocking VIDIOC_DQBUF returned in `< 0.5×` the capture interval (the driver already had
it buffered) — from `main.rs` → `DecimationGate::poll` → `genlock_emit_gate`, and change the resync
trigger to `lag > GENLOCK_MAX_CATCHUP_INTERVALS && !queue_had_frame`. A buffered frame catches up ONE
interval (fills its boundary, never leaps); an EMPTY-queue frame (the loop genuinely WAITED for it — a
device stall that produced nothing, or a real clock STEP) keeps the resync (honest skip).

Why the dequeue signal is the RIGHT discriminator (and why raising the bound alone is wrong):
- A long **DQBUF** block ⟺ the device produced nothing (that's WHY dequeue blocked) → those
  boundaries genuinely had no content → resync is honest. `dequeue_duration_ms` is large → `false`.
- An emit-loop-side block (send/processing) leaves the device producing → frames buffer → on resume
  DQBUF returns fast → `dequeue_duration_ms` small → `true` → catch up (the frames exist).
- A cold-boot NTP step (#131) inflates `now_ns` (CLOCK_REALTIME) but NOT the dequeue duration (a
  monotonic `Instant::elapsed()`), so the post-step frame reads as a normal single-frame wait →
  `false` → still resyncs. **This is why #131 is preserved for free** — do NOT just raise the fixed
  bound (that would make a genuine multi-second step creep through hundreds of stale-frame emits).

Three-band read of the ONE `dequeue_duration_ms` signal: `(0, 0.5×)` buffered / `[0.5×, 1.5×)` normal
single-frame wait / `≥ 1.5×` stall (`CAPTURE_STALL_FACTOR`). `frame_from_nonempty_queue` and
`is_capture_stall` are the two ends. Fail-safe: an unknown/non-finite measurement → `false`
(queue-blind = today's behavior), so a bad reading can never SUPPRESS an honest skip — and guard
`frame_interval_ms` for finiteness too (a `+inf` interval would otherwise falsely read "buffered",
the unsafe direction). All Tier-0 (`cfg(target_os="linux")`, not probe-gated): verify via
`rustc --edition 2021 --test src/genlock_pacing.rs` (and `src/capture_stall.rs`) standalone — a
combined build with `genlock_pacing`/`capture_stall` as submodules runs the real `DecimationGate::poll`.

## Root-causing method that worked: faithful Python port + live journal cross-check

Port `genlock_emit_gate` + `DecimationGate` + `boundary_skip_count` verbatim to a Python sim and
drive it with the real pattern (62 fps, isolated dupe every ~15 captures). It reproduced the
EXACT live `9 boundary interval(s)` skip and the 18-skips/8 s rate — pinning the deterministic
root cause before touching code, and giving exact RED/GREEN test thresholds. Read the live
proof read-only over ssh: `journalctl -u camera-box | grep -E 'Streaming:|#707|dupe-preferring'`.
