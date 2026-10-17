//! Issue 1425 (E3 slice S4, design 6225701358) — the two-clock bench of the ARRIVAL rebase, on the
//! plant of `asrc_stamp_bench.rs` (a `#[path]` child of it: the same OBS mixer, `asrc_process_audio`
//! and arrival servo), with OBS's ingest replayed for a source whose stamps are NOT direct (libobs
//! latches `timing_adjust` at its first packet).
//!
//! - **The VLC model** (stream's `NL playlist`, the stock `vlc-video`): 1024-frame packets whose stamps
//!   run on the source's own SAMPLE clock (`libvlc_clock()` relative to plugin load: each packet's stamp
//!   advances by its nominal duration), the source +20 ppm fast against the mixer. The arrival servo
//!   locks and compresses the packets by ~20 ppm, so `next_audio_ts_min` (advanced by the RESAMPLED
//!   frames) falls behind the stamps at ~72 ms/h. Without the rebase, OBS's 70 ms smoothing PLACES a
//!   packet at its stamp after ~55 min: a ~70 ms zero-filled gap the arrival servo's level loop then
//!   compresses away, which is stretch again, so the next placement comes sooner (consult F2).
//!   Measured: 62 placements in 8 h, every ~7 min once it runs, the depth up to 575 ms off the one it
//!   locked at. With the rebase (`audio_arrival_rebase`, the production decision) the packet appends:
//!   no placement, nothing zero-filled or overwritten, 8 counted rebases (one per 70 ms of drift), the
//!   depth as close to the one it locked at as the NDI twin's.
//! - **The NDI twin**: an NDI sender with genlock off stamps on its disciplined WALL clock (the mixer's
//!   rate) while its samples come from a card +20 ppm fast. The resampled continuation follows the
//!   stamps, so neither OBS nor the rebase ever does anything: 0 placements and 0 rebases both ways.
//!
//! The rebase is applied the way `source_output_audio_data` applies it: in the stamp domain before the
//! smoothing, in the system domain before the push-back check, for a source whose arrival servo has
//! captured its level.

use super::*;
use crate::genlock_audio_pairing::{audio_arrival_rebase, AUDIO_TS_SMOOTHING_NS};

const VLC_FRAMES: u64 = 1024;
const SRC_PPM: f64 = 20.0;
/// The stamp-domain origin of the VLC model (`libvlc_clock() - time_start`, a few seconds after the
/// plugin loaded): far from the mixer clock, so libobs latches `timing_adjust`.
const VLC_STAMP0: u64 = 5_000_000_000;
/// The NDI sender's wall clock (Unix epoch): not direct either.
const NDI_WALL0: u64 = 1_790_000_000_000_000_000;
/// The arrival servo settles on its depth by then (the regression locks after 60 s).
const SETTLED_S: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stamps {
    /// Stock VLC: the stamp advances by the packet's nominal duration (the sample clock).
    SampleClock,
    /// NDI with genlock off: the stamp is the sender's wall (= mixer-rate) completion time.
    WallClock,
}

#[derive(Debug, Default)]
struct RebaseRun {
    /// Packets placed after the first one (the source's first packet is always placed).
    placements: u32,
    /// The largest |placement - buffer end| of those (ms): the zero-filled gap or the overwrite.
    max_place_ms: f64,
    /// The sum of those (ms).
    placed_ms: f64,
    /// Arrival times (s) of those placements.
    place_at_s: Vec<f64>,
    /// Packets the rebase let append (the C counter).
    rebases: u32,
    /// The largest |level_avg - the depth at lock| from SETTLED_S on (ms).
    max_level_err_ms: f64,
    /// The arrival servo captured its depth.
    captured: bool,
}

/// OBS's `source_output_audio_data` for one non-direct, Off-hold packet; returns whether it was
/// placed (and where, against the buffer end).
struct Ingest {
    rule: bool,
    timing_set: bool,
    timing_adjust: u64,
}

impl Ingest {
    fn packet(&mut self, obs: &mut Obs, stamp: u64, now: u64, dur: u64) -> (bool, i64, bool) {
        let mut in_ts = stamp;
        assert!(
            stamp.abs_diff(now) >= MAX_TS_VAR,
            "issue 1425 bench: the source's stamps are not direct"
        );
        // the basis the ingest predicts: an Off hold on non-direct stamps, mixed, a resampler
        let basis = audio_asrc_placement(AudioHoldMode::Off, false, false, true);
        assert_eq!(basis, AsrcBasis::Arrival);
        obs.c.set_basis(basis);
        let captured = obs.c.level_captured();
        let rule = self.rule;
        let rebase =
            move |diff: u64| rule && audio_arrival_rebase(basis, false, true, captured, diff);
        let rebase_ts =
            self.timing_set && obs.next_ts_min != 0 && rebase(obs.next_ts_min.abs_diff(stamp));
        if rebase_ts {
            obs.next_ts_min = stamp;
        }
        if !self.timing_set {
            self.timing_set = true;
            self.timing_adjust = now.wrapping_sub(stamp);
        } else if obs.next_ts_min != 0 {
            let diff = obs.next_ts_min.abs_diff(in_ts);
            if diff > MAX_TS_VAR {
                // handle_ts_jump: timing re-latched, the queued buffer dropped
                self.timing_adjust = now.wrapping_sub(in_ts);
                obs.audio_ts = now;
                obs.end = now;
                obs.next_sys_min = now;
            } else if diff < TS_SMOOTHING_THRESHOLD {
                in_ts = obs.next_ts_min;
            }
        }
        obs.next_ts_min = in_ts + dur;
        in_ts = in_ts.wrapping_add(self.timing_adjust);
        let rebase_sys = obs.next_sys_min != 0 && rebase(obs.next_sys_min.abs_diff(in_ts));
        if rebase_sys {
            obs.next_sys_min = in_ts;
        }
        let push_back = obs.next_sys_min == in_ts
            || (obs.next_sys_min != 0 && obs.next_sys_min.abs_diff(in_ts) < TS_SMOOTHING_THRESHOLD);
        obs.next_sys_min = obs.next_ts_min.wrapping_add(self.timing_adjust);
        let appended = push_back && obs.audio_ts != 0;
        let off = in_ts as i64 - obs.end as i64;
        if appended {
            obs.end += dur;
        } else {
            if obs.audio_ts == 0 || in_ts < obs.audio_ts {
                obs.audio_ts = in_ts;
                obs.next_sys_min = in_ts;
            }
            obs.end = in_ts + dur;
        }
        (!appended, off, appended && (rebase_ts || rebase_sys))
    }
}

fn run_rebase(stamps: Stamps, rule: bool, run_s: u64, frames: u64) -> RebaseRun {
    let src = Source {
        name: "vlc",
        packet_frames: frames,
        jitter_ns: 1_000_000,
    };
    let case = Case::new(src, SRC_PPM, 4, 1, Servo::Arrival);
    let mut obs = Obs::new(&case);
    let mut ing = Ingest {
        rule,
        timing_set: false,
        timing_adjust: 0,
    };
    // the same delivery (completion + jitter) for every stamp model and rule: only the stamps differ
    let mut rng = Lcg(0x1425_6225);
    let packet_ns = frames_ns(frames);
    let mut start = (MONO0 + case.phase_ns) as f64;
    let mut prev_arrival = 0_u64;
    let mut k = 0_u64;
    let mut r = RebaseRun::default();
    let end_ns = (MONO0 + run_s * NS_PER_S) as f64;
    while start < end_ns {
        let complete = start + packet_ns as f64 / (1.0 + SRC_PPM * 1e-6);
        let arrival = ((complete + src.jitter_ns as f64 * rng.next()) as u64).max(prev_arrival);
        prev_arrival = arrival;
        let stamp = match stamps {
            Stamps::SampleClock => VLC_STAMP0 + k * packet_ns,
            Stamps::WallClock => NDI_WALL0 + (complete as u64 - MONO0) - packet_ns,
        };
        obs.mix_until(arrival);
        let dur = obs.asrc_process(arrival);
        let (placed, off, rebased) = ing.packet(&mut obs, stamp, arrival, dur);
        if placed && k > 0 {
            let ms = off as f64 / 1e6;
            r.placements += 1;
            r.max_place_ms = r.max_place_ms.max(ms.abs());
            r.placed_ms += ms.abs();
            r.place_at_s.push((arrival - MONO0) as f64 / 1e9);
        }
        if rebased {
            r.rebases += 1;
        }
        let t_s = (arrival - MONO0) / NS_PER_S;
        if t_s >= SETTLED_S && obs.c.level_captured() {
            let err = (obs.c.level_avg_ms() - obs.c.level_target_ms()).abs();
            r.max_level_err_ms = r.max_level_err_ms.max(err);
        }
        start = complete;
        k += 1;
    }
    r.captured = obs.c.level_captured();
    r
}

/// The largest interval between two consecutive placements (min).
fn max_interval_min(at_s: &[f64]) -> f64 {
    at_s.windows(2)
        .map(|w| (w[1] - w[0]) / 60.0)
        .fold(0.0, f64::max)
}

#[test]
fn stock_obs_places_a_drifting_vlc_source_70_ms_about_hourly_1425() {
    // anti-tautology (the stock ingest, no rebase): the arrival servo's stretch walks the continuation
    // off the VLC stamps, and OBS's 70 ms smoothing places a packet at its stamp; each placement is a
    // ~70 ms gap the level loop then compresses away, which is stretch again, so the next one comes
    // sooner (consult F2): measured 62 placements in 8 h, the first at 55 min, then every ~7 min, the
    // depth up to 575 ms off the one it locked at
    let r = run_rebase(Stamps::SampleClock, false, EIGHT_HOURS_S, VLC_FRAMES);
    assert!(r.captured, "issue 1425 bench: the arrival servo must lock");
    assert!(
        r.placements >= 8,
        "issue 1425: over 8 h at +20 ppm the stock ingest must place the VLC source at least \
         hourly: {} placements at {:?} s",
        r.placements,
        r.place_at_s
    );
    assert!(
        r.place_at_s[0] <= 3600.0 && max_interval_min(&r.place_at_s) <= 58.0,
        "issue 1425: the first placement within the first hour, then one at least every 58 min: \
         {:?}",
        r.place_at_s
    );
    let mean_place = r.placed_ms / f64::from(r.placements);
    assert!(
        mean_place >= 69.0 && r.max_place_ms < 80.0,
        "issue 1425: each placement is a ~70 ms gap or overwrite: mean {mean_place:.2} ms, max {:.2} ms",
        r.max_place_ms
    );
    assert_eq!(r.rebases, 0, "issue 1425: the stock ingest rebases nothing");
    assert!(
        r.max_level_err_ms > 70.0,
        "issue 1425: the placements move the arrival servo's depth off the one it locked at: {:.2} ms",
        r.max_level_err_ms
    );
}

#[test]
fn a_locked_vlc_source_is_never_placed_by_its_stamps_1425() {
    // the VLC packet (one mixer tick) and the mbc-sized one: the depth reading of a tick-long packet
    // carries its walking packet-vs-tick phase (the NDI twin reads the same 14.4 ms at 1024 frames, 1.1 ms
    // at 128), so the depth is judged against the twin on the same delivery: the rebase adds nothing
    for frames in [VLC_FRAMES, 128] {
        let r = run_rebase(Stamps::SampleClock, true, EIGHT_HOURS_S, frames);
        let twin = run_rebase(Stamps::WallClock, true, EIGHT_HOURS_S, frames);
        assert!(r.captured, "issue 1425 bench: the arrival servo must lock");
        assert_eq!(
            r.placements, 0,
            "issue 1425: {frames}-frame packets: a locked arrival source must never be placed by its \
             stamps over 8 h: {} placements ({:.1} ms zero-filled or overwritten) at {:?} s",
            r.placements, r.placed_ms, r.place_at_s
        );
        assert!(
            (7..=9).contains(&r.rebases),
            "issue 1425: {frames}-frame packets: one counted rebase per 70 ms of drift (~72 ms/h at \
             +20 ppm): {}",
            r.rebases
        );
        assert!(
            r.max_level_err_ms <= twin.max_level_err_ms + 0.5,
            "issue 1425: {frames}-frame packets: the depth stays on the one it locked at as closely as \
             the NDI twin's from {SETTLED_S} s on: {:.3} ms vs {:.3} ms",
            r.max_level_err_ms,
            twin.max_level_err_ms
        );
    }
}

#[test]
fn an_ndi_wall_clock_source_never_reaches_the_band_either_way_1425() {
    let without = run_rebase(Stamps::WallClock, false, EIGHT_HOURS_S, VLC_FRAMES);
    let with = run_rebase(Stamps::WallClock, true, EIGHT_HOURS_S, VLC_FRAMES);
    for (rule, r) in [(false, &without), (true, &with)] {
        assert!(r.captured, "issue 1425 bench: the arrival servo must lock");
        assert!(
            r.placements == 0 && r.rebases == 0,
            "issue 1425: an NDI source stamping on its wall clock (rebase rule {rule}) stays on its \
             continuation: {} placements, {} rebases",
            r.placements,
            r.rebases
        );
    }
    assert_eq!(
        with.max_level_err_ms.to_bits(),
        without.max_level_err_ms.to_bits(),
        "issue 1425: the rebase changes nothing for a source that never drifts: {:.6} vs {:.6} ms",
        with.max_level_err_ms,
        without.max_level_err_ms
    );
    // the threshold the bench replays is the decision's own
    assert_eq!(TS_SMOOTHING_THRESHOLD, AUDIO_TS_SMOOTHING_NS);
}
