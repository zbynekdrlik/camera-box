//! issue 1196 — the mined REAL-captured-frame AUX FIXTURE (promotion precondition 1 of
//! `.claude/rules/projection-tap-tear-detect.md`, per `pattern-change-needs-decode-fixture`).
//!
//! The aux Vernier tick pair (two small ~210px QRs painted into the bottom burn-free gaps,
//! `src/aux_tick.rs`) is a NEW painted element — the synthetic painter round-trip
//! (`aux_tick_pair_round_trips_alongside_the_dual_qr_1196` in `src/probe/painter.rs`) landed with
//! the pattern change, but a crisp synthetic canvas proves nothing about the REAL lossy chain
//! (painted monitor → physical camera → HDMI splitter → cambox grabber → 2×NDI hops → the stream
//! box's 4K upscale → mp4 recording). THIS file is that second, real-chain proof layer: the
//! committed fixtures under `tests/fixtures/tear-781/` are real 1920×1080 grayscale frames from
//! E2E run 2099068429's stream recording (the first green run with decodable aux content:
//! aux_decode_fraction 0.60–0.82 across all 10 windows, multi_path_suspect 0.0, single-tile) —
//! `stream-2099068429-frame-1399.png` and `stream-2099068429-frame-4792.png`, two frames from two
//! DIFFERENT windows of the run, taken verbatim from the box-side pixel-proof retention
//! (`stream-partial-2099068429-pixels/`). For these exact pixels the run's own partial
//! (`stream-partial-2099068429.json`) carries both aux payloads, and zbar independently reproduces
//! them (full-frame AND from the two aux design rectangles alone) — so a decode miss here is a
//! DECODER regression, never a pixel problem (the #186/#921 discriminator).
//!
//! Provenance note (honest scope): run 2099068429 was an all-CAM3-window run — these frames come
//! from cam3's grabber leg of the splitter rig, which traverses the full optical + 2×NDI + 4K +
//! mp4 chain but not the imag-projection HDMI hop (cam2's leg). They prove the small-QR
//! chain-survival question the fixture exists for; the projection-leg (cam2-window) confirmation
//! stays inside the remaining promotion preconditions, and the tear gate stays REPORT-ONLY
//! (`tear_detect::gates_overall_pass()` = false) regardless of this fixture.
//!
//! What is pinned, per fixture frame:
//!   * the PRODUCTION stream-extraction decode shape (`decode_qr_luma_all_fast_then_robust_
//!     grouped_optical` with the stream box's exact #632/#707 gate) reads BOTH aux payloads
//!     (`AUX_TICK_RUN_ID`, `gen_ts_ns = 0`) with `frame_id`s EQUAL to the primary dual-QR pair's —
//!     the Vernier mirror property;
//!   * the plain OFFLINE robust decode (`decode_qr_luma_all_robust`) recovers the same aux pair —
//!     the aux marks sit inside the #202 bottom band, so their decodability must not depend on any
//!     particular fast-path gate configuration;
//!   * the decoded ids flow through `tear_detect` v2.1 as clean SINGLE-SOURCE content: not
//!     multi-path suspect, not torn, union span exactly the Vernier adjacency, full aux coverage.

#![cfg(feature = "probe")]

use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{
    decode_qr_luma_all_fast_then_robust_grouped_optical, decode_qr_luma_all_robust,
};
use camera_box::probe::recording_latency::{
    AUX_TICK_RUN_ID, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use camera_box::tear_detect::{
    frame_cluster_count, is_multi_path_suspect, is_torn_frame, window_tear_stats,
    TearSignalViability, VERNIER_MAX_SPREAD,
};
use image::GrayImage;
use std::path::PathBuf;

/// The painted primary (cam2 dual-QR Vernier) run_id on this content — the E2E run id itself
/// (`RUN_ID=2099068429`), which the painter uses as the optical run_id. A committed real frame's
/// payloads are immutable history, so pinning the literal is correct here (unlike the burn ids,
/// which come from the single source of truth so a default change can never stale them).
const PRIMARY_RUN_ID: u32 = 2099068429;

/// Each committed fixture + the exact adjacent Vernier tick pair BOTH the primary dual-QR band
/// and the bottom aux pair carry in it (verified against the run's own partial and independently
/// via zbar — see the module doc).
const CASES: &[(&str, [u32; 2])] = &[
    ("stream-2099068429-frame-1399.png", [10439, 10440]),
    ("stream-2099068429-frame-4792.png", [17223, 17224]),
];

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

/// The PRODUCTION stream-box extraction decode, exactly as `--extract-partial stream` runs it
/// (recording-verdict.rs: mandatory = the strih + stream hop burns, any-of = the seven
/// camera-under-test capture-burn ids, optical = both Vernier halves of the pinned cam2 run_id).
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

/// The `(run_id, frame_id)` pairs decoded for one run_id, sorted.
fn ids_for(payloads: &[Payload], run_id: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = payloads
        .iter()
        .filter(|p| p.run_id == run_id)
        .map(|p| p.frame_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// The production decode reads BOTH aux tick marks — with the Vernier mirror property (aux ids ==
/// the primary dual-QR pair's ids) and the wire-format constant `gen_ts_ns = 0` — from every
/// committed real frame. This is the real-chain decodability proof the promotion preconditions
/// require (a synthetic canvas cannot provide it).
#[test]
fn production_decode_reads_both_aux_ticks_from_every_real_frame() {
    for &(name, expected) in CASES {
        let payloads = production_stream_decode(fixture_luma(name));
        let aux = ids_for(&payloads, AUX_TICK_RUN_ID);
        assert_eq!(
            aux, expected,
            "{name}: the aux tick pair must decode exactly {expected:?}; got aux {aux:?} \
             (all payloads: {payloads:?})"
        );
        for p in payloads.iter().filter(|p| p.run_id == AUX_TICK_RUN_ID) {
            assert_eq!(
                p.gen_ts_ns, 0,
                "{name}: an aux mark always carries the constant gen_ts_ns = 0 (issue 1196 wire \
                 format); got {p:?}"
            );
        }
        let primary = ids_for(&payloads, PRIMARY_RUN_ID);
        assert_eq!(
            primary, expected,
            "{name}: the primary dual-QR pair must decode the SAME adjacent tick pair the aux \
             mirrors; got primary {primary:?}"
        );
    }
}

/// The plain offline robust decode (`decode_qr_luma_all_robust` — full-frame + the #202 bottom
/// band tiles, no fast-path gate involved) recovers the same aux pair: the aux marks live inside
/// the bottom band, so their decodability must not hinge on any particular gate configuration.
#[test]
fn plain_robust_decode_also_recovers_the_aux_pair() {
    for &(name, expected) in CASES {
        let payloads = decode_qr_luma_all_robust(fixture_luma(name));
        let aux = ids_for(&payloads, AUX_TICK_RUN_ID);
        assert_eq!(
            aux, expected,
            "{name}: decode_qr_luma_all_robust must recover the aux pair {expected:?}; got \
             {aux:?}"
        );
    }
}

/// The decoded real-frame ids flow through the `tear_detect` v2.1 classifier as clean
/// SINGLE-SOURCE content: one cluster, not multi-path suspect, not torn, union span exactly the
/// Vernier adjacency — and the two-frame window reports FULL aux coverage with zero suspects and
/// `Unproven` viability (healthy content never fabricates an `Observed`).
#[test]
fn real_frames_flow_through_tear_detect_as_clean_single_source() {
    let mut per_frame: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
    for &(name, _) in CASES {
        let payloads = production_stream_decode(fixture_luma(name));
        let primary = ids_for(&payloads, PRIMARY_RUN_ID);
        let aux = ids_for(&payloads, AUX_TICK_RUN_ID);
        assert!(
            !is_multi_path_suspect(&primary, &aux),
            "{name}: a single-tile real frame must never read multi-path suspect \
             (primary {primary:?}, aux {aux:?})"
        );
        assert_eq!(
            frame_cluster_count(&primary),
            1,
            "{name}: one tile's dual-QR band = one cluster"
        );
        assert!(
            !is_torn_frame(&primary, &aux),
            "{name}: healthy content must not read torn (primary {primary:?}, aux {aux:?})"
        );
        per_frame.push((primary, aux));
    }
    let stats = window_tear_stats(&per_frame);
    assert_eq!(stats.total_frames, CASES.len() as u32);
    assert_eq!(stats.decodable_frames, CASES.len() as u32);
    assert_eq!(stats.tear_frames, 0, "no tear on healthy real frames");
    assert_eq!(stats.tear_fraction, 0.0);
    assert_eq!(
        stats.max_spread, VERNIER_MAX_SPREAD,
        "each frame's primary∪aux union spans exactly the even/odd Vernier adjacency"
    );
    assert_eq!(
        stats.aux_decode_fraction, 1.0,
        "both aux marks decode on every committed frame — the chain-survival coverage this \
         fixture exists to prove"
    );
    assert_eq!(stats.primary_dark_aux_alive_fraction, 0.0);
    assert_eq!(stats.multi_path_suspect_frames, 0);
    assert_eq!(stats.multi_path_suspect_fraction, 0.0);
    assert_eq!(stats.max_cluster_count, 1);
    assert_eq!(stats.max_multi_path_spread, 0);
    assert_eq!(
        stats.viability,
        TearSignalViability::Unproven,
        "healthy content must never fabricate an Observed viability"
    );
}
