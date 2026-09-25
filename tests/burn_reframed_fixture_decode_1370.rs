//! issue 1370 regression LOCK — a crisp digital node burn must decode whatever the camera shows.
//!
//! THE BUG: on 25.9.2026 the physical camera's view of the cam2 monitor changed (framing and/or
//! shutter/ISO). The large OPTICAL dual-QR and the painted aux tick marks now reach down into the
//! bottom band, so the #202 bottom tile that holds the cam1 capture burn also holds optical finder
//! patterns. The release E2E (run 68573319) then flagged 280 `BURN-UNREADABLE` slots over all seven
//! cams while every one of those burns is a crisp overlay `zbarimg` reads from the same pixels.
//! rqrr's single `detect_grids` pass returns nothing for that tile, so the burn is lost with it.
//!
//! THE FIX (issue 1370): after the plain + tile passes, every EXPECTED node burn that is still
//! missing is decoded from an isolated crop of ITS OWN known overlay slot
//! (`camera_box::burn_regions`, the `burn_geom` corner table). A lone crisp QR in its own crop
//! cannot form a cross-code capstone group with the optical content, so the camera view no longer
//! decides whether the burn reads.
//!
//! THE FIXTURES are real 1920x1080 grayscale strih-recording frames of run 68573319, committed
//! verbatim from the run's own pixel-proof retention (`cam1-missing/frame-{355,532}.png` on dev1)
//! — the exact pixels the production decode consumed. Both anchors hold on dev1:
//!   * the run's production decode (`strih-partial-68573319.json`, frames 355 and 532) carries
//!     strih 911002 + the optical + the aux marks but NO 911001, while the neighbouring frames
//!     do carry it (`911001.10463` / `911001.10467` around 355);
//!   * `zbarimg -q --raw` reads the exact payloads asserted below from the same PNGs.
//!
//! The decode call is the production strih extract shape: mandatory = strih's own hop burn,
//! any-of = every camera-under-test id, optical gate pinned to the run's cam2 run_id (2 halves).

#![cfg(feature = "probe")]

use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{
    decode_qr_luma_all, decode_qr_luma_all_fast_then_robust_grouped_pathed_optical,
    decode_qr_luma_all_robust, DecodePath,
};
use camera_box::probe::recording_latency::{
    AUX_TICK_RUN_ID, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STRIH,
};
use image::GrayImage;
use std::path::PathBuf;

/// The run's cam2 optical run_id (its dual-QR Vernier payloads carry this).
const OPTICAL_RUN_ID: u32 = 68_573_319;

/// The camera-under-test any-of group, exactly the production strih extract's list.
const CAMERA_IDS: [u32; 7] = [
    BURN_RUN_ID_CAM1,
    BURN_RUN_ID_CAM2,
    BURN_RUN_ID_CAM3,
    BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5,
    BURN_RUN_ID_CAM6,
    BURN_RUN_ID_CAM7,
];

/// One real frame + the burns `zbarimg` reads from it (full payloads, from the PNG itself).
struct Case {
    file: &'static str,
    cam1: Payload,
    strih: Payload,
}

fn cases() -> [Case; 2] {
    [
        Case {
            file: "strih-68573319-frame-355.png",
            cam1: Payload {
                run_id: BURN_RUN_ID_CAM1,
                frame_id: 10465,
                gen_ts_ns: 1_790_334_425_204_564_191,
            },
            strih: Payload {
                run_id: BURN_RUN_ID_STRIH,
                frame_id: 17801,
                gen_ts_ns: 1_790_334_425_234_623_958,
            },
        },
        Case {
            file: "strih-68573319-frame-532.png",
            cam1: Payload {
                run_id: BURN_RUN_ID_CAM1,
                frame_id: 10818,
                gen_ts_ns: 1_790_334_431_090_293_836,
            },
            strih: Payload {
                run_id: BURN_RUN_ID_STRIH,
                frame_id: 17978,
                gen_ts_ns: 1_790_334_431_134_440_536,
            },
        },
    ]
}

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "burn-reframed-1370",
        name,
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

fn ids(payloads: &[Payload]) -> Vec<(u32, u32)> {
    payloads.iter().map(|p| (p.run_id, p.frame_id)).collect()
}

/// The production strih extract decode for one frame.
fn production_strih_decode(luma: GrayImage) -> (Vec<Payload>, DecodePath) {
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        luma,
        &[BURN_RUN_ID_STRIH],
        &CAMERA_IDS,
        Some((OPTICAL_RUN_ID, 2)),
    )
}

/// Precondition (the bug condition, from real pixels): the full-frame pass reads strih's burn
/// but MISSES the cam1 burn on these reframed frames, and so do the #202 bottom tiles.
#[test]
fn full_frame_pass_and_tiles_miss_the_reframed_cam1_burn_1370() {
    for case in cases() {
        let robust = decode_qr_luma_all_robust(fixture_luma(case.file));
        assert!(
            !robust.iter().any(|p| p.run_id == BURN_RUN_ID_CAM1),
            "{}: precondition — plain + the #202 tiles miss the cam1 burn (the production decode \
             of these exact pixels did); got {:?}",
            case.file,
            ids(&robust)
        );
        let full = decode_qr_luma_all(fixture_luma(case.file));
        assert!(
            full.contains(&case.strih),
            "{}: precondition — the full-frame pass reads strih's burn {:?}; got {:?}",
            case.file,
            case.strih,
            ids(&full)
        );
        assert!(
            !full.iter().any(|p| p.run_id == BURN_RUN_ID_CAM1),
            "{}: precondition — the full-frame pass misses the cam1 burn on the reframed view; \
             got {:?}",
            case.file,
            ids(&full)
        );
    }
}

/// THE issue-1370 lock: the production strih decode reads the crisp cam1 burn — the EXACT
/// payload zbar reads — whatever optical content the camera puts next to it.
#[test]
fn production_strih_decode_reads_the_cam1_burn_on_a_reframed_view_1370() {
    for case in cases() {
        let (got, path) = production_strih_decode(fixture_luma(case.file));
        assert_eq!(
            path,
            DecodePath::Robust,
            "{}: the cam1 burn is missing from the plain pass, so the robust fallback must run",
            case.file
        );
        assert!(
            got.contains(&case.cam1),
            "{}: the crisp cam1 burn {:?} must decode (zbar reads it from the same pixels) — a \
             present digital burn is never BURN-UNREADABLE; got {:?}",
            case.file,
            case.cam1,
            ids(&got)
        );
        assert!(
            got.contains(&case.strih),
            "{}: strih's burn {:?} still decodes; got {:?}",
            case.file,
            case.strih,
            ids(&got)
        );
    }
}

/// The recovery only ADDS: every payload the robust decode (plain ∪ Otsu ∪ tiles) already read
/// is returned byte-identical, and anything extra is an expected burn — never aux content the
/// tear detector reads by run_id. (An extra optical-run payload may come from the #754 top-band
/// pass the production path runs when the optical read is short; that pass is not this fix.)
#[test]
fn recovery_is_a_superset_of_the_robust_decode_that_adds_only_expected_burns_1370() {
    for case in cases() {
        let luma = fixture_luma(case.file);
        let robust = decode_qr_luma_all_robust(luma.clone());
        let (got, _path) = production_strih_decode(luma);
        for p in &robust {
            assert!(
                got.contains(p),
                "{}: robust payload {p:?} must survive byte-identical; got {:?}",
                case.file,
                ids(&got)
            );
        }
        for p in got
            .iter()
            .filter(|p| !robust.contains(p) && p.run_id != OPTICAL_RUN_ID)
        {
            assert!(
                CAMERA_IDS.contains(&p.run_id) || p.run_id == BURN_RUN_ID_STRIH,
                "{}: the recovery may only add an EXPECTED burn, never {p:?}",
                case.file
            );
        }
        assert_eq!(
            got.iter().filter(|p| p.run_id == AUX_TICK_RUN_ID).count(),
            robust
                .iter()
                .filter(|p| p.run_id == AUX_TICK_RUN_ID)
                .count(),
            "{}: no aux tick payload is ever added",
            case.file
        );
    }
}
