//! Unit tests of `crate::genlock_fifo_relabel` (issue 1372 part B).

use super::*;

const I30: u64 = 33_333_333;
const I60: u64 = 16_666_667;
/// 7.10.2026 02:00:00 UTC — the step instant of the second logged nightly step.
const W0: u64 = 1_791_338_400_000_000_000;
const MONO0: u64 = 123_456_789_000_000;
/// The quantized dantesync 1.16.0 step and the measured 6.10. one.
const S: i64 = 1_600_000_000;
const S_RAW: i64 = 1_543_160_000;
const PIN_NS: u64 = 1_026_000_000;

fn add(ts: u64, step: i64) -> u64 {
    ts.wrapping_add(step as u64)
}

/// A booking that has seen one tick before the step and one after it.
fn booked(step: i64) -> Booking {
    let mut b = Booking::new();
    assert!(!b.observe(MONO0, W0 - I30, MONO0 + 1_000));
    assert!(b.observe(MONO0 + I30, add(W0, step), MONO0 + I30 + 1_000));
    b
}

/// A source that released one tick before the booking (`apply` reads the gap since its previous
/// release: a source that was not releasing at the step never relabels).
fn releasing(b: &Booking) -> RelabelState {
    RelabelState {
        last_release_mono_ns: b.mono_ns.saturating_sub(I30),
        ..RelabelState::default()
    }
}

/// A 30 fps source queue: `n` frames, the oldest at `first`.
fn frames(first: u64, n: usize, step_ns: u64) -> Vec<u64> {
    (0..n as u64).map(|k| first + k * step_ns).collect()
}

#[test]
fn continuity_and_step_matching_1372() {
    // a 30 fps source on a 30 fps canvas: the step, a duplicate and a one-slot gap continue
    for d in [I30, 0, 2 * I30] {
        assert!(continuous(d as i64, I30, I30), "{d}");
    }
    assert!(!continuous((3 * I30) as i64, I30, I30));
    assert!(!continuous(-(I30 as i64) - 1, I30, I30));
    // the booked step carried by one frame, a duplicate or a one-slot gap at the boundary
    for d in [S + I30 as i64, S, S + 2 * I30 as i64] {
        assert!(delta_carries_step(d, S, I30, I30), "{d}");
    }
    // a plain frame, a step that is two frames off the booked one, and the opposite sign do not
    for d in [I30 as i64, S + 3 * I30 as i64, -S + I30 as i64] {
        assert!(!delta_carries_step(d, S, I30, I30), "{d}");
    }
    assert!(delta_carries_step(-S + I30 as i64, -S, I30, I30));
    // a 60 fps camera into the 30 fps canvas: its own step is one 60 fps slot
    assert!(continuous(I60 as i64, I60, I30));
    assert!(delta_carries_step(S + I60 as i64, S, I60, I30));
    assert!(!delta_carries_step(I60 as i64, S, I60, I30));
    // the minimum relabelled step and the jump record threshold
    assert!(step_relabels(S) && step_relabels(-S) && step_relabels(MIN_STEP_NS));
    assert!(!step_relabels(MIN_STEP_NS - 1) && !step_relabels(-51_039_000));
    assert!(jump_recorded(S + I30 as i64, I30) && jump_recorded(-S, I30));
    assert!(!jump_recorded(2 * I30 as i64, I30) && !jump_recorded(0, I30));
}

#[test]
fn no_step_books_nothing_and_relabels_nothing_1372() {
    let mut b = Booking::new();
    let (mut mono, mut wall) = (MONO0, W0);
    for _ in 0..300 {
        // a routine dantesync phase correction (-146 us) and read jitter never book
        assert!(!b.observe(mono, wall, mono + 1_500));
        mono += I30;
        wall += I30 - 146_000 / 300;
    }
    assert_eq!(b.seq, 0);
    let mut st = releasing(&b);
    let mut queue = frames(W0 - PIN_NS, 30, I30);
    let before = queue.clone();
    let (mut boundary, mut rx_last) = (queue[0], queue[29]);
    assert_eq!(
        st.apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            W0,
            b.mono_ns + 1_000
        ),
        None
    );
    assert_eq!(queue, before);
    assert_eq!(
        (boundary, rx_last, st.relabelled),
        (before[0], before[29], 0)
    );
    // an arrival is never relabelled without a booking
    assert_eq!(
        st.receive(rx_last, rx_last + I30, I30, MONO0),
        rx_last + I30
    );
}

#[test]
fn a_step_relabels_every_queued_frame_once_1372() {
    for step in [S, S_RAW] {
        let b = booked(step);
        assert_eq!((b.seq, b.step_ns), (1, step));
        let mut st = releasing(&b);
        // the deep 2ME PGM: ~31 frames queued, the presented one a pin old, all stamped before the
        // step (the receiver stepped first)
        let presented = W0 - PIN_NS;
        let mut queue = frames(presented + I30, 31, I30);
        let before = queue.clone();
        let mut boundary = presented + I30;
        let mut rx_last = before[30];
        let wall_now = add(W0, step) + 5_000_000;
        let p = st
            .apply(
                &b,
                &mut queue,
                &mut boundary,
                &mut rx_last,
                I30,
                I30,
                PIN_NS,
                wall_now,
                b.mono_ns + 1_000,
            )
            .expect("a booked step relabels");
        assert_eq!(
            p,
            Plan {
                queue_old: 31,
                prev_old: true,
                newest_old: true
            }
        );
        for (q, o) in queue.iter().zip(&before) {
            assert_eq!(*q, add(*o, step), "every queued frame moves by exactly S");
        }
        assert_eq!(boundary, add(presented + I30, step));
        assert_eq!(rx_last, add(before[30], step));
        assert_eq!(st.relabelled, 31);
        // the arrival window: open, one latency window after the step instant
        assert!(st.arrival.old_epoch && st.arrival.step_ns == step);
        assert_eq!(st.arrival.until_ns, b.wall_ns + PIN_NS + 5_000_000);
        // once per booking: a second release with the same booking changes nothing
        let again = queue.clone();
        assert_eq!(
            st.apply(
                &b,
                &mut queue,
                &mut boundary,
                &mut rx_last,
                I30,
                I30,
                PIN_NS,
                wall_now,
                b.mono_ns + 1_000
            ),
            None
        );
        assert_eq!(queue, again);
        assert_eq!(st.relabelled, 31);
    }
}

#[test]
fn a_late_old_epoch_arrival_is_relabelled_until_the_sender_steps_1372() {
    let b = booked(S);
    let mut st = releasing(&b);
    let mut queue = frames(W0 - 60 * I60, 4, I60);
    let raw_last = queue[3];
    let mut boundary = queue[0];
    let mut rx_last = raw_last;
    st.apply(
        &b,
        &mut queue,
        &mut boundary,
        &mut rx_last,
        I30,
        I60,
        3_000_000,
        add(W0, S),
        b.mono_ns + 1_000,
    )
    .expect("relabels");
    // the camera has not stepped yet: its next two frames continue the OLD timeline
    let mut prev = rx_last;
    for k in 1..=2u64 {
        let raw = raw_last + k * I60;
        let got = st.receive(prev, raw, I60, MONO0 + k);
        assert_eq!(got, add(raw, S), "old-epoch arrival {k} is relabelled");
        prev = got;
    }
    // the camera steps: its raw stamp now continues the relabelled timeline -- not relabelled,
    // and the window closes
    let stepped = add(raw_last + 3 * I60, S);
    assert_eq!(st.receive(prev, stepped, I60, MONO0 + 3), stepped);
    assert!(!st.arrival.old_epoch);
    // nothing after it is relabelled, even an old-looking stamp
    let old_looking = raw_last + 4 * I60;
    assert_eq!(
        st.receive(stepped, old_looking, I60, MONO0 + 4),
        old_looking
    );
    assert_eq!(st.relabelled, 4 + 2);
}

#[test]
fn a_stamp_jump_that_is_not_the_booked_step_is_never_relabelled_1372() {
    // a sender restart (+700 ms) and a song change (-5 s) inside the window
    for jump in [700_000_000i64, -5_000_000_000] {
        let b = booked(S);
        let mut st = releasing(&b);
        let mut queue = frames(W0 - 4 * I30, 3, I30);
        let raw_last = queue[2];
        let (mut boundary, mut rx_last) = (queue[0], raw_last);
        st.apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            add(W0, S),
            b.mono_ns + 1_000,
        )
        .expect("relabels");
        let raw = add(raw_last, jump);
        assert_eq!(st.receive(rx_last, raw, I30, MONO0 + 9), raw, "jump {jump}");
        assert!(!st.arrival.old_epoch, "jump {jump}: the window closes");
        assert_eq!(st.jump_ns, add(raw, 0).wrapping_sub(rx_last) as i64);
        // and the frame after it is not relabelled either
        assert_eq!(st.receive(raw, raw + I30, I30, MONO0 + 10), raw + I30);
        assert_eq!(st.relabelled, 3);
    }
}

#[test]
fn a_second_step_reapplies_with_its_own_size_1372() {
    let mut b = booked(S);
    let mut st = releasing(&b);
    let mut queue = frames(W0 - 10 * I30, 6, I30);
    let first = queue.clone();
    let (mut boundary, mut rx_last) = (queue[0], queue[5]);
    st.apply(
        &b,
        &mut queue,
        &mut boundary,
        &mut rx_last,
        I30,
        I30,
        PIN_NS,
        add(W0, S),
        b.mono_ns + 1_000,
    )
    .expect("first step");
    // a second step of -200 ms, two ticks later
    let s2: i64 = -200_000_000;
    assert!(b.observe(
        MONO0 + 3 * I30,
        add(add(W0, S) + 2 * I30, s2),
        MONO0 + 3 * I30 + 1_000
    ));
    assert_eq!((b.seq, b.step_ns), (2, s2));
    let p = st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            add(add(W0, S), s2),
            b.mono_ns + 1_000,
        )
        .expect("second step");
    assert_eq!(p.queue_old, 6);
    for (q, o) in queue.iter().zip(&first) {
        assert_eq!(*q, add(add(*o, S), s2));
    }
    assert_eq!(st.arrival.step_ns, s2);
    assert_eq!(st.relabelled, 12);
}

#[test]
fn a_negative_step_1372() {
    let neg = -S;
    // the receiver stepped first: every queued frame moves back by |S|
    let b = booked(neg);
    let mut st = releasing(&b);
    let mut queue = frames(W0 - 31 * I30, 31, I30);
    let before = queue.clone();
    let (mut boundary, mut rx_last) = (queue[0], queue[30]);
    let p = st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            add(W0, neg),
            b.mono_ns + 1_000,
        )
        .expect("relabels");
    assert_eq!((p.queue_old, p.prev_old, p.newest_old), (31, true, true));
    assert_eq!(queue[0], add(before[0], neg));
    assert_eq!(rx_last, add(before[30], neg));

    // the sender stepped first, a deep queue: the stamps of the two epochs OVERLAP in value
    // (|S| = 1.6 s < a 2 s queue), so only the FIFO order tells them apart -- the old prefix
    // moves, the new suffix stays
    let b = booked(neg);
    let mut st = releasing(&b);
    let mut queue = frames(W0 - 60 * I30, 54, I30);
    let old_last = *queue.last().expect("non-empty");
    for k in 1..=6u64 {
        queue.push(add(old_last + k * I30, neg));
    }
    let before = queue.clone();
    let (mut boundary, mut rx_last) = (queue[0], *queue.last().expect("non-empty"));
    let newest = rx_last;
    let p = st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            add(W0, neg),
            b.mono_ns + 1_000,
        )
        .expect("relabels");
    assert_eq!((p.queue_old, p.prev_old, p.newest_old), (54, true, false));
    assert_eq!(queue[53], add(before[53], neg));
    assert_eq!(queue[54..], before[54..]);
    assert_eq!(rx_last, newest, "the newest frame is new-epoch");
    assert!(!st.arrival.old_epoch);
    // the relabelled timeline is continuous across the boundary
    assert_eq!(queue[54] - queue[53], I30);

    // the sender stepped first and its new-epoch frames were already presented: the jump was
    // remembered on arrival, the queue holds only new frames -- nothing moves
    let b = booked(neg);
    let mut st = releasing(&b);
    let old = W0 - 2 * I30;
    let first_new = add(old + I30, neg);
    assert_eq!(
        st.receive(old, first_new, I30, b.mono_ns - 30_000_000),
        first_new
    );
    assert_eq!(st.jump_ns, neg + I30 as i64);
    let mut queue = frames(first_new + I30, 2, I30);
    let before = queue.clone();
    let (mut boundary, mut rx_last) = (first_new + I30, queue[1]);
    let p = st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            3_000_000,
            add(W0, neg),
            b.mono_ns + 1_000,
        )
        .expect("plans");
    assert_eq!(p, Plan::default());
    assert_eq!(queue, before);
    assert_eq!((boundary, rx_last), (first_new + I30, before[1]));
    assert!(!st.arrival.old_epoch);
}

#[test]
fn a_step_under_the_minimum_is_left_to_the_regrid_1372() {
    for step in [-51_039_000i64, 51_039_000, MIN_STEP_NS - 1] {
        let b = booked(step);
        let mut st = releasing(&b);
        let mut queue = frames(W0 - 3 * I30, 3, I30);
        let before = queue.clone();
        let (mut boundary, mut rx_last) = (queue[0], queue[2]);
        assert_eq!(
            st.apply(
                &b,
                &mut queue,
                &mut boundary,
                &mut rx_last,
                I30,
                I30,
                PIN_NS,
                add(W0, step),
                b.mono_ns + 1_000
            ),
            None,
            "{step}"
        );
        assert_eq!(queue, before);
        assert_eq!(st.seq, b.seq, "the booking is consumed");
        assert!(!st.arrival.old_epoch);
    }
}

#[test]
fn the_window_ends_one_latency_window_after_the_step_1372() {
    // a sender that never steps: its old-epoch frames are relabelled for one latency window of its
    // own stamp timeline (here the presented age, 70 ms over a 3 ms pin), never after it
    let b = booked(S);
    let mut st = releasing(&b);
    let presented = W0 - 70_000_000;
    let mut queue = frames(presented + I30, 2, I30);
    let (mut boundary, mut rx_last) = (presented + I30, queue[1]);
    let raw_last = queue[1];
    st.apply(
        &b,
        &mut queue,
        &mut boundary,
        &mut rx_last,
        I30,
        I30,
        3_000_000,
        add(W0, S),
        b.mono_ns + 1_000,
    )
    .expect("relabels");
    assert_eq!(st.arrival.until_ns, b.wall_ns + 70_000_000);
    let mut prev = rx_last;
    let mut relabelled = 0;
    for k in 1..=10u64 {
        let raw = raw_last + k * I30;
        let got = st.receive(prev, raw, I30, MONO0 + k);
        if got != raw {
            relabelled += 1;
            assert!(add(raw, S) <= st.arrival.until_ns);
        }
        prev = got;
    }
    // stamps up to W0 + 70 ms + a hair: W0 - 3 ms + k * 33.3 ms for k = 1..=2
    assert_eq!(relabelled, 2);
    assert!(!st.arrival.old_epoch);
}

#[test]
fn the_sender_first_positive_step_relabels_only_the_old_prefix_1372() {
    let b = booked(S);
    let mut st = releasing(&b);
    // three old frames, then two the sender stamped after ITS step (30 ms before the receiver)
    let mut queue = frames(W0 - 5 * I30, 3, I30);
    let last_old = queue[2];
    queue.push(add(last_old + I30, S));
    queue.push(add(last_old + 2 * I30, S));
    let before = queue.clone();
    let (mut boundary, mut rx_last) = (queue[0], queue[4]);
    let p = st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            PIN_NS,
            add(W0, S),
            b.mono_ns + 1_000,
        )
        .expect("relabels");
    assert_eq!((p.queue_old, p.prev_old, p.newest_old), (3, true, false));
    assert_eq!(
        queue[..3],
        [add(before[0], S), add(before[1], S), add(before[2], S)]
    );
    assert_eq!(queue[3..], before[3..]);
    assert!(queue.windows(2).all(|w| w[1] - w[0] == I30));
    assert_eq!(rx_last, before[4]);
    assert!(!st.arrival.old_epoch);
}

#[test]
fn a_jump_remembered_too_long_before_the_step_is_not_the_sender_step_1372() {
    let b = booked(-S);
    // the same jump, remembered within / beyond one window before the booking
    assert!(sender_stepped_before(
        -S + I30 as i64,
        b.mono_ns - PIN_NS,
        -S,
        I30,
        I30,
        b.mono_ns,
        PIN_NS
    ));
    assert!(!sender_stepped_before(
        -S + I30 as i64,
        b.mono_ns - PIN_NS - 1,
        -S,
        I30,
        I30,
        b.mono_ns,
        PIN_NS
    ));
    // a remembered jump that is not the booked step
    assert!(!sender_stepped_before(
        700_000_000,
        b.mono_ns,
        -S,
        I30,
        I30,
        b.mono_ns,
        PIN_NS
    ));
    assert!(!sender_stepped_before(
        0, b.mono_ns, -S, I30, I30, b.mono_ns, PIN_NS
    ));
}

const NS: u64 = 1_000_000_000;

#[test]
fn a_source_that_was_not_releasing_at_the_step_takes_the_booking_without_relabelling_1372() {
    let b = booked(S);
    let mono_now = b.mono_ns + 1_000;
    for (name, last_release) in [
        ("a new source", 0),
        ("a source silent for 3 h", b.mono_ns - 3 * 3600 * NS),
        (
            "a source silent just over the bound",
            mono_now - APPLY_MAX_GAP_NS - 1,
        ),
    ] {
        let mut st = RelabelState {
            last_release_mono_ns: last_release,
            ..RelabelState::default()
        };
        // frames that arrived after the step, and a boundary left from before the silence
        let mut queue = frames(add(W0, S) - 3 * I30, 3, I30);
        let before = queue.clone();
        let stale_boundary = W0 - 3 * 3600 * NS;
        let (mut boundary, mut rx_last) = (stale_boundary, queue[2]);
        let p = st.apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            3_000_000,
            add(W0, S),
            mono_now,
        );
        assert_eq!(p, None, "{name}: no relabel");
        assert_eq!(queue, before, "{name}");
        assert_eq!((boundary, rx_last), (stale_boundary, before[2]), "{name}");
        assert_eq!(st.seq, b.seq, "{name}: the booking is taken");
        assert!(!st.arrival.old_epoch, "{name}: no window");
        assert_eq!(st.relabelled, 0, "{name}");
        assert_eq!(st.last_release_mono_ns, mono_now, "{name}");
        // and no later arrival is relabelled
        assert_eq!(
            st.receive(rx_last, rx_last + I30, I30, mono_now + 1),
            rx_last + I30,
            "{name}"
        );
    }
    // exactly at the bound the source still counts as releasing
    let mut st = RelabelState {
        last_release_mono_ns: mono_now - APPLY_MAX_GAP_NS,
        ..RelabelState::default()
    };
    let mut queue = frames(W0 - 4 * I30, 3, I30);
    let (mut boundary, mut rx_last) = (queue[0], queue[2]);
    assert!(st
        .apply(
            &b,
            &mut queue,
            &mut boundary,
            &mut rx_last,
            I30,
            I30,
            3_000_000,
            add(W0, S),
            mono_now,
        )
        .is_some());
}

#[test]
fn a_stale_detector_reference_reseeds_instead_of_booking_1372() {
    let mut bk = Booking::new();
    assert!(!bk.observe(MONO0, W0, MONO0 + 1_000));
    // no genlock release for 2 s while the wall stepped 1.6 s: the read re-seeds, books nothing
    let m = MONO0 + 2 * NS;
    assert!(!bk.observe(m, add(W0 + 2 * NS, S), m + 1_000));
    assert_eq!(bk.seq, 0);
    // a raw-clock drift summed over hours of silence (150 ms) is not booked either
    let m = MONO0 + 3 * 3600 * NS;
    let w = add(W0 + 3 * 3600 * NS, S) + 150_000_000;
    assert!(!bk.observe(m, w, m + 1_000));
    assert_eq!(bk.seq, 0);
    // reads resumed: the next real step is booked as usual
    assert!(bk.observe(m + I30, w + I30 + 200_000_000, m + I30 + 1_000));
    assert_eq!((bk.seq, bk.step_ns), (1, 200_000_000));
    // an untrusted (preempted) read in between never refreshes the reference
    let mut bk = Booking::new();
    assert!(!bk.observe(MONO0, W0, MONO0 + 1_000));
    let m = MONO0 + 900_000_000;
    assert!(!bk.observe(m, W0 + 900_000_000, m + 500_000));
    let m = MONO0 + 1_200_000_000;
    assert!(!bk.observe(m, add(W0 + 1_200_000_000, S), m + 1_000));
    assert_eq!(bk.seq, 0, "1.2 s since the last TRUSTED read: re-seeded");
    // exactly at the bound the reference holds: a step right at 1 s is booked
    let mut bk = Booking::new();
    assert!(!bk.observe(MONO0, W0, MONO0 + 1_000));
    assert!(bk.observe(MONO0 + NS, add(W0 + NS, S), MONO0 + NS + 1_000));
    assert_eq!(bk.seq, 1);
}

#[test]
fn the_window_never_takes_more_than_the_age_cap_1372() {
    assert_eq!(window_ns(3_000_000, 3 * 3600 * NS), WINDOW_MAX_AGE_NS);
    assert_eq!(
        window_ns(3_000_000, WINDOW_MAX_AGE_NS + 1),
        WINDOW_MAX_AGE_NS
    );
    assert_eq!(window_ns(1_026_000_000, 70_000_000), 1_026_000_000);
    assert_eq!(window_ns(3_000_000, 70_000_000), 70_000_000);
    // the pin itself is never cut
    assert_eq!(window_ns(2_500_000_000, 3 * 3600 * NS), 2_500_000_000);
}
