//! Issue 1372 — the send pacing of the vendored obs-vban VBAN output, the Tier-0 authority.
//!
//! obs-vban 0.3.1 (`vendor/obs-vban/src/vban-output-thread.c`) sent at most ONE packet per wake
//! and woke on the audio callback or after a truncated 4 ms timeout. On resolume the OBS audio
//! callback takes 14–28 ms of each 21.3 ms tick and stalls now and then (55.3 ms), so the cg VBAN
//! stream left in bursts and gaps and the FOH receiver likely underran (finding 5845239583).
//!
//! The patched send thread keeps a jitter buffer of a configurable target depth (default 64 ms,
//! clamped 20–200 ms) and sends packet `n` at `t0 + n × packet_duration` on the disciplined
//! `os_gettime_ns` clock. This module is the pure decision the thread calls on every wake:
//!
//! * **Anchor.** `t0` = the moment a full packet is first buffered + the target. Anchoring on
//!   "depth reached the target" would waste one whole audio block of the budget (the block that
//!   completes the depth is already in it), which the logged 55 ms stall pattern underruns.
//! * **Send.** Every packet whose deadline is at or before `now` goes out in this wake, never one
//!   per wake. Lateness (`now − deadline`) feeds `late_max_ns`.
//! * **Underflow.** A due packet with less than a packet buffered stops the schedule: nothing is
//!   sent or fabricated (no zero-fill), `underflows` counts it, and the next full packet re-anchors.
//! * **Overflow.** More than `target + 200 ms` buffered drops the oldest whole packets down to the
//!   target, and `overflows` counts it.
//!
//! The C twin is `vendor/obs-vban/src/vban-pacing.h`; `tests/vban_pacing_parity_1372.rs` compiles
//! the shipped header and requires identical decisions. Packet contents and the VBAN frame
//! counter are untouched by this module: it only decides WHEN and HOW MANY packets go out.

/// Default jitter-buffer target depth when the output setting is unset (0) or negative.
pub const TARGET_MS_DEFAULT: i64 = 64;
/// Smallest accepted target depth.
pub const TARGET_MS_MIN: i64 = 20;
/// Largest accepted target depth.
pub const TARGET_MS_MAX: i64 = 200;
/// Depth above the target at which the oldest packets are dropped.
pub const OVERFLOW_HEADROOM_MS: u64 = 200;
/// The `obs-vban pacing:` log line cadence.
pub const LOG_INTERVAL_NS: u64 = 10_000_000_000;

const NS_PER_SEC: u64 = 1_000_000_000;

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

/// `floor(ms × rate / 1000)`.
pub fn ms_to_samples(ms: u64, rate: u32) -> u64 {
    ms * u64::from(rate.max(1)) / 1000
}

/// One wake's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    /// Packets of `packet_samples` to send now, oldest first.
    pub send: u32,
    /// Oldest samples to drop BEFORE sending (whole packets).
    pub drop_samples: u64,
    /// The next deadline to sleep to; 0 = wait for audio.
    pub wake_ns: u64,
}

/// The pacing state of one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacing {
    pub target_ms: u32,
    pub target_ns: u64,
    pub target_samples: u64,
    pub overflow_samples: u64,
    pub packet_samples: u32,
    pub rate: u32,
    pub running: bool,
    pub primed: bool,
    pub prime_ns: u64,
    pub t0_ns: u64,
    pub n_sent: u64,
    pub underflows: u64,
    pub overflows: u64,
    pub late_max_ns: u64,
}

impl Pacing {
    pub fn new(target_ms: i64, packet_samples: u32, rate: u32) -> Self {
        let target_ms = clamp_target_ms(target_ms);
        let rate = rate.max(1);
        let target_samples = ms_to_samples(u64::from(target_ms), rate);
        Pacing {
            target_ms,
            target_ns: u64::from(target_ms) * 1_000_000,
            target_samples,
            overflow_samples: target_samples + ms_to_samples(OVERFLOW_HEADROOM_MS, rate),
            packet_samples: packet_samples.max(1),
            rate,
            running: false,
            primed: false,
            prime_ns: 0,
            t0_ns: 0,
            n_sent: 0,
            underflows: 0,
            overflows: 0,
            late_max_ns: 0,
        }
    }

    /// The deadline of packet `n` of the current schedule.
    pub fn deadline_ns(&self, n: u64) -> u64 {
        self.t0_ns + samples_to_ns(n * u64::from(self.packet_samples), self.rate)
    }

    /// The decision for a wake at `now_ns` with `buffered` samples waiting.
    pub fn step(&mut self, now_ns: u64, buffered: u64) -> Step {
        let ps = u64::from(self.packet_samples);
        let mut d = Step::default();
        let mut avail = buffered;

        if avail > self.overflow_samples {
            let excess = avail - self.target_samples;
            d.drop_samples = excess - excess % ps;
            avail -= d.drop_samples;
            self.overflows += 1;
        }

        if !self.running {
            if !self.primed {
                if avail < ps {
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
        }

        let mut deadline = self.deadline_ns(self.n_sent);
        while deadline <= now_ns {
            if avail < ps {
                self.running = false;
                self.primed = false;
                self.underflows += 1;
                d.wake_ns = 0;
                return d;
            }
            self.late_max_ns = self.late_max_ns.max(now_ns - deadline);
            avail -= ps;
            d.send += 1;
            self.n_sent += 1;
            deadline = self.deadline_ns(self.n_sent);
        }
        d.wake_ns = deadline;
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

    fn dur(p: &Pacing, n: u64) -> u64 {
        p.deadline_ns(n) - p.t0_ns
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
    }

    #[test]
    fn waits_for_a_full_packet_then_anchors_target_later() {
        let mut p = Pacing::new(64, PS, RATE);
        assert_eq!(p.step(1_000, 0), Step::default());
        assert_eq!(p.step(2_000, u64::from(PS) - 1), Step::default());
        let s = p.step(5_000_000, u64::from(PS));
        assert_eq!(s.send, 0, "no send before the anchor");
        assert_eq!(s.wake_ns, 5_000_000 + 64_000_000);
        assert!(!p.running);
        // One ns early: still waiting.
        let s = p.step(69_000_000 - 1, 4096);
        assert_eq!((s.send, s.wake_ns), (0, 69_000_000));
        // Exactly at the anchor: packet 0 goes out.
        let s = p.step(69_000_000, 4096);
        assert!(p.running);
        assert_eq!(p.t0_ns, 69_000_000);
        assert_eq!(s.send, 1);
        assert_eq!(s.wake_ns, 69_000_000 + dur(&p, 1));
    }

    #[test]
    fn every_due_packet_is_sent_in_one_wake() {
        let mut p = Pacing::new(64, PS, RATE);
        p.step(0, 10_000);
        p.step(64_000_000, 10_000);
        assert_eq!(p.n_sent, 1);
        // Woken 12 ms late: packets 1, 2 and 3 are due (deadlines 4.98, 9.96, 14.94 ms).
        let now = 64_000_000 + 12_000_000 + 2_937_500;
        let s = p.step(now, 10_000);
        assert_eq!(s.send, 3);
        assert_eq!(p.n_sent, 4);
        assert_eq!(s.wake_ns, p.deadline_ns(4));
        assert_eq!(p.late_max_ns, now - p.deadline_ns(1));
        assert_eq!(p.take_late_max_ns(), now - p.deadline_ns(1));
        assert_eq!(p.late_max_ns, 0);
    }

    #[test]
    fn a_deadline_one_ns_ahead_is_not_due() {
        let mut p = Pacing::new(64, PS, RATE);
        p.step(0, 10_000);
        p.step(64_000_000, 10_000);
        let d1 = p.deadline_ns(1);
        assert_eq!(p.step(d1 - 1, 10_000).send, 0);
        assert_eq!(p.step(d1, 10_000).send, 1);
    }

    #[test]
    fn underflow_sends_nothing_counts_and_reanchors_on_the_next_full_packet() {
        let mut p = Pacing::new(64, PS, RATE);
        p.step(0, u64::from(PS) * 2);
        let s = p.step(64_000_000, u64::from(PS) * 2);
        assert_eq!(s.send, 1);
        // Packet 1 due with only PS - 1 samples: nothing is sent, the schedule stops.
        let s = p.step(p.deadline_ns(1), u64::from(PS) - 1);
        assert_eq!(s, Step::default());
        assert_eq!(p.underflows, 1);
        assert!(!p.running && !p.primed);
        // Still short: keep waiting, no second count.
        assert_eq!(p.step(80_000_000, u64::from(PS) - 1), Step::default());
        assert_eq!(p.underflows, 1);
        // A full packet arrives: the schedule re-anchors target later.
        let s = p.step(100_000_000, 1024);
        assert_eq!((s.send, s.wake_ns), (0, 164_000_000));
    }

    #[test]
    fn partial_burst_before_an_underflow_is_still_sent() {
        let mut p = Pacing::new(64, PS, RATE);
        p.step(0, 10_000);
        p.step(64_000_000, 10_000);
        // Packets 1..=4 due, only two packets buffered: two go out, then the underflow.
        let s = p.step(p.deadline_ns(4), u64::from(PS) * 2 + 7);
        assert_eq!((s.send, s.wake_ns), (2, 0));
        assert_eq!(p.underflows, 1);
    }

    #[test]
    fn overflow_drops_the_oldest_whole_packets_down_to_the_target() {
        let mut p = Pacing::new(64, PS, RATE);
        assert_eq!(p.target_samples, 3072);
        assert_eq!(p.overflow_samples, 3072 + 9600);
        p.step(0, 1024);
        p.step(64_000_000, 1024);
        let at = p.deadline_ns(1) - 1;
        // Exactly at the limit: no drop.
        assert_eq!(p.step(at, p.overflow_samples).drop_samples, 0);
        assert_eq!(p.overflows, 0);
        let s = p.step(at, p.overflow_samples + 1);
        let excess = p.overflow_samples + 1 - p.target_samples;
        assert_eq!(s.drop_samples, excess - excess % u64::from(PS));
        assert_eq!(s.drop_samples % u64::from(PS), 0);
        assert_eq!(p.overflows, 1);
        assert!(p.overflow_samples + 1 - s.drop_samples >= p.target_samples);
    }

    #[test]
    fn other_rates_and_packet_sizes() {
        let p = Pacing::new(19, 256, 44_100);
        assert_eq!(p.target_ms, 20);
        assert_eq!(p.target_samples, 882);
        assert_eq!(p.overflow_samples, 882 + 8820);
        let p = Pacing::new(64, 0, 0);
        assert_eq!((p.packet_samples, p.rate), (1, 1));
    }
}
