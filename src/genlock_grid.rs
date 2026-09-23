//! #1355 — ONE per-second genlock frame grid for every sender stamp, receiver deadline and
//! render tick.
//!
//! ## Why this module exists
//!
//! Every genlocked sender stamps its frames on a PER-SECOND grid: the camera sender
//! (`crate::ndi::floor_boundary_100ns`) and the OBS sender (`genlock_floor_boundary_100ns` in
//! `vendor/distroav/src/ndi-output.cpp`) restart the slot count at every whole second, so slot
//! `k` of second `S` is `S + floor(k * 1 s / fps)`. The receiver used a DIFFERENT grid until
//! #1355: the ts-align release deadline (`genlock_phase_pin_deadline`, obs-source.c) floored to
//! `(t / interval) * interval` and the render tick (`genlock_next_deadline`, obs-video.c) to
//! `t - t % interval + interval`, both counted from 1970. At 30 fps the receiver interval is
//! `33_333_333 ns`, so 30 of them are `999_999_990 ns` — the 1970 grid loses 10 ns every second
//! against the per-second grid, 0.864 ms per day. The stamp offset measured on the stream box
//! tracked that drift exactly (1.1 → 2.0 ms over 22.–24.9.2026): the frame behind the FIFO head
//! sat ~2 ms past the floored deadline, "due" under the 5 ms hysteresis but NOT due in the
//! missing-stamp HOLD / GAP-RESYNC check, so every irregular sender stamp moved the deep
//! `NDI 2ME PGM` FIFO one frame deeper, and the depth walked with the calendar date.
//!
//! This module is the Tier-0 authority for the ONE grid. The C port is
//! `vendor/obs-studio/libobs/obs-genlock-grid.h` (included by both obs-source.c and
//! obs-video.c); `tests/genlock_relock_selection_parity.rs` compiles that header and requires
//! byte-identical results from the functions here over vectors spanning a whole day.
//!
//! ## Units
//!
//! The receiver works in NANOSECONDS; the sender stamps in 100 ns units. Both floor the SAME
//! rational `k / fps` second with the same multiply-then-divide order, so a sender stamp is at
//! most 99 ns BEFORE the receiver grid point of its own slot and never after it — the property
//! [`per_second_floor`] + [`grid_floor_ns`] are tested against across a day.
//!
//! ## Non-integer rates
//!
//! A per-second grid exists only for an integer frame rate. The rig canvases are 30 / 60 fps;
//! for anything else (29.97 → `33_366_666 ns`) [`integer_fps`] returns `None` and the helpers
//! keep the pre-#1355 1970-grid arithmetic, byte-identical to before.
//!
//! Pure `std`, no `crate::` imports — standalone-rustc Tier-0 testable.

/// Nanoseconds per second — the receiver-side grid unit.
pub const NS_PER_SECOND: u64 = 1_000_000_000;

/// 100 ns units per second — the NDI sender timecode unit (`floor_boundary_100ns`).
pub const UNITS_100NS_PER_SECOND: u64 = 10_000_000;

/// The integer frame rate `interval_ns` belongs to, or `None` when it is not an integer rate.
///
/// `interval_ns` comes from the canvas as `1e9 * fps_den / fps_num` (integer division), so for
/// an integer rate `fps * interval_ns` falls short of one second by `1e9 mod fps < fps`
/// nanoseconds (`30 * 33_333_333 = 999_999_990`). A rounded-up interval (`16_666_667` for 60)
/// overshoots by less than `fps` as well. Anything further off — 29.97's `33_366_666` is ~1 ms
/// off per second — is a fractional rate with no per-second grid. `0` (unknown video info) →
/// `None`.
///
/// Mirror of the C `genlock_grid_integer_fps()` (obs-genlock-grid.h), which returns 0 for None.
pub fn integer_fps(interval_ns: u64) -> Option<u64> {
    if interval_ns == 0 {
        return None;
    }
    let fps = (NS_PER_SECOND + interval_ns / 2) / interval_ns;
    if fps == 0 {
        return None;
    }
    let diff = (fps * interval_ns).abs_diff(NS_PER_SECOND);
    (diff < fps).then_some(fps)
}

/// The slot (`0..fps`) an `offset` into its second falls in, AT OR BEFORE the offset.
///
/// A boundary `b_k = floor(k * units / fps)` sits up to one unit BELOW the exact rational, so
/// `offset * fps / units` under-counts by one for an offset exactly ON such a boundary — the
/// #1009 promotion (same as `floor_boundary_100ns`) fixes it; the under-count is provably at
/// most one slot, so one promotion suffices. `fps > 0` is the caller's guard.
fn slot_in_second(offset: u64, fps: u64, units: u64) -> u64 {
    let mut slot = offset * fps / units;
    if (slot + 1) * units / fps <= offset {
        slot += 1;
    }
    slot
}

/// The per-second grid boundary AT OR BEFORE `t`, in any unit (`units` per second).
///
/// `per_second_floor(t, fps, 10_000_000)` is exactly the sender stamp
/// `floor_boundary_100ns(t, fps)` for a non-negative `t` (asserted in `src/ndi.rs` tests); with
/// `NS_PER_SECOND` it is the receiver grid. `fps == 0` returns `t` unchanged (no alignment),
/// like the sender.
pub fn per_second_floor(t: u64, fps: u64, units: u64) -> u64 {
    if fps == 0 || units == 0 {
        return t;
    }
    let sec = (t / units) * units;
    sec + slot_in_second(t - sec, fps, units) * units / fps
}

/// The per-second grid boundary STRICTLY AFTER `t`, in any unit. For the last slot of a second
/// (`slot + 1 == fps`) the expression is exactly the next whole second, so the roll-over needs no
/// branch. `fps == 0` returns `t`.
pub fn per_second_next(t: u64, fps: u64, units: u64) -> u64 {
    if fps == 0 || units == 0 {
        return t;
    }
    let sec = (t / units) * units;
    sec + (slot_in_second(t - sec, fps, units) + 1) * units / fps
}

/// The receiver grid point AT OR BEFORE `t_ns` for a canvas of frame interval `interval_ns`.
///
/// Integer rate → the per-second grid ([`per_second_floor`] in ns). Fractional rate → the
/// pre-#1355 `(t / interval) * interval` (no per-second grid exists). `interval_ns == 0` →
/// `t_ns` unchanged (unknown video info, never divide by zero).
///
/// Mirror of the C `genlock_grid_floor_ns()` (obs-genlock-grid.h) — keep both in lock-step.
pub fn grid_floor_ns(t_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return t_ns;
    }
    match integer_fps(interval_ns) {
        Some(fps) => per_second_floor(t_ns, fps, NS_PER_SECOND),
        None => (t_ns / interval_ns) * interval_ns,
    }
}

/// The receiver grid point STRICTLY AFTER `t_ns` — the render tick's next boundary.
///
/// Integer rate → [`per_second_next`] in ns (the last slot of a second rolls over to the next
/// whole second). Fractional rate → the pre-#1355 `t - t % interval + interval`.
/// `interval_ns == 0` → `t_ns`.
///
/// Mirror of the C `genlock_grid_next_boundary_ns()` (obs-genlock-grid.h) — keep both in
/// lock-step.
pub fn grid_next_boundary_ns(t_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return t_ns;
    }
    match integer_fps(interval_ns) {
        Some(fps) => per_second_next(t_ns, fps, NS_PER_SECOND),
        None => t_ns - (t_ns % interval_ns) + interval_ns,
    }
}

/// A stamp interval longer than this is a timeline discontinuity (sender restart, clock step),
/// not a run of missing stamps, and is not counted.
pub const STAMP_TRACK_MAX_DELTA_NS: u64 = NS_PER_SECOND;

/// #1355 part 3 — per-input DUPLICATE / MISSING stamp-interval counters on ARRIVAL, the evidence
/// behind the `stamp_dup=` / `stamp_gap=` tokens on the `genlock-fifo audit` line.
///
/// A sender that stamps at SEND time (strih-lx's OBS program output) puts a slow frame into the
/// NEXT 1/30 s cell: the receiver then sees a stamp that skips a slot (a gap) followed by a stamp
/// equal to it (a duplicate). Before #1355 that pair moved the deep FIFO one frame; after it the
/// FIFO absorbs it, but the irregularity is still a sender defect worth measuring — this tracker
/// makes it countable per input instead of inferred.
///
/// Rules, applied to each received stamp in arrival order:
/// - equal to the previous stamp → one duplicate;
/// - a positive interval below [`STAMP_TRACK_MAX_DELTA_NS`] updates the source's own step (the
///   SMALLEST positive interval seen, the #1042 min-delta rule — a gap or a duplicate never shrinks
///   it) and, when it exceeds 1.5 steps, counts `round(interval / step) − 1` missing intervals;
/// - a backward stamp or a jump of a second or more is a discontinuity: nothing is counted.
///
/// [`StampTrack::reset_timeline`] forgets the previous stamp and the step (the receiver's flush
/// seam — the source went inactive); the cumulative counters survive, like every audit counter.
///
/// Mirror of the C `genlock_stamp_track_observe()` (obs-genlock-grid.h) — keep both in lock-step.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StampTrack {
    pub last_ts: u64,
    pub min_delta_ns: u64,
    pub dups: u64,
    pub gaps: u64,
}

impl StampTrack {
    /// Account one received stamp.
    pub fn observe(&mut self, ts: u64) {
        if self.last_ts != 0 && ts >= self.last_ts {
            let delta = ts - self.last_ts;
            if delta == 0 {
                self.dups += 1;
            } else if delta < STAMP_TRACK_MAX_DELTA_NS {
                if self.min_delta_ns == 0 || delta < self.min_delta_ns {
                    self.min_delta_ns = delta;
                }
                let step = self.min_delta_ns;
                if delta * 2 > step * 3 {
                    self.gaps += (delta + step / 2) / step - 1;
                }
            }
        }
        self.last_ts = ts;
    }

    /// The source went inactive: the next stamp starts a new timeline.
    pub fn reset_timeline(&mut self) {
        self.last_ts = 0;
        self.min_delta_ns = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I30: u64 = 33_333_333;
    const I60: u64 = 16_666_666;
    /// 2026-09-23 17:16:33 UTC — the second of the live stream audit line the design measured.
    const SEC_2309: u64 = 1_790_176_593;
    const DAY_NS: u64 = 86_400 * NS_PER_SECOND;

    fn lcg(x: &mut u64) -> u64 {
        *x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *x >> 11
    }

    /// The sender stamp (ns) of an instant `t_ns`: the per-second floor in 100 ns units, the
    /// arithmetic of `floor_boundary_100ns` / `genlock_floor_boundary_100ns`.
    fn sender_stamp_ns(t_ns: u64, fps: u64) -> u64 {
        per_second_floor(t_ns / 100, fps, UNITS_100NS_PER_SECOND) * 100
    }

    #[test]
    fn integer_fps_recognises_rig_rates_and_rejects_fractional_1355() {
        assert_eq!(integer_fps(I30), Some(30));
        assert_eq!(integer_fps(I60), Some(60));
        assert_eq!(integer_fps(16_666_667), Some(60));
        assert_eq!(integer_fps(40_000_000), Some(25));
        assert_eq!(integer_fps(41_666_666), Some(24));
        assert_eq!(
            integer_fps(33_366_666),
            None,
            "29.97 has no per-second grid"
        );
        assert_eq!(
            integer_fps(16_683_333),
            None,
            "59.94 has no per-second grid"
        );
        assert_eq!(integer_fps(0), None);
        assert_eq!(integer_fps(3 * NS_PER_SECOND), None, "slower than 1 fps");
    }

    /// The whole point: at EVERY time of day a sender stamp has a receiver grid point within
    /// the 100 ns quantisation right at-or-after it — the two grids coincide. On the pre-#1355
    /// 1970 grid this fails by up to a whole frame, and by an amount that depends on the date.
    #[test]
    fn receiver_grid_coincides_with_the_sender_grid_all_day_1355() {
        for (fps, interval) in [(30u64, I30), (60, I60)] {
            let day0 = SEC_2309 * NS_PER_SECOND;
            let mut x = 0x1355_u64;
            for i in 0..2_000u64 {
                // Spread across a whole day, plus a random sub-second offset.
                let t = day0 + i * (DAY_NS / 2_000) + lcg(&mut x) % NS_PER_SECOND;
                let s = sender_stamp_ns(t, fps);
                let r = grid_next_boundary_ns(s - 1, interval);
                assert!(
                    r >= s && r - s < 100,
                    "fps {fps}: sender stamp {s} (from t={t}) has no receiver grid point in \
                     [s, s+100): next receiver boundary at-or-after it is {r} (off by {} ns)",
                    r as i128 - s as i128
                );
                // And the floored deadline of that grid point admits the stamp.
                assert!(grid_floor_ns(r, interval) >= s);
            }
        }
    }

    /// Exactly-on-boundary and one-before cases for every slot of a second, and the roll-over
    /// into the next second. Slot k's boundary is `sec + floor(k * 1e9 / fps)`.
    #[test]
    fn grid_floor_and_next_on_exact_boundaries_1355() {
        for (fps, interval) in [(30u64, I30), (60, I60)] {
            let sec = SEC_2309 * NS_PER_SECOND;
            let b = |k: u64| sec + k * NS_PER_SECOND / fps;
            for k in 0..fps {
                assert_eq!(grid_floor_ns(b(k), interval), b(k), "fps {fps} slot {k} ON");
                assert_eq!(
                    grid_next_boundary_ns(b(k), interval),
                    b(k + 1),
                    "next after ON"
                );
                assert_eq!(
                    grid_next_boundary_ns(b(k) - 1, interval),
                    b(k),
                    "next from -1"
                );
                if k > 0 {
                    assert_eq!(grid_floor_ns(b(k) - 1, interval), b(k - 1), "just before");
                }
            }
            // The last slot rolls over to the next whole second, exactly.
            assert_eq!(b(fps), sec + NS_PER_SECOND);
            assert_eq!(
                grid_next_boundary_ns(b(fps - 1), interval),
                sec + NS_PER_SECOND
            );
            assert_eq!(grid_floor_ns(sec + NS_PER_SECOND - 1, interval), b(fps - 1));
        }
    }

    /// The whole second is ALWAYS a grid point, whatever the date — the 1970 grid only hits a
    /// whole second once every 3.3 million seconds.
    #[test]
    fn whole_seconds_are_grid_points_on_every_date_1355() {
        for days in [0u64, 1, 7, 30, 365] {
            let t = (SEC_2309 + days * 86_400) * NS_PER_SECOND;
            assert_eq!(grid_floor_ns(t, I30), t, "day +{days}");
            assert_eq!(grid_floor_ns(t, I60), t, "day +{days}");
            assert_eq!(grid_next_boundary_ns(t - 1, I30), t, "day +{days}");
        }
    }

    /// floor <= t < next, and one grid step is `interval` or `interval + 1` ns (the per-second
    /// slots alternate 33_333_333 / 33_333_334 at 30 fps) — never a skipped or a doubled slot.
    #[test]
    fn floor_and_next_bracket_every_instant_1355() {
        let mut x = 0x0bad_5eed_u64;
        for interval in [I30, I60] {
            for _ in 0..20_000 {
                let t = SEC_2309 * NS_PER_SECOND + lcg(&mut x) % (40 * DAY_NS);
                let f = grid_floor_ns(t, interval);
                let n = grid_next_boundary_ns(t, interval);
                assert!(f <= t && t < n, "t={t} floor={f} next={n}");
                assert!(n - f == interval || n - f == interval + 1, "step {}", n - f);
                assert_eq!(grid_next_boundary_ns(f, interval), n);
            }
        }
    }

    #[test]
    fn degenerate_and_fractional_intervals_keep_the_old_arithmetic_1355() {
        let t = SEC_2309 * NS_PER_SECOND + 123_456_789;
        assert_eq!(grid_floor_ns(t, 0), t);
        assert_eq!(grid_next_boundary_ns(t, 0), t);
        let i2997 = 33_366_666;
        assert_eq!(grid_floor_ns(t, i2997), (t / i2997) * i2997);
        assert_eq!(grid_next_boundary_ns(t, i2997), t - t % i2997 + i2997);
    }

    #[test]
    fn per_second_floor_in_100ns_units_matches_known_sender_stamps_1355() {
        // The values the sender's own floor_boundary_100ns tests pin (src/ndi.rs).
        let u = UNITS_100NS_PER_SECOND;
        assert_eq!(per_second_floor(0, 30, u), 0);
        assert_eq!(per_second_floor(333_333, 30, u), 333_333);
        assert_eq!(per_second_floor(333_332, 30, u), 0);
        assert_eq!(per_second_floor(333_334, 30, u), 333_333);
        assert_eq!(per_second_floor(u, 30, u), u);
        assert_eq!(per_second_floor(u - 1, 30, u), 9_666_666);
        assert_eq!(per_second_floor(5, 0, u), 5, "fps 0 = no alignment");
        assert_eq!(per_second_next(9_666_666, 30, u), u);
    }

    /// The real 30 fps sender stamps (a day in, per-second 100 ns grid: steps of 33_333_300 and
    /// 33_333_400 ns) — a clean stream counts nothing.
    fn sender_stream(n: u64) -> Vec<u64> {
        let t0 = SEC_2309 * NS_PER_SECOND + 5 * NS_PER_SECOND;
        (0..n)
            .map(|k| sender_stamp_ns(t0 + k * NS_PER_SECOND / 30, 30))
            .collect()
    }

    #[test]
    fn stamp_track_counts_nothing_on_a_clean_sender_stream_1355() {
        let mut t = StampTrack::default();
        for s in sender_stream(300) {
            t.observe(s);
        }
        assert_eq!((t.dups, t.gaps), (0, 0), "{t:?}");
        assert_eq!(t.min_delta_ns, 33_333_300);
    }

    /// The strih-lx slow-frame signature: frame k lands in slot k+1 (a gap), frame k+1 stamps the
    /// same slot (a duplicate).
    #[test]
    fn stamp_track_counts_the_gap_then_duplicate_pair_1355() {
        let s = sender_stream(10);
        let mut t = StampTrack::default();
        for &x in &s[..4] {
            t.observe(x);
        }
        t.observe(s[5]); // frame 4 slow -> stamped slot 5
        t.observe(s[5]); // frame 5 on time -> slot 5 again
        for &x in &s[6..] {
            t.observe(x);
        }
        assert_eq!((t.dups, t.gaps), (1, 1), "{t:?}");
    }

    #[test]
    fn stamp_track_counts_every_missing_interval_and_60fps_steps_1355() {
        let mut t = StampTrack::default();
        let step = 16_666_600u64;
        let t0 = SEC_2309 * NS_PER_SECOND;
        for k in [0u64, 1, 2, 5, 6, 6, 6, 7] {
            t.observe(t0 + k * step);
        }
        assert_eq!(t.gaps, 2, "2..5 misses slots 3 and 4: {t:?}");
        assert_eq!(t.dups, 2, "{t:?}");
        assert_eq!(t.min_delta_ns, step);
    }

    #[test]
    fn stamp_track_ignores_discontinuities_and_resets_the_timeline_1355() {
        let mut t = StampTrack::default();
        let s = sender_stream(4);
        for &x in &s {
            t.observe(x);
        }
        t.observe(s[3] - 10 * 33_333_300); // backward step
        t.observe(s[3] + 2 * NS_PER_SECOND); // forward jump >= 1 s
        assert_eq!((t.dups, t.gaps), (0, 0), "{t:?}");
        // The step is learned from positive in-range intervals only.
        assert_eq!(t.min_delta_ns, 33_333_300);
        t.reset_timeline();
        assert_eq!((t.last_ts, t.min_delta_ns), (0, 0));
        t.observe(s[0]);
        t.observe(s[0]);
        assert_eq!(
            t.dups, 1,
            "a duplicate right after a reset still counts: {t:?}"
        );
        // The first stamp after a reset is never compared against the pre-reset timeline.
        let mut u = StampTrack::default();
        u.observe(s[0]);
        u.reset_timeline();
        u.observe(s[0] + 5 * 33_333_300);
        assert_eq!((u.dups, u.gaps), (0, 0), "{u:?}");
    }
}
