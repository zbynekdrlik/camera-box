//! issue 1196 — the aux Vernier tick pair's geometry (bottom burn-gap placement).
//!
//! The projection-tap tear detector (`crate::tear_detect`, issue 781) is structurally blind on the
//! single-vertical-band dual-QR content: a horizontal scanout seam corrupts both primary QR halves
//! at the same height, so a torn frame goes undecodable instead of showing two generations. The
//! cure is VERTICAL tick redundancy: the painter additionally blits two SMALL payload-minimal QRs
//! into the burn-free gaps of the bottom band — LEFT carries the latest EVEN tick, RIGHT the
//! latest ODD tick (the same `vernier_ids` discipline as the primary pair), under the reserved
//! `probe::recording_latency::AUX_TICK_RUN_ID` with `gen_ts_ns = 0` (constant, so a settled aux
//! mark's rendered pixels are byte-identical across ticks by construction — the #854 anti-blur
//! property with zero extra state).
//!
//! ## Placement (1920×1080 design space, derived from the REAL obstacle model)
//!
//! The free vertical strip is `[top_margin + qr_size, canvas_h − sweep BAND_HEIGHT_PX)` =
//! `[724, 960)` at the rig defaults. The downstream overlays cover that strip's bottom rows in
//! fixed x-ranges (all y∈[736,1056)): strih's bottom-left corner burn `[40, 342)`, the cambox's
//! own center capture burn `[800, 1120)`, stream's bottom-right corner burn `[1538, 1840)`. The
//! two aux marks sit centered in the two WIDE gaps between them:
//!
//! - LEFT:  x ∈ [466, 676)  (gap [342, 800), ≥124px clearance both sides)
//! - RIGHT: x ∈ [1224, 1434) (gap [1120, 1538), ≥104px clearance both sides)
//! - both:  y ∈ [745, 955)  (21px below the primary band bottom 724, 5px above the sweep band 960)
//!
//! **Documented defect (issue 1196, corrected 2026-09-01 from the known-torn run 1700989544):**
//! imag-nb's BottomCenterLeft corner burn zone `[382, 684)` OVERLAPS the LEFT aux, and — contrary to
//! the original design synthesis's "grounding fact 1" — imag's burn (911003) IS in the projected
//! scene cam2's grabber captures (cam2 films imag-nb's OBS projector output, which renders imag's
//! own burn). The run_id census of the real stream partial proves it: 911003 is present on ~99% of
//! the CAM2 projection-window frames (240/241 torn frames carry it), and on those windows the LEFT
//! (even) aux is OCCLUDED — only the RIGHT (odd) aux decodes (all 241 torn aux marks are odd), so
//! `aux_decode_fraction` (BOTH marks) reads ~0 on the projection leg while the single RIGHT aux
//! carries the operative cross-band tear signal. The LIVE tear gate works on that single mark; the
//! REDUNDANCY the both-mark pair was meant to provide is absent on the projection leg. Relocating
//! the LEFT aux OUT of `[382, 684)` (to restore both-mark redundancy) is the tracked follow-up
//! (issue 1266). The rejected alternative — cramming BOTH marks into the single `[1120, 1538)` gap —
//! stays rejected: ~190px marks decode worse and one localized artifact would kill both.
//!
//! ## Why this lives at the crate root (default features)
//!
//! Same seam pattern as `crate::colour_scale` / `crate::motion_sweep`: the PURE geometry + the
//! machine-proven no-overlap tests compile Tier-0 (no probe deps); the probe-gated painter
//! (`src/probe/qr.rs::blit_aux_tick_bgra`) only CALLS [`aux_tick_rects`] to blit.

use crate::colour_scale::Rect;

/// The fixed design canvas the placement constants below are expressed in.
pub const DESIGN_W: u32 = 1920;
/// See [`DESIGN_W`].
pub const DESIGN_H: u32 = 1080;

/// Aux QR side (px, design space). Payload-minimal (`gen_ts_ns = 0` keeps the encoded string
/// short) so the EC-H modules stay as large as the gap allows; small-QR decodability through the
/// real lossy chain is arbitrated by the mined real-frame fixture (a promotion precondition).
pub const AUX_QR_SIZE_PX: u32 = 210;

/// Top edge y (px, design space): 21px below the rig primary band bottom (24 + 700 = 724) and
/// 5px above the motion-sweep band top (1080 − 120 = 960).
pub const AUX_TOP_Y_PX: u32 = 745;

/// LEFT aux horizontal center (px, design space) — the middle of the `[342, 800)` burn-free gap.
pub const AUX_LEFT_CENTER_X_PX: u32 = 571;

/// RIGHT aux horizontal center (px, design space) — the middle of the `[1120, 1538)` gap.
pub const AUX_RIGHT_CENTER_X_PX: u32 = 1329;

/// `v * num / den` in u64 (no overflow for canvas-scale values).
fn scale(v: u32, num: u32, den: u32) -> u32 {
    (v as u64 * num as u64 / den as u64) as u32
}

/// The two aux tick marks' pixel rectangles `[left, right]` on a `canvas_w×canvas_h` canvas whose
/// primary dual-QR is `qr_size` tall, top-anchored at `top_margin` (the SAME parameters
/// `colour_scale_patches` takes, so painter and any verifier agree on one geometry).
///
/// Positions/size scale proportionally from the design space (x by width, y + size by height).
/// Returns `None` — the painter then simply paints no aux marks — when the layout cannot fit
/// honestly: a degenerate canvas, an aux mark that would start above the primary band's bottom
/// (`top_margin + qr_size` — e.g. the 2560×1080 override canvas, whose width-scaled 933px primary
/// leaves no bottom strip), one that would reach into the motion-sweep band
/// (`crate::motion_sweep::BAND_HEIGHT_PX`), one that would leave the canvas, or a mutual overlap.
pub fn aux_tick_rects(
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
) -> Option<[Rect; 2]> {
    if canvas_w == 0 || canvas_h == 0 {
        return None;
    }
    let size = scale(AUX_QR_SIZE_PX, canvas_h, DESIGN_H);
    if size == 0 {
        return None;
    }
    let y = scale(AUX_TOP_Y_PX, canvas_h, DESIGN_H);
    // The free vertical strip: below the primary band, above the motion-sweep band.
    let primary_bottom = top_margin.checked_add(qr_size)?;
    let sweep_top = canvas_h.saturating_sub(crate::motion_sweep::BAND_HEIGHT_PX);
    if y < primary_bottom || y.checked_add(size)? > sweep_top {
        return None;
    }
    let mut rects = [Rect {
        x: 0,
        y,
        w: size,
        h: size,
    }; 2];
    for (rect, center) in rects
        .iter_mut()
        .zip([AUX_LEFT_CENTER_X_PX, AUX_RIGHT_CENTER_X_PX])
    {
        let cx = scale(center, canvas_w, DESIGN_W);
        rect.x = cx.checked_sub(size / 2)?;
        if rect.x.checked_add(size)? > canvas_w {
            return None;
        }
    }
    if rects[0].intersects(&rects[1]) {
        return None;
    }
    Some(rects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour_scale::{
        colour_scale_patches, dual_qr_rects, DEFAULT_QR_SIZE, TOP_MARGIN_PX,
    };
    use crate::motion_sweep::{ball_rect, sweep_band};

    const W: u32 = 1920;
    const H: u32 = 1080;

    // The downstream overlay rectangles the aux marks must stay clear of — the SAME mirror set
    // src/colour_scale.rs's own no-overlap tests pin (cam1_burn_origin: centered, bottom-anchored,
    // 320px, margin 24; corner burns: side 302, margin 40; imag: BottomCenterLeft, one margin
    // clear of BURN_BL). Re-stated here because those consts live in colour_scale's private test
    // module.
    const CAM_CENTER_BURN: Rect = Rect {
        x: (W - 320) / 2,
        y: H - 320 - 24,
        w: 320,
        h: 320,
    };
    const CORNER_SIDE: u32 = 302;
    const CORNER_MARGIN: u32 = 40;
    const BURN_BL: Rect = Rect {
        x: CORNER_MARGIN,
        y: H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };
    const BURN_BR: Rect = Rect {
        x: W - CORNER_MARGIN - CORNER_SIDE,
        y: H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };
    const BURN_IMAG: Rect = Rect {
        x: CORNER_MARGIN + CORNER_SIDE + CORNER_MARGIN,
        y: H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };

    fn rig_rects() -> [Rect; 2] {
        aux_tick_rects(W, H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
            .expect("the rig aux layout must be non-degenerate")
    }

    #[test]
    fn canonical_rects_are_the_design_values() {
        let [l, r] = rig_rects();
        assert_eq!(
            l,
            Rect {
                x: 466,
                y: 745,
                w: 210,
                h: 210
            },
            "left aux: centered in the [342, 800) gap"
        );
        assert_eq!(
            r,
            Rect {
                x: 1224,
                y: 745,
                w: 210,
                h: 210
            },
            "right aux: centered in the [1120, 1538) gap"
        );
        assert!(!l.intersects(&r));
    }

    #[test]
    fn aux_never_touches_the_primary_qrs_or_colour_patches() {
        let aux = rig_rects();
        let qrs = dual_qr_rects(W, H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
            .expect("the rig dual-QR layout must be non-degenerate");
        let patches: Vec<_> = colour_scale_patches(W, H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
            .into_iter()
            .map(|(rect, _rgb)| rect)
            .collect();
        assert!(!patches.is_empty());
        for a in &aux {
            for qr in &qrs {
                assert!(
                    !a.intersects(qr),
                    "aux {a:?} must not overlap a dual-QR half {qr:?}"
                );
            }
            for p in &patches {
                assert!(
                    !a.intersects(p),
                    "aux {a:?} must not overlap colour patch {p:?}"
                );
            }
        }
    }

    #[test]
    fn aux_never_touches_the_motion_sweep_band_or_ball() {
        // The whole sweep band (not just the ball) is painted dark every frame — the aux marks
        // must sit fully above it, and (belt-and-braces) never share a pixel with the ball over a
        // full sweep period.
        let aux = rig_rects();
        let band = sweep_band(W, H);
        for a in &aux {
            assert!(
                !a.intersects(&band),
                "aux {a:?} must stay above the sweep band {band:?}"
            );
        }
        for f in 0..2000u64 {
            let b = ball_rect(f, W, H);
            for a in &aux {
                assert!(
                    !a.intersects(&b),
                    "aux {a:?} intersected the sweep ball at frame {f}: {b:?}"
                );
            }
        }
    }

    #[test]
    fn aux_avoids_the_downstream_burn_overlays_on_the_stream_path() {
        // The overlays that ARE in the stream-recording cam2 window: strih's BL corner burn, the
        // cambox's own center capture burn, stream's BR corner burn. An aux mark under any of
        // them would be covered downstream and never decode.
        let aux = rig_rects();
        for (name, burn) in [
            ("cambox center burn", CAM_CENTER_BURN),
            ("strih BL burn", BURN_BL),
            ("stream BR burn", BURN_BR),
        ] {
            for a in &aux {
                assert!(
                    !a.intersects(&burn),
                    "aux {a:?} must not sit under the {name} {burn:?}"
                );
            }
        }
        // And both marks stay fully in-canvas.
        for a in &aux {
            assert!(a.x + a.w <= W && a.y + a.h <= H, "aux {a:?} in-canvas");
        }
    }

    #[test]
    fn left_aux_overlaps_the_imag_burn_zone_by_documented_exception() {
        // Pin the KNOWN defect (module doc, corrected 2026-09-01): the left aux sits inside imag's
        // BottomCenterLeft burn zone [382, 684). Contrary to the original design assumption, imag's
        // burn (911003) IS in the projected scene cam2 captures, so on the CAM2 projection window the
        // left aux is OCCLUDED (~99% of frames carry 911003) — only the RIGHT aux decodes, which is
        // the operative single-mark cross-band tear signal. Relocating the left aux out of the imag
        // zone (to restore both-mark redundancy) is the tracked follow-up (issue 1266). If this
        // placement is ever moved, update the module doc's defect paragraph together with this pin.
        let [l, r] = rig_rects();
        assert!(
            l.intersects(&BURN_IMAG),
            "the left aux deliberately shares the imag burn zone (documented exception)"
        );
        assert!(
            !r.intersects(&BURN_IMAG),
            "the right aux stays clear of every burn zone including imag's"
        );
    }

    #[test]
    fn the_2560_override_canvas_yields_none_no_bottom_strip() {
        // The 2560×1080 override canvas scales the primary QR by WIDTH (700 → 933), so the
        // primary band bottom (24 + 933 = 957) leaves no room above the sweep band (960): the
        // aux pair honestly does not fit and the painter paints none — never a false overlap.
        let qr =
            crate::painter_mode::scaled_qr_size(700, crate::painter_mode::BASELINE_CANVAS_W, 2560);
        assert_eq!(qr, 933);
        assert_eq!(aux_tick_rects(2560, 1080, qr, TOP_MARGIN_PX), None);
    }

    #[test]
    fn degenerate_inputs_yield_none() {
        assert_eq!(aux_tick_rects(0, 0, DEFAULT_QR_SIZE, TOP_MARGIN_PX), None);
        assert_eq!(aux_tick_rects(W, 0, DEFAULT_QR_SIZE, TOP_MARGIN_PX), None);
        // A primary QR tall enough to swallow the whole strip.
        assert_eq!(aux_tick_rects(W, H, 1000, TOP_MARGIN_PX), None);
        // A canvas too short for the sweep band + strip.
        assert_eq!(aux_tick_rects(W, 200, DEFAULT_QR_SIZE, TOP_MARGIN_PX), None);
    }
}
