//! #1355 part 2 — the grid-drift BENCH: the deep stream `NDI 2ME PGM` FIFO (an N==1, 30-into-30
//! source) fed by the strih-lx program sender, simulated tick by tick with the SAME decision
//! functions the vendored C release uses, under the measured rig statistics — and under either the
//! pre-#1355 1970 grid or the production grid (`crate::genlock_grid`).
//!
//! ## What it reproduces (live, 20.–24.9.2026, 10 stream logs + 2 strih-lx logs)
//!
//! At a CONSTANT pin (987 ms) the 2ME PGM FIFO flipped between depth states 31 and 32 frames,
//! 10–45 times per hour, bursty. Mechanism (#1355 design comment): the sender stamps on the
//! per-second grid, the receiver floored its release deadline and ticked on the 1970 grid, and the
//! two drift 10 ns/s apart; by 24.9. the stamps sat ~2.0 ms AFTER the floored deadline. A frame 2 ms
//! past the deadline is "due" under the 5 ms hysteresis but NOT past it for the missing-stamp
//! GAP-RESYNC check, so when the sender produces an irregular stamp — strih-lx stamps at SEND time,
//! a slow frame lands in the next 1/30 s cell = a gap followed by a duplicate — the receiver HOLDs
//! one tick (a `late_hold`) and the FIFO sits one frame deeper: 31 → 32. It comes back down only
//! when a late stream render tick lets the #859 settle-back drain see one frame too many (32 → 31,
//! `dropped_due +1`).
//!
//! ## Model (every number is a measured rig statistic, see [`BenchConfig::live_2026_09_24`])
//!
//! - **Sender (strih-lx OBS program output):** renders on its own genlock tick (the grid under
//!   test), hands the frame to NDI after a send delay (median 20.7 ms, p99 24.9 ms measured) plus a
//!   rare late-render tail, and stamps it with the per-second FLOOR of the SEND instant — the
//!   arithmetic of `genlock_floor_boundary_100ns` ([`per_second_floor`] in 100 ns units). Sends are
//!   serial (a frame can never be handed over before its predecessor).
//! - **Receiver (stream OBS):** ticks on the grid under test with the measured tick jitter
//!   (p99 0.53 ms; 0.1 % of ticks > 10 ms late). Each tick runs a port of the N==1 branches of
//!   `genlock_release_tick` (obs-source.c): ACQUIRE / BACKLOG relock via
//!   [`relock_select_nearest`], STEADY on the locked boundary, GAP RESYNC on the floored deadline,
//!   HOLD / late HOLD, the erase loop, and the #859 settle-back drain via [`should_drain_one`]. The
//!   #1049 converge shed is N>=2-only and inert here.
//! - **The deadline** is the production [`phase_pinned_deadline`] (the Rust authority of the C
//!   `genlock_phase_pin_deadline`) and **the render tick** is the production
//!   [`grid_next_boundary_ns`] (the authority of the C `genlock_next_deadline` boundary) — so a bench
//!   run on [`GridModel::Production`] exercises exactly today's arithmetic. [`GridModel::Legacy1970`]
//!   keeps a local copy of the pre-#1355 arithmetic so the bench can always show what the fix
//!   removed.
//! - **The flip count** samples the presented frame's age every 150 ticks (the 5 s audit cadence),
//!   `state = round(age / interval)`, and counts sample-to-sample changes after a warm-up — the same
//!   methodology as the live analysis of the `ts_head_skew_ms` audit field.
//!
//! The two TAIL rates (the sender's late-render frames, the receiver's late ticks) are the only
//! calibrated knobs: the design measured their existence and the correlation (flips 11× more likely
//! in strih-lx late-render windows) but not a per-frame rate. They are set so the 1970-grid run
//! lands inside the live 10–45 flips/h band; the claim under test is that the SAME inputs on the
//! production grid do not flip.

use crate::genlock_backlog::{
    backlog_relock_threshold, phase_anchor_from_present, phase_pinned_deadline,
    relock_anchor_age_ns, relock_select_nearest, should_drain_one, PHASE_PIN_HYSTERESIS_NS,
};
use crate::genlock_grid::{
    grid_next_boundary_ns, per_second_floor, StampTrack, NS_PER_SECOND, UNITS_100NS_PER_SECOND,
};
use std::collections::{BTreeMap, VecDeque};

/// The 30 fps canvas interval of both OBS boxes (`1e9 / 30`, integer).
pub const CANVAS_INTERVAL_NS: u64 = 33_333_333;
const FPS: u64 = 30;
/// Audit cadence in ticks (5 s at 30 fps) — how often the flip counter samples the state.
const SAMPLE_EVERY_TICKS: u64 = 150;

/// Which grid the sender tick, the receiver tick and the receiver deadline use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridModel {
    /// The pre-#1355 arithmetic: `(t / interval) * interval` / `t - t % interval + interval`,
    /// counted from 1970 — a local copy kept only so the bench can show what the fix removed.
    Legacy1970,
    /// Today's production arithmetic: [`phase_pinned_deadline`] + [`grid_next_boundary_ns`].
    Production,
}

impl GridModel {
    fn deadline_floor(self, t: u64) -> u64 {
        match self {
            GridModel::Legacy1970 => (t / CANVAS_INTERVAL_NS) * CANVAS_INTERVAL_NS,
            GridModel::Production => phase_pinned_deadline(t, CANVAS_INTERVAL_NS),
        }
    }

    fn next_tick(self, t: u64) -> u64 {
        match self {
            GridModel::Legacy1970 => t - (t % CANVAS_INTERVAL_NS) + CANVAS_INTERVAL_NS,
            GridModel::Production => grid_next_boundary_ns(t, CANVAS_INTERVAL_NS),
        }
    }
}

/// One bench scenario.
#[derive(Clone, Debug)]
pub struct BenchConfig {
    pub grid: GridModel,
    /// How far the per-second grid sits AFTER the 1970 grid at the start (the date-dependent
    /// offset; 2.0 ms on 24.9.2026, growing 10 ns per second). Rounded down to 10 ns.
    pub start_offset_ns: u64,
    pub duration_s: u64,
    /// Samples before this many seconds are warm-up (acquire + queue build) and not counted.
    pub warmup_s: u64,
    /// The stream 2ME PGM pin.
    pub latency_ms: u32,
    pub seed: u64,
    /// strih-lx render-tick -> NDI hand-over delay: median + spread (p99 = median + 2.33 σ).
    pub send_delay_median_ns: u64,
    pub send_delay_sigma_ns: u64,
    /// Late-render tail: probability per frame (parts per million) and the extra delay range
    /// ADDED on top of one full interval (such a frame lands in the next 1/30 s cell).
    pub send_late_ppm: u64,
    pub send_late_extra_max_ns: u64,
    /// strih-lx -> stream network hand-over (constant).
    pub network_ns: u64,
    /// Stream render-tick lateness: half-normal core σ, plus a tail (ppm per tick) uniformly
    /// late in `[tick_late_min_ns, tick_late_max_ns]`.
    pub tick_jitter_sigma_ns: u64,
    pub tick_late_ppm: u64,
    pub tick_late_min_ns: u64,
    pub tick_late_max_ns: u64,
}

impl BenchConfig {
    /// The rig as measured for #1355 (stream log 23.–24.9. + strih-lx logs), pin 987 ms, offset
    /// 2.0 ms. The two `*_ppm` tails are the calibrated knobs (see the module doc).
    pub fn live_2026_09_24(grid: GridModel) -> Self {
        BenchConfig {
            grid,
            start_offset_ns: 2_000_000,
            duration_s: 6 * 3600,
            warmup_s: 120,
            latency_ms: 987,
            seed: 0x1355_2026_0924,
            send_delay_median_ns: 20_700_000,
            send_delay_sigma_ns: 1_800_000,
            send_late_ppm: 300,
            send_late_extra_max_ns: 12_000_000,
            network_ns: 1_000_000,
            tick_jitter_sigma_ns: 200_000,
            tick_late_ppm: 1_000,
            tick_late_min_ns: 10_000_000,
            tick_late_max_ns: 30_000_000,
        }
    }
}

/// What one bench run observed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BenchReport {
    /// Counted (post-warm-up) hours.
    pub hours: f64,
    /// 5 s sampled depth-state changes after warm-up (the live `flips/h` metric).
    pub flips: u64,
    pub flips_per_hour: f64,
    /// 5 s samples per depth state (frames of presented age).
    pub state_samples: BTreeMap<u64, u64>,
    /// Per-TICK state changes after warm-up (includes one-tick blips the 5 s audit cannot see).
    pub tick_state_changes: u64,
    pub holds: u64,
    pub late_holds: u64,
    pub resyncs: u64,
    pub relocks: u64,
    pub drains: u64,
    pub dropped_due: u64,
    pub underruns: u64,
    /// The sender's stamp irregularity as the receiver's `stamp_dup=` / `stamp_gap=` audit tokens
    /// would count it ([`StampTrack`], the production arrival-side tracker): stamps equal to their
    /// predecessor, and missing stamp intervals.
    pub stamp_dups: u64,
    pub stamp_gaps: u64,
}

/// The first whole second (Unix epoch) at which the per-second grid sits `offset_ns` after the
/// 1970 grid. `1e9 = 30 * 33_333_333 + 10`, so second `B` carries offset `(10 * B) mod interval`;
/// `B = offset/10 + 53 * interval` solves it at a realistic (late-2025) epoch.
pub fn start_second_for_offset(offset_ns: u64) -> u64 {
    (offset_ns % CANVAS_INTERVAL_NS) / 10 + 53 * CANVAS_INTERVAL_NS
}

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

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Approximately standard normal (Irwin–Hall, 12 uniforms).
    fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }

    fn ppm(&mut self, ppm: u64) -> bool {
        self.next() % 1_000_000 < ppm
    }

    fn uniform_ns(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            lo
        } else {
            lo + self.next() % (hi - lo)
        }
    }
}

/// The sender side: strih-lx render ticks -> send -> per-second FLOOR stamp -> arrival.
struct Sender {
    grid: GridModel,
    tick: u64,
    last_send: u64,
    track: StampTrack,
    rng: Rng,
}

impl Sender {
    /// The next frame as `(arrival_wall_ns, stamp_ns, new_dups, new_missing_intervals)`.
    fn next_frame(&mut self, cfg: &BenchConfig) -> (u64, u64, u64, u64) {
        self.tick = self.grid.next_tick(self.tick);
        let core =
            cfg.send_delay_median_ns as f64 + cfg.send_delay_sigma_ns as f64 * self.rng.normal();
        let mut delay = core.max(0.0) as u64;
        if self.rng.ppm(cfg.send_late_ppm) {
            delay = CANVAS_INTERVAL_NS + self.rng.uniform_ns(0, cfg.send_late_extra_max_ns);
        }
        let send = (self.tick + delay).max(self.last_send + 1);
        self.last_send = send;
        let stamp = per_second_floor(send / 100, FPS, UNITS_100NS_PER_SECOND) * 100;
        // The production arrival-side tracker (the C genlock_stamp_track_observe) counts the
        // irregularity the receiver's stamp_dup= / stamp_gap= audit tokens would show.
        let (dups, gaps) = (self.track.dups, self.track.gaps);
        self.track.observe(stamp);
        (
            send + cfg.network_ns,
            stamp,
            self.track.dups - dups,
            self.track.gaps - gaps,
        )
    }
}

/// The receiver's per-source FIFO state — the fields `genlock_release_tick` reads and writes.
#[derive(Default)]
struct Fifo {
    queue: VecDeque<u64>,
    locked_next_boundary: u64,
    anchor: u64,
    ticks_since_drain: u64,
    presented: Option<u64>,
}

/// Counter deltas of one tick (only the post-warm-up ones are reported).
#[derive(Default)]
struct TickCounters {
    holds: u64,
    late_holds: u64,
    resyncs: u64,
    relocks: u64,
    drains: u64,
    dropped_due: u64,
    underruns: u64,
}

impl Fifo {
    /// One render tick: the N==1 branches of the C `genlock_release_tick`, in its order.
    fn tick(&mut self, cfg: &BenchConfig, wall: u64, c: &mut TickCounters) {
        if self.queue.is_empty() {
            c.underruns += 1;
            return;
        }
        let reserve_ns = cfg.latency_ms as u64 * 1_000_000;
        let present_ts = cfg.grid.deadline_floor(wall.saturating_sub(reserve_ns));
        let due = self
            .queue
            .iter()
            .take_while(|&&ts| ts <= present_ts + PHASE_PIN_HYSTERESIS_NS)
            .count();
        let head = self.queue[0];
        let mut drain_eligible = false;
        let mut anchor_update = false;
        let release;
        if self.locked_next_boundary == 0 {
            // ACQUIRE (N==1: no #1161 bracket).
            self.ticks_since_drain = 0;
            if due == 0 {
                c.holds += 1;
                return;
            }
            let q: Vec<u64> = self.queue.iter().copied().collect();
            let age = relock_anchor_age_ns(self.anchor, cfg.latency_ms);
            release = relock_select_nearest(&q, wall, age) + 1;
        } else if self.queue.len() as u64 > backlog_relock_threshold(cfg.latency_ms, 30, 1, 1)
            && due > 0
        {
            // BACKLOG relock (#1003 phase-continuity selection + the stale-anchor re-select).
            c.relocks += 1;
            let q: Vec<u64> = self.queue.iter().copied().collect();
            let mut sel =
                relock_select_nearest(&q, wall, relock_anchor_age_ns(self.anchor, cfg.latency_ms));
            if sel == 0 && self.anchor != 0 {
                self.anchor = 0;
                sel = relock_select_nearest(&q, wall, relock_anchor_age_ns(0, cfg.latency_ms));
            }
            release = sel + 1;
        } else if head <= self.locked_next_boundary {
            // STEADY (N==1 present-oldest).
            release = 1;
            drain_eligible = true;
            anchor_update = true;
        } else if present_ts >= head {
            // GAP RESYNC — upstream skipped a stamp and the next frame has aged past the deadline.
            c.resyncs += 1;
            release = 1;
            anchor_update = true;
        } else {
            // HOLD — late when the boundary itself has aged past the deadline.
            if present_ts >= self.locked_next_boundary {
                c.late_holds += 1;
            } else {
                c.holds += 1;
            }
            return;
        }
        let mut to_drop = release - 1;
        while to_drop > 0 && self.queue.len() > 1 {
            self.queue.pop_front();
            c.dropped_due += 1;
            to_drop -= 1;
        }
        if drain_eligible {
            if should_drain_one(
                self.queue.len() as u64,
                cfg.latency_ms,
                30,
                1,
                self.ticks_since_drain,
            ) && self.queue.len() > 1
            {
                self.queue.pop_front();
                c.dropped_due += 1;
                c.drains += 1;
                self.ticks_since_drain = 0;
            } else {
                self.ticks_since_drain += 1;
            }
        }
        let presented = self
            .queue
            .pop_front()
            .expect("release keeps at least one frame");
        if anchor_update {
            self.anchor = phase_anchor_from_present(wall, presented);
        }
        self.locked_next_boundary = presented + CANVAS_INTERVAL_NS;
        self.presented = Some(presented);
    }
}

/// Run one scenario and report what the receiver did.
pub fn run_bench(cfg: &BenchConfig) -> BenchReport {
    let t0 = start_second_for_offset(cfg.start_offset_ns) * NS_PER_SECOND;
    let end = t0 + cfg.duration_s * NS_PER_SECOND;
    let warm = t0 + cfg.warmup_s * NS_PER_SECOND;
    let mut rng = Rng(cfg.seed);
    let mut sender = Sender {
        grid: cfg.grid,
        tick: t0,
        last_send: 0,
        track: StampTrack::default(),
        rng: Rng(rng.next()),
    };
    let mut fifo = Fifo::default();
    let mut pending: VecDeque<(u64, u64)> = VecDeque::new();
    let mut report = BenchReport::default();
    let mut c = TickCounters::default();
    let mut nominal = cfg.grid.next_tick(t0);
    let mut tick_no: u64 = 0;
    let mut last_sample: Option<u64> = None;
    let mut last_tick_state: Option<u64> = None;
    while nominal < end {
        let mut late = (rng.normal().abs() * cfg.tick_jitter_sigma_ns as f64) as u64;
        if rng.ppm(cfg.tick_late_ppm) {
            late = rng.uniform_ns(cfg.tick_late_min_ns, cfg.tick_late_max_ns);
        }
        let wall = nominal + late;
        // Produce every frame that has arrived by this tick (keep one frame of look-ahead).
        while pending.back().map_or(true, |&(arrival, _)| arrival <= wall) {
            let (arrival, stamp, dup, gap) = sender.next_frame(cfg);
            if arrival >= warm {
                report.stamp_dups += dup;
                report.stamp_gaps += gap;
            }
            pending.push_back((arrival, stamp));
        }
        while pending.front().is_some_and(|&(arrival, _)| arrival <= wall) {
            let (_, stamp) = pending.pop_front().expect("front exists");
            fifo.queue.push_back(stamp);
        }
        let counted = nominal >= warm;
        let mut tc = TickCounters::default();
        fifo.tick(cfg, wall, &mut tc);
        if counted {
            c.holds += tc.holds;
            c.late_holds += tc.late_holds;
            c.resyncs += tc.resyncs;
            c.relocks += tc.relocks;
            c.drains += tc.drains;
            c.dropped_due += tc.dropped_due;
            c.underruns += tc.underruns;
        }
        if let Some(p) = fifo.presented {
            let state = (nominal.saturating_sub(p) + CANVAS_INTERVAL_NS / 2) / CANVAS_INTERVAL_NS;
            if counted {
                if last_tick_state.is_some_and(|s| s != state) {
                    report.tick_state_changes += 1;
                }
                if tick_no % SAMPLE_EVERY_TICKS == 0 {
                    *report.state_samples.entry(state).or_insert(0) += 1;
                    if last_sample.is_some_and(|s| s != state) {
                        report.flips += 1;
                    }
                    last_sample = Some(state);
                }
            }
            last_tick_state = Some(state);
        }
        tick_no += 1;
        nominal = cfg.grid.next_tick(nominal);
    }
    report.hours = cfg.duration_s.saturating_sub(cfg.warmup_s) as f64 / 3600.0;
    report.flips_per_hour = if report.hours > 0.0 {
        report.flips as f64 / report.hours
    } else {
        0.0
    };
    report.holds = c.holds;
    report.late_holds = c.late_holds;
    report.resyncs = c.resyncs;
    report.relocks = c.relocks;
    report.drains = c.drains;
    report.dropped_due = c.dropped_due;
    report.underruns = c.underruns;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(r: &BenchReport, states: &[u64]) -> f64 {
        let total: u64 = r.state_samples.values().sum();
        let hit: u64 = states
            .iter()
            .map(|s| r.state_samples.get(s).copied().unwrap_or(0))
            .sum();
        hit as f64 / total.max(1) as f64
    }

    #[test]
    fn start_second_carries_the_requested_offset_1355() {
        for off in [300_000u64, 2_000_000, 16_000_000, 32_000_000] {
            let t = start_second_for_offset(off) * NS_PER_SECOND;
            let legacy_floor = (t / CANVAS_INTERVAL_NS) * CANVAS_INTERVAL_NS;
            assert_eq!(t - legacy_floor, off, "offset {off}");
        }
    }

    /// The bench reproduces the live defect on the pre-#1355 arithmetic: the 2ME PGM FIFO lives in
    /// depth states 31/32 and flips between them inside the measured 10–45 flips/h band, driven by
    /// late holds (up) and settle-back drains (down).
    #[test]
    fn legacy_1970_grid_reproduces_the_live_31_32_flip_1355() {
        let r = run_bench(&BenchConfig::live_2026_09_24(GridModel::Legacy1970));
        assert!(
            (10.0..=45.0).contains(&r.flips_per_hour),
            "legacy flips/h {:.1} outside the live 10-45 band: {r:?}",
            r.flips_per_hour
        );
        assert!(share(&r, &[31, 32]) > 0.95, "states {:?}", r.state_samples);
        assert!(
            share(&r, &[31]) > 0.1 && share(&r, &[32]) > 0.1,
            "{:?}",
            r.state_samples
        );
        assert!(r.late_holds > 0 && r.drains > 0, "{r:?}");
        assert_eq!(r.relocks, 0, "no backlog storm in the live data: {r:?}");
    }

    /// The fix: the SAME inputs on the production grid do not walk. After warm-up the FIFO keeps one
    /// depth state (no 5 s-sampled flip), with no settle-back drain.
    #[test]
    fn production_grid_holds_one_depth_state_1355() {
        let r = run_bench(&BenchConfig::live_2026_09_24(GridModel::Production));
        assert!(
            r.flips_per_hour <= 1.0,
            "production flips/h {:.2} — the depth still walks: {r:?}",
            r.flips_per_hour
        );
        assert_eq!(r.drains, 0, "{r:?}");
        assert!(
            r.stamp_dups > 0 && r.stamp_gaps > 0,
            "the bench must still inject the sender irregularity: {r:?}"
        );
    }

    /// The date-walk: on the 1970 grid the result depends on where the calendar date puts the stamp
    /// offset; on the production grid it does not.
    #[test]
    fn production_grid_is_independent_of_the_date_1355() {
        for off in [300_000u64, 11_000_000, 22_000_000, 32_000_000] {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.start_offset_ns = off;
            cfg.duration_s = 2 * 3600;
            let r = run_bench(&cfg);
            assert!(
                r.flips_per_hour <= 1.0 && r.drains == 0,
                "offset {off}: production flips/h {:.2}: {r:?}",
                r.flips_per_hour
            );
        }
    }
}
