//! Regression test for #106: the recorded-file → ffmpeg → rqrr analysis
//! entrypoint must decode a strih-6519-type frame (two sharp QRs side by side)
//! to BOTH QRs. This is the foundation for the #105/#107 loss/delivery verdict,
//! which counts ONLY from the recorded OBS program-output file — never an NDI tap
//! or the lz4 spool.
//!
//! The committed fixture `tests/fixtures/strih-6519-dual-qr.mkv` is a 1-frame
//! lossless (ffv1, gray8) clip carrying two sharp side-by-side QRs rendered by the
//! repo's own dual-QR painter (`render_qr_dual_bgra`): left = even tick
//! (frame_id 6518), right = odd tick (frame_id 6519). opencv `detectAndDecodeMulti`
//! returned 0 on the real strih frame 6519; the rqrr path MUST return 2.
//!
//! This drives the FULL entrypoint (spawn ffmpeg to emit gray8 rawvideo at native
//! resolution → feed each frame's luma into the existing rqrr decoder), so it
//! requires `ffmpeg`/`ffprobe` on PATH (a documented runtime dependency of
//! camera-box, installed in CI).

#![cfg(feature = "probe")]

use camera_box::probe::recording::analyze_recording;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect()
}

#[test]
fn strih_6519_dual_qr_frame_decodes_both_qrs() {
    let path = fixture("strih-6519-dual-qr.mkv");
    let frames = analyze_recording(&path).expect("analyze_recording on fixture mkv");

    // The fixture is exactly one frame.
    assert_eq!(frames.len(), 1, "fixture is a single-frame clip");

    let frame = &frames[0];
    // The strih-6519 acceptance bar: a 2-sharp-QR frame MUST decode to 2 (the
    // exact case opencv silently returned 0/1 on). No false-negatives.
    assert_eq!(
        frame.payloads.len(),
        2,
        "strih-6519-type frame must decode to BOTH QRs (got {:?})",
        frame.payloads
    );

    // Both painted ticks are present (left even 6518, right odd 6519); the Vernier
    // effective tick is the max of the two.
    let ids: Vec<u32> = frame.payloads.iter().map(|p| p.frame_id).collect();
    assert!(ids.contains(&6518), "left (even) QR tick 6518 present: {ids:?}");
    assert!(ids.contains(&6519), "right (odd) QR tick 6519 present: {ids:?}");
    assert_eq!(
        frame.tick,
        Some(6519),
        "effective Vernier tick = max(left, right)"
    );
}
