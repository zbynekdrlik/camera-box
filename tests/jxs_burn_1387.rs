//! Issue 1387 slice 1a -- in burn mode the JPEG XS link carries the BURNED frame, the decode
//! fixture `.claude/rules/pattern-change-needs-decode-fixture.md` asks for.
//!
//! The burn thread's own step (`probe::burn_jxs::burn_then_offer_jxs`) burns the cam1 QR into a
//! synthetic YUYV frame and offers it to the link type the capture loop holds (`JxsLink`, started
//! with the real socket). The slices cross loopback UDP, the real decoder rebuilds the picture,
//! and the repo's own QR decode (`probe::qr::decode_qr_luma`) reads the burned payload back from
//! the RECEIVED luma. Before slice 1a the link saw the frame before the burn: no QR on JPEG XS.
#![cfg(feature = "probe")]
#![cfg(all(target_os = "linux", feature = "jxs"))]

use camera_box::capture::FrameInfo;
use camera_box::jxs_link::{psnr, FrameAssembler, FrameReport, PacketHeader, HEADER_LEN};
use camera_box::jxs_recv::{DecodeSink, JxsRecvSocket};
use camera_box::jxs_send::{BppMode, JxsSender, JxsSocket, SenderOptions, DEFAULT_BPP};
use camera_box::probe::burn_jxs::{burn_then_offer_jxs, BurnJxsOffer};
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::decode_qr_luma;
use image::GrayImage;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use svt_jpegxs_sys::YuvPlanes;

/// A 640x480 frame: the cam1 burn (320 px, bottom-centred, 24 px margin) fits, and the height is
/// whole 16-line slices.
const W: usize = 640;
const H: usize = 480;

/// A smooth synthetic YUYV picture (gradients + a soft circle): no QR anywhere in it.
fn yuyv_frame() -> Vec<u8> {
    let mut out = Vec::with_capacity(W * H * 2);
    for y in 0..H {
        for x in (0..W).step_by(2) {
            let luma = |x: usize| {
                let dx = x as f64 - (W / 2) as f64;
                let dy = y as f64 - (H / 3) as f64;
                let circle = if dx * dx + dy * dy < 60.0 * 60.0 {
                    50.0
                } else {
                    0.0
                };
                (40.0 + x as f64 * 100.0 / W as f64 + y as f64 * 40.0 / H as f64 + circle) as u8
            };
            out.push(luma(x));
            out.push((100 + y * 40 / H) as u8);
            out.push(luma(x + 1));
            out.push((150 - x * 30 / W) as u8);
        }
    }
    out
}

/// The luma plane of a tight YUYV buffer.
fn luma_of(yuyv: &[u8]) -> Vec<u8> {
    yuyv.iter().step_by(2).copied().collect()
}

fn info() -> FrameInfo {
    FrameInfo {
        width: W as u32,
        height: H as u32,
        fourcc: v4l::FourCC::new(b"YUYV"),
        stride: (W * 2) as u32,
        capture_monotonic_100ns: 0,
        dequeue_duration_ms: 0.0,
        sequence: 0,
    }
}

/// Receive and decode one frame from `rx` (10 s at most).
fn receive_one(rx: &mut JxsRecvSocket) -> (FrameReport, YuvPlanes) {
    let mut assembler = FrameAssembler::new();
    let got: RefCell<Option<(FrameReport, YuvPlanes)>> = RefCell::new(None);
    let mut sink = DecodeSink::new(|r: &FrameReport, p: Option<&YuvPlanes>| {
        *got.borrow_mut() = Some((*r, p.expect("decoded").clone()));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.borrow().is_none() && Instant::now() < deadline {
        rx.recv_batch(|d, at| {
            // the FEC parity datagrams are not slices
            if d[0] == camera_box::jxs_link::fec::FEC_MAGIC {
                return;
            }
            let h = PacketHeader::decode(d).expect("header");
            assembler.push(&h, &d[HEADER_LEN..], at, &mut sink);
        })
        .unwrap();
        assert!(
            sink.last_error.is_none(),
            "decoder error: {:?}",
            sink.last_error
        );
    }
    drop(sink);
    got.into_inner().expect("a decoded frame within 10 s")
}

#[test]
fn in_burn_mode_the_jpeg_xs_link_carries_the_burned_frame_and_its_qr_decodes() {
    let mut rx = JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_timeout(Some(Duration::from_millis(100))).unwrap();
    let target = format!("127.0.0.1:{}", rx.local_addr().unwrap().port());
    let link = JxsSender::start_retrying(
        Some(&target),
        "CAM5",
        SenderOptions::new(BppMode::Fixed(DEFAULT_BPP), None),
        JxsSocket::connect,
        Duration::from_secs(1),
    )
    .expect("the link starts when the target is set");

    let raw = yuyv_frame();
    let raw_luma = luma_of(&raw);
    assert_eq!(
        decode_qr_luma(GrayImage::from_raw(W as u32, H as u32, raw_luma.clone()).unwrap()),
        None,
        "the captured picture holds no QR of its own"
    );
    let payload = Payload {
        run_id: 911_001,
        frame_id: 4242,
        gen_ts_ns: 1_761_845_600_123_456_789,
    };
    // an even slot of the 60 fps emit grid: a frame the 30 fps JPEG XS grid takes
    let offer = BurnJxsOffer {
        timecode_100ns: 17_618_456_000_000_000,
        emit_fps: 60,
        capture_realtime_100ns: 0,
    };
    let mut buf = raw.clone();
    burn_then_offer_jxs(&mut buf, &info(), Some(&payload), Some((&link, &offer)));

    // the buffer the NDI send carries next holds the burn
    let burned_luma = luma_of(&buf);
    assert_eq!(
        decode_qr_luma(GrayImage::from_raw(W as u32, H as u32, burned_luma.clone()).unwrap()),
        Some(payload),
        "the NDI buffer carries the burn"
    );

    let (report, decoded) = receive_one(&mut rx);
    assert_eq!(report.timecode, offer.timecode_100ns as u64);
    assert_eq!(report.camera_id, 5);
    assert_eq!(report.lost_slices, 0);
    let got = decoded.y().to_vec();
    let to_burned = psnr(&burned_luma, &got);
    let to_raw = psnr(&raw_luma, &got);
    eprintln!(
        "received luma PSNR: {to_burned:.2} dB to the burned frame, {to_raw:.2} dB to the raw one"
    );
    assert_eq!(
        decode_qr_luma(GrayImage::from_raw(W as u32, H as u32, got).unwrap()),
        Some(payload),
        "the QR burned into the frame decodes from the picture JPEG XS delivered"
    );
    assert!(
        to_burned >= 30.0 && to_burned > to_raw + 6.0,
        "the received picture is the BURNED frame: {to_burned:.2} dB to it, {to_raw:.2} dB to the raw one"
    );
}
