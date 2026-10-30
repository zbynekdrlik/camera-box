//! Issue 1387 (owner 29.10.2026: "nech je teda nase riesenie sota a nech aj nieco znesie a vie sa
//! nejako obnovit rozchodit. Aby sa mi nestalo ze ... otvori nejake video na stream pc a sa mu
//! saturne prijem a zacne padat video spoj"): the JPEG XS sender's DELAY-BASED congestion control,
//! pure.
//!
//! The receiver reports, once a second, two delay signals (`crate::jxs_link::congestion_meter`):
//! the queuing delay (the path's queue above its quiet minimum) and the arrival span of a whole
//! frame. The sender adds its own send span of the same second. A bottleneck that cross traffic
//! fills (a download on the receiving PC) shows as a growing queue and a stretched arrival span
//! BEFORE its buffer overflows -- the WebRTC GCC / L4S principle, delay before loss.
//!
//! The rules, one decision per NEW report (its seq):
//! - **Overuse** = queuing delay over [`CongestionConfig::queue_overuse_us`], or the arrival span's
//!   STRETCH (arrival span - send span) GROWN over its own quiet base (its minimum over the last
//!   [`BASE_REPORTS`] reports, like the queue's) by more than `max(stretch_overuse_us,
//!   stretch_overuse_frac x send span)`, or a lost slice. Then the PACE factor drops x`decrease` at
//!   once (never below `min_factor`; the sender also never paces below what a frame needs in its
//!   interval, `crate::jxs_link::pace_auto`): the frame is spread thinner, its burst queues less.
//!   The base matters (live 30.10.2026, strih-snv -> stream-snv): the two spans are measured at
//!   different points (an encoder-bound send, the program's NDI frame sharing the receiver's port
//!   once a frame) and read 1.5-2.5 ms apart report after report with the queue at 0 and nothing
//!   lost; judged raw, that constant offset read "overuse" every ~8 s, sawed the pace 900 -> 738
//!   and pinned the auto bpp at 4.
//!   The base is kept per send regime (the bpp) and ages by STEADY time ([`STRETCH_BASE_S`]: the
//!   time since the first report minus the time after an overuse, so a persistent bottleneck never
//!   ages its own evidence out, while a level left in quiet times re-learns its base, that report
//!   recorded, not judged). The safe start's reports are neither judged nor recorded
//!   ([`StretchMode::Skip`]). Reports under the congestion factor's cut pace ARE recorded: the base
//!   is a minimum, so their smaller stretch only keeps the judgement sensitive while a download
//!   lasts (not recording them cost the bench's download 30 lossy frames instead of 1), and they
//!   age out 30 steady seconds after it.
//! - **A DELAY overuse** -- a queue, or a stretch GROWN over its base (for `bpp_after_overuse`
//!   reports in a row: 1 = at once) -- ALSO asks the auto bpp for ONE step down
//!   (`crate::jxs_auto_bpp::StepReason::Congestion`), at most once per `bpp_settle_s`: the queue
//!   needs a moment to drain before the next step is judged. A burst spread AT its base never asks
//!   (it is no overuse at all); a grown one does, because in a saturating download the first
//!   datagram's queue barely moves while the stretch grows a second before the overflow. A loss
//!   alone never asks: the auto bpp already steps on a loss report (`receiver-loss`).
//!
//! The defaults are tuned on the bottleneck bench (`crate::jxs_congestion_bench`, a 1 GbE port
//! with a 1 MB buffer and a download ramping to 60 % of it in 5 s): at 25 % stretch and two
//! reports before the bpp ask, the first datagram was lost a second BEFORE the first step; at
//! 15 % and one report with a 2 s settle (FEC overhead in the model) the download cost 49 lossy
//! frames; with a 1 s settle the step comes 2 s before the first loss and the whole download costs
//! ONE lossy frame instead of 87 (review round 1).
//!
//! - **Clean** = queuing delay under `queue_clear_us`, no stretch, no loss. After
//!   `clean_before_increase` clean reports in a row the factor climbs back `increase_step` per
//!   clean report, up to 1. The bpp climbs back through the auto bpp's own clean-seconds rule.
//! - Between the two (a queue that neither grows past the overuse bound nor clears): hold.
//! - No report, or a report whose delay fields were not measured (a version-1 receiver): hold.

/// Reports the path's "no queue" base is taken over (30 s at one a second): longer than any
/// adaptation, so a queue that persists while the sender still adapts never becomes the base.
pub const BASE_REPORTS: usize = 30;
/// The span stretch's base is the minimum over this many seconds of its level's reports.
pub const STRETCH_BASE_S: f64 = 30.0;
/// A path delay this far above the base: start over (a BACKSTOP). A clock step should never get
/// here: the receiver removes its own steps (`WallSteps`), a sender step cancels per frame (its
/// timecode and its own delay move together) and the report windows around it are not judged
/// (`PaceControl`'s step epochs, review round 3); a downward jump needs nothing (the minimum takes
/// it). An earlier rule that moved the base on a "tight" jump invented a queue after a real one
/// drained at once (review round 2 of item 2), and is gone.
pub const STEP_RESET_US: i64 = 100_000;

/// The sender's half of the queuing-delay signal (pure): the receiver's MEAN of (first arrival -
/// timecode) minus the SENDER's own mean of (first send - timecode) of the same frames = the path
/// delay plus the clocks' offset, constant on a quiet path. The queue = the path delay above its
/// minimum over the last [`BASE_REPORTS`]. The sender's part (on a cambox: the capture's phase in
/// its 60 fps emit slot, a sawtooth that wanders 0..16.7 ms and jumps a slot at an edge, plus the
/// encode) cancels; the receiver alone cannot remove it (review round 1 of item 2). MEANS, not
/// medians: the two ends' frame sets can differ by a frame, and at a slot-edge crossing the delays
/// are two groups 16.7 ms apart, where a one-frame shift flips a median by a whole slot but moves
/// a mean by 16.7 / 30 = 0.56 ms (review round 2).
#[derive(Debug, Default, Clone)]
pub struct PathDelay {
    history: std::collections::VecDeque<i64>,
}

impl PathDelay {
    /// One report: the receiver's delay mean and the sender's own mean (us). Returns the queuing
    /// delay in us (`None` right after a big clock step).
    pub fn update(&mut self, rx_mean_us: i64, own_mean_us: i64) -> Option<u32> {
        let path = rx_mean_us - own_mean_us;
        let base = self.history.iter().copied().fold(path, i64::min);
        if path - base > STEP_RESET_US {
            self.history.clear();
            self.history.push_back(path);
            return None;
        }
        self.history.push_back(path);
        while self.history.len() > BASE_REPORTS {
            self.history.pop_front();
        }
        Some((path - base).clamp(0, u32::MAX as i64 - 1) as u32)
    }
}

/// The controller's tuning; [`CongestionConfig::default`] is the sender's setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CongestionConfig {
    /// Queuing delay above which the path is over-used (us). A 1 GbE port fills ~4 ms of queue with
    /// 500 KB: a third of a typical small-switch buffer.
    pub queue_overuse_us: u32,
    /// Queuing delay under which the path counts as clean (us).
    pub queue_clear_us: u32,
    /// Arrival span longer than the send span by this much (us) ...
    pub stretch_overuse_us: u32,
    /// ... and by this fraction of the send span, whichever is larger, is over-use.
    pub stretch_overuse_frac: f64,
    /// The pace factor's multiplier on over-use.
    pub decrease: f64,
    /// The pace factor never goes below this.
    pub min_factor: f64,
    /// The factor's climb per clean report, once climbing.
    pub increase_step: f64,
    /// Clean reports in a row before the factor climbs.
    pub clean_before_increase: u32,
    /// Delay over-use reports in a row that ask the auto bpp for a step down.
    pub bpp_after_overuse: u32,
    /// Seconds between two bpp steps the controller asks for.
    pub bpp_settle_s: f64,
}

impl Default for CongestionConfig {
    fn default() -> Self {
        Self {
            queue_overuse_us: 4_000,
            queue_clear_us: 1_500,
            stretch_overuse_us: 1_500,
            stretch_overuse_frac: 0.15,
            decrease: 0.85,
            min_factor: 0.5,
            increase_step: 0.05,
            clean_before_increase: 5,
            bpp_after_overuse: 1,
            bpp_settle_s: 1.0,
        }
    }
}

/// What one receiver report says about the path, plus the sender's own send span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathReport {
    pub seq: u32,
    pub queue_delay_us: Option<u32>,
    /// The receiver's p50 arrival span of a whole frame (us).
    pub recv_span_us: Option<u32>,
    /// The sender's p50 send span (first send -> last send of a frame) of the same second (us).
    pub send_span_us: Option<u32>,
    pub lost_slices: u32,
    /// The sender withheld this report's queuing delay (a window across its own wall step, review
    /// round 3): the report holds the clean count where it is, never counts as clean.
    pub delay_unjudged: bool,
    /// The send regime the spans were measured in (the sender's bpp): the stretch's quiet base is
    /// kept per regime, because the two spans' offset depends on the frame's size against the
    /// encoder's span and the pace (`None` = one shared regime).
    pub stretch_regime: Option<u32>,
    /// How this report's stretch is used ([`StretchMode`]).
    pub stretch_mode: StretchMode,
}

/// How a report's stretch (arrival span - send span) is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StretchMode {
    /// Judged against the base and recorded into it (the steady pace).
    #[default]
    Base,
    /// Neither (the safe start before the first receiver link).
    Skip,
}

/// The path's state as last judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathState {
    #[default]
    Unknown,
    Clean,
    Hold,
    Overuse,
}

impl std::fmt::Display for PathState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown",
            Self::Clean => "clean",
            Self::Hold => "hold",
            Self::Overuse => "overuse",
        })
    }
}

/// One decision.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CongestionStep {
    /// The pace factor now in force, 0.5..=1.
    pub factor: f64,
    /// Ask the auto bpp for one step down now.
    pub bpp_down: bool,
    pub state: PathState,
    /// The judged report changed the factor.
    pub changed: bool,
}

/// The controller (module doc).
#[derive(Debug, Clone)]
pub struct Congestion {
    cfg: CongestionConfig,
    factor: f64,
    over_streak: u32,
    clean_streak: u32,
    last_seq: Option<u32>,
    last_bpp_down_s: Option<f64>,
    state: PathState,
    /// Per send regime, its last [`BASE_REPORTS`] measured stretches (arrival span - send span,
    /// us): their minimum is the quiet base the stretch is judged against. A regime's entries age
    /// only with that regime's own reports, so a step back to a bpp left a minute ago judges
    /// against the base it learnt then (before the congestion), never a new one.
    stretch_history:
        std::collections::BTreeMap<Option<u32>, std::collections::VecDeque<(f64, i64)>>,
    /// The caller's time of the previous judged report, and the seconds spent after an overuse
    /// since the first one: the steady clock the stretch base ages on is `now - congested_s`.
    last_report_s: Option<f64>,
    congested_s: f64,
}

impl Default for Congestion {
    fn default() -> Self {
        Self::new(CongestionConfig::default())
    }
}

impl Congestion {
    pub fn new(cfg: CongestionConfig) -> Self {
        Self {
            cfg,
            factor: 1.0,
            over_streak: 0,
            clean_streak: 0,
            last_seq: None,
            last_bpp_down_s: None,
            state: PathState::Unknown,
            stretch_history: std::collections::BTreeMap::new(),
            last_report_s: None,
            congested_s: 0.0,
        }
    }

    pub fn factor(&self) -> f64 {
        self.factor
    }

    /// The factor in whole percent (the pace decision's input).
    pub fn factor_pct(&self) -> u32 {
        (self.factor * 100.0).round().clamp(1.0, 100.0) as u32
    }

    pub fn state(&self) -> PathState {
        self.state
    }

    fn hold(&self) -> CongestionStep {
        CongestionStep {
            factor: self.factor,
            bpp_down: false,
            state: self.state,
            changed: false,
        }
    }

    /// Judge `r` at `now_s` (the caller's seconds). A report already judged (the same seq) holds.
    pub fn on_report(&mut self, r: &PathReport, now_s: f64) -> CongestionStep {
        if self.last_seq == Some(r.seq) {
            return self.hold();
        }
        self.last_seq = Some(r.seq);
        let c = self.cfg;
        // the time since the previous report counts as congested when that report read overuse
        if let Some(last) = self.last_report_s {
            if self.state == PathState::Overuse {
                self.congested_s += (now_s - last).max(0.0);
            }
        }
        self.last_report_s = Some(now_s);
        let stretch_over = match (r.recv_span_us, r.send_span_us) {
            (Some(rx), Some(tx)) if r.stretch_mode != StretchMode::Skip => {
                let stretch = rx as i64 - tx as i64;
                let steady_now = now_s - self.congested_s;
                let history = self.stretch_history.entry(r.stretch_regime).or_default();
                // the base ages by steady time: a level left longer than the window learns it
                // anew, a congested stretch of time never ages it
                while history
                    .front()
                    .is_some_and(|(t, _)| steady_now - t > STRETCH_BASE_S)
                {
                    history.pop_front();
                }
                let base = history.iter().map(|(_, s)| *s).min();
                history.push_back((steady_now, stretch));
                while history.len() > BASE_REPORTS {
                    history.pop_front();
                }
                let bound = (c.stretch_overuse_us as f64).max(c.stretch_overuse_frac * tx as f64);
                // no base yet (a new or re-learnt level): recorded, not judged
                base.is_some_and(|b| (stretch - b) as f64 > bound)
            }
            _ => false,
        };
        let queue_over = r.queue_delay_us.is_some_and(|q| q > c.queue_overuse_us);
        let delay_over = queue_over || stretch_over;
        let before = self.factor;
        if delay_over || r.lost_slices > 0 {
            self.state = PathState::Overuse;
            self.clean_streak = 0;
            self.factor = (self.factor * c.decrease).max(c.min_factor);
            let mut bpp_down = false;
            if delay_over {
                self.over_streak += 1;
                let settled = self
                    .last_bpp_down_s
                    .is_none_or(|t| now_s - t >= c.bpp_settle_s);
                if self.over_streak >= c.bpp_after_overuse && settled {
                    bpp_down = true;
                    self.last_bpp_down_s = Some(now_s);
                }
            } else {
                self.over_streak = 0;
            }
            return CongestionStep {
                factor: self.factor,
                bpp_down,
                state: self.state,
                changed: self.factor != before,
            };
        }
        if r.delay_unjudged {
            // a withheld delay is no evidence either way: hold, the clean run and the over-use
            // streak neither grow nor restart (review rounds 3 and 4)
            self.state = PathState::Hold;
            return self.hold();
        }
        self.over_streak = 0;
        let measured = r.queue_delay_us.is_some() || r.recv_span_us.is_some();
        let clean = measured && r.queue_delay_us.is_none_or(|q| q < c.queue_clear_us);
        if clean {
            self.state = PathState::Clean;
            self.clean_streak += 1;
            if self.clean_streak >= c.clean_before_increase {
                self.factor = (self.factor + c.increase_step).min(1.0);
            }
        } else {
            self.state = if measured {
                PathState::Hold
            } else {
                PathState::Unknown
            };
            self.clean_streak = 0;
        }
        CongestionStep {
            factor: self.factor,
            bpp_down: false,
            state: self.state,
            changed: self.factor != before,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(seq: u32, queue: u32, rx: u32, tx: u32) -> PathReport {
        PathReport {
            seq,
            queue_delay_us: Some(queue),
            recv_span_us: Some(rx),
            send_span_us: Some(tx),
            lost_slices: 0,
            delay_unjudged: false,
            ..PathReport::default()
        }
    }

    #[test]
    fn a_growing_queue_lowers_the_pace_at_once_and_the_bpp_when_it_holds() {
        let mut c = Congestion::default();
        let s = c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0);
        assert_eq!(
            (s.state, s.bpp_down, s.changed),
            (PathState::Overuse, true, true)
        );
        assert!((s.factor - 0.85).abs() < 1e-9);
        // the next ask waits for the settle time (the queue drains first)
        assert!(!c.on_report(&rep(2, 6_000, 20_000, 20_000), 1.5).bpp_down);
        assert!(!c.on_report(&rep(3, 6_000, 20_000, 20_000), 1.9).bpp_down);
        assert!(c.on_report(&rep(4, 6_000, 20_000, 20_000), 2.0).bpp_down);
        // a tuned bpp_after_overuse of 2 waits for a second report in a row
        let mut two = Congestion::new(CongestionConfig {
            bpp_after_overuse: 2,
            ..CongestionConfig::default()
        });
        assert!(!two.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0).bpp_down);
        assert!(two.on_report(&rep(2, 6_000, 20_000, 20_000), 2.0).bpp_down);
        let mut c = Congestion::default();
        // the factor never falls under its floor
        for k in 6..40 {
            c.on_report(&rep(k, 6_000, 20_000, 20_000), k as f64);
        }
        assert!((c.factor() - 0.5).abs() < 1e-9);
        assert_eq!(c.factor_pct(), 50);
    }

    #[test]
    fn a_stretch_that_grows_over_its_quiet_base_is_over_use_and_a_small_one_is_not() {
        // the quiet base: arrival = send span + 4 ms (above the raw 3 ms bound: a rule that judged
        // the raw stretch fails every quiet report here, review of the live fix)
        let quiet = |c: &mut Congestion, from: u32, tx: u32| {
            for k in from..from + 5 {
                let s = c.on_report(&rep(k, 0, tx + 4_000, tx), k as f64);
                assert_ne!(s.state, PathState::Overuse, "quiet report {k}");
            }
        };
        let mut c = Congestion::default();
        quiet(&mut c, 1, 20_000);
        // 20 ms sent, 27.5 ms arrived: 3.5 ms over the base > max(1.5 ms, 15 % = 3 ms)
        assert_eq!(
            c.on_report(&rep(10, 0, 27_500, 20_000), 10.0).state,
            PathState::Overuse
        );
        let mut c = Congestion::default();
        quiet(&mut c, 1, 20_000);
        // 20 ms sent, 26.5 ms arrived: 2.5 ms over the base <= 3 ms
        assert_eq!(
            c.on_report(&rep(10, 0, 26_500, 20_000), 10.0).state,
            PathState::Clean
        );
        // a short send span takes the 1.5 ms floor: 4 ms sent, 10.5 ms arrived (2.5 over the base)
        let mut c = Congestion::default();
        quiet(&mut c, 1, 4_000);
        assert_eq!(
            c.on_report(&rep(10, 0, 10_500, 4_000), 10.0).state,
            PathState::Overuse
        );
    }

    /// Live 30.10.2026 (strih-snv -> stream-snv, coordinator): the arrival span read 13.5-13.8 ms
    /// against a send span of 11.5-12.2 ms report after report, with the queue at 0.0-0.1 ms and
    /// no loss -- a CONSTANT offset of the two measurements (the program's NDI burst sharing the
    /// receiver's port each frame, the encoder-bound send), never a growing bottleneck. It read
    /// "overuse" every ~8 s, cut the pace and asked the auto bpp down each time: the bpp stayed
    /// at 4. A constant offset is the stretch's quiet base, never overuse.
    #[test]
    fn a_constant_span_offset_with_no_queue_is_never_over_use() {
        let mut c = Congestion::default();
        for k in 1..120u32 {
            // the live pairs: the send span moves with the pace, the arrival span with the encoder
            let (rx, tx) = if k % 2 == 0 {
                (13_800, 12_200)
            } else {
                (13_500, 11_500)
            };
            let s = c.on_report(&rep(k, 100, rx, tx), k as f64);
            assert_ne!(s.state, PathState::Overuse, "report {k}");
            assert!(!s.bpp_down, "report {k}");
        }
        assert!(
            (c.factor() - 1.0).abs() < 1e-9,
            "the pace factor stays at 100 %"
        );
    }

    /// A burst spread that stays at its base (the live 30.10.2026 offset) never asks the auto bpp
    /// for a step (coordinator: "a burst-spread overuse with no queue must never step the bpp
    /// down"); a stretch that GROWS over its base does, queue or not: in the saturating-download
    /// bench the first datagram's queue barely moves (0.60 -> 0.62 ms) while the stretch grows
    /// 1.6 -> 4.3 ms a second before the buffer overflows, so the grown stretch is the early signal.
    #[test]
    fn a_stretch_at_its_base_never_asks_for_a_bpp_step_and_a_grown_one_does() {
        let mut c = Congestion::default();
        for k in 1..20 {
            // 4 ms over the send span: above the raw 3 ms bound, at its own base
            let s = c.on_report(&rep(k, 0, 24_000, 20_000), k as f64);
            assert!(!s.bpp_down, "report {k}: an offset at its base never asks");
        }
        let s = c.on_report(&rep(20, 0, 28_000, 20_000), 20.0);
        assert_eq!(s.state, PathState::Overuse);
        assert!(s.bpp_down, "a grown stretch asks, with no queue");
    }

    #[test]
    fn a_loss_lowers_the_pace_but_never_asks_for_a_bpp_step_by_itself() {
        let mut c = Congestion::default();
        for k in 1..10 {
            let s = c.on_report(
                &PathReport {
                    lost_slices: 3,
                    ..rep(k, 0, 20_000, 20_000)
                },
                k as f64,
            );
            assert!(!s.bpp_down, "the auto bpp steps on the loss report itself");
        }
        assert!(c.factor() < 0.9);
    }

    #[test]
    fn clean_reports_climb_the_factor_back_after_the_clean_run() {
        let mut c = Congestion::default();
        c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0);
        c.on_report(&rep(2, 6_000, 20_000, 20_000), 2.0);
        let low = c.factor();
        for k in 3..7 {
            assert_eq!(
                c.on_report(&rep(k, 500, 20_000, 20_000), k as f64).factor,
                low
            );
        }
        let s = c.on_report(&rep(7, 500, 20_000, 20_000), 7.0);
        assert!(
            (s.factor - (low + 0.05)).abs() < 1e-9,
            "the 5th clean report climbs"
        );
        for k in 8..40 {
            c.on_report(&rep(k, 500, 20_000, 20_000), k as f64);
        }
        assert_eq!(c.factor(), 1.0, "back to the full pace");
        // a report between the bounds holds and restarts the clean run
        let mut c = Congestion::default();
        c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0);
        for k in 2..20 {
            let s = c.on_report(&rep(k, 2_500, 20_000, 20_000), k as f64);
            assert_eq!(s.state, PathState::Hold);
        }
        assert!((c.factor() - 0.85).abs() < 1e-9);
    }

    #[test]
    fn one_report_is_judged_once_and_an_unmeasured_one_holds() {
        let mut c = Congestion::default();
        c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0);
        let s = c.on_report(&rep(1, 6_000, 20_000, 20_000), 2.0);
        assert!(!s.changed && !s.bpp_down);
        assert!((c.factor() - 0.85).abs() < 1e-9);
        // a version-1 receiver: no delay fields -- never clean, never over-used
        let s = c.on_report(
            &PathReport {
                seq: 2,
                ..PathReport::default()
            },
            3.0,
        );
        assert_eq!(s.state, PathState::Unknown);
        assert!((s.factor - 0.85).abs() < 1e-9);
    }

    #[test]
    fn states_print_as_short_tokens() {
        assert_eq!(PathState::Overuse.to_string(), "overuse");
        assert_eq!(PathState::Clean.to_string(), "clean");
        assert_eq!(PathState::Hold.to_string(), "hold");
        assert_eq!(PathState::Unknown.to_string(), "unknown");
    }

    /// The queue is the path delay above its quiet minimum: the clock offset and the sender's own
    /// delay (the capture phase sawtooth and its slot jumps) cancel.
    #[test]
    fn the_path_delay_cancels_the_senders_own_part_and_measures_the_queue() {
        let mut p = PathDelay::default();
        // a cambox phase sawtooth: the sender's own delay wanders 21..39 ms and jumps a slot back
        for k in 0..60i64 {
            let own = 21_000 + (k * 1_500) % 18_000;
            let rx = 3_000_000 + own + 400; // the receiver clock 3 s ahead, 0.4 ms of path
            assert!(p.update(rx, own).unwrap() <= 50, "report {k}");
        }
        assert_eq!(p.update(3_000_000 + 30_000 + 6_400, 30_000), Some(6_000));
        assert_eq!(p.update(3_000_000 + 25_000 + 8_400, 25_000), Some(8_000));
    }

    /// Review round 2: a standing queue that drains at once (the download stops) reads 0 in the
    /// very next report, and one that appears at once reads in full at once.
    #[test]
    fn a_standing_queue_that_drains_at_once_reads_no_queue_in_the_next_report() {
        let mut p = PathDelay::default();
        for _ in 0..10 {
            p.update(3_040_000, 40_000);
        }
        for k in 1..=8 {
            assert_eq!(
                p.update(3_040_000 + k * 1_000, 40_000),
                Some(k as u32 * 1_000)
            );
        }
        for _ in 0..10 {
            assert_eq!(p.update(3_048_000, 40_000), Some(8_000));
        }
        assert_eq!(
            p.update(3_040_000, 40_000),
            Some(0),
            "drained: no phantom queue"
        );
        assert_eq!(p.update(3_040_000, 40_000), Some(0));
        assert_eq!(
            p.update(3_047_000, 40_000),
            Some(7_000),
            "an abrupt onset is not hidden"
        );
    }

    #[test]
    fn a_big_clock_step_starts_over_and_the_old_base_ages_out() {
        let mut p = PathDelay::default();
        p.update(3_040_000, 40_000);
        // the nightly date step (+200 ms) no WallSteps saw: start over
        assert_eq!(p.update(3_240_000, 40_000), None);
        assert_eq!(p.update(3_240_000, 40_000), Some(0));
        // a raised floor older than the window becomes the new base
        let mut p = PathDelay::default();
        p.update(3_040_000, 40_000);
        for _ in 0..BASE_REPORTS {
            p.update(3_045_000, 40_000);
        }
        assert_eq!(p.update(3_045_000, 40_000), Some(0));
    }

    /// Review round 3: a report whose queuing delay the sender withheld (its own wall step) during
    /// a standing queue never raises the factor, however many come, and never restarts the clean
    /// run either.
    #[test]
    fn an_unjudged_report_holds_and_never_counts_as_clean() {
        let mut c = Congestion::default();
        c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0);
        let lowered = c.factor();
        assert!(lowered < 1.0);
        for k in 2..20 {
            let s = c.on_report(
                &PathReport {
                    queue_delay_us: None,
                    delay_unjudged: true,
                    ..rep(k, 0, 20_000, 20_000)
                },
                k as f64,
            );
            assert_eq!(s.state, PathState::Hold, "report {k}");
            assert!(!s.changed);
        }
        assert!((c.factor() - lowered).abs() < 1e-9, "never raised");
        // the clean run counts only judged clean reports
        let cfg = CongestionConfig::default();
        for k in 0..cfg.clean_before_increase - 1 {
            c.on_report(&rep(100 + k, 0, 20_000, 20_000), 100.0 + k as f64);
            c.on_report(
                &PathReport {
                    queue_delay_us: None,
                    delay_unjudged: true,
                    ..rep(200 + k, 0, 20_000, 20_000)
                },
                200.0 + k as f64,
            );
        }
        assert!((c.factor() - lowered).abs() < 1e-9, "not yet");
        c.on_report(&rep(300, 0, 20_000, 20_000), 300.0);
        assert!(
            c.factor() > lowered,
            "the last judged clean report completes the run"
        );
    }

    /// Review round 4 (a nit): an unjudged report in the middle of a real delay overuse run leaves
    /// the over-use streak where it was, so a `bpp_after_overuse` of 2 still asks at the next
    /// over-used report.
    #[test]
    fn an_unjudged_report_keeps_the_overuse_streak() {
        let mut c = Congestion::new(CongestionConfig {
            bpp_after_overuse: 2,
            ..CongestionConfig::default()
        });
        assert!(!c.on_report(&rep(1, 6_000, 20_000, 20_000), 1.0).bpp_down);
        let s = c.on_report(
            &PathReport {
                queue_delay_us: None,
                delay_unjudged: true,
                ..rep(2, 0, 20_000, 20_000)
            },
            2.0,
        );
        assert_eq!(s.state, PathState::Hold);
        assert!(
            c.on_report(&rep(3, 6_000, 20_000, 20_000), 3.0).bpp_down,
            "the second over-used report in the run asks"
        );
    }

    /// Review of the live fix: the base ages by TIME. A level left for a while comes back to a
    /// base it learnt then; when the offset has since stepped up (the program's NDI frame grows
    /// from black to cameras), a count-aged base, refreshed only by that level's own rare visits,
    /// reads overuse at every return for hours. After 30 s without a report at a level its base
    /// is learnt anew (that report recorded, not judged).
    #[test]
    fn a_base_older_than_its_window_is_learnt_anew() {
        let mut c = Congestion::default();
        let at = |seq, rx, tx| PathReport {
            stretch_regime: Some(4),
            ..rep(seq, 0, rx, tx)
        };
        for k in 1..31 {
            c.on_report(&at(k, 10_000, 10_000), k as f64);
        }
        // the sender left 4 bpp at 31 s and returns at 200 s: the offset is now 2.5 ms
        for k in 0..30u32 {
            let s = c.on_report(&at(200 + k, 12_500, 10_000), 200.0 + k as f64);
            assert_ne!(s.state, PathState::Overuse, "report at {} s", 200 + k);
            assert!(!s.bpp_down);
        }
    }

    /// The base is kept per regime (bpp): at 4 bpp an encoder-bound send arrives as sent, at 8 bpp
    /// a wire-bound one arrives 3.5 ms later (the per-frame burst lands inside it). Alternating
    /// between the two levels is never overuse; one shared base (the 4-bpp minimum) would read
    /// every 8-bpp report as a 3.5 ms growth.
    #[test]
    fn each_bpp_level_keeps_its_own_stretch_base() {
        let mut c = Congestion::default();
        let at = |seq: u32, bpp: u32, rx, tx| PathReport {
            stretch_regime: Some(bpp),
            ..rep(seq, 0, rx, tx)
        };
        for k in 1..40u32 {
            let r = if k % 10 < 5 {
                at(k, 4, 11_800, 11_800)
            } else {
                at(k, 8, 24_500, 21_000)
            };
            let s = c.on_report(&r, k as f64);
            assert_ne!(s.state, PathState::Overuse, "report {k}");
        }
    }
}
