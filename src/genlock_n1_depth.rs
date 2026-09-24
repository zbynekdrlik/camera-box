//! issue 1367 — the N==1 PIN-DERIVED DEPTH. The Tier-0 authority; the C `genlock_n1_*` helpers +
//! `genlock_n1_hold_due` and the N==1 branch of `genlock_phase_converge_due` (obs-source.c) mirror
//! this in lock-step (tests/genlock_relock_selection_parity.rs).
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
//! target → shed ONE frame (the lifted #1049 converge shed); shallower → HOLD one tick
//! ([`should_hold_n1_phase`]). Both share the #859 drain throttle.
//!
//! Three details carry the design, each found in the issue-1355 bench (`genlock_grid_bench.rs`):
//! - DEPTH IS THE PRESENTED AGE, not the queue length. A render tick running late by more than the
//!   ~21.7 ms sender→receiver skew already holds the NEXT frame, so the queue reads one frame deep
//!   at the correct state. Reducing the drain hysteresis to one frame therefore churned (10 sheds/h,
//!   17–21 flips/h between 30 and 31, against 0). The SHED reads
//!   `(age + N1_TICK_EARLY_MARGIN_NS) / interval`: immune to a tick up to one interval (minus the
//!   margin) late, and tolerant of a wake a hair early (wall-vs-monotonic rate error over one
//!   sleep, microseconds). The margin is kept at 100 us because the misfire band is exactly
//!   `[interval − margin, interval)` of lateness: at 2 ms a 31.3–33.3 ms late tick that does not
//!   yet skip a slot read the settled conveyor one frame deep and shed (review round 1). A tick
//!   LATER than a whole interval skips its slot (`genlock_next_deadline`), the conveyor really is
//!   one frame deeper then, and the shed correctly repays it. A tick further EARLY than the
//!   margin reads one frame shallow, which only defers a shed to the next on-grid tick — the ±2 ms
//!   `GENLOCK_MAX_SLEW_NS` clamp bounds the per-tick slew, not the phase.
//! - THE 1 µs PIN TOLERANCE. The integer interval (33_333_333) is ~1/3 ns short of a real frame,
//!   so a pin that IS a whole number of frames (100 ms) would count one frame too many and put the
//!   target on the drain's own edge (23 flips/h in the bench). A sub-microsecond excess is not a
//!   real extra frame.
//! - DEEP SOURCES ONLY. The rule acts only when `floor_frames + N1_DEEP_MARGIN_FRAMES <= base`,
//!   where `floor = wall − newest queued stamp` (the achievable-floor reference of #1049). A shallow
//!   source (the `cg` feeds, the imag cameras at 3 ms) has a depth decided by its ARRIVAL, not its
//!   pin, so a pin-derived target would fight the floor; it stays byte-identical to before. The
//!   two-frame margin keeps a late-tick understatement of the floor (one frame at most) from ever
//!   admitting a floor-dominated source.
//! - THE HOLD ROUNDS, THE SHED DOES NOT. The shed reads the late-tolerant depth above; the hold
//!   reads the depth ROUNDED to the nearest frame ([`n1_rounded_depth_frames`]), so the two
//!   decisions sit half a frame apart. On one shared edge, a render tick whose phase sat on it read
//!   one frame shallow on one tick (hold) and one frame deep on the next (shed) — a hold/shed
//!   limit cycle, ~1100 each per hour on the bench's 1970-grid control.
//!
//! Split out of `genlock_backlog.rs` (already past the ~1000-line budget): `genlock_backlog::
//! should_converge_phase` delegates its N==1 branch here, the release port's STEADY branch calls
//! [`should_hold_n1_phase`] directly (the C `genlock_should_hold_n1_phase`, the probe
//! `ReleaseCadence`, the issue-1355 bench). Pure `std` + one crate constant — Tier-0 verifiable.

use crate::genlock_backlog::DRAIN_MIN_TICK_INTERVAL;

/// issue 1367 — a pin within this many ns ABOVE a whole number of frame intervals counts as that
/// whole number (the integer interval is fractionally short of a real frame). Mirror of the C
/// `GENLOCK_N1_PIN_FRAME_TOLERANCE_NS`.
pub const N1_PIN_FRAME_TOLERANCE_NS: u64 = 1_000;

/// issue 1367 — how early a render tick may wake before its grid point and still read (for the
/// SHED) the presented depth it is really on: a wall-vs-monotonic rate error over one sleep is
/// microseconds, so 100 us is ample. It is also the width of the misfire band at the other end —
/// a tick late by `interval − margin` or more reads one frame deep — so it must stay tiny (2 ms
/// misfired on 31.3–33.3 ms render hitches, review round 1). Mirror of the C
/// `GENLOCK_N1_TICK_EARLY_MARGIN_NS`.
pub const N1_TICK_EARLY_MARGIN_NS: u64 = 100_000;

/// issue 1367 — the pin-derived depth must exceed the achievable floor by at least this many frames
/// for the N==1 rule to act (a DEEP source). Mirror of the C `GENLOCK_N1_DEEP_MARGIN_FRAMES`.
pub const N1_DEEP_MARGIN_FRAMES: u64 = 2;

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

/// issue 1367 — the presented depth, in whole frames, of a frame stamped `stamp_ns` presented at
/// `wall_now_ns`: `(wall − stamp + N1_TICK_EARLY_MARGIN_NS) / interval`. Immune to a tick up to one
/// interval (minus the margin) late, tolerant of a tick up to the margin early. `interval_ns == 0`
/// returns 0; a stamp ahead of wall reads age 0.
pub fn n1_depth_frames(wall_now_ns: u64, stamp_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    wall_now_ns
        .saturating_sub(stamp_ns)
        .saturating_add(N1_TICK_EARLY_MARGIN_NS)
        / interval_ns
}

/// issue 1367 — is this N==1 source DEEP (its pin, not its arrival, decides its depth)? True when
/// the freshest queued frame's age in whole frames plus [`N1_DEEP_MARGIN_FRAMES`] is at most
/// [`n1_base_frames`]. `interval_ns == 0` is never deep.
pub fn n1_is_deep_source(
    wall_now_ns: u64,
    newest_stamp_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
) -> bool {
    if interval_ns == 0 {
        return false;
    }
    let floor_frames = wall_now_ns.saturating_sub(newest_stamp_ns) / interval_ns;
    floor_frames.saturating_add(N1_DEEP_MARGIN_FRAMES) <= n1_base_frames(latency_ms, interval_ns)
}

/// issue 1367 — the N==1 SHED half, the lifted branch of
/// [`crate::genlock_backlog::should_converge_phase`]: shed one frame
/// when the last presented depth (read from the locked boundary, `boundary == last presented +
/// interval`) is deeper than [`n1_target_frames`] on a deep source, throttled by the shared #859
/// counter. An unlocked boundary (0) or a degenerate interval never sheds.
pub fn n1_shed_due(
    wall_now_ns: u64,
    locked_boundary_ns: u64,
    newest_stamp_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    ticks_since_last_drain: u64,
) -> bool {
    interval_ns != 0
        && locked_boundary_ns != 0
        && n1_is_deep_source(wall_now_ns, newest_stamp_ns, latency_ms, interval_ns)
        && n1_depth_frames(wall_now_ns, locked_boundary_ns, interval_ns)
            > n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 — the N==1 HOLD half: on a STEADY tick, HOLD one tick (a deliberate repeat, the
/// conveyor one frame deeper next tick) when presenting the queue head now would put a deep N==1
/// source SHALLOWER than [`n1_target_frames`]. Reads the HEAD's age, not the boundary's: a startup
/// DUPLICATE sits one interval below the boundary and already deepens the conveyor when presented,
/// so a boundary-based read would hold in front of it and overshoot. Shares the #859 throttle (the
/// caller resets it on a hold). Inert for `source_multiple >= 2` (the N>=2 conveyor has its own
/// #1049 shed), for a shallow source, and for a degenerate interval. Mirror of the C
/// `genlock_n1_hold_due`.
pub fn should_hold_n1_phase(
    wall_now_ns: u64,
    head_stamp_ns: u64,
    newest_stamp_ns: u64,
    latency_ms: u32,
    interval_ns: u64,
    source_multiple: u32,
    ticks_since_last_drain: u64,
) -> bool {
    source_multiple < 2
        && interval_ns != 0
        && n1_is_deep_source(wall_now_ns, newest_stamp_ns, latency_ms, interval_ns)
        && n1_rounded_depth_frames(wall_now_ns, head_stamp_ns, interval_ns)
            < n1_target_frames(latency_ms, interval_ns)
        && ticks_since_last_drain >= DRAIN_MIN_TICK_INTERVAL
}

/// issue 1367 — the presented depth ROUNDED to the nearest frame: `(wall − stamp + interval/2) /
/// interval`. The HOLD reads this, the SHED reads [`n1_depth_frames`], so the two decisions sit
/// half a frame apart instead of on one shared edge. A stamp phase that lands exactly on a shared
/// edge (the sender's clock a couple of ms ahead of the receiver, or — in the bench — the 1970
/// grid's 2 ms date offset) otherwise read the same depth one frame shallow on one tick (HOLD) and
/// one frame deep on the next (SHED), a hold/shed limit cycle (~1100 each per hour in the bench).
/// Rounded, a HOLD needs the conveyor at least half a frame shallow; a SHED still fires only on a
/// whole extra frame (minus the 100 us early-tick margin), so a late tick never reads deeper.
pub fn n1_rounded_depth_frames(wall_now_ns: u64, stamp_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        return 0;
    }
    wall_now_ns
        .saturating_sub(stamp_ns)
        .saturating_add(interval_ns / 2)
        / interval_ns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genlock_backlog::should_converge_phase;

    const I30: u64 = 33_333_333; // ~30 Hz frame interval (ns)
    const I60: u64 = 16_666_667; // ~60 Hz frame interval (ns)

    // issue 1367 — the N==1 pin-derived depth, arithmetic edges (the faithful proof of each
    // threshold; the dynamic proof is the restart bench in genlock_grid_bench.rs).

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
    fn n1_depth_reads_are_immune_to_a_late_tick_and_half_a_frame_apart_1367() {
        let s = 1_000_000_000_000u64;
        // The SHED read: a tick up to one interval minus 100 us late still reads its true depth
        // (review round 1: at a 2 ms margin a 31.3-33.3 ms late tick read one frame deep).
        assert_eq!(n1_depth_frames(s + 31 * I30, s, I30), 31);
        assert_eq!(n1_depth_frames(s + 31 * I30 + 31_000_000, s, I30), 31);
        assert_eq!(n1_depth_frames(s + 31 * I30 + 33_000_000, s, I30), 31);
        assert_eq!(n1_depth_frames(s + 32 * I30 - 100_000, s, I30), 32);
        assert_eq!(n1_depth_frames(s + 32 * I30 - 100_001, s, I30), 31);
        // ... and a tick up to 100 us EARLY (wall-vs-monotonic rate error over one sleep) still
        // reads it too; a tick further early reads one frame shallow, which only defers a shed.
        assert_eq!(n1_depth_frames(s + 31 * I30 - 100_000, s, I30), 31);
        assert_eq!(n1_depth_frames(s + 31 * I30 - 100_001, s, I30), 30);
        // The HOLD read rounds: the two decisions sit half a frame apart.
        assert_eq!(n1_rounded_depth_frames(s + 31 * I30 - I30 / 2, s, I30), 31);
        assert_eq!(
            n1_rounded_depth_frames(s + 31 * I30 - I30 / 2 - 1, s, I30),
            30
        );
        assert_eq!(
            n1_rounded_depth_frames(s + 31 * I30 + 5_000_000, s, I30),
            31
        );
        // Degenerate inputs never divide by zero; a stamp ahead of wall reads age 0.
        assert_eq!(n1_depth_frames(s, s, 0), 0);
        assert_eq!(n1_rounded_depth_frames(s, s + I30, I30), 0);
    }

    #[test]
    fn n1_deep_source_guard_needs_two_frames_of_pin_over_the_arrival_floor_1367() {
        let w = 1_000_000_000_000u64;
        // pin 987: base 30. The freshest frame one interval old (the stream 2ME PGM) is deep.
        assert!(n1_is_deep_source(w, w - I30, 987, I30));
        // Edge: floor 28 frames + 2 == base 30 -> deep; floor 29 frames -> shallow.
        assert!(n1_is_deep_source(w, w - 28 * I30, 987, I30));
        assert!(!n1_is_deep_source(w, w - 29 * I30, 987, I30));
        // The 3 ms cg / imag case (base 1) is never deep, whatever its floor.
        assert!(!n1_is_deep_source(w, w, 3, I30));
        assert!(!n1_is_deep_source(w, w, 3, I60));
        assert!(!n1_is_deep_source(w, w - I30, 987, 0));
    }

    #[test]
    fn n1_shed_fires_only_a_whole_frame_past_the_target_on_a_deep_source_1367() {
        let w = 1_000_000_000_000u64;
        let newest = w - I30; // deep
        let shed =
            |age: u64, ticks: u64| should_converge_phase(w, w - age, newest, 987, I30, 1, ticks);
        assert!(!shed(31 * I30, 100), "at the target (31 frames) -> inert");
        assert!(
            !shed(31 * I30 + 33_000_000, 100),
            "a 33 ms late tick at the target -> inert"
        );
        assert!(
            shed(32 * I30 - 100_000, 100),
            "one frame over, 100 us early -> sheds"
        );
        assert!(
            !shed(32 * I30 - 100_001, 100),
            "one ns under the edge -> inert"
        );
        assert!(shed(33 * I30, 100), "two frames over -> sheds");
        assert!(!shed(32 * I30, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
        assert!(
            shed(32 * I30, DRAIN_MIN_TICK_INTERVAL),
            "throttle exactly met"
        );
        // Shallow source, unlocked boundary, degenerate interval: never.
        assert!(!should_converge_phase(
            w,
            w - 5 * I30,
            w - I30,
            3,
            I30,
            1,
            100
        ));
        assert!(!should_converge_phase(w, 0, newest, 987, I30, 1, 100));
        assert!(!should_converge_phase(
            w,
            w - 40 * I30,
            newest,
            987,
            0,
            1,
            100
        ));
        // source_multiple 0 is treated as N==1 (the C wrapper floors n at 1).
        assert!(should_converge_phase(
            w,
            w - 33 * I30,
            newest,
            987,
            I30,
            0,
            100
        ));
    }

    #[test]
    fn n1_hold_fires_only_half_a_frame_short_of_the_target_on_a_deep_source_1367() {
        let w = 1_000_000_000_000u64;
        let newest = w - I30;
        let hold = |age: u64, n: u32, ticks: u64| {
            should_hold_n1_phase(w, w - age, newest, 987, I30, n, ticks)
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
            !hold(31 * I30 - 3_000_000, 1, 100),
            "a 3 ms EARLY tick at the target -> inert"
        );
        assert!(
            hold(30 * I30 + 5_000_000, 1, 100),
            "a 5 ms late tick one short -> still holds"
        );
        assert!(!hold(30 * I30, 1, DRAIN_MIN_TICK_INTERVAL - 1), "throttled");
        assert!(hold(30 * I30, 0, 100), "source_multiple 0 is N==1");
        assert!(!hold(30 * I30, 2, 100), "an N>=2 source has its own shed");
        assert!(
            !should_hold_n1_phase(w, w - I30 / 3, w, 3, I30, 1, 100),
            "shallow never holds"
        );
        assert!(!should_hold_n1_phase(
            w,
            w - 30 * I30,
            newest,
            987,
            0,
            1,
            100
        ));
    }
}
