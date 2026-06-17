//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::luma::{bgra_to_luma, crop_center, uyvy_to_luma};
use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};

/// Blit `payload`'s QR (EC-H), centered within the horizontal band
/// `[band_x, band_x + band_w)`, onto an existing white BGRA `canvas`.
fn blit_qr_bgra(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    band_x: u32,
    band_w: u32,
    payload: &Payload,
    qr_size: u32,
) {
    let s = payload.encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H)
        .expect("payload is small, encodes within QR capacity");
    let qr: GrayImage = code
        .render::<Luma<u8>>()
        .min_dimensions(qr_size, qr_size)
        .max_dimensions(qr_size, qr_size)
        .quiet_zone(true)
        .build();
    let (qw, qh) = (qr.width().min(band_w), qr.height().min(canvas_h));
    let ox = band_x + (band_w - qw) / 2;
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
}

/// Render `payload` as a QR (EC level H), centered on a white BGRA canvas.
/// Returns a `canvas_w * canvas_h * 4` BGRA byte buffer.
pub fn render_qr_bgra(payload: &Payload, canvas_w: u32, canvas_h: u32, qr_size: u32) -> Vec<u8> {
    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize]; // white BGRA
    blit_qr_bgra(
        &mut canvas,
        canvas_w,
        canvas_h,
        0,
        canvas_w,
        payload,
        qr_size,
    );
    canvas
}

/// Two QRs side by side: `left` centered in `[0, w/2)`, `right` in `[w/2, w)`.
pub fn render_qr_dual_bgra(
    left: &Payload,
    right: &Payload,
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
) -> Vec<u8> {
    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize];
    let half = canvas_w / 2;
    blit_qr_bgra(&mut canvas, canvas_w, canvas_h, 0, half, left, qr_size);
    blit_qr_bgra(
        &mut canvas,
        canvas_w,
        canvas_h,
        half,
        canvas_w - half,
        right,
        qr_size,
    );
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

/// Turn one captured NDI frame into a decoded `Payload`, or None.
/// Dispatches BGRA/BGRX vs UYVY by fourcc, converts to luma (padded-stride
/// aware), restricts the QR decode to the centered `decode_crop` square (the
/// ROI speed fix), and decodes. Shared by the single-tap reader and the
/// multi-tap reader so the decode path has one tested implementation.
pub fn decode_capture(
    fourcc: u32,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    decode_crop: u32,
) -> Option<Payload> {
    let full = match &fourcc.to_le_bytes() {
        b"BGRA" | b"BGRX" => bgra_to_luma(data, width, height, stride),
        _ => uyvy_to_luma(data, width, height, stride),
    };
    let img = crop_center(&full, decode_crop, decode_crop);
    decode_qr_luma(img)
}

/// Decode both QR regions of a dual-QR frame and reconcile. Each half is converted
/// to luma, the QR-ROI cropped, and decoded; a blurred (mid-transition) QR fails
/// CRC inside `Payload::decode` and is silently dropped. The frame's identity is
/// the CRC-valid payload with the highest `frame_id` (freshest sharp region); at
/// least one region is always sharp on the Vernier display, so this returns `Some`
/// for every well-framed capture. `None` only when neither half decodes.
pub fn decode_capture_dual(
    fourcc: u32,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    roi: u32,
) -> Option<Payload> {
    let full = match &fourcc.to_le_bytes() {
        b"BGRA" | b"BGRX" => bgra_to_luma(data, width, height, stride),
        _ => uyvy_to_luma(data, width, height, stride),
    };
    let half = width / 2;
    let left = image::imageops::crop_imm(&full, 0, 0, half, height).to_image();
    let right = image::imageops::crop_imm(&full, half, 0, width - half, height).to_image();
    let roi = roi.min(half).min(height);
    let cand = [
        decode_qr_luma(crop_center(&left, roi, roi)),
        decode_qr_luma(crop_center(&right, roi, roi)),
    ];
    cand.into_iter().flatten().max_by_key(|p| p.frame_id)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn decode_capture_roundtrips_bgra_frame() {
        let p = Payload {
            run_id: 3,
            frame_id: 99,
            gen_ts_ns: 42,
        };
        // 1920x1080 BGRA frame carrying a centered QR, tight stride.
        let bgra = render_qr_bgra(&p, 1920, 1080, 700);
        let fourcc = u32::from_le_bytes(*b"BGRA");
        let got = decode_capture(fourcc, &bgra, 1920, 1080, 1920 * 4, 820);
        assert_eq!(got, Some(p));
    }

    #[test]
    fn decode_capture_none_on_blank() {
        let blank = vec![255u8; (640 * 480 * 4) as usize];
        let fourcc = u32::from_le_bytes(*b"BGRA");
        assert_eq!(decode_capture(fourcc, &blank, 640, 480, 640 * 4, 400), None);
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

    #[test]
    fn dual_render_places_two_decodable_qrs_left_and_right() {
        let l = Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        };
        let r = Payload {
            run_id: 7,
            frame_id: 101,
            gen_ts_ns: 2,
        };
        let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
        let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
        assert_eq!(bgra.len(), (cw * ch * 4) as usize);
        let full = bgra_to_luma(&bgra, cw, ch, cw * 4);
        // Left half image and right half image each decode to their own payload.
        let left_img = image::imageops::crop_imm(&full, 0, 0, cw / 2, ch).to_image();
        let right_img = image::imageops::crop_imm(&full, cw / 2, 0, cw / 2, ch).to_image();
        assert_eq!(decode_qr_luma(left_img), Some(l));
        assert_eq!(decode_qr_luma(right_img), Some(r));
    }

    #[test]
    fn dual_decode_returns_highest_frame_id_and_tolerates_one_blurred() {
        let l = Payload {
            run_id: 7,
            frame_id: 200,
            gen_ts_ns: 1,
        };
        let r = Payload {
            run_id: 7,
            frame_id: 201,
            gen_ts_ns: 2,
        };
        let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
        let fourcc = u32::from_le_bytes(*b"BGRA");

        // Both sharp -> highest frame_id (201).
        let both = render_qr_dual_bgra(&l, &r, cw, ch, qs);
        assert_eq!(
            decode_capture_dual(fourcc, &both, cw, ch, cw * 4, 620),
            Some(r)
        );

        // Right region blanked (simulating an unreadable/blurred QR) -> falls back to left (200).
        let l_only = render_qr_dual_bgra(&l, &r, cw, ch, qs);
        let mut blanked = l_only.clone();
        let half = (cw / 2) as usize;
        for y in 0..ch as usize {
            for x in half..cw as usize {
                let i = (y * cw as usize + x) * 4;
                blanked[i] = 255;
                blanked[i + 1] = 255;
                blanked[i + 2] = 255;
                blanked[i + 3] = 255;
            }
        }
        assert_eq!(
            decode_capture_dual(fourcc, &blanked, cw, ch, cw * 4, 620),
            Some(l)
        );
    }
}
