//! Issue 1367 (Option 3) — the two-clock A/V PAIRING bench for a genlocked NDI source that carries
//! its own audio (the SongPlayer → cg OBS chain).
//!
//! A test-only child of `genlock_audio_pairing` (declared there with `#[path]`), so the plain
//! standalone recipe `rustc --test --edition 2021 src/genlock_audio_pairing.rs` runs it too.
//!
//! ## Model
//!
//! - **Two clocks.** The WALL clock (the NDI timecodes, the video release deadline, the render tick
//!   grid) and OBS's MONOTONIC clock (QPC: the audio mixer, the render tick's `video_time`).
//!   `mono = wall + off(t)`, and `off` walks [`Bench::drift_total_ns`] (300 ms) linearly over the run:
//!   the resolume `wall_qpc_drift_ms` magnitude.
//! - **Video.** The sender stamps frames on the per-second grid, at the canvas rate or at
//!   `source_multiple` × it (a 60p source on a 30p canvas). The receiver FIFO presents a frame
//!   `d(t)` canvas frames behind the tick, `d` following a scripted depth profile (depth changes, a
//!   sender restart, an OBS restart). On an N ≥ 2 source the presented frame is the NEWEST matured
//!   one, (N − 1) source intervals younger than the queue head. The frame presents at the tick's
//!   monotonic instant. The receiver measures `tick − presented stamp` every tick and runs the
//!   production tracker [`video_delay_track`].
//! - **Audio ingest** runs the production decisions: [`audio_hold_mode`] / [`audio_hold_ms`] pick
//!   the hold, [`audio_wall_to_mono_ns`] reads the live offset, [`audio_place_term_ns`] places a
//!   packet, and a change re-places it and shifts the level target by [`audio_place_shift_ms`]. The
//!   packet timecode is the sender's emit instant, and each packet arrives after a 1–4 ms jittered
//!   lag. Placed audio plays back to back by sample count.
//! - **ASRC**, first order (the real servo is `media-io/asrc-compensator.c`, benched in
//!   `src/asrc_bench.rs`): a rate estimate that locks after [`ASRC_LOCK_S`] and converges on the
//!   true wall-vs-mono rate with an EMA of time constant [`ASRC_TAU_S`], plus a P level loop on the
//!   per-second mean buffered depth against the target captured at lock, clamped at
//!   [`LEVEL_MAX_PPM`]. A resync (sender restart) or an OBS restart re-captures, while a deliberate
//!   re-placement shifts the target. The rate estimate converges on the TRUE drift rate by
//!   construction, so this bench ASSUMES the drift is absorbed between placements. The real servo's
//!   ability to do that is proven separately, by `src/asrc_bench.rs`. What this bench proves is the
//!   placement: each (re-)placement must land at `timecode + live offset + measured delay`.
//!
//! The A/V error of a tick is `audio play instant(timecode = presented stamp) − video present
//! instant`, both on the monotonic clock. The gate excludes [`GATE_SKIP_S`] after the start and
//! after every scripted event: a video depth step is instantaneous, while the audio follows it after
//! the tracker's settle. Everywhere else, `|A/V| ≤ 5 ms`.
//!
//! **Anti-tautology variants** (each MUST fail the same gate):
//! - `LatchedOffset`: the placement uses the wall→mono offset latched at the first packet.
//! - `LegacyLatency`: the #1303 fixed `latency_ms` hold on the arrival basis.
//! - `HeadSample` (on a 2× source): the delay sampled on the queue HEAD instead of the presented
//!   frame over-reads by one source interval.

use super::*;

const IV_NUM: u64 = 1_000_000_000;
const FPS: u64 = 30;
const IV_NS: u64 = IV_NUM / FPS;
const W0: u64 = 1_790_000_000_000_000_000;
const MONO0: u64 = 86_400_000_000_000;
const LATENCY_MS: u32 = 3;
/// The rate servo locks this long after a (re)capture.
const ASRC_LOCK_S: f64 = 5.0;
/// The rate estimate's EMA time constant.
const ASRC_TAU_S: f64 = 20.0;
/// The level loop: ppm per ms of mean depth error, clamped.
const LEVEL_KP_PPM_PER_MS: f64 = 1.0;
const LEVEL_MAX_PPM: f64 = 20.0;
/// The gate skips this long after the start and after every scripted event.
const GATE_SKIP_S: u64 = 6;
/// The acceptance bound.
const GATE_MAX_AV_MS: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Production,
    LatchedOffset,
    LegacyLatency,
    HeadSample,
}

#[derive(Debug, Clone, Copy)]
enum Event {
    /// The FIFO settles at a new depth (frames).
    Depth(u64),
    /// The sender stops for `silent_s`, then resumes at a new depth; the audio resyncs.
    SenderRestart { silent_s: u64, depth: u64 },
    /// OBS restarts: every receiver state is zeroed; the FIFO comes back at `depth`.
    ObsRestart { depth: u64 },
}

struct Bench {
    duration_s: u64,
    drift_total_ns: i64,
    events: Vec<(u64, Event)>,
    initial_depth: u64,
    single_tick_hold_every: u64,
    /// Source frames per canvas frame (1 = canvas rate, 2 = a 60p source on a 30p canvas).
    source_multiple: u64,
}

impl Bench {
    fn scripted() -> Bench {
        Bench {
            duration_s: 3600,
            drift_total_ns: 300_000_000,
            initial_depth: 3,
            events: vec![
                (600, Event::Depth(2)),
                (1200, Event::Depth(4)),
                (
                    1800,
                    Event::SenderRestart {
                        silent_s: 3,
                        depth: 1,
                    },
                ),
                (2400, Event::ObsRestart { depth: 3 }),
                (3000, Event::Depth(2)),
            ],
            single_tick_hold_every: 0,
            source_multiple: 1,
        }
    }
}

#[derive(Debug, Default)]
struct Outcome {
    max_abs_av_ms: f64,
    gated_ticks: u64,
    placements: u32,
    final_drift_ns: i64,
    worst_at_s: f64,
}

struct Asrc {
    since_capture_s: f64,
    rate_est_ppm: f64,
    level_ppm: f64,
    target_ms: Option<f64>,
    win_sum: f64,
    win_n: u32,
}

impl Asrc {
    fn fresh() -> Asrc {
        Asrc {
            since_capture_s: 0.0,
            rate_est_ppm: 0.0,
            level_ppm: 0.0,
            target_ms: None,
            win_sum: 0.0,
            win_n: 0,
        }
    }
    fn recapture(&mut self) {
        self.since_capture_s = 0.0;
        self.level_ppm = 0.0;
        self.target_ms = None;
        self.win_sum = 0.0;
        self.win_n = 0;
    }
}

struct Audio {
    placed: bool,
    anchor_tc: u64,
    anchor_mono: u64,
    correction_ns: f64,
    timing_adjust: u64,
    mode: AudioHoldMode,
    hold_ms: u32,
    latched_off: Option<i64>,
}

impl Audio {
    fn fresh() -> Audio {
        Audio {
            placed: false,
            anchor_tc: 0,
            anchor_mono: 0,
            correction_ns: 0.0,
            timing_adjust: 0,
            mode: AudioHoldMode::Off,
            hold_ms: 0,
            latched_off: None,
        }
    }
    /// The monotonic instant the sample with timecode `tc` plays.
    fn play_mono(&self, tc: u64) -> f64 {
        self.anchor_mono as f64 + (tc as f64 - self.anchor_tc as f64) + self.correction_ns
    }
}

fn lcg(x: &mut u64) -> u64 {
    *x = x
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *x >> 33
}

fn simulate(b: &Bench, variant: Variant) -> Outcome {
    let ticks = b.duration_s * FPS;
    let mut out = Outcome::default();
    let mut rng = 0x1367u64;
    let mut tracker = VideoDelayTracker::default();
    let mut audio = Audio::fresh();
    let mut asrc = Asrc::fresh();
    let mut depth = b.initial_depth;
    let mut silent_until_tick = 0u64;
    let mut resync_pending = false;
    let mut skip_until_tick = GATE_SKIP_S * FPS;
    let true_ppm = b.drift_total_ns as f64 / (b.duration_s as f64 * 1e9) * 1e6;
    let dt_s = IV_NS as f64 / 1e9;

    for k in 0..ticks {
        // ---- scripted events -------------------------------------------------------------
        for &(at_s, ev) in &b.events {
            if k == at_s * FPS {
                skip_until_tick = k + GATE_SKIP_S * FPS;
                match ev {
                    Event::Depth(d) => depth = d,
                    Event::SenderRestart { silent_s, depth: d } => {
                        silent_until_tick = k + silent_s * FPS;
                        skip_until_tick = silent_until_tick + GATE_SKIP_S * FPS;
                        depth = d;
                        resync_pending = true;
                    }
                    Event::ObsRestart { depth: d } => {
                        tracker = VideoDelayTracker::default();
                        audio = Audio::fresh();
                        asrc = Asrc::fresh();
                        depth = d;
                    }
                }
            }
        }

        // ---- the two clocks at this render tick ---------------------------------------------
        let t_ns = k * IV_NUM / FPS;
        let wall = W0 + t_ns;
        let drift = (b.drift_total_ns as i128 * t_ns as i128
            / (b.duration_s as i128 * 1_000_000_000)) as i64;
        out.final_drift_ns = drift;
        let off = (MONO0 as i64).wrapping_sub(W0 as i64).wrapping_add(drift);
        let mono = (wall as i64).wrapping_add(off) as u64;

        // the sender runs silent across a restart
        if k < silent_until_tick {
            asrc.since_capture_s += dt_s;
            continue;
        }

        // ---- video: the presented head and the tracker ---------------------------------------
        let mut d = depth;
        if b.single_tick_hold_every > 0 && k % b.single_tick_hold_every == 0 {
            d += 1;
        }
        // the PRESENTED frame (the newest matured one) and, on an N >= 2 source, the queue head
        // (N - 1) source intervals older.
        let head = W0 + (k - d.min(k)) * IV_NUM / FPS;
        let queue_head = head - (b.source_multiple - 1) * IV_NUM / (FPS * b.source_multiple);
        let sampled = if variant == Variant::HeadSample {
            queue_head
        } else {
            head
        };
        video_delay_track(&mut tracker, video_delay_sample_ns(wall, sampled), IV_NS);

        // ---- audio: one packet timecoded at emit, arriving now --------------------------------
        let lag = 1_000_000 + lcg(&mut rng) % 3_000_000;
        let tc = wall - lag;
        let video_delay_ms = if variant == Variant::LegacyLatency {
            0
        } else {
            tracker.applied_ms
        };
        let mode = audio_hold_mode(true, LATENCY_MS, true, video_delay_ms);
        let hold = audio_hold_ms(mode, LATENCY_MS, video_delay_ms);
        let resync = !audio.placed || resync_pending;
        if resync {
            // reset_audio_timing: the arrival basis is re-captured
            audio.timing_adjust = mono.wrapping_sub(tc);
        }
        if resync || mode != audio.mode || hold != audio.hold_ms {
            let live = audio_wall_to_mono_ns(mono, wall);
            let off_used = match variant {
                Variant::LatchedOffset => *audio.latched_off.get_or_insert(live),
                _ => live,
            };
            let term = audio_place_term_ns(mode, hold, off_used, audio.timing_adjust);
            let prev_term =
                audio_place_term_ns(audio.mode, audio.hold_ms, off_used, audio.timing_adjust);
            let placed = tc
                .wrapping_add(audio.timing_adjust)
                .wrapping_add(term as u64);
            if resync {
                asrc.recapture();
            } else if let Some(t) = asrc.target_ms.as_mut() {
                *t += audio_place_shift_ms(term, prev_term);
            }
            audio.placed = true;
            audio.anchor_tc = tc;
            audio.anchor_mono = placed;
            audio.correction_ns = 0.0;
            audio.mode = mode;
            audio.hold_ms = hold;
            resync_pending = false;
            out.placements += 1;
        }

        // ---- ASRC: rate servo + level loop ----------------------------------------------------
        asrc.since_capture_s += dt_s;
        if asrc.since_capture_s >= ASRC_LOCK_S {
            let alpha = dt_s / (ASRC_TAU_S + dt_s);
            asrc.rate_est_ppm += alpha * (true_ppm - asrc.rate_est_ppm);
        }
        audio.correction_ns += (asrc.rate_est_ppm + asrc.level_ppm) * 1e-6 * IV_NS as f64;
        let depth_ms = (audio.play_mono(tc) - mono as f64) / 1e6;
        asrc.win_sum += depth_ms;
        asrc.win_n += 1;
        if asrc.win_n as u64 == FPS {
            let mean = asrc.win_sum / asrc.win_n as f64;
            asrc.win_sum = 0.0;
            asrc.win_n = 0;
            if asrc.since_capture_s >= ASRC_LOCK_S {
                match asrc.target_ms {
                    None => asrc.target_ms = Some(mean),
                    Some(target) => {
                        asrc.level_ppm = (LEVEL_KP_PPM_PER_MS * (target - mean))
                            .clamp(-LEVEL_MAX_PPM, LEVEL_MAX_PPM)
                    }
                }
            }
        }

        // ---- the A/V error of this tick ----------------------------------------------------------
        let av_ms = (audio.play_mono(head) - mono as f64) / 1e6;
        if k >= skip_until_tick {
            out.gated_ticks += 1;
            if av_ms.abs() > out.max_abs_av_ms {
                out.max_abs_av_ms = av_ms.abs();
                out.worst_at_s = t_ns as f64 / 1e9;
            }
        }
    }
    out
}

#[test]
fn av_pair_within_5ms_across_depth_changes_restarts_and_300ms_drift() {
    let b = Bench::scripted();
    let o = simulate(&b, Variant::Production);
    eprintln!("production: {o:?}");
    assert!(
        o.final_drift_ns >= 299_000_000,
        "the bench must walk the wall-vs-mono offset by 300 ms, walked {} ns",
        o.final_drift_ns
    );
    assert!(
        o.gated_ticks > 90_000,
        "the gate must cover most of the run, got {}",
        o.gated_ticks
    );
    assert!(
        o.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms at t={:.1} s exceeds {GATE_MAX_AV_MS} ms",
        o.max_abs_av_ms,
        o.worst_at_s
    );
    // one start placement + a latency→timecode switch, one re-placement per depth change, the
    // resync + its re-application, the OBS restart's placement + switch: no churn beyond that.
    assert!(o.placements <= 10, "placements = {} (churn)", o.placements);
}

#[test]
fn a_placement_latched_at_the_first_packet_fails_the_gate() {
    let o = simulate(&Bench::scripted(), Variant::LatchedOffset);
    eprintln!("latched: {o:?}");
    assert!(
        o.max_abs_av_ms > 50.0,
        "a latched wall→mono offset must walk the audio off the video by the drift, got {:.2} ms",
        o.max_abs_av_ms
    );
}

#[test]
fn the_1303_fixed_latency_hold_fails_the_gate() {
    let o = simulate(&Bench::scripted(), Variant::LegacyLatency);
    eprintln!("legacy: {o:?}");
    assert!(
        o.max_abs_av_ms > 50.0,
        "the fixed 3 ms arrival hold must lead the video by ~its depth, got {:.2} ms",
        o.max_abs_av_ms
    );
}

#[test]
fn single_tick_holds_neither_churn_nor_break_the_pairing() {
    let mut b = Bench::scripted();
    // one tick in 15 presents a frame one tick older (a hold on a shallow cg feed).
    b.single_tick_hold_every = 15;
    let o = simulate(&b, Variant::Production);
    let base = simulate(&Bench::scripted(), Variant::Production);
    eprintln!("holds: {o:?}");
    assert_eq!(
        o.placements, base.placements,
        "single held ticks must not re-place the audio"
    );
    // the held ticks themselves present a frame 33 ms older than the audio beside it — that is the
    // video's own repeat, not a pairing error; every other tick stays paired.
    assert!(o.max_abs_av_ms <= IV_NS as f64 / 1e6 + GATE_MAX_AV_MS);
}

#[test]
fn a_source_at_twice_the_canvas_rate_pairs_on_the_presented_frame() {
    let mut b = Bench::scripted();
    b.source_multiple = 2;
    let o = simulate(&b, Variant::Production);
    eprintln!("2x production: {o:?}");
    assert!(
        o.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms at t={:.1} s on a 2x source exceeds {GATE_MAX_AV_MS} ms",
        o.max_abs_av_ms,
        o.worst_at_s
    );
    let h = simulate(&b, Variant::HeadSample);
    eprintln!("2x head-sample: {h:?}");
    assert!(
        h.max_abs_av_ms > 10.0,
        "sampling the queue head of a 2x source must over-read by a source interval, got {:.2} ms",
        h.max_abs_av_ms
    );
}
