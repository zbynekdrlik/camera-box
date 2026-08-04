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
    _source_multiple: u32,
) -> u64 {
    // #940 RED: source_multiple not yet applied -- pre-fix (bare, unscaled margin) behaviour.
    steady_depth_frames(latency_ms, fps_num, fps_den).saturating_add(QDEPTH_RELOCK_MARGIN)
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
/// [`DRAIN_HYSTERESIS_FRAMES`] above [`steady_depth_frames`], shed exactly ONE extra frame.
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

/// Should this tick shed exactly ONE EXTRA frame to slew the queue back toward the depth its
/// own configured latency implies? `depth` is the queue depth observed THIS tick (before any
/// release); `ticks_since_last_drain` is how many ticks have passed since the last time this
/// returned `true` (the caller resets it to 0 exactly when it drains, and increments it every
/// other tick). Degenerate `fps_num`/`fps_den` (0) route through [`steady_depth_frames`], which
/// returns 0 rather than dividing by zero — never panics.
pub fn should_drain_one(
    depth: u64,
    latency_ms: u32,
    fps_num: u32,
    fps_den: u32,
    ticks_since_last_drain: u64,
) -> bool {
    let target = steady_depth_frames(latency_ms, fps_num, fps_den);
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
}
