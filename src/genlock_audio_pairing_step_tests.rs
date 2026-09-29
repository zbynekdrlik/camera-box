//! Issue 1381 (design 5882391108) — the unit tests of the per-source audio SKEW HOLD across a wall
//! step (`audio_step_hold` and its helpers), a `#[path]` child of `genlock_audio_pairing` beside
//! `genlock_audio_pairing_tests.rs` (split to keep each file under the ~1000-line budget). Run
//! standalone with the module: `rustc --test --edition 2021 src/genlock_audio_pairing.rs`.

use super::*;

const WALL: u64 = 1_790_000_000_123_456_789;
const PACKET: u64 = 33_333_333;
const MIN: i64 = 2_000_000;
const OFF: i64 = -1_789_950_000_000_000_000;

/// Drive the hold with a steady 33.3 ms packet feed; `step_ns` is the WALL step (+ = forward), so the
/// live offset (mono − wall) moves by −step; `follow_ns` is how far the stamps have followed.
fn feed(s: &mut AudioStepHold, k: u64, step_ns: i64, follow_ns: i64) -> (i64, AudioStepRelease) {
    let now = 1_000_000_000_000 + k * PACKET;
    let raw = (WALL + k * PACKET).wrapping_add(follow_ns as u64);
    audio_step_hold(s, true, OFF - step_ns, raw, PACKET, now, false, MIN)
}

#[test]
fn a_wall_step_holds_the_pre_step_offset_until_the_stamps_follow_1381() {
    let mut s = AudioStepHold::default();
    for k in 0..10 {
        assert_eq!(feed(&mut s, k, 0, 0), (OFF, AudioStepRelease::None));
    }
    // the receiver's wall jumps +682 ms: the packet keeps the pre-step offset
    assert_eq!(
        feed(&mut s, 10, 682_474_000, 0),
        (OFF, AudioStepRelease::None)
    );
    assert!(s.active && s.step_ns == 682_474_000 && s.held_off_ns == OFF);
    for k in 11..150 {
        assert_eq!(
            feed(&mut s, k, 682_474_000, 0),
            (OFF, AudioStepRelease::None),
            "issue 1381: still held at packet {k}"
        );
    }
    // the stamps follow (+682 ms): released onto the live offset, the landing continuous
    let (off, release) = feed(&mut s, 150, 682_474_000, 682_474_000);
    assert_eq!(
        (off, release),
        (OFF - 682_474_000, AudioStepRelease::Followed)
    );
    assert!(!s.active);
    assert_eq!(audio_step_residual_ns(s.held_off_ns, off), 0);
    // from then on the live offset, no new hold
    assert_eq!(
        feed(&mut s, 151, 682_474_000, 682_474_000),
        (OFF - 682_474_000, AudioStepRelease::None)
    );
}

#[test]
fn a_follow_in_pieces_keeps_the_landing_continuous_1381() {
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    assert_eq!(feed(&mut s, 1, -400_000_000, 0).0, OFF);
    // half of the step followed: the held offset moves with the stamps (raw + off constant)
    let (off, release) = feed(&mut s, 2, -400_000_000, -200_000_000);
    assert_eq!((off, release), (OFF + 200_000_000, AudioStepRelease::None));
    let raw = (WALL + 2 * PACKET).wrapping_sub(200_000_000);
    assert_eq!(
        raw.wrapping_add(off as u64),
        (WALL + 2 * PACKET).wrapping_add(OFF as u64),
        "issue 1381: a partial follow must never open a hole"
    );
    // the rest follows within one packet: released
    let (_, release) = feed(&mut s, 3, -400_000_000, -390_000_000);
    assert_eq!(release, AudioStepRelease::Followed);
}

#[test]
fn a_hold_is_bounded_and_ends_on_a_reset_or_outside_timecode_1381() {
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    feed(&mut s, 1, 682_000_000, 0);
    // held while less than the bound has passed since the start (packet 1)
    let mut k = 2;
    while (k - 1) * PACKET < AUDIO_STEP_HOLD_MAX_NS {
        assert_eq!(feed(&mut s, k, 682_000_000, 0).1, AudioStepRelease::None);
        k += 1;
    }
    let (off, release) = feed(&mut s, k, 682_000_000, 0);
    assert_eq!(
        (off, release),
        (OFF - 682_000_000, AudioStepRelease::Timeout)
    );
    assert!(audio_step_release_places(
        release,
        audio_step_residual_ns(s.held_off_ns, off),
        PACKET
    ));
    assert_eq!(
        audio_step_residual_ns(s.held_off_ns, off),
        -682_000_000,
        "issue 1381: the timeout applies the whole step at once (the log's residual_ms=)"
    );
    // a timeline reset ends a hold (the ingest places that packet itself)
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    feed(&mut s, 1, 682_000_000, 0);
    let now = 1_000_000_000_000 + 2 * PACKET;
    let (off, release) = audio_step_hold(
        &mut s,
        true,
        OFF - 682_000_000,
        WALL + 2 * PACKET,
        PACKET,
        now,
        true,
        MIN,
    );
    assert_eq!((off, release), (OFF - 682_000_000, AudioStepRelease::Reset));
    assert!(!audio_step_release_places(
        release,
        audio_step_residual_ns(s.held_off_ns, off),
        PACKET
    ));
    // leaving timecode mode ends it and forgets the previous packet
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    feed(&mut s, 1, 682_000_000, 0);
    let (off, release) = audio_step_hold(&mut s, false, 7, WALL, PACKET, 1, false, MIN);
    assert_eq!((off, release), (7, AudioStepRelease::Reset));
    assert!(!s.active && s.prev_packet_ns == 0);
    assert_eq!(
        audio_step_hold(&mut s, false, 7, WALL, PACKET, 1, false, MIN),
        (7, AudioStepRelease::None)
    );
    // a jumped follow applies nothing (it appends or books as usual); a catch-up applies the step
    assert!(!audio_step_release_places(
        AudioStepRelease::Followed,
        1_000_000,
        PACKET
    ));
    assert!(audio_step_release_places(
        AudioStepRelease::Followed,
        -682_000_000,
        PACKET
    ));
    assert!(!audio_step_release_places(
        AudioStepRelease::None,
        -682_000_000,
        PACKET
    ));
    assert_eq!(AudioStepRelease::Timeout.token(), "timeout");
    assert_eq!(AudioStepRelease::Followed.token(), "followed");
    assert_eq!(AudioStepRelease::Reset.token(), "reset");
}

#[test]
fn no_hold_on_jitter_a_small_step_a_first_packet_or_a_joint_step_1381() {
    // offset jitter within the 2 ms threshold
    let mut s = AudioStepHold::default();
    for k in 0..50 {
        let step = if k % 2 == 0 { 1_900_000 } else { 0 };
        let (off, release) = feed(&mut s, k, step, 0);
        assert!(!s.active && release == AudioStepRelease::None && off == OFF - step);
    }
    // a step within one packet: nothing held (the booking band handles it as before)
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    assert_eq!(
        feed(&mut s, 1, 33_000_000, 0),
        (OFF - 33_000_000, AudioStepRelease::None)
    );
    assert!(!s.active);
    // a first packet has nothing to compare against
    let mut s = AudioStepHold::default();
    assert_eq!(
        feed(&mut s, 0, 682_000_000, 0),
        (OFF - 682_000_000, AudioStepRelease::None)
    );
    assert!(!s.active);
    // the sender stepped in the same packet as the receiver (the old 50 ms date step, 3 ms apart)
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    assert_eq!(
        feed(&mut s, 1, 51_000_000, 51_000_000),
        (OFF - 51_000_000, AudioStepRelease::None)
    );
    assert!(!s.active);
    // a timeline reset packet never starts a hold
    let mut s = AudioStepHold::default();
    feed(&mut s, 0, 0, 0);
    let (_, release) = audio_step_hold(
        &mut s,
        true,
        OFF - 682_000_000,
        WALL + PACKET,
        PACKET,
        2,
        true,
        MIN,
    );
    assert!(!s.active && release == AudioStepRelease::None);
}

#[test]
fn the_hold_arithmetic_wraps_like_c_1381() {
    let mut s = AudioStepHold {
        prev_off_ns: i64::MAX,
        prev_raw_ns: u64::MAX,
        prev_packet_ns: 2,
        ..AudioStepHold::default()
    };
    // offset wraps from i64::MAX to i64::MIN (a +1 jump in two's complement): within the threshold
    let (off, release) = audio_step_hold(&mut s, true, i64::MIN, 1, 2, 5, false, MIN);
    assert_eq!((off, release), (i64::MIN, AudioStepRelease::None));
    assert!(!s.active);
}

/// One packet of a feed whose arrivals are spaced by `gap_ns` (a catch-up burst compresses them):
/// advances the receiver's monotonic `now` by the gap and the sender's grid stamp by one packet.
fn burst(
    s: &mut AudioStepHold,
    now: &mut u64,
    k: &mut u64,
    gap_ns: u64,
    off: i64,
) -> (i64, AudioStepRelease) {
    *now += gap_ns;
    *k += 1;
    audio_step_hold(s, true, off, WALL + *k * PACKET, PACKET, *now, false, MIN)
}

#[test]
fn a_sender_that_catches_up_without_jumping_its_stamps_is_released_at_once_1381() {
    // SongPlayer's audio emitter: grid stamps never jump under 1 s; after a forward step it emits the
    // missed slots as a burst, so its continuous stamps arrive faster than real time until the step
    // is caught up (modelled here at 4x: each packet a quarter slot after the previous one).
    let mut s = AudioStepHold::default();
    let step = 682_474_000_i64;
    let (mut now, mut k) = (1_000_000_000_000_u64, 0_u64);
    audio_step_hold(&mut s, true, OFF, WALL, PACKET, now, false, MIN);
    for _ in 0..9 {
        burst(&mut s, &mut now, &mut k, PACKET, OFF);
    }
    for _ in 0..140 {
        let r = burst(&mut s, &mut now, &mut k, PACKET, OFF - step);
        assert_eq!(r, (OFF, AudioStepRelease::None), "packet {k}");
    }
    assert!(s.active && s.step_ns == step);
    // the burst: the stamps' age falls by three quarters of a slot per packet; held until it is back
    // within one packet of the pre-step age, then released once
    let mut released = None;
    for _ in 0..40 {
        let (off, release) = burst(&mut s, &mut now, &mut k, PACKET / 4, OFF - step);
        if release != AudioStepRelease::None {
            released = Some((off, release));
            break;
        }
        assert_eq!(off, OFF, "still held at packet {k}");
    }
    let (off, release) = released.expect("issue 1381: the catch-up must release the hold");
    assert_eq!((off, release), (OFF - step, AudioStepRelease::Followed));
    let residual = audio_step_residual_ns(s.held_off_ns, off);
    assert_eq!(
        residual, -step,
        "issue 1381: the release applies the whole step"
    );
    assert!(audio_step_release_places(release, residual, PACKET));
    assert_eq!(audio_stamp_age_ns(10, 3, 4), 3);
    assert_eq!(audio_stamp_age_ns(0, 1, 0), -1, "wraps like C");
}

#[test]
fn a_sender_that_stepped_first_is_never_held_1381() {
    // review round 1: the SENDER's box stepped first (a cross-box timecode source whose sender is the
    // date master). Its stamps are already on the new wall when the receiver's own step lands, so the
    // receiver's step brings them BACK onto its wall: holding the pre-step offset would put the
    // audio a whole step off for the full 10 s bound. The receiver-step packet is a zero-length hold,
    // released at once with the whole step as its residual (the ingest places it: the window before
    // left the audio a step off its stamps), and nothing is held after it.
    for step in [682_474_000_i64, -682_474_000, 89_703_000] {
        // (a) the sender's stamps jump by the step, the receiver steps 2 s later
        let mut s = AudioStepHold::default();
        for k in 0..10 {
            feed(&mut s, k, 0, 0);
        }
        for k in 10..70 {
            assert_eq!(feed(&mut s, k, 0, step), (OFF, AudioStepRelease::None));
        }
        assert_eq!(
            feed(&mut s, 70, step, step),
            (OFF - step, AudioStepRelease::Followed),
            "issue 1381: step {step}: the receiver's step is released on its own packet"
        );
        assert!(!s.active && s.step_ns == step && s.start_ns == 1_000_000_000_000 + 70 * PACKET);
        let residual = audio_step_residual_ns(s.held_off_ns, OFF - step);
        assert!(
            residual == -step
                && audio_step_release_places(AudioStepRelease::Followed, residual, PACKET),
            "issue 1381: step {step}: the zero-length release places the whole step once"
        );
        for k in 71..400 {
            let r = feed(&mut s, k, step, step);
            assert_eq!(
                r,
                (OFF - step, AudioStepRelease::None),
                "issue 1381: step {step}: packet {k} after the receiver's own step must map \
                 through the live offset, never a hold"
            );
            assert!(!s.active);
        }
        // (b) the sender caught up with continuous stamps (a burst forward, a pause backward), the
        // receiver steps 2 s after the catch-up
        let mut s = AudioStepHold::default();
        let (mut now, mut k) = (1_000_000_000_000_u64, 0_u64);
        audio_step_hold(&mut s, true, OFF, WALL, PACKET, now, false, MIN);
        for _ in 0..9 {
            burst(&mut s, &mut now, &mut k, PACKET, OFF);
        }
        if step > 0 {
            // forward: 4x arrivals until the stamps are the step ahead of the receiver's wall
            let mut caught = 0_u64;
            while caught < step as u64 {
                burst(&mut s, &mut now, &mut k, PACKET / 4, OFF);
                caught += PACKET - PACKET / 4;
            }
        } else {
            // backward: the sender pauses for the step
            burst(&mut s, &mut now, &mut k, PACKET + step.unsigned_abs(), OFF);
        }
        for _ in 0..60 {
            let r = burst(&mut s, &mut now, &mut k, PACKET, OFF);
            assert_eq!(r, (OFF, AudioStepRelease::None));
        }
        let (off, release) = burst(&mut s, &mut now, &mut k, PACKET, OFF - step);
        assert_eq!((off, release), (OFF - step, AudioStepRelease::Followed));
        assert!(audio_step_release_places(
            release,
            audio_step_residual_ns(s.held_off_ns, off),
            PACKET
        ));
        for _ in 0..300 {
            let r = burst(&mut s, &mut now, &mut k, PACKET, OFF - step);
            assert_eq!(
                r,
                (OFF - step, AudioStepRelease::None),
                "issue 1381: step {step}: after a caught-up sender the receiver's step must not \
                 hold (packet {k})"
            );
            assert!(!s.active);
        }
    }
}

#[test]
fn the_nominal_age_follows_in_band_and_reanchors_out_of_band_1381() {
    // review round 1: the reference both age tests use. Seeded by the first packet, moved by 1/1024
    // of an in-band difference (truncated toward zero, both signs), left alone out of band, and
    // re-anchored after ten minutes out of band without a hold.
    let base = 1_000_000_000_000_u64;
    let mut s = AudioStepHold::default();
    audio_step_hold(&mut s, true, OFF, WALL, PACKET, base, false, MIN);
    let seed = audio_stamp_age_ns(base, WALL, OFF);
    assert_eq!((s.nominal_age_ns, s.nominal_dev_since_ns), (seed, 0));
    // an arrival 3.000001 ms late / 5.000003 ms early: +2929 / -4885 ns (1/1024 of the
    // difference, truncated toward zero)
    audio_step_hold(
        &mut s,
        true,
        OFF,
        WALL + PACKET,
        PACKET,
        base + PACKET + 3_000_001,
        false,
        MIN,
    );
    assert_eq!(s.nominal_age_ns, seed + 2_929);
    audio_step_hold(
        &mut s,
        true,
        OFF,
        WALL + 2 * PACKET,
        PACKET,
        base + 2 * PACKET - 5_000_003,
        false,
        MIN,
    );
    assert_eq!(s.nominal_age_ns, seed + 2_929 - 4_885);
    // stamps 100 ms ahead of the receiver's wall (the sender moved alone): out of band, the timer runs
    let nominal = s.nominal_age_ns;
    let far = 100_000_000_000_u64;
    let t0 = base + 3 * PACKET;
    for n in 0..6_u64 {
        let raw = WALL + 3 * PACKET + n * far + 100_000_000;
        let r = audio_step_hold(&mut s, true, OFF, raw, PACKET, t0 + n * far, false, MIN);
        assert_eq!(r, (OFF, AudioStepRelease::None));
        assert_eq!(
            (s.nominal_age_ns, s.nominal_dev_since_ns),
            (nominal, t0),
            "packet {n}"
        );
    }
    // exactly ten minutes out of band: the age is the nominal from now on
    let raw = WALL + 3 * PACKET + 6 * far + 100_000_000;
    audio_step_hold(&mut s, true, OFF, raw, PACKET, t0 + 6 * far, false, MIN);
    assert_eq!(
        (s.nominal_age_ns, s.nominal_dev_since_ns),
        (audio_stamp_age_ns(t0 + 6 * far, raw, OFF), 0),
        "issue 1381: an age out of band for AUDIO_STEP_NOMINAL_REANCHOR_NS re-anchors"
    );
    assert_eq!(AUDIO_STEP_NOMINAL_REANCHOR_NS, 6 * far);
    // inside a hold the nominal is frozen
    let mut s = AudioStepHold::default();
    for k in 0..10 {
        feed(&mut s, k, 0, 0);
    }
    let frozen = s.nominal_age_ns;
    for k in 10..40 {
        feed(&mut s, k, 682_474_000, 0);
        assert!(s.active && s.nominal_age_ns == frozen && s.nominal_dev_since_ns == 0);
    }
}

#[test]
fn the_render_thread_freeze_is_bounded_by_the_hold_bound_1381() {
    let start = 5_000_000_000_u64;
    assert!(audio_step_freezes_video(true, start, start));
    assert!(audio_step_freezes_video(
        true,
        start,
        start + AUDIO_STEP_HOLD_MAX_NS - 1
    ));
    assert!(
        !audio_step_freezes_video(true, start, start + AUDIO_STEP_HOLD_MAX_NS),
        "issue 1381: a hold whose audio stopped never freezes the video side past the bound"
    );
    assert!(!audio_step_freezes_video(false, start, start));
}
