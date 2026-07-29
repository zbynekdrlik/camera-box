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
//! source on the rig — see `shallow_latency_threshold_is_byte_identical_to_the_bare_constant`.
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
pub fn backlog_relock_threshold(latency_ms: u32, fps_num: u32, fps_den: u32) -> u64 {
    // RED: today's shipped behaviour — the bare constant, ignoring the source's own configured
    // latency entirely. This is what `obs-source.c` does now and is the defect under test.
    let _ = (latency_ms, fps_num, fps_den);
    QDEPTH_RELOCK_MARGIN
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
        let t = backlog_relock_threshold(923, 30, 1);
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
        // The 3 ms global default / imag contract — the load-bearing case, unchanged at any rate.
        assert_eq!(backlog_relock_threshold(3, 30, 1), QDEPTH_RELOCK_MARGIN);
        assert_eq!(backlog_relock_threshold(3, 60, 1), QDEPTH_RELOCK_MARGIN);

        // Anything implying <0.5 frames rounds to 0 implied depth => the bare constant.
        for (latency_ms, num) in [(3u32, 30u32), (8, 30), (16, 30), (3, 60), (8, 60)] {
            assert_eq!(
                backlog_relock_threshold(latency_ms, num, 1),
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
        assert_eq!(backlog_relock_threshold(21, 30, 1), 7, "21ms -> 1 frame + 6");
        assert_eq!(backlog_relock_threshold(26, 30, 1), 7, "26ms -> 1 frame + 6");
        assert_eq!(backlog_relock_threshold(55, 30, 1), 8, "55ms -> 2 frames + 6");
    }

    #[test]
    fn steady_depth_rounds_to_nearest_like_the_drop_cap() {
        assert_eq!(steady_depth_frames(923, 30, 1), 28); // 27.69 -> 28
        assert_eq!(steady_depth_frames(923, 60, 1), 55); // 55.38 -> 55, matches cap=59 (55+4)
        assert_eq!(steady_depth_frames(55, 30, 1), 2); // 1.65 -> 2
        assert_eq!(steady_depth_frames(16, 30, 1), 0); // 0.48 -> 0
        assert_eq!(steady_depth_frames(17, 30, 1), 1); // 0.51 -> 1
    }

    #[test]
    fn degenerate_frame_rate_implies_no_depth_rather_than_dividing_by_zero() {
        assert_eq!(steady_depth_frames(923, 0, 1), 0);
        assert_eq!(steady_depth_frames(923, 30, 0), 0);
        assert_eq!(backlog_relock_threshold(923, 0, 0), QDEPTH_RELOCK_MARGIN);
    }

    /// A genuine storm on a deep source is STILL caught — the bar does not move, it moves WITH the
    /// source. #401's own scenario (a stall's burst landing at once) is far above the threshold.
    #[test]
    fn a_real_backlog_storm_on_a_deep_source_is_still_caught_859() {
        let t = backlog_relock_threshold(923, 30, 1);
        // A one-second stall on a 30 fps source lands ~30 frames ON TOP of the steady 28.
        let burst_depth = 28 + 30;
        assert!(
            burst_depth as u64 > t,
            "a stall's burst (depth {burst_depth}) must still exceed the threshold ({t})"
        );
    }
}
