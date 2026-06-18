//! Integration test for #108: the per-hop ABSOLUTE latency engine must read a
//! REALLY-DECODED recorded frame (cam2's dual-QR + this node's #111 burn QR,
//! decoded through the actual rqrr path) and recover the KNOWN cam→strih offset.
//!
//! This complements the pure unit tests (which feed hand-built `RecordingFrame`s):
//! here the `RecordingFrame` is produced by the SAME `decode_qr_luma_all` /
//! `decode_recording_frame` path the real recorded-file analysis uses, so it proves
//! the engine works end-to-end on pixels — the cam2 QR and the node burn QR both
//! survive in one frame and the latency math picks the right two stamps.
//!
//! No ffmpeg needed: the frame luma is rendered in-process (the burn filter's burn
//! is a QR in the rendered frame; the recorded-file decoder reads it identically),
//! so this runs in plain CI without the fixture-mkv dependency.

#![cfg(feature = "probe")]

use camera_box::probe::luma::bgra_to_luma;
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::render_qr_bgra;
use camera_box::probe::recording::decode_recording_frame;
use camera_box::probe::recording_latency::{
    cam_strih_samples, hop_latency, split_payloads, RunIds, BURN_RUN_ID_STRIH,
};

const CAM2_RUN_ID: u32 = 6519; // representative cam2 run_id (outside the burn range)

fn strih_ids() -> RunIds {
    RunIds {
        node_burn: BURN_RUN_ID_STRIH,
        cam2: Some(CAM2_RUN_ID),
        other_burns: vec![],
    }
}

/// Build one recorded-frame luma carrying cam2's QR in the TOP half and the node's
/// #111 burn QR in the BOTTOM half — two non-overlapping QRs in one frame, exactly
/// the dedicated-PROBE-scene layout (#111 burns its QR at a bottom strip, distinct
/// from cam2's centered QR). Returns the composited gray8 image.
fn frame_with_cam2_and_node(
    cam2: &Payload,
    node: &Payload,
    w: u32,
    half_h: u32,
    qr: u32,
) -> image::GrayImage {
    // Two stacked BGRA half-canvases, each centering one QR, then concatenated.
    let top = render_qr_bgra(cam2, w, half_h, qr); // cam2 centered in the top half
    let bottom = render_qr_bgra(node, w, half_h, qr); // node centered in the bottom half
    let h = half_h * 2;
    let mut full = vec![255u8; (w * h * 4) as usize];
    let top_bytes = (w * half_h * 4) as usize;
    full[..top_bytes].copy_from_slice(&top);
    full[top_bytes..top_bytes * 2].copy_from_slice(&bottom);
    bgra_to_luma(&full, w, h, w * 4)
}

#[test]
fn decoded_frame_yields_cam_strih_latency_from_real_pixels() {
    // KNOWN offset: strih renders 180 ms after cam2 painted. The decoded frame's two
    // QRs must produce exactly that cam→strih latency through the real decode path.
    let off_ns = 180_000_000i64; // 180 ms
    let base = 1_700_000_000_000_000_000i64; // ~2023 epoch ns (wall-clock domain)

    let (w, half_h, qr) = (900u32, 700u32, 560u32);

    let mut frames = Vec::new();
    for i in 0..4u64 {
        let cam_g = base + i as i64 * 33_333_333;
        let cam2 = Payload {
            run_id: CAM2_RUN_ID,
            frame_id: 100 + i as u32,
            gen_ts_ns: cam_g,
        };
        let node = Payload {
            run_id: BURN_RUN_ID_STRIH,
            frame_id: 5000 + i as u32,
            gen_ts_ns: cam_g + off_ns,
        };
        let luma = frame_with_cam2_and_node(&cam2, &node, w, half_h, qr);
        let rf = decode_recording_frame(i, luma);
        // Both QRs must decode out of the real pixels.
        assert!(
            rf.payloads.len() >= 2,
            "frame {i}: expected both cam2 + node QRs, got {:?}",
            rf.payloads
        );
        // The engine's splitter must find both stamps by run_id.
        let (c, n) = split_payloads(&rf, &strih_ids());
        assert!(
            c.is_some() && n.is_some(),
            "frame {i}: split must find both"
        );
        frames.push(rf);
    }

    let samples = cam_strih_samples(&frames, &strih_ids());
    assert_eq!(samples.len(), 4, "every decoded frame yields a sample");
    let h = hop_latency("cam→strih", &samples).expect("non-empty hop");
    assert!(
        (h.stats.p50_ms - 180.0).abs() < 1e-6,
        "decoded cam→strih p50 must be the known 180 ms offset, got {}",
        h.stats.p50_ms
    );
    assert!((h.stats.p99_ms - 180.0).abs() < 1e-6);
    assert!(h.jitter_ms.abs() < 1e-6, "constant offset = zero jitter");
    assert!(
        h.drift_ms_per_min.abs() < 1e-6,
        "constant offset = zero drift"
    );
}
