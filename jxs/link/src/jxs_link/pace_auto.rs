//! Issue 1387: WHICH pace a JPEG XS sender runs, decided from the two links (owner 29.10.2026:
//! no hand-set number for the 1 GbE port of one venue; "pozor aby zmeny co urobis pre snv
//! fungovali aj v pp ak tam maju ine sietovky/ine rychlosti").
//!
//! Pure: the sender passes in what it knows and gets a [`PaceDecision`] (`mbps` 0 = line rate,
//! the schedule itself is `super::pace`). The rules, in order:
//!
//! 1. **An explicit setting wins** (`CAMERA_BOX_JXS_PACE_MBPS` / the output's `jxs_pace_mbps` /
//!    the C ABI's `pace_mbps` above 0): the override for a path the link speeds do not describe.
//! 2. **The receiver's link is known:** slower than the sender's own link (or the sender's own
//!    is unknown) = pace at [`LINK_PACE_PERCENT`] of it, the rest left to the port's other
//!    traffic (1 GbE -> 900, the rate the 29.10.2026 23:35 live run carried at 8 bpp with no
//!    loss; 2.5 GbE -> 2250). As fast or faster = line rate: nothing steps the burst down (a
//!    1 GbE cambox to the 10 GbE strih keeps the owner's line-rate choice).
//! 3. **No receiver link yet** (no report, or a version-1 receiver that does not send it): the
//!    SAFE START = the frame-spread rate, the frame's own wire bits spread over
//!    1 / [`START_SPREAD_DIVISOR`] of the frame interval (~520 Mb/s at 4 bpp 1080p30). Derived
//!    from the frame, never from a guessed link: any link that carries the stream at all carries
//!    twice its average. Never above the sender's own link (that is line rate anyway).
//!
//! 4. **Congestion** (`crate::jxs_congestion`, the receiver's delay signal): a factor under 100 %
//!    lowers a PACED automatic pace to that share of it, but never below what one frame needs in
//!    its interval plus a tenth ([`frame_need_mbps`]): slower than that, frames back up and drop at
//!    the sender. A line-rate sender (a cambox into the faster strih: the owner's "never pace a
//!    cambox") and an explicit setting are never paced by it; there the congestion control acts
//!    through the bpp alone (review round 1).
//!
//! Every paced value is clamped into the schedule's bounds (`PACE_MBPS_MIN..=PACE_MBPS_MAX`).

use super::fec::FEC_GROUP;
use super::pace::{PACE_MBPS_MAX, PACE_MBPS_MIN, WIRE_OVERHEAD_BYTES};
use super::{HEADER_LEN, SEGMENT_DATA_LEN};

/// The share of the receiver's link the JPEG XS flow is paced to; the rest is headroom for the
/// port's other traffic (NDI, control).
pub const LINK_PACE_PERCENT: u64 = 90;
/// The safe start spreads a frame over this fraction of its interval (2 = half of it).
pub const START_SPREAD_DIVISOR: u64 = 2;

/// Where the pace came from (the status line's `pace_src=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaceSource {
    /// An explicit setting.
    Fixed,
    /// [`LINK_PACE_PERCENT`] of the receiver's (slower) link.
    ReceiverLink,
    /// The receiver's link is as fast as the sender's, or the frame-spread rate is not below it.
    #[default]
    LineRate,
    /// No receiver link known yet: the frame-spread rate.
    StartSpread,
    /// The automatic pace lowered by the congestion factor.
    Congestion,
}

impl std::fmt::Display for PaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fixed => "fixed",
            Self::ReceiverLink => "receiver-link",
            Self::LineRate => "line-rate",
            Self::StartSpread => "start-spread",
            Self::Congestion => "congestion",
        })
    }
}

/// What the sender knows when it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaceInputs {
    /// The explicit setting, 0 = automatic.
    pub setting_mbps: u32,
    /// The sender's own link (`None` = unknown).
    pub own_link_mbps: Option<u32>,
    /// The receiver's link from its report (`None` = no report with one yet).
    pub receiver_link_mbps: Option<u32>,
    /// One frame's wire bytes at the current bpp ([`frame_wire_bytes`]).
    pub frame_wire_bytes: u64,
    /// The link's frame rate.
    pub fps: u32,
    /// The congestion controller's pace factor in percent; 0 or 100 and above = none.
    pub congestion_pct: u32,
}

/// The pace to run: `mbps` 0 = line rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaceDecision {
    pub mbps: u32,
    pub source: PaceSource,
}

fn clamp(mbps: u64) -> u32 {
    mbps.clamp(PACE_MBPS_MIN as u64, PACE_MBPS_MAX as u64) as u32
}

/// The wire bytes of one frame of `width` x `height` at `bpp`: the codestream (`w x h x bpp / 8`
/// bytes, what SVT-JPEG-XS's rate control codes every frame to) cut per 16-line slice into
/// datagrams of [`SEGMENT_DATA_LEN`], plus the header packet, the FEC parity datagrams (one per
/// [`FEC_GROUP`] of a slice, `super::fec`, each a full payload), the 16-byte header and the
/// Ethernet overhead of every datagram. An estimate from the geometry (slices of equal size).
pub fn frame_wire_bytes(width: u32, height: u32, bpp: u32) -> u64 {
    let code = width as u64 * height as u64 * bpp as u64 / 8;
    if code == 0 {
        return 0;
    }
    let slices = (height as u64).div_ceil(super::SLICE_HEIGHT_LINES as u64);
    let per_slice = code.div_ceil(slices);
    let datagrams = per_slice.div_ceil(SEGMENT_DATA_LEN as u64).max(1);
    let data = slices * datagrams + 1;
    let parity = slices * datagrams.div_ceil(FEC_GROUP as u64) + 1;
    code + parity * SEGMENT_DATA_LEN as u64
        + (data + parity) * (HEADER_LEN as u64 + WIRE_OVERHEAD_BYTES)
}

/// The rate that spreads `frame_wire_bytes` over 1 / [`START_SPREAD_DIVISOR`] of the interval of
/// `fps` (Mb/s, rounded up; 0 for an empty frame or no rate).
pub fn frame_spread_mbps(frame_wire_bytes: u64, fps: u32) -> u64 {
    (frame_wire_bytes * 8 * fps as u64 * START_SPREAD_DIVISOR).div_ceil(1_000_000)
}

/// The least a pace may be for `frame_wire_bytes` every `1 / fps` s: the frame's own rate plus a
/// tenth (Mb/s, rounded up; 0 for an empty frame or no rate).
pub fn frame_need_mbps(frame_wire_bytes: u64, fps: u32) -> u64 {
    (frame_wire_bytes * 8 * fps as u64 * 11).div_ceil(10_000_000)
}

/// The decision (module doc).
pub fn decide(i: &PaceInputs) -> PaceDecision {
    let d = decide_links(i);
    let pct = i.congestion_pct as u64;
    if d.mbps == 0 || d.source == PaceSource::Fixed || pct == 0 || pct >= 100 {
        return d;
    }
    let base = d.mbps as u64;
    let lowered = clamp((base * pct / 100).max(frame_need_mbps(i.frame_wire_bytes, i.fps)));
    if (lowered as u64) < base {
        PaceDecision {
            mbps: lowered,
            source: PaceSource::Congestion,
        }
    } else {
        d
    }
}

/// Rules 1-3 (module doc): the pace from the two links alone.
fn decide_links(i: &PaceInputs) -> PaceDecision {
    let line = PaceDecision {
        mbps: 0,
        source: PaceSource::LineRate,
    };
    if i.setting_mbps > 0 {
        return PaceDecision {
            mbps: clamp(i.setting_mbps as u64),
            source: PaceSource::Fixed,
        };
    }
    if let Some(rx) = i.receiver_link_mbps {
        if i.own_link_mbps.is_some_and(|own| own <= rx) {
            return line;
        }
        return PaceDecision {
            mbps: clamp(rx as u64 * LINK_PACE_PERCENT / 100),
            source: PaceSource::ReceiverLink,
        };
    }
    let spread = frame_spread_mbps(i.frame_wire_bytes, i.fps);
    if spread == 0 || i.own_link_mbps.is_some_and(|own| spread >= own as u64) {
        return line;
    }
    PaceDecision {
        mbps: clamp(spread),
        source: PaceSource::StartSpread,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(own: Option<u32>, rx: Option<u32>) -> PaceDecision {
        decide(&PaceInputs {
            setting_mbps: 0,
            own_link_mbps: own,
            receiver_link_mbps: rx,
            frame_wire_bytes: frame_wire_bytes(1920, 1080, 4),
            fps: 30,
            congestion_pct: 100,
        })
    }

    fn paced(mbps: u32, source: PaceSource) -> PaceDecision {
        PaceDecision { mbps, source }
    }

    #[test]
    fn a_frame_on_the_wire_is_the_codestream_plus_its_headers_and_ethernet() {
        // 4 bpp: 1036800 bytes in 68 slices of 15248 = 11 datagrams each + the header packet,
        // 1 parity per slice + 1: 749 data + 69 parity datagrams of 82 header + Ethernet bytes
        let w = frame_wire_bytes(1920, 1080, 4);
        assert_eq!(w, 1_036_800 + 69 * 1456 + (749 + 69) * 82);
        assert_eq!(w, 1_204_340);
        // 8 bpp: 21 datagrams per slice, 2 parities each
        assert_eq!(
            frame_wire_bytes(1920, 1080, 8),
            2_073_600 + 137 * 1456 + (1429 + 137) * 82
        );
        assert_eq!(frame_wire_bytes(0, 1080, 4), 0);
        // the spread rate: 1204340 bytes x 8 x 30 x 2 = 578.1 Mb/s
        assert_eq!(frame_spread_mbps(w, 30), 579);
        assert_eq!(frame_spread_mbps(w, 0), 0);
    }

    #[test]
    fn a_slower_receiver_link_is_paced_to_ninety_percent_of_it_on_any_venue() {
        // strih-snv 10 GbE -> stream-snv 1 GbE (SNV)
        assert_eq!(
            at(Some(10_000), Some(1000)),
            paced(900, PaceSource::ReceiverLink)
        );
        // strih-pp 10 GbE -> a 2.5 GbE receiver (PP): its own number, nothing hand-set
        assert_eq!(
            at(Some(10_000), Some(2500)),
            paced(2250, PaceSource::ReceiverLink)
        );
        // the sender's own link unknown (Wi-Fi, a tunnel): pace to the receiver anyway
        assert_eq!(at(None, Some(1000)), paced(900, PaceSource::ReceiverLink));
        // a receiver on a 100 Mb/s link: the schedule's floor, the auto bpp does the rest
        assert_eq!(
            at(Some(1000), Some(100)),
            paced(100, PaceSource::ReceiverLink)
        );
    }

    #[test]
    fn a_receiver_as_fast_or_faster_than_the_sender_means_line_rate() {
        // a 1 GbE cambox to the 10 GbE strih: the owner's camera choice, never paced
        assert_eq!(at(Some(1000), Some(10_000)), paced(0, PaceSource::LineRate));
        // equal links: nothing steps the burst down
        assert_eq!(at(Some(1000), Some(1000)), paced(0, PaceSource::LineRate));
        assert_eq!(
            at(Some(10_000), Some(25_000)),
            paced(0, PaceSource::LineRate)
        );
    }

    #[test]
    fn no_receiver_link_yet_starts_with_the_frame_spread_never_above_the_own_link() {
        assert_eq!(at(Some(10_000), None), paced(579, PaceSource::StartSpread));
        assert_eq!(at(None, None), paced(579, PaceSource::StartSpread));
        // the spread is not below the own link: that IS line rate
        assert_eq!(at(Some(500), None), paced(0, PaceSource::LineRate));
        // nothing to spread yet (no frame size, no rate): line rate
        let none = decide(&PaceInputs {
            own_link_mbps: Some(10_000),
            ..PaceInputs::default()
        });
        assert_eq!(none, paced(0, PaceSource::LineRate));
        // a tiny frame: the schedule's floor
        let tiny = decide(&PaceInputs {
            own_link_mbps: Some(10_000),
            frame_wire_bytes: frame_wire_bytes(64, 16, 4),
            fps: 30,
            ..PaceInputs::default()
        });
        assert_eq!(tiny, paced(PACE_MBPS_MIN, PaceSource::StartSpread));
    }

    #[test]
    fn an_explicit_setting_overrides_every_link() {
        for (own, rx) in [
            (Some(10_000), Some(1000)),
            (Some(1000), Some(10_000)),
            (None, None),
        ] {
            let d = decide(&PaceInputs {
                setting_mbps: 800,
                own_link_mbps: own,
                receiver_link_mbps: rx,
                frame_wire_bytes: frame_wire_bytes(1920, 1080, 8),
                fps: 30,
                congestion_pct: 60,
            });
            assert_eq!(
                d,
                paced(800, PaceSource::Fixed),
                "congestion never lowers an override"
            );
        }
    }

    #[test]
    fn the_sources_print_as_short_tokens() {
        assert_eq!(PaceSource::Fixed.to_string(), "fixed");
        assert_eq!(PaceSource::ReceiverLink.to_string(), "receiver-link");
        assert_eq!(PaceSource::LineRate.to_string(), "line-rate");
        assert_eq!(PaceSource::StartSpread.to_string(), "start-spread");
        assert_eq!(PaceSource::Congestion.to_string(), "congestion");
    }

    fn congested(own: Option<u32>, rx: Option<u32>, bpp: u32, pct: u32) -> PaceDecision {
        decide(&PaceInputs {
            own_link_mbps: own,
            receiver_link_mbps: rx,
            frame_wire_bytes: frame_wire_bytes(1920, 1080, bpp),
            fps: 30,
            congestion_pct: pct,
            ..PaceInputs::default()
        })
    }

    /// Issue 1387 item 2: the congestion factor lowers the automatic pace, never under what a frame
    /// needs, and a line-rate sender starts pacing at that share of its own link.
    #[test]
    fn congestion_lowers_the_pace_but_never_below_the_frames_own_need() {
        // 900 x 85 % = 765
        assert_eq!(
            congested(Some(10_000), Some(1000), 8, 85),
            paced(765, PaceSource::Congestion)
        );
        // 8 bpp (FEC included) needs 2401484 x 8 x 30 x 1.1 = 634 Mb/s: 50 % of 900 stops there
        assert_eq!(frame_need_mbps(frame_wire_bytes(1920, 1080, 8), 30), 634);
        assert_eq!(
            congested(Some(10_000), Some(1000), 8, 50),
            paced(634, PaceSource::Congestion)
        );
        // a 1 GbE cambox at line rate to the 10 GbE strih: never paced (the bpp acts alone)
        assert_eq!(
            congested(Some(1000), Some(10_000), 4, 50),
            paced(0, PaceSource::LineRate)
        );
        // no factor, or a factor that would not lower anything: the link decision as before
        assert_eq!(
            congested(Some(10_000), Some(1000), 8, 100),
            paced(900, PaceSource::ReceiverLink)
        );
        assert_eq!(
            congested(Some(10_000), Some(1000), 8, 0),
            paced(900, PaceSource::ReceiverLink)
        );
        // 12 bpp needs 952 Mb/s, more than the 900 pace: nothing lower is possible
        assert_eq!(frame_need_mbps(frame_wire_bytes(1920, 1080, 12), 30), 952);
        assert_eq!(
            congested(Some(10_000), Some(1000), 12, 60),
            paced(900, PaceSource::ReceiverLink)
        );
        // nothing known about either link at line rate: nothing to lower from
        assert_eq!(
            decide(&PaceInputs {
                congestion_pct: 50,
                ..PaceInputs::default()
            }),
            paced(0, PaceSource::LineRate)
        );
    }
}
