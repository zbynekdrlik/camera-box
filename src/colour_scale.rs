//! #367 — fixed colour-reference scale painted ALONGSIDE the dual-QR.
//!
//! The cam2 monitor (the screen cam1 films) carries the dual-QR Vernier in the TOP
//! band; this adds a row of solid known-sRGB patches along the BOTTOM band so the
//! colours can be checked BY EYE on the monitor AND sampled per-patch from the
//! recording and compared to the expected value — the mechanism the #364 per-camera
//! COLOUR gate is built on.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/
//! `drm`, which balloon the shared dev1 `target/`). This module is the PURE layout
//! seam — the same pattern as `src/reannounce.rs`: geometry + the colour table with
//! NO probe deps, so it unit-tests Tier-0 (default features, no framebuffer). The
//! probe-gated painter (`src/probe/qr.rs::blit_colour_scale_bgra`) iterates
//! [`colour_scale_patches`] to fill the framebuffer; the #364 colour gate iterates the
//! SAME function to know where each patch is and what colour to expect. Keeping the
//! patch table here means there is ONE source of truth for both the painter and the
//! verifier.

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

/// Height (px) of the colour-scale band along the BOTTOM edge of the canvas. Modest so
/// that with the default top-anchored dual-QR (`qr_size` 700 ⇒ bottom row ≈ 724 on a
/// 1080-tall canvas) the band's top (1080 − 120 = 960) stays well clear of it.
pub const BAND_H: u32 = 120;

/// The fixed reference colours, painted LEFT → RIGHT across the band: white, black, the
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

/// The colour-scale layout: each reference patch's pixel rectangle paired with its known
/// sRGB colour, left → right across a full-width band along the BOTTOM of the canvas.
///
/// The band is [`BAND_H`] px tall, anchored to the bottom edge (`y = canvas_h − BAND_H`),
/// so it never overlaps the top-anchored dual-QR (verified in tests for the default
/// `qr_size`). The patches tile the band: each is `canvas_w / n` px wide and the LAST one
/// extends to the right edge so the integer remainder leaves no unpainted column. Returns
/// EMPTY for a canvas too small to hold the band (`canvas_w < n` or `canvas_h <= BAND_H`),
/// so the painter simply draws nothing rather than panicking.
pub fn colour_scale_patches(canvas_w: u32, canvas_h: u32) -> Vec<(Rect, Rgb)> {
    let n = PATCH_COLOURS.len() as u32;
    // Too narrow for one patch per colour, or shorter than the band ⇒ paint nothing.
    if canvas_w < n || canvas_h <= BAND_H {
        return Vec::new();
    }
    let band_y = canvas_h - BAND_H;
    let patch_w = canvas_w / n;
    PATCH_COLOURS
        .iter()
        .enumerate()
        .map(|(i, &rgb)| {
            let i = i as u32;
            let x = i * patch_w;
            // The LAST patch absorbs the integer remainder so the band spans the full
            // width with no unpainted right-edge column.
            let w = if i == n - 1 { canvas_w - x } else { patch_w };
            (
                Rect {
                    x,
                    y: band_y,
                    w,
                    h: BAND_H,
                },
                rgb,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default dual-QR geometry this band must NOT overlap — mirrors
    // src/probe/qr.rs (`VAnchor::Top` + `TOP_MARGIN_PX` = 24) and the default
    // `--qr-size` 700 the permanent cam2 painter uses. Re-stated here because qr.rs is
    // behind `feature = "probe"` and this Tier-0 test must compile without it. If the
    // dual-QR layout changes, update these to match.
    const QR_TOP_MARGIN_PX: u32 = 24;
    const DEFAULT_QR_SIZE: u32 = 700;
    const CANVAS_W: u32 = 1920;
    const CANVAS_H: u32 = 1080;

    #[test]
    fn patch_count_matches_the_colour_table() {
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H);
        assert_eq!(
            patches.len(),
            PATCH_COLOURS.len(),
            "one rectangle per reference colour (expected {}, got {})",
            PATCH_COLOURS.len(),
            patches.len()
        );
        // Lock the table itself so a colour silently dropped/added is caught: 8 named
        // colours + a 5-step ramp = 13.
        assert_eq!(PATCH_COLOURS.len(), 13, "8 named colours + 5-step ramp");
    }

    #[test]
    fn patch_colours_are_in_the_expected_order_with_exact_values() {
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H);
        // Each patch carries exactly the table colour in order.
        for (i, (_, rgb)) in patches.iter().enumerate() {
            assert_eq!(*rgb, PATCH_COLOURS[i], "patch {i} colour");
        }
        // Spot-check concrete sRGB values (NOT a tautology against the table): the named
        // primaries/secondaries must be the pure corners of the cube, in order.
        assert_eq!(patches[0].1, Rgb::new(255, 255, 255), "white");
        assert_eq!(patches[1].1, Rgb::new(0, 0, 0), "black");
        assert_eq!(patches[2].1, Rgb::new(255, 0, 0), "red");
        assert_eq!(patches[3].1, Rgb::new(0, 255, 0), "green");
        assert_eq!(patches[4].1, Rgb::new(0, 0, 255), "blue");
        assert_eq!(patches[5].1, Rgb::new(0, 255, 255), "cyan");
        assert_eq!(patches[6].1, Rgb::new(255, 0, 255), "magenta");
        assert_eq!(patches[7].1, Rgb::new(255, 255, 0), "yellow");
        // The 5-step grayscale ramp.
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
    fn every_patch_is_within_canvas_bounds_and_non_empty() {
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H);
        assert!(!patches.is_empty());
        for (rect, _) in &patches {
            assert!(rect.w > 0 && rect.h > 0, "non-empty rect: {rect:?}");
            assert!(
                rect.x + rect.w <= CANVAS_W,
                "rect within width: {rect:?} (canvas_w {CANVAS_W})"
            );
            assert!(
                rect.y + rect.h <= CANVAS_H,
                "rect within height: {rect:?} (canvas_h {CANVAS_H})"
            );
        }
    }

    #[test]
    fn patches_tile_the_full_width_contiguously_without_gaps_or_overlap() {
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H);
        // First patch starts at the left edge; each subsequent patch starts exactly where
        // the previous ended (no gap, no overlap); the last reaches the right edge.
        assert_eq!(patches[0].0.x, 0, "first patch at left edge");
        for w in patches.windows(2) {
            let (a, b) = (&w[0].0, &w[1].0);
            assert_eq!(
                b.x,
                a.x + a.w,
                "patch {b:?} must start exactly where {a:?} ends"
            );
        }
        let last = &patches.last().unwrap().0;
        assert_eq!(
            last.x + last.w,
            CANVAS_W,
            "last patch reaches the right edge (full-width band)"
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
    fn band_sits_below_and_never_overlaps_the_dual_qr_region() {
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H);
        assert!(
            !patches.is_empty(),
            "must produce patches to check non-overlap"
        );
        // The two top-anchored dual-QR halves (default qr_size), as rendered by
        // render_qr_dual_bgra: each centered in its half-width, top margin 24.
        let qr_bottom = QR_TOP_MARGIN_PX + DEFAULT_QR_SIZE; // 724
        let half = CANVAS_W / 2;
        let qx_left = (half - DEFAULT_QR_SIZE) / 2; // centered in [0, half)
        let qr_left = Rect {
            x: qx_left,
            y: QR_TOP_MARGIN_PX,
            w: DEFAULT_QR_SIZE,
            h: DEFAULT_QR_SIZE,
        };
        let qr_right = Rect {
            x: half + qx_left,
            y: QR_TOP_MARGIN_PX,
            w: DEFAULT_QR_SIZE,
            h: DEFAULT_QR_SIZE,
        };
        for (rect, _) in &patches {
            assert!(
                rect.y >= qr_bottom,
                "colour band top {} must be at/below the dual-QR bottom {qr_bottom}: {rect:?}",
                rect.y
            );
            assert!(
                !rect.intersects(&qr_left) && !rect.intersects(&qr_right),
                "colour patch {rect:?} must not overlap the dual-QR halves \
                 ({qr_left:?} / {qr_right:?})"
            );
        }
    }

    #[test]
    fn degenerate_canvas_yields_no_patches() {
        // Too narrow for one patch per colour, or shorter than the band — paint nothing.
        assert!(colour_scale_patches(5, 1080).is_empty(), "too narrow");
        assert!(colour_scale_patches(1920, BAND_H).is_empty(), "too short");
        assert!(colour_scale_patches(0, 0).is_empty(), "zero canvas");
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
}
