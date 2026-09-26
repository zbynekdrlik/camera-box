//! issue 1370 — the BURN-ISOLATED recovery pass of the recording decode.
//!
//! After the plain full-frame pass and the #202 bottom tiles
//! (`qr::decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`), every EXPECTED node burn
//! that is still missing is decoded from a crop of ITS OWN known overlay slot
//! ([`crate::burn_regions`], the `burn_geom` table) and nothing else.
//!
//! Root cause it fixes: a #202 tile holds a third of the whole bottom band. When the camera's
//! view of the cam2 monitor puts optical content there (the dual-QR, the painted aux marks),
//! rqrr's single `detect_grids` pass over that tile can come back empty, and the crisp burn inside
//! it is lost with it. That happened on 25.9.2026 after the camera view changed: run 68573319 had 280
//! `BURN-UNREADABLE` slots that `zbarimg` reads from the same pixels. A slot crop holds the burn,
//! its own white quiet zone and a few px of pad, so no foreign capstone can join it. Whether a
//! digital burn decodes no longer depends on where the camera points.
//!
//! Contract (the fast path and every already-read payload are untouched):
//! - It runs only on the ROBUST branch, and only when an expected burn is still missing after the
//!   tiles ([`missing_expected_burns`]). A clean frame never pays for it.
//! - It merges ONLY payloads of the missing run_ids the slot can hold ([`merge_payloads`]).
//!   So the result is a strict SUPERSET of the plain + tile result, byte-identical for everything
//!   already read. It never adds optical or aux content, which the tear and continuity metrics
//!   read by run_id.
//! - Each slot gets a 1x look, then a [`BURN_REGION_UPSCALE`]x CatmullRom look only when the 1x
//!   look read no burn of that slot at all (a slot carries one burn).
//! - A run_id without a reserved slot (the aux marks, SongPlayer, an operator override) is never
//!   localized, so it costs nothing here.
//! - issue 1367: each read is placed on the frame and kept only inside its own slot, like every
//!   other pass of the camera-chain decode (`probe::burn_echo`). The slot crop IS that
//!   acceptance region, so for the slot's own ids this can never reject; it holds the "no pass
//!   admits an echo" invariant by construction.
//!
//! It lives in its own module rather than in `qr.rs` (already over the ~1000-line budget). The
//! pure slot geometry is the Tier-0 [`crate::burn_regions`]; this is the thin probe-gated decode
//! glue plus the probe-gated pins of that table against the camera-burn writer (`probe::qr`) and
//! the reserved run_ids (`probe::recording_latency`).

use crate::burn_regions::{node_burn_in_own_slot, recovery_crop, recovery_slots, slot_for_run_id};
use crate::probe::burn_echo::{reads_in_frame, LocatedPayload};
use crate::probe::payload::Payload;
use crate::probe::qr::{decode_qr_luma_all_located, merge_payloads};
use crate::probe::recording_latency::{AUX_TICK_RUN_ID, BURN_RUN_ID_SONGPLAYER};
use image::GrayImage;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The upscale factor of the second, conditional look at a slot crop. The 1x crop already reads
/// every burn on the real run-68573319 frames. The 2x look (the #202 idea: bigger modules for
/// rqrr's finder) is a second chance for a softer burn, for example after a stream-recording hop.
pub const BURN_REGION_UPSCALE: u32 = 2;

/// Process-wide count of node-burn payloads this pass read that the plain + tile passes had
/// missed. Verdict LOG only, like `qr::decode_path_counts`; tests assert on payloads instead.
static BURN_REGION_RECOVERIES: AtomicU64 = AtomicU64::new(0);

/// Node burns recovered by the burn-isolated slot crops since process start. A large value marks a
/// run whose camera view pushes optical content into the burn tiles.
pub fn burn_region_recovery_count() -> u64 {
    BURN_REGION_RECOVERIES.load(Ordering::Relaxed)
}

/// The expected burn run_ids STILL absent from `payloads`, by the #207/#632 fast-path gate's own
/// semantics: every missing MANDATORY id, plus the whole ANY-OF group when none of its members
/// decoded (the deployed camera is one of them, but which one is unknown). Empty ⇔ the burn half of
/// the gate holds.
pub fn missing_expected_burns(
    payloads: &[Payload],
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
) -> Vec<u32> {
    let present = |id: u32| payloads.iter().any(|p| p.run_id == id);
    let mut missing: Vec<u32> = mandatory_burn_run_ids
        .iter()
        .copied()
        .filter(|&id| !present(id))
        .collect();
    if !any_of_burn_run_ids.iter().any(|&id| present(id)) {
        for &id in any_of_burn_run_ids {
            if !missing.contains(&id) {
                missing.push(id);
            }
        }
    }
    missing
}

/// Recover the expected burns `out` is still missing (see [`missing_expected_burns`]) from their
/// isolated slot crops, merging them into `out`. A no-op when nothing is missing.
pub fn recover_missing_burns(
    img: &GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    out: &mut Vec<Payload>,
) {
    let missing = missing_expected_burns(out, mandatory_burn_run_ids, any_of_burn_run_ids);
    if !missing.is_empty() {
        burn_region_passes(img, &missing, out);
    }
}

/// Decode each slot that can hold one of `missing_run_ids` from its isolated crop and merge the
/// crop's payloads of those ids into `out` (see the module doc for the contract).
pub fn burn_region_passes(img: &GrayImage, missing_run_ids: &[u32], out: &mut Vec<Payload>) {
    warn_once_on_unlocalized(missing_run_ids);
    let (w, h) = (img.width(), img.height());
    for slot in recovery_slots(missing_run_ids) {
        // The missing ids this slot holds (an id without a fixed slot is never localized).
        let wanted: Vec<u32> = missing_run_ids
            .iter()
            .copied()
            .filter(|&id| slot_for_run_id(id) == Some(slot))
            .collect();
        let Some(r) = recovery_crop(slot, w, h) else {
            continue;
        };
        let crop = image::imageops::crop_imm(img, r.x, r.y, r.w, r.h).to_image();
        // Reads in frame pixels, so the issue-1367 own-slot check below sees where they sit.
        let first = reads_in_frame(decode_qr_luma_all_located(crop.clone()), r.x, r.y, 1.0, 1.0);
        // One slot carries one burn: if the 1x look read THIS slot's burn under a run_id that is
        // not missing (for example the deployed camera while the others are "missing"), a 2x
        // look cannot find a missing one there.
        let slot_occupied = first
            .iter()
            .any(|l| slot_for_run_id(l.payload.run_id) == Some(slot));
        let keep_wanted = |reads: Vec<LocatedPayload>| -> Vec<Payload> {
            reads
                .into_iter()
                .filter(|l| {
                    wanted.contains(&l.payload.run_id)
                        && node_burn_in_own_slot(l.payload.run_id, l.cx, l.cy, w, h)
                })
                .map(|l| l.payload)
                .collect()
        };
        let mut found = keep_wanted(first);
        if found.is_empty() && !slot_occupied {
            let upscaled = image::imageops::resize(
                &crop,
                r.w * BURN_REGION_UPSCALE,
                r.h * BURN_REGION_UPSCALE,
                image::imageops::FilterType::CatmullRom,
            );
            let back = 1.0 / f64::from(BURN_REGION_UPSCALE);
            found = keep_wanted(reads_in_frame(
                decode_qr_luma_all_located(upscaled),
                r.x,
                r.y,
                back,
                back,
            ));
        }
        let before = out.len();
        merge_payloads(out, found);
        let added = out.len() - before;
        if added > 0 {
            BURN_REGION_RECOVERIES.fetch_add(added as u64, Ordering::Relaxed);
            tracing::debug!(
                slot = ?slot,
                added,
                crop = ?r,
                "issue 1370: burn-isolated slot crop recovered node burn(s) the plain + tile \
                 passes missed"
            );
        }
    }
}

/// Log ONCE per process when an expected burn run_id is not a reserved id at all (an operator
/// `--burn-*-run-id` override), so the pass silently skipping it stays visible. The reserved ids
/// without a fixed overlay slot (SongPlayer, the painted aux marks) are expected and not logged.
fn warn_once_on_unlocalized(missing_run_ids: &[u32]) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let unlocalized: Vec<u32> = missing_run_ids
        .iter()
        .copied()
        .filter(|&id| {
            slot_for_run_id(id).is_none() && id != BURN_RUN_ID_SONGPLAYER && id != AUX_TICK_RUN_ID
        })
        .collect();
    if !unlocalized.is_empty() && !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            run_ids = ?unlocalized,
            "issue 1370: expected burn run_id(s) that are not reserved ids (an operator \
             --burn-*-run-id override?) have no overlay slot — the burn-isolated slot recovery \
             cannot look for them (logged once per process)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn_regions::{
        slot_rect, BurnSlot, CAM_BURN_BOTTOM_MARGIN_PX, CAM_BURN_DESIGN_H, CAM_BURN_QR_PX,
    };
    use crate::colour_scale::Rect;
    use crate::probe::luma::bgra_to_luma;
    use crate::probe::qr::{
        cam1_burn_origin, render_payload_qr, render_qr_dual_bgra, CAM1_BURN_BOTTOM_MARGIN_PX,
        CAM1_BURN_QR_PX,
    };
    // AUX_TICK_RUN_ID + BURN_RUN_ID_SONGPLAYER come in through `super::*`.
    use crate::probe::recording_latency::{
        BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4, BURN_RUN_ID_CAM5,
        BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_CG, BURN_RUN_ID_IMAG, BURN_RUN_ID_STREAM,
        BURN_RUN_ID_STRIH,
    };

    const CAMERA_IDS: [u32; 7] = [
        BURN_RUN_ID_CAM1,
        BURN_RUN_ID_CAM2,
        BURN_RUN_ID_CAM3,
        BURN_RUN_ID_CAM4,
        BURN_RUN_ID_CAM5,
        BURN_RUN_ID_CAM6,
        BURN_RUN_ID_CAM7,
    ];

    fn p(run_id: u32, frame_id: u32, gen_ts_ns: i64) -> Payload {
        Payload {
            run_id,
            frame_id,
            gen_ts_ns,
        }
    }

    /// Blit `payload`'s QR (rendered at most `qr_px`) with its top-left at `(ox, oy)`.
    fn blit(frame: &mut GrayImage, payload: &Payload, qr_px: u32, ox: u32, oy: u32) {
        let qr = render_payload_qr(payload, qr_px);
        for y in 0..qr.height() {
            for x in 0..qr.width() {
                frame.put_pixel(ox + x, oy + y, *qr.get_pixel(x, y));
            }
        }
    }

    /// A 1920x1080 dual-QR painter frame (optical run_id 7) with each given burn blitted at the
    /// PRODUCTION position of its slot: the camera burn through `qr::cam1_burn_origin` (the real
    /// YUYV writer's formula), a corner burn at its `burn_geom` slot origin.
    fn painter_frame_with_burns(burns: &[Payload]) -> GrayImage {
        let (w, h) = (1920u32, 1080u32);
        let bgra = render_qr_dual_bgra(&p(7, 100, 1), &p(7, 101, 2), w, h, 700);
        let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
        for b in burns {
            match slot_for_run_id(b.run_id).expect("test burns use reserved ids") {
                BurnSlot::CameraCapture => {
                    let qr = render_payload_qr(b, CAM1_BURN_QR_PX);
                    let (ox, oy) = cam1_burn_origin(w, h, qr.width(), qr.height());
                    blit(&mut luma, b, CAM1_BURN_QR_PX, ox, oy);
                }
                slot => {
                    let r = slot_rect(slot, w, h).unwrap();
                    blit(&mut luma, b, r.w, r.x, r.y);
                }
            }
        }
        luma
    }

    #[test]
    fn missing_expected_burns_mirrors_the_fast_path_gate() {
        let strih = p(BURN_RUN_ID_STRIH, 1, 1);
        let cam3 = p(BURN_RUN_ID_CAM3, 2, 2);
        let stream = p(BURN_RUN_ID_STREAM, 3, 3);
        let mandatory = [BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM];
        // Everything present: nothing missing.
        assert!(missing_expected_burns(&[strih, cam3, stream], &mandatory, &CAMERA_IDS).is_empty());
        // One any-of member present satisfies the whole group; a missing mandatory id is listed.
        assert_eq!(
            missing_expected_burns(&[strih, cam3], &mandatory, &CAMERA_IDS),
            vec![BURN_RUN_ID_STREAM]
        );
        // No any-of member present: the whole group is missing (its order kept), after the
        // mandatory ids.
        let mut want = vec![BURN_RUN_ID_STREAM];
        want.extend_from_slice(&CAMERA_IDS);
        assert_eq!(
            missing_expected_burns(&[strih], &mandatory, &CAMERA_IDS),
            want
        );
        // An empty any-of group is vacuously satisfied (the imag / single-group shape).
        assert!(missing_expected_burns(&[strih, stream], &mandatory, &[]).is_empty());
        // An id in both groups is listed once.
        assert_eq!(
            missing_expected_burns(&[], &[BURN_RUN_ID_CAM1], &[BURN_RUN_ID_CAM1]),
            vec![BURN_RUN_ID_CAM1]
        );
    }

    #[test]
    fn each_slot_crop_reads_a_burn_placed_at_its_production_position_1370() {
        let burns = [
            p(BURN_RUN_ID_CAM1, 10465, 1_790_334_425_204_564_191),
            p(BURN_RUN_ID_STRIH, 17801, 1_790_334_425_234_623_958),
            p(BURN_RUN_ID_STREAM, 5, 5),
            p(BURN_RUN_ID_IMAG, 6, 6),
            p(BURN_RUN_ID_CG, 7, 7),
        ];
        let luma = painter_frame_with_burns(&burns);
        let missing: Vec<u32> = burns.iter().map(|b| b.run_id).collect();
        let mut out = Vec::new();
        burn_region_passes(&luma, &missing, &mut out);
        for b in &burns {
            assert!(
                out.contains(b),
                "the {:?} slot crop must read {b:?}; got {out:?}",
                slot_for_run_id(b.run_id)
            );
        }
        assert_eq!(
            out.len(),
            burns.len(),
            "only the missing burns are added — never the optical dual-QR: {out:?}"
        );
    }

    #[test]
    fn the_pass_keeps_read_payloads_and_adds_only_missing_ids_1370() {
        // The camera slot holds cam2's burn, but only cam1 is missing: the crop reads cam2's
        // burn and must NOT merge it (it is not a missing id), and the pre-existing payloads stay
        // exactly as they were, in order.
        let cam2 = p(BURN_RUN_ID_CAM2, 42, 42);
        let luma = painter_frame_with_burns(&[cam2]);
        let already = vec![p(7, 100, 1), p(BURN_RUN_ID_STRIH, 9, 9)];
        let mut out = already.clone();
        burn_region_passes(&luma, &[BURN_RUN_ID_CAM1], &mut out);
        assert_eq!(out, already, "a non-missing burn is never merged");

        // With cam2 among the missing ids (the any-of group), it is appended after them.
        let mut out = already.clone();
        recover_missing_burns(&luma, &[BURN_RUN_ID_STRIH], &CAMERA_IDS, &mut out);
        assert_eq!(
            out[..2],
            already[..],
            "already-read payloads stay byte-identical"
        );
        assert_eq!(
            out[2..],
            [cam2],
            "only the recovered camera burn is appended"
        );
    }

    #[test]
    fn nothing_missing_means_no_pass_1370() {
        // The strih burn is present in `out`; the frame's crop would read a DIFFERENT strih
        // frame_id if it ran. It must not run.
        let luma = painter_frame_with_burns(&[p(BURN_RUN_ID_STRIH, 999, 9)]);
        let already = vec![p(BURN_RUN_ID_STRIH, 1, 1)];
        let mut out = already.clone();
        recover_missing_burns(&luma, &[BURN_RUN_ID_STRIH], &[], &mut out);
        assert_eq!(out, already);
    }

    // ---- parity pins: the Tier-0 slot table vs the probe-side writers and ids ----
    // (The corner slots are pinned to the shipped burn-geom.hpp by
    // tests/burn_regions_cpp_parity_1370.rs, which runs on default features.)

    #[test]
    fn camera_slot_matches_the_cam1_burn_writer_at_the_design_height_1370() {
        assert_eq!(CAM_BURN_QR_PX, CAM1_BURN_QR_PX);
        assert_eq!(CAM_BURN_BOTTOM_MARGIN_PX, CAM1_BURN_BOTTOM_MARGIN_PX);
        let h = CAM_BURN_DESIGN_H;
        for w in [1920, 1440, 900, 500] {
            let (x, y) = cam1_burn_origin(w, h, CAM1_BURN_QR_PX, CAM1_BURN_QR_PX);
            let want = Rect {
                x,
                y,
                w: CAM1_BURN_QR_PX,
                h: CAM1_BURN_QR_PX,
            };
            assert_eq!(
                slot_rect(BurnSlot::CameraCapture, w, h),
                Some(want),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn slot_ids_match_the_reserved_burn_run_ids_1370() {
        for id in CAMERA_IDS {
            assert_eq!(slot_for_run_id(id), Some(BurnSlot::CameraCapture), "{id}");
        }
        assert_eq!(
            slot_for_run_id(BURN_RUN_ID_STRIH),
            Some(BurnSlot::BottomLeft)
        );
        assert_eq!(
            slot_for_run_id(BURN_RUN_ID_STREAM),
            Some(BurnSlot::BottomRight)
        );
        assert_eq!(
            slot_for_run_id(BURN_RUN_ID_IMAG),
            Some(BurnSlot::BottomCenterLeft)
        );
        assert_eq!(
            slot_for_run_id(BURN_RUN_ID_CG),
            Some(BurnSlot::BottomCenterRight)
        );
        // Painted by the sender / painted optical content: no fixed burn slot.
        assert_eq!(slot_for_run_id(BURN_RUN_ID_SONGPLAYER), None);
        assert_eq!(slot_for_run_id(AUX_TICK_RUN_ID), None);
    }
}
