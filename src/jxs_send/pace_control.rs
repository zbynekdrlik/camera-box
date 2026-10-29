//! Issue 1387: the packet thread's automatic pace -- what it knows about both links, the
//! decision (`jxs_link::pace_auto::decide`, pure) and the one log line per change.
//!
//! The receiver's link comes from its report (version 2, `link_mbps`); the sender's own link is
//! re-read from sysfs once a second ([`PaceControl::refresh_own_link`]), so a port renegotiated to
//! another speed, or a cable moved to another NIC, takes effect within a second with no setting.

use crate::jxs_link::pace_auto::{self, PaceDecision, PaceInputs};
use crate::jxs_link::ReceiverReport;

/// The pace the sending thread runs, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    current: Option<PaceDecision>,
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
            current: None,
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

    /// A receiver report: a version-2 report names its link (0 = it does not know it: the safe
    /// start again); a version-1 report never does.
    pub fn on_report(&mut self, r: &ReceiverReport) {
        self.receiver_link_mbps = (r.link_mbps > 0).then_some(r.link_mbps);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jxs_link::pace_auto::PaceSource;

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
        assert_eq!(start.mbps, 526);
        assert_eq!(p.changed(), None, "nothing new: the schedule stays");
        p.on_report(&report(1000));
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
        p.on_report(&report(10_000));
        assert_eq!(p.changed().map(|d| d.source), Some(PaceSource::LineRate));
        p.on_report(&report(0));
        let back = p.changed().unwrap();
        assert_eq!(back.source, PaceSource::StartSpread);
        assert_eq!(back.mbps, 1052, "8 bpp spread over half the frame");
        let mut w = crate::jxs_link::SendWindow::default();
        p.on_report(&report(2500));
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
        p.on_report(&report(10_000));
        assert_eq!(
            p.changed(),
            Some(PaceDecision {
                mbps: 0,
                source: PaceSource::LineRate
            })
        );
        let mut fixed = PaceControl::new(700, None);
        fixed.on_report(&report(1000));
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
        p.on_report(&report(1000));
        assert_eq!(p.changed().map(|d| d.mbps), Some(900));
    }
}
