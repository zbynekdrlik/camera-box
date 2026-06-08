//! Extract a grayscale (luma) image from captured frames for QR decoding.

use image::GrayImage;

/// UYVY 4:2:2 → luma. Layout per 2 px: [U, Y0, V, Y1]; luma is every odd byte.
pub fn uyvy_to_luma(data: &[u8], width: u32, height: u32) -> GrayImage {
    let n = (width as usize) * (height as usize);
    let mut buf = vec![0u8; n];
    for (i, px) in buf.iter_mut().enumerate() {
        let idx = i * 2 + 1;
        if idx < data.len() {
            *px = data[idx];
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

/// BGRA → luma via BT.601 integer weights.
pub fn bgra_to_luma(data: &[u8], width: u32, height: u32) -> GrayImage {
    let n = (width as usize) * (height as usize);
    let mut buf = vec![0u8; n];
    for (i, px) in buf.iter_mut().enumerate() {
        let o = i * 4;
        if o + 2 < data.len() {
            let b = data[o] as u32;
            let g = data[o + 1] as u32;
            let r = data[o + 2] as u32;
            *px = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

/// Crop a centered `cw`×`ch` region from a grayscale image (clamped to bounds).
/// Used to limit QR decoding to the region where the QR is painted, which keeps
/// per-frame decode fast enough to track the capture in real time.
pub fn crop_center(img: &GrayImage, cw: u32, ch: u32) -> GrayImage {
    let (w, h) = (img.width(), img.height());
    let cw = cw.clamp(1, w);
    let ch = ch.clamp(1, h);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn crop_center_clamps_to_bounds() {
        let img = GrayImage::from_raw(2, 2, vec![1, 2, 3, 4]).unwrap();
        let c = crop_center(&img, 99, 99);
        assert_eq!(c.dimensions(), (2, 2));
    }

    #[test]
    fn uyvy_picks_luma_bytes() {
        let img = uyvy_to_luma(&[10, 200, 20, 100], 2, 1);
        assert_eq!(img.get_pixel(0, 0)[0], 200);
        assert_eq!(img.get_pixel(1, 0)[0], 100);
    }

    #[test]
    fn bgra_white_and_black() {
        let data = [255, 255, 255, 255, 0, 0, 0, 255];
        let img = bgra_to_luma(&data, 2, 1);
        assert_eq!(img.get_pixel(0, 0)[0], 255);
        assert_eq!(img.get_pixel(1, 0)[0], 0);
    }
}
