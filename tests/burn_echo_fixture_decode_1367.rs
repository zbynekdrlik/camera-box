//! issue 1367 regression LOCK — the recording decode must never accept an ECHO of a node burn.
//!
//! THE BUG: the owner re-cabled cam2's capture input to the strih-lx built-in HDMI, which shows
//! the vk-direct MULTIVIEW. cam2 now films a screen that holds smaller, still decodable copies of
//! the node burns inside the multiview cells (Preview, Program, the camera cells), cam2's OWN
//! earlier burns among them. The decode accepted a node run_id wherever rqrr found it. On the
//! release E2E (run 386740541) the echoes added stale cam2 ids (copies/gaps 145/151 and 51/53 on
//! the two CAM2 windows), and an echo pair even satisfied the fast-path gate, so the real burns
//! were never read.
//!
//! THE FIX (issue 1367): a node burn has ONE known place (`camera_box::burn_regions`). A read of
//! a slotted run_id counts only when its detected centre lies inside its own slot + pad; any other
//! read of it is an echo, dropped before it merges, before the fast-path gate and before the
//! optical-short check.
//!
//! THE FIXTURES are real 1920x1080 grayscale strih-recording frames of run 386740541, committed
//! verbatim from the run's pixel-proof retention (`cam2-missing/frame-{1521,7790}.png` on dev1).
//! Anchors, all on dev1:
//!   * the run's production decode (`strih-partial-386740541.json`) of frame 1521 is
//!     [optical 35275, aux 35284, 911008.17043, 911002.16602] — two echoes (Preview / Program
//!     cells) and neither real burn; frame 7790 carries the echo 911009.51761 (Program cell)
//!     beside the real 911009.51769;
//!   * the real rqrr (plain + Otsu) over the production passes reproduces those reads and places
//!     them: 17043 at (481,454), 16602 at (1061,442), 51761 at (1442,453) — all in the top half —
//!     while the real burns sit in their slots: cam2 at (963,916), strih at (194,893);
//!   * `zbarimg -q --raw` and OpenCV read exactly the in-slot payloads asserted below.
//!
//! The strih extract of that run passed no `--cam2-run-id`, so it decoded UNPINNED; the pinned
//! shape (the #707 optical gate) is covered too.

#![cfg(feature = "probe")]

use camera_box::burn_regions::slot_for_run_id;
use camera_box::probe::burn_echo::NodeBurnGate;
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{
    decode_qr_luma_all, decode_qr_luma_all_fast_then_robust_gated,
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical, DecodePath,
};
use camera_box::probe::recording_latency::{
    BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4, BURN_RUN_ID_CAM5,
    BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STRIH,
};
use image::GrayImage;
use std::path::PathBuf;

/// The run's cam2 optical run_id (its dual-QR Vernier payloads carry this).
const OPTICAL_RUN_ID: u32 = 386_740_541;

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

/// One real frame, its crisp in-slot burns (full payloads read from the PNG) and the echoes the
/// production decode accepted from it.
struct Case {
    file: &'static str,
    in_slot: [Payload; 2],
    echoes: &'static [(u32, u32)],
}

fn cases() -> [Case; 2] {
    [
        Case {
            file: "strih-386740541-frame-1521.png",
            in_slot: [
                Payload {
                    run_id: BURN_RUN_ID_CAM2,
                    frame_id: 39230,
                    gen_ts_ns: 1_790_388_703_695_341_713,
                },
                Payload {
                    run_id: BURN_RUN_ID_STRIH,
                    frame_id: 16607,
                    gen_ts_ns: 1_790_388_703_738_570_747,
                },
            ],
            echoes: &[(BURN_RUN_ID_CAM3, 17043), (BURN_RUN_ID_STRIH, 16602)],
        },
        Case {
            file: "strih-386740541-frame-7790.png",
            in_slot: [
                Payload {
                    run_id: BURN_RUN_ID_CAM2,
                    frame_id: 51769,
                    gen_ts_ns: 1_790_388_912_675_318_352,
                },
                Payload {
                    run_id: BURN_RUN_ID_STRIH,
                    frame_id: 22774,
                    gen_ts_ns: 1_790_388_912_704_567_831,
                },
            ],
            echoes: &[(BURN_RUN_ID_CAM2, 51761)],
        },
    ]
}

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "burn-echo-1367",
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

/// The production strih extract decode for one frame (`pinned` = the #707 optical gate on).
fn production_strih_decode(luma: GrayImage, pinned: bool) -> (Vec<Payload>, DecodePath) {
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        luma,
        &[BURN_RUN_ID_STRIH],
        &CAMERA_IDS,
        pinned.then_some((OPTICAL_RUN_ID, 2)),
    )
}

/// The slotted node burns of a decode result (the payloads the echo gate governs).
fn node_burns(payloads: &[Payload]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = payloads
        .iter()
        .filter(|p| slot_for_run_id(p.run_id).is_some())
        .map(|p| (p.run_id, p.frame_id))
        .collect();
    v.sort_unstable();
    v
}

/// THE issue-1367 lock: on both real frames, in both gate shapes, the node burns the decode
/// returns are EXACTLY the crisp in-slot burns (the exact payloads zbar reads) — every echo is
/// rejected.
#[test]
fn only_the_in_slot_burns_are_accepted_every_echo_rejected_1367() {
    for case in cases() {
        for pinned in [false, true] {
            let (got, _path) = production_strih_decode(fixture_luma(case.file), pinned);
            for echo in case.echoes {
                assert!(
                    !ids(&got).contains(echo),
                    "{} (pinned={pinned}): the echo {echo:?} lies outside its node's slot and must \
                     never count as that node's burn; got {:?}",
                    case.file,
                    ids(&got)
                );
            }
            for burn in &case.in_slot {
                assert!(
                    got.contains(burn),
                    "{} (pinned={pinned}): the crisp in-slot burn {burn:?} must decode; got {:?}",
                    case.file,
                    ids(&got)
                );
            }
            let mut want: Vec<(u32, u32)> = case
                .in_slot
                .iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect();
            want.sort_unstable();
            assert_eq!(
                node_burns(&got),
                want,
                "{} (pinned={pinned}): exactly the in-slot node burns, nothing else",
                case.file
            );
        }
    }
}

/// An echo no longer buys the FAST path: with the echoes gone, strih's mandatory burn is missing
/// from the full-frame pass, so the robust fallback runs and reads the real burns (on frame 1521
/// the unpinned production shape used to return FAST on two echoes).
#[test]
fn echoes_never_satisfy_the_fast_path_gate_1367() {
    for case in cases() {
        let (_got, path) = production_strih_decode(fixture_luma(case.file), false);
        assert_eq!(
            path,
            DecodePath::Robust,
            "{}: the full-frame and top-band passes read strih only as an echo, so the robust \
             fallback must run",
            case.file
        );
    }
}

/// The gate touches node burns only: every optical dual-QR and aux payload the plain full-frame
/// pass reads survives byte-identical.
#[test]
fn optical_and_aux_payloads_are_untouched_1367() {
    for case in cases() {
        let luma = fixture_luma(case.file);
        let plain = decode_qr_luma_all(luma.clone());
        let (got, _path) = production_strih_decode(luma, false);
        for p in plain.iter().filter(|p| slot_for_run_id(p.run_id).is_none()) {
            assert!(
                got.contains(p),
                "{}: the unslotted payload {p:?} must survive; got {:?}",
                case.file,
                ids(&got)
            );
        }
    }
}

/// The rejected echoes are reported (the count the verdict carries), and they are exactly what the
/// gate removed: with the gate OFF — the pre-issue-1367 decode, still what the per-frame test
/// helpers run — the same frame keeps the echoes, so it is the gate and nothing else that drops
/// them.
#[test]
fn the_gate_reports_the_echoes_it_rejected_and_off_keeps_them_1367() {
    for case in cases() {
        for pinned in [false, true] {
            let optical = pinned.then_some((OPTICAL_RUN_ID, 2));
            let gated = decode_qr_luma_all_fast_then_robust_gated(
                fixture_luma(case.file),
                &[BURN_RUN_ID_STRIH],
                &CAMERA_IDS,
                optical,
                NodeBurnGate::OwnSlot,
            );
            for echo in case.echoes {
                assert!(
                    ids(&gated.echoes).contains(echo),
                    "{} (pinned={pinned}): the echo {echo:?} must be counted as rejected; got {:?}",
                    case.file,
                    ids(&gated.echoes)
                );
            }
            for e in &gated.echoes {
                assert!(
                    slot_for_run_id(e.run_id).is_some(),
                    "{} (pinned={pinned}): only a slotted node burn is ever an echo: {e:?}",
                    case.file
                );
            }
            let off = decode_qr_luma_all_fast_then_robust_gated(
                fixture_luma(case.file),
                &[BURN_RUN_ID_STRIH],
                &CAMERA_IDS,
                optical,
                NodeBurnGate::Off,
            );
            assert!(
                off.echoes.is_empty(),
                "{}: Off never reports echoes",
                case.file
            );
            assert!(
                case.echoes.iter().any(|e| ids(&off.payloads).contains(e)),
                "{} (pinned={pinned}): without the gate the decode keeps an echo (the bug this \
                 gate fixes); got {:?}",
                case.file,
                ids(&off.payloads)
            );
        }
    }
}
