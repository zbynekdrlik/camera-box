//! Issue 1367 (design 5845361166) — the two-clock bench for the TIMECODE mode of the per-source
//! ASRC: a genlock source whose audio is placed at its own NDI timecode (`audio_hold=timecode`, the
//! resolume `sp-*` inputs from SongPlayer) is judged by where each packet lands against its stamp,
//! never by when it arrived.
//!
//! ## Plant
//!
//! - **Clocks.** OBS's monotonic clock (the audio mixer) and the fleet wall clock tick together
//!   (dantesync 1.9.0 + the disciplined media clock), so a CORRECT sender needs 0 ppm. A date step
//!   moves both walls (sender and receiver, 3 ms apart) and never the monotonic clock.
//! - **Sender.** 1600-sample packets at 48 kHz (one 30 fps slot, 33.3 ms), stamped with its wall
//!   clock at emit (emit up to 1 ms late), floored to the NDI 100 ns unit. Each packet arrives 2–5 ms
//!   later (the DistroAV receive thread), never out of order.
//! - **OBS ingest** replays `source_output_audio_data` step for step: the raw-domain 70 ms TS
//!   smoothing snap and the > 2 s `handle_ts_jump` reset, the system-domain push-back check, the
//!   genlock timecode term through the live wall→mono offset ([`audio_place_term_ns`]), the
//!   append-after-reset guard ([`audio_push_back_allowed`]), append vs place (a placement before the
//!   buffer start resets it, the OBS `reset_audio_data`), and the production measurement
//!   ([`audio_actual_place_ns`] against [`audio_intended_raw_ns`]). `asrc_process_audio` runs first
//!   on the RAW packet (arrival master, the buffer depth), and the resampler output of each packet is
//!   `1600·(1 − (applied + recover)/1e6)` samples with the fractional part carried.
//! - **Mixer.** 1024-sample ticks, 64 ms behind real time; each tick moves the source's `audio_ts`
//!   to the end of the rendered window (a buffer start in the future waits).
//!
//! A/V of a packet = where its first sample landed minus where its TRUE slot belongs (slot + hold,
//! plus the sender's own timeline move in the stamp-leap scenario), on the monotonic clock.
//!
//! ## Variants
//!
//! - `Production`: the timecode path — the ingest feeds the servo the placement error, books jumps
//!   ([`RealtimeAsrcCompensator::observe_placement`]) and gives it the stamp advance between appended
//!   packets; `asrc_process_audio` only reads the servo.
//! - `Legacy` (anti-tautology): today's arrival servo on the same feed.
//! - `NoBooking` (anti-tautology): the timecode path without the jump booking (the level loop alone).
//!
//! ## Acceptance (design 5845361166)
//!
//! Production: `|A/V| <= 2 ms` from 60 s after each event on and before it, the rate estimate within
//! ±5 ppm on the correct sender throughout. The one exception is arithmetic: two skipped slots owe
//! 66.7 ms, which the 1000 ppm budget (1 ms per second) pays in 66.7 s, so that case is held from the
//! event + 66.7 s + 5 s (flagged on the ticket, comment 5845468362). Legacy must fail the catch-up
//! and the stamp leap, NoBooking the skipped and the duplicated slot.

use crate::asrc_bench::{RealtimeAsrcCompensator, STEP_RECOVER_PPM};
use crate::genlock_audio_pairing::{
    audio_actual_place_ns, audio_asrc_error_ms, audio_asrc_timecode, audio_hold_action,
    audio_intended_raw_ns, audio_level_shift_ns, audio_place_error_ns, audio_place_term_ns,
    audio_placed_slew_fold_ns, audio_push_back_allowed, audio_slew_book_ts_ns, audio_slew_ppm,
    audio_slew_step_ns, audio_stamp_interval_s, audio_stamp_mono_ns, audio_wall_to_mono_ns,
    genlock_audio_delay_ns, AudioHoldAction, AudioHoldMode,
};

const RATE: u64 = 48_000;
const PACKET_FRAMES: u64 = 1600;
const TICK_FRAMES: u64 = 1024;
const MONO0: u64 = 50_000_000_000_000;
const WALL0: u64 = 1_790_000_000_000_000_000;
const HOLD_MS: u32 = 100;
/// The latched depth after a one-frame relock (the #1367 hold slew scenarios).
const HOLD_SLEWED_MS: u32 = 133;
/// SkipThenSlew: the relock comes this long after the skipped slot, while the payment still owes.
const SLEW_AFTER_SKIP_NS: u64 = 5 * NS_PER_S;
const BUFFERING_NS: u64 = 64_000_000;
const MAX_TS_VAR: u64 = 2_000_000_000;
const TS_SMOOTHING_THRESHOLD: u64 = 70_000_000;
const NS_PER_S: u64 = 1_000_000_000;
const EVENT_AT_NS: u64 = 400 * NS_PER_S;
const RUN_NS: u64 = 800 * NS_PER_S;
/// Checked from here: the servo has locked (≥ 60 s span) and captured.
const STEADY_FROM_NS: u64 = 150 * NS_PER_S;
const SENDER_STEP_LAG_NS: u64 = 3_000_000;
const WALL_STEP_NS: u64 = 50_000_000;
const RESTART_SILENT_NS: u64 = 3 * NS_PER_S;
const CATCHUP_LAG_NS: f64 = 60_000_000.0;
const CATCHUP_NS: f64 = 200.0 * 1e9;
/// The sender's own timeline move in the stamp-leap scenario (a slot skipped whose stamp jumped
/// 80 ms: 47 ms past the missing slot, the live 11:38 excess shape).
const STAMP_LEAP_NS: u64 = 47_000_000;
const AV_BOUND_MS: f64 = 2.0;
const RATE_BOUND_PPM: f64 = 5.0;

fn frames_ns(frames: u64) -> u64 {
    frames * NS_PER_S / RATE
}

fn slot_ns(k: u64) -> u64 {
    k * PACKET_FRAMES * NS_PER_S / RATE
}

fn abs_diff(a: u64, b: u64) -> u64 {
    a.abs_diff(b)
}

/// The same deterministic LCG as the ASRC parity driver.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / 9_007_199_254_740_992.0
    }

    fn ns(&mut self, lo: u64, span: u64) -> u64 {
        lo + (span as f64 * self.next()) as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    /// `n` consecutive sender slots are never sent (their samples are lost).
    Skip(u64),
    /// The sender repeats the slot at the event (same stamp, same samples).
    Dup,
    /// The sender stops for 3 s, restarts on its real-time grid, and its packets then arrive 60 ms
    /// late, catching up linearly over 200 s (a +300 ppm ARRIVAL pacing transient, stamps correct).
    Restart,
    /// A +50 ms fleet date step: the receiver wall at the event, the sender wall 3 ms later.
    WallStep,
    /// One slot never sent and the sender's stamps (and its timeline) 47 ms later from then on:
    /// OBS PLACES the next packet at its stamp (an 80 ms jump), so the buffer grows while the arrival
    /// count lost a slot.
    StampLeap,
    /// The receiver's latched depth moves one frame (100 -> 133 ms) while the audio plays: the #1367
    /// hold SLEW moves the placement at 1000 ppm, a deliberate move the servo must not book.
    HoldSlew,
    /// A skipped slot, then 5 s later (28 ms still owed) the same one-frame relock: the recovery
    /// payment WAITS while the slew owes (never 2000 ppm), then finishes.
    SkipThenSlew,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Variant {
    Production,
    Legacy,
    NoBooking,
}

#[derive(Debug, Clone, Copy)]
struct Packet {
    /// The true slot the samples belong to.
    slot: u64,
    /// The sender's own timeline move this packet carries (stamp leap).
    leap_ns: u64,
    stamp: u64,
    arrival_ns: u64,
}

fn sender_packets(event: Event) -> Vec<Packet> {
    let mut rng = Lcg(0x1367_5845);
    let event_slot = EVENT_AT_NS * RATE / (PACKET_FRAMES * NS_PER_S);
    let mut out = Vec::new();
    let mut prev_arrival = 0_u64;
    let mut leap_ns = 0_u64;
    let mut k = 0_u64;
    loop {
        let nominal = slot_ns(k);
        if nominal >= RUN_NS {
            break;
        }
        let mut emit_copies = 1;
        match event {
            Event::Skip(n) if (event_slot..event_slot + n).contains(&k) => emit_copies = 0,
            Event::SkipThenSlew if k == event_slot => emit_copies = 0,
            Event::Dup if k == event_slot => emit_copies = 2,
            Event::Restart if (EVENT_AT_NS..EVENT_AT_NS + RESTART_SILENT_NS).contains(&nominal) => {
                emit_copies = 0
            }
            Event::StampLeap if k == event_slot => {
                emit_copies = 0;
                leap_ns = STAMP_LEAP_NS;
            }
            _ => {}
        }
        // a stamp leap moves the sender's STAMPS (its timeline), not when it sends: the arrival count
        // lost exactly the one slot. A duplicated slot resends the SAME packet: the same stamp.
        let emit = nominal + rng.ns(0, 1_000_000);
        let sender_step = if event == Event::WallStep && emit >= EVENT_AT_NS + SENDER_STEP_LAG_NS {
            WALL_STEP_NS
        } else {
            0
        };
        let stamp = (WALL0 + emit + leap_ns + sender_step) / 100 * 100;
        for _ in 0..emit_copies {
            let mut lag = 2_000_000 + rng.ns(0, 3_000_000);
            if event == Event::Restart && emit >= EVENT_AT_NS + RESTART_SILENT_NS {
                let into = (emit - EVENT_AT_NS - RESTART_SILENT_NS) as f64;
                lag += (CATCHUP_LAG_NS * (1.0 - into / CATCHUP_NS)).max(0.0) as u64;
            }
            let arrival_ns = (emit + lag).max(prev_arrival);
            prev_arrival = arrival_ns;
            out.push(Packet {
                slot: k,
                leap_ns,
                stamp,
                arrival_ns,
            });
        }
        k += 1;
    }
    out
}

fn receiver_wall(t_ns: u64, event: Event) -> u64 {
    let step = if event == Event::WallStep && t_ns >= EVENT_AT_NS {
        WALL_STEP_NS
    } else {
        0
    };
    WALL0 + t_ns + step
}

/// The receiver's latched audio hold at arrival `t_ns` (the pairing's video-delay latch).
fn receiver_hold_ms(t_ns: u64, event: Event) -> u32 {
    match event {
        Event::HoldSlew if t_ns >= EVENT_AT_NS => HOLD_SLEWED_MS,
        Event::SkipThenSlew if t_ns >= EVENT_AT_NS + SLEW_AFTER_SKIP_NS => HOLD_SLEWED_MS,
        _ => HOLD_MS,
    }
}

/// One OBS source: `asrc_process_audio` + `source_output_audio_data` + the mixer.
struct Obs {
    variant: Variant,
    c: RealtimeAsrcCompensator,
    has_last: bool,
    last_mono: u64,
    ppm_res: f64,
    frac: f64,
    raw_cur_s: f64,
    timing_set: bool,
    timing_adjust: u64,
    next_ts_min: u64,
    next_sys_min: u64,
    audio_ts: u64,
    end: u64,
    prev_mode: AudioHoldMode,
    prev_hold_ms: u32,
    slew_remaining_ns: i64,
    slew_step_ns: i64,
    /// A recovery payment and a slew step reached the same resampler call (the 2000 ppm stack).
    stacked: bool,
    have_prev: bool,
    prev_stamp_mono: u64,
    prev_raw_s: f64,
    tick: u64,
}

impl Obs {
    fn new(variant: Variant) -> Self {
        Obs {
            variant,
            c: RealtimeAsrcCompensator::new(),
            has_last: false,
            last_mono: 0,
            ppm_res: 0.0,
            frac: 0.0,
            raw_cur_s: 0.0,
            timing_set: false,
            timing_adjust: 0,
            next_ts_min: 0,
            next_sys_min: 0,
            audio_ts: 0,
            end: 0,
            prev_mode: AudioHoldMode::Off,
            prev_hold_ms: 0,
            slew_remaining_ns: 0,
            slew_step_ns: 0,
            stacked: false,
            have_prev: false,
            prev_stamp_mono: 0,
            prev_raw_s: 0.0,
            tick: 0,
        }
    }

    /// Render every mixer tick due by monotonic `mono_now`.
    fn mix_until(&mut self, mono_now: u64) {
        loop {
            let tick_at = MONO0 + frames_ns((self.tick + 1) * TICK_FRAMES);
            if tick_at > mono_now {
                return;
            }
            self.tick += 1;
            if self.audio_ts != 0 {
                let window_end = tick_at - BUFFERING_NS;
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

    fn timecode_path(&self) -> bool {
        self.variant != Variant::Legacy
    }

    /// `asrc_process_audio`: returns this packet's output duration.
    fn asrc_process(&mut self, mono_now: u64) -> u64 {
        let raw_s = PACKET_FRAMES as f64 / RATE as f64;
        self.raw_cur_s = raw_s;
        if !self.has_last {
            self.has_last = true;
            self.last_mono = mono_now;
        } else {
            let master_s = (mono_now - self.last_mono) as f64 / 1e9;
            self.last_mono = mono_now;
            let buffered_ms = self.buffered_ns() as f64 / 1e6;
            self.c.set_level_absolute(false);
            self.c.set_level_offset_ms(0.0);
            self.c.set_step_recover_hold(self.slew_remaining_ns != 0);
            let (applied, recover) = if self.c.timecode() {
                (self.c.applied_ppm(), self.c.take_step_recover_ppm())
            } else {
                self.c.compensate_with_level(raw_s, master_s, buffered_ms);
                (self.c.applied_ppm(), self.c.step_recover_ppm())
            };
            // the #1367 placement slew rides on the same resampler call
            let dt_ns = frames_ns(PACKET_FRAMES);
            let step = audio_slew_step_ns(self.slew_remaining_ns, dt_ns);
            self.slew_remaining_ns -= step;
            self.slew_step_ns += step;
            self.stacked |= step != 0 && recover != 0.0;
            self.ppm_res = audio_slew_ppm(step, dt_ns) - applied - recover;
        }
        let frames_f = PACKET_FRAMES as f64 * (1.0 + self.ppm_res / 1e6) + self.frac;
        let frames = frames_f.floor();
        self.frac = frames_f - frames;
        frames_ns(frames as u64)
    }

    /// `source_output_audio_data` for one packet; returns where its first sample landed.
    fn ingest(
        &mut self,
        pkt: &Packet,
        mono_now: u64,
        wall_now: u64,
        hold_ms: u32,
        dur: u64,
    ) -> u64 {
        let ts = pkt.stamp;
        let mut in_ts = ts;
        let mut timeline_reset = false;
        if !self.timing_set {
            self.timing_adjust = mono_now.wrapping_sub(ts);
            self.timing_set = true;
        } else if self.next_ts_min != 0 {
            let diff = abs_diff(self.next_ts_min, ts);
            if diff > MAX_TS_VAR {
                // handle_ts_jump -> reset_audio_timing + reset_audio_data
                self.timing_adjust = mono_now.wrapping_sub(ts);
                self.audio_ts = mono_now;
                self.end = mono_now;
                self.next_sys_min = mono_now;
                timeline_reset = true;
            } else if diff < TS_SMOOTHING_THRESHOLD {
                in_ts = self.next_ts_min;
            }
        }
        self.next_ts_min = in_ts + dur;
        let slew_step = std::mem::take(&mut self.slew_step_ns);
        if slew_step != 0 {
            self.next_ts_min = audio_slew_book_ts_ns(self.next_ts_min, slew_step);
            self.c.shift_level_target(slew_step as f64 / 1e6);
        }
        in_ts = in_ts.wrapping_add(self.timing_adjust);
        let mut push_back = false;
        if self.next_sys_min == in_ts {
            push_back = true;
        } else if self.next_sys_min != 0 {
            let diff = abs_diff(self.next_sys_min, in_ts);
            if diff < TS_SMOOTHING_THRESHOLD {
                push_back = true;
            } else if diff > MAX_TS_VAR {
                self.timing_adjust = mono_now.wrapping_sub(ts);
                in_ts = ts.wrapping_add(self.timing_adjust);
                timeline_reset = true;
            }
        }
        let mode = AudioHoldMode::Timecode;
        let off_live = audio_wall_to_mono_ns(mono_now, wall_now);
        let term = audio_place_term_ns(mode, hold_ms, off_live, self.timing_adjust);
        in_ts = in_ts.wrapping_add(term as u64);
        let prev_term = audio_place_term_ns(
            self.prev_mode,
            self.prev_hold_ms,
            off_live,
            self.timing_adjust,
        );
        push_back = audio_push_back_allowed(push_back, timeline_reset, mode);
        let action = audio_hold_action(
            self.prev_mode,
            self.prev_hold_ms,
            mode,
            hold_ms,
            push_back,
            true,
            self.slew_remaining_ns != 0,
        );
        let tc = self.timecode_path() && audio_asrc_timecode(mode, false, true);
        if self.c.timecode() != tc {
            self.have_prev = false;
        }
        self.c.set_timecode(tc);
        match action {
            AudioHoldAction::Slew => {
                self.slew_remaining_ns += term.wrapping_sub(prev_term);
            }
            AudioHoldAction::Place | AudioHoldAction::Replace => {
                push_back = false;
                let shift = audio_level_shift_ns(
                    action,
                    self.prev_mode,
                    term,
                    prev_term,
                    self.slew_remaining_ns,
                );
                self.c.shift_level_target(shift as f64 / 1e6);
                self.slew_remaining_ns = 0;
            }
            _ => {}
        }
        self.prev_mode = mode;
        self.prev_hold_ms = hold_ms;
        self.next_sys_min = self.next_ts_min.wrapping_add(self.timing_adjust);
        let fold = audio_placed_slew_fold_ns(
            action,
            !(push_back && self.audio_ts != 0),
            self.slew_remaining_ns,
        );
        if fold != 0 {
            self.c.shift_level_target(fold as f64 / 1e6);
            self.slew_remaining_ns = 0;
        }

        let intended = audio_intended_raw_ns(ts, self.timing_adjust, 0, 0, term);
        let appended = push_back && self.audio_ts != 0;
        let actual = audio_actual_place_ns(appended, self.audio_ts, self.buffered_ns(), in_ts);
        let place_err_ns = audio_place_error_ns(actual, intended);
        if appended {
            self.end = actual + dur;
        } else if self.audio_ts == 0 || in_ts < self.audio_ts {
            // source_output_audio_place -> reset_audio_data(in.timestamp)
            self.audio_ts = in_ts;
            self.end = in_ts + dur;
            self.next_sys_min = in_ts;
        } else {
            self.end = self.end.max(in_ts + dur);
        }

        // asrc_timecode_ingest, for a source the servo judges in timecode mode
        if self.c.timecode() {
            let err_ms = audio_asrc_error_ms(place_err_ns, self.slew_remaining_ns);
            self.c.set_step_recover_hold(self.slew_remaining_ns != 0);
            if self.variant != Variant::NoBooking {
                self.c
                    .observe_placement(err_ms, self.raw_cur_s * 1000.0, !appended);
            }
            let stamp_mono = audio_stamp_mono_ns(ts, off_live);
            if appended && self.have_prev {
                let master_s = audio_stamp_interval_s(self.prev_stamp_mono, stamp_mono);
                if master_s > 0.0 {
                    self.c
                        .compensate_with_level(self.prev_raw_s, master_s, err_ms);
                }
            }
            self.prev_stamp_mono = stamp_mono;
            self.prev_raw_s = self.raw_cur_s;
            self.have_prev = true;
        }
        actual
    }
}

#[derive(Debug, Default)]
struct Run {
    /// max |A/V| (ms) over the steady minutes before the event.
    av_before_ms: f64,
    /// max |A/V| (ms) from the event + the settle time to the end.
    av_after_ms: f64,
    /// |A/V| (ms) of the last packet.
    av_final_ms: f64,
    /// max |estimated| (ppm) from the steady start to the end.
    est_max_ppm: f64,
    jumps: u32,
    recovering: bool,
    recover_owed_final_ms: f64,
    /// max |A/V| (ms) from the event to the event + the settle time (a slew trails by up to a frame).
    av_peak_ms: f64,
    /// A recovery payment and a slew step reached the same resampler call.
    stacked: bool,
}

fn run(event: Event, variant: Variant, settle_ns: u64) -> Run {
    let mut obs = Obs::new(variant);
    let mut r = Run::default();
    for pkt in sender_packets(event) {
        let mono_now = MONO0 + pkt.arrival_ns;
        let hold_ms = receiver_hold_ms(pkt.arrival_ns, event);
        obs.mix_until(mono_now);
        let dur = obs.asrc_process(mono_now);
        let wall = receiver_wall(pkt.arrival_ns, event);
        let actual = obs.ingest(&pkt, mono_now, wall, hold_ms, dur);
        let truth = MONO0 + slot_ns(pkt.slot) + pkt.leap_ns + genlock_audio_delay_ns(hold_ms);
        let av_ms = actual.wrapping_sub(truth) as i64 as f64 / 1e6;
        let t = pkt.arrival_ns;
        if (STEADY_FROM_NS..EVENT_AT_NS).contains(&t) {
            r.av_before_ms = r.av_before_ms.max(av_ms.abs());
        }
        if t >= EVENT_AT_NS + settle_ns {
            r.av_after_ms = r.av_after_ms.max(av_ms.abs());
        } else if t >= EVENT_AT_NS {
            r.av_peak_ms = r.av_peak_ms.max(av_ms.abs());
        }
        if t >= STEADY_FROM_NS {
            r.est_max_ppm = r.est_max_ppm.max(obs.c.estimated_ppm().abs());
        }
        r.recovering |= obs.c.step_recover_ms() != 0.0;
        r.av_final_ms = av_ms;
    }
    r.jumps = obs.c.place_jump_count();
    r.recover_owed_final_ms = obs.c.step_recover_ms();
    r.stacked = obs.stacked;
    r
}

const SETTLE_NS: u64 = 60 * NS_PER_S;
/// Two skipped slots owe 66.7 ms: the 1000 ppm budget pays them in 66.7 s (+ 5 s margin).
const SETTLE_TWO_SLOTS_NS: u64 = 72 * NS_PER_S;
/// A skip, the relock 5 s later: 5 s paid, the 33.3 ms slew (payment held), the other 28.3 ms paid.
const SETTLE_SKIP_THEN_SLEW_NS: u64 = 75 * NS_PER_S;

fn scenarios() -> [(Event, u64); 8] {
    [
        (Event::Skip(1), SETTLE_NS),
        (Event::Skip(2), SETTLE_TWO_SLOTS_NS),
        (Event::Dup, SETTLE_NS),
        (Event::Restart, SETTLE_NS),
        (Event::WallStep, SETTLE_NS),
        (Event::StampLeap, SETTLE_NS),
        (Event::HoldSlew, SETTLE_NS),
        (Event::SkipThenSlew, SETTLE_SKIP_THEN_SLEW_NS),
    ]
}

#[test]
fn timecode_asrc_holds_av_and_rate_through_every_sender_event_1367() {
    for (event, settle) in scenarios() {
        let r = run(event, Variant::Production, settle);
        assert!(
            r.av_before_ms <= AV_BOUND_MS && r.av_after_ms <= AV_BOUND_MS,
            "issue 1367: {event:?}: |A/V| must stay <= {AV_BOUND_MS} ms before the event and from \
             the event + {} s on: {r:?}",
            settle / NS_PER_S
        );
        assert!(
            r.est_max_ppm <= RATE_BOUND_PPM,
            "issue 1367: {event:?}: the rate estimate of a correct sender must stay within \
             ±{RATE_BOUND_PPM} ppm: {r:?}"
        );
        assert_eq!(
            r.recover_owed_final_ms, 0.0,
            "issue 1367: {event:?}: everything booked must be paid by the end: {r:?}"
        );
        assert!(
            !r.stacked,
            "issue 1367: {event:?}: a recovery payment must wait while the placement slew owes \
             (never 2000 ppm on one resampler): {r:?}"
        );
    }
}

#[test]
fn a_hold_slew_is_followed_never_booked_and_holds_the_payment_1367() {
    // review round 1: a one-frame relock while the audio plays is a deliberate SLEW -- the servo's
    // error excludes what the slew still owes, so nothing is booked, and the audio trails the new
    // hold by at most a frame until the slew lands (the slew itself is real: peak >= 30 ms).
    let slew = run(Event::HoldSlew, Variant::Production, SETTLE_NS);
    assert!(
        slew.jumps == 0 && !slew.recovering && (30.0..=34.0).contains(&slew.av_peak_ms),
        "issue 1367: a hold slew books nothing and the audio follows it: {slew:?}"
    );
    // a skip still being paid when the relock comes: booked once, paid only around the slew
    let both = run(
        Event::SkipThenSlew,
        Variant::Production,
        SETTLE_SKIP_THEN_SLEW_NS,
    );
    assert!(
        both.jumps == 1 && both.recovering && !both.stacked,
        "issue 1367: the skip is booked once and its payment waits for the slew: {both:?}"
    );
}

#[test]
fn a_skipped_slot_books_one_jump_and_pays_at_the_budget_1367() {
    for (event, jumps) in [(Event::Skip(1), 1), (Event::Skip(2), 1), (Event::Dup, 1)] {
        let r = run(event, Variant::Production, SETTLE_NS);
        assert!(
            r.jumps == jumps && r.recovering,
            "issue 1367: {event:?} must be booked as exactly {jumps} placement jump and paid: {r:?}"
        );
    }
    // a placement at the stamp and a pure arrival transient book nothing
    for event in [Event::StampLeap, Event::Restart] {
        let r = run(event, Variant::Production, SETTLE_NS);
        assert!(
            r.jumps == 0 && !r.recovering,
            "issue 1367: {event:?} lands on its stamp: nothing is owed: {r:?}"
        );
    }
    // the budget is the 1000 ppm pitch budget
    assert!((STEP_RECOVER_PPM - 1000.0).abs() < 1e-9);
}

#[test]
fn the_arrival_servo_fails_the_catch_up_and_the_stamp_leap_1367() {
    let catch_up = run(Event::Restart, Variant::Legacy, SETTLE_NS);
    assert!(
        catch_up.est_max_ppm > RATE_BOUND_PPM && catch_up.av_after_ms > AV_BOUND_MS,
        "issue 1367: the arrival servo must read the restart catch-up as a rate and move the audio \
         off its stamps (the bench would otherwise pass vacuously): {catch_up:?}"
    );
    let leap = run(Event::StampLeap, Variant::Legacy, SETTLE_NS);
    assert!(
        leap.av_after_ms > AV_BOUND_MS,
        "issue 1367: the arrival servo must drag the audio off a timeline the sender moved (a loss \
         by count, an excess by depth: no booking, the level loop drains it): {leap:?}"
    );
}

#[test]
fn without_the_jump_booking_a_skipped_slot_is_still_off_after_60_s_1367() {
    for event in [Event::Skip(1), Event::Dup] {
        let r = run(event, Variant::NoBooking, SETTLE_NS);
        assert!(
            r.av_after_ms > AV_BOUND_MS,
            "issue 1367: {event:?}: the level loop alone must NOT settle within 60 s — the \
             1000 ppm booking is what does: {r:?}"
        );
    }
}
