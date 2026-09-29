//! Issue 1367 slice D2 — the STAMP-DRIVEN emit decision.
//!
//! While the capture phase tracker (`crate::capture_phase`) drives a stream, every frame carries
//! the grid slot it is stamped in, and [`crate::dupe_decimation::DecimationGate`] decides on that
//! slot instead of on the poll wall clock after the dequeue: the slot of the last EMITTED frame vs
//! this frame's slot, nothing else. So a frame drained late from a backlogged queue still emits (its
//! slot is new), and the only sheds are real duplicate slots.
//!
//! Dependency direction: a leaf like `shed` / `signature` (only `shed`'s repeat cap and the
//! `genlock_pacing` / `genlock_grid` math); `gate` applies it.

use super::shed::STARVATION_REPEAT_MAX;
use crate::genlock_grid::grid_steps_between;
use crate::genlock_pacing::GENLOCK_MAX_CATCHUP_INTERVALS;

/// What the gate does with a stamp-driven frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampSlotAction {
    /// No emitted slot is known yet: emit, no fill.
    Latch,
    /// One slot after the last emitted frame: emit.
    Advance,
    /// The same slot as the last emitted frame: drop (a real duplicate stamp, one per fast
    /// crossing).
    Duplicate,
    /// `missing` slots between the last emitted frame and this one, within the catch-up bound:
    /// emit, preceded by `repeats` starvation repeats stamped at the missing slots (one per slow
    /// crossing, one per dropped USB frame). `repeats < missing` only when the consecutive
    /// repeat budget ran out (a half-rate leg must still look down).
    Gap { missing: u64, repeats: u64 },
    /// A forward jump beyond the catch-up bound (a clock step, a long device gap) or any backward
    /// move: emit and re-latch, no fill. `skipped` is the forward jump's missing slots (0 backward).
    Resync { skipped: u64 },
}

/// Decide on a stamp-driven frame in slot `slot_ns` (a grid point) given the slot of the last
/// emitted frame (`0` = none yet) and how many more consecutive starvation repeats may be emitted
/// (`STARVATION_REPEAT_MAX` minus the current run). Pure; `interval_ns > 0` is the caller's guard.
pub fn stamp_slot_action(
    last_emitted_slot_ns: u64,
    slot_ns: u64,
    interval_ns: u64,
    repeat_budget: u64,
) -> StampSlotAction {
    if last_emitted_slot_ns == 0 {
        return StampSlotAction::Latch;
    }
    if slot_ns == last_emitted_slot_ns {
        return StampSlotAction::Duplicate;
    }
    if slot_ns < last_emitted_slot_ns {
        return StampSlotAction::Resync { skipped: 0 };
    }
    let missing = grid_steps_between(last_emitted_slot_ns, slot_ns, interval_ns).saturating_sub(1);
    if missing == 0 {
        StampSlotAction::Advance
    } else if missing <= GENLOCK_MAX_CATCHUP_INTERVALS {
        StampSlotAction::Gap {
            missing,
            repeats: missing.min(repeat_budget.min(STARVATION_REPEAT_MAX)),
        }
    } else {
        StampSlotAction::Resync { skipped: missing }
    }
}

#[cfg(test)]
mod tests;
