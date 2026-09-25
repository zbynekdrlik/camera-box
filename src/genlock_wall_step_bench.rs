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
    grid_floor_ns, grid_next_boundary_ns, per_second_floor, UNITS_100NS_PER_SECOND,
};
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

// ---- 1. the render tick + the NDI sender stamp --------------------------------------------------

const INTERVAL_NS: u64 = 33_333_333; // the 30 fps canvas
const MONO0: u64 = 50_000_000_000_000; // ~14 h since boot
const WALL0: u64 = 1_790_378_167_000_000_000; // 60 s before the logged step
const STEP_AT_MONO: u64 = MONO0 + 60_000_000_000;

/// The wall clock at a monotonic instant: continuous until the step, then −51 ms.
fn wall_at(mono: u64) -> u64 {
    let wall = WALL0 + (mono - MONO0);
    if mono >= STEP_AT_MONO {
        wall.wrapping_add(STEP_NS as u64)
    } else {
        wall
    }
}

/// Signed distance (ns) of a wall instant from its nearest grid point.
fn grid_phase_err(wall: u64) -> i64 {
    let nearest = grid_floor_ns(wall + INTERVAL_NS / 2, INTERVAL_NS);
    wall.wrapping_sub(nearest) as i64
}

#[derive(Debug)]
struct TickRun {
    /// Scheduled ticks after the step whose wall instant is more than 100 µs off the grid.
    off_grid_ticks: usize,
    /// Scheduled ticks after the step until the first on-grid one (inclusive of the off ones).
    ticks_to_regrid: usize,
    /// Stamp intervals of 0 slots after the step (a repeated stamp).
    dups: usize,
    /// Stamp intervals of 2+ slots after the step (a skipped slot).
    gaps: usize,
    /// Wall steps the detector counted.
    steps: u64,
}

/// Run the render tick 120 s with the step at 60 s. Each tick fires at its deadline + 0–200 µs,
/// renders for 3–10 ms and then computes the next deadline exactly like obs-video.c
/// `genlock_next_deadline` (a bracketed mono/wall/mono read → the detector → the wall-grid target
/// → [`deadline_ns`]); the sender floors the wall clock at emit, 2–8 ms after the fire, like
/// DistroAV `genlock_emit_timecode_100ns`. `regrid = false` is the legacy tick (the 2 ms clamp
/// even across a step).
fn run_render_tick(regrid: bool) -> TickRun {
    let mut rng = Lcg(0x1372);
    let mut detector = WallStepState::new();
    let first_wall = wall_at(MONO0);
    let mut sched = MONO0 + (grid_next_boundary_ns(first_wall, INTERVAL_NS) - first_wall);
    let mut stamps: Vec<(u64, u64)> = Vec::new(); // (sched mono, stamp 100 ns)
    let mut off_grid_ticks = 0;
    let mut ticks_to_regrid = 0;
    let mut regridded = false;
    for _ in 0..(120 * 30) {
        if sched >= STEP_AT_MONO {
            let off = grid_phase_err(wall_at(sched)).unsigned_abs() > 100_000;
            if off {
                off_grid_ticks += 1;
            }
            if !regridded {
                ticks_to_regrid += 1;
                regridded = !off;
            }
        }
        let fire = sched + rng.ns(0, 200_000);
        let emit = fire + rng.ns(2_000_000, 8_000_000);
        stamps.push((
            sched,
            per_second_floor(wall_at(emit) / 100, 30, UNITS_100NS_PER_SECOND),
        ));
        let call = fire + rng.ns(3_000_000, 10_000_000);
        let mono_before = call;
        let wall = wall_at(call + 1_000);
        let mono = call + 2_000;
        let step = detector.observe(mono_before, wall, mono);
        let target = mono + (grid_next_boundary_ns(wall, INTERVAL_NS) - wall);
        let stock = sched + INTERVAL_NS;
        sched = deadline_ns(target, stock, regrid && step != 0).max(call);
    }
    // Stamp intervals from the SECOND stamp emitted after the step on: the first one carries the
    // step itself (the wall moved back 51 ms) and is inherent to a date step on every sender.
    let slot = UNITS_100NS_PER_SECOND / 30;
    let after: Vec<u64> = stamps
        .iter()
        .filter(|(s, _)| *s >= STEP_AT_MONO)
        .map(|&(_, st)| st)
        .collect();
    let mut dups = 0;
    let mut gaps = 0;
    for w in after.windows(2).skip(1) {
        let d = w[1] as i64 - w[0] as i64;
        let slots = (d as f64 / slot as f64).round() as i64;
        if slots <= 0 {
            dups += 1;
        } else if slots >= 2 {
            gaps += 1;
        }
    }
    TickRun {
        off_grid_ticks,
        ticks_to_regrid,
        dups,
        gaps,
        steps: detector.steps(),
    }
}

#[test]
fn the_render_tick_and_the_sender_regrid_in_one_tick_1372() {
    let prod = run_render_tick(true);
    assert_eq!(
        prod.steps, 1,
        "the detector must count the one step: {prod:?}"
    );
    // The one tick already scheduled when the wall stepped lands off the new grid; the tick after
    // it is back on the grid.
    assert_eq!(
        prod.ticks_to_regrid, 2,
        "issue 1372: the render tick must re-grid in ONE tick after the step: {prod:?}"
    );
    assert_eq!(
        prod.off_grid_ticks, 1,
        "issue 1372: only the tick scheduled before the step may be off the grid: {prod:?}"
    );
    assert_eq!(
        (prod.dups, prod.gaps),
        (0, 0),
        "issue 1372: after the step the sender must stamp one slot per frame: {prod:?}"
    );

    // Anti-tautology: the legacy 2 ms/tick clamp on the SAME feed walks the tick back over ~9
    // ticks (51 ms mod one 33.3 ms frame = 17.7 ms at 2 ms per tick), off phase meanwhile.
    let legacy = run_render_tick(false);
    assert!(
        legacy.off_grid_ticks >= 8 && legacy.ticks_to_regrid >= 9,
        "the legacy clamp must slew over >= 8 off-grid ticks on this feed: {legacy:?}"
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
