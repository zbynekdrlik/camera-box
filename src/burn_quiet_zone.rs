//! issue 1367 — find a node burn's white quiet-zone box inside its slot crop.
//!
//! The burn-isolated slot recovery (`probe::burn_region_decode`) decodes a missing burn from its
//! slot plus a few px of pad ([`crate::burn_regions::recovery_crop`]). The camera slot models the
//! capture burn at its 320 px request, but the writer renders the QR with whole-pixel modules
//! (`qrcode`'s `max_dimensions`): the version-4 burn payload is 41 modules with its quiet zone, so
//! the burn is 287 px, centred and bottom-anchored at (816, 769) on 1080p. The 336 px camera crop
//! therefore always holds a 24 px strip left of the burn and a 41 px strip above it. That is
//! harmless over ordinary camera picture. When cam2 films the strih-lx multiview, those strips
//! hold multiview QR content, and rqrr's single grid pass over the crop read nothing on 7 frames of
//! run 324220913, although `zbarimg` reads the crisp burn.
//!
//! The fallback, run only when the 1x slot crop read no burn of the slot: find the burn's own
//! white box inside the crop ([`locate_burn_box`]), crop exactly that box, surround it with a white
//! border of about four modules ([`tight_border`]) and decode that. The search never leaves the
//! slot crop, so this look never reads anything outside it (an echo inside the crop is the same
//! known limit the 1x and 2x looks have).
//!
//! ## How the box is found
//!
//! The quiet zone is a solid near-white ring about four modules wide around the code. So:
//! - the columns of its left and right sides hold a vertical near-white run as tall as the box;
//! - the rows of its top and bottom sides hold a horizontal near-white run as wide as the box;
//! - inside the code every column and row is broken by dark modules.
//!
//! [`locate_burn_box`] keeps the columns and rows whose longest near-white run reaches the lower
//! edge of the size band ([`size_band`]) and returns the rectangle they span, when both of its
//! sides are inside the band. A connected-component bounding box does not work here: on the
//! run-324220913 crops the box touches bright multiview pixels, and the component leaks to the crop
//! edge at every threshold measured (the white box spans columns 24..=309, the component 0..=335).
//!
//! "Near-white" is the midpoint between the crop's Otsu threshold and its white level
//! ([`near_white_threshold`]). At the Otsu threshold itself the light multiview rows above the box
//! join its top edge (the box then reads 328 px tall on those crops).
//!
//! This is the pure, Tier-0 half (default features). The probe-gated decode glue takes the
//! histogram from [`luma_histogram`] and the Otsu threshold from `probe::qr::otsu_threshold`,
//! crops, borders and decodes.

use crate::colour_scale::Rect;

/// The smallest white box accepted, as a fraction (numerator, denominator) of the slot's design
/// burn side: 0.8. Integer, so the band edges never depend on float rounding.
pub const BURN_BOX_MIN_FRACTION: (u64, u64) = (4, 5);

/// The largest white box accepted, as a fraction (numerator, denominator) of the slot's design
/// burn side: 1.05.
pub const BURN_BOX_MAX_FRACTION: (u64, u64) = (21, 20);

/// The white level of a crop is this percentile of its luma (the burn's quiet zone and light
/// modules cover far more than the top 1 % of a slot crop that holds a burn).
pub const WHITE_LEVEL_PERCENTILE: f64 = 0.99;

/// The white border added around the located box is its longer side divided by this: about four
/// modules of the node burn: its payload encodes as a version-4 EC-H code, 41 modules wide with
/// its own quiet zone, so a tenth of the box is 4.1 modules.
pub const TIGHT_BORDER_DIVISOR: u32 = 10;

/// The accepted white-box sides for a slot whose design burn side is `expected_side`:
/// `[ceil(0.8 x side), floor(1.05 x side)]`, inclusive. Empty (`lo > hi`) for a zero side.
pub fn size_band(expected_side: u32) -> (u32, u32) {
    let side = u64::from(expected_side);
    let (min_n, min_d) = BURN_BOX_MIN_FRACTION;
    let (max_n, max_d) = BURN_BOX_MAX_FRACTION;
    let lo = (side * min_n).div_ceil(min_d);
    let hi = side * max_n / max_d;
    // lo <= side always fits; 1.05 x a side above ~4.09e9 does not, so saturate.
    (
        u32::try_from(lo.max(1)).unwrap_or(u32::MAX),
        u32::try_from(hi).unwrap_or(u32::MAX),
    )
}

/// The luma histogram of a crop: `hist[v]` = how many bytes of `luma` equal `v`.
pub fn luma_histogram(luma: &[u8]) -> [u64; 256] {
    let mut hist = [0u64; 256];
    for &v in luma {
        hist[usize::from(v)] += 1;
    }
    hist
}

/// The near-white threshold of a crop: the midpoint between its Otsu threshold `otsu` and its
/// white level (the [`WHITE_LEVEL_PERCENTILE`] of `hist`). A pixel at or above it is near-white.
/// Never below `otsu`, so an empty or flat histogram just returns `otsu`.
pub fn near_white_threshold(hist: &[u64; 256], otsu: u8) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return otsu;
    }
    // The smallest level at or below which at least the percentile of all pixels lie.
    let want = (WHITE_LEVEL_PERCENTILE * total as f64).ceil() as u64;
    let mut seen = 0u64;
    let mut white = 255u8;
    for (level, &count) in hist.iter().enumerate() {
        seen += count;
        if seen >= want {
            white = level as u8;
            break;
        }
    }
    if white <= otsu {
        return otsu;
    }
    otsu + (white - otsu) / 2
}

/// The white quiet-zone box of a burn inside a slot crop (see the module doc), in crop pixels.
///
/// `luma` is the crop, row-major, `width` x `height`. `near_white` is the threshold from
/// [`near_white_threshold`]; `expected_side` is the slot's design burn side
/// (`burn_regions::slot_rect(..).w`). `None` when the buffer does not match its size, when no
/// column or no row carries a near-white run as long as the band's lower edge, or when the box they
/// span is outside [`size_band`] on either side.
pub fn locate_burn_box(
    luma: &[u8],
    width: u32,
    height: u32,
    near_white: u8,
    expected_side: u32,
) -> Option<Rect> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || luma.len() != w * h {
        return None;
    }
    let (lo, hi) = size_band(expected_side);
    if lo > hi {
        return None;
    }
    let lo_len = lo as usize;
    // Longest near-white run per column (kept while scanning rows) and per row.
    let mut col_run = vec![0usize; w];
    let mut col_best = vec![0usize; w];
    let mut rows: Vec<usize> = Vec::new();
    for (y, row) in luma.chunks_exact(w).enumerate() {
        let mut run = 0usize;
        let mut best = 0usize;
        for (x, &p) in row.iter().enumerate() {
            if p >= near_white {
                run += 1;
                best = best.max(run);
                col_run[x] += 1;
                col_best[x] = col_best[x].max(col_run[x]);
            } else {
                run = 0;
                col_run[x] = 0;
            }
        }
        if best >= lo_len {
            rows.push(y);
        }
    }
    let x0 = col_best.iter().position(|&b| b >= lo_len)?;
    let x1 = col_best.iter().rposition(|&b| b >= lo_len)?;
    let (y0, y1) = (*rows.first()?, *rows.last()?);
    let b = Rect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0 + 1) as u32,
        h: (y1 - y0 + 1) as u32,
    };
    let in_band = |s: u32| (lo..=hi).contains(&s);
    (in_band(b.w) && in_band(b.h)).then_some(b)
}

/// The white border, in px, added on every side of a located box before it is decoded.
pub fn tight_border(b: Rect) -> u32 {
    b.w.max(b.h) / TIGHT_BORDER_DIVISOR
}

/// Where the top-left pixel of the bordered box image sits on the frame, for a box located in a
/// slot crop whose top-left is at `(crop_x, crop_y)` on the frame. It can be left of or above the
/// frame edge (the border is not frame content), so it is signed.
pub fn bordered_origin(crop_x: u32, crop_y: u32, b: Rect, border: u32) -> (f64, f64) {
    (
        f64::from(crop_x) + f64::from(b.x) - f64::from(border),
        f64::from(crop_y) + f64::from(b.y) - f64::from(border),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` x `h` crop of `bg`, with a `side` x `side` burn-like box at `(bx, by)`: a white
    /// quiet-zone ring `ring` px wide around a checkerboard of `module` px dark/white modules.
    fn crop_with_box(w: u32, h: u32, bg: u8, bx: u32, by: u32, side: u32, ring: u32) -> Vec<u8> {
        let mut px = vec![bg; (w * h) as usize];
        let module = 8;
        for y in by..by + side {
            for x in bx..bx + side {
                let (ix, iy) = (x - bx, y - by);
                let in_ring = ix < ring || iy < ring || ix >= side - ring || iy >= side - ring;
                let v = if in_ring || ((ix / module) + (iy / module)) % 2 == 0 {
                    250
                } else {
                    10
                };
                px[(y * w + x) as usize] = v;
            }
        }
        px
    }

    #[test]
    fn size_band_is_0_8_to_1_05_of_the_design_side_1367() {
        assert_eq!(size_band(320), (256, 336));
        assert_eq!(size_band(302), (242, 317));
        assert_eq!(size_band(640), (512, 672));
        // A zero side has no band.
        let (lo, hi) = size_band(0);
        assert!(lo > hi);
        // A side whose 1.05 x overflows u32 saturates instead of wrapping to a tiny upper edge.
        assert_eq!(size_band(u32::MAX), (3_435_973_836, u32::MAX));
    }

    /// A `w` x `h` dark crop with full-height white bars at the given columns and full-width white
    /// bars at the given rows (each bar `bar` px thick).
    fn crop_with_bars(w: u32, h: u32, cols: &[u32], rows: &[u32], bar: u32) -> Vec<u8> {
        let mut px = vec![40u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let on_col = cols.iter().any(|&c| x >= c && x < c + bar);
                let on_row = rows.iter().any(|&r| y >= r && y < r + bar);
                if on_col || on_row {
                    px[(y * w + x) as usize] = 250;
                }
            }
        }
        px
    }

    #[test]
    fn each_side_of_the_box_is_checked_against_the_band_on_its_own_1367() {
        let (w, h) = (336u32, 336u32);
        // Tall bars at x 0 and 300 (w = 330, in band for 320), one wide bar at y 100 (h = 10).
        let px = crop_with_bars(w, h, &[0, 300], &[100], 30);
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 320),
            None,
            "h below the band"
        );
        // The transpose: w below the band.
        let px = crop_with_bars(w, h, &[100], &[0, 300], 30);
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 320),
            None,
            "w below the band"
        );
        // For a 302 px slot (band 242..317): w = 336 is above the band, h = 271 is inside it.
        let px = crop_with_bars(w, h, &[0, 330], &[10, 275], 6);
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 302),
            None,
            "w above the band"
        );
        // The transpose: h above the band.
        let px = crop_with_bars(w, h, &[10, 275], &[0, 330], 6);
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 302),
            None,
            "h above the band"
        );
        // Both sides in band: found.
        let px = crop_with_bars(w, h, &[10, 275], &[10, 275], 6);
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 302),
            Some(Rect {
                x: 10,
                y: 10,
                w: 271,
                h: 271
            })
        );
    }

    #[test]
    fn near_white_is_the_midpoint_between_otsu_and_the_white_level_1367() {
        // The run-324220913 crops: Otsu 121, white level 255 -> 188.
        let mut hist = [0u64; 256];
        hist[10] = 500;
        hist[255] = 500;
        assert_eq!(near_white_threshold(&hist, 121), 188);
        // The white level is the 99th percentile, so a few hot pixels above it do not move it.
        let mut hist = [0u64; 256];
        hist[10] = 500;
        hist[200] = 495;
        hist[255] = 5;
        assert_eq!(near_white_threshold(&hist, 100), 150);
        // Flat or empty: never below Otsu.
        let mut hist = [0u64; 256];
        hist[90] = 10;
        assert_eq!(near_white_threshold(&hist, 120), 120);
        assert_eq!(near_white_threshold(&[0u64; 256], 77), 77);
    }

    #[test]
    fn a_shrunk_lower_burn_box_is_found_exactly_1367() {
        // The run-324220913 shape on the 336 px camera crop: a 286 px box at (24, 41).
        let px = crop_with_box(336, 336, 40, 24, 41, 286, 30);
        assert_eq!(
            locate_burn_box(&px, 336, 336, 188, 320),
            Some(Rect {
                x: 24,
                y: 41,
                w: 286,
                h: 286
            })
        );
    }

    #[test]
    fn bright_surround_touching_the_box_on_some_rows_does_not_move_its_sides_1367() {
        // Light multiview strips run from the crop edge into the box on a few rows (the leak
        // that makes a connected-component box span the whole crop width).
        let (w, h) = (336u32, 336u32);
        let mut px = crop_with_box(w, h, 40, 24, 41, 286, 30);
        for y in [120u32, 121, 122, 250, 251] {
            for x in 0..24 {
                px[(y * w + x) as usize] = 250;
            }
        }
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 320),
            Some(Rect {
                x: 24,
                y: 41,
                w: 286,
                h: 286
            })
        );
    }

    #[test]
    fn a_bright_band_above_the_burn_is_cut_off_by_squaring_the_box_1367() {
        // The burn-reframed / stream fixtures: light rows fill the 41 px strip above the burn and
        // join its top quiet band, so the qualifying rows span 0..=326 (a 286 x 327 span). The
        // burn is square, so the box is the bottom-anchored square whose top row qualifies.
        let (w, h) = (336u32, 336u32);
        let mut px = crop_with_box(w, h, 40, 24, 41, 286, 30);
        for y in 0..41 {
            for x in 0..w {
                px[(y * w + x) as usize] = 250;
            }
        }
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 320),
            Some(Rect {
                x: 24,
                y: 41,
                w: 286,
                h: 286
            })
        );
        // The transpose (a bright strip left of the burn): the right-anchored square.
        let mut px = crop_with_box(w, h, 40, 41, 24, 286, 30);
        for y in 0..h {
            for x in 0..41 {
                px[(y * w + x) as usize] = 250;
            }
        }
        assert_eq!(
            locate_burn_box(&px, w, h, 188, 320),
            Some(Rect {
                x: 41,
                y: 24,
                w: 286,
                h: 286
            })
        );
    }

    #[test]
    fn a_non_square_span_with_no_qualifying_square_is_rejected_1367() {
        // Thin bars: columns 0..6 and 286..292 (w = 292), rows 0..6 and 327..333 (h = 333). Both
        // sides are in the 320 band, but no square of side 292 has a qualifying row at both edges.
        let (w, h) = (336u32, 336u32);
        let px = crop_with_bars(w, h, &[0, 286], &[0, 327], 6);
        assert_eq!(locate_burn_box(&px, w, h, 188, 320), None);
    }

    #[test]
    fn a_box_outside_the_size_band_is_rejected_1367() {
        // 240 px < 0.8 x 320: too small.
        let px = crop_with_box(336, 336, 40, 40, 40, 240, 30);
        assert_eq!(locate_burn_box(&px, 336, 336, 188, 320), None);
        // An all-white crop spans 336 x 336, inside the band for 320 (1.05 x 320 = 336) ...
        let white = vec![250u8; 336 * 336];
        assert!(locate_burn_box(&white, 336, 336, 188, 320).is_some());
        // ... but not for a 302 px corner slot (1.05 x 302 = 317).
        assert_eq!(locate_burn_box(&white, 336, 336, 188, 302), None);
    }

    #[test]
    fn a_crop_without_a_white_box_has_none_1367() {
        // Dark background, no burn.
        let px = vec![40u8; 336 * 336];
        assert_eq!(locate_burn_box(&px, 336, 336, 188, 320), None);
        // A small white box (the size of a multiview echo) has no run as long as the band.
        let px = crop_with_box(336, 336, 40, 24, 41, 120, 20);
        assert_eq!(locate_burn_box(&px, 336, 336, 188, 320), None);
    }

    #[test]
    fn a_size_mismatch_or_empty_crop_is_none_1367() {
        assert_eq!(locate_burn_box(&[250u8; 10], 5, 5, 188, 320), None);
        assert_eq!(locate_burn_box(&[], 0, 0, 188, 320), None);
        assert_eq!(locate_burn_box(&[250u8; 16], 4, 4, 188, 0), None);
    }

    #[test]
    fn luma_histogram_counts_every_byte_1367() {
        let h = luma_histogram(&[0, 40, 40, 255, 255, 255]);
        assert_eq!((h[0], h[40], h[255]), (1, 2, 3));
        assert_eq!(h.iter().sum::<u64>(), 6);
        assert_eq!(luma_histogram(&[]).iter().sum::<u64>(), 0);
    }

    #[test]
    fn the_border_is_about_four_modules_1367() {
        let b = Rect {
            x: 24,
            y: 41,
            w: 286,
            h: 287,
        };
        // 287 / 10 = 28 px; a module of the 41-module box is 287 / 41 = 7 px.
        assert_eq!(tight_border(b), 28);
        // The LONGER side sets it.
        let tall = Rect {
            x: 0,
            y: 0,
            w: 279,
            h: 287,
        };
        assert_eq!(tight_border(tall), 28);
        let wide = Rect {
            x: 0,
            y: 0,
            w: 287,
            h: 279,
        };
        assert_eq!(tight_border(wide), 28);
    }

    #[test]
    fn the_bordered_origin_maps_a_read_back_to_the_frame_1367() {
        let b = Rect {
            x: 24,
            y: 41,
            w: 286,
            h: 286,
        };
        // Camera crop at (792, 728): the box's top-left is at frame (816, 769), the bordered
        // image starts `border` px before it.
        assert_eq!(bordered_origin(792, 728, b, 31), (785.0, 738.0));
        // A box at the crop's own corner of a crop at the frame edge lies partly off-frame.
        let corner = Rect {
            x: 0,
            y: 0,
            w: 250,
            h: 250,
        };
        assert_eq!(bordered_origin(0, 5, corner, 27), (-27.0, -22.0));
    }
}
