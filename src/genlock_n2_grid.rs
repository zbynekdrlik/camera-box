//! Issue 1367 slice D1 — the GRID-EXACT N>=2 genlock conveyor: which queued frame an N>=2 source
//! (a 60 fps camera into the 30 fps strih canvas) puts on air at a render tick.
//!
//! ## Why this module exists
//!
//! Before D1 the N>=2 release in `genlock_release_tick` (obs-source.c) ran a boundary conveyor:
//! every present re-anchored a LOCKED boundary to the presented stamp + one canvas interval, so
//! WHICH frame of each 60 fps pair went on air was whatever the lock had picked. The lock picked by
//! ARRIVAL (the ACQUIRE / backlog relock took the frame nearest the configured latency among the
//! frames that had arrived; a GAP RESYNC adopted the head), and the #1049 converge shed's dead band
//! (a source interval + 15 ms) held both parities. The camera arrival lag on strih-lx sits around
//! the 16.7 ms edge (35–50 ms, measured 28.9.2026), so every strih OBS restart, lock or relock drew
//! a new 33 / 50 / 67 ms presented age per camera — the S half of the per-restart camera lottery.
//!
//! D1 makes the presented stamp a PURE FUNCTION of the tick and the pin:
//!
//! - **The tick instant `T`** ([`n2_tick_ns`]) is the render tick's SCHEDULED wall instant on the
//!   per-second canvas grid — the grid point the tick is on — or, when the tick is off the grid (a
//!   wall step not re-gridded yet), the canvas grid floor of the processing wall.
//! - **The target stamp** ([`n2_target_stamp_ns`]) is
//!   `grid_floor(T − GENLOCK_N2_AGE_BASE_NS − pin, canvas_interval / N)`: the per-second SOURCE
//!   grid point at least `50 ms + pin` before the tick. At the production pin (3 ms) that is four
//!   60 fps frames, 66.7 ms; one more source interval of pin is exactly one more frame.
//! - **The pick** ([`n2_select`]): the newest queued frame stamped at most half a source interval
//!   after the target (the slack covers sender stamp rounding: a 100 ns-unit sender stamp sits up
//!   to 99 ns BEFORE its receiver grid point). Every older frame is erased. If that frame is the
//!   target's own, the tick is [`N2Kind::OnTarget`]; if the target has not arrived yet and the
//!   newest arrived frame is older, it is presented for THIS tick only ([`N2Kind::Early`], the
//!   `n2_early=` audit counter) — nothing re-anchors, the next tick targets the grid again. With
//!   nothing at or before the target the tick HOLDS ([`N2Kind::Hold`]).
//!
//! ACQUIRE, relock and gap all present the same stamp, so every camera and every restart land on
//! the same frame; the per-tick erase down to the target bounds the queue, so N>=2 sources no
//! longer need the #1049 converge shed, the backlog relock, the #1161 bracket hold or the #1003
//! phase anchor. N==1 sources (the stream `NDI 2ME PGM`, the shallow CG latch, imag's 60-into-60)
//! never reach this module.
//!
//! The C port is the contiguous `genlock_n2_*` block in `vendor/obs-studio/libobs/obs-source.c`;
//! `tests/genlock_relock_selection_parity.rs` lifts it verbatim and requires byte-identical
//! results from the functions here. The probe `ReleaseCadence` (src/probe/genlock.rs) delegates its
//! N>=2 ticks here. Pure: only [`crate::genlock_grid`], so it is standalone-rustc Tier-0 testable.

use crate::genlock_grid::grid_floor_ns;

/// The fixed part of an N>=2 source's presented stamp age, added to the pin. 50 ms covers the
/// measured strih-lx camera arrival lag (stamp to receive, 35–50 ms at p50, 28.9.2026) with a
/// margin; at the production pin 3 ms the presented age is 66.7 ms = four 60 fps frames. ONE fleet
/// constant, never a per-box number; the pin stays the one knob. Mirror of the C
/// `GENLOCK_N2_AGE_BASE_NS` (obs-source.c).
pub const GENLOCK_N2_AGE_BASE_NS: u64 = 50_000_000;

/// What an N>=2 tick does ([`n2_select`]). The C mirror encodes it as `GENLOCK_N2_HOLD` 0 /
/// `GENLOCK_N2_ON_TARGET` 1 / `GENLOCK_N2_EARLY` 2 ([`N2Kind::code`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum N2Kind {
    /// No queued frame is at or before the target: repeat the current frame (a late or a benign
    /// early hold, classified by the caller).
    Hold,
    /// The target's own frame is presented.
    OnTarget,
    /// The target has not arrived; the newest arrived frame before it is presented for this tick
    /// only (counted as `n2_early=`).
    Early,
}

impl N2Kind {
    /// The C encoding (`GENLOCK_N2_HOLD` 0, `GENLOCK_N2_ON_TARGET` 1, `GENLOCK_N2_EARLY` 2).
    pub fn code(self) -> u32 {
        match self {
            N2Kind::Hold => 0,
            N2Kind::OnTarget => 1,
            N2Kind::Early => 2,
        }
    }
}

/// The outcome of [`n2_select`]: the kind, and (not on a hold) the queue index to present — every
/// frame before it is erased, so `index` is also the erase count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct N2Pick {
    pub kind: N2Kind,
    /// The queue index presented (= the number of older frames erased). 0 on a hold.
    pub index: usize,
}

/// The source frame interval of an N>=2 source: `canvas_interval / n` (16_666_666 ns for a 60 fps
/// source into a 30 fps canvas). `n == 0` → 0 (never divide by zero; the caller only reaches
/// here with `n >= 2`).
pub fn n2_source_interval_ns(canvas_interval_ns: u64, n: u32) -> u64 {
    if n == 0 {
        return 0;
    }
    canvas_interval_ns / n as u64
}

/// The tick instant `T` the target is derived from. `on_grid` is the caller's
/// `n1_tick_is_on_grid(tick_wall_ns, canvas_interval_ns)` (the C `genlock_n1_tick_is_on_grid`):
/// on the grid, `T` is the canvas grid point the scheduled tick belongs to (the tick instant is read
/// microseconds early, so a bare floor would fall one slot back); off the grid (a wall step the
/// render tick has not re-gridded yet), the canvas grid floor of the processing wall.
pub fn n2_tick_ns(
    tick_wall_ns: u64,
    wall_now_ns: u64,
    canvas_interval_ns: u64,
    on_grid: bool,
) -> u64 {
    if on_grid {
        // The nearest canvas grid point: within the 2 ms on-grid window that is the scheduled
        // slot itself, whichever side of it the read landed.
        grid_floor_ns(
            tick_wall_ns.saturating_add(canvas_interval_ns / 2),
            canvas_interval_ns,
        )
    } else {
        grid_floor_ns(wall_now_ns, canvas_interval_ns)
    }
}

/// The stamp an N>=2 source presents at tick `tick_ns`:
/// `grid_floor(tick − GENLOCK_N2_AGE_BASE_NS − pin, canvas_interval / n)` on the per-second
/// source grid, saturating at 0.
pub fn n2_target_stamp_ns(tick_ns: u64, pin_ns: u64, canvas_interval_ns: u64, n: u32) -> u64 {
    let age = GENLOCK_N2_AGE_BASE_NS.saturating_add(pin_ns);
    grid_floor_ns(
        tick_ns.saturating_sub(age),
        n2_source_interval_ns(canvas_interval_ns, n),
    )
}

/// Pick the frame to present from `queue_stamps` (arrival order, oldest first; a single NDI source
/// delivers in stamp order): the last frame of the leading run stamped at most half a source
/// interval after `target_ns`. See [`N2Kind`] for the three outcomes.
pub fn n2_select(queue_stamps: &[u64], target_ns: u64, source_interval_ns: u64) -> N2Pick {
    let half = source_interval_ns / 2;
    let limit = target_ns.saturating_add(half);
    let count = queue_stamps.iter().take_while(|&&ts| ts <= limit).count();
    if count == 0 {
        return N2Pick {
            kind: N2Kind::Hold,
            index: 0,
        };
    }
    let index = count - 1;
    let kind = if queue_stamps[index].saturating_add(half) >= target_ns {
        N2Kind::OnTarget
    } else {
        N2Kind::Early
    };
    N2Pick { kind, index }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genlock_grid::{grid_advance_ns, per_second_floor, UNITS_100NS_PER_SECOND};

    const I30: u64 = 33_333_333;
    const I60: u64 = 16_666_666;
    const PIN3: u64 = 3_000_000;
    /// A whole second, 29.9.2026-ish (the per-second grid restarts here).
    const S: u64 = 1_790_640_000_000_000_000;

    /// The 30 fps per-second canvas grid point `j` slots after `S` (j < 30).
    fn tick(j: u64) -> u64 {
        S + j * 1_000_000_000 / 30
    }

    /// A camera sender stamp: the 100 ns-unit per-second 60 fps floor of the capture instant, in ns.
    fn sender_stamp(t: u64) -> u64 {
        per_second_floor(t / 100, 60, UNITS_100NS_PER_SECOND) * 100
    }

    /// The source interval of a 60-into-30 source, and of any other integer multiple.
    #[test]
    fn source_interval_is_the_canvas_interval_over_n() {
        assert_eq!(n2_source_interval_ns(I30, 2), I60);
        assert_eq!(n2_source_interval_ns(I30, 3), 11_111_111);
        assert_eq!(n2_source_interval_ns(I30, 0), 0, "n == 0 never divides");
    }

    /// At the production pin every 30 fps tick of a second targets the 60 fps grid point exactly
    /// four source frames before it — 66.7 ms (66_666_666 or 66_666_667 ns on the per-second grid).
    #[test]
    fn production_pin_targets_four_source_frames_back() {
        for j in 0..30 {
            let t = tick(j);
            let target = n2_target_stamp_ns(t, PIN3, I30, 2);
            assert_eq!(
                grid_floor_ns(target, I60),
                target,
                "tick {j}: the target is a 60 fps per-second grid point"
            );
            let age = t - target;
            assert!(
                (66_666_666..=66_666_667).contains(&age),
                "tick {j}: presented age {age} ns, want 66.7 ms (four 60 fps frames)"
            );
        }
    }

    /// The pin stays the ONE knob: each extra source interval of pin moves the target exactly one
    /// source frame older, and any pin inside the same source interval keeps the same frame.
    #[test]
    fn one_source_interval_of_pin_is_exactly_one_frame() {
        let t = tick(7);
        let base = n2_target_stamp_ns(t, PIN3, I30, 2);
        for k in 0..40u64 {
            let pin = PIN3 + k * 16_666_667;
            let target = n2_target_stamp_ns(t, pin, I30, 2);
            let frames = (base - target + I60 / 2) / I60;
            assert_eq!(frames, k, "pin {pin} ns: {frames} frames older, want {k}");
        }
        // 3..=16 ms all land on the same frame (50 + pin ∈ (50, 66.7] ms).
        for pin_ms in 1..=16u64 {
            assert_eq!(
                n2_target_stamp_ns(t, pin_ms * 1_000_000, I30, 2),
                base,
                "pin {pin_ms} ms"
            );
        }
        assert_eq!(
            t - n2_target_stamp_ns(t, 17_000_000, I30, 2),
            t - base + I60,
            "pin 17 ms (50 + 17 > 66.7) is one frame deeper"
        );
    }

    /// The tick instant: on the grid it is the grid point the scheduled tick belongs to — a tick
    /// read microseconds EARLY must not fall one slot back — and off the grid it is the canvas grid
    /// floor of the processing wall.
    #[test]
    fn tick_instant_snaps_on_grid_and_floors_off_grid() {
        let g = tick(11);
        for off in [-2_000_000i64, -5_000, -1, 0, 1, 1_999_999, 2_000_000] {
            let tw = g.saturating_add_signed(off);
            assert_eq!(
                n2_tick_ns(tw, tw + 700_000, I30, true),
                g,
                "on-grid offset {off}"
            );
        }
        // Off the grid (a wall step): the canvas floor of the processing wall, not the tick read.
        let tw = g + 12_000_000;
        assert_eq!(n2_tick_ns(tw, tw + 1_000_000, I30, false), g);
        assert_eq!(
            n2_tick_ns(g - 1, g - 1, I30, false),
            tick(10),
            "floor, never forward"
        );
    }

    /// The 60 fps per-second grid point `k` slots after (`k > 0`) or before the grid point `g`.
    fn slot(g: u64, k: i64) -> u64 {
        if k >= 0 {
            grid_advance_ns(g, k as u64, I60)
        } else {
            let mut p = g;
            for _ in 0..(-k) {
                p = grid_floor_ns(p - 1, I60);
            }
            p
        }
    }

    /// `count` consecutive camera sender stamps starting at the slot `first` slots from grid point
    /// `g` (each captured a few ns after its slot point).
    fn stamps(g: u64, first: i64, count: i64) -> Vec<u64> {
        (first..first + count)
            .map(|k| sender_stamp(slot(g, k) + 3))
            .collect()
    }

    /// Steady state: the queue holds the frame before the target, the target and two newer frames.
    /// The target's own frame is presented and the older one erased.
    #[test]
    fn steady_tick_presents_the_target_and_erases_the_older_frame() {
        let t = tick(4);
        let target = n2_target_stamp_ns(t, PIN3, I30, 2);
        let q = stamps(target, -1, 4);
        let pick = n2_select(&q, target, I60);
        assert_eq!(pick.kind, N2Kind::OnTarget);
        assert_eq!(pick.index, 1);
        assert!(
            q[pick.index] <= target && target - q[pick.index] < 100,
            "the target's own stamp"
        );
    }

    /// The target has not arrived but the frame one source interval before it has: that frame goes
    /// on air for this tick only.
    #[test]
    fn missing_target_presents_the_frame_before_it_as_early() {
        let t = tick(9);
        let target = n2_target_stamp_ns(t, PIN3, I30, 2);
        let q = stamps(target, -2, 2);
        let pick = n2_select(&q, target, I60);
        assert_eq!(pick.kind, N2Kind::Early);
        assert_eq!(
            pick.index, 1,
            "the newest arrived frame, the one before the target"
        );
    }

    /// Nothing at or before the target (a cold start, or a sender that is behind): HOLD.
    #[test]
    fn only_younger_frames_hold() {
        let t = tick(2);
        let target = n2_target_stamp_ns(t, PIN3, I30, 2);
        let q = stamps(target, 1, 3);
        assert_eq!(n2_select(&q, target, I60).kind, N2Kind::Hold);
        assert_eq!(n2_select(&[], target, I60).kind, N2Kind::Hold);
    }

    /// The half-interval slack edges: a stamp up to half a source interval after the target is
    /// the target's frame; one ns more is the next frame and is not presented.
    #[test]
    fn half_interval_slack_edges() {
        let target = S + 500_000_000;
        let half = I60 / 2;
        let at_edge = n2_select(&[target - I60, target + half], target, I60);
        assert_eq!((at_edge.kind, at_edge.index), (N2Kind::OnTarget, 1));
        let past = n2_select(&[target - I60, target + half + 1], target, I60);
        assert_eq!((past.kind, past.index), (N2Kind::Early, 0));
        // A stamp exactly half an interval before the target still counts as on target; one ns
        // older is early.
        let low = n2_select(&[target - half], target, I60);
        assert_eq!(low.kind, N2Kind::OnTarget);
        let lower = n2_select(&[target - half - 1], target, I60);
        assert_eq!(lower.kind, N2Kind::Early);
    }

    /// A duplicate stamp (a free-running grabber beat) at the target: the later copy is presented.
    #[test]
    fn duplicate_target_stamp_presents_the_later_copy() {
        let target = S + 250_000_000;
        let pick = n2_select(&[target - I60, target, target, target + I60], target, I60);
        assert_eq!((pick.kind, pick.index), (N2Kind::OnTarget, 2));
    }

    /// RESTART DETERMINISM: whatever instant the receiver (re)starts and whatever arrival lag the
    /// camera has inside the budget, the tick presents the SAME stamp — the one of the target slot.
    #[test]
    fn any_acquire_instant_and_arrival_lag_presents_the_same_stamp() {
        let t = tick(17);
        let target = n2_target_stamp_ns(t, PIN3, I30, 2);
        let want = sender_stamp(target + 50);
        let mut cases = 0;
        for lag_ms in 0..=66u64 {
            let lag = lag_ms * 1_000_000 + 300_000;
            if lag > t - target {
                continue;
            }
            // The receiver started `start_ms` before this tick: frames that ARRIVED since then
            // (stamp + lag in [t - start, t]) are queued, nothing else.
            for start_ms in [1u64, 9, 17, 34, 51, 70, 120, 400] {
                let start = t - start_ms * 1_000_000;
                let q: Vec<u64> = stamps(target, -40, 44)
                    .into_iter()
                    .filter(|&s| s + lag >= start && s + lag <= t)
                    .collect();
                let pick = n2_select(&q, target, I60);
                if q.contains(&want) {
                    assert_eq!(
                        pick.kind,
                        N2Kind::OnTarget,
                        "lag {lag_ms} ms start {start_ms} ms"
                    );
                    assert_eq!(q[pick.index], want, "lag {lag_ms} ms start {start_ms} ms");
                    cases += 1;
                } else {
                    // Started after the target frame arrived: only younger frames, a hold.
                    assert_eq!(
                        pick.kind,
                        N2Kind::Hold,
                        "lag {lag_ms} ms start {start_ms} ms"
                    );
                }
            }
        }
        assert!(
            cases > 300,
            "the sweep must exercise the on-target case: {cases}"
        );
    }

    /// The saturation edges: a tick younger than the age targets 0 (never wraps), and a target near
    /// the top of the range never overflows the slack.
    #[test]
    fn saturation_edges() {
        assert_eq!(n2_target_stamp_ns(10_000_000, PIN3, I30, 2), 0);
        let pick = n2_select(&[u64::MAX - 5], u64::MAX - 3, I60);
        assert_eq!((pick.kind, pick.index), (N2Kind::OnTarget, 0));
        assert_eq!(N2Kind::Hold.code(), 0);
        assert_eq!(N2Kind::OnTarget.code(), 1);
        assert_eq!(N2Kind::Early.code(), 2);
    }
}
