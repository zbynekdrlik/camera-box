//! Issue 1367 — the unit tests of `genlock_n1_depth` (a `#[path]` child, split out to keep the
//! authority under the ~1000-line budget, review round 2). They compile with the module.

use super::*;
use crate::genlock_backlog::{
    should_converge_phase, GENLOCK_N2_JITTER_BUDGET_NS, PHASE_PIN_HYSTERESIS_NS,
};

const I30: u64 = 33_333_333; // ~30 Hz frame interval (ns)
const I60: u64 = 16_666_667; // ~60 Hz frame interval (ns)

// issue 1367 — the N==1 pin-derived depth, arithmetic edges (the faithful proof of each
// threshold; the dynamic proof is the restart bench in genlock_grid_bench.rs).

#[test]
fn n1_tick_wall_is_the_processing_wall_minus_the_lateness_1367() {
    let wall = 1_000_000_000_000u64;
    let sched = 5_000_000_000u64;
    // On schedule, 45 ms late and 70 ms late: the lateness comes straight off the wall.
    assert_eq!(n1_tick_wall_ns(wall, sched, sched), wall);
    assert_eq!(
        n1_tick_wall_ns(wall, sched + 45_000_000, sched),
        wall - 45_000_000
    );
    assert_eq!(
        n1_tick_wall_ns(wall, sched + 70_000_000, sched),
        wall - 70_000_000
    );
    // A monotonic read that has not reached the schedule reads the processing wall; an absurd
    // lateness saturates at 0.
    assert_eq!(n1_tick_wall_ns(wall, sched - 1, sched), wall);
    assert_eq!(n1_tick_wall_ns(10, 1_000, 0), 0);
}

#[test]
fn n1_base_frames_is_the_resync_depth_with_a_one_microsecond_pin_tolerance_1367() {
    assert_eq!(n1_base_frames(987, I30), 30); // 29.61 frames -> 30
    assert_eq!(n1_base_frames(963, I30), 29); // 28.89 -> 29
    assert_eq!(n1_base_frames(1010, I30), 31); // 30.30 -> 31

    // An exact whole number of frames: the integer interval is 1/3 ns short of a real frame,
    // so 1000 ms reads 30.0000003 frames; the 1 us tolerance keeps it at 30, not 31.
    assert_eq!(n1_base_frames(1000, I30), 30);
    assert_eq!(n1_base_frames(100, I30), 3);
    assert_eq!(n1_base_frames(1001, I30), 31);
    assert_eq!(n1_base_frames(3, I30), 1);
    assert_eq!(n1_base_frames(500, I60), 30);
    assert_eq!(n1_base_frames(0, I30), 0);
    assert_eq!(n1_base_frames(987, 0), 0);
    assert_eq!(n1_target_frames(987, I30), 31);
}

#[test]
fn n1_depth_rounds_to_the_nearest_frame_1367() {
    let s = 1_000_000_000_000u64;
    // A schedule phase of up to half a frame either way reads the true depth.
    assert_eq!(n1_depth_frames(s + 31 * I30, s, I30), 31);
    assert_eq!(n1_depth_frames(s + 31 * I30 + 10_000_000, s, I30), 31);
    assert_eq!(n1_depth_frames(s + 31 * I30 - 10_000_000, s, I30), 31);
    assert_eq!(n1_depth_frames(s + 31 * I30 - I30 / 2, s, I30), 31);
    assert_eq!(n1_depth_frames(s + 31 * I30 - I30 / 2 - 1, s, I30), 30);
    assert_eq!(n1_depth_frames(s + 32 * I30 - I30 / 2 - 1, s, I30), 31);
    assert_eq!(n1_depth_frames(s + 32 * I30 - I30 / 2, s, I30), 32);
    // Degenerate inputs never divide by zero; a stamp ahead of the tick reads age 0.
    assert_eq!(n1_depth_frames(s, s, 0), 0);
    assert_eq!(n1_depth_frames(s, s + I30, I30), 0);
}

#[test]
fn n1_deep_source_guard_needs_two_frames_of_pin_over_the_arrival_floor_1367() {
    // pin 987: base 30. The freshest frame one interval old (the stream 2ME PGM) is deep.
    assert!(n1_is_deep_source(I30, 987, I30));
    // Edge: floor 28 frames + 2 == base 30 -> deep; floor 29 frames -> shallow.
    assert!(n1_is_deep_source(28 * I30, 987, I30));
    assert!(n1_is_deep_source(29 * I30 - 1, 987, I30));
    assert!(!n1_is_deep_source(29 * I30, 987, I30));
    // The 3 ms cg / imag case (base 1) is never deep, whatever its floor.
    assert!(!n1_is_deep_source(0, 3, I30));
    assert!(!n1_is_deep_source(0, 3, I60));
    assert!(!n1_is_deep_source(I30, 987, 0));
}

#[test]
fn n1_shed_fires_only_a_whole_frame_past_the_target_on_a_deep_source_1367() {
    let w = 1_000_000_000_000u64;
    let shed = |age: u64, ticks: u64| n1_shed_due(w, w - age, I30, 987, I30, ticks);
    assert!(!shed(31 * I30, 100), "at the target (31 frames) -> inert");
    // The rounded edge sits at 32 frames minus half a frame (I30 is odd: one ns past 31.5).
    assert!(
        !shed(32 * I30 - I30 / 2 - 1, 100),
        "just under half a frame over -> inert"
    );
    assert!(shed(32 * I30 - I30 / 2, 100), "half a frame over -> sheds");
    assert!(
        shed(32 * I30 - 10_000_000, 100),
        "one frame over, 10 ms early -> sheds"
    );
    assert!(shed(33 * I30, 100), "two frames over -> sheds");
    assert!(!shed(32 * I30, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
    assert!(
        shed(32 * I30, DRAIN_MIN_TICK_INTERVAL),
        "throttle exactly met"
    );
    // Shallow source, unlocked boundary, degenerate interval: never.
    assert!(!n1_shed_due(w, w - 5 * I30, I30, 3, I30, 100));
    assert!(!n1_shed_due(w, w - 40 * I30, 29 * I30, 987, I30, 100));
    assert!(!n1_shed_due(w, 0, I30, 987, I30, 100));
    assert!(!n1_shed_due(w, w - 40 * I30, I30, 987, 0, 100));
}

#[test]
fn n1_hold_fires_only_a_whole_frame_short_of_the_target_on_a_deep_source_1367() {
    let w = 1_000_000_000_000u64;
    let hold =
        |age: u64, n: u32, ticks: u64| should_hold_n1_phase(w, w - age, I30, 987, I30, n, ticks);
    assert!(
        hold(30 * I30, 1, 100),
        "the resync depth (30), one short of the target -> holds"
    );
    assert!(!hold(31 * I30, 1, 100), "at the target -> inert");
    assert!(
        !hold(31 * I30 - I30 / 2, 1, 100),
        "exactly half a frame short -> inert"
    );
    assert!(hold(31 * I30 - I30 / 2 - 1, 1, 100), "one ns more -> holds");
    assert!(
        !hold(31 * I30 - 10_000_000, 1, 100),
        "a 10 ms EARLY schedule phase at the target -> inert"
    );
    assert!(
        hold(30 * I30 + 10_000_000, 1, 100),
        "a 10 ms late schedule phase one short -> still holds"
    );
    assert!(!hold(30 * I30, 1, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
    assert!(hold(30 * I30, 0, 100), "source_multiple 0 is N==1");
    assert!(!hold(30 * I30, 2, 100), "an N>=2 source has its own shed");
    assert!(
        !should_hold_n1_phase(w, w - I30 / 3, 0, 3, I30, 1, 100),
        "shallow never holds"
    );
    assert!(!should_hold_n1_phase(w, w - 30 * I30, I30, 987, 0, 1, 100));
}

#[test]
fn n1_tick_is_on_grid_within_two_milliseconds_of_a_grid_point_1367() {
    let s = 1_000_000_000_000u64; // a whole second: a grid point at 30 and 60 fps
    for interval in [I30, I60] {
        for (offset, on) in [
            (0i64, true),
            (1_999_999, true),
            (2_000_000, true),
            (2_000_001, false),
            (-2_000_000, true),
            (-2_000_001, false),
            (10_000_000, false),
            (-10_000_000, false),
        ] {
            let t = s.saturating_add_signed(offset);
            assert_eq!(
                n1_tick_is_on_grid(t, interval),
                on,
                "offset {offset} ns at interval {interval}"
            );
        }
        // +1_999_999 / +2_000_001 above also bracket the clamped catch-up tick (`video_sleep`
        // after an overrun of under 2 ms past the next slot schedules slot + 2 ms).
        // Every grid point of the second is on the grid (the per-second slots, not k * I).
        let mut g = s;
        for _ in 0..(1_000_000_000 / interval) {
            assert!(n1_tick_is_on_grid(g, interval), "grid point {g}");
            assert!(
                !n1_tick_is_on_grid(g + 3_000_000, interval),
                "3 ms past {g}"
            );
            g = crate::genlock_grid::grid_next_boundary_ns(g, interval);
        }
    }
    // 29.97 fps has no per-second grid: the check floors on the 1970 grid, the same fallback
    // the render tick uses (`genlock_next_deadline`).
    let i2997 = 33_366_666u64;
    // This 29.97 grid point sits ~16.7 ms from any 30 fps per-second slot, so a floor on the
    // wrong grid would read it OFF the grid.
    let g2997 = 30_501 * i2997;
    assert!(
        !n1_tick_is_on_grid(g2997, I30),
        "the same instant on the 30 fps grid is off"
    );
    assert!(n1_tick_is_on_grid(g2997, i2997));
    assert!(n1_tick_is_on_grid(g2997 + 2_000_000, i2997));
    assert!(n1_tick_is_on_grid(g2997 - 2_000_000, i2997));
    assert!(!n1_tick_is_on_grid(g2997 + 3_000_000, i2997));
    assert!(!n1_tick_is_on_grid(g2997 - 3_000_000, i2997));
    // The pure predicate at its edges, whatever grid the caller floors on.
    assert!(n1_tick_on_grid(1_000, 0));
    assert!(!n1_tick_on_grid(1_000, 3_000_000 + 1_001));
    assert!(!n1_tick_on_grid(10_000_000, 5_000_000));
    assert!(n1_tick_on_grid(u64::MAX, u64::MAX - 2_000_000));
}

/// issue 1367 — the shed and the hold never share an edge: over a dense sweep of presented
/// ages there is no age at which both fire, and between them sits a one-frame dead-band.
#[test]
fn the_shed_and_the_hold_leave_a_one_frame_dead_band_1367() {
    let w = 1_000_000_000_000u64;
    for step in 0..2_000u64 {
        let age = 29 * I30 + step * (4 * I30 / 2_000);
        let shed = n1_shed_due(w, w - age, I30, 987, I30, 100);
        let hold = should_hold_n1_phase(w, w - age, I30, 987, I30, 1, 100);
        assert!(!(shed && hold), "age {age}");
        let inert = (31 * I30 - I30 / 2..32 * I30 - I30 / 2).contains(&age);
        assert_eq!(inert, !shed && !hold, "age {age}");
    }
}

/// issue 1367 — every decision of `should_converge_phase` is BYTE-IDENTICAL to the pre-1367
/// authority (a verbatim copy of it) over a deterministic spread of the whole argument space,
/// N==1 included (it stays inert there: the SOURCE wrapper routes an N==1 tick to
/// [`n1_shed_due`]), and the N==1 hold never touches an N>=2 source.
#[test]
fn converge_decisions_are_byte_identical_to_before_1367() {
    fn pre_1367(
        wall_now_ns: u64,
        locked_boundary_ns: u64,
        newest_stamp_ns: u64,
        latency_ms: u32,
        interval_ns: u64,
        source_multiple: u32,
        ticks_since_last_drain: u64,
    ) -> bool {
        if interval_ns == 0 || locked_boundary_ns == 0 {
            return false;
        }
        if source_multiple < 2 {
            return false;
        }
        let n = source_multiple.max(1) as u64;
        let reserve_ns = (latency_ms as u64).saturating_mul(1_000_000);
        let floor_ns = wall_now_ns.saturating_sub(newest_stamp_ns);
        let target = reserve_ns.max(floor_ns);
        let quantum = interval_ns / n;
        let budget = PHASE_PIN_HYSTERESIS_NS.max(GENLOCK_N2_JITTER_BUDGET_NS);
        let threshold = target.saturating_add(quantum).saturating_add(budget);
        let age = wall_now_ns.saturating_sub(locked_boundary_ns);
        age > threshold && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
    }
    let w = 1_000_000_000_000u64;
    let mut x: u64 = 0x1367_2026_0924_0001;
    let mut fired = 0;
    for i in 0..20_000u64 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let interval = [I30, I60, 0][(x >> 7) as usize % 3];
        let latency = ((x >> 20) % 1200) as u32;
        let n = ((x >> 3) % 5) as u32; // 0..4, the N==1 floor included
        let ticks = (x >> 11) % 60;
        let age = latency as u64 * 1_000_000 + (x >> 33) % 90_000_000;
        let boundary = if i % 97 == 0 {
            0
        } else {
            w.saturating_sub(age)
        };
        let newest = w.saturating_sub((x >> 45) % 80_000_000);
        let old = pre_1367(w, boundary, newest, latency, interval, n, ticks);
        fired += usize::from(old);
        assert_eq!(
            should_converge_phase(w, boundary, newest, latency, interval, n, ticks),
            old,
            "n={n} latency={latency} interval={interval} boundary={boundary} newest={newest} \
             ticks={ticks}"
        );
        if n >= 2 {
            assert!(!should_hold_n1_phase(
                w,
                boundary,
                w - newest,
                latency,
                interval,
                n,
                ticks
            ));
        }
    }
    assert!(
        fired > 1000 && fired < 19_000,
        "the spread must exercise both outcomes: {fired}"
    );
}

// ---- issue 1367 (ROZHODNUTÉ 5827497952): the shallow per-lock depth -------------------------

/// One shallow-source present tick on the grid at pin 3 (base 1), not deep, no imag cap.
fn tick(relock: bool, floor_frames: u64) -> ShallowTick {
    ShallowTick {
        n1: true,
        relock,
        on_grid: true,
        floor_frames,
        base_frames: 1,
        deep: false,
        min_latency_box: false,
    }
}

#[test]
fn shallow_target_is_one_frame_over_the_worse_of_pin_and_floor_1367() {
    // pin 3 at 30 fps: base 1. The live floors (newest-frame ages): `NDI test` ~31 ms (1 frame),
    // `sp-slow_video` ~64 ms (2 frames), `CG-obs` 33-67 ms (max 2 frames).
    let base = n1_base_frames(3, I30);
    assert_eq!(base, 1);
    assert_eq!(n1_shallow_target_frames(base, 1, false, false), (2, false));
    assert_eq!(n1_shallow_target_frames(base, 2, false, false), (3, false));
    assert_eq!(n1_shallow_target_frames(base, 0, false, false), (2, false));
    // a floor under the pin: the pin rule's own base + 1.
    assert_eq!(n1_shallow_target_frames(30, 1, false, false), (31, false));
    // a DEEP source latches base + 1 whatever its floor did in the window (review round 1: a
    // post-ACQUIRE stall must never latch a D the deep rule would fight).
    assert_eq!(n1_shallow_target_frames(30, 33, true, false), (31, false));
    // the min-latency (imag) guard: a D above base + 1 is REPORTED and not applied (0).
    assert_eq!(n1_shallow_target_frames(base, 2, false, true), (0, true));
    assert_eq!(n1_shallow_target_frames(base, 1, false, true), (2, false));
    assert_eq!(
        n1_shallow_target_frames(u64::MAX, 0, false, false).0,
        u64::MAX
    );
}

#[test]
fn a_sender_restart_gap_is_a_relock_a_lost_frame_is_not_1367() {
    assert!(!n1_shallow_gap_is_relock(I30));
    assert!(!n1_shallow_gap_is_relock(N1_SHALLOW_RELOCK_GAP_NS - 1));
    assert!(n1_shallow_gap_is_relock(N1_SHALLOW_RELOCK_GAP_NS));
    assert!(n1_shallow_gap_is_relock(3_000_000_000));
}

fn run_window(s: &mut ShallowDepth, floors: &[u64], on_grid: bool) -> u32 {
    let mut latches = 0;
    for (i, &f) in floors.iter().enumerate() {
        let t = ShallowTick {
            on_grid,
            ..tick(i == 0, f)
        };
        if n1_shallow_track(s, t) {
            latches += 1;
        }
    }
    latches
}

#[test]
fn the_depth_latches_once_after_the_settle_window_on_the_max_floor_1367() {
    let mut s = ShallowDepth::default();
    let floors: Vec<u64> = (0..N1_SHALLOW_SETTLE_TICKS as u64)
        .map(|i| if i % 7 == 0 { 2 } else { 1 })
        .collect();
    // the window before its last tick latches nothing.
    let latched = run_window(&mut s, &floors[..floors.len() - 1], true);
    assert_eq!(latched, 0);
    assert_eq!(s.target_frames, 0, "no D before the window closes");
    assert!(s.measuring);
    assert!(n1_shallow_track(&mut s, tick(false, 1)));
    assert_eq!(s.target_frames, 3, "max floor 2 + 1");
    assert!(!s.measuring);
    // constant until the next relock, whatever the floor does short of a whole window over D.
    for f in [0, 5, 1, 2] {
        assert!(!n1_shallow_track(&mut s, tick(false, f)));
        assert_eq!(s.target_frames, 3);
    }
}

#[test]
fn off_grid_ticks_are_not_sampled_1367() {
    let mut s = ShallowDepth::default();
    let floors = vec![9u64; 2 * N1_SHALLOW_SETTLE_TICKS as usize];
    assert_eq!(run_window(&mut s, &floors, false), 0);
    assert_eq!(s.window_ticks, 0);
    assert_eq!(s.target_frames, 0);
}

#[test]
fn a_relock_keeps_the_old_depth_until_the_new_window_latches_1367() {
    let mut s = ShallowDepth::default();
    run_window(&mut s, &[1u64; N1_SHALLOW_SETTLE_TICKS as usize], true);
    assert_eq!(s.target_frames, 2);
    // relock: a new window; the old D stays maintained meanwhile.
    assert!(!n1_shallow_track(&mut s, tick(true, 2)));
    assert!(s.measuring);
    assert_eq!(s.target_frames, 2);
    for _ in 1..N1_SHALLOW_SETTLE_TICKS - 1 {
        assert!(!n1_shallow_track(&mut s, tick(false, 2)));
        assert_eq!(s.target_frames, 2);
    }
    assert!(n1_shallow_track(&mut s, tick(false, 2)));
    assert_eq!(s.target_frames, 3, "the relock found a deeper floor");
    // a relock that finds the same floor changes nothing.
    let latched = run_window(&mut s, &[2u64; N1_SHALLOW_SETTLE_TICKS as usize], true);
    assert_eq!(latched, 1);
    assert_eq!(s.target_frames, 3);
}

#[test]
fn an_n2_source_never_carries_a_shallow_depth_and_an_n1_one_always_measures_1367() {
    let mut s = ShallowDepth::default();
    run_window(&mut s, &[1u64; N1_SHALLOW_SETTLE_TICKS as usize], true);
    assert_eq!(s.target_frames, 2);
    let n2 = ShallowTick {
        n1: false,
        ..tick(false, 1)
    };
    assert!(!n1_shallow_track(&mut s, n2));
    assert_eq!(s, ShallowDepth::default());
    // review round 1: back on N==1 WITHOUT a relock (a 60p sender switched to 30p inside a
    // second) the source still opens a window and latches.
    let mut latched = 0;
    for _ in 0..N1_SHALLOW_SETTLE_TICKS {
        latched += u32::from(n1_shallow_track(&mut s, tick(false, 1)));
    }
    assert_eq!(latched, 1);
    assert_eq!(s.target_frames, 2);
}

#[test]
fn a_floor_that_stays_at_or_over_d_re_measures_without_a_relock_1367() {
    let mut s = ShallowDepth::default();
    run_window(&mut s, &[1u64; N1_SHALLOW_SETTLE_TICKS as usize], true);
    assert_eq!(s.target_frames, 2);
    // one short excursion to the floor D resets as soon as it clears.
    for _ in 0..N1_SHALLOW_SETTLE_TICKS - 1 {
        assert!(!n1_shallow_track(&mut s, tick(false, 2)));
    }
    assert!(!n1_shallow_track(&mut s, tick(false, 1)));
    assert_eq!(s.over_ticks, 0);
    assert!(!s.measuring);
    // the arrival rose for good: a whole window at/over D re-opens the window, keeping the old D
    // until the new one latches.
    for _ in 0..N1_SHALLOW_SETTLE_TICKS - 1 {
        assert!(!n1_shallow_track(&mut s, tick(false, 3)));
    }
    assert!(!n1_shallow_track(&mut s, tick(false, 3)));
    assert!(s.measuring, "the arrival rose: a new window");
    assert_eq!(s.target_frames, 2);
    let mut latched = 0;
    for _ in 1..N1_SHALLOW_SETTLE_TICKS {
        latched += u32::from(n1_shallow_track(&mut s, tick(false, 3)));
    }
    assert_eq!(latched, 1);
    assert_eq!(s.target_frames, 4, "max floor 3 + 1");
}

#[test]
fn the_min_latency_guard_reports_and_stores_no_depth_1367() {
    let mut s = ShallowDepth::default();
    let mut latched = 0;
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        let t = ShallowTick {
            min_latency_box: true,
            ..tick(i == 0, 3)
        };
        latched += u32::from(n1_shallow_track(&mut s, t));
    }
    assert_eq!(latched, 1, "the capped latch is reported once");
    assert_eq!(
        s.target_frames, 0,
        "no depth applied: the rule never governs"
    );
    assert!(s.capped);
    // no auto re-measure churn: a capped source stays reported until a real relock.
    for _ in 0..3 * N1_SHALLOW_SETTLE_TICKS {
        let t = ShallowTick {
            min_latency_box: true,
            ..tick(false, 3)
        };
        assert!(!n1_shallow_track(&mut s, t));
    }
    assert!(!s.measuring);
    assert!(!n1_shallow_governs(s.target_frames, I30, 3, I30));
}

#[test]
fn a_deep_source_latches_the_pin_depth_1367() {
    let mut s = ShallowDepth::default();
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        // a post-ACQUIRE stall pushes the floor over the pin for part of the window.
        let t = ShallowTick {
            base_frames: 30,
            deep: true,
            ..tick(i == 0, if i < 10 { 33 } else { 1 })
        };
        n1_shallow_track(&mut s, t);
    }
    assert_eq!(s.target_frames, 31, "base + 1, never floor_max + 1");
}

#[test]
fn a_stall_on_the_latch_tick_cannot_decide_deep_1367() {
    // review round 2: a deep source whose stall is still running on the ONE latch tick (it
    // reads not-deep there) must still latch base + 1; the window's majority decides.
    let mut s = ShallowDepth::default();
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        let last = i + 1 == N1_SHALLOW_SETTLE_TICKS;
        let t = ShallowTick {
            base_frames: 30,
            deep: !last,
            ..tick(i == 0, if last { 33 } else { 1 })
        };
        n1_shallow_track(&mut s, t);
    }
    assert_eq!(s.target_frames, 31, "the majority was deep: base + 1");
    // and the other way round: one deep-looking tick in a shallow window stays shallow.
    let mut s = ShallowDepth::default();
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        let t = ShallowTick {
            deep: i + 1 == N1_SHALLOW_SETTLE_TICKS,
            ..tick(i == 0, 2)
        };
        n1_shallow_track(&mut s, t);
    }
    assert_eq!(s.target_frames, 3, "the majority was shallow: floor 2 + 1");
    assert!(!n1_shallow_window_deep(45, 90) && n1_shallow_window_deep(46, 90));
    assert!(!n1_shallow_window_deep(0, 0) && n1_shallow_window_deep(u32::MAX, u32::MAX));
}

#[test]
fn the_shallow_rule_governs_only_a_latched_non_deep_source_1367() {
    assert!(n1_shallow_governs(2, I30, 3, I30));
    assert!(!n1_shallow_governs(0, I30, 3, I30), "nothing latched");
    assert!(!n1_shallow_governs(2, I30, 3, 0));
    // a deep source keeps the pin rule.
    assert!(!n1_shallow_governs(31, I30, 987, I30));
    assert!(n1_shallow_governs(31, 29 * I30, 987, I30));
}

#[test]
fn the_shallow_shed_and_hold_keep_a_one_frame_dead_band_around_d_1367() {
    let w = 1_000_000_000_000u64;
    let d = 3u64;
    // shed: the last presented depth deeper than D.
    let shed = |age: u64, ticks: u64| n1_shallow_shed_due(w, w - age, I30, 3, I30, d, ticks);
    assert!(!shed(3 * I30, 100), "at D -> inert");
    assert!(!shed(4 * I30 - I30 / 2 - 1, 100));
    assert!(shed(4 * I30 - I30 / 2, 100), "half a frame over -> sheds");
    assert!(!shed(4 * I30, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
    assert!(!n1_shallow_shed_due(w, 0, I30, 3, I30, d, 100), "unlocked");
    assert!(
        !n1_shallow_shed_due(w, w - 5 * I30, I30, 3, I30, 0, 100),
        "no D"
    );
    // hold: the head would go on air shallower than D.
    let hold = |age: u64, ticks: u64| n1_shallow_hold_due(w, w - age, I30, 3, I30, d, ticks);
    assert!(hold(2 * I30, 100), "one short -> holds");
    assert!(!hold(3 * I30, 100), "at D -> inert");
    assert!(!hold(3 * I30 - I30 / 2, 100), "exactly half short -> inert");
    assert!(hold(3 * I30 - I30 / 2 - 1, 100));
    assert!(!hold(2 * I30, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
    // never both at one age.
    for step in 0..2_000u64 {
        let age = I30 + step * (4 * I30 / 2_000);
        assert!(!(shed(age, 100) && hold(age, 100)), "age {age}");
    }
    // a deep source is left to the pin rule.
    assert!(!n1_shallow_hold_due(
        w,
        w - 20 * I30,
        I30,
        987,
        I30,
        31,
        100
    ));
    assert!(!n1_shallow_shed_due(
        w,
        w - 40 * I30,
        I30,
        987,
        I30,
        31,
        100
    ));
}
