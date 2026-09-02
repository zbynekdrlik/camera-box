//! #367 / #364 — fixed colour-reference scale painted ALONGSIDE the dual-QR.
//!
//! The cam2 monitor (the screen cam1 films) carries the dual-QR Vernier in the TOP band — two
//! QR halves side by side with a CENTRAL GAP between them. This module lays a column of solid
//! known-sRGB patches VERTICALLY inside that central gap so the colours can be checked BY EYE on
//! the monitor AND sampled per-patch from the recording and compared to the expected value — the
//! mechanism the #364 per-camera COLOUR gate is built on.
//!
//! ## Why the column is in the central gap, not the bottom band (#364 rig finding)
//!
//! The scale originally ran along the BOTTOM band (`y = canvas_h − BAND_H`). On the rig the
//! camera's framing of the cam2 monitor CROPPED that band off the bottom — it never reached the
//! recording, so the gate had nothing to sample (the painted fb0 source was clean; only the
//! bottom strip was out of frame). The two dual-QR halves ARE captured (they decode), so the
//! central gap between them is reliably in frame. Moving the scale into that gap puts it where
//! the camera always sees it.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/`drm`, which
//! balloon the shared dev1 `target/`). This module is the PURE layout seam — the same pattern as
//! `src/reannounce.rs`: geometry + the colour table with NO probe deps, so it unit-tests Tier-0
//! (default features, no framebuffer). The probe-gated painter
//! (`src/probe/qr.rs::blit_colour_scale_bgra`) iterates [`colour_scale_patches`] to fill the
//! framebuffer; the #364 colour gate iterates the SAME function to know where each patch is and
//! what colour to expect. Keeping the patch table AND the gap geometry here means there is ONE
//! source of truth for both the painter (write) and the verifier (read).

/// A pixel rectangle on the canvas. Top-left origin; HALF-OPEN — it covers the pixels
/// `x in [x, x + w)` and `y in [y, y + h)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// True when this rectangle shares at least one pixel with `other` (half-open
    /// overlap — two rectangles that merely touch along an edge do NOT intersect).
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// True when pixel `(x, y)` lies inside this rectangle (half-open: `x in [x, x+w)`,
    /// `y in [y, y+h)`). Used by the #364 colour sampler to skip burn-covered pixels.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A solid sRGB colour (8-bit per channel) — the KNOWN value of one reference patch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Top-edge clearance (px) of the top-anchored dual-QR — the Tier-0 canonical value, MIRRORED by
/// the probe-gated `qr::TOP_MARGIN_PX` (which is defined as this constant). The colour column
/// shares the QR's vertical span (top = this margin, bottom = margin + qr_size), so it lives here
/// where the pure gap geometry can use it without depending on the probe module.
pub const TOP_MARGIN_PX: u32 = 24;

/// Default on-screen size (px) of each dual-QR half the permanent cam2 painter renders
/// (frame-probe `--qr-size` default). The #364 colour gate samples recordings of that painter, so
/// it derives the central gap with this same qr_size by default.
pub const DEFAULT_QR_SIZE: u32 = 700;

/// #718/#754 — crop height (px), measured from the top, for a `frac` fraction of `frame_h`,
/// floor-clamped to `[1, frame_h]`. Pure arithmetic seam shared by the two probe-gated retry
/// crops that need it: `qr::robust_optical_top_band` (the #754 continuity-decode top-band
/// recovery) and `colour_sample::detect_dual_qr`'s OWN retry (#718 — the colour-gate localizer
/// never got the #754 fix at all). Both crops are `(0, 0, w, this_height)` — top-left anchored,
/// so a grid detected inside the crop keeps VALID full-frame pixel coordinates, no transform
/// needed. Kept here (not duplicated in each probe-gated call site) so the crop-height math has
/// ONE source of truth and is Tier-0 unit-tested without pulling in `image`/`rqrr` (#185).
pub fn top_band_crop_height(frame_h: u32, frac: f32) -> u32 {
    (((frame_h as f32) * frac) as u32).clamp(1, frame_h.max(1))
}

/// Inner margin (px) inset on EACH side of the central gap before the colour column, so a patch
/// never touches a QR edge (covers the QR quiet zone + any integer rounding in the gap geometry).
pub const COLUMN_INNER_MARGIN_PX: u32 = 10;

/// The fixed reference colours, painted TOP → BOTTOM down the column: white, black, the
/// six primaries/secondaries (R, G, B, C, M, Y), then a 5-step grayscale ramp
/// (0, 64, 128, 192, 255). Each is a KNOWN sRGB value so the #364 colour gate can sample
/// the patch region and compare. The ORDER and the VALUES are the contract the gate
/// depends on — a recording is checked against exactly this table, so do not reorder or
/// re-tint without updating the gate.
pub const PATCH_COLOURS: &[Rgb] = &[
    Rgb::new(255, 255, 255), // white
    Rgb::new(0, 0, 0),       // black
    Rgb::new(255, 0, 0),     // red
    Rgb::new(0, 255, 0),     // green
    Rgb::new(0, 0, 255),     // blue
    Rgb::new(0, 255, 255),   // cyan
    Rgb::new(255, 0, 255),   // magenta
    Rgb::new(255, 255, 0),   // yellow
    // 5-step grayscale ramp (its 0 / 255 ends double as black/white references):
    Rgb::new(0, 0, 0),
    Rgb::new(64, 64, 64),
    Rgb::new(128, 128, 128),
    Rgb::new(192, 192, 192),
    Rgb::new(255, 255, 255),
];

/// The colour-scale layout: each reference patch's pixel rectangle paired with its known sRGB
/// colour, stacked TOP → BOTTOM in a VERTICAL column inside the central gap between the two
/// top-anchored dual-QR halves.
///
/// The gap is derived from the SAME geometry `render_qr_dual_bgra` / `blit_qr_bgra` use
/// (`half = canvas_w / 2`; each QR centered in its half; top-anchored at `top_margin`): the
/// column spans `x in [left_qr_end + margin, right_qr_start − margin)` and `y` over the QR's
/// vertical extent `[top_margin, top_margin + qr_size)`, so it never overlaps either QR half nor
/// the bottom-anchored burns. The patches tile the column: each is `column_h / n` px tall and the
/// LAST one extends to the column bottom so the integer remainder leaves no unpainted strip.
/// Returns EMPTY for a degenerate layout (no gap, or a column too short to hold one px per patch),
/// so the painter simply draws nothing rather than panicking.
pub fn colour_scale_patches(
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
) -> Vec<(Rect, Rgb)> {
    let n = PATCH_COLOURS.len() as u32;
    let Some(gap) = dual_qr_gap(canvas_w, canvas_h, qr_size, top_margin) else {
        return Vec::new();
    };
    // Inset the column inside the gap on BOTH sides so a patch never touches a QR edge.
    let inset = COLUMN_INNER_MARGIN_PX;
    if gap.w <= 2 * inset {
        return Vec::new();
    }
    let col_x = gap.x + inset;
    let col_w = gap.w - 2 * inset;
    let (col_y, col_h) = (gap.y, gap.h);
    let patch_h = col_h / n;
    // Too short to give every patch at least one pixel ⇒ paint nothing.
    if patch_h == 0 {
        return Vec::new();
    }
    PATCH_COLOURS
        .iter()
        .enumerate()
        .map(|(i, &rgb)| {
            let i = i as u32;
            let y = col_y + i * patch_h;
            // The LAST patch absorbs the integer remainder so the column reaches its bottom with
            // no unpainted strip.
            let h = if i == n - 1 {
                col_y + col_h - y
            } else {
                patch_h
            };
            (
                Rect {
                    x: col_x,
                    y,
                    w: col_w,
                    h,
                },
                rgb,
            )
        })
        .collect()
}

/// The two top-anchored dual-QR halves' pixel rectangles on the canvas, `[left, right]`, derived
/// from the SAME geometry `render_qr_dual_bgra` / `blit_qr_bgra` use: `half = canvas_w / 2`; each QR
/// centered in its half (`ox = band_x + (band_w − qw) / 2`, `qw = qr_size.min(band_w)`);
/// top-anchored at `top_margin` over its vertical extent `qh = qr_size.min(canvas_h)`. `None` for a
/// degenerate layout (a QR half doesn't fit).
///
/// The #364 colour gate uses these KNOWN canvas QR positions as the fixed correspondences for the
/// affine that localizes the recorded colour column: a camera filming the cam2 monitor lands the
/// painted content shifted + scaled + slightly keystoned, NOT pixel-identical, so the gate pairs
/// these canvas QR corners with the QR corners the probe DETECTS in the recording and fits the
/// canvas→recording transform from them (see `colour_verify::fit_affine`).
pub fn dual_qr_rects(
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
) -> Option<[Rect; 2]> {
    let half = canvas_w / 2;
    let right_band_w = canvas_w - half;
    // Mirror blit_qr_bgra's `qw = qr.width().min(band_w)` for the left ([0, half)) and right
    // ([half, canvas_w)) halves, and the QR's vertical extent.
    let qw_l = qr_size.min(half);
    let qw_r = qr_size.min(right_band_w);
    let qh = qr_size.min(canvas_h);
    if qw_l == 0 || qw_r == 0 || qh == 0 {
        return None;
    }
    let left_x = (half - qw_l) / 2; // band_x = 0
    let right_x = half + (right_band_w - qw_r) / 2; // band_x = half
                                                    // Mirror qr_origin_y(canvas_h, qh, VAnchor::Top) = top_margin.min(canvas_h − qh).
    let oy = top_margin.min(canvas_h - qh);
    Some([
        Rect {
            x: left_x,
            y: oy,
            w: qw_l,
            h: qh,
        },
        Rect {
            x: right_x,
            y: oy,
            w: qw_r,
            h: qh,
        },
    ])
}

/// The central captured gap between the two top-anchored dual-QR halves: the rectangle
/// `[left_qr_end, right_qr_start) × [qr_top, qr_top + qr_height)`, derived from [`dual_qr_rects`] so
/// there is ONE QR geometry. `None` when the layout is degenerate (a QR half doesn't fit, or the
/// halves leave no gap between them).
fn dual_qr_gap(canvas_w: u32, canvas_h: u32, qr_size: u32, top_margin: u32) -> Option<Rect> {
    let [left, right] = dual_qr_rects(canvas_w, canvas_h, qr_size, top_margin)?;
    let left_qr_end = left.x + left.w;
    let right_qr_start = right.x;
    if right_qr_start <= left_qr_end {
        return None; // the halves meet — no gap
    }
    Some(Rect {
        x: left_qr_end,
        y: left.y,
        w: right_qr_start - left_qr_end,
        h: left.h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default dual-QR geometry the colour column must fit BETWEEN — mirrors
    // src/probe/qr.rs (`VAnchor::Top` + `TOP_MARGIN_PX` = 24, each half centered in its half-width
    // via blit_qr_bgra) and the default `--qr-size` 700 the permanent cam2 painter uses. Re-stated
    // here because qr.rs is behind `feature = "probe"` and this Tier-0 test must compile without it.
    // If the dual-QR layout changes, update these to match.
    const CANVAS_W: u32 = 1920;
    const CANVAS_H: u32 = 1080;
    const QR_SIZE: u32 = 700;
    const TOP_MARGIN: u32 = 24;

    const HALF: u32 = CANVAS_W / 2; // 960
    const QX: u32 = (HALF - QR_SIZE) / 2; // 130 — each QR centered in its half
    const QR_BOTTOM: u32 = TOP_MARGIN + QR_SIZE; // 724
                                                 // Left QR  [130, 830) x [24, 724);  Right QR [1090, 1790) x [24, 724).
    const QR_LEFT: Rect = Rect {
        x: QX,
        y: TOP_MARGIN,
        w: QR_SIZE,
        h: QR_SIZE,
    };
    const QR_RIGHT: Rect = Rect {
        x: HALF + QX,
        y: TOP_MARGIN,
        w: QR_SIZE,
        h: QR_SIZE,
    };
    // Central gap between the two halves: x in [830, 1090), y in [24, 724).
    const GAP_X0: u32 = QX + QR_SIZE; // 830 (left QR right edge)
    const GAP_X1: u32 = HALF + QX; // 1090 (right QR left edge)

    // cam1 center-bottom burn — mirror qr::cam1_burn_origin (centered, bottom-anchored, 320px,
    // margin 24): ox=(1920-320)/2=800, oy=1080-320-24=736 → [800,1120) x [736,1056).
    const CAM1_BURN: Rect = Rect {
        x: (CANVAS_W - 320) / 2,
        y: CANVAS_H - 320 - 24,
        w: 320,
        h: 320,
    };
    // strih/stream corner burns — mirror probe::colour_sample (side = 0.28·h = 302, margin = 40):
    // top = 1080-40-302 = 738; bottom-left [40,342) x [738,1040); bottom-right [1578,1880) x same
    // (W - margin - side = 1920 - 40 - 302 = 1578; the const below is authoritative — this comment
    // previously carried a stale [1538,1840), corrected with issue 1270).
    const CORNER_SIDE: u32 = 302;
    const CORNER_MARGIN: u32 = 40;
    const BURN_BL: Rect = Rect {
        x: CORNER_MARGIN,
        y: CANVAS_H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };
    const BURN_BR: Rect = Rect {
        x: CANVAS_W - CORNER_MARGIN - CORNER_SIDE,
        y: CANVAS_H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };
    // #463 — imag's bottom-center-left corner burn: mirrors
    // `burn_geom::corner_placement`'s new `Corner::BottomCenterLeft` case (vendor/distroav/src/
    // burn-geom.hpp) — one `CORNER_MARGIN` clear of BURN_BL's trailing edge (40+302+40=382),
    // same side/row as the other two corner burns. [382,684) x [738,1040): 78px clear of
    // BURN_BL's right edge (342, after the 40px gap) and 116px clear of CAM1_BURN's left edge
    // (800) — no overlap with either, and (being bottom-anchored, same row as BURN_BL/BURN_BR)
    // no overlap with the colour column (which ends at QR_BOTTOM=724, well above row 738).
    const BURN_IMAG: Rect = Rect {
        x: CORNER_MARGIN + CORNER_SIDE + CORNER_MARGIN,
        y: CANVAS_H - CORNER_MARGIN - CORNER_SIDE,
        w: CORNER_SIDE,
        h: CORNER_SIDE,
    };

    fn default_patches() -> Vec<(Rect, Rgb)> {
        colour_scale_patches(CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN)
    }

    #[test]
    fn patch_count_and_table_are_unchanged() {
        let patches = default_patches();
        assert_eq!(
            patches.len(),
            PATCH_COLOURS.len(),
            "one rectangle per reference colour (expected {}, got {})",
            PATCH_COLOURS.len(),
            patches.len()
        );
        // Lock the table itself so a colour silently dropped/added is caught: 8 named colours +
        // a 5-step ramp = 13.
        assert_eq!(PATCH_COLOURS.len(), 13, "8 named colours + 5-step ramp");
        // Each patch carries exactly the table colour in order.
        for (i, (_, rgb)) in patches.iter().enumerate() {
            assert_eq!(*rgb, PATCH_COLOURS[i], "patch {i} colour");
        }
        // Spot-check concrete sRGB values (NOT a tautology against the table).
        assert_eq!(patches[0].1, Rgb::new(255, 255, 255), "white");
        assert_eq!(patches[1].1, Rgb::new(0, 0, 0), "black");
        assert_eq!(patches[2].1, Rgb::new(255, 0, 0), "red");
        assert_eq!(patches[3].1, Rgb::new(0, 255, 0), "green");
        assert_eq!(patches[4].1, Rgb::new(0, 0, 255), "blue");
        assert_eq!(patches[5].1, Rgb::new(0, 255, 255), "cyan");
        assert_eq!(patches[6].1, Rgb::new(255, 0, 255), "magenta");
        assert_eq!(patches[7].1, Rgb::new(255, 255, 0), "yellow");
        let ramp: Vec<u8> = patches[8..13].iter().map(|(_, c)| c.r).collect();
        assert_eq!(ramp, vec![0, 64, 128, 192, 255], "grayscale ramp steps");
        for (_, c) in &patches[8..13] {
            assert!(
                c.r == c.g && c.g == c.b,
                "ramp patch is neutral gray: {c:?}"
            );
        }
    }

    #[test]
    fn every_patch_is_inside_the_central_gap_and_within_canvas() {
        let patches = default_patches();
        assert!(!patches.is_empty());
        for (rect, _) in &patches {
            assert!(rect.w > 0 && rect.h > 0, "non-empty rect: {rect:?}");
            // Within the canvas.
            assert!(rect.x + rect.w <= CANVAS_W, "within width: {rect:?}");
            assert!(rect.y + rect.h <= CANVAS_H, "within height: {rect:?}");
            // Strictly inside the central gap [GAP_X0, GAP_X1) x [TOP_MARGIN, QR_BOTTOM).
            assert!(
                rect.x >= GAP_X0 && rect.x + rect.w <= GAP_X1,
                "patch x-range must sit in the gap [{GAP_X0}, {GAP_X1}): {rect:?}"
            );
            assert!(
                rect.y >= TOP_MARGIN && rect.y + rect.h <= QR_BOTTOM,
                "patch y-range must sit in the QR vertical span [{TOP_MARGIN}, {QR_BOTTOM}): {rect:?}"
            );
        }
    }

    #[test]
    fn no_patch_intersects_a_qr_half_or_any_burn() {
        let patches = default_patches();
        assert!(!patches.is_empty());
        // #463: BURN_IMAG (imag's new bottom-center-left corner burn) joins the obstacle set —
        // every colour patch must stay samplable with FOUR burns now, not three.
        let obstacles = [QR_LEFT, QR_RIGHT, CAM1_BURN, BURN_BL, BURN_BR, BURN_IMAG];
        for (rect, _) in &patches {
            for ob in &obstacles {
                assert!(
                    !rect.intersects(ob),
                    "colour patch {rect:?} must not overlap obstacle {ob:?}"
                );
            }
        }
    }

    #[test]
    fn four_burns_do_not_overlap_each_other_463() {
        // #463 — imag's new bottom-center-left burn must not collide with EITHER of its bottom-row
        // neighbours (BURN_BL / BURN_BR) NOR the horizontally-centered cam1 capture burn. This is
        // the "third burn must not collide with the other 3 burns" requirement, checked pairwise
        // across all four (not just vs the colour patches, which the test above already covers).
        let burns = [
            ("cam1", CAM1_BURN),
            ("strih (BL)", BURN_BL),
            ("stream (BR)", BURN_BR),
            ("imag (BCL)", BURN_IMAG),
        ];
        for i in 0..burns.len() {
            for j in (i + 1)..burns.len() {
                let (name_a, rect_a) = burns[i];
                let (name_b, rect_b) = burns[j];
                assert!(
                    !rect_a.intersects(&rect_b),
                    "burn {name_a} {rect_a:?} must not overlap burn {name_b} {rect_b:?}"
                );
            }
        }
        // And every burn stays fully in-canvas.
        for (name, r) in &burns {
            assert!(
                r.x + r.w <= CANVAS_W && r.y + r.h <= CANVAS_H,
                "burn {name} {r:?} must stay within the {CANVAS_W}x{CANVAS_H} canvas"
            );
        }
    }

    #[test]
    fn patches_tile_the_column_top_to_bottom_contiguously() {
        let patches = default_patches();
        // All patches share one x and width (a single vertical column).
        let (cx, cw) = (patches[0].0.x, patches[0].0.w);
        for (rect, _) in &patches {
            assert_eq!(rect.x, cx, "all patches in one column (x): {rect:?}");
            assert_eq!(rect.w, cw, "all patches same width: {rect:?}");
        }
        // First patch starts at the column top; each subsequent patch starts exactly where the
        // previous ended (no gap, no overlap); the last reaches the column bottom.
        assert_eq!(patches[0].0.y, TOP_MARGIN, "first patch at column top");
        for w in patches.windows(2) {
            let (a, b) = (&w[0].0, &w[1].0);
            assert_eq!(
                b.y,
                a.y + a.h,
                "patch {b:?} must start exactly where {a:?} ends"
            );
        }
        let last = &patches.last().unwrap().0;
        assert_eq!(
            last.y + last.h,
            QR_BOTTOM,
            "last patch reaches the column bottom (no unpainted strip)"
        );
        // And explicitly: no two patches intersect.
        for i in 0..patches.len() {
            for j in (i + 1)..patches.len() {
                assert!(
                    !patches[i].0.intersects(&patches[j].0),
                    "patches {i} and {j} overlap: {:?} vs {:?}",
                    patches[i].0,
                    patches[j].0
                );
            }
        }
    }

    #[test]
    fn degenerate_or_too_small_gap_yields_no_patches() {
        // Too narrow for two QR halves with any gap between them.
        assert!(
            colour_scale_patches(5, 1080, QR_SIZE, TOP_MARGIN).is_empty(),
            "too narrow ⇒ no gap"
        );
        // qr_size as wide as each half ⇒ the halves meet, no gap.
        assert!(
            colour_scale_patches(CANVAS_W, CANVAS_H, 1000, TOP_MARGIN).is_empty(),
            "qr too wide ⇒ no gap"
        );
        // Column too short to give every patch ≥1px.
        assert!(
            colour_scale_patches(CANVAS_W, 10, QR_SIZE, TOP_MARGIN).is_empty(),
            "too short ⇒ no column"
        );
        assert!(
            colour_scale_patches(0, 0, QR_SIZE, TOP_MARGIN).is_empty(),
            "zero canvas"
        );
    }

    #[test]
    fn rect_intersects_is_half_open() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        assert!(
            a.intersects(&Rect {
                x: 5,
                y: 5,
                w: 10,
                h: 10
            }),
            "overlap"
        );
        // Touching along the right edge (x = 10) does NOT intersect (half-open).
        assert!(
            !a.intersects(&Rect {
                x: 10,
                y: 0,
                w: 10,
                h: 10
            }),
            "edge-touch"
        );
        assert!(
            !a.intersects(&Rect {
                x: 20,
                y: 20,
                w: 5,
                h: 5
            }),
            "disjoint"
        );
    }

    #[test]
    fn top_band_crop_height_matches_the_754_production_constant() {
        // 1080 * 0.67 = 723.6 -> floors to 723 (matches qr.rs's own historical inline computation,
        // proven live on the #754/#718 fixtures — never round UP past what the pixels support).
        assert_eq!(top_band_crop_height(1080, 0.67), 723);
    }

    #[test]
    fn top_band_crop_height_floor_clamps_to_at_least_one_px() {
        // A degenerate 0-height frame must never divide-by-zero or panic downstream — clamp to 1.
        assert_eq!(top_band_crop_height(0, 0.67), 1);
        // A vanishingly small frac still yields >= 1px, never 0 (a 0px crop is unusable).
        assert_eq!(top_band_crop_height(100, 0.0), 1);
    }

    #[test]
    fn top_band_crop_height_never_exceeds_frame_height() {
        // frac >= 1.0 clamps to the full frame height, never overshoots it.
        assert_eq!(top_band_crop_height(1080, 1.0), 1080);
        assert_eq!(top_band_crop_height(1080, 2.5), 1080);
    }

    #[test]
    fn top_band_crop_height_scales_with_frame_height() {
        // A 4K-tall frame gets a proportionally taller crop, not a fixed pixel count.
        assert_eq!(top_band_crop_height(2160, 0.67), 1447);
    }
}
