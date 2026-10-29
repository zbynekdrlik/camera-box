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
//! Every paced value is clamped into the schedule's bounds (`PACE_MBPS_MIN..=PACE_MBPS_MAX`).

use super::pace::{wire_bytes, PACE_MBPS_MAX, PACE_MBPS_MIN};
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
}

impl std::fmt::Display for PaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fixed => "fixed",
            Self::ReceiverLink => "receiver-link",
            Self::LineRate => "line-rate",
            Self::StartSpread => "start-spread",
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
/// bytes, what SVT-JPEG-XS's rate control codes every frame to) cut into datagrams of
/// [`SEGMENT_DATA_LEN`] + the 16-byte header, plus the Ethernet overhead of each.
pub fn frame_wire_bytes(width: u32, height: u32, bpp: u32) -> u64 {
    let _ = (
        width,
        height,
        bpp,
        HEADER_LEN,
        SEGMENT_DATA_LEN,
        wire_bytes(0),
    );
    0
}

/// The rate that spreads `frame_wire_bytes` over 1 / [`START_SPREAD_DIVISOR`] of the interval of
/// `fps` (Mb/s, rounded up; 0 for an empty frame or no rate).
pub fn frame_spread_mbps(frame_wire_bytes: u64, fps: u32) -> u64 {
    let _ = (frame_wire_bytes, fps);
    0
}

/// The decision (module doc).
pub fn decide(i: &PaceInputs) -> PaceDecision {
    let _ = (i, clamp(0));
    PaceDecision::default()
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
        })
    }

    fn paced(mbps: u32, source: PaceSource) -> PaceDecision {
        PaceDecision { mbps, source }
    }

    #[test]
    fn a_frame_on_the_wire_is_the_codestream_plus_its_headers_and_ethernet() {
        // 1920 x 1080 x 4 / 8 = 1036800 bytes = 713 datagrams of 1456 (the last one shorter)
        let w = frame_wire_bytes(1920, 1080, 4);
        assert_eq!(w, 1_036_800 + 713 * 16 + 713 * 66);
        assert_eq!(frame_wire_bytes(1920, 1080, 8), {
            let code = 2_073_600u64;
            let n = code.div_ceil(1456);
            code + n * (16 + 66)
        });
        assert_eq!(frame_wire_bytes(0, 1080, 4), 0);
        // the spread rate: 1095266 bytes x 8 x 30 x 2 = 525.7 Mb/s
        assert_eq!(frame_spread_mbps(w, 30), 526);
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
        assert_eq!(at(Some(10_000), None), paced(526, PaceSource::StartSpread));
        assert_eq!(at(None, None), paced(526, PaceSource::StartSpread));
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
            });
            assert_eq!(d, paced(800, PaceSource::Fixed));
        }
    }

    #[test]
    fn the_sources_print_as_short_tokens() {
        assert_eq!(PaceSource::Fixed.to_string(), "fixed");
        assert_eq!(PaceSource::ReceiverLink.to_string(), "receiver-link");
        assert_eq!(PaceSource::LineRate.to_string(), "line-rate");
        assert_eq!(PaceSource::StartSpread.to_string(), "start-spread");
    }
}
