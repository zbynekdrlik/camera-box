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
use camera_box::probe::qr::{render_qr_bgra, render_qr_dual_bgra};
use camera_box::probe::recording::decode_recording_frame;
use camera_box::probe::recording_latency::{
    cam_strih_samples, hop_latency, split_payloads, strih_stream_samples, RunIds,
    BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};

const CAM2_RUN_ID: u32 = 6519; // representative cam2 run_id (outside the burn range)

fn strih_ids() -> RunIds {
    RunIds {
        node_burn: BURN_RUN_ID_STRIH,
        cam2: Some(CAM2_RUN_ID),
        other_burns: vec![],
    }
}

fn stream_ids() -> RunIds {
    RunIds {
        node_burn: BURN_RUN_ID_STREAM,
        cam2: Some(CAM2_RUN_ID),
        other_burns: vec![BURN_RUN_ID_STRIH],
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

/// Build one recorded-frame luma carrying BOTH cam2 Vernier halves (top, via the real
/// `render_qr_dual_bgra`: two side-by-side QRs, same run_id + gen_ts_ns, DIFFERENT
/// frame_id) and one node burn QR (bottom). This is the true dual-QR cam2 paint the
/// strih/stream PROBE scene records — the prior fixture rendered a single cam2 QR and
/// so never exercised the canonical-tick selection (finding #3).
fn frame_with_dual_cam2_and_node(
    cam2_even: &Payload,
    cam2_odd: &Payload,
    node: &Payload,
    w: u32,
    half_h: u32,
    qr: u32,
) -> image::GrayImage {
    let top = render_qr_dual_bgra(cam2_even, cam2_odd, w, half_h, qr); // two cam2 halves
    let bottom = render_qr_bgra(node, w, half_h, qr); // node burn centered in bottom half
    let h = half_h * 2;
    let mut full = vec![255u8; (w * h * 4) as usize];
    let top_bytes = (w * half_h * 4) as usize;
    full[..top_bytes].copy_from_slice(&top);
    full[top_bytes..top_bytes * 2].copy_from_slice(&bottom);
    bgra_to_luma(&full, w, h, w * 4)
}

#[test]
fn dual_vernier_cam2_real_pixels_canonical_tick_and_both_hops() {
    // The cam2 painter emits a DUAL-QR Vernier (two QRs/frame, same gen_ts_ns, different
    // frame_id). Decoding through the real rqrr path, the engine must (a) select the
    // canonical cam2 tick = max(frame_id), (b) recover the known cam→strih latency, and
    // (c) pair strih↔stream on the SAME optical instant even though the two recordings
    // decode the dual halves in different rqrr order (finding #1 + #3, on real pixels).
    let base = 1_700_000_000_000_000_000i64;
    let cam_strih_off = 160_000_000i64; // strih renders 160 ms after cam2 paint
    let strih_stream_off = 35_000_000i64; // stream renders 35 ms after strih
    let (w, half_h, qr) = (1280u32, 700u32, 560u32);

    let mut strih_frames = Vec::new();
    let mut stream_frames = Vec::new();
    for i in 0..4u64 {
        let cam_g = base + i as i64 * 33_333_333;
        // even tick 600+2i (LEFT), odd tick 601+2i (RIGHT) -> canonical = odd.
        let even_tick = 600 + 2 * i as u32;
        let odd_tick = 601 + 2 * i as u32;
        let cam2_even = Payload {
            run_id: CAM2_RUN_ID,
            frame_id: even_tick,
            gen_ts_ns: cam_g,
        };
        let cam2_odd = Payload {
            run_id: CAM2_RUN_ID,
            frame_id: odd_tick,
            gen_ts_ns: cam_g,
        };

        // strih burn: 160 ms after cam2 paint.
        let strih_node = Payload {
            run_id: BURN_RUN_ID_STRIH,
            frame_id: 5000 + i as u32,
            gen_ts_ns: cam_g + cam_strih_off,
        };
        // strih records the dual halves LEFT-then-RIGHT.
        let strih_luma =
            frame_with_dual_cam2_and_node(&cam2_even, &cam2_odd, &strih_node, w, half_h, qr);
        let srf = decode_recording_frame(i, strih_luma);
        // Both cam2 halves + the node burn must decode (≥3 payloads).
        assert!(
            srf.payloads.len() >= 3,
            "strih frame {i}: expected 2 cam2 halves + node, got {:?}",
            srf.payloads
        );
        // Canonical cam2 tick = the ODD (max frame_id) half.
        let (c, n) = split_payloads(&srf, &strih_ids());
        assert_eq!(
            c.unwrap().frame_id,
            odd_tick,
            "strih frame {i}: canonical cam2 tick must be max(frame_id)"
        );
        assert_eq!(n.unwrap().run_id, BURN_RUN_ID_STRIH);
        strih_frames.push(srf);

        // stream records the SAME cam2 halves but in SWAPPED order (RIGHT-then-LEFT) and
        // renders 35 ms after strih. The forwarded strih burn is present too (foreign).
        let stream_node = Payload {
            run_id: BURN_RUN_ID_STREAM,
            frame_id: 9000 + i as u32,
            gen_ts_ns: cam_g + cam_strih_off + strih_stream_off,
        };
        // swap halves to simulate non-identical rqrr grid order across the two MKVs.
        let stream_luma =
            frame_with_dual_cam2_and_node(&cam2_odd, &cam2_even, &stream_node, w, half_h, qr);
        let strf = decode_recording_frame(i + 7, stream_luma);
        assert!(
            strf.payloads.len() >= 3,
            "stream frame {i}: expected 2 cam2 halves + node, got {:?}",
            strf.payloads
        );
        let (sc, sn) = split_payloads(&strf, &stream_ids());
        assert_eq!(
            sc.unwrap().frame_id,
            odd_tick,
            "stream frame {i}: canonical cam2 tick must ALSO be max(frame_id) despite swapped order"
        );
        assert_eq!(sn.unwrap().run_id, BURN_RUN_ID_STREAM);
        stream_frames.push(strf);
    }

    // (b) cam→strih recovered from real pixels.
    let cs = cam_strih_samples(&strih_frames, &strih_ids());
    assert_eq!(cs.len(), 4);
    let cs_h = hop_latency("cam→strih", &cs).expect("non-empty cam→strih");
    assert!(
        (cs_h.stats.p50_ms - 160.0).abs() < 1e-6,
        "decoded cam→strih p50 must be 160 ms, got {}",
        cs_h.stats.p50_ms
    );

    // (c) strih→stream pairs on the canonical tick despite swapped decode order.
    let ss = strih_stream_samples(&strih_frames, &stream_frames, &strih_ids(), &stream_ids());
    assert_eq!(
        ss.len(),
        4,
        "all four shared canonical ticks must pair across the two real-pixel recordings"
    );
    let ss_h = hop_latency("strih→stream", &ss).expect("non-empty strih→stream");
    assert!(
        (ss_h.stats.p50_ms - 35.0).abs() < 1e-6,
        "decoded strih→stream p50 must be 35 ms, got {}",
        ss_h.stats.p50_ms
    );
    assert!(ss_h.jitter_ms.abs() < 1e-6, "constant offset = zero jitter");
}
