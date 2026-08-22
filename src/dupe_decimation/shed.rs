// (#1165 split) No imports needed — this shed-decision cluster is self-contained (its only
// cross-module references are the full-path intra-doc links to `crate::dupe_decimation::gate`).

// ── (#1145) v2 tuning constants ──────────────────────────────────────────────

/// (#1167) The largest corrupted-slot make-up DEFICIT the gate carries. Each corrupted buffer
/// dropped in `src/capture.rs::process_frame` (before the emit gate) removes a would-be-emitted
/// GOOD frame from an over-rate stream, so its 60 fps slot would be absorbed by the over-rate
/// shed machinery ([`ShedAction::Retire`] / [`ShedAction::Drain`] — advance the boundary, emit
/// nothing) instead of filled → emit under-runs by exactly the corrupted rate (the strih FIFO
/// hold → cam1 align sawtooth). [`crate::dupe_decimation::DecimationGate::note_corrupted_frame`] accrues one unit of this
/// deficit per corrupted drop; [`crate::dupe_decimation::DecimationGate::poll`] reclaims it 1:1 by converting the NEXT
/// slot-skipping Retire/Drain into a copy emit (the nearest good frame). Bounded to `8` (the
/// #707/#1131 resync catch-up bound): beyond a burst this size the source is genuinely starved
/// and the existing #1111 copy valve / `enough_unique` handoff carries it, so the make-up never
/// forces a long tail of copies after corruption stops.
pub const CORRUPTED_MAKEUP_MAX_DEFICIT: u64 = 8;

/// (#1145) The largest boundary lag (in whole emit-boundary intervals) at which a stale
/// over-rate content-dupe is RETIRED rather than emitted as a copy. Chosen well BELOW the #707
/// resync trigger ([`crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS`] = 8): retirement drains
/// the dupe-driven lag at up to the dupe rate, so at a genuine over-rate the lag never approaches
/// the resync bound (measured peak ~4 across seeds/jitter in the off-rig sim). Beyond this ceiling
/// a genuine sustained deficit is building, so the late-dupe valve emits a copy instead — the
/// panic floor that keeps the emit grid boundary-locked. `4` gives 0 emitted copies at the rig
/// takt (61.x) with realistic jitter AND a comfortable margin (4) to the resync bound.
pub const RETIRE_MAX_LAG_INTERVALS: u64 = 4;

/// (#1145) Trailing wall-clock window over which the UNIQUE (non-dupe) capture arrivals are counted
/// to decide whether the source carries enough distinct content to hold a steady 60 fps without
/// fabricating copies. 2 s is long enough to integrate out per-frame jitter and dupe clustering
/// (a windowed COUNT, unlike an interval EMA, reads the true unique RATE regardless of local
/// spacing) yet short enough that retirement engages within ~2 s of a sustained over-rate.
pub const UNIQUE_RATE_WINDOW_NS: u64 = 2_000_000_000;

/// (#1145) Margin (in unique captures) subtracted from the window's theoretical full count
/// (`UNIQUE_RATE_WINDOW_NS / interval_ns`, e.g. ~120 at a 60 fps target) to derive the retirement
/// floor ([`retire_min_uniques`]). The source must be delivering nearly the full target's worth of
/// DISTINCT content over the trailing window for retirement to engage; below it the source is
/// genuinely starved (a sub-60 source padded to 60 by DUPLICATION — a 50->60 pulldown) and the
/// late-dupe copy valve stays engaged to hold the emit grid at the target (keeping the strih FIFO
/// locked AND leaving the content-dupes in the recording for the duplication-masked pulldown
/// detector). The count is pruned by `now_ns` on EVERY poll (honest at every instant). A true-60
/// over-rate source's honestly-pruned count dips to ~115 at dupe instants under heavy jitter, while
/// a 50-fps pulldown reads ~100/2 s — so the floor MUST sit between them, and cannot ALSO be above a
/// 57.9-unique source (which reads ~114-117, overlapping the jittery-60 case: a 2 s windowed COUNT
/// genuinely cannot separate 60-unique-with-jitter from 57.9-unique). We prioritize the RIG (unique
/// 60): `6` -> floor 114 at 60 fps == 57 fps, which reliably retires the rig even at 30 % jitter,
/// keeps a real pulldown (~100) on the copy valve, and puts the retire/copy boundary at ~57 unique
/// fps — deliberately aligned with the #666 EMIT-rate-deficit floor (5 % of 60), so any source whose
/// honest emit would trip #666 (< 57 fps) gets copies to hold 60, and any source above it emits its
/// honest rate. Parametric on `interval_ns` (#1145 review): follows a non-60 emit target instead of
/// silently no-oping.
pub const RETIRE_UNIQUE_COUNT_MARGIN: u64 = 6;

/// (#1145 review 🔴) Freshness bound for retirement, in whole emit intervals: retirement engages only
/// when the MOST RECENT unique capture arrived within this many intervals of `now`. A genuinely
/// FROZEN source (a dead painter / wedged upstream feeding a still — the #1052/#365 frozen-input
/// class) delivers 100% content-dupes: no unique ever refreshes the window, so its stale count stays
/// high and — without this bound — retirement would fire forever and collapse the NDI emit to ~0 fps
/// (a total output BLACKOUT, strictly worse than a frozen picture). The freshness bound makes a
/// freeze fall back to the late-dupe copy valve within ~this many intervals (a frozen PICTURE on a
/// LIVE, FIFO-fed stream — the pre-#1145 behavior). `5` intervals (~83 ms at 60 fps) is safely above
/// the largest gap since a unique during healthy over-rate operation (an isolated dupe pair sits ~2-3
/// intervals after a unique) yet kills a freeze promptly.
pub const RETIRE_UNIQUE_FRESH_BOUND_INTERVALS: u64 = 5;

/// (#1145) The minimum UNIQUE captures within [`UNIQUE_RATE_WINDOW_NS`] for retirement to engage at
/// the given emit `interval_ns` — the window's theoretical full count minus
/// [`RETIRE_UNIQUE_COUNT_MARGIN`]. `interval_ns == 0` (genlock off) never retires.
pub fn retire_min_uniques(interval_ns: u64) -> usize {
    if interval_ns == 0 {
        return usize::MAX;
    }
    (UNIQUE_RATE_WINDOW_NS / interval_ns).saturating_sub(RETIRE_UNIQUE_COUNT_MARGIN) as usize
}

/// (#1145 v3) Minimum captures in the trailing [`UNIQUE_RATE_WINDOW_NS`] before the OCCUPANCY floor
/// (below) is consulted — a small-sample guard so a cold start (few captures) can never satisfy the
/// ratio. `30` ≈ half a second of captures.
pub const RETIRE_OCCUPANCY_MIN_SAMPLES: usize = 30;

/// (#1145 v3) The GAP-IMMUNE occupancy floor: the minimum `unique / total` capture ratio (percent)
/// in the trailing window for [`crate::dupe_decimation::DecimationGate::enough_unique_to_hold_target`] to hold, an OR
/// supplement to the ABSOLUTE count floor ([`retire_min_uniques`]). A capture HICCUP transiently
/// depresses the absolute count (the 2 s window spans dead time with no captures), forcing a genuine
/// over-rate card onto the #1111 copy valve for ~the gap duration — the surplus then exports into the
/// strih FIFO (the #1145 v3 residual). The unique/total RATIO is gap-immune (a gap admits NO captures,
/// so BOTH counts drop equally). `95` is #666-safe: this arm is gated on `sustained_over_rate` (capture
/// takt below [`RETIRE_MIN_TAKT_INTERVAL_NS`] = capture rate `> 60.3`), so `unique >= 0.95 × total`
/// with `total`-rate `> 60.3` guarantees the retired emit (= the unique rate) stays `>= 0.95 × 60 = 57`
/// (the #666 emit-deficit floor) — an under-rate / starved source (NOT over-rate) never reaches this
/// arm, so retiring can never drop it below 57. A 50->60 pulldown (~0.83 ratio) stays on the copy valve.
pub const RETIRE_OCCUPANCY_MIN_PERCENT: u64 = 95;

// ── (#1145 v2) queue-DEPTH-bounded drain: absorb the over-rate takt CONTINUOUSLY ──────────────
//
// The merged v1 shed/retire keys on [`crate::genlock_pacing::genlock_lag_intervals`] (BOUNDARY
// staleness). When the emit loop is send-bound (~60 fps) and the card captures 61.x, the loop
// processes the OLDEST buffered V4L2 frame each poll and `now` (realtime) lands right on the
// advancing boundary, so the lag reads ~0 the whole time — v1 is BLIND to the growing
// capture->emit QUEUE RESIDENCE. The residence sawtooths (delivery lag 67->167 ms, issue
// 1110/1130) until the 4-deep V4L2 buffer overflow-drops in a burst, and THAT burst is what the
// #1142 uniformity gate reads at ~0.77-0.89 on cam1. v2 measures the residence DIRECTLY
// (`now_monotonic - capture_monotonic`) and sheds the oldest frame once it exceeds a small
// target, draining the over-rate one frame at a time instead of letting it accumulate — GATED on
// a sustained-over-rate capture takt so a healthy 60.00 card (and a #1131 buffered-drain
// stall-recovery on one) is byte-identical to v1.

/// (#1145 v2) The over-rate capture takt threshold, as the minimum EMA capture INTERVAL below which
/// the card is "sustained over-rate": `1e9 / 60.3` ns (~16.584 ms). Integer form to keep it a plain
/// `const`. The ticket names this bound explicitly ("pri sustained over-rate takt >60.3"): a 60.00
/// card reads an EMA interval of ~16.667 ms (ABOVE this — NOT over-rate → the whole depth-shed is
/// OFF, so a healthy card and a transient #1131 stall-recovery on one stay byte-identical to v1),
/// while a 61.x card reads ~16.3 ms (below → over-rate → depth-shed engages). Deliberately at 60.3
/// (not 60.0) so ordinary sub-frame jitter on a genuine 60.00 card never trips it.
pub const RETIRE_MIN_TAKT_INTERVAL_NS: u64 = 1_000_000_000 * 10 / 603;

/// (#1145 v2) Right-shift for the integer EMA that smooths the capture takt: `new = old + ((sample
/// - old) >> SHIFT)`. `8` gives a ~2^8 = 256-frame (~4 s at 60 fps) time constant — long enough to
/// integrate out per-frame V4L2 dequeue jitter into the true sustained takt, short enough that a
/// card that starts drifting is classified over-rate within a few seconds. Init-seeded to the first
/// observed interval so there is no long cold-start (see [`crate::dupe_decimation::DecimationGate::note_capture_takt`]).
pub const TAKT_EMA_SHIFT: u32 = 8;

/// (#1145 v3) The largest inter-capture interval that is FOLDED into the capture-takt EMA
/// ([`crate::dupe_decimation::DecimationGate::note_capture_takt`]) — `3×` the 60 fps emit interval (50 ms). A genuine takt
/// change shows in EVERY sample (~8-25 ms at an over-rate); a delivery HICCUP (a blocked V4L2
/// dequeue — a CPU/#752/USB stall) shows as ONE huge outlier that is NOT a takt change. Folding
/// that outlier into the ~256-frame EMA poisons it: at the 61.5 fps rig takt the EMA sits ~0.32 ms
/// below [`RETIRE_MIN_TAKT_INTERVAL_NS`], so a single `>~99 ms` hiccup flips `sustained_over_rate`
/// off and the τ≈256-frame recovery holds it off for ~7 s (500 ms gap) / ~12 s (1.5 s gap) —
/// disarming depth-Drain, FastDrain AND the round-3 noisy-dupe compare, so the over-rate surplus
/// leaks into the strih FIFO (the #1145 v3 residual). A sample above this bound is SKIPPED (not
/// folded), while `prev_capture_mono_ns` still advances so the NEXT interval is measured cleanly
/// from the post-gap capture. `3×` (not 2×) leaves headroom above the worst legitimate over-rate +
/// USB jitter sample (≤ ~2× nominal) while still excluding any genuine multi-interval stall.
pub const TAKT_GAP_EXCLUDE_NS: u64 = 3 * (1_000_000_000 / 60);

/// (#1145 v3 review 🟡 F1) How many CONSECUTIVE over-[`TAKT_GAP_EXCLUDE_NS`] inter-capture samples
/// distinguish a one-off delivery HICCUP (skip the lone outlier) from a GENUINE sustained rate
/// COLLAPSE (a card dropping below ~20 fps — every interval over-bound). At/above this count the takt
/// EMA is RESET so `sustained_over_rate` disarms (a collapsed card is NOT over-rate) and re-seeds when
/// it recovers, instead of latching the over-rate drains on forever. `3` catches a genuine collapse
/// in ~3 frames while a lone hiccup (exactly ONE over-bound sample) never reaches it — the B.1 fix is
/// fully preserved. A collapsed `< 20 fps` card is itself an alarm-class failure owned by the
/// grabber-STUCK self-heal; this just keeps the over-rate arming honest through it.
pub const TAKT_GAP_SUSTAINED_COUNT: u32 = 3;

/// (#1145 v2) The queue-residence depth (in whole emit intervals) at/above which the oldest queued
/// frame is SHED to drain one interval of delivery latency — the target the over-rate is held to.
/// `2`: an emitted frame then carries at most ~1 interval of queue residence (fresh), and the
/// capture-stage residence never climbs toward the 4-deep V4L2 overflow, so the downstream FIFO is
/// fed fresh content too. A healthy 60.00 card sits at residence ~0-1 and (being NOT over-rate) never
/// reaches this arm regardless. Calibration value; the live E2E re-measure tunes it.
pub const QUEUE_DEPTH_SHED_INTERVALS: u64 = 2;

/// (#1145 v2) A DETECTED content-dupe drains one interval EARLIER than [`QUEUE_DEPTH_SHED_INTERVALS`]
/// — shedding a byte-identical re-sample is always content-safe (its neighbour carries the same
/// painted frame), so draining it at residence `>= 1` keeps the queue shallower with ZERO risk of
/// dropping a distinct painted frame. `1`. Only reached when already over-rate.
pub const QUEUE_DEPTH_DUPE_SHED_INTERVALS: u64 = 1;

/// (#1145 v2) Sanity ceiling on the computed queue-residence depth: a bogus/huge
/// `capture_monotonic` (or a clock-domain mismatch) must never be read as a runaway depth that
/// force-sheds far beyond the real 4-deep V4L2 buffer. `8` == the #707/#1131 resync catch-up bound
/// ([`crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS`]); a real residence cannot exceed the
/// buffer depth, so clamping here only defends against a garbage timestamp.
pub const QUEUE_DEPTH_SANE_MAX_INTERVALS: u64 = 8;

/// (#1167) The Drain-hold PANIC FLOOR: after this many CONSECUTIVE Drain polls have HELD the SAME
/// boundary (the #1167 Drain drops the oldest to bound residence but no longer advances), fill the
/// slot with a copy instead of holding forever — the fail-SAFE guard against a bogus stuck-high
/// residence signal (a garbage `capture_mono`). Aliased to [`QUEUE_DEPTH_SANE_MAX_INTERVALS`]
/// (the residence clamp), NOT reusing it directly, so a future clamp retune cannot silently move
/// the floor. A genuine hold run caps at ~4-6 (the 4-deep V4L2 buffer + the 5-interval freshness
/// bound), so `8` is unreachable except via a garbage timestamp.
pub const DRAIN_HOLD_PANIC_FLOOR: u64 = QUEUE_DEPTH_SANE_MAX_INTERVALS;

// ── (#1167 v3) PACE the convergence skips so they never BURST ─────────────────────────────────

/// (#1167 v3) The minimum gap, in whole emit intervals of MONOTONIC time, between two convergence
/// slot-SKIPS (any boundary-advance-emit-nothing shed: a latched-Retire, a latched-Drain, or the
/// steady shallow trickle-drain). v2/dev.533 held cam1's AVERAGE emit at 59.94 but windows
/// oscillated 300/300/293: when the degrading grabber's ~3.5fps surplus creeps grid lag past
/// [`RETIRE_MAX_LAG_INTERVALS`], FastDrain fires and LATCHES, and the whole shallow tail drains as a
/// BURST of ~6-7 advance-emit-nothing sheds within a fraction of a second — one 5s window drops to
/// 293 and cam1's presented-frame_id jumps ~+7 vs its siblings (the [4i/8align] "mutual stability
/// <=1 id" abort). This SMEARS those skips: at most ONE convergence skip per `30` intervals (500ms
/// == 2 skips/s cap), so the presented-id never jumps more than +1 at a time and the strih FIFO
/// re-buffers between skips. `30` (2 skips/s) keeps the worst-case steady emit floor at ~58 fps —
/// comfortably above the #666 emit-deficit floor (57 fps). Measured on the MONOTONIC clock (a
/// duration between downstream-visible id jumps), NOT `now_ns`: `now_ns` is DanteSync-phase-STEPPED
/// (a backward step would freeze convergence during exactly the events that inject lag; a forward
/// step would grant a free skip coincident with a lag injection). FastDrain itself is deliberately
/// NOT paced (its +2 deep-backlog drain must converge a genuine reconnect within the 12s bound at the
/// low dupe rate — verified off-rig); the trickle keeps steady lag below the FastDrain band so
/// FastDrain essentially never fires in steady state.
pub const CONVERGE_SKIP_MIN_GAP_INTERVALS: u64 = 30;

/// (#1167 v3) The smallest grid lag (in whole boundary intervals) at which the STEADY (non-
/// converging) shallow-lag Retire path takes a PACED skip instead of filling — the trickle that
/// drains the slowly-accumulating shallow lag before it can creep past [`RETIRE_MAX_LAG_INTERVALS`]
/// and trip a FastDrain BURST. Because steady lag accumulates slowly (well under one interval/s), the
/// trickle demand is low, so with the [`CONVERGE_SKIP_MIN_GAP_INTERVALS`] budget it fires at most ~1
/// skip per 5s window (299-300, never a burst). `2` keeps a healthy 60.00 card (never over-rate)
/// and a lag-0/lag-1 steady over-rate box (dupes DEFER / FILL, no skip) untouched — the trickle only
/// engages once a real interval of grid lag has built up (off-rig: `2` reliably prevents the
/// creep→FastDrain burst where `3` let a burst slip through).
pub const SHALLOW_DRAIN_LAG_MIN: u64 = 2;

// ── (#1167 v4) bounded last-frame REPEAT on empty-queue STARVATION ────────────────────────────

/// (#1167 v4) The largest number of CONSECUTIVE last-frame repeats the gate emits to fill
/// empty-queue 60fps slots before it gives up and lets the slot skip (the honest #131 resync). The
/// whole v2/v3 fill machinery runs inside [`crate::dupe_decimation::DecimationGate::poll`], which
/// fires ONCE PER CAPTURED FRAME — so when the grabber captures BELOW 60 fps (the sick-ShadowCast
/// wander dips to 57.9) fewer than 60 polls happen per second and there is nothing to fill the empty
/// boundaries with. `poll` instead reports up to this many last-frame repeats (re-emit the current
/// GOOD frame — never corrupted content) for the boundaries an empty-queue dip left unfilled, so emit
/// holds ~60 in BOTH wander directions. Bounded, and the count is CONSECUTIVE (reset by ANY on-time
/// capture, see [`crate::dupe_decimation::DecimationGate::poll`]): a source that keeps pace (up to the
/// mild-wander band, e.g. 57.9 fps — its drift crosses a boundary only ~2×/s, isolated, with on-time
/// frames between that reset the count) is fully filled. The cap's EXPOSURE reach is precise: it only
/// bites when EVERY poll is ≥1 interval late so no on-time capture ever resets it — i.e. ≤~30 fps
/// (a genuinely dead/half-dead leg), which then under-runs and stays visible to #666 / #1133
/// leg-health. It does NOT by itself expose a moderate SUSTAINED under-rate (~31–56 fps, which has
/// occasional on-time resets and IS filled to 60 on the emit side); that band is caught by the
/// capture-rate health guards (#656/#717/#971 self-heal, which read the SAME takt EMA on the capture
/// side) — see `.claude/rules/self-heal-frozen-leg-attribution.md`. A FROZEN source delivers dupes
/// (`Emit{copy:true}`), which the `!copy` gate excludes outright → 0 repeats → under-runs regardless.
/// So a dead/frozen camera always looks down; the cap's job is bounding a burst + killing an infinite
/// freeze-loop, not classifying every under-rate. `4`: ≥3 (the ticket's floor),
/// comfortably above a healthy 57.9 fps wobble's isolated single-slot repeats (with margin for a
/// brief multi-interval stumble), and ≤ the #131 resync catch-up bound
/// ([`crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS`] = 8) so a capped-out starvation hands
/// off cleanly to the honest resync-skip. Calibration value; the live E2E re-measures it.
pub const STARVATION_REPEAT_MAX: u64 = 4;

/// (#1145 v2) The queue-residence depth of a captured frame, in whole emit intervals: how long the
/// frame sat between its CAPTURE instant (`capture_mono_ns`, the V4L2 buffer's `CLOCK_MONOTONIC`
/// timestamp) and the instant the loop PROCESSED it (`now_mono_ns`, `monotonic_clock_ns()`), divided
/// by `interval_ns`. This is a DURATION, so it is measured on the monotonic clock (immune to the
/// DanteSync/NTP realtime steps the emit boundary is gridded to). `0` (no drain) when the signal is
/// unavailable: `interval_ns == 0` (genlock off), `capture_mono_ns == 0` (the FrameInfo "no real
/// measurement" sentinel), or `now_mono_ns <= capture_mono_ns` (a monotonic non-advance / bogus
/// stamp). Clamped to [`QUEUE_DEPTH_SANE_MAX_INTERVALS`] so a garbage timestamp can never force a
/// runaway shed. (#1145 v2 review 🔵) The residence trusts `capture_mono_ns` to be the SAME
/// `CLOCK_MONOTONIC` domain as `now_mono_ns` — the repo-wide #286 assumption for the V4L2 buffer
/// timestamp; a device stamping a lower-epoch domain would read a huge residence, but the
/// `QUEUE_DEPTH_SANE_MAX_INTERVALS` clamp bounds the consequence to a bounded (not unbounded) drain.
pub fn queue_depth_intervals(now_mono_ns: u64, capture_mono_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 || capture_mono_ns == 0 || now_mono_ns <= capture_mono_ns {
        return 0;
    }
    ((now_mono_ns - capture_mono_ns) / interval_ns).min(QUEUE_DEPTH_SANE_MAX_INTERVALS)
}

// ── (#889/#1145) victim-selection decision ────────────────────────────────────

/// (#1145) The per-captured-frame shed/emit decision, one of five actions. `would_emit` is the
/// PACING gate's verdict (did this capture cross the wall-clock boundary?); `is_dupe` whether it is
/// a byte-identical content dupe of the immediately preceding capture; `lag_intervals` how many
/// whole boundary intervals `now` sits PAST the pending boundary
/// ([`crate::genlock_pacing::genlock_lag_intervals`]); `enough_unique_to_hold_target` whether the
/// trailing-window UNIQUE rate proves the source can hold a steady 60 fps without copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShedAction {
    /// Emit this captured frame and ADVANCE the boundary one interval. `copy == true` marks a
    /// content-dupe emitted as the late-dupe valve (a repeated frame downstream — the starvation
    /// floor / double-dupe guard); `false` for a genuinely unique tick.
    Emit { copy: bool },
    /// #889 on-time deferral: HOLD the boundary (do NOT advance) and shed this content-dupe — the
    /// next capture re-evaluates against the SAME still-pending boundary, so the dupe is replaced
    /// by a unique that still lands inside the interval (lag-neutral).
    Defer,
    /// #1145 stale-boundary retirement (the DECISION for a shallow-stale over-rate dupe). The
    /// boundary the dupe crossed is already stale (`lag >= 1` — the downstream hold for it already
    /// happened), and it sacrifices no unique + drains the dupe-driven lag. **(#1167 v3) `poll` now
    /// REINTERPRETS this decision by application in THREE cases**, all PACED by the shared
    /// [`crate::dupe_decimation::CONVERGE_SKIP_MIN_GAP_INTERVALS`] budget: (a) while CONVERGING a deep
    /// backlog → a paced retire (advance, emit nothing; paced-out → FILL); (b) in STEADY over-rate once
    /// lag has crept to [`SHALLOW_DRAIN_LAG_MIN`] → a paced single-slot TRICKLE retire that bleeds the
    /// creep off before it can reach the FastDrain band and BURST (the #1167 v3 fix for the 300/293
    /// oscillation); (c) otherwise (lag below the trickle threshold, or paced-out) → FILL the slot with
    /// a copy of the nearest good frame, so no 60fps slot is skipped between paced skips. See
    /// [`DecimationGate::poll`].
    Retire,
    /// #1145 v2 queue-DEPTH drain: shed the OLDEST (this) frame — the sustained-over-rate absorption —
    /// to bound the queue RESIDENCE (`now_monotonic - capture_monotonic`) once it exceeds the depth
    /// target, so the delivery-latency sawtooth is drained one frame at a time instead of a burst.
    /// **(#1167) `poll` splits the application**: while CONVERGING a deep backlog it ADVANCES the
    /// boundary (as #1145 v2 did — the drop contributes to convergence); in STEADY over-rate it HOLDS
    /// the boundary (drops the oldest but does NOT advance) so the next fresher frame fills the same
    /// slot — never a skipped slot — with a panic-floor copy-fill after
    /// [`DRAIN_HOLD_PANIC_FLOOR`] consecutive holds. See [`DecimationGate::poll`]. Fires only at a
    /// genuine over-rate takt. Distinct from [`Retire`](Self::Retire): Retire keys
    /// on BOUNDARY lag and only sheds a content-dupe; Drain keys on the queue RESIDENCE and, above
    /// the target, sheds the oldest frame regardless (its downstream tick has already passed, so it
    /// is a controlled single-frame drop that pre-empts the uncontrolled V4L2 overflow-drop).
    ///
    /// (#1145 v2 review 🔵) #1131 interaction for an OVER-RATE card: a transient-stall buffered
    /// drain on an over-rate card now DRAINS (sheds the oldest one at a time) rather than emitting
    /// every buffered frame — the intended bound-latency behavior (emitting the whole burst would
    /// just re-judder). This is a SINGLE-frame drop, never a grid-resync leap, so #1131's
    /// leap-past-and-discard-a-run is still avoided. A 60.00 card is unaffected (not over-rate →
    /// never drains → emits every buffered frame, byte-identical to v1 — constraint c).
    Drain,
    /// (#1145 v2.1) DEEP-backlog accelerated drain: shed this content-dupe AND advance the boundary
    /// by TWO intervals (retire an EXTRA already-stale boundary), emitting nothing — the accelerated
    /// convergence of a deep emit-grid backlog (the delivery-latency lag the owner's painter-QR
    /// measured at 12+ frames after a reconnect / restart / burn toggle). Fires ONLY at a sustained
    /// over-rate when the grid lag is DEEP (`lag > `[`RETIRE_MAX_LAG_INTERVALS`], == 2x the
    /// [`QUEUE_DEPTH_SHED_INTERVALS`] target), where v2 would emit the late dupe as a COPY (no grid
    /// advance) — so the deep backlog drained only at the send-slack rate (~0.3 frame/s, the owner's
    /// measured ~35 s). Distinct from [`Retire`](Self::Retire): Retire advances ONE interval (the
    /// steady over-rate absorption, lag <= 4); FastDrain advances TWO (drain up to 2 slots per emit
    /// interval) — the extra boundary is ALSO already stale (lag > 4 >> 2), so no new downstream gap
    /// and no unique dropped (only the dupe is shed; the +2 is guarded in [`crate::dupe_decimation::DecimationGate::poll`] so
    /// it never advances the grid into the future). Only DUPES take this path, so the issue-1131
    /// "never drop a unique while uniques exist" constraint holds and the emit rate stays >= the #666
    /// floor.
    FastDrain,
    /// Between boundaries (`!would_emit`): blind-shed, boundary unchanged — the pre-existing pacing
    /// decimation drop.
    BlindShed,
}

/// (#889/#1111/#1145) Decide the [`ShedAction`] for one captured frame. Pure — driven entirely by
/// its inputs, so the whole cadence policy is testable off real hardware.
///
/// - `!would_emit` (between boundaries) -> [`ShedAction::BlindShed`] (unchanged blind pacing).
/// - unique tick -> [`ShedAction::Emit`]`{ copy: false }` (unchanged).
/// - content-dupe, `lag == 0` (on-time/surplus): #889 -> [`ShedAction::Defer`] once; a second dupe
///   for the SAME boundary (`already_deferred`) -> [`ShedAction::Emit`]`{ copy: true }` (the bounded
///   one-deferral guard — validated dupes are isolated pairs).
/// - content-dupe, `1 <= lag <= `[`RETIRE_MAX_LAG_INTERVALS`] (SHALLOW-stale boundary): #1145 ->
///   [`ShedAction::Retire`] as the DECISION. (#1167 v3) [`crate::dupe_decimation::DecimationGate::poll`]
///   REINTERPRETS that Retire, PACED by the shared budget: during a deep-backlog convergence a paced
///   retire (advance, emit nothing; paced-out → FILL); in STEADY over-rate a paced single-slot TRICKLE
///   retire ONCE lag has crept to [`crate::dupe_decimation::SHALLOW_DRAIN_LAG_MIN`] (bleeds the creep
///   off before it bursts — the 300/293 fix), else FILL a copy of the nearest good frame (holds 60
///   between paced skips). The decision stays Retire so the #1145 decision tests + deep-backlog
///   convergence rate are preserved; only the application changed.
/// - content-dupe otherwise (NOT enough unique — genuine starvation; OR `lag > `the retire ceiling
///   but NOT the deep FastDrain band): [`ShedAction::Emit`]`{ copy: true }` — the #1111 late-dupe
///   valve, a starvation floor that holds the emit grid boundary-locked at 60.
///
/// (#1145 v2) BEFORE all of the above, a sustained-over-rate QUEUE-DEPTH drain runs — this is the
/// arm that actually bounds the delivery-latency sawtooth the lag-based v1 could not see.
/// `queue_depth_intervals` is the frame's monotonic queue residence ([`queue_depth_intervals`]);
/// `sustained_over_rate` whether the capture takt EMA is genuine over-rate (a healthy 60.00 card is
/// FALSE here, so this whole block is skipped and the card is byte-identical to v1 — including a
/// #1131 buffered-drain stall-recovery on one). When over-rate:
/// - residence `>= `[`QUEUE_DEPTH_SHED_INTERVALS`] -> [`ShedAction::Drain`]: shed the OLDEST (this)
///   frame regardless of dupeness — a controlled single-frame drop that drains one interval of
///   latency and pre-empts the uncontrolled V4L2 overflow-drop (the burst that shows as judder).
/// - a DETECTED content-dupe at residence `>= `[`QUEUE_DEPTH_DUPE_SHED_INTERVALS`] ->
///   [`ShedAction::Drain`]: drains one interval EARLIER, always content-safe (a byte-identical
///   re-sample carries no distinct painted frame).
pub fn dupe_shed_action(
    would_emit: bool,
    is_dupe: bool,
    already_deferred_this_boundary: bool,
    lag_intervals: u64,
    enough_unique_to_hold_target: bool,
    queue_depth_intervals: u64,
    sustained_over_rate: bool,
) -> ShedAction {
    if !would_emit {
        return ShedAction::BlindShed;
    }
    // (#1145 v2) sustained-over-rate queue-DEPTH drain — the continuous over-rate absorption that
    // keeps the delivery latency flat (see the module + `queue_depth_intervals` docs). Gated on
    // `sustained_over_rate` so a healthy 60.00 card never reaches it (byte-identical to v1); shed
    // the oldest frame once its queue residence exceeds the target, one frame at a time.
    if sustained_over_rate {
        // (#1145 v2 review 🔵) The FIRST arm INTENTIONALLY sheds the oldest regardless of
        // `enough_unique_to_hold_target` — when the residence has already reached the target the
        // latency MUST be bounded, so bounding it overrides the "keep content-dupes for the
        // duplication-masked pulldown detector" invariant the second arm + retirement preserve. In
        // practice this only bites a genuinely-starved source captured at an over-rate takt (rare);
        // there the bounded-latency win outranks preserving a dupe the detector could read.
        if queue_depth_intervals >= QUEUE_DEPTH_SHED_INTERVALS {
            return ShedAction::Drain;
        }
        // The SECOND arm drains a DETECTED dupe one interval earlier — content-safe, and it DOES
        // preserve the pulldown invariant (`enough_unique_to_hold_target` gate), so a starved source
        // never loses its dupes here.
        if is_dupe
            && enough_unique_to_hold_target
            && queue_depth_intervals >= QUEUE_DEPTH_DUPE_SHED_INTERVALS
        {
            return ShedAction::Drain;
        }
    }
    if !is_dupe {
        return ShedAction::Emit { copy: false };
    }
    if lag_intervals == 0 {
        if already_deferred_this_boundary {
            return ShedAction::Emit { copy: true };
        }
        return ShedAction::Defer;
    }
    if enough_unique_to_hold_target && lag_intervals <= RETIRE_MAX_LAG_INTERVALS {
        // (#1145) shallow-stale boundary: the DECISION is Retire (drain the dupe-driven lag). (#1167)
        // its poll APPLICATION now depends on whether the gate is CONVERGING a deep backlog: in steady
        // over-rate it FILLS the slot with a copy (holds 60 — the ticket invariant), while during a
        // deep-backlog convergence (a FastDrain fired recently, lag still elevated) it RETIRES (advance,
        // emit nothing) so the grid catches up fast. Keeping the decision as Retire preserves the whole
        // #1145 decision-test surface + the deep-backlog convergence rate; only `poll` reinterprets it.
        return ShedAction::Retire;
    }
    // (#1145 v2.1) DEEP backlog (lag > RETIRE_MAX_LAG_INTERVALS == 2x the depth target) at a
    // sustained over-rate with enough distinct content: this is a TRANSIENT grid backlog to drain
    // (a reconnect / restart / burn-toggle left the emit grid behind), NOT a genuine sustained
    // deficit. v2 emitted these late dupes as COPIES (no grid advance), so a deep backlog drained
    // only at the send-slack rate. Retire the dupe AND advance TWO stale boundaries (FastDrain), so
    // the delivery-latency backlog converges at ~2x the dupe rate — single-digit seconds instead of
    // ~35 s. Gated on `sustained_over_rate` so a healthy 60.00 card (and a non-over-rate deficit)
    // is UNAFFECTED (below), and on `enough_unique_to_hold_target` so a genuinely starved OR frozen
    // source still emits the copy (the panic floor). Only dupes take this path — no unique dropped.
    if sustained_over_rate
        && enough_unique_to_hold_target
        && lag_intervals > RETIRE_MAX_LAG_INTERVALS
    {
        return ShedAction::FastDrain;
    }
    ShedAction::Emit { copy: true }
}

/// (#1167) Should this poll RECLAIM a corrupted-induced slot — convert a slot-skipping over-rate
/// shed into a copy emit of the nearest good frame? True iff a make-up is owed
/// (`corrupted_makeup_deficit > 0`) AND the base [`ShedAction`] would advance the boundary while
/// emitting NOTHING for a slot that a captured frame IS available to fill: [`ShedAction::Retire`]
/// (a stale-boundary dupe retirement) or [`ShedAction::Drain`] (a queue-depth over-rate drop).
///
/// Deliberately NOT the other actions: [`ShedAction::Emit`] already fills the slot;
/// [`ShedAction::Defer`] HOLDS the boundary so the next unique still fills it (no slot lost);
/// [`ShedAction::BlindShed`] is a between-boundaries drop (no slot to fill); and
/// [`ShedAction::FastDrain`] is the deep-backlog accelerated convergence (issue-1145 v2.1) — a
/// corruption make-up there would fight the backlog drain, so a deep backlog converges first and
/// the deficit is reclaimed once steady Retire/Drain resume. Pure — the whole make-up policy is
/// Tier-0 testable off hardware.
pub fn corrupted_makeup_reclaims(action: ShedAction, corrupted_makeup_deficit: u64) -> bool {
    corrupted_makeup_deficit > 0 && matches!(action, ShedAction::Retire | ShedAction::Drain)
}
