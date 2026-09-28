//! Issue 1367 slice D1 — the 60-into-30 TWO-CLOCK bench for the grid-exact N>=2 conveyor.
//!
//! Seven strih-lx cameras (60 fps, stamped on the per-second 60 fps grid in 100 ns units, the
//! camera sender clock) feed a 30 fps strih canvas whose render tick runs on the per-second 30 fps
//! grid of the receiver clock, a few hundred µs late and read a few µs early (the scheduled-instant
//! read of `genlock_n1_tick_wall_now`). Each camera's frames ARRIVE after a lag drawn from the
//! arrival lag measured on strih-lx on 28.9.2026 ([`CAMERA_LAG_PPM`]), delivered in order (a late
//! frame holds the ones behind it, so a tail frame makes a burst). The receiver is RESTARTED at
//! random instants: an empty FIFO, no lock, no confirmed multiple.
//!
//! Each tick runs a port of the top of `genlock_release_tick` (obs-source.c): the sticky source
//! multiple first; an N>=2 source takes the grid release through the REAL authority
//! ([`crate::genlock_n2_grid`]); an inconclusive first tick (fewer than two queued frames, no
//! confirmed multiple) takes the N==1 ACQUIRE / STEADY / GAP / HOLD path, which this bench models
//! only as far as that start-up needs (the N==1 governor, drain and converge shed never act on a
//! 3 ms camera in one or two ticks).
//!
//! What it proves:
//! - inside the budget (every lag at most 66.7 ms at the production pin), EVERY camera presents
//!   the frame of the 60 fps slot four slots before the tick on EVERY restart, every tick after the
//!   start-up, a lossless every-second-frame cadence (`every_restart_lands_on_the_same_frame_1367`);
//! - with the measured tails, a late target costs only that tick (early, hold or underrun) and the
//!   next present is back on the grid; the per-camera `n2_early` rate is reported
//!   (`measured_tails_cost_only_their_own_tick_1367`, run with `--nocapture` for the table).
//!
//! The tail rates are an UPPER BOUND, not a prediction. The histogram is sampled per TICK, so one
//! real stall of several ticks shows as several tail samples; the bench draws every FRAME's lag
//! independently from it and delivers in order, so every tail draw becomes a stall of its own that
//! also holds the frames behind it. Its misses (early + holds + underruns) come out about 2-8x the
//! measured share of tail samples (cam3: 1.9 % against 0.41 %). The expected live `n2_early` rate
//! is at most the measured share of tail samples per camera (`CAMERA_LAG_PPM` bins 5..=8: cam1
//! 0.089, cam2 0.126, cam3 0.405, cam4 0.153, cam5 0, cam6 0.050, cam7 0.025 %, pooled 0.097 %),
//! since a missed target is presented early only when an older frame is still queued — during a
//! stall the queue runs empty and the tick is an underrun instead.

use crate::genlock_backlog::{
    phase_pinned_deadline, relock_anchor_age_ns, relock_select_nearest,
    source_interval_from_stamps, PHASE_PIN_HYSTERESIS_NS,
};
use crate::genlock_grid::{grid_advance_ns, per_second_floor, UNITS_100NS_PER_SECOND};
use crate::genlock_n1_depth::n1_tick_is_on_grid;
use crate::genlock_n2_grid::{
    n2_select, n2_source_interval_ns, n2_target_stamp_ns, n2_tick_ns, N2Kind,
};
use std::collections::VecDeque;

const I30: u64 = 33_333_333;
const I60: u64 = 16_666_666;
const PIN_MS: u32 = 3;
/// A whole second, 29.9.2026 (both per-second grids restart here).
const S0: u64 = 1_790_640_000_000_000_000;
/// The grid target sits four 60 fps slots before the tick at the production pin (66.7 ms).
const TARGET_SLOTS_BACK: u64 = 4;

/// The measured strih-lx arrival lag per camera (28.9.2026, `2026-09-28 1*.txt`, 30 793
/// `genlock-fifo audit` samples of the seven program inputs): the share, in ppm, of ticks whose
/// NEWEST queued frame was k 60 fps slots old (`head_skew − (depth + erased − 1) × 16.7 ms`). Bin
/// k (1..=7) = a lag in `((k − 1) × 16.7, k × 16.7]` ms; bin 8 = 116.7..250 ms (the stall
/// samples, up to 333 ms measured). The target at 66.7 ms is missed only in bins 5..=8.
const CAMERA_LAG_PPM: [(&str, [u32; 8]); 7] = [
    ("cam1", [637, 91_013, 884_257, 23_199, 764, 0, 0, 127]),
    ("cam2", [1_538, 174_989, 814_379, 7_833, 419, 139, 279, 419]),
    (
        "cam3",
        [5_068, 194_627, 765_331, 30_917, 1_013, 506, 506, 2_027],
    ),
    ("cam4", [9_174, 525_484, 458_205, 5_606, 509, 0, 0, 1_019]),
    ("cam5", [4_034, 169_440, 797_276, 29_248, 0, 0, 0, 0]),
    ("cam6", [5_032, 166_582, 791_645, 36_235, 0, 0, 0, 503]),
    ("cam7", [886, 244_996, 746_009, 7_854, 0, 0, 0, 253]),
];

/// splitmix64 — a deterministic, dependency-free random stream.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

/// One camera's lag draw: a histogram bin by its ppm share, then uniform inside the bin, never
/// over `cap_ns`.
struct LagModel {
    ppm: [u32; 8],
    cap_ns: u64,
}

impl LagModel {
    /// The measured distribution, tails included.
    fn measured(ppm: [u32; 8]) -> Self {
        Self {
            ppm,
            cap_ns: u64::MAX,
        }
    }

    /// The measured distribution with the tail bins (5..=8, over 66.7 ms) removed and the top bin
    /// capped at 66 ms: every frame arrives before the tick that targets it (its stamp sits up to
    /// 1 µs after the slot point and the tick runs up to 0.6 ms late, never early).
    fn inside_budget(ppm: [u32; 8]) -> Self {
        let mut p = ppm;
        for b in p.iter_mut().skip(4) {
            *b = 0;
        }
        Self {
            ppm: p,
            cap_ns: 66_000_000,
        }
    }

    fn draw(&self, rng: &mut Rng) -> u64 {
        let total: u64 = self.ppm.iter().map(|&p| p as u64).sum();
        let mut r = rng.below(total);
        let mut bin = 0usize;
        for (i, &p) in self.ppm.iter().enumerate() {
            if r < p as u64 {
                bin = i;
                break;
            }
            r -= p as u64;
        }
        let lo = bin as u64 * 16_666_667;
        let hi = if bin == 7 {
            250_000_000
        } else {
            (bin as u64 + 1) * 16_666_666
        }
        .min(self.cap_ns);
        lo + 1 + rng.below(hi - lo)
    }
}

/// The camera sender stamp of 60 fps slot `slot` counted from `S0`: the 100 ns per-second floor
/// of its capture instant (captured 1 µs into the slot).
fn stamp_of_slot(slot: u64) -> u64 {
    let capture = grid_advance_ns(S0, slot, I60) + 1_000;
    per_second_floor(capture / 100, 60, UNITS_100NS_PER_SECOND) * 100
}

/// The 60 fps slot a stamp belongs to (a stamp sits at most 99 ns before its slot point).
fn slot_of(stamp: u64) -> u64 {
    ((stamp + 100 - S0) as u128 * 60 / 1_000_000_000) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// get_closest_frame found the FIFO empty (an underrun: OBS repeats the frame).
    Underrun,
    Hold {
        late: bool,
    },
    /// A present: the stamp, whether the grid release took it early, and whether the N==1 path
    /// (an inconclusive tick) presented it.
    Present {
        stamp: u64,
        early: bool,
        legacy: bool,
    },
}

/// The receiver side of one camera input: the FIFO and the release state that survives ticks.
#[derive(Default)]
struct Receiver {
    queue: VecDeque<u64>,
    boundary: u64,
    last_known_n: u32,
}

impl Receiver {
    /// Mirror of `genlock_effective_source_multiple`: the min adjacent delta of the first six
    /// queued stamps, latched; an inconclusive tick bridges with the latch, never invents one.
    fn effective_n(&mut self) -> u32 {
        let window: Vec<u64> = self.queue.iter().take(6).copied().collect();
        match source_interval_from_stamps(&window) {
            Some(si) => {
                let n = ((I30 + si / 2) / si).max(1) as u32;
                self.last_known_n = n;
                n
            }
            None => self.last_known_n.max(1),
        }
    }

    /// One render tick: `tick_wall` is the scheduled-instant read (the grid slot, microseconds
    /// early), `wall` the processing wall (the slot plus the tick's lateness).
    fn tick(&mut self, tick_wall: u64, wall: u64) -> Outcome {
        if self.queue.is_empty() {
            return Outcome::Underrun;
        }
        let pin_ns = PIN_MS as u64 * 1_000_000;
        let present_ts = phase_pinned_deadline(wall - pin_ns, I30);
        let n = self.effective_n();
        if n >= 2 {
            let on_grid = n1_tick_is_on_grid(tick_wall, I30);
            let t = n2_tick_ns(tick_wall, wall, I30, on_grid);
            let target = n2_target_stamp_ns(t, pin_ns, I30, n);
            let stamps: Vec<u64> = self.queue.iter().copied().collect();
            let pick = n2_select(&stamps, target, n2_source_interval_ns(I30, n));
            if pick.kind == N2Kind::Hold {
                return Outcome::Hold {
                    late: self.boundary != 0 && present_ts >= self.boundary,
                };
            }
            self.queue.drain(..pick.index);
            let stamp = self.queue.pop_front().expect("the pick");
            self.boundary = stamp + I30;
            return Outcome::Present {
                stamp,
                early: pick.kind == N2Kind::Early,
                legacy: false,
            };
        }
        // The N==1 path: only an inconclusive tick (one queued frame, no confirmed multiple).
        let due = self
            .queue
            .iter()
            .take_while(|&&ts| ts <= present_ts + PHASE_PIN_HYSTERESIS_NS)
            .count();
        if self.boundary == 0 {
            self.last_known_n = 0;
            if due == 0 {
                return Outcome::Hold { late: false };
            }
            let stamps: Vec<u64> = self.queue.iter().copied().collect();
            let sel = relock_select_nearest(&stamps, wall, relock_anchor_age_ns(0, PIN_MS));
            self.queue.drain(..sel);
        } else {
            let head = *self.queue.front().expect("non-empty");
            if head > self.boundary {
                if present_ts < head {
                    return Outcome::Hold {
                        late: present_ts >= self.boundary,
                    };
                }
                self.last_known_n = 0; // GAP RESYNC
            }
        }
        let stamp = self.queue.pop_front().expect("the head");
        self.boundary = stamp + I30;
        Outcome::Present {
            stamp,
            early: false,
            legacy: true,
        }
    }
}

/// Per-camera bench totals. Everything but `startup_ticks` counts from the first on-target
/// present after the restart (the grid has started); the start-up is the FIFO filling to the
/// target age, including the one N==1 present an inconclusive first tick may take.
#[derive(Debug, Default, Clone, Copy)]
struct CameraRun {
    ticks: u64,
    startup_ticks: u64,
    on_target: u64,
    early: u64,
    late_holds: u64,
    benign_holds: u64,
    underruns: u64,
    legacy_presents: u64,
    /// Grid presents whose slot was not the tick's target slot (must stay 0).
    off_target: u64,
}

/// Run one camera from a receiver restart at wall instant `restart` for `ticks` render ticks.
fn run_camera(lag: &LagModel, seed: u64, restart: u64, ticks: u64) -> CameraRun {
    let mut rng = Rng(seed);
    let mut rx = Receiver::default();
    let mut out = CameraRun::default();
    // The first canvas tick at or after the restart, and the frames the sender is sending then.
    let first_tick = ((restart - S0) as u128 * 30).div_ceil(1_000_000_000) as u64;
    let mut slot = (((restart - S0) as u128 * 60 / 1_000_000_000) as u64).saturating_sub(20);
    let mut last_arrival = 0u64;
    let mut pending: Option<(u64, u64)> = None; // (stamp, arrival) of the next undelivered frame
    let mut grid_started = false;
    for j in first_tick..first_tick + ticks {
        let scheduled = grid_advance_ns(S0, j, I30);
        // Receiver tick jitter: a few hundred µs late, 0.1 % of ticks 10-30 ms late.
        let lateness = if rng.below(1_000) == 0 {
            10_000_000 + rng.below(20_000_000)
        } else {
            rng.below(600_000)
        };
        let wall = scheduled + lateness;
        let tick_wall = scheduled - rng.below(20_000); // the scheduled read comes out µs early
        loop {
            let (stamp, arrival) = match pending {
                Some(p) => p,
                None => {
                    let stamp = stamp_of_slot(slot);
                    let arrival = (stamp + lag.draw(&mut rng)).max(last_arrival);
                    slot += 1;
                    last_arrival = arrival;
                    (stamp, arrival)
                }
            };
            if arrival > wall {
                pending = Some((stamp, arrival));
                break;
            }
            pending = None;
            if arrival >= restart {
                rx.queue.push_back(stamp);
            }
        }
        out.ticks += 1;
        let outcome = rx.tick(tick_wall, wall);
        let target_slot = 2 * j - TARGET_SLOTS_BACK;
        if !grid_started {
            match outcome {
                Outcome::Present {
                    stamp,
                    early: false,
                    legacy: false,
                } if slot_of(stamp) == target_slot => grid_started = true,
                _ => {
                    out.startup_ticks += 1;
                    if let Outcome::Present { legacy: true, .. } = outcome {
                        out.legacy_presents += 1;
                    }
                    continue;
                }
            }
        }
        match outcome {
            Outcome::Underrun => out.underruns += 1,
            Outcome::Hold { late: true } => out.late_holds += 1,
            Outcome::Hold { late: false } => out.benign_holds += 1,
            Outcome::Present { legacy: true, .. } => out.legacy_presents += 1,
            Outcome::Present {
                stamp, early: true, ..
            } => {
                out.early += 1;
                assert!(
                    slot_of(stamp) < target_slot,
                    "an early present is older than the target"
                );
            }
            Outcome::Present { stamp, .. } => {
                if slot_of(stamp) == target_slot {
                    out.on_target += 1;
                } else {
                    out.off_target += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE CLAIM: inside the budget, every camera lands on the SAME frame after EVERY restart —
    /// the grid target, four 60 fps slots before the tick — on every tick after the start-up, with
    /// a lossless every-second-frame cadence. The restarts land at random instants and the lags
    /// are drawn per frame from each camera's measured distribution without its tail.
    #[test]
    fn every_restart_lands_on_the_same_frame_1367() {
        let mut rng = Rng(0x0013_67D1_2026_0928);
        let mut restarts = 0;
        let (mut max_startup, mut max_legacy) = (0, 0);
        for (name, ppm) in CAMERA_LAG_PPM {
            let lag = LagModel::inside_budget(ppm);
            for _ in 0..20 {
                let restart = S0 + 10_000_000_000 + rng.below(3_000_000_000);
                let r = run_camera(&lag, rng.next(), restart, 240);
                // The start-up: the FIFO fills to the target age (the target's frame is 66.7 ms
                // old at the tick) while an inconclusive first tick or two (one queued frame, no
                // confirmed multiple) take the N==1 path — a bounded transient, never a phase.
                max_startup = max_startup.max(r.startup_ticks);
                max_legacy = max_legacy.max(r.legacy_presents);
                assert!(r.startup_ticks <= 6, "{name}: a long start-up: {r:?}");
                assert!(r.legacy_presents <= 3, "{name}: {r:?}");
                // From the first grid present on, EVERY tick presents the target frame: the same
                // slot for every restart, one frame pair per tick (a lossless 30 fps cadence).
                assert_eq!(
                    r.on_target,
                    r.ticks - r.startup_ticks,
                    "{name}: a tick after the start-up was not on the target: {r:?}"
                );
                assert_eq!(
                    r.off_target + r.early + r.late_holds + r.underruns,
                    0,
                    "{name}: {r:?}"
                );
                restarts += 1;
            }
        }
        assert_eq!(restarts, 140);
        eprintln!(
            "140 restarts: every tick after the start-up on the target; start-up at most \
             {max_startup} ticks, at most {max_legacy} N==1 presents in it"
        );
    }

    /// With the measured tails, a late target costs only its own tick (an early present, a hold
    /// or an underrun — never an off-target grid present, so no phase sticks), and the per-camera
    /// `n2_early` rate at 66.7 ms is reported (the D1 budget: at most 0.1 % of the ticks) next to
    /// the measured share of tail samples. The bench rates are an upper bound (module doc): the
    /// i.i.d. per-frame draw turns every tail sample into a stall of its own.
    #[test]
    fn measured_tails_cost_only_their_own_tick_1367() {
        let mut rng = Rng(0x0013_67D1_7A11_0928);
        eprintln!(
            "camera   ticks  on_target  n2_early (rate)  late_holds  underruns  legacy  \
             misses (bench, upper bound)  tail samples (measured)"
        );
        let mut total = CameraRun::default();
        for (name, ppm) in CAMERA_LAG_PPM {
            let lag = LagModel::measured(ppm);
            let restart = S0 + 10_000_000_000 + rng.below(1_000_000_000);
            let r = run_camera(&lag, rng.next(), restart, 54_000); // 30 min
            let tail: u32 = ppm[4..].iter().sum();
            let misses = r.early + r.late_holds + r.underruns;
            eprintln!(
                "{name}   {:>6}  {:>9}  {:>5} ({:.3} %)  {:>10}  {:>9}  {:>6}  {:>6.3} %  {:>6.3} %",
                r.ticks,
                r.on_target,
                r.early,
                r.early as f64 * 100.0 / r.ticks as f64,
                r.late_holds,
                r.underruns,
                r.legacy_presents,
                misses as f64 * 100.0 / r.ticks as f64,
                tail as f64 / 10_000.0
            );
            assert_eq!(
                r.off_target, 0,
                "{name}: a late frame moved the phase: {r:?}"
            );
            if tail == 0 {
                assert_eq!(
                    r.early + r.late_holds + r.underruns,
                    0,
                    "{name}: no tail, no miss: {r:?}"
                );
            } else {
                assert!(
                    r.early + r.late_holds + r.underruns > 0,
                    "{name}: the tail must be exercised: {r:?}"
                );
            }
            total.ticks += r.ticks;
            total.early += r.early;
        }
        eprintln!(
            "all      {:>6}  n2_early {} ({:.3} %)",
            total.ticks,
            total.early,
            total.early as f64 * 100.0 / total.ticks as f64
        );
    }

    /// The model itself: a lag draw lands in its bin, the tail-free model never draws over the
    /// budget, and the slot mapping inverts the stamp.
    #[test]
    fn the_model_draws_inside_its_bins_1367() {
        let mut rng = Rng(7);
        for (_, ppm) in CAMERA_LAG_PPM {
            let inside = LagModel::inside_budget(ppm);
            for _ in 0..2_000 {
                let l = inside.draw(&mut rng);
                assert!(l > 0 && l <= 66_000_000, "{l}");
            }
        }
        let one_bin = LagModel::measured([0, 0, 1, 0, 0, 0, 0, 0]);
        for _ in 0..500 {
            let l = one_bin.draw(&mut rng);
            assert!((33_333_335..=49_999_998).contains(&l), "{l}");
        }
        for k in [0u64, 1, 59, 60, 61, 1_000] {
            assert_eq!(slot_of(stamp_of_slot(k)), k);
        }
    }
}
