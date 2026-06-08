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
}
