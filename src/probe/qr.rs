//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::luma::{bgra_to_luma, crop_center, crop_center_luma, uyvy_to_luma};
use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};

/// Vertical placement of the painted QR within the canvas.
///
/// - `Center` — vertically centered (the original single-QR / Phase-1 loopback layout).
/// - `Top` — anchored to the TOP band with [`TOP_MARGIN_PX`] of clearance from the top
///   edge. The #111 4-corner layout: the camera dual-QR sits in the TOP band so the
///   strih/stream render-time burns (drawn ~300px in the BOTTOM corners by the DistroAV
///   burn filter) stay fully clear of it in the composited stream recording. Without this
///   the camera QR was vertically centered and the center-bottom burn covered ~220px of
///   each half — the readability failure #111 fixes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VAnchor {
    Center,
    Top,
}

/// Top-edge clearance (px) for [`VAnchor::Top`] — the camera dual-QR's top row sits this
/// far below the frame top, leaving the rest of the frame's lower region for the bottom
/// burns. Kept modest so a ~700px QR + this margin still ends well above the bottom-corner
/// burns on a 1080-tall frame (700 + 24 = 724 < the burn band start ~740).
pub const TOP_MARGIN_PX: u32 = 24;

/// Top-left y origin for a `qh`-tall QR on a `canvas_h`-tall canvas under `anchor`.
/// Pure geometry so the no-overlap test can assert the camera QR vs burn rectangles
/// without rendering. `Top` clamps so a too-tall QR never starts above the frame.
pub fn qr_origin_y(canvas_h: u32, qh: u32, anchor: VAnchor) -> u32 {
    match anchor {
        VAnchor::Center => (canvas_h.saturating_sub(qh)) / 2,
        VAnchor::Top => TOP_MARGIN_PX.min(canvas_h.saturating_sub(qh)),
    }
}

/// Blit `payload`'s QR (EC-H), centered within the horizontal band
/// `[band_x, band_x + band_w)` and vertically placed per `anchor`, onto an existing white
/// BGRA `canvas`.
// Private blit helper with intentionally positional geometry args (canvas dims, band,
// payload, size, vertical anchor); the two call sites pass them inline and a parameter
// struct would only add indirection for one internal helper.
#[allow(clippy::too_many_arguments)]
fn blit_qr_bgra(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    band_x: u32,
    band_w: u32,
    payload: &Payload,
    qr_size: u32,
    anchor: VAnchor,
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
    let oy = qr_origin_y(canvas_h, qh, anchor);
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
        VAnchor::Center,
    );
    canvas
}

/// Two QRs side by side in the TOP band: `left` centered in `[0, w/2)`, `right` in
/// `[w/2, w)`, both anchored to the top (#111 4-corner layout — the camera dual-QR stays
/// in the top band so the strih/stream bottom-corner burns never overlap it).
pub fn render_qr_dual_bgra(
    left: &Payload,
    right: &Payload,
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
) -> Vec<u8> {
    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize];
    let half = canvas_w / 2;
    blit_qr_bgra(
        &mut canvas,
        canvas_w,
        canvas_h,
        0,
        half,
        left,
        qr_size,
        VAnchor::Center,
    );
    blit_qr_bgra(
        &mut canvas,
        canvas_w,
        canvas_h,
        half,
        canvas_w - half,
        right,
        qr_size,
        VAnchor::Center,
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

/// Long-side cap (px) the single-QR ROI is downscaled to before `rqrr`. Decouples
/// the on-screen QR size from the decode cost: a BIG QR (low spatial frequency →
/// survives the DistroAV NDI re-compression at the OBS outputs, ~0.5% torn instead of
/// ~3%) can be used while the decode ROI is shrunk to this cap so the dev1 tap still
/// tracks 30 fps. The big-module QR is already past NDI compression by the time the tap
/// has it, so downscaling for decode is lossless to the pattern.
const SINGLE_DECODE_CAP: u32 = 760;

/// Downscale a luma image so its long side is at most `cap` px (Triangle filter);
/// returns it unchanged when already within `cap`.
fn downscale_luma(img: GrayImage, cap: u32) -> GrayImage {
    let m = img.width().max(img.height());
    if m <= cap {
        return img;
    }
    let nw = (img.width() * cap / m).max(1);
    let nh = (img.height() * cap / m).max(1);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// Turn one captured NDI frame into a decoded `Payload`, or None.
/// Dispatches BGRA/BGRX vs UYVY by fourcc, converts to luma (padded-stride
/// aware), restricts the QR decode to the centered `decode_crop` square (the
/// ROI speed fix), downscales it to `SINGLE_DECODE_CAP` so a big QR still decodes
/// fast, and decodes. Shared by the single-tap reader and the multi-tap reader so
/// the decode path has one tested implementation.
pub fn decode_capture(
    fourcc: u32,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    decode_crop: u32,
) -> Option<Payload> {
    // Convert ONLY the centered QR ROI from the raw frame (skip the full-frame luma),
    // then downscale it so the live tap tracks a full 30 fps even with a big QR.
    let img = crop_center_luma(
        fourcc,
        data,
        width,
        height,
        stride,
        decode_crop,
        decode_crop,
    );
    decode_qr_luma(downscale_luma(img, SINGLE_DECODE_CAP))
}

/// Width (px) the dual-QR band is downscaled to before the single `rqrr` pass. Both QRs
/// live in ONE horizontal band, so dual decode is ONE prepare+detect (rqrr finds both
/// grids), not two — that is what keeps the 3 concurrent dev1 taps tracking 30 fps (two
/// separate ROI passes bottlenecked them to ~12-15 fps, dropping half the frames at the
/// NDI receiver and inflating apparent loss). 1280 px keeps each ~700 px QR at ~470 px
/// (rqrr needs a few px/module) while keeping the prepare cost at ~the single-QR path's.
const DUAL_BAND_WIDTH: u32 = 1280;

/// Run one rqrr prepare+detect pass over a luma image, returning all CRC-valid payloads.
fn rqrr_decode_all(img: GrayImage) -> Vec<Payload> {
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let mut out = Vec::new();
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if let Some(p) = Payload::decode(&content) {
                out.push(p);
            }
        }
    }
    out
}

/// Otsu's global threshold (0..=255) maximizing between-class variance of a gray
/// histogram. PURE + total: an empty/flat image returns 128 (a neutral mid-gray cut).
/// Used to binarize a SOFT optical capture before rqrr (a QR filmed off a monitor is
/// low-contrast/anti-aliased gray8; rqrr's own adaptive prepare can miss it, but a hard
/// black/white cut at the Otsu split recovers it — proven on the live cam1 grab).
pub fn otsu_threshold(hist: &[u64; 256]) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut w_bg, mut sum_bg) = (0u64, 0.0f64);
    // Track the between-class-variance PLATEAU (a clean bimodal histogram maximizes the
    // variance across the whole gap between the two peaks), and return its MIDPOINT — so a
    // dark peak at 20 and a light peak at 230 cut at ~125, not at the dark peak itself
    // (which would binarize the dark cluster to white). This is the standard Otsu
    // plateau-averaging refinement.
    let (mut best_var, mut plateau_lo, mut plateau_hi) = (-1.0f64, 128usize, 128usize);
    for (t, &count) in hist.iter().enumerate() {
        w_bg += count;
        if w_bg == 0 {
            continue;
        }
        let w_fg = total - w_bg;
        if w_fg == 0 {
            break;
        }
        sum_bg += t as f64 * count as f64;
        let m_bg = sum_bg / w_bg as f64;
        let m_fg = (sum_all - sum_bg) / w_fg as f64;
        let between = w_bg as f64 * w_fg as f64 * (m_bg - m_fg) * (m_bg - m_fg);
        if between > best_var + f64::EPSILON {
            best_var = between;
            plateau_lo = t;
            plateau_hi = t;
        } else if (between - best_var).abs() <= f64::EPSILON {
            plateau_hi = t; // extend the plateau
        }
    }
    ((plateau_lo + plateau_hi) / 2) as u8
}

/// Binarize a luma image at its Otsu threshold (>= threshold → 255, else 0). The hard
/// black/white image is what rqrr's finder pattern locking needs from a soft capture.
fn binarize_otsu(img: &GrayImage) -> GrayImage {
    let mut hist = [0u64; 256];
    for p in img.pixels() {
        hist[p.0[0] as usize] += 1;
    }
    let t = otsu_threshold(&hist);
    let mut out = img.clone();
    for p in out.pixels_mut() {
        p.0[0] = if p.0[0] >= t { 255 } else { 0 };
    }
    out
}

/// Decode ALL CRC-valid QR payloads in one grayscale image. First the plain rqrr pass
/// (rqrr's own adaptive prepare — best for the clean genlocked strih/stream recordings);
/// if it finds NOTHING, retry once on the Otsu-binarized image. The binarized retry
/// recovers the SOFT optical cam1 grab (a QR filmed off a monitor at gray8) that the
/// plain pass misses, WITHOUT changing the clean-path result (which already decodes on
/// pass 1, so the retry never runs there). `detect_grids` returns every QR in one pass,
/// so the two side-by-side dual-QR codes are read together.
pub fn decode_qr_luma_all(img: GrayImage) -> Vec<Payload> {
    let first = rqrr_decode_all(img.clone());
    if !first.is_empty() {
        return first;
    }
    // Nothing on the plain pass — the soft optical capture. Retry on a hard Otsu cut.
    rqrr_decode_all(binarize_otsu(&img))
}

/// Decode a dual-QR frame and reconcile. Both QRs sit in one horizontal band across the
/// full width (left QR in the left half, right in the right half), so we crop that single
/// band, downscale it to `DUAL_BAND_WIDTH`, and decode it in ONE `rqrr` pass that finds
/// BOTH codes — roughly the cost of the single-QR path, which is what lets the dev1 taps
/// keep up with 30 fps. A blurred (mid-transition) QR fails CRC inside `Payload::decode`
/// and is dropped; the frame's identity is the CRC-valid payload with the highest
/// `frame_id` (freshest sharp region). At least one region is always sharp on the Vernier
/// display, so this returns `Some` for every well-framed capture. `None` only when neither
/// code decodes.
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
    // One full-width band tall enough to hold both QRs, then a single downscaled rqrr pass
    // over both. The #111 dual-QR is TOP-anchored (render_qr_dual_bgra → VAnchor::Center, so
    // the strih/stream bottom-corner burns never overlap it), so crop from the TOP — a
    // centered crop would miss the now-top QRs. The live multitap tap passes
    // roi = qr_size + 120, tall enough to cover the top margin + the full QR. crop_top
    // clamps the requested size to the image.
    let band_h = roi.min(height);
    let band = crop_center(&full, width, band_h);
    let band = if band.width() > DUAL_BAND_WIDTH {
        let nh = (band.height() * DUAL_BAND_WIDTH / band.width()).max(1);
        image::imageops::resize(
            &band,
            DUAL_BAND_WIDTH,
            nh,
            image::imageops::FilterType::Triangle,
        )
    } else {
        band
    };
    decode_qr_luma_all(band)
        .into_iter()
        .max_by_key(|p| p.frame_id)
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
    fn otsu_splits_a_bimodal_histogram_between_the_two_peaks() {
        // A clean black/white image: mass at 0 and 255. Otsu must cut between them.
        let mut hist = [0u64; 256];
        hist[20] = 1000; // dark cluster
        hist[230] = 1000; // light cluster
        let t = super::otsu_threshold(&hist);
        assert!(
            t > 20 && t < 230,
            "threshold between the two peaks, got {t}"
        );
    }

    #[test]
    fn otsu_empty_histogram_is_neutral_midgray() {
        assert_eq!(super::otsu_threshold(&[0u64; 256]), 128);
    }

    #[test]
    fn decode_recovers_soft_low_contrast_qr_via_binarized_retry() {
        // A SOFT optical capture: render a QR, then compress its dynamic range into a
        // narrow low-contrast band (sim. a QR filmed off a monitor at gray8). The plain
        // rqrr pass struggles; the Otsu-binarized retry in decode_qr_luma_all recovers it.
        let p = Payload {
            run_id: 9,
            frame_id: 123,
            gen_ts_ns: 7,
        };
        let bgra = render_qr_bgra(&p, 1280, 720, 600);
        let mut luma = bgra_to_luma(&bgra, 1280, 720, 1280 * 4);
        // Squash contrast: map 0..255 -> ~96..160 (a soft, low-contrast gray8 capture).
        for px in luma.pixels_mut() {
            px.0[0] = 96 + (px.0[0] as u32 * 64 / 255) as u8;
        }
        let got = decode_qr_luma_all(luma);
        assert!(
            got.contains(&p),
            "the Otsu-binarized retry must recover the soft low-contrast QR"
        );
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
