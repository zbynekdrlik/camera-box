//! Issue 1387 item 2 -- the bottleneck bench of the JPEG XS link's congestion control (test only).
//!
//! The owner's scenario (29.10.2026): someone starts a big download on the stream PC, its receive
//! link saturates, and the JPEG XS program link from strih must keep its picture, degrade
//! gracefully and climb back afterwards.
//!
//! The bench runs the REAL decision code on both ends -- the receiver's `CongestionMeter` +
//! `ReportWindow`, the sender's `PaceControl` (link pace + `Congestion`) and `AutoBpp` -- over a
//! modelled path:
//! - **Two clocks.** The receiver's clock runs 3 s ahead of the sender's and 20 ppm fast; the
//!   genlock timecode is the sender's.
//! - **The sender** (10 GbE) encodes a 1080p frame every 1/30 s at the auto bpp, its packets
//!   leaving over 10 ms of encode, each send held to the pace the controller decides.
//! - **The bottleneck** is the receiver's 1 GbE switch port: a drop-tail FIFO of 1 MB served at
//!   1000 Mb/s, shared with CROSS traffic (the download) that ramps from 0 to 600 Mb/s at
//!   60..65 s, holds to 120 s and stops. Packets are modelled in groups of 8 datagrams.
//!
//! What must hold: the first congestion step is taken BEFORE the first JPEG XS datagram is
//! dropped, no frame loses its picture while the download runs, the losses stop within seconds of
//! the saturation, and the link climbs back to 8 bpp and the full pace after the download.

use crate::jxs_auto_bpp::{AutoBpp, AutoBppConfig, Decision, Second, StepReason};
use crate::jxs_link::congestion_meter::CongestionMeter;
use crate::jxs_link::pace_auto::PaceSource;
use crate::jxs_link::{ReportWindow, SEGMENT_DATA_LEN};
use crate::jxs_send::PaceControl;

/// One modelled send: this many datagrams back to back.
const GROUP: usize = 8;
/// A full datagram on the wire (payload + headers + Ethernet), bytes.
const DGRAM_WIRE: f64 = 1538.0;
const FRAME_US: f64 = 1_000_000.0 / 30.0;
const PORT_BYTES_PER_US: f64 = 1000.0 / 8.0;
const PORT_BUFFER_BYTES: f64 = 1_000_000.0;
const SENDER_LINK_MBPS: f64 = 10_000.0;
const RX_OFFSET_US: f64 = 3_000_000.0;
const RX_PPM: f64 = 20e-6;
const TC0_100NS: u64 = 17_617_300_000_000_000;

fn cross_mbps(t_s: f64) -> f64 {
    match t_s {
        t if t < 60.0 => 0.0,
        t if t < 65.0 => 600.0 * (t - 60.0) / 5.0,
        t if t < 120.0 => 600.0,
        _ => 0.0,
    }
}

/// The drop-tail port: a fluid backlog in bytes.
struct Port {
    backlog: f64,
    at_us: f64,
}

impl Port {
    /// A send of `bytes` arriving at `t`; `None` = dropped, else the time its last byte left.
    fn push(&mut self, t: f64, bytes: f64) -> Option<f64> {
        if t > self.at_us {
            self.backlog = (self.backlog - (t - self.at_us) * PORT_BYTES_PER_US).max(0.0);
            self.at_us = t;
        }
        if self.backlog + bytes > PORT_BUFFER_BYTES {
            return None;
        }
        self.backlog += bytes;
        Some(self.at_us + self.backlog / PORT_BYTES_PER_US)
    }
}

#[derive(Debug, Default)]
struct Outcome {
    bpp_at_60s: u32,
    first_congestion_step_s: Option<f64>,
    first_jxs_drop_s: Option<f64>,
    /// Frames with any lost datagram while the download ran (60..120 s) and after 70 s.
    lossy_frames_download: u32,
    lossy_frames_after_70s: u32,
    /// The largest share of one frame's datagrams lost.
    worst_frame_loss: f64,
    min_bpp_download: u32,
    bpp_at_end: u32,
    pace_at_end: (u32, Option<PaceSource>),
    cross_dropped_bytes: f64,
}

fn run(congestion_control: bool, end_s: f64) -> Outcome {
    let mut out = Outcome {
        min_bpp_download: u32::MAX,
        ..Outcome::default()
    };
    let mut auto = AutoBpp::new(AutoBppConfig::new(4, 8));
    let mut pace = PaceControl::new(0, None);
    pace.set_own_link(Some(SENDER_LINK_MBPS as u32));
    let mut bpp = 4u32;
    auto.applied(bpp, TC0_100NS);
    let mut pace_mbps = 0u32;
    let mut port = Port {
        backlog: 0.0,
        at_us: 0.0,
    };
    let mut meter = CongestionMeter::default();
    let mut window = ReportWindow::default();
    let mut next_cross_us = 0.0f64;
    let mut sender_free_us = 0.0f64;
    let frames = (end_s * 30.0) as u64;
    for k in 0..frames {
        let t0 = k as f64 * FRAME_US;
        let t_s = t0 / 1e6;
        let tc = TC0_100NS + (t0 * 10.0) as u64;
        if auto.target() != bpp {
            bpp = auto.target();
            auto.applied(bpp, tc);
        }
        pace.on_frames(1920, 1080, bpp);
        if let Some(d) = pace.changed() {
            pace_mbps = d.mbps;
        }
        let rate = if pace_mbps > 0 {
            pace_mbps as f64
        } else {
            SENDER_LINK_MBPS
        };
        let code = 1920.0 * 1080.0 * bpp as f64 / 8.0;
        let dgrams = (code / SEGMENT_DATA_LEN as f64).ceil() as usize;
        let groups = dgrams.div_ceil(GROUP);
        // our sends, then the cross traffic of the same span, through the port in time order
        let mut events: Vec<(f64, bool)> = Vec::with_capacity(groups + 64);
        let mut first_send = None;
        let mut last_send = 0.0;
        for g in 0..groups {
            let produced = t0 + 2_000.0 + g as f64 * 10_000.0 / groups as f64;
            let at = produced.max(sender_free_us);
            sender_free_us = at + (GROUP as f64 * DGRAM_WIRE * 8.0) / rate;
            first_send.get_or_insert(at);
            last_send = at;
            events.push((at, true));
        }
        let span_end = t0 + FRAME_US;
        while next_cross_us < span_end {
            let c = cross_mbps(next_cross_us / 1e6);
            if c < 1.0 {
                // (under 1 Mb/s the next packet would land seconds away: none this frame)
                next_cross_us = span_end;
                break;
            }
            events.push((next_cross_us, false));
            next_cross_us += (GROUP as f64 * DGRAM_WIRE * 8.0) / c;
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut first_rx, mut last_rx, mut got) = (None::<f64>, 0.0f64, 0usize);
        for (at, ours) in events {
            match port.push(at, GROUP as f64 * DGRAM_WIRE) {
                Some(left) if ours => {
                    let rx = (left + 50.0) * (1.0 + RX_PPM) + RX_OFFSET_US;
                    first_rx.get_or_insert(rx);
                    last_rx = rx;
                    got += GROUP;
                }
                Some(_) => {}
                None if ours => {
                    out.first_jxs_drop_s.get_or_insert(at / 1e6);
                }
                None => out.cross_dropped_bytes += GROUP as f64 * DGRAM_WIRE,
            }
        }
        pace.on_wire((last_send - first_send.unwrap_or(last_send)) / 1000.0);
        let sent = groups * GROUP;
        let lost = sent.saturating_sub(got);
        let to_100ns = |us: f64| (us * 10.0) as i64;
        meter.note_frame(
            tc,
            first_rx.map(to_100ns).unwrap_or(0),
            to_100ns(last_rx),
            lost as u32,
        );
        window.note_frame(lost as u32, tc, false, first_rx.map(|_| 500));
        if (60.0..120.0).contains(&t_s) {
            out.min_bpp_download = out.min_bpp_download.min(bpp);
            if lost > 0 {
                out.lossy_frames_download += 1;
            }
        }
        if t_s >= 70.0 && lost > 0 {
            out.lossy_frames_after_70s += 1;
        }
        out.worst_frame_loss = out.worst_frame_loss.max(lost as f64 / sent as f64);
        if k == 60 * 30 {
            out.bpp_at_60s = bpp;
        }
        // once a second: the receiver's report, then the sender's second
        if k % 30 == 29 {
            let now_s = (t0 + FRAME_US) / 1e6;
            let mut r = window.take(100, 0);
            r.link_mbps = 1000;
            if congestion_control {
                let c = meter.take();
                r.queue_delay_us = c.queue_delay_us;
                r.span_p50_us = c.span_p50_us;
            }
            pace.on_report(&r, now_s);
            auto.note_report(r, now_s);
            let second = Second {
                frames: 30,
                encode_p99_ms: 10.0,
                dropped_busy: 0,
                encode_errors: 0,
                congested: pace.take_bpp_down(),
            };
            if let Decision::Step { to, reason, .. } = auto.tick(&second, now_s) {
                if reason == StepReason::Congestion && to < bpp {
                    out.first_congestion_step_s.get_or_insert(now_s);
                }
            }
        }
    }
    out.bpp_at_end = bpp;
    let d = pace.decision();
    out.pace_at_end = (d.mbps, Some(d.source));
    out
}

#[test]
fn the_link_survives_a_saturating_download_and_climbs_back() {
    let o = run(true, 420.0);
    eprintln!("{o:?}");
    assert_eq!(o.bpp_at_60s, 8, "the quiet link climbs to 8 bpp first");
    assert!(
        o.cross_dropped_bytes > 0.0,
        "the download really saturates the port"
    );
    let step = o
        .first_congestion_step_s
        .expect("the queue growth stepped the bpp down");
    if let Some(drop) = o.first_jxs_drop_s {
        assert!(
            step <= drop,
            "the first step ({step:.1} s) comes before the first lost datagram ({drop:.1} s)"
        );
    }
    assert!(
        o.worst_frame_loss < 0.25,
        "no frame loses its picture: worst {:.3}",
        o.worst_frame_loss
    );
    assert!(
        o.lossy_frames_download <= 10,
        "a handful of lossy frames while it adapted: {}",
        o.lossy_frames_download
    );
    assert!(
        o.lossy_frames_after_70s == 0,
        "losses stop once adapted: {} lossy frames after 70 s",
        o.lossy_frames_after_70s
    );
    assert!(o.min_bpp_download < 8, "it degraded the bpp, gracefully");
    assert_eq!(o.bpp_at_end, 8, "back to 8 bpp after the download");
    assert_eq!(
        o.pace_at_end,
        (900, Some(PaceSource::ReceiverLink)),
        "back to the full pace"
    );
}

/// The control the bench measures against: the same path with the delay signal withheld (a
/// version-1 receiver) leaves only the loss reports, and the link loses datagrams first.
#[test]
fn without_the_delay_signal_the_same_download_costs_lost_datagrams_first() {
    let o = run(false, 130.0);
    eprintln!("{o:?}");
    assert!(o.first_congestion_step_s.is_none());
    assert!(o.first_jxs_drop_s.is_some(), "loss is the only signal left");
    assert!(
        o.lossy_frames_download >= 50,
        "loss-driven alone loses far more: {}",
        o.lossy_frames_download
    );
}
