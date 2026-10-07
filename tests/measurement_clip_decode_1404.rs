//! issue 1404 — the camera-box measurement clip decodes through the production recording decode.
//!
//! `scripts/gen_measurement_clip.py` paints the cam2 painter's dual-QR Vernier under the reserved
//! run id 911016 (`recording_latency::MEASUREMENT_CLIP_RUN_ID`), the painter's geometry (700 px
//! budget, top margin 24, each QR centred in its half). The fixture is frame 1801 of the REAL
//! deliverable `measurement-clip-v1.mp4` (H.264, sha256 5eaf3f9d…), decoded to gray by ffmpeg:
//! the lossy artifact the CG segments play, not a crisp synthetic canvas. Two independent
//! decoders read both halves of it on dev1 before this test was written: zbarimg and OpenCV
//! (`scripts/youtube_leg_ticks.py` `half_ticks`); the real rqrr 0.9 plain pass read both too.
//!
//! What this pins:
//! - the recording decode reads both 911016 payloads (left = tick 3602, right = tick 3601);
//! - the clip's ids never become the cam2 Vernier tick (`RecordingFrame::tick` stays `None`:
//!   911016 is in `NODE_BURN_RUN_IDS`), so a CG segment cannot corrupt the camera-chain metrics;
//! - the robust optical decode (a strict superset) reads them too.

#![cfg(feature = "probe")]

use camera_box::probe::payload::Payload;
use camera_box::probe::qr::decode_qr_luma_all_robust_optical;
use camera_box::probe::recording::decode_recording_frame_with_burns;
use camera_box::probe::recording_latency::MEASUREMENT_CLIP_RUN_ID;
use image::GrayImage;
use std::path::PathBuf;

fn fixture_luma() -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "measurement-clip-1404",
        "clip-v1-frame-1801.png",
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

/// The two halves of clip frame 1801: the painter's pair for the 60 Hz tick 3602, each carrying
/// its own tick's content time (tick * 1e9 / 60, rounded half up).
fn frame_1801_payloads() -> [Payload; 2] {
    [
        Payload {
            run_id: MEASUREMENT_CLIP_RUN_ID,
            frame_id: 3602,
            gen_ts_ns: 60_033_333_333,
        },
        Payload {
            run_id: MEASUREMENT_CLIP_RUN_ID,
            frame_id: 3601,
            gen_ts_ns: 60_016_666_667,
        },
    ]
}

#[test]
fn the_clip_frame_decodes_both_halves_and_never_becomes_the_vernier_tick_1404() {
    let luma = fixture_luma();
    assert_eq!(luma.dimensions(), (1920, 1080));
    let frame = decode_recording_frame_with_burns(1801, luma, &[]);
    for want in frame_1801_payloads() {
        assert!(
            frame.payloads.contains(&want),
            "the recording decode must read {} from the clip frame; got {:?}",
            want.encode(),
            frame.payloads
        );
    }
    assert_eq!(
        frame.tick, None,
        "the measurement clip's ids are tick-excluded: they must never become the cam2 tick"
    );
}

#[test]
fn the_robust_optical_decode_reads_the_clip_frame_too_1404() {
    let payloads = decode_qr_luma_all_robust_optical(fixture_luma());
    for want in frame_1801_payloads() {
        assert!(
            payloads.contains(&want),
            "the robust optical decode must read {}; got {payloads:?}",
            want.encode()
        );
    }
}
