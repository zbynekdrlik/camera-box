//! Issue 1367 (ROZHODNUTÉ 5842640404) — the unit tests of the shallow latch's receive-time
//! arrival-lag budget (`n1_shallow_latch_floor_frames` and the tracker reading it for the
//! histogram only). A `#[path]` child of `genlock_n1_depth_tests.rs`, split out to keep that file
//! under the ~1000-line budget.

use super::*;

#[test]
fn the_latch_floor_is_the_budgeted_receive_lag_rounded_up_1367() {
    let b = GENLOCK_N2_JITTER_BUDGET_NS;
    // one frame while lag + budget stays within the first frame edge, the next one past it.
    assert_eq!(n1_shallow_latch_floor_frames(0, I30), 1);
    assert_eq!(n1_shallow_latch_floor_frames(8_000_000, I30), 1);
    assert_eq!(n1_shallow_latch_floor_frames(I30 - b, I30), 1);
    assert_eq!(n1_shallow_latch_floor_frames(I30 - b + 1, I30), 2);
    // the live song-start case: idle 25 ms is within 15 ms under the edge -> two frames.
    assert_eq!(n1_shallow_latch_floor_frames(25_000_000, I30), 2);
    assert_eq!(n1_shallow_latch_floor_frames(60_000_000, I30), 3);
    // the interval is a parameter (a 60p canvas), a degenerate interval reads 0, no overflow.
    assert_eq!(n1_shallow_latch_floor_frames(8_000_000, I60), 2);
    assert_eq!(n1_shallow_latch_floor_frames(25_000_000, 0), 0);
    assert_eq!(
        n1_shallow_latch_floor_frames(u64::MAX, I30),
        u64::MAX.div_ceil(I30)
    );
}

#[test]
fn the_latch_reads_the_budgeted_floor_and_the_rise_watch_the_raw_one_1367() {
    // idle lag 25 ms: the raw tick floor is 1 frame, the budgeted latch floor 2 -> D 3 at once.
    let idle = |relock: bool| ShallowTick {
        latch_floor_frames: n1_shallow_latch_floor_frames(25_000_000, I30),
        ..tick(relock, 1)
    };
    let mut s = ShallowDepth::default();
    let mut latched = 0;
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        latched += u32::from(n1_shallow_track(&mut s, idle(i == 0)));
    }
    assert_eq!((latched, s.target_frames), (1, 3), "the budget latched D 3");
    assert_eq!(s.floor_max_frames, 1, "floor_max stays the RAW floor");
    // content lag 36 ms (+11 ms send cost): the raw floor becomes 2, still under D, so three whole
    // windows never re-measure.
    for _ in 0..3 * N1_SHALLOW_SETTLE_TICKS {
        let t = ShallowTick {
            latch_floor_frames: n1_shallow_latch_floor_frames(36_000_000, I30),
            ..tick(false, 2)
        };
        assert!(!n1_shallow_track(&mut s, t));
        assert!(!s.measuring, "a rise inside the budget re-measured");
    }
    assert_eq!(s.target_frames, 3);
    // a genuine rise past the budget (70 ms: raw floor 3 = D) still re-measures, and the new
    // latch reads the budgeted floor (85 ms -> 3 frames) -> D 4, exactly the base + 3 clamp.
    let rise = ShallowTick {
        latch_floor_frames: n1_shallow_latch_floor_frames(70_000_000, I30),
        ..tick(false, 3)
    };
    let mut latched = 0;
    for _ in 0..2 * N1_SHALLOW_SETTLE_TICKS {
        latched += u32::from(n1_shallow_track(&mut s, rise));
    }
    assert_eq!(latched, 1, "one re-measure, one latch");
    assert_eq!((s.target_frames, s.capped), (4, false));
}
