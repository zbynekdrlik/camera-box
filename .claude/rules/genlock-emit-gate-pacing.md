---
paths:
  - "src/dupe_decimation/**"
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

## #1145 round 3 — noise-tolerant content-compare detection (the EIGHTH piece)

v1/v2/v2.1 all keyed the WHOLE dupe machinery on `is_dupe = prev_hash == content_hash` — EXACT FNV
equality. That works for CAM1's steady ~64 fps card, whose surplus is a byte-identical BUFFER-REPEAT.
It does NOT work for CAM2, the marginal jittery ~61 fps PAINTER box: the rig path has a full optical
hop (painter monitor → camera → HDMI splitter → ShadowCast → USB), so CAM2's surplus is a noisy
optical RE-SAMPLE of the same painted frame (sensor noise), NOT byte-identical. The exact hash reads
`is_dupe=false`, the dupe falls out at `if !is_dupe { Emit{copy:false} }` (emitted as a "unique" = a
held painted-id downstream, Δ1) and the un-absorbed over-rate forces a compensating shed (a skipped
painted-id, Δ3) — the balanced Δ1/Δ3 aliasing churn the #1142 uniformity gate honestly REDs (live
CAM2 0.93–0.95; issue-1130 comment 5364318219 attributes it, painter-box CPU contention #899 as the
jitter amplifier). Off-rig sim (real poll, noisy re-samples) reproduces it to within ~0.005 of the
live per-segment numbers.

The fix DETECTS the noisy re-sample so the shed prefers a PROVEN dupe (retiring/deferring a proven
dupe never starves uniqueness). Design + false-positive-safety pressure-tested via a gated Fable
consult; the ASYMMETRY is the whole design: a false-NEGATIVE (miss a dupe) reverts to the pre-round-3
heuristic (status quo); a false-POSITIVE (call a genuine UNIQUE a dupe) DROPS a real frame = a genuine
gap = strictly worse. So it is biased hard to false-negative and caged structurally, NOT by a
blind-tuned threshold.

- **Signature = `dupe_content_sig(frame,w,h,stride) -> (u64, Vec<u8>)`** — ONE pass yields the exact
  FNV hash (BYTE-identical to the legacy `dupe_content_hash`, which now delegates — its 4 tests intact,
  so a byte-identical dupe still short-circuits) PLUS a luma (Y) lattice: the Y byte (even YUYV offset)
  every `DUPE_SIG_PIXEL_STRIDE`(8) pixels across the SAME `DUPE_HASH_SAMPLE_ROWS`(8) rows the hash
  reads (stride-honoring).
- **Comparator = `frames_are_content_dupes(prev,now) -> bool`** — a two-threshold SPARSE-DIFF, NOT a
  block-mean (mean-preserving on a QR flip → false-POSITIVE), NOT a dHash (threshold-inversion: a small
  QR's flip-Hamming can be BELOW the flat-region noise-Hamming), NOT a SAD-sum (no physical margin).
  `changed = |Y_now[i] − Y_prev[i] − median_offset| ≥ NOISY_DUPE_DIFF_THETA(48)`; dupe iff
  `changed_count ≤ NOISY_DUPE_MAX_CHANGED(6)`. The MEDIAN of the per-point diffs is a calibration-free
  global exposure / display-PWM-backlight-beat compensation (robust to the QR outliers — they are a
  minority, and a bidirectional flip keeps the median ~0). Physical margins on EACH threshold: per-point
  sensor noise σ≈2–8 vs a module-flip swing ≈100–180 luma (θ=48 is ≥5σ above noise, ≤½ the swing);
  per-count a real flip moves tens of sampled points vs K=6. Empty/mismatched lattices → NOT a dupe
  (fail-safe). The 3 constants are calibration; the live E2E re-measure validates them.
- **The structural CAGE (makes the constants efficacy-only, not safety-critical):** (1) the noisy
  comparator is ARMED only under `sustained_over_rate` (the takt EMA the gate already owns) — a healthy
  60.00 card NEVER consults it, so it is provably byte-identical; (2) NEVER classify two CONSECUTIVE
  frames as noisy-dupes (`prev_was_noisy_dupe`; exact dupes exempt) — a ~61 fps surplus is ~1 isolated
  dupe/s, so a run would be a slow content FADE the sparse-diff cannot tell from a still; the cap
  hard-bounds even a mis-tuned comparator to every-other-frame and kills the fade-chaining class; (3)
  the existing nets stay (`enough_unique`, #1131 no-unique-drop, #666 floor 57). A detected noisy dupe
  is ALSO excluded from the unique-rate window (it carries no distinct content), correcting `enough_unique`.
- **Wiring keeps `poll` 6-arg** (all pre-round-3 tests byte-untouched): `note_frame_luma(&luma)` is a
  paired pre-call (main.rs computes `dupe_content_sig` once, stages the lattice, then polls);
  `is_dupe = exact_dupe || (sustained_over_rate && !prev_was_noisy_dupe && frames_are_content_dupes(...))`,
  exact FIRST (never demote a byte-identical dupe). A poll with no staged lattice clears prev + returns
  not-dupe — the self-neutralizing fail-safe that keeps every legacy caller unchanged.
- **Sim (REAL poll, root.rs `#[path]`):** the marginal noisy card at 61.0/61.3 → 1.0000 and 61.5 →
  0.9766 (all ≥0.95, Δ1/Δ3 collapsed) vs the exact-hash baseline 0.9415/0.9245/0.9130; CAM1-64
  byte-identical WITH == WITHOUT `note_frame_luma` (identical decisions); healthy 60.00 inert. Model
  noisy dupes as a SIGNATURE property (same painted id re-rendered with a FRESH noise draw), NOT a
  jittered-timestamp artifact — and give the synthetic "QR" a per-module AVALANCHE bit (splitmix of
  id,y,x), NEVER a popcount-parity model (parity is position-independent → ALL modules flip together or
  NONE → a degenerate all-or-nothing "QR" that false-passes or false-fails the comparator test). The
  ≥0.95 uniformity + clean QR-contiguity acceptance is the live E2E re-measure after deploy
  (supervisor's step). **UNVERIFIED (supervisor's live step):** the interaction with the frozen-leg
  attribution paths (a frozen painter now reads as all-dupes → the copy valve holds the grid, arguably
  more correct) is not verified off-rig.

## #1145 v3 — arming-signal robustness through a capture HICCUP (the NINTH piece)

The rounds v1/v2/v2.1/round-3 all bound content-age in STEADY state, but a single capture HICCUP (a
blocked V4L2 dequeue — a CPU/#752/USB stall of `>~99 ms`) disarms every cam-side over-rate drain for
SECONDS, so the surplus then leaks into the strih genlock FIFO — the ±5..±11-frame cam1 wobble the
qr-align `[4i/8align]` gate REDs (the age physically RESIDES in the strih FIFO: skew 126–210 ms vs
cam3's 76 ms = 3–8 frames; the FIFO's `converge_sheds +4-5/tick` is it honestly draining a real
backlog while the cam box re-excites it after each hiccup). Diagnosed as an ARMING-SIGNAL POISONING
CASCADE (NOT a steady sawtooth — in keep-up dupes absorb at `Defer`, age 0):

1. **Takt-EMA poisoning** — the 61.5 fps EMA sits ~0.32 ms below `RETIRE_MIN_TAKT_INTERVAL_NS`, so ONE
   `>~99 ms` gap sample folded into the ~256-frame EMA flips `sustained_over_rate` off, and the
   τ≈256-frame recovery holds it off ~7 s (500 ms gap) / ~12 s (1.5 s). While off, depth-Drain,
   FastDrain AND the round-3 noisy-dupe compare are ALL dead.
2. **`enough_unique` count depression** — a gap `>~100 ms` drops the ABSOLUTE unique COUNT below
   `retire_min_uniques` for ~the gap duration → dupes hit the #1111 COPY valve instead of Retire.
3. **Dead band** — a 6–8-interval hiccup (`<= GENLOCK_MAX_CATCHUP_INTERVALS`=8 → no #131 resync)
   leaves persistent grid lag while (1)+(2) killed every drain → copies emit at wire rate → the V4L2
   queue rides full → the `>60/s` surplus exports into the strih FIFO at ~+0.5/s.

The fix RETUNES the two arming signals (error-driven family kept; NO new ShedAction, NO micro-shed —
an open-loop steady-cadence credit shedder was REJECTED: in keep-up the excess IS the dupes and
`Defer` already absorbs them at zero age cost, so a credit shedder double-counts that and regresses
the emit rate 59.97→59.20, off-rig-proven):

- **B.1 gap-excluded takt fold** (`note_capture_takt` + `TAKT_GAP_EXCLUDE_NS` = 3× the 60 fps emit
  interval = 50 ms): a genuine takt change shows in EVERY sample; a hiccup in ONE — so SKIP folding an
  inter-capture interval above the bound (still advance `prev_capture_mono_ns` so the next interval is
  measured cleanly). Keeps `sustained_over_rate` armed through the hiccup.
- **B.2 occupancy-relative unique floor** (`enough_unique_to_hold_target` + a new `all_capture_times`
  window + `RETIRE_OCCUPANCY_MIN_PERCENT`=95, `RETIRE_OCCUPANCY_MIN_SAMPLES`=30): OR-in
  `unique/all >= 95%` alongside the existing absolute count + freshness gates. A gap admits NO
  captures so it depresses BOTH counts equally → the RATIO is gap-immune. **#666-SAFE by gating on
  `sustained_over_rate`** (capture rate `> 60.3`): `unique >= 0.95 × total` with total-rate `> 60.3`
  ⇒ retired emit (= unique rate) `>= 0.95 × 60 = 57` (the #666 floor); an under-rate/starved source
  (NOT over-rate) never reaches this arm, so retiring can never drop it below 57. Freshness stays
  FIRST (a frozen source still falls to the copy valve, never a blackout); a 50→60 pulldown (~0.83
  ratio) stays on the copy valve.

`all_capture_times` is pushed every poll, pruned in lock-step with `unique_capture_times`, and cleared
on the same #131 backward-clock-step. `poll` stays 6-arg; NO `DupeShedLog`/summary/counter change.
Verified 64/64 via the scratch route below (the 2 [red] arming tests GREEN, every preservation test
unchanged + a steady-no-hiccup anti-over-shed pin). **Supervisor's live rig step:** confirm the strih
`genlock-fifo audit 'NDI cam1'` `converge_sheds` go quiescent (≈0 between events, depth pinned 1–2)
and `[4i/8align]` holds a stable cross-camera offset.

- **CAVEAT — 64 fps card + the #666 floor (deferred follow-up):** the consult flagged that the
  EXISTING `+2` FastDrain (and any future "generalise +2 to lag>=2") has a latent #666 hazard at a
  genuinely 64 fps card — unfilled-boundary rate = 2× (fps−60), so `2×4 = 8 > 3` would emit ~52 fps
  during episodes (the v2.1 sim only pinned 61.5). This v3 change does NOT touch FastDrain and does
  NOT add the +2/lag>=2 generalisation (deliberately out of scope). If the rig re-measure still shows
  mid-band dwell, that generalisation needs a retire-rate token bucket (≤3 unfilled boundaries/s) —
  file it as its own ticket; do not add it without the 64 fps guard.

## #1167 — corrupted-slot MAKE-UP: fill the slot a pre-gate corruption drop vacated (the TENTH piece)

A corrupted V4L2 buffer (`V4L2_BUF_FLAG_ERROR` / short) is dropped in
`src/capture.rs::process_frame` with `return Ok(())` **BEFORE the callback**, so it NEVER reaches
`DecimationGate::poll`. At an OVER-RATE that removes a would-be-emitted GOOD frame from the stream,
and the over-rate absorption (`ShedAction::Retire` / `Drain` — advance the boundary, emit nothing)
then SKIPS that 60 fps slot instead of filling it → emit under-runs by EXACTLY the corrupted rate
→ a strih genlock-FIFO hold → the cam1 presented-frame_id align sawtooth → `[4i/8align]` abort.
An AT-rate box (`sustained_over_rate` false → Retire/Drain OFF) instead fills the same gap with the
#1111 copy valve, so it holds 60 with identical corruption — that is why ONLY the over-rate box
breaks. Diagnosed off-rig (real `poll`, scratch route): a true-60 source over-captured at 62 fps
under 0.8 corrupted/s emits 59.13 fps (== the live "~59.1"; the emit deficit == the corrupted count
EXACTLY — each corrupted frame costs one slot), vs 59.93 with the fix.

The fix — a BOUNDED make-up deficit (NO new `ShedAction`, NO summary/tuple change):
- **`DecimationGate::note_corrupted_frame()`** (called from `main.rs` when `capture.corrupted_frames()`
  increases across a `process_frame` call, guarded by `out_interval_ns > 0`) accrues
  `corrupted_makeup_deficit` (capped at `CORRUPTED_MAKEUP_MAX_DEFICIT = 8` — beyond a burst this
  size the source is genuinely starved and the #1111 copy valve / `enough_unique` handoff carries
  it) AND sets `pending_takt_gap`.
- **Pure `corrupted_makeup_reclaims(action, deficit)`** = `deficit > 0 && matches!(action, Retire | Drain)`
  (Tier-0 testable). In `poll`, after the unchanged `dupe_shed_action`, if it fires: decrement the
  deficit, `next_boundary_ns = candidate_next` (the SAME single-interval advance Retire/Drain/Emit
  do), clear `deferred_this_boundary`, `record_dupe_emitted()`, `return true` — emit the CURRENT
  GOOD frame (a dupe/repeat in the Retire case, a fresh good frame in the Drain case). Corrupted
  CONTENT is never forwarded (it was already dropped in capture.rs); the make-up emits a SUBSEQUENT
  good frame.
- **`note_capture_takt` gap-excludes the corrupted-spanning interval** (`pending_takt_gap`): that
  inter-capture interval spans the missing sample (~32 ms, below the 50 ms `TAKT_GAP_EXCLUDE_NS`, so
  it would otherwise be folded and creep the EMA toward disarm) — SKIP the fold, advance the
  baseline, and (review 🟡) leave `consecutive_takt_gaps` UNTOUCHED (a corrupted interval carries no
  takt evidence, so resetting it would erase #1145 v3 F1 collapse evidence — a dying card with a
  corrupted storm could latch `sustained_over_rate` on a collapsed source). Keeps the arming armed.

Composition: SINGLE-slot (issue-1131 preserved; `FastDrain` deliberately NOT converted — it is the
deep-backlog convergence, and a make-up there would fight the drain; the deficit is reclaimed once
steady Retire/Drain resume); #1111 valve untouched; #1145 v3 arming stays armed; INERT with no
corruption. The make-up copies land in the reused #1111 `dupe_emitted` counter — attribute them via
the `corrupted` count on the same `Streaming:` line (a healthy over-rate box shows ~0).

- **Doc-placement GOTCHA (review 🟡):** the pure helper must sit BELOW `dupe_shed_action`, not
  between that fn's rustdoc block and the fn — rustdoc attaches a contiguous `///` run to the
  FOLLOWING item, so inserting a helper there silently steals `dupe_shed_action`'s doc.
- **Stale-deficit carry (accepted by design):** a deficit accrued while the make-up can't fire
  (at-rate periods / FastDrain backlogs) persists and can later convert up to 8 legitimate over-rate
  sheds into copies; bounded to 8/episode and self-terminating. If live data ever shows copy bursts
  trailing a corruption episode, add an aging/clear-on-valve rule.
- **Tier-0 verify:** the whole cluster is `cfg(target_os="linux")` pure logic → `cargo fmt --all
  --check` + the rustc `--test` scratch replica (RED with the make-up disabled 59.13 → GREEN 59.93).
  The Drain leg needs `now_mono` 2+ emit-intervals AHEAD of a monotonic `capture_mono` to reach
  `queue_depth >= QUEUE_DEPTH_SHED_INTERVALS` (a constant residence-2 drains EVERY frame — model a
  warmed gate + one drain frame, per `drain_leg_make_up_emits_instead_of_dropping_1167`).

## GOTCHA — verify pacing changes against the REAL modules, never a hand-simplified re-model (#1145)

The rule below ("faithful Python port") is right that a port reproduces the live behavior — but a
hand-SIMPLIFIED re-model silently DIVERGES. A shortened `DecimationGate`/`Cur` re-write disagreed
with the real #1111 test (it read emit 58 at 62/period-15 where the real code holds ~60 via copies),
which would have mis-designed the fix. The authoritative off-rig check is the CLAUDE.md/#557 SCRATCH
route: copy the ACTUAL `src/genlock_pacing.rs` + the whole `src/dupe_decimation/` DIRECTORY (`mod.rs`
+ `signature.rs` + `shed.rs` + `gate.rs` + `tests.rs` — the module was split into a dir for the
~1000-line budget) into a scratch dir with a `root.rs` that `mod genlock_pacing; mod dupe_decimation;`
(rustc resolves `dupe_decimation/mod.rs`), then `rustc --edition 2021 --test root.rs` runs the REAL
`DecimationGate::poll` + the real test suite (70 tests: 44 `dupe_decimation::tests::` + 26
`genlock_pacing::tests::`). NO `sed` is needed — `crate::genlock_pacing::*` resolves because
`genlock_pacing` is a top-level `mod` in the scratch crate (the old single-file recipe's
`sed 's/crate::genlock_pacing:://g'` was never actually required and is retired). For
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
