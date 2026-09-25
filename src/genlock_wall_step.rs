//! Issue 1372 — the ONE wall-step detector of the genlock render tick, and the one-tick re-grid it
//! decides.
//!
//! ## Why this module exists
//!
//! The render tick is slaved to the WALL clock: every deadline is the next per-second grid point
//! of the wall clock, mapped into the monotonic (media) sleep timebase through the LIVE
//! `wall − mono` offset (`genlock_next_deadline`, obs-video.c). The per-tick correction against the
//! stock deadline (`cur_time + interval`) is clamped to [`MAX_SLEW_NS`] (2 ms) so scheduling
//! jitter and slow corrections never jerk the tick.
//!
//! A coordinated dantesync fleet DATE STEP (dantesync 1.9.0: NTP steps the date, the Dante tick
//! sets the rate) moves the wall clock by up to ~50 ms at once while the media clock
//! (`os_gettime_ns`, issue 1372 part A: it follows the dantesync RATE, never a step) stays
//! continuous. On the first live step (−51 ms, 25.9.2026 23:17:07 UTC) the tick then SLEWED back
//! to the stepped wall grid at 2 ms per tick — ~9 ticks off phase at 30 fps — and the Windows NDI
//! sender, which stamps the floor of the wall clock at emit (`genlock_floor_boundary_100ns`,
//! DistroAV ndi-output.cpp), stamped off-phase frames the whole time (the strih-lx `CG-obs`
//! underruns/relocks).
//!
//! This module decides the step and the re-grid:
//!
//! - [`wall_offset_ns`] reads `wall − mono` through a BRACKETED read (mono, wall, mono): the
//!   offset is taken against the midpoint of the two monotonic reads, and a read whose bracket is
//!   wider than [`READ_MAX_NS`] (the thread was preempted between the reads) is rejected, so a
//!   preemption can never look like a step.
//! - [`WallStepState::observe`] compares the offset with the previous tick's: a jump beyond
//!   [`WALL_STEP_MIN_NS`] is a wall STEP. The media clock follows the wall RATE, so between steps
//!   the offset is flat (a raw-QPC fallback drifts it by < 1 µs per tick) and nothing else trips it.
//! - [`deadline_ns`] returns the wall-grid target UNCLAMPED on a step — the tick lands on the new
//!   grid in ONE tick, and so do the stamps the sender floors at emit — and the pre-issue-1372
//!   2 ms clamp otherwise.
//!
//! The C twin is `vendor/obs-studio/libobs/obs-genlock-wall-step.h` (stdint only);
//! `tests/genlock_wall_step_parity_1372.rs` compiles it and requires byte-identical results.
//! The two-clock bench that replays the logged step is the test-only child
//! `crate::genlock_wall_step_bench`.
//!
//! Pure `std`, no `crate::` imports — standalone-rustc Tier-0 testable.

/// The per-tick slew clamp of the render tick, ns (obs-video.c `GENLOCK_MAX_SLEW_NS`).
pub const MAX_SLEW_NS: i64 = 2_000_000;

/// A `wall − mono` jump beyond this between two ticks is a wall STEP, ns. Equal to the slew
/// clamp: a smaller step is absorbed in one tick by the clamp already, so only a step the clamp
/// would spread over several ticks needs the re-grid.
pub const WALL_STEP_MIN_NS: i64 = MAX_SLEW_NS;

/// The widest accepted (mono, wall, mono) bracket, ns. A wider one means the reading thread was
/// preempted between the reads, and its offset is not trusted.
pub const READ_MAX_NS: u64 = 100_000;

/// `wall − mono` for one bracketed read: `wall` minus the midpoint of the two monotonic reads that
/// surround it. `None` when the bracket is wider than [`READ_MAX_NS`] or runs backwards. The
/// subtraction wraps like the C `int64_t` cast (wall in epoch ns, mono in ns since boot: the
/// difference fits an `i64` by orders of magnitude).
///
/// Mirror of the C `genlock_wall_offset_ns()`.
pub fn wall_offset_ns(mono_before: u64, wall: u64, mono_after: u64) -> Option<i64> {
    if mono_after < mono_before || mono_after - mono_before > READ_MAX_NS {
        return None;
    }
    let mid = mono_before + (mono_after - mono_before) / 2;
    Some(wall.wrapping_sub(mid) as i64)
}

/// The detector state: the last trusted offset and a running step count (telemetry).
///
/// Mirror of the C `struct genlock_wall_step_state` (`have` / `offset_ns` / `steps`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WallStepState {
    have: bool,
    offset_ns: i64,
    steps: u64,
}

impl WallStepState {
    /// A detector with no previous reading.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one bracketed read; returns the wall STEP in ns (`0` = none). The first trusted read
    /// only seeds the detector; an untrusted read (see [`wall_offset_ns`]) decides nothing and
    /// keeps the previous offset. Every trusted read becomes the new reference, so a slow drift of
    /// the offset is never summed into a false step.
    ///
    /// Mirror of the C `genlock_wall_step_observe()`.
    pub fn observe(&mut self, mono_before: u64, wall: u64, mono_after: u64) -> i64 {
        let Some(offset) = wall_offset_ns(mono_before, wall, mono_after) else {
            return 0;
        };
        if !self.have {
            self.have = true;
            self.offset_ns = offset;
            return 0;
        }
        self.offset_ns = offset;
        0
    }

    /// How many wall steps this detector has seen.
    pub fn steps(&self) -> u64 {
        self.steps
    }
}

/// The render-tick deadline: `target` (the next wall-grid point mapped into the monotonic
/// timebase) against `stock` (`cur_time + interval`). On a detected wall step (`regrid`) the
/// target is taken as-is — the tick re-grids in ONE tick. Otherwise the correction is clamped to
/// ±[`MAX_SLEW_NS`], the pre-issue-1372 behaviour, byte-identical.
///
/// Mirror of the C `genlock_wall_step_deadline_ns()`.
pub fn deadline_ns(target: u64, stock: u64, regrid: bool) -> u64 {
    let _ = regrid;
    let corr = target.wrapping_sub(stock) as i64;
    if corr > MAX_SLEW_NS {
        stock.wrapping_add(MAX_SLEW_NS as u64)
    } else if corr < -MAX_SLEW_NS {
        stock.wrapping_sub(MAX_SLEW_NS as u64)
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALL0: u64 = 1_790_378_227_000_000_000; // the logged step instant (epoch ns)
    const MONO0: u64 = 123_456_789_000_000; // ~34 h since boot

    #[test]
    fn offset_is_wall_minus_the_bracket_midpoint_1372() {
        assert_eq!(
            wall_offset_ns(MONO0, WALL0, MONO0 + 2_000),
            Some((WALL0 - (MONO0 + 1_000)) as i64)
        );
        assert_eq!(
            wall_offset_ns(MONO0, WALL0, MONO0 + READ_MAX_NS),
            Some((WALL0 - (MONO0 + READ_MAX_NS / 2)) as i64)
        );
    }

    #[test]
    fn a_preempted_or_backward_bracket_is_not_trusted_1372() {
        assert_eq!(wall_offset_ns(MONO0, WALL0, MONO0 + READ_MAX_NS + 1), None);
        assert_eq!(wall_offset_ns(MONO0 + 10, WALL0, MONO0), None);
    }

    #[test]
    fn the_logged_step_is_detected_once_with_its_size_1372() {
        let mut s = WallStepState::new();
        let interval = 33_333_333_u64;
        let mut mono = MONO0;
        let mut wall = WALL0 - 10 * interval;
        let mut steps = Vec::new();
        for k in 0..20 {
            if k == 10 {
                wall -= 51_039_000; // the announced −51.039 ms date step
            }
            steps.push(s.observe(mono, wall, mono + 1_500));
            mono += interval;
            wall += interval;
        }
        assert_eq!(steps.iter().filter(|&&x| x != 0).count(), 1);
        assert_eq!(steps[10], -51_039_000);
        assert_eq!(s.steps(), 1);
    }

    #[test]
    fn the_first_read_seeds_and_small_moves_never_step_1372() {
        let mut s = WallStepState::new();
        assert_eq!(s.observe(MONO0, WALL0, MONO0 + 100), 0);
        // a routine dantesync phase step (−146 µs, live win-resolume) and read jitter
        assert_eq!(
            s.observe(MONO0 + 1_000, WALL0 + 1_000 - 146_000, MONO0 + 1_100),
            0
        );
        // exactly the threshold is not a step; one ns more is
        assert_eq!(
            s.observe(
                MONO0 + 2_000,
                WALL0 + 2_000 - 146_000 + 2_000_000,
                MONO0 + 2_100
            ),
            0
        );
        assert_eq!(
            s.observe(
                MONO0 + 3_000,
                WALL0 + 3_000 - 146_000 + 2_000_000 + 2_000_001,
                MONO0 + 3_100
            ),
            2_000_001
        );
    }

    #[test]
    fn an_untrusted_read_neither_steps_nor_moves_the_reference_1372() {
        let mut s = WallStepState::new();
        s.observe(MONO0, WALL0, MONO0 + 100);
        // preempted: the wall read looks 5 ms off, but the bracket is 5 ms wide
        assert_eq!(s.observe(MONO0 + 10, WALL0 + 10, MONO0 + 5_000_010), 0);
        // the next good read compares against the ORIGINAL reference: no step
        assert_eq!(s.observe(MONO0 + 20, WALL0 + 20, MONO0 + 120), 0);
        assert_eq!(s.steps(), 0);
    }

    #[test]
    fn a_slow_offset_drift_is_never_summed_into_a_step_1372() {
        // a raw-QPC fallback: 1000 ppm (the clamp) of drift = 33 µs per 30 fps tick, for an hour
        let mut s = WallStepState::new();
        let mut mono = MONO0;
        let mut wall = WALL0;
        for _ in 0..108_000 {
            assert_eq!(s.observe(mono, wall, mono + 1_000), 0);
            mono += 33_333_333;
            wall += 33_333_333 + 33_333;
        }
        assert_eq!(s.steps(), 0);
    }

    #[test]
    fn deadline_clamps_without_a_step_and_regrids_on_one_1372() {
        let stock = MONO0;
        // within the clamp: the target itself
        assert_eq!(
            deadline_ns(stock + 1_500_000, stock, false),
            stock + 1_500_000
        );
        assert_eq!(
            deadline_ns(stock - 1_500_000, stock, false),
            stock - 1_500_000
        );
        // beyond: clamped to ±2 ms (the pre-issue-1372 behaviour)
        assert_eq!(
            deadline_ns(stock + 15_700_000, stock, false),
            stock + 2_000_000
        );
        assert_eq!(
            deadline_ns(stock - 17_600_000, stock, false),
            stock - 2_000_000
        );
        // a detected step: the target, in one tick
        assert_eq!(
            deadline_ns(stock + 15_700_000, stock, true),
            stock + 15_700_000
        );
        assert_eq!(
            deadline_ns(stock - 17_600_000, stock, true),
            stock - 17_600_000
        );
    }
}
