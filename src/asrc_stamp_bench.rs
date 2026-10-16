//! Issue 1425 (E2 slice 1, design 6205974120) — the two-clock bench of the STAMP basis of the
//! per-source ASRC: a directly stamped, non-genlock, mixed source (the stream `mbc` ASIO input, a
//! PulseAudio program input, a media clip) is judged by where each packet lands against its OWN
//! stamp, setpoint 0, never by its buffer depth against the issue-1355 absolute 100 ms target.
//!
//! ## Why
//!
//! The arrival servo holds the mix-buffer DEPTH. That depth carries the launch-random global audio
//! buffering B and, for a source whose packet is one mixer tick long (PulseAudio stamps every
//! 1024-frame packet "now minus the packet"), the launch-random packet-vs-tick phase. So the A/V
//! displacement of such a source changed with every OBS launch. Judged by its stamp, B and the phase
//! drop out: the source's A/V is its driver's constant stamp bias.
//!
//! ## Plant
//!
//! - **Clocks.** OBS's monotonic clock (the mixer) and the source's sample clock, `ppm` apart
//!   (+ = the source runs fast), plus an optional slow sinusoidal wander (`wander_ppm` peak, two-hour
//!   period) for the 8 h case.
//! - **Source.** `mbc`: 128-sample packets (2.67 ms) with up to 2 ms of bursty delivery; `pulse`:
//!   1024-frame packets (one mixer tick) with up to 1 ms. A packet is complete when its last sample
//!   was captured; it ARRIVES up to the jitter later (an LCG, never out of order); the driver stamps
//!   it "arrival minus the NOMINAL packet duration". So each stamp carries the packet's delivery
//!   jitter, the servo input the design names as the risk.
//! - **Launch phase.** The source's packet grid starts `phase_ns` into the mixer's tick grid.
//! - **OBS ingest.** `source_output_audio_data` for a directly stamped source, step for step: the
//!   raw-domain 70 ms TS smoothing snap, the system-domain push-back check, the hold mode `Off`
//!   (no term), the ASRC basis ([`audio_asrc_placement`]) and the timecode-mode flag the compensator
//!   gets for a non-arrival basis, the beyond-cap backstop, append vs place (OBS `reset_audio_data`
//!   on a placement before the buffer start), and the production measurement
//!   ([`audio_place_measured`] / [`audio_actual_place_ns`] against [`audio_intended_raw_ns`]) feeding
//!   `asrc_timecode_ingest` (booking + the stamp advance as the master block). `asrc_process_audio`
//!   runs first on the RAW packet: the arrival servo against the mixer clock and the buffer depth
//!   (absolute target, offset 0), or only reading the servo for a placement-judged source. The
//!   resampler output of each packet is `frames·(1 − (applied + recover)/1e6)` with the fractional
//!   part carried.
//! - **Mixer.** 1024-sample ticks. Each tick renders the window ending `lag` behind it and moves the
//!   source's buffer start there. `lag` = the source's own lateness (one packet + its jitter + 1 ms)
//!   plus B mixer ticks: B counts the buffering ABOVE what the source needs to be on time (below
//!   that, OBS's own dynamic increase raises the buffering; a window that overtakes the buffer end
//!   is counted as an underrun and must not happen).
//!
//! A/V of a packet = where its first sample lands in the mix timeline minus where its stamp says it
//! belongs once the jitter is taken out: the packet's true capture instant + the driver's constant
//! stamp bias (`completion − nominal duration + jitter/2`). Averaged per second (a viewer and the
//! E2E A/V gate never see the per-packet delivery jitter).
//!
//! ## Variants
//!
//! - `Production`: the basis the ingest decides ([`audio_asrc_placement`]): `Stamp` here.
//! - `Arrival` (anti-tautology): today's arrival servo on the same feed (the absolute 100 ms target).
//! - `Unservoed` (anti-tautology): no ASRC at all; the A/V walks with the source rate.
//!
//! ## Acceptance (design 6205974120, consult section 5)
//!
//! Production, from the steady state (600 s) on: |A/V| ≤ 1 ms at B ∈ {0, 1, 2, 4, 6} ticks, every
//! launch phase and −20 / 0 / +20 ppm, for 128- and 1024-sample packets; the spread across B and the
//! launch phase ≤ 1 ms; over 8 h with a ±1 ppm wander the A/V moves < 2 ms; no placement jump is ever
//! booked (no phantom jump from the 1–2 ms delivery jitter on 128-sample packets) and nothing
//! underruns. `Arrival` must spread A/V across B and the phase; `Unservoed` must drift with the rate.
//!
//! **A delivery STALL, a real loss and equal-stamp bursts are the rows of `asrc_stamp_bench_stall.rs`**
//! (round 2, design 6206822792). The jitter here is a fifth of the compensator's 10 ms jump band, so
//! the "0 jumps" assertions above are about jitter only. A driver that stamps at arrival (PulseAudio,
//! WASAPI without device timing) puts a stall into its stamps as a transient of about −S: the stamp
//! basis books a placement jump only once it persists (one second of audio and 8 packets), and a
//! packet with no stamp advance carries its samples into the rate point still waiting
//! (`audio_asrc_rate_point`). Before round 2 a 20 ms `mbc` stall moved the A/V +6.5 ms for ~800 s.
//!
//! ## Measured (at landing)
//!
//! - Production, +20 ppm, the 20 launches per source: max |A/V| 0.59 ms (`mbc`) / 0.54 ms (pulse),
//!   spread of the steady means 0.34 / 0.19 ms, 0 jumps, 0 underruns, one placement (the first).
//! - Production at −20 / 0 / +20 ppm: max |A/V| ≤ 0.72 ms.
//! - Production over 8 h, +20 ppm ± 1 ppm wander: minute A/V moved 0.53 ms (`mbc`) / 0.60 ms (pulse),
//!   max |A/V| 0.43 ms, 0 jumps, 0 fallbacks.
//! - `Arrival`, same feed: steady A/V +33.7 / +58.4 / +38.7 / +0.9 / −36.8 ms (`mbc`) at
//!   B = 0 / 1 / 2 / 4 / 6 ticks; for pulse the launch phase alone moves it 2.1 … 15 ms at one B.
//!   The absolute 100 ms target only fits B = 4 extra ticks of this plant, the floor's own design.
//! - `Unservoed` at ±20 ppm: ±12 ms more A/V over the 600 s steady window.
//!
//! ## Measured (round 2, design 6206822792; the rows above unchanged to the printed digit)
//!
//! - Stalls of 8 / 12 / 15 / 20 / 30 / 60 ms at 700 s: 0 jumps; |A/V − pre-stall mean| ≤ 0.042 ms
//!   (`mbc`) / 0.211 ms (pulse) from the stall second on.
//! - The 120 ms stall: OBS's own placement past its 70 ms smoothing leaves the audio +44.8 ms
//!   (`mbc`) / +31.5 ms (pulse) late on every basis (the arrival servo +44.5 / +33.5 ms, never back
//!   within 1 ms in the run). The stamp basis books it once at 701.09 s and is back within 1 ms at
//!   745 s / 732 s; the burst's compressed stamps then hold the A/V within 1.03 / 0.52 ms while they
//!   stay in the 600 s rate regression.
//! - A real 44 ms loss: booked once at 701.05 s (`mbc`) / 701.03 s (pulse), back within 1 ms at
//!   743 s (the arrival servo: 742 s / not back in the run).
//! - Equal-stamp bursts, 8 h: drift 0.13 ms (`mbc`) / 0.24 ms (pulse) from 1800 s on, 0 jumps. The
//!   A/V settles p·d late (0.66 ms `mbc` / 5.5 ms pulse at p = 0.25): only the early-stamped first
//!   packet of each pair reaches the level loop (the second has no rate point of its own).

use crate::asrc_bench::RealtimeAsrcCompensator;
use crate::genlock_audio_pairing::{
    audio_actual_place_ns, audio_asrc_error_ms, audio_asrc_placement, audio_asrc_rate_point,
    audio_hold_action, audio_intended_raw_ns, audio_place_error_ns, audio_place_measured,
    audio_place_term_ns, audio_stamp_interval_s, audio_stamp_mono_ns, AsrcBasis, AsrcRatePending,
    AudioHoldAction, AudioHoldMode,
};

const RATE: u64 = 48_000;
const TICK_FRAMES: u64 = 1024;
const NS_PER_S: u64 = 1_000_000_000;
const MONO0: u64 = 50_000_000_000_000;
const MAX_TS_VAR: u64 = 2_000_000_000;
const TS_SMOOTHING_THRESHOLD: u64 = 70_000_000;
/// The steady state is checked from here (the regression locks after a 60 s span; the level loop
/// then pulls what drifted before the lock back with its ~500 s time constant).
const STEADY_FROM_S: u64 = 600;
const RUN_S: u64 = 1200;
const EIGHT_HOURS_S: u64 = 8 * 3600;
const WANDER_PERIOD_S: f64 = 7200.0;
const AV_BOUND_MS: f64 = 1.0;
const SPREAD_BOUND_MS: f64 = 1.0;
const DRIFT_BOUND_MS: f64 = 2.0;
const BUFFERINGS: [u64; 5] = [0, 1, 2, 4, 6];
const PHASES_QUARTER_TICKS: [u64; 4] = [0, 1, 2, 3];
const RATES_PPM: [f64; 3] = [-20.0, 0.0, 20.0];
/// Issue 1425 round 2: the chance a draw of [`Event::EqualStampBursts`] is a two-packet read.
const BURST_PAIR_P: f64 = 0.25;

fn frames_ns(frames: u64) -> u64 {
    frames * NS_PER_S / RATE
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Source {
    name: &'static str,
    packet_frames: u64,
    jitter_ns: u64,
}

/// The stream `mbc` (ASIO over Dante): 128-sample blocks, bursty delivery up to 2 ms.
const MBC: Source = Source {
    name: "mbc",
    packet_frames: 128,
    jitter_ns: 2_000_000,
};

/// A PulseAudio input (strih-snv `ASIO zvuk`, stream-pp `Zvuk z mixu`): 1024-frame packets, one mixer
/// tick each, 1 ms of jitter.
const PULSE: Source = Source {
    name: "pulse",
    packet_frames: 1024,
    jitter_ns: 1_000_000,
};

const SOURCES: [Source; 2] = [MBC, PULSE];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Servo {
    Production,
    Arrival,
    Unservoed,
}

/// Issue 1425 round 2 (design 6206822792): a disturbance of the delivery, the acceptance rows of
/// `asrc_stamp_bench_stall.rs`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    None,
    /// Delivery stops at `at_s` for `ms`: every packet completed in that span arrives at its end,
    /// in one burst (the driver stamps each "arrival minus the packet").
    Stall {
        at_s: f64,
        ms: f64,
    },
    /// A real sample loss: the packets completed in `ms` from `at_s` are never delivered.
    Loss {
        at_s: f64,
        ms: f64,
    },
    /// From the start, each draw delivers two packets in one read with probability
    /// [`BURST_PAIR_P`] (one arrival, so equal stamps), else one.
    EqualStampBursts,
}

#[derive(Debug, Clone, Copy)]
struct Case {
    src: Source,
    ppm: f64,
    wander_ppm: f64,
    buffering_ticks: u64,
    phase_ns: u64,
    run_s: u64,
    servo: Servo,
    event: Event,
}

impl Case {
    fn new(src: Source, ppm: f64, buffering_ticks: u64, phase_quarter: u64, servo: Servo) -> Self {
        Case {
            src,
            ppm,
            wander_ppm: 0.0,
            buffering_ticks,
            phase_ns: frames_ns(TICK_FRAMES) * phase_quarter / 4,
            run_s: RUN_S,
            servo,
            event: Event::None,
        }
    }
}

/// The same deterministic LCG as the other ASRC benches.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / 9_007_199_254_740_992.0
    }
}

/// One OBS source: `asrc_process_audio` + `source_output_audio_data` + the mixer.
struct Obs {
    servo: Servo,
    c: RealtimeAsrcCompensator,
    packet_frames: u64,
    lag_ns: u64,
    has_last: bool,
    last_mono: u64,
    ppm_res: f64,
    frac: f64,
    raw_cur_s: f64,
    next_ts_min: u64,
    next_sys_min: u64,
    audio_ts: u64,
    end: u64,
    tick: u64,
    have_prev: bool,
    prev_stamp_mono: u64,
    prev_raw_s: f64,
    pending: AsrcRatePending,
    underruns: u32,
    backstops: u32,
    placements: u32,
    basis_seen: Option<AsrcBasis>,
}

impl Obs {
    fn new(case: &Case) -> Self {
        let packet_ns = frames_ns(case.src.packet_frames);
        Obs {
            servo: case.servo,
            c: RealtimeAsrcCompensator::new(),
            packet_frames: case.src.packet_frames,
            lag_ns: packet_ns
                + case.src.jitter_ns
                + 1_000_000
                + frames_ns(case.buffering_ticks * TICK_FRAMES),
            has_last: false,
            last_mono: 0,
            ppm_res: 0.0,
            frac: 0.0,
            raw_cur_s: 0.0,
            next_ts_min: 0,
            next_sys_min: 0,
            audio_ts: 0,
            end: 0,
            tick: 0,
            have_prev: false,
            prev_stamp_mono: 0,
            prev_raw_s: 0.0,
            pending: AsrcRatePending::default(),
            underruns: 0,
            backstops: 0,
            placements: 0,
            basis_seen: None,
        }
    }

    /// Render every mixer tick due by `mono_now`.
    fn mix_until(&mut self, mono_now: u64) {
        loop {
            let tick_at = MONO0 + frames_ns((self.tick + 1) * TICK_FRAMES);
            if tick_at > mono_now {
                return;
            }
            self.tick += 1;
            if self.audio_ts != 0 {
                let window_end = tick_at.saturating_sub(self.lag_ns);
                if window_end > self.end {
                    self.underruns += 1;
                }
                self.audio_ts = self.audio_ts.max(window_end);
                self.end = self.end.max(self.audio_ts);
            }
        }
    }

    fn buffered_ns(&self) -> u64 {
        if self.audio_ts == 0 {
            0
        } else {
            self.end.saturating_sub(self.audio_ts)
        }
    }

    /// `asrc_process_audio`: returns this packet's resampled duration.
    fn asrc_process(&mut self, mono_now: u64) -> u64 {
        let raw_s = self.packet_frames as f64 / RATE as f64;
        self.raw_cur_s = raw_s;
        if self.servo == Servo::Unservoed {
            self.ppm_res = 0.0;
        } else if !self.has_last {
            self.has_last = true;
            self.last_mono = mono_now;
        } else {
            let master_s = (mono_now - self.last_mono) as f64 / 1e9;
            self.last_mono = mono_now;
            let buffered_ms = self.buffered_ns() as f64 / 1e6;
            // a mixed, non-genlock source: the issue-1355 absolute capture rule, sync offset 0
            self.c.set_level_absolute(true);
            self.c.set_level_offset_ms(0.0);
            self.c.set_step_recover_hold(false);
            let (applied, recover) = if self.c.timecode() {
                (self.c.applied_ppm(), self.c.take_step_recover_ppm())
            } else {
                self.c.compensate_with_level(raw_s, master_s, buffered_ms);
                (self.c.applied_ppm(), self.c.step_recover_ppm())
            };
            self.ppm_res = -applied - recover;
        }
        let frames_f = self.packet_frames as f64 * (1.0 + self.ppm_res / 1e6) + self.frac;
        let frames = frames_f.floor();
        self.frac = frames_f - frames;
        frames_ns(frames as u64)
    }

    /// `source_output_audio_data` for one packet; returns where its first sample landed.
    fn ingest(&mut self, stamp: u64, mono_now: u64, dur: u64) -> u64 {
        let mut in_ts = stamp;
        // detects 'directly' set timestamps: timing_adjust = 0
        let direct_ts = stamp.abs_diff(mono_now) < MAX_TS_VAR;
        assert!(
            direct_ts,
            "issue 1425 bench: the source stamps on the mixer clock"
        );
        let timing_adjust: u64 = 0;
        if self.next_ts_min != 0 && self.next_ts_min.abs_diff(in_ts) < TS_SMOOTHING_THRESHOLD {
            in_ts = self.next_ts_min;
        }
        self.next_ts_min = in_ts + dur;
        // the system-domain check: an equal or a < 70 ms timestamp appends
        let mut push_back = self.next_sys_min == in_ts
            || (self.next_sys_min != 0
                && self.next_sys_min.abs_diff(in_ts) < TS_SMOOTHING_THRESHOLD);
        let mode = AudioHoldMode::Off;
        let term = audio_place_term_ns(mode, 0, 0, timing_adjust);
        let basis = match self.servo {
            Servo::Production => {
                audio_asrc_placement(mode, direct_ts && timing_adjust == 0, false, true)
            }
            Servo::Arrival | Servo::Unservoed => AsrcBasis::Arrival,
        };
        self.basis_seen = Some(basis);
        let tc = basis != AsrcBasis::Arrival;
        if self.c.timecode() != tc {
            self.have_prev = false;
        }
        self.c.set_basis(basis);
        let action = audio_hold_action(mode, 0, mode, 0, push_back, true, false);
        assert_eq!(
            action,
            AudioHoldAction::Continue,
            "issue 1425 bench: a non-genlock source never takes a genlock hold action"
        );
        self.next_sys_min = self.next_ts_min.wrapping_add(timing_adjust);
        let intended = audio_intended_raw_ns(stamp, timing_adjust, 0, 0, term);
        if tc && push_back && self.audio_ts != 0 {
            let would = audio_actual_place_ns(true, self.audio_ts, self.buffered_ns(), in_ts);
            let err_ms = audio_asrc_error_ms(audio_place_error_ns(would, intended), 0);
            if self.c.place_beyond_cap(err_ms, self.raw_cur_s * 1000.0) {
                push_back = false;
                in_ts = intended;
                self.backstops += 1;
            }
        }
        let appended = push_back && self.audio_ts != 0;
        let actual = audio_actual_place_ns(appended, self.audio_ts, self.buffered_ns(), in_ts);
        if appended {
            self.end = actual + dur;
        } else {
            self.placements += 1;
            if self.audio_ts == 0 || in_ts < self.audio_ts {
                self.audio_ts = in_ts;
                self.next_sys_min = in_ts;
            }
            self.end = in_ts + dur;
        }
        if tc && audio_place_measured(mode, basis) {
            // asrc_timecode_ingest: the placement error books jumps, the stamp advance is the master
            let err_ms = audio_asrc_error_ms(audio_place_error_ns(actual, intended), 0);
            self.c
                .observe_placement(err_ms, self.raw_cur_s * 1000.0, !appended);
            let stamp_mono = audio_stamp_mono_ns(stamp, 0);
            let contiguous = appended && self.have_prev;
            let master_s = if contiguous {
                audio_stamp_interval_s(self.prev_stamp_mono, stamp_mono)
            } else {
                0.0
            };
            // issue 1425 round 2: on the stamp basis a packet with no advance adds its
            // predecessor's samples to the rate point still waiting
            let point = audio_asrc_rate_point(
                basis,
                contiguous,
                master_s,
                self.prev_raw_s,
                &mut self.pending,
            );
            if point.due {
                self.c
                    .compensate_with_level(point.raw_s, point.master_s, err_ms);
            }
            if !point.keep_prev_stamp {
                self.prev_stamp_mono = stamp_mono;
            }
            self.prev_raw_s = self.raw_cur_s;
            self.have_prev = true;
        }
        actual
    }
}

#[derive(Debug, Default)]
struct Run {
    /// Per-second mean A/V (ms), one entry per second of arrival time.
    av_s: Vec<f64>,
    place_jumps: u32,
    underruns: u32,
    backstops: u32,
    placements: u32,
    fallbacks: u32,
    basis: Option<AsrcBasis>,
    /// Issue 1425 round 2: arrival time (s) of the packet that made the first booked jump.
    first_jump_s: Option<f64>,
}

impl Run {
    fn steady(&self) -> &[f64] {
        &self.av_s[STEADY_FROM_S as usize..]
    }
    fn steady_mean_ms(&self) -> f64 {
        let s = self.steady();
        s.iter().sum::<f64>() / s.len() as f64
    }
    fn steady_max_abs_ms(&self) -> f64 {
        self.steady().iter().fold(0.0_f64, |m, v| m.max(v.abs()))
    }
    /// The largest move of the minute-averaged A/V away from the first steady minute.
    fn steady_drift_ms(&self) -> f64 {
        let minutes: Vec<f64> = self
            .steady()
            .chunks(60)
            .filter(|c| c.len() == 60)
            .map(|c| c.iter().sum::<f64>() / 60.0)
            .collect();
        minutes
            .iter()
            .fold(0.0_f64, |m, v| m.max((v - minutes[0]).abs()))
    }
}

fn run(case: &Case) -> Run {
    let mut obs = Obs::new(case);
    let mut rng = Lcg(0x1425_6205 ^ case.phase_ns ^ (case.buffering_ticks << 40));
    let packet_ns = frames_ns(case.src.packet_frames) as f64;
    let half_jitter = case.src.jitter_ns as f64 / 2.0;
    let mut start = (MONO0 + case.phase_ns) as f64;
    let mut prev_arrival = 0_u64;
    // Event::EqualStampBursts: packets left in the current burst, and its one arrival
    let (mut burst_left, mut burst_arrival) = (0_u32, 0.0_f64);
    let (mut sum, mut n, mut second) = (0.0_f64, 0_u32, 0_u64);
    let mut r = Run::default();
    let end_ns = (MONO0 + case.run_s * NS_PER_S) as f64;
    while start < end_ns {
        let t_s = (start - MONO0 as f64) / 1e9;
        let ppm =
            case.ppm + case.wander_ppm * (2.0 * std::f64::consts::PI * t_s / WANDER_PERIOD_S).sin();
        // the source clock runs `ppm` fast: a nominal packet takes less mixer time
        let complete = start + packet_ns / (1.0 + ppm * 1e-6);
        let complete_s = (complete - MONO0 as f64) / 1e9;
        let mut arrival_f = complete + case.src.jitter_ns as f64 * rng.next();
        match case.event {
            Event::None => {}
            Event::Stall { at_s, ms } => {
                if complete_s >= at_s && complete_s < at_s + ms / 1e3 {
                    arrival_f = arrival_f.max(MONO0 as f64 + (at_s + ms / 1e3) * 1e9);
                }
            }
            Event::Loss { at_s, ms } => {
                if complete_s >= at_s && complete_s < at_s + ms / 1e3 {
                    start = complete;
                    continue;
                }
            }
            Event::EqualStampBursts => {
                if burst_left == 0 {
                    burst_left = if rng.next() < BURST_PAIR_P { 2 } else { 1 };
                    let last = start + f64::from(burst_left) * packet_ns / (1.0 + ppm * 1e-6);
                    burst_arrival = last + case.src.jitter_ns as f64 * rng.next();
                }
                burst_left -= 1;
                arrival_f = burst_arrival;
            }
        }
        let arrival = (arrival_f as u64).max(prev_arrival);
        prev_arrival = arrival;
        let stamp = arrival - frames_ns(case.src.packet_frames);
        // the stamp without its jitter: the true capture instant + the driver's constant bias
        let truth = complete - packet_ns + half_jitter;
        obs.mix_until(arrival);
        let dur = obs.asrc_process(arrival);
        let actual = obs.ingest(stamp, arrival, dur);
        if r.first_jump_s.is_none() && obs.c.place_jump_count() > 0 {
            r.first_jump_s = Some((arrival - MONO0) as f64 / 1e9);
        }
        let av_ms = (actual as f64 - truth) / 1e6;
        let sec = (arrival - MONO0) / NS_PER_S;
        if sec != second {
            r.av_s.push(if n == 0 { 0.0 } else { sum / f64::from(n) });
            (sum, n, second) = (0.0, 0, sec);
        }
        sum += av_ms;
        n += 1;
        start = complete;
    }
    r.place_jumps = obs.c.place_jump_count();
    r.underruns = obs.underruns;
    r.backstops = obs.backstops;
    r.placements = obs.placements;
    r.fallbacks = obs.c.level_fallback_count();
    r.basis = obs.basis_seen;
    r
}

/// Issue 1425 round 2: the stall / loss / equal-stamp acceptance rows, in their own file so this one
/// does not grow past its budget.
#[path = "asrc_stamp_bench_stall.rs"]
mod stall;

/// Every buffering x launch phase at +20 ppm (the live mbc residual), for one source.
fn sweep(src: Source, servo: Servo) -> Vec<(u64, u64, Run)> {
    let mut out = Vec::new();
    for &b in &BUFFERINGS {
        for &p in &PHASES_QUARTER_TICKS {
            out.push((b, p, run(&Case::new(src, 20.0, b, p, servo))));
        }
    }
    out
}

fn spread_ms(runs: &[(u64, u64, Run)]) -> f64 {
    let means: Vec<f64> = runs.iter().map(|(_, _, r)| r.steady_mean_ms()).collect();
    let lo = means.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = means.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    hi - lo
}

#[test]
fn a_stamped_source_holds_av_at_every_buffering_and_launch_phase_1425() {
    for src in SOURCES {
        let runs = sweep(src, Servo::Production);
        for (b, p, r) in &runs {
            assert_eq!(
                r.basis,
                Some(AsrcBasis::Stamp),
                "issue 1425: {} is a directly stamped mixed source: the ASRC must judge it by its \
                 stamp",
                src.name
            );
            assert!(
                r.steady_max_abs_ms() <= AV_BOUND_MS,
                "issue 1425: {} B={b} ticks phase={p}/4: |A/V| must stay <= {AV_BOUND_MS} ms from \
                 {STEADY_FROM_S} s on, max {:.3} ms (mean {:.3})",
                src.name,
                r.steady_max_abs_ms(),
                r.steady_mean_ms()
            );
            assert!(
                r.place_jumps == 0 && r.backstops == 0 && r.underruns == 0 && r.placements == 1,
                "issue 1425: {} B={b} phase={p}/4: the delivery jitter must book no phantom jump, \
                 nothing may underrun, and only the first packet is placed: jumps {} backstops {} \
                 underruns {} placements {}",
                src.name,
                r.place_jumps,
                r.backstops,
                r.underruns,
                r.placements
            );
        }
        let spread = spread_ms(&runs);
        assert!(
            spread <= SPREAD_BOUND_MS,
            "issue 1425: {}: the A/V must not depend on the buffering or the launch phase (the \
             restart proof): spread {spread:.3} ms over {} launches",
            src.name,
            runs.len()
        );
    }
}

#[test]
fn a_stamped_source_holds_av_at_every_source_rate_1425() {
    for src in SOURCES {
        for ppm in RATES_PPM {
            let r = run(&Case::new(src, ppm, 4, 1, Servo::Production));
            assert!(
                r.steady_max_abs_ms() <= AV_BOUND_MS && r.place_jumps == 0 && r.fallbacks == 0,
                "issue 1425: {} at {ppm:+} ppm: |A/V| <= {AV_BOUND_MS} ms, no booked jump, no \
                 fallback: max {:.3} ms, jumps {}, fallbacks {}",
                src.name,
                r.steady_max_abs_ms(),
                r.place_jumps,
                r.fallbacks
            );
        }
    }
}

#[test]
fn a_stamped_source_does_not_drift_over_eight_hours_1425() {
    for src in SOURCES {
        let mut case = Case::new(src, 20.0, 4, 2, Servo::Production);
        case.wander_ppm = 1.0;
        case.run_s = EIGHT_HOURS_S;
        let r = run(&case);
        assert!(
            r.steady_drift_ms() < DRIFT_BOUND_MS && r.steady_max_abs_ms() <= AV_BOUND_MS,
            "issue 1425: {} over 8 h at +20 ppm with a +-1 ppm wander: the minute A/V must move < \
             {DRIFT_BOUND_MS} ms and stay within {AV_BOUND_MS} ms: drift {:.3} ms, max {:.3} ms",
            src.name,
            r.steady_drift_ms(),
            r.steady_max_abs_ms()
        );
        assert!(
            r.place_jumps == 0 && r.underruns == 0 && r.fallbacks == 0,
            "issue 1425: {} over 8 h: no booked jump, no underrun, no fallback: {} / {} / {}",
            src.name,
            r.place_jumps,
            r.underruns,
            r.fallbacks
        );
    }
}

#[test]
fn the_arrival_servo_moves_av_with_the_buffering_and_the_phase_1425() {
    // anti-tautology: today's arrival servo on the same feed. Its depth target is absolute, so the
    // A/V follows the buffering (and, for a tick-long packet, the launch phase): the bench would pass
    // vacuously if this did not fail the production bounds.
    for src in SOURCES {
        let runs = sweep(src, Servo::Arrival);
        let spread = spread_ms(&runs);
        assert!(
            spread > 10.0 * SPREAD_BOUND_MS,
            "issue 1425: {}: the arrival servo must spread the A/V over the buffering, else the \
             bench proves nothing: spread {spread:.3} ms",
            src.name
        );
    }
    // the tick-long packet: at EVERY buffering the launch phase alone moves the arrival servo's A/V
    // (the per-callback depth reading is taken at one packet-vs-tick phase per launch; consult F2)
    let pulse = sweep(PULSE, Servo::Arrival);
    for b in BUFFERINGS {
        let at_b: Vec<_> = pulse.iter().filter(|(rb, _, _)| *rb == b).collect();
        let means: Vec<f64> = at_b.iter().map(|(_, _, r)| r.steady_mean_ms()).collect();
        let lo = means.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = means.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            hi - lo > SPREAD_BOUND_MS,
            "issue 1425: at B={b} ticks the arrival servo must carry the PulseAudio packet-vs-tick \
             phase into the A/V: spread over the phase {:.3} ms",
            hi - lo
        );
    }
}

#[test]
fn without_a_servo_the_av_walks_with_the_source_rate_1425() {
    // anti-tautology: no ASRC. +20 ppm = the samples land 20 us/s later than their stamps, -20 earlier.
    for (ppm, sign) in [(20.0, 1.0), (-20.0, -1.0)] {
        let r = run(&Case::new(MBC, ppm, 4, 0, Servo::Unservoed));
        let last = *r.av_s.last().expect("a run");
        let first = r.av_s[STEADY_FROM_S as usize];
        assert!(
            (last - first) * sign > DRIFT_BOUND_MS,
            "issue 1425: with no servo a {ppm:+} ppm source must drift by more than \
             {DRIFT_BOUND_MS} ms in {} s: {first:.3} -> {last:.3} ms",
            RUN_S - STEADY_FROM_S
        );
    }
}
