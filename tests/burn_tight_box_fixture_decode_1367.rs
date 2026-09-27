//! issue 1367 regression LOCK — the slot recovery must read a crisp own-slot burn that the fixed
//! slot crop misses.
//!
//! THE BUG: cam2 films the strih-lx HDMI multiview. In the strih recording its capture burn box is
//! about 286 px instead of 320, and its centre sits about 16 px below the camera slot centre. The
//! issue-1370 slot crop (slot + 8 px pad) then also holds a strip of the multiview's QR content
//! along its left and top edges, and rqrr's single grid pass over it reads nothing. On the release
//! E2E (run 324220913) that happened on 7 frames, which the verdict counted as cam2
//! `BURN-UNREADABLE`, while the burn sits crisply in its own slot.
//!
//! THE FIX (issue 1367, design v2): when the 1x slot crop reads no burn of the slot, the recovery
//! finds the burn's own white quiet-zone box inside the crop (`camera_box::burn_quiet_zone`),
//! decodes exactly that box inside a white border, and keeps only a read of a missing id inside the
//! slot, like every other pass.
//!
//! THE FIXTURES are real 1920x1080 grayscale strih-recording frames of run 324220913, committed
//! verbatim from the run's pixel-proof retention (`cam2-missing/frame-{1775,2008,8150}.png` on
//! dev1). Anchors, all on dev1:
//!   * the run's production decode (`strih-partial-324220913.json`, unpinned, mandatory strih,
//!     any-of the seven cameras) carries strih's burn and the optical reads but NO 911009 on these
//!     three frames, while the neighbours carry it (1774 -> 43307, 1776 -> 43311);
//!   * the real rqrr 0.9.3 (plain + the production Otsu) reads nothing from the camera recovery
//!     crop (792, 728, 336 x 336) of these frames, and reads the payloads asserted below from the
//!     tight white box (frame 816, 769, 286 x 287) inside a 31 px white border, centred at
//!     (963, 916);
//!   * `zbarimg -q --raw` reads the same 2008 and 8150 payloads from the full frames (it reads
//!     nothing on 1775).
//!
//! Caveat, as in the issue-1370 lock: the PNGs are ffmpeg's decode of the recorded frames, not the
//! verdict's own Y plane. The production 1x crop misses on both, so the RED is anchored on the
//! run's partial, never on a resized replica.

#![cfg(feature = "probe")]

use camera_box::burn_regions::{recovery_crop, BurnSlot};
use camera_box::probe::burn_region_decode::burn_region_passes;
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical, decode_qr_luma_all_reads,
    DecodePath,
};
use camera_box::probe::recording_latency::{
    AUX_TICK_RUN_ID, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use image::GrayImage;
use std::path::PathBuf;

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

/// One real frame and cam2's burn on it (the full payload the real rqrr reads from the tight box).
struct Case {
    file: &'static str,
    cam2: Payload,
}

fn cases() -> [Case; 3] {
    let cam2 = |frame_id, gen_ts_ns| Payload {
        run_id: BURN_RUN_ID_CAM2,
        frame_id,
        gen_ts_ns,
    };
    [
        Case {
            file: "strih-324220913-frame-1775.png",
            cam2: cam2(43309, 1_790_467_872_332_891_638),
        },
        Case {
            file: "strih-324220913-frame-2008.png",
            cam2: cam2(43775, 1_790_467_880_086_585_221),
        },
        Case {
            file: "strih-324220913-frame-8150.png",
            cam2: cam2(56060, 1_790_468_084_834_987_911),
        },
    ]
}

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "burn-tight-box-1367",
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

/// Precondition (the bug condition, from real pixels): the fixed camera recovery crop, plain and
/// Otsu, reads no burn at all on these frames.
#[test]
fn the_fixed_camera_slot_crop_reads_nothing_on_these_frames_1367() {
    for case in cases() {
        let luma = fixture_luma(case.file);
        let r = recovery_crop(BurnSlot::CameraCapture, luma.width(), luma.height()).unwrap();
        let crop = image::imageops::crop_imm(&luma, r.x, r.y, r.w, r.h).to_image();
        let reads = decode_qr_luma_all_reads(crop);
        assert!(
            reads.is_empty(),
            "{}: precondition — the 1x camera slot crop reads nothing (the production decode of \
             these pixels had no cam2 burn); got {:?}",
            case.file,
            reads
        );
    }
}

/// THE issue-1367 lock at the recovery pass: with every camera id missing, the camera slot
/// recovery reads cam2's crisp burn — the exact payload — and adds nothing else.
#[test]
fn the_slot_recovery_reads_the_shrunk_lower_cam2_burn_1367() {
    for case in cases() {
        let mut out = Vec::new();
        burn_region_passes(&fixture_luma(case.file), &CAMERA_IDS, &mut out);
        assert_eq!(
            out,
            vec![case.cam2],
            "{}: the recovery must read exactly cam2's own-slot burn {:?}; got {:?}",
            case.file,
            case.cam2,
            ids(&out)
        );
    }
}

/// The production strih extract decode of these frames now carries cam2's burn, and everything
/// else it reads is strih's burn, the optical run, or the aux marks — never another camera id.
#[test]
fn production_strih_decode_reads_the_cam2_burn_1367() {
    for case in cases() {
        let (got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
            fixture_luma(case.file),
            &[BURN_RUN_ID_STRIH],
            &CAMERA_IDS,
            None,
        );
        assert_eq!(
            path,
            DecodePath::Robust,
            "{}: no camera burn is in the plain pass, so the robust fallback must run",
            case.file
        );
        assert!(
            got.contains(&case.cam2),
            "{}: the crisp cam2 burn {:?} must decode — a present digital burn is never \
             BURN-UNREADABLE; got {:?}",
            case.file,
            case.cam2,
            ids(&got)
        );
        assert!(
            got.iter().any(|p| p.run_id == BURN_RUN_ID_STRIH),
            "{}: strih's burn still decodes; got {:?}",
            case.file,
            ids(&got)
        );
        for p in &got {
            assert!(
                p.run_id == BURN_RUN_ID_CAM2
                    || p.run_id == BURN_RUN_ID_STRIH
                    || p.run_id == AUX_TICK_RUN_ID
                    || p.run_id == 324_220_913,
                "{}: unexpected payload {p:?} in {:?}",
                case.file,
                ids(&got)
            );
        }
    }
}

/// Guard: a slot that holds no burn reads nothing. A strih recording carries no stream burn, and
/// its bottom-right slot shows only the multiview; the recovery adds nothing there.
#[test]
fn a_slot_without_a_burn_adds_nothing_1367() {
    for case in cases() {
        let mut out = Vec::new();
        burn_region_passes(&fixture_luma(case.file), &[BURN_RUN_ID_STREAM], &mut out);
        assert!(
            out.is_empty(),
            "{}: the stream slot of a strih recording holds no burn; got {:?}",
            case.file,
            ids(&out)
        );
    }
}
