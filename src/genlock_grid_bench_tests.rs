//! The issue-1355 / issue-1367 grid-drift + N==1 depth bench tests (split out of
//! `genlock_grid_bench.rs` to keep both files under the ~1000-line budget).

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

/// The bench reproduces the live defect on the pre-#1355 arithmetic: a stamp that sits 2 ms past
/// the 1970-floored deadline makes an irregular sender stamp cost a LATE HOLD (a visible
/// duplicate, the FIFO one frame deeper), 10–45 times an hour, and every one is later undone by
/// a drain or a phase shed (a visible skip). The 2ME PGM lives in depth states 31/32.
///
/// The late hold is the date-walk mechanism itself, so the control counts it. The live 10–45
/// FLIPS/h were its consequence while only a random late-tick drain undid it; since issue 1367
/// the N==1 depth rule undoes it within a second, so the 5 s-sampled flip count no longer shows
/// the defect while the duplicate + skip pairs still do.
#[test]
fn legacy_1970_grid_reproduces_the_live_31_32_flip_1355() {
    let r = run_bench(&BenchConfig::live_2026_09_24(GridModel::Legacy1970));
    let late_holds_per_hour = r.late_holds as f64 / r.hours;
    assert!(
        (10.0..=45.0).contains(&late_holds_per_hour),
        "legacy late holds/h {late_holds_per_hour:.1} outside the live 10-45 band: {r:?}"
    );
    assert!(share(&r, &[31, 32]) > 0.95, "states {:?}", r.state_samples);
    assert!(
        r.drains + r.converge_sheds > 0,
        "each late hold must be undone by a drain or a shed: {r:?}"
    );
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
    assert_eq!(r.converge_sheds, 0, "{r:?}");
    assert!(
        r.stamp_dups > 0 && r.stamp_gaps > 0,
        "the bench must still inject the sender irregularity: {r:?}"
    );
}

/// The date-walk, stated for what it is. The production grid never sees the 1970-grid offset
/// (its grid is a pure function of the whole second), so two dates give byte-identical runs BY
/// CONSTRUCTION — asserted as equality, not re-proven by re-running a flip bound per date. The
/// 1970 grid on the same two dates is NOT identical: that difference is the date-walk.
#[test]
fn production_grid_does_not_see_the_1970_offset_but_the_1970_grid_does_1355() {
    let run = |grid, off| {
        let mut cfg = BenchConfig::live_2026_09_24(grid);
        cfg.start_offset_ns = off;
        cfg.duration_s = 3600;
        run_bench(&cfg)
    };
    assert_eq!(
        run(GridModel::Production, 300_000),
        run(GridModel::Production, 22_000_000)
    );
    assert_ne!(
        run(GridModel::Legacy1970, 300_000),
        run(GridModel::Legacy1970, 22_000_000)
    );
}

/// What the production grid DOES see: the pin. Across one whole frame of pins (and one each
/// side) the FIFO keeps ONE depth state with no settle-back drain — the state follows the pin
/// (30 / 31 / 32 frames), never the date or a sender hiccup. On the 1970 grid every one of these
/// pins pays the date-walk late hold (~11-22/h over 2 h in the live statistics).
#[test]
fn production_grid_holds_one_state_at_every_pin_phase_1355() {
    for pin in [950u32, 963, 967, 975, 987, 999, 1010] {
        let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
        cfg.latency_ms = pin;
        cfg.duration_s = 2 * 3600;
        let r = run_bench(&cfg);
        assert!(
            r.flips_per_hour <= 1.0 && r.drains == 0 && r.converge_sheds == 0 && r.late_holds == 0,
            "pin {pin}: production flips/h {:.2}: {r:?}",
            r.flips_per_hour
        );
        let mut legacy = cfg.clone();
        legacy.grid = GridModel::Legacy1970;
        let l = run_bench(&legacy);
        let late_holds_per_hour = l.late_holds as f64 / l.hours;
        assert!(
            late_holds_per_hour >= 10.0,
            "pin {pin}: the 1970-grid control stopped paying late holds \
             ({late_holds_per_hour:.2}/h) — the bench no longer reproduces the defect, so this \
             test would prove nothing: {l:?}"
        );
    }
}

/// One strih-lx OBS restart at 5 min (8 s silent) with a `k`-slot startup stall, measured over
/// the hour that follows a BOUNDED 60 s settle (goal item 1: the latency is right after every
/// restart by itself, not after the next random sender hiccup).
fn after_restart(pin: u32, k: u64) -> BenchReport {
    let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
    cfg.latency_ms = pin;
    cfg.restart = Some(SenderRestart {
        at_s: 300,
        outage_ms: 8_000,
        stall_slots: k,
    });
    cfg.warmup_s = 300 + 8 + 60;
    cfg.duration_s = cfg.warmup_s + 3600;
    run_bench(&cfg)
}

/// The depth state holding at least 99 % of the 5 s samples, if one does.
fn settled_state(r: &BenchReport) -> Option<u64> {
    let total: u64 = r.state_samples.values().sum();
    r.state_samples
        .iter()
        .find(|&(_, &n)| n * 100 >= total * 99)
        .map(|(&state, _)| state)
}

/// issue 1367 (goal item 1): the depth after a restart is a function of the pin alone. The live
/// rig settled on 31 OR 32 frames at pin 987 depending on the sender's startup stall. After the
/// fix every stall (0 to 3 slots) settles on `ceil(pin / interval) + 1` frames, and the steady
/// single gap/dup sender tail after the settle costs no shed at all.
#[test]
fn every_restart_stall_settles_on_one_pin_derived_depth_1367() {
    for pin in [987u32, 963, 1010] {
        let target = (pin as u64 * 1_000_000).div_ceil(CANVAS_INTERVAL_NS) + 1;
        let settled: Vec<(u64, Option<u64>, BenchReport)> = (0..=3)
            .map(|k| {
                let r = after_restart(pin, k);
                (k, settled_state(&r), r)
            })
            .collect();
        let summary: Vec<(u64, Option<u64>)> = settled.iter().map(|(k, s, _)| (*k, *s)).collect();
        for (k, state, r) in &settled {
            assert_eq!(
                *state,
                Some(target),
                "pin {pin}, {k}-slot startup stall: settled on {state:?}, want {target} \
                 (all stalls: {summary:?}): {r:?}"
            );
            assert_eq!(
                r.drains + r.converge_sheds,
                0,
                "pin {pin}, {k}-slot stall: the steady tail after the settle shed frames: {r:?}"
            );
            assert!(
                r.flips_per_hour <= 1.0,
                "pin {pin}, {k}-slot stall: flips/h {:.2}: {r:?}",
                r.flips_per_hour
            );
        }
    }
}

/// One restart like [`after_restart`] under an explicit seed and receiver tick phase.
fn after_restart_with(pin: u32, k: u64, seed: u64, tick_offset_ns: i64) -> BenchReport {
    let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
    cfg.latency_ms = pin;
    cfg.seed = seed;
    cfg.receiver_tick_offset_ns = tick_offset_ns;
    cfg.restart = Some(SenderRestart {
        at_s: 300,
        outage_ms: 8_000,
        stall_slots: k,
    });
    cfg.warmup_s = 300 + 8 + 60;
    cfg.duration_s = cfg.warmup_s + 3600;
    run_bench(&cfg)
}

fn corrections(r: &BenchReport) -> u64 {
    r.drains + r.converge_sheds + r.n1_grows
}

/// issue 1367 — the restart result does not hinge on one random seed: three more sender/tick
/// random streams, every stall k = 0..3, all settle on `ceil(pin/interval) + 1` within 60 s
/// with no correction in the hour after.
#[test]
fn restart_settles_on_the_same_depth_at_every_seed_1367() {
    let target = 987_000_000u64.div_ceil(CANVAS_INTERVAL_NS) + 1;
    for seed in [1u64, 2, 3] {
        for k in 0..=3 {
            let r = after_restart_with(987, k, seed, 0);
            assert_eq!(
                settled_state(&r),
                Some(target),
                "seed {seed}, {k}-slot stall: {r:?}"
            );
            assert_eq!(corrections(&r), 0, "seed {seed}, {k}-slot stall: {r:?}");
        }
    }
}

/// issue 1367 — a SETTLED deep N==1 source under the live sender tail never pays a correction:
/// the single gap/dup tail is neutral at `base + 1`, so the rule stays silent (2 h per point,
/// three seeds, a pin on each side of 987 and one whole frame apart).
#[test]
fn steady_sender_tail_never_corrects_a_settled_deep_source_1367() {
    for seed in [1u64, 2, 3] {
        for pin in [950u32, 987, 1010] {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.latency_ms = pin;
            cfg.seed = seed;
            cfg.duration_s = 2 * 3600;
            let r = run_bench(&cfg);
            assert_eq!(corrections(&r), 0, "seed {seed}, pin {pin}: {r:?}");
            assert_eq!(r.late_holds, 0, "seed {seed}, pin {pin}: {r:?}");
        }
    }
}

/// issue 1367 — the trade-off stated in the design, measured: a sender tail TEN times the live
/// rate. A single late frame is neutral at `base + 1`; only two late frames close together
/// (a double hiccup) reach an under- or over-depth, and each such event costs at most one hold
/// plus one shed. At 3000 ppm per frame the double-late rate is (3e-3)^2 x 108 000 frames/h
/// ~= 1 per hour, so the bound is TWICE that expected rate — each of shed and hold at most two
/// per hour, over five seeds — never a drain or a late hold, and the depth stays on `base + 1`.
#[test]
fn stressed_sender_tail_costs_at_most_two_correction_pairs_per_hour_1367() {
    for seed in [
        BenchConfig::live_2026_09_24(GridModel::Production).seed,
        1,
        2,
        3,
        4,
    ] {
        let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
        cfg.seed = seed;
        cfg.send_late_ppm *= 10;
        let r = run_bench(&cfg);
        let cap = 2 * r.hours.ceil() as u64;
        assert!(
            r.converge_sheds <= cap && r.n1_grows <= cap,
            "seed {seed}: more than two sheds or holds per hour at a 10x tail: {r:?}"
        );
        assert_eq!(r.drains + r.late_holds, 0, "seed {seed}: {r:?}");
        assert!(
            share(&r, &[31]) > 0.99,
            "seed {seed}: states {:?}",
            r.state_samples
        );
    }
}

/// issue 1367 (review rounds 1-2) — a render tick that runs LATE, by anything short of a
/// genuine slot skip, must never make the N==1 rule correct a settled conveyor on its own. The
/// live tail is 10–30 ms; these stress tails reach 45 and 60 ms, and every such overrun (below
/// two intervals) is followed by a CATCH-UP tick (`video_sleep`), so no slot is lost and no
/// correction is owed. Reading the depth at the processing wall misfired the N==1 shed on such
/// ticks (50 sheds + 50 holds in 2 h at 45 ms); read at the tick's SCHEDULED instant it cannot.
///
/// The one thing a 60 ms tick still does is inflate the QUEUE by two frames, which the #859
/// drain (a queue-length rule, untouched here: its hysteresis stays at 2 and it predates this
/// ticket) reads as an over-deep conveyor and sheds. Before issue 1367 that drop was absorbing
/// (30 frames forever, a whole-frame A/V step from one hitch); now the N==1 hold regrows the
/// frame on the next throttle window. So at 60 ms: no N==1 shed, and exactly one N==1 hold per
/// drain, never a hold of its own.
#[test]
fn a_late_or_catch_up_render_tick_never_corrects_a_settled_source_1367() {
    for tail_max_ms in [45u64, 60] {
        for seed in [
            BenchConfig::live_2026_09_24(GridModel::Production).seed,
            1,
            2,
        ] {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.seed = seed;
            cfg.tick_late_max_ns = tail_max_ms * 1_000_000;
            cfg.duration_s = 2 * 3600;
            let r = run_bench(&cfg);
            assert_eq!(
                r.skipped_ticks, 0,
                "tail {tail_max_ms} ms, seed {seed}: {r:?}"
            );
            assert_eq!(
                r.converge_sheds, 0,
                "tail {tail_max_ms} ms, seed {seed}: the N==1 shed misfired on a late tick: \
                 {r:?}"
            );
            assert_eq!(
                r.n1_grows, r.drains,
                "tail {tail_max_ms} ms, seed {seed}: an N==1 hold that does not repay a #859 \
                 drain corrected a settled source: {r:?}"
            );
            if tail_max_ms <= 45 {
                assert_eq!(r.drains, 0, "tail {tail_max_ms} ms, seed {seed}: {r:?}");
            }
            assert!(
                share(&r, &[31]) > 0.99,
                "tail {tail_max_ms} ms, seed {seed}: states {:?}",
                r.state_samples
            );
        }
    }
}

/// issue 1367 (review rounds 1-3) — the rule only acts while the render tick's scheduled
/// instant sits ON the grid (within `GENLOCK_MAX_SLEW_NS`, 2 ms). A real tick is off the grid only
/// for the few ticks after a wall-clock step while the slew pulls it back; a CONSTANT phase is a
/// stress bound. Inside ±2 ms the settled source never corrects and every 0..3-slot restart lands
/// on `base + 1`; outside it the rule defers — no correction at all, whatever the phase. Asserted
/// with the late-tick tail OFF, so no random late tick can hand the result to the #859 drain.
#[test]
fn a_render_tick_schedule_phase_corrects_only_on_the_grid_1367() {
    let target = 987_000_000u64.div_ceil(CANVAS_INTERVAL_NS) + 1;
    for offset_ms in [-10i64, -5, -2, 0, 2, 5, 10] {
        let offset = offset_ms * 1_000_000;
        let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
        cfg.receiver_tick_offset_ns = offset;
        cfg.duration_s = 2 * 3600;
        let r = run_bench(&cfg);
        assert_eq!(corrections(&r), 0, "schedule phase {offset_ms} ms: {r:?}");
        for k in 0..=3 {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.receiver_tick_offset_ns = offset;
            cfg.tick_late_ppm = 0;
            cfg.restart = Some(SenderRestart {
                at_s: 300,
                outage_ms: 8_000,
                stall_slots: k,
            });
            cfg.warmup_s = 300 + 8 + 60;
            cfg.duration_s = cfg.warmup_s + 3600;
            let r = run_bench(&cfg);
            if offset_ms.abs() <= 2 {
                assert_eq!(
                    settled_state(&r),
                    Some(target),
                    "on-grid schedule phase {offset_ms} ms, {k}-slot stall: {r:?}"
                );
                assert_eq!(
                    corrections(&r),
                    0,
                    "on-grid schedule phase {offset_ms} ms, {k}-slot stall: {r:?}"
                );
            } else {
                assert_eq!(
                    r.converge_sheds + r.n1_grows,
                    0,
                    "off-grid schedule phase {offset_ms} ms, {k}-slot stall: the rule must \
                     defer: {r:?}"
                );
            }
        }
    }
}

/// issue 1367 — an on-grid LATE schedule phase (+2 ms, the edge of the on-grid window, the
/// direction a real tick errs in) never makes the rule correct a settled source at ANY deep pin,
/// including the ones whose frame headroom is tiny (999 ms: 1 ms; 1000 ms: an exact 30 frames on
/// the per-second grid), and every 0 / 3-slot restart stall still lands on `base + 1`. The
/// late-tick tail is off so only the phase is under test.
#[test]
fn an_on_grid_late_phase_never_corrects_any_deep_pin_1367() {
    for pin in [950u32, 963, 987, 999, 1000, 1010, 1024] {
        let target = (pin as u64 * 1_000_000 - 1_000).div_ceil(CANVAS_INTERVAL_NS) + 1;
        for offset_ms in [0i64, 2] {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.latency_ms = pin;
            cfg.tick_late_ppm = 0;
            cfg.receiver_tick_offset_ns = offset_ms * 1_000_000;
            cfg.duration_s = 3600;
            let r = run_bench(&cfg);
            assert_eq!(
                corrections(&r),
                0,
                "pin {pin}, schedule phase +{offset_ms} ms: {r:?}"
            );
            assert!(
                share(&r, &[target]) > 0.99,
                "pin {pin}, schedule phase +{offset_ms} ms: {:?}",
                r.state_samples
            );
            for k in [0u64, 3] {
                let mut cfg = cfg.clone();
                cfg.restart = Some(SenderRestart {
                    at_s: 300,
                    outage_ms: 8_000,
                    stall_slots: k,
                });
                cfg.warmup_s = 300 + 8 + 60;
                cfg.duration_s = cfg.warmup_s + 1800;
                let r = run_bench(&cfg);
                assert_eq!(
                    settled_state(&r),
                    Some(target),
                    "pin {pin}, schedule phase +{offset_ms} ms, {k}-slot stall: {r:?}"
                );
            }
        }
    }
}

/// issue 1367 (review round 3) — a WALL-CLOCK STEP on the stream box must not make the rule
/// correct a settled conveyor. After a step of δ the scheduled ticks sit δ off the grid and slew
/// back 2 ms per tick; read there, a settled 31-frame conveyor reads 32 after a forward step of
/// more than half a frame (shed) and 30 once back on the grid (hold) — a visible skip plus a
/// duplicate where the old code, keyed on the locked boundary, did nothing. The rule defers while
/// the tick is off the grid. Steps of ±5 / ±10 / ±17 / ±25 ms at pins 987 and 999, 3 seeds.
#[test]
fn a_wall_clock_step_never_corrects_a_settled_source_1367() {
    for pin in [987u32, 999] {
        let target = (pin as u64 * 1_000_000 - 1_000).div_ceil(CANVAS_INTERVAL_NS) + 1;
        for delta_ms in [5i64, -5, 10, -10, 17, -17, 25, -25] {
            for seed in [
                BenchConfig::live_2026_09_24(GridModel::Production).seed,
                1,
                2,
            ] {
                let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
                cfg.latency_ms = pin;
                cfg.seed = seed;
                cfg.tick_late_ppm = 0;
                cfg.duration_s = 3600;
                cfg.tick_step = Some(TickStep {
                    at_s: 1800,
                    delta_ns: delta_ms * 1_000_000,
                });
                let r = run_bench(&cfg);
                assert_eq!(
                    corrections(&r),
                    0,
                    "pin {pin}, wall step {delta_ms} ms, seed {seed}: {r:?}"
                );
                assert!(
                    share(&r, &[target]) > 0.99,
                    "pin {pin}, wall step {delta_ms} ms, seed {seed}: {:?}",
                    r.state_samples
                );
            }
        }
    }
}

/// issue 1367 — a SHALLOW N==1 source (the 3 ms `cg` feeds and imag cameras; a 60 ms pin that
/// the arrival skew still dominates) is decided by its arrival, not its pin: the deep-source
/// guard keeps both new decisions silent, so its behaviour is exactly the pre-1367 one.
#[test]
fn shallow_n1_source_is_untouched_1367() {
    for seed in [BenchConfig::live_2026_09_24(GridModel::Production).seed, 1] {
        for pin in [3u32, 60] {
            let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
            cfg.latency_ms = pin;
            cfg.seed = seed;
            cfg.duration_s = 2 * 3600;
            let r = run_bench(&cfg);
            assert_eq!(
                r.converge_sheds + r.n1_grows,
                0,
                "seed {seed}, pin {pin}: the N==1 rule acted on a shallow source: {r:?}"
            );
        }
    }
}
