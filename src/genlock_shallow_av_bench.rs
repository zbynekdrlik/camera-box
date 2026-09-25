//! Issue 1367 (ROZHODNUTÉ 5827497952) — the two-clock A/V bench for a SHALLOW genlocked source with
//! jittery arrivals: the per-lock latched video depth + the slewed audio.
//!
//! A test-only child of `genlock_audio_pairing_bench` (declared there with `#[path]`): the audio leg
//! is that bench's [`AudioLeg`] (the production ingest / withhold / slew decisions + the first-order
//! ASRC), and the VIDEO is the real N==1 port of the C `genlock_release_tick`,
//! `crate::genlock_grid_bench::Fifo` — the same decisions the issue-1355 grid bench runs, now with
//! the shallow rule ([`crate::genlock_n1_depth::n1_shallow_track`] / `_hold_due` / `_shed_due` and the
//! drain suppression).
//!
//! ## Model
//!
//! - **Sender** (a cg feed at a 3 ms pin, SongPlayer / cg OBS): one frame per per-second grid slot,
//!   stamped with the slot; it ARRIVES `lag` later, `lag` uniform in the scenario's band, delivered
//!   in order. The live figures are FLOORS, i.e. the newest queued frame's AGE on a grid tick
//!   (`ts_head_skew_ms − (ts_due − 1) × 33`), which is the lag rounded UP to the next frame: resolume
//!   `NDI test` ~31 ms (lag 22-31 ms → 1 frame), `sp-slow_video` ~64 ms (lag 40-64 ms → 2 frames),
//!   strih-lx `CG-obs` 33-67 ms (lag 28-40 ms, straddling one frame → 1 or 2 frames). A band that
//!   straddles the SECOND frame edge (50-80 ms → 2 or 3 frames) and a band that RISES mid-run (no
//!   gap: 28-40 → 70-95 ms, every floor climbs to D) or changes at a sender restart cover the review
//!   cases.
//!   Optional DISTURBANCES: a lost frame (a stamp gap) and a late spike (+45 ms on one frame, in
//!   order, so the frames behind it wait too). A sender restart = 3 s of silence.
//! - **Receiver**: render ticks on the grid, each scheduled on its slot and run up to a few ms late
//!   (half-normal 0.3 ms + a rare 5-20 ms tail); the FIFO ticks at the processing wall with the
//!   scheduled instant (the C `wall_now` + `video_time`). The audio's video delay is the production
//!   tracker under [`video_delay_lock_ms`] (the latched D, pending while the first window measures).
//! - **Audio**: one packet per tick timecoded at the sender's emit instant (1-4 ms before arrival).
//! - **Two clocks**: `mono = wall + off(t)`, `off` walks 300 ms per hour (resolume's drift).
//!
//! ## What it proves (per floor, one hour, an OBS restart and a sender restart)
//!
//! - the depth latches once per lock at `max(base, floor_max) + 1` and the presented depth stays ON it
//!   (a constant video delay) — on a clean feed on every tick after the lock settles;
//! - the audio is placed once per (re)start, straight onto the latched delay: 0 slews, 0 steps;
//! - `|A/V| ≤ 5 ms` on every gated tick while the audio is settled; while it SLEWS onto a new hold
//!   (1 ms per second after a depth change, ~33 s per frame) it trails the video by up to one frame,
//!   measured separately and bounded by the songplayer gate (`SLEW_MAX_AV_MS`, 40 ms);
//! - no shed / hold / drain / underrun churn: zero corrections on a clean feed, and at most one
//!   correction per injected disturbance on a disturbed one.
//!
//! The anti-tautology run switches the rule off (`BenchConfig::shallow_depth_rule = false`, the
//! pre-rule conveyor): on the disturbed feed its depth random-walks off the target and the free
//! tracker re-times the audio.

use super::*;
use crate::genlock_grid::grid_next_boundary_ns;
use crate::genlock_grid_bench::{tick_on_grid, BenchConfig, Fifo, GridModel, TickCounters};
use crate::genlock_n1_depth::n1_tick_wall_ns;
use std::collections::VecDeque;

/// The wall-vs-mono drift over the hour (resolume's `wall_qpc_drift_ms`).
const DRIFT_PER_HOUR_NS: i64 = 300_000_000;
/// A late spike adds this to one frame's arrival.
const LATE_SPIKE_NS: u64 = 45_000_000;
/// The songplayer post-deploy A/V gate: the bound the SLEW window (audio still walking onto a new
/// hold, ≤ one frame behind) must stay inside. The settled pairing keeps `GATE_MAX_AV_MS`.
const SLEW_MAX_AV_MS: f64 = 40.0;

#[derive(Clone, Copy, Debug)]
struct Scenario {
    lag_min_ns: u64,
    lag_max_ns: u64,
    /// Expected latched depth, frames.
    want_depth: u64,
    /// Disturbances per million frames (0 = a clean feed).
    drop_ppm: u64,
    late_ppm: u64,
    rule: bool,
    min_latency_box: bool,
    /// From this many seconds after the start the lag band becomes `(min, max)` ms (no gap).
    band_change: Option<(u64, u64, u64)>,
    /// design 5830750134: a sender TRANSIENT — frames stamped in `[at_ms, at_ms + dur_ms)` after
    /// the start arrive `extra_ms` later (in order, so the frames right behind wait too): the live
    /// 25.9.2026 12:01 song change that latched `floor_max_frames=11`.
    burst: Option<(u64, u64, u64)>,
}

impl Scenario {
    fn clean(lag_min_ms: u64, lag_max_ms: u64, want_depth: u64) -> Scenario {
        Scenario {
            lag_min_ns: lag_min_ms * 1_000_000,
            lag_max_ns: lag_max_ms * 1_000_000,
            want_depth,
            drop_ppm: 0,
            late_ppm: 0,
            rule: true,
            min_latency_box: false,
            band_change: None,
            burst: None,
        }
    }
}

#[derive(Debug, Default)]
struct Run {
    /// Latched depths, one per lock (start, sender restart, OBS restart).
    latched: Vec<u64>,
    capped: bool,
    /// Presenting ticks in the gated (settled) windows, and how many sat at the latched depth.
    gated_presents: u64,
    at_depth: u64,
    /// Presented-depth changes between consecutive gated presenting ticks.
    depth_changes: u64,
    /// Gated presents per presented depth (frames).
    depth_hist: std::collections::BTreeMap<u64, u64>,
    max_abs_av_ms: f64,
    av_ticks: u64,
    /// |A/V| while the audio is still slewing onto a new hold (excluded from `max_abs_av_ms`, which
    /// is the settled pairing), and how many ticks the audio spent slewing.
    max_abs_av_slew_ms: f64,
    slew_av_ticks: u64,
    slewing_ticks: u64,
    corrections: u64,
    /// BACKLOG relocks in the gated windows (a D the FIFO cannot reach storms them).
    relocks: u64,
    drains: u64,
    underruns: u64,
    late_holds: u64,
    disturbances: u64,
    places: u32,
    slews: u32,
    steps: u32,
}

fn run(sc: Scenario) -> Run {
    const DURATION_S: u64 = 3600;
    const OBS_RESTART_S: u64 = 1200;
    const SENDER_RESTART_S: u64 = 2400;
    const SENDER_SILENT_S: u64 = 3;
    // the gate waits for the lock window (3 s), the move onto D (a throttled step per second) and
    // the audio placement; the same after every restart.
    const SETTLE_S: u64 = 12;

    let cfg = BenchConfig {
        latency_ms: LATENCY_MS,
        min_latency_box: sc.min_latency_box,
        shallow_depth_rule: sc.rule,
        ..BenchConfig::live_2026_09_24(GridModel::Production)
    };
    let mut out = Run::default();
    let mut rng = 0x1367_5827u64;
    let mut fifo = Fifo::default();
    let mut tracker = VideoDelayTracker::default();
    let mut audio = AudioLeg::fresh(false);
    let mut totals = (0u32, 0u32, 0u32);
    let mut arrivals: VecDeque<(u64, u64)> = VecDeque::new();
    let mut sender_slot = W0;
    let mut last_arrival = 0u64;
    let mut nominal = W0;
    let mut prev_wall = 0u64;
    let mut settle_until = W0 + SETTLE_S * NS_PER_S;
    let mut last_depth: Option<u64> = None;
    let mut resync = false;
    let mut last_latched = 0u64;
    let end = W0 + DURATION_S * NS_PER_S;
    let obs_restart = W0 + OBS_RESTART_S * NS_PER_S;
    let silent = (
        W0 + SENDER_RESTART_S * NS_PER_S,
        W0 + (SENDER_RESTART_S + SENDER_SILENT_S) * NS_PER_S,
    );
    let mut obs_restarted = false;
    let mut sender_back = false;
    let true_ppm = DRIFT_PER_HOUR_NS as f64 / 3600e9 * 1e6;

    while nominal < end {
        // ---- events ------------------------------------------------------------------------------
        if !obs_restarted && nominal >= obs_restart {
            obs_restarted = true;
            fifo = Fifo::default();
            tracker = VideoDelayTracker::default();
            totals.0 += audio.places;
            totals.1 += audio.slews;
            totals.2 += audio.steps;
            audio = AudioLeg::fresh(false);
            last_latched = 0;
            last_depth = None;
            settle_until = nominal + SETTLE_S * NS_PER_S;
        }
        if let Some((at_s, _, _)) = sc.band_change {
            if nominal == W0 + at_s * NS_PER_S {
                // the arrival changed: the re-measure (a whole window over D, then a window) plus
                // the move onto the new D.
                last_depth = None;
                settle_until = nominal + SETTLE_S * NS_PER_S;
            }
        }
        if !sender_back && nominal >= silent.1 {
            sender_back = true;
            resync = true;
            last_depth = None;
            settle_until = nominal + SETTLE_S * NS_PER_S;
        }

        // ---- the sender: every slot up to this tick (+ look-ahead), stamped on the grid ----------
        while sender_slot <= nominal + 4 * IV_NS {
            let stamp = sender_slot;
            sender_slot = grid_next_boundary_ns(sender_slot, IV_NS);
            if stamp >= silent.0 && stamp < silent.1 {
                continue;
            }
            let (lo, hi) = match sc.band_change {
                Some((at_s, lo_ms, hi_ms)) if stamp >= W0 + at_s * NS_PER_S => {
                    (lo_ms * 1_000_000, hi_ms * 1_000_000)
                }
                _ => (sc.lag_min_ns, sc.lag_max_ns),
            };
            let mut lag = lo + lcg(&mut rng) % (hi - lo + 1);
            if lcg(&mut rng) % 1_000_000 < sc.drop_ppm {
                out.disturbances += 1;
                continue;
            }
            if lcg(&mut rng) % 1_000_000 < sc.late_ppm {
                out.disturbances += 1;
                lag += LATE_SPIKE_NS;
            }
            if let Some((at_ms, dur_ms, extra_ms)) = sc.burst {
                let from = W0 + at_ms * 1_000_000;
                if stamp >= from && stamp < from + dur_ms * 1_000_000 {
                    lag += extra_ms * 1_000_000;
                }
            }
            let arrival = (stamp + lag).max(last_arrival + 1);
            last_arrival = arrival;
            arrivals.push_back((arrival, stamp));
        }

        // ---- the receiver tick: scheduled on its slot, run a little late -------------------------
        let mut late = (lcg(&mut rng) % 300_000) + (lcg(&mut rng) % 300_000) / 2;
        if lcg(&mut rng) % 1_000_000 < 500 {
            late = 5_000_000 + lcg(&mut rng) % 15_000_000;
        }
        let scheduled = nominal;
        let wall = (scheduled + late).max(prev_wall + 1_000);
        prev_wall = wall;
        while arrivals.front().is_some_and(|&(a, _)| a <= wall) {
            let (_, stamp) = arrivals.pop_front().expect("front exists");
            fifo.queue.push_back(stamp);
        }
        let drift = (DRIFT_PER_HOUR_NS as i128 * (nominal - W0) as i128 / 3_600_000_000_000) as i64;
        let off = (MONO0 as i64).wrapping_sub(W0 as i64).wrapping_add(drift);
        let mono = (wall as i64).wrapping_add(off) as u64;
        let mono_sched = (scheduled as i64).wrapping_add(off) as u64;

        // the audio tracker's lock input is read BEFORE this tick's latch, the C order (the present
        // tail runs the tracker, then genlock_shallow_latch).
        let lock = video_delay_lock_ms(fifo.shallow.target_frames, fifo.shallow.measuring, IV_NS);
        let mut c = TickCounters::default();
        fifo.tick(&cfg, wall, scheduled, &mut c);
        // the sender's own 3 s silence is the event, not churn: it is outside the gate.
        let gated = nominal >= settle_until && !(nominal >= silent.0 && nominal < silent.1);
        if gated {
            out.corrections += c.n1_grows + c.converge_sheds;
            out.drains += c.drains;
            out.relocks += c.relocks;
            out.underruns += c.underruns;
            out.late_holds += c.late_holds;
        }
        if fifo.shallow.target_frames != 0 && fifo.shallow.target_frames != last_latched
            || c.shallow_latches > 0
        {
            last_latched = fifo.shallow.target_frames;
            out.latched.push(last_latched);
            out.capped |= fifo.shallow.capped;
        }

        // ---- the render-thread tracker (the present tail) ----------------------------------------
        let tick_wall = n1_tick_wall_ns(wall, wall, scheduled);
        if fifo.presented_now && tick_on_grid(GridModel::Production, tick_wall) {
            let presented = fifo.presented.expect("a present sets it");
            video_delay_track(
                &mut tracker,
                lock,
                video_delay_sample_ns(tick_wall, presented),
                IV_NS,
            );
        }

        // ---- the audio: one packet, timecoded at emit --------------------------------------------
        if !(nominal >= silent.0 && nominal < silent.1) {
            let tc = wall - (1_000_000 + lcg(&mut rng) % 3_000_000);
            audio.ingest(tc, mono, wall, tracker.applied_ms, resync);
            resync = false;
            audio.asrc_tick(true_ppm, IV_NS, tc, mono);
        }

        // ---- the gate ----------------------------------------------------------------------------
        if gated && fifo.presented_now {
            let presented = fifo.presented.expect("a present sets it");
            let depth = (scheduled - presented + IV_NS / 2) / IV_NS;
            out.gated_presents += 1;
            *out.depth_hist.entry(depth).or_insert(0) += 1;
            if depth == fifo.shallow.target_frames {
                out.at_depth += 1;
                if audio.playing() && !audio.slewing() {
                    let av = audio.av_ms(presented, mono_sched);
                    out.av_ticks += 1;
                    out.max_abs_av_ms = out.max_abs_av_ms.max(av.abs());
                }
            }
            if last_depth.is_some_and(|d| d != depth) {
                out.depth_changes += 1;
            }
            last_depth = Some(depth);
        }
        // the SLEW window (review round 3): measured on EVERY presenting tick outside the sender's
        // silence, not only inside the settled gate -- the slew starts at the re-latch, seconds
        // before the gate reopens, and its first seconds are the largest |A/V|.
        let sender_silent = nominal >= silent.0 && nominal < silent.1;
        if !sender_silent && fifo.presented_now && audio.playing() && audio.slewing() {
            let presented = fifo.presented.expect("a present sets it");
            let depth = (scheduled - presented + IV_NS / 2) / IV_NS;
            if depth == fifo.shallow.target_frames {
                // the video is on the new D, the audio still on its way.
                let av = audio.av_ms(presented, mono_sched);
                out.slew_av_ticks += 1;
                out.max_abs_av_slew_ms = out.max_abs_av_slew_ms.max(av.abs());
            }
        }
        out.slewing_ticks += u64::from(audio.slewing());
        nominal = grid_next_boundary_ns(nominal, IV_NS);
    }
    out.places = totals.0 + audio.places;
    out.slews = totals.1 + audio.slews;
    out.steps = totals.2 + audio.steps;
    out
}

const NS_PER_S: u64 = 1_000_000_000;

fn assert_clean(name: &str, sc: Scenario) {
    let r = run(sc);
    eprintln!("{name}: {r:?}");
    assert!(
        !r.latched.is_empty() && r.latched.iter().all(|&d| d == sc.want_depth),
        "{name}: latched {:?}, want {} every lock",
        r.latched,
        sc.want_depth
    );
    assert!(r.gated_presents > 90_000, "{name}: gate too thin");
    assert_eq!(
        r.at_depth, r.gated_presents,
        "{name}: the presented depth left the latched D"
    );
    assert_eq!(r.depth_changes, 0, "{name}: the video delay moved");
    assert_eq!(r.corrections, 0, "{name}: hold/shed churn");
    assert_eq!(r.drains, 0, "{name}: drain churn");
    assert_eq!(r.underruns, 0, "{name}: underruns");
    assert_eq!(r.late_holds, 0, "{name}: late holds");
    assert_eq!(r.steps, 0, "{name}: an audio step re-placement");
    assert_eq!(r.slews, 0, "{name}: the audio re-timed");
    assert_eq!(
        r.places, 3,
        "{name}: one placement per start, sender restart and OBS restart"
    );
    assert!(r.av_ticks > 90_000, "{name}: A/V gate too thin");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "{name}: |A/V| {:.2} ms",
        r.max_abs_av_ms
    );
}

#[test]
fn ndi_test_floor_31ms_locks_two_frames_and_pairs_1367() {
    assert_clean("ndi-test", Scenario::clean(22, 31, 2));
}

#[test]
fn sp_slow_floor_64ms_locks_three_frames_and_pairs_1367() {
    assert_clean("sp-slow", Scenario::clean(40, 64, 3));
}

#[test]
fn cg_obs_floor_33_to_67ms_locks_three_frames_and_pairs_1367() {
    // the lag straddles one frame, so the rounded floor flips 1 <-> 2 frame to frame: the depth
    // latches on the worse one.
    assert_clean("cg-obs", Scenario::clean(28, 40, 3));
}

fn disturbed(rule: bool) -> Scenario {
    Scenario {
        drop_ppm: 400,
        late_ppm: 300,
        rule,
        ..Scenario::clean(28, 40, 3)
    }
}

#[test]
fn a_disturbed_feed_returns_to_its_depth_and_the_audio_never_moves_1367() {
    let r = run(disturbed(true));
    eprintln!("disturbed: {r:?}");
    assert!(r.disturbances > 50, "the feed must be disturbed");
    assert!(r.latched.iter().all(|&d| d == 3), "latched {:?}", r.latched);
    // every disturbance costs at most one correction, nothing more.
    assert!(
        r.corrections <= r.disturbances,
        "corrections {} for {} disturbances",
        r.corrections,
        r.disturbances
    );
    // the depth is back on D after each one: off D only for the throttle window per disturbance.
    let off_d = r.gated_presents - r.at_depth;
    assert!(
        off_d <= r.disturbances * 32,
        "{off_d} presents off D for {} disturbances",
        r.disturbances
    );
    assert_eq!((r.steps, r.slews), (0, 0), "the audio must never move");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms",
        r.max_abs_av_ms
    );
}

#[test]
fn without_the_rule_the_same_feed_floats_and_re_times_the_audio_1367() {
    let with = run(disturbed(true));
    let without = run(disturbed(false));
    eprintln!("rule off: {without:?}");
    assert!(without.latched.is_empty(), "the rule is off");
    // with the rule every excursion returns to D within the throttle window, so D holds nearly
    // every present; without it the depth random-walks and no single depth dominates like that.
    let modal =
        |r: &Run| *r.depth_hist.values().max().expect("presents") as f64 / r.gated_presents as f64;
    assert!(modal(&with) > 0.999, "with the rule: {:.4}", modal(&with));
    assert!(
        modal(&without) < 0.9,
        "the floating conveyor must random-walk off any one depth: {:.4} ({:?})",
        modal(&without),
        without.depth_hist
    );
    assert!(
        without.slews > 0,
        "the free tracker must re-time the audio on a floating depth"
    );
}

#[test]
fn the_min_latency_guard_reports_and_never_governs_1367() {
    // a floor over the imag cap (lag 40-64 ms: 2 frames, D would be 3 > base + 1 = 2) is REPORTED
    // and not applied: no depth is stored, the rule never holds/sheds and the #859 drain stays on
    // (report instead, review round 1: a forced shallower D would churn against the arrival).
    let r = run(Scenario {
        min_latency_box: true,
        ..Scenario::clean(40, 64, 2)
    });
    eprintln!("min-latency: {r:?}");
    assert!(r.capped, "a capped latch must be reported");
    assert!(
        !r.latched.is_empty() && r.latched.iter().all(|&d| d == 0),
        "no depth may be applied: {:?}",
        r.latched
    );
    assert_eq!(r.corrections, 0, "the capped rule must not hold or shed");
}

#[test]
fn a_band_straddling_the_second_frame_edge_locks_four_frames_1367() {
    assert_clean("straddle-67", Scenario::clean(50, 80, 4));
}

/// A run whose arrival band changes, with the audio re-timed ONCE by a slew and never stepped.
fn assert_band_change(name: &str, sc: Scenario, latched: &[u64]) -> Run {
    let r = run(sc);
    eprintln!("{name}: {r:?}");
    let mut seen: Vec<u64> = r.latched.clone();
    seen.dedup();
    assert_eq!(seen, latched, "{name}: latched sequence");
    assert_eq!(r.steps, 0, "{name}: an audio step re-placement");
    assert_eq!(r.slews, 1, "{name}: the new D is slewed in exactly once");
    assert_eq!(r.places, 3, "{name}: start, sender restart, OBS restart");
    // the slew window itself (review round 2): the audio walks onto the new hold at 1 ms per second,
    // so for ~33 s after a one-frame re-time it trails the video by up to one frame. Bounded by the
    // songplayer A/V gate (40 ms), measured (not vacuous), and over within 40 s of slewing.
    // the START of the slew is inside the measurement: a one-frame re-time begins one frame
    // (33.3 ms) apart, so the peak must reach that minus the residual (a sampling that started even
    // a second late reads <= 32.3 ms).
    assert!(
        r.slew_av_ticks > 0 && r.max_abs_av_slew_ms >= 32.5,
        "{name}: the start of the slew window was not measured: {r:?}"
    );
    assert!(
        r.max_abs_av_slew_ms <= SLEW_MAX_AV_MS,
        "{name}: |A/V| while slewing {:.2} ms",
        r.max_abs_av_slew_ms
    );
    assert!(
        r.slewing_ticks <= 40 * 30,
        "{name}: the audio slewed {} ticks (> 40 s) for one frame",
        r.slewing_ticks
    );
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "{name}: |A/V| {:.2} ms",
        r.max_abs_av_ms
    );
    assert!(r.av_ticks > 80_000, "{name}: A/V gate too thin");
    r
}

#[test]
fn a_rising_arrival_re_measures_and_slews_the_audio_once_1367() {
    // lag 28-40 ms (D 3) rises to 70-95 ms at t = 1500 s with no gap: every rounded floor is 3 =
    // D, so after a whole window at/over D the depth re-measures to 4 and the audio slews +33 ms
    // once. (A 60-80 ms band floors at 2 or 3 and never re-measures -- it waits for a relock.)
    let r = assert_band_change(
        "rising",
        Scenario {
            band_change: Some((1500, 70, 95)),
            ..Scenario::clean(28, 40, 3)
        },
        &[3, 4],
    );
    // the re-measure happened at the RISE, not at the later sender restart (t = 2400 s): D 4 is
    // presented from ~1510 s on (the sender-restart relatch alone would give ~1190 s of it).
    let at_4 = r.depth_hist.get(&4).copied().unwrap_or(0);
    assert!(
        at_4 > 55_000,
        "rising: D 4 presented {at_4} ticks -- the rise did not re-measure before the restart"
    );
}

#[test]
fn a_sender_restart_on_a_new_band_relatches_and_slews_once_1367() {
    // the sender comes back at t = 2403 s on a slower path: the restart relatch finds D 4, the
    // resync placement keeps the old delay, and the new one is slewed in once.
    assert_band_change(
        "restart-band",
        Scenario {
            band_change: Some((2403, 60, 80)),
            ..Scenario::clean(28, 40, 3)
        },
        &[3, 4],
    );
}

// ---- design 5830750134: the shallow latch never latches an outlier ------------------------------

/// The sender comes back from its restart (t = 2403 s, the relock that opens a fresh window) with
/// its first frames 300 ms late for `dur_ms` — the live song change: every `sp-*` source re-latched
/// at 12:01:17 and `sp-slow_video` measured `floor_max_frames=11` (a +300 ms transient on its
/// 40-64 ms band floors at 11 frames).
fn transient(dur_ms: u64) -> Scenario {
    Scenario {
        burst: Some((2_403_000, dur_ms, 300)),
        ..Scenario::clean(40, 64, 3)
    }
}

/// The acceptance of design 5830750134: every latch inside the cap, no relock churn once settled,
/// the settled A/V pairing within 5 ms, and the depth back on the healthy D.
fn assert_transient(name: &str, r: &Run, max_slews: u32) {
    eprintln!("{name}: {r:?}");
    let cap = 1 + crate::genlock_n1_depth::N1_SHALLOW_MAX_EXTRA_FRAMES;
    assert!(
        r.latched.iter().all(|&d| d <= cap),
        "{name}: latched {:?} over the cap {cap}",
        r.latched
    );
    assert_eq!(r.latched.last(), Some(&3), "{name}: re-converged on D 3");
    assert_eq!(r.relocks, 0, "{name}: relock churn once settled");
    assert_eq!(r.late_holds, 0, "{name}: late holds once settled");
    assert_eq!(r.steps, 0, "{name}: an audio step re-placement");
    assert!(r.slews <= max_slews, "{name}: {} slews", r.slews);
    assert_eq!(r.places, 3, "{name}: start, sender restart, OBS restart");
    assert!(r.av_ticks > 80_000, "{name}: A/V gate too thin");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "{name}: |A/V| {:.2} ms",
        r.max_abs_av_ms
    );
    let at_3 = r.depth_hist.get(&3).copied().unwrap_or(0);
    assert!(
        at_3 * 100 >= r.gated_presents * 99,
        "{name}: D 3 held on {at_3} of {} gated presents: {:?}",
        r.gated_presents,
        r.depth_hist
    );
}

#[test]
fn a_short_transient_in_the_settle_window_is_ignored_by_the_p90_latch_1367() {
    // 150 ms (5 ticks, under a tenth of the window): the percentile never sees it, the relatch
    // finds the same D 3 and the audio never moves.
    let r = run(transient(150));
    assert_transient("transient-150ms", &r, 0);
    assert!(r.latched.iter().all(|&d| d == 3), "latched {:?}", r.latched);
}

#[test]
fn the_live_one_second_transient_never_latches_the_400ms_depth_1367() {
    // the live case: a one-second song-change transient in the window after the sender restart.
    // Before the fix the window MAX latched D 12 (400 ms), the hold drove the queue into a backlog
    // relock storm and the audio held 400 ms against a far shallower video. Now the window's spread
    // rejects it, the next window latches the healthy D 3, and the audio never moves.
    let r = run(transient(1_000));
    assert_transient("transient-1s", &r, 0);
    assert!(r.latched.iter().all(|&d| d == 3), "latched {:?}", r.latched);
}

#[test]
fn a_whole_window_transient_latches_the_clamp_then_re_measures_1367() {
    // 4 s: the whole first window sits on the transient (no spread to reject), so the latch is the
    // clamp base + 3 = 4, reported. Once the floor falls back, a whole window two frames under it
    // re-measures onto D 3. The audio follows the clamp and back by the slew, never a step.
    let r = run(transient(4_000));
    assert!(r.capped, "the over-cap latch must be reported");
    assert!(r.latched.contains(&4), "latched {:?}", r.latched);
    assert_transient("transient-4s", &r, 2);
}
