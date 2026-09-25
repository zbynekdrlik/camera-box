//! Issue 1367 — the two-clock A/V PAIRING bench for a genlocked NDI source that carries its own
//! audio (the SongPlayer → cg OBS chain).
//!
//! A test-only child of `genlock_audio_pairing` (declared there with `#[path]`), so the plain
//! standalone recipe `rustc --test --edition 2021 src/genlock_audio_pairing.rs` runs it too. Its
//! audio leg ([`AudioLeg`]) is shared with the shallow-source bench (`genlock_shallow_av_bench.rs`,
//! a child of this module), which drives it from a real simulated FIFO.
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
//!   sender restart, an OBS restart) — the FREE Option-3 tracker path (an N ≥ 2 source; a shallow
//!   N == 1 source now LOCKS its depth, see the shallow bench). On an N ≥ 2 source the presented
//!   frame is the NEWEST matured one, (N − 1) source intervals younger than the queue head. The frame
//!   presents at the tick's monotonic instant. The receiver measures `tick − presented stamp` every
//!   tick and runs the production tracker [`video_delay_track`].
//! - **Audio ingest** runs the production decisions ([`AudioLeg::ingest`]): [`audio_hold_mode`] /
//!   [`audio_hold_ms`] pick the hold (WITHHELD while the delay is unknown, [`audio_withhold_expired`]),
//!   [`audio_hold_action`] decides place / continue / SLEW / step, [`audio_wall_to_mono_ns`] reads the
//!   live offset and [`audio_place_term_ns`] places. A hold change while playing is SLEWED
//!   ([`audio_slew_step_ns`]): the placement moves 1 ms per second through the resampler, and the
//!   level target moves with each consumed increment. The packet timecode is the sender's emit
//!   instant, and each packet arrives after a 1–4 ms jittered lag. Placed audio plays back to back by
//!   sample count.
//! - **ASRC**, first order (the real servo is `media-io/asrc-compensator.c`, benched in
//!   `src/asrc_bench.rs`): a rate estimate that locks after [`ASRC_LOCK_S`] and converges on the
//!   true wall-vs-mono rate with an EMA of time constant [`ASRC_TAU_S`], plus a P level loop on the
//!   per-second mean buffered depth against the target captured at lock, clamped at
//!   [`LEVEL_MAX_PPM`]. A resync (sender restart) or an OBS restart re-captures, while a deliberate
//!   re-placement or slew shifts the target. The rate estimate converges on the TRUE drift rate by
//!   construction, so this bench ASSUMES the drift is absorbed between placements. The real servo's
//!   ability to do that is proven separately, by `src/asrc_bench.rs`. What this bench proves is the
//!   placement: each (re-)placement must land at `timecode + live offset + measured delay`, and a
//!   hold change converges there by a slew, never a step.
//!
//! The A/V error of a tick is `audio play instant(timecode = presented stamp) − video present
//! instant`, both on the monotonic clock. The gate excludes [`GATE_SKIP_S`] after the start and
//! after every scripted event, and every tick while a deliberate slew is still converging (a free
//! tracker re-times the audio 1 ms per second toward the new depth). Everywhere else,
//! `|A/V| ≤ 5 ms`.
//!
//! **Anti-tautology variants** (each MUST fail the same gate):
//! - `LatchedOffset`: the placement uses the wall→mono offset latched at the first packet.
//! - `LegacyLatency`: the #1303 fixed `latency_ms` hold on the arrival basis.
//! - `HeadSample` (on a 2× source): the delay sampled on the queue HEAD instead of the presented
//!   frame over-reads by one source interval.
//!
//! The 2× `Production` run gives the same result as the 1× run, because the bench's presented stamp
//! does not depend on the source rate. It adds no evidence of its own. Only `HeadSample`
//! discriminates the two samplings. That the C samples the PRESENTED frame (`next_frame`) is pinned
//! by `tests/genlock_audio_timecode_placement_1367.rs`, not by this bench.

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
    /// The slew steps are not booked out of the smoothing timeline.
    NoBooking,
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
    places: u32,
    slews: u32,
    steps: u32,
    slewing_ticks: u64,
    smoothing_snaps: u32,
    max_ts_div_ns: u64,
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
    /// The deliberate level shift of `asrc_compensator_shift_level_target`: the target AND the open
    /// window's readings move by `delta_ms`.
    fn shift(&mut self, delta_ms: f64) {
        self.win_sum += delta_ms * self.win_n as f64;
        if let Some(t) = self.target_ms.as_mut() {
            *t += delta_ms;
        }
    }
}

/// The audio leg of one source: the ingest decisions, the placement, the slew and the ASRC — the
/// production seams of `source_output_audio_data` + `asrc_process_audio`, in one struct so the
/// shallow-source bench drives the SAME model.
pub(super) struct AudioLeg {
    placed: bool,
    timing_set: bool,
    anchor_tc: u64,
    anchor_mono: u64,
    correction_ns: f64,
    timing_adjust: u64,
    mode: AudioHoldMode,
    hold_ms: u32,
    first_packet_mono: u64,
    latched_off: Option<i64>,
    latch_offset: bool,
    slew_remaining_ns: i64,
    asrc: Asrc,
    /// The ingest's smoothing timeline (`next_audio_ts_min`) and the source's own next timestamp:
    /// a slew step stretches the samples, so unless it is BOOKED ([`audio_slew_book_ts_ns`]) the
    /// smoothed timeline walks away from the source time, and at `TS_SMOOTHING_THRESHOLD` (70 ms)
    /// the ingest snaps the audio back to the old placement. Only the SLEW is modelled here (the
    /// servo's own stretch has the same class of limit, recorded in the rule).
    book_steps: bool,
    ts_raw_next: u64,
    ts_next_min: u64,
    pub(super) smoothing_snaps: u32,
    pub(super) max_ts_div_ns: u64,
    pub(super) places: u32,
    pub(super) slews: u32,
    pub(super) steps: u32,
    pub(super) withheld: u64,
    /// Issue 1367 (live 25.9.2026 12:31): the anti-tautology variant — a packet right after a
    /// timeline reset is APPENDED as OBS decides, never corrected by [`audio_push_back_allowed`].
    legacy_append_after_reset: bool,
    /// The smoothed placement error the production ingest measures (actual − intended).
    place_err_ns: i64,
    place_err_seeded: bool,
}

/// `TS_SMOOTHING_THRESHOLD` of `obs-source.c`.
const TS_SMOOTHING_THRESHOLD_NS: u64 = 70_000_000;

impl AudioLeg {
    pub(super) fn fresh(latch_offset: bool) -> AudioLeg {
        AudioLeg {
            placed: false,
            timing_set: false,
            anchor_tc: 0,
            anchor_mono: 0,
            correction_ns: 0.0,
            timing_adjust: 0,
            mode: AudioHoldMode::Off,
            hold_ms: 0,
            first_packet_mono: 0,
            latched_off: None,
            latch_offset,
            slew_remaining_ns: 0,
            asrc: Asrc::fresh(),
            book_steps: true,
            ts_raw_next: 0,
            ts_next_min: 0,
            smoothing_snaps: 0,
            max_ts_div_ns: 0,
            places: 0,
            slews: 0,
            steps: 0,
            withheld: 0,
            legacy_append_after_reset: false,
            place_err_ns: 0,
            place_err_seeded: false,
        }
    }

    /// The anti-tautology variant of the live 12:31 defect: OBS's append after a timeline reset is
    /// taken as-is, so the hold is lost at every sender restart.
    pub(super) fn with_legacy_append_after_reset(mut self) -> AudioLeg {
        self.legacy_append_after_reset = true;
        self
    }

    /// The pairing offset's AUDIO side as the production audit computes it
    /// ([`audio_realized_delay_ns`] over the measured placement error).
    pub(super) fn realized_delay_ns(&self) -> i64 {
        audio_realized_delay_ns(
            self.hold_ms,
            self.slew_remaining_ns,
            self.place_err_ns,
            self.place_err_seeded,
        )
    }

    /// The anti-tautology variant: the slew steps are NOT booked out of the smoothing timeline.
    pub(super) fn without_booking(mut self) -> AudioLeg {
        self.book_steps = false;
        self
    }

    /// Audio is in the mix.
    pub(super) fn playing(&self) -> bool {
        self.placed
    }

    /// A deliberate slew is still converging.
    pub(super) fn slewing(&self) -> bool {
        self.slew_remaining_ns != 0
    }

    /// The monotonic instant the sample with timecode `tc` plays.
    fn play_mono(&self, tc: u64) -> f64 {
        self.anchor_mono as f64 + (tc as f64 - self.anchor_tc as f64) + self.correction_ns
    }

    /// The A/V error (ms) of a frame stamped `presented_stamp` presenting at `present_mono`.
    pub(super) fn av_ms(&self, presented_stamp: u64, present_mono: u64) -> f64 {
        (self.play_mono(presented_stamp) - present_mono as f64) / 1e6
    }

    /// One audio packet with timecode `tc`, arriving at `mono` / `wall`, while the render thread's
    /// applied video delay is `video_delay_ms`. `resync` = a timeline discontinuity (a sender
    /// restart: `reset_audio_timing`).
    pub(super) fn ingest(
        &mut self,
        tc: u64,
        mono: u64,
        wall: u64,
        video_delay_ms: u32,
        resync: bool,
    ) {
        if self.first_packet_mono == 0 {
            self.first_packet_mono = mono.max(1);
        }
        let expired = audio_withhold_expired(self.first_packet_mono, mono);
        let mode = audio_hold_mode(true, LATENCY_MS, true, video_delay_ms, expired);
        let hold = audio_hold_ms(mode, LATENCY_MS, video_delay_ms);
        if resync || !self.timing_set {
            // reset_audio_timing: the arrival basis is re-captured
            self.timing_adjust = mono.wrapping_sub(tc);
            self.timing_set = true;
        }
        // OBS's own continuity verdict: back to back. A sender restart's >2 s timestamp jump runs
        // handle_ts_jump, which empties the buffer and puts its start AND next_audio_sys_ts_min on the
        // ARRIVAL instant -- the packet's pre-term timestamp equals it, so OBS still appends (live
        // 25.9.2026 12:31); the production ingest corrects that for an active genlock hold.
        let obs_push_back = self.placed;
        let continuous = if self.legacy_append_after_reset {
            obs_push_back
        } else {
            audio_push_back_allowed(obs_push_back, resync, mode)
        };
        let action = audio_hold_action(
            self.mode,
            self.hold_ms,
            mode,
            hold,
            continuous,
            true,
            self.slew_remaining_ns != 0,
        );
        let live = audio_wall_to_mono_ns(mono, wall);
        let off_used = if self.latch_offset {
            *self.latched_off.get_or_insert(live)
        } else {
            live
        };
        let term = audio_place_term_ns(mode, hold, off_used, self.timing_adjust);
        let prev_term = audio_place_term_ns(self.mode, self.hold_ms, off_used, self.timing_adjust);
        match action {
            AudioHoldAction::Withhold => {
                self.withheld += 1;
                self.placed = false;
            }
            AudioHoldAction::Slew => {
                self.slew_remaining_ns = self
                    .slew_remaining_ns
                    .wrapping_add(term.wrapping_sub(prev_term));
                self.slews += 1;
            }
            AudioHoldAction::Continue if continuous => {
                if resync {
                    // appended at the reset buffer start: the ARRIVAL instant -- the genlock term
                    // never reaches the samples (the live 12:31 defect).
                    self.asrc.recapture();
                    self.anchor_tc = tc;
                    self.anchor_mono = mono;
                    self.correction_ns = 0.0;
                    self.ts_raw_next = tc;
                    self.ts_next_min = tc;
                }
            }
            AudioHoldAction::Place | AudioHoldAction::Replace | AudioHoldAction::Continue => {
                let shift = audio_level_shift_ns(
                    action,
                    self.mode,
                    term,
                    prev_term,
                    self.slew_remaining_ns,
                );
                if !continuous || !self.mode.is_active() {
                    self.asrc.recapture();
                } else {
                    self.asrc.shift(shift as f64 / 1e6);
                }
                if action == AudioHoldAction::Replace {
                    self.steps += 1;
                } else {
                    self.places += 1;
                }
                self.placed = true;
                self.anchor_tc = tc;
                self.anchor_mono = tc
                    .wrapping_add(self.timing_adjust)
                    .wrapping_add(term as u64);
                self.correction_ns = 0.0;
                self.slew_remaining_ns = 0;
                self.ts_raw_next = tc;
                self.ts_next_min = tc;
            }
        }
        self.mode = mode;
        self.hold_ms = hold;
        // the production ingest's placement measurement: where this packet's first sample actually
        // plays (the modelled buffer) against where the hold meant it to (in.timestamp after the term).
        if self.placed && mode.is_active() {
            let intended = tc
                .wrapping_add(self.timing_adjust)
                .wrapping_add(term as u64);
            let actual = self.play_mono(tc).round() as u64;
            let err = audio_place_error_ns(actual, intended);
            self.place_err_ns =
                audio_place_error_smooth_ns(self.place_err_ns, err, self.place_err_seeded);
            self.place_err_seeded = true;
        } else {
            self.place_err_seeded = false;
        }
    }

    /// One render tick of the ASRC (`dt_ns` of audio): the rate servo, the slew increment (the
    /// resampler stretch of `asrc_process_audio`, shifting the level target by the same amount) and
    /// the level loop. `tc_now` is the latest packet's timecode, `mono` the tick's monotonic instant.
    pub(super) fn asrc_tick(&mut self, true_ppm: f64, dt_ns: u64, tc_now: u64, mono: u64) {
        let dt_s = dt_ns as f64 / 1e9;
        self.asrc.since_capture_s += dt_s;
        if !self.placed {
            return;
        }
        if self.asrc.since_capture_s >= ASRC_LOCK_S {
            let alpha = dt_s / (ASRC_TAU_S + dt_s);
            self.asrc.rate_est_ppm += alpha * (true_ppm - self.asrc.rate_est_ppm);
        }
        let step = audio_slew_step_ns(self.slew_remaining_ns, dt_ns);
        self.slew_remaining_ns -= step;
        self.correction_ns += step as f64;
        self.asrc.shift(step as f64 / 1e6);
        // the smoothing timeline: the source advanced dt, the stretched samples dt + step.
        self.ts_raw_next = self.ts_raw_next.wrapping_add(dt_ns);
        let smoothed = self
            .ts_next_min
            .wrapping_add(dt_ns)
            .wrapping_add(step as u64);
        self.ts_next_min = if self.book_steps {
            audio_slew_book_ts_ns(smoothed, step)
        } else {
            smoothed
        };
        let div = (self.ts_next_min.wrapping_sub(self.ts_raw_next) as i64).unsigned_abs();
        self.max_ts_div_ns = self.max_ts_div_ns.max(div);
        if div >= TS_SMOOTHING_THRESHOLD_NS {
            // the ingest takes the raw timestamp: the audio snaps back by the divergence.
            let back = self.ts_next_min.wrapping_sub(self.ts_raw_next) as i64;
            self.correction_ns -= back as f64;
            self.ts_next_min = self.ts_raw_next;
            self.smoothing_snaps += 1;
        }
        self.correction_ns += (self.asrc.rate_est_ppm + self.asrc.level_ppm) * 1e-6 * dt_ns as f64;
        let depth_ms = (self.play_mono(tc_now) - mono as f64) / 1e6;
        self.asrc.win_sum += depth_ms;
        self.asrc.win_n += 1;
        if self.asrc.win_n as u64 == IV_NUM / dt_ns.max(1) {
            let mean = self.asrc.win_sum / self.asrc.win_n as f64;
            self.asrc.win_sum = 0.0;
            self.asrc.win_n = 0;
            if self.asrc.since_capture_s >= ASRC_LOCK_S {
                match self.asrc.target_ms {
                    None => self.asrc.target_ms = Some(mean),
                    Some(target) => {
                        self.asrc.level_ppm = (LEVEL_KP_PPM_PER_MS * (target - mean))
                            .clamp(-LEVEL_MAX_PPM, LEVEL_MAX_PPM)
                    }
                }
            }
        }
    }
}

pub(super) fn lcg(x: &mut u64) -> u64 {
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
    let leg = |v: Variant| {
        let a = AudioLeg::fresh(v == Variant::LatchedOffset);
        if v == Variant::NoBooking {
            a.without_booking()
        } else {
            a
        }
    };
    let mut audio = leg(variant);
    let mut depth = b.initial_depth;
    let mut snaps = 0u32;
    let mut max_div = 0u64;
    let mut silent_until_tick = 0u64;
    let mut resync_pending = false;
    let mut skip_until_tick = GATE_SKIP_S * FPS;
    let true_ppm = b.drift_total_ns as f64 / (b.duration_s as f64 * 1e9) * 1e6;
    let mut totals = (0u32, 0u32, 0u32);

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
                        totals.0 += audio.places;
                        totals.1 += audio.slews;
                        totals.2 += audio.steps;
                        snaps += audio.smoothing_snaps;
                        max_div = max_div.max(audio.max_ts_div_ns);
                        audio = leg(variant);
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
        video_delay_track(&mut tracker, 0, video_delay_sample_ns(wall, sampled), IV_NS);

        // ---- audio: one packet timecoded at emit, arriving now --------------------------------
        let lag = 1_000_000 + lcg(&mut rng) % 3_000_000;
        let tc = wall - lag;
        let video_delay_ms = if variant == Variant::LegacyLatency {
            0
        } else {
            tracker.applied_ms
        };
        audio.ingest(tc, mono, wall, video_delay_ms, resync_pending);
        resync_pending = false;
        audio.asrc_tick(true_ppm, IV_NS, tc, mono);

        // ---- the A/V error of this tick ----------------------------------------------------------
        if audio.slewing() {
            out.slewing_ticks += 1;
        }
        if audio.playing() && k >= skip_until_tick && !audio.slewing() {
            let av_ms = audio.av_ms(head, mono);
            out.gated_ticks += 1;
            if av_ms.abs() > out.max_abs_av_ms {
                out.max_abs_av_ms = av_ms.abs();
                out.worst_at_s = t_ns as f64 / 1e9;
            }
        }
    }
    out.places = totals.0 + audio.places;
    out.slews = totals.1 + audio.slews;
    out.steps = totals.2 + audio.steps;
    out.smoothing_snaps = snaps + audio.smoothing_snaps;
    out.max_ts_div_ns = max_div.max(audio.max_ts_div_ns);
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
        o.gated_ticks > 60_000,
        "the gate must cover most of the run, got {}",
        o.gated_ticks
    );
    assert!(
        o.max_abs_av_ms <= GATE_MAX_AV_MS,
        "|A/V| {:.2} ms at t={:.1} s exceeds {GATE_MAX_AV_MS} ms",
        o.max_abs_av_ms,
        o.worst_at_s
    );
    // issue 1367 (ROZHODNUTÉ 5827497952): no hold change ever STEPS the playing audio. The
    // placements are the start, the sender-restart resync and the OBS restart (each after the
    // withhold, straight onto the measured delay); every depth change is a slew.
    assert_eq!(o.steps, 0, "a step re-placement while playing");
    // the booked slew never walks the smoothing timeline (the 100 ms sender-restart slew included).
    assert_eq!(o.smoothing_snaps, 0, "a slew snapped back at 70 ms");
    assert!(
        o.max_ts_div_ns < 1_000,
        "booked steps keep the smoothing timeline on the source time: {} ns",
        o.max_ts_div_ns
    );
    assert_eq!(o.places, 3, "places = {}", o.places);
    assert!(o.slews <= 6, "slews = {} (churn)", o.slews);
    // a slew moves 1 ms per second: the scripted changes (33 / 67 / 100 / 33 ms) settle within
    // their own size in seconds.
    let slewing_s = o.slewing_ticks as f64 / FPS as f64;
    assert!(
        slewing_s <= 33.4 + 66.7 + 100.1 + 100.1 + 33.4 + 5.0,
        "slewing {slewing_s:.1} s"
    );
}

#[test]
fn an_unbooked_slew_walks_the_smoothing_timeline_and_snaps_back_1367() {
    // the anti-tautology twin of the booking: the scripted run carries a 100 ms slew (the sender
    // restart re-times the free tracker from 4 frames to 1), which walks an unbooked smoothing
    // timeline past TS_SMOOTHING_THRESHOLD, and the audio snaps back to the old placement.
    let o = simulate(&Bench::scripted(), Variant::NoBooking);
    eprintln!("no booking: {o:?}");
    assert!(
        o.smoothing_snaps > 0 && o.max_ts_div_ns >= TS_SMOOTHING_THRESHOLD_NS,
        "an unbooked 100 ms slew must reach the 70 ms smoothing threshold: {o:?}"
    );
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
        (o.places, o.slews, o.steps),
        (base.places, base.slews, base.steps),
        "single held ticks must not re-time the audio"
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

#[test]
fn a_playing_hold_change_slews_instead_of_stepping_1367() {
    // a playing source whose delay moves 100 -> 67 ms is slewed, not re-placed.
    let mut leg = AudioLeg::fresh(false);
    let tc = W0;
    leg.ingest(tc, MONO0, W0, 100, false);
    assert_eq!((leg.places, leg.slews, leg.steps), (1, 0, 0));
    leg.ingest(tc + IV_NS, MONO0 + IV_NS, W0 + IV_NS, 67, false);
    assert_eq!(
        (leg.places, leg.slews, leg.steps),
        (1, 1, 0),
        "a playing change slews"
    );
    assert!(leg.slewing());
}

#[cfg(test)]
#[path = "genlock_shallow_av_bench.rs"]
mod shallow;
