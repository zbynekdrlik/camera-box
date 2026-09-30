//! Issue 1381 (design 5902870861, ROZHODNUTÉ 5902983227, receiver slice 3) — the unit tests of a
//! sender-first step of ONE slot (N = +1) as a pending relabel: the forward bound one packet − 100 ns
//! on the sender's 100 ns grid, the half-packet age band, the away guard against a late follow, and
//! the fold's slot inside a pending. A `#[path]` child of `pending_tests` (split to keep each file
//! under the ~1000-line budget), so it shares its `Feed` and constants.

use super::*;

/// The one-slot sender-first steps: S from just over one slot to just under two (N = +1).
const ONE_SLOT_STEPS: [i64; 5] = [35_000_000, 40_000_000, 50_000_000, 60_000_000, 66_000_000];

/// The two stamp jumps an N = +1 relabel makes on the sender's 100 ns per-second grid (its 30 fps
/// slots are 33 333 300 / 33 333 300 / 33 333 400 ns): one packet + 34 ns on two grid positions,
/// one packet − 66 ns on the third.
const ONE_SLOT_JUMPS: [i64; 2] = [PACKET as i64 + 34, PACKET as i64 - 66];

#[test]
fn a_one_slot_forward_jump_with_continuous_arrival_is_a_pending_relabel_1381() {
    let p = PACKET;
    let pi = p as i64;
    let j = AUDIO_RELABEL_ARRIVAL_JITTER_NS;
    assert_eq!(
        AUDIO_RELABEL_FORWARD_TOLERANCE_NS, 100,
        "one NDI timecode unit"
    );
    // forward: one packet − 100 ns and up, continuous arrival (the emit re-phased r earlier)
    for jump in [pi - 66, pi + 34, pi - 100, pi, pi + 1] {
        assert!(
            audio_relabel_pending(jump, p - 6_666_733, p, MIN),
            "issue 1381: a forward stamp jump of {jump} ns with continuous arrival is a pending \
             relabel"
        );
    }
    assert!(
        !audio_relabel_pending(pi - 101, p, p, MIN),
        "issue 1381: one ns more than one NDI unit under one packet is not a slot"
    );
    // the arrival still decides: a skipped slot jumps the same one packet, but its arrival gaps
    assert!(!audio_relabel_pending(pi - 66, 2 * p, p, MIN));
    assert!(!audio_relabel_pending(pi + 34, 2 * p, p, MIN));
    assert!(audio_relabel_pending(pi - 66, p + j, p, MIN));
    assert!(!audio_relabel_pending(pi - 66, p + j + 1, p, MIN));
    // backward still needs MORE than one packet: a duplicated slot and an N = −1 relabel are
    // exactly one packet back, and nothing less than that
    for jump in [-pi, -(pi - 66), -(pi - 100), -(pi - 1)] {
        assert!(
            !audio_relabel_pending(jump, p, p, MIN),
            "issue 1381: a backward stamp jump of {jump} ns keeps today's path"
        );
    }
    assert!(audio_relabel_pending(-pi - 1, p, p, MIN));
    // packets shorter than the tolerance: the 2 ms step minimum still rules
    assert!(!audio_relabel_pending(MIN, 50, 50, MIN));
    assert!(audio_relabel_pending(MIN + 1, 50, 50, MIN));
}

#[test]
fn a_one_slot_sender_first_step_resolves_at_the_receiver_step_1381() {
    // slice 2 missed the one-packet − 66 ns jump (not over one packet): its stamps were appended and
    // this box's later step PLACED the packet (a zero-length `followed` release, residual −S). The
    // first relabelled block goes out re-phased (its age −S) or, as the sender contract's §5 has it,
    // still on the old schedule with only the next emit re-phased (its age −J; review round 2)
    for step in ONE_SLOT_STEPS {
        for jump in ONE_SLOT_JUMPS {
            for (lag_packets, rephase_first) in
                [(15_u64, true), (90, true), (15, false), (90, false)]
            {
                let mut f = Feed::new();
                f.steady(WARM);
                let r = step - jump;
                f.shift = jump;
                f.early = if rephase_first { r as u64 } else { 0 };
                assert!(
                    f.starts(),
                    "issue 1381: step {step} jump {jump} (first block re-phased {rephase_first}): \
                     a one-slot sender-first step starts a pending relabel"
                );
                assert_eq!(
                    f.take(),
                    (OFF - jump, AudioStepRelease::None),
                    "step {step} jump {jump}"
                );
                assert!(f.s.active && f.s.relabel_pending && f.s.step_ns == jump);
                f.early = r as u64;
                for _ in 0..lag_packets {
                    assert_eq!(
                        f.take(),
                        (OFF - jump, AudioStepRelease::None),
                        "step {step} jump {jump}: held on the continuous timeline"
                    );
                }
                f.off = OFF - step;
                let (off, rel) = f.take();
                assert_eq!(
                    (off, rel),
                    (OFF - step, AudioStepRelease::RelabelPending),
                    "issue 1381: step {step} jump {jump} lag {lag_packets}: this box's own step of \
                     −(one slot + r) resolves the pending"
                );
                let residual = audio_step_residual_ns(f.s.held_off_ns, off);
                assert!(
                    residual == -r && !audio_step_release_places(rel, residual, PACKET),
                    "issue 1381: step {step} jump {jump}: released with −r, never placed"
                );
                f.steady(100);
            }
        }
    }
}

#[test]
fn the_pending_age_band_is_half_a_packet_1381() {
    // the stamps must jump AWAY from this box's wall by more than HALF a packet: a one-slot step's
    // stamp age sits about one slot off (−S), so a late packet's arrival jitter no longer decides it
    let p = PACKET as i64;
    // the age is `arrival shift − stamp jump`: this arrival shift puts a −(one packet + 1 ms) jump's
    // age exactly on half a packet (p / 2 truncates, so it is spelled out, never approximated)
    let edge = p / 2 - p - 1_000_000;
    // (stamp jump, arrival later than the grid (< 0 = earlier), starts)
    let cases: [(i64, i64, bool); 5] = [
        // over one packet, 10 ms late: age −(J − 10 ms) = −24.3 ms -- inside the old one-packet band
        (p + 1_000_000, 10_000_000, true),
        // one packet − 66 ns, 5 ms late: age −28.3 ms
        (p - 66, 5_000_000, true),
        // backward, 17.7 ms early: age exactly half a packet (strict) -- and one ns more
        (-p - 1_000_000, edge, false),
        (-p - 1_000_000, edge + 1, true),
        // an age back inside the band (5 ms under half a packet): never a pending start
        (-p - 1_000_000, edge - 5_000_000, false),
    ];
    for (jump, late, starts) in cases {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        let now = (f.now() as i64 + late) as u64;
        assert_eq!(
            audio_step_relabel_pending_starts(&f.s, true, f.off, f.raw(), PACKET, now, MIN),
            starts,
            "issue 1381: jump {jump} arriving {late} ns late: age {} ns",
            late - jump
        );
    }
}

/// The slice-2 pending-start decision (design 5901213031) verbatim: the reference the slice-3 start
/// must contain.
fn slice2_relabel_pending(jump_ns: i64, gap_ns: u64, packet_ns: u64, min_ns: i64) -> bool {
    let jump = jump_ns.unsigned_abs();
    jump > min_ns.unsigned_abs()
        && jump > packet_ns
        && gap_ns <= packet_ns.saturating_add(AUDIO_RELABEL_ARRIVAL_JITTER_NS)
}

#[test]
fn the_slice_3_start_adds_only_the_one_slot_widenings_and_drops_only_late_follows_1381() {
    // every packet slice 2 started a pending on still starts one, except a LATE FOLLOW, and the only
    // new starts are a forward jump of one packet − 100 ns ..= one packet, or an age between half a
    // packet and one packet. So a skipped slot (an arrival gap), a duplicated slot and an N = −1
    // relabel (one packet back) -- none of them a slice-2 start, none in either new region -- keep the
    // slice-2 path byte for byte (the bench pins the traces too). Swept from a steady feed AND from a
    // feed whose age is already far off (this box stepped first by 60 ms and its skew hold timed out).
    // Review round 2: which jump is a late follow comes from the SCENARIO, never from the decision's
    // own age test -- on the timed-out base the unmatched step is known (60 ms), and by ROZHODNUTÉ
    // 5903945145 its follow is a stamp jump of the same direction within one packet + the arrival
    // budget of it. A follow never starts; nothing else is dropped or added.
    let p = PACKET as i64;
    let unmatched_step = 60_000_000_i64;
    let mut jumps = vec![0_i64, 3_000_000, 10_000_000, 40_000_000, 500_000_000, 2 * p];
    for d in [0_i64, 1, 34, 66, 99, 100, 101, 1_000_000] {
        jumps.extend([p - d, p + d]);
    }
    let jumps: Vec<i64> = jumps.iter().flat_map(|&j| [j, -j]).collect();
    let (mut added_forward, mut added_age, mut dropped_late) = (0, 0, 0);
    let base = |timed_out: bool| {
        let mut f = Feed::new();
        f.steady(WARM);
        if timed_out {
            f.off = OFF - 60_000_000;
            for _ in 0..400 {
                f.take();
            }
            assert!(!f.s.active, "the hold timed out");
        }
        f
    };
    for (timed_out, jump) in [false, true]
        .into_iter()
        .flat_map(|t| jumps.iter().map(move |&j| (t, j)))
    {
        for late in [
            -20_000_000_i64,
            -(p / 2 + 1_000_001),
            -5_000_000,
            0,
            5_000_000,
            10_000_000,
            15_000_000,
            15_000_001,
            p,
        ] {
            for off_move in [0_i64, -1_000_000, -40_000_000] {
                let late_follow = timed_out
                    && jump > 0
                    && (jump - unmatched_step).unsigned_abs()
                        <= PACKET + AUDIO_RELABEL_ARRIVAL_JITTER_NS;
                let mut f = base(timed_out);
                f.shift = jump;
                f.off += off_move;
                let now = (f.now() as i64 + late) as u64;
                let raw = f.raw();
                let new =
                    audio_step_relabel_pending_starts(&f.s, true, f.off, raw, PACKET, now, MIN);
                let age_dev = audio_stamp_age_ns(now, raw, f.off).wrapping_sub(f.s.nominal_age_ns);
                let stamp_jump =
                    raw.wrapping_sub(f.s.prev_raw_ns.wrapping_add(f.s.prev_packet_ns)) as i64;
                let old = f.off.wrapping_sub(f.s.prev_off_ns).unsigned_abs() <= MIN.unsigned_abs()
                    && age_dev.unsigned_abs() > PACKET
                    && slice2_relabel_pending(
                        stamp_jump,
                        now.wrapping_sub(f.s.prev_arrival_ns),
                        PACKET,
                        MIN,
                    );
                assert!(
                    !(late_follow && new),
                    "issue 1381: jump {jump} late {late} off {off_move}: the 60 ms step's late \
                     follow started a pending relabel"
                );
                assert!(
                    !old || new || late_follow,
                    "issue 1381: jump {jump} late {late} off {off_move}: a slice-2 start must \
                     still start unless it is a late follow"
                );
                dropped_late += usize::from(old && !new);
                if new && !old {
                    let forward_slot = stamp_jump > 0 && stamp_jump >= p - 100 && stamp_jump <= p;
                    let age_band =
                        age_dev.unsigned_abs() > PACKET / 2 && age_dev.unsigned_abs() <= PACKET;
                    assert!(
                        forward_slot || age_band,
                        "issue 1381: jump {jump} late {late} off {off_move}: a new start outside \
                         the one-slot forward jump and the half-packet age band"
                    );
                    added_forward += usize::from(forward_slot);
                    added_age += usize::from(age_band);
                }
            }
        }
    }
    assert!(
        added_forward > 0 && added_age > 0 && dropped_late > 0,
        "the sweep must reach both widenings and the late-follow narrowing: {added_forward} \
         forward, {added_age} age, {dropped_late} late follows"
    );
}

#[test]
fn a_late_relabel_after_a_timed_out_hold_never_starts_a_pending_1381() {
    // review round 1 (slice 3): this box stepped first by S and the sender did not follow within the
    // 10 s bound, so the skew hold timed out and the stamps' age sits S off the nominal. The sender's
    // LATE relabel by N slots then moves the age back TOWARD the nominal: r off on its first
    // relabelled block (not re-phased yet), about 0 after. With r between half a packet and one
    // packet the half-packet band alone read that first block as a sender-first step -- a false
    // pending, another 10 s hold, then J applied once (a 33.3 ms zero-filled gap in the bench). And
    // with the block up to the arrival budget late, r plus that lateness passes one packet: the slice-2
    // rule (every age over one packet is away) started the same false pending (review round 2, the
    // bench's 66 ms step on the − 66 ns grid position).
    let half = PACKET as i64 / 2;
    let mut over_one_packet = 0;
    for step in [
        50_000_000_i64,
        55_000_000,
        60_000_000,
        66_000_000,
        682_474_000,
    ] {
        let jump = relabel_jump(step);
        let r = step - jump;
        let mut f = Feed::new();
        f.steady(WARM);
        f.off = OFF - step;
        let mut released = false;
        for _ in 0..400 {
            released |= f.take().1 == AudioStepRelease::Timeout;
        }
        assert!(released && !f.s.active, "step {step}: the hold timed out");
        f.shift = jump;
        assert!(
            !f.starts(),
            "issue 1381: step {step} (r {r} ns): a late relabel after a timed-out hold is a \
             follow, never a pending start"
        );
        for late in [
            1_000_000_u64,
            5_000_000,
            10_000_000,
            AUDIO_RELABEL_ARRIVAL_JITTER_NS,
        ] {
            let now = f.now() + late;
            let age_dev = audio_stamp_age_ns(now, f.raw(), f.off).wrapping_sub(f.s.nominal_age_ns);
            over_one_packet += usize::from(age_dev.unsigned_abs() > PACKET);
            assert!(
                !audio_step_relabel_pending_starts(&f.s, true, f.off, f.raw(), PACKET, now, MIN),
                "issue 1381: step {step} (r {r} ns), its first relabelled block {late} ns late (age \
                 {age_dev} ns off): a late follow, never a pending start"
            );
        }
        f.take();
        assert!(
            !(f.s.active && f.s.relabel_pending),
            "issue 1381: step {step}: no pending runs"
        );
        f.early = r as u64;
        for _ in 0..5 {
            f.take();
            assert!(!f.s.active, "step {step}: nothing holds after the follow");
        }
        if step < 100_000_000 {
            assert!(
                r > half,
                "the one-slot rows must sit in the half-packet band"
            );
        }
    }
    assert!(
        over_one_packet > 0,
        "the lateness sweep must reach an age over one packet (the slice-2 false start)"
    );
}

#[test]
fn a_jump_toward_the_nominal_still_starts_a_pending_over_one_packet_1381() {
    // the away-from-the-nominal guard (review round 1) applies only inside the new half-packet band:
    // over one packet the slice-2 start is kept as it was. The sender's box stepped first by +682 ms
    // and this box never followed (the pending timed out, the stamps' age sits −682 ms off); the
    // sender's box then steps back by 300 ms first. Its stamps jump back TOWARD this box's wall, to
    // −382 ms: slice 2 started a pending there, and so does slice 3.
    let mut f = Feed::new();
    f.steady(WARM);
    let jump = relabel_jump(682_474_000);
    f.shift = jump;
    f.early = (682_474_000 - jump) as u64;
    assert!(f.starts());
    let mut timed_out = false;
    for _ in 0..400 {
        timed_out |= f.take().1 == AudioStepRelease::Timeout;
    }
    assert!(timed_out && !f.s.active);
    f.shift += relabel_jump(-300_000_000);
    assert!(
        f.starts(),
        "issue 1381: a stamp jump of 300 ms back toward this box's wall, its age still 382 ms off, \
         keeps the slice-2 pending start"
    );
}

#[test]
fn the_away_guard_is_strict_further_off_than_the_previous_packet_1381() {
    // review round 2 (slice 3): the half-packet band's away guard at its exact edge. A packet 20 ms
    // late (its age +20 ms off, no stamp jump), then a 40 ms forward stamp jump whose age lands EXACTLY
    // as far off on the other side: not further off, never a start; 1 ns further, a start. Everything
    // else holds at the edge (a relabel-shaped jump, continuous arrival, an offset move under 2 ms).
    // Review round 2: with this box's offset moved by ±1 ms on the edge packet, the previous packet's
    // age is still read through the offset IT was mapped through, never the live one.
    for off_move in [0_i64, -1_000_000, 1_000_000] {
        let mut f = Feed::new();
        f.early = 30_000_000;
        f.steady(WARM);
        f.early = 10_000_000;
        f.take();
        let prev_dev = audio_stamp_age_ns(f.s.prev_arrival_ns, f.s.prev_raw_ns, f.s.prev_off_ns)
            .wrapping_sub(f.s.nominal_age_ns);
        assert!(prev_dev > PACKET as i64 / 2 && prev_dev < PACKET as i64);
        f.shift = 40_000_000;
        let raw = f.raw();
        let off = f.off + off_move;
        let edge = raw
            .wrapping_add(off as u64)
            .wrapping_add(f.s.nominal_age_ns as u64)
            .wrapping_sub(prev_dev as u64);
        assert!(audio_relabel_pending(
            40_000_000,
            edge.wrapping_sub(f.s.prev_arrival_ns),
            PACKET,
            MIN
        ));
        assert!(
            !audio_step_relabel_pending_starts(&f.s, true, off, raw, PACKET, edge, MIN),
            "issue 1381: offset move {off_move}: an age exactly as far off as the previous \
             packet's is not away"
        );
        assert!(
            audio_step_relabel_pending_starts(&f.s, true, off, raw, PACKET, edge - 1, MIN),
            "issue 1381: offset move {off_move}: 1 ns further off than the previous packet's age \
             is away"
        );
    }
}

#[test]
fn the_away_guard_far_bound_is_one_packet_plus_the_arrival_budget_1381() {
    // review round 2: a jump back TOWARD the nominal from an age already far off is away only when it
    // lands more than one packet + the 15 ms arrival budget off. The sender's box stepped +100 ms
    // (three slots) and this box never followed: the pending timed out, the age sits 100 ms off. The
    // sender's box then steps back: landing exactly on the bound is no start, 1 ns past it a start,
    // and the review's two mutants' bands (budget / 2, 2 · budget) sit on either side.
    let far = (PACKET + AUDIO_RELABEL_ARRIVAL_JITTER_NS) as i64;
    for (lands_ms_off, starts) in [
        (far, false),
        (far + 1, true),
        (45_000_000, false),
        (55_000_000, true),
    ] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = 100_000_000;
        assert!(f.starts(), "the +100 ms sender-first step starts a pending");
        let mut timed_out = false;
        for _ in 0..400 {
            timed_out |= f.take().1 == AudioStepRelease::Timeout;
        }
        assert!(timed_out && !f.s.active);
        f.shift -= 100_000_000 - lands_ms_off;
        assert_eq!(
            f.starts(),
            starts,
            "issue 1381: a toward jump landing {lands_ms_off} ns off (bound {far})"
        );
    }
}

#[test]
fn a_never_followed_pending_applies_its_jump_once_on_every_grid_position_1381() {
    // ROZHODNUTÉ 5903945145 point 2: a pending this box never follows applies its jump J once at the
    // timeout -- PLACED, whatever the grid position. Slice 3 placed only J over one packet (+ 34 ns)
    // and left J = one packet − 66 ns to the timecode ASRC's 1000 ppm booking (33 s).
    for jump in ONE_SLOT_JUMPS {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        f.early = (40_000_000 - jump) as u64;
        assert!(f.starts());
        let mut released = None;
        for _ in 0..400 {
            let (off, rel) = f.take();
            if rel != AudioStepRelease::None {
                released = Some((rel, audio_step_residual_ns(f.s.held_off_ns, off)));
                break;
            }
        }
        let (rel, residual) = released.expect("the pending ends");
        assert_eq!((rel, residual), (AudioStepRelease::Timeout, jump));
        assert!(
            audio_step_release_places(rel, residual, PACKET),
            "issue 1381: a never-followed pending of J = {jump} ns applies J once at the timeout"
        );
    }
}

#[test]
fn a_timeout_places_one_slot_or_more_and_books_a_sub_slot_move_1381() {
    // review round 3: point 2 of ROZHODNUTÉ 5903945145 is about a pending's jump J, one slot on every
    // grid position. A relabel-shaped jump INSIDE a pending folds, so its timeout residual can be
    // any size; under one slot it stays booked by the timecode ASRC like any sub-slot move.
    let p = PACKET as i64;
    let slot = p - AUDIO_RELABEL_FORWARD_TOLERANCE_NS as i64;
    for (residual, places) in [
        (slot, true),
        (-slot, true),
        (p - 66, true),
        (682_474_000, true),
        (slot - 1, false),
        (-(slot - 1), false),
        (-966, false),
        (1, false),
        (0, false),
    ] {
        assert_eq!(
            audio_step_release_places(AudioStepRelease::Timeout, residual, PACKET),
            places,
            "issue 1381: a timeout residual of {residual} ns"
        );
    }
    // the review's shape: a pending of J = one packet + 34 ns, then the sender steps back by one
    // packet + 1 µs (relabel-shaped, it folds), and this box never steps
    let jump = p + 34;
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = jump;
    f.early = (40_000_000 - jump) as u64;
    assert!(f.starts());
    f.take();
    f.take();
    f.shift -= p + 1_000;
    let mut released = None;
    for _ in 0..400 {
        let (off, rel) = f.take();
        if rel != AudioStepRelease::None {
            released = Some((rel, audio_step_residual_ns(f.s.held_off_ns, off)));
            break;
        }
    }
    let (rel, residual) = released.expect("the pending ends");
    assert_eq!((rel, residual), (AudioStepRelease::Timeout, jump - p - 1_000));
    assert!(
        !audio_step_release_places(rel, residual, PACKET),
        "issue 1381: a folded pending's sub-slot timeout residual is booked, not placed"
    );
}

#[test]
fn a_backward_move_inside_a_pending_folds_under_one_packet_and_a_duplicated_slot_keeps_it_1381() {
    // review round 2 (slice 3): the fold's slot is asymmetric like the pending bound. Backward it
    // still starts at one packet: a duplicated slot (exactly one packet back) keeps the held offset,
    // a move just under one packet back (one packet − 50 ns) folds like the skew hold's.
    let step = 682_474_000_i64;
    let jump = relabel_jump(step);
    for (back, folds) in [(PACKET as i64, false), (PACKET as i64 - 50, true)] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        f.early = (step - jump) as u64;
        f.take();
        for _ in 0..3 {
            f.take();
        }
        let held = f.s.held_off_ns;
        f.shift -= back;
        f.take();
        assert!(f.s.active && f.s.relabel_pending);
        assert_eq!(
            f.s.held_off_ns,
            if folds { held + back } else { held },
            "issue 1381: a backward move of {back} ns inside a pending"
        );
    }
}

#[test]
fn a_skipped_slot_inside_a_pending_keeps_the_held_offset_on_every_grid_position_1381() {
    // review round 1 (slice 3): inside a pending the fold split "under one packet" (folded like the
    // skew hold's) from "a slot or more" (folded only when relabel-shaped) at exactly one packet, so a
    // skipped slot at the − 66 ns grid position (its stamps one packet − 66 ns on, its arrival one
    // packet late) was folded: the held offset moved by a slot and this box's step resolved it with
    // p − r instead of −r. The split now uses the pending bound's own slot (forward from one packet −
    // 100 ns), so a skipped slot keeps the held offset on every grid position.
    let step = 682_474_000_i64;
    let jump = relabel_jump(step);
    let r = step - jump;
    for skip_jump in [PACKET as i64 - 66, PACKET as i64 + 34, PACKET as i64] {
        let mut f = Feed::new();
        f.steady(WARM);
        f.shift = jump;
        f.early = r as u64;
        f.take();
        for _ in 0..5 {
            f.take();
        }
        let held = f.s.held_off_ns;
        // one slot never sent: the arrival gaps one packet, the stamps jump `skip_jump`
        f.k += 1;
        f.shift += skip_jump - PACKET as i64;
        for _ in 0..5 {
            assert_eq!(
                f.take(),
                (held, AudioStepRelease::None),
                "issue 1381: a skipped slot ({skip_jump} ns) inside a pending must keep its held \
                 offset"
            );
        }
        assert!(f.s.active && f.s.relabel_pending && f.s.held_off_ns == held);
        f.off = OFF - step;
        let (off, rel) = f.take();
        assert_eq!(
            (off, rel, audio_step_residual_ns(f.s.held_off_ns, off)),
            (OFF - step, AudioStepRelease::RelabelPending, -r),
            "issue 1381: skip {skip_jump}: this box's step resolves it with −r"
        );
    }
}

/// Review round 2 (ROZHODNUTÉ 5903945145): the review's two-box date-step probe. A seeded LCG for the
/// arrival jitter, uniform in ±`j`.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn uni(&mut self, j: i64) -> i64 {
        if j == 0 {
            0
        } else {
            (self.next() % (2 * j as u64 + 1)) as i64 - j
        }
    }
}

const MS: i64 = 1_000_000;
const SEC: i64 = 1_000_000_000;
/// A sender wall on a whole second, so its 100 ns per-second grid is the stamps' grid.
const WALL_S: i64 = 1_790_000_000_000_000_000;

/// Block `k`'s boundary on a sender's per-second 100 ns grid at `fps`.
fn grid_ns(k: i64, fps: i64) -> i64 {
    (k * 10_000_000).div_euclid(fps) * 100
}

/// What one two-box run saw.
struct TwoBox {
    /// The blocks a pending relabel started on.
    starts: Vec<i64>,
    /// Every hold release: (block, reason, residual).
    releases: Vec<(i64, AudioStepRelease, i64)>,
    /// The sender's first relabelled block.
    relabel_block: i64,
    packet: u64,
}

/// One timecode source across a date step S on BOTH boxes: this box's wall steps at `recv_at`, the
/// sender's at `send_at` (ns from the start). The sender relabels (contract §5): N = floor(S / slot),
/// its first relabelled block on the old schedule, the next ones re-phased by r. Every arrival is
/// jittered by ±`jitter` (kept in order), the stamps on the sender's 100 ns grid.
#[allow(clippy::too_many_arguments)]
fn two_box(
    fps: i64,
    step: i64,
    recv_at: i64,
    send_at: i64,
    jitter: i64,
    seed: u64,
    total: i64,
) -> TwoBox {
    let packet = (48_000 / fps) as u64 * 1_000_000_000 / 48_000;
    let slot = SEC / fps;
    let n = (i128::from(step) * i128::from(fps)).div_euclid(i128::from(SEC)) as i64;
    let mut s = AudioStepHold::default();
    let mut rng = Lcg(seed.wrapping_mul(7919).wrapping_add(17));
    let mut out = TwoBox {
        starts: Vec::new(),
        releases: Vec::new(),
        relabel_block: -1,
        packet,
    };
    let mut prev_arrival = i64::MIN;
    for k in 0..total / slot {
        let old_emit = grid_ns(k, fps);
        let (label, emit) = if old_emit < send_at {
            (old_emit, old_emit)
        } else {
            if out.relabel_block < 0 {
                out.relabel_block = k;
            }
            let label = grid_ns(k + n, fps);
            if k == out.relabel_block {
                (label, old_emit)
            } else {
                (label, label - step)
            }
        };
        let arrival = (emit + rng.uni(jitter)).max(prev_arrival + 1_000);
        prev_arrival = arrival;
        let off = if arrival >= recv_at { OFF - step } else { OFF };
        let was_pending = s.active && s.relabel_pending;
        let (mapped, release) = audio_step_hold(
            &mut s,
            true,
            off,
            (WALL_S + label) as u64,
            packet,
            (BASE as i64 + arrival) as u64,
            false,
            MIN,
        );
        if s.active && s.relabel_pending && !was_pending {
            out.starts.push(k);
        }
        if release != AudioStepRelease::None {
            out.releases
                .push((k, release, audio_step_residual_ns(s.held_off_ns, mapped)));
        }
    }
    out
}

#[test]
fn a_jittered_late_follow_never_starts_a_pending_1381() {
    // review round 2 (ROZHODNUTÉ 5903945145): this box's wall steps first, by S at 10 s. Under ±1–5 ms
    // of arrival jitter a step just over one slot (|S| = 34–38 ms) releases the skew hold EARLY on the
    // age test, and the in-band nominal then tracks the step the stamps never matched. The sender
    // relabels `lag` later; the away guard alone read that late follow as a sender-first step --
    // backward (N = −2 overshoots the nominal) and forward with a lag of 60 s or more -- a 10 s hold,
    // then one slot placed (66.7 ms overwritten, or a 33.3 ms gap). The review's whole grid: from the
    // sender's relabel on no pending relabel ever starts.
    let mut unmatched_releases = 0;
    let mut runs = 0;
    for fps in [30_i64, 60] {
        for step_ms in [
            -80_i64, -60, -50, -45, -40, -38, -37, -36, -35, -34, 34, 35, 36, 38, 40, 45, 50, 55,
            60, 66,
        ] {
            for lag_s in [2_i64, 5, 12, 20, 60, 120] {
                for jitter_ms in [0_i64, 1, 2, 3, 5] {
                    for seed in 0..12_u64 {
                        let o = two_box(
                            fps,
                            step_ms * MS,
                            10 * SEC,
                            (10 + lag_s) * SEC,
                            jitter_ms * MS,
                            seed,
                            (25 + lag_s) * SEC,
                        );
                        runs += 1;
                        let late: Vec<i64> = o
                            .starts
                            .iter()
                            .copied()
                            .filter(|&k| k >= o.relabel_block)
                            .collect();
                        assert!(
                            late.is_empty(),
                            "issue 1381: fps {fps} S {step_ms} ms lag {lag_s} s jitter ±{jitter_ms} \
                             ms seed {seed}: the late follow at block {} started a pending relabel \
                             at {late:?} (releases {:?})",
                            o.relabel_block,
                            o.releases
                        );
                        unmatched_releases += o
                            .releases
                            .iter()
                            .filter(|&&(k, rel, res)| {
                                k < o.relabel_block
                                    && rel != AudioStepRelease::Reset
                                    && res.unsigned_abs() > o.packet
                            })
                            .count();
                    }
                }
            }
        }
    }
    assert!(
        runs == 14_400 && unmatched_releases > 1_000,
        "the grid must reach the unmatched releases it is about: {unmatched_releases} in {runs} runs"
    );
}

#[test]
fn a_genuine_sender_first_step_still_starts_and_resolves_1381() {
    // review round 2: the remembered step never costs a GENUINE sender-first step -- the sender's box
    // steps first at 10 s and this box follows 1, 3 or 6 s later: the pending starts on the relabelled
    // block and this box's own step resolves it, every whole-slot step, both rates, jitter to ±7 ms.
    for fps in [30_i64, 60] {
        for step_ms in [-682_i64, -100, -66, -50, -36, 34, 36, 40, 50, 66, 100, 682] {
            for lag_s in [1_i64, 3, 6] {
                for jitter_ms in [0_i64, 2, 5, 7] {
                    for seed in 0..4_u64 {
                        let o = two_box(
                            fps,
                            step_ms * MS,
                            (10 + lag_s) * SEC,
                            10 * SEC,
                            jitter_ms * MS,
                            seed,
                            (22 + lag_s) * SEC,
                        );
                        assert!(
                            o.starts.first() == Some(&o.relabel_block)
                                && o
                                    .releases
                                    .iter()
                                    .any(|r| r.1 == AudioStepRelease::RelabelPending),
                            "issue 1381: fps {fps} S {step_ms} ms lag {lag_s} s jitter ±{jitter_ms} \
                             ms seed {seed}: a sender-first step must start on block {} and resolve \
                             (starts {:?}, releases {:?})",
                            o.relabel_block,
                            o.starts,
                            o.releases
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn an_early_age_release_remembers_the_unmatched_step_and_its_follow_clears_it_1381() {
    // ROZHODNUTÉ 5903945145: this box steps +34 ms first; the next packet arrives 2 ms early, so its
    // age is back within one packet of the nominal and the skew hold releases on the age test. That
    // step (−34 ms in offset terms) is remembered, with the release packet's arrival. 60 s of packets
    // alternating 2 ms / 0 ms early later, the sender's one-slot relabel (one packet − 66 ns) is its
    // follow: no pending start, and the memory is cleared.
    let mut f = Feed::new();
    f.early = 10_000_000;
    f.steady(WARM);
    f.off = OFF - 34_000_000;
    assert_eq!(
        f.take(),
        (OFF, AudioStepRelease::None),
        "the step starts a hold"
    );
    f.early = 12_000_000;
    let at = f.now();
    let (off, rel) = f.take();
    assert_eq!((off, rel), (OFF - 34_000_000, AudioStepRelease::Followed));
    assert_eq!(
        (f.s.unmatched_ns, f.s.unmatched_at_ns),
        (-34_000_000, at),
        "issue 1381: the step the stamps never matched is remembered"
    );
    for i in 0..1800 {
        f.early = if i % 2 == 0 { 10_000_000 } else { 12_000_000 };
        assert_eq!(f.take().1, AudioStepRelease::None);
    }
    assert_eq!(f.s.unmatched_ns, -34_000_000, "nothing else clears it");
    f.early = 10_000_000;
    f.shift = PACKET as i64 - 66;
    assert!(
        !f.starts(),
        "issue 1381: the remembered step's follow never starts a pending relabel"
    );
    f.take();
    assert!(!f.s.active, "no hold of any kind");
    assert_eq!(
        f.s.unmatched_ns, 0,
        "issue 1381: the follow clears the remembered step"
    );
}

#[test]
fn only_a_hold_that_ended_unmatched_is_remembered_1381() {
    // ROZHODNUTÉ 5903945145: the timeout of a skew hold (this box's step, the stamps never followed)
    // is remembered; a follow the stamps made (released within one packet), a pending relabel's
    // timeout (the SENDER's step: its counterpart is this box's own offset jump) and a reset are not.
    let packets_to_bound = AUDIO_STEP_HOLD_MAX_NS.div_ceil(PACKET) as usize + 2;
    // a skew hold's timeout
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 60_000_000;
    let mut released = None;
    for _ in 0..packets_to_bound {
        let now = f.now();
        let (_, rel) = f.take();
        if rel != AudioStepRelease::None {
            released = Some((rel, now));
            break;
        }
    }
    let (rel, at) = released.expect("the hold ends");
    assert_eq!(rel, AudioStepRelease::Timeout);
    assert_eq!((f.s.unmatched_ns, f.s.unmatched_at_ns), (-60_000_000, at));
    // the stamps follow: released within one packet, nothing remembered
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 60_000_000;
    f.take();
    f.shift = 60_000_000;
    assert_eq!(f.take().1, AudioStepRelease::Followed);
    assert_eq!(f.s.unmatched_ns, 0, "a matched step is never remembered");
    // the stamps follow to EXACTLY one packet short: released (within one packet), not remembered
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 60_000_000;
    f.take();
    f.shift = 60_000_000 - PACKET as i64;
    let (off, rel) = f.take();
    assert_eq!(
        (rel, audio_step_residual_ns(f.s.held_off_ns, off)),
        (AudioStepRelease::Followed, -(PACKET as i64))
    );
    assert_eq!(
        f.s.unmatched_ns, 0,
        "a move of exactly one packet is not remembered"
    );
    // a stamp move of the follow's size and sign that is no whole slot (continuous arrival, 20 ms)
    // keeps the memory: only a relabel-shaped follow clears it
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 60_000_000;
    for _ in 0..packets_to_bound {
        f.take();
    }
    assert_eq!(f.s.unmatched_ns, -60_000_000);
    f.shift = 20_000_000;
    f.take();
    assert_eq!(
        f.s.unmatched_ns, -60_000_000,
        "a sub-slot stamp move keeps the memory"
    );
    // a pending relabel's timeout
    let mut f = Feed::new();
    f.steady(WARM);
    f.shift = relabel_jump(682_474_000);
    let mut timed_out = false;
    for _ in 0..packets_to_bound {
        timed_out |= f.take().1 == AudioStepRelease::Timeout;
    }
    assert!(timed_out);
    assert_eq!(
        f.s.unmatched_ns, 0,
        "a pending relabel's timeout is not remembered"
    );
    // a timeline reset inside a hold
    let mut f = Feed::new();
    f.steady(WARM);
    f.off = OFF - 60_000_000;
    f.take();
    assert_eq!(f.take_with(true, true).1, AudioStepRelease::Reset);
    assert_eq!(f.s.unmatched_ns, 0, "a reset is not remembered");
}

#[test]
fn the_remembered_step_holds_for_the_window_and_timecode_off_forgets_it_1381() {
    // ROZHODNUTÉ 5903945145: the follow window is the re-anchor window (600 s), strictly; a stamp jump
    // of the same direction as the memory (the wrong way) or outside one packet + the budget of its
    // size is no follow; leaving timecode mode forgets it.
    let at = 5_000_000_000_u64;
    let s = AudioStepHold {
        unmatched_ns: -34_000_000,
        unmatched_at_ns: at,
        ..AudioStepHold::default()
    };
    let p = PACKET as i64;
    let w = AUDIO_STEP_NOMINAL_REANCHOR_NS;
    let far = (PACKET + AUDIO_RELABEL_ARRIVAL_JITTER_NS) as i64;
    for (jump, now, follows) in [
        (p - 66, at + w - 1, true),
        (p - 66, at + w, false),
        (-(p - 66), at + 1, false),
        (34_000_000 + far, at + 1, true),
        (34_000_000 + far + 1, at + 1, false),
        (0, at + 1, false),
        // the wrong direction even where the size would fit
        (-10_000_000, at + 1, false),
    ] {
        assert_eq!(
            audio_step_unmatched_follow(&s, jump, PACKET, now),
            follows,
            "issue 1381: jump {jump} at {} ns after the memory",
            now - at
        );
    }
    // a zero stamp move never follows, also a memory of the other sign (a backward wall step)
    let back = AudioStepHold {
        unmatched_ns: 36_000_000,
        unmatched_at_ns: at,
        ..AudioStepHold::default()
    };
    assert!(audio_step_unmatched_follow(
        &back,
        -66_666_667,
        PACKET,
        at + 1
    ));
    assert!(!audio_step_unmatched_follow(&back, 0, PACKET, at + 1));
    let mut f = Feed::new();
    f.steady(WARM);
    f.s.unmatched_ns = -34_000_000;
    f.s.unmatched_at_ns = at;
    f.take_with(false, false);
    assert_eq!(
        f.s.unmatched_ns, 0,
        "issue 1381: timecode off forgets the step"
    );
}
