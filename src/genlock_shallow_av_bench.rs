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
//!   correction per injected disturbance on a disturbed one;
//! - a SONG CHANGE (the sender skipping stamps in bursts, starving the queue) never puts the video on
//!   air under the latched D, so the video delay the audio follows stays on D (design 5833339163 —
//!   before it, every skip presented the post-gap head one frame young: `video_delay_ms=67` against
//!   an audio hold of 100 ms).
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
    /// live 25.9.2026 12:31: the anti-tautology audio leg that appends after a timeline reset.
    legacy_append: bool,
    /// design 5830750134: a sender TRANSIENT — frames stamped in `[at_ms, at_ms + dur_ms)` after
    /// the start arrive `extra_ms` later (in order, so the frames right behind wait too): the live
    /// 25.9.2026 12:01 song change that latched `floor_max_frames=11`.
    burst: Option<(u64, u64, u64)>,
    /// design 5833339163: a SONG CHANGE `(at_s, dur_s)` — for `dur_s` seconds from `at_s` the sender
    /// SKIPS stamps in bursts ([`song_change_skips`]): single lost frames plus runs long enough to
    /// starve the queue, the live 25.9.2026 15:29 sp-slow_video pattern (`stamp_gap` +11 and
    /// `underruns` +7 per 5 s audit while the operator switched scenes).
    song_change: Option<(u64, u64)>,
    /// design 5844353368: SONGS `(start_s, end_s)` on an idle feed — inside each the lag band is
    /// `content_ms` (the playing content's decode cost), and each song END is a SongPlayer re-lock:
    /// the sender skips [`SONG_RELOCK_GAP_NS`] of stamps (a relock gap), audio keeps flowing.
    songs: &'static [(u64, u64)],
    /// The content lag band `(min, max)` ms inside a song.
    content_ms: (u64, u64),
}

/// design 5844353368: the stamp gap a SongPlayer re-lock between songs leaves (over the 1 s relock
/// gap, so the receiver re-locks on the idle feed that follows).
const SONG_RELOCK_GAP_NS: u64 = 1_500_000_000;

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
            legacy_append: false,
            song_change: None,
            songs: &[],
            content_ms: (0, 0),
        }
    }
}

#[derive(Debug, Default)]
struct Run {
    /// Latched depths, one per lock (start, sender restart, OBS restart).
    latched: Vec<u64>,
    /// ROZHODNUTÉ 5842640404: every latch (one per lock, plus one per re-measure).
    latches: u64,
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
    /// The largest gap between the REPORTED pairing offset (the audit's realized audio side minus
    /// the measured video delay) and the TRUE A/V error of the samples, on the same ticks.
    max_pairing_vs_av_ms: f64,
    /// The largest gap between the production placement formula and the modelled truth, ms.
    max_formula_vs_truth_ms: f64,
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
    /// design 5833339163: the song-change window (its skips + 3 s): presents, presents SHALLOWER
    /// than the latched D, the lowest smoothed video delay the audit's `video_delay_ms=` reads, the
    /// audio's applied delay range (`audio_delay_ms=`), |A/V| of the samples on every present, and
    /// the stamps skipped / queue underruns it contained.
    sc_presents: u64,
    sc_shallow: u64,
    sc_min_video_delay_ms: u32,
    sc_audio_delay_ms: (u32, u32),
    sc_max_abs_av_ms: f64,
    sc_skipped: u64,
    sc_underruns: u64,
}

/// design 5833339163: the song-change skip pattern — every 500 ms of the window the sender skips
/// `1 + (k mod 3)` consecutive stamps (burst `k`): a lone lost frame, a pair, and a run of three
/// that empties the 40-64 ms feed's queue (a starvation underrun). About 12 skipped stamps per 5 s,
/// the live `stamp_gap` rate. True when `stamp` is skipped.
fn song_change_skips(stamp: u64, at_s: u64, dur_s: u64) -> bool {
    const PERIOD_NS: u64 = 500_000_000;
    let from = W0 + at_s * NS_PER_S;
    if stamp < from || stamp >= from + dur_s * NS_PER_S {
        return false;
    }
    let off = stamp - from;
    let burst = off / PERIOD_NS;
    off % PERIOD_NS < (1 + burst % 3) * IV_NS
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
    let mut out = Run {
        sc_min_video_delay_ms: u32::MAX,
        sc_audio_delay_ms: (u32::MAX, 0),
        ..Run::default()
    };
    let song_window = sc
        .song_change
        .map(|(at_s, dur_s)| (W0 + at_s * NS_PER_S, W0 + (at_s + dur_s + 3) * NS_PER_S));
    let mut rng = 0x1367_5827u64;
    let mut fifo = Fifo::default();
    let mut tracker = VideoDelayTracker::default();
    let leg = || {
        if sc.legacy_append {
            AudioLeg::fresh(false).with_legacy_append_after_reset()
        } else {
            AudioLeg::fresh(false)
        }
    };
    let mut audio = leg();
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
            audio = leg();
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
        if sc
            .songs
            .iter()
            .any(|&(a, b)| nominal == W0 + a * NS_PER_S || nominal == W0 + b * NS_PER_S)
        {
            // a song start (content) or its end (the re-lock): the gate reopens once settled.
            last_depth = None;
            settle_until = nominal + SETTLE_S * NS_PER_S;
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
            let song_end_gap = sc.songs.iter().any(|&(_, b)| {
                let end = W0 + b * NS_PER_S;
                stamp >= end && stamp < end + SONG_RELOCK_GAP_NS
            });
            if song_end_gap {
                continue;
            }
            if let Some((at_s, dur_s)) = sc.song_change {
                if song_change_skips(stamp, at_s, dur_s) {
                    out.sc_skipped += 1;
                    continue;
                }
            }
            let in_song = sc
                .songs
                .iter()
                .any(|&(a, b)| stamp >= W0 + a * NS_PER_S && stamp < W0 + b * NS_PER_S);
            let (lo, hi) = match sc.band_change {
                _ if in_song => (sc.content_ms.0 * 1_000_000, sc.content_ms.1 * 1_000_000),
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
            let (arrival, stamp) = arrivals.pop_front().expect("front exists");
            fifo.receive(arrival, stamp);
        }
        let drift = (DRIFT_PER_HOUR_NS as i128 * (nominal - W0) as i128 / 3_600_000_000_000) as i64;
        let off = (MONO0 as i64).wrapping_sub(W0 as i64).wrapping_add(drift);
        let mono = (wall as i64).wrapping_add(off) as u64;
        let mono_sched = (scheduled as i64).wrapping_add(off) as u64;

        // the audio tracker's lock input is read BEFORE this tick's latch, the C order (the present
        // tail runs the tracker, then genlock_shallow_latch).
        let lock = video_delay_lock_ms(fifo.shallow.target_frames, fifo.shallow.measuring, IV_NS);
        let mut c = TickCounters::default();
        // design 5844353368: the C latch reads the audio hold mode the audio thread set last.
        fifo.audio_flowing = audio.mode == AudioHoldMode::Timecode;
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
        out.latches += c.shallow_latches;
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
                    let reported = pairing_offset_ms(
                        audio.realized_delay_ns(),
                        video_delay_reference_ns(tracker.smoothed_ns, LATENCY_MS),
                    );
                    out.max_pairing_vs_av_ms =
                        out.max_pairing_vs_av_ms.max((reported as f64 - av).abs());
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
        // design 5833339163: the song-change window, on EVERY presenting tick (a shallow present is
        // exactly what the settled gate above skips).
        let in_song = song_window.is_some_and(|(a, b)| nominal >= a && nominal < b);
        if in_song {
            out.sc_underruns += c.underruns;
            if fifo.presented_now {
                let presented = fifo.presented.expect("a present sets it");
                let depth = (scheduled - presented + IV_NS / 2) / IV_NS;
                out.sc_presents += 1;
                if depth < fifo.shallow.target_frames {
                    out.sc_shallow += 1;
                }
                if tracker.smoothed_ns != 0 {
                    out.sc_min_video_delay_ms = out
                        .sc_min_video_delay_ms
                        .min(video_delay_round_ms(tracker.smoothed_ns));
                }
                out.sc_audio_delay_ms.0 = out.sc_audio_delay_ms.0.min(tracker.applied_ms);
                out.sc_audio_delay_ms.1 = out.sc_audio_delay_ms.1.max(tracker.applied_ms);
                if audio.playing() && !audio.slewing() {
                    let av = audio.av_ms(presented, mono_sched);
                    out.sc_max_abs_av_ms = out.sc_max_abs_av_ms.max(av.abs());
                }
            }
        }
        out.slewing_ticks += u64::from(audio.slewing());
        out.max_formula_vs_truth_ms = out
            .max_formula_vs_truth_ms
            .max(audio.max_formula_vs_truth_ns / 1e6);
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

// ROZHODNUTÉ 5842656021 (re-baseline): the latch adds the 15 ms arrival-jitter budget to the
// receive-time lag, so a feed whose p90 lag sits within 15 ms under a frame edge latches one frame
// deeper -- `NDI test` (lag 22-31 ms) 2 -> 3 and `sp-slow` (40-64 ms) 3 -> 4. Those are exactly the
// feeds a content change pushes across the edge.
#[test]
fn ndi_test_floor_31ms_locks_three_frames_and_pairs_1367() {
    assert_clean("ndi-test", Scenario::clean(22, 31, 3));
}

#[test]
fn sp_slow_floor_64ms_locks_four_frames_and_pairs_1367() {
    assert_clean("sp-slow", Scenario::clean(40, 64, 4));
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
    // ROZHODNUTÉ 5842656021: the budgeted latch floor of 70-95 ms asks for base + 4, so the new D is
    // the base + 3 clamp, reported (capped).
    let r = assert_band_change(
        "rising",
        Scenario {
            band_change: Some((1500, 70, 95)),
            ..Scenario::clean(28, 40, 3)
        },
        &[3, 4],
    );
    assert!(r.capped, "rising: the clamped re-latch must be reported");
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

// ---- ROZHODNUTÉ 5842640404: a content-dependent send cost never re-measures the latch -----------

/// A feed whose idle (black) lag rises by the sender's content-dependent compression cost at
/// t = 1500 s -- songplayer 147 measured +8..+11 ms from black to playing. The OBS restart (1200 s)
/// latches on the idle lag, the sender restart (2403 s) on the content lag.
fn song_start(idle_ms: (u64, u64), content_ms: (u64, u64), want_depth: u64) -> Scenario {
    Scenario {
        band_change: Some((1500, content_ms.0, content_ms.1)),
        ..Scenario::clean(idle_ms.0, idle_ms.1, want_depth)
    }
}

#[test]
fn a_song_start_inside_the_budget_never_re_measures_the_latch_1367() {
    // the live case: an idle lag ~25 ms (one frame at the tick) latched D 2 and the +11 ms playing
    // cost put the tick floor on D, so every song start re-measured to 3 and slewed the audio
    // +33 ms. The budgeted latch floor of the idle lag already asks for D 3: no re-measure, no slew.
    let sc = song_start((24, 26), (35, 37), 3);
    assert_clean("song-start-25-36", sc);
    let r = run(sc);
    assert_eq!(r.latches, 3, "one latch per lock, no re-measure: {r:?}");
}

#[test]
fn an_idle_lag_far_under_the_edge_pays_no_headroom_1367() {
    // idle 8 ms -> content 19 ms: the locks made on the idle lag stay base + 1 = 2 (8 + 15 ms is
    // inside the first frame) and the song start never re-measures (the tick floor of 19 ms is
    // one frame, under D) -- the budget costs a frame only where the jitter can cross an edge.
    // The sender-restart lock at 2403 s is made on the 18-20 ms content lag itself, whose p90 plus
    // the budget (~35 ms) crosses the first edge: that lock is 3, the budget doing its job.
    let r = run(song_start((7, 9), (18, 20), 2));
    eprintln!("song-start-8-19: {r:?}");
    assert_eq!(r.latched, [2, 2, 3], "idle, OBS restart, content relock");
    assert_eq!(r.latches, 3, "one latch per lock, no re-measure: {r:?}");
    // D 2 is presented from the start to the sender restart (2400 s), minus the settle windows:
    // a re-measure at the song start (1500 s) would cut that to ~44 000 presents.
    let at_2 = r.depth_hist.get(&2).copied().unwrap_or(0);
    assert!(
        at_2 > 60_000,
        "song-start-8-19: D 2 held only {at_2} presents: {:?}",
        r.depth_hist
    );
    assert_eq!(r.corrections, 0, "hold/shed churn");
    assert_eq!(r.steps, 0, "an audio step re-placement");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms",
        r.max_abs_av_ms
    );
}

#[test]
fn a_min_latency_box_keeps_the_raw_floor_and_stays_governed_1367() {
    // ROZHODNUTÉ 5842848307 (option 2): on the imag marker the latch histogram reads the RAW tick
    // floor. An idle lag of 24-26 ms is one frame at the tick, so the input stays governed at
    // base + 1 = 2 -- never the budgeted 3 that the min-latency guard would only report (capped).
    let sc = Scenario {
        min_latency_box: true,
        ..Scenario::clean(24, 26, 2)
    };
    assert_clean("min-latency-24-26", sc);
    let r = run(sc);
    assert!(
        !r.capped,
        "the min-latency input must never be reported capped: {r:?}"
    );
    assert_eq!(r.latches, 3, "one latch per lock: {r:?}");
}

#[test]
fn without_the_marker_the_same_feed_latches_the_budget_1367() {
    // the same 24-26 ms feed on a box without the marker keeps the budget: D 3.
    assert_clean("no-marker-24-26", Scenario::clean(24, 26, 3));
}

#[test]
fn a_rise_past_the_budget_still_re_measures_1367() {
    // idle 8 ms (D 2) -> 60 ms: the tick floor reaches D for a whole window, so the latch
    // re-measures onto the budgeted 60 ms lag (3 frames -> D 4), and the audio follows by one slew.
    let r = run(song_start((7, 9), (59, 61), 2));
    eprintln!("song-start-8-60: {r:?}");
    let mut seen = r.latched.clone();
    seen.dedup();
    assert_eq!(seen, [2, 4], "latched sequence");
    assert_eq!(r.latches, 4, "one re-measure at the rise: {r:?}");
    assert_eq!((r.steps, r.slews), (0, 1), "the new D is slewed in once");
    assert_eq!(r.places, 3, "start, sender restart, OBS restart");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms",
        r.max_abs_av_ms
    );
}

// ---- design 5830750134: the shallow latch never latches an outlier ------------------------------

/// The sender comes back from its restart (t = 2403 s, the relock that opens a fresh window) with
/// its first frames 300 ms late for `dur_ms` — the live song change: every `sp-*` source re-latched
/// at 12:01:17 and `sp-slow_video` measured `floor_max_frames=11` (a +300 ms transient on its
/// 40-64 ms band floors at 11 frames). ROZHODNUTÉ 5842656021: the healthy D of that band is 4 with
/// the arrival-jitter budget (was 3).
fn transient(dur_ms: u64) -> Scenario {
    Scenario {
        burst: Some((2_403_000, dur_ms, 300)),
        ..Scenario::clean(40, 64, 4)
    }
}

/// The healthy latched depth of the 40-64 ms band the transient cases run on (re-baselined by
/// ROZHODNUTÉ 5842656021 from 3).
const TRANSIENT_D: u64 = 4;

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
    assert_eq!(
        r.latched.last(),
        Some(&TRANSIENT_D),
        "{name}: re-converged on D {TRANSIENT_D}"
    );
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
    let at_d = r.depth_hist.get(&TRANSIENT_D).copied().unwrap_or(0);
    assert!(
        at_d * 100 >= r.gated_presents * 99,
        "{name}: D {TRANSIENT_D} held on {at_d} of {} gated presents: {:?}",
        r.gated_presents,
        r.depth_hist
    );
}

#[test]
fn a_short_transient_in_the_settle_window_is_ignored_by_the_p90_latch_1367() {
    // 150 ms (5 ticks, under a tenth of the window): the percentile never sees it, the relatch
    // finds the same D and the audio never moves.
    let r = run(transient(150));
    assert_transient("transient-150ms", &r, 0);
    assert!(
        r.latched.iter().all(|&d| d == TRANSIENT_D),
        "latched {:?}",
        r.latched
    );
}

#[test]
fn the_live_one_second_transient_never_latches_the_400ms_depth_1367() {
    // the live case: a one-second song-change transient in the window after the sender restart.
    // Before the fix the window MAX latched D 12 (400 ms), the hold drove the queue into a backlog
    // relock storm and the audio held 400 ms against a far shallower video. Now the window's spread
    // rejects it, the next window latches the healthy D, and the audio never moves.
    let r = run(transient(1_000));
    assert_transient("transient-1s", &r, 0);
    assert!(
        r.latched.iter().all(|&d| d == TRANSIENT_D),
        "latched {:?}",
        r.latched
    );
}

#[test]
fn a_whole_window_transient_latches_the_clamp_then_re_measures_1367() {
    // 4 s: the whole first window sits on the transient (no spread to reject), so the latch is the
    // clamp base + 3 = 4, reported. Once the floor falls back, a whole window two frames under it
    // re-measures onto the healthy D (4 since ROZHODNUTÉ 5842656021, the clamp's own value, so the
    // audio never has to move). The audio follows by the slew, never a step.
    let r = run(transient(4_000));
    assert!(r.capped, "the over-cap latch must be reported");
    assert!(r.latched.contains(&4), "latched {:?}", r.latched);
    assert_eq!(
        r.latches, 4,
        "start, OBS restart, the clamped sender-restart latch and its re-measure: {r:?}"
    );
    assert_transient("transient-4s", &r, 2);
}

// ---- live 25.9.2026 12:31: the hold must reach the SAMPLES after a sender restart ---------------

#[test]
fn a_sender_restart_never_appends_the_audio_at_arrival_1367() {
    // every run has a sender restart (t = 2400 s, 3 s of silence: a >2 s timestamp jump). OBS then
    // resets the buffer to the ARRIVAL instant and appends; the fixed ingest places at the term. The
    // settled pairing of the SAMPLES (not the bookkeeping) stays within 5 ms, and the pairing offset
    // the audit reports tracks the true A/V error.
    let r = run(Scenario::clean(40, 64, 3));
    eprintln!("placed-after-reset: {r:?}");
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms",
        r.max_abs_av_ms
    );
    assert!(
        r.max_pairing_vs_av_ms <= 4.0,
        "the reported pairing offset left the true A/V by {:.2} ms",
        r.max_pairing_vs_av_ms
    );
    // review round 1: the measurement runs the PRODUCTION formula (audio_ts + buffered for an
    // append) over an OBS-style buffer, and it agrees with the modelled truth on every packet.
    assert!(
        r.max_formula_vs_truth_ms <= 0.01,
        "the placement formula left the truth by {:.4} ms",
        r.max_formula_vs_truth_ms
    );
}

#[test]
fn the_append_after_reset_loses_the_hold_and_the_audit_now_says_so_1367() {
    // the anti-tautology: OBS's append taken as-is (the live ebea02a2d behaviour) puts the audio on
    // its arrival after the restart -- ~1-2 frames EARLY on this 40-64 ms feed (live: +101 ms on a
    // 133 ms hold). The audit's pairing offset measures the samples, so it reads that gap instead
    // of the old structural 0.
    let r = run(Scenario {
        legacy_append: true,
        ..Scenario::clean(40, 64, 3)
    });
    eprintln!("legacy-append: {r:?}");
    assert!(
        r.max_abs_av_ms > 30.0,
        "the legacy append must lose the hold: |A/V| {:.2} ms",
        r.max_abs_av_ms
    );
    assert!(
        r.max_pairing_vs_av_ms <= 4.0,
        "the audit must report the lost hold: pairing vs true A/V {:.2} ms",
        r.max_pairing_vs_av_ms
    );
    // the append after the reset goes through audio_ts (the arrival) + an empty buffer.
    assert!(
        r.max_formula_vs_truth_ms <= 0.01,
        "the placement formula left the truth by {:.4} ms",
        r.max_formula_vs_truth_ms
    );
}

// ---- design 5833339163: a song change never shortens the latched video depth -----------------------

#[test]
fn a_song_change_keeps_the_video_on_its_latched_depth_and_the_audio_paired_1367() {
    // live 25.9.2026 15:29 on resolume: SongPlayer's song change and the operator's scene switches
    // skipped stamps on sp-slow_video (lag 40-64 ms, then D 3 = 100 ms). Every skip put the post-gap head
    // on air at once at its arrival age (the GAP RESYNC), one frame under D, and the throttled hold
    // took a second per frame to climb back: `video_delay_ms=67 audio_delay_ms=100
    // audio_pairing_offset_ms=33` for ~10 s while the latched depth and the audio stayed 3 / 100.
    // Now a skipped stamp costs the one repeat it costs anyway: the conveyor HOLDS until the head
    // is D frames old, so the video never leaves D and the audio never needs to move.
    // ROZHODNUTÉ 5842656021: the band's D is 4 (133 ms) with the arrival-jitter budget.
    let r = run(Scenario {
        song_change: Some((1500, 12)),
        ..Scenario::clean(40, 64, 4)
    });
    eprintln!("song-change: {r:?}");
    let d_ms = video_delay_round_ms(4 * IV_NS);
    assert!(
        r.sc_skipped >= 20 && r.sc_underruns > 0,
        "the song change must skip stamps and starve the queue: {r:?}"
    );
    assert!(r.latched.iter().all(|&d| d == 4), "latched {:?}", r.latched);
    assert!(r.sc_presents > 300, "the song-change window is too thin");
    assert_eq!(
        r.sc_audio_delay_ms,
        (d_ms, d_ms),
        "the audio hold must stay on the latched {d_ms} ms"
    );
    assert_eq!(
        r.sc_shallow, 0,
        "{} presents under the latched D during the song change",
        r.sc_shallow
    );
    assert!(
        r.sc_min_video_delay_ms >= d_ms - 1,
        "video_delay_ms fell to {} (audio {d_ms}) during the song change",
        r.sc_min_video_delay_ms
    );
    assert!(
        r.sc_max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms during the song change",
        r.sc_max_abs_av_ms
    );
    assert_eq!((r.steps, r.slews), (0, 0), "the audio must never move");
}

// design 5844353368: the sticky content floor scenario -- a child module (this file sits at the
// ~1000-line budget); it reuses `run`, `Scenario` and the gate constants above.
#[path = "genlock_shallow_av_bench_sticky.rs"]
mod sticky;
