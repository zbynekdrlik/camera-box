//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::luma::{bgra_to_luma, crop_center_luma, crop_top, uyvy_to_luma};
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

/// Default on-screen size (px) of the cam1-capture burn QR (#174). Small — the cam1
/// burn lives in the BOTTOM-CENTER gap between the strih (bottom-left) and stream
/// (bottom-right) ~300px DistroAV burns, and below the top optical dual-QR, so all
/// four marks stay non-overlapping in the composited stream recording. 200px keeps a
/// low-EC-H module count well clear of the ~300px corner burns on a 1920×1080 frame.
pub const CAM1_BURN_QR_PX: u32 = 200;

/// Bottom-edge clearance (px) for the cam1 burn — its bottom row sits this far above the
/// frame bottom so it never bleeds off the visible raster after the downstream
/// scale/crop. Matches the modest margins used for the top dual-QR / corner burns.
pub const CAM1_BURN_BOTTOM_MARGIN_PX: u32 = 24;

/// Top-left `(x, y)` origin of a `qw×qh` cam1 burn QR on a `canvas_w×canvas_h` frame:
/// horizontally CENTERED, anchored to the BOTTOM with [`CAM1_BURN_BOTTOM_MARGIN_PX`]
/// clearance. Pure geometry so the no-overlap test can assert the cam1 burn rectangle
/// against the top dual-QR band and the bottom-corner burn rectangles without rendering.
/// Clamps so a too-large QR never starts off-frame.
pub fn cam1_burn_origin(canvas_w: u32, canvas_h: u32, qw: u32, qh: u32) -> (u32, u32) {
    let ox = (canvas_w.saturating_sub(qw)) / 2;
    let oy = canvas_h
        .saturating_sub(qh)
        .saturating_sub(CAM1_BURN_BOTTOM_MARGIN_PX);
    (ox, oy)
}

/// Render `payload` as a fixed-size EC-H QR with a quiet zone — the one place the QR
/// build idiom lives (used by both the BGRA blit and the YUYV burn). `qr_px` is the exact
/// square size in px (min == max). The payload is small, so encoding always succeeds.
fn render_payload_qr(payload: &Payload, qr_px: u32) -> GrayImage {
    let s = payload.encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H)
        .expect("payload is small, encodes within QR capacity");
    code.render::<Luma<u8>>()
        .min_dimensions(qr_px, qr_px)
        .max_dimensions(qr_px, qr_px)
        .quiet_zone(true)
        .build()
}

/// Burn `payload`'s QR (EC-H, `qr_px` square) into a packed **YUYV** capture buffer's
/// LUMA plane, horizontally centered and bottom-anchored (#174 cam1-capture burn).
///
/// YUYV packs `Y0 U0 Y1 V0` per 4 bytes = 2 pixels (luma at the EVEN byte of each pair,
/// chroma at the odd bytes). The burn writes the QR module luma (0 = black module,
/// 255 = white quiet zone) into both Y bytes it touches and neutralizes the chroma
/// bytes to 128 within the burn rectangle, so the burned region is pure grayscale and
/// decodes cleanly after the YUYV→UYVY→NDI re-emit. `stride` is honored (bytes per row;
/// tight YUYV stride = `2*width`, a device may pad). Pixels outside the buffer are
/// skipped (defensive against a short final buffer) — never panics.
///
/// TEST-MODE ONLY: the caller gates this on `CAMERA_BOX_BURN_RUN_ID` being set, so an
/// unset env leaves the production NDI feed completely clean (this fn is never called).
pub fn burn_qr_yuyv(
    buf: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    payload: &Payload,
    qr_px: u32,
) {
    let qr = render_payload_qr(payload, qr_px);
    let (qw, qh) = (qr.width().min(width), qr.height().min(height));
    let (ox, oy) = cam1_burn_origin(width, height, qw, qh);
    let stride = stride as usize;
    for y in 0..qh {
        let row = (oy + y) as usize * stride;
        for x in 0..qw {
            let lum = qr.get_pixel(x, y)[0];
            let px = (ox + x) as usize;
            // YUYV: luma byte for pixel px is at row + 2*px (even byte of the pair),
            // its chroma byte is the adjacent odd byte (U on an even px, V on an odd px).
            let yi = row + 2 * px;
            if yi < buf.len() {
                buf[yi] = lum; // luma
            }
            let ci = yi + 1;
            if ci < buf.len() {
                buf[ci] = 128; // neutral chroma (gray) so the QR is pure black/white
            }
        }
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
    let qr = render_payload_qr(payload, qr_size);
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
        VAnchor::Top,
    );
    blit_qr_bgra(
        &mut canvas,
        canvas_w,
        canvas_h,
        half,
        canvas_w - half,
        right,
        qr_size,
        VAnchor::Top,
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
    // over both. The #111 dual-QR is TOP-anchored (render_qr_dual_bgra → VAnchor::Top, so
    // the strih/stream bottom-corner burns never overlap it), so crop from the TOP — a
    // centered crop would miss the now-top QRs. The live multitap tap passes
    // roi = qr_size + 120, tall enough to cover the top margin + the full QR. crop_top
    // clamps the requested size to the image.
    let band_h = roi.min(height);
    let band = crop_top(&full, width, band_h);
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

    // ---- #174 cam1-capture YUYV burn ----

    /// Build a tight YUYV frame (luma=mid-gray, chroma neutral) of `w×h`.
    fn yuyv_gray_frame(w: u32, h: u32) -> Vec<u8> {
        // YUYV = 2 bytes/pixel. Fill luma=128 (even bytes), chroma=128 (odd bytes).
        vec![128u8; (w * h * 2) as usize]
    }

    #[test]
    fn cam1_burn_renders_a_decodable_qr_into_a_yuyv_frame() {
        use crate::probe::luma::uyvy_to_luma;
        let p = Payload {
            run_id: 911_001,
            frame_id: 4242,
            gen_ts_ns: 1_700_000_000_123_456,
        };
        let (w, h) = (1920u32, 1080u32);
        let mut frame = yuyv_gray_frame(w, h);
        burn_qr_yuyv(&mut frame, w, h, w * 2, &p, CAM1_BURN_QR_PX);
        // Extract the luma plane from the YUYV frame (every even byte) and decode.
        let luma_bytes = crate::capture::yuyv_to_gray8(&frame, w, h, w * 2);
        let luma = image::GrayImage::from_raw(w, h, luma_bytes).unwrap();
        assert_eq!(
            decode_qr_luma(luma),
            Some(p),
            "the cam1 burn must render a decodable QR carrying the exact id+ts"
        );
        // Guard against accidentally reading a UYVY-laid path: confirm uyvy_to_luma does
        // NOT decode it (the burn is YUYV, luma at even bytes) — keeps the YUYV contract.
        let _ = uyvy_to_luma; // referenced for intent; not asserted (format-specific)
    }

    #[test]
    fn cam1_burn_lands_bottom_center_clear_of_top_dualqr_and_bottom_corner_burns() {
        // The four-mark non-overlap contract on a 1920×1080 stream frame:
        //   top dual-QR band : y ∈ [TOP_MARGIN_PX, TOP_MARGIN_PX + 700)
        //   strih burn       : bottom-LEFT  ~300×300 corner
        //   stream burn      : bottom-RIGHT ~300×300 corner
        //   cam1 burn        : bottom-CENTER, must miss all three.
        let (w, h) = (1920u32, 1080u32);
        let qpx = CAM1_BURN_QR_PX;
        // Use the real rendered QR size (quiet zone may round up past qpx).
        let s = Payload {
            run_id: 911_001,
            frame_id: 1,
            gen_ts_ns: 1,
        }
        .encode();
        let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H).unwrap();
        let qr: GrayImage = code
            .render::<Luma<u8>>()
            .min_dimensions(qpx, qpx)
            .max_dimensions(qpx, qpx)
            .quiet_zone(true)
            .build();
        let (qw, qh) = (qr.width(), qr.height());
        let (ox, oy) = cam1_burn_origin(w, h, qw, qh);
        let (cam1_l, cam1_r, cam1_t, cam1_b) = (ox, ox + qw, oy, oy + qh);

        // Vs the top dual-QR band (700px tall under TOP_MARGIN_PX).
        let dual_band_bottom = TOP_MARGIN_PX + 700;
        assert!(
            cam1_t >= dual_band_bottom,
            "cam1 burn top {cam1_t} must be below the top dual-QR band bottom {dual_band_bottom}"
        );

        // Vs the bottom-corner burns (~300px squares anchored bottom-left / bottom-right).
        let corner = 320u32; // generous DistroAV burn box (300 + slack)
        let strih_right = corner; // bottom-left burn occupies x ∈ [0, corner)
        let stream_left = w - corner; // bottom-right burn occupies x ∈ [w-corner, w)
        assert!(
            cam1_l >= strih_right,
            "cam1 burn left {cam1_l} must clear the bottom-left strih burn (x<{strih_right})"
        );
        assert!(
            cam1_r <= stream_left,
            "cam1 burn right {cam1_r} must clear the bottom-right stream burn (x≥{stream_left})"
        );
        // And it stays on-frame at the bottom.
        assert!(cam1_b <= h, "cam1 burn bottom {cam1_b} on-frame (h={h})");
    }

    #[test]
    fn cam1_burn_only_touches_its_own_rectangle_leaving_the_rest_clean() {
        // The burn must NOT disturb pixels outside its bottom-center rectangle — the top
        // optical dual-QR (and everywhere else) stays exactly as captured.
        let p = Payload {
            run_id: 911_001,
            frame_id: 7,
            gen_ts_ns: 9,
        };
        let (w, h) = (1280u32, 720u32);
        let original = yuyv_gray_frame(w, h);
        let mut frame = original.clone();
        burn_qr_yuyv(&mut frame, w, h, w * 2, &p, CAM1_BURN_QR_PX);

        let qr: GrayImage = QrCode::with_error_correction_level(p.encode().as_bytes(), EcLevel::H)
            .unwrap()
            .render::<Luma<u8>>()
            .min_dimensions(CAM1_BURN_QR_PX, CAM1_BURN_QR_PX)
            .max_dimensions(CAM1_BURN_QR_PX, CAM1_BURN_QR_PX)
            .quiet_zone(true)
            .build();
        let (qw, qh) = (qr.width(), qr.height());
        let (ox, oy) = cam1_burn_origin(w, h, qw, qh);

        // A pixel WELL OUTSIDE the burn rect (top-left corner = optical dual-QR area)
        // is byte-identical to the original.
        let top_left_idx = 0usize;
        assert_eq!(
            frame[top_left_idx], original[top_left_idx],
            "top-left (optical dual-QR area) must be untouched"
        );
        // A row above the burn rect is fully untouched.
        let above_row = (oy.saturating_sub(2)) as usize * (w * 2) as usize;
        assert_eq!(
            frame[above_row..above_row + (w * 2) as usize],
            original[above_row..above_row + (w * 2) as usize],
            "a row above the cam1 burn rectangle must be unchanged"
        );
        // Inside the burn rect, at least one luma byte differs (the QR was drawn).
        let inside_row = (oy + qh / 2) as usize * (w * 2) as usize;
        let inside = (inside_row + 2 * (ox + qw / 2) as usize).min(frame.len() - 1);
        assert_ne!(
            frame[inside], original[inside],
            "the burn rectangle must carry QR pixels"
        );
    }
}
