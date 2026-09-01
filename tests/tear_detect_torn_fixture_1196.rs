//! issue 1196 — the mined REAL-captured-frame TORN fixture (promotion precondition 2 of
//! `.claude/rules/projection-tap-tear-detect.md`, per `pattern-change-needs-decode-fixture`).
//!
//! The sibling `aux_tick_fixture_decode_1196.rs` proves the aux marks DECODE through the real chain
//! on a HEALTHY run. THIS file proves the other half the LIVE gate flip needs: that a REAL scanout
//! tear is DETECTED as torn on real pixels, and a real HEALTHY frame is not. Frames come verbatim
//! from the known-torn calibration run 1700989544's box-side pixel-proof retention
//! (`stream-partial-1700989544-pixels/`; imag projector vsync disabled off-air → a deterministic
//! un-vsynced projection tear on the CAM2 leg). For each, the run's own partial
//! (`stream-partial-1700989544.json`) — the REAL rqrr output for those exact pixels — is the ground
//! truth, and `zbarimg` independently reproduces the AUX mark (see below).
//!
//! WHAT THE KNOWN-TORN RUN PROVED (per-frame mining, recorded in the ticket + module doc):
//! the PRIMARY dual-QR span is ALWAYS <= 1 (the primary band is structurally blind to a tear —
//! a horizontal seam corrupts both halves at the same height). EVERY torn frame is a
//! `primary[X, X+1]` pair + exactly ONE aux mark `[Y > X+1]` from a later generation: the bottom
//! aux band, scanned out later, catches the newer generation. So the AUX SINGLE-MARK CROSS-BAND is
//! the operative tear signal on the projection leg — NOT the primary band — and dropping the aux
//! from the union would make the gate blind. `zbarimg --raw` on `frame-8090` independently reads
//! the aux mark `37781` (one generation ahead of the primary pair 37779/37780 the rqrr partial
//! recorded), the cross-band evidence; zbar (a weaker single-pass decoder) does not recover the
//! torn primary on any of these frames, so the primary ground truth is the rqrr partial (the same
//! `decode_qr_luma_all_fast_then_robust_grouped_optical` this test calls).
//!
//! HONEST provenance: `frame-8090` is the ONLY torn frame in the run's pixel-proof retention (the
//! retention flags a bounded set of copies/gaps-suspect frames), but it is the EXACT pixel the
//! verdict decoded as torn, and the two healthy contrast frames are the retained frames adjacent to
//! the run's larger torn CAM2 window. The synthetic cross-band logic is covered exhaustively in
//! `src/tear_detect.rs`'s own unit tests; this file is the real-chain proof for the ONE torn pixel
//! the retention preserved.

#![cfg(feature = "probe")]

use camera_box::probe::payload::Payload;
use camera_box::probe::qr::decode_qr_luma_all_fast_then_robust_grouped_optical;
use camera_box::probe::recording_latency::{
    AUX_TICK_RUN_ID, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use camera_box::tear_detect::{is_torn_frame, window_tear_stats, TearSignalViability};
use image::GrayImage;
use std::path::PathBuf;

/// The painted primary (cam2 dual-QR Vernier) run_id on this content — the E2E run id itself.
/// A committed real frame's payloads are immutable history, so pinning the literal is correct.
const PRIMARY_RUN_ID: u32 = 1700989544;

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "tear-781",
        name,
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

/// The PRODUCTION stream-box extraction decode, exactly as `--extract-partial stream` ran it for
/// run 1700989544 (mandatory = strih+stream hop burns, any-of = the seven camera capture-burn ids,
/// optical = both Vernier halves of the pinned cam2 run_id). This is the SAME function whose output
/// IS the run's partial, so it reproduces the partial's ids for these exact pixels.
fn production_stream_decode(luma: GrayImage) -> Vec<Payload> {
    decode_qr_luma_all_fast_then_robust_grouped_optical(
        luma,
        &[BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        &[
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_CAM2,
            BURN_RUN_ID_CAM3,
            BURN_RUN_ID_CAM4,
            BURN_RUN_ID_CAM5,
            BURN_RUN_ID_CAM6,
            BURN_RUN_ID_CAM7,
        ],
        Some((PRIMARY_RUN_ID, 2)),
    )
}

fn ids_for(payloads: &[Payload], run_id: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = payloads
        .iter()
        .filter(|p| p.run_id == run_id)
        .map(|p| p.frame_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// The REAL torn frame: the production decode reads the primary dual-QR pair (37779, 37780) AND one
/// aux mark from a LATER generation (37781); `tear_detect` flags it TORN via the primary∪aux union
/// span (> the even/odd Vernier adjacency). This is the real-pixel proof that the aux single-mark
/// cross-band detects a genuine scanout tear on the projection leg — the operative signal the LIVE
/// gate keys on.
#[test]
fn real_torn_frame_is_detected_via_the_aux_cross_band_1196() {
    let payloads = production_stream_decode(fixture_luma("stream-1700989544-frame-8090-torn.png"));
    let primary = ids_for(&payloads, PRIMARY_RUN_ID);
    let aux = ids_for(&payloads, AUX_TICK_RUN_ID);
    assert_eq!(
        primary,
        vec![37779, 37780],
        "the primary dual-QR pair (rqrr ground truth from the run's partial); got {primary:?} \
         (all payloads: {payloads:?})"
    );
    assert_eq!(
        aux,
        vec![37781],
        "exactly ONE aux mark, one generation AHEAD of the primary pair — the scanout tear caught \
         by the bottom aux band; got {aux:?}"
    );
    // The primary band alone spans only the even/odd adjacency (1) — blind; the union with the
    // later aux mark spans 2 -> a real cross-band scanout tear.
    assert!(
        is_torn_frame(&primary, &aux),
        "the primary∪aux union must read TORN (primary {primary:?}, aux {aux:?})"
    );
    let stats = window_tear_stats(&[(primary, aux)]);
    assert_eq!(
        stats.tear_frames, 1,
        "the real torn frame counts as one tear"
    );
    assert_eq!(stats.viability, TearSignalViability::Observed);
    assert_eq!(
        stats.multi_path_suspect_frames, 0,
        "a single-tile projection-leg frame is never a multi-path suspect"
    );
    assert!(
        (stats.aux_any_decode_fraction - 1.0).abs() < 1e-9,
        "the aux single mark decoded (the operative-signal diagnostic)"
    );
    assert_eq!(
        stats.aux_decode_fraction, 0.0,
        "only ONE aux mark decoded — the both-mark metric reads 0.0 on the projection leg while \
         the tear is still detected via the single-mark cross-band"
    );
}

/// Two REAL healthy frames from the same run: the production decode reads the primary pair + a
/// single IN-SYNC aux mark (same generation), so the union span is exactly the Vernier adjacency
/// and `tear_detect` reads NOT torn / Unproven — the gate does not false-positive on healthy pixels.
#[test]
fn real_healthy_frames_are_not_torn_1196() {
    let cases: &[(&str, [u32; 2], u32)] = &[
        (
            "stream-1700989544-frame-8497-healthy.png",
            [38588, 38589],
            38589,
        ),
        (
            "stream-1700989544-frame-8498-healthy.png",
            [38589, 38590],
            38589,
        ),
    ];
    let mut per_frame: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
    for &(name, expected_primary, expected_aux) in cases {
        let payloads = production_stream_decode(fixture_luma(name));
        let primary = ids_for(&payloads, PRIMARY_RUN_ID);
        let aux = ids_for(&payloads, AUX_TICK_RUN_ID);
        assert_eq!(
            primary,
            expected_primary.to_vec(),
            "{name}: primary dual-QR pair (rqrr ground truth); got {primary:?}"
        );
        assert_eq!(
            aux,
            vec![expected_aux],
            "{name}: one in-sync aux mark; got {aux:?}"
        );
        assert!(
            !is_torn_frame(&primary, &aux),
            "{name}: an in-sync single aux mark must not read torn (primary {primary:?}, aux \
             {aux:?})"
        );
        per_frame.push((primary, aux));
    }
    let stats = window_tear_stats(&per_frame);
    assert_eq!(stats.tear_frames, 0, "no tear on real healthy frames");
    assert_eq!(stats.tear_fraction, 0.0);
    assert_eq!(
        stats.viability,
        TearSignalViability::Unproven,
        "healthy content must never fabricate an Observed viability"
    );
    assert!(
        (stats.aux_any_decode_fraction - 1.0).abs() < 1e-9,
        "a single aux mark decodes on each healthy frame too — aux operable, just in sync"
    );
}
