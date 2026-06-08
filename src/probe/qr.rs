//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};

/// Render `payload` as a QR (EC level H), centered on a white BGRA canvas.
/// Returns a `canvas_w * canvas_h * 4` BGRA byte buffer.
pub fn render_qr_bgra(payload: &Payload, canvas_w: u32, canvas_h: u32, qr_size: u32) -> Vec<u8> {
    let s = payload.encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H)
        .expect("payload is small, encodes within QR capacity");
    let qr: GrayImage = code
        .render::<Luma<u8>>()
        .min_dimensions(qr_size, qr_size)
        .max_dimensions(qr_size, qr_size)
        .quiet_zone(true)
        .build();

    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize]; // white BGRA
    let (qw, qh) = (qr.width().min(canvas_w), qr.height().min(canvas_h));
    let ox = (canvas_w - qw) / 2;
    let oy = (canvas_h - qh) / 2;
    for y in 0..qh {
        for x in 0..qw {
            let lum = qr.get_pixel(x, y)[0];
            let ci = (((oy + y) * canvas_w + (ox + x)) * 4) as usize;
            canvas[ci] = lum;
            canvas[ci + 1] = lum;
            canvas[ci + 2] = lum;
            canvas[ci + 3] = 255;
        }
    }
    canvas
}

/// Decode the first QR found in a grayscale image into a Payload, or None.
pub fn decode_qr_luma(img: GrayImage) -> Option<Payload> {
    let mut prepared = rqrr::PreparedImage::prepare(img);
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if let Some(p) = Payload::decode(&content) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::luma::bgra_to_luma;
    use image::imageops::{resize, FilterType};

    fn sample() -> Payload {
        Payload {
            run_id: 7,
            frame_id: 12345,
            gen_ts_ns: 9_876_543_210,
        }
    }

    #[test]
    fn clean_roundtrip() {
        let p = sample();
        let bgra = render_qr_bgra(&p, 1280, 720, 600);
        let luma = bgra_to_luma(&bgra, 1280, 720, 1280 * 4);
        assert_eq!(decode_qr_luma(luma), Some(p));
    }

    #[test]
    fn survives_downscale_and_noise() {
        let p = sample();
        let bgra = render_qr_bgra(&p, 1920, 1080, 700);
        let full = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);

        let small = resize(&full, 960, 540, FilterType::Triangle);
        let mut back = resize(&small, 1920, 1080, FilterType::Triangle);

        for (i, px) in back.iter_mut().enumerate() {
            let d: i16 = if i % 3 == 0 { 6 } else { -6 };
            *px = (*px as i16 + d).clamp(0, 255) as u8;
        }

        assert_eq!(decode_qr_luma(back), Some(p));
    }

    #[test]
    fn blank_image_decodes_to_none() {
        let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
        assert_eq!(decode_qr_luma(blank), None);
    }

    #[test]
    fn render_centers_qr_and_writes_gray_bgra() {
        // Asymmetric canvas so x- and y-centering are exercised distinctly.
        let p = Payload {
            run_id: 1,
            frame_id: 2,
            gen_ts_ns: 3,
        };
        let (cw, ch, qs) = (1000u32, 800u32, 400u32);
        let canvas = render_qr_bgra(&p, cw, ch, qs);
        assert_eq!(canvas.len(), (cw * ch * 4) as usize);

        let (mut min_x, mut max_x, mut min_y, mut max_y) = (cw, 0u32, ch, 0u32);
        for y in 0..ch {
            for x in 0..cw {
                let i = ((y * cw + x) * 4) as usize;
                let (b, g, r, a) = (canvas[i], canvas[i + 1], canvas[i + 2], canvas[i + 3]);
                // Every pixel: opaque, and gray (B==G==R) — white background or QR module.
                assert_eq!(a, 255, "alpha must be 255 at ({x},{y})");
                assert!(b == g && g == r, "B==G==R at ({x},{y}): {b},{g},{r}");
                if b != 255 {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        // The QR's non-white bounding box must be centered: equal margins each side.
        let (left, right) = (min_x as i64, (cw - 1 - max_x) as i64);
        let (top, bottom) = (min_y as i64, (ch - 1 - max_y) as i64);
        assert!(
            (left - right).abs() <= 1,
            "x-centered: left={left} right={right}"
        );
        assert!(
            (top - bottom).abs() <= 1,
            "y-centered: top={top} bottom={bottom}"
        );
    }
}
