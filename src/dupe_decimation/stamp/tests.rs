use super::*;
use crate::dupe_decimation::DecimationGate;
use crate::genlock_grid::{grid_advance_ns, grid_floor_ns, NS_PER_SECOND};
use crate::genlock_pacing::boundary_skip_count;

const I60: u64 = NS_PER_SECOND / 60;
/// 2026-09-23 17:16:33 UTC — a real date (the #1355 tests' second).
const SEC_2309: u64 = 1_790_176_593;

fn slot(k: u64) -> u64 {
    grid_advance_ns(SEC_2309 * NS_PER_SECOND, k, I60)
}

// ── stamp_slot_action (pure) ─────────────────────────────────────────────────

#[test]
fn stamp_slot_action_covers_every_case() {
    let b = STARVATION_REPEAT_MAX;
    assert_eq!(
        stamp_slot_action(0, slot(5), I60, b),
        StampSlotAction::Latch
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(6), I60, b),
        StampSlotAction::Advance
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(5), I60, b),
        StampSlotAction::Duplicate
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(7), I60, b),
        StampSlotAction::Gap {
            missing: 1,
            repeats: 1
        }
    );
    let edge = 1 + GENLOCK_MAX_CATCHUP_INTERVALS;
    assert_eq!(
        stamp_slot_action(slot(5), slot(5 + edge), I60, b),
        StampSlotAction::Gap {
            missing: GENLOCK_MAX_CATCHUP_INTERVALS,
            repeats: STARVATION_REPEAT_MAX
        },
        "a gap at the catch-up bound fills at most the repeat cap"
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(5 + edge + 1), I60, b),
        StampSlotAction::Resync {
            skipped: GENLOCK_MAX_CATCHUP_INTERVALS + 1
        }
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(3), I60, b),
        StampSlotAction::Resync { skipped: 0 }
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(8), I60, 1),
        StampSlotAction::Gap {
            missing: 2,
            repeats: 1
        },
        "a spent repeat budget leaves the rest of the gap unfilled"
    );
    assert_eq!(
        stamp_slot_action(slot(5), slot(7), I60, 0),
        StampSlotAction::Gap {
            missing: 1,
            repeats: 0
        }
    );
}

#[test]
fn a_whole_second_roll_over_is_a_plain_advance() {
    // Slot 59 of a second to slot 0 of the next: one grid step on the per-second grid.
    let last = grid_advance_ns(SEC_2309 * NS_PER_SECOND, 59, I60);
    let next = (SEC_2309 + 1) * NS_PER_SECOND;
    assert_eq!(grid_advance_ns(last, 1, I60), next);
    assert_eq!(
        stamp_slot_action(last, next, I60, STARVATION_REPEAT_MAX),
        StampSlotAction::Advance
    );
}

// ── DecimationGate on stamp slots ────────────────────────────────────────────

/// Drive the gate like the capture loop: stage the slot, poll with a poll instant that is
/// deliberately far from the slot (the decision must not read it), note the emitted stamp.
/// Returns (emitted, repeats, #707 skip) per frame.
fn drive(gate: &mut DecimationGate, slots: &[u64], poll_offset_ns: u64) -> Vec<(bool, u64, u64)> {
    slots
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let prev = gate.next_boundary_ns();
            gate.note_stamp_slot(s);
            // The poll wall clock: the slot plus a large, varying latency.
            let now = s + poll_offset_ns + (k as u64 % 7) * 3_000_000;
            let emit = gate.poll(now, I60, 0x5eed ^ k as u64, k % 3 == 0, now, s);
            let repeats = gate.last_poll_starvation_repeats();
            let skip = boundary_skip_count(prev, gate.next_boundary_ns(), I60)
                .saturating_sub(gate.last_poll_intentional_extra_advance());
            if emit {
                gate.note_emitted_stamp_100ns(
                    crate::capture_phase::slot_stamp_100ns(s, I60, 60),
                    I60,
                );
            }
            (emit, repeats, skip)
        })
        .collect()
}

#[test]
fn stamp_slots_emit_on_advance_drop_duplicates_and_fill_gaps_whatever_the_poll_time() {
    // 20 on-time slots, a duplicate (fast crossing), 20 more, a missing slot (slow crossing),
    // 20 more. The poll instant lags 25-43 ms — more than two slots — on purpose.
    let mut ks: Vec<u64> = (0..20).collect();
    ks.push(19);
    ks.extend(20..40);
    ks.extend(41..61);
    let slots: Vec<u64> = ks.iter().map(|&k| slot(k)).collect();
    let mut gate = DecimationGate::new();
    let out = drive(&mut gate, &slots, 25_000_000);
    let emits = out.iter().filter(|o| o.0).count();
    assert_eq!(emits, slots.len() - 1, "only the duplicate slot is dropped");
    assert!(!out[20].0, "the duplicate slot");
    let repeats: u64 = out.iter().map(|o| o.1).sum();
    assert_eq!(
        repeats, 1,
        "the one missing slot is filled by one starvation repeat"
    );
    assert_eq!(
        out[41].1, 1,
        "the frame after the missing slot carries the fill"
    );
    assert!(out.iter().all(|o| o.2 == 0), "no #707 skip");
    let (dupe_shed, blind_shed, copies, retired, drained, fast) = gate.take_shed_counts();
    assert_eq!(
        (dupe_shed, blind_shed, copies, retired, drained, fast),
        (0, 1, 0, 0, 0, 0)
    );
    assert_eq!(gate.take_starvation_repeats(), 1);
    assert_eq!(gate.last_stamp_action(), Some(StampSlotAction::Advance));
}

#[test]
fn a_clock_step_on_stamp_slots_is_one_resync_and_one_skip_line() {
    let mut ks: Vec<u64> = (0..30).collect();
    ks.extend(72..100); // +700 ms: 42 slots ahead
    let slots: Vec<u64> = ks.iter().map(|&k| slot(k)).collect();
    let mut gate = DecimationGate::new();
    let out = drive(&mut gate, &slots, 12_000_000);
    assert!(out.iter().all(|o| o.0), "every frame emits");
    assert_eq!(
        out.iter().map(|o| o.1).sum::<u64>(),
        0,
        "a clock step is never filled"
    );
    let skips: Vec<u64> = out.iter().map(|o| o.2).filter(|&s| s > 0).collect();
    assert_eq!(
        skips,
        vec![42],
        "one #707 SKIP of the 42 slots the step leapt"
    );

    // Backward: one re-latch, no skip line, the slots continue from the new position.
    let mut ks: Vec<u64> = (100..130).collect();
    ks.extend(58..90);
    let slots: Vec<u64> = ks.iter().map(|&k| slot(k)).collect();
    let mut gate = DecimationGate::new();
    let mut resyncs = 0;
    for (k, &s) in slots.iter().enumerate() {
        gate.note_stamp_slot(s);
        assert!(gate.poll(s + 9_000_000, I60, k as u64, false, 0, s));
        gate.note_emitted_stamp_100ns(crate::capture_phase::slot_stamp_100ns(s, I60, 60), I60);
        if matches!(
            gate.last_stamp_action(),
            Some(StampSlotAction::Resync { .. })
        ) {
            resyncs += 1;
        }
        assert_eq!(gate.last_poll_starvation_repeats(), 0);
    }
    assert_eq!(resyncs, 1);
}

#[test]
fn a_half_rate_leg_on_stamp_slots_stops_being_filled_after_the_repeat_cap() {
    // Every other slot missing (a dying leg): the consecutive repeat cap must stop the fill so
    // the leg still under-runs downstream.
    let slots: Vec<u64> = (0..40).map(|k| slot(2 * k)).collect();
    let mut gate = DecimationGate::new();
    let out = drive(&mut gate, &slots, 12_000_000);
    let repeats: u64 = out.iter().map(|o| o.1).sum();
    assert_eq!(repeats, STARVATION_REPEAT_MAX);
}

#[test]
fn the_stamp_path_continues_from_the_poll_time_path_and_back() {
    let mut gate = DecimationGate::new();
    // Poll-time path: 60 fps captures polled 11 ms after capture (today's gate).
    let mut last_stamp = 0;
    let mut emitted_slots = Vec::new();
    for k in 0..30u64 {
        let cap = slot(k) + 8_000_000;
        let now = cap + 11_000_000;
        if gate.poll(now, I60, k, false, now, cap) {
            last_stamp = crate::capture_phase::slot_stamp_100ns(grid_floor_ns(cap, I60), I60, 60);
            gate.note_emitted_stamp_100ns(last_stamp, I60);
            emitted_slots.push(grid_floor_ns(cap, I60));
        }
        assert_eq!(gate.last_stamp_action(), None);
    }
    assert!(last_stamp > 0);
    // The tracker locks: the next frame's slot is one after the last emitted one.
    for k in 30..60u64 {
        gate.note_stamp_slot(slot(k));
        let now = slot(k) + 19_000_000;
        assert!(gate.poll(now, I60, k, true, now, slot(k)), "frame {k}");
        assert_eq!(gate.last_poll_starvation_repeats(), 0, "frame {k}");
        gate.note_emitted_stamp_100ns(
            crate::capture_phase::slot_stamp_100ns(slot(k), I60, 60),
            I60,
        );
        emitted_slots.push(slot(k));
    }
    // The tracker re-seeds: back on the poll-time gate, the first poll emits on time (no fill).
    for k in 60..90u64 {
        let cap = slot(k) + 8_000_000;
        let now = cap + 11_000_000;
        assert!(gate.poll(now, I60, k, false, now, cap), "frame {k}");
        assert_eq!(gate.last_poll_starvation_repeats(), 0, "frame {k}");
        gate.note_emitted_stamp_100ns(
            crate::capture_phase::slot_stamp_100ns(grid_floor_ns(cap, I60), I60, 60),
            I60,
        );
        emitted_slots.push(grid_floor_ns(cap, I60));
    }
    assert!(
        emitted_slots
            .windows(2)
            .all(|w| grid_advance_ns(w[0], 1, I60) == w[1]),
        "every emitted stamp is one slot after the previous, across both transitions"
    );
}
