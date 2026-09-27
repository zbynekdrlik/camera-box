//! Issue 1367 — the genlock libobs audio-buffering FLOOR: every OBS launch starts with the SAME
//! global audio buffering, so the stream `mbc` ASRC level (and with it the audio position against
//! the video) no longer depends on a startup race.
//!
//! ## Why this module exists
//!
//! libobs mixes audio in 1024-sample ticks. The mix window runs `buffering` behind real time,
//! and stock OBS grows `buffering` DYNAMICALLY: it adds ticks only when a source is behind the
//! window at startup (obs-audio.c `add_audio_buffering`). Whether it grows is a startup race. On the
//! stream box the release E2E of 27.9.2026 found both outcomes:
//!
//! - session `2026-09-27 01-33-25`: `adding 85 milliseconds of audio buffering (source: ASIO Input
//!   Capture)`; the `mbc` ASRC held its #1355 absolute target all session;
//! - session `2026-09-27 12-18-15`: NO buffering; `mbc` sat at 27 ms against the 118 ms target,
//!   the restore pushed −140 ppm for 40 min, then the #1355 fallback re-latched the target at a
//!   random depth (54.8 ms, 73.5 ms). Every camera read −28 ms on the release A/V gate.
//!
//! The depth a direct-timestamp source (`mbc`, ASIO) settles on without any servo correction —
//! its NATURAL depth — is `buffering + base + sync_offset`. `base` is its own arrival latency
//! against the mix window plus the mean of the tick sawtooth. It was measured at 8.4 ms and 8.9 ms
//! in the two sessions above, whose buffering differed by 85 ms. The #1355 absolute target is
//! `LEVEL_TARGET_MS + sync_offset`, so the sync offset CANCELS: the level servo must bridge
//! `target − (buffering + base)`, whatever offset the #1333 split / #856 trim writes.
//!
//! The servo cannot bridge an arbitrary gap. It stretches or compresses the source's samples, which
//! moves the source's smoothed timeline away from its raw timestamps. Once they are
//! `TS_SMOOTHING_THRESHOLD` (70 ms, obs-source.c) apart, the next packet is re-placed at its raw
//! stamp and the correction is gone. The threshold is a symmetric `uint64_diff`, so this limits
//! both directions. Live: at −140 ppm the `mbc` level climbed at most +51…+59 ms and then snapped
//! back. The band this module holds is therefore HALF that threshold ([`LEVEL_REACH_NS`], ±35 ms).
//! It has a LOWER and an UPPER edge: too little buffering leaves the target out of reach from
//! below (the no-buffering session), and too much buffering puts it out of reach from above.
//!
//! ## What it decides (ROZHODNUTÉ 5857354949: a deterministic FLOOR, not a hard cap)
//!
//! - [`plan`] (at `obs_reset_audio2`): the floor is [`FLOOR_MS`] rounded up to whole ticks exactly
//!   as OBS rounds its own maximum ([`ticks_for_ms`]). Fixed buffering is never honoured, so the
//!   frontend low-latency toggle (fixed 20 ms) is OVERRIDDEN and reported. The maximum stays the
//!   caller's, or OBS's default 45 ticks when it would not sit above the floor.
//! - [`action`] (every mixer tick): below the floor, raise the buffering to the floor in one tick
//!   (the first audio tick of every launch); at or above it, OBS's dynamic increase for a late
//!   source stays active up to the maximum. The resolume cg OBS legitimately grows 128–362 ms on
//!   media / `NDI test` starts, and a hard cap would drop that late audio on FOH/VBAN.
//! - [`band_error_ns`] / [`band_ok`]: the gap the servo must bridge, and whether it is inside the
//!   band. The shipped floor keeps it inside for a base of 0–25 ms at 44.1 and 48 kHz; the libobs
//!   log names a dynamic increase that leaves it.
//!
//! The C twin is `vendor/obs-studio/libobs/obs-genlock-audio-buffering.h` (stdint/stdbool only).
//! `tests/genlock_audio_buffering_parity_1367.rs` compiles it as-is, requires byte-identical results,
//! and lifts the C defines the invariant depends on.
//!
//! Pure `std`, no `crate::` imports — standalone-rustc Tier-0 testable.

/// The genlock audio-buffering floor, ms, before tick rounding: 4 ticks = 85.33 ms at 48 kHz,
/// 92.88 ms at 44.1 kHz. It is the live-proven `01-33-25` configuration: the `mbc` absolute target
/// held for 10 h, through four trims, with 0 fallbacks.
pub const FLOOR_MS: u32 = 85;

/// OBS's own maximum when the caller asks for none (`obs_reset_audio2`: 45 ticks = 960 ms at
/// 48 kHz). It is also the maximum when the request is overridden.
pub const DEFAULT_MAX_TICKS: u32 = 45;

/// libobs `AUDIO_OUTPUT_FRAMES` (media-io/audio-io.h): the samples per mixer tick.
pub const AUDIO_OUTPUT_FRAMES: u32 = 1024;

/// obs-source.c `TS_SMOOTHING_THRESHOLD`, ns: the timeline distance at which a packet is re-placed
/// at its raw stamp, which discards whatever the level servo stretched or compressed.
pub const TS_SMOOTHING_THRESHOLD_NS: i64 = 70_000_000;

/// How far from the natural depth the absolute level target may sit, ns: half of
/// [`TS_SMOOTHING_THRESHOLD_NS`], so the servo never works near the re-placement edge.
pub const LEVEL_REACH_NS: i64 = TS_SMOOTHING_THRESHOLD_NS / 2;

/// The `mbc` base, ns: its offset-free depth at zero buffering. Measured at 8.4 ms (`01-33-25`,
/// 85.33 ms buffering) and 8.9 ms (`12-18-15`, none). Used for the libobs log note only.
pub const LEVEL_BASE_NOMINAL_NS: u64 = 9_000_000;

/// The base range the band invariant is proven over, ns: 0 up to the ~25 ms transport base a
/// genlock source shows (asrc-compensator.h).
pub const LEVEL_BASE_MAX_NS: u64 = 25_000_000;

/// What [`plan`] decides at `obs_reset_audio2`. Mirror of the C
/// `struct genlock_audio_buffering_plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferingPlan {
    /// The floor, in ticks, applied on the first mixer tick of every launch.
    pub floor_ticks: u32,
    /// The ceiling of the dynamic increase above the floor, in ticks. Never below the floor.
    pub max_ticks: u32,
    /// Whether OBS's fixed-buffering mode is used. Always false under the floor: the floor is
    /// the fixed part, and the part above it stays dynamic.
    pub fixed: bool,
    /// True when the request (a fixed / low-latency buffering, or a maximum below the floor) was
    /// replaced; libobs logs it.
    pub overridden: bool,
}

/// What the mixer does about its buffering on one tick. Mirror of the C
/// `GENLOCK_AUDIO_BUFFERING_*` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferingAction {
    /// Nothing: at the maximum, or no source is behind the window.
    None = 0,
    /// Raise the total to the floor in this tick (the first audio tick of a launch).
    Floor = 1,
    /// OBS's own dynamic increase for a source behind the window, above the floor.
    Dynamic = 2,
}

/// Ticks for `ms` of buffering, rounded up — `obs_reset_audio2`'s own formula, including its
/// `uint32_t` arithmetic (`ms · rate / 1000`, plus `frames − 1`, divided by `frames`). 0 for
/// `frames == 0`.
pub fn ticks_for_ms(ms: u32, rate: u32, frames: u32) -> u32 {
    if frames == 0 {
        return 0;
    }
    let max_frames = ms.wrapping_mul(rate) / 1000;
    max_frames.wrapping_add(frames - 1) / frames
}

/// The duration of `ticks` mixer ticks, ns (truncated). 0 for `rate == 0`.
pub fn ticks_ns(ticks: u32, frames: u32, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    u64::from(ticks)
        .wrapping_mul(u64::from(frames))
        .wrapping_mul(1_000_000_000)
        / u64::from(rate)
}

/// The buffering plan for a reset request: the floor, the dynamic maximum, and whether the
/// request was overridden. `req_max_ms == 0` means "OBS default" ([`DEFAULT_MAX_TICKS`]).
pub fn plan(req_max_ms: u32, req_fixed: bool, rate: u32, frames: u32) -> BufferingPlan {
    // RED stub: stock obs_reset_audio2 -- no floor, the caller's fixed flag and maximum as-is.
    let max_ticks = if req_max_ms != 0 {
        ticks_for_ms(req_max_ms, rate, frames)
    } else {
        DEFAULT_MAX_TICKS
    };
    BufferingPlan {
        floor_ticks: 0,
        max_ticks,
        fixed: req_fixed,
        overridden: false,
    }
}

/// The floor-then-dynamic rule for one mixer tick. `source_behind` is obs-audio.c's
/// `min_ts < ts.start` (a source's audio lies before the mix window). At the maximum nothing is
/// added (`audio_buffering_maxed`); below the floor the floor is raised first.
pub fn action(
    total_ticks: i32,
    floor_ticks: i32,
    max_ticks: i32,
    source_behind: bool,
) -> BufferingAction {
    // RED stub: stock audio_callback -- only the dynamic increase, no floor.
    let _ = floor_ticks;
    if total_ticks < max_ticks && source_behind {
        BufferingAction::Dynamic
    } else {
        BufferingAction::None
    }
}

/// The gap the level servo must bridge, ns: the absolute level target minus the natural depth
/// `buffering + base`. The sync offset is not an input — it moves both sides by the same amount
/// (see [`level_gap_ns`]). Positive = the servo must stretch (depth below target). Wrapping two's
/// complement, like the C.
pub fn band_error_ns(buffering_ns: u64, base_ns: u64, target_ns: i64) -> i64 {
    (target_ns as u64).wrapping_sub(buffering_ns.wrapping_add(base_ns)) as i64
}

/// Whether the gap is inside the band the level servo can hold (±[`LEVEL_REACH_NS`]).
pub fn band_ok(error_ns: i64) -> bool {
    (-LEVEL_REACH_NS..=LEVEL_REACH_NS).contains(&error_ns)
}

/// The natural depth of a direct-timestamp source at a sync offset, ns: `buffering + base +
/// sync_offset`. The placement adds the offset before the samples enter the buffer
/// (obs-source.c `in.timestamp += sync_offset`). Live, a −4 → −18 ms trim moved `mbc` 96.8 → 81.1
/// ms, and the `01-33-25` trims 41 → 18 ms moved the level with its target.
pub fn natural_depth_ns(buffering_ns: u64, base_ns: u64, sync_offset_ns: i64) -> i64 {
    (buffering_ns.wrapping_add(base_ns) as i64).wrapping_add(sync_offset_ns)
}

/// The #1355 absolute level target at a sync offset, ns: `target + sync_offset`
/// (`asrc_compensator_set_level_offset_ms(last_sync_offset)`).
pub fn absolute_target_ns(target_ns: i64, sync_offset_ns: i64) -> i64 {
    target_ns.wrapping_add(sync_offset_ns)
}

/// The gap at a given sync offset: [`absolute_target_ns`] − [`natural_depth_ns`]. It equals
/// [`band_error_ns`] for every offset — this is the cancellation the band relies on.
pub fn level_gap_ns(buffering_ns: u64, base_ns: u64, target_ns: i64, sync_offset_ns: i64) -> i64 {
    absolute_target_ns(target_ns, sync_offset_ns).wrapping_sub(natural_depth_ns(
        buffering_ns,
        base_ns,
        sync_offset_ns,
    ))
}

/// Whether a floor of `floor_ticks` keeps `target_ns` inside the band for EVERY base in
/// `0..=`[`LEVEL_BASE_MAX_NS`] at `rate`. The two band edges are the extreme bases, since the gap
/// is linear in the base.
pub fn floor_holds_band(floor_ticks: u32, rate: u32, frames: u32, target_ns: i64) -> bool {
    let buffering = ticks_ns(floor_ticks, frames, rate);
    band_ok(band_error_ns(buffering, 0, target_ns))
        && band_ok(band_error_ns(buffering, LEVEL_BASE_MAX_NS, target_ns))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #1355 absolute level target (asrc_bench LEVEL_TARGET_MS = 100.0), ns. The integration
    /// gate lifts the shipped C value and holds the invariant on THAT; this copy only drives the
    /// unit tests.
    const TARGET_NS: i64 = 100_000_000;
    const RATES: [u32; 2] = [44_100, 48_000];
    const MS: u64 = 1_000_000;

    #[test]
    fn ticks_round_up_like_obs_reset_audio2() {
        // OBS's own max: 960 ms at 48 kHz is exactly 45 ticks; the low-latency 20 ms is 1 tick.
        assert_eq!(ticks_for_ms(960, 48_000, AUDIO_OUTPUT_FRAMES), 45);
        assert_eq!(ticks_for_ms(20, 48_000, AUDIO_OUTPUT_FRAMES), 1);
        // The floor: 4 ticks at both production rates.
        assert_eq!(ticks_for_ms(FLOOR_MS, 48_000, AUDIO_OUTPUT_FRAMES), 4);
        assert_eq!(ticks_for_ms(FLOOR_MS, 44_100, AUDIO_OUTPUT_FRAMES), 4);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 48_000), 85_333_333);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 44_100), 92_879_818);
        // An exact multiple does not round up; one sample over does.
        assert_eq!(ticks_for_ms(64, 48_000, AUDIO_OUTPUT_FRAMES), 3);
        assert_eq!(ticks_for_ms(0, 48_000, AUDIO_OUTPUT_FRAMES), 0);
        assert_eq!(ticks_for_ms(85, 48_000, 0), 0);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 0), 0);
    }

    #[test]
    fn plan_starts_every_launch_at_the_floor_and_keeps_the_dynamic_maximum() {
        // The frontend's normal request: no maximum, not fixed.
        let p = plan(0, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!(
            p,
            BufferingPlan {
                floor_ticks: 4,
                max_ticks: DEFAULT_MAX_TICKS,
                fixed: false,
                overridden: false
            }
        );
        // An explicit maximum above the floor is kept.
        let p = plan(500, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!((p.floor_ticks, p.max_ticks, p.overridden), (4, 24, false));
    }

    #[test]
    fn the_low_latency_toggle_is_overridden_never_a_hard_cap() {
        // OBSBasic::ResetAudio's LowLatencyAudioBuffering: fixed 20 ms.
        let p = plan(20, true, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.overridden && !p.fixed);
        assert_eq!((p.floor_ticks, p.max_ticks), (4, DEFAULT_MAX_TICKS));
        // A fixed request at any size is still dynamic above the floor.
        let p = plan(500, true, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.overridden && !p.fixed);
        assert_eq!(p.max_ticks, DEFAULT_MAX_TICKS);
        // A maximum below the floor would be a hard cap at (or under) the floor: overridden.
        let p = plan(50, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.overridden);
        assert_eq!(p.max_ticks, DEFAULT_MAX_TICKS);
        // A maximum exactly at the floor is allowed (the floor ticks are reachable).
        let p = plan(FLOOR_MS, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(!p.overridden);
        assert_eq!(p.max_ticks, 4);
    }

    #[test]
    fn the_maximum_never_sits_below_the_floor() {
        // A rate so high that the floor exceeds OBS's default 45 ticks: the maximum follows it.
        let p = plan(0, false, 768_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.floor_ticks > DEFAULT_MAX_TICKS);
        assert_eq!(p.max_ticks, p.floor_ticks);
        // No rate: no floor, OBS default maximum.
        let p = plan(0, false, 0, AUDIO_OUTPUT_FRAMES);
        assert_eq!((p.floor_ticks, p.max_ticks), (0, DEFAULT_MAX_TICKS));
    }

    #[test]
    fn the_first_tick_raises_the_floor_then_obs_grows_dynamically_above_it() {
        use BufferingAction::*;
        // First tick of a launch: the floor, whether or not a source is behind.
        assert_eq!(action(0, 4, 45, false), Floor);
        assert_eq!(action(0, 4, 45, true), Floor);
        // Partial (never happens in one pass, but the rule is "below the floor -> floor").
        assert_eq!(action(3, 4, 45, true), Floor);
        // At the floor: nothing unless a source is behind the window.
        assert_eq!(action(4, 4, 45, false), None);
        assert_eq!(action(4, 4, 45, true), Dynamic);
        // Above the floor the dynamic increase stays active up to the maximum.
        assert_eq!(action(17, 4, 45, true), Dynamic);
        assert_eq!(action(44, 4, 45, true), Dynamic);
        assert_eq!(action(45, 4, 45, true), None);
        assert_eq!(action(46, 4, 45, true), None);
        // A maximum at the floor: the floor is reached, then nothing grows.
        assert_eq!(action(0, 4, 4, true), Floor);
        assert_eq!(action(4, 4, 4, true), None);
        // No floor (no rate): stock dynamic behaviour.
        assert_eq!(action(0, 0, 45, true), Dynamic);
        assert_eq!(action(0, 0, 45, false), None);
    }

    #[test]
    fn the_shipped_floor_holds_the_band_for_every_base_at_both_rates() {
        for rate in RATES {
            let floor = plan(0, false, rate, AUDIO_OUTPUT_FRAMES).floor_ticks;
            assert!(
                floor_holds_band(floor, rate, AUDIO_OUTPUT_FRAMES, TARGET_NS),
                "{floor} ticks at {rate} Hz leave the band"
            );
            let buffering = ticks_ns(floor, AUDIO_OUTPUT_FRAMES, rate);
            for base_ms in 0..=25_u64 {
                let e = band_error_ns(buffering, base_ms * MS, TARGET_NS);
                assert!(band_ok(e), "{rate} Hz base {base_ms} ms: gap {e} ns");
            }
        }
    }

    #[test]
    fn too_little_or_too_much_buffering_leaves_the_band() {
        let r = 48_000;
        let f = AUDIO_OUTPUT_FRAMES;
        // No buffering (the 12-18-15 session): gap ~91 ms at the nominal base.
        let e = band_error_ns(0, LEVEL_BASE_NOMINAL_NS, TARGET_NS);
        assert_eq!(e, 91_000_000);
        assert!(!band_ok(e));
        assert!(!floor_holds_band(0, r, f, TARGET_NS));
        // 3 ticks (64 ms) is too little at a 0 ms base; 6 ticks (128 ms) too much at a 25 ms base.
        assert!(!floor_holds_band(3, r, f, TARGET_NS));
        assert!(!floor_holds_band(6, r, f, TARGET_NS));
        // 4 and 5 ticks hold it.
        assert!(floor_holds_band(4, r, f, TARGET_NS));
        assert!(floor_holds_band(5, r, f, TARGET_NS));
        // The ROZHODNUTÉ edge: buffering + 9 ms base above 135 ms breaks it.
        assert!(band_ok(band_error_ns(
            126 * MS,
            LEVEL_BASE_NOMINAL_NS,
            TARGET_NS
        )));
        assert!(!band_ok(band_error_ns(
            126 * MS + 1,
            LEVEL_BASE_NOMINAL_NS,
            TARGET_NS
        )));
    }

    #[test]
    fn a_target_change_that_leaves_the_band_is_caught() {
        // The shipped 4-tick floor at 48 kHz holds 100 ms, and the band edges are where they
        // must be: 85.33 ms + 0 base + 35 = 120.33 ms highest, 85.33 + 25 − 35 = 75.33 lowest.
        let f = AUDIO_OUTPUT_FRAMES;
        assert!(floor_holds_band(4, 48_000, f, 120_333_333));
        assert!(!floor_holds_band(4, 48_000, f, 120_333_334));
        assert!(floor_holds_band(4, 48_000, f, 75_333_333));
        assert!(!floor_holds_band(4, 48_000, f, 75_333_332));
        assert!(!floor_holds_band(4, 48_000, f, 150_000_000));
    }

    #[test]
    fn the_sync_offset_cancels_out_of_the_gap() {
        // Over the whole ±500 ms range the #1333 split may write (AUDIO_OFFSET_CLAMP_MS; the
        // integration gate lifts the real clamp), the gap is the offset-free band error.
        let buffering = ticks_ns(4, AUDIO_OUTPUT_FRAMES, 48_000);
        for base_ms in [0_u64, 9, 25] {
            let e = band_error_ns(buffering, base_ms * MS, TARGET_NS);
            for x_ms in -500..=500_i64 {
                assert_eq!(
                    level_gap_ns(buffering, base_ms * MS, TARGET_NS, x_ms * 1_000_000),
                    e
                );
            }
        }
    }

    #[test]
    fn the_two_measured_sessions_fit_the_model() {
        // 01-33-25: 4 ticks (85.33 ms), offset 41 ms, first level_avg 134.74 ms.
        // 12-18-15: no buffering, offset 18 ms, first level_avg 26.94 ms.
        let a_buf = ticks_ns(4, AUDIO_OUTPUT_FRAMES, 48_000) as i64;
        let a_base = 134_740_000 - 41_000_000 - a_buf;
        let b_base = 26_940_000 - 18_000_000;
        // Both bases land in the band's 0..25 ms and agree within 1 ms, whatever the buffering.
        for base in [a_base, b_base] {
            assert!(
                (0..=LEVEL_BASE_MAX_NS as i64).contains(&base),
                "base {base}"
            );
        }
        assert!((a_base - b_base).abs() < 1_000_000);
        // With them, the 85 ms session holds the band and the no-buffering one does not.
        assert!(band_ok(band_error_ns(
            a_buf as u64,
            a_base as u64,
            TARGET_NS
        )));
        assert!(!band_ok(band_error_ns(0, b_base as u64, TARGET_NS)));
        // The nominal base is the rounded measurement.
        assert!((LEVEL_BASE_NOMINAL_NS as i64 - b_base).abs() < 1_000_000);
    }

    #[test]
    fn the_band_is_half_the_re_placement_threshold() {
        assert_eq!(LEVEL_REACH_NS * 2, TS_SMOOTHING_THRESHOLD_NS);
        assert!(band_ok(LEVEL_REACH_NS) && band_ok(-LEVEL_REACH_NS));
        assert!(!band_ok(LEVEL_REACH_NS + 1) && !band_ok(-LEVEL_REACH_NS - 1));
        assert!(!band_ok(i64::MIN) && !band_ok(i64::MAX));
    }
}
