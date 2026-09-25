//! issue 1367 — the `ReleaseCadence` N==1 pin-derived-depth tests (the probe mirror of the C
//! `genlock_should_converge_phase` routing and `genlock_should_hold_n1_phase`). Split out of
//! `genlock.rs` to keep the lane from growing that file further.

use super::*;

/// issue 1367 (review round 1): the N==1 shed runs only for a tick of the N==1 STEADY branch. On
/// the N>=2 branch (`last_known_n` latched >= 2 by `effective_source_multiple`) a post-erase
/// re-measure that reads a conclusive n == 1 (a pair straddling a dropped 60 fps frame) must stay
/// inert, exactly as before the N==1 rule — the C wrapper carries the same guard.
#[test]
fn n2_branch_tick_never_enters_the_n1_shed_1367() {
    use std::collections::VecDeque;
    const I30: u64 = 33_333_333;
    let wall = 1_000_000_000_000u64;
    // A deep pin (987 ms, base 30, target 31) whose remaining queue reads n == 1 conclusively
    // (two frames one canvas interval apart, the freshest one interval old), with the locked
    // boundary 33 frames old — over the N==1 target.
    let queue: VecDeque<u64> = [wall - 2 * I30, wall - I30].into();
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(wall - 33 * I30);
    cadence.ticks_since_last_drain = 100;
    cadence.last_known_n = 1; // a tick of the N==1 STEADY branch
    assert!(cadence.should_converge_phase(&queue, 987, I30, wall));
    cadence.last_known_n = 2; // the same queue on a tick of the N>=2 branch
    assert!(!cadence.should_converge_phase(&queue, 987, I30, wall));
}

/// issue 1367 — the N==1 HOLD, at tick level: a deep N==1 conveyor whose queue head would go on
/// air one frame SHALLOWER than its pin-derived depth (30 frames at pin 987, target 31) HOLDS
/// one tick — nothing presented, nothing dropped, the drain throttle reset — and the same head
/// one tick later (now 31 frames old) presents. A head already at the target presents at once.
#[test]
fn n1_steady_tick_holds_a_too_shallow_deep_conveyor_1367() {
    use std::collections::VecDeque;
    const I30: u64 = 33_333_333;
    let wall = 1_000_000_000_000u64;
    let head = wall - 30 * I30;
    let mut queue: VecDeque<u64> = (0..30u64).map(|k| head + k * I30).collect();
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(head);
    cadence.last_known_n = 1;
    cadence.ticks_since_last_drain = 100;
    let held = cadence.tick(wall, 987, I30, &mut queue);
    assert_eq!(
        held.presented, None,
        "one frame short of the target -> HOLD"
    );
    assert!(held.dropped.is_empty() && !held.late_hold && !held.relocked);
    assert_eq!(
        cadence.ticks_since_last_drain, 0,
        "the hold resets the drain throttle"
    );
    assert_eq!(queue.len(), 30, "a hold consumes nothing");
    // One tick later the same head is 31 frames old: at the target, presented (the throttle
    // is below its interval now, so neither the hold nor a shed may act anyway).
    queue.push_back(wall);
    let next = cadence.tick(wall + I30, 987, I30, &mut queue);
    assert_eq!(next.presented, Some(head));
    // A fresh conveyor already at the target presents at once.
    let head2 = wall - 31 * I30;
    let mut q2: VecDeque<u64> = (0..31u64).map(|k| head2 + k * I30).collect();
    let mut c2 = ReleaseCadence::new();
    c2.locked_next_boundary_ns = Some(head2);
    c2.last_known_n = 1;
    c2.ticks_since_last_drain = 100;
    assert_eq!(c2.tick(wall, 987, I30, &mut q2).presented, Some(head2));
}
