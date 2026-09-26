//! Issue 1372 — the two-clock WALL-STEP bench: the first coordinated dantesync fleet date step,
//! replayed against the three production decisions it hit.
//!
//! The logged event (25.9.2026 23:17:07 UTC, issue 1372 comment 5840984909): the fleet date
//! stepped −51.039 ms. The media clock (`os_gettime_ns`, issue 1372 part A) follows the dantesync
//! RATE and never a step, so it stayed continuous, and so did the Dante audio clock. At the step:
//!
//! - the render tick slewed back to the stepped wall grid at 2 ms per tick, and the Windows NDI
//!   sender (which floors the wall clock at emit) stamped off phase meanwhile;
//! - resolume's genlock LOCK went DEGRADED `qpc_drift` for the whole 300 s window;
//! - the stream `mbc` ASRC lost 44 ms of Dante samples (a REAL loss, upstream of OBS — the STEP 0
//!   finding, issuecomment-5841032894), and the proportional ±100 ppm restore took 3–4 min.
//!
//! Each section drives the SAME production functions the vendored C mirrors (parity-gated):
//! [`crate::genlock_wall_step`] (the render tick), [`crate::genlock_lock_state`] (the widget's
//! step verdict and re-baseline) and [`crate::asrc_bench::RealtimeAsrcCompensator`] (the ASRC).
//! The requirements (ROZHODNUTÉ 5841039244): a one-tick sender re-grid, no DEGRADED, the `mbc`
//! level back within ±2 ms of its target within 60 s, and no clamp change without a confirmed step.
//! The legacy render tick and the legacy widget run on the same feed and must fail, so neither
//! requirement can pass vacuously; for the ASRC the legacy proportional restore is the RED run of
//! this bench (60 s after the step the level was still up to ~36 ms off its target).

use crate::asrc_bench::{RealtimeAsrcCompensator, STEP_RECOVER_PPM};
use crate::genlock_grid::{
    grid_floor_ns, grid_next_boundary_ns, per_second_floor, NS_PER_SECOND, UNITS_100NS_PER_SECOND,
};
use crate::genlock_grid_bench::{BenchConfig, Fifo, GridModel, TickCounters};
use crate::genlock_lock_state::{
    qpc_drift_beyond_bound, qpc_wall_step_rebase_ms, GENLOCK_QPC_STEP_BOUND_MS,
    GENLOCK_QPC_WALL_STEPS_PER_WINDOW, GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS, GENLOCK_QPC_WINDOW_S,
};
use crate::genlock_wall_step::{deadline_ns, WallStepState};
use std::collections::VecDeque;

/// The logged fleet date step, ns (dantesync `date_step_pending_ns=-51039000`).
const STEP_NS: i64 = -51_039_000;

/// A deterministic LCG in `[0, 1)` (the same generator as the ASRC parity driver).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / 9_007_199_254_740_992.0
    }

    /// A uniform integer in `[lo, hi)` ns.
    fn ns(&mut self, lo: u64, hi: u64) -> u64 {
        lo + ((hi - lo) as f64 * self.next()) as u64
    }
}

// ---- 1. the render tick + the NDI sender stamp + a receiver FIFO ---------------------------------

const INTERVAL_NS: u64 = 33_333_333; // the 30 fps canvas
const MONO0: u64 = 50_000_000_000_000; // ~14 h since boot
const WALL0: u64 = 1_790_378_167_000_000_000; // 60 s before the logged step
const STEP_AT_MONO: u64 = MONO0 + 60_000_000_000;

/// The wall clock at a monotonic instant, `step_ns` applied from `STEP_AT_MONO` on. A coordinated
/// fleet date step applies the SAME step on every box at (to the ms) the same instant.
fn wall_at(mono: u64, step_ns: i64) -> u64 {
    let wall = WALL0 + (mono - MONO0);
    if mono >= STEP_AT_MONO {
        wall.wrapping_add(step_ns as u64)
    } else {
        wall
    }
}

/// Grid slots from stamp `a` to stamp `b` (ns, both on the per-second grid; negative = backward).
fn slots_between(a: u64, b: u64) -> i64 {
    ((b as i64 - a as i64) as f64 / INTERVAL_NS as f64).round() as i64
}

/// The sender's emit delay after the tick fires (render → video-io → DistroAV `send_video`), ns.
/// TIGHT is an unloaded box; WIDE spreads it over most of a frame, like the video-io lag the
/// resolume `cg-obs` log shows (comment 5841262854). On the wide spread an off-phase tick puts
/// emits on both sides of a grid boundary, which is how the legacy slew walks the stamps off phase.
const EMIT_TIGHT: (u64, u64) = (2_000_000, 8_000_000);
const EMIT_WIDE: (u64, u64) = (0, 30_000_000);

/// Signed distance (ns) of a wall instant from its nearest grid point.
fn grid_phase_err(wall: u64) -> i64 {
    let nearest = grid_floor_ns(wall + INTERVAL_NS / 2, INTERVAL_NS);
    wall.wrapping_sub(nearest) as i64
}

/// One box's render tick: obs-video.c `genlock_next_deadline` (a bracketed mono/wall/mono read →
/// the detector → the wall-grid target → [`WallStepState::regrid_due`] → [`deadline_ns`]) followed
/// by `video_sleep`: a deadline still ahead of the clock is slept to; one already past falls back to
/// `cur_time + interval · count`, the OLD grid. `regrid = false` is the legacy tick (the 2 ms clamp
/// even across a step).
struct RenderTick {
    detector: WallStepState,
    regrid: bool,
    /// The deadline this tick was scheduled for (the C `*p_time`).
    sched: u64,
    /// Extra time between deciding the deadline and `os_sleepto_ns` on the tick that detects the
    /// step (the `genlock-regrid:` log write, a preemption): a re-grid target closer than this is
    /// already past when the sleep samples the clock.
    late_on_step: u64,
}

impl RenderTick {
    fn new(regrid: bool, step_ns: i64) -> Self {
        let first_wall = wall_at(MONO0, step_ns);
        RenderTick {
            detector: WallStepState::new(),
            regrid,
            sched: MONO0 + (grid_next_boundary_ns(first_wall, INTERVAL_NS) - first_wall),
            late_on_step: 0,
        }
    }

    /// Compute the next deadline at `call` (the end of this tick's render) and sleep to it.
    fn advance(&mut self, call: u64, step_ns: i64) {
        let mono_before = call;
        let wall = wall_at(call + 1_000, step_ns);
        let mono = call + 2_000;
        let step = self.detector.observe(mono_before, wall, mono);
        let target = mono + (grid_next_boundary_ns(wall, INTERVAL_NS) - wall);
        let stock = self.sched + INTERVAL_NS;
        let regrid = self.detector.regrid_due(step, target, stock) && self.regrid;
        let deadline = deadline_ns(target, stock, regrid);
        // video_sleep: os_sleepto_ns samples the clock after the (possible) log line
        let now = mono + 50_000 + if step != 0 { self.late_on_step } else { 0 };
        self.sched = if deadline > now {
            deadline
        } else {
            let count = ((now - self.sched) / INTERVAL_NS).max(1);
            self.sched + INTERVAL_NS * count
        };
    }
}

#[derive(Debug)]
struct TickRun {
    /// Scheduled sender ticks after the step whose wall instant is more than 100 µs off the grid.
    off_grid_ticks: usize,
    /// Scheduled sender ticks after the step until the first on-grid one (inclusive).
    ticks_to_regrid: usize,
    /// Stamp intervals of 0 slots after the step (a repeated stamp).
    dups: usize,
    /// Stamp intervals of 2+ slots after the step (a skipped slot).
    gaps: usize,
    /// The one stamp interval that carries the step itself, in slots (negative = backward).
    step_interval_slots: i64,
    /// Wall steps the sender's detector counted.
    steps: u64,
    /// Receiver FIFO counters in the 30 s after the step, minus the same seed's no-step run.
    receiver: Option<ReceiverCost>,
}

/// What a receiver FIFO put on air in the 30 s after the step, as the viewer sees it: ticks that
/// presented nothing (the previous frame stays on air), presents whose stamp is not newer than the
/// previous one's slot (a backward / repeated frame), and grid slots skipped between two presents;
/// plus the relocks and underruns the FIFO counted. Compared against the identical run with no step.
/// (The FIFO's own `resyncs` counter is not used: on the per-second grid every third stamp interval
/// is 100 ns longer than `interval`, which the N==1 port books as a GAP RESYNC with a normal one-frame
/// release, ~10 per second with or without a step.)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReceiverCost {
    held_ticks: i64,
    backward_presents: i64,
    skipped_slots: i64,
    relocks: i64,
    underruns: i64,
}

impl ReceiverCost {
    /// Frames the viewer sees out of order: held, repeated/backward, or skipped.
    fn visible(&self) -> i64 {
        self.held_ticks + self.backward_presents + self.skipped_slots
    }
}

/// Run a sender box and (optionally) a receiver box 120 s with the coordinated step at 60 s.
/// Sender: each tick fires at its deadline + 0–200 µs, renders 3–10 ms (on the tick that detects
/// the step, `stall_ns` passes between the deadline decision and the sleep, so a re-grid target
/// closer than that is already past when the sleep samples the clock), and the NDI sender floors the wall clock at emit, `emit_ns` after the fire, like DistroAV
/// `genlock_emit_timecode_100ns`. Receiver (`receiver_latency_ms`): the production N==1 FIFO port
/// ([`Fifo`], the issue-1355 grid bench) fed those stamps 1–3 ms after emit, on its own render tick
/// with its own detector, the same wall step at the same instant. `regrid` switches BOTH ticks.
fn run_boxes(
    regrid: bool,
    step_ns: i64,
    stall_ns: u64,
    receiver_latency_ms: Option<u32>,
    emit_ns: (u64, u64),
) -> TickRun {
    let mut rng = Lcg(0x1372);
    let mut tx = RenderTick::new(regrid, step_ns);
    tx.late_on_step = stall_ns;
    let mut stamps: Vec<(u64, u64)> = Vec::new(); // (sched mono, stamp 100 ns)
    let mut in_flight: VecDeque<(u64, u64)> = VecDeque::new(); // (arrival mono, stamp ns)
    let mut off_grid_ticks = 0;
    let mut ticks_to_regrid = 0;
    let mut regridded = false;
    let mut rx = RenderTick::new(regrid, step_ns);
    let mut rx_rng = Lcg(0x1372_00ff);
    let mut fifo = Fifo::default();
    let rx_cfg = receiver_latency_ms.map(|ms| {
        let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
        cfg.latency_ms = ms;
        cfg
    });
    let mut cost = ReceiverCost::default();
    let end = MONO0 + 120 * NS_PER_SECOND;
    while tx.sched < end {
        if tx.sched >= STEP_AT_MONO {
            let off = grid_phase_err(wall_at(tx.sched, step_ns)).unsigned_abs() > 100_000;
            if off {
                off_grid_ticks += 1;
            }
            if !regridded {
                ticks_to_regrid += 1;
                regridded = !off;
            }
        }
        let fire = tx.sched + rng.ns(0, 200_000);
        let emit = fire + rng.ns(emit_ns.0, emit_ns.1);
        let stamp = per_second_floor(wall_at(emit, step_ns) / 100, 30, UNITS_100NS_PER_SECOND);
        stamps.push((tx.sched, stamp));
        in_flight.push_back((emit + rng.ns(1_000_000, 3_000_000), stamp * 100));
        let call = fire + rng.ns(3_000_000, 10_000_000);
        tx.advance(call, step_ns);
        // the receiver ticks up to the sender's next fire, taking every frame that has arrived
        if let Some(cfg) = rx_cfg.as_ref() {
            while rx.sched < tx.sched {
                let rx_fire = rx.sched + rx_rng.ns(0, 300_000);
                while in_flight
                    .front()
                    .is_some_and(|&(arrival, _)| arrival <= rx_fire)
                {
                    let (_, st) = in_flight.pop_front().expect("front exists");
                    fifo.queue.push_back(st);
                }
                let wall = wall_at(rx_fire, step_ns);
                let scheduled = rx.sched.wrapping_add(wall.wrapping_sub(rx_fire));
                let before = fifo.presented;
                let mut tc = TickCounters::default();
                fifo.tick(cfg, wall, scheduled, &mut tc);
                if rx.sched >= STEP_AT_MONO && rx.sched < STEP_AT_MONO + 30 * NS_PER_SECOND {
                    cost.relocks += tc.relocks as i64;
                    cost.underruns += tc.underruns as i64;
                    match (before, fifo.presented_now) {
                        (_, false) => cost.held_ticks += 1,
                        (Some(prev), true) => {
                            let now = fifo.presented.expect("a present sets it");
                            match slots_between(prev, now) {
                                d if d <= 0 => cost.backward_presents += 1,
                                d => cost.skipped_slots += d - 1,
                            }
                        }
                        (None, true) => {}
                    }
                }
                rx.advance(rx_fire + rx_rng.ns(3_000_000, 10_000_000), step_ns);
            }
        } else {
            in_flight.clear();
        }
    }
    let slot = UNITS_100NS_PER_SECOND / 30;
    let slots = |a: u64, b: u64| ((b as i64 - a as i64) as f64 / slot as f64).round() as i64;
    let first_after = stamps
        .iter()
        .position(|(s, _)| *s >= STEP_AT_MONO)
        .expect("the run reaches the step");
    let step_interval_slots = slots(stamps[first_after - 1].1, stamps[first_after].1);
    let (mut dups, mut gaps) = (0, 0);
    for w in stamps[first_after..].windows(2) {
        match slots(w[0].1, w[1].1) {
            s if s <= 0 => dups += 1,
            s if s >= 2 => gaps += 1,
            _ => {}
        }
    }
    let receiver = receiver_latency_ms.map(|_| cost);
    TickRun {
        off_grid_ticks,
        ticks_to_regrid,
        dups,
        gaps,
        step_interval_slots,
        steps: tx.detector.steps(),
        receiver,
    }
}

/// The step's cost to a receiver: the step run minus the identical run with no step.
fn receiver_cost(regrid: bool, step_ns: i64, latency_ms: u32) -> ReceiverCost {
    let with = run_boxes(regrid, step_ns, 0, Some(latency_ms), EMIT_WIDE)
        .receiver
        .expect("receiver ran");
    let without = run_boxes(regrid, 0, 0, Some(latency_ms), EMIT_WIDE)
        .receiver
        .expect("receiver ran");
    ReceiverCost {
        held_ticks: with.held_ticks - without.held_ticks,
        backward_presents: with.backward_presents - without.backward_presents,
        skipped_slots: with.skipped_slots - without.skipped_slots,
        relocks: with.relocks - without.relocks,
        underruns: with.underruns - without.underruns,
    }
}

#[test]
fn the_render_tick_and_the_sender_regrid_in_one_tick_1372() {
    for step_ns in [STEP_NS, -STEP_NS] {
        let prod = run_boxes(true, step_ns, 0, None, EMIT_TIGHT);
        assert_eq!(
            prod.steps, 1,
            "the detector must count the one step: {prod:?}"
        );
        // The one tick already scheduled when the wall stepped lands off the new grid; the tick
        // after it is back on the grid.
        assert_eq!(
            (prod.ticks_to_regrid, prod.off_grid_ticks),
            (2, 1),
            "issue 1372: the render tick must re-grid in ONE tick after a {step_ns} ns step: {prod:?}"
        );
        // After the stamp interval that carries the step itself, one slot per frame.
        assert_eq!(
            (prod.dups, prod.gaps),
            (0, 0),
            "issue 1372: after the step the sender must stamp one slot per frame: {prod:?}"
        );
        // Anti-tautology: the legacy 2 ms/tick clamp on the SAME feed walks the tick back over
        // ~8-9 ticks (51 ms mod one 33.3 ms frame at 2 ms per tick), off phase meanwhile.
        let legacy = run_boxes(false, step_ns, 0, None, EMIT_TIGHT);
        assert!(
            legacy.off_grid_ticks >= 7 && legacy.ticks_to_regrid >= 8,
            "the legacy clamp must slew over >= 7 off-grid ticks on this feed: {legacy:?}"
        );
    }
    // On the wide emit spread the legacy slew walks the stamps off phase (a repeated and a skipped
    // stamp while the tick is off the grid); the one-tick re-grid does not.
    for step_ns in [STEP_NS, -STEP_NS] {
        let prod = run_boxes(true, step_ns, 0, None, EMIT_WIDE);
        let legacy = run_boxes(false, step_ns, 0, None, EMIT_WIDE);
        assert_eq!(
            (prod.dups, prod.gaps),
            (0, 0),
            "issue 1372: a wide emit spread must not put a repeated/skipped stamp after the re-grid: \
             {prod:?}"
        );
        assert!(
            legacy.dups + legacy.gaps >= 2,
            "the legacy slew must stamp off phase on the wide emit spread: {legacy:?}"
        );
    }
    // The step interval itself is inherent to a date step on EVERY sender (the wall moved): the
    // stamp after a −51 ms step repeats an earlier slot (backward), after +51 ms it skips a slot.
    assert!(run_boxes(true, STEP_NS, 0, None, EMIT_TIGHT).step_interval_slots <= 0);
    assert!(run_boxes(true, -STEP_NS, 0, None, EMIT_TIGHT).step_interval_slots >= 2);
}

#[test]
fn a_regrid_whose_target_passed_during_a_stall_still_lands_1372() {
    // On the tick that detects the step, 45 ms (more than a frame) pass between the deadline decision
    // and the sleep: the re-grid target is already past when os_sleepto_ns samples the clock, and
    // video_sleep falls back to `cur_time + interval · count` on the OLD grid. The pending re-grid
    // still lands on the next tick instead of degrading to the 2 ms slew.
    let prod = run_boxes(true, STEP_NS, 45_000_000, None, EMIT_TIGHT);
    assert!(
        prod.ticks_to_regrid <= 3 && prod.off_grid_ticks <= 2,
        "issue 1372: a stalled re-grid must still land within one more tick: {prod:?}"
    );
    let legacy = run_boxes(false, STEP_NS, 45_000_000, None, EMIT_TIGHT);
    assert!(
        legacy.off_grid_ticks >= 7,
        "the legacy clamp slews after the stall too: {legacy:?}"
    );
}

#[test]
fn a_receiver_fifo_pays_the_step_once_and_no_more_than_the_slew_did_1372() {
    // The coordinated step reaching a receiver FIFO: the deep stream `NDI 2ME PGM` (pin 987 ms) and a
    // shallow cg feed (3 ms), on the wide emit spread. Both boxes step together, so the receiver's
    // present deadline and the sender's stamps move by the same 51 ms. What remains is inherent: the
    // stamp interval that carries the step is 1.5 frames long (backward after −51 ms: a repeated
    // frame; forward after +51 ms: a skipped one), plus the one tick in which the receiver's own
    // render tick changes phase. The legacy slew pays more on the deep FIFO (its off-phase stamps
    // add a repeat and a skip); on the shallow one the receiver's re-grid tick costs a held frame the
    // legacy slew spreads out, and the sender side pays that back.
    for latency_ms in [987u32, 3] {
        for step_ns in [STEP_NS, -STEP_NS] {
            let prod = receiver_cost(true, step_ns, latency_ms);
            let legacy = receiver_cost(false, step_ns, latency_ms);
            assert_eq!(
                (prod.relocks, prod.underruns),
                (0, 0),
                "issue 1372: pin {latency_ms} ms, step {step_ns} ns: the step must not relock or \
                 underrun the receiver: {prod:?}"
            );
            assert!(
                prod.visible() <= 5 && prod.visible() <= legacy.visible(),
                "issue 1372: pin {latency_ms} ms, step {step_ns} ns: the step may cost the viewer at \
                 most the inherent 1.5-frame jump plus one re-grid tick, and never more than the \
                 legacy slew: prod {prod:?} legacy {legacy:?}"
            );
        }
    }
    // Anti-tautology: the deep FIFO on the legacy slew pays strictly more for the same step.
    let prod = receiver_cost(true, STEP_NS, 987);
    let legacy = receiver_cost(false, STEP_NS, 987);
    assert!(
        prod.visible() < legacy.visible(),
        "the legacy slew must cost the deep FIFO more: prod {prod:?} legacy {legacy:?}"
    );
}

// ---- 2. the LOCK indicator's qpc_drift step verdict ---------------------------------------------

#[derive(Debug)]
struct WidgetRun {
    /// Seconds the qpc_drift verdict read DEGRADED.
    degraded_s: usize,
    /// Steps the widget booked (one `genlock-wall-step:` line each).
    booked: usize,
}

/// Run the widget's 1 Hz qpc loop (OBSBasicStatusBar.cpp) for 900 s over libobs'
/// `wall_qpc_drift_ms` (integer ms, cumulative since start) with the given wall steps
/// `(second, ms)`. `rebase = false` is the legacy widget (no booking).
fn run_widget(steps: &[(i64, i64)], rebase: bool) -> WidgetRun {
    let mut history: VecDeque<(i64, i64)> = VecDeque::new();
    let mut booked_at: VecDeque<i64> = VecDeque::new();
    let mut degraded_s = 0;
    let mut booked = 0;
    for t in 0..900_i64 {
        let now_ms = t * 1000;
        let qpc: i64 = steps
            .iter()
            .filter(|(at, _)| *at <= t)
            .map(|(_, ms)| ms)
            .sum();
        while booked_at
            .front()
            .is_some_and(|&b| now_ms - b > GENLOCK_QPC_WINDOW_S * 1000)
        {
            booked_at.pop_front();
        }
        if rebase {
            if let Some(&(_, back)) = history.back() {
                let r = qpc_wall_step_rebase_ms(
                    qpc - back,
                    GENLOCK_QPC_STEP_BOUND_MS,
                    GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS,
                    booked_at.len() as i64,
                    GENLOCK_QPC_WALL_STEPS_PER_WINDOW,
                );
                if r != 0 {
                    for s in history.iter_mut() {
                        s.1 += r;
                    }
                    booked_at.push_back(now_ms);
                    booked += 1;
                }
            }
        }
        history.push_back((now_ms, qpc));
        while history.len() > 1
            && history
                .front()
                .is_some_and(|&(ms, _)| now_ms - ms > GENLOCK_QPC_WINDOW_S * 1000)
        {
            history.pop_front();
        }
        let (mut max_step, mut prev) = (0_i64, history.front().map_or(0, |s| s.1));
        for &(_, v) in &history {
            max_step = max_step.max((v - prev).abs());
            prev = v;
        }
        let oldest = history.front().copied().unwrap_or((now_ms, qpc));
        let elapsed = now_ms - oldest.0;
        let verdict = qpc_drift_beyond_bound(
            elapsed >= GENLOCK_QPC_WINDOW_S * 1000 * 9 / 10,
            qpc - oldest.1,
            elapsed,
            max_step,
            GENLOCK_QPC_STEP_BOUND_MS,
        );
        if verdict.beyond_bound {
            degraded_s += 1;
        }
    }
    WidgetRun { degraded_s, booked }
}

#[test]
fn the_date_step_never_degrades_the_lock_indicator_1372() {
    let logged = [(300, -51)];
    let prod = run_widget(&logged, true);
    assert_eq!(
        (prod.degraded_s, prod.booked),
        (0, 1),
        "issue 1372: the logged date step must be booked once and never DEGRADE: {prod:?}"
    );
    // Anti-tautology: the legacy widget reads it DEGRADED for the whole 300 s window (the live
    // resolume `qpc_drift` from the step on).
    let legacy = run_widget(&logged, false);
    assert!(
        legacy.degraded_s >= 299,
        "the legacy widget must DEGRADE for the window: {legacy:?}"
    );
}

#[test]
fn a_step_storm_and_a_clock_set_still_degrade_1372() {
    // a second step 60 s after the first stays in the history for its whole window
    let storm = run_widget(&[(300, -51), (360, 40)], true);
    assert_eq!(
        storm.booked, 1,
        "only one step per window is booked: {storm:?}"
    );
    assert!(
        storm.degraded_s >= 299,
        "a step storm must DEGRADE: {storm:?}"
    );
    // a clock SET (one hour) is no date step
    let set = run_widget(&[(300, -3_600_000)], true);
    assert_eq!(set.booked, 0, "{set:?}");
    assert!(set.degraded_s >= 299, "a clock set must DEGRADE: {set:?}");
    // two date steps a full window apart are both booked
    let apart = run_widget(&[(100, -51), (500, -49)], true);
    assert_eq!((apart.degraded_s, apart.booked), (0, 2), "{apart:?}");
}

// ---- 3. the stream `mbc` ASRC level --------------------------------------------------------------

const MBC_TARGET_MS: f64 = 113.0; // live: 100 ms absolute + the +13 ms sync offset
const EVENT_AT_S: f64 = 1200.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum MbcEvent {
    /// The logged event: 44 ms of Dante samples never delivered (the mixer drains the gap).
    Loss(f64),
    /// A master-clock-only jump of this many ms: no sample is lost and no buffer moves.
    MasterOnly(f64),
    /// Nothing happens.
    None,
}

#[derive(Debug)]
struct MbcRun {
    /// Largest |1 s mean level − target| from the event + 60 s to the end.
    settled_err_ms: f64,
    /// The captured setpoint at the end (the owed recovery moved it and must have returned it).
    final_target_ms: f64,
    /// Still owed at the event + 60 s.
    owed_at_60s_ms: f64,
    /// Largest |recovery rate| before the event / over the whole run, ppm.
    recover_ppm_before: f64,
    recover_ppm_max: f64,
    /// Largest |servo applied| in the minute before the event / after it, ppm.
    applied_before: f64,
    applied_after: f64,
    /// Whether the proportional restore burst ever ran after the event.
    restore_after: bool,
}

/// The live stream `mbc` operating point, like the ASRC parity `tick` scenario: 128-sample Dante
/// callbacks with 1 ms of bursty delivery into a buffer the mixer drains in 1024-sample (21.33 ms)
/// ticks. Since dantesync 1.9.0 + the disciplined media clock the source and the mixer tick
/// together (0 ppm). The resampler output of every callback is the compensator's corrected advance,
/// which carries the servo's `applied_ppm` AND the recovery rate.
fn run_mbc(event: MbcEvent) -> MbcRun {
    let block_s = 128.0 / 48000.0;
    let tick_ms = 1024.0 / 48000.0 * 1000.0;
    let tick_s = tick_ms / 1000.0;
    let mut rng = Lcg(0x1372_0044);
    let mut c = RealtimeAsrcCompensator::new();
    c.set_level_offset_ms(MBC_TARGET_MS - 100.0);
    let mut buffer_ms = MBC_TARGET_MS + tick_ms / 2.0;
    let mut t = 0.0_f64;
    let mut next_tick_s = tick_s;
    let mut prev_jitter = 0.0_f64;
    let mut event_done = false;
    let mut extra_master_s = 0.0_f64;
    let (mut sec_sum, mut sec_n, mut sec_end) = (0.0_f64, 0_u32, 1.0_f64);
    let mut run = MbcRun {
        settled_err_ms: 0.0,
        final_target_ms: 0.0,
        owed_at_60s_ms: f64::NAN,
        recover_ppm_before: 0.0,
        recover_ppm_max: 0.0,
        applied_before: 0.0,
        applied_after: 0.0,
        restore_after: false,
    };
    while t < EVENT_AT_S + 900.0 {
        if !event_done && t >= EVENT_AT_S {
            event_done = true;
            match event {
                MbcEvent::Loss(ms) => {
                    // no callback for `ms`: the mixer keeps draining, and the next callback's master
                    // block spans the gap while its raw advance is one normal block
                    t += ms / 1000.0;
                    extra_master_s = ms / 1000.0;
                }
                MbcEvent::MasterOnly(ms) => extra_master_s = ms / 1000.0,
                MbcEvent::None => {}
            }
        }
        // the callback arrives up to 1 ms late (bursty delivery); its master block is the time since
        // the previous arrival, plus a gap the event put in front of it
        let jitter = rng.next() * 0.001;
        let arrival_s = block_s + jitter - prev_jitter;
        prev_jitter = jitter;
        let master_s = arrival_s + extra_master_s;
        extra_master_s = 0.0;
        t += arrival_s;
        while next_tick_s <= t {
            buffer_ms -= tick_ms;
            next_tick_s += tick_s;
        }
        // the level is what the servo reads: the depth before this callback's samples land
        sec_sum += buffer_ms;
        sec_n += 1;
        let corrected_s = c.compensate_with_level(block_s, master_s, buffer_ms);
        buffer_ms += corrected_s * 1000.0;

        let after = t >= EVENT_AT_S;
        run.recover_ppm_max = run.recover_ppm_max.max(c.step_recover_ppm().abs());
        if !after {
            run.recover_ppm_before = run.recover_ppm_before.max(c.step_recover_ppm().abs());
            if t >= EVENT_AT_S - 60.0 {
                run.applied_before = run.applied_before.max(c.applied_ppm().abs());
            }
        } else {
            run.applied_after = run.applied_after.max(c.applied_ppm().abs());
            run.restore_after |= c.level_restore();
            if run.owed_at_60s_ms.is_nan() && t >= EVENT_AT_S + 60.0 {
                run.owed_at_60s_ms = c.step_recover_ms();
            }
        }
        if t >= sec_end {
            let mean = sec_sum / f64::from(sec_n);
            if sec_end >= EVENT_AT_S + 60.0 {
                run.settled_err_ms = run.settled_err_ms.max((mean - MBC_TARGET_MS).abs());
            }
            sec_sum = 0.0;
            sec_n = 0;
            sec_end += 1.0;
        }
    }
    run.final_target_ms = c.level_target_ms();
    run
}

#[test]
fn the_mbc_loss_is_recovered_within_60_s_1372() {
    let r = run_mbc(MbcEvent::Loss(44.0));
    assert!(
        r.settled_err_ms <= 2.0,
        "issue 1372: the mbc level must be back within ±2 ms of its target from the step + 60 s on: {r:?}"
    );
    assert!(
        r.owed_at_60s_ms == 0.0 && (r.final_target_ms - MBC_TARGET_MS).abs() < 1e-6,
        "issue 1372: the whole 44 ms must be paid back and the setpoint returned: {r:?}"
    );
    assert!(
        (r.recover_ppm_max - STEP_RECOVER_PPM).abs() < 1e-6 && r.recover_ppm_before == 0.0,
        "issue 1372: the recovery runs at exactly STEP_RECOVER_PPM and never before the step: {r:?}"
    );
    // The servo itself stays in its steady band: the recovery is a separate term, and the booked
    // setpoint keeps the level loop (P, restore burst) from reading the loss as an error.
    assert!(
        !r.restore_after && r.applied_after <= r.applied_before.max(1.0) + 1.0,
        "issue 1372: the servo must not slew outside its steady band for a booked step: {r:?}"
    );
}

#[test]
fn no_clamp_change_without_a_confirmed_step_1372() {
    for event in [
        MbcEvent::None,
        // below the 10 ms re-base threshold: the ordinary level loop absorbs it
        MbcEvent::Loss(5.0),
        // a master-clock-only jump: the step re-bases, but the buffer does not corroborate a loss
        MbcEvent::MasterOnly(44.0),
    ] {
        let r = run_mbc(event);
        assert_eq!(
            r.recover_ppm_max, 0.0,
            "issue 1372: {event:?} must never engage the 1000 ppm recovery: {r:?}"
        );
    }
}
