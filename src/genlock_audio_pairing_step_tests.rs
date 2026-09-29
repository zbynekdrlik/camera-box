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
        let (off, release) = feed(&mut s, k, if k % 2 == 0 { 1_900_000 } else { 0 }, 0);
        assert!(!s.active && release == AudioStepRelease::None && off != 0);
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
    // offset wraps from i64::MAX to i64::MIN (a -1 step in two's complement): within the threshold
    let (off, release) = audio_step_hold(&mut s, true, i64::MIN, 1, 2, 5, false, MIN);
    assert_eq!((off, release), (i64::MIN, AudioStepRelease::None));
    assert!(!s.active);
}

#[test]
fn a_sender_that_catches_up_without_jumping_its_stamps_is_released_at_once_1381() {
    // SongPlayer's audio emitter: grid stamps never jump under 1 s; after a forward step it emits the
    // missed slots at once, so its stamps arrive a whole step EARLIER on the monotonic clock.
    let mut s = AudioStepHold::default();
    let step = 682_474_000_i64;
    let now = |k: u64, early: u64| 1_000_000_000_000 + k * PACKET - early;
    let raw = |k: u64| WALL + k * PACKET;
    for k in 0..10 {
        audio_step_hold(&mut s, true, OFF, raw(k), PACKET, now(k, 0), false, MIN);
    }
    for k in 10..160 {
        let (off, release) = audio_step_hold(
            &mut s,
            true,
            OFF - step,
            raw(k),
            PACKET,
            now(k, 0),
            false,
            MIN,
        );
        assert_eq!((off, release), (OFF, AudioStepRelease::None), "packet {k}");
    }
    assert!(s.active && s.step_ns == step);
    // the catch-up burst: slot 160 arrives the step earlier than its schedule
    let (off, release) = audio_step_hold(
        &mut s,
        true,
        OFF - step,
        raw(160),
        PACKET,
        now(160, step as u64),
        false,
        MIN,
    );
    assert_eq!((off, release), (OFF - step, AudioStepRelease::Followed));
    let residual = audio_step_residual_ns(s.held_off_ns, off);
    assert_eq!(
        residual, -step,
        "issue 1381: the release applies the whole step"
    );
    assert!(audio_step_release_places(release, residual, PACKET));
    // a stamp that is already back on the live wall never starts a hold
    let mut s = AudioStepHold::default();
    audio_step_hold(&mut s, true, OFF, raw(0), PACKET, now(0, 0), false, MIN);
    let (_, release) = audio_step_hold(
        &mut s,
        true,
        OFF - step,
        raw(1),
        PACKET,
        now(1, step as u64),
        false,
        MIN,
    );
    assert!(!s.active && release == AudioStepRelease::None);
    assert_eq!(audio_stamp_age_ns(10, 3, 4), 3);
    assert_eq!(audio_stamp_age_ns(0, 1, 0), -1, "wraps like C");
}
