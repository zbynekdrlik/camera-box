//! #68 Task A/C: the painter must advance ids on the SAME wall-clock frame
//! boundaries the genlock decimator samples on, or the two 30 fps cadences drift
//! out of phase and the decimator skips ~13% of painted ids — which the new
//! endpoint-sequence check then reports as (spurious) "missing" generator ids.
//!
//! `next_wall_boundary_ns` is the pure pacing math (mirrors src/ndi.rs's genlock
//! boundary): given an absolute wall-clock ns and a frame period ns, the next
//! aligned boundary is the strictly-next multiple of the period. Sharing this
//! discipline with the decimator means every painted id lands on a tick the
//! decimator also consumes, preserving contiguity through the 60→30 decimation.

use camera_box::probe::painter::next_wall_boundary_ns;

#[test]
fn boundary_advances_to_next_multiple() {
    // period 33_333_333 ns (~30 fps). A time just past a boundary advances to the
    // next multiple, NOT the current one.
    let period = 33_333_333u64;
    let at = period * 100 + 5; // 5 ns past the 100th boundary
    assert_eq!(next_wall_boundary_ns(at, period), period * 101);
}

#[test]
fn exact_boundary_advances_to_the_following_one() {
    // On an exact boundary, the NEXT boundary is one period later (strict next, so
    // the painter never busy-loops emitting two ids at the same instant).
    let period = 33_333_333u64;
    let at = period * 50;
    assert_eq!(next_wall_boundary_ns(at, period), period * 51);
}

#[test]
fn just_before_boundary_lands_on_it() {
    let period = 33_333_333u64;
    let at = period * 7 - 1;
    assert_eq!(next_wall_boundary_ns(at, period), period * 7);
}

#[test]
fn zero_period_is_guarded_no_panic() {
    // A 0 period (fps 0) must not divide-by-zero; it returns `now` (zero wait).
    assert_eq!(next_wall_boundary_ns(123_456, 0), 123_456);
}

#[test]
fn distinct_boundaries_are_one_period_apart() {
    // Consecutive boundaries differ by exactly one period — the cadence the
    // decimator expects. Kills a mutant that adds 0 or 2 periods.
    let period = 16_666_667u64; // ~60 fps
    let b1 = next_wall_boundary_ns(period * 3 + 10, period);
    let b2 = next_wall_boundary_ns(b1, period);
    assert_eq!(b2 - b1, period);
}
