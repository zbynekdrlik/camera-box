//! Issues 1372 + 1381 — the send pacing of the vendored obs-vban VBAN output, the Tier-0 authority.
//!
//! obs-vban 0.3.1 (`vendor/obs-vban/src/vban-output-thread.c`) sent at most ONE packet per wake,
//! so the cg VBAN stream left in bursts and gaps (issue 1372). The paced send thread keeps a jitter
//! buffer of a configurable target depth (default 64 ms, clamped 20–200 ms) and sends packet `n` at
//! `t0 + n × packet_duration` on the disciplined `os_gettime_ns` clock.
//!
//! Issue 1381 made the schedule a FIXED timeline (the model SongPlayer's own VBAN runs on): the
//! pacer never throws audio away because the OBS audio thread was late. This module is the pure
//! decision the thread calls on every wake:
//!
//! * **Anchor.** `t0` = the moment a full packet is first buffered + the target, set ONCE. Only
//!   an operator retarget moves it (up), and only the hard-ceiling resync skips slots of it.
//! * **Send.** Every packet whose slot is due goes out in this wake, audio from the buffer.
//! * **Late audio.** A due slot whose audio is not buffered yet waits for it, up to
//!   [`GRACE_MS`] past the slot. The audio then leaves late but COMPLETE and `late_sends` counts
//!   it. The catch-up after it is capped at twice real time: the next packet leaves at the
//!   earliest half a packet duration after the previous one, until the schedule is met again. A
//!   thread that merely woke late (its audio was there) still sends every due packet at once.
//! * **Silence.** A slot still without audio [`GRACE_MS`] after its deadline is filled with a
//!   silence packet, and so is every following slot, on schedule, until the buffer holds the
//!   target again (resuming earlier left the stream at the grace edge and cut a string of short
//!   holes). One silence episode is ONE counted discontinuity.
//! * **Stale repay.** Each silence packet stands in for a packet of audio still to come, so the
//!   episode leaves a debt of `stale_samples`. When the pacer is back on schedule and the buffer
//!   holds `target + debt` (the stall's backlog has arrived), the debt is dropped in ONE drop of
//!   whole packets and the latency is the anchored one again. Until then the stalled audio plays
//!   late, so the drop is a forward skip, its own audible splice: `repays` counts it. The caller
//!   advances the VBAN frame counter across every dropped packet, so the receiver's own loss
//!   counter sees it. A buffering hole in OBS brings no backlog: its debt is never repaid (nothing
//!   is discarded, the silence IS the hole) and it is forgiven when the next silence episode
//!   starts.
//! * **Hard ceiling.** More than [`CEILING_MS`] buffered, or the next slot more than
//!   [`CEILING_MS`] overdue (a send thread frozen for seconds, a host that slept), is one counted
//!   resync: the buffer drops back to the target and the schedule continues at the first slot at
//!   or after now, instead of chasing an old grid at twice real time.
//! * **Retarget.** A new target while running moves the schedule later (up) or drops the
//!   difference at the next wake, never below the new target (down, a counted discontinuity).
//!
//! There is no trim and no overflow drop any more. The C twin is
//! `vendor/obs-vban/src/vban-pacing.h`; `tests/vban_pacing_parity_1372.rs` compiles the shipped
//! header and requires identical decisions. This module decides WHEN and HOW MANY packets go out,
//! whether a packet is audio or silence, and what is dropped; packet contents are the caller's.

/// Default jitter-buffer target depth when the output setting is unset (0) or negative.
pub const TARGET_MS_DEFAULT: i64 = 64;
/// Smallest accepted target depth.
pub const TARGET_MS_MIN: i64 = 20;
/// Largest accepted target depth.
pub const TARGET_MS_MAX: i64 = 200;
/// How long past its deadline a slot waits for late audio before it is filled with silence.
pub const GRACE_MS: u64 = 100;
/// Buffered audio above this, or a next slot more than this overdue, is a counted resync.
pub const CEILING_MS: u64 = 2000;
/// The `obs-vban pacing:` log line cadence.
pub const LOG_INTERVAL_NS: u64 = 10_000_000_000;
/// The longest single wait on the audio event.
pub const IDLE_WAIT_MS: u32 = 10;

const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_MS: u64 = 1_000_000;

/// The target depth the thread uses for an output setting value.
pub fn clamp_target_ms(ms: i64) -> u32 {
    if ms <= 0 {
        return TARGET_MS_DEFAULT as u32;
    }
    if ms < TARGET_MS_MIN {
        return TARGET_MS_MIN as u32;
    }
    if ms > TARGET_MS_MAX {
        return TARGET_MS_MAX as u32;
    }
    ms as u32
}

/// `floor(samples × 1e9 / rate)` without overflowing 64 bits for any realistic sample count.
pub fn samples_to_ns(samples: u64, rate: u32) -> u64 {
    let rate = u64::from(rate.max(1));
    (samples / rate) * NS_PER_SEC + (samples % rate) * NS_PER_SEC / rate
}

/// `floor(ns × rate / 1e9)` without overflowing 64 bits for any realistic span.
pub fn ns_to_samples(ns: u64, rate: u32) -> u64 {
    let rate = u64::from(rate.max(1));
    (ns / NS_PER_SEC) * rate + (ns % NS_PER_SEC) * rate / NS_PER_SEC
}

/// `floor(ms × rate / 1000)`.
pub fn ms_to_samples(ms: u64, rate: u32) -> u64 {
    ms * u64::from(rate.max(1)) / 1000
}

/// The audio-event wait (whole ms) for a step that asked to wait for audio: until `wake_ns`
/// rounded UP to the next ms (0 when it has passed), never longer than [`IDLE_WAIT_MS`], and
/// [`IDLE_WAIT_MS`] when there is no deadline (`wake_ns` 0).
pub fn wait_ms(now_ns: u64, wake_ns: u64) -> u32 {
    if wake_ns == 0 {
        return IDLE_WAIT_MS;
    }
    if wake_ns <= now_ns {
        return 0;
    }
    (wake_ns - now_ns)
        .div_ceil(NS_PER_MS)
        .min(u64::from(IDLE_WAIT_MS)) as u32
}

/// One wake's decision. The caller applies it in this order: drop, audio packets, silence
/// packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    /// Audio packets of `packet_samples` to send now from the head of the buffer, oldest first.
    pub send: u32,
    /// Silence packets to send after the audio ones (each advances the frame counter).
    pub silence: u32,
    /// Oldest samples to drop BEFORE sending, always whole packets; the frame counter advances
    /// by `drop_samples / packet_samples`.
    pub drop_samples: u64,
    /// The next deadline; 0 = none (wait for audio with [`IDLE_WAIT_MS`]).
    pub wake_ns: u64,
    /// Wait on the audio event (at most until `wake_ns`, see [`wait_ms`]) instead of sleeping
    /// to `wake_ns`.
    pub wait_audio: bool,
}

/// The pacing state of one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacing {
    pub target_ms: u32,
    pub target_ns: u64,
    pub target_samples: u64,
    pub grace_ns: u64,
    pub ceiling_samples: u64,
    pub ceiling_ns: u64,
    /// Half a packet duration: the catch-up spacing (twice real time).
    pub half_packet_ns: u64,
    pub packet_samples: u32,
    pub rate: u32,
    pub running: bool,
    pub primed: bool,
    pub prime_ns: u64,
    pub t0_ns: u64,
    /// Slots used so far (audio and silence packets); slot `n` is due at `deadline_ns(n)`.
    pub n_sent: u64,
    /// The earliest instant the next packet may leave while catching up; 0 = no cap.
    pub catchup_ns: u64,
    /// The next slot is due and waiting for its audio (within the grace).
    pub starved: bool,
    /// In a silence episode.
    pub silent: bool,
    /// Silence samples sent that the audio still owes (the debt of the current episode).
    pub stale_samples: u64,
    /// The debt is dropped at the start of the next wake.
    pub repay_ready: bool,
    /// Dropped at the next wake after a lower target, never below the target.
    pub pending_drop_samples: u64,
    /// Audio packets that left after their deadline because their audio came late.
    pub late_sends: u64,
    /// Silence episodes, resyncs and retarget drops.
    pub discontinuities: u64,
    /// Stale-debt drops made after the audio resumed: each is a forward skip, its own splice.
    pub repays: u64,
    pub silence_samples: u64,
    pub discarded_samples: u64,
    pub resyncs: u64,
    pub late_max_ns: u64,
}

impl Pacing {
    pub fn new(target_ms: i64, packet_samples: u32, rate: u32) -> Self {
        let packet_samples = packet_samples.max(1);
        let rate = rate.max(1);
        let mut p = Pacing {
            target_ms: 0,
            target_ns: 0,
            target_samples: 0,
            grace_ns: GRACE_MS * NS_PER_MS,
            ceiling_samples: ms_to_samples(CEILING_MS, rate),
            ceiling_ns: CEILING_MS * NS_PER_MS,
            half_packet_ns: samples_to_ns(u64::from(packet_samples), rate) / 2,
            packet_samples,
            rate,
            running: false,
            primed: false,
            prime_ns: 0,
            t0_ns: 0,
            n_sent: 0,
            catchup_ns: 0,
            starved: false,
            silent: false,
            stale_samples: 0,
            repay_ready: false,
            pending_drop_samples: 0,
            late_sends: 0,
            discontinuities: 0,
            repays: 0,
            silence_samples: 0,
            discarded_samples: 0,
            resyncs: 0,
            late_max_ns: 0,
        };
        p.set_target(clamp_target_ms(target_ms));
        p
    }

    fn set_target(&mut self, target_ms: u32) {
        self.target_ms = target_ms;
        self.target_ns = u64::from(target_ms) * NS_PER_MS;
        self.target_samples = ms_to_samples(u64::from(target_ms), self.rate);
    }

    /// A new target (an output setting value) while the output exists. Running: a higher target
    /// moves the schedule later by the difference, a lower one drops the difference at the next
    /// wake. Not running: the anchor simply uses the new target. The counters carry on.
    pub fn retarget(&mut self, target_ms: i64) {
        let new_ms = clamp_target_ms(target_ms);
        if new_ms == self.target_ms {
            return;
        }
        let old_ms = self.target_ms;
        self.set_target(new_ms);
        if self.running {
            if new_ms > old_ms {
                self.t0_ns += u64::from(new_ms - old_ms) * NS_PER_MS;
            } else {
                self.pending_drop_samples += ms_to_samples(u64::from(old_ms - new_ms), self.rate);
            }
        }
    }

    /// The deadline of slot `n` of the schedule.
    pub fn deadline_ns(&self, n: u64) -> u64 {
        self.t0_ns + samples_to_ns(n * u64::from(self.packet_samples), self.rate)
    }

    /// The buffer depth at which a silence episode ends.
    pub fn resume_samples(&self) -> u64 {
        self.target_samples.max(u64::from(self.packet_samples))
    }

    /// The first slot at or after `now_ns` (never before the next unsent one).
    fn first_slot_at_or_after(&self, now_ns: u64) -> u64 {
        let mut k = if now_ns > self.t0_ns {
            ns_to_samples(now_ns - self.t0_ns, self.rate) / u64::from(self.packet_samples)
        } else {
            0
        };
        k = k.max(self.n_sent);
        while self.deadline_ns(k) < now_ns {
            k += 1;
        }
        k
    }

    /// After a packet that virtually left at `v`: the next one may leave half a packet later,
    /// until that is no later than its own deadline (caught up).
    fn catch_up_from(&mut self, v: u64) {
        let next = v + self.half_packet_ns;
        self.catchup_ns = if next > self.deadline_ns(self.n_sent) {
            next
        } else {
            0
        };
    }

    /// The decision for a wake at `now_ns` with `buffered` samples waiting.
    pub fn step(&mut self, now_ns: u64, buffered: u64) -> Step {
        let ps = u64::from(self.packet_samples);
        let mut d = Step::default();
        let mut avail = buffered;

        if self.running
            && (avail > self.ceiling_samples
                || now_ns > self.deadline_ns(self.n_sent) + self.ceiling_ns)
        {
            // The hard ceiling: one counted resync back to the target, on the grid.
            let excess = avail.saturating_sub(self.target_samples);
            let t = excess - excess % ps;
            avail -= t;
            d.drop_samples = t;
            self.discarded_samples += t;
            self.resyncs += 1;
            self.discontinuities += 1;
            self.n_sent = self.first_slot_at_or_after(now_ns);
            self.catchup_ns = 0;
            self.starved = false;
            self.silent = false;
            self.stale_samples = 0;
            self.repay_ready = false;
            self.pending_drop_samples = 0;
        } else if !self.running {
            if !self.primed {
                if avail < ps {
                    d.wait_audio = true;
                    return d;
                }
                self.primed = true;
                self.prime_ns = now_ns;
            }
            let start = self.prime_ns + self.target_ns;
            if now_ns < start {
                d.wake_ns = start;
                return d;
            }
            self.running = true;
            self.t0_ns = start;
            self.n_sent = 0;
            self.catchup_ns = 0;
            self.starved = false;
            self.silent = false;
            self.stale_samples = 0;
            self.repay_ready = false;
        } else {
            if self.repay_ready {
                // the backlog is here: the debt goes in one drop of whole packets
                let t = self.stale_samples.min(avail - avail % ps);
                avail -= t;
                d.drop_samples += t;
                self.discarded_samples += t;
                self.stale_samples -= t;
                self.repay_ready = false;
                if t > 0 {
                    self.repays += 1;
                }
            }
            if self.pending_drop_samples > 0 {
                // never below the new target: a dip takes only what is above it
                let mut t = self
                    .pending_drop_samples
                    .min(avail.saturating_sub(self.target_samples));
                t -= t % ps;
                avail -= t;
                d.drop_samples += t;
                self.pending_drop_samples = 0;
                if t > 0 {
                    self.discarded_samples += t;
                    self.discontinuities += 1;
                }
            }
        }

        let resume = self.resume_samples();
        loop {
            let deadline = self.deadline_ns(self.n_sent);
            let eligible = self.catchup_ns.max(deadline);
            if eligible > now_ns {
                d.wake_ns = eligible;
                break;
            }
            if avail >= ps && (!self.silent || avail >= resume) {
                // waited for its audio and really left after its (possibly moved) deadline
                let late = (self.starved && now_ns > deadline) || self.catchup_ns > deadline;
                let v = if self.starved { now_ns } else { eligible };
                self.starved = false;
                self.silent = false;
                if late {
                    self.late_sends += 1;
                }
                self.late_max_ns = self.late_max_ns.max(now_ns - deadline);
                avail -= ps;
                d.send += 1;
                self.n_sent += 1;
                self.catch_up_from(v);
            } else if self.silent || now_ns - deadline >= self.grace_ns {
                let v = if self.silent { eligible } else { now_ns };
                if !self.silent {
                    // a new episode; an earlier debt never got its backlog and is forgiven
                    self.silent = true;
                    self.starved = false;
                    self.discontinuities += 1;
                    self.stale_samples = 0;
                    self.repay_ready = false;
                }
                // a silence slot is late by design; late_max is the lateness of AUDIO packets
                self.silence_samples += ps;
                self.stale_samples += ps;
                d.silence += 1;
                self.n_sent += 1;
                self.catch_up_from(v);
            } else {
                self.starved = true;
                d.wake_ns = deadline + self.grace_ns;
                d.wait_audio = true;
                break;
            }
        }

        if !self.starved
            && !self.silent
            && self.catchup_ns == 0
            && self.stale_samples > 0
            && avail >= self.target_samples + self.stale_samples
        {
            self.repay_ready = true;
        }
        d
    }

    /// The largest lateness since the last call, then reset (the 10 s log window).
    pub fn take_late_max_ns(&mut self) -> u64 {
        std::mem::take(&mut self.late_max_ns)
    }
}

#[cfg(test)]
#[path = "vban_pacing_bench.rs"]
mod bench;

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const PS: u32 = 239;
    const P: u64 = PS as u64;
    const MS: u64 = 1_000_000;

    /// Primed at 0 with `buffered`, then the anchor wake at the target: packet 0 is out.
    fn started(target_ms: i64, buffered: u64) -> Pacing {
        let mut p = Pacing::new(target_ms, PS, RATE);
        p.step(0, buffered);
        p.step(u64::from(p.target_ms) * MS, buffered);
        assert!(p.running);
        assert_eq!(p.n_sent, 1);
        p
    }

    /// Starved on slot `n_sent` with no audio (packet 1 of a fresh 64 ms schedule).
    fn starved_on_slot_1() -> Pacing {
        let mut p = started(64, P);
        let d1 = p.deadline_ns(1);
        let s = p.step(d1, 0);
        assert_eq!(
            s,
            Step {
                wake_ns: d1 + GRACE_MS * MS,
                wait_audio: true,
                ..Step::default()
            }
        );
        assert!(p.starved);
        p
    }

    /// Into a silence episode on slot 1 (its grace expired with no audio).
    fn silent_from_slot_1() -> Pacing {
        let mut p = starved_on_slot_1();
        let now = p.deadline_ns(1) + GRACE_MS * MS;
        let s = p.step(now, 0);
        assert_eq!((s.send, s.silence, s.drop_samples), (0, 1, 0));
        assert!(p.silent);
        p
    }

    #[test]
    fn target_is_clamped_to_20_200_with_64_default() {
        assert_eq!(clamp_target_ms(0), 64);
        assert_eq!(clamp_target_ms(-5), 64);
        assert_eq!(clamp_target_ms(1), 20);
        assert_eq!(clamp_target_ms(19), 20);
        assert_eq!(clamp_target_ms(20), 20);
        assert_eq!(clamp_target_ms(64), 64);
        assert_eq!(clamp_target_ms(200), 200);
        assert_eq!(clamp_target_ms(201), 200);
    }

    #[test]
    fn packet_deadlines_are_exact_sample_time() {
        assert_eq!(samples_to_ns(239, 48_000), 4_979_166);
        assert_eq!(samples_to_ns(48_000, 48_000), NS_PER_SEC);
        assert_eq!(
            samples_to_ns(44_100 * 3600 + 1, 44_100),
            3600 * NS_PER_SEC + 22_675
        );
        // A day of 239-sample packets never drifts: packet n is n × 239 samples from t0.
        let n = 48_000 * 86_400 / 239;
        assert_eq!(
            samples_to_ns(n * 239, 48_000),
            n * 239 * NS_PER_SEC / 48_000
        );
        assert_eq!(ns_to_samples(NS_PER_SEC, 48_000), 48_000);
        assert_eq!(ns_to_samples(4_979_166, 48_000), 238);
        assert_eq!(ns_to_samples(4_979_167, 48_000), 239);
        assert_eq!(
            ns_to_samples(86_400 * NS_PER_SEC + 1, 48_000),
            48_000 * 86_400
        );
    }

    #[test]
    fn the_constants_are_the_decided_ones_1381() {
        let p = Pacing::new(64, PS, RATE);
        assert_eq!(GRACE_MS, 100);
        assert_eq!(p.grace_ns, 100 * MS);
        assert_eq!(p.ceiling_samples, 96_000, "2 s at 48 kHz");
        assert_eq!(p.half_packet_ns, 4_979_166 / 2);
        assert_eq!(
            p.resume_samples(),
            3_072,
            "a silence episode ends at the target depth"
        );
        assert_eq!(
            Pacing::new(20, 256, 8_000).resume_samples(),
            256,
            "never below a packet"
        );
    }

    #[test]
    fn wait_ms_rounds_up_and_caps_at_the_idle_wait() {
        assert_eq!(wait_ms(5, 0), IDLE_WAIT_MS);
        assert_eq!(wait_ms(5 * MS, 5 * MS), 0);
        assert_eq!(wait_ms(5 * MS, 4 * MS), 0);
        assert_eq!(wait_ms(5 * MS, 5 * MS + 1), 1);
        assert_eq!(wait_ms(0, 3 * MS), 3);
        assert_eq!(wait_ms(0, 3 * MS + 1), 4);
        assert_eq!(wait_ms(0, 10 * MS), 10);
        assert_eq!(wait_ms(0, 250 * MS), IDLE_WAIT_MS);
    }

    #[test]
    fn waits_for_a_full_packet_then_anchors_target_later() {
        let mut p = Pacing::new(64, PS, RATE);
        let idle = Step {
            wait_audio: true,
            ..Step::default()
        };
        assert_eq!(p.step(1_000, 0), idle);
        assert_eq!(p.step(2_000, P - 1), idle);
        let s = p.step(5_000_000, P);
        assert_eq!(s.send, 0, "no send before the anchor");
        assert_eq!((s.wake_ns, s.wait_audio), (5_000_000 + 64_000_000, false));
        assert!(!p.running);
        // One ns early: still waiting.
        let s = p.step(69_000_000 - 1, 4096);
        assert_eq!((s.send, s.wake_ns), (0, 69_000_000));
        // Exactly at the anchor: packet 0 goes out.
        let s = p.step(69_000_000, 4096);
        assert!(p.running);
        assert_eq!(p.t0_ns, 69_000_000);
        assert_eq!(s.send, 1);
        assert_eq!(s.wake_ns, p.deadline_ns(1));
        assert_eq!(p.late_sends, 0);
    }

    #[test]
    fn a_late_wake_with_the_audio_there_sends_every_due_packet_at_once() {
        let mut p = started(64, 10_000);
        // Woken 12 ms late: packets 1, 2 and 3 are due. Their audio is there, so this is not a
        // late send and there is no catch-up cap.
        let now = p.deadline_ns(3) + 2_000_000;
        let s = p.step(now, 10_000);
        assert_eq!((s.send, s.silence, s.drop_samples), (3, 0, 0));
        assert_eq!(p.n_sent, 4);
        assert_eq!(s.wake_ns, p.deadline_ns(4));
        assert_eq!((p.late_sends, p.catchup_ns), (0, 0));
        assert_eq!(p.late_max_ns, now - p.deadline_ns(1));
        assert_eq!(p.take_late_max_ns(), now - p.deadline_ns(1));
        assert_eq!(p.late_max_ns, 0);
    }

    #[test]
    fn a_deadline_one_ns_ahead_is_not_due() {
        let mut p = started(64, 10_000);
        let d1 = p.deadline_ns(1);
        assert_eq!(p.step(d1 - 1, 10_000).send, 0);
        assert_eq!(p.step(d1, 10_000).send, 1);
    }

    #[test]
    fn a_slot_without_audio_waits_for_it_up_to_the_grace_and_never_moves_t0() {
        let mut p = starved_on_slot_1();
        let t0 = p.t0_ns;
        let d1 = p.deadline_ns(1);
        // Still short one ns before the grace ends: keep waiting, nothing sent or counted.
        let s = p.step(d1 + GRACE_MS * MS - 1, P - 1);
        assert_eq!((s.send, s.silence, s.wake_ns), (0, 0, d1 + GRACE_MS * MS));
        assert!(s.wait_audio);
        assert_eq!(
            (p.discontinuities, p.silence_samples, p.late_sends),
            (0, 0, 0)
        );
        // The audio arrives 99 ms late: the packet leaves now, complete, and t0 never moved.
        let now = d1 + 99 * MS;
        let s = p.step(now, 3 * P);
        assert_eq!((s.send, s.silence, s.drop_samples), (1, 0, 0));
        assert_eq!(p.t0_ns, t0);
        assert_eq!(p.late_sends, 1);
        assert!(!p.starved);
        // The next one is held by the catch-up cap: half a packet later, not all at once.
        assert_eq!(p.catchup_ns, now + p.half_packet_ns);
        assert_eq!((s.wake_ns, s.wait_audio), (now + p.half_packet_ns, false));
        assert_eq!(
            (p.discontinuities, p.silence_samples, p.discarded_samples),
            (0, 0, 0)
        );
    }

    #[test]
    fn the_catch_up_after_late_audio_is_capped_at_twice_real_time() {
        let mut p = starved_on_slot_1();
        let d1 = p.deadline_ns(1);
        let arrive = d1 + 40 * MS;
        let s = p.step(arrive, 100 * P);
        assert_eq!(s.send, 1);
        // Wake exactly on each capped instant: one packet each, half a packet apart, until the
        // cap meets the schedule; from then on the deadlines rule and nothing is late.
        let mut now = s.wake_ns;
        let mut capped = 0u64;
        while p.catchup_ns != 0 {
            assert_eq!(now, p.catchup_ns.max(p.deadline_ns(p.n_sent)));
            let s = p.step(now, 100 * P);
            assert_eq!(s.send, 1, "one packet per half-packet step");
            capped += 1;
            now = s.wake_ns;
        }
        // 40 ms behind at +4.98 ms per packet gained per two packets sent: about 16 capped sends.
        assert!((15..=17).contains(&capped), "{capped} capped sends");
        assert_eq!(p.late_sends, 1 + capped);
        assert_eq!(now, p.deadline_ns(p.n_sent), "back on the fixed schedule");
        let s = p.step(now, 100 * P);
        assert_eq!(s.send, 1);
        assert_eq!(p.late_sends, 1 + capped, "on time again");
        // Woken late inside the cap, every packet whose capped instant has passed goes out.
        let mut q = starved_on_slot_1();
        let s = q.step(q.deadline_ns(1) + 40 * MS, 100 * P);
        let late_wake = s.wake_ns + 3 * q.half_packet_ns;
        assert_eq!(q.step(late_wake, 100 * P).send, 4);
    }

    #[test]
    fn past_the_grace_the_slot_is_filled_with_silence_and_counted_once() {
        let mut p = starved_on_slot_1();
        let d1 = p.deadline_ns(1);
        let now = d1 + GRACE_MS * MS;
        let s = p.step(now, P - 1);
        assert_eq!(
            (s.send, s.silence, s.drop_samples, s.wait_audio),
            (0, 1, 0, false)
        );
        assert!(p.silent && !p.starved);
        assert_eq!(p.discontinuities, 1);
        assert_eq!((p.silence_samples, p.stale_samples), (P, P));
        assert_eq!(
            p.late_max_ns, 0,
            "late_max is the lateness of AUDIO packets; a silence slot is late by design"
        );
        // The overdue slots follow at twice real time, then silence goes out on schedule.
        assert_eq!(s.wake_ns, now + p.half_packet_ns);
        let mut t = s.wake_ns;
        let mut sent = 1u64;
        while t < d1 + 400 * MS {
            let s = p.step(t, P - 1);
            assert_eq!((s.send, s.drop_samples), (0, 0));
            sent += u64::from(s.silence);
            t = s.wake_ns;
        }
        assert_eq!(p.discontinuities, 1, "one episode, however long");
        assert_eq!(p.silence_samples, sent * P);
        assert_eq!(p.stale_samples, sent * P);
        assert_eq!(p.catchup_ns, 0, "caught up: silence on schedule");
        assert_eq!(p.discarded_samples, 0);
        // A silence packet never counts as a late send.
        assert_eq!(p.late_sends, 0);
    }

    #[test]
    fn silence_ends_only_when_the_buffer_holds_the_target_again() {
        let mut p = silent_from_slot_1();
        let resume = p.resume_samples();
        let mut t = p.catchup_ns.max(p.deadline_ns(p.n_sent));
        // More than a packet but less than the target: still silence.
        for _ in 0..30 {
            let s = p.step(t, resume - 1);
            assert_eq!(s.send, 0, "resumed below the target depth");
            t = s.wake_ns;
        }
        assert!(p.silent);
        let stale = p.stale_samples;
        let s = p.step(t, resume);
        assert_eq!(
            (s.send, s.silence),
            (1, 0),
            "audio again at the target depth"
        );
        assert!(!p.silent);
        assert_eq!(p.stale_samples, stale, "the debt waits for the backlog");
        assert_eq!(p.discontinuities, 1);
    }

    #[test]
    fn the_debt_is_repaid_in_one_drop_once_the_backlog_is_there() {
        let mut p = silent_from_slot_1();
        let mut t = p.catchup_ns.max(p.deadline_ns(p.n_sent));
        while p.silent {
            let s = p.step(
                t,
                if p.silence_samples >= 20 * P {
                    4_000
                } else {
                    0
                },
            );
            t = s.wake_ns;
        }
        let stale = p.stale_samples;
        assert!(stale >= 20 * P);
        assert_eq!(stale % P, 0);
        // Catch up the schedule (cap) with a small buffer: no repay while behind.
        while p.catchup_ns != 0 {
            let s = p.step(t, p.target_samples + P);
            assert_eq!(s.drop_samples, 0);
            t = s.wake_ns;
        }
        assert!(!p.repay_ready);
        // On schedule, one sample short of target + debt after the sends: still no repay.
        let s = p.step(t, p.target_samples + stale + P - 1);
        assert_eq!((s.send, s.drop_samples), (1, 0));
        assert!(!p.repay_ready);
        t = s.wake_ns;
        // Exactly target + debt left after this wake's send: the next wake drops the debt.
        let s = p.step(t, p.target_samples + stale + P);
        assert_eq!((s.send, s.drop_samples), (1, 0));
        assert!(p.repay_ready);
        t = s.wake_ns;
        let s = p.step(t, p.target_samples + stale + 3 * P);
        assert_eq!(
            s.drop_samples, stale,
            "exactly the silence, once, whole packets"
        );
        assert_eq!(s.send, 1);
        assert_eq!(p.stale_samples, 0);
        assert_eq!(p.discarded_samples, p.silence_samples);
        // One silence episode, and the repay is its own audible splice (a forward skip after the
        // stalled audio played late): counted as a repay, not hidden in the episode.
        assert_eq!((p.discontinuities, p.repays), (1, 1));
        // Never twice.
        let s = p.step(s.wake_ns, p.target_samples + stale + 3 * P);
        assert_eq!(s.drop_samples, 0);
    }

    #[test]
    fn no_repay_while_the_catch_up_cap_still_holds_packets() {
        // Right after a resume the pacer is behind its schedule and the cap holds packets that
        // are due; those packets are part of the buffer, so a buffer of target + debt is not a
        // backlog yet. The debt is dropped only once the schedule is met again.
        let mut p = silent_from_slot_1();
        let mut t = p.catchup_ns.max(p.deadline_ns(p.n_sent));
        while p.silent {
            let s = p.step(
                t,
                if p.silence_samples >= 20 * P {
                    4_000
                } else {
                    0
                },
            );
            t = s.wake_ns;
        }
        let stale = p.stale_samples;
        assert!(p.catchup_ns != 0, "the resume starts behind the schedule");
        let deep = p.target_samples + stale + 8 * P;
        let mut behind = 0;
        while p.catchup_ns != 0 {
            let s = p.step(t, deep);
            assert_eq!(s.drop_samples, 0, "dropped while behind the schedule");
            if p.catchup_ns != 0 {
                assert!(!p.repay_ready, "armed the repay while behind the schedule");
            }
            t = s.wake_ns;
            behind += 1;
        }
        assert!(behind >= 3, "{behind} capped wakes");
        // On schedule with the backlog buffered: the next wake drops the whole debt, once.
        if !p.repay_ready {
            t = p.step(t, deep).wake_ns;
        }
        assert!(p.repay_ready);
        let s = p.step(t, deep);
        assert_eq!(s.drop_samples, stale);
        assert_eq!(p.discarded_samples, p.silence_samples);
        assert_eq!((p.discontinuities, p.repays), (1, 1));
    }

    #[test]
    fn a_retarget_up_while_waiting_never_counts_an_on_time_packet_late() {
        // Review round 1: the waiting slot moves 36 ms into the future; its audio then arrives
        // exactly at the moved deadline. That packet is on time, not a late send.
        let mut p = starved_on_slot_1();
        let d1 = p.deadline_ns(1);
        p.retarget(100);
        let moved = p.deadline_ns(1);
        assert_eq!(moved, d1 + 36 * MS);
        let s = p.step(moved, 10 * P);
        assert_eq!(s.send, 1);
        assert_eq!(p.late_sends, 0, "on time at the moved deadline");
        assert_eq!(p.catchup_ns, 0, "no catch-up cap for an on-time packet");
        // Still overdue after a small move: that one IS a late send.
        let mut q = starved_on_slot_1();
        let d1 = q.deadline_ns(1);
        q.step(d1 + 50 * MS, 0);
        q.retarget(74);
        assert!(q.deadline_ns(1) < d1 + 60 * MS);
        q.step(d1 + 60 * MS, 10 * P);
        assert_eq!(q.late_sends, 1);
    }

    #[test]
    fn a_retarget_up_that_moves_the_waiting_slot_ahead_ends_the_wait() {
        // Review round 2: the thread steps right after a retarget (same loop turn). When the
        // waiting slot moved into the future nothing is waiting any more, so a later wake that
        // finds its audio there is a merely late wake: every due packet at once, no late send, no
        // catch-up cap.
        let mut p = starved_on_slot_1();
        let d1 = p.deadline_ns(1);
        p.retarget(100);
        let s = p.step(d1 + 5 * MS, 0);
        assert_eq!((s.send, s.silence, s.wait_audio), (0, 0, false));
        assert_eq!(s.wake_ns, p.deadline_ns(1));
        assert!(!p.starved, "the moved slot is not due: nothing is waiting");
        let s = p.step(p.deadline_ns(1) + 12 * MS, 10 * P);
        assert_eq!(s.send, 3, "packets 1, 2 and 3 are due and buffered");
        assert_eq!((p.late_sends, p.catchup_ns), (0, 0));
    }

    #[test]
    fn a_new_episode_forgives_a_debt_whose_backlog_never_came() {
        let mut p = silent_from_slot_1();
        let mut t = p.catchup_ns.max(p.deadline_ns(p.n_sent));
        for _ in 0..10 {
            t = p.step(t, 0).wake_ns;
        }
        // Resume at the target depth; the buffer stays there (a buffering hole: no backlog).
        let first = p.stale_samples;
        assert!(first > 5 * P);
        let mut k = 0;
        while p.silent || k < 40 {
            let s = p.step(t, p.resume_samples());
            t = s.wake_ns;
            k += 1;
        }
        assert_eq!(p.stale_samples, first, "no backlog, no repay");
        // Starve past the grace again: a new episode, the old debt is forgiven.
        let d = p.deadline_ns(p.n_sent);
        p.step(d, 0);
        let s = p.step(d + GRACE_MS * MS, 0);
        assert_eq!(s.silence, 1);
        assert_eq!(p.discontinuities, 2);
        assert_eq!(p.stale_samples, P);
        assert_eq!(p.discarded_samples, 0);
    }

    #[test]
    fn there_is_no_trim_and_no_overflow_drop() {
        // A second of audio above the target for 30 s, on schedule: nothing is ever dropped.
        let deep = 3_072 + 48_000;
        let mut p = started(64, deep);
        let mut t = p.deadline_ns(1);
        let end = t + 30 * NS_PER_SEC;
        while t < end {
            let s = p.step(t, deep);
            assert_eq!((s.drop_samples, s.silence), (0, 0));
            t = s.wake_ns;
        }
        assert_eq!(
            (p.discarded_samples, p.discontinuities, p.resyncs),
            (0, 0, 0)
        );
    }

    #[test]
    fn the_hard_ceiling_is_one_counted_resync_back_to_the_target_on_the_grid() {
        let mut p = started(64, 10_000);
        let t0 = p.t0_ns;
        // Exactly the ceiling is still fine.
        let now = p.deadline_ns(1);
        let s = p.step(now, p.ceiling_samples);
        assert_eq!((s.drop_samples, p.resyncs), (0, 0));
        // A frozen send thread: 2.5 s later, 2.5 s of audio above the ceiling.
        let now = now + 2_500 * MS;
        let buffered = p.ceiling_samples + 1;
        let s = p.step(now, buffered);
        let excess = buffered - p.target_samples;
        assert_eq!(s.drop_samples, excess - excess % P);
        assert_eq!((p.resyncs, p.discontinuities), (1, 1));
        assert_eq!(p.discarded_samples, s.drop_samples);
        assert_eq!(p.t0_ns, t0, "the grid never moves");
        // The overdue slots are skipped: the next packet is the first slot at or after now.
        assert!(p.deadline_ns(p.n_sent - u64::from(s.send)) >= now);
        assert!(p.deadline_ns(p.n_sent - u64::from(s.send) - 1) < now);
        assert!(s.send <= 1);
        assert_eq!(p.late_sends, 0);
    }

    #[test]
    fn a_schedule_more_than_the_ceiling_behind_resyncs_instead_of_chasing_it() {
        // A host that slept: no audio, the clock jumped. Exactly the ceiling late is the normal
        // silence path; one ns more skips the grid forward (nothing to drop).
        let mut p = started(64, P);
        let d1 = p.deadline_ns(1);
        let s = p.step(d1 + p.ceiling_ns, 0);
        assert_eq!((s.silence, p.resyncs), (1, 0));
        let mut q = started(64, P);
        let t0 = q.t0_ns;
        let now = d1 + q.ceiling_ns + 1;
        let s = q.step(now, 0);
        assert_eq!((q.resyncs, q.discontinuities, s.drop_samples), (1, 1, 0));
        assert_eq!((s.send, s.silence), (0, 0));
        assert_eq!(q.t0_ns, t0, "the grid never moves");
        assert!(q.deadline_ns(q.n_sent) >= now);
        assert!(q.deadline_ns(q.n_sent - 1) < now);
        // Not chasing: the next slot waits for audio like any other.
        assert!(s.wait_audio || s.wake_ns == q.deadline_ns(q.n_sent));
    }

    #[test]
    fn retarget_up_delays_the_schedule_and_down_drops_the_difference() {
        let mut p = started(64, 10_000);
        let next = p.deadline_ns(p.n_sent);
        p.retarget(100);
        assert_eq!(p.target_ms, 100);
        assert_eq!(p.target_samples, 4_800);
        assert_eq!(
            p.deadline_ns(p.n_sent),
            next + 36_000_000,
            "36 ms later, nothing dropped"
        );
        let s = p.step(next, 10_000);
        assert_eq!((s.send, s.drop_samples), (0, 0));
        assert_eq!(s.wake_ns, next + 36_000_000);

        let mut p = started(64, 10_000);
        let next = p.deadline_ns(p.n_sent);
        p.retarget(40);
        assert_eq!(p.deadline_ns(p.n_sent), next, "the schedule stays");
        let s = p.step(next, 10_000);
        // 24 ms = 1152 samples, whole packets: 4 x 239 = 956.
        assert_eq!(s.drop_samples, 956);
        assert_eq!(s.send, 1);
        assert_eq!((p.discarded_samples, p.discontinuities), (956, 1));
        // The same value, or a value that clamps to it, changes nothing.
        p.retarget(40);
        p.retarget(40);
        assert_eq!(p.step(p.deadline_ns(p.n_sent), 10_000).drop_samples, 0);
        assert_eq!(p.discontinuities, 1);
        // Before the schedule runs, a retarget only moves the anchor.
        let mut p = Pacing::new(64, PS, RATE);
        p.step(1_000, 500);
        p.retarget(30);
        assert_eq!(p.step(2_000, 500).wake_ns, 1_000 + 30_000_000);
        assert_eq!(p.discontinuities, 0);
    }

    #[test]
    fn a_large_retarget_down_during_a_dip_never_drops_below_the_new_target() {
        // 200 -> 20 ms is a 180 ms pending drop. Taken from a 62.5 ms dip it would leave less
        // than a packet; it drops only down to the new target.
        let mut p = started(200, 15_000);
        p.retarget(20);
        assert_eq!(p.pending_drop_samples, 8_640);
        let s = p.step(p.deadline_ns(p.n_sent), 3_000);
        // 3000 - 960 = 2040 above the new target, whole packets: 8 x 239 = 1912.
        assert_eq!(s.drop_samples, 1_912);
        assert_eq!(s.send, 1);
        assert_eq!((p.silence_samples, p.discontinuities), (0, 1));
    }

    #[test]
    fn every_drop_is_whole_packets() {
        // The caller advances the VBAN frame counter by drop_samples / packet_samples.
        let mut p = started(64, 10_000);
        let s = p.step(p.deadline_ns(1) + 3 * NS_PER_SEC, p.ceiling_samples + 117);
        assert!(s.drop_samples > 0);
        assert_eq!(s.drop_samples % P, 0);
    }

    #[test]
    fn other_rates_and_packet_sizes() {
        let p = Pacing::new(19, 256, 44_100);
        assert_eq!(p.target_ms, 20);
        assert_eq!(p.target_samples, 882);
        assert_eq!(p.ceiling_samples, 88_200);
        let p = Pacing::new(64, 0, 0);
        assert_eq!((p.packet_samples, p.rate), (1, 1));
    }
}
