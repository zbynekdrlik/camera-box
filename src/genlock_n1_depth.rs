//! issue 1367 — the N==1 PIN-DERIVED DEPTH. The Tier-0 authority; the C `genlock_n1_*` helpers
//! (obs-source.c) mirror this in lock-step (tests/genlock_relock_selection_parity.rs).
//!
//! WHY. A deep N==1 input (the stream `NDI 2ME PGM`, pin 987) has TWO absorbing depths. A strih OBS
//! restart empties the FIFO, the release keeps its locked boundary, GAP-RESYNCs onto the first
//! post-restart frame at `ceil(pin / interval)` frames (30), and then presents each startup-stall
//! DUPLICATE stamp one tick later on the STEADY path (+1 frame each). The #859 drain only fires
//! above `ceil + 2` (32), so 30 / 31 / 32 were all absorbing: live, four restarts landed on 32, 31,
//! 32, 31 frames — a whole-frame (~33 ms) A/V step per restart at a constant pin.
//!
//! THE RULE. Settle to `target = base + 1` frames, where `base = ceil((pin − 1 µs) / interval)`
//! is the resync depth and `+1` is the depth the healthy sender gap/dup tail produces anyway (a
//! gap+dup pair at `base` deepens by one; at `base + 1` or deeper it is neutral). Deeper than the
//! target → shed ONE frame ([`n1_shed_due`]); shallower → HOLD one tick ([`should_hold_n1_phase`]).
//! Both share the #859 drain throttle, and together they leave a one-frame dead-band around the
//! target.
//!
//! What carries the design, found in the issue-1355 bench (`genlock_grid_bench.rs`):
//! - DEPTH IS THE PRESENTED AGE AT THE TICK'S SCHEDULED INSTANT, never the queue length and never
//!   the processing wall. A render tick that runs late already holds the NEXT frame, so the queue
//!   reads one frame deep at the correct state (lowering the drain hysteresis instead churned 10
//!   sheds/h, 17–21 flips/h). And a late tick is not a lost slot: `video_sleep` (obs-video.c)
//!   counts one frame for any overrun below two intervals and schedules the next slot from the
//!   previous TARGET, so the next tick is a CATCH-UP. The presented age read at the processing
//!   wall of a late tick is one frame too deep for the whole overrun (review rounds 1-2: a margin
//!   on the processing wall only moved the misfire band). Read at the scheduled instant
//!   ([`n1_tick_wall_ns`] — `video_time`, the `sys_time` `async_tick` passes down, mapped into wall
//!   time) the age carries no lateness at all. An overrun of two intervals or more DOES skip slots
//!   (`count >= 2`); the next tick's scheduled instant then jumps with them, and the depth it reads
//!   is the genuinely deeper one the shed repays.
//! - BOTH HALVES READ THE ROUNDED DEPTH ([`n1_depth_frames`], `(age + interval/2) / interval`).
//!   Presented stamps and scheduled ticks sit on the same per-second grid (issue 1355), so the
//!   scheduled age is a whole number of frames plus the stamp's sub-100-ns floor and the tick's
//!   schedule phase (a tick after a wall-clock step sits off the grid until `GENLOCK_MAX_SLEW_NS`
//!   pulls it back, 2 ms per tick). Rounding keeps the READ exact for up to half a frame of that
//!   phase, and a shed needs the rounded depth at `target + 1` while a hold needs it at
//!   `target − 1`: the two decisions never share an edge (a shared edge limit-cycled hold/shed
//!   ~1100 each per hour in the bench).
//! - ONLY ON THE GRID ([`n1_tick_is_on_grid`], review round 3). A wall-clock STEP on the box
//!   leaves the scheduled ticks off the grid by the step, and `genlock_next_deadline` pulls them
//!   back only `GENLOCK_MAX_SLEW_NS` (2 ms) per tick. Read there, a settled conveyor reads one
//!   frame deep after a forward step of more than half a frame (a shed) and one frame shallow once
//!   back on the grid (a hold): a skip plus a duplicate where the boundary-keyed conveyor did
//!   nothing. Both halves therefore act only while the scheduled tick is within
//!   [`N1_ON_GRID_NS`] of a per-second grid point and defer otherwise. A normal or caught-up late
//!   tick is scheduled on its slot or at most `GENLOCK_MAX_SLEW_NS` after it (a tick that overran
//!   the next grid point by under 2 ms sleeps to that slot plus the clamped 2 ms), so it is on the
//!   grid; at that exact +2 ms edge the wall/monotonic read order can defer one tick. The same
//!   condition removes the early-phase case beyond 2 ms: an early tick past the pin's headroom to
//!   the next frame edge moves the release deadline a frame, so a sender gap/dup would cost a
//!   hold/shed pair. At pins whose headroom is under 2 ms (999 ms, 1000 ms) an early tick inside
//!   the window still can — only on the last one or two ticks of a backward-step slew.
//! - THE 1 µs PIN TOLERANCE. The integer interval (33_333_333) is ~1/3 ns short of a real frame,
//!   so a pin that IS a whole number of frames (100 ms) would count one frame too many and put the
//!   target on the drain's own edge (23 flips/h in the bench). A sub-microsecond excess is not a
//!   real extra frame.
//! - DEEP SOURCES ONLY. The rule acts only when `floor_frames + N1_DEEP_MARGIN_FRAMES <= base`,
//!   where the ARRIVAL FLOOR is `wall − newest queued stamp` at the processing wall (the
//!   achievable-floor reference of #1049). A shallow source (the `cg` feeds, the imag cameras at
//!   3 ms) has a depth decided by its ARRIVAL, not its pin, so a pin-derived target would fight the
//!   floor; it stays byte-identical to before. The two-frame margin keeps a late tick's larger
//!   floor (one frame at most) from ever admitting a floor-dominated source.
//!
//! N>=2 is untouched: `genlock_backlog::should_converge_phase` keeps its `source_multiple < 2`
//! early return byte for byte; the SOURCE wrapper routes an N==1 tick here instead (the C
//! `genlock_should_converge_phase`, the probe `ReleaseCadence::should_converge_phase`, the
//! issue-1355 bench), and the release port's N==1 STEADY branch calls [`should_hold_n1_phase`]
//! (the C `genlock_should_hold_n1_phase`). A separate module because `genlock_backlog.rs` is already
//! past the ~1000-line budget. Pure `std` + one crate constant — Tier-0 verifiable.

use crate::genlock_backlog::DRAIN_MIN_TICK_INTERVAL;

/// issue 1367 — a pin within this many ns ABOVE a whole number of frame intervals counts as that
/// whole number (the integer interval is fractionally short of a real frame). Mirror of the C
/// `GENLOCK_N1_PIN_FRAME_TOLERANCE_NS`.
pub const N1_PIN_FRAME_TOLERANCE_NS: u64 = 1_000;

/// issue 1367 — the pin-derived depth must exceed the arrival floor by at least this many frames
/// for the N==1 rule to act (a DEEP source). Mirror of the C `GENLOCK_N1_DEEP_MARGIN_FRAMES`.
pub const N1_DEEP_MARGIN_FRAMES: u64 = 2;

/// issue 1367 (review round 3) — the N==1 rule acts only while the render tick's scheduled
/// instant is within this many ns of a grid point: the render tick's own per-tick slew clamp
/// (`GENLOCK_MAX_SLEW_NS`, obs-video.c), so a tick still slewing back after a wall-clock step is
/// off the grid and the first on-grid tick is at most one slew step away. Mirror of the C
/// `GENLOCK_N1_ON_GRID_NS`.
pub const N1_ON_GRID_NS: u64 = 2_000_000;

/// issue 1367 — the WALL instant the current render tick was SCHEDULED for: the processing wall
/// minus how far the processing runs behind the tick's scheduled monotonic instant
/// (`wall_now − (mono_now − scheduled_mono)`, saturating). The C passes `os_gettime_ns()` and
/// `obs->video.video_time`; a tick that has not yet reached its schedule (never, in OBS) reads the
/// processing wall. Mirror of the C `genlock_n1_tick_wall_ns`.
pub fn n1_tick_wall_ns(wall_now_ns: u64, mono_now_ns: u64, scheduled_mono_ns: u64) -> u64 {
    wall_now_ns.saturating_sub(mono_now_ns.saturating_sub(scheduled_mono_ns))
}

/// issue 1367 (review round 3) — is the scheduled tick `tick_wall_ns` within [`N1_ON_GRID_NS`]
/// of the grid point `grid_floor_ns`, where `grid_floor_ns` is the grid floor of
/// `tick_wall_ns + N1_ON_GRID_NS` (the only grid point that can be that close)? Pure; the caller
/// supplies the floor of its own grid. Mirror of the C `genlock_n1_tick_on_grid`.
pub fn n1_tick_on_grid(tick_wall_ns: u64, grid_floor_ns: u64) -> bool {
    let shifted = tick_wall_ns.saturating_add(N1_ON_GRID_NS);
    shifted >= grid_floor_ns && shifted - grid_floor_ns <= 2 * N1_ON_GRID_NS
}

/// issue 1367 (review round 3) — [`n1_tick_on_grid`] on the ONE per-second genlock grid the
/// render tick and the senders use ([`crate::genlock_grid::grid_floor_ns`]). Mirror of the C
/// `genlock_n1_tick_is_on_grid`.
pub fn n1_tick_is_on_grid(tick_wall_ns: u64, interval_ns: u64) -> bool {
    n1_tick_on_grid(
        tick_wall_ns,
        crate::genlock_grid::grid_floor_ns(tick_wall_ns.saturating_add(N1_ON_GRID_NS), interval_ns),
    )
}

/// issue 1367 — the depth, in frames, a deep N==1 source lands on after a GAP RESYNC:
/// `ceil((pin − N1_PIN_FRAME_TOLERANCE_NS) / interval)`. `interval_ns == 0` returns 0.
pub fn n1_base_frames(latency_ms: u32, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    let reserve_ns = (latency_ms as u64).saturating_mul(1_000_000);
    reserve_ns
        .saturating_sub(N1_PIN_FRAME_TOLERANCE_NS)
        .div_ceil(interval_ns)
}

/// issue 1367 — the depth, in frames, a deep N==1 source settles on: [`n1_base_frames`] + 1.
pub fn n1_target_frames(latency_ms: u32, interval_ns: u64) -> u64 {
    n1_base_frames(latency_ms, interval_ns).saturating_add(1)
}

/// issue 1367 — the presented depth, ROUNDED to whole frames, of a frame stamped `stamp_ns` at the
/// tick's scheduled instant `tick_wall_ns`: `(tick_wall − stamp + interval/2) / interval`.
/// `interval_ns == 0` returns 0; a stamp ahead of the tick reads age 0.
pub fn n1_depth_frames(tick_wall_ns: u64, stamp_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    tick_wall_ns
        .saturating_sub(stamp_ns)
        .saturating_add(interval_ns / 2)
        / interval_ns
}

/// issue 1367 — is this N==1 source DEEP (its pin, not its arrival, decides its depth)? True when
/// the arrival floor (`wall − newest queued stamp`, ns) in whole frames plus
/// [`N1_DEEP_MARGIN_FRAMES`] is at most [`n1_base_frames`]. `interval_ns == 0` is never deep.
pub fn n1_is_deep_source(arrival_floor_ns: u64, latency_ms: u32, interval_ns: u64) -> bool {
    if interval_ns == 0 {
        return false;
    }
    (arrival_floor_ns / interval_ns).saturating_add(N1_DEEP_MARGIN_FRAMES)
        <= n1_base_frames(latency_ms, interval_ns)
}

/// issue 1367 — the N==1 SHED half (the caller gates it on [`n1_tick_is_on_grid`]): shed one
/// frame when the last presented depth (read from the
/// locked boundary, `boundary == last presented + interval`, at this tick's scheduled instant) is
/// deeper than [`n1_target_frames`] on a deep source, throttled by the shared #859 counter. An
/// unlocked boundary (0) or a degenerate interval never sheds. Mirror of the C
/// `genlock_n1_shed_due`.
pub fn n1_shed_due(
    tick_wall_ns: u64,
    locked_boundary_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    ticks_since_last_drain: u64,
) -> bool {
    interval_ns != 0
        && locked_boundary_ns != 0
        && n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, locked_boundary_ns, interval_ns)
            > n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 — the N==1 HOLD half (the caller gates it on [`n1_tick_is_on_grid`]): on a STEADY
/// tick, HOLD one tick (a deliberate repeat, the
/// conveyor one frame deeper next tick) when presenting the queue head now would put a deep N==1
/// source SHALLOWER than [`n1_target_frames`] (both read at the tick's scheduled instant). Reads
/// the HEAD's age, not the boundary's: a startup DUPLICATE sits one interval below the boundary and
/// already deepens the conveyor when presented, so a boundary-based read would hold in front of it
/// and overshoot. Shares the #859 throttle (the caller resets it on a hold). Inert for
/// `source_multiple >= 2` (the N>=2 conveyor has its own #1049 shed), for a shallow source, and for
/// a degenerate interval. Mirror of the C `genlock_n1_hold_due`.
pub fn should_hold_n1_phase(
    tick_wall_ns: u64,
    head_stamp_ns: u64,
    arrival_floor_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    source_multiple: u32,
    ticks_since_last_drain: u64,
) -> bool {
    source_multiple < 2
        && interval_ns != 0
        && n1_is_deep_source(arrival_floor_ns, latency_ms, interval_ns)
        && n1_depth_frames(tick_wall_ns, head_stamp_ns, interval_ns)
            < n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

#[cfg(test)]
mod tests {
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
        let hold = |age: u64, n: u32, ticks: u64| {
            should_hold_n1_phase(w, w - age, I30, 987, I30, n, ticks)
        };
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
}
