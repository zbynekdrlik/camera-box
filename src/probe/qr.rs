//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::burn_echo::{self, LocatedPayload};
use crate::probe::luma::{bgra_to_luma, crop_center_luma, crop_top, uyvy_to_luma};
use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

// issue 1374 — the recording decode core (the fast-then-robust family, the #202 tiles, the #754
// top band, the issue-1367 echo-gated core) lives in `recording_decode`. Re-exported here so every
// existing `qr::` call path keeps compiling unchanged.
pub use crate::probe::recording_decode::{
    decode_path_counts, decode_qr_luma_all_fast_then_robust,
    decode_qr_luma_all_fast_then_robust_gated, decode_qr_luma_all_fast_then_robust_grouped,
    decode_qr_luma_all_fast_then_robust_grouped_optical,
    decode_qr_luma_all_fast_then_robust_grouped_pathed,
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical,
    decode_qr_luma_all_fast_then_robust_pathed, decode_qr_luma_all_robust,
    decode_qr_luma_all_robust_optical, DecodePath,
};

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
/// burns on a 1080-tall frame (700 + 24 = 724 < the burn band start ~740). SINGLE source of
/// truth is [`crate::colour_scale::TOP_MARGIN_PX`] (the Tier-0 module that derives the colour
/// column's vertical span from it); re-exposed here at the QR module's documented path.
pub const TOP_MARGIN_PX: u32 = crate::colour_scale::TOP_MARGIN_PX;

/// Top-left y origin for a `qh`-tall QR on a `canvas_h`-tall canvas under `anchor`.
/// Pure geometry so the no-overlap test can assert the camera QR vs burn rectangles
/// without rendering. `Top` clamps so a too-tall QR never starts above the frame.
pub fn qr_origin_y(canvas_h: u32, qh: u32, anchor: VAnchor) -> u32 {
    match anchor {
        VAnchor::Center => (canvas_h.saturating_sub(qh)) / 2,
        VAnchor::Top => TOP_MARGIN_PX.min(canvas_h.saturating_sub(qh)),
    }
}

/// Default on-screen size (px) of the cam1-capture burn QR (#174, enlarged in #186).
/// The cam1 burn lives in the BOTTOM-CENTER gap between the strih (bottom-left) and
/// stream (bottom-right) corner burns, and below the top optical dual-QR. It is burned
/// at the cam1 CAPTURE resolution (≈1920×1080) and then RIDES through NDI → strih →
/// the stream box's 4K upscale, where a too-small burn went SOFT and rqrr missed it
/// (#186 — the cam1 bottom-center burn was the worst, 14 of 22 misses at 200px).
///
/// 320px is the largest that keeps the bottom-CENTER burn fully clear of the top dual-QR:
/// bottom-anchored, its top row sits at `h - CAM1_BURN_BOTTOM_MARGIN_PX - 320` = 736 on a
/// 1080-tall frame, below the top dual-QR's bottom (~724 at qr_size 700) — a real gap. The
/// 60% bigger modules (vs 200px) survive the 4K upscale so rqrr decodes EVERY frame's burn
/// (the #186 strict-zero gate requires it: a burn that does not decode is a defect, never
/// excluded). The bottom-center gap is ≈1240px wide (between the ~400px corner burns), so
/// 320px has ample horizontal clearance from both.
pub const CAM1_BURN_QR_PX: u32 = 320;

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
pub(crate) fn render_payload_qr(payload: &Payload, qr_px: u32) -> GrayImage {
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

/// Fill the #367 fixed colour-reference scale into a BGRA `canvas` (`canvas_w×canvas_h` —
/// the same buffer `render_qr_*_bgra` produced), per the pure
/// [`crate::colour_scale::colour_scale_patches`] layout. Each reference patch's rectangle
/// is filled with its known sRGB colour (BGRA byte order, opaque) in the VERTICAL column inside
/// the central gap between the two dual-QR halves (derived from `qr_size` + `top_margin` — the
/// SAME geometry `render_qr_dual_bgra` used). The painter calls this AFTER rendering the QR(s)
/// (when `--colour-scale` is on), so the recorded frame carries a colour reference between the
/// dual-QR halves — the per-patch sample the #364 colour gate compares against. No-op for a
/// degenerate layout (`colour_scale_patches` returns empty). Each write is bounds-checked so a
/// short/odd buffer is never indexed out of range (never panics).
pub fn blit_colour_scale_bgra(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
) {
    for (rect, rgb) in
        crate::colour_scale::colour_scale_patches(canvas_w, canvas_h, qr_size, top_margin)
    {
        let y_end = (rect.y + rect.h).min(canvas_h);
        let x_end = (rect.x + rect.w).min(canvas_w);
        for y in rect.y..y_end {
            for x in rect.x..x_end {
                let ci = (((y * canvas_w) + x) * 4) as usize;
                if ci + 3 < canvas.len() {
                    canvas[ci] = rgb.b; // B
                    canvas[ci + 1] = rgb.g; // G
                    canvas[ci + 2] = rgb.r; // R
                    canvas[ci + 3] = 255; // A (opaque)
                }
            }
        }
    }
}

/// Fill a pixel `rect` of a BGRA `canvas` (`canvas_w` wide) with an opaque `(b, g, r)` colour.
/// Every write is bounds-checked so a short/odd buffer is never indexed out of range.
fn fill_rect_bgra(
    canvas: &mut [u8],
    canvas_w: u32,
    rect: &crate::colour_scale::Rect,
    b: u8,
    g: u8,
    r: u8,
) {
    let x_end = rect.x + rect.w;
    let y_end = rect.y + rect.h;
    for y in rect.y..y_end {
        for x in rect.x..x_end {
            let ci = (((y * canvas_w) + x) * 4) as usize;
            if ci + 3 < canvas.len() {
                canvas[ci] = b;
                canvas[ci + 1] = g;
                canvas[ci + 2] = r;
                canvas[ci + 3] = 255;
            }
        }
    }
}

/// #751 — blit the constant-velocity motion sweep (the UFO-test judder indicator) into a BGRA
/// `canvas` for painter frame `frame_idx`: fill the bottom [`crate::motion_sweep::BAND_HEIGHT_PX`]
/// band with a dark track, then draw the bright sweeping ball on it. The band lives fully OUTSIDE
/// the top dual-QR decode zones AND the central colour-reference column (machine-proven in
/// `crate::motion_sweep`), so this never affects any QR / colour-patch read. The painter calls this
/// AFTER rendering the QR(s) + colour scale (when `--motion-sweep` is on, default in --paint-only).
pub fn blit_motion_sweep_bgra(canvas: &mut [u8], canvas_w: u32, canvas_h: u32, frame_idx: u64) {
    let band = crate::motion_sweep::sweep_band(canvas_w, canvas_h);
    let ball = crate::motion_sweep::ball_rect(frame_idx, canvas_w, canvas_h);
    // Dark track background first, then the bright ball on top (BGRA(0,255,255) = bright yellow —
    // max contrast on the dark track, and not a QR-like pattern).
    fill_rect_bgra(canvas, canvas_w, &band, 32, 32, 32);
    fill_rect_bgra(canvas, canvas_w, &ball, 0, 255, 255);
}

/// issue 1196 — blit the aux Vernier tick pair (two small EC-H QRs) into the bottom burn-free
/// gaps, per the pure [`crate::aux_tick::aux_tick_rects`] layout (derived from the SAME
/// `qr_size`/`top_margin` the dual-QR was rendered with, so the painter and the Tier-0 geometry
/// proofs agree). The caller passes payloads built from `vernier_ids` — LEFT carries the latest
/// EVEN tick, RIGHT the latest ODD tick — under `recording_latency::AUX_TICK_RUN_ID` with
/// `gen_ts_ns = 0` (constant, so a settled aux mark's rendered pixels are byte-identical across
/// ticks by construction — the #854 anti-blur property with zero extra state). No-op when the
/// layout cannot fit (`aux_tick_rects` returns `None`, e.g. the 2560-wide override canvas whose
/// width-scaled primary leaves no bottom strip). Each mark is clamped to its proven rectangle so
/// the blit can never reach outside it; every write is bounds-checked (never panics).
pub fn blit_aux_tick_bgra(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
    left: &Payload,
    right: &Payload,
) {
    let Some(rects) = crate::aux_tick::aux_tick_rects(canvas_w, canvas_h, qr_size, top_margin)
    else {
        return;
    };
    for (rect, payload) in rects.iter().zip([left, right]) {
        let qr = render_payload_qr(payload, rect.w.min(rect.h));
        let (qw, qh) = (qr.width().min(rect.w), qr.height().min(rect.h));
        // Center the (possibly module-rounded, slightly smaller) rendered QR inside its proven
        // rectangle — mirrors blit_qr_bgra's band-centering, keeps the clearances symmetric.
        let (ox, oy) = (rect.x + (rect.w - qw) / 2, rect.y + (rect.h - qh) / 2);
        for y in 0..qh {
            for x in 0..qw {
                let lum = qr.get_pixel(x, y)[0];
                let ci = (((oy + y) * canvas_w + (ox + x)) * 4) as usize;
                if ci + 3 < canvas.len() {
                    canvas[ci] = lum;
                    canvas[ci + 1] = lum;
                    canvas[ci + 2] = lum;
                    canvas[ci + 3] = 255;
                }
            }
        }
    }
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

/// Run one rqrr prepare+detect pass over a luma image, returning all CRC-valid payloads (no
/// panic guard: the tests' bare-rqrr baseline; production goes through [`rqrr_decode_all_catch`]).
#[cfg(test)]
fn rqrr_decode_all(img: GrayImage) -> Vec<Payload> {
    burn_echo::payloads(rqrr_decode_all_located(img))
}

/// [`rqrr_decode_all`] that also keeps WHERE each payload was read: the centre of its rqrr grid
/// (the mean of the four `bounds` corners) in `img` pixels. The issue-1367 echo gate needs it.
fn rqrr_decode_all_located(img: GrayImage) -> Vec<LocatedPayload> {
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let mut out = Vec::new();
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if let Some(payload) = Payload::decode(&content) {
                let b = &grid.bounds;
                out.push(LocatedPayload {
                    payload,
                    cx: b.iter().map(|p| f64::from(p.x)).sum::<f64>() / 4.0,
                    cy: b.iter().map(|p| f64::from(p.y)).sum::<f64>() / 4.0,
                });
            }
        }
    }
    out
}

/// Install (ONCE per process) a panic hook that SILENCES rqrr's internal
/// `assert!(scan >= 1)` (rqrr-0.9.3 identify/grid.rs) and chains every OTHER panic to the
/// previously-installed hook. The tile retry catches that assert (it is expected on a
/// degenerate tile — lead-in/teardown black frames, near-uniform crops), but the DEFAULT
/// panic hook still PRINTS the message + backtrace note to stderr for each one — on a
/// ~9000-frame recording with several tiles per frame that is thousands of dumps drowning
/// real errors. Suppressing only the rqrr-grid assert keeps every genuine panic loud. The
/// chain (not a replace) preserves whatever hook was set before (e.g. a test harness's).
fn install_rqrr_assert_silencer() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // rqrr's grid identifier panics with the literal "scan >= 1" assert; its file
            // path contains "rqrr". Match either so a version bump that rewords the message
            // still suppresses the SAME caught assert, never an unrelated panic.
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("");
            let from_rqrr = info
                .location()
                .map(|l| l.file().contains("rqrr"))
                .unwrap_or(false);
            if from_rqrr && msg.contains("scan >= 1") {
                return; // expected, caught by rqrr_decode_all_catch — stay silent
            }
            prev(info); // every other panic: report exactly as before
        }));
    });
}

/// Panic-safe wrapper around [`rqrr_decode_all`]: rqrr's grid identifier has an internal
/// `assert!(scan >= 1)` (rqrr-0.9.3 identify/grid.rs:206), and its `Perspective::map` has a
/// SEPARATE `assert!(x <= i32::MAX as f64)` (geometry.rs:55) — both PANIC on certain
/// degenerate finder geometries (a near-empty/pathologically-small tile for the first; a
/// near-degenerate homography for the second). A panic here would abort the whole frame's
/// decode (and, via the worker pool, the whole `--extract-partial` run). Catch it and treat a
/// panicking pass as "found nothing" (the other pass/tile still covers the frame). The
/// [`install_rqrr_assert_silencer`] hook keeps the already-known `scan >= 1` assert from
/// spamming stderr while leaving every other panic's default stderr report intact; EITHER way
/// `catch_unwind` here still catches it (the silencer only affects what gets PRINTED, never
/// what gets caught). Used by BOTH the #202 tiled retry AND (#673) the primary full-frame pass
/// in [`decode_qr_luma_all`] — the old assumption that "the full frame never panics" was
/// live-disproven on a real recording (see that function's doc). Returns each payload with its
/// grid centre ([`rqrr_decode_all_located`]) so the issue-1367 echo gate can place it.
pub(crate) fn rqrr_decode_all_catch(img: GrayImage) -> Vec<LocatedPayload> {
    install_rqrr_assert_silencer();
    // #673 opt-in diagnostic: when QR_DECODE_PANIC_DUMP_DIR is set, save the EXACT
    // panic-triggering frame as a PNG for building a real-pixel regression fixture later — a
    // caught rqrr internal panic is rare (one confirmed incident in ~11000 real frames) and
    // was, before this fix, effectively impossible to capture without crashing the whole run.
    // Zero cost when unset (one cached env lookup, no clone).
    static DUMP_DIR: OnceLock<Option<String>> = OnceLock::new();
    static DUMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let dump_dir = DUMP_DIR.get_or_init(|| std::env::var("QR_DECODE_PANIC_DUMP_DIR").ok());
    let img_for_dump = dump_dir.as_ref().map(|_| img.clone());
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rqrr_decode_all_located(img)
    })) {
        Ok(reads) => reads,
        Err(e) => {
            // Log every caught rqrr internal panic (not just the silenced "scan >= 1" one).
            // Diagnostic only: the caller already treats "nothing decoded" as a normal outcome
            // (an already-gated undecodable frame), so this never changes pass/fail — it just
            // makes a caught crash visible instead of silent (comprehensive-logging.md).
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            if let (Some(dir), Some(dump_img)) = (dump_dir, img_for_dump) {
                let n = DUMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = format!("{dir}/qr-decode-panic-{n}.png");
                match dump_img.save(&path) {
                    Ok(()) => tracing::warn!(
                        panic_message = %msg,
                        path,
                        "rqrr internal panic caught during QR decode — frame dumped for repro (#673)"
                    ),
                    Err(io_err) => tracing::warn!(
                        panic_message = %msg,
                        path,
                        error = %io_err,
                        "rqrr internal panic caught during QR decode — frame dump FAILED (#673)"
                    ),
                }
            } else {
                tracing::warn!(
                    panic_message = %msg,
                    "rqrr internal panic caught during QR decode — treating this frame/tile as \
                     undecodable (#673)"
                );
            }
            Vec::new()
        }
    }
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
/// `pub(crate)` (#718): the colour-gate localizer (`colour_sample::detect_dual_qr`) reuses this
/// SAME hard-Otsu retry — it never had it at all, unlike the continuity decoder's
/// [`decode_qr_luma_all`] below, which has applied it since #363.
pub(crate) fn binarize_otsu(img: &GrayImage) -> GrayImage {
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

/// Decode ALL CRC-valid QR payloads in one grayscale image: the plain rqrr pass UNION the
/// Otsu-binarized pass, de-duped by `(run_id, frame_id)` ([`merge_payloads`]). BOTH passes
/// ALWAYS run.
///
/// #363 — the optical dual-QR read is the HARD verdict gate (restored in #372), so a
/// present-but-SOFT optical QR (a QR filmed off a monitor: low-contrast + moiré +
/// colour-cast) must NEVER be left undecoded as a phantom `optical_undecodable`. The OLD
/// logic ran the Otsu retry ONLY when the plain pass found NOTHING (`if !first.is_empty()
/// { return first }`). On the stream recording the plain pass finds the crisp DIGITAL BURNS
/// (so `first` is non-empty) but MISSES the soft optical dual-QR — so the Otsu retry was
/// SKIPPED and the present optical QR was never recovered, marking ~87 % of stream frames
/// optical-undecodable even though rqrr reads the very same frames once Otsu-binarized.
/// Merging plain ∪ Otsu recovers the optical even when the plain pass already returned the
/// burns; the result is always a SUPERSET of the plain pass (never fewer). rqrr's
/// `detect_grids` returns every QR in one pass, so the two side-by-side dual-QR codes are
/// read together. Cost: one extra cheap full-frame Otsu rqrr pass per frame on the OFFLINE
/// decode (correctness over offline-decode speed); the ~10× tiled passes stay conditional
/// behind the fast/robust gate.
///
/// #673 — BOTH calls go through [`rqrr_decode_all_catch`] (panic-safe), not the bare
/// `rqrr_decode_all` the doc above historically described. rqrr's `Perspective::map`
/// (rqrr-0.9.3 geometry.rs:55) has its OWN internal `assert!(x <= i32::MAX as f64)` —
/// DIFFERENT from the tile-path's already-guarded `assert!(scan >= 1)` (identify/grid.rs) —
/// that PANICS when a detected grid's homography is near-degenerate. The old assumption
/// ("on the full frame this never fires, the big optical QR is always well-formed") was
/// live-disproven 2026-07-11: a real stream recording (RUN_ID 1783735291, restart-survival
/// dispatch #466) crashed a decode worker thread mid-run via exactly this assert, aborting
/// the WHOLE `--extract-partial` run with zero partial output. A panicking full-frame pass is
/// now treated identically to a genuinely-empty decode (already the normal, already-gated
/// "undecodable frame" outcome) — this changes crash-vs-no-crash, never any pass/fail
/// semantics of the zero-loss verdict itself.
pub fn decode_qr_luma_all(img: GrayImage) -> Vec<Payload> {
    let (plain, otsu) = plain_and_otsu_reads(img);
    let mut out = burn_echo::payloads(plain);
    // The hard Otsu cut recovers the soft optical capture the plain adaptive prepare misses,
    // even when the plain pass already decoded the crisp burns (#363).
    merge_payloads(&mut out, burn_echo::payloads(otsu));
    out
}

/// The two passes of [`decode_qr_luma_all`] (plain, then Otsu-binarized), each read with its
/// grid centre in `img` pixels and nothing merged yet.
pub(crate) fn plain_and_otsu_reads(img: GrayImage) -> (Vec<LocatedPayload>, Vec<LocatedPayload>) {
    let otsu = binarize_otsu(&img);
    (rqrr_decode_all_catch(img), rqrr_decode_all_catch(otsu))
}

/// Every read of [`decode_qr_luma_all`]'s two passes, plain then Otsu, with its grid centre in
/// `img` pixels and NO identity merge (issue 1367). The echo gate must judge every read before
/// reads merge: a keep-first merge could otherwise keep an echo of a burn and drop the in-slot
/// read of the same id. Merged keep-first after gating, the payloads equal `decode_qr_luma_all`'s.
pub fn decode_qr_luma_all_reads(img: GrayImage) -> Vec<LocatedPayload> {
    let (mut reads, otsu) = plain_and_otsu_reads(img);
    reads.extend(otsu);
    reads
}

/// #754 — fraction of the frame HEIGHT, measured from the TOP, the optical-recovery crop
/// covers. The LARGE cam2 dual-QR Vernier sits in the TOP band (the #111 layout anchors it
/// `VAnchor::Top`, y ∈ [24, 724] of 1080 ≈ the top 0.67); the small crisp node burns sit in the
/// BOTTOM corners/center (`TILE_BOTTOM_BAND_FRAC`). This crop keeps the WHOLE optical while
/// EXCLUDING every bottom burn — so [`robust_optical_top_band`] only ever ADDS optical payloads,
/// never a burn (a burn cannot appear in a top-only crop), which is what makes the recovery
/// safe to fire pin-independently.
///
/// **Why 0.67, and why a crop at all (the #754 root cause):** rqrr's `prepare()` runs a
/// serpentine adaptive threshold + full-frame capstone/grid detection. The #751 motion-sweep's
/// bright high-contrast bottom band pushes the already-marginal SOFT optical (a QR filmed off a
/// monitor) over rqrr's full-frame decode edge — rqrr LOCATES the optical grid but the
/// perspective/threshold decode returns an error — so the optical present-rate decays 100%→0%
/// over ~4 min of sweep runtime WHILE the crisp bottom burns still decode 100% (cv2 reads the
/// same pixels fine throughout — it is an rqrr full-frame COVERAGE gap, not the pixels, not a
/// validation-layer rejection). Cropping to the top band gives rqrr's threshold a clean
/// histogram over ONLY the optical's region (no sweep pixels), and it decodes again. Measured on
/// the 30 real late-range imag AND strih pixel-proof frames of run 303636614: FULL frame 0/30
/// recovered, this 0.67 top-band crop 30/30 (a plateau across 0.60–0.70; whole-frame downscale
/// is NOT robust at 6–10/30). Mirror of why [`robust_tile_passes`] crops the BOTTOM band to
/// recover the small burns — the optical never had the equivalent TOP-band recovery, because the
/// old "the large optical ALWAYS decodes full-frame" assumption held until #751 broke it.
///
/// `pub(crate)` (#718): `colour_sample::detect_dual_qr` reuses this SAME fraction for its own
/// top-band retry crop — the colour-gate localizer never had ANY top-band recovery at all,
/// unlike this continuity-decode path.
///
/// [`robust_optical_top_band`]: crate::probe::recording_decode::robust_optical_top_band
/// [`robust_tile_passes`]: crate::probe::recording_decode::robust_tile_passes
pub(crate) const OPTICAL_TOP_BAND_FRAC: f32 = 0.67;

/// Merge `add` into `into`, keeping each DISTINCT `(run_id, frame_id)` payload once. The
/// 60→30 beat + multiple tiles surface the SAME burn many times; this de-dups by the full
/// identity so a node's burn is counted once, never inflated, and a recovered burn from a
/// later pass is added if (and only if) it is new. The kept copy is the FIRST seen (the
/// full-frame pass runs before the tiles) — its `gen_ts_ns` is authoritative because a
/// CRC-valid QR for a given `(run_id, frame_id)` always carries the SAME `gen_ts_ns` (the
/// payload is one atomic encoded mark), so the dropped duplicates can never differ in it.
pub(crate) fn merge_payloads(into: &mut Vec<Payload>, add: Vec<Payload>) {
    for p in add {
        if !into
            .iter()
            .any(|q| q.run_id == p.run_id && q.frame_id == p.frame_id)
        {
            into.push(p);
        }
    }
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
    // centered crop would miss the now-top QRs. The recorded-file decode passes
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

// issue 1374: `pub(super)` so `recording_decode`'s tests share the fixture/blit helpers below.
#[cfg(test)]
pub(super) mod tests {
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

    // ============================================================================
    // #363 — the SOFT optical dual-QR (a QR filmed off a monitor: low-contrast +
    // moiré + colour-cast) must be RECOVERED from the real stream-recording frames
    // even though the crisp DIGITAL BURNS already decode on the plain pass. The
    // optical read is the HARD verdict gate (#372). The OLD `decode_qr_luma_all`
    // ran the Otsu retry ONLY when the plain pass found NOTHING — so on these frames
    // the plain pass returned the burns (non-empty) → the Otsu retry was SKIPPED →
    // the present optical QR was marked a PHANTOM `optical_undecodable` (~87 % of
    // stream frames). The fix runs plain ∪ Otsu ALWAYS, recovering it.
    // ============================================================================

    /// Load a committed real recording-frame fixture as the luma plane the decoder
    /// consumes (the exact gray8 the verdict feeds rqrr).
    pub(in crate::probe) fn optical_fixture_luma(name: &str) -> GrayImage {
        let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
            .iter()
            .collect();
        image::open(&path)
            .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
            .to_luma8()
    }

    #[test]
    fn optical_soft_dual_qr_recovered_on_real_stream_frames() {
        // The optical dual-QR's run_id on the real failing recording (run 354003); its two
        // Vernier halves carry an even and an odd frame_id (consecutive frames).
        const OPTICAL_RUN_ID: u32 = 354_003;
        // The f5/f150 cam1/strih/stream digital-burn run_ids — the recording per-frame path.
        const BURN_IDS: &[u32] = &[911_001, 911_002, 911_004];
        for name in ["optical-soft-f5.png", "optical-soft-f150.png"] {
            let luma = optical_fixture_luma(name);

            // RED-condition lock (permanent): the PLAIN rqrr pass MISSES the optical dual-QR.
            // It finds the crisp burns but not the soft optical — exactly what the old
            // early-return ("Otsu only if plain empty") swallowed (plain non-empty ⇒ Otsu
            // skipped ⇒ optical never recovered). If a future decoder reads the optical on the
            // plain pass, this documents the shift — re-tune, never delete.
            let plain = rqrr_decode_all(luma.clone());
            assert!(
                !plain.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
                "{name}: the PLAIN rqrr pass is expected to MISS the soft optical dual-QR \
                 (run_id {OPTICAL_RUN_ID}); got {:?}",
                plain
                    .iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            );

            // GREEN: decode_qr_luma_all (plain ∪ Otsu) recovers BOTH optical halves — two
            // distinct frame_ids (the even+odd Vernier pair) under run_id 354003. Reverting
            // to the early-return makes this return only burns → 0 optical → this fails.
            let all = decode_qr_luma_all(luma.clone());
            let mut optical: Vec<u32> = all
                .iter()
                .filter(|p| p.run_id == OPTICAL_RUN_ID)
                .map(|p| p.frame_id)
                .collect();
            optical.sort_unstable();
            optical.dedup();
            assert!(
                optical.len() >= 2,
                "{name}: decode_qr_luma_all must recover BOTH optical dual-QR halves \
                 (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got optical frame_ids \
                 {optical:?} from {:?}",
                all.iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            );

            // AND the recording per-frame path (fast-then-robust, gated on the burn ids) also
            // surfaces the optical — the verdict's actual decode call reads it too.
            let rec = decode_qr_luma_all_fast_then_robust(luma, BURN_IDS);
            assert!(
                rec.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
                "{name}: the recording per-frame decode must also surface the optical dual-QR \
                 (run_id {OPTICAL_RUN_ID}); got {:?}",
                rec.iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            );
        }
    }

    // ============================================================================
    // #202 — robust offline decode recovers the small node burns rqrr's single
    // full-frame `detect_grids` pass intermittently misses (the residual
    // burn-unreadable misses on PRESENT, sharp frames; cv2 reads them, rqrr's
    // full-frame pass doesn't). The tiled/upscaled robust pass gives rqrr a fair
    // look at each region so the burn's finder pattern locks.
    // ============================================================================

    /// Composite the QR for `p` (rendered at `qr_px`) onto a white luma frame at top-left
    /// `(ox, oy)`. Models a small burn drawn into a large recorded frame.
    pub(in crate::probe) fn blit_burn_luma(
        frame: &mut GrayImage,
        p: &Payload,
        qr_px: u32,
        ox: u32,
        oy: u32,
    ) {
        let qr = render_payload_qr(p, qr_px);
        let (qw, qh) = (qr.width(), qr.height());
        for y in 0..qh {
            for x in 0..qw {
                let (px, py) = (ox + x, oy + y);
                if px < frame.width() && py < frame.height() {
                    frame.put_pixel(px, py, qr.get_pixel(x, y).to_owned());
                }
            }
        }
    }

    #[test]
    fn full_frame_otsu_union_recovers_a_softened_burn_bare_rqrr_misses() {
        // A 1920×1080 frame carrying the LARGE optical dual-QR (top) plus a 260px node burn
        // (bottom-left corner) SOFTENED by a Gaussian blur (sigma 2.8) — anti-aliased as the
        // NDI/encode chain leaves it. The BARE single rqrr pass (rqrr's own adaptive prepare)
        // MISSES this softened burn; #363's full-frame Otsu union (`decode_qr_luma_all` = plain
        // ∪ Otsu) RECOVERS it — a hard black/white cut at the Otsu split locks rqrr's finder
        // where the soft adaptive prepare failed. This locks the #363 improvement at the unit
        // level: the union does real recovery work beyond the bare plain pass.
        //
        // NOTE on the #202 tiled recovery: #363 strengthened the full-frame pass enough that
        // this *synthetic* perfect-QR-plus-blur burn no longer needs the tiles (the Otsu union
        // reads it full-frame). The genuine "the full-frame pass misses a burn that only the
        // tiled+upscaled robust pass recovers" gap is a size-disparity / detector-coverage
        // effect that only reproduces on real encoder-degraded 4K frames — it is locked on
        // REAL pixels by `fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame`
        // (below) and by tests/burn_fixture_decode.rs.
        let left = Payload {
            run_id: 136_141_133,
            frame_id: 4000,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 136_141_133,
            frame_id: 4001,
            gen_ts_ns: 2,
        };
        let burn = Payload {
            run_id: 911_001, // cam1 capture burn
            frame_id: 1234,
            gen_ts_ns: 3,
        };
        let (w, h) = (1920u32, 1080u32);
        // Big top dual-QR (700px each half, top-anchored) — the easy-to-find marks.
        let bgra = render_qr_dual_bgra(&left, &right, w, h, 700);
        let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
        // A 260px burn in the bottom-left corner (where the strih burn lives), then soften
        // the WHOLE frame as the recording chain does.
        let qh = render_payload_qr(&burn, 260).height();
        blit_burn_luma(&mut luma, &burn, 260, 40, h - qh - 40);
        let luma = image::imageops::blur(&luma, 2.8);

        let bare = rqrr_decode_all(luma.clone());
        let full = decode_qr_luma_all(luma.clone());
        let robust = decode_qr_luma_all_robust(luma);

        // The big optical QRs decode on the bare pass (sanity: the frame is well-formed).
        assert!(
            bare.iter().any(|p| p.run_id == left.run_id),
            "the big optical QR must decode on the bare rqrr pass (frame is well-formed): {bare:?}"
        );
        // RED condition this locks: the BARE single rqrr pass MISSES the softened burn — exactly
        // what the old early-return swallowed (it returned the bare pass when non-empty).
        assert!(
            !bare.contains(&burn),
            "the bare single rqrr pass is expected to MISS the softened burn (the soft-capture \
             condition #363 recovers); bare={bare:?}"
        );
        // GREEN: the #363 full-frame Otsu union recovers it — and robust (⊇ full) keeps it.
        assert!(
            full.contains(&burn),
            "#363: the full-frame Otsu union must recover the softened cam1 burn the bare pass \
             missed; full={full:?}"
        );
        assert!(
            robust.contains(&burn),
            "robust must keep the recovered burn (superset of the full-frame pass): robust={robust:?}"
        );
    }

    #[test]
    fn merge_payloads_keeps_each_distinct_identity_once() {
        // Pure de-dup contract: same (run_id, frame_id) collapses; a different frame_id (or
        // run_id) is a distinct mark and is kept.
        let a = Payload {
            run_id: 1,
            frame_id: 10,
            gen_ts_ns: 1,
        };
        let a_dup = Payload {
            run_id: 1,
            frame_id: 10,
            gen_ts_ns: 999, // same identity, different ts — still a duplicate
        };
        let b = Payload {
            run_id: 1,
            frame_id: 11,
            gen_ts_ns: 1,
        };
        let c = Payload {
            run_id: 2,
            frame_id: 10,
            gen_ts_ns: 1,
        };
        let mut into = vec![a];
        merge_payloads(&mut into, vec![a_dup, b, c]);
        assert_eq!(
            into.len(),
            3,
            "a_dup collapses onto a; b and c are new: {into:?}"
        );
        assert!(into.contains(&a) && into.contains(&b) && into.contains(&c));
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

    // ---- #367 colour-reference scale blit ----

    #[test]
    fn colour_scale_blit_fills_each_patch_and_leaves_the_qr_halves_untouched() {
        use crate::colour_scale::{colour_scale_patches, DEFAULT_QR_SIZE, TOP_MARGIN_PX};
        let (w, h) = (1920u32, 1080u32);
        let mut canvas = vec![255u8; (w * h * 4) as usize];
        blit_colour_scale_bgra(&mut canvas, w, h, DEFAULT_QR_SIZE, TOP_MARGIN_PX);

        // Each patch CENTRE carries exactly its known colour in BGRA order, opaque — proving
        // the blit honours the pure layout's colour table (not a tautology: it reads the real
        // framebuffer bytes the painter would present).
        let patches = colour_scale_patches(w, h, DEFAULT_QR_SIZE, TOP_MARGIN_PX);
        assert!(!patches.is_empty(), "1920x1080 must yield patches");
        for (rect, rgb) in &patches {
            let cx = rect.x + rect.w / 2;
            let cy = rect.y + rect.h / 2;
            let i = (((cy * w) + cx) * 4) as usize;
            assert_eq!(canvas[i], rgb.b, "B at patch centre ({cx},{cy})");
            assert_eq!(canvas[i + 1], rgb.g, "G at patch centre ({cx},{cy})");
            assert_eq!(canvas[i + 2], rgb.r, "R at patch centre ({cx},{cy})");
            assert_eq!(canvas[i + 3], 255, "opaque at patch centre ({cx},{cy})");
        }
        // The colour column lives ONLY in the central gap: a pixel inside the LEFT QR half
        // (x=200, y=400) is still the untouched white background — the scale never bleeds into
        // either QR region.
        let in_left_qr = (((400 * w) + 200) * 4) as usize;
        assert_eq!(
            &canvas[in_left_qr..in_left_qr + 4],
            &[255u8, 255, 255, 255],
            "the dual-QR halves are untouched by the colour scale"
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
        // The four-mark non-overlap contract on a 1920×1080 stream frame (#186 enlarged):
        //   top dual-QR band : y ∈ [TOP_MARGIN_PX, TOP_MARGIN_PX + 700)
        //   strih burn       : bottom-LEFT  ~302×302 corner (DistroAV qr_px 0.28×1080, enlarged)
        //   stream burn      : bottom-RIGHT ~302×302 corner
        //   cam1 burn        : bottom-CENTER ~320×320, must miss all three.
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

        // Vs the bottom-corner burns (#186: ~302px squares = 0.28×1080, anchored bottom-left /
        // bottom-right with a 40px edge margin, so the corner box occupies x ∈ [0, margin+302)
        // on the left and [w-margin-302, w) on the right).
        let corner = 40u32 + 302u32; // DistroAV burn margin (40) + canvas-relative qr_px (302)
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
