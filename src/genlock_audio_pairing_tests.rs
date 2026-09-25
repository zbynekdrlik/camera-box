//! Issue 1367 — the unit tests of `genlock_audio_pairing` (a `#[path]` child, split out to keep the
//! authority under the ~1000-line budget). Run standalone with the module:
//! `rustc --test --edition 2021 src/genlock_audio_pairing.rs`.

use super::*;

const IV30: u64 = 33_333_333;
const IV60: u64 = 16_666_666;
const WALL: u64 = 1_790_000_000_123_456_789;

#[test]
fn delay_is_hold_in_ns() {
    assert_eq!(genlock_audio_delay_ns(3), 3_000_000);
    assert_eq!(genlock_audio_delay_ns(0), 0);
    assert_eq!(genlock_audio_delay_ns(2000), 2_000_000_000);
}

// ---- the video stamp→present delay measurement ---------------------------------------

#[test]
fn sample_never_returns_the_unseeded_sentinel() {
    // a frame stamped at or after the tick would read 0 and re-seed the EMA every tick.
    assert_eq!(video_delay_sample_ns(WALL, WALL), 1);
    assert_eq!(video_delay_sample_ns(WALL, WALL + 5_000_000), 1);
    let mut t = VideoDelayTracker::default();
    run(&mut t, 100_000_000, IV30, 200);
    for _ in 0..50 {
        video_delay_track(&mut t, 0, video_delay_sample_ns(WALL, WALL + 1), IV30);
        assert_ne!(
            t.smoothed_ns, 0,
            "a seeded EMA must never fall back to the sentinel"
        );
    }
}

#[test]
fn ema_step_never_overflows_at_the_extremes() {
    // two's-complement wrap, exactly as the C uint64 arithmetic: the difference reads -2 / +2
    // and an eighth of it truncates to 0, so the EMA stays put.
    assert_eq!(video_delay_smooth_ns(1, u64::MAX), 1);
    assert_eq!(video_delay_smooth_ns(u64::MAX, 1), u64::MAX);
    // i64::MAX minus a "negative" (above 2^63) smoothed value overflowed the old signed
    // difference; the wrapping form reads it as -11 and steps by -11/8 = -1.
    assert_eq!(
        video_delay_smooth_ns((1u64 << 63) + 10, i64::MAX as u64),
        (1u64 << 63) + 9
    );
    // a difference that crosses i64 must not panic.
    assert_eq!(
        video_delay_smooth_ns(i64::MAX as u64 + 7, 3),
        (i64::MAX as u64 + 7)
            .wrapping_add(((3u64.wrapping_sub(i64::MAX as u64 + 7)) as i64 / 8) as u64)
    );
    // an ordinary step is unchanged by the wrapping form.
    assert_eq!(video_delay_smooth_ns(100_000_000, 108_000_000), 101_000_000);
}

#[test]
fn only_a_timecode_hold_needs_the_live_offset() {
    use AudioHoldMode::*;
    assert!(!audio_needs_live_offset(Off, Off));
    assert!(!audio_needs_live_offset(Latency, Latency));
    assert!(!audio_needs_live_offset(Latency, Off));
    assert!(audio_needs_live_offset(Timecode, Latency));
    assert!(audio_needs_live_offset(Latency, Timecode));
    assert!(audio_needs_live_offset(Timecode, Timecode));
}

#[test]
fn sample_is_the_head_age_at_the_scheduled_tick() {
    assert_eq!(
        video_delay_sample_ns(1_000_100_000_000, 1_000_000_000_000),
        100_000_000
    );
    // a frame stamped after the tick (a sender stamping ahead) clamps to 1 ns, never 0.
    assert_eq!(video_delay_sample_ns(5, 9), 1);
}

#[test]
fn ema_seeds_on_the_first_sample_and_steps_an_eighth() {
    assert_eq!(video_delay_smooth_ns(0, 97_000_000), 97_000_000);
    assert_eq!(video_delay_smooth_ns(100_000_000, 108_000_000), 101_000_000);
    assert_eq!(video_delay_smooth_ns(100_000_000, 92_000_000), 99_000_000);
    // truncation toward zero on both signs (C int64 division).
    assert_eq!(video_delay_smooth_ns(100, 107), 100);
    assert_eq!(video_delay_smooth_ns(100, 93), 100);
}

#[test]
fn round_ms_is_nearest_and_never_zero() {
    assert_eq!(video_delay_round_ms(66_666_666), 67);
    assert_eq!(video_delay_round_ms(66_499_999), 66);
    assert_eq!(video_delay_round_ms(66_500_000), 67);
    assert_eq!(video_delay_round_ms(0), 1);
    assert_eq!(video_delay_round_ms(400_000), 1);
    assert_eq!(video_delay_round_ms(u64::MAX), u32::MAX);
}

#[test]
fn moved_is_half_a_frame_or_more() {
    assert!(
        video_delay_moved(0, 100_000_000, IV30),
        "nothing applied yet always moves"
    );
    // 30 fps: half a frame is 16.67 ms.
    assert!(!video_delay_moved(100, 116_666_666, IV30));
    assert!(video_delay_moved(100, 116_666_667, IV30));
    assert!(video_delay_moved(100, 83_333_333, IV30));
    assert!(!video_delay_moved(100, 83_333_334, IV30));
    // 60 fps: 8.33 ms.
    assert!(video_delay_moved(50, 58_333_333, IV60));
    assert!(!video_delay_moved(50, 58_333_332, IV60));
    // unknown interval never moves once something is applied.
    assert!(!video_delay_moved(50, 900_000_000, 0));
}

fn run(t: &mut VideoDelayTracker, sample: u64, iv: u64, ticks: u32) {
    for _ in 0..ticks {
        video_delay_track(t, 0, sample, iv);
    }
}

#[test]
fn tracker_applies_the_first_delay_after_the_settle() {
    let mut t = VideoDelayTracker::default();
    video_delay_track(&mut t, 0, 100_000_000, IV30);
    assert_eq!(t.applied_ms, 0, "armed, not applied");
    assert_eq!(t.settle_ticks, VIDEO_DELAY_SETTLE_TICKS);
    run(&mut t, 100_000_000, IV30, VIDEO_DELAY_SETTLE_TICKS - 1);
    assert_eq!(t.applied_ms, 0);
    run(&mut t, 100_000_000, IV30, 1);
    assert_eq!(t.applied_ms, 100);
    assert_eq!(t.settle_ticks, 0);
}

#[test]
fn a_one_frame_step_lands_on_the_new_delay_not_mid_step() {
    let mut t = VideoDelayTracker::default();
    run(&mut t, 100_000_000, IV30, 200);
    assert_eq!(t.applied_ms, 100);
    // the FIFO settles one frame shallower.
    run(&mut t, 66_666_667, IV30, 400);
    assert_eq!(
        t.applied_ms, 67,
        "the re-application must apply the NEW delay (66.7 ms), not the value at the half-frame crossing (~83 ms)"
    );
    // and deeper again.
    run(&mut t, 100_000_000, IV30, 400);
    assert_eq!(t.applied_ms, 100);
}

#[test]
fn single_tick_holds_never_rearm() {
    let mut t = VideoDelayTracker::default();
    run(&mut t, 100_000_000, IV30, 200);
    for i in 0..3000u32 {
        // one tick in ten presents the frame a tick older (a hold).
        let s = if i % 10 == 0 {
            133_333_333
        } else {
            100_000_000
        };
        video_delay_track(&mut t, 0, s, IV30);
        assert_eq!(
            t.settle_ticks, 0,
            "a single held tick must not arm a re-application"
        );
    }
    assert_eq!(t.applied_ms, 100);
}

#[test]
fn a_transient_that_reverses_during_the_settle_applies_nothing() {
    let mut t = VideoDelayTracker::default();
    run(&mut t, 100_000_000, IV30, 200);
    // a 20-tick excursion one frame deeper arms the settle, then the delay comes back.
    run(&mut t, 133_333_333, IV30, 20);
    assert!(t.settle_ticks > 0);
    run(&mut t, 100_000_000, IV30, VIDEO_DELAY_SETTLE_TICKS);
    assert_eq!(t.settle_ticks, 0);
    assert_eq!(
        t.applied_ms, 100,
        "the reversed transient must leave the applied delay alone"
    );
}

// ---- the audio hold + placement --------------------------------------------------------

#[test]
fn hold_mode_selection() {
    assert_eq!(
        audio_hold_mode(false, 3, true, 97, false),
        AudioHoldMode::Off
    );
    assert_eq!(
        audio_hold_mode(true, 0, true, 97, false),
        AudioHoldMode::Off
    );
    assert_eq!(
        audio_hold_mode(true, 3, true, 0, true),
        AudioHoldMode::Latency
    );
    assert_eq!(
        audio_hold_mode(true, 3, true, 0, false),
        AudioHoldMode::Pending
    );
    assert_eq!(
        audio_hold_mode(true, 3, false, 97, false),
        AudioHoldMode::Latency
    );
    assert_eq!(
        audio_hold_mode(true, 3, false, 0, false),
        AudioHoldMode::Latency
    );
    assert_eq!(
        audio_hold_mode(true, 3, true, 97, false),
        AudioHoldMode::Timecode
    );
    assert_eq!(
        audio_hold_mode(true, 3, true, 97, true),
        AudioHoldMode::Timecode
    );
    assert_eq!(audio_hold_ms(AudioHoldMode::Off, 3, 97), 0);
    assert_eq!(audio_hold_ms(AudioHoldMode::Latency, 3, 97), 3);
    assert_eq!(audio_hold_ms(AudioHoldMode::Timecode, 3, 97), 97);
    assert_eq!(AudioHoldMode::Timecode.code(), 2);
    assert_eq!(AudioHoldMode::Latency.token(), "latency");
}

#[test]
fn wall_to_mono_offset_is_signed() {
    // QPC-style monotonic (seconds since boot) vs a 2026 wall epoch.
    let wall = 1_790_000_000_000_000_000u64;
    let mono = 86_400_000_000_000u64;
    assert_eq!(audio_wall_to_mono_ns(mono, wall), mono as i64 - wall as i64);
    assert_eq!(audio_wall_to_mono_ns(wall + 5, wall), 5);
}

#[test]
fn timecode_term_places_the_packet_at_tc_plus_offset_plus_delay() {
    // the ingest computes in.timestamp = tc + timing_adjust, then adds the term.
    let tc = 1_790_000_000_123_000_000u64;
    let arrival_wall = tc + 2_500_000;
    let mono_at_arrival = 86_400_000_000_000u64;
    let timing_adjust = mono_at_arrival.wrapping_sub(tc); // reset_audio_timing
    let off_live = audio_wall_to_mono_ns(mono_at_arrival, arrival_wall);
    let term = audio_place_term_ns(AudioHoldMode::Timecode, 97, off_live, timing_adjust);
    let placed = tc.wrapping_add(timing_adjust).wrapping_add(term as u64);
    // tc mapped to mono through the live offset, plus the video's delay.
    let want = (tc as i64 + off_live + 97_000_000) as u64;
    assert_eq!(placed, want);
    // the latency term keeps the #1303 arrival basis.
    let lat = audio_place_term_ns(AudioHoldMode::Latency, 3, off_live, timing_adjust);
    assert_eq!(
        tc.wrapping_add(timing_adjust).wrapping_add(lat as u64),
        mono_at_arrival + 3_000_000
    );
    assert_eq!(
        audio_place_term_ns(AudioHoldMode::Off, 97, off_live, timing_adjust),
        0
    );
}

#[test]
fn a_later_placement_follows_the_live_offset_not_the_first_one() {
    // the wall-vs-mono offset walked 300 ms since the first packet (resolume: 318 ms).
    let tc0 = 1_790_000_000_000_000_000u64;
    let mono0 = 50_000_000_000_000u64;
    let timing_adjust = mono0.wrapping_sub(tc0);
    let tc = tc0 + 3_600_000_000_000; // an hour later
    let off_live = audio_wall_to_mono_ns(mono0 + 3_600_000_000_000 + 300_000_000, tc);
    let term = audio_place_term_ns(AudioHoldMode::Timecode, 67, off_live, timing_adjust);
    let placed = tc.wrapping_add(timing_adjust).wrapping_add(term as u64);
    assert_eq!(placed, mono0 + 3_600_000_000_000 + 300_000_000 + 67_000_000);
}

#[test]
fn shift_is_the_delay_delta_for_a_timecode_change() {
    let off = -1_789_000_000_000_000_000i64;
    let ta = 17_000_000_000_000_000_000u64;
    let a = audio_place_term_ns(AudioHoldMode::Timecode, 100, off, ta);
    let b = audio_place_term_ns(AudioHoldMode::Timecode, 67, off, ta);
    // the level shift of a timecode->timecode change is exactly the delay delta (the live offset
    // and timing_adjust cancel); from a zero term the shift is the hold itself.
    assert_eq!(
        audio_level_shift_ns(AudioHoldAction::Place, AudioHoldMode::Timecode, b, a, 0),
        -33_000_000
    );
    let l = audio_place_term_ns(AudioHoldMode::Latency, 3, off, ta);
    assert_eq!(
        audio_level_shift_ns(AudioHoldAction::Place, AudioHoldMode::Latency, l, 0, 0),
        3_000_000
    );
}

// ---- the pairing offset + health -------------------------------------------------------

#[test]
fn video_reference_is_the_measurement_else_the_pin() {
    assert_eq!(video_delay_reference_ns(97_400_000, 3), 97_400_000);
    assert_eq!(video_delay_reference_ns(0, 3), 3_000_000);
}

#[test]
fn pairing_offset_is_measured_against_the_real_video_delay() {
    // the #1303 fixed 3 ms hold against a 97 ms video delay: the defect is VISIBLE now.
    assert_eq!(pairing_offset_ms(3_000_000, 97_000_000), -94);
    // audio following the measured delay.
    assert_eq!(pairing_offset_ms(97_000_000, 97_400_000), 0);
    assert_eq!(pairing_offset_ms(100_000_000, 83_400_000), 16);
    assert_eq!(pairing_offset_ms(67_000_000, 83_700_000), -16);
    // never held.
    assert_eq!(pairing_offset_ms(0, 97_000_000), -97);
    assert_eq!(pairing_offset_ms(923_000_000, 923_000_000), 0);
}

fn healthy() -> AudioPairingFacets {
    AudioPairingFacets {
        audio_enabled: true,
        is_program_source: true,
        asrc_saturated: false,
        pairing_offset_ms: 0,
        frame_interval_ms: 33,
    }
}

#[test]
fn healthy_program_audio_is_ok() {
    assert_eq!(decide_audio_health(&healthy()), AudioPairingHealth::Ok);
}

#[test]
fn program_source_with_audio_off_is_degraded() {
    let mut f = healthy();
    f.audio_enabled = false;
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::AudioDisabledOnProgram
    );
}

#[test]
fn non_program_source_with_audio_off_is_ok() {
    // a camera input keeps ndi_audio=false by design — NOT a fault.
    let mut f = healthy();
    f.is_program_source = false;
    f.audio_enabled = false;
    f.pairing_offset_ms = -97;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
}

#[test]
fn asrc_saturated_is_degraded() {
    let mut f = healthy();
    f.asrc_saturated = true;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::AsrcSaturated);
}

#[test]
fn asrc_saturated_ignored_when_audio_disabled_non_program() {
    let mut f = healthy();
    f.is_program_source = false;
    f.audio_enabled = false;
    f.asrc_saturated = true;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
}

#[test]
fn pairing_offset_within_half_a_frame_is_ok() {
    let mut f = healthy();
    f.pairing_offset_ms = 16; // 2*16 = 32, not > 33
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
    f.pairing_offset_ms = -16;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
}

#[test]
fn pairing_offset_beyond_half_a_frame_is_degraded() {
    let mut f = healthy();
    f.pairing_offset_ms = 17;
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::PairingOffsetExceeded
    );
    f.pairing_offset_ms = -94; // the resolume #1303 defect
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::PairingOffsetExceeded
    );
}

#[test]
fn pairing_offset_bound_follows_frame_interval() {
    // at 60 fps half a frame is 8 ms.
    let mut f = healthy();
    f.frame_interval_ms = 16;
    f.pairing_offset_ms = 9;
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::PairingOffsetExceeded
    );
    f.pairing_offset_ms = 8;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::Ok);
}

#[test]
fn pairing_offset_extremes_do_not_overflow() {
    let mut f = healthy();
    f.pairing_offset_ms = i64::MIN;
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::PairingOffsetExceeded
    );
}

// ---- precedence ----------------------------------------------------------------

#[test]
fn audio_disabled_program_beats_asrc_and_offset() {
    let mut f = healthy();
    f.audio_enabled = false;
    f.asrc_saturated = true;
    f.pairing_offset_ms = 999;
    assert_eq!(
        decide_audio_health(&f),
        AudioPairingHealth::AudioDisabledOnProgram
    );
}

#[test]
fn asrc_saturated_beats_pairing_offset() {
    let mut f = healthy();
    f.asrc_saturated = true;
    f.pairing_offset_ms = 999;
    assert_eq!(decide_audio_health(&f), AudioPairingHealth::AsrcSaturated);
}

#[test]
fn codes_match_the_c_enum_values() {
    assert_eq!(AudioPairingHealth::Ok.code(), 0);
    assert_eq!(AudioPairingHealth::AudioDisabledOnProgram.code(), 1);
    assert_eq!(AudioPairingHealth::AsrcSaturated.code(), 2);
    assert_eq!(AudioPairingHealth::PairingOffsetExceeded.code(), 3);
}

// ---- issue 1367 (ROZHODNUTÉ 5827497952): the locked video delay + the slewed audio ---------

#[test]
fn the_lock_follows_the_latched_depth_else_waits_else_is_free_1367() {
    assert_eq!(video_delay_lock_ms(2, false, IV30), 67);
    assert_eq!(
        video_delay_lock_ms(3, true, IV30),
        100,
        "a relock keeps the old D"
    );
    assert_eq!(video_delay_lock_ms(2, false, IV60), 33);
    assert_eq!(video_delay_lock_ms(0, true, IV30), VIDEO_DELAY_LOCK_PENDING);
    assert_eq!(video_delay_lock_ms(0, false, IV30), 0);
    assert_eq!(video_delay_lock_ms(2, true, 0), VIDEO_DELAY_LOCK_PENDING);
    // a huge D never aliases the pending sentinel.
    assert_eq!(
        video_delay_lock_ms(u64::MAX, false, IV30),
        VIDEO_DELAY_LOCK_PENDING - 1
    );
}

#[test]
fn a_locked_tracker_applies_the_lock_and_never_follows_the_float_1367() {
    let mut t = VideoDelayTracker::default();
    // pending: the EMA runs, nothing is applied, nothing arms.
    run_locked(&mut t, VIDEO_DELAY_LOCK_PENDING, 66_666_667, 200);
    assert_eq!(t.applied_ms, 0);
    assert_eq!(t.settle_ticks, 0);
    assert!(t.smoothed_ns > 60_000_000);
    // latched: applied at once, and a floating depth never moves it. (design 5830750134: under a
    // lock the tracker now COUNTS a sustained offset of the realized delay -- the settle counter is
    // no longer pinned at 0 -- but excursions shorter than VIDEO_DELAY_FOLLOW_TICKS never apply.)
    run_locked(&mut t, 100, 66_666_667, 1);
    assert_eq!(t.applied_ms, 100);
    for i in 0..2_000u32 {
        let s = [66_666_667u64, 100_000_000, 133_333_333][(i / 100 % 3) as usize];
        video_delay_track(&mut t, 100, s, IV30);
        assert_eq!(t.applied_ms, 100);
        assert!(t.settle_ticks < VIDEO_DELAY_FOLLOW_TICKS);
    }
    // released (an N>=2 source): the free tracker takes over from the EMA.
    run_locked(&mut t, 0, 66_666_667, 200);
    assert_eq!(t.applied_ms, 67);
}

fn run_locked(t: &mut VideoDelayTracker, lock: u32, sample: u64, ticks: u32) {
    for _ in 0..ticks {
        video_delay_track(t, lock, sample, IV30);
    }
}

#[test]
fn the_withhold_expires_ten_seconds_after_the_first_packet_1367() {
    assert!(!audio_withhold_expired(0, u64::MAX), "no packet yet");
    let t0 = 5_000_000_000u64;
    assert!(!audio_withhold_expired(t0, t0));
    assert!(!audio_withhold_expired(t0, t0 + AUDIO_WITHHOLD_MAX_NS - 1));
    assert!(audio_withhold_expired(t0, t0 + AUDIO_WITHHOLD_MAX_NS));
    assert!(
        !audio_withhold_expired(t0, 1),
        "a clock behind the first packet"
    );
    assert_eq!(AudioHoldMode::Pending.token(), "pending");
    assert_eq!(AudioHoldMode::Pending.code(), 3);
    assert!(!AudioHoldMode::Pending.is_active());
    assert!(!AudioHoldMode::Off.is_active());
    assert!(AudioHoldMode::Latency.is_active());
    assert!(AudioHoldMode::Timecode.is_active());
    assert_eq!(audio_hold_ms(AudioHoldMode::Pending, 3, 97), 0);
    assert_eq!(audio_place_term_ns(AudioHoldMode::Pending, 97, -5, 7), 0);
}

#[test]
fn a_hold_change_while_playing_slews_never_steps_1367() {
    use AudioHoldAction::*;
    use AudioHoldMode::*;
    let act = |pm, ph, m, h, cont, slew, pend| audio_hold_action(pm, ph, m, h, cont, slew, pend);
    // withheld while the delay is unknown.
    assert_eq!(act(Off, 0, Pending, 0, false, true, false), Withhold);
    assert_eq!(act(Pending, 0, Pending, 0, true, true, false), Withhold);
    // the first real placement.
    assert_eq!(act(Pending, 0, Timecode, 100, true, true, false), Place);
    assert_eq!(act(Off, 0, Latency, 3, true, true, false), Place);
    // steady.
    assert_eq!(
        act(Timecode, 100, Timecode, 100, true, true, false),
        Continue
    );
    assert_eq!(act(Off, 0, Off, 0, false, true, false), Continue);
    // a relock with a new D, or the late latency->timecode switch: a SLEW.
    assert_eq!(act(Timecode, 100, Timecode, 67, true, true, false), Slew);
    assert_eq!(act(Latency, 3, Timecode, 100, true, true, false), Slew);
    assert_eq!(act(Timecode, 67, Timecode, 100, true, true, true), Slew);
    // no resampler: the legacy step.
    assert_eq!(
        act(Timecode, 100, Timecode, 67, true, false, false),
        Replace
    );
    // a discontinuity places at the full new term, slew or not.
    assert_eq!(act(Timecode, 100, Timecode, 67, false, true, false), Place);
    assert_eq!(act(Timecode, 67, Timecode, 67, false, true, true), Place);
    assert_eq!(act(Timecode, 67, Timecode, 67, true, false, true), Place);
    assert_eq!(act(Timecode, 67, Timecode, 67, true, true, true), Continue);
    // genlock turned off while playing.
    assert_eq!(act(Timecode, 67, Off, 0, true, true, false), Place);
    assert_eq!(Replace.code(), 4);
}

#[test]
fn a_placement_shifts_the_level_by_the_true_buffer_jump_1367() {
    use AudioHoldAction::*;
    use AudioHoldMode::*;
    // 100 -> 67 ms with 10 ms of an earlier slew still owed.
    assert_eq!(
        audio_level_shift_ns(Place, Timecode, 67_000_000, 100_000_000, 10_000_000),
        -23_000_000
    );
    assert_eq!(
        audio_level_shift_ns(Replace, Timecode, 67_000_000, 100_000_000, 0),
        -33_000_000
    );
    // a first placement moves nothing (no captured level), a slew moves it step by step.
    assert_eq!(audio_level_shift_ns(Place, Pending, i64::MIN, 0, 0), 0);
    assert_eq!(audio_level_shift_ns(Place, Off, 5, 0, 0), 0);
    assert_eq!(audio_level_shift_ns(Slew, Timecode, 67, 100, 0), 0);
    assert_eq!(audio_level_shift_ns(Continue, Timecode, 67, 100, 9), 0);
}

#[test]
fn the_slew_moves_one_ms_per_second_and_lands_exactly_1367() {
    let dt = 10_666_666u64; // one 512-sample callback at 48 kHz
    assert_eq!(audio_slew_step_ns(33_000_000, dt), 10_666);
    assert_eq!(audio_slew_step_ns(-33_000_000, dt), -10_666);
    assert_eq!(
        audio_slew_step_ns(4_000, dt),
        4_000,
        "the tail lands exactly"
    );
    assert_eq!(audio_slew_step_ns(0, dt), 0);
    assert_eq!(
        audio_slew_step_ns(i64::MIN, u64::MAX),
        -((u64::MAX / 1_000_000) as i64),
        "a saturated cap, never an overflow"
    );
    assert_eq!(audio_slew_ppm(10_666, dt), 10_666.0 * 1e6 / dt as f64);
    assert_eq!(audio_slew_ppm(5, 0), 0.0);
    // a 33 ms change settles in ~33 s of callbacks, landing on exactly 0 owed.
    let mut remaining = 33_333_333i64;
    let mut callbacks = 0u32;
    while remaining != 0 {
        remaining -= audio_slew_step_ns(remaining, dt);
        callbacks += 1;
    }
    let secs = callbacks as f64 * dt as f64 / 1e9;
    assert!((33.0..34.0).contains(&secs), "settled in {secs} s");
}

#[test]
fn a_slew_step_is_booked_out_of_the_smoothing_timeline_1367() {
    // a stretch (positive step) leaves more samples than source time: the next expected source
    // timestamp moves back by the step; a compress moves it forward.
    assert_eq!(audio_slew_book_ts_ns(1_000_000_000, 10_666), 999_989_334);
    assert_eq!(audio_slew_book_ts_ns(1_000_000_000, -10_666), 1_000_010_666);
    assert_eq!(audio_slew_book_ts_ns(5, 0), 5);
    // wraps like the C uint64 arithmetic.
    assert_eq!(audio_slew_book_ts_ns(0, 1), u64::MAX);
}

#[test]
fn a_late_placement_folds_the_owed_slew_into_the_level_1367() {
    use AudioHoldAction::*;
    assert_eq!(
        audio_placed_slew_fold_ns(Continue, true, 7_000_000),
        7_000_000
    );
    assert_eq!(
        audio_placed_slew_fold_ns(Slew, true, -33_000_000),
        -33_000_000
    );
    assert_eq!(audio_placed_slew_fold_ns(Continue, false, 7_000_000), 0);
    assert_eq!(audio_placed_slew_fold_ns(Slew, false, 7_000_000), 0);
    assert_eq!(audio_placed_slew_fold_ns(Place, true, 7_000_000), 0);
    assert_eq!(audio_placed_slew_fold_ns(Replace, true, 7_000_000), 0);
    assert_eq!(audio_placed_slew_fold_ns(Withhold, true, 7_000_000), 0);
}

// ---- issue 1367 (design 5830750134): the hold never exceeds the realized video delay ------------

#[test]
fn a_locked_hold_never_exceeds_the_realized_video_delay_1367() {
    // live 25.9.2026 12:04: the latched D 12 asked 400 ms, the video realized ~233 ms, and the audio
    // held 400 (audio_pairing_offset_ms walking to +166). The realized delay bounds the hold once
    // it has stayed half a frame or more under it for VIDEO_DELAY_FOLLOW_TICKS.
    let mut t = VideoDelayTracker::default();
    run_locked(&mut t, VIDEO_DELAY_LOCK_PENDING, 233_333_333, 200);
    run_locked(&mut t, 400, 233_333_333, 1);
    assert_eq!(t.applied_ms, 400, "a new lock applies at once");
    assert_eq!(t.locked_ms, 400);
    run_locked(&mut t, 400, 233_333_333, VIDEO_DELAY_FOLLOW_TICKS - 1);
    assert_eq!(t.applied_ms, 400, "not before the follow window");
    run_locked(&mut t, 400, 233_333_333, 1);
    assert_eq!(t.applied_ms, 233, "the hold follows the realized delay");
    // it stays there, and a NEW (re-measured) lock applies at once again.
    run_locked(&mut t, 400, 233_333_333, 500);
    assert_eq!(t.applied_ms, 233);
    run_locked(&mut t, 100, 233_333_333, 1);
    assert_eq!(t.applied_ms, 100);
}

#[test]
fn the_hold_climb_onto_a_new_lock_never_moves_the_audio_1367() {
    // a fresh latch at D 4 while the conveyor still sits at 1 frame: the hold climbs one frame per
    // 30-tick throttle window (90 ticks to D), inside the follow window, so the audio stays on the
    // lock the whole way.
    let mut t = VideoDelayTracker::default();
    run_locked(&mut t, VIDEO_DELAY_LOCK_PENDING, 33_333_333, 100);
    run_locked(&mut t, 133, 33_333_333, 1);
    for depth in 1..=4u64 {
        run_locked(&mut t, 133, depth * IV30, 30);
        assert_eq!(t.applied_ms, 133, "climbing at depth {depth}");
    }
    run_locked(&mut t, 133, 4 * IV30, 1_000);
    assert_eq!((t.applied_ms, t.settle_ticks), (133, 0));
}

#[test]
fn a_realized_delay_over_the_lock_is_followed_too_1367() {
    // a capped lock (base + 3) under a genuinely slow arrival: the video presents deeper than the
    // lock, and the audio pairs with the video actually on air.
    let mut t = VideoDelayTracker::default();
    run_locked(&mut t, 133, 200_000_000, 1);
    assert_eq!(t.applied_ms, 133);
    run_locked(&mut t, 133, 200_000_000, VIDEO_DELAY_FOLLOW_TICKS + 20);
    assert_eq!(t.applied_ms, 200);
    // released to the free tracker: the follow count never leaks into its settle countdown.
    run_locked(&mut t, 133, 100_000_000, 50);
    run_locked(&mut t, 0, 200_000_000, 1);
    // a fresh free-tracker arm, not the lock's leftover count ticking down.
    assert_eq!((t.locked_ms, t.settle_ticks), (0, VIDEO_DELAY_SETTLE_TICKS));
}
