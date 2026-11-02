//! Issue 1387 slice 1a -- in burn mode (the E2E's temporary probe-featured camera-box with
//! `CAMERA_BOX_BURN_RUN_ID` set) the JPEG XS link carries the BURNED frame.
//!
//! The strih OBS programs the JPEG XS inputs (`JXS camN src`), so the full-path E2E must find the
//! camera's QR burn in the JPEG XS stream too. In burn mode the NDI sender lives on the
//! `cam1-burn` thread and sends the job's copied buffer after the QR is drawn into it; the JPEG XS
//! link moves to that thread as well and is offered the SAME buffer, after the QR and before the
//! NDI send ([`burn_then_offer_jxs`]). Only the CURRENT frame's job carries a [`BurnJxsOffer`]: a
//! starvation repeat stays NDI-only, exactly as the production capture loop offers JPEG XS only
//! the current frame.
//!
//! Production (no burn) is untouched: the capture loop offers the raw frame as before.

use crate::capture::FrameInfo;
use crate::jxs_send::{JxsFrame, JxsLink};
use crate::probe::payload::Payload;
use crate::probe::qr::{burn_qr_yuyv, CAM1_BURN_QR_PX};

/// The current frame's JPEG XS hand-off, stamped on the capture thread and carried with its burn
/// job (the fields of [`JxsFrame`] the burn thread cannot know).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurnJxsOffer {
    /// The genlock timecode the NDI frame carries (the capture loop's `capture_timecode_100ns`).
    pub timecode_100ns: i64,
    /// The NDI emit rate (0 = genlock off): selects the 30 fps grid frames.
    pub emit_fps: u32,
    /// The frame's RAW capture instant on the wall clock (0 = unknown), for the hand-off
    /// diagnostics' split.
    pub capture_realtime_100ns: i64,
}

impl BurnJxsOffer {
    /// The JPEG XS frame for `buf`: the burn job's copy of the captured frame, so it has the
    /// capture's own layout (`info`'s geometry, stride and fourcc), as the capture loop's raw
    /// offer does.
    pub fn frame<'a>(&self, buf: &'a [u8], info: &FrameInfo) -> JxsFrame<'a> {
        JxsFrame {
            data: buf,
            width: info.width,
            height: info.height,
            stride: info.stride,
            yuyv: info.fourcc.str().map(|f| f == "YUYV").unwrap_or(false),
            timecode_100ns: self.timecode_100ns,
            emit_fps: self.emit_fps,
            capture_realtime_100ns: self.capture_realtime_100ns,
        }
    }
}

/// The burn thread's work on one job before its NDI send: draw the QR (`qr` is `Some` for a YUYV
/// frame) into `buf`, the buffer the NDI send then carries, and hand the frame to the JPEG XS
/// link (`jxs` is `Some` only for the current frame of a box whose link is on). Never blocks:
/// the link's `offer` converts into a free picture or counts a drop.
pub fn burn_then_offer_jxs(
    buf: &mut [u8],
    info: &FrameInfo,
    qr: Option<&Payload>,
    jxs: Option<(&JxsLink, &BurnJxsOffer)>,
) {
    if let Some(payload) = qr {
        burn_qr_yuyv(
            buf,
            info.width,
            info.height,
            info.stride,
            payload,
            CAM1_BURN_QR_PX,
        );
    }
    // after the QR: the link converts the burned buffer (a copy into its own picture), so the
    // NDI send that follows reads the same bytes
    if let Some((link, offer)) = jxs {
        link.offer(&offer.frame(buf, info));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v4l::FourCC;

    fn info(fourcc: &[u8; 4]) -> FrameInfo {
        FrameInfo {
            width: 640,
            height: 480,
            fourcc: FourCC::new(fourcc),
            stride: 1344,
            capture_monotonic_100ns: 7,
            dequeue_duration_ms: 0.0,
            sequence: 3,
        }
    }

    #[test]
    fn the_offer_carries_the_buffer_the_capture_geometry_and_the_frames_own_stamp() {
        let buf = vec![9u8; 1344 * 480];
        let o = BurnJxsOffer {
            timecode_100ns: 17_618_456_000_000_000,
            emit_fps: 60,
            capture_realtime_100ns: 17_618_455_999_900_000,
        };
        let f = o.frame(&buf, &info(b"YUYV"));
        assert!(std::ptr::eq(f.data, buf.as_slice()), "the job's own buffer");
        assert_eq!((f.width, f.height, f.stride), (640, 480, 1344));
        assert!(f.yuyv);
        assert_eq!(f.timecode_100ns, o.timecode_100ns);
        assert_eq!(f.emit_fps, 60);
        assert_eq!(f.capture_realtime_100ns, o.capture_realtime_100ns);
        // a substituted non-YUYV frame is offered as such: the link counts it, never converts it
        assert!(!o.frame(&buf, &info(b"MJPG")).yuyv);
    }
}
