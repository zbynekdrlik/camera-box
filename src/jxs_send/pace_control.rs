//! Issue 1387: the packet thread's automatic pace -- what it knows about both links, the
//! decision (`jxs_link::pace_auto::decide`, pure) and the one log line per change -- and the
//! delay-based congestion control of the path (`crate::jxs_congestion`), fed the receiver's
//! queuing delay + arrival span and this sender's own send spans: it lowers the pace factor and
//! asks the auto bpp for a step down before the path loses anything.
//!
//! The receiver's link comes from its report (version 2, `link_mbps`); the sender's own link is
//! re-read from sysfs once a second ([`PaceControl::refresh_own_link`]), so a port renegotiated to
//! another speed, or a cable moved to another NIC, takes effect within a second with no setting.

use crate::jxs_congestion::{Congestion, PathDelay, PathReport, PathState, StretchMode};
use crate::jxs_link::pace_auto::{self, PaceDecision, PaceInputs};
use crate::jxs_link::ReceiverReport;

/// Send spans kept for the path report (one second of frames at 30 fps).
const SEND_SPANS: usize = 30;
/// Own first-send delays kept (three seconds of frames): a report covers the second before it.
const OWN_DELAYS: usize = 90;
/// The sender's own wall steps (`WallSteps::steps_100ns`, read with each first send) moving by
/// more than this between two frames = its wall stepped under its timecode (the nightly fleet date
/// step, or a smaller dantesync step): a new step epoch. `CLOCK_REALTIME - CLOCK_MONOTONIC` also
/// moves by the slew (ppm) and by read jitter, both far below 1 ms between two frames. 100 ns.
const OWN_STEP_100NS: i64 = 10_000;

/// The sender's own delay over a report's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnMean {
    /// No own first send in the window.
    Unknown,
    /// The window spans an own wall step (or follows one): not judged.
    Unjudged,
    /// The mean (us).
    Judged(i64),
}

/// The pace the sending thread runs, and why.
#[derive(Debug, Clone)]
pub struct PaceControl {
    /// The explicit setting (0 = automatic).
    setting_mbps: u32,
    /// The interface the sender's socket sends on (`None` = unknown).
    own_iface: Option<String>,
    own_link_mbps: Option<u32>,
    receiver_link_mbps: Option<u32>,
    /// The geometry and bpp of the frames being sent (the safe start's frame spread).
    width: u32,
    height: u32,
    bpp: u32,
    fps: u32,
    /// The congestion controller's factor in percent (100 = none).
    congestion_pct: u32,
    current: Option<PaceDecision>,
    /// The delay-based congestion control and what it is fed.
    congestion: Congestion,
    send_spans_us: std::collections::VecDeque<u32>,
    /// The newest queuing delay (us): the receiver's delay minus this sender's own ([`PathDelay`]).
    queue_delay_us: Option<u32>,
    path_delay: PathDelay,
    /// Per frame: its timecode, this sender's own delay (first send - timecode, both 100 ns) and
    /// the step epoch it was sent in.
    own_delays: std::collections::VecDeque<(u64, i64, u64)>,
    /// Bumped at every own step ([`OWN_STEP_100NS`]).
    epoch: u64,
    /// The own wall steps read with the previous first send (100 ns).
    last_steps_100ns: Option<i64>,
    /// The seq of the last report handled (a duplicated datagram is handled once).
    last_report_seq: Option<u32>,
    /// The epoch of the previous report's own window (`None` = it was not one epoch).
    last_report_epoch: Option<u64>,
    /// The controller asked for a bpp step down not yet handed to the auto bpp.
    bpp_down: bool,
}

impl PaceControl {
    /// `setting_mbps` 0 = automatic; `own_iface` = the socket's interface
    /// (`JxsSocket::local_iface`), read for its speed at once.
    pub fn new(setting_mbps: u32, own_iface: Option<String>) -> Self {
        let mut p = Self {
            setting_mbps,
            own_iface,
            own_link_mbps: None,
            receiver_link_mbps: None,
            width: 0,
            height: 0,
            bpp: 0,
            fps: crate::jxs_link::JXS_FPS as u32,
            congestion_pct: 100,
            current: None,
            congestion: Congestion::default(),
            send_spans_us: std::collections::VecDeque::with_capacity(SEND_SPANS),
            queue_delay_us: None,
            path_delay: PathDelay::default(),
            own_delays: std::collections::VecDeque::with_capacity(OWN_DELAYS),
            epoch: 0,
            last_steps_100ns: None,
            last_report_seq: None,
            last_report_epoch: None,
            bpp_down: false,
        };
        p.refresh_own_link();
        p
    }

    /// Re-read the own link's speed (sysfs, once a second from the packet thread).
    pub fn refresh_own_link(&mut self) {
        self.own_link_mbps = self
            .own_iface
            .as_deref()
            .and_then(crate::jxs_link::link_speed::link_speed_mbps);
    }

    /// The own link as given (tests; a stand-in for the sysfs read).
    pub fn set_own_link(&mut self, mbps: Option<u32>) {
        self.own_link_mbps = mbps;
    }

    /// A receiver report at `now_s` (the caller's seconds): a version-2 report names its link (0 =
    /// it does not know it: the safe start again; a version-1 report never does) and its delay
    /// signal, which the congestion control judges against this sender's recent send spans.
    pub fn on_report(&mut self, r: &ReceiverReport, now_s: f64) {
        if self.last_report_seq == Some(r.seq) {
            // a duplicated datagram: judged once (review round 3)
            return;
        }
        self.last_report_seq = Some(r.seq);
        self.receiver_link_mbps = (r.link_mbps > 0).then_some(r.link_mbps);
        let own = self.own_delay_mean_us(r);
        let delay_unjudged = r.delay_mean_us.is_some() && own == OwnMean::Unjudged;
        self.queue_delay_us = match (r.delay_mean_us, own) {
            (Some(rx), OwnMean::Judged(own)) => self.path_delay.update(rx as i64, own),
            _ => None,
        };
        let mut spans: Vec<u32> = self.send_spans_us.iter().copied().collect();
        spans.sort_unstable();
        // the send span is when send() returned; the wire may be slower: a sender on its own
        // slower NIC (a 1 GbE cambox at 8 bpp: ~18 ms of wire for a ~14 ms encode) is never read as
        // a stretched path (review round 1)
        let wire_floor_us = self.own_link_mbps.filter(|m| *m > 0).map(|m| {
            (pace_auto::frame_wire_bytes(self.width, self.height, self.bpp) * 8 / m as u64) as u32
        });
        // a sender at LINE RATE has its own NIC as the bottleneck by construction (and on a cambox
        // NDI goes out ahead of JPEG XS in the egress tree's priority band): no stretch is judged
        // there, the queuing delay still covers the path (review round 2)
        let line_rate = self.current.is_some_and(|d| d.mbps == 0);
        let send_span_us = (!spans.is_empty() && !line_rate)
            .then(|| spans[(spans.len() - 1) / 2].max(wire_floor_us.unwrap_or(0)));
        let before = self.congestion.state();
        let step = self.congestion.on_report(
            &PathReport {
                seq: r.seq,
                queue_delay_us: self.queue_delay_us,
                recv_span_us: r.span_p50_us,
                send_span_us,
                lost_slices: r.lost_slices,
                delay_unjudged,
                // the stretch's base per bpp; the safe start's frame-spread pace is not the
                // steady regime (live 30.10.2026: its spans set a base the receiver-link pace
                // never reaches again)
                stretch_regime: Some(self.bpp),
                stretch_mode: if self
                    .current
                    .is_none_or(|d| d.source == pace_auto::PaceSource::StartSpread)
                {
                    StretchMode::Skip
                } else {
                    StretchMode::Base
                },
            },
            now_s,
        );
        self.bpp_down |= step.bpp_down;
        self.congestion_pct = self.congestion.factor_pct();
        if step.changed || (step.state != before && step.state == PathState::Overuse) {
            tracing::info!(
                "jxs send: path {} (queue_ms={} span_rx/tx_ms={}/{} lost_slices={}) -- pace factor {} %{} (issue 1387)",
                step.state,
                self.queue_delay_us.map_or("-".to_string(), |q| format!("{:.1}", q as f64 / 1000.0)),
                r.span_p50_us.map_or("-".to_string(), |q| format!("{:.1}", q as f64 / 1000.0)),
                send_span_us.map_or("-".to_string(), |q| format!("{:.1}", q as f64 / 1000.0)),
                r.lost_slices,
                self.congestion_pct,
                if step.bpp_down { ", asks the auto bpp for a step down" } else { "" },
            );
        }
    }

    /// This sender's own MEAN delay (us) over the frames the report's second covers: timecodes in
    /// (newest - 1 s, newest] (about 30 frames, the receiver's set of the period give or take a
    /// frame: it finalizes a frame one frame late, which moves a mean by that frame over 30).
    /// `Unknown` = none of them known; `Unjudged` = the window is not the SAME step epoch as the
    /// previous report's (review round 3): across a sender wall step the two ends' frame sets
    /// differ by up to the step's worth of frames, each of them carrying the step, so the means
    /// disagree by up to the step; that report and the next one are not judged (the congestion
    /// control holds, the path delay's history is not touched).
    fn own_delay_mean_us(&mut self, r: &ReceiverReport) -> OwnMean {
        let newest = r.last_timecode_100ns;
        let window = || {
            self.own_delays
                .iter()
                .filter(move |(tc, _, _)| *tc <= newest && *tc + 10_000_000 > newest)
        };
        let first = window().next().map(|(_, _, e)| *e);
        let one_epoch = first.filter(|e| window().all(|(_, _, x)| x == e));
        let judged = one_epoch.is_some() && one_epoch == self.last_report_epoch;
        self.last_report_epoch = one_epoch;
        if first.is_none() {
            return OwnMean::Unknown;
        }
        if !judged {
            return OwnMean::Unjudged;
        }
        let (sum, n) = window().fold((0i128, 0i128), |(s, n), (_, d, _)| (s + *d as i128, n + 1));
        OwnMean::Judged((sum / n.max(1) / 10) as i64)
    }

    /// A frame's first send: its timecode, the wall clock minus its own steps and those steps
    /// (`WallSteps::steps_100ns`, read once: wall = `realtime_100ns() - steps`), all 100 ns.
    pub fn on_first_send(&mut self, timecode_100ns: u64, wall_100ns: i64, steps_100ns: i64) {
        if timecode_100ns == 0 {
            return;
        }
        let delay = wall_100ns - timecode_100ns.min(i64::MAX as u64) as i64;
        if let Some(prev) = self.last_steps_100ns {
            if (steps_100ns - prev).abs() > OWN_STEP_100NS {
                self.epoch += 1;
            }
        }
        self.last_steps_100ns = Some(steps_100ns);
        if self.own_delays.len() == OWN_DELAYS {
            self.own_delays.pop_front();
        }
        self.own_delays
            .push_back((timecode_100ns, delay, self.epoch));
    }

    /// One whole frame's send span (first send -> last send, ms).
    pub fn on_wire(&mut self, wire_ms: f64) {
        if self.send_spans_us.len() == SEND_SPANS {
            self.send_spans_us.pop_front();
        }
        self.send_spans_us
            .push_back((wire_ms * 1000.0).clamp(0.0, u32::MAX as f64) as u32);
    }

    /// Whether the congestion control asked for a bpp step down since the last call (once).
    pub fn take_bpp_down(&mut self) -> bool {
        std::mem::take(&mut self.bpp_down)
    }

    /// The congestion controller's pace factor (`crate::jxs_congestion`), in percent.
    pub fn set_congestion_pct(&mut self, pct: u32) {
        self.congestion_pct = pct;
    }

    /// The frames now being sent.
    pub fn on_frames(&mut self, width: u32, height: u32, bpp: u32) {
        self.width = width;
        self.height = height;
        self.bpp = bpp;
    }

    /// The decision for what is known now.
    pub fn decision(&self) -> PaceDecision {
        pace_auto::decide(&PaceInputs {
            setting_mbps: self.setting_mbps,
            own_link_mbps: self.own_link_mbps,
            receiver_link_mbps: self.receiver_link_mbps,
            frame_wire_bytes: pace_auto::frame_wire_bytes(self.width, self.height, self.bpp),
            fps: self.fps,
            congestion_pct: self.congestion_pct,
        })
    }

    /// The decision when it differs from the one in force (the first call always answers),
    /// logged once; `None` = keep the running schedule.
    pub fn changed(&mut self) -> Option<PaceDecision> {
        let d = self.decision();
        if self.current == Some(d) {
            return None;
        }
        let from = self.current.map_or(0, |c| c.mbps);
        self.current = Some(d);
        tracing::info!(
            "jxs send: pace {from} -> {} Mb/s of wire ({}; receiver link {} Mb/s, own link {} Mb/s on {}; 0 = line rate / unknown) (issue 1387)",
            d.mbps,
            d.source,
            self.receiver_link_mbps.unwrap_or(0),
            self.own_link_mbps.unwrap_or(0),
            self.own_iface.as_deref().unwrap_or("-"),
        );
        Some(d)
    }

    /// The status line's pace fields.
    pub fn fill(&self, w: &mut crate::jxs_link::SendWindow) {
        let d = self.current.unwrap_or_default();
        w.pace_mbps = d.mbps;
        w.pace_source = d.source;
        w.rx_link_mbps = self.receiver_link_mbps.unwrap_or(0);
        w.own_link_mbps = self.own_link_mbps.unwrap_or(0);
        w.path_state = self.congestion.state();
        w.congestion_pct = self.congestion_pct;
        w.queue_delay_us = self.queue_delay_us;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jxs_link::pace_auto::PaceSource;

    impl PaceControl {
        /// A report with a fresh seq each time (the tests here judge the link, not the path).
        fn on_report_t(&mut self, r: &ReceiverReport) {
            let seq = rand_seq();
            self.on_report(&ReceiverReport { seq, ..*r }, 0.0);
        }
    }

    fn rand_seq() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(1000);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn report(link: u32) -> ReceiverReport {
        ReceiverReport {
            camera_id: 100,
            link_mbps: link,
            ..ReceiverReport::default()
        }
    }

    /// The strih output on 10 GbE to a stream box on 1 GbE: the safe start until the first
    /// report, then 900 from the report, line rate when the receiver moves to a faster port, and
    /// back to the safe start when a report says the link is unknown.
    #[test]
    fn the_pace_follows_the_receivers_report_and_changes_once_per_change() {
        let mut p = PaceControl::new(0, None);
        p.set_own_link(Some(10_000));
        p.on_frames(1920, 1080, 4);
        let start = p.changed().expect("the first decision");
        assert_eq!(start.source, PaceSource::StartSpread);
        assert_eq!(start.mbps, 579);
        assert_eq!(p.changed(), None, "nothing new: the schedule stays");
        p.on_report_t(&report(1000));
        assert_eq!(
            p.changed(),
            Some(PaceDecision {
                mbps: 900,
                source: PaceSource::ReceiverLink
            })
        );
        // a bpp step does not move a link-derived pace
        p.on_frames(1920, 1080, 8);
        assert_eq!(p.changed(), None);
        p.on_report_t(&report(10_000));
        assert_eq!(p.changed().map(|d| d.source), Some(PaceSource::LineRate));
        p.on_report_t(&report(0));
        let back = p.changed().unwrap();
        assert_eq!(back.source, PaceSource::StartSpread);
        assert_eq!(back.mbps, 1153, "8 bpp spread over half the frame");
        let mut w = crate::jxs_link::SendWindow::default();
        p.on_report_t(&report(2500));
        p.changed();
        p.fill(&mut w);
        assert_eq!(
            (w.pace_mbps, w.pace_source, w.rx_link_mbps, w.own_link_mbps),
            (2250, PaceSource::ReceiverLink, 2500, 10_000)
        );
    }

    /// A 1 GbE cambox to the 10 GbE strih: line rate as soon as the strih reports (the owner's
    /// camera choice), and an explicit setting overrides every report.
    #[test]
    fn a_cambox_runs_at_line_rate_and_a_setting_overrides_the_reports() {
        let mut p = PaceControl::new(0, None);
        p.set_own_link(Some(1000));
        p.on_frames(1920, 1080, 4);
        p.changed();
        p.on_report_t(&report(10_000));
        assert_eq!(
            p.changed(),
            Some(PaceDecision {
                mbps: 0,
                source: PaceSource::LineRate
            })
        );
        let mut fixed = PaceControl::new(700, None);
        fixed.on_report_t(&report(1000));
        assert_eq!(
            fixed.changed(),
            Some(PaceDecision {
                mbps: 700,
                source: PaceSource::Fixed
            })
        );
    }

    /// The own link is read from the interface the socket sends on: the loopback has no speed
    /// (unknown), so a receiver link paces whatever this box's NIC is.
    #[test]
    fn an_unknown_own_link_paces_to_the_receiver() {
        let lo = crate::jxs_link::link_speed::iface_name(1);
        let mut p = PaceControl::new(0, lo);
        assert_eq!(p.own_link_mbps, None, "the loopback reports no speed");
        p.on_report_t(&report(1000));
        assert_eq!(p.changed().map(|d| d.mbps), Some(900));
    }

    /// Issue 1387 item 2: a growing queue in the reports lowers the pace through the factor, asks
    /// the auto bpp for a step once, and clean reports climb it back.
    #[test]
    fn the_receivers_queue_lowers_the_pace_and_asks_the_auto_bpp_once() {
        let mut p = PaceControl::new(0, None);
        p.set_own_link(Some(10_000));
        p.on_frames(1920, 1080, 8);
        for _ in 0..30 {
            p.on_wire(19.7);
        }
        const TC: u64 = 17_617_300_000_000_000;
        // this sender's own delay: 2 ms after each timecode; the receiver's clock 1 s ahead
        let feed = |p: &mut PaceControl, seq: u32| {
            for k in 0..30u64 {
                let tc = TC + (seq as u64 * 30 + k) * 333_333;
                p.on_first_send(tc, tc as i64 + 20_000, 0);
            }
            TC + (seq as u64 * 30 + 29) * 333_333
        };
        let quiet_at = |seq, newest| ReceiverReport {
            seq,
            camera_id: 100,
            frames: 30,
            link_mbps: 1000,
            last_timecode_100ns: newest,
            delay_mean_us: Some(1_000_000 + 2_000 + 300),
            delay_spread_us: Some(500),
            span_p50_us: Some(19_900),
            ..ReceiverReport::default()
        };
        let newest = feed(&mut p, 0);
        let quiet = |seq| quiet_at(seq, newest);
        // the first report only notes the window's step epoch (review round 3): not judged
        p.on_report(&quiet(0), 0.5);
        assert_eq!(p.queue_delay_us, None);
        p.on_report(&quiet(1), 1.0);
        assert_eq!(p.changed().map(|d| d.mbps), Some(900));
        let queued = |seq| ReceiverReport {
            delay_mean_us: Some(1_000_000 + 2_000 + 7_300),
            delay_spread_us: Some(5_000),
            ..quiet(seq)
        };
        p.on_report(&queued(2), 2.0);
        assert!(
            p.take_bpp_down(),
            "a delay overuse asks the auto bpp at once"
        );
        assert!(!p.take_bpp_down(), "taken");
        assert_eq!(
            p.changed().map(|d| (d.mbps, d.source)),
            Some((765, pace_auto::PaceSource::Congestion))
        );
        p.on_report(&queued(3), 2.5);
        assert!(!p.take_bpp_down(), "not again within the settle time");
        let mut w = crate::jxs_link::SendWindow::default();
        p.fill(&mut w);
        assert_eq!(
            (w.path_state, w.congestion_pct, w.queue_delay_us),
            (PathState::Overuse, 72, Some(7_000))
        );
        for k in 4..60 {
            p.on_report(&quiet(k), k as f64);
        }
        assert_eq!(
            p.changed().map(|d| (d.mbps, d.source)),
            Some((900, pace_auto::PaceSource::ReceiverLink))
        );
        // a stretched arrival against this sender's own send span is overuse too
        p.on_report(
            &ReceiverReport {
                span_p50_us: Some(26_000),
                ..quiet(70)
            },
            70.0,
        );
        let mut w = crate::jxs_link::SendWindow::default();
        p.fill(&mut w);
        assert_eq!(w.path_state, PathState::Overuse);
    }

    /// Review round 1: a 1 GbE cambox at line rate whose NIC needs longer than its encoder (8 bpp:
    /// ~18 ms of wire for a ~14 ms encode) is not a stretched path, and its capture phase sawtooth
    /// (the sender's own delay wandering and jumping a slot) is not a queue: it stays clean, at line
    /// rate, at 8 bpp. Review round 2: the receiver's frame set is one frame off the sender's (it
    /// finalizes a frame one frame late) and the slot crossing lands at frame 15 of a report: a
    /// MEAN moves by one frame's difference over 30, a p50 would have moved by the whole slot.
    #[test]
    fn a_line_rate_cambox_with_a_phase_sawtooth_stays_clean() {
        let mut p = PaceControl::new(0, None);
        p.set_own_link(Some(1000));
        p.on_frames(1920, 1080, 8);
        const TC: u64 = 17_617_300_000_000_000;
        const FRAME: u64 = 333_333;
        // the capture phase in its 60 fps slot (100 ns): +40 us a frame, back a slot when it
        // crosses the edge -- at frame 15 of report 3 (j = 105), and again 416 frames later
        let phase = |j: i64| (((j - 105) * 400).rem_euclid(166_667)) + 20_000;
        let tc = |j: i64| TC + j as u64 * FRAME;
        let mut j = 0i64;
        for seq in 0..60u32 {
            let last = (seq as i64) * 30 + 29;
            while j <= last {
                p.on_first_send(tc(j), tc(j) as i64 + phase(j), 0);
                p.on_wire(13.8);
                j += 1;
            }
            // the receiver's set: frames last-30 ..= last-1 (it heard `last` but finalizes it next
            // period), each 3 ms of path later than the sender's own delay
            let rx_sum: i64 = (last - 30..last).map(|k| phase(k) + 30_000).sum();
            p.on_report(
                &ReceiverReport {
                    seq,
                    camera_id: 5,
                    frames: 30,
                    link_mbps: 10_000,
                    last_timecode_100ns: tc(last),
                    delay_mean_us: Some((rx_sum / 30 / 10) as i32),
                    delay_spread_us: Some(1_200),
                    // the receiver sees the wire span: 18 ms
                    span_p50_us: Some(18_100),
                    ..ReceiverReport::default()
                },
                seq as f64,
            );
            assert!(!p.take_bpp_down(), "report {seq}: never a congestion step");
            let mut w = crate::jxs_link::SendWindow::default();
            p.changed();
            p.fill(&mut w);
            assert_ne!(w.path_state, PathState::Overuse, "report {seq}");
            assert!(
                w.queue_delay_us.unwrap_or(0) < 1_000,
                "report {seq}: queue {:?}",
                w.queue_delay_us
            );
            assert_eq!(
                (w.pace_mbps, w.pace_source),
                (0, pace_auto::PaceSource::LineRate)
            );
        }
    }

    /// Review round 3: a small sender wall step (30 ms, under any delay-jump heuristic) mid-report,
    /// with the receiver's frame set one frame off the sender's window, never reads as a queue:
    /// the sender knows its own steps (`WallSteps`), so every step starts a new epoch. And a
    /// duplicated report datagram is handled once.
    #[test]
    fn a_small_own_wall_step_is_never_judged_and_a_duplicate_report_once() {
        let mut p = PaceControl::new(0, None);
        p.set_own_link(Some(10_000));
        p.on_frames(1920, 1080, 8);
        for _ in 0..30 {
            p.on_wire(19.7);
        }
        const TC: u64 = 17_617_300_000_000_000;
        const FRAME: i64 = 333_333;
        // the sender's wall steps +30 ms at frame 15 of report 3 (j = 105): its timecodes jump,
        // its own delay (wall minus steps) does not
        let step = |j: i64| if j >= 105 { 300_000 } else { 0 };
        let tc = |j: i64| (TC as i64 + j * FRAME + step(j)) as u64;
        let mut j = 0i64;
        let mut queued = Vec::new();
        for seq in 0..10u32 {
            let last = seq as i64 * 30 + 29;
            while j <= last {
                p.on_first_send(tc(j), tc(j) as i64 + 20_000, step(j));
                j += 1;
            }
            // the receiver: its frames last-30 ..= last-1, each 3 ms of path after the own delay;
            // the arrival is on ITS wall, which did not step: arrival - timecode moves with the step
            let rx_sum: i64 = (last - 30..last).map(|k| 20_000 + 30_000 - step(k)).sum();
            let r = ReceiverReport {
                seq,
                camera_id: 100,
                frames: 30,
                link_mbps: 1000,
                last_timecode_100ns: tc(last),
                delay_mean_us: Some((1_000_000 * 10 + rx_sum / 30) as i32 / 10),
                delay_spread_us: Some(500),
                span_p50_us: Some(19_900),
                ..ReceiverReport::default()
            };
            p.on_report(&r, seq as f64);
            queued.push(p.queue_delay_us);
            assert!(!p.take_bpp_down(), "report {seq}: never a congestion step");
            assert!(
                p.queue_delay_us.unwrap_or(0) < 1_000,
                "report {seq}: queue {:?}",
                p.queue_delay_us
            );
            if seq == 0 {
                // the duplicate of the first (unjudged) report stays unjudged
                p.on_report(&r, 0.5);
                assert_eq!(p.queue_delay_us, None, "a duplicate is handled once");
            }
        }
        // the step report (3) and the next one (4) are not judged, the rest are
        assert_eq!(queued[3], None);
        assert_eq!(queued[4], None);
        assert!(queued[6].is_some());
    }
}
