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
fn a_sender_that_stepped_first_is_never_held_by_the_receiver_step_1381() {
    // review round 1: the SENDER's box stepped first (a cross-box timecode source whose sender is the
    // date master). Its stamps are already on the new wall when the receiver's own step lands, so the
    // receiver's step brings them BACK onto its wall: holding the pre-step offset would put the
    // audio a whole step off for the full 10 s bound. Nothing is held after the receiver's step.
    for step in [682_474_000_i64, -682_474_000, 89_703_000] {
        // (a) the sender's stamps jump by the step (past the nominal's warm-up), the receiver steps
        // 2 s later. Design 5901213031: the stamp jump (over one packet, continuous arrival) is a
        // PENDING relabel -- the packets keep appending on their continuous timeline (the stamps read
        // shifted by -J) -- and the receiver's own step releases it within one packet, never placed
        // (a raw-clock sender: the remainder is 0)
        let mut s = AudioStepHold::default();
        for k in 0..40 {
            feed(&mut s, k, 0, 0);
        }
        for k in 40..100 {
            assert_eq!(
                feed(&mut s, k, 0, step),
                (OFF - step, AudioStepRelease::None)
            );
            assert!(s.active && s.relabel_pending, "step {step}: pending at {k}");
        }
        assert_eq!(
            feed(&mut s, 100, step, step),
            (OFF - step, AudioStepRelease::RelabelPending),
            "issue 1381: step {step}: the receiver's step resolves the pending relabel"
        );
        assert!(
            !s.active
                && s.relabel_pending
                && s.step_ns == step
                && s.start_ns == 1_000_000_000_000 + 40 * PACKET
        );
        let residual = audio_step_residual_ns(s.held_off_ns, OFF - step);
        assert!(
            residual == 0
                && !audio_step_release_places(AudioStepRelease::RelabelPending, residual, PACKET),
            "issue 1381: step {step}: the receiver's step moves the landing by nothing"
        );
        for k in 101..400 {
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
        for _ in 0..39 {
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
    assert_eq!(
        (s.nominal_age_ns, s.nominal_dev_since_ns, s.nominal_warm),
        (seed, 0, AUDIO_STEP_NOMINAL_WARM_PACKETS)
    );
    // the warm-up: steady packets, the nominal stays on the seed
    let w = u64::from(AUDIO_STEP_NOMINAL_WARM_PACKETS);
    for k in 1..=w {
        audio_step_hold(
            &mut s,
            true,
            OFF,
            WALL + k * PACKET,
            PACKET,
            base + k * PACKET,
            false,
            MIN,
        );
    }
    assert_eq!((s.nominal_age_ns, s.nominal_warm), (seed, 0));
    // an arrival 3.000001 ms late / 5.000003 ms early: +2929 / -4885 ns (1/1024 of the
    // difference, truncated toward zero)
    let k = w + 1;
    audio_step_hold(
        &mut s,
        true,
        OFF,
        WALL + k * PACKET,
        PACKET,
        base + k * PACKET + 3_000_001,
        false,
        MIN,
    );
    assert_eq!(s.nominal_age_ns, seed + 2_929);
    let k = w + 2;
    audio_step_hold(
        &mut s,
        true,
        OFF,
        WALL + k * PACKET,
        PACKET,
        base + k * PACKET - 5_000_003,
        false,
        MIN,
    );
    assert_eq!(s.nominal_age_ns, seed + 2_929 - 4_885);
    // stamps 100 ms ahead of the receiver's wall (the sender moved alone, after a pause -- a jump
    // with continuous arrival would be a pending relabel, design 5901213031): out of band, the timer
    // runs
    let nominal = s.nominal_age_ns;
    let far = 100_000_000_000_u64;
    let k = w + 3;
    let t0 = base + k * PACKET + far;
    for n in 0..6_u64 {
        let raw = WALL + k * PACKET + (n + 1) * far + 100_000_000;
        let r = audio_step_hold(&mut s, true, OFF, raw, PACKET, t0 + n * far, false, MIN);
        assert_eq!(r, (OFF, AudioStepRelease::None));
        assert_eq!(
            (s.nominal_age_ns, s.nominal_dev_since_ns),
            (nominal, t0),
            "packet {n}"
        );
    }
    // exactly ten minutes out of band: the age is the nominal from now on
    let raw = WALL + k * PACKET + 7 * far + 100_000_000;
    audio_step_hold(&mut s, true, OFF, raw, PACKET, t0 + 6 * far, false, MIN);
    assert_eq!(
        (s.nominal_age_ns, s.nominal_dev_since_ns),
        (audio_stamp_age_ns(t0 + 6 * far, raw, OFF), 0),
        "issue 1381: an age out of band for AUDIO_STEP_NOMINAL_REANCHOR_NS re-anchors"
    );
    assert_eq!(AUDIO_STEP_NOMINAL_REANCHOR_NS, 6 * far);
    // inside a hold the nominal is frozen
    let mut s = AudioStepHold::default();
    for k in 0..40 {
        feed(&mut s, k, 0, 0);
    }
    let frozen = s.nominal_age_ns;
    for k in 40..70 {
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

#[test]
fn the_nominal_warms_up_past_a_connect_backlog_and_survives_a_timeline_reset_1381() {
    // review round 2: the first packet arrives two slots late (a backlog queued at connect); the
    // warm-up (every packet, 1/4 of the difference) brings the nominal onto the steady age within the
    // first second instead of keeping the old seed for ten minutes.
    let base = 1_000_000_000_000_u64;
    let late = 2 * PACKET;
    let mut s = AudioStepHold::default();
    audio_step_hold(&mut s, true, OFF, WALL, PACKET, base + late, false, MIN);
    let steady = audio_stamp_age_ns(base, WALL, OFF);
    assert_eq!(s.nominal_age_ns, steady + late as i64);
    for k in 1..=u64::from(AUDIO_STEP_NOMINAL_WARM_PACKETS) {
        let now = base + k * PACKET + if k == 1 { PACKET } else { 0 };
        audio_step_hold(
            &mut s,
            true,
            OFF,
            WALL + k * PACKET,
            PACKET,
            now,
            false,
            MIN,
        );
    }
    assert!(
        s.nominal_age_ns.abs_diff(steady) < 100_000 && s.nominal_warm == 0,
        "issue 1381: the warm-up must converge onto the steady age: nominal {} steady {steady}",
        s.nominal_age_ns
    );
    // a timeline reset keeps the nominal (a sender whose stamps jumped past OBS's 2 s limit stepped
    // first; its age comes back with the receiver's own step) and never starts a hold
    let nominal = s.nominal_age_ns;
    let k = u64::from(AUDIO_STEP_NOMINAL_WARM_PACKETS) + 1;
    let r = audio_step_hold(
        &mut s,
        true,
        OFF,
        WALL + k * PACKET + 2_500_000_000,
        PACKET,
        base + k * PACKET,
        true,
        MIN,
    );
    assert_eq!(r, (OFF, AudioStepRelease::None));
    assert!(
        !s.active && s.nominal_age_ns == nominal && s.nominal_dev_since_ns == base + k * PACKET,
        "issue 1381: a timeline reset must keep the nominal (its skewed age only starts the timer)"
    );
}

// Issue 1381 (design 5900385541) — the RELABEL: a sender whose stamps jump WITH the wall step.

/// One slot of a 30 fps sender (1600 samples at 48 kHz) on the per-second grid.
fn slot_ns(k: i64) -> i64 {
    k * 1_000_000_000 / 30
}

/// What a relabelling sender does at a wall step of `step_ns`: its stamps jump N = floor(S / slot)
/// slots (the contract's floor, toward −∞). Returns the stamp jump against the continuous timeline
/// and the live offset's jump (−S).
fn relabel(step_ns: i64) -> (i64, i64) {
    let n = (i128::from(step_ns) * 30).div_euclid(1_000_000_000) as i64;
    (slot_ns(n), -step_ns)
}

#[test]
fn a_relabel_cancels_the_wall_step_to_under_one_packet_1381() {
    let p = PACKET;
    // the live and scripted steps: +260 ms (r 26.7), +682 ms (r 15.8), -1.5 s (r 0), +2.5 s (r 0),
    // +2.51 s (r 10), a step between -1 slot and 0 (N = -1, r 13.3)
    for step in [
        260_000_000_i64,
        682_474_000,
        89_703_000,
        -682_474_000,
        -1_500_000_000,
        2_500_000_000,
        2_510_000_000,
        -20_000_000,
    ] {
        let (stamp, off) = relabel(step);
        let r = stamp + off;
        assert!(
            (-(p as i64)..=0).contains(&r),
            "the floor puts the landing r earlier, under one slot: step {step} r {r}"
        );
        assert!(
            audio_relabel(stamp, off, p, MIN),
            "issue 1381: a {} ms step relabelled by N slots must read as a relabel (r = {} ms)",
            step as f64 / 1e6,
            r as f64 / 1e6
        );
    }
    // exactly one packet apart is NOT a relabel (strict), one ns less is
    let s = 66_666_667_i64;
    assert!(!audio_relabel(s, -s - p as i64, p, MIN));
    assert!(!audio_relabel(s, -s + p as i64, p, MIN));
    assert!(audio_relabel(s, -s - p as i64 + 1, p, MIN));
    assert!(audio_relabel(s, -s + p as i64 - 1, p, MIN));
}

#[test]
fn only_a_joint_stamp_and_offset_step_is_a_relabel_1381() {
    let p = PACKET;
    // a catch-up sender (stamps continuous) after a forward step, a paused sender after a backward
    // one: the stamps never jumped -- today's path (the hold's catch-up release places once)
    assert!(!audio_relabel(0, -682_474_000, p, MIN));
    assert!(!audio_relabel(0, 682_474_000, p, MIN));
    assert!(!audio_relabel(1_000_000, -20_000_000, p, MIN));
    // a stamp leap, a skipped or duplicated slot, a sender that stepped first: no receiver step
    for stamp in [
        80_000_000_i64,
        33_333_333,
        -33_333_333,
        682_474_000,
        3_000_000_000,
    ] {
        assert!(!audio_relabel(stamp, 0, p, MIN));
        assert!(!audio_relabel(stamp, 1_999_999, p, MIN));
    }
    // steady jitter: neither moved past the 2 ms threshold (a small stamp step within one packet
    // must never read as a relabel on its own)
    assert!(!audio_relabel(900_000, -900_000, p, MIN));
    assert!(!audio_relabel(MIN, -MIN, p, MIN));
    assert!(audio_relabel(MIN + 1, -MIN - 1, p, MIN));
    assert!(!audio_relabel(MIN + 1, -MIN, p, MIN));
    assert!(!audio_relabel(MIN, -MIN - 1, p, MIN));
    // a step the stamps did not cancel (a raw-wall sender that followed by 100 ms of a 682 ms step)
    assert!(!audio_relabel(100_000_000, -682_474_000, p, MIN));
    // two's-complement extremes never panic (the sum wraps exactly like the C mirror's)
    assert!(audio_relabel(i64::MIN, i64::MIN, u64::MAX, MIN));
    assert!(audio_relabel(i64::MIN, i64::MAX, 2, 0));
    assert!(!audio_relabel(i64::MAX, 1, u64::MAX, i64::MIN));
}

#[test]
fn the_jumps_read_the_offset_the_previous_packet_was_mapped_through_1381() {
    let base = 1_000_000_000_000_u64;
    let mut s = AudioStepHold::default();
    assert_eq!(
        audio_step_relabel_jumps(&s, true, OFF, WALL),
        None,
        "no previous timecode packet"
    );
    for k in 0..40 {
        audio_step_hold(
            &mut s,
            true,
            OFF,
            WALL + k * PACKET,
            PACKET,
            base + k * PACKET,
            false,
            MIN,
        );
    }
    assert_eq!(audio_step_relabel_jumps(&s, false, OFF, WALL), None);
    // outside a hold: against the previous live offset
    assert_eq!(
        audio_step_relabel_jumps(&s, true, OFF - 7, WALL + 40 * PACKET + 11),
        Some((11, -7))
    );
    // the receiver steps +260 ms on a packet whose stamp has not moved: the hold starts
    let step = 260_000_000_i64;
    let r = audio_step_hold(
        &mut s,
        true,
        OFF - step,
        WALL + 40 * PACKET,
        PACKET,
        base + 40 * PACKET,
        false,
        MIN,
    );
    assert_eq!(r, (OFF, AudioStepRelease::None));
    assert!(s.active);
    // inside the hold: against the HELD offset, so the next packet's relabel reads the whole step
    let (stamp, off) = relabel(step);
    let raw = (WALL + 41 * PACKET).wrapping_add(stamp as u64);
    assert_eq!(
        audio_step_relabel_jumps(&s, true, OFF - step, raw),
        Some((stamp, off))
    );
}

#[test]
fn a_relabel_releases_a_running_hold_followed_and_is_never_placed_by_it_1381() {
    // the split shape (the contract's section 6): the receiver's step lands on a packet whose stamp
    // has not moved (the hold starts), the relabelled stamps on the next one. The relabel reads the
    // whole step, the hold releases FOLLOWED on that packet with the landing move -r as its residual,
    // and never places it (|r| < one packet). The joint shape (both on the same packet) never starts
    // a hold. Either way the packet maps through the live offset.
    let base = 1_000_000_000_000_u64;
    for step in [
        260_000_000_i64,
        682_474_000,
        -1_500_000_000,
        2_500_000_000,
        -20_000_000,
    ] {
        for split in [true, false] {
            let mut s = AudioStepHold::default();
            let mut k = 0_u64;
            let take = |s: &mut AudioStepHold, k: u64, off: i64, follow: i64| {
                let raw = (WALL + k * PACKET).wrapping_add(follow as u64);
                audio_step_hold(s, true, off, raw, PACKET, base + k * PACKET, false, MIN)
            };
            for _ in 0..40 {
                take(&mut s, k, OFF, 0);
                k += 1;
            }
            let live = OFF - step;
            if split {
                let (_, rel) = take(&mut s, k, live, 0);
                assert_eq!(rel, AudioStepRelease::None);
                assert_eq!(s.active, step.unsigned_abs() > PACKET, "step {step}");
                if !s.active {
                    // a step within one packet starts no hold: its offset jump was appended on its
                    // own packet, and the later stamp jump alone is the existing under-70 ms path
                    continue;
                }
                k += 1;
            }
            let (stamp, _) = relabel(step);
            let raw = (WALL + k * PACKET).wrapping_add(stamp as u64);
            let (sj, oj) =
                audio_step_relabel_jumps(&s, true, live, raw).expect("a previous packet");
            assert!(
                audio_relabel(sj, oj, PACKET, MIN),
                "issue 1381: step {step} split {split}: the relabelled packet must read as a relabel"
            );
            let was_active = s.active;
            let (use_off, rel) = take(&mut s, k, live, stamp);
            assert_eq!(
                use_off, live,
                "step {step} split {split}: mapped through the live offset"
            );
            if was_active {
                let residual = audio_step_residual_ns(s.held_off_ns, live);
                assert_eq!(rel, AudioStepRelease::Followed, "step {step}");
                assert_eq!(
                    residual,
                    sj.wrapping_add(oj),
                    "the residual IS the landing move"
                );
                assert!(!audio_step_release_places(rel, residual, PACKET));
            } else {
                assert_eq!(rel, AudioStepRelease::None, "step {step} split {split}");
                assert!(!s.active, "a joint relabel never starts a hold");
            }
        }
    }
}
