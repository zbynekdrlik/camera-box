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
use camera_box::probe::recording::{decode_recording_frame, decode_recording_frame_with_burns};
use camera_box::probe::recording_latency::{
    cam_strih_samples, hop_latency, per_frame_latency_csv_rows, split_payloads,
    strih_stream_samples, RunIds, BURN_RUN_ID_CAM1, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use std::collections::HashMap;

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
        // #423: this frame carries ONLY the strih node burn (never cam1/stream), so
        // `decode_recording_frame`'s default full-`NODE_BURN_RUN_IDS` gate could never
        // be satisfied and every frame gratuitously ran the ~10×-cost robust tile
        // recovery chasing burns that structurally cannot be here — the dominant cost
        // behind this test's multi-second-per-frame runtime. Passing the burn this
        // frame ACTUALLY carries (mirroring what a real strih recording passes in
        // production, `analyze_recording_with_burns`) lets the cheap fast path fire.
        let rf = decode_recording_frame_with_burns(i, luma, &[BURN_RUN_ID_STRIH]);
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
        // #423: only the strih burn is actually rendered here — see the note on the
        // decode call below (mirrors the fast-gate fix for the parallel_decode CI
        // slow-timeout, module doc-comment in src/probe/recording.rs).
        let srf = decode_recording_frame_with_burns(i, strih_luma, &[BURN_RUN_ID_STRIH]);
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
        // renders 35 ms after strih. Only the stream node burn is rendered into this
        // frame (`frame_with_dual_cam2_and_node` takes one `node` payload) — no strih
        // burn is actually present here.
        let stream_node = Payload {
            run_id: BURN_RUN_ID_STREAM,
            frame_id: 9000 + i as u32,
            gen_ts_ns: cam_g + cam_strih_off + strih_stream_off,
        };
        // swap halves to simulate non-identical rqrr grid order across the two MKVs.
        let stream_luma =
            frame_with_dual_cam2_and_node(&cam2_odd, &cam2_even, &stream_node, w, half_h, qr);
        // #423: only the stream burn is actually rendered here (see the strih call
        // above) — `decode_recording_frame`'s full-`NODE_BURN_RUN_IDS` default gate
        // could never be satisfied by a frame carrying just one node burn, so this
        // test paid the ~10×-cost robust tile recovery on every frame for nothing.
        let strf = decode_recording_frame_with_burns(i + 7, stream_luma, &[BURN_RUN_ID_STREAM]);
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

/// Composite a recorded stream frame carrying the three co-located node burns (cam1, strih,
/// stream) and OPTIONALLY the cam2 optical QR — all decoded through the REAL rqrr path. When
/// `cam2` is `None`, NO cam2 QR is rendered: that is an OPTICAL-DECODE DROPOUT (cam1 filming
/// cam2's monitor momentarily fails to read the QR) while the DIGITAL burns are still present.
fn stream_frame_with_burns_and_optional_cam2(
    cam2: Option<&Payload>,
    cam1: &Payload,
    strih: &Payload,
    stream: &Payload,
    w: u32,
    quarter_h: u32,
    qr: u32,
) -> image::GrayImage {
    // Four stacked quarter-canvases: [cam2-or-blank | cam1 | strih | stream], each centering
    // one QR (the optical dropout quarter is left blank = no decodable QR there).
    let blank = vec![255u8; (w * quarter_h * 4) as usize];
    let q_cam2 = match cam2 {
        Some(p) => render_qr_bgra(p, w, quarter_h, qr),
        None => blank.clone(),
    };
    let q_cam1 = render_qr_bgra(cam1, w, quarter_h, qr);
    let q_strih = render_qr_bgra(strih, w, quarter_h, qr);
    let q_stream = render_qr_bgra(stream, w, quarter_h, qr);
    let h = quarter_h * 4;
    let mut full = vec![255u8; (w * h * 4) as usize];
    let qb = (w * quarter_h * 4) as usize;
    full[..qb].copy_from_slice(&q_cam2);
    full[qb..qb * 2].copy_from_slice(&q_cam1);
    full[qb * 2..qb * 3].copy_from_slice(&q_strih);
    full[qb * 3..qb * 4].copy_from_slice(&q_stream);
    bgra_to_luma(&full, w, h, w * 4)
}

#[test]
fn per_frame_csv_keeps_burn_hops_unbroken_across_real_optical_dropout_216() {
    // #216 end-to-end on REAL pixels: a stretch of recorded frames whose cam2 OPTICAL QR is
    // undecodable (an optical-decode dropout — NOT a chain loss) while the cam1/strih/stream
    // burns are present the whole time. The per-frame latency CSV must emit a row for EVERY
    // such frame so the cam1→strih / strih→stream / cam1→stream lines stay UNBROKEN (the
    // ~150s blank band #216 reports was caused by skipping these rows). Only the cam2-optical
    // identity column is empty on a dropout row.
    let base = 1_700_000_000_000_000_000i64;
    let cam1_strih_off = 60_000_000i64; // +60 ms
    let strih_stream_off = 59_000_000i64; // +59 ms (so cam1→stream end-to-end = 119 ms)
    let (w, quarter_h, qr) = (900u32, 360u32, 300u32);

    // 6 frames: cam2 present on 0 and 5; frames 1..=4 are the optical dropout (no cam2 QR),
    // all six carrying the three burns (the chain stayed alive across the dropout).
    let cam2_present = [true, false, false, false, false, true];
    let mut frames = Vec::new();
    for i in 0..6u64 {
        let cam_g = base + i as i64 * 33_333_333;
        let cam2 = Payload {
            run_id: CAM2_RUN_ID,
            frame_id: 700 + i as u32,
            gen_ts_ns: cam_g,
        };
        let cam1 = Payload {
            run_id: BURN_RUN_ID_CAM1,
            frame_id: 2000 + i as u32,
            gen_ts_ns: cam_g,
        };
        let strih = Payload {
            run_id: BURN_RUN_ID_STRIH,
            frame_id: 5000 + i as u32,
            gen_ts_ns: cam_g + cam1_strih_off,
        };
        let stream = Payload {
            run_id: BURN_RUN_ID_STREAM,
            frame_id: 9000 + i as u32,
            gen_ts_ns: cam_g + cam1_strih_off + strih_stream_off,
        };
        let cam2_opt = if cam2_present[i as usize] {
            Some(&cam2)
        } else {
            None
        };
        let luma = stream_frame_with_burns_and_optional_cam2(
            cam2_opt, &cam1, &strih, &stream, w, quarter_h, qr,
        );
        let rf = decode_recording_frame(i, luma);
        // All three burns must decode out of the real pixels on every frame.
        let n_burns = rf
            .payloads
            .iter()
            .filter(|p| {
                matches!(
                    p.run_id,
                    BURN_RUN_ID_CAM1 | BURN_RUN_ID_STRIH | BURN_RUN_ID_STREAM
                )
            })
            .count();
        assert_eq!(
            n_burns, 3,
            "frame {i}: all three burns must decode (the chain is alive across the dropout)"
        );
        // The dropout frames (1..=4) must have NO decodable cam2 QR.
        let has_cam2 = rf.payloads.iter().any(|p| p.run_id == CAM2_RUN_ID);
        assert_eq!(
            has_cam2, cam2_present[i as usize],
            "frame {i}: cam2 QR decodability must match the rendered layout"
        );
        frames.push(rf);
    }

    let rows = per_frame_latency_csv_rows(
        &frames,
        BURN_RUN_ID_CAM1,
        BURN_RUN_ID_STRIH,
        BURN_RUN_ID_STREAM,
        Some(CAM2_RUN_ID),
        &HashMap::new(),
    );
    // #216: ALL six frames emit a row — the four dropout frames are NOT skipped.
    assert_eq!(
        rows.len(),
        6,
        "#216: every burn-carrying frame emits a row incl. the optical-dropout stretch"
    );
    for (i, r) in rows.iter().enumerate() {
        assert!(
            (r.cam1_strih_ms.unwrap() - 60.0).abs() < 1e-6,
            "row {i}: cam1→strih = +60 ms (burn-only hop, present across the real dropout)"
        );
        assert!(
            (r.strih_stream_ms.unwrap() - 59.0).abs() < 1e-6,
            "row {i}: strih→stream = +59 ms"
        );
        assert!(
            (r.cam1_stream_ms.unwrap() - 119.0).abs() < 1e-6,
            "row {i}: cam1→stream = +119 ms end-to-end"
        );
        assert!(r.gen_ts_ns > 0, "row {i}: positive x anchor");
    }
    // The four dropout rows carry NO cam2 tick (empty frame_id), the two healthy rows do.
    assert_eq!(rows[0].frame_id, Some(700));
    assert_eq!(
        rows[1].frame_id, None,
        "#216: dropout row has empty frame_id but still a row"
    );
    assert_eq!(rows[2].frame_id, None);
    assert_eq!(rows[3].frame_id, None);
    assert_eq!(rows[4].frame_id, None);
    assert_eq!(rows[5].frame_id, Some(705));
}
