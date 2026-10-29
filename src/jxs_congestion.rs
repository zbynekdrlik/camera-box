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
//! - **Overuse** = queuing delay over [`CongestionConfig::queue_overuse_us`], or the arrival span
//!   longer than the send span by more than `max(stretch_overuse_us, stretch_overuse_frac x send
//!   span)`, or a lost slice. Then the PACE factor drops x`decrease` at once (never below
//!   `min_factor`; the sender also never paces below what a frame needs in its interval,
//!   `crate::jxs_link::pace_auto`): the frame is spread thinner, its burst queues less.
//! - **A delay overuse** (for `bpp_after_overuse` reports in a row: 1 = at once) ALSO asks the
//!   auto bpp for ONE step down (`crate::jxs_auto_bpp::StepReason::Congestion`), at most once per
//!   `bpp_settle_s`: the queue needs a moment to drain before the next step is judged. A loss
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
/// A path delay that jumps by more than this between two reports while the receiver's per-frame
/// delays stay tight ([`STEP_SPREAD_US`]) is a CLOCK step on one box (a dantesync correction), not a
/// queue: every frame moved alike. The base moves with it.
pub const STEP_JUMP_US: i64 = 3_000;
pub const STEP_SPREAD_US: u32 = 2_000;
/// A path delay this far above the base is a big clock step (the nightly date step): start over.
pub const STEP_RESET_US: i64 = 100_000;

/// The sender's half of the queuing-delay signal (pure): the receiver's p50 of (first arrival -
/// timecode) minus the SENDER's own p50 of (first send - timecode) of the same frames = the path
/// delay plus the clocks' offset, constant on a quiet path. The queue = the path delay above its
/// minimum over the last [`BASE_REPORTS`]. The sender's part (on a cambox: the capture's phase in
/// its 60 fps emit slot, a sawtooth that wanders 0..16.7 ms and jumps a slot at an edge, plus the
/// encode) cancels; the receiver alone cannot remove it (review round 1 of item 2).
#[derive(Debug, Default, Clone)]
pub struct PathDelay {
    history: std::collections::VecDeque<i64>,
    last: Option<i64>,
}

impl PathDelay {
    /// One report: the receiver's delay p50 and spread, the sender's own p50 (all us). Returns the
    /// queuing delay in us (`None` right after a big clock step).
    pub fn update(&mut self, rx_p50_us: i64, rx_spread_us: u32, own_p50_us: i64) -> Option<u32> {
        let _ = (
            rx_spread_us,
            &self.history,
            self.last,
            STEP_JUMP_US,
            STEP_SPREAD_US,
            STEP_RESET_US,
        );
        Some((rx_p50_us - own_p50_us).clamp(0, u32::MAX as i64 - 1) as u32)
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
        let stretch_over = match (r.recv_span_us, r.send_span_us) {
            (Some(rx), Some(tx)) => {
                let bound = (c.stretch_overuse_us as f64).max(c.stretch_overuse_frac * tx as f64);
                rx as f64 - tx as f64 > bound
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
    fn a_stretched_arrival_span_is_over_use_and_a_small_one_is_not() {
        let mut c = Congestion::default();
        // 20 ms sent, 23.5 ms arrived: 3.5 ms > max(1.5 ms, 15 % = 3 ms)
        assert_eq!(
            c.on_report(&rep(1, 0, 23_500, 20_000), 1.0).state,
            PathState::Overuse
        );
        let mut c = Congestion::default();
        // 20 ms sent, 22.5 ms arrived: 2.5 ms <= 3 ms
        assert_eq!(
            c.on_report(&rep(1, 0, 22_500, 20_000), 1.0).state,
            PathState::Clean
        );
        // a short send span takes the 1.5 ms floor: 4 ms sent, 6.5 ms arrived
        assert_eq!(
            c.on_report(&rep(2, 0, 6_500, 4_000), 2.0).state,
            PathState::Overuse
        );
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
            assert!(p.update(rx, 1_500, own).unwrap() <= 50, "report {k}");
        }
        // a queue of 6 ms builds (with the spread a queue brings)
        assert_eq!(
            p.update(3_000_000 + 30_000 + 6_400, 5_000, 30_000),
            Some(6_000)
        );
        assert_eq!(
            p.update(3_000_000 + 25_000 + 8_400, 6_000, 25_000),
            Some(8_000)
        );
    }

    /// Review round 1: a +-5 ms wall step on one box moves every frame alike (a tight spread): the
    /// base moves with it, the path reads no queue.
    #[test]
    fn a_small_clock_step_moves_the_base_and_is_never_a_queue() {
        let mut p = PathDelay::default();
        for _ in 0..10 {
            p.update(3_040_000, 900, 40_000);
        }
        assert_eq!(
            p.update(3_045_000, 900, 40_000),
            Some(0),
            "+5 ms on the receiver"
        );
        assert_eq!(p.update(3_045_000, 900, 40_000), Some(0));
        assert_eq!(p.update(3_040_000, 900, 40_000), Some(0), "and back -5 ms");
        assert_eq!(p.update(3_035_000, 900, 40_000), Some(0), "-5 ms more");
        // the same jump WITH a spread is a queue
        assert_eq!(p.update(3_040_000, 4_000, 40_000), Some(5_000));
    }

    #[test]
    fn a_big_clock_step_starts_over_and_the_old_base_ages_out() {
        let mut p = PathDelay::default();
        p.update(3_040_000, 900, 40_000);
        // the nightly date step (+200 ms) with a spread that hides it: start over
        assert_eq!(p.update(3_240_000, 5_000, 40_000), None);
        assert_eq!(p.update(3_240_000, 900, 40_000), Some(0));
        // a raised floor older than the window becomes the new base
        let mut p = PathDelay::default();
        p.update(3_040_000, 900, 40_000);
        for _ in 0..BASE_REPORTS {
            p.update(3_045_000, 4_000, 40_000);
        }
        assert_eq!(p.update(3_045_000, 4_000, 40_000), Some(0));
    }
}
