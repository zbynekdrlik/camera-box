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

#[cfg(test)]
mod tests {
    use super::*;

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
