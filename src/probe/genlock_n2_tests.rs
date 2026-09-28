//! Issue 1367 slice D1 — the `ReleaseCadence` N>=2 GRID-EXACT release (the probe mirror of the C
//! `genlock_release_tick_n2_grid`). Split out of `genlock.rs` to keep that file from growing.
//!
//! The sims here model today's render tick, not the #401-era one: every tick is SCHEDULED on the
//! per-second canvas grid and runs up to 2 ms late (never early — `video_sleep` sleeps to the grid
//! point), and every camera stamp is the 100 ns-unit per-second 60 fps floor of its capture instant.

use super::*;
use crate::genlock_grid::{grid_advance_ns, per_second_floor, UNITS_100NS_PER_SECOND};
use std::collections::VecDeque;

const I30: u64 = 33_333_333;
const I60: u64 = 16_666_666;
/// A whole second (the per-second grid restarts here).
const S0: u64 = 1_790_640_000_000_000_000;

/// The camera sender stamp of the 60 fps slot `slot` counted from `S0` (captured 1 ms into it).
fn stamp_of_slot(slot: u64) -> u64 {
    let capture = grid_advance_ns(S0, slot, I60) + 1_000_000;
    per_second_floor(capture / 100, 60, UNITS_100NS_PER_SECOND) * 100
}

/// One run: the receiver starts at canvas tick `start_tick` (counted from `S0`) with an empty
/// FIFO; 60 fps frames arrive `lag_of(slot)` after their stamp, in order. Returns, per tick that
/// presented, `(tick, presented stamp, n2_early)`, plus the late holds after the first present.
fn run_n2(
    start_tick: u64,
    ticks: u64,
    lag_of: &dyn Fn(u64) -> u64,
) -> (Vec<(u64, u64, bool)>, usize) {
    let mut cadence = ReleaseCadence::new();
    let mut queue: VecDeque<u64> = VecDeque::new();
    // The first slot that can still arrive after the start (older ones were sent before it).
    let mut next_slot = (start_tick * 2).saturating_sub(12);
    let mut presents = Vec::new();
    let mut late_holds = 0usize;
    for k in start_tick..start_tick + ticks {
        let scheduled = grid_advance_ns(S0, k, I30);
        let wall = scheduled + (k * 7_919 % 2_000_000); // 0..2 ms late, never early
        let start_wall = grid_advance_ns(S0, start_tick, I30);
        loop {
            let s = stamp_of_slot(next_slot);
            let arrival = s + lag_of(next_slot);
            if arrival > wall {
                break;
            }
            if arrival >= start_wall {
                queue.push_back(s);
            }
            next_slot += 1;
        }
        let out = cadence.tick(wall, 3, I30, &mut queue);
        if let Some(p) = out.presented {
            presents.push((k, p, out.n2_early));
        } else if out.late_hold && !presents.is_empty() {
            late_holds += 1;
        }
    }
    (presents, late_holds)
}

/// The deterministic ±4 ms arrival jitter around `base_ms` (the old #401 sims' hash).
fn jittered(base_ms: u64) -> impl Fn(u64) -> u64 {
    move |slot| base_ms * 1_000_000 + (slot * 2_654_435_761) % 8_000_001
}

/// A 60 fps camera into the 30 fps canvas at the production pin presents EVERY SECOND frame, and
/// each one is the grid target: the frame of the 60 fps slot four slots before the tick (66.7 ms).
/// Replaces the #726 boundary-conveyor lock `cadence_60_into_30_presents_uniform_every_second_frame`
/// (whose sim ticked 2 ms EARLY every other tick — a render tick that no longer exists).
#[test]
fn n2_60_into_30_presents_every_second_frame_at_the_grid_age_1367() {
    let (presents, late_holds) = run_n2(30, 600, &jittered(20));
    let steady: Vec<&(u64, u64, bool)> = presents.iter().skip(5).collect();
    assert!(steady.len() > 580, "steady presents: {}", steady.len());
    for &&(k, p, early) in &steady {
        assert_eq!(
            p,
            stamp_of_slot(2 * k - 4),
            "tick {k}: the grid target (four 60 fps slots back), never an arrival-picked frame"
        );
        assert!(
            !early,
            "tick {k}: an arrival inside the budget is never early"
        );
    }
    for w in steady.windows(2) {
        assert_eq!(w[1].0, w[0].0 + 1, "a present on every tick (no hold)");
    }
    assert_eq!(late_holds, 0, "no late hold in steady state");
}

/// RESTART DETERMINISM — the S half of the per-restart camera lottery: whatever instant the strih
/// OBS (re)starts and whatever arrival lag the camera has inside the budget, the steady presented
/// frame is the same function of the tick.
#[test]
fn n2_every_restart_and_lag_lands_on_the_same_frame_1367() {
    let mut runs = 0;
    for start in [30u64, 31, 47, 58, 90, 91, 133] {
        for lag_ms in [5u64, 18, 33, 36, 49, 52, 58] {
            let (presents, _) = run_n2(start, 120, &jittered(lag_ms));
            let steady: Vec<&(u64, u64, bool)> = presents.iter().skip(6).collect();
            assert!(steady.len() > 100, "start {start} lag {lag_ms}");
            for &&(k, p, early) in &steady {
                assert_eq!(
                    p,
                    stamp_of_slot(2 * k - 4),
                    "start {start} lag {lag_ms} ms tick {k}"
                );
                assert!(!early, "start {start} lag {lag_ms} ms tick {k}");
            }
            runs += 1;
        }
    }
    assert_eq!(runs, 49);
}

/// A target that arrives late (outside the budget) goes on air one tick EARLY: the frame one
/// source interval before it is presented, flagged, and the next tick is back on the grid — the
/// late frame never re-anchors the phase (the boundary conveyor it replaces moved the camera one
/// frame older until the #1049 shed pulled it back).
#[test]
fn n2_late_target_is_early_for_one_tick_then_back_on_the_grid_1367() {
    // Slot 2*40 - 4 = 76 is the target of tick 40; it arrives 90 ms late.
    let lag = |slot: u64| {
        if slot == 76 {
            90_000_000
        } else {
            20_000_000
        }
    };
    let (presents, _) = run_n2(30, 30, &lag);
    let at = |k: u64| {
        presents
            .iter()
            .find(|p| p.0 == k)
            .copied()
            .expect("presented")
    };
    assert_eq!(at(39), (39, stamp_of_slot(74), false));
    assert_eq!(
        at(40),
        (40, stamp_of_slot(75), true),
        "the frame before the late target, flagged early"
    );
    assert_eq!(at(41), (41, stamp_of_slot(78), false), "back on the grid");
    assert_eq!(at(42), (42, stamp_of_slot(80), false));
}

/// The logged 20:50:58 cam6 burst (17 queued 60 fps frames, the head 300 ms old): the grid release
/// sheds it to the target in ONE tick — no relock, no anchor — and keeps the confirmed multiple.
#[test]
fn n2_burst_sheds_to_the_target_in_one_tick_1367() {
    let wall = grid_advance_ns(S0, 60, I30);
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(wall - 400_000_000);
    cadence.last_known_n = 2;
    let first = 2 * 60 - 18; // the head, 300 ms (18 slots) old
    let mut queue: VecDeque<u64> = (first..first + 17).map(stamp_of_slot).collect();
    let out = cadence.tick(wall, 3, I30, &mut queue);
    assert_eq!(out.presented, Some(stamp_of_slot(2 * 60 - 4)));
    assert_eq!(
        out.dropped.len(),
        14,
        "every frame before the target, in one tick"
    );
    assert!(!out.relocked && !out.late_hold && !out.n2_early);
    assert_eq!(queue.len(), 2, "the two younger frames stay queued");
    assert_eq!(cadence.last_known_n, 2);
    assert_eq!(
        cadence.phase_anchor_ns, 0,
        "the grid release keeps no phase anchor"
    );
}

/// A HOLD on the grid path: benign while unlocked (a cold start before the target has arrived),
/// late once the conveyor has presented and the target is missing.
#[test]
fn n2_hold_is_benign_unlocked_and_late_when_locked_1367() {
    let wall = grid_advance_ns(S0, 50, I30);
    let young: VecDeque<u64> = (98..101).map(stamp_of_slot).collect(); // all younger than slot 96
    let mut cold = ReleaseCadence::new();
    cold.last_known_n = 2;
    let mut q = young.clone();
    let out = cold.tick(wall, 3, I30, &mut q);
    assert_eq!(
        (out.presented, out.late_hold),
        (None, false),
        "unlocked: benign"
    );
    let mut locked = ReleaseCadence::new();
    locked.last_known_n = 2;
    locked.locked_next_boundary_ns = Some(stamp_of_slot(94) + I30);
    let mut q = young;
    let out = locked.tick(wall, 3, I30, &mut q);
    assert_eq!(
        (out.presented, out.late_hold),
        (None, true),
        "locked: the target is late"
    );
    assert_eq!(q.len(), 3, "a hold consumes nothing");
}
