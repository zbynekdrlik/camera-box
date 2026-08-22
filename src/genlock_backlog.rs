//! #859 — the genlock FIFO's BACKLOG-STORM threshold, made latency-relative.
//!
//! `obs-source.c`'s backlog-relock branch fires when
//! `async_frames.num > GENLOCK_QDEPTH_RELOCK && due > 0`, re-locking to the newest due frame and
//! erasing every jumped frame into `genlock_dropped_due`. `GENLOCK_QDEPTH_RELOCK` is the bare
//! constant `6`, and the comment above it states the assumption it was calibrated on verbatim:
//!
//! > depth > GENLOCK_QDEPTH_RELOCK — steady depth is ~1-2 at any skew, the boundary paces arrivals
//!
//! That assumption holds for a source configured at a SHALLOW latency (the whole strih box runs
//! 3–55 ms, and the imag contract is 3 ms). It is FALSE for a source configured DEEP: the held
//! latency is `wall_now - reserve_ms`, so a source pinned at `latency_ms = 923` (the value #856's
//! A/V controller must set on the stream box's `NDI 2ME PGM` to align against the mbc's 1 s
//! mastering) has a STEADY depth of ~28 frames. `28 > 6` is permanently true, so the FIFO believes
//! it is in backlog on EVERY tick.
//!
//! Live evidence (stream box, `genlock-fifo audit 'NDI 2ME PGM'`, 2026-07-29):
//!
//! ```text
//! depth=29 peak=31 cap=59 latency_ms=923
//! relocks=2793427   (+1 per frame — this is #796's "useless as a health signal")
//! holds=4385 -> 4386, dropped_due=13938 -> 13939   (+1 each over the same 120 s)
//! ```
//!
//! Most ticks the relock is harmless (`due == 1` ⇒ `release = 1` ⇒ nothing erased), but whenever
//! arrival jitter makes `due == 2` the branch erases one frame (`dropped_due`) and the next tick
//! repeats the last frame (`holds`) — the paired duplicate/skip the #859 gate run measured in the
//! recording: +59 duplicates and +57 skips injected by the strih→stream hop, 58 of 61 duplicates
//! within 1–3 frames of their partner skip. The cam→strih leg, whose sources all sit below the
//! bare `6`, carried 2 duplicates in 9626 frames and `holds=0` on every source.
//!
//! The FIX is to make the threshold latency-relative, exactly as
//! `genlock_source_drop_cap()` in the same file already is (it reports `cap=59` for this source =
//! `latency_frames + 4`). A queue is only in BACKLOG when it exceeds the depth its OWN configured
//! latency implies, plus the same margin the constant always encoded.
//!
//! This does NOT relax any gate: a genuine backlog storm on a deep source is still caught, just at
//! the depth that is genuinely anomalous FOR THAT SOURCE. And it is a no-op for every shallow
//! source on the rig — see `sub_half_frame_latency_threshold_is_byte_identical_to_the_bare_constant_859`.
//!
//! Pure + crate-root (not under `src/probe/`) so it is Tier-0 verifiable locally — the
//! `src/reannounce.rs` / `src/colour_scale.rs` pattern. `src/probe/genlock.rs`'s
//! `ReleaseCadence::QDEPTH_RELOCK` and `obs-source.c`'s `GENLOCK_QDEPTH_RELOCK` both derive from
//! here and must stay in lock-step.

/// The margin above a source's implied steady depth before its queue counts as a backlog storm.
///
/// This is the ORIGINAL `GENLOCK_QDEPTH_RELOCK` value, unchanged — under the old code it was the
/// whole threshold because the implied steady depth was assumed to be ~1-2 and simply ignored.
pub const QDEPTH_RELOCK_MARGIN: u64 = 6;

/// The steady-state FIFO depth implied by a source's configured genlock latency, in frames,
/// rounded to nearest.
///
/// `fps_num`/`fps_den` MUST be the **source** frame rate, not the canvas rate. The quantity being
/// bounded is `async_frames.num`, which counts frames as the SOURCE delivered them — a 60 fps
/// source feeding a 30 fps canvas queues two entries per canvas interval. The two rates coincide
/// for the 30-into-30 hop this ticket is about (`NDI 2ME PGM`), but diverge for every 60-into-30
/// cambox input on strih, where using the canvas rate would under-estimate the implied depth by
/// exactly the source multiple. The C caller has this available as
/// `canvas_rate * genlock_effective_source_multiple(source, interval)`.
///
/// Mirrors the rounding `genlock_source_drop_cap()` already uses for the drop cap
/// (`(latency_ms * fps + 500) / 1000`), so the two latency-derived quantities in the FIFO agree.
/// A zero/degenerate frame rate yields 0 (no implied depth) rather than dividing by zero.
pub fn steady_depth_frames(latency_ms: u32, fps_num: u32, fps_den: u32) -> u64 {
    if fps_num == 0 || fps_den == 0 {
        return 0;
    }
    let den = 1000u64.saturating_mul(fps_den as u64);
    let num = (latency_ms as u64)
        .saturating_mul(fps_num as u64)
        .saturating_add(500u64.saturating_mul(fps_den as u64));
    num / den
}

/// The backlog-relock threshold for a source: a queue depth STRICTLY GREATER than this is a
/// backlog storm. Callers keep the `depth > threshold` comparison shape they already had.
///
/// #940 piece 2 — the MARGIN is now scaled by `source_multiple`: the source's own integer
/// rate multiple over the canvas (1 for a 1:1 30-into-30 hop like the stream box's
/// `NDI 2ME PGM`; 2 for a 60-into-30 camera ingest). A 60-into-30 source queues an ARRIVAL
/// SURPLUS of 2 frames per canvas render tick, plus the measured cam→strih arrival jitter
/// (~8 ms, docs/genlock-latency-floor-rationale, issue #272) that bunches those arrivals
/// 3-4 deep transiently — the bare (unscaled) margin is exceeded on ~every tick even at the
/// rig's shallow per-source latencies, firing the backlog-relock branch PERMANENTLY
/// (~35-70 relocks per 5-minute window measured live on `NDI cam1`/`NDI cam2`, issue #940).
/// Scaling the margin by the source multiple absorbs that structural arrival surplus
/// without touching the depth a genuinely deep-latency source implies
/// ([`steady_depth_frames`] itself is untouched).
///
/// `source_multiple = 1` is BYTE-IDENTICAL to the pre-#940 threshold for every 30-into-30
/// source on the rig (`QDEPTH_RELOCK_MARGIN * 1 == QDEPTH_RELOCK_MARGIN`) — see
/// `arrival_surplus_margin_is_a_no_op_for_a_1to1_source_940`. `source_multiple` is an
/// EXPLICIT caller-supplied value (never inferred from `fps_num`/`fps_den` here) because the
/// caller already has to measure/track it separately for the STEADY depth's own
/// SOURCE-rate requirement (see this module's `steady_depth_frames` doc) — the C caller has
/// `genlock_effective_source_multiple(source, interval)`; the Rust caller has
/// `ReleaseCadence::effective_source_multiple`. A degenerate `source_multiple` of `0`
/// (should never happen — callers already `.max(1)` their own measured value) is floored to
/// `1` here too, so it can never silently zero out the margin.
///
/// Mirror of the C `genlock_backlog_relock_qdepth()` (obs-source.c) — keep both in lock-step.
pub fn backlog_relock_threshold(
    latency_ms: u32,
    fps_num: u32,
    fps_den: u32,
    source_multiple: u32,
) -> u64 {
    let margin = QDEPTH_RELOCK_MARGIN.saturating_mul(source_multiple.max(1) as u64);
    steady_depth_frames(latency_ms, fps_num, fps_den).saturating_add(margin)
}

/// The source frame interval (ns) implied by a window of the front queued frame stamps: the
/// **MINIMUM** strictly-increasing consecutive delta.
///
/// The measured interval feeds the source-rate multiple `n = round(canvas_interval / interval)`
/// that [`backlog_relock_threshold`] (and the settle-back drain / release cadence) budget the
/// FIFO depth at, so mis-reading it directly mis-derives the threshold.
///
/// #1042 — every source stamps on the monotonic, evenly-spaced DanteSync grid, so the TRUE frame
/// interval is the SMALLEST gap between adjacent stamps: a duplicate or a dropped/decimated frame
/// only ever ENLARGES a gap, never shrinks it below the true period. Taking the FIRST increasing
/// pair (pre-#1042) over-stated the interval whenever that first pair straddled a dropped frame
/// (two source intervals), under-stating the multiple and collapsing the backlog-relock threshold
/// on a genuine 60-into-30 source (stream box's `Zaloha kamera`, ~1/sec spurious relocks — the
/// issue-796 health-signal complaint). The minimum is byte-identical to the first pair on any
/// clean grid-stamped window and only differs — strictly more correctly — when the window carries
/// a leading/embedded gap. This generalises the issue-741/issue-707 B2 "scan past a degenerate
/// front pair" intent from "skip a leading duplicate" to "take the true minimum period".
///
/// `None` when there is no strictly-increasing pair in the window (fewer than two usable stamps,
/// or an all-duplicate/non-monotonic window) — the caller then bridges with its sticky latch and
/// never fabricates a multiple.
///
/// Mirror of the loop in the C `genlock_measure_source_multiple` (obs-source.c) and
/// `src/probe/genlock.rs` `ReleaseCadence::measure_source_multiple` — keep all three in lock-step.
pub fn source_interval_from_stamps(stamps: &[u64]) -> Option<u64> {
    let mut min_delta: Option<u64> = None;
    for w in stamps.windows(2) {
        if w[1] > w[0] {
            let d = w[1] - w[0];
            min_delta = Some(min_delta.map_or(d, |m| m.min(d)));
        }
    }
    min_delta
}

/// #859 follow-up — the SLEW-LIMITED SETTLE-BACK DRAIN.
///
/// The `backlog_relock_threshold` fix above stopped the backlog-relock branch firing on
/// EVERY tick in steady state (`relocks` went from +1/frame to 1 in 125304 frames, live). But
/// that branch was ALSO the FIFO's only mechanism for shedding excess queue depth after a
/// genlock latency SETPOINT INCREASE — with it gated off in steady state, the plain N==1
/// release path (`release = 1` every tick) holds depth CONSTANT forever: consuming exactly one
/// frame per tick against an inflow of exactly one never falls behind, but never catches up
/// either. Measured live: a +34 ms setpoint step produced +134 ms of ACTUAL delay that held
/// stable across 6 consecutive samples 20+ minutes apart — a parked overshoot, not a decaying
/// transient.
///
/// This is a bounded, ADDITIONAL path alongside the unchanged backlog-relock branch: at most
/// once every [`DRAIN_MIN_TICK_INTERVAL`] ticks, while the queue sits more than
/// [`DRAIN_HYSTERESIS_FRAMES`] above [`drain_target_frames`], shed exactly ONE extra frame.
/// The rate is bounded by construction, so it can never reproduce the every-tick paired
/// duplicate/skip storm the old (removed) per-tick trim caused.
///
/// Mirrors: `vendor/obs-studio/libobs/obs-source.c` `genlock_should_drain_one()` /
/// `genlock_ticks_since_drain` and `src/probe/genlock.rs`
/// `ReleaseCadence::should_drain_one` / `ticks_since_last_drain` — keep all three in
/// lock-step.
pub const DRAIN_HYSTERESIS_FRAMES: u64 = 2;

/// #859 — minimum render ticks between two drain events. Bounds the drain rate to at most one
/// frame per this many ticks, which is what makes it structurally incapable of producing the
/// per-tick burst the disabled backlog-relock branch used to cause as a side effect.
pub const DRAIN_MIN_TICK_INTERVAL: u64 = 30;

/// #998 — the settle-back drain's own target, deliberately SEPARATE from
/// [`steady_depth_frames`] even though the arithmetic is closely related.
///
/// The ts-align hold's natural steady depth is `ceil(latency/interval) + 1..+2` (plus arrival
/// jitter) — a value that sits strictly ABOVE the floor of `latency/interval`. Reusing
/// `steady_depth_frames`'s ROUND-to-nearest here (as the drain target did before #998) picks the
/// WRONG side of that natural depth whenever `frac(latency_ms/interval) < 0.5`: round == floor,
/// so the target undershoots the hold's true steady depth by exactly one frame. `depth > target +
/// DRAIN_HYSTERESIS_FRAMES` then holds PERMANENTLY even though the queue sits at its own correct
/// depth — the drain fires every [`DRAIN_MIN_TICK_INTERVAL`] ticks, sheds a frame
/// (`dropped_due`), the boundary re-anchors low, and the very next tick's hold regains it via a
/// `late_hold` — one duplicated + one skipped program frame every ~[`DRAIN_MIN_TICK_INTERVAL`]
/// ticks, forever, on any source whose configured latency happens to land below-half-frac. Live
/// evidence (stream box `NDI 2ME PGM`): +152 `dropped_due`/+151 `late_holds` per ~355s run at
/// latency_ms=941 (frac .23), +161/+162 at 915 (frac .45); +0 at 856/891/930 (frac .68/.73/.90,
/// where round == ceil already).
///
/// The fix is CEIL, not round: an upper bound of the hold's natural depth can never sit BELOW
/// it, so the drain never mistakes the hold's own steady state for backlog. At
/// `frac(latency_ms/interval) >= 0.5`, ceil == round, so this is byte-identical to the pre-#998
/// drain target there — every clean-run source on the rig is unaffected (see
/// `drain_target_is_byte_identical_to_round_at_frac_ge_half_998`); a genuine backlog (depth far
/// above even the corrected target) still drains (see
/// `should_drain_one_still_drains_a_genuine_backlog_998`).
///
/// Degenerate `fps_num`/`fps_den` (0) return 0, matching [`steady_depth_frames`]'s own
/// convention, rather than dividing by zero.
///
/// Mirror of the C `genlock_should_drain_one()`'s target line (`(held_ns + interval - 1) /
/// interval`) in `vendor/obs-studio/libobs/obs-source.c` — keep both in lock-step.
/// `src/probe/genlock.rs ReleaseCadence::should_drain_one` delegates straight to
/// [`should_drain_one`] below with no arithmetic of its own, so it inherits this fix
/// automatically.
pub fn drain_target_frames(latency_ms: u32, fps_num: u32, fps_den: u32) -> u64 {
    if fps_num == 0 || fps_den == 0 {
        return 0;
    }
    let den = 1000u64.saturating_mul(fps_den as u64);
    let num = (latency_ms as u64).saturating_mul(fps_num as u64);
    // Ceiling division: (num + den - 1) / den. den >= 1000 here (fps_den >= 1 checked above),
    // so `den - 1` never underflows.
    num.saturating_add(den - 1) / den
}

/// Should this tick shed exactly ONE EXTRA frame to slew the queue back toward the depth its
/// own configured latency implies? `depth` is the queue depth observed THIS tick (before any
/// release); `ticks_since_last_drain` is how many ticks have passed since the last time this
/// returned `true` (the caller resets it to 0 exactly when it drains, and increments it every
/// other tick). Degenerate `fps_num`/`fps_den` (0) route through [`drain_target_frames`], which
/// returns 0 rather than dividing by zero — never panics.
///
/// #998: the target is [`drain_target_frames`] (CEIL), not [`steady_depth_frames`] (round) — see
/// that function's doc for why reusing round here limit-cycled the drain against the ts-align
/// hold whenever `frac(latency_ms/interval) < 0.5`.
pub fn should_drain_one(
    depth: u64,
    latency_ms: u32,
    fps_num: u32,
    fps_den: u32,
    ticks_since_last_drain: u64,
) -> bool {
    let target = drain_target_frames(latency_ms, fps_num, fps_den);
    depth > target.saturating_add(DRAIN_HYSTERESIS_FRAMES)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// #940 piece 3 — the STRUCTURAL fix for the deep-latency A/V-offset step. Quantizes an
/// already-computed ts-align RESERVE deadline (the Rust `genlock_present_ts_reserve()`, the
/// C `genlock_present_ts_reserve()`) to the canvas frame GRID:
///
/// `phase_pinned_deadline(raw_deadline_ns, interval_ns) = floor(raw_deadline_ns / interval_ns) * interval_ns`
///
/// WHY: the pre-#940 deadline was a raw continuous quantity (`wall_now - latency`), so
/// "which frame is due right now" depended on the EXACT sub-ms instant a lock/relock
/// happened to fire — a hidden per-lock-episode phase, re-sampled on every ACQUIRE and
/// every BACKLOG-STORM relock, measured live as a ±1–2-frame A/V-offset step between lock
/// episodes at deep latency (issue #940: -12..-131 ms band, later +39..+105 ms band with
/// the confounding dock-corrector wander eliminated). Quantizing removes the dependency on
/// exactly WHEN a relock fires: due-ness becomes a pure function of wall time.
///
/// A degenerate `interval_ns` (0 — unknown video info) returns the raw deadline unchanged
/// rather than dividing by zero, matching every other genlock helper's degenerate-interval
/// convention in this module.
///
/// Mirror of the C `genlock_phase_pin_deadline()` (obs-source.c) — keep both in lock-step.
/// See [`PHASE_PIN_HYSTERESIS_NS`] for the companion grid-comparison hysteresis this
/// quantization requires (the design's own documented risk: a frame arriving essentially
/// exactly on a grid line must not flap due/not-due from ordinary render-tick jitter on
/// this floor division).
pub fn phase_pinned_deadline(raw_deadline_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return raw_deadline_ns;
    }
    (raw_deadline_ns / interval_ns) * interval_ns
}

/// #940 piece 3 — the hysteresis SLACK added to [`phase_pinned_deadline`]'s output before
/// the `due` comparison. Sized well below one frame interval at ANY rig fps (33.3 ms @
/// 30 fps, 16.6 ms @ 60 fps — see [`phase_pin_hysteresis_is_a_small_fraction_of_any_rig_frame_interval_940`]),
/// so it can never pull in an extra frame; it exists ONLY to stop a frame captured
/// essentially exactly on a grid line from flipping due/not-due tick to tick because of
/// ordinary sub-ms render-tick slew on the floor division above — the same shape of guard
/// `genlock_present_ts`'s existing `+interval/2` boundary-churn bias already applies to the
/// (separate, frame-count) preload path, sized here instead to the sub-frame reserve-ms
/// deadline this quantizes.
///
/// Mirror of the C `GENLOCK_PHASE_PIN_HYSTERESIS_NS` (obs-source.c).
pub const PHASE_PIN_HYSTERESIS_NS: u64 = 5_000_000; // 5 ms

/// Is `frame_ts_ns` due against the phase-pinned `deadline_ns` (already
/// [`phase_pinned_deadline`]'s output), with [`PHASE_PIN_HYSTERESIS_NS`] slack? The
/// comparison is inclusive at the hysteresis boundary (`<=`), matching every other due/not-due
/// comparison in this codebase (`ts <= present_ts`).
pub fn phase_pinned_is_due(frame_ts_ns: u64, deadline_ns: u64) -> bool {
    frame_ts_ns <= deadline_ns.saturating_add(PHASE_PIN_HYSTERESIS_NS)
}

/// #1049 — the STEADY-conveyor PHASE-CONVERGENCE shed decision, N>=2 ONLY. Should this N>=2 tick
/// shed exactly ONE EXTRA frame to slew the conveyor's presentation PHASE back toward the
/// configured latency? (N==1 is gated off — see the `source_multiple < 2` early return: an N==1
/// shed does not stick and limit-cycles, and only N>=2 sources carry the ladder pathology.)
///
/// The phase-locked conveyor (`genlock_locked_next_boundary_ns`) is a pure FOLLOWER: it
/// re-anchors to the presented stamp every STEADY present and has no restoring force toward the
/// configured latency, so whatever phase it locks at ACQUIRE (or after a walk event — a GAP
/// RESYNC adopting the oldest frame's age, a sticky-N present-oldest crawl, an ACQUIRE against a
/// connect-burst queue) is carried forward INDEFINITELY. The #1003 anchor-nearest relock does not
/// fix it — it PRESERVES the phase by design (deep-source flap prevention), so each backlog storm
/// re-selects the very frame the conveyor already holds. The #859 settle-back drain cannot fix it
/// either: it bounds DEPTH with a fixed 2-frame hysteresis ([`DRAIN_HYSTERESIS_FRAMES`]), so a
/// 1-2 canvas-frame phase error sits INSIDE that hysteresis by construction — and it is eligible
/// only on the N==1 path; the N>=2 conveyor has no drain at all. Live evidence (issue 1049,
/// 2026-08-14): a stable per-camera frame-quantized A/V-offset ladder across 5 E2E runs on the
/// strih 60-into-30 ingests, each rung a 1-2 canvas-frame presentation offset locked at the last
/// OBS relaunch that never converged.
///
/// The COMPARATOR is the conveyor's own on-air age `S = wall_now_ns - locked_boundary_ns`, NOT
/// the phase anchor: at decision time `locked_boundary_ns == last_presented_ts + interval` and
/// render ticks are one interval apart, so `wall_now - boundary` == the last tick's on-air age S
/// (exact but for the ±2 ms render slew, which [`PHASE_PIN_HYSTERESIS_NS`] absorbs). The anchor is
/// the wrong quantity here — it saturates to 0 by contract on some sources
/// (`phase_anchor_from_present`) and is one tick stale — whereas the boundary is always live.
///
/// The TARGET the shed converges toward is `max(configured latency, ACHIEVABLE FLOOR)`, where the
/// floor is `wall_now_ns - newest_stamp_ns` — the age of the FRESHEST queued frame, i.e. the
/// smallest on-air age the conveyor could physically present (a frame cannot go on air before it
/// ARRIVES). This is the review-found correction (issue 1049): a reserve-only target ignored the
/// transport skew floor, so when `skew > reserve + interval/n + hysteresis` (the rig's ~20 ms skew
/// at the 3 ms prod floor) the shed fired forever at the natural phase — a shed pushed the boundary
/// below what had arrived, the next tick(s) regained it, and 30 ticks later it shed again: exactly
/// the #998 drop/regain limit cycle this design claims to be incapable of. Flooring the target at
/// the achievable phase restores the #998 invariant on the SKEW axis too.
///
/// Fires when `S` has drifted a full shed-quantum (one SOURCE interval `interval_ns / n`) plus
/// [`PHASE_PIN_HYSTERESIS_NS`] ABOVE that target, throttled to at most once every
/// [`DRAIN_MIN_TICK_INTERVAL`] ticks (the counter is SHARED with the #859 drain — reset on a shed
/// OR a drain — so no new per-source field is needed and the ≤1-extra-drop-per-30-ticks bound is
/// preserved). The caller sheds one frame with the same drop-older/present-fresher idiom the #859
/// drain uses, which re-anchors the boundary to the fresher stamp: the reduced phase STICKS (the
/// boundary is the conveyor's only phase memory), one SOURCE interval (~16.7 ms) shed per second.
///
/// `interval_ns / n` (the shed quantum) + the 5 ms hysteresis mirrors the #998 lesson applied to
/// PHASE instead of DEPTH: the threshold sits strictly above the natural steady S, so a shed's own
/// effect lands strictly inside the no-shed band (post-shed `S' = S - interval/n`, always
/// `> target >= floor` — never below the achievable phase), which is what makes it structurally
/// incapable of the #998 drop/regain limit cycle. A correctly-held DEEP source (issue 1003, Zaloha
/// 1000 ms) has `S ≈ configured` far below the threshold, and a shallow source with a large skew
/// has `S ≈ floor` far below `floor + quantum + hysteresis`, so BOTH are inert by the SAME
/// comparison that drives it — no regression, not a special case. A degenerate `interval_ns`
/// (0, unknown video info) or an UNLOCKED boundary (0, ACQUIRE) returns `false`.
///
/// Mirror of the C `genlock_phase_converge_due()` (obs-source.c) and the probe
/// `ReleaseCadence::should_converge_phase` delegator — keep all three in lock-step.
pub fn should_converge_phase(
    wall_now_ns: u64,
    locked_boundary_ns: u64,
    newest_stamp_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    source_multiple: u32,
    ticks_since_last_drain: u64,
) -> bool {
    if interval_ns == 0 || locked_boundary_ns == 0 {
        return false;
    }
    // #1049 (coordinator's live finding) — convergence is N>=2 ONLY. An N==1 source (30-into-30)
    // delivers exactly ONE frame per render tick, so a phase shed (present one frame fresher) is
    // NOT SUSTAINABLE: the queue cannot refill fast enough, the source HOLDS the very next tick and
    // regains the shed frame within the throttle window — a shed->hold->shed limit cycle (the #998
    // dup+skip signature), measured live on the stream box's deep `NDI 2ME PGM` (n=1, 990 ms, whose
    // natural grid-quantized hold ~1033 ms sits one frame above configured at frac 0.7, above the
    // reserve-based threshold). An N>=2 source delivers >=2 frames per tick, so a shed STICKS —
    // which is both why the strih 60-into-30 ladder converges and why the pathology is N>=2-only: a
    // per-camera acquire-phase ladder exists only ACROSS the multi-camera N>=2 ingests, while a
    // single N==1 source has no cross-source spread and its A/V offset is corrected by the
    // ±50 ms 2ME PGM controller. A hysteresis band cannot separate the natural hold from a real
    // error here — the natural overshoot is frac-dependent (up to ceil+2 frames) and differs by n.
    if source_multiple < 2 {
        return false;
    }
    let n = source_multiple.max(1) as u64;
    let reserve_ns = (latency_ms as u64).saturating_mul(1_000_000);
    // The achievable floor: the freshest queued frame's on-air age (a frame cannot present before
    // it arrives). saturating_sub -> a stamp AHEAD of wall reads floor 0 (falls back to reserve).
    let floor_ns = wall_now_ns.saturating_sub(newest_stamp_ns);
    let target = reserve_ns.max(floor_ns);
    let quantum = interval_ns / n;
    let threshold = target
        .saturating_add(quantum)
        .saturating_add(PHASE_PIN_HYSTERESIS_NS);
    let age = wall_now_ns.saturating_sub(locked_boundary_ns);
    age > threshold && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

// ---------------------------------------------------------------------------------------------
// #1003 — PHASE-CONTINUITY RELOCK (history-anchored selection). The Tier-0 authority; the C
// `genlock_relock_select_nearest()` / `genlock_phase_anchor_ns` (obs-source.c, obs-internal.h)
// mirror this in lock-step.
//
// WHY the #940 grid pin was not enough. The release phase of a lock episode is minted in ONE
// place — the newest-due selection at ACQUIRE / BACKLOG relock — and that selection is a
// cross-grid comparison SAMPLED AT ONE INSTANT, carrying two independent binary edges that
// ordinary slew flips between episodes:
//
//   * Edge 1 — the pin quantizes to the RECEIVER grid. [`phase_pinned_deadline`] floors to the
//     33,333,333 ns canvas interval, and the floor's step point sits at tick-phase
//     `latency_ms mod interval` (~23.0 ms at the live 923 ms knob). A relock fires on a render
//     tick; ±2 ms of tick slew near that step point moves the whole pinned cell by one interval.
//   * Edge 2 — the stamps being compared live on the SENDER's grid (33,333,300 ns in 100 ns
//     units), a 33 ns/frame beat (~3.6 ms/h) against the receiver grid, plus DanteSync inter-box
//     skew wander. [`PHASE_PIN_HYSTERESIS_NS`] is a FIXED offset edge inside that DRIFTING
//     relative phase — whether the newest stamp lands just inside or just outside it changes
//     episode to episode. No hysteresis SIZE removes this; it only moves where the coin lands.
//
// Two independent edges = up to four outcomes spanning two frames, which is exactly the measured
// −64.5 / +56..63 ms per-episode steps (issue 1003). Once locked, the cadence is a stamp-anchored
// conveyor that never re-consults the wall deadline, so whatever the relock latches, the whole
// episode holds.
//
// STRUCTURAL STATEMENT: any instant-sampled, STATELESS selection rule has an edge somewhere.
// Determinism requires the relock to INHERIT the phase from HISTORY rather than re-sample it.
// So the source tracks the steady conveyor's own on-air age (`wall_now − presented_ts`, updated
// on every STEADY / GAP-RESYNC present) and a relock presents the queued frame NEAREST that
// remembered age instead of the newest due one. Nearest-neighbour selection is CONTINUOUS: the
// selection point sits half a stamp interval from the operating point BY CONSTRUCTION, so there
// is no edge for slew or beat to flip. Depth is still corrected by whole frames (the frames
// older than the selected one are erased into `dropped_due` exactly as before) — only the PHASE
// is inherited.
//
// Deliberately NOT changed by this ticket: the `due` prefix scan and its hysteresis (they still
// QUALIFY due-ness and still trigger the backlog branch — they simply no longer SELECT), the
// reserve computation, [`backlog_relock_threshold`] (issue 859), [`drain_target_frames`]
// (issue 998) and [`BackwardStepGuard`] (issue 1009).
// ---------------------------------------------------------------------------------------------

/// The age (ns) the relock selection should target: the tracked phase anchor, FLOORED at the
/// source's configured latency.
///
/// `anchor_ns == 0` is the UNSET sentinel — it matches the C field's `bzalloc` zero-init, so a
/// source that has never presented a steady frame (cold start, post-flush, just after a
/// backward-step regime ended) falls back to the configured latency, which is the phase the
/// wall-deadline path would have produced anyway.
///
/// The FLOOR is a bound, not a preference: the conveyor always holds AT LEAST the configured
/// latency, so in the intended (deep-source) regime the anchor sits at or above it and the
/// floor is inert. It matters because [`phase_anchor_from_present`] SATURATES and nothing in
/// the types stops a degenerate or stale anchor coming back SMALLER than the hold — an anchor
/// near 0 would target the live edge and erase the whole delay line in a single relock.
///
/// SCOPE: #1003 was framed as a deep-latency fix, but the anchor is SET and meaningful on the
/// shallow (3–55 ms) fleet sources too — issue 1049 corrected an earlier claim here.
///
/// The earlier revision of this paragraph reasoned that at a shallow hold
/// `phase_anchor_from_present` "routinely saturates to 0" (the anchor reads UNSET) because the
/// sender's ceil-to-boundary bias is up to a full interval AHEAD of the receiver wall clock. That
/// was true of the pre-issue-1009 senders, but issue 1009 flipped emit stamping to the FLOOR
/// boundary (at-or-before the emit instant, never future-dated — `.claude/rules/
/// genlock-hold-collapse-diagnosis.md`), shifting every stamp ~one interval EARLIER. Post-1009 a
/// presented stamp sits in the PAST, so `wall_now - presented_ts` is comfortably positive
/// (≈ configured + phase_error) even at a 20–36 ms hold, and the anchor is SET. Consequence,
/// confirmed live (issue 1049): the #1003 anchor-nearest relock then PRESERVES whatever phase the
/// conveyor holds across every backlog storm, so a 1–2-frame acquire-phase error persists forever
/// on the strih 60-into-30 ingests — which is exactly why [`should_converge_phase`] adds a bounded
/// restoring force toward the configured latency (this module, issue 1049). The FLOOR below is
/// still a correctness bound, not a "shallow sources are unaffected" statement.
///
/// Mirror of the C `genlock_relock_target_age_ns()` (obs-source.c) — keep both in lock-step.
pub fn relock_anchor_age_ns(anchor_ns: u64, latency_ms: u32) -> u64 {
    let configured = latency_ms as u64 * 1_000_000;
    if anchor_ns > configured {
        anchor_ns
    } else {
        configured
    }
}

/// The phase anchor to remember after presenting `presented_ts_ns` at wall instant
/// `wall_now_ns` — the conveyor's own measured on-air age.
///
/// Saturating: a frame stamped AHEAD of the receiver's wall clock (the sender's deliberate
/// ceil-to-boundary future bias, issue 1009 defect B) would otherwise underflow. Saturating to
/// 0 makes such a degenerate sample read as UNSET via [`relock_anchor_age_ns`], i.e. the next
/// relock falls back to the configured latency rather than targeting a nonsense age.
///
/// Mirror of the C anchor update in `ready_async_frame()`'s STEADY / GAP present tail.
pub fn phase_anchor_from_present(wall_now_ns: u64, presented_ts_ns: u64) -> u64 {
    wall_now_ns.saturating_sub(presented_ts_ns)
}

/// #1003 — the relock selection itself. Returns the INDEX into `queue_ts` (arrival order,
/// OLDEST first) of the frame whose capture stamp is NEAREST the anchor-implied target
/// `wall_now_ns − anchor_age_ns`.
///
/// Ties resolve toward the OLDER frame (the lower index) — a strict `<` comparison. Two stamps
/// exactly equidistant from the target means the target sits precisely between them; taking the
/// older one keeps the selection monotone as the target sweeps forward, so a tie can never make
/// the choice oscillate between neighbours on successive episodes (the very failure mode this
/// function exists to remove).
///
/// The caller converts the index into the existing release shape: `release = index + 1`, so the
/// unchanged `to_drop = release - 1` erase loop retires exactly the `index` older frames into
/// `dropped_due` and presents the selected one — the same `da_erase(.,0)` + `remove_async_frame()`
/// idiom the ACQUIRE / relock / N>=2 paths already use. That is what keeps a relock's DEPTH
/// correction intact while the PHASE is inherited.
///
/// An empty slice returns 0: the C call sites are only ever reached with `async_frames.num >= 1`
/// (`ready_async_frame` guards on it and both relock branches additionally require `due > 0`),
/// so this is a defensive convention, never a live path — it can never index out of bounds.
///
/// Mirror of the C `genlock_relock_select_nearest()` (obs-source.c) — keep both in lock-step.
pub fn relock_select_nearest(queue_ts: &[u64], wall_now_ns: u64, anchor_age_ns: u64) -> usize {
    if queue_ts.is_empty() {
        return 0;
    }
    let target = wall_now_ns.saturating_sub(anchor_age_ns);
    // `abs_diff` is exactly the C mirror's `a > b ? a - b : b - a`; spelled this way
    // because clippy::manual_abs_diff rejects the explicit ternary form here.
    let dist = |ts: u64| ts.abs_diff(target);
    let mut best = 0usize;
    let mut best_d = dist(queue_ts[0]);
    for (i, &ts) in queue_ts.iter().enumerate().skip(1) {
        let d = dist(ts);
        // STRICT `<` — an equal distance keeps the already-chosen OLDER frame.
        if d < best_d {
            best = i;
            best_d = d;
        }
    }
    best
}

/// #1161 — the fail-open MARGIN (ticks) the ACQUIRE bracketing gate ([`relock_acquire_should_hold`])
/// adds on top of `ceil(reserve/interval)` before it force-acquires regardless of queue depth.
/// `ceil(reserve/interval)` is how many render ticks a from-empty queue needs to deepen the OLDEST
/// frame to the (raised) reserve; the small extra margin covers arrival jitter. Mirror of the C
/// `GENLOCK_ACQUIRE_BRACKET_FAILOPEN_TICKS` (obs-source.c).
pub const ACQUIRE_BRACKET_FAILOPEN_TICKS: u64 = 3;

/// #1161 — the STAGE-2 ACQUIRE BRACKETING GATE decision. Should this ACQUIRE tick HOLD (return
/// false, present nothing, let the queue deepen) instead of acquiring the lock now? The caller
/// gates this to N>=2 sources only (see the C `genlock_release_tick` ACQUIRE branch) and increments
/// `ticks_held` on each hold, resetting it to 0 on any non-holding tick.
///
/// WHY it exists (issue 1161). The PRIMARY frame-mover is the setter forcing a bounded re-acquire
/// on a pin RISE (it zeroes `genlock_locked_next_boundary_ns`): that alone lets the ACQUIRE branch
/// rebuild the FIFO to the raised depth and moves the presented frame off the shallow old-depth
/// toward the new reserve — it is what closes the ticket's one-canvas-frame residual. THIS gate is
/// the PRECISION half. Without it, the ACQUIRE branch acquires as soon as a frame is `due`, and
/// [`relock_select_nearest`] picks the queued frame NEAREST `wall_now − reserve`. Because the #940
/// phase-pinned deadline FLOORS the raw reserve to the receiver grid, a `due` frame is only at least
/// `reserve − PHASE_PIN_HYSTERESIS_NS` (5 ms) old — so a bare re-acquire lands up to ~5 ms BELOW the
/// raised target (and the nearest-selection could pick a slightly-younger neighbour), a SUB-frame
/// residual the downward-only [`should_converge_phase`] shed can never raise back. This gate holds
/// the acquire until the oldest queued frame has aged to the FULL reserve, so a frame AT the target
/// depth exists for the selection to land on — then the existing selection runs byte-identical (the
/// phase is re-anchored via history, never free-run).
///
/// The decision, in order:
/// * `interval_ns == 0` (degenerate video info) → NEVER hold — fail open to today's acquire.
/// * `oldest_queued_age_ns >= reserve_ns` → the queue has bracketed the target; a frame at/past the
///   reserve exists → acquire now (return false). This is why the gate is INERT at the production
///   3 ms pin: the oldest queued frame is essentially always older than 3 ms.
/// * otherwise HOLD, UNLESS the fail-open cap `ceil(reserve/interval) + ACQUIRE_BRACKET_FAILOPEN_TICKS`
///   ticks have already been spent holding — a pathological queue whose oldest frame is continually
///   replaced by a fresher one faster than it can age (an overrun-capped delay line) would otherwise
///   hold forever, a NEW permanent-freeze mode; the cap degrades it to today's acquire instead
///   (`no-timeout-band-aids.md` — this is a bounded safety valve, never the primary path: in the
///   ordinary case the oldest frame simply ages to the reserve on its own within a few ticks).
///
/// Mirror of the C `genlock_relock_acquire_should_hold()` (obs-source.c) — keep both in lock-step;
/// the C-vs-Rust parity is asserted by `tests/genlock_relock_selection_parity.rs`.
pub fn relock_acquire_should_hold(
    oldest_queued_age_ns: u64,
    reserve_ns: u64,
    interval_ns: u64,
    ticks_held: u64,
) -> bool {
    // Degenerate video info: never hold, and never divide by zero computing the cap.
    if interval_ns == 0 {
        return false;
    }
    // The queue has bracketed the target — a frame at/past the reserve exists for the selection to
    // land on. Acquire now. (Inclusive `>=`, matching every other due comparison in this module.)
    if oldest_queued_age_ns >= reserve_ns {
        return false;
    }
    // Not deep enough yet — HOLD to let the queue deepen, UNLESS the fail-open cap is reached. The
    // cap is `ceil(reserve/interval)` (ticks a from-empty queue needs to age its oldest frame to
    // the reserve) plus a small jitter margin. Rust uses `div_ceil` (clippy manual_div_ceil); the
    // C mirror (`genlock_relock_acquire_should_hold`) keeps the explicit `(a + b - 1) / b` form of
    // the SAME ceil — the parity gate runs both, and the values are identical for these unsigned
    // in-range inputs (`reserve_ns` is bounded by the ms latency clamp, no overflow).
    let cap = reserve_ns.div_ceil(interval_ns) + ACQUIRE_BRACKET_FAILOPEN_TICKS;
    ticks_held < cap
}

// ---------------------------------------------------------------------------------------------
// #1009 — the backward-clock-step guard, RE-QUALIFIED (Tier-0 mirror of the C guard in
// obs-source.c's ts-align release, the issue-147 branch).
//
// The overnight −900 ms collapse (issue 1007 forensics): the deployed guard triggers on
// `max(queued ts) > wall_now + interval` during any routine due==0 hold tick — a margin of ONE
// frame interval, single-tick — while the SENDER deliberately stamps up to one interval in the
// FUTURE (ceil-to-boundary, defect B of issue 1009). Normal operation therefore sits only
// `network delay` away from the trigger (measured excess at trigger: min 0.3 ms), so a few ms
// of sender-ahead clock skew fires it every tick, the re-anchor re-locks the issue-401 cadence
// boundary to the live edge, and NOTHING ever restores the configured hold — a permanent
// absorbing live-edge state (depth 0-1 at a 894 ms knob, offline-verified −900.35 ms A/V).
//
// The re-qualified guard (this module is the authority; the C mirrors it in lock-step):
//   * margin ≫ the sender's deliberate future bias: `max(3×interval, 250 ms)`, never one
//     interval;
//   * sustained: the condition must hold for `BACKWARD_STEP_SUSTAIN_TICKS` CONSECUTIVE due==0
//     ticks before the first re-anchor — a single-tick excursion falls through to the cadence
//     (which presents/holds normally off the locked stamp-relative boundary, so nothing
//     freezes while qualifying);
//   * self-heal: on leaving the regime the locked boundary is ZEROED, which is the existing
//     ACQUIRE state — the wall-deadline path rebuilds the queue to the configured latency
//     depth and re-locks (a bounded ~latency_ms transient, never a permanent collapse);
//   * loud: entry warns once per event (as before), a PERSISTENT regime re-warns on a bounded
//     cadence (older than BACKWARD_REGIME_WARN_AFTER_NS, at most once per
//     BACKWARD_REGIME_WARN_INTERVAL_NS), and every re-anchor tick increments a cumulative
//     `reanchor_ticks` counter the audit line / E2E gates can assert stays 0.
// ---------------------------------------------------------------------------------------------

/// The FLOOR of the backward-step trigger margin, ns (250 ms). A real NTP/PTP backward step in
/// the issue-147 evidence is hundreds of ms to seconds; sender-ahead stamp skew (the issue-1007
/// storm) measured tens of ms at the extreme. 250 ms cleanly separates the populations.
pub const BACKWARD_STEP_MIN_MARGIN_NS: u64 = 250_000_000;

/// The interval-scaled component of the trigger margin: at least 3 frame intervals, so the
/// sender's deliberate ceil-to-boundary future bias (≤1 interval) plus arrival bunching can
/// never come close, at any frame rate.
pub const BACKWARD_STEP_MARGIN_INTERVALS: u64 = 3;

/// How many CONSECUTIVE due==0 ticks the over-margin condition must persist before the guard
/// fires its first re-anchor. A one/two-tick excursion (a stamp outlier, a mid-correction
/// seam) is NOT a backward step.
pub const BACKWARD_STEP_SUSTAIN_TICKS: u32 = 3;

/// A re-anchor regime older than this is abnormal and must start re-warning (the once-per-latch
/// entry WARN alone let the overnight collapse run silent for 3+ hours).
pub const BACKWARD_REGIME_WARN_AFTER_NS: u64 = 2_000_000_000;

/// Minimum spacing between bounded-cadence regime warnings (never per-tick log spam).
pub const BACKWARD_REGIME_WARN_INTERVAL_NS: u64 = 5_000_000_000;

/// The backward-step trigger margin for a given render interval: `max(3×interval, 250 ms)`.
///
/// Mirror of the C `genlock_backward_step_margin_ns()` (obs-source.c) — keep in lock-step.
pub fn backward_step_margin_ns(interval_ns: u64) -> u64 {
    (BACKWARD_STEP_MARGIN_INTERVALS * interval_ns).max(BACKWARD_STEP_MIN_MARGIN_NS)
}

/// What the guard decided for this tick. The caller (the C ts-align release / the test FIFO
/// sim) applies the side effects; the guard owns only the qualification state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackwardStepAction {
    /// No over-margin condition and no regime active — a normal tick, fall through to the
    /// cadence untouched.
    None,
    /// A qualification run is in progress — fall through to the cadence (NO re-anchor,
    /// NO self-heal this tick; the cadence's locked boundary keeps presenting/holding).
    /// Out of a regime this is the ENTRY qualification (over-margin ticks not yet
    /// sustained); inside a regime it is the EXIT qualification (clear ticks not yet
    /// sustained — a single clear tick inside a flap must not end the regime, because
    /// every exit costs a bounded ~latency_ms re-ACQUIRE hold; review hardening).
    Pending,
    /// A qualified, sustained backward step — re-anchor THIS tick: present the oldest queued
    /// frame and re-lock the cadence boundary to it (+interval). `entry` = first tick of this
    /// event (count it + entry WARN); `warn` = the bounded-cadence persistent-regime WARN is
    /// due this tick.
    Reanchor { entry: bool, warn: bool },
    /// The regime just ENDED — self-heal: the caller must ZERO the locked cadence boundary
    /// (and clear the sticky-N latch, like every other source-timeline seam) so the release
    /// re-ACQUIREs the configured hold from the wall deadline (queue rebuilds to latency
    /// depth; bounded transient), then fall through to the cadence.
    SelfHeal,
}

/// The per-source backward-step qualification state (mirrors the C per-source fields on
/// `obs_source`: `genlock_in_backward_step`, `genlock_backward_pending_ticks`,
/// `genlock_backward_regime_start_ns`, `genlock_backward_last_warn_ns`,
/// `genlock_backward_regime_ticks`). One instance per genlock source, ticked only on the
/// ts-align release path. This module is the Tier-0 authority; keep the C in lock-step.
#[derive(Debug, Default)]
pub struct BackwardStepGuard {
    in_step: bool,
    pending_ticks: u32,
    regime_start_ns: u64,
    last_warn_ns: u64,
    reanchor_ticks: u64,
}

impl BackwardStepGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is a re-anchor regime currently latched (the C `genlock_in_backward_step`)?
    pub fn in_step(&self) -> bool {
        self.in_step
    }

    /// Cumulative re-anchor ticks across all regimes (the C `genlock_backward_regime_ticks`
    /// audit counter). Healthy operation keeps this at 0.
    pub fn reanchor_ticks(&self) -> u64 {
        self.reanchor_ticks
    }

    /// Evaluate one due==0 hold tick. `max_queued_ts_ns` is the NEWEST queued stamp (the C
    /// scans for the true max — arrival order is non-monotonic across a step seam),
    /// `wall_now_ns` the receiver wall clock at the deadline read, `log_now_ns` the monotonic
    /// clock used for the warn cadence (the C `now_ns` from os_gettime_ns).
    pub fn tick_due0(
        &mut self,
        max_queued_ts_ns: u64,
        wall_now_ns: u64,
        interval_ns: u64,
        log_now_ns: u64,
    ) -> BackwardStepAction {
        // #1009 re-qualified trigger: margin >> the sender's deliberate ceil-to-boundary
        // future bias (max(3×interval, 250 ms)), so plain sender-ahead stamp skew can never
        // come close — only a REAL local backward step exceeds it.
        let head_future =
            max_queued_ts_ns > wall_now_ns.saturating_add(backward_step_margin_ns(interval_ns));
        if head_future {
            if self.in_step {
                // An over-margin tick breaks any exit-clear run (pending_ticks doubles as
                // the consecutive-CLEAR counter while the regime is active).
                self.pending_ticks = 0;
                // Regime continues: re-anchor, and re-warn on a bounded cadence once the
                // regime is abnormal-old (> BACKWARD_REGIME_WARN_AFTER_NS), at most one warn
                // per BACKWARD_REGIME_WARN_INTERVAL_NS — never per-tick, never
                // once-per-latch-only. The entry WARN does not pre-arm the spacing: the
                // FIRST cadence warn fires the moment the regime crosses the age threshold
                // (last_warn_ns is 0 until a cadence warn actually fires).
                self.reanchor_ticks += 1;
                let warn = log_now_ns.saturating_sub(self.regime_start_ns)
                    > BACKWARD_REGIME_WARN_AFTER_NS
                    && log_now_ns.saturating_sub(self.last_warn_ns)
                        >= BACKWARD_REGIME_WARN_INTERVAL_NS;
                if warn {
                    self.last_warn_ns = log_now_ns;
                }
                return BackwardStepAction::Reanchor { entry: false, warn };
            }
            // Not yet in a regime: the condition must SUSTAIN across consecutive due==0
            // ticks before the first re-anchor — a 1-2 tick excursion falls through to the
            // cadence (which presents/holds normally off its locked boundary).
            self.pending_ticks = self.pending_ticks.saturating_add(1);
            if self.pending_ticks >= BACKWARD_STEP_SUSTAIN_TICKS {
                self.in_step = true;
                // From here pending_ticks counts consecutive CLEAR ticks (exit
                // qualification) — start the run empty.
                self.pending_ticks = 0;
                self.regime_start_ns = log_now_ns;
                self.last_warn_ns = 0;
                self.reanchor_ticks += 1;
                return BackwardStepAction::Reanchor {
                    entry: true,
                    warn: false,
                };
            }
            return BackwardStepAction::Pending;
        }
        if self.in_step {
            // Review hardening: the EXIT is qualified like the entry — the condition must
            // stay clear for BACKWARD_STEP_SUSTAIN_TICKS consecutive due==0 ticks before
            // the regime ends. A condition flapping around the margin (head_future
            // sawtooths in interval quanta at a crossing) must not exit-and-re-enter per
            // flap: every exit costs a bounded ~latency_ms re-ACQUIRE hold.
            self.pending_ticks = self.pending_ticks.saturating_add(1);
            if self.pending_ticks >= BACKWARD_STEP_SUSTAIN_TICKS {
                self.pending_ticks = 0;
                // #1009 SELF-HEAL: the qualified regime ended — the caller must zero the
                // locked cadence boundary so the release re-ACQUIREs the configured hold
                // from the wall deadline (the collapse must never be an absorbing state).
                self.in_step = false;
                return BackwardStepAction::SelfHeal;
            }
            return BackwardStepAction::Pending;
        }
        self.pending_ticks = 0;
        BackwardStepAction::None
    }

    /// A due>0 tick (frames matured against the wall deadline) — the condition is structurally
    /// absent this tick, and the exit is IMMEDIATE (no sustain needed): frames aged past the
    /// reserve against the wall deadline is structural proof the receiver clock genuinely
    /// caught up — a marginal flap at the live edge only ever produces young frames. Ends any
    /// active regime with the same SELF-HEAL contract as [`Self::tick_due0`]'s clear path.
    pub fn tick_due_positive(&mut self) -> BackwardStepAction {
        self.pending_ticks = 0;
        if self.in_step {
            self.in_step = false;
            return BackwardStepAction::SelfHeal;
        }
        BackwardStepAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this ticket is about: the stream box's `NDI 2ME PGM` at the latency #856's
    /// A/V controller sets. Its OBSERVED steady depth is 29 with peak 31 — both must sit BELOW
    /// the threshold, or the FIFO relocks every tick and sheds a frame on every jitter excursion.
    #[test]
    fn deep_latency_source_does_not_read_as_backlog_at_its_own_steady_depth_859() {
        // Live values from the stream box audit: latency_ms=923 on a 30.000 fps canvas.
        // 'NDI 2ME PGM' is a 30-into-30 (1:1) hop, so source_multiple=1.
        let t = backlog_relock_threshold(923, 30, 1, 1);
        assert!(
            t >= 29,
            "#859: observed steady depth 29 must not exceed the backlog threshold ({t})"
        );
        assert!(
            t >= 31,
            "#859: observed PEAK depth 31 must not exceed the backlog threshold either, or a \
             routine jitter excursion still triggers the storm branch ({t})"
        );
        // 923 ms @ 30 fps -> 28 implied frames, + the original margin 6 = 34.
        assert_eq!(t, 34, "923ms @30fps => 28 implied frames + margin 6");
    }

    /// The no-regression property that makes this safe to ship to the whole rig.
    ///
    /// NOTE the earlier revision of this test asserted something arithmetically FALSE — that
    /// 16 ms at 60 fps "implies <0.5 frames" and must therefore stay at the bare 6. It does not:
    /// `16 * 60 / 1000 = 0.96` frames, which rounds to 1 and gives 7. The claim was wrong, not the
    /// implementation, so it is corrected here rather than the code being bent to satisfy it.
    ///
    /// The property that is actually true, and actually load-bearing, is narrower: a source whose
    /// configured latency implies LESS THAN HALF a frame keeps today's threshold exactly. That
    /// covers the two values the rig genuinely depends on — the 3 ms global default and the 3 ms
    /// imag latency contract — at both canvas rates.
    #[test]
    fn sub_half_frame_latency_threshold_is_byte_identical_to_the_bare_constant_859() {
        // The 3 ms global default / imag contract — the load-bearing case, unchanged at any
        // rate. #940 piece 2: source_multiple=1 isolates the STEADY-DEPTH-vs-fps behaviour
        // this test is actually about, independent of the NEW margin-scaling piece 2 adds
        // (see arrival_surplus_aware_margin_doubles_for_a_60_into_30_source_940 for that).
        assert_eq!(backlog_relock_threshold(3, 30, 1, 1), QDEPTH_RELOCK_MARGIN);
        assert_eq!(backlog_relock_threshold(3, 60, 1, 1), QDEPTH_RELOCK_MARGIN);

        // Anything implying <0.5 frames rounds to 0 implied depth => the bare constant.
        for (latency_ms, num) in [(3u32, 30u32), (8, 30), (16, 30), (3, 60), (8, 60)] {
            assert_eq!(
                backlog_relock_threshold(latency_ms, num, 1, 1),
                QDEPTH_RELOCK_MARGIN,
                "{latency_ms}ms @{num}fps implies <0.5 frames — threshold must stay the bare 6"
            );
        }
    }

    /// The flip side, stated honestly rather than hidden: a source configured deep ENOUGH to imply
    /// a whole frame or more DOES get a slightly higher threshold, and that is the intended
    /// behaviour — the threshold tracks the depth the source itself was configured to hold.
    ///
    /// These are the strih box's real per-source latencies on its 30 fps canvas. All of them
    /// report `holds=0` today, so nothing regresses; they simply stop counting their own
    /// configured buffer as a backlog.
    #[test]
    fn deeper_shallow_sources_move_with_their_own_configured_depth_859() {
        // All 1:1 30-into-30 (source_multiple=1) — the strih PGM/PVW class, not a camera
        // ingest.
        assert_eq!(
            backlog_relock_threshold(21, 30, 1, 1),
            7,
            "21ms -> 1 frame + 6"
        );
        assert_eq!(
            backlog_relock_threshold(26, 30, 1, 1),
            7,
            "26ms -> 1 frame + 6"
        );
        assert_eq!(
            backlog_relock_threshold(55, 30, 1, 1),
            8,
            "55ms -> 2 frames + 6"
        );
    }

    #[test]
    fn steady_depth_rounds_to_nearest_like_the_drop_cap() {
        assert_eq!(steady_depth_frames(923, 30, 1), 28); // 27.69 -> 28
        assert_eq!(steady_depth_frames(923, 60, 1), 55); // 55.38 -> 55, matches cap=59 (55+4)
        assert_eq!(steady_depth_frames(55, 30, 1), 2); // 1.65 -> 2
        assert_eq!(steady_depth_frames(16, 30, 1), 0); // 0.48 -> 0
        assert_eq!(steady_depth_frames(17, 30, 1), 1); // 0.51 -> 1
    }

    /// The depth being bounded counts SOURCE frames, so a 60 fps cambox input into strih's 30 fps
    /// canvas implies twice the depth the canvas rate would suggest. Passing the canvas rate here
    /// would under-estimate by exactly the source multiple and leave those inputs closer to the
    /// backlog branch than intended.
    #[test]
    fn implied_depth_follows_the_source_rate_not_the_canvas_rate_859() {
        // strih 'NDI cam3': 26 ms configured, 60 fps source, 30 fps canvas.
        assert_eq!(
            steady_depth_frames(26, 60, 1),
            2,
            "26ms of a 60fps source = 1.56 -> 2 frames"
        );
        assert_eq!(
            steady_depth_frames(26, 30, 1),
            1,
            "the canvas rate would say 1 — too low"
        );
        // #940 piece 2: source_multiple=1 isolates the pre-#940 steady-depth-vs-fps
        // behaviour this test is about (the NEW margin scaling for a genuine 60-into-30
        // source is arrival_surplus_aware_margin_doubles_for_a_60_into_30_source_940).
        assert_eq!(backlog_relock_threshold(26, 60, 1, 1), 8);

        // The hop this ticket is about is 30-into-30, so the two rates coincide there.
        assert_eq!(
            steady_depth_frames(923, 30, 1),
            steady_depth_frames(923, 30, 1),
            "NDI 2ME PGM is a 30fps source on a 30fps canvas — no multiple to apply"
        );
    }

    #[test]
    fn degenerate_frame_rate_implies_no_depth_rather_than_dividing_by_zero() {
        assert_eq!(steady_depth_frames(923, 0, 1), 0);
        assert_eq!(steady_depth_frames(923, 30, 0), 0);
        assert_eq!(backlog_relock_threshold(923, 0, 0, 1), QDEPTH_RELOCK_MARGIN);
    }

    /// A genuine storm on a deep source is STILL caught — the bar does not move, it moves WITH the
    /// source. #401's own scenario (a stall's burst landing at once) is far above the threshold.
    #[test]
    fn a_real_backlog_storm_on_a_deep_source_is_still_caught_859() {
        let t = backlog_relock_threshold(923, 30, 1, 1);
        // A one-second stall on a 30 fps source lands ~30 frames ON TOP of the steady 28.
        let burst_depth = 28 + 30;
        assert!(
            burst_depth as u64 > t,
            "a stall's burst (depth {burst_depth}) must still exceed the threshold ({t})"
        );
    }

    // ---- #940 piece 2: arrival-surplus-aware relock threshold ----------------------------

    /// The no-op guarantee that makes piece 2 safe to ship to the whole rig: for every
    /// 30-into-30 source (source_multiple=1), the margin-scaled threshold is BYTE-IDENTICAL
    /// to the bare pre-#940 formula (steady_depth_frames + the unscaled QDEPTH_RELOCK_MARGIN).
    #[test]
    fn arrival_surplus_margin_is_a_no_op_for_a_1to1_source_940() {
        for (latency_ms, num, den) in [(923u32, 30u32, 1u32), (3, 30, 1), (26, 30, 1), (55, 30, 1)]
        {
            assert_eq!(
                backlog_relock_threshold(latency_ms, num, den, 1),
                steady_depth_frames(latency_ms, num, den) + QDEPTH_RELOCK_MARGIN,
                "{latency_ms}ms @{num}/{den}fps, source_multiple=1 must equal the bare \
                 pre-#940 formula exactly"
            );
        }
    }

    /// The mechanism piece 2 fixes: a 60-into-30 camera ingest churns permanently at its
    /// shallow per-source latency because the arrival SURPLUS (2 frames/tick) plus jitter
    /// bunches transiently past the bare margin. Doubling the margin for a measured
    /// source_multiple=2 absorbs the observed bunching (peaks 5-8, live #940 audit) without
    /// touching the depth the latency itself implies.
    #[test]
    fn arrival_surplus_aware_margin_doubles_for_a_60_into_30_source_940() {
        // NDI cam2: 4ms configured, 60fps source -- steady_depth_frames(4,60,1) == 0 (implies
        // <0.5 frame), so the pre-#940 threshold was the bare 6, permanently exceeded by the
        // observed 3-4-deep bunching (issue #940 audit-log correlation).
        let steady = steady_depth_frames(4, 60, 1);
        assert_eq!(
            steady, 0,
            "4ms @60fps implies <0.5 frame (sanity: this is the live #940 cam2 case)"
        );
        assert_eq!(
            backlog_relock_threshold(4, 60, 1, 2),
            steady + QDEPTH_RELOCK_MARGIN * 2,
            "60-into-30 (source_multiple=2) must scale the MARGIN, not just the depth"
        );
        assert_eq!(backlog_relock_threshold(4, 60, 1, 2), 12);

        // The stream box's 'NDI 2ME PGM' (30-into-30, this ticket's own #859 regression case)
        // must NOT move: it is source_multiple=1, so its threshold stays exactly 34.
        assert_eq!(backlog_relock_threshold(923, 30, 1, 1), 34);
    }

    /// A degenerate `source_multiple` of `0` (should never happen -- callers already
    /// `.max(1)` their own measured value) must behave as multiple=1, never as a zero
    /// margin -- the same degenerate-input discipline every other function in this module
    /// applies.
    #[test]
    fn arrival_surplus_margin_source_multiple_zero_behaves_as_one_940() {
        assert_eq!(
            backlog_relock_threshold(3, 30, 1, 0),
            backlog_relock_threshold(3, 30, 1, 1)
        );
        assert_eq!(
            backlog_relock_threshold(4, 60, 1, 0),
            backlog_relock_threshold(4, 60, 1, 1)
        );
    }

    /// #1042 — the source interval is the MINIMUM adjacent grid delta over the scan window, not
    /// the FIRST strictly-increasing pair. Every source stamps on the monotonic, evenly-spaced
    /// DanteSync grid, so the true frame interval is the SMALLEST gap between adjacent stamps — a
    /// duplicate or a dropped/decimated frame only ever ENLARGES a gap, never shrinks it below the
    /// true period. Live #1042: on the stream box's 60fps `Zaloha kamera` the FIRST increasing
    /// front pair intermittently straddled a dropped frame (33.3ms → read as 30fps, n=1), which
    /// collapsed the backlog-relock threshold below the source's real ~59-deep queue and fired the
    /// branch spuriously (~1/sec — the #796 relock-log-as-health-signal pollution).
    #[test]
    fn source_interval_is_the_min_grid_delta_not_the_first_1042() {
        let g = 16_666_666u64; // 60fps grid interval
        let t = 4_000_000_000u64;
        // A front-scan window whose FIRST strictly-increasing pair straddles a dropped frame
        // (2 source intervals), followed by true 1-interval pairs: first pair = 2g, min = g.
        let with_leading_gap = [t, t + 2 * g, t + 3 * g, t + 4 * g, t + 5 * g, t + 6 * g];
        assert_eq!(
            source_interval_from_stamps(&with_leading_gap),
            Some(g),
            "the true source interval is the MINIMUM adjacent grid delta, not the first \
             increasing pair (which straddles the leading gap)"
        );

        // A clean 60fps window is unchanged (first pair already IS the minimum).
        let clean = [t, t + g, t + 2 * g, t + 3 * g];
        assert_eq!(source_interval_from_stamps(&clean), Some(g));

        // A genuine 30fps (30-into-30) window still reads its own interval — never fabricates a
        // smaller one. All deltas are the canvas interval; min == that interval.
        let canvas = 33_333_333u64;
        let one_to_one = [t, t + canvas, t + 2 * canvas];
        assert_eq!(source_interval_from_stamps(&one_to_one), Some(canvas));

        // No strictly-increasing pair anywhere → inconclusive (the caller bridges with its latch).
        assert_eq!(source_interval_from_stamps(&[t, t, t]), None);
        assert_eq!(source_interval_from_stamps(&[t]), None);
    }

    /// #1042 — the observable consequence: a 60-into-30 source held 1000ms whose front scan window
    /// has a dropped-frame gap must still derive `n = 2` and a threshold ABOVE its ~59-63 held
    /// depth, so the backlog-relock branch stays quiet in steady state (the ticket's acceptance).
    #[test]
    fn a_60_into_30_source_with_a_front_gap_keeps_its_threshold_above_the_held_depth_1042() {
        let g = 16_666_666u64; // 60fps grid
        let canvas = 33_333_333u64; // 30fps canvas interval
        let t = 4_000_000_000u64;
        let with_leading_gap = [t, t + 2 * g, t + 3 * g, t + 4 * g, t + 5 * g, t + 6 * g];
        let si = source_interval_from_stamps(&with_leading_gap).expect("a monotonic window");
        // The source-rate multiple the caller derives from that interval (round-to-nearest).
        let n = ((canvas + si / 2) / si).max(1) as u32;
        assert_eq!(n, 2, "60-into-30 → 2 source frames per canvas interval");
        let src_num = u32::try_from(1_000_000_000u64 * n as u64).unwrap();
        let src_den = u32::try_from(canvas).unwrap();
        let threshold = backlog_relock_threshold(1000, src_num, src_den, n);
        assert!(
            threshold >= 64,
            "a 1000ms 60-into-30 source holds ~60 buffers (live peak 63); threshold {threshold} \
             must sit above it so the backlog branch does not fire spuriously"
        );
    }

    /// #859 follow-up — at exactly the steady target depth, never drain: this is the queue's
    /// own configured buffer, not an overshoot.
    #[test]
    fn no_drain_at_exactly_the_steady_target_depth_859() {
        let target = steady_depth_frames(957, 30, 1); // the stream box's post-step setpoint
        assert!(
            !should_drain_one(target, 957, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "depth == target must never drain, even with plenty of ticks since the last one"
        );
    }

    /// #859 follow-up — the hysteresis BOUNDARY: depth == target + DRAIN_HYSTERESIS_FRAMES must
    /// still NOT drain (strictly-greater comparison), or ordinary arrival jitter around the
    /// target would trigger a drain that then has nothing genuinely excess to shed.
    #[test]
    fn no_drain_at_target_plus_hysteresis_exactly_859() {
        let target = steady_depth_frames(957, 30, 1);
        let boundary = target + DRAIN_HYSTERESIS_FRAMES;
        assert!(
            !should_drain_one(boundary, 957, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "depth == target + hysteresis exactly must NOT drain (strictly greater only)"
        );
        assert!(
            should_drain_one(boundary + 1, 957, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "one frame past the hysteresis boundary, with enough elapsed ticks, MUST drain"
        );
    }

    /// #859 follow-up — the RATE LIMIT: even with the queue persistently above threshold, a
    /// drain fires at most once per DRAIN_MIN_TICK_INTERVAL ticks.
    #[test]
    fn drain_is_rate_limited_to_once_per_min_tick_interval_859() {
        let target = steady_depth_frames(957, 30, 1);
        let deep = target + DRAIN_HYSTERESIS_FRAMES + 3; // well above threshold, persistently
        assert!(
            !should_drain_one(deep, 957, 30, 1, 0),
            "just after a drain (ticks_since_last_drain=0) must NOT drain again immediately"
        );
        assert!(
            !should_drain_one(deep, 957, 30, 1, DRAIN_MIN_TICK_INTERVAL - 1),
            "one tick short of the interval must still NOT drain"
        );
        assert!(
            should_drain_one(deep, 957, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "at exactly DRAIN_MIN_TICK_INTERVAL ticks, an over-threshold queue MUST drain"
        );
    }

    /// #859 follow-up — the RATE BOUND + CONVERGENCE property that makes this safe to ship: a
    /// queue overshooting its target by 5 frames converges into the tolerated
    /// `target..=target+DRAIN_HYSTERESIS_FRAMES` band within the simulated window (never fully
    /// to the bare target — the hysteresis is what stops it oscillating on ordinary jitter once
    /// there), and the number of drains that fired is bounded by construction to at most one per
    /// DRAIN_MIN_TICK_INTERVAL ticks — it can never reproduce the every-tick backlog-relock
    /// burst the disabled branch used to cause.
    #[test]
    fn slew_limited_drain_converges_and_never_bursts_859() {
        let latency_ms = 957;
        let (fps_num, fps_den) = (30, 1);
        let target = steady_depth_frames(latency_ms, fps_num, fps_den);
        // 5 frames above target: 3 of those are genuinely excess (beyond the hysteresis band),
        // so exactly 3 drains are needed to settle at target + DRAIN_HYSTERESIS_FRAMES.
        let mut depth = target + 5;
        let mut ticks_since_last_drain: u64 = 0; // fresh lock — nothing drained yet
        let mut drains = 0u64;
        const TICKS: u64 = 200;
        for _ in 0..TICKS {
            if should_drain_one(depth, latency_ms, fps_num, fps_den, ticks_since_last_drain) {
                depth -= 1; // exactly ONE extra frame shed — never more per tick
                drains += 1;
                ticks_since_last_drain = 0;
            } else {
                ticks_since_last_drain += 1;
            }
            // steady inflow: one frame arrives and one is normally consumed each tick, so
            // depth is otherwise unchanged — only a drain event moves it (matches the ticket's
            // finding that the plain release=1/tick path holds depth CONSTANT on its own).
        }
        assert!(
            drains <= TICKS / DRAIN_MIN_TICK_INTERVAL + 1,
            "drains ({drains}) exceeded the rate bound (at most one per {DRAIN_MIN_TICK_INTERVAL} \
             ticks over {TICKS} ticks) — the drain is no longer bounded by construction"
        );
        assert_eq!(
            drains, 3,
            "exactly 3 frames were genuinely excess above the hysteresis band"
        );
        assert_eq!(
            depth,
            target + DRAIN_HYSTERESIS_FRAMES,
            "a 5-frame overshoot must converge into the tolerated band (target + hysteresis) \
             within {TICKS} ticks, and stop there — never all the way to the bare target"
        );
    }

    /// #859 follow-up — degenerate fps inputs must never panic. A degenerate rate implies
    /// target=0 (mirrors [`steady_depth_frames`]'s own degenerate guard), so it does NOT mean
    /// "never drain" — a depth above the bare hysteresis band still (correctly) drains once
    /// enough ticks have elapsed; only the depth==0 / ticks==0 case stays quiet.
    #[test]
    fn should_drain_one_degenerate_fps_never_panics_859() {
        assert!(should_drain_one(1000, 957, 0, 1, DRAIN_MIN_TICK_INTERVAL));
        assert!(should_drain_one(1000, 957, 30, 0, DRAIN_MIN_TICK_INTERVAL));
        assert!(!should_drain_one(0, 0, 0, 0, 0));
        assert!(should_drain_one(
            DRAIN_HYSTERESIS_FRAMES + 1,
            957,
            0,
            1,
            DRAIN_MIN_TICK_INTERVAL
        ));
    }

    // ---- #998: drain target CEIL, not round (frac(latency/interval) < 0.5 limit-cycle) ----

    /// #998 — pinned against the issue's own worked-by-hand values: ceil(latency*fps/1000/den).
    #[test]
    fn drain_target_frames_pinned_values_998() {
        assert_eq!(
            drain_target_frames(941, 30, 1),
            29,
            "941ms @30fps: ceil(28.23) = 29"
        );
        assert_eq!(
            drain_target_frames(915, 30, 1),
            28,
            "915ms @30fps: ceil(27.45) = 28"
        );
        assert_eq!(
            drain_target_frames(891, 30, 1),
            27,
            "891ms @30fps: ceil(26.73) = 27"
        );
        assert_eq!(
            drain_target_frames(930, 30, 1),
            28,
            "930ms @30fps: ceil(27.90) = 28"
        );
        assert_eq!(
            drain_target_frames(856, 30, 1),
            26,
            "856ms @30fps: ceil(25.68) = 26"
        );
        assert_eq!(
            drain_target_frames(941, 0, 1),
            0,
            "degenerate fps_num => 0, never divides by zero"
        );
        assert_eq!(
            drain_target_frames(941, 30, 0),
            0,
            "degenerate fps_den => 0, never divides by zero"
        );
    }

    /// #998 — `drain_target_frames` must be a strict CEIL, never equal to the plain floor
    /// division, at every fractional latency this module's own fixtures exercise (a regression
    /// here would silently resurrect the round-based bug via a "ceil" that isn't one).
    #[test]
    fn drain_target_frames_is_never_below_the_true_ceiling_998() {
        for (latency_ms, fps_num, fps_den) in [
            (941u32, 30u32, 1u32),
            (915, 30, 1),
            (891, 30, 1),
            (930, 30, 1),
            (856, 30, 1),
            (957, 30, 1),
        ] {
            let target = drain_target_frames(latency_ms, fps_num, fps_den);
            let floor = (latency_ms as u64 * fps_num as u64) / (1000 * fps_den as u64);
            let exact_frames = (latency_ms as f64 * fps_num as f64) / (1000.0 * fps_den as f64);
            assert!(
                (target as f64) >= exact_frames,
                "drain_target_frames({latency_ms},{fps_num},{fps_den}) = {target} must be >= the \
                 exact frame count {exact_frames} (a true ceiling never rounds down)"
            );
            if exact_frames.fract() > 1e-9 {
                assert_eq!(
                    target,
                    floor + 1,
                    "a non-integer exact frame count must ceil up by exactly one from the floor"
                );
            }
        }
    }

    /// #998 THE REGRESSION — the live limit-cycle at latency_ms=941 (frac .23): today's
    /// round-based target is 28, so depth=31 (the observed steady depth) reads as
    /// `31 > 28+2=30` => drains every tick even though the queue is at its OWN correct depth.
    /// With the ceil target (29), `31 > 29+2=31` is FALSE — no drain.
    #[test]
    fn no_false_drain_at_frac_under_half_latency_941_998() {
        assert!(
            !should_drain_one(31, 941, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998: the live steady depth (31) at latency_ms=941 (frac .23) must NOT read as \
             backlog against the ceil target (29) + hysteresis (2) = 31 — depth must exceed the \
             boundary, not merely reach it"
        );
    }

    /// #998 — the SAME regression at latency_ms=915 (frac .45). The live break under the OLD
    /// round-based target (27) is at depth=30 (`30 > 27+2=29` => TRUE); depth=29 was already
    /// FALSE under the old code too (`29 > 29` is false). Both are pinned here so the fix is
    /// verified against BOTH observed live depths, not just the one that flips.
    #[test]
    fn no_false_drain_at_frac_under_half_latency_915_998() {
        assert!(
            !should_drain_one(29, 915, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998: depth=29 at latency_ms=915 (frac .45) must NOT drain against the ceil \
             target (28) + hysteresis (2) = 30"
        );
        assert!(
            !should_drain_one(30, 915, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998: depth=30 at latency_ms=915 (frac .45) is the live break under the OLD \
             round-based target (27+2=29, 30>29 => TRUE) — must NOT drain against the ceil \
             target (28) + hysteresis (2) = 30"
        );
    }

    /// #998 — a GENUINE backlog at the SAME anomalous latency (941) must still drain: the fix
    /// narrows WHEN the drain fires, it must never disable it for an actually-excess queue.
    #[test]
    fn should_drain_one_still_drains_a_genuine_backlog_998() {
        assert!(
            should_drain_one(32, 941, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998: depth=32 at latency_ms=941 is 1 frame past the ceil target (29) + \
             hysteresis (2) = 31 — a real overshoot, must still drain (both before and after \
             this fix — this is not a regression-only pin)"
        );
    }

    /// #998 — clean-run behavior at `frac(latency/interval) >= 0.5` is BYTE-IDENTICAL: ceil ==
    /// round there, so this pins the untouched boundary at latency_ms=891 (frac .73, one of the
    /// ticket's own measured-clean values) both sides of the hysteresis band.
    #[test]
    fn drain_target_is_byte_identical_to_round_at_frac_ge_half_998() {
        assert_eq!(
            drain_target_frames(891, 30, 1),
            steady_depth_frames(891, 30, 1),
            "#998: at frac >= 0.5, ceil must equal round exactly"
        );
        assert!(
            !should_drain_one(29, 891, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998 no-regression: depth=29 at latency_ms=891 must NOT drain (target 27 + \
             hysteresis 2 = 29, not strictly exceeded)"
        );
        assert!(
            should_drain_one(30, 891, 30, 1, DRAIN_MIN_TICK_INTERVAL),
            "#998 no-regression: depth=30 at latency_ms=891 MUST drain (one past the boundary) \
             — unchanged from the pre-#998 round-based behavior since ceil==round here"
        );
    }

    // ---- #940 piece 3: phase-pinned deadline + grid-comparison hysteresis ----------------

    const I30: u64 = 33_333_333; // ~30 Hz frame interval (ns)
    const I60: u64 = 16_666_667; // ~60 Hz frame interval (ns)

    /// The core floor-to-grid arithmetic, at a plain round interval so the numbers are
    /// easy to verify by hand.
    #[test]
    fn phase_pinned_deadline_floors_to_the_grid_940() {
        // 100 / 30 = 3.33 -> floor 3 -> 90.
        assert_eq!(phase_pinned_deadline(100, 30), 90);
        // Already exactly on a grid line -> unchanged (idempotent).
        assert_eq!(phase_pinned_deadline(90, 30), 90);
        // One ns short of the NEXT grid line still floors DOWN to the current one, never up.
        assert_eq!(phase_pinned_deadline(119, 30), 90);
        assert_eq!(phase_pinned_deadline(120, 30), 120);
        // Zero deadline (early-boot wall clock) -> zero, not a panic.
        assert_eq!(phase_pinned_deadline(0, 30), 0);
    }

    /// The same arithmetic at the rig's REAL frame intervals, so the magnitude of what the
    /// floor can shift a deadline by is visible in the test itself, not just asserted as an
    /// abstract property. Worst case is just under one full interval earlier than the raw
    /// (continuous) deadline — the bounded, deliberate cost of removing the lock-history
    /// dependency (see the module doc on [`phase_pinned_deadline`]).
    #[test]
    fn phase_pinned_deadline_shifts_at_most_just_under_one_interval_940() {
        for interval in [I30, I60] {
            let raw = 900_000_000_000u64 + interval - 1; // one ns short of the next grid line
            let pinned = phase_pinned_deadline(raw, interval);
            assert!(
                pinned <= raw,
                "the grid floor must never move the deadline LATER"
            );
            assert!(
                raw - pinned < interval,
                "the grid floor must never shift the deadline by a WHOLE interval or more \
                 (raw={raw}, pinned={pinned}, interval={interval})"
            );
        }
    }

    /// Degenerate interval (0 — unknown video info) must return the raw deadline unchanged,
    /// matching every other genlock helper's degenerate-interval convention in this module
    /// (never divide by zero, never invent a value).
    #[test]
    fn phase_pinned_deadline_degenerate_interval_returns_raw_940() {
        assert_eq!(phase_pinned_deadline(123_456, 0), 123_456);
        assert_eq!(phase_pinned_deadline(0, 0), 0);
    }

    /// The grid-comparison hysteresis: a frame captured essentially exactly on a grid line
    /// (i.e. just PAST the phase-pinned deadline) must still read as due, within
    /// [`PHASE_PIN_HYSTERESIS_NS`] — this is the flapping guard the design's own risk note
    /// calls out. A frame beyond the hysteresis window stays not-due.
    #[test]
    fn phase_pinned_is_due_applies_hysteresis_at_the_boundary_940() {
        let deadline = 900_000_000_000u64;
        assert!(
            phase_pinned_is_due(deadline, deadline),
            "exactly at the deadline must be due"
        );
        assert!(
            phase_pinned_is_due(deadline + PHASE_PIN_HYSTERESIS_NS, deadline),
            "exactly at the hysteresis boundary must still be due (inclusive)"
        );
        assert!(
            !phase_pinned_is_due(deadline + PHASE_PIN_HYSTERESIS_NS + 1, deadline),
            "one ns past the hysteresis boundary must NOT be due"
        );
        assert!(
            phase_pinned_is_due(deadline - 1, deadline),
            "a frame already past due (ts before the deadline) stays due"
        );
    }

    /// Sanity invariant: the hysteresis must stay a SMALL fraction of a frame interval at
    /// the fastest rate this rig ever runs (60 fps) — this is what makes it structurally
    /// incapable of pulling in an extra frame. If a future edit ever grows this constant
    /// toward a real fraction of an interval, this test catches it immediately rather than
    /// relying on someone noticing during review.
    #[test]
    #[allow(clippy::assertions_on_constants)] // deliberate compile-time invariant, kept as a
                                              // runtime test for consistency with this file's
                                              // other tests (never a const {} block).
    fn phase_pin_hysteresis_is_a_small_fraction_of_any_rig_frame_interval_940() {
        assert!(
            PHASE_PIN_HYSTERESIS_NS < I60 / 2,
            "PHASE_PIN_HYSTERESIS_NS ({PHASE_PIN_HYSTERESIS_NS} ns) must stay well under \
             half a 60fps frame interval ({} ns), or it risks pulling in an extra frame",
            I60 / 2
        );
    }

    // ------------------------------------------------------------------------------------
    // #1009 — backward-step guard re-qualification + self-heal acceptance tests.
    //
    // The FIFO sim below models the C ts-align release exactly as obs-source.c structures
    // it for a 1:1 (N==1) source: phase-pinned wall deadline -> due prefix scan -> the
    // due==0 guard -> the issue-401 locked-boundary cadence (ACQUIRE / STEADY / GAP / HOLD).
    // The settle-drain branch is omitted (no #1009 scenario holds a parked overshoot). The
    // BACKLOG-STORM branch IS modelled since #1003 — but no #1009 scenario reaches it, so
    // every pre-existing #1009 result is unchanged (verified: relocks/dropped stay 0 in all
    // four of them).
    // ------------------------------------------------------------------------------------

    const BASE_1009: u64 = 10_000_000_000_000;
    const RESERVE_MS_1009: u32 = 894; // the live knob the overnight collapse bypassed
    const RESERVE_NS_1009: u64 = RESERVE_MS_1009 as u64 * 1_000_000;

    // ------------------------------------------------------------------------------------
    // #1003 — the sim is PARAMETERISED BY SELECTION STRATEGY so the defect and the fix are
    // observable side by side in Tier-0. Everything else about it is the #1009 model.
    // ------------------------------------------------------------------------------------

    /// The sender's own floor-boundary stamp grid, in 100 ns units (NDI stamps in 100 ns
    /// ticks, so a 30 fps period truncates to 333,333 ticks = 33,333,300 ns) — 33 ns per
    /// frame BELOW the receiver's 33,333,333 ns canvas interval. That 33 ns/frame beat
    /// (~3.6 ms/h) is Edge 2: the fixed 5 ms comparison hysteresis is a FIXED edge inside a
    /// DRIFTING relative phase.
    const SENDER_GRID_1003: u64 = 33_333_300;
    /// The live stream-box knob at the measurements in issue 1003.
    const RESERVE_MS_1003: u32 = 923;
    const RESERVE_NS_1003: u64 = RESERVE_MS_1003 as u64 * 1_000_000;
    /// Transport delay cam -> receiver.
    const NET_NS_1003: u64 = 1_000_000;

    /// Which rule the relock branches (ACQUIRE / BACKLOG STORM) use to SELECT the frame to
    /// present. The whole point of #1003 is that the first is edge-ridden and the second is
    /// not, so both live here and the tests drive each explicitly.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SelectionStrategy {
        /// The DEPLOYED rule: present the NEWEST frame the wall deadline calls due.
        /// Instant-sampled and stateless — carries both edges.
        NewestDue,
        /// #1003: present the frame NEAREST the tracked phase anchor
        /// ([`relock_select_nearest`]). Continuous — inherits the phase from history.
        PhaseAnchored,
    }

    struct SimFifo1009 {
        queue: std::collections::VecDeque<u64>,
        boundary: u64,
        guard: BackwardStepGuard,
        holds: u64,
        underruns: u64,
        backward_events: u64,
        regime_warns: u64,
        selfheals: u64,
        /// The tick index of the FIRST re-anchor ever fired (None = guard never fired) —
        /// the honest sustained-qualification signal (review finding on issue 1009: an
        /// age-threshold filter here was tautological, it could never fail).
        first_reanchor_tick: Option<u64>,
        /// (tick index, wall - presented stamp) per presentation.
        presented: Vec<(u64, i64)>,
        /// #1003: which selection rule the relock branches use.
        strategy: SelectionStrategy,
        /// #1003: the tracked conveyor phase anchor (0 = UNSET), mirroring the C
        /// `genlock_phase_anchor_ns`.
        phase_anchor_ns: u64,
        /// #1003: the configured hold, so the #1009 (894 ms) and #1003 (923 ms) scenarios
        /// can share one sim.
        reserve_ns: u64,
        /// #1003: frames erased by a relock selection (the C `genlock_dropped_due`).
        dropped: u64,
        /// #1003: BACKLOG-STORM relock events (the C `genlock_relocks`).
        relocks: u64,
        /// #1003: (tick index, presented age) recorded ONLY for RELOCK presents
        /// (ACQUIRE / BACKLOG) — the release phase each lock episode mints.
        relock_presents: Vec<(u64, i64)>,
        /// #1003: how many times a SELF-HEAL cleared an anchor that was actually SET. The
        /// honest signal that the seam clears — a test asserting only "the anchor is 0"
        /// would also pass on a run that never established one.
        selfheal_anchor_clears: u64,
    }

    impl SimFifo1009 {
        /// The #1009 default: the DEPLOYED selection rule at the #1009 knob, so every
        /// pre-existing #1009 acceptance test is byte-identical to before #1003.
        fn new() -> Self {
            Self::with(SelectionStrategy::NewestDue, RESERVE_NS_1009)
        }

        /// #1003: a sim with an explicit selection strategy and configured hold.
        fn with(strategy: SelectionStrategy, reserve_ns: u64) -> Self {
            Self {
                queue: std::collections::VecDeque::new(),
                boundary: 0,
                guard: BackwardStepGuard::new(),
                holds: 0,
                underruns: 0,
                backward_events: 0,
                regime_warns: 0,
                selfheals: 0,
                first_reanchor_tick: None,
                presented: Vec::new(),
                strategy,
                phase_anchor_ns: 0,
                reserve_ns,
                dropped: 0,
                relocks: 0,
                relock_presents: Vec::new(),
                selfheal_anchor_clears: 0,
            }
        }

        /// Change the configured hold at runtime — the sim's mirror of the C
        /// `obs_source_set_genlock_latency_ms()` (a knob hot-apply, which is one of the
        /// routine relock triggers the ticket's live evidence lists).
        fn set_reserve_ms(&mut self, ms: u32) {
            if self.reserve_ns != ms as u64 * 1_000_000 {
                // #1003: the remembered on-air age describes a hold that no longer
                // exists. Carrying it across a setpoint change targets the OLD phase
                // forever — and, on a DECREASE, sheds nothing while the lowered
                // threshold qualifies the backlog branch every tick.
                self.phase_anchor_ns = 0;
            }
            self.reserve_ns = ms as u64 * 1_000_000;
        }

        /// The backlog-storm threshold this sim's own configured hold implies (issue 859 —
        /// UNCHANGED by #1003; it still decides WHEN a relock fires, only the SELECTION
        /// inside the branch changes).
        fn backlog_threshold(&self) -> u64 {
            backlog_relock_threshold((self.reserve_ns / 1_000_000) as u32, 30, 1, 1)
        }

        /// The ACQUIRE / BACKLOG-STORM relock present. Selects per [`SelectionStrategy`],
        /// erases the older frames into `dropped` (the C `da_erase(.,0)` +
        /// `remove_async_frame()` + `genlock_dropped_due` idiom, unchanged — this is how a
        /// relock still corrects DEPTH), presents the selected frame and returns its stamp.
        ///
        /// The anchor is deliberately NOT updated here: a relock INHERITS the phase, it does
        /// not redefine it (#1003). Only STEADY / GAP presents move the anchor.
        fn relock_present(&mut self, k: u64, wall: u64, backlog: bool, due: usize) -> u64 {
            let idx = match self.strategy {
                SelectionStrategy::NewestDue => due - 1,
                SelectionStrategy::PhaseAnchored => {
                    let q: Vec<u64> = self.queue.iter().copied().collect();
                    let ms = (self.reserve_ns / 1_000_000) as u32;
                    let mut i = relock_select_nearest(
                        &q,
                        wall,
                        relock_anchor_age_ns(self.phase_anchor_ns, ms),
                    );
                    // #1003 review finding: a BACKLOG relock that would shed NOTHING is
                    // proof the anchor is STALE — the branch only fires above the
                    // latency-implied depth, so an anchor pointing at (or before) the
                    // queue head cannot describe a queue this deep. Carrying it re-fires
                    // the branch every tick shedding nothing, and because the branch
                    // pre-empts STEADY the settle-back drain never runs either. Drop the
                    // stale anchor and re-select against the CONFIGURED latency: one
                    // relock sheds the overshoot and the anchor rebuilds from the next
                    // STEADY present. (ACQUIRE is exempt: idx 0 there just means "present
                    // the head", and the fresh lock stops the branch re-firing.)
                    if backlog && i == 0 && self.phase_anchor_ns != 0 {
                        self.phase_anchor_ns = 0;
                        i = relock_select_nearest(&q, wall, relock_anchor_age_ns(0, ms));
                    }
                    i
                }
            };
            for _ in 0..idx {
                self.queue.pop_front().expect("selected index is in range");
                self.dropped += 1;
            }
            let ts = self.queue.pop_front().expect("selected frame");
            let age = wall as i64 - ts as i64;
            self.presented.push((k, age));
            self.relock_presents.push((k, age));
            ts
        }

        /// One render tick at receiver wall time `wall` (also the warn-cadence clock — the
        /// sim never steps the log clock). Mirrors the C release path's decision order.
        fn tick(&mut self, k: u64, wall: u64, log_now: u64) {
            if self.queue.is_empty() {
                // A true-empty BEFORE anything was ever presented is the startup build
                // phase (the C holds/build-fill path, issue 269 [4]) — only a post-start
                // empty is real starvation.
                if !self.presented.is_empty() {
                    self.underruns += 1;
                }
                return;
            }
            let present_ts = phase_pinned_deadline(wall.saturating_sub(self.reserve_ns), I30);
            // The C due scan is a PREFIX scan in arrival order.
            let due = self
                .queue
                .iter()
                .take_while(|&&ts| phase_pinned_is_due(ts, present_ts))
                .count();
            if due == 0 {
                let max_ts = *self.queue.iter().max().unwrap();
                match self.guard.tick_due0(max_ts, wall, I30, log_now) {
                    BackwardStepAction::Reanchor { entry, warn } => {
                        if self.first_reanchor_tick.is_none() {
                            self.first_reanchor_tick = Some(k);
                        }
                        if entry {
                            self.backward_events += 1;
                        }
                        if warn {
                            self.regime_warns += 1;
                        }
                        let ts = self.queue.pop_front().unwrap();
                        self.boundary = ts + I30;
                        self.presented.push((k, wall as i64 - ts as i64));
                        return;
                    }
                    BackwardStepAction::SelfHeal => {
                        self.selfheals += 1;
                        self.boundary = 0;
                        // #1003: the receiver wall clock stepped, so every `wall - ts` age
                        // sampled before the correction is off by the step. CLEAR the
                        // anchor — the re-ACQUIRE then falls back to the CONFIGURED
                        // latency, which is exactly #1009's own self-heal contract.
                        if self.phase_anchor_ns != 0 {
                            self.selfheal_anchor_clears += 1;
                        }
                        self.phase_anchor_ns = 0;
                    }
                    BackwardStepAction::Pending | BackwardStepAction::None => {}
                }
            } else if matches!(self.guard.tick_due_positive(), BackwardStepAction::SelfHeal) {
                self.selfheals += 1;
                self.boundary = 0;
                // #1003: same seam as the due==0 self-heal above — a stepped clock
                // invalidates every sampled age, so the anchor is cleared, not carried.
                if self.phase_anchor_ns != 0 {
                    self.selfheal_anchor_clears += 1;
                }
                self.phase_anchor_ns = 0;
            }
            // The issue-401 cadence, N==1 paths.
            if self.boundary == 0 {
                if due > 0 {
                    // ACQUIRE (a relock branch — see relock_present).
                    let ts = self.relock_present(k, wall, false, due);
                    self.boundary = ts + I30;
                } else {
                    self.holds += 1;
                }
            } else if self.queue.len() as u64 > self.backlog_threshold() && due > 0 {
                // BACKLOG STORM (issue 859 threshold, unchanged) — the OTHER relock branch.
                self.relocks += 1;
                let ts = self.relock_present(k, wall, true, due);
                self.boundary = ts + I30;
            } else if *self.queue.front().unwrap() <= self.boundary {
                // STEADY: the head matured by the locked boundary.
                let ts = self.queue.pop_front().unwrap();
                self.boundary = ts + I30;
                // #1003: the steady conveyor's own on-air age IS the phase anchor.
                self.phase_anchor_ns = phase_anchor_from_present(wall, ts);
                self.presented.push((k, wall as i64 - ts as i64));
            } else if present_ts >= *self.queue.front().unwrap() {
                // GAP RESYNC: aged past the reserve, beyond the boundary.
                let ts = self.queue.pop_front().unwrap();
                self.boundary = ts + I30;
                // #1003: upstream skipped stamps, so any pre-gap anchor is stale — this
                // present RE-DERIVES it from the frame actually put on air (the same one
                // line serves both "update on GAP present" and "do not carry the pre-seam
                // value forward").
                self.phase_anchor_ns = phase_anchor_from_present(wall, ts);
                self.presented.push((k, wall as i64 - ts as i64));
            } else {
                self.holds += 1;
            }
        }
    }

    /// Drive `n_ticks` of a 30 fps receiver against a sender whose frames arrive with
    /// `net_ns` transport delay, stamped with the WORST-CASE deployed ceil bias (a full
    /// interval in the future at emit) plus a per-tick sender-clock-ahead skew from
    /// `skew_at(k)`. `sender_period_ns` slightly above I30 produces the routine due==0
    /// hold ticks of the live rig (boundary churn). `wall_offset_at(k)` models receiver
    /// wall-clock steps (0 = none).
    fn run_sim_1009(
        fifo: &mut SimFifo1009,
        n_ticks: u64,
        sender_period_ns: u64,
        net_ns: u64,
        skew_at: impl Fn(u64) -> u64,
        wall_offset_at: impl Fn(u64) -> i64,
    ) {
        let mut next_emit = 0u64;
        for k in 0..n_ticks {
            let true_now = BASE_1009 + k * I30;
            // Deliver every frame emitted (plus transport delay) by this tick instant.
            loop {
                let e = BASE_1009 + next_emit * sender_period_ns;
                if e + net_ns > true_now {
                    break;
                }
                // Worst-case deployed sender stamp: ceil-to-boundary = up to one full
                // interval ahead of the sender clock, which itself is `skew` ahead.
                let stamp = e + skew_at(next_emit) + I30;
                fifo.queue.push_back(stamp);
                next_emit += 1;
            }
            let wall = (true_now as i64 + wall_offset_at(k)) as u64;
            // The warn-cadence clock is monotonic (immune to the wall steps we model).
            fifo.tick(k, wall, true_now);
        }
    }

    /// Acceptance 1 (#1009): a sustained sender-ahead skew of 5-50 ms must NEVER fire the
    /// backward-step path and NEVER collapse the configured hold. This is exactly the
    /// overnight trigger shape (dantesync chase-step skew + the sender's ceil future bias).
    #[test]
    fn sender_ahead_skew_5_to_50ms_never_fires_the_guard_or_collapses_the_hold_1009() {
        for skew_ms in [5u64, 20, 50] {
            let skew = skew_ms * 1_000_000;
            let mut fifo = SimFifo1009::new();
            // Sender ~2% slow: routine due==0 hold ticks occur (the live boundary churn).
            run_sim_1009(&mut fifo, 900, I30 + I30 / 50, 1_000_000, |_| skew, |_| 0);
            assert_eq!(
                fifo.backward_events, 0,
                "skew {skew_ms} ms: the backward-step guard fired {} event(s) on plain \
                 sender-ahead stamp skew — the issue-1007 hair-trigger (margin must be \
                 >> the sender's one-interval future bias)",
                fifo.backward_events
            );
            assert_eq!(
                fifo.guard.reanchor_ticks(),
                0,
                "skew {skew_ms} ms: re-anchor ticks must stay 0 in normal operation"
            );
            // The hold must be the configured latency, not the live edge: presented frames
            // in the settled tail sit ~reserve old (± 2 intervals of quantization).
            let tail: Vec<i64> = fifo
                .presented
                .iter()
                .filter(|(k, _)| *k >= 700)
                .map(|(_, age)| *age)
                .collect();
            assert!(
                !tail.is_empty(),
                "skew {skew_ms} ms: the sim presented nothing in the settled tail"
            );
            let min_ok = RESERVE_NS_1009 as i64 - 2 * I30 as i64;
            let max_ok = RESERVE_NS_1009 as i64 + 2 * I30 as i64;
            for age in &tail {
                assert!(
                    (min_ok..=max_ok).contains(age),
                    "skew {skew_ms} ms: presented-frame age {age} ns is outside the \
                     configured hold {RESERVE_NS_1009}±2 intervals — the hold collapsed \
                     (live-edge consumption)",
                );
            }
        }
    }

    /// Acceptance 1b (#1009): when a sender-ahead skew CLEARS (the dantesync correction
    /// lands), the hold must still be the configured latency afterwards — under the
    /// deployed guard the build/steady phase was already absorbed into permanent live-edge
    /// mode and never came back.
    #[test]
    fn hold_survives_a_sender_skew_episode_and_its_clearing_1009() {
        let mut fifo = SimFifo1009::new();
        // 40 ms sender-ahead skew for the first 300 ticks, then corrected to 0.
        run_sim_1009(
            &mut fifo,
            900,
            I30 + I30 / 50,
            1_000_000,
            |j| if j < 300 { 40_000_000 } else { 0 },
            |_| 0,
        );
        assert_eq!(
            fifo.backward_events, 0,
            "a 40 ms sender-ahead episode fired the backward-step guard ({} events) — \
             the hair-trigger defect",
            fifo.backward_events
        );
        let tail: Vec<i64> = fifo
            .presented
            .iter()
            .filter(|(k, _)| *k >= 750)
            .map(|(_, age)| *age)
            .collect();
        assert!(!tail.is_empty(), "nothing presented in the settled tail");
        let min_ok = RESERVE_NS_1009 as i64 - 2 * I30 as i64;
        let max_ok = RESERVE_NS_1009 as i64 + 2 * I30 as i64;
        for age in &tail {
            assert!(
                (min_ok..=max_ok).contains(age),
                "after the skew episode cleared, presented-frame age {age} ns is outside \
                 the configured hold {RESERVE_NS_1009}±2 intervals — the collapse is a \
                 permanent absorbing state (no self-heal)",
            );
        }
        assert_eq!(
            fifo.underruns, 0,
            "live-edge consumption starves the FIFO ({} underruns) — the hold collapsed",
            fifo.underruns
        );
    }

    /// Acceptance 2 + 3 (#1009): a REAL backward wall-clock step (500 ms here, >= the 250 ms
    /// margin floor and sustained) must still recover per issue-147's original intent —
    /// qualified (not single-tick), presenting throughout, loudly re-warning while the
    /// regime persists — and once the clock is corrected the guard must SELF-HEAL back to
    /// the configured hold within bounded time.
    #[test]
    fn real_backward_step_recovers_and_the_hold_returns_after_the_episode_1009() {
        let mut fifo = SimFifo1009::new();
        const STEP_AT: u64 = 400;
        const STEP_LEN: u64 = 150; // 5 s regime — long enough to demand cadence re-warns
        const STEP_NS: i64 = -500_000_000;
        run_sim_1009(
            &mut fifo,
            900,
            I30, // exact-rate sender: the step itself makes every tick due==0
            1_000_000,
            |_| 0,
            |k| {
                if (STEP_AT..STEP_AT + STEP_LEN).contains(&k) {
                    STEP_NS
                } else {
                    0
                }
            },
        );
        // The step was detected and counted as ONE event.
        assert_eq!(
            fifo.backward_events, 1,
            "a real 500 ms sustained backward step must be detected exactly once (got {})",
            fifo.backward_events
        );
        // Sustained qualification: the guard must NOT re-anchor within the first
        // BACKWARD_STEP_SUSTAIN_TICKS-1 ticks of the step (never a single-tick trigger).
        let early_reanchors = fifo
            .presented
            .iter()
            .filter(|(k, age)| {
                (STEP_AT..STEP_AT + BACKWARD_STEP_SUSTAIN_TICKS as u64 - 1).contains(k)
                    && *age > RESERVE_NS_1009 as i64 + STEP_NS.unsigned_abs() as i64 / 2
            })
            .count();
        assert_eq!(
            early_reanchors,
            0,
            "the guard re-anchored within the first {} ticks of the step — the sustained \
             qualification is missing (single-tick hair-trigger)",
            BACKWARD_STEP_SUSTAIN_TICKS - 1
        );
        // Presentation must CONTINUE through the regime (the issue-147 no-freeze intent).
        let regime_presents = fifo
            .presented
            .iter()
            .filter(|(k, _)| (STEP_AT..STEP_AT + STEP_LEN).contains(k))
            .count();
        assert!(
            regime_presents as u64 >= STEP_LEN - 8,
            "the feed must keep presenting through a backward-step regime \
             (presented {regime_presents} of {STEP_LEN} regime ticks)"
        );
        // A regime older than BACKWARD_REGIME_WARN_AFTER_NS must re-warn on a bounded
        // cadence — at least once for this 5 s regime, never per-tick spam.
        assert!(
            fifo.regime_warns >= 1,
            "a 5 s re-anchor regime produced no bounded-cadence WARN — the silent-collapse \
             defect (once-per-latch logging)"
        );
        assert!(
            fifo.regime_warns <= 3,
            "regime warns must be cadence-bounded, got {} for a 5 s regime",
            fifo.regime_warns
        );
        // Re-anchor ticks were accounted (the audit/gate counter).
        assert!(
            fifo.guard.reanchor_ticks() > 0,
            "a real regime must move the reanchor_ticks audit counter"
        );
        // SELF-HEAL: once the clock correction lands, the guard must hand the release back
        // to the configured hold (boundary zeroed -> re-ACQUIRE at latency depth).
        assert!(
            fifo.selfheals >= 1,
            "the regime ended but the guard never signalled SELF-HEAL — the hold-collapse \
             is a permanent absorbing state"
        );
        // Bounded return: within ~latency + slack after the step ends, presented-frame age
        // must be back at the configured hold.
        let rebuild_deadline = STEP_AT + STEP_LEN + 27 + 30;
        let tail: Vec<i64> = fifo
            .presented
            .iter()
            .filter(|(k, _)| *k >= rebuild_deadline)
            .map(|(_, age)| *age)
            .collect();
        assert!(
            !tail.is_empty(),
            "nothing presented after the rebuild window"
        );
        let min_ok = RESERVE_NS_1009 as i64 - 2 * I30 as i64;
        let max_ok = RESERVE_NS_1009 as i64 + 2 * I30 as i64;
        for age in &tail {
            assert!(
                (min_ok..=max_ok).contains(age),
                "after the backward-step episode the hold must return to the configured \
                 {RESERVE_NS_1009} ns within bounded time; presented age {age} ns"
            );
        }
    }

    /// #1009: the margin formula — max(3×interval, 250 ms) — and the guard actually
    /// honouring it: a sustained condition BELOW the margin never fires; one ABOVE it
    /// fires only after BACKWARD_STEP_SUSTAIN_TICKS consecutive ticks.
    #[test]
    fn margin_is_3_intervals_floored_at_250ms_and_the_guard_honours_it_1009() {
        assert_eq!(
            backward_step_margin_ns(I30),
            BACKWARD_STEP_MIN_MARGIN_NS,
            "3×33.3 ms = 100 ms < the 250 ms floor"
        );
        assert_eq!(
            backward_step_margin_ns(I60),
            BACKWARD_STEP_MIN_MARGIN_NS,
            "3×16.7 ms = 50 ms < the 250 ms floor"
        );
        assert_eq!(
            backward_step_margin_ns(100_000_000),
            300_000_000,
            "3 intervals wins once it exceeds the floor"
        );

        // Below the margin, sustained forever: never fires.
        let mut g = BackwardStepGuard::new();
        let wall = BASE_1009;
        for t in 0..10u64 {
            let a = g.tick_due0(wall + 240_000_000, wall, I30, wall + t);
            assert!(
                matches!(a, BackwardStepAction::None | BackwardStepAction::Pending),
                "tick {t}: a sustained 240 ms excursion is BELOW the 250 ms margin and \
                 must never re-anchor (got {a:?})"
            );
        }
        assert_eq!(g.reanchor_ticks(), 0);

        // Above the margin: Pending for the first SUSTAIN-1 ticks, Reanchor on the Nth.
        let mut g = BackwardStepGuard::new();
        for t in 0..(BACKWARD_STEP_SUSTAIN_TICKS - 1) {
            let a = g.tick_due0(wall + 400_000_000, wall, I30, wall + t as u64);
            assert_eq!(
                a,
                BackwardStepAction::Pending,
                "tick {t}: an over-margin condition must QUALIFY (pending), not fire \
                 single-tick"
            );
        }
        let a = g.tick_due0(
            wall + 400_000_000,
            wall,
            I30,
            wall + BACKWARD_STEP_SUSTAIN_TICKS as u64,
        );
        assert_eq!(
            a,
            BackwardStepAction::Reanchor {
                entry: true,
                warn: false
            },
            "the {BACKWARD_STEP_SUSTAIN_TICKS}th consecutive over-margin tick fires the \
             qualified re-anchor (entry edge)"
        );
    }

    /// #1009 review hardening: the regime EXIT must be qualified like the entry. A
    /// condition FLAPPING around the margin (a slewing clock crossing it — max_ts advances
    /// in whole-interval quanta while the wall advances continuously, so head_future
    /// sawtooths at the crossing) must NOT exit-and-re-enter per flap cycle: every exit
    /// runs the SELF-HEAL re-ACQUIRE, which costs a bounded ~latency_ms hold while the
    /// queue rebuilds — an unhysteretic exit turns a marginal condition into a repeated
    /// freeze loop. Exit therefore requires BACKWARD_STEP_SUSTAIN_TICKS CONSECUTIVE clear
    /// due==0 ticks (a due>0 tick still exits immediately — frames aged past the reserve
    /// against the wall deadline is structural proof the condition is really over).
    #[test]
    fn regime_exit_requires_sustained_clear_not_a_single_tick_1009() {
        let wall = BASE_1009;
        let mut g = BackwardStepGuard::new();
        // Enter the regime: SUSTAIN consecutive over-margin ticks.
        for t in 0..BACKWARD_STEP_SUSTAIN_TICKS {
            g.tick_due0(wall + 400_000_000, wall, I30, wall + t as u64);
        }
        assert!(g.in_step(), "setup: the regime must be active");
        let events_after_entry = 1u64; // one entry so far

        // FLAP: alternating clear / over-margin due==0 ticks — the regime must PERSIST
        // (no SelfHeal, no new entry events), because no clear run reaches the sustain
        // requirement.
        let mut entries = events_after_entry;
        for t in 0..10u64 {
            let over = t % 2 == 1;
            let m = if over { 400_000_000 } else { 10_000_000 };
            let a = g.tick_due0(wall + m, wall, I30, wall + 100 + t);
            assert_ne!(
                a,
                BackwardStepAction::SelfHeal,
                "flap tick {t}: a single clear tick inside a flap must NOT end the regime \
                 (each exit costs a ~latency_ms re-ACQUIRE hold)"
            );
            if let BackwardStepAction::Reanchor { entry, .. } = a {
                if entry {
                    entries += 1;
                }
            }
            assert!(
                g.in_step(),
                "flap tick {t}: the regime must persist through the flap"
            );
        }
        assert_eq!(
            entries, 1,
            "a flap must not mint new backward-step EVENTS (entry re-fired)"
        );

        // SUSTAINED clear: exactly BACKWARD_STEP_SUSTAIN_TICKS consecutive clear due==0
        // ticks end the regime, once, via SELF-HEAL.
        for t in 0..(BACKWARD_STEP_SUSTAIN_TICKS - 1) {
            let a = g.tick_due0(wall + 10_000_000, wall, I30, wall + 200 + t as u64);
            assert_ne!(
                a,
                BackwardStepAction::SelfHeal,
                "clear tick {t}: the exit must not fire before the clear run sustains"
            );
            assert!(g.in_step());
        }
        let a = g.tick_due0(wall + 10_000_000, wall, I30, wall + 300);
        assert_eq!(
            a,
            BackwardStepAction::SelfHeal,
            "the {BACKWARD_STEP_SUSTAIN_TICKS}th consecutive clear due==0 tick must \
             SELF-HEAL (this is also the ONLY test of tick_due0's SelfHeal branch — the \
             sender-side-correction regime end, review finding on issue 1009)"
        );
        assert!(!g.in_step(), "the regime must be over after the self-heal");
    }

    /// #1009: a 1-2 tick over-margin TRANSIENT (a stamp outlier, a correction seam) must
    /// never fire, and leaving it must not self-heal-reset anything (no regime existed).
    #[test]
    fn a_short_over_margin_transient_never_fires_the_guard_1009() {
        let mut g = BackwardStepGuard::new();
        let wall = BASE_1009;
        for t in 0..(BACKWARD_STEP_SUSTAIN_TICKS - 1) {
            let a = g.tick_due0(wall + 900_000_000, wall, I30, wall + t as u64);
            assert!(
                matches!(a, BackwardStepAction::Pending),
                "a not-yet-sustained over-margin tick must be Pending (got {a:?})"
            );
        }
        // The condition clears before qualifying.
        let a = g.tick_due0(wall + 10_000_000, wall, I30, wall + 10);
        assert_eq!(
            a,
            BackwardStepAction::None,
            "clearing an unqualified transient is a plain None (no regime ever started)"
        );
        assert_eq!(g.reanchor_ticks(), 0, "a transient must never re-anchor");
        assert!(!g.in_step(), "a transient must never latch the regime");
    }

    // ------------------------------------------------------------------------------------
    // #1003 — phase-continuity relock acceptance tests.
    //
    // The scenario is the live stream-box one: a 923 ms hold, a sender stamping on its own
    // 33,333,300 ns floor grid, a receiver rendering on the 33,333,333 ns canvas grid, and a
    // FORCED re-lock episode (what a knob hot-apply / program switch / recording start does).
    // ------------------------------------------------------------------------------------

    /// The tick a forced re-lock episode fires on, comfortably past the ~28-frame build-up
    /// the 923 ms hold implies.
    const RELOCK_TICK_1003: u64 = 300;

    /// Render-tick slews (ns) the relock episode fires at. All within the ±2 ms the deployed
    /// FIFO actually sees — and, thanks to [`step_aligned_base_1003`], STRADDLING the #940
    /// floor-pin step point, with a one-NANOSECOND pair (`-1`, `0`) either side of it.
    /// (>=5 of them — the ticket's own acceptance bar for the number of episodes.)
    const SLEW_SWEEP_1003: [i64; 5] = [-2_000_000, -1_000_000, -1, 0, 1_000_000];
    const _: () = assert!(SLEW_SWEEP_1003.len() >= 5);

    /// The episode base that puts the #940 floor-pin STEP POINT at exactly zero render-tick
    /// slew on tick [`RELOCK_TICK_1003`].
    ///
    /// `(wall − reserve) mod interval` is CONSTANT in the tick index — wall advances by
    /// exactly one interval per tick — so the step point cannot be reached by choosing a
    /// different tick; it moves only with the base. Shifting the base is precisely what
    /// distinguishes one real deployment (or one lock episode hours later, after the grids
    /// have beaten past each other) from another, which is why the live evidence sees the
    /// step across episodes rather than within one run.
    fn step_aligned_base_1003() -> u64 {
        let w = BASE_1009 + RELOCK_TICK_1003 * I30 - RESERVE_NS_1003;
        BASE_1009 + (I30 - (w % I30)) % I30
    }

    struct Episode1003 {
        /// The conveyor's own age on the tick BEFORE the forced relock.
        steady_before: i64,
        /// The age the forced relock minted — the episode's release phase.
        relock_age: i64,
        /// The tracked anchor immediately before the relock tick.
        anchor_before: u64,
        /// The tracked anchor immediately after it.
        anchor_after: u64,
    }

    /// Warm the conveyor to steady state at the nominal grid phase, then force ONE re-lock
    /// episode and hold a render-tick slew of `slew_ns` from that tick onward — modelling a
    /// lock episode that fires at a different point of the grid than the one that
    /// established the conveyor. Reports the phase either side of the episode.
    ///
    /// The relock may legitimately HOLD for a tick or two before it presents: below the
    /// floor-pin step point nothing is due yet, so the ACQUIRE branch holds and acquires on
    /// the next tick. That hold is part of the episode, so the phase is read from the FIRST
    /// relock present after the forced reset, whenever it lands.
    fn episode_1003(strategy: SelectionStrategy, slew_ns: i64) -> Episode1003 {
        /// How many ticks the forced relock is given to actually present.
        const SETTLE_TICKS: u64 = 4;
        let base = step_aligned_base_1003();
        let mut fifo = SimFifo1009::with(strategy, RESERVE_NS_1003);
        let mut next_emit = 0u64;
        for k in 0..RELOCK_TICK_1003 {
            let true_now = base + k * I30;
            // The sender emits on ITS OWN grid; the receiver renders on the canvas grid.
            loop {
                let e = base + next_emit * SENDER_GRID_1003;
                if e + NET_NS_1003 > true_now {
                    break;
                }
                fifo.queue.push_back(e);
                next_emit += 1;
            }
            fifo.tick(k, true_now, true_now);
        }
        let relocks_before_episode = fifo.relock_presents.len();
        let steady_before = fifo.presented.last().copied();
        let anchor_before = fifo.phase_anchor_ns;
        // THE FORCED RE-LOCK: the cadence loses its lock exactly as a knob hot-apply /
        // source reset does, so the relock branch re-selects from here.
        fifo.boundary = 0;
        for k in RELOCK_TICK_1003..RELOCK_TICK_1003 + SETTLE_TICKS {
            let true_now = base + k * I30;
            loop {
                let e = base + next_emit * SENDER_GRID_1003;
                if e + NET_NS_1003 > true_now {
                    break;
                }
                fifo.queue.push_back(e);
                next_emit += 1;
            }
            let wall = (true_now as i64 + slew_ns) as u64;
            fifo.tick(k, wall, true_now);
            if fifo.relock_presents.len() > relocks_before_episode {
                // The relock presented — stop here so `phase_anchor_ns` is read exactly
                // as the relock left it, before any later STEADY tick moves it again.
                break;
            }
        }
        let (sb_tick, sb_age) =
            steady_before.expect("the conveyor must have presented before the relock");
        assert_eq!(
            sb_tick,
            RELOCK_TICK_1003 - 1,
            "setup: the conveyor must present on the tick immediately before the relock \
             (got a present at tick {sb_tick}) — otherwise `steady_before` is not the phase \
             the relock is supposed to inherit"
        );
        assert!(
            fifo.relock_presents.len() > relocks_before_episode,
            "the forced relock never presented within {SETTLE_TICKS} ticks, so the episode \
             measured nothing. A working nearest-anchor selection presents immediately, or \
             after a single hold when the grid step leaves nothing due yet; a selection that \
             jumps to the newest QUEUED frame drains the delay line and starves the FIFO \
             instead."
        );
        let (_relock_tick, relock_age) = *fifo
            .relock_presents
            .last()
            .expect("the forced relock must have presented a frame");
        Episode1003 {
            steady_before: sb_age,
            relock_age,
            anchor_before,
            anchor_after: fifo.phase_anchor_ns,
        }
    }

    /// #1003 ACCEPTANCE 1 — the fix. Five forced re-lock episodes, fired at render-tick
    /// slews that STRADDLE the #940 floor-pin step point (including a one-nanosecond pair
    /// either side of it), must all land the SAME release phase as the conveyor they
    /// interrupted, within well under half a frame.
    ///
    /// This is the Tier-0 form of the ticket's acceptance item 1 ("relock lands the SAME
    /// release phase (±<10 ms) across >=5 forced re-lock episodes at latency >=900 ms").
    #[test]
    fn forced_relock_preserves_release_phase_within_10ms_across_5_episodes_1003() {
        const TOL_NS: i64 = 10_000_000; // the ticket's own ±10 ms bar
        let mut deltas = Vec::new();
        for slew in SLEW_SWEEP_1003 {
            let ep = episode_1003(SelectionStrategy::PhaseAnchored, slew);
            let delta = ep.relock_age - ep.steady_before;
            assert!(
                delta.abs() < TOL_NS,
                "slew {slew} ns: the forced relock minted a release phase {delta} ns away \
                 from the conveyor it interrupted (steady {} ns -> relock {} ns). The whole \
                 point of the phase anchor is that a relock corrects DEPTH, never PHASE.",
                ep.steady_before,
                ep.relock_age
            );
            deltas.push(delta);
        }
        let spread =
            deltas.iter().max().expect("5 episodes") - deltas.iter().min().expect("5 episodes");
        assert!(
            spread < TOL_NS,
            "the five episodes minted release phases spanning {spread} ns ({deltas:?}) — \
             they must agree to within {TOL_NS} ns, or the A/V offset still steps between \
             lock episodes and the ±20 ms gate stays a lottery"
        );
    }

    /// #1003 DEFECT LOCK — the same five episodes under the DEPLOYED instant-sampled rule
    /// step a WHOLE FRAME. This is the mechanism the ticket measured in the field
    /// (−64.5 / +56..63 ms episode steps), reproduced deterministically in Tier-0.
    ///
    /// Kept permanently, pinned explicitly to [`SelectionStrategy::NewestDue`]: it documents
    /// exactly WHY the selection rule may never go back to newest-due, and it is the control
    /// that proves the acceptance test above is measuring something real rather than a
    /// scenario too gentle to expose either rule.
    #[test]
    fn instant_sampled_selection_steps_a_whole_frame_at_the_grid_edges_1003() {
        let deltas: Vec<i64> = SLEW_SWEEP_1003
            .iter()
            .map(|&slew| {
                let ep = episode_1003(SelectionStrategy::NewestDue, slew);
                ep.relock_age - ep.steady_before
            })
            .collect();
        let spread =
            deltas.iter().max().expect("5 episodes") - deltas.iter().min().expect("5 episodes");
        // A "whole frame" with generous slack for the 33 ns/frame sender-grid beat.
        let whole_frame = (I30 * 4 / 5) as i64;
        assert!(
            spread >= whole_frame,
            "the deployed newest-due rule was expected to step ~a whole frame across the \
             grid edge, but the episodes only spread {spread} ns ({deltas:?}). Either the \
             scenario no longer straddles the #940 floor-pin step point (so the acceptance \
             test above proves nothing), or the selection rule already changed."
        );
        // The damning pair: SLEW_SWEEP_1003[2] and [3] differ by ONE NANOSECOND of render-
        // tick slew, and the floor-pin step point sits exactly between them.
        let one_ns_step = (deltas[2] - deltas[3]).abs();
        assert!(
            one_ns_step >= whole_frame,
            "one NANOSECOND of render-tick slew ({} ns vs {} ns) moved the release phase by \
             {one_ns_step} ns — this is Edge 1, and it is what makes every lock episode a \
             fresh ±1-frame dice roll. Expected >= {whole_frame} ns.",
            SLEW_SWEEP_1003[2],
            SLEW_SWEEP_1003[3]
        );
    }

    /// #1003 — a relock must still do its JOB. The phase anchor changes WHICH frame a relock
    /// selects; it must not stop the relock shedding the queue depth a stall's burst built
    /// up, or the issue-859 backlog branch becomes decorative and the FIFO parks overshot.
    #[test]
    fn relock_still_sheds_backlog_depth_while_preserving_phase_1003() {
        const STALL_AT: u64 = 300;
        const STALL_TICKS: u64 = 14; // >= the issue-859 margin, so the branch really fires
        const N_TICKS: u64 = 360;
        let base = step_aligned_base_1003();
        let mut fifo = SimFifo1009::with(SelectionStrategy::PhaseAnchored, RESERVE_NS_1003);
        let mut next_emit = 0u64;
        let mut steady_before = 0i64;
        let mut depth_at_resume = 0usize;
        let mut relock_age = None;
        let mut depth_after_relock = None;
        for k in 0..N_TICKS {
            let true_now = base + k * I30;
            loop {
                let e = base + next_emit * SENDER_GRID_1003;
                if e + NET_NS_1003 > true_now {
                    break;
                }
                fifo.queue.push_back(e);
                next_emit += 1;
            }
            if (STALL_AT..STALL_AT + STALL_TICKS).contains(&k) {
                // RENDER STALL: frames keep arriving, the compositor never ticks. On resume
                // the queue is STALL_TICKS deep above its steady depth — the "stall's burst"
                // the backlog branch exists for.
                if k == STALL_AT {
                    steady_before = fifo.presented.last().map(|(_, a)| *a).unwrap_or(0);
                }
                continue;
            }
            if k == STALL_AT + STALL_TICKS {
                depth_at_resume = fifo.queue.len();
            }
            let relocks_before = fifo.relocks;
            fifo.tick(k, true_now, true_now);
            if fifo.relocks > relocks_before && depth_after_relock.is_none() {
                depth_after_relock = Some(fifo.queue.len());
                relock_age = fifo.relock_presents.last().map(|(_, a)| *a);
            }
        }
        let threshold = backlog_relock_threshold(RESERVE_MS_1003, 30, 1, 1);
        assert!(
            depth_at_resume as u64 > threshold,
            "the stall left a queue of only {depth_at_resume} frames, not deeper than the \
             issue-859 backlog threshold ({threshold}), so the backlog branch is never \
             exercised. Either the stall no longer builds a real burst, or the relock \
             selection is draining the delay line it is supposed to preserve."
        );
        assert!(
            fifo.relocks >= 1,
            "the backlog branch never fired — the issue-859 trigger must be UNCHANGED by \
             #1003 (the anchor changes WHICH frame is selected, never WHEN a relock happens)"
        );
        let depth_after = depth_after_relock.expect("a relock fired, so a depth was recorded");
        assert!(
            (depth_after as u64) <= threshold,
            "the relock left the queue at {depth_after} frames, still above the backlog \
             threshold {threshold} — nearest-anchor selection must still SHED depth by whole \
             frames (it erases every frame older than the one it selects)"
        );
        assert!(
            fifo.dropped > 0,
            "depth was shed without dropping anything — the erase-into-dropped_due \
             accounting is what makes the shed VISIBLE (the pre-#401 silent-erase defect)"
        );
        let relock_age = relock_age.expect("a relock fired, so an age was recorded");
        let delta = relock_age - steady_before;
        assert!(
            delta.abs() < 10_000_000,
            "the backlog relock shed depth but moved the release phase by {delta} ns \
             (steady {steady_before} ns -> relock {relock_age} ns) — depth correction must \
             not cost phase"
        );
    }

    /// #1003 — the anchor's LIFECYCLE, the part that is easy to get subtly wrong.
    ///
    /// * the relock path (ACQUIRE / BACKLOG) neither clears nor redefines it — that is what
    ///   lets a re-ACQUIRE, including the one a self-heal triggers, inherit a phase at all;
    /// * a GAP RESYNC RE-DERIVES it from the frame actually put on air (upstream skipped
    ///   stamps, so the pre-gap value must not be carried forward);
    /// * a backward-step regime end CLEARS it — the receiver wall clock moved, so every
    ///   `wall − ts` age sampled before the correction is wrong by the step, and the
    ///   configured-latency fallback is the only honest target (this is exactly #1009's own
    ///   self-heal contract: re-acquire the CONFIGURED hold).
    #[test]
    fn anchor_survives_relock_rederives_on_gap_and_clears_on_backward_regime_end_1003() {
        // --- the target-selection contract itself -----------------------------------
        assert_eq!(
            relock_anchor_age_ns(948_000_000, RESERVE_MS_1003),
            948_000_000,
            "a SET anchor is the selection target"
        );
        assert_eq!(
            relock_anchor_age_ns(0, RESERVE_MS_1003),
            RESERVE_NS_1003,
            "an UNSET anchor (0, the C bzalloc zero-init) falls back to the CONFIGURED \
             latency — the phase the wall-deadline path would have produced anyway"
        );
        // The FLOOR (review finding): an anchor SMALLER than the configured hold would
        // target the live edge and erase the whole delay line in one relock. Nothing in
        // the types prevents one (phase_anchor_from_present saturates), so bound it.
        assert_eq!(
            relock_anchor_age_ns(1_000_000, RESERVE_MS_1003),
            RESERVE_NS_1003,
            "an anchor BELOW the configured hold must be floored at it — an anchor near 0 \
             targets the live edge, and the relock would erase the entire delay line"
        );

        // --- the relock path must PRESERVE the anchor -------------------------------
        let ep = episode_1003(SelectionStrategy::PhaseAnchored, 0);
        assert_ne!(
            ep.anchor_before, 0,
            "setup: the conveyor must have established an anchor before the forced relock"
        );
        assert_eq!(
            ep.anchor_after, ep.anchor_before,
            "the ACQUIRE / relock path must neither CLEAR nor REDEFINE the anchor — a relock \
             INHERITS the phase. Redefining it here would let each episode re-mint a phase, \
             which is the whole defect; clearing it would drop every re-ACQUIRE (including \
             the one after a self-heal) back to the edge-ridden fallback."
        );

        // --- a GAP RESYNC RE-DERIVES it ---------------------------------------------
        let mut fifo = SimFifo1009::with(SelectionStrategy::PhaseAnchored, RESERVE_NS_1003);
        let wall = BASE_1009 + 1_000 * I30;
        let stale_anchor = 700_000_000; // a deliberately WRONG pre-gap value
        fifo.phase_anchor_ns = stale_anchor;
        // Upstream skipped stamps: the head sits BEYOND the locked boundary, but has aged
        // well past the reserve — the GAP RESYNC branch, not STEADY and not a relock.
        let head = wall - RESERVE_NS_1003 - 2 * I30;
        fifo.queue.push_back(head);
        fifo.boundary = head - 5 * I30;
        fifo.tick(0, wall, wall);
        assert_eq!(
            fifo.presented.len(),
            1,
            "setup: the GAP RESYNC branch must have presented the aged head"
        );
        assert_eq!(
            fifo.relock_presents.len(),
            0,
            "setup: this must be a GAP present, not a relock"
        );
        assert_eq!(
            fifo.phase_anchor_ns,
            wall - head,
            "a GAP RESYNC must RE-DERIVE the anchor from the frame it actually put on air"
        );
        assert_ne!(
            fifo.phase_anchor_ns, stale_anchor,
            "a GAP RESYNC must never carry the pre-gap anchor forward — upstream skipped \
             stamps, so that age describes a timeline that no longer exists"
        );

        // --- a backward-step regime end CLEARS it -----------------------------------
        let mut fifo = SimFifo1009::with(SelectionStrategy::PhaseAnchored, RESERVE_NS_1009);
        run_sim_1009(
            &mut fifo,
            900,
            I30, // exact-rate sender: the step itself makes every tick due==0
            1_000_000,
            |_| 0,
            |k| {
                if (400..550).contains(&k) {
                    -500_000_000
                } else {
                    0
                }
            },
        );
        assert!(
            fifo.selfheals >= 1,
            "setup: the 500 ms backward step must end in a SELF-HEAL (#1009)"
        );
        assert!(
            fifo.selfheal_anchor_clears >= 1,
            "the self-heal ended a backward-step regime without clearing a SET phase anchor. \
             The receiver wall clock moved by the step, so every `wall - ts` age sampled \
             before the correction is wrong by exactly that much — re-acquiring against it \
             would re-establish the hold at a phase off by the whole clock step."
        );
    }

    /// #1003 REGRESSION (adversarial review finding) — a latency SETPOINT DECREASE must still
    /// converge, and must not turn the backlog branch into a per-tick relock storm.
    ///
    /// The phase anchor IS the conveyor's current age, so after a knob decrease it still
    /// describes the OLD, deeper hold. `relock_select_nearest` then returns index 0 (the
    /// target sits at or before the queue head), `release` is 1, and the relock sheds
    /// NOTHING — while the lowered latency has already dropped
    /// [`backlog_relock_threshold`] below the unchanged depth, so the branch qualifies on
    /// EVERY tick. Worse, the backlog branch pre-empts STEADY, so `drain_eligible` is never
    /// set and the issue-859/998 settle-back drain — the only other convergence path — never
    /// runs either. Measured before the fix: the hold stayed at the OLD 933 ms forever with
    /// 800 relocks in 800 ticks and zero frames shed, which is exactly the
    /// `relocks`-as-a-useless-health-signal state issue 859 removed.
    ///
    /// Two guards make it structurally impossible, and both are mirrored in the C:
    /// the setpoint change CLEARS the anchor (the remembered age describes a hold that no
    /// longer exists), and a relock that would shed nothing treats the anchor as STALE,
    /// clears it and re-selects against the configured latency.
    #[test]
    fn latency_setpoint_decrease_converges_without_a_relock_storm_1003() {
        const SETTLE: u64 = 300;
        const AFTER: u64 = 800;
        const NEW_MS: u32 = 400;
        let base = step_aligned_base_1003();
        let mut fifo = SimFifo1009::with(SelectionStrategy::PhaseAnchored, RESERVE_NS_1003);
        let mut next_emit = 0u64;
        for k in 0..SETTLE + AFTER {
            let true_now = base + k * I30;
            loop {
                let e = base + next_emit * SENDER_GRID_1003;
                if e + NET_NS_1003 > true_now {
                    break;
                }
                fifo.queue.push_back(e);
                next_emit += 1;
            }
            if k == SETTLE {
                fifo.set_reserve_ms(NEW_MS);
            }
            fifo.tick(k, true_now, true_now);
        }
        let tail: Vec<i64> = fifo
            .presented
            .iter()
            .filter(|(k, _)| *k >= SETTLE + AFTER - 30)
            .map(|(_, age)| *age)
            .collect();
        assert!(!tail.is_empty(), "nothing presented in the settled tail");
        let target = NEW_MS as i64 * 1_000_000;
        let tol = 2 * I30 as i64;
        for age in &tail {
            assert!(
                (age - target).abs() <= tol,
                "after lowering the hold {RESERVE_MS_1003} ms -> {NEW_MS} ms the FIFO still \
                 presents frames {age} ns old (target {target} ns +/- 2 intervals) — the \
                 setpoint decrease never converged. The stale anchor makes every relock shed \
                 ZERO frames, and because the backlog branch pre-empts STEADY the \
                 settle-back drain never runs either."
            );
        }
        assert!(
            fifo.relocks <= 30,
            "the backlog branch fired {} times in {AFTER} ticks after the setpoint change — \
             a per-tick relock storm. Each firing shed nothing and logged a per-event line, \
             which is precisely the useless-`relocks`-counter state issue 859 removed.",
            fifo.relocks
        );
    }

    /// #1003 — the TIE contract, pinned directly. `relock_select_nearest` resolves an exactly
    /// equidistant target toward the OLDER frame (the lower index, a strict `<`).
    ///
    /// This is not cosmetic. A `<=` would prefer the NEWER frame, and since the target sweeps
    /// forward continuously, a target sitting near the midpoint would flip between the two
    /// neighbours from episode to episode — reintroducing exactly the kind of edge this whole
    /// ticket exists to remove, in the one place a nearest-neighbour rule can still have one.
    ///
    /// Worth keeping because a tie is genuinely reachable on this rig, not a contrivance: both
    /// sender grids (33,333,300 ns and 16,666,600 ns) are EVEN, so `i*grid + grid/2` is an
    /// exact integer nanosecond that a wall instant can land on.
    #[test]
    fn relock_selection_resolves_an_exact_tie_toward_the_older_frame_1003() {
        const G: u64 = SENDER_GRID_1003;
        let q: Vec<u64> = (0..6).map(|i| BASE_1009 + i * G).collect();
        // Target EXACTLY midway between stamp 2 and stamp 3.
        let target = BASE_1009 + 2 * G + G / 2;
        let age = 900_000_000u64;
        let sel = relock_select_nearest(&q, target + age, age);
        assert_eq!(
            sel, 2,
            "an exactly equidistant target must select the OLDER neighbour (index 2), not the \
             newer one (index 3). A non-strict compare here lets the choice oscillate between \
             neighbours as the target sweeps, which is an edge — the exact class of defect \
             #1003 removes everywhere else."
        );
        // And the contract holds one nanosecond either side of the midpoint, so the tie is a
        // genuine boundary rather than a wide flat region.
        assert_eq!(
            relock_select_nearest(&q, target - 1 + age, age),
            2,
            "one ns BEFORE the midpoint is unambiguously nearer the older frame"
        );
        assert_eq!(
            relock_select_nearest(&q, target + 1 + age, age),
            3,
            "one ns AFTER the midpoint is unambiguously nearer the newer frame"
        );
    }

    // ------------------------------------------------------------------------------------
    // #1049 — bounded PHASE CONVERGENCE on the steady conveyor.
    // ------------------------------------------------------------------------------------

    /// The comparator is `wall_now - locked_boundary` against `reserve + interval/n + hysteresis`
    /// AND the shared drain throttle. Below the threshold: inert. Above it, once the throttle has
    /// elapsed: fires.
    #[test]
    fn converge_fires_only_above_reserve_plus_quantum_plus_hysteresis_1049() {
        let latency_ms = 20u32;
        let reserve = latency_ms as u64 * 1_000_000;
        let n = 2u32;
        let quantum = I30 / n as u64; // one SOURCE interval
        let threshold = reserve + quantum + PHASE_PIN_HYSTERESIS_NS;
        let wall = 1_000_000_000_000u64;
        // A fresh arrival (floor 0) so the target is the configured latency — this test is about
        // the reserve-based threshold; the floor path has its own test below.
        let newest = wall;
        // age == threshold -> NOT due (strict `>`).
        let boundary_at_threshold = wall - threshold;
        assert!(
            !should_converge_phase(wall, boundary_at_threshold, newest, latency_ms, I30, n, 100),
            "at exactly the threshold the shed must NOT fire (strict >, mirrors #998's own \
             boundary discipline)"
        );
        // age == threshold + 1 -> due (throttle satisfied).
        assert!(
            should_converge_phase(
                wall,
                boundary_at_threshold - 1,
                newest,
                latency_ms,
                I30,
                n,
                100
            ),
            "one ns above the threshold with the throttle elapsed must fire"
        );
        // A held age at the configured latency (no phase error) is far below the threshold.
        assert!(
            !should_converge_phase(wall, wall - reserve, newest, latency_ms, I30, n, 100),
            "a conveyor held AT the configured latency has nothing to converge"
        );
        // A held age one canvas frame over configured (the observed 1-frame rung) fires.
        assert!(
            should_converge_phase(
                wall,
                wall - (reserve + I30),
                newest,
                latency_ms,
                I30,
                n,
                100
            ),
            "a held age one full canvas frame over configured must converge"
        );
    }

    /// #1049 (review finding) — the target is `max(reserve, floor)`, floor = the freshest queued
    /// frame's on-air age. When the transport skew floors the achievable phase ABOVE the reserve,
    /// a reserve-only threshold would shed forever at the natural phase (a #998-class limit cycle);
    /// flooring the target keeps it inert until the phase exceeds the ACHIEVABLE floor by a quantum.
    #[test]
    fn converge_targets_max_reserve_floor_1049() {
        let wall = 1_000_000_000_000u64;
        let reserve_ms = 3u32; // the prod floor
        let n = 2u32;
        let quantum = I30 / n as u64;
        // A large transport skew: the freshest frame is 40 ms old, far above the 3 ms reserve.
        let skew = 40_000_000u64;
        let newest = wall - skew;
        // The conveyor naturally holds ~floor + quantum (the maturation window). With a
        // reserve-only target that is WAY over reserve + quantum + hysteresis -> would shed
        // forever. With the floor-aware target it is at/below floor + quantum + hysteresis -> inert.
        let natural = wall - (skew + quantum); // one quantum above the floor — the natural phase
        assert!(
            !should_converge_phase(wall, natural, newest, reserve_ms, I30, n, 100),
            "at the natural phase (~floor + one quantum) with a large skew the shed must be INERT \
             — a reserve-only threshold would limit-cycle here (#998 class)"
        );
        // A genuine error ABOVE the achievable floor by more than a quantum + hysteresis DOES fire.
        let errored = wall - (skew + 2 * I30);
        assert!(
            should_converge_phase(wall, errored, newest, reserve_ms, I30, n, 100),
            "a phase two full canvas frames above the achievable floor still converges"
        );
        // The floor never drives the target BELOW reserve: a deep reserve with a tiny skew still
        // targets the reserve (floor < reserve -> target = reserve).
        let deep_ms = 200u32;
        let tiny_skew_newest = wall - 5_000_000; // 5 ms floor, well below the 200 ms reserve
        assert!(
            !should_converge_phase(
                wall,
                wall - deep_ms as u64 * 1_000_000,
                tiny_skew_newest,
                deep_ms,
                I30,
                n,
                100
            ),
            "floor below reserve -> target stays at reserve; a source held AT reserve is inert"
        );
    }

    /// The throttle is SHARED with the #859 drain (`DRAIN_MIN_TICK_INTERVAL`): a big phase error
    /// still sheds at most once per that many ticks.
    #[test]
    fn converge_is_throttled_by_the_shared_drain_counter_1049() {
        let wall = 1_000_000_000_000u64;
        let newest = wall; // floor 0 -> target = reserve
                           // A gross 3-frame overshoot — unambiguously over the threshold.
        let boundary = wall - (20 * 1_000_000 + 3 * I30);
        assert!(
            !should_converge_phase(
                wall,
                boundary,
                newest,
                20,
                I30,
                2,
                DRAIN_MIN_TICK_INTERVAL - 1
            ),
            "below the shared throttle interval the shed must wait, even for a large error"
        );
        assert!(
            should_converge_phase(wall, boundary, newest, 20, I30, 2, DRAIN_MIN_TICK_INTERVAL),
            "at the throttle interval the shed fires"
        );
    }

    /// The shed quantum is the SOURCE interval `interval/n`, so a 60-into-30 source's threshold
    /// sits one 60 Hz interval (not one canvas interval) above the target — the tightest bound
    /// that still clears the natural steady phase (the #998 lesson applied to phase).
    #[test]
    fn converge_quantum_is_the_source_interval_1049() {
        let wall = 1_000_000_000_000u64;
        let newest = wall; // floor 0 -> target = reserve
        let reserve = 20u64 * 1_000_000;
        // n=2: quantum = I30/2 = I60. An age of reserve + I60 + hysteresis is the boundary.
        let just_over = wall - (reserve + I30 / 2 + PHASE_PIN_HYSTERESIS_NS + 1);
        assert!(
            should_converge_phase(wall, just_over, newest, 20, I30, 2, 100),
            "n=2 threshold is reserve + I30/2 + hysteresis"
        );
        let just_under = wall - (reserve + I30 / 2 + PHASE_PIN_HYSTERESIS_NS);
        assert!(
            !should_converge_phase(wall, just_under, newest, 20, I30, 2, 100),
            "one ns below the n=2 threshold is inert"
        );
        // n=1 is gated off entirely (#1049 coordinator finding — the shed does not stick), so the
        // same held age that fired at n=2 is INERT on a 1:1 source regardless of the threshold.
        assert!(
            !should_converge_phase(wall, just_over, newest, 20, I30, 1, 100),
            "n=1 is below the N>=2 gate — inert regardless of the held age"
        );
    }

    /// No regression on the #1003 deep hold: a deep N>=2 source correctly held at 1000 ms has
    /// `S ≈ configured`, far below the threshold, so the shed is inert — by the SAME comparison
    /// that drives it, not a special case.
    #[test]
    fn converge_is_inert_at_a_correctly_held_deep_source_1049() {
        let wall = 1_000_000_000_000u64;
        let newest = wall - 8_000_000; // an 8 ms floor, far below the 1000 ms hold
        for jitter in [-2_000_000i64, 0, 2_000_000, 8_000_000] {
            // A deep N>=2 source held at 1000 ms ± ordinary sub-frame jitter.
            let age = (1000i64 * 1_000_000 + jitter) as u64;
            assert!(
                !should_converge_phase(wall, wall - age, newest, 1000, I30, 2, 1_000_000),
                "a deep N>=2 source held at ~configured (jitter {jitter} ns) must never converge"
            );
        }
        // Only a genuine >=1-frame walk over configured on a deep N>=2 source converges (the
        // shed sticks there — the multi-frame arrival sustains the fresher phase).
        assert!(
            should_converge_phase(
                wall,
                wall - (1000 * 1_000_000 + I30 + I30),
                newest,
                1000,
                I30,
                2,
                100
            ),
            "a deep N>=2 source that walked a full frame over configured self-corrects"
        );
    }

    /// #1049 (coordinator's live finding) — convergence is N>=2 ONLY. The deep n=1 stream source
    /// `NDI 2ME PGM` (30-into-30, configured 990 ms) sits at its natural grid-quantized hold
    /// ~1033 ms — one frame above configured at frac 0.7, ABOVE the reserve-based threshold — so a
    /// naive decision would shed it; but an n=1 shed does not stick (1 frame/tick can't sustain a
    /// fresher phase → shed-hold-shed limit cycle, the #998 signature). The decision MUST go INERT
    /// for n<2 while the SAME held age on an n>=2 source still converges (the shed sticks).
    #[test]
    fn convergence_is_n2_only_the_deep_n1_source_is_inert_1049() {
        // wall large enough that a 1033 ms subtraction never underflows.
        let wall = 2_000_000_000_000u64;
        let s = 1033 * 1_000_000; // the live natural hold at reserve 990, frac 0.7
        let boundary = wall - s;
        let newest = wall - 33_000_000; // freshest frame ~1 canvas frame old -> small floor
                                        // n=1: INERT (gated — the shed would oscillate). This is the RED→GREEN of the fix.
        assert!(
            !should_converge_phase(wall, boundary, newest, 990, I30, 1, 100),
            "a deep n=1 source at its natural grid-quantized hold must NOT shed — the n=1 shed \
             does not stick and limit-cycles (the live stream-box `NDI 2ME PGM` oscillation)"
        );
        // n=0 degenerate also gated (0 < 2), no divide-by-zero.
        assert!(
            !should_converge_phase(wall, boundary, newest, 990, I30, 0, 100),
            "n=0 (degenerate) is below the N>=2 gate -> inert"
        );
        // The SAME held age on an n>=2 source STILL converges — the gate is n-specific, not a
        // blanket disable, so the strih 60-into-30 convergence is untouched.
        assert!(
            should_converge_phase(wall, boundary, newest, 990, I30, 2, 100),
            "the same held age on an N>=2 source still converges (the shed sticks there) — the \
             strih ladder fix must stay intact"
        );
    }

    /// Degenerate inputs never fire and never panic.
    #[test]
    fn converge_degenerate_interval_or_unlocked_boundary_is_false_1049() {
        let wall = 1_000_000_000_000u64;
        let newest = wall;
        assert!(
            !should_converge_phase(wall, wall - 500_000_000, newest, 20, 0, 2, 100),
            "interval 0 (unknown video info) -> no convergence, no divide-by-zero"
        );
        assert!(
            !should_converge_phase(wall, 0, newest, 20, I30, 2, 100),
            "an UNLOCKED boundary (0, ACQUIRE) -> nothing to converge"
        );
        // A boundary AHEAD of wall (sender future-bias residue) saturates the age to 0 -> inert.
        assert!(
            !should_converge_phase(wall, wall + I30, newest, 20, I30, 2, 100),
            "a boundary ahead of wall saturates to age 0 -> inert"
        );
        // A newest_stamp AHEAD of wall (floor saturates to 0) -> target = reserve, still evaluates.
        assert!(
            should_converge_phase(
                wall,
                wall - (20 * 1_000_000 + 2 * I30),
                wall + I30,
                20,
                I30,
                2,
                100
            ),
            "a newest stamp ahead of wall floors to 0 -> target = reserve, no panic"
        );
        // source_multiple 0 (degenerate) is BELOW the N>=2 gate -> inert; no divide-by-zero.
        assert!(
            !should_converge_phase(
                wall,
                wall - (20 * 1_000_000 + 2 * I30),
                newest,
                20,
                I30,
                0,
                100
            ),
            "source_multiple 0 (degenerate) is below the N>=2 gate -> inert"
        );
    }

    // ---- The behavioural RED->GREEN: a faithful N>=2 conveyor (mirrors the C
    // `genlock_release_tick` N>=2 STEADY branch and the probe `ReleaseCadence::tick`), driven with
    // the probe's own arrival model. WITHOUT the shed an injected phase persists forever; WITH it
    // the phase converges to within one canvas frame of configured within seconds. ----

    struct SimConveyor1049 {
        boundary: u64,
        last_known_n: u32,
        ticks_since_drain: u64,
        converge: bool,
        drops: u64,
    }

    impl SimConveyor1049 {
        fn new(converge: bool) -> Self {
            Self {
                boundary: 0,
                last_known_n: 0,
                ticks_since_drain: 0,
                converge,
                drops: 0,
            }
        }

        /// Structural source multiple from the front stamp grid (mirrors
        /// `effective_source_multiple` with the sticky-N latch).
        fn eff_n(&mut self, q: &std::collections::VecDeque<u64>) -> u32 {
            let s: Vec<u64> = q.iter().take(8).copied().collect();
            let measured = match source_interval_from_stamps(&s) {
                Some(iv) if iv > 0 => Some(((I30 as f64 / iv as f64).round() as u32).max(1)),
                _ => None,
            };
            match measured {
                Some(m) => {
                    self.last_known_n = m;
                    m
                }
                None => self.last_known_n.max(1),
            }
        }

        /// One render tick; returns the presented on-air age (`wall - stamp`) if any.
        fn tick(
            &mut self,
            wall: u64,
            reserve_ms: u32,
            q: &mut std::collections::VecDeque<u64>,
        ) -> Option<i64> {
            if q.is_empty() {
                return None;
            }
            let deadline =
                phase_pinned_deadline(wall.saturating_sub(reserve_ms as u64 * 1_000_000), I30);
            let due = q
                .iter()
                .take_while(|&&ts| phase_pinned_is_due(ts, deadline))
                .count();
            if self.boundary == 0 {
                self.last_known_n = 0;
                self.ticks_since_drain = 0;
                if due == 0 {
                    return None;
                }
                let qv: Vec<u64> = q.iter().copied().collect();
                let sel = relock_select_nearest(&qv, wall, relock_anchor_age_ns(0, reserve_ms));
                for _ in 0..sel {
                    q.pop_front();
                }
                let ts = q.pop_front().unwrap();
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            if *q.front().unwrap() <= self.boundary {
                let n = self.eff_n(q);
                let old_boundary = self.boundary;
                if n >= 2 {
                    let mature_deadline = self.boundary + I30 / 2;
                    let matured_n = q
                        .iter()
                        .take_while(|&&ts| ts <= mature_deadline)
                        .count()
                        .max(1);
                    for _ in 0..matured_n - 1 {
                        q.pop_front();
                    }
                    // The #1049 phase shed (converge_eligible on the N>=2 path; the depth drain is
                    // N==1-only). q.front() is now the would-be-presented newest matured; a shed
                    // drops it and presents the next (one SOURCE interval fresher).
                    self.maybe_shed(wall, old_boundary, reserve_ms, n, false, q);
                    let ts = q.pop_front().unwrap();
                    self.boundary = ts + I30;
                    return Some(wall as i64 - ts as i64);
                }
                // N==1: the #859 depth drain AND the #1049 phase shed both eligible.
                self.maybe_shed(wall, old_boundary, reserve_ms, 1, true, q);
                let ts = q.pop_front().unwrap();
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            if deadline >= *q.front().unwrap() {
                let ts = q.pop_front().unwrap();
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            None
        }

        /// Shed at most ONE extra frame: the #859 depth drain (N==1) OR the #1049 phase converge
        /// (both paths), sharing `ticks_since_drain`. Mirror of the C tail's shed block.
        fn maybe_shed(
            &mut self,
            wall: u64,
            old_boundary: u64,
            reserve_ms: u32,
            n: u32,
            drain_eligible: bool,
            q: &mut std::collections::VecDeque<u64>,
        ) {
            // Mirror of the C tail: the #859 depth drain (byte-identical) then the #1049 phase
            // converge, sharing `ticks_since_drain`. No explicit "already shed" flag is needed —
            // after a drain resets the counter to 0, should_converge_phase reads ticks < the
            // throttle interval and returns false, so the two can never both fire this tick.
            if drain_eligible {
                if should_drain_one(q.len() as u64, reserve_ms, 60, 1, self.ticks_since_drain)
                    && q.len() > 1
                {
                    q.pop_front();
                    self.drops += 1;
                    self.ticks_since_drain = 0;
                } else {
                    self.ticks_since_drain += 1;
                }
            }
            // `self.converge` stands in for "the #1049 block is present"; when it is, both STEADY
            // paths are converge_eligible (the C `if (converge_eligible)`), and the block also
            // maintains the shared counter on the N>=2 path (`!drain_eligible`, where the drain
            // block did not run). The floor (freshest queued stamp) is `q.back()` — erasing OLDER
            // matured frames never touches it, so it is the same freshest arrival the C reads as
            // `array[num-1]`.
            if self.converge {
                let newest = *q
                    .back()
                    .expect("STEADY branch is entered with a non-empty queue");
                if should_converge_phase(
                    wall,
                    old_boundary,
                    newest,
                    reserve_ms,
                    I30,
                    n,
                    self.ticks_since_drain,
                ) && q.len() > 1
                {
                    q.pop_front();
                    self.drops += 1;
                    self.ticks_since_drain = 0;
                } else if !drain_eligible {
                    self.ticks_since_drain += 1;
                }
            }
        }
    }

    /// Drive the conveyor: exact 2-per-tick 60 Hz arrivals (no over-rate), a `skew_ns` transport
    /// skew + deterministic jitter + ±2 ms render slew (the probe's `run_cadence_sim_ratio`
    /// model). At `inject_tick` shove the locked boundary back `inject_frames` canvas frames — the
    /// phase a walk event (GAP RESYNC / sticky-N crawl / connect-burst ACQUIRE) mints. Returns
    /// (per-tick presented ages, total extra drops).
    ///
    /// #1049 (review finding): the transport skew is a PARAMETER, not fixed at 8 ms — the natural
    /// steady on-air age is FLOORED by it (a frame cannot be presented before it arrives), so the
    /// convergence threshold must clear `max(reserve, floor)`, and a too-narrow skew envelope
    /// hides a spurious-shed limit cycle at the rig's real ~20 ms skew on shallow reserves.
    fn run_conveyor_1049(
        reserve_ms: u32,
        src_interval: u64,
        skew_ns: u64,
        converge: bool,
        inject_frames: i64,
        inject_tick: u64,
        n_ticks: u64,
    ) -> (Vec<(u64, i64)>, u64) {
        const BASE: u64 = 1_000_000_000_000;
        let mut c = SimConveyor1049::new(converge);
        let mut q: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut next = 0u64;
        let skew = skew_ns;
        let mut ages = Vec::new();
        for k in 0..n_ticks {
            let slew: i64 = if k % 2 == 0 { 2_000_000 } else { -2_000_000 };
            let wall = (BASE + k * I30).saturating_add_signed(slew);
            loop {
                let arr = BASE + next * src_interval + skew + ((next * 2_654_435_761) % 8_000_001)
                    - 4_000_000;
                if arr > wall {
                    break;
                }
                q.push_back(BASE + next * src_interval);
                next += 1;
            }
            if let Some(age) = c.tick(wall, reserve_ms, &mut q) {
                ages.push((k, age));
            }
            if k == inject_tick && c.boundary != 0 {
                c.boundary = (c.boundary as i64 - inject_frames * I30 as i64) as u64;
            }
        }
        (ages, c.drops)
    }

    fn tail_mean_1049(ages: &[(u64, i64)], from: u64, to: u64) -> f64 {
        let v: Vec<i64> = ages
            .iter()
            .filter(|(k, _)| *k >= from && *k < to)
            .map(|(_, a)| *a)
            .collect();
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<i64>() as f64 / v.len() as f64
        }
    }

    /// The behavioural proof: on a 60-into-30 conveyor, an injected 2-canvas-frame presentation
    /// phase PERSISTS forever WITHOUT the shed, and CONVERGES to within one canvas frame of
    /// configured (with only a handful of bounded drops) WITH it — while a run with no injection
    /// never sheds (no limit cycle), and the deep #1003 hold is untouched.
    #[test]
    fn steady_conveyor_persists_an_injected_phase_without_convergence_and_sheds_it_with_1049() {
        let reserve = 20u32;
        let inject = 2i64;
        let skew = 8_000_000u64;
        // RED (no convergence): the injected phase stays elevated above the natural hold.
        let (red, red_drops) = run_conveyor_1049(reserve, I60, skew, false, inject, 200, 900);
        let red_pre = tail_mean_1049(&red, 170, 200);
        let red_final = tail_mean_1049(&red, 800, 900);
        assert_eq!(
            red_drops, 0,
            "without convergence there is no extra shed at all"
        );
        assert!(
            red_final > red_pre + I30 as f64 * 0.8,
            "RED: the injected phase must PERSIST — final {red_final} not meaningfully above the \
             pre-inject hold {red_pre} (expected ~+{inject} canvas frames)"
        );

        // GREEN (convergence): the phase sheds back to within one canvas frame of configured.
        let (green, green_drops) = run_conveyor_1049(reserve, I60, skew, true, inject, 200, 900);
        let green_final = tail_mean_1049(&green, 800, 900);
        let excess_frames = (green_final - reserve as f64 * 1_000_000.0) / I30 as f64;
        assert!(
            excess_frames < 1.0,
            "GREEN: convergence must bring the held age to within one canvas frame of configured, \
             got {excess_frames:.2} frames excess (final {green_final} ns)"
        );
        assert!(
            green_final < red_final - I30 as f64 * 0.8,
            "GREEN: convergence must drop the held age well below the un-converged RED value \
             (green {green_final} vs red {red_final})"
        );
        assert!(
            (1..=6).contains(&green_drops),
            "GREEN: the shed is bounded — a couple of drops to close a 2-frame error, never a \
             storm (got {green_drops})"
        );
    }

    /// No limit cycle: a conveyor with convergence ON but NO injected error never sheds a single
    /// frame in steady state, across every rig latency AND every rig transport skew and both
    /// source multiples — the threshold sits above the natural steady phase everywhere (the #998
    /// lesson honoured on BOTH axes).
    ///
    /// #1049 (review finding): the SKEW sweep is load-bearing. The natural on-air age is floored
    /// by the transport skew, so a reserve-only threshold spuriously sheds forever when
    /// `skew > reserve + interval/n + hysteresis` (measured live: 20 ms skew at the 3 ms prod
    /// floor, up to 59 ms on strih `NDI cam5`). This sweep is exactly the envelope that exposes
    /// it — an earlier revision with only an 8 ms skew passed while limit-cycling at 15-30 ms.
    #[test]
    fn convergence_never_sheds_at_the_natural_steady_phase_1049() {
        for skew_ms in [8u64, 15, 20, 30, 59] {
            for reserve in [3u32, 8, 20, 26, 36] {
                for (src, label) in [(I60, "60-into-30"), (I30, "30-into-30")] {
                    let (_ages, drops) =
                        run_conveyor_1049(reserve, src, skew_ms * 1_000_000, true, 0, 100_000, 900);
                    assert_eq!(
                        drops, 0,
                        "skew {skew_ms} ms / reserve {reserve} ms {label}: convergence sheds \
                         NOTHING at the natural steady phase — a spurious shed here is a #998-style \
                         limit cycle (the target must clear max(reserve, floor), not reserve alone)"
                    );
                }
            }
        }
    }

    /// The deep #1003 hold is untouched: a 1000 ms 1:1 source with no injected error never sheds.
    #[test]
    fn deep_1003_hold_is_untouched_by_convergence_1049() {
        let (_ages, drops) = run_conveyor_1049(1000, I30, 8_000_000, true, 0, 200, 1200);
        assert_eq!(
            drops, 0,
            "the deep 1000 ms hold must never converge (S == configured) — no #1003 regression"
        );
    }

    // -----------------------------------------------------------------------------------------
    // #1161 — the STAGE-2 ACQUIRE BRACKETING GATE (relock_acquire_should_hold).
    // -----------------------------------------------------------------------------------------

    /// The RED reproduction of #1161: a re-acquire after a pin INCREASE (reserve now 53 ms) over a
    /// still-shallow queue (oldest only 20 ms old) must HOLD so the queue can deepen to the raised
    /// target — otherwise the acquire lands one canvas frame below target and the downward-only
    /// shed can never raise it. The pre-fix behaviour (no bracketing) never holds → the frame
    /// stays put. This is the test that fails against the [red] stub and passes against the fix.
    #[test]
    fn holds_while_the_queue_has_not_deepened_to_the_reserve_1161() {
        let reserve = 53u64 * 1_000_000;
        assert!(
            relock_acquire_should_hold(20 * 1_000_000, reserve, I30, 0),
            "#1161: an ACQUIRE over a queue whose oldest frame (20 ms) is younger than the raised \
             reserve (53 ms) must HOLD to let the FIFO deepen, not acquire at the shallow floor."
        );
    }

    /// Once the oldest queued frame has aged to (or past) the reserve, a frame at the target depth
    /// exists for the selection to land on → acquire now (do NOT hold).
    #[test]
    fn acquires_once_the_oldest_frame_reaches_the_reserve_1161() {
        let reserve = 53u64 * 1_000_000;
        assert!(
            !relock_acquire_should_hold(54 * 1_000_000, reserve, I30, 0),
            "#1161: with the oldest frame (54 ms) aged past the raised reserve (53 ms) the gate \
             must acquire — the queue has bracketed the target."
        );
    }

    /// The boundary is inclusive: oldest age EXACTLY == reserve acquires (a frame at the target
    /// exists), mirroring every other `>=`/`<=` due comparison in this module.
    #[test]
    fn acquires_exactly_at_the_reserve_boundary_1161() {
        let reserve = 40u64 * 1_000_000;
        assert!(
            !relock_acquire_should_hold(reserve, reserve, I30, 0),
            "#1161: oldest age == reserve must acquire (inclusive boundary)."
        );
    }

    /// The fail-open cap bounds the hold: `ceil(reserve/interval) + ACQUIRE_BRACKET_FAILOPEN_TICKS`
    /// ticks. At reserve 53 ms, interval 33.3 ms → ceil(53/33.3)=2, cap = 2 + 3 = 5. The gate holds
    /// for ticks 0..=4 (queue still shallow) then force-acquires at tick 5 rather than freezing —
    /// so a pathological never-deepening queue degrades to today's acquire, no new hold-collapse.
    #[test]
    fn fail_open_cap_forces_acquire_after_the_bounded_hold_1161() {
        let reserve = 53u64 * 1_000_000;
        let young = 20u64 * 1_000_000; // never reaches the reserve on its own in this test
        let cap = reserve.div_ceil(I30) + ACQUIRE_BRACKET_FAILOPEN_TICKS; // = 5
        assert_eq!(
            cap, 5,
            "#1161: sanity — the cap derivation is ceil(53/33.3)+3 = 5"
        );
        assert!(
            relock_acquire_should_hold(young, reserve, I30, cap - 1),
            "#1161: one tick below the cap must still HOLD"
        );
        assert!(
            !relock_acquire_should_hold(young, reserve, I30, cap),
            "#1161: at the fail-open cap the gate must force-acquire (never freeze forever)"
        );
    }

    /// A degenerate (0) interval — unknown video info — must never hold; fail open to today's
    /// acquire, never divide by zero computing the cap.
    #[test]
    fn degenerate_interval_never_holds_1161() {
        assert!(
            !relock_acquire_should_hold(1_000_000, 53 * 1_000_000, 0, 0),
            "#1161: a 0 interval must fail open (never hold, never divide by zero)."
        );
    }

    /// Inert at the production 3 ms pin: once a normal 60 fps frame (~16 ms old) is queued, the
    /// oldest age already exceeds 3 ms, so the gate acquires immediately — it never adds a hold to
    /// the default production path (the ACQUIRE branch is only entered at cold start / a forced
    /// re-acquire, and there the first real frame is already older than 3 ms).
    #[test]
    fn inert_at_the_production_pin_once_a_normal_frame_is_queued_1161() {
        let reserve = 3u64 * 1_000_000;
        assert!(
            !relock_acquire_should_hold(16 * 1_000_000, reserve, I60, 0),
            "#1161: at the 3 ms production pin a normal queued frame (16 ms) acquires immediately — \
             the gate is inert on the production path."
        );
    }

    /// End-to-end shape: a from-shallow re-acquire on an N>=2 (60 fps) source into a 30 fps canvas.
    /// The oldest frame ages one canvas interval per tick from a shallow 4 ms; the gate HOLDs each
    /// tick until the oldest brackets the raised 90 ms reserve, THEN acquires — and the whole hold
    /// stays within the fail-open cap (never trips it in the healthy case).
    #[test]
    fn bracketing_gate_holds_from_shallow_then_acquires_within_the_cap_1161() {
        let reserve = 90u64 * 1_000_000;
        let cap = reserve.div_ceil(I30) + ACQUIRE_BRACKET_FAILOPEN_TICKS;
        let mut oldest_age = 4u64 * 1_000_000; // shallow, just re-acquired at the old pin depth
        let mut acquired_at: Option<u64> = None;
        for (ticks_held, tick) in (0..40u64).enumerate() {
            if !relock_acquire_should_hold(oldest_age, reserve, I30, ticks_held as u64) {
                acquired_at = Some(tick);
                break;
            }
            oldest_age += I30; // the queue deepens one canvas interval per tick while holding
        }
        let at = acquired_at.expect("#1161: the gate must eventually acquire");
        // ceil(90/33.3) = 3, so the oldest (4 ms + 3*33.3 ms = ~104 ms) brackets 90 ms at tick 3.
        assert_eq!(
            at, 3,
            "#1161: the gate should bracket a 90 ms target in 3 ticks from shallow"
        );
        assert!(
            at < cap,
            "#1161: a healthy deepening queue must acquire by AGING (tick {at}) well before the \
             fail-open cap ({cap}) — the cap is a safety valve, not the primary path"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // #1161 — END-TO-END pin-rise sim (the proof the merged Stage-2 gate lacked). That gate shipped
    // with only UNIT tests of `relock_acquire_should_hold`; it passed them yet was INERT live,
    // because a per-source reserve BELOW the arrival transport floor has no leverage. This drives
    // the FULL ts-align conveyor — ACQUIRE + the #1161 Part B gate + STEADY N>=2 boundary-follower +
    // `to_drop` decimation + the #1049 converge shed — applies a mid-stream reserve RISE with Part A
    // (zero the locked boundary), and proves BOTH regimes:
    //   * reserve raised ABOVE the source's natural present age -> the presented frame MOVES to
    //     ~reserve, the queue DEEPENS, and the deep hold STICKS through STEADY N>=2 (no collapse);
    //   * reserve raised but still BELOW the arrival floor -> INERT (the live #1161 signature:
    //     depth stuck, present age unchanged) — a PHYSICAL limit the FIFO cannot overcome (it can
    //     never present a frame fresher than the arrival edge), so the floor-3 aligner must target
    //     an above-floor pin. This is the arrival-floor limit made executable.
    // Faithful mirror of the C `genlock_release_tick` ts-align path; reuses this module's authority
    // functions (`relock_acquire_should_hold` / `relock_select_nearest` / `should_converge_phase` /
    // `source_interval_from_stamps`). Tier-0 (rustc --test), no rig.
    struct PinRiseFifo1161 {
        boundary: u64,
        last_known_n: u32,
        bracket_ticks: u64,
        anchor_ns: u64,
        ticks_since_drain: u64,
        depth_at_audit: usize,
    }
    impl PinRiseFifo1161 {
        fn new() -> Self {
            Self {
                boundary: 0,
                last_known_n: 0,
                bracket_ticks: 0,
                anchor_ns: 0,
                ticks_since_drain: 0,
                depth_at_audit: 0,
            }
        }
        /// Mirror of `genlock_effective_source_multiple` with the sticky-N latch.
        fn eff_n(&mut self, q: &std::collections::VecDeque<u64>) -> u32 {
            let s: Vec<u64> = q.iter().take(8).copied().collect();
            match source_interval_from_stamps(&s) {
                Some(iv) if iv > 0 => {
                    let m = ((I30 as f64 / iv as f64).round() as u32).max(1);
                    self.last_known_n = m;
                    m
                }
                _ => self.last_known_n.max(1),
            }
        }
        /// One render tick; returns the presented on-air age (`wall - stamp`) ns, or None on a hold.
        /// Mirror of the C `genlock_release_tick` ts-align path (ACQUIRE + Part B gate + STEADY).
        fn tick(
            &mut self,
            wall: u64,
            reserve_ms: u32,
            q: &mut std::collections::VecDeque<u64>,
        ) -> Option<i64> {
            if q.is_empty() {
                return None;
            }
            self.depth_at_audit = q.len();
            let reserve_ns = reserve_ms as u64 * 1_000_000;
            let deadline =
                phase_pinned_deadline(wall.saturating_sub(reserve_ns), I30);
            let due = q
                .iter()
                .take_while(|&&ts| phase_pinned_is_due(ts, deadline))
                .count();
            if self.boundary == 0 {
                // ACQUIRE. #726 STICKY-N + #859 settle clock reset (mirror the C).
                self.last_known_n = 0;
                self.ticks_since_drain = 0;
                // #1161 Part B bracketing gate (N>=2 only).
                let n = self.eff_n(q);
                if n >= 2 {
                    let oldest_age = wall.saturating_sub(q[0]);
                    if relock_acquire_should_hold(oldest_age, reserve_ns, I30, self.bracket_ticks) {
                        self.bracket_ticks += 1;
                        return None; // HOLD — let the queue deepen to the raised reserve.
                    }
                }
                self.bracket_ticks = 0;
                if due == 0 {
                    return None;
                }
                let qv: Vec<u64> = q.iter().copied().collect();
                let sel = relock_select_nearest(&qv, wall, relock_anchor_age_ns(self.anchor_ns, reserve_ms));
                for _ in 0..sel {
                    q.pop_front();
                }
                let ts = q.pop_front().unwrap();
                self.anchor_ns = wall.saturating_sub(ts);
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            if *q.front().unwrap() <= self.boundary {
                // STEADY.
                let n = self.eff_n(q);
                let old_boundary = self.boundary;
                if n >= 2 {
                    let mature_deadline = self.boundary + I30 / 2;
                    let matured_n = q
                        .iter()
                        .take_while(|&&ts| ts <= mature_deadline)
                        .count()
                        .max(1);
                    for _ in 0..matured_n - 1 {
                        q.pop_front();
                    }
                    let newest = *q.back().unwrap();
                    if should_converge_phase(
                        wall,
                        old_boundary,
                        newest,
                        reserve_ms,
                        I30,
                        n,
                        self.ticks_since_drain,
                    ) && q.len() > 1
                    {
                        q.pop_front();
                        self.ticks_since_drain = 0;
                    } else {
                        self.ticks_since_drain += 1;
                    }
                    let ts = q.pop_front().unwrap();
                    self.anchor_ns = wall.saturating_sub(ts);
                    self.boundary = ts + I30;
                    return Some(wall as i64 - ts as i64);
                }
                let ts = q.pop_front().unwrap();
                self.anchor_ns = wall.saturating_sub(ts);
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            if deadline >= *q.front().unwrap() {
                // GAP RESYNC.
                let ts = q.pop_front().unwrap();
                self.anchor_ns = wall.saturating_sub(ts);
                self.boundary = ts + I30;
                return Some(wall as i64 - ts as i64);
            }
            None // HOLD (the boundary's frame has not arrived).
        }
    }

    /// Drive the conveyor: 60 fps stamps on the DanteSync grid, arriving at `stamp + skew (+ jitter)`
    /// (a frame cannot be presented before it arrives — `skew_ns` is the transport floor), consumed
    /// at 30 fps render ticks with +-2 ms slew. The reserve rises `lo_ms -> hi_ms` at `rise_tick`
    /// with Part A (boundary=0, bracket=0, anchor=0). Returns (pre_age_ms, pre_depth, post_age_ms,
    /// post_depth, tail_age_ms) — means over settle windows before/after the rise, plus the last-20
    /// tail (to prove a deep hold STICKS and does not collapse back).
    fn run_pin_rise_1161(
        skew_ns: u64,
        lo_ms: u32,
        hi_ms: u32,
        rise_tick: u64,
        n_ticks: u64,
    ) -> (f64, usize, f64, usize, f64) {
        const BASE: u64 = 1_000_000_000_000;
        let mut f = PinRiseFifo1161::new();
        let mut q: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut next = 0u64;
        let mut reserve = lo_ms;
        let mut pre: Vec<i64> = Vec::new();
        let mut post: Vec<i64> = Vec::new();
        let mut tail: Vec<i64> = Vec::new();
        let mut pre_depth = 0usize;
        let mut post_depth = 0usize;
        for k in 0..n_ticks {
            let slew: i64 = if k % 2 == 0 { 2_000_000 } else { -2_000_000 };
            let wall = (BASE + k * I30).saturating_add_signed(slew);
            loop {
                let jitter = ((next.wrapping_mul(2_654_435_761)) % 8_000_001) as i64 - 4_000_000;
                let arr = (BASE + next * I60 + skew_ns).saturating_add_signed(jitter);
                if arr > wall {
                    break;
                }
                q.push_back(BASE + next * I60);
                next += 1;
            }
            if k == rise_tick {
                reserve = hi_ms;
                f.boundary = 0; // Part A
                f.bracket_ticks = 0;
                f.anchor_ns = 0;
            }
            let age = f.tick(wall, reserve, &mut q);
            if k >= rise_tick.saturating_sub(40) && k < rise_tick {
                if let Some(a) = age {
                    pre.push(a);
                }
                pre_depth = f.depth_at_audit;
            }
            if k >= rise_tick + 8 && k < rise_tick + 60 {
                if let Some(a) = age {
                    post.push(a);
                }
                post_depth = f.depth_at_audit;
            }
            if k + 20 >= n_ticks {
                if let Some(a) = age {
                    tail.push(a);
                }
            }
        }
        let mean = |v: &[i64]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<i64>() as f64 / v.len() as f64 / 1e6
            }
        };
        (mean(&pre), pre_depth, mean(&post), post_depth, mean(&tail))
    }

    /// GREEN — the frame-mover WORKS when the raised reserve exceeds the arrival floor: on a shallow
    /// (skew 8 ms) 60->30 source, a 17->50 ms pin rise moves the presented age from ~17 ms to ~50 ms
    /// (== the reserve) and DEEPENS the queue. This is the "depth deepening, head-skew ~ reserve"
    /// behaviour the ticket's off-rig GREEN calls for.
    #[test]
    fn pin_rise_above_the_arrival_floor_moves_the_frame_and_deepens_1161() {
        let (pre_age, pre_depth, post_age, post_depth, _tail) =
            run_pin_rise_1161(8 * 1_000_000, 17, 50, 400, 700);
        assert!(
            pre_age < 30.0,
            "#1161: PRE present age should sit near the shallow 17 ms pin, got {pre_age:.1} ms"
        );
        assert!(
            (post_age - 50.0).abs() <= 6.0,
            "#1161: a pin rise ABOVE the arrival floor must move the frame to ~reserve (50 ms); \
             got {post_age:.1} ms (pre {pre_age:.1} ms)"
        );
        assert!(
            post_depth > pre_depth,
            "#1161: the raised reserve must DEEPEN the queue (pre depth {pre_depth}, post {post_depth})"
        );
    }

    /// RED-in-spirit (the LIVE signature, now locked as a DOCUMENTED physical limit): a 17->50 ms
    /// pin rise on a transport-dominated source (skew 59 ms -> the live `ts_head_skew_ms ~ 76`,
    /// `depth 2`) is INERT — the presented age does not move and the queue does not deepen, because
    /// the reserve (50 ms) sits BELOW the arrival floor (~59-66 ms). No FIFO change can move it (a
    /// frame cannot be presented before it arrives); the floor-3 aligner must target an above-floor
    /// pin. This test is WHY the merged gate was inert live.
    #[test]
    fn pin_rise_below_the_arrival_floor_is_inert_the_live_signature_1161() {
        let (pre_age, pre_depth, post_age, post_depth, _tail) =
            run_pin_rise_1161(59 * 1_000_000, 17, 50, 400, 700);
        assert!(
            (post_age - pre_age).abs() < 16.7,
            "#1161: a below-floor pin rise must be INERT (< one source interval of movement); \
             pre {pre_age:.1} ms -> post {post_age:.1} ms is the live HOLD-INERT signature"
        );
        assert_eq!(
            post_depth, pre_depth,
            "#1161: a below-floor pin rise must NOT deepen the queue (the stuck-depth-2 signature)"
        );
    }

    /// A DEEP above-floor hold STICKS through STEADY N>=2 — it is NOT collapsed back to the arrival
    /// edge by the boundary-follower. At skew 59 ms a 17->92 ms rise moves the presented age to
    /// ~100 ms (one full canvas frame deeper) AND the last-20-tick tail stays there (within one
    /// source interval), proving the STEADY N>=2 `mature_deadline` follower maintains the raised
    /// hold rather than draining it. This is what makes an above-floor aligner pin a durable fix.
    #[test]
    fn a_deep_above_floor_hold_sticks_through_steady_n2_no_collapse_1161() {
        let (pre_age, pre_depth, post_age, post_depth, tail_age) =
            run_pin_rise_1161(59 * 1_000_000, 17, 92, 400, 700);
        assert!(
            post_age - pre_age >= 25.0,
            "#1161: a 92 ms reserve (above the ~66 ms floor) must move the frame ~one canvas frame \
             deeper; pre {pre_age:.1} ms -> post {post_age:.1} ms"
        );
        assert!(
            post_depth > pre_depth,
            "#1161: the deep hold must deepen the queue (pre {pre_depth}, post {post_depth})"
        );
        assert!(
            (tail_age - post_age).abs() <= 16.7,
            "#1161: the deep hold must STICK — the tail ({tail_age:.1} ms) must not collapse back \
             toward the arrival edge from the post-rise depth ({post_age:.1} ms)"
        );
    }

}
