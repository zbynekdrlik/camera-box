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
    // every packet slice 2 started a pending on still starts one, except a LATE FOLLOW (review rounds
    // 1-2: an age back toward the nominal, within one packet + the arrival budget of it), and the only
    // new starts are a forward jump of one packet − 100 ns ..= one packet, or an age between half a
    // packet and one packet. So a skipped slot (an arrival gap), a duplicated slot and an N = −1
    // relabel (one packet back) -- none of them a slice-2 start, none in either new region -- keep the
    // slice-2 path byte for byte (the bench pins the traces too). Swept from a steady feed AND from a
    // feed whose age is already far off (this box stepped first by 60 ms and its skew hold timed out).
    let p = PACKET as i64;
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
                let prev_age_dev =
                    audio_stamp_age_ns(f.s.prev_arrival_ns, f.s.prev_raw_ns, f.s.prev_off_ns)
                        .wrapping_sub(f.s.nominal_age_ns);
                let late_follow = age_dev.unsigned_abs()
                    <= PACKET + AUDIO_RELABEL_ARRIVAL_JITTER_NS
                    && age_dev.unsigned_abs() <= prev_age_dev.unsigned_abs();
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
    // else holds at the edge (a relabel-shaped jump, continuous arrival, no offset move).
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
    let edge = raw
        .wrapping_add(f.off as u64)
        .wrapping_add(f.s.nominal_age_ns as u64)
        .wrapping_sub(prev_dev as u64);
    assert!(audio_relabel_pending(
        40_000_000,
        edge.wrapping_sub(f.s.prev_arrival_ns),
        PACKET,
        MIN
    ));
    assert!(
        !audio_step_relabel_pending_starts(&f.s, true, f.off, raw, PACKET, edge, MIN),
        "issue 1381: an age exactly as far off as the previous packet's is not away"
    );
    assert!(
        audio_step_relabel_pending_starts(&f.s, true, f.off, raw, PACKET, edge - 1, MIN),
        "issue 1381: 1 ns further off than the previous packet's age is away"
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
