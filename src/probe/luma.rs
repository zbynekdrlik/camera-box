//! Extract a grayscale (luma) image from captured frames for QR decoding.

use image::GrayImage;

/// UYVY 4:2:2 → luma. Layout per 2 px: [U, Y0, V, Y1]; luma is every odd byte.
/// `stride` is the source row pitch in bytes (NDI `line_stride_in_bytes`), which
/// may exceed `width*2` when the row is padded.
pub fn uyvy_to_luma(data: &[u8], width: u32, height: u32, stride: u32) -> GrayImage {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut buf = vec![0u8; w * h];
    for y in 0..h {
        let row = y * s;
        for x in 0..w {
            let idx = row + x * 2 + 1; // Y within the [U Y V Y] pixel pair
            if idx < data.len() {
                buf[y * w + x] = data[idx];
            }
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

/// BGRA → luma via BT.601 integer weights. `stride` is the source row pitch in
/// bytes, which may exceed `width*4` when the row is padded.
pub fn bgra_to_luma(data: &[u8], width: u32, height: u32, stride: u32) -> GrayImage {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut buf = vec![0u8; w * h];
    for y in 0..h {
        let row = y * s;
        for x in 0..w {
            let o = row + x * 4;
            if o + 2 < data.len() {
                let b = data[o] as u32;
                let g = data[o + 1] as u32;
                let r = data[o + 2] as u32;
                buf[y * w + x] = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
            }
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

/// Crop a centered `cw`×`ch` region from a grayscale image (saturated to bounds).
/// Used to limit QR decoding to the region where the QR is painted, which keeps
/// per-frame decode fast enough to track the capture in real time.
pub fn crop_center(img: &GrayImage, cw: u32, ch: u32) -> GrayImage {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return GrayImage::from_raw(1, 1, vec![0]).expect("1x1");
    }
    let cw = cw.min(w).max(1);
    let ch = ch.min(h).max(1);
    let ox = (w - cw) / 2;
    let oy = (h - ch) / 2;
    let mut out = vec![0u8; (cw as usize) * (ch as usize)];
    for y in 0..ch {
        for x in 0..cw {
            out[(y * cw + x) as usize] = img.get_pixel(ox + x, oy + y)[0];
        }
    }
    GrayImage::from_raw(cw, ch, out).expect("buffer sized cw*ch")
}

/// Crop a `cw`-wide × `ch`-tall band anchored to the TOP of a grayscale image
/// (horizontally centered, top edge at y=0; saturated to bounds). The #111 dual-QR is
/// painted in the TOP band (so the strih/stream bottom-corner burns never overlap it), so
/// the live-capture dual decoder crops from the top — a vertically-centered crop would
/// miss the now-top-anchored QRs.
pub fn crop_top(img: &GrayImage, cw: u32, ch: u32) -> GrayImage {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return GrayImage::from_raw(1, 1, vec![0]).expect("1x1");
    }
    let cw = cw.min(w).max(1);
    let ch = ch.min(h).max(1);
    let ox = (w - cw) / 2;
    let mut out = vec![0u8; (cw as usize) * (ch as usize)];
    for y in 0..ch {
        for x in 0..cw {
            out[(y * cw + x) as usize] = img.get_pixel(ox + x, y)[0];
        }
    }
    GrayImage::from_raw(cw, ch, out).expect("buffer sized cw*ch")
}

/// Convert ONLY the centered `cw`×`ch` region of a raw frame to luma, directly from
/// the source bytes — skipping the full-frame luma conversion. The QR is centered, so
/// the live tap only needs this ROI; converting just it (≈0.7 MP) instead of the whole
/// 2-8 MP frame shaves enough per-frame time to let the dev1 tap track a full 30 fps
/// (the full-frame `bgra_to_luma`/`uyvy_to_luma` + `crop_center` left it at ~29.2 fps,
/// a ~2.6% receiver drop that contaminated the loss measurement). `fourcc` BGRA/BGRX ⇒
/// BT.601 weights, else UYVY luma byte. Identical output to
/// `crop_center(&{bgra,uyvy}_to_luma(..), cw, ch)`.
pub fn crop_center_luma(
    fourcc: u32,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    cw: u32,
    ch: u32,
) -> GrayImage {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    if w == 0 || h == 0 {
        return GrayImage::from_raw(1, 1, vec![0]).expect("1x1");
    }
    let cw = cw.min(width).max(1) as usize;
    let ch = ch.min(height).max(1) as usize;
    let ox = (w - cw) / 2;
    let oy = (h - ch) / 2;
    let is_bgra = matches!(&fourcc.to_le_bytes(), b"BGRA" | b"BGRX");
    let mut out = vec![0u8; cw * ch];
    for yy in 0..ch {
        let row = (oy + yy) * s;
        for xx in 0..cw {
            let sx = ox + xx;
            out[yy * cw + xx] = if is_bgra {
                let o = row + sx * 4;
                if o + 2 < data.len() {
                    let b = data[o] as u32;
                    let g = data[o + 1] as u32;
                    let r = data[o + 2] as u32;
                    ((r * 299 + g * 587 + b * 114) / 1000) as u8
                } else {
                    0
                }
            } else {
                let idx = row + sx * 2 + 1; // Y in [U Y V Y]
                if idx < data.len() {
                    data[idx]
                } else {
                    0
                }
            };
        }
    }
    GrayImage::from_raw(cw as u32, ch as u32, out).expect("buffer sized cw*ch")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_center_luma_matches_full_then_crop_bgra() {
        // A 16x8 BGRA frame with a per-pixel gradient; the ROI-direct luma must equal
        // converting the whole frame then center-cropping.
        let (w, h) = (16u32, 8u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            data[i * 4] = (i % 256) as u8; // B
            data[i * 4 + 1] = ((i * 3) % 256) as u8; // G
            data[i * 4 + 2] = ((i * 7) % 256) as u8; // R
            data[i * 4 + 3] = 255;
        }
        let fourcc = u32::from_le_bytes(*b"BGRA");
        let want = crop_center(&bgra_to_luma(&data, w, h, w * 4), 6, 4);
        let got = crop_center_luma(fourcc, &data, w, h, w * 4, 6, 4);
        assert_eq!(want.dimensions(), got.dimensions());
        assert_eq!(want.as_raw(), got.as_raw());
    }

    #[test]
    fn crop_center_luma_matches_full_then_crop_uyvy() {
        let (w, h) = (16u32, 8u32);
        let mut data = vec![0u8; (w * h * 2) as usize];
        for i in 0..(w * h) as usize {
            data[i * 2] = (i % 256) as u8; // chroma
            data[i * 2 + 1] = ((i * 5) % 256) as u8; // Y
        }
        let fourcc = u32::from_le_bytes(*b"UYVY");
        let want = crop_center(&uyvy_to_luma(&data, w, h, w * 2), 6, 4);
        let got = crop_center_luma(fourcc, &data, w, h, w * 2, 6, 4);
        assert_eq!(want.as_raw(), got.as_raw());
    }

    #[test]
    fn crop_center_extracts_middle() {
        // 4x2 image; crop the center 2x2 (ox=1, oy=0).
        let img = GrayImage::from_raw(4, 2, vec![0, 1, 2, 3, 10, 11, 12, 13]).unwrap();
        let c = crop_center(&img, 2, 2);
        assert_eq!(c.dimensions(), (2, 2));
        assert_eq!(c.get_pixel(0, 0)[0], 1);
        assert_eq!(c.get_pixel(1, 0)[0], 2);
        assert_eq!(c.get_pixel(0, 1)[0], 11);
        assert_eq!(c.get_pixel(1, 1)[0], 12);
    }

    #[test]
    fn crop_center_saturates_to_bounds() {
        let img = GrayImage::from_raw(2, 2, vec![1, 2, 3, 4]).unwrap();
        let c = crop_center(&img, 99, 99);
        assert_eq!(c.dimensions(), (2, 2));
    }

    #[test]
    fn crop_center_handles_empty_image() {
        let img = GrayImage::from_raw(0, 0, vec![]).unwrap();
        let c = crop_center(&img, 10, 10);
        assert_eq!(c.dimensions(), (1, 1)); // does not panic
    }

    #[test]
    fn uyvy_picks_luma_bytes() {
        let img = uyvy_to_luma(&[10, 200, 20, 100], 2, 1, 4);
        assert_eq!(img.get_pixel(0, 0)[0], 200);
        assert_eq!(img.get_pixel(1, 0)[0], 100);
    }

    #[test]
    fn uyvy_honors_padded_stride() {
        // width 1, height 2, stride 4 (2-byte pixel padded to 4): luma 111 then 222.
        let data = [0, 111, 0, 0, 0, 222, 0, 0];
        let img = uyvy_to_luma(&data, 1, 2, 4);
        assert_eq!(img.get_pixel(0, 0)[0], 111);
        assert_eq!(img.get_pixel(0, 1)[0], 222);
    }

    #[test]
    fn bgra_white_and_black() {
        let data = [255, 255, 255, 255, 0, 0, 0, 255];
        let img = bgra_to_luma(&data, 2, 1, 8);
        assert_eq!(img.get_pixel(0, 0)[0], 255);
        assert_eq!(img.get_pixel(1, 0)[0], 0);
    }

    #[test]
    fn uyvy_multi_row_indexing() {
        // 2x2: rows [U Y V Y]; luma must land at buf[y*w + x], not a degenerate index.
        let data = [0, 10, 0, 20, 0, 30, 0, 40];
        let img = uyvy_to_luma(&data, 2, 2, 4);
        assert_eq!(img.get_pixel(0, 0)[0], 10);
        assert_eq!(img.get_pixel(1, 0)[0], 20);
        assert_eq!(img.get_pixel(0, 1)[0], 30);
        assert_eq!(img.get_pixel(1, 1)[0], 40);
    }

    #[test]
    fn uyvy_guards_short_data() {
        // Pixel 1's Y index is 3; with only 3 bytes it must stay 0, not read OOB.
        let img = uyvy_to_luma(&[10, 200, 20], 2, 1, 4);
        assert_eq!(img.get_pixel(0, 0)[0], 200);
        assert_eq!(img.get_pixel(1, 0)[0], 0);
    }

    #[test]
    fn bgra_guards_short_data() {
        // Pixel 1 needs bytes 4..7 but only 6 exist -> must stay 0, not read OOB.
        let img = bgra_to_luma(&[255, 255, 255, 255, 0, 0], 2, 1, 8);
        assert_eq!(img.get_pixel(0, 0)[0], 255);
        assert_eq!(img.get_pixel(1, 0)[0], 0);
    }

    #[test]
    fn bgra_channel_weights() {
        // Distinct B/G/R so the channel offsets (o, o+1, o+2) are all pinned.
        assert_eq!(
            bgra_to_luma(&[0, 0, 255, 255], 1, 1, 4).get_pixel(0, 0)[0],
            76
        ); // red
        assert_eq!(
            bgra_to_luma(&[0, 255, 0, 255], 1, 1, 4).get_pixel(0, 0)[0],
            149
        ); // green
        assert_eq!(
            bgra_to_luma(&[255, 0, 0, 255], 1, 1, 4).get_pixel(0, 0)[0],
            29
        ); // blue
    }

    #[test]
    fn crop_center_handles_zero_width() {
        // 0-width but non-zero height: the OR guard must catch it (AND would not).
        let img = GrayImage::from_raw(0, 5, vec![]).unwrap();
        let c = crop_center(&img, 4, 4);
        assert_eq!(c.dimensions(), (1, 1));
    }

    #[test]
    fn crop_center_x_offset_is_centered() {
        // 8-wide gradient cropped to 2 wide -> ox = (8-2)/2 = 3 -> columns 3,4.
        let img = GrayImage::from_raw(8, 1, vec![0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        let c = crop_center(&img, 2, 1);
        assert_eq!(c.get_pixel(0, 0)[0], 3);
        assert_eq!(c.get_pixel(1, 0)[0], 4);
    }

    #[test]
    fn crop_center_y_offset_is_centered() {
        // 8-tall gradient cropped to 2 tall -> oy = (8-2)/2 = 3 -> rows 3,4.
        let img = GrayImage::from_raw(1, 8, vec![0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        let c = crop_center(&img, 1, 2);
        assert_eq!(c.get_pixel(0, 0)[0], 3);
        assert_eq!(c.get_pixel(0, 1)[0], 4);
    }

    #[test]
    fn crop_top_anchors_at_y_zero_not_centered() {
        // #111: crop_top must take the TOP rows (oy=0), NOT the centered rows crop_center
        // would. 8-tall gradient cropped to 2 tall -> rows 0,1 (not 3,4).
        let img = GrayImage::from_raw(1, 8, vec![0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        let c = crop_top(&img, 1, 2);
        assert_eq!(
            c.get_pixel(0, 0)[0],
            0,
            "crop_top must start at the top row"
        );
        assert_eq!(c.get_pixel(0, 1)[0], 1);
        // And it must differ from a centered crop (proves it's top-anchored, not centered).
        let center = crop_center(&img, 1, 2);
        assert_ne!(
            c.get_pixel(0, 0)[0],
            center.get_pixel(0, 0)[0],
            "crop_top must NOT equal crop_center (the #111 top-anchor)"
        );
    }

    #[test]
    fn crop_top_is_horizontally_centered() {
        // 8-wide single row -> crop to 2 wide is horizontally centered: ox=(8-2)/2=3.
        let img = GrayImage::from_raw(8, 1, vec![0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        let c = crop_top(&img, 2, 1);
        assert_eq!(c.get_pixel(0, 0)[0], 3);
        assert_eq!(c.get_pixel(1, 0)[0], 4);
    }
}
