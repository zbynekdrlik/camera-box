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

## #1167 (2026-08-22) — the make-up was a MIS-DIAGNOSIS; the real fix is FILL-EVERY-SLOT (the ELEVENTH piece)

The make-up above (the TENTH piece) **does not fire live, and cannot fix the persistent deficit** —
hard-debug re-diagnosis proved BOTH of these off-rig against the real `poll`:

- **The `corrupted` count on the `Streaming:` line is CUMULATIVE, not per-window.** `main.rs` resets
  `frame_count`/`emit_count`/`last_report` every 5 s but NEVER `capture.corrupted_frames()` (its own
  code comment says "a cumulative count"). "4 corrupted" identical across consecutive windows = 0 NEW
  corrupted = a frozen STARTUP artifact, misread as "4/5s". So `note_corrupted_frame` fires ~0 times
  steady-state → the deficit stays 0 → the reclaim never fires (airtight: the deficit has no reset, so
  a nonzero deficit would eventually surface a `record_dupe_emitted` copy — but `late-dupe copies` is
  0 in EVERY window). The make-up is INERT, working exactly as designed with nothing to make up. The
  prior off-rig replica INJECTED `note_corrupted_frame` at 0.8/s — modelling ongoing corruption that
  does not exist live (the replica-vs-live divergence). **The make-up code is kept but DORMANT.**
- **The persistent 58.5 fps (over-rate) vs cam3 60.0 (at-rate, identical hardware) is the over-rate
  absorption SKIPPING emit slots**, NOT corruption. `ShedAction::Drain` (residence≥2) and the
  shallow-lag `Retire` (lag 1–4) ADVANCE a boundary while emitting NOTHING — a skipped 60fps slot the
  buffered surplus could have filled. Continuous send-jitter on the busier over-rate box re-injects
  shallow lag every window (live cam1: `retired`>0, `drained`>0, `fast_drained`==0), so the skip is a
  CONTINUOUS strih-FIFO hold = the cam1 align sawtooth. The single diagnostic identity, from EXISTING
  journalctl: `sent + retired + drained + 2×fast_drained ≈ 300/5s` — ~300 ⇒ decimation-limited (the
  fill reaches 60); short ⇒ a send-throughput residual survives (CPU contention / the #899 lane), a
  separate ticket. Post-deploy it degenerates to `60 − emit_fps` = the send-limited residual.

The fix — FILL every slot while a good frame is buffered (the owner's invariant: "a single-slot dupe
is acceptable; a skipped slot is never"), NO new `ShedAction`, the pure `dupe_shed_action` DECISION
byte-UNCHANGED (so the whole #1145 decision-test surface + deep-backlog convergence are preserved);
only `poll`'s APPLICATION of two arms changes:
- **`ShedAction::Drain` HOLDS the boundary** (drop the oldest to bound residence exactly as before,
  but do NOT advance) → the next fresher frame fills the same slot. Residence stays bounded at
  `QUEUE_DEPTH_SHED_INTERVALS`; the #1145 v2 sawtooth fix + V4L2-overflow pre-emption hold. A new
  `consecutive_drain_holds` field + a PANIC FLOOR (after `QUEUE_DEPTH_SANE_MAX_INTERVALS` consecutive
  holds on one boundary — a bogus stuck-high residence — fill with a copy) makes it fail-SAFE not
  fail-black. Reset to 0 whenever the boundary advances (any emit / FastDrain).
- **The shallow-lag `Retire` is REINTERPRETED by a `converging_deep_backlog` LATCH** (set true in the
  FastDrain arm, cleared at `lag_intervals == 0`): while CONVERGING a deep backlog → RETIRE (advance,
  emit nothing — the #1145 v2.1 fast-convergence rate is untouched: a copy there advances `now` by the
  send cost and slows convergence); in STEADY over-rate → FILL the slot with a copy of the nearest
  good frame (holds 60). A deep reconnect backlog trips FastDrain → latches → retires the shallow tail
  → converges fast; continuous shallow-lag jitter never trips FastDrain (`fast_drained==0`) → never
  latches → fills every slot. `FastDrain` (deep band) is byte-unchanged except it sets the latch.
- **#1142-safe:** a fill during a lag episode occupies a slot NO unique could fill (a missed
  boundary), so no unique is displaced — an UNPAIRED downstream Δ0, not the paired copy+dropped-unique
  Δ0/Δ3 churn the pre-#1145 valve caused at lag 0. `retired` now goes ~0 and `dupe_emitted` becomes a
  LEGITIMATE nonzero on the over-rate box (attribute via the retired count going to 0). The absolute
  `WINDOW_COPIES_GAPS_TOLERANCE` is the live-E2E re-check after deploy (data-first).
- **Off-rig (real `poll`, #557 scratch):** the sanctioned `run_queue_sim` over-rate 59.7 → ~60.0
  (residence still ≤ target, 0 overflow); a 24-frame deep backlog STILL converges in 8.65 s (≤12 s);
  the at-rate 60.0 control unchanged; 73/73 module tests pass. Design chosen via a gated Fable consult
  (hybrid: hold-Drain + latch-gated shallow-fill + kept FastDrain + panic floor).

## #1167 v3 (2026-08-22) — PACE the convergence so it AMORTIZES, never bursts (the TWELFTH piece)

The eleventh piece (fill-every-slot) raised cam1's AVERAGE emit to 59.94 (299.7/300) but windows
still oscillated **300/300/293**: the degrading grabber's ~3.5 fps surplus CREEPS grid lag upward
(steady shallow-lag dupes FILL and never drain the lag; Drain-HOLDs advance the wall clock but not
the boundary) until it crosses `RETIRE_MAX_LAG_INTERVALS`; then FastDrain fires, LATCHES, and the
whole shallow tail drains as a **BURST** of ~6-7 advance-emit-nothing sheds in a fraction of a second
→ one 5s window drops to 293 → cam1's presented-frame_id jumps ~+7 vs its siblings → `[4i/8align]`
"mutual stability ≤1 id" ABORT. Reproduced off-rig against the REAL `poll` (send-bound 63.5 fps creep
model, #557 scratch route): current code shows max consecutive-emit boundary **delta = 3** (a +2
FastDrain jump), skips bunching (min gap 0-17 intervals), a burst of 2-3 skips per 30-interval window.

v3 PACES the convergence — NO new `ShedAction`, the pure `dupe_shed_action` DECISION byte-UNCHANGED,
only `poll`'s application changes (two new `shed.rs` consts + one `gate.rs` field):
- **A STEADY shallow-lag trickle-drain.** When `!converging_deep_backlog && sustained_over_rate &&
  lag >= SHALLOW_DRAIN_LAG_MIN(2)` AND the pace budget allows, a shallow-lag Retire takes ONE
  single-slot skip (advance, emit nothing) to bleed the creep off BEFORE it reaches the FastDrain
  band; else it FILLS (the eleventh-piece invariant, unchanged). At the slow steady creep the trickle
  demand is low → ~1 skip per gap → 299-300 windows, and lag never reaches the FastDrain band so
  FastDrain essentially never fires steady-state → NO burst.
- **The latched convergence TAIL (Retire/Drain) is PACED** with the same budget (paced-out Retire →
  FILL; paced-out Drain → the v2 STEADY-HOLD, NEVER a stale FILL — a Drain's frame is ≥2 intervals
  stale) so any tail smears to ≤1 skip per gap.
- **ALL skip sites share ONE MONOTONIC pace budget** `last_converge_skip_mono_ns` (a min-gap
  predicate — inherently depth-1, no saved-up burst; monotonic so DanteSync realtime steps can neither
  freeze convergence nor grant a free skip). `CONVERGE_SKIP_MIN_GAP_INTERVALS = 30` (500 ms, ≤2
  skips/s cap → steady emit floor ~58 fps, above the #666 floor 57).
- **KEY GOTCHA — FastDrain is DELIBERATELY LEFT UNPACED and at +2; do NOT pace it or halve it to +1.**
  A design consult argued deep-lag convergence is advance-driven hence pace-independent — the REAL
  `run_grid_backlog_sim` DISPROVES it: boundary lag drains only when a shed actually SKIPS (a paced-out
  FILL advances +1 but consumes ~a full interval of `now`, so it does NOT drain lag). Pacing FastDrain
  OR halving its +2 to +1 throttles the deep drain to the (low) dupe rate and blows the 12s bound —
  measured **15.3 s** for a 24-frame backlog at the 61.5 fps takt (dupe rate only ~1.5/s), vs 9.3 s
  with the +2. So FastDrain keeps the v2 +2 burst for a genuine DEEP reconnect (an accepted rare
  window dip); the STEADY 300/293 oscillation is fixed UPSTREAM by the trickle, not by touching
  FastDrain. FastDrain still STAMPS the shared budget so the paced tail/trickle around it stay
  suppressed (one coherent skip stream). The `SHALLOW_DRAIN_LAG_MIN` threshold must be low (`2`, not
  `3`): off-rig, `3` let a creep→FastDrain burst slip through, `2` reliably prevents it.
- **Composition:** issue-1131 — every skip is single-slot except FastDrain's deliberate +2 (unchanged,
  #707-deducted); #666 — the 2/s pace cap keeps steady emit ≥58 fps; #1142 — smearing strictly
  improves uniformity vs a localized burst; frozen-source — the freshness copy valve is untouched
  (never a 0 fps blackout); healthy-60 — every new branch is `sustained_over_rate`-gated and the pace
  stamp only mutates on a performed skip, so a 60.00 card is byte-inert. NO counter/tuple/summary
  change (`take_shed_counts` stays a 6-tuple; a trickle skip records as `retired`, so on the live
  over-rate box `retired` goes to ~1 per gap — attribute via the paced cadence). `main.rs` untouched.
- **Off-rig (real `poll`, #557 scratch):** RED (send-bound 63.5 creep) delta 3 / min-skip-gap 17 /
  burst 2 → GREEN delta 2 / gap ≥34 / burst 1; the 24-frame deep backlog STILL converges 8.65 s
  (≤12 s) and 12-frame 4.65 s (≤6.5 s); 75/75 module tests pass. Tune the window-sent proxy against
  the LIVE slack — the deficit is throughput-slack-dependent (a heavily send-bound sim over-estimates
  it); the load-bearing invariants are the skip GAP + the ±1 id jump, not the raw window count.
- **Supervisor's live rig step:** confirm the `[4i/8align]` cam1 offset holds ±1 across the last
  trilogy of rounds and the per-5s `Streaming:` windows stay 299-300 (no 293 dips) on cam1.

## #1167 v4 (2026-08-22) — bounded last-frame REPEAT on empty-queue STARVATION (the THIRTEENTH piece)

v2/v3 fixed the OVER-rate direction (grabber >60). But the sick ShadowCast grabber's rate WANDERS
across 60 (live 57.9–63.6 fps). In an UNDER-rate window the whole v2/v3 fill machinery — which runs
INSIDE `poll`, fired ONCE PER CAPTURED FRAME (`main.rs` polls inside `capture.process_frame`, whose
blocking `self.stream.next()?` has NO grid timer) — has NOTHING to fill with: it only CONVERTS a
captured frame's shed into a copy; it cannot fabricate an emit when NO capture arrived. Fewer than 60
polls/s ⇒ fewer than 60 emits. Each empty-queue boundary (`queue_had_frame==false`, the loop genuinely
waited) goes unfilled; grid lag creeps until `genlock_emit_gate`'s #131 resync (`lag>8 &&
!queue_had_frame`) skips it → emit under-runs. Reproduced off-rig (REAL `poll`, #557 scratch, unique
captures, empty queue): **57.9fps → 290/300 sent, 36 #707 skips/20s, ZERO dupe-decimation events**
(`sustained_over_rate` FALSE at 57.9: takt 17.27ms > `RETIRE_MIN_TAKT_INTERVAL_NS` 16.58ms) — a
DIFFERENT regime from v2/v3. The #1145 copy valve can't help (it needs a DUPE arriving to convert; an
under-CAPTURING source delivers FEWER all-unique frames, not extra dupes).

The fix — FILL empty-queue slots by REPEATING the last frame, bounded, in `poll`'s `Emit` arm (NO new
`ShedAction`, the pure `dupe_shed_action` DECISION byte-UNCHANGED — only the `Emit` application + two
new fields + a const change):
- **Gated on a POSITIVE `sustained_under_rate()`, NOT `!sustained_over_rate`.** `takt_ema != 0 &&
  takt_ema > STARVATION_MIN_TAKT_INTERVAL_NS` (= `1e9*10/598` = 1e9/59.8 ≈ 16.722ms — the slow-side
  mirror of the 60.3 over-rate threshold). The `takt_ema != 0` requirement is LOAD-BEARING: a caller
  that disables the takt EMA (`capture_mono_ns==0` → `takt_ema==0`) reads `!sustained_over_rate` as
  TRUE even at an over-rate content pattern, so keying the fill on that fired v4 wrongly and broke 5
  legacy over-rate/starved tests (they pass mono=0). A positive under-rate signal fires ONLY on a
  measured under-rate; over/under-rate are mutually exclusive (STARVATION_MIN 16.722 > RETIRE_MIN
  16.584), and a healthy 60.0 card (EMA ~16.667) reads FALSE for both.
- **Full gate (Emit arm):** `!copy && !queue_had_frame && sustained_under_rate() &&
  !converging_deep_backlog && 1 <= lag_intervals <= GENLOCK_MAX_CATCHUP_INTERVALS`. `poll` reports
  `min(lag, STARVATION_REPEAT_MAX - consecutive_starvation_repeats)` repeats via
  `last_poll_starvation_repeats()` and advances `next_boundary = candidate_next + repeats*interval`
  (capped so it never overshoots `now`; the `lag<=8` gate guarantees `genlock_emit_gate` did NOT
  resync, so `candidate_next == boundary+interval`).
- **FORWARD-FILL (re-emit the CURRENT good frame), NOT a saved last-good buffer.** The dispatch
  SUGGESTED a saved buffer; that would add a **~4MB memcpy PER FRAME** to the carefully-zero-copy
  production hot path (the whole point of #279/#280) — a jitter/throughput regression the #899
  RT-isolation work cannot afford. The current frame passed `process_frame`'s V4L2_BUF_FLAG_ERROR
  check before the callback (GOOD bytes — the "never emit corrupted content" constraint holds), and
  showing it ≤`STARVATION_REPEAT_MAX` frames early for a past slot vs a saved previous frame is a
  ≤4-frame difference the strih genlock FIFO re-times away, IDENTICAL in the id-contiguity
  ([4i/8align]) + uniformity metrics that gate this.
- **`main.rs` wiring:** the send path (cfg-split burn/production) is factored into a nested
  `emit_one(emit_timecode_100ns)` FnMut closure (mirroring the existing `tee_grab` nested-closure
  pattern); the loop `for j in (1..=starvation_repeats).rev()` emits the current frame at each
  repeat's own boundary timecode (`genlock_pacing::starvation_repeat_timecode_100ns(base, j, fps)` =
  `base − j*(10_000_000/fps)`, a pure Tier-0 helper — distinct per-slot timecodes are REQUIRED or the
  FIFO collapses the repeats into one slot), then `emit_one(capture_timecode_100ns)` for the current.
  0 repeats in every healthy/over-rate window ⇒ the loop is a no-op, byte-identical to the pre-v4
  single send.
- **BOUNDED by `STARVATION_REPEAT_MAX`=4 CONSECUTIVE repeats, reset by ANY on-time capture**
  (`lag_intervals==0`). A live-but-slow 57.9fps grabber crosses a boundary only ~2×/s with ~27 on-time
  frames between (each resetting the counter) → never approaches the cap → fully filled. **The cap's
  EXPOSURE reach is precise (review 🔵):** it bites only when EVERY poll is ≥1 interval late so no
  on-time capture ever resets it — i.e. ≤~30fps (a genuinely dead/half-dead leg), which then under-runs
  → visible to #666. It does NOT by itself expose a moderate SUSTAINED under-rate (~31–56fps, which has
  occasional on-time resets and IS filled to 60 on the emit side); that band's exposure is the
  capture-rate health guards (#656/#717/#971 self-heal, reading the SAME takt EMA on the CAPTURE side —
  `.claude/rules/self-heal-frozen-leg-attribution.md`). A FROZEN source delivers DUPES →
  `Emit{copy:true}` → the `!copy` gate excludes it → 0 repeats → under-runs regardless. So **a
  dead/frozen camera still looks down** (the ticket's hard constraint); the cap's job is bounding a
  burst + killing an infinite freeze-loop, not classifying every under-rate. Verified: frozen source 0
  repeats; 30fps stays exposed under 60.
- **DECOUPLED from v2/v3** (the dispatch's "a repeat is NOT a convergence skip"): a SEPARATE counter
  (`DupeShedLog.starvation_repeats`, drained via `take_starvation_repeats()` — the byte-frozen 6-tuple
  `take_shed_counts` is UNCHANGED), a SEPARATE under-rate gate, and it NEVER touches the v3
  `last_converge_skip_mono_ns` pace budget or the retire/drain/fast_drain counters. Folded into
  `last_poll_intentional_extra_advance()` (= `fast_drain_extra + starvation_repeats`, mutually
  exclusive) so a FILLED slot is deducted from the #707 boundary-skip diagnostic (a fill is not a
  sick-leg SKIP). APPENDED a summary segment (`{n} starvation last-frame repeats (#1167 v4
  empty-queue slot-fill)`) — existing substrings byte-frozen.
- **Off-rig (real `poll`, #557 scratch):** 57.9fps 290→300/window, 59.7 299→300, 0 net #707 skips;
  over-rate 63.6 + healthy 60.0 byte-inert; frozen source 0 repeats; 30fps half-rate exposed under 60.
  83/83 module tests pass; `cargo fmt --all --check` clean. **The `#1167 (TENTH piece)` corrupted
  make-up stays DORMANT** (its own 2026-08-22 note already records it does not fire live — v4 is the
  real under-rate fix, that was the over-rate corruption case).
- **Uniformity honesty:** at 57.9fps the repeat rate is ~11/window (3.5% dups → uniformity ~0.965,
  margin +0.015 over the 0.95 floor). The CONSECUTIVE cap bounds a burst + prevents dead-leg masking,
  NOT a moderate SUSTAINED under-rate (out of the wander scenario — a deep sustained deficit is caught
  by the existing uniformity / #666 gates, not papered over).
- **main.rs is UNVERIFIABLE locally** (Tier-0 #557 blocks all cargo compile) — the `emit_one` closure
  refactor's type/borrow correctness is CI's first check; `cargo fmt --all --check` (which parses
  main.rs) is the only local net and was clean.
- **Supervisor's live rig step:** confirm the per-5s `Streaming:` windows hold 299-300 on cam1 in an
  UNDER-rate window (grabber < 60) with `starvation last-frame repeats` > 0, and `[4i/8align]` cam1
  offset holds ±1 across the wander in BOTH directions.

## #1167 v5 (2026-08-22) — make the empty-queue fill REGIME-INDEPENDENT (the FOURTEENTH piece)

v4 fixed the SUSTAINED under-rate direction, but the causal chain for the cam1 `[4i/8align]`
sawtooth was still open: cam1 averages OVER-rate (61–63 fps) yet its sick ShadowCast grabber has
25.6 ms `#707 V4L2 DEQUEUE STALL` episodes. During a stall the queue drains empty, the emit slot
crosses a boundary from an EMPTY queue — but v4's fill was gated on a POSITIVE
`sustained_under_rate()` (takt EMA > `STARVATION_MIN_TAKT_INTERVAL_NS`), which reads FALSE on an
average-over-rate box, so NO fill fired: the slot went out late/gapped, the grid crept behind, the
#131 resync skipped the accumulated boundaries, emit under-ran (live cam1 windows 297–301), and the
strih receiver saw a ~37 ms arrival gap + catch-up burst (`recv-timing #797` cap_max=37.48 ms on
cam1 vs 19–21 on cam2/3/4) → depth 1→8→1 → converge_shed → the presented frame_id sawtooth. The
live evidence closing the chain: PTP LOCK everywhere + 0 dantesync steps (clock ruled out).

The fix — replace the rate gate with a REGIME-INDEPENDENT "live capture takt" signal so the
empty-queue fill fires in BOTH wander directions (`src/dupe_decimation/gate.rs`,
`apply_starvation_fill`'s Emit-arm gate):
- **`has_live_capture_takt()` = `takt_ema_interval_ns != 0`, replacing `sustained_under_rate()`.**
  The gate becomes `!copy && !queue_had_frame && has_live_capture_takt() && !converging_deep_backlog
  && 1 <= lag <= GENLOCK_MAX_CATCHUP_INTERVALS`. A live grabber near 60 (over/at/under) keeps a
  non-zero EMA, so the fill is no longer rate-gated. `sustained_under_rate()` +
  `STARVATION_MIN_TAKT_INTERVAL_NS` are REMOVED (dead after v5).
- **The mono=0 legacy-test collision is solved the SAME way v4's `takt_ema != 0` requirement did,
  NOT by `!sustained_over_rate`.** A caller that disables the takt EMA (`capture_mono_ns == 0`, the
  legacy over-rate/starved unit tests) reads `takt_ema == 0` → `has_live_capture_takt()` FALSE →
  the fill never arms → those tests stay byte-inert. This is why keying on the EMA (not on
  `!sustained_over_rate`, which reads TRUE with the EMA disabled) is load-bearing — the exact trap
  that broke v4's first attempt on 5 legacy tests.
- **Dead-leg semantics preserved FOR FREE.** A genuine collapse (≥ `TAKT_GAP_SUSTAINED_COUNT`
  consecutive > `TAKT_GAP_EXCLUDE_NS` gaps — a card below ~20 fps) RESETS the takt EMA to 0, so
  `has_live_capture_takt()` disarms → the dead leg under-runs → visible to #666 / frozen-leg
  attribution. A frozen source delivers dupes (`Emit{copy:true}`) → the `!copy` gate excludes it.
  A dead/half-dead camera still looks down (the ticket's hard constraint).
- **NOT a decoupling violation.** The over-rate v2/v3 machinery acts ONLY on a NON-empty queue; the
  fill acts ONLY on an empty queue (`!queue_had_frame`) — disjoint by queue state. The measured
  WIRE rate is IDENTICAL (v4 = v5 = 59.967 fps at 62 fps + a stall): the fill merely moves the
  post-stall empty-queue slots from LATE Drain-hold poll-emits to ON-boundary repeats, killing the
  grid lag and the wire gap. The v4 counter, the #707 fold, the boundary math and the `main.rs`
  forward-fill loop are all byte-unchanged.
- **Two legacy tests reconciled** (their PREMISE the spec deliberately changes; both green on the
  pre-fix AND post-fix code, so not fix-tailored): `over_rate_never_starvation_repeats` →
  `over_rate_with_a_full_queue_never_starvation_repeats` (it forced an over-rate + EMPTY-queue combo
  that v5 now correctly FILLS; corrected to the physical non-empty-queue over-rate state, where the
  `!queue_had_frame` gate keeps the fill off); `over_rate_fills_every_60fps_slot` now counts the
  WIRE rate (poll-emits + starvation repeats) since a post-stall slot is filled by a repeat, not a
  poll-emit.
- **Off-rig (real `poll`, #557 scratch):** over-rate 62 fps + periodic 45 ms stalls,
  `sustained_over_rate=true`. v4: repeats=0, net_skips=18, max_lag=9 (into the resync band), windows
  295–298. v5: repeats>0, net_skips=0, max_lag≤4, windows 300–302. Full module suite 84/84; `cargo
  fmt --all --check` clean. The receiver-side recv-cap_max smoothing (the FIFO paces the
  contiguous-timecode burst) is the supervisor's live-rig verification — the sender-side
  net_skips/max_lag/windows are the faithful off-rig analogues.
- **TESTING GOTCHA — model `queue_had_frame` as "did the loop BLOCK for this frame", NOT
  `run_queue_sim`'s residence proxy.** `run_queue_sim` (the #1145 sawtooth sim) approximates
  `queue_had_frame = now - cap_ns < cap_int/2` (residence < half an interval). That is WRONG for a
  backed-up OVER-rate queue: an over-rate frame has a HIGH residence but an INSTANT dequeue (queue
  full), so the real #1131 `frame_from_nonempty_queue(dequeue_duration_ms, …)` reads TRUE while the
  proxy reads FALSE — inflating the empty-queue slot count. When you write a NEW empty-queue-fill
  test (as the v5 `run_over_rate_stall_sim` does), set `queue_had_frame` from whether the loop
  actually WAITED on an empty queue for that frame (a `waited_for_this` flag), the faithful
  dequeue-duration analogue — otherwise the sim fires the fill on backed-up over-rate frames that
  production never would. (The existing `over_rate_fills_…` test tolerates the proxy because it only
  reads the wire RATE, which is proxy-independent.)
- **Supervisor's live rig step:** confirm the per-5s `Streaming:` windows hold 299–301 on cam1
  through the OVER-rate wander (grabber > 60 with `#707` DQBUF stalls) with `starvation last-frame
  repeats` > 0, the strih `NDI cam1` `recv-timing #797` cap_max drops toward the cam2/3/4 ~20 ms
  band, converge_sheds stop climbing, and `[4i/8align]` cam1 holds ±1.

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
