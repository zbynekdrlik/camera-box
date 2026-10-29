//! Issue 1387: the JPEG XS sender's AUTOMATIC bits-per-pixel controller (owner decision
//! 29.10.2026: "auto adjust ... aby sa bpp menilo dynamicky podla toho co je mozne a co strih vie
//! prijat aby s tym nebolo vela rezie").
//!
//! A pure state machine, ticked once per second by the sender's packet thread with what the
//! second showed ([`Second`]) and fed every receiver report ([`ReceiverReport`], the 32-byte
//! datagram defined in `crate::jxs_link`). It answers a [`Decision`]; the sender applies a step
//! by opening a session at the new bpp and swapping it in at a frame boundary, then calls
//! [`AutoBpp::applied`] with the timecode of the first frame encoded at it.
//!
//! The rules:
//! - **Step DOWN at once** (one step, never below `min`) on a dropped frame (`dropped_busy`), an
//!   encode p99 over `down_fraction` of the frame budget, or a receiver report of lost slices or
//!   late frames about a frame encoded at the CURRENT bpp (a report about older frames describes a
//!   bpp already left and is ignored). A receiver cap below the current bpp steps straight to it.
//! - **Step UP one step** only after `up_after_s` CLEAN seconds in a row: frames encoded, encode
//!   p99 under `up_headroom` of the budget, no drop, no encode error, and a fresh receiver report
//!   about the current bpp with nothing lost or late. Never above `max` or the receiver's cap.
//! - **No report for `silent_after_s`: hold** (never climb blind).
//! - **A step that failed backs off:** each step down from level L doubles the clean run needed
//!   to climb back to L (capped at `max_backoff_s`); `decay_after_s` clean seconds below it halve
//!   the penalty again, so a scene that got easier is retried.
//! - While a step is being applied (`pending`) nothing else is decided.

/// The controller's tuning. [`AutoBppConfig::new`] is the sender's setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoBppConfig {
    pub min: u32,
    pub max: u32,
    /// One frame's budget at the JPEG XS rate (33.3 ms at 30 fps).
    pub budget_ms: f64,
    /// A clean second needs encode p99 under this fraction of the budget.
    pub up_headroom: f64,
    /// Encode p99 over this fraction of the budget steps down.
    pub down_fraction: f64,
    /// Clean seconds in a row before a step up.
    pub up_after_s: u32,
    /// A report older than this is silence.
    pub silent_after_s: f64,
    /// The longest clean run a failed level can demand.
    pub max_backoff_s: u32,
    /// Clean seconds below a failed level that halve its penalty.
    pub decay_after_s: u32,
}

impl AutoBppConfig {
    /// The sender's setting: 33.3 ms budget, climb at p99 < 60 % of it after 10 clean s, fall at
    /// > 90 %, hold after 3 s without a report, back off up to 5 min, forgive after 30 min.
    pub fn new(min: u32, max: u32) -> Self {
        Self {
            min,
            max: max.max(min),
            budget_ms: 1000.0 / crate::jxs_link::JXS_FPS as f64,
            up_headroom: 0.6,
            down_fraction: 0.9,
            up_after_s: 10,
            silent_after_s: 3.0,
            max_backoff_s: 300,
            decay_after_s: 1800,
        }
    }
}

/// What one second of sending showed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Second {
    /// Frames sent whole in the second.
    pub frames: u32,
    /// The p99 of their submit -> last packet sent time, ms.
    pub encode_p99_ms: f64,
    pub dropped_busy: u64,
    pub encode_errors: u64,
}

/// The receiver's one-per-second report (wire format: `crate::jxs_link::ReceiverReport`).
pub use crate::jxs_link::ReceiverReport;

/// Why the bpp changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepReason {
    /// `up_after_s` clean seconds (with this many seconds required).
    Clean {
        needed_s: u32,
    },
    DroppedFrame,
    EncodeSlow,
    ReceiverLoss,
    ReceiverLate,
    ReceiverCap,
}

impl std::fmt::Display for StepReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clean { needed_s } => write!(f, "clean-{needed_s}s"),
            Self::DroppedFrame => write!(f, "dropped-frame"),
            Self::EncodeSlow => write!(f, "encode-slow"),
            Self::ReceiverLoss => write!(f, "receiver-loss"),
            Self::ReceiverLate => write!(f, "receiver-late"),
            Self::ReceiverCap => write!(f, "receiver-cap"),
        }
    }
}

/// Why nothing changed this second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// A step is being applied.
    Pending,
    /// No receiver report for `silent_after_s`.
    SilentReceiver,
    /// The newest report describes frames of a bpp already left.
    ReportNotYetCurrent,
    /// Nothing was sent this second.
    NoFrames,
    /// Encode p99 between the up and down fractions, or an encode error.
    NoHeadroom,
    /// Clean, counting (`streak` of `needed`).
    Counting { streak: u32, needed: u32 },
    /// At the ceiling (`max` or the receiver's cap).
    AtCeiling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Hold(Hold),
    Step {
        from: u32,
        to: u32,
        reason: StepReason,
    },
}

/// The bpp levels the controller tracks a back-off penalty for (0..LEVELS); the sender's highest
/// bpp (`jxs_send::MAX_BPP`, 12) must lie below it (a const assert there).
pub const LEVELS: usize = 33;

/// A report whose seq is at most this far BELOW the newest one is a late (reordered) datagram and
/// is dropped; further below is a restarted receiver whose seq started over (one report per
/// second: 16 s of reordering never happens on a LAN).
pub const REORDER_WINDOW: u32 = 16;

#[derive(Debug, Clone)]
pub struct AutoBpp {
    cfg: AutoBppConfig,
    current: u32,
    pending: Option<(u32, StepReason)>,
    /// The timecode of the first frame encoded at `current` (`None` = not yet known).
    current_since_tc: Option<u64>,
    streak: u32,
    /// Seconds clean at the current level (the decay clock).
    clean_at_level: u32,
    /// Back-off exponent per level (each failed step down from it adds one).
    failed: [u8; LEVELS],
    report: Option<(ReceiverReport, f64)>,
    /// The sequence number of the last report a step down acted on.
    acted_seq: Option<u32>,
    /// The sequence number of the last report a clean second was counted on.
    counted_seq: Option<u32>,
    last_reason: Option<StepReason>,
}

impl AutoBpp {
    /// Start at `min` (never climb blind): the first frames at it set [`Self::applied`].
    pub fn new(cfg: AutoBppConfig) -> Self {
        Self {
            current: cfg.min,
            cfg,
            pending: None,
            current_since_tc: None,
            streak: 0,
            clean_at_level: 0,
            failed: [0; LEVELS],
            report: None,
            acted_seq: None,
            counted_seq: None,
            last_reason: None,
        }
    }

    pub fn current(&self) -> u32 {
        self.current
    }

    /// The bpp the encoder should run: the pending step's target, else the current one.
    pub fn target(&self) -> u32 {
        self.pending.map_or(self.current, |(to, _)| to)
    }

    pub fn last_reason(&self) -> Option<StepReason> {
        self.last_reason
    }

    pub fn config(&self) -> &AutoBppConfig {
        &self.cfg
    }

    /// A receiver report arrived at `now_s` (the caller's monotonic seconds).
    pub fn note_report(&mut self, r: ReceiverReport, now_s: f64) {
        if let Some((old, _)) = self.report {
            if r.seq < old.seq {
                if old.seq - r.seq <= REORDER_WINDOW {
                    // a datagram that arrived late (UDP reorders): older than what is known
                    return;
                }
                // the receiver restarted (its seq starts over): an old seq says nothing about it
                self.acted_seq = None;
                self.counted_seq = None;
            }
        }
        self.report = Some((r, now_s));
    }

    /// The pending step is in effect from the frame with timecode `first_tc` on (or, with no
    /// step pending, the start: the first frame at the starting bpp).
    pub fn applied(&mut self, bpp: u32, first_tc: u64) {
        if let Some((to, reason)) = self.pending {
            if to == bpp {
                self.current = to;
                self.last_reason = Some(reason);
                self.pending = None;
                self.current_since_tc = Some(first_tc);
                self.streak = 0;
                self.clean_at_level = 0;
            }
        } else if bpp == self.current && self.current_since_tc.is_none() {
            self.current_since_tc = Some(first_tc);
        }
    }

    /// The session at the pending bpp could not be opened: stay where we are, and count it as a
    /// failure of that level so it is not retried at once.
    pub fn refused(&mut self, bpp: u32) {
        if self.pending.map(|(to, _)| to) == Some(bpp) {
            self.pending = None;
            self.penalize(bpp);
            self.streak = 0;
        }
    }

    /// The pending step was not applied for a reason that is not the level's (the frame
    /// geometry was refused): stay where we are, no back-off.
    pub fn cancelled(&mut self, bpp: u32) {
        if self.pending.map(|(to, _)| to) == Some(bpp) {
            self.pending = None;
            self.streak = 0;
        }
    }

    fn penalize(&mut self, level: u32) {
        if let Some(f) = self.failed.get_mut(level as usize) {
            *f = f.saturating_add(1).min(16);
        }
    }

    /// Clean seconds needed to climb to `level`.
    pub fn needed_for(&self, level: u32) -> u32 {
        let k = self.failed.get(level as usize).copied().unwrap_or(0) as u32;
        let base = self.cfg.up_after_s.max(1);
        base.checked_shl(k)
            .unwrap_or(u32::MAX)
            .min(self.cfg.max_backoff_s.max(base))
    }

    fn step(&mut self, to: u32, reason: StepReason) -> Decision {
        let from = self.current;
        // every level from `to + 1` up to `from` proved too much -- except under a receiver's
        // cap, which is the receiver's policy, not a failure of those levels
        if to < from && reason != StepReason::ReceiverCap {
            for l in to + 1..=from {
                self.penalize(l);
            }
        }
        self.pending = Some((to, reason));
        self.streak = 0;
        Decision::Step { from, to, reason }
    }

    /// One second at `now_s`.
    pub fn tick(&mut self, s: &Second, now_s: f64) -> Decision {
        if self.pending.is_some() {
            return Decision::Hold(Hold::Pending);
        }
        let floor = self.cfg.min;
        let down = self.current.saturating_sub(1).max(floor);
        // the sender's own trouble: at once
        if self.current > floor && s.dropped_busy > 0 {
            return self.step(down, StepReason::DroppedFrame);
        }
        if self.current > floor
            && s.frames > 0
            && s.encode_p99_ms > self.cfg.down_fraction * self.cfg.budget_ms
        {
            return self.step(down, StepReason::EncodeSlow);
        }
        let fresh = self
            .report
            .filter(|(_, at)| now_s - at <= self.cfg.silent_after_s)
            .map(|(r, _)| r);
        // A report is about the current bpp when its newest frame was sent at it. (A frame the
        // receiver could not decode still counts as a frame with its timecode and its loss.)
        let about_current = |r: &ReceiverReport| {
            self.current_since_tc
                .is_some_and(|since| r.frames > 0 && r.last_timecode_100ns >= since)
        };
        if let Some(r) = fresh {
            // the receiver's cap applies whatever frames it describes
            if r.max_bpp > 0 && self.current > r.max_bpp.max(floor) {
                let to = r.max_bpp.max(floor);
                return self.step(to, StepReason::ReceiverCap);
            }
            if about_current(&r) && self.acted_seq != Some(r.seq) && self.current > floor {
                if r.lost_slices > 0 {
                    self.acted_seq = Some(r.seq);
                    return self.step(down, StepReason::ReceiverLoss);
                }
                if r.late_frames > 0 {
                    self.acted_seq = Some(r.seq);
                    return self.step(down, StepReason::ReceiverLate);
                }
            }
        }
        // is this second clean?
        let Some(r) = fresh else {
            self.streak = 0;
            return Decision::Hold(Hold::SilentReceiver);
        };
        if s.frames == 0 {
            self.streak = 0;
            return Decision::Hold(Hold::NoFrames);
        }
        if s.dropped_busy > 0
            || s.encode_errors > 0
            || s.encode_p99_ms >= self.cfg.up_headroom * self.cfg.budget_ms
        {
            self.streak = 0;
            return Decision::Hold(Hold::NoHeadroom);
        }
        if !about_current(&r) {
            // no evidence about the current bpp: the run of clean seconds is broken
            self.streak = 0;
            return Decision::Hold(Hold::ReportNotYetCurrent);
        }
        if r.lost_slices > 0 || r.late_frames > 0 {
            // at the floor: nothing lower to go to, and not clean
            self.streak = 0;
            return Decision::Hold(Hold::NoHeadroom);
        }
        let next = self.current + 1;
        let ceiling = if r.max_bpp > 0 {
            self.cfg.max.min(r.max_bpp)
        } else {
            self.cfg.max
        };
        if self.counted_seq == Some(r.seq) {
            // one report counts for one clean second: a receiver reporting slower than the tick
            // is not credited twice (neither clean nor broken: wait for its next report)
            if next > ceiling {
                return Decision::Hold(Hold::AtCeiling);
            }
            return Decision::Hold(Hold::Counting {
                streak: self.streak,
                needed: self.needed_for(next),
            });
        }
        self.counted_seq = Some(r.seq);
        self.streak = self.streak.saturating_add(1);
        // the decay clock counts clean REPORTS (one per second from a healthy receiver)
        self.clean_at_level = self.clean_at_level.saturating_add(1);
        if self.clean_at_level >= self.cfg.decay_after_s {
            self.clean_at_level = 0;
            if let Some(f) = self.failed.get_mut(next as usize) {
                *f = f.saturating_sub(1);
            }
        }
        if next > ceiling {
            return Decision::Hold(Hold::AtCeiling);
        }
        let needed = self.needed_for(next);
        if self.streak >= needed {
            return self.step(next, StepReason::Clean { needed_s: needed });
        }
        Decision::Hold(Hold::Counting {
            streak: self.streak,
            needed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TC0: u64 = 17_617_300_000_000_000;

    fn ctl(min: u32, max: u32) -> AutoBpp {
        let mut c = AutoBpp::new(AutoBppConfig::new(min, max));
        c.applied(min, TC0);
        c
    }

    fn good() -> Second {
        Second {
            frames: 30,
            encode_p99_ms: 10.0,
            dropped_busy: 0,
            encode_errors: 0,
        }
    }

    fn rep(seq: u32, tc: u64) -> ReceiverReport {
        ReceiverReport {
            camera_id: 5,
            max_bpp: 0,
            seq,
            frames: 30,
            lost_slices: 0,
            late_frames: 0,
            decode_p99_us: 3_000,
            last_timecode_100ns: tc,
        }
    }

    /// `n` clean seconds from second `t0` on, each with a fresh clean report about frames after
    /// `tc`; returns the decisions.
    fn clean_run(c: &mut AutoBpp, t0: u32, n: u32, tc: u64) -> Vec<Decision> {
        (0..n)
            .map(|i| {
                let t = (t0 + i) as f64;
                c.note_report(rep(t0 + i, tc + 1 + i as u64), t);
                c.tick(&good(), t)
            })
            .collect()
    }

    #[test]
    fn it_starts_at_the_floor_and_climbs_one_step_only_after_ten_clean_seconds() {
        let mut c = ctl(4, 8);
        assert_eq!(c.current(), 4);
        let d = clean_run(&mut c, 0, 10, TC0);
        for (i, x) in d[..9].iter().enumerate() {
            assert_eq!(
                *x,
                Decision::Hold(Hold::Counting {
                    streak: i as u32 + 1,
                    needed: 10
                })
            );
        }
        assert_eq!(
            d[9],
            Decision::Step {
                from: 4,
                to: 5,
                reason: StepReason::Clean { needed_s: 10 }
            }
        );
        assert_eq!(c.target(), 5);
        assert_eq!(c.current(), 4, "not applied until the swap");
        // nothing more is decided while the step is being applied
        assert_eq!(c.tick(&good(), 10.0), Decision::Hold(Hold::Pending));
        c.applied(5, TC0 + 1000);
        assert_eq!(c.current(), 5);
        assert_eq!(c.last_reason(), Some(StepReason::Clean { needed_s: 10 }));
    }

    #[test]
    fn one_unclean_second_restarts_the_count() {
        let mut c = ctl(4, 8);
        clean_run(&mut c, 0, 9, TC0);
        c.note_report(rep(9, TC0 + 50), 9.0);
        let slowish = Second {
            encode_p99_ms: 25.0, // over 60 % of 33.3, under 90 %
            ..good()
        };
        assert_eq!(c.tick(&slowish, 9.0), Decision::Hold(Hold::NoHeadroom));
        assert_eq!(c.current(), 4, "no step down for a merely slow-ish second");
        let d = clean_run(&mut c, 10, 10, TC0 + 60);
        assert!(matches!(d[9], Decision::Step { to: 5, .. }), "{d:?}");
        assert!(d[..9].iter().all(|x| matches!(x, Decision::Hold(_))));
    }

    #[test]
    fn it_steps_down_at_once_on_a_drop_a_slow_encode_or_a_receiver_loss() {
        // a dropped frame
        let mut c = ctl(4, 8);
        c.current = 7;
        let d = c.tick(
            &Second {
                dropped_busy: 1,
                ..good()
            },
            0.0,
        );
        assert_eq!(
            d,
            Decision::Step {
                from: 7,
                to: 6,
                reason: StepReason::DroppedFrame
            }
        );
        // even with no receiver report at all (the sender's own trouble needs none)
        let mut c = ctl(4, 8);
        c.current = 7;
        let d = c.tick(
            &Second {
                encode_p99_ms: 31.0,
                ..good()
            },
            0.0,
        );
        assert!(matches!(
            d,
            Decision::Step {
                to: 6,
                reason: StepReason::EncodeSlow,
                ..
            }
        ));
        // a receiver loss about the current bpp
        let mut c = ctl(4, 8);
        c.current = 6;
        c.current_since_tc = Some(TC0);
        c.note_report(
            ReceiverReport {
                lost_slices: 2,
                ..rep(1, TC0 + 5)
            },
            0.0,
        );
        assert!(matches!(
            c.tick(&good(), 0.5),
            Decision::Step {
                to: 5,
                reason: StepReason::ReceiverLoss,
                ..
            }
        ));
        // late frames
        let mut c = ctl(4, 8);
        c.current = 6;
        c.note_report(
            ReceiverReport {
                late_frames: 1,
                ..rep(1, TC0 + 5)
            },
            0.0,
        );
        assert!(matches!(
            c.tick(&good(), 0.5),
            Decision::Step {
                reason: StepReason::ReceiverLate,
                ..
            }
        ));
    }

    #[test]
    fn a_loss_report_about_frames_of_a_bpp_already_left_steps_nothing() {
        let mut c = ctl(4, 8);
        c.current = 6;
        c.current_since_tc = Some(TC0 + 1000);
        // lost slices, but its newest frame predates the switch to 6
        c.note_report(
            ReceiverReport {
                lost_slices: 9,
                ..rep(1, TC0 + 999)
            },
            0.0,
        );
        assert_eq!(
            c.tick(&good(), 0.5),
            Decision::Hold(Hold::ReportNotYetCurrent)
        );
        assert_eq!(c.target(), 6);
    }

    #[test]
    fn one_lossy_report_steps_down_once() {
        let mut c = ctl(4, 8);
        c.current = 6;
        let lossy = ReceiverReport {
            lost_slices: 1,
            ..rep(7, TC0 + 5)
        };
        c.note_report(lossy, 0.0);
        assert!(matches!(c.tick(&good(), 0.2), Decision::Step { to: 5, .. }));
        c.applied(5, TC0 + 10);
        // the same report again (no new one yet): it is about frames before 5 anyway, and acted on
        assert!(matches!(c.tick(&good(), 0.9), Decision::Hold(_)));
        assert_eq!(c.target(), 5);
    }

    #[test]
    fn a_silent_receiver_holds_and_never_climbs() {
        let mut c = ctl(4, 8);
        for t in 0..60 {
            assert_eq!(
                c.tick(&good(), t as f64),
                Decision::Hold(Hold::SilentReceiver)
            );
        }
        assert_eq!(c.target(), 4);
        // a report goes stale after 3 s: the count stops and restarts
        clean_run(&mut c, 100, 5, TC0);
        assert_eq!(c.tick(&good(), 108.5), Decision::Hold(Hold::SilentReceiver));
        let d = clean_run(&mut c, 110, 10, TC0 + 100);
        assert!(matches!(d[9], Decision::Step { to: 5, .. }), "{d:?}");
        // a silent receiver never makes the sender step DOWN either
        let mut c = ctl(4, 8);
        c.current = 7;
        assert_eq!(c.tick(&good(), 50.0), Decision::Hold(Hold::SilentReceiver));
        assert_eq!(c.target(), 7);
    }

    #[test]
    fn the_bounds_hold_and_the_receiver_cap_lowers_the_ceiling() {
        let mut c = ctl(4, 5);
        let d = clean_run(&mut c, 0, 10, TC0);
        assert!(matches!(d[9], Decision::Step { to: 5, .. }));
        c.applied(5, TC0 + 20);
        let d = clean_run(&mut c, 10, 40, TC0 + 30);
        assert!(
            d.iter().all(|x| *x == Decision::Hold(Hold::AtCeiling)),
            "never above max"
        );
        // never below min: a drop at the floor holds
        let mut c = ctl(4, 8);
        let d = c.tick(
            &Second {
                dropped_busy: 3,
                ..good()
            },
            0.0,
        );
        assert!(matches!(d, Decision::Hold(_)), "{d:?}");
        assert_eq!(c.target(), 4);
        // a receiver cap under the current bpp steps straight to it
        let mut c = ctl(4, 10);
        c.current = 9;
        c.note_report(
            ReceiverReport {
                max_bpp: 6,
                ..rep(1, TC0 + 1)
            },
            0.0,
        );
        assert_eq!(
            c.tick(&good(), 0.5),
            Decision::Step {
                from: 9,
                to: 6,
                reason: StepReason::ReceiverCap
            }
        );
        c.applied(6, TC0 + 2);
        // a cap is the receiver's policy, not a failure of 7..9: climbing back is not slowed
        assert_eq!((c.needed_for(7), c.needed_for(9)), (10, 10));
        // the cap is also the ceiling while climbing
        let mut c = ctl(4, 10);
        c.current = 6;
        for t in 0..20 {
            c.note_report(
                ReceiverReport {
                    max_bpp: 6,
                    ..rep(t, TC0 + 1 + t as u64)
                },
                t as f64,
            );
            assert_eq!(c.tick(&good(), t as f64), Decision::Hold(Hold::AtCeiling));
        }
        // a cap below min is clamped to min
        let mut c = ctl(4, 10);
        c.current = 8;
        c.note_report(
            ReceiverReport {
                max_bpp: 2,
                ..rep(1, TC0 + 1)
            },
            0.0,
        );
        assert!(matches!(c.tick(&good(), 0.1), Decision::Step { to: 4, .. }));
    }

    #[test]
    fn a_failed_level_needs_a_longer_clean_run_each_time_and_is_forgiven_slowly() {
        let mut c = ctl(4, 8);
        assert_eq!(c.needed_for(5), 10);
        // climb to 5, which then drops a frame
        clean_run(&mut c, 0, 10, TC0);
        c.applied(5, TC0 + 100);
        assert!(matches!(
            c.tick(
                &Second {
                    dropped_busy: 1,
                    ..good()
                },
                10.0
            ),
            Decision::Step { to: 4, .. }
        ));
        c.applied(4, TC0 + 200);
        assert_eq!(c.needed_for(5), 20, "back-off doubled");
        let d = clean_run(&mut c, 11, 20, TC0 + 300);
        assert!(d[..19].iter().all(|x| matches!(x, Decision::Hold(_))));
        assert!(matches!(
            d[19],
            Decision::Step {
                to: 5,
                reason: StepReason::Clean { needed_s: 20 },
                ..
            }
        ));
        // the penalty is capped
        for _ in 0..10 {
            c.penalize(5);
        }
        assert_eq!(c.needed_for(5), 300);
        // and decays after 30 min clean below it
        let mut c = ctl(4, 5);
        for _ in 0..3 {
            c.penalize(5);
        }
        assert_eq!(c.needed_for(5), 80);
        c.clean_at_level = c.cfg.decay_after_s - 1;
        c.note_report(rep(1, TC0 + 1), 0.0);
        c.tick(&good(), 0.0);
        assert_eq!(c.needed_for(5), 40);
    }

    #[test]
    fn a_refused_session_keeps_the_current_bpp_and_counts_against_the_level() {
        let mut c = ctl(4, 8);
        clean_run(&mut c, 0, 10, TC0);
        assert_eq!(c.target(), 5);
        c.refused(5);
        assert_eq!((c.current(), c.target()), (4, 4));
        assert_eq!(c.needed_for(5), 20);
        // an applied() for another bpp than the pending one changes nothing
        let mut c = ctl(4, 8);
        clean_run(&mut c, 0, 10, TC0);
        c.applied(6, TC0 + 5);
        assert_eq!((c.current(), c.target()), (4, 5));
    }

    #[test]
    fn a_report_not_yet_about_the_current_bpp_breaks_the_clean_run() {
        let mut c = ctl(4, 8);
        clean_run(&mut c, 0, 9, TC0);
        // fresh reports, but their newest frame predates the current bpp's first frame
        c.current_since_tc = Some(TC0 + 1_000);
        for t in 9..12u32 {
            c.note_report(rep(t, TC0 + 500), t as f64);
            assert_eq!(
                c.tick(&good(), t as f64),
                Decision::Hold(Hold::ReportNotYetCurrent)
            );
        }
        // the count starts over: one clean second is 1 of 10, not the 10th
        c.note_report(rep(12, TC0 + 2_000), 12.0);
        assert_eq!(
            c.tick(&good(), 12.0),
            Decision::Hold(Hold::Counting {
                streak: 1,
                needed: 10
            })
        );
    }

    #[test]
    fn one_report_counts_for_one_clean_second() {
        let mut c = ctl(4, 8);
        c.note_report(rep(1, TC0 + 1), 0.0);
        let first = c.tick(&good(), 0.0);
        assert_eq!(
            first,
            Decision::Hold(Hold::Counting {
                streak: 1,
                needed: 10
            })
        );
        // the same report, still fresh, a second and two later: not credited again, not broken
        assert_eq!(c.tick(&good(), 1.0), first);
        assert_eq!(c.tick(&good(), 2.0), first);
        c.note_report(rep(2, TC0 + 2), 2.5);
        assert_eq!(
            c.tick(&good(), 3.0),
            Decision::Hold(Hold::Counting {
                streak: 2,
                needed: 10
            })
        );
    }

    #[test]
    fn a_report_without_a_frame_says_nothing_about_the_bpp() {
        // the receiver counts an undecodable frame as a frame (its timecode + its loss), so a
        // report without any frame carries no timecode to tie a loss to a bpp: no step
        for lost in [0, 40] {
            let mut c = ctl(4, 8);
            c.current = 7;
            c.note_report(
                ReceiverReport {
                    frames: 0,
                    lost_slices: lost,
                    last_timecode_100ns: 0,
                    ..rep(1, 0)
                },
                0.0,
            );
            assert_eq!(
                c.tick(&good(), 0.5),
                Decision::Hold(Hold::ReportNotYetCurrent),
                "lost {lost}"
            );
        }
    }

    #[test]
    fn a_cancelled_step_adds_no_back_off_and_a_repeated_report_at_the_ceiling_says_so() {
        let mut c = ctl(4, 8);
        clean_run(&mut c, 0, 10, TC0);
        assert_eq!(c.target(), 5);
        c.cancelled(5);
        assert_eq!((c.current(), c.target(), c.needed_for(5)), (4, 4, 10));
        // at the ceiling, a report counted already reads AtCeiling, not Counting
        let mut c = ctl(4, 4);
        c.note_report(rep(1, TC0 + 1), 0.0);
        assert_eq!(c.tick(&good(), 0.0), Decision::Hold(Hold::AtCeiling));
        assert_eq!(c.tick(&good(), 1.0), Decision::Hold(Hold::AtCeiling));
    }

    #[test]
    fn a_restarted_receiver_whose_seq_starts_over_is_heard_again() {
        let mut c = ctl(4, 8);
        c.current = 6;
        c.note_report(
            ReceiverReport {
                lost_slices: 1,
                ..rep(3, TC0 + 5)
            },
            0.0,
        );
        assert!(matches!(c.tick(&good(), 0.1), Decision::Step { to: 5, .. }));
        c.refused(5);
        // the receiver restarts: its seq starts over (from far above, 40 -> 0) and reaches 3
        // again with a new loss
        c.note_report(rep(40, TC0 + 6), 0.5);
        c.note_report(rep(0, TC0 + 6), 1.0);
        c.note_report(
            ReceiverReport {
                lost_slices: 2,
                ..rep(3, TC0 + 7)
            },
            2.0,
        );
        assert!(
            matches!(c.tick(&good(), 2.1), Decision::Step { to: 5, .. }),
            "the stale acted seq 3 no longer hides the new report 3"
        );
    }

    #[test]
    fn a_late_reordered_report_is_dropped_and_never_acts_twice() {
        let mut c = ctl(4, 8);
        c.current = 6;
        c.note_report(
            ReceiverReport {
                lost_slices: 1,
                ..rep(5, TC0 + 5)
            },
            0.0,
        );
        assert!(matches!(c.tick(&good(), 0.1), Decision::Step { to: 5, .. }));
        c.refused(5);
        // seq 4 arrives late, also lossy: dropped, so neither a second step nor a reset
        c.note_report(
            ReceiverReport {
                lost_slices: 3,
                ..rep(4, TC0 + 6)
            },
            0.5,
        );
        let d = c.tick(&good(), 0.6);
        assert!(!matches!(d, Decision::Step { .. }), "{d:?}");
        // and a clean late one is not credited
        let mut c = ctl(4, 8);
        c.note_report(rep(5, TC0 + 5), 0.0);
        c.tick(&good(), 0.0);
        c.note_report(rep(4, TC0 + 4), 0.5);
        assert_eq!(
            c.tick(&good(), 1.0),
            Decision::Hold(Hold::Counting {
                streak: 1,
                needed: 10
            })
        );
    }

    #[test]
    fn a_lossy_report_acts_once_even_when_its_step_is_refused() {
        let mut c = ctl(4, 8);
        c.current = 6;
        c.note_report(
            ReceiverReport {
                lost_slices: 3,
                ..rep(7, TC0 + 5)
            },
            0.0,
        );
        assert!(matches!(c.tick(&good(), 0.2), Decision::Step { to: 5, .. }));
        c.refused(5);
        assert_eq!(c.current(), 6);
        // the same report, still fresh and about bpp 6: it already acted, no second step
        let d = c.tick(&good(), 0.9);
        assert!(!matches!(d, Decision::Step { .. }), "{d:?}");
        // a NEW lossy report acts again
        c.note_report(
            ReceiverReport {
                lost_slices: 1,
                ..rep(8, TC0 + 6)
            },
            1.0,
        );
        assert!(matches!(c.tick(&good(), 1.1), Decision::Step { to: 5, .. }));
    }

    #[test]
    fn reasons_print_as_short_tokens() {
        assert_eq!(StepReason::Clean { needed_s: 10 }.to_string(), "clean-10s");
        assert_eq!(StepReason::DroppedFrame.to_string(), "dropped-frame");
        assert_eq!(StepReason::ReceiverCap.to_string(), "receiver-cap");
    }
}
