//! Issue 1367 — the genlock libobs audio-buffering FLOOR, and since issue 1425 the FREEZE. Since
//! issue 1425 E2 slice 3 (design 6220191978) the floor is 0.
//!
//! ## Why this module exists
//!
//! libobs mixes audio in 1024-sample ticks. The mix window runs `buffering` behind real time,
//! and stock OBS grows `buffering` DYNAMICALLY: it adds ticks only when a source is behind the
//! window (obs-audio.c `add_audio_buffering`). Whether it grows at launch is a startup race (the
//! stream box drew 0 ms and 85 ms on 27.9.2026).
//!
//! Issue 1367 raised every launch to an 85 ms floor. The reason was the #1355 absolute ASRC level
//! target (100 ms + the sync offset): a direct-timestamp source (`mbc`) settles at `buffering + base +
//! sync_offset` (base ≈ 9 ms), and the level servo can bridge only about half of OBS's 70 ms
//! TS-smoothing threshold, so a launch with no buffering left the target out of reach and its
//! fallback re-latched at a random depth.
//!
//! Issue 1425 removed that reason. Since E2 slice 1 a directly stamped mixed source is judged by its
//! own stamp (setpoint 0), so its A/V no longer depends on the buffering. Since slice 2 the buffering
//! cannot grow while an audio encoder runs. Slice 3 deleted the absolute target and set the floor to
//! 0: a launch starts at 0 like stock OBS, its own startup race draws 0–85 ms, and nothing moves it
//! once an encoder runs.
//!
//! ## What it decides (ROZHODNUTÉ 5857354949: a FLOOR, never a hard cap)
//!
//! - [`plan`] (at `obs_reset_audio2`): the floor is [`FLOOR_MS`] rounded up to whole ticks exactly
//!   as OBS rounds its own maximum ([`ticks_for_ms`]); 0 ticks today. Fixed buffering is never
//!   honoured, so the frontend low-latency toggle (fixed 20 ms) is OVERRIDDEN and reported. The
//!   maximum stays the caller's, or OBS's default 45 ticks when it would not sit above the floor.
//! - [`action`] (every mixer tick): below the floor, raise the buffering to the floor in one tick
//!   (never at a 0 floor); at or above it, OBS's dynamic increase for a late source stays active up to
//!   the maximum. The resolume cg OBS legitimately grows 128–362 ms on media / `NDI test` starts, and
//!   a hard cap would drop that late audio on FOH/VBAN.
//!
//! ## Issue 1425 (E2 slice 2, design 6216736061 + ROZHODNUTÉ 6217557472): the FREEZE
//!
//! A buffering increase while an encoder runs makes that encoder's audio permanently later
//! (obs-encoder.c counts audio pts in samples after the output start). On stream-snv the E1 events
//! grew the mix +85 / +128 / +192 ms (two ASIO outages, a 432 ms mixer stall) while it streamed,
//! each an audio-after-picture step on every live output until a restart. So while an encoder runs
//! the dynamic increase is OFF:
//!
//! - [`frozen`]: frozen while an output with an audio encoder is active, and only then. With no
//!   encoder a growth moves no live output's A/V, so such a box (the resolume cg OBS) keeps stock
//!   growth.
//! - [`action`] takes it: a frozen mix never grows dynamically. The floor is never frozen.
//! - A mixed source that falls behind a frozen mix is re-anchored instead (obs-genlock-mix-guard.h
//!   `MIXED_LATE`, pinned by the issue-1381 lift): one counted dropout of its late span on that
//!   source. Nothing ever steps down mid-run; the buffering starts over at the next launch.
//!
//! The C twin is `vendor/obs-studio/libobs/obs-genlock-audio-buffering.h` (stdint/stdbool only).
//! `tests/genlock_audio_buffering_parity_1367.rs` compiles it as-is, requires byte-identical results,
//! and lifts the C defines.
//!
//! Pure `std`, no `crate::` imports — standalone-rustc Tier-0 testable.

/// The genlock audio-buffering floor, ms, before tick rounding. 0 since issue 1425 E2 slice 3: it was
/// 85 (4 ticks = 85.33 ms at 48 kHz) only so the retired #1355 absolute ASRC level target stayed in
/// reach.
pub const FLOOR_MS: u32 = 0;

/// OBS's own maximum when the caller asks for none (`obs_reset_audio2`: 45 ticks = 960 ms at
/// 48 kHz). It is also the maximum when the request is overridden.
pub const DEFAULT_MAX_TICKS: u32 = 45;

/// libobs `AUDIO_OUTPUT_FRAMES` (media-io/audio-io.h): the samples per mixer tick.
pub const AUDIO_OUTPUT_FRAMES: u32 = 1024;

/// What [`plan`] decides at `obs_reset_audio2`. Mirror of the C
/// `struct genlock_audio_buffering_plan` (built by `genlock_audio_buffering_make_plan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferingPlan {
    /// The floor, in ticks, applied on the first mixer tick of every launch.
    pub floor_ticks: u32,
    /// The ceiling of the dynamic increase above the floor, in ticks. Never below the floor.
    pub max_ticks: u32,
    /// Whether OBS's fixed-buffering mode is used. Always false under the floor: the floor is
    /// the fixed part, and the part above it stays dynamic. obs-audio.c keeps the upstream fixed
    /// branch byte-identical for rebases; it is unreachable in the genlock build.
    pub fixed: bool,
    /// True when the request (a fixed / low-latency buffering, or a maximum below the floor) was
    /// replaced; libobs logs it.
    pub overridden: bool,
}

/// What the mixer does about its buffering on one tick. Mirror of the C
/// `GENLOCK_AUDIO_BUFFERING_ACTION_*` values.
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
    plan_with_floor(FLOOR_MS, req_max_ms, req_fixed, rate, frames)
}

/// [`plan`] for a given floor. The shipped floor is 0 (issue 1425 slice 3), where the override of
/// a request below the floor and the lift of the maximum to the floor never act; this keeps both
/// rules of `genlock_audio_buffering_make_plan` testable (the C lift drives them with a 4-tick
/// floor set by hand).
pub fn plan_with_floor(
    floor_ms: u32,
    req_max_ms: u32,
    req_fixed: bool,
    rate: u32,
    frames: u32,
) -> BufferingPlan {
    let floor_ticks = ticks_for_ms(floor_ms, rate, frames);
    let req_ticks = if req_max_ms != 0 {
        ticks_for_ms(req_max_ms, rate, frames)
    } else {
        DEFAULT_MAX_TICKS
    };
    let overridden = req_fixed || req_ticks < floor_ticks;
    let max_ticks = if overridden {
        DEFAULT_MAX_TICKS
    } else {
        req_ticks
    };
    BufferingPlan {
        floor_ticks,
        max_ticks: max_ticks.max(floor_ticks),
        fixed: false,
        overridden,
    }
}

/// The floor-then-dynamic rule for one mixer tick. `source_behind` is obs-audio.c's
/// `min_ts < ts.start` (a source's audio lies before the mix window). At the maximum nothing is
/// added (`audio_buffering_maxed`); below the floor the floor is raised first. `frozen` is this
/// tick's [`frozen`] decision (issue 1425): a frozen mix never grows dynamically, the floor still
/// rises.
pub fn action(
    total_ticks: i32,
    floor_ticks: i32,
    max_ticks: i32,
    source_behind: bool,
    frozen: bool,
) -> BufferingAction {
    if total_ticks >= max_ticks {
        BufferingAction::None
    } else if total_ticks < floor_ticks {
        BufferingAction::Floor
    } else if source_behind && !frozen {
        BufferingAction::Dynamic
    } else {
        BufferingAction::None
    }
}

/// Issue 1425 slice 3 (ROZHODNUTÉ 6221252305): the launch settle window, ns of mix time after the
/// mix's first audio tick, during which a running encoder does not freeze the buffering (the
/// startup race draws it). Mirror of `GENLOCK_AUDIO_BUFFERING_LAUNCH_SETTLE_NS`.
pub const LAUNCH_SETTLE_NS: u64 = 30_000_000_000;

/// Whether the launch's startup race is over: at least [`LAUNCH_SETTLE_NS`] since the mix's first
/// audio tick (`first_tick_ns`, 0 = no tick yet). Both are `audio_callback`'s INPUT tick start, which
/// only advances. An instant before the first tick reads NOT settled: the window keeps the mix
/// unfrozen, never mutes a member. Mirror of `genlock_audio_buffering_launch_settled`.
pub fn launch_settled(first_tick_ns: u64, now_ns: u64) -> bool {
    if first_tick_ns == 0 {
        return false;
    }
    now_ns >= first_tick_ns && now_ns - first_tick_ns >= LAUNCH_SETTLE_NS
}

/// Issue 1425 (ROZHODNUTÉ 6217557472 + 6221252305): whether the mix's buffering is frozen on this
/// tick — an output with an audio encoder is active AND the launch has settled. Nothing else
/// freezes it.
pub fn frozen(encoder_active: bool, settled: bool) -> bool {
    encoder_active && settled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_round_up_like_obs_reset_audio2() {
        // OBS's own max: 960 ms at 48 kHz is exactly 45 ticks; the low-latency 20 ms is 1 tick.
        assert_eq!(ticks_for_ms(960, 48_000, AUDIO_OUTPUT_FRAMES), 45);
        assert_eq!(ticks_for_ms(20, 48_000, AUDIO_OUTPUT_FRAMES), 1);
        // 85 ms (the issue-1367 floor) is 4 ticks at both production rates.
        assert_eq!(ticks_for_ms(85, 48_000, AUDIO_OUTPUT_FRAMES), 4);
        assert_eq!(ticks_for_ms(85, 44_100, AUDIO_OUTPUT_FRAMES), 4);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 48_000), 85_333_333);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 44_100), 92_879_818);
        // An exact multiple does not round up; one sample over does.
        assert_eq!(ticks_for_ms(64, 48_000, AUDIO_OUTPUT_FRAMES), 3);
        assert_eq!(ticks_for_ms(0, 48_000, AUDIO_OUTPUT_FRAMES), 0);
        assert_eq!(ticks_for_ms(85, 48_000, 0), 0);
        assert_eq!(ticks_ns(4, AUDIO_OUTPUT_FRAMES, 0), 0);
    }

    #[test]
    fn the_floor_is_zero_ticks_at_both_rates_1425() {
        // Issue 1425 E2 slice 3: no floor. A launch starts at 0 like stock OBS.
        assert_eq!(FLOOR_MS, 0);
        for rate in [44_100, 48_000] {
            assert_eq!(ticks_for_ms(FLOOR_MS, rate, AUDIO_OUTPUT_FRAMES), 0);
            assert_eq!(plan(0, false, rate, AUDIO_OUTPUT_FRAMES).floor_ticks, 0);
        }
    }

    #[test]
    fn plan_starts_every_launch_at_the_floor_and_keeps_the_dynamic_maximum() {
        // The frontend's normal request: no maximum, not fixed.
        let p = plan(0, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!(
            p,
            BufferingPlan {
                floor_ticks: 0,
                max_ticks: DEFAULT_MAX_TICKS,
                fixed: false,
                overridden: false
            }
        );
        // An explicit maximum is kept, whatever its size.
        let p = plan(500, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!((p.floor_ticks, p.max_ticks, p.overridden), (0, 24, false));
        let p = plan(50, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!((p.floor_ticks, p.max_ticks, p.overridden), (0, 3, false));
    }

    #[test]
    fn the_low_latency_toggle_is_overridden_never_a_hard_cap() {
        // OBSBasic::ResetAudio's LowLatencyAudioBuffering: fixed 20 ms.
        let p = plan(20, true, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.overridden && !p.fixed);
        assert_eq!((p.floor_ticks, p.max_ticks), (0, DEFAULT_MAX_TICKS));
        // A fixed request at any size is still dynamic.
        let p = plan(500, true, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(p.overridden && !p.fixed);
        assert_eq!(p.max_ticks, DEFAULT_MAX_TICKS);
        // With no floor, a small dynamic maximum is a maximum, not an override.
        let p = plan(1, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert!(!p.overridden);
        assert_eq!(p.max_ticks, 1);
    }

    #[test]
    fn the_maximum_never_sits_below_the_floor() {
        // The shipped floor 0: no rate or any rate keeps the OBS default maximum.
        for rate in [0, 48_000, 768_000] {
            let p = plan(0, false, rate, AUDIO_OUTPUT_FRAMES);
            assert_eq!((p.floor_ticks, p.max_ticks), (0, DEFAULT_MAX_TICKS));
        }
        // A non-zero floor (the issue-1367 85 ms): a request below it is overridden to the
        // default maximum, and at a rate where the floor is more ticks than the default maximum the
        // default itself sits below it (overridden) and is lifted to the floor (85 ms at 768 kHz =
        // 64 ticks).
        let p = plan_with_floor(85, 50, false, 48_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!(
            (p.floor_ticks, p.max_ticks, p.overridden),
            (4, DEFAULT_MAX_TICKS, true)
        );
        let p = plan_with_floor(85, 0, false, 768_000, AUDIO_OUTPUT_FRAMES);
        assert_eq!((p.floor_ticks, p.max_ticks, p.overridden), (64, 64, true));
        // plan() is plan_with_floor(FLOOR_MS, ..).
        for (max, fixed) in [(0, false), (20, true), (500, false)] {
            assert_eq!(
                plan(max, fixed, 44_100, AUDIO_OUTPUT_FRAMES),
                plan_with_floor(FLOOR_MS, max, fixed, 44_100, AUDIO_OUTPUT_FRAMES)
            );
        }
    }

    #[test]
    fn the_first_tick_raises_a_floor_then_obs_grows_dynamically_above_it() {
        use BufferingAction::*;
        // The rule, on a 4-tick floor (the issue-1367 shape the action still implements).
        assert_eq!(action(0, 4, 45, false, false), Floor);
        assert_eq!(action(0, 4, 45, true, false), Floor);
        assert_eq!(action(3, 4, 45, true, false), Floor);
        assert_eq!(action(4, 4, 45, false, false), None);
        assert_eq!(action(4, 4, 45, true, false), Dynamic);
        assert_eq!(action(17, 4, 45, true, false), Dynamic);
        assert_eq!(action(44, 4, 45, true, false), Dynamic);
        assert_eq!(action(45, 4, 45, true, false), None);
        assert_eq!(action(46, 4, 45, true, false), None);
        assert_eq!(action(0, 4, 4, true, false), Floor);
        assert_eq!(action(4, 4, 4, true, false), None);
        // The shipped 0 floor: stock dynamic behaviour from the first tick.
        assert_eq!(action(0, 0, 45, true, false), Dynamic);
        assert_eq!(action(0, 0, 45, false, false), None);
        assert_eq!(action(1, 0, 45, true, false), Dynamic);
    }

    #[test]
    fn a_frozen_mix_never_grows_dynamically_but_still_raises_the_floor_1425() {
        use BufferingAction::*;
        // Frozen: a source behind the window no longer grows the mix (the E1 ratchet).
        assert_eq!(action(4, 4, 45, true, true), None);
        assert_eq!(action(17, 4, 45, true, true), None);
        assert_eq!(action(4, 4, 45, false, true), None);
        assert_eq!(action(0, 0, 45, true, true), None);
        // A floor is never frozen: the first tick of a launch (or an audio reset) still raises it.
        assert_eq!(action(0, 4, 45, true, true), Floor);
        assert_eq!(action(0, 4, 45, false, true), Floor);
        assert_eq!(action(3, 4, 45, true, true), Floor);
        // Not frozen: OBS's own dynamic increase, as before.
        assert_eq!(action(4, 4, 45, true, false), Dynamic);
        assert_eq!(action(45, 4, 45, true, false), None);
    }

    #[test]
    fn only_a_running_audio_encoder_after_the_launch_settle_freezes_the_mix_1425() {
        // A stream or a recording freezes it once the launch has settled; with no audio encoder
        // (the resolume cg OBS) the mix keeps stock growth, however long it has been quiet.
        assert!(frozen(true, true));
        assert!(!frozen(false, true));
        // ROZHODNUTÉ 6221252305: inside the launch settle window an encoder does not freeze it.
        assert!(!frozen(true, false));
        assert!(!frozen(false, false));
        assert_eq!(
            action(0, 0, 45, true, frozen(false, true)),
            BufferingAction::Dynamic
        );
        assert_eq!(
            action(0, 0, 45, true, frozen(true, true)),
            BufferingAction::None
        );
        assert_eq!(
            action(0, 0, 45, true, frozen(true, false)),
            BufferingAction::Dynamic
        );
    }

    #[test]
    fn the_launch_settles_30_s_of_mix_time_after_the_first_tick_1425() {
        let first = 1_000_000_000;
        assert_eq!(LAUNCH_SETTLE_NS, 30_000_000_000);
        // No tick yet: never settled.
        assert!(!launch_settled(0, first));
        assert!(!launch_settled(0, u64::MAX));
        // The first tick, then the window's edge.
        assert!(!launch_settled(first, first));
        assert!(!launch_settled(first, first + LAUNCH_SETTLE_NS - 1));
        assert!(launch_settled(first, first + LAUNCH_SETTLE_NS));
        assert!(launch_settled(first, first + 3_600_000_000_000));
        // An instant before the first tick reads NOT settled (the input tick only advances, so it
        // never happens live; if it did, the window keeps the mix unfrozen rather than mute).
        assert!(!launch_settled(first, first - 1));
        // Never overflows at the top of the clock.
        assert!(!launch_settled(u64::MAX - 1, u64::MAX));
    }
}
