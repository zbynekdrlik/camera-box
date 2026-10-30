//! Issue 1387 item 2 -- the bottleneck bench of the JPEG XS link's congestion control (test only).
//!
//! The owner's scenario (29.10.2026): someone starts a big download on the stream PC, its receive
//! link saturates, and the JPEG XS program link from strih must keep its picture, degrade
//! gracefully and climb back afterwards.
//!
//! The bench runs the REAL decision code on both ends -- the receiver's `CongestionMeter` +
//! `ReportWindow`, the sender's `PaceControl` (link pace + `PathDelay` + `Congestion`) and
//! `AutoBpp` -- over a modelled path:
//! - **Two clocks.** The receiver's clock runs 3 s ahead of the sender's and 20 ppm fast; the
//!   genlock timecode is the sender's. A receiver wall STEP is removed by the meter's `WallSteps`
//!   (review round 2: the arrival is read on the wall, the step correction when the frame is
//!   processed), so only the frame whose datagrams arrive across the step keeps it, uncorrected: a
//!   frame of residue, never a moved base.
//! - **The sender** encodes a 1080p frame every 1/30 s at the auto bpp; its packets come out of the
//!   encoder over the encode span (FEC parity included), `send()` returns at once at line rate or
//!   at the pace, and its own NIC serializes them at its link speed.
//! - **The path** ends in the receiver's switch port: a drop-tail FIFO served at the port rate,
//!   shared with CROSS traffic. Packets are modelled in groups of 8 datagrams.
//!
//! Scenarios: (1) strih's 10 GbE sender into stream's 1 GbE port (1 MB buffer) with a download that
//! ramps 0 -> 600 Mb/s at 60..65 s, holds to 120 s and stops (unresponsive: harsher than TCP),
//! with and without the delay signal; (2) a 1 GbE cambox at line rate into the 10 GbE strih with
//! the capture-phase sawtooth of its timecodes and +-5 ms wall steps on the receiver (review round
//! 1): it must never read congestion.

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
const RX_OFFSET_US: f64 = 3_000_000.0;
const RX_PPM: f64 = 20e-6;
const TC0_100NS: u64 = 17_617_300_000_000_000;

#[derive(Clone, Copy)]
struct Scenario {
    sender_link_mbps: f64,
    port_mbps: f64,
    port_buffer_bytes: f64,
    /// The receiver's link as its report names it.
    rx_link_mbps: u32,
    /// The cross traffic at the port (Mb/s) at a time (s).
    cross: fn(f64) -> f64,
    /// The encoder's span over a frame's packets (us).
    encode_us: f64,
    /// The cambox capture phase in its 60 fps slot drifts 1.2 ms a second (the timecode is the slot).
    phase_sawtooth: bool,
    /// Receiver wall steps: (at s, by us).
    rx_steps: &'static [(f64, f64)],
    /// SENDER wall steps (the fleet date step): (at s, by us). The genlock timecode follows the
    /// stepped wall; the sender's own first-send delay is read on its wall minus its own steps, so
    /// per frame the step cancels -- but the two ends' frame sets differ around it (review round 3).
    tx_steps: &'static [(f64, f64)],
    delay_signal: bool,
    end_s: f64,
}

fn download(t_s: f64) -> f64 {
    match t_s {
        t if t < 60.0 => 0.0,
        t if t < 65.0 => 600.0 * (t - 60.0) / 5.0,
        t if t < 120.0 => 600.0,
        _ => 0.0,
    }
}

fn quiet(_: f64) -> f64 {
    0.0
}

/// The drop-tail port: a fluid backlog in bytes.
struct Port {
    backlog: f64,
    at_us: f64,
    bytes_per_us: f64,
    buffer: f64,
}

impl Port {
    /// A send of `bytes` arriving at `t`; `None` = dropped, else the time its last byte left.
    fn push(&mut self, t: f64, bytes: f64) -> Option<f64> {
        if t > self.at_us {
            self.backlog = (self.backlog - (t - self.at_us) * self.bytes_per_us).max(0.0);
            self.at_us = t;
        }
        if self.backlog + bytes > self.buffer {
            return None;
        }
        self.backlog += bytes;
        Some(self.at_us + self.backlog / self.bytes_per_us)
    }
}

#[derive(Debug, Default)]
struct Outcome {
    bpp_at_60s: u32,
    first_congestion_step_s: Option<f64>,
    congestion_steps: u32,
    first_jxs_drop_s: Option<f64>,
    /// Frames with any lost datagram while the download ran (60..120 s) and after 70 s.
    lossy_frames_download: u32,
    lossy_frames_after_70s: u32,
    lossy_frames: u32,
    /// The largest share of one frame's datagrams lost.
    worst_frame_loss: f64,
    min_bpp_download: u32,
    bpp_at_end: u32,
    pace_at_end: (u32, Option<PaceSource>),
    cross_dropped_bytes: f64,
}

fn run(sc: Scenario) -> Outcome {
    let mut out = Outcome {
        min_bpp_download: u32::MAX,
        ..Outcome::default()
    };
    let mut auto = AutoBpp::new(AutoBppConfig::new(4, 8));
    let mut pace = PaceControl::new(0, None);
    pace.set_own_link(Some(sc.sender_link_mbps as u32));
    let mut bpp = 4u32;
    auto.applied(bpp, TC0_100NS);
    let mut pace_mbps = 0u32;
    let mut port = Port {
        backlog: 0.0,
        at_us: 0.0,
        bytes_per_us: sc.port_mbps / 8.0,
        buffer: sc.port_buffer_bytes,
    };
    let mut meter = CongestionMeter::default();
    let mut window = ReportWindow::default();
    let mut next_cross_us = 0.0f64;
    let mut pace_free_us = 0.0f64;
    let mut nic_free_us = 0.0f64;
    let frames = (sc.end_s * 30.0) as u64;
    for k in 0..frames {
        let t0 = k as f64 * FRAME_US;
        let t_s = t0 / 1e6;
        let tx_step: f64 = sc
            .tx_steps
            .iter()
            .filter(|(at, _)| t_s >= *at)
            .map(|(_, by)| by)
            .sum();
        let tc = (TC0_100NS as i64 + ((t0 + tx_step) * 10.0) as i64) as u64;
        if auto.target() != bpp {
            bpp = auto.target();
            auto.applied(bpp, tc);
        }
        pace.on_frames(1920, 1080, bpp);
        if let Some(d) = pace.changed() {
            pace_mbps = d.mbps;
        }
        let code = 1920.0 * 1080.0 * bpp as f64 / 8.0;
        // the data datagrams plus one FEC parity per 12 (per slice: ~1 in 11)
        let dgrams = (code / SEGMENT_DATA_LEN as f64).ceil() as usize;
        let dgrams = dgrams + dgrams.div_ceil(11);
        let groups = dgrams.div_ceil(GROUP);
        let phase = if sc.phase_sawtooth {
            (t_s * 1_200.0) % 16_667.0
        } else {
            0.0
        };
        // our sends, then the cross traffic of the same span, through the port in time order
        let mut events: Vec<(f64, bool)> = Vec::with_capacity(groups + 64);
        let (mut first_send, mut last_send) = (None::<f64>, 0.0f64);
        for g in 0..groups {
            let produced = t0 + phase + 2_000.0 + g as f64 * sc.encode_us / groups as f64;
            let sent_at = if pace_mbps > 0 {
                let at = produced.max(pace_free_us);
                pace_free_us = at + (GROUP as f64 * DGRAM_WIRE * 8.0) / pace_mbps as f64;
                at
            } else {
                produced
            };
            let nic = sent_at.max(nic_free_us);
            nic_free_us = nic + (GROUP as f64 * DGRAM_WIRE * 8.0) / sc.sender_link_mbps;
            first_send.get_or_insert(sent_at);
            last_send = sent_at;
            events.push((nic_free_us, true));
        }
        let span_end = t0 + FRAME_US;
        while next_cross_us < span_end {
            let c = (sc.cross)(next_cross_us / 1e6);
            if c < 1.0 {
                // (under 1 Mb/s the next packet would land seconds away: none this frame)
                next_cross_us = span_end;
                break;
            }
            events.push((next_cross_us, false));
            next_cross_us += (GROUP as f64 * DGRAM_WIRE * 8.0) / c;
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        // `WallSteps` removes a step from every frame processed after it; the frame that arrived
        // across it (its first datagram before, its processing after) keeps it once
        let rx_step: f64 = sc
            .rx_steps
            .iter()
            .filter(|(at, _)| t_s >= *at && t_s < *at + FRAME_US / 1e6)
            .map(|(_, by)| by)
            .sum();
        let (mut first_rx, mut last_rx, mut got) = (None::<f64>, 0.0f64, 0usize);
        for (at, ours) in events {
            match port.push(at, GROUP as f64 * DGRAM_WIRE) {
                Some(left) if ours => {
                    let rx = (left + 50.0) * (1.0 + RX_PPM) + RX_OFFSET_US + rx_step;
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
        let first_send = first_send.unwrap_or(last_send);
        pace.on_wire((last_send - first_send) / 1000.0);
        // the sender's wall minus its own steps: the step is in the timecode only, and the
        // sender's `WallSteps` reads it
        pace.on_first_send(
            tc,
            (first_send * 10.0) as i64 + TC0_100NS as i64,
            (tx_step * 10.0) as i64,
        );
        let sent = groups * GROUP;
        let lost = sent.saturating_sub(got);
        let to_100ns = |us: f64| (us * 10.0) as i64 + TC0_100NS as i64;
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
        if lost > 0 {
            out.lossy_frames += 1;
            if t_s >= 70.0 {
                out.lossy_frames_after_70s += 1;
            }
        }
        out.worst_frame_loss = out.worst_frame_loss.max(lost as f64 / sent as f64);
        if k == 60 * 30 {
            out.bpp_at_60s = bpp;
        }
        // once a second: the receiver's report, then the sender's second
        if k % 30 == 29 {
            let now_s = (t0 + FRAME_US) / 1e6;
            let mut r = window.take(100, 0);
            r.link_mbps = sc.rx_link_mbps;
            let c = meter.take();
            if sc.delay_signal {
                r.delay_mean_us = c.delay_mean_us;
                r.delay_spread_us = c.delay_spread_us;
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
                    out.congestion_steps += 1;
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

const STRIH_TO_STREAM: Scenario = Scenario {
    sender_link_mbps: 10_000.0,
    port_mbps: 1_000.0,
    port_buffer_bytes: 1_000_000.0,
    rx_link_mbps: 1000,
    cross: download,
    encode_us: 10_000.0,
    phase_sawtooth: false,
    rx_steps: &[],
    tx_steps: &[],
    delay_signal: true,
    end_s: 420.0,
};

#[test]
fn the_link_survives_a_saturating_download_and_climbs_back() {
    let o = run(STRIH_TO_STREAM);
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
    let o = run(Scenario {
        delay_signal: false,
        end_s: 130.0,
        ..STRIH_TO_STREAM
    });
    eprintln!("{o:?}");
    assert!(o.first_congestion_step_s.is_none());
    assert!(o.first_jxs_drop_s.is_some(), "loss is the only signal left");
    assert!(
        o.lossy_frames_download >= 50,
        "loss-driven alone loses far more: {}",
        o.lossy_frames_download
    );
}

/// Review round 1: a 1 GbE cambox at line rate into the 10 GbE strih. Its NIC needs ~19 ms of
/// wire for an 8 bpp frame its encoder hands over in ~14 ms, its timecodes carry the capture
/// phase sawtooth, and the receiver's wall clock steps +5 ms then -5 ms: none of it is congestion.
#[test]
fn a_line_rate_cambox_with_a_phase_sawtooth_and_clock_steps_never_reads_congestion() {
    let o = run(Scenario {
        sender_link_mbps: 1_000.0,
        port_mbps: 10_000.0,
        port_buffer_bytes: 4_000_000.0,
        rx_link_mbps: 10_000,
        cross: quiet,
        encode_us: 13_800.0,
        phase_sawtooth: true,
        rx_steps: &[(70.0, 5_000.0), (90.0, -5_000.0)],
        tx_steps: &[],
        delay_signal: true,
        end_s: 120.0,
    });
    eprintln!("{o:?}");
    assert_eq!(o.congestion_steps, 0, "never a congestion step");
    assert_eq!(o.lossy_frames, 0);
    assert_eq!(o.bpp_at_60s, 8);
    assert_eq!(o.bpp_at_end, 8, "it stays at 8 bpp");
    assert_eq!(
        o.pace_at_end,
        (0, Some(PaceSource::LineRate)),
        "a cambox is never paced"
    );
}

/// Review round 3: the nightly fleet DATE step on the SENDER (+200 ms, later -200 ms) on a quiet
/// paced path. Per frame the step cancels (timecode and own delay move together), but the
/// receiver's frames of one period and the sender's timecode window around the newest one are not
/// the same frames across the step: a mean over each must never read as a queue.
#[test]
fn a_sender_date_step_never_reads_as_a_queue() {
    let o = run(Scenario {
        cross: quiet,
        tx_steps: &[(70.5, 200_000.0), (85.3, 30_000.0), (100.4, -200_000.0)],
        end_s: 130.0,
        ..STRIH_TO_STREAM
    });
    eprintln!("{o:?}");
    assert_eq!(o.congestion_steps, 0, "never a congestion step");
    assert_eq!(o.lossy_frames, 0);
    assert_eq!(o.bpp_at_end, 8);
    assert_eq!(o.pace_at_end, (900, Some(PaceSource::ReceiverLink)));
}
