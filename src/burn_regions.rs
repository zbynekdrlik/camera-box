//! issue 1370 — the node-burn SLOT table: where each digital node burn sits on a W×H frame.
//!
//! Every node burn is a crisp overlay at a KNOWN position. The camera-under-test capture burn
//! (cam1..cam7, `probe::qr::burn_qr_yuyv`) is horizontally centred and bottom-anchored. The OBS
//! render burns sit in the bottom corners chosen by the DistroAV filter's host role
//! (`vendor/distroav/src/burn-geom.hpp::corner_placement`): strih bottom-left, stream
//! bottom-right, imag bottom-center-left (#463), cg OBS bottom-center-right (#1301).
//!
//! The recording decode (`probe::burn_region_decode`) uses this table for its burn-isolated
//! recovery pass: when the plain full-frame pass and the #202 bottom tiles still miss an EXPECTED
//! burn, it decodes that burn's own slot crop ([`recovery_crop`]) alone. A tile that also holds
//! optical finder patterns can come back empty from rqrr's single `detect_grids` pass, which
//! made the decode depend on where the camera points (issue 1370, run 68573319: a reframed camera
//! view put the optical dual-QR and the aux marks into the cam1 burn's tile). A lone crisp QR in
//! its own crop has no foreign capstones to group with.
//!
//! ## The ONE Rust copy of the burn geometry, pinned to the shipped C++
//!
//! - The corner slots reproduce `burn_geom::corner_placement` exactly, including the narrow-canvas
//!   fallback tiers and the `band_cy` rounding of an odd side. They are canvas-relative (a
//!   fraction of the frame height), so they hold on any recording size.
//! - The camera slot equals `qr::cam1_burn_origin` with `CAM1_BURN_QR_PX` = 320 and a 24 px bottom
//!   margin on the 1080-high design frame. The camera burn is rendered on the 1080 capture frame,
//!   so on a recording of another height the slot scales with the height (640 px on 4K). A smaller
//!   burn stays inside the centred, bottom-anchored slot.
//! - Consumers: the recovery pass (`probe::burn_region_decode`) and the colour gate's burn dodge
//!   (`probe::colour_sample::node_burn_exclusions`, the slots padded by 6 px).
//! - Pins: `tests/burn_regions_cpp_parity_1370.rs` compiles the shipped `burn-geom.hpp` and checks
//!   every corner on production, 720p, 4K and narrow canvases (default features). The probe-gated
//!   tests in `src/probe/burn_region_decode.rs` check the camera slot against the `probe::qr`
//!   writer and the run_id map against `probe::recording_latency::BURN_RUN_ID_*`.
//!
//! ## Why this lives at the crate root (default features)
//!
//! It follows the same seam as `crate::colour_scale` / `crate::aux_tick`. The pure geometry and
//! its tests compile Tier-0 without probe dependencies. The probe-gated decoder only calls
//! [`recovery_slots`] and [`recovery_crop`].

use crate::colour_scale::Rect;

/// Height of the design frame the camera capture burn is rendered on (the fleet's 1080p
/// capture). The camera slot scales by `frame_h / CAM_BURN_DESIGN_H`.
pub const CAM_BURN_DESIGN_H: u32 = 1080;

/// Camera capture burn side at the design height. Mirrors `probe::qr::CAM1_BURN_QR_PX`, pinned by
/// a probe-gated parity test.
pub const CAM_BURN_QR_PX: u32 = 320;

/// Camera capture burn bottom margin at the design height. Mirrors
/// `probe::qr::CAM1_BURN_BOTTOM_MARGIN_PX`, pinned by a probe-gated parity test.
pub const CAM_BURN_BOTTOM_MARGIN_PX: u32 = 24;

/// Corner burn side as a fraction of the frame height (`burn_geom::BURN_QR_HEIGHT_FRACTION`).
pub const CORNER_BURN_HEIGHT_FRACTION: f64 = 0.28;

/// Corner burn edge margin as a fraction of the frame height (`burn_geom::BURN_MARGIN_FRACTION`).
pub const CORNER_BURN_MARGIN_FRACTION: f64 = 40.0 / 1080.0;

/// Extra px around a slot in the recovery crop at the 1080 design height (scaled with the
/// height, never below this). The slot already contains the burn's own white quiet zone. The pad
/// only absorbs integer rounding and resampling so a burn edge is never cut.
pub const RECOVERY_PAD_PX: u32 = 8;

/// Where a node burn sits on the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BurnSlot {
    /// The camera-under-test capture burn: centred, bottom-anchored (cam1..cam7).
    CameraCapture,
    /// strih's OBS render burn (`burn_geom::Corner::BottomLeft`).
    BottomLeft,
    /// stream's OBS render burn (`burn_geom::Corner::BottomRight`).
    BottomRight,
    /// imag's OBS render burn (`burn_geom::Corner::BottomCenterLeft`, #463).
    BottomCenterLeft,
    /// cg OBS's render burn (`burn_geom::Corner::BottomCenterRight`, #1301).
    BottomCenterRight,
}

impl BurnSlot {
    /// Every slot, in a fixed order (the order [`recovery_slots`] returns).
    pub const ALL: [BurnSlot; 5] = [
        BurnSlot::CameraCapture,
        BurnSlot::BottomLeft,
        BurnSlot::BottomRight,
        BurnSlot::BottomCenterLeft,
        BurnSlot::BottomCenterRight,
    ];
}

/// The slot of a reserved node-burn run_id (the `probe::recording_latency::BURN_RUN_ID_*`
/// defaults). `None` for an id without a fixed overlay position: the SongPlayer content burn
/// (911014, painted by the sender), the painted aux tick marks (911013, optical content, not a
/// burn) and an operator-overridden `--burn-*-run-id` value (the rig uses the reserved defaults).
/// Such an id is never localized, so the recovery pass skips it.
pub fn slot_for_run_id(run_id: u32) -> Option<BurnSlot> {
    match run_id {
        // cam1, cam4, cam3, cam2, cam5, cam6, cam7 — the same capture burn on every camera box.
        911_001 | 911_007..=911_012 => Some(BurnSlot::CameraCapture),
        911_002 => Some(BurnSlot::BottomLeft),
        911_004 => Some(BurnSlot::BottomRight),
        911_003 => Some(BurnSlot::BottomCenterLeft),
        911_015 => Some(BurnSlot::BottomCenterRight),
        _ => None,
    }
}

/// The slots worth an isolated look for a set of missing burn run_ids, deduplicated, in
/// [`BurnSlot::ALL`] order. An id with no fixed slot ([`slot_for_run_id`] = `None`) contributes
/// nothing: there is no known position to crop. Empty input ⇒ empty output.
pub fn recovery_slots(missing_run_ids: &[u32]) -> Vec<BurnSlot> {
    let wanted: Vec<BurnSlot> = missing_run_ids
        .iter()
        .filter_map(|&id| slot_for_run_id(id))
        .collect();
    BurnSlot::ALL
        .iter()
        .copied()
        .filter(|s| wanted.contains(s))
        .collect()
}

/// `v * frame_h / CAM_BURN_DESIGN_H`, rounded down (exact at the design height).
fn scale_to_height(v: u32, frame_h: u32) -> u32 {
    (u64::from(v) * u64::from(frame_h) / u64::from(CAM_BURN_DESIGN_H)) as u32
}

/// The design rectangle of `slot` on a `frame_w`×`frame_h` frame, the burn's white quiet zone
/// included. `None` for an empty frame. Always inside the frame.
pub fn slot_rect(slot: BurnSlot, frame_w: u32, frame_h: u32) -> Option<Rect> {
    if frame_w == 0 || frame_h == 0 {
        return None;
    }
    if slot == BurnSlot::CameraCapture {
        // `qr::cam1_burn_origin`: horizontally centred, `margin` above the bottom edge.
        let side = scale_to_height(CAM_BURN_QR_PX, frame_h)
            .min(frame_w)
            .min(frame_h)
            .max(1);
        let margin = scale_to_height(CAM_BURN_BOTTOM_MARGIN_PX, frame_h);
        return Some(Rect {
            x: (frame_w - side) / 2,
            y: frame_h.saturating_sub(side).saturating_sub(margin),
            w: side,
            h: side,
        });
    }
    // `burn_geom::corner_placement` with the canvas-relative size + margin.
    let margin = ((CORNER_BURN_MARGIN_FRACTION * f64::from(frame_h)) as u32).max(8);
    let max_w = frame_w.saturating_sub(2 * margin).max(1);
    let max_h = frame_h.saturating_sub(2 * margin).max(1);
    let side = ((CORNER_BURN_HEIGHT_FRACTION * f64::from(frame_h)) as u32)
        .max(64)
        .min(max_w)
        .min(max_h)
        .max(1);
    // `burn_qr::render` centres the square on `band_cy` (bottom edge at `frame_h - margin`), so an
    // odd side sits 1 px lower than `frame_h - margin - side` — mirror the C++ rounding exactly.
    let half = side / 2;
    let band_cy = if frame_h > margin + half {
        frame_h - margin - half
    } else {
        half
    };
    let top = band_cy - half;
    let right_x = frame_w.saturating_sub(margin).saturating_sub(side);
    let x = match slot {
        BurnSlot::BottomLeft => margin,
        BurnSlot::BottomRight => right_x,
        BurnSlot::BottomCenterLeft => {
            // #463 tiers: one margin clear of BottomLeft; else flush against it; else frame edge.
            let wanted = margin + side + margin;
            let flush = margin + side;
            if wanted + side <= frame_w {
                wanted
            } else if flush + side <= frame_w {
                flush
            } else {
                frame_w.saturating_sub(side)
            }
        }
        BurnSlot::BottomCenterRight => {
            // #1301 tiers, from the right: one margin clear of BottomRight; else flush against
            // it; else the frame's left edge.
            if right_x > margin + side {
                right_x - margin - side
            } else {
                right_x.saturating_sub(side)
            }
        }
        BurnSlot::CameraCapture => unreachable!("handled above"),
    };
    Some(Rect {
        x,
        y: top,
        w: side,
        h: side,
    })
}

/// The recovery crop for `slot`: its [`slot_rect`] grown by [`RECOVERY_PAD_PX`] (scaled with the
/// frame height) on every side, clamped to the frame. `None` for an empty frame.
pub fn recovery_crop(slot: BurnSlot, frame_w: u32, frame_h: u32) -> Option<Rect> {
    let r = slot_rect(slot, frame_w, frame_h)?;
    let pad = scale_to_height(RECOVERY_PAD_PX, frame_h).max(RECOVERY_PAD_PX);
    let x0 = r.x.saturating_sub(pad);
    let y0 = r.y.saturating_sub(pad);
    let x1 = (r.x + r.w).saturating_add(pad).min(frame_w);
    let y1 = (r.y + r.h).saturating_add(pad).min(frame_h);
    Some(Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn production_1080_slots_match_the_documented_burn_positions() {
        // Hand-derived numbers from burn-geom.hpp + qr::cam1_burn_origin on 1920x1080
        // (margin 40, side 0.28*1080 = 302, top 1080-40-302 = 738; cam 320 at (800, 736)).
        let (w, h) = (1920, 1080);
        let slot = |s| slot_rect(s, w, h).unwrap();
        assert_eq!(slot(BurnSlot::CameraCapture), rect(800, 736, 320, 320));
        assert_eq!(slot(BurnSlot::BottomLeft), rect(40, 738, 302, 302));
        assert_eq!(slot(BurnSlot::BottomRight), rect(1578, 738, 302, 302));
        assert_eq!(slot(BurnSlot::BottomCenterLeft), rect(382, 738, 302, 302));
        assert_eq!(slot(BurnSlot::BottomCenterRight), rect(1236, 738, 302, 302));
    }

    #[test]
    fn a_4k_recording_scales_every_slot_with_the_height() {
        // 0.28*2160 = 604.8 -> 604; margin 40/1080*2160 = 80; top 2160-80-604 = 1476.
        // Camera: 320*2 = 640 centred, margin 48 -> (1600, 2160-640-48 = 1472).
        let (w, h) = (3840, 2160);
        let slot = |s| slot_rect(s, w, h).unwrap();
        assert_eq!(slot(BurnSlot::CameraCapture), rect(1600, 1472, 640, 640));
        assert_eq!(slot(BurnSlot::BottomLeft), rect(80, 1476, 604, 604));
        assert_eq!(slot(BurnSlot::BottomRight), rect(3156, 1476, 604, 604));
        assert_eq!(slot(BurnSlot::BottomCenterLeft), rect(764, 1476, 604, 604));
        assert_eq!(
            slot(BurnSlot::BottomCenterRight),
            rect(2472, 1476, 604, 604)
        );
    }

    #[test]
    fn an_odd_burn_side_is_centred_on_band_cy_like_burn_geom() {
        // 720p: margin 40/1080*720 = 26.67 -> 26, side 0.28*720 = 201.6 -> 201 (odd), half 100,
        // band_cy 720-26-100 = 594, top 594-100 = 494 (not 720-26-201 = 493).
        let r = slot_rect(BurnSlot::BottomLeft, 1280, 720).unwrap();
        assert_eq!(r, rect(26, 494, 201, 201));
        // The square still ends at or above frame_h - margin + 1 and stays in frame.
        assert!(r.y + r.h <= 720);
    }

    #[test]
    fn no_two_slots_overlap_and_every_slot_is_in_frame() {
        for (w, h) in [(1920, 1080), (3840, 2160), (1280, 720)] {
            let rects: Vec<Rect> = BurnSlot::ALL
                .iter()
                .map(|&s| slot_rect(s, w, h).unwrap())
                .collect();
            for (i, a) in rects.iter().enumerate() {
                assert!(
                    a.w > 0 && a.x + a.w <= w && a.y + a.h <= h,
                    "{w}x{h} slot {i} {a:?}"
                );
                for (j, b) in rects.iter().enumerate().skip(i + 1) {
                    assert!(
                        !a.intersects(b),
                        "{w}x{h}: slot {i} {a:?} overlaps slot {j} {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn narrow_canvas_uses_the_burn_geom_fallback_tiers() {
        // 1080 high, margin 40, side 302. On 900 wide BCL keeps its full gap (382+302 = 684 fits)
        // -> tier 1; on 650 wide: wanted 684 > 650, flush 342+302 = 644 <= 650 -> tier 2 at 342;
        // on 600 wide: flush 644 > 600 -> tier 3 at 600-302 = 298.
        let bcl = |w| slot_rect(BurnSlot::BottomCenterLeft, w, 1080).unwrap().x;
        assert_eq!(bcl(900), 382);
        assert_eq!(bcl(650), 342);
        assert_eq!(bcl(600), 298);
        // BCR from the right: br_x = w-342. 1200: br_x 858 > 342 -> 858-342 = 516 (tier 1);
        // 650: br_x 308 <= 342 -> 308-302 = 6 (tier 2); 500: br_x 158 -> saturates to 0 (tier 3).
        let bcr = |w| slot_rect(BurnSlot::BottomCenterRight, w, 1080).unwrap().x;
        assert_eq!(bcr(1200), 516);
        assert_eq!(bcr(650), 6);
        assert_eq!(bcr(500), 0);
    }

    #[test]
    fn recovery_crop_contains_the_slot_and_stays_in_frame() {
        for (w, h) in [(1920, 1080), (3840, 2160), (1280, 720), (320, 200)] {
            for &s in &BurnSlot::ALL {
                let r = slot_rect(s, w, h).unwrap();
                let c = recovery_crop(s, w, h).unwrap();
                assert!(
                    c.x <= r.x && c.y <= r.y && c.x + c.w >= r.x + r.w && c.y + c.h >= r.y + r.h,
                    "{w}x{h} {s:?}: crop {c:?} must contain slot {r:?}"
                );
                assert!(
                    c.x + c.w <= w && c.y + c.h <= h,
                    "{w}x{h} {s:?}: {c:?} in frame"
                );
            }
        }
        // On 1080 the camera crop is the slot grown by 8 px each way.
        assert_eq!(
            recovery_crop(BurnSlot::CameraCapture, 1920, 1080),
            Some(rect(792, 728, 336, 336))
        );
        assert_eq!(
            recovery_crop(BurnSlot::BottomLeft, 1920, 1080),
            Some(rect(32, 730, 318, 318))
        );
        // 4K doubles the pad.
        assert_eq!(
            recovery_crop(BurnSlot::CameraCapture, 3840, 2160),
            Some(rect(1584, 1456, 672, 672))
        );
    }

    #[test]
    fn an_empty_frame_has_no_slots() {
        for &s in &BurnSlot::ALL {
            assert_eq!(slot_rect(s, 0, 1080), None);
            assert_eq!(slot_rect(s, 1920, 0), None);
            assert_eq!(recovery_crop(s, 0, 0), None);
        }
    }

    #[test]
    fn every_reserved_burn_id_maps_to_its_host_role_slot() {
        for id in [
            911_001, 911_007, 911_008, 911_009, 911_010, 911_011, 911_012,
        ] {
            assert_eq!(slot_for_run_id(id), Some(BurnSlot::CameraCapture), "{id}");
        }
        assert_eq!(slot_for_run_id(911_002), Some(BurnSlot::BottomLeft));
        assert_eq!(slot_for_run_id(911_004), Some(BurnSlot::BottomRight));
        assert_eq!(slot_for_run_id(911_003), Some(BurnSlot::BottomCenterLeft));
        assert_eq!(slot_for_run_id(911_015), Some(BurnSlot::BottomCenterRight));
        // No fixed overlay position: aux tick marks (painted optical), SongPlayer (sender-painted),
        // the test-fixture synthetics, an optical run_id.
        for id in [911_013, 911_014, 911_005, 911_006, 911_099, 68_573_319, 7] {
            assert_eq!(slot_for_run_id(id), None, "{id}");
        }
    }

    #[test]
    fn recovery_slots_dedupe_and_keep_the_fixed_order() {
        assert!(recovery_slots(&[]).is_empty());
        assert_eq!(recovery_slots(&[911_002]), vec![BurnSlot::BottomLeft]);
        // The whole camera any-of group collapses to ONE camera crop.
        assert_eq!(
            recovery_slots(&[911_001, 911_009, 911_008, 911_007, 911_010, 911_011, 911_012]),
            vec![BurnSlot::CameraCapture]
        );
        assert_eq!(
            recovery_slots(&[911_004, 911_002, 911_001]),
            vec![
                BurnSlot::CameraCapture,
                BurnSlot::BottomLeft,
                BurnSlot::BottomRight
            ]
        );
    }

    #[test]
    fn an_id_without_a_fixed_slot_is_never_localized() {
        assert!(recovery_slots(&[123_456]).is_empty());
        // The aux marks and SongPlayer have no burn slot; the strih burn still gets its own.
        assert_eq!(
            recovery_slots(&[911_013, 911_002, 911_014]),
            vec![BurnSlot::BottomLeft]
        );
    }
}
