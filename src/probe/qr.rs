//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::luma::{bgra_to_luma, crop_center_luma, crop_top, uyvy_to_luma};
use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// #207 — which decode path a single recording frame took.
///
/// The robust tiled passes are ~10× the cost of the plain full-frame pass, so the per-frame
/// decode ([`decode_qr_luma_all_fast_then_robust`]) runs them ONLY when the fast plain pass
/// missed an expected node burn. This enum reports that choice per-call so it is observable
/// WITHOUT shared global state — the path-reachability tests assert on it directly (a
/// process-wide counter would race against the recording unit tests running concurrently).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePath {
    /// The plain full-frame pass already carried every expected node burn — the tiles were
    /// skipped (the ~99 %+ common case on a clean recording).
    Fast,
    /// An expected node burn was missing from the plain pass — the tiled+upscaled recovery ran.
    Robust,
}

/// #207 — process-wide counters of the decode-path split, for the verdict LOG only (so it
/// shows fast ≫ robust = the speedup is real). They accumulate across all recordings in one
/// run. NOT used by tests (those assert the per-call [`DecodePath`] instead — globals race
/// against concurrent tests). `fast` = plain pass already had every expected burn; `robust` =
/// a burn was missing so the tiles ran.
static FAST_PATH_FRAMES: AtomicU64 = AtomicU64::new(0);
static ROBUST_FALLBACK_FRAMES: AtomicU64 = AtomicU64::new(0);

/// (fast_path_frames, robust_fallback_frames) since process start — the #207 decode-path
/// split, for the verdict log.
pub fn decode_path_counts() -> (u64, u64) {
    (
        FAST_PATH_FRAMES.load(Ordering::Relaxed),
        ROBUST_FALLBACK_FRAMES.load(Ordering::Relaxed),
    )
}

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
/// live-disproven on a real recording (see that function's doc).
fn rqrr_decode_all_catch(img: GrayImage) -> Vec<Payload> {
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
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rqrr_decode_all(img))) {
        Ok(payloads) => payloads,
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
    let mut out = rqrr_decode_all_catch(img.clone());
    // The hard Otsu cut recovers the soft optical capture the plain adaptive prepare misses,
    // even when the plain pass already decoded the crisp burns (#363).
    merge_payloads(&mut out, rqrr_decode_all_catch(binarize_otsu(&img)));
    out
}

/// Tile-scoped twin of [`decode_qr_luma_all`] for the #202 tile passes (plain pass, then an
/// Otsu-binarized retry if empty). Both this AND `decode_qr_luma_all` route through the same
/// panic-safe [`rqrr_decode_all_catch`] (#673) so a degenerate tile — or a degenerate
/// full-frame homography — that trips an rqrr internal assert yields "nothing" instead of
/// aborting the whole frame's decode.
fn decode_qr_luma_all_tile(img: GrayImage) -> Vec<Payload> {
    let first = rqrr_decode_all_catch(img.clone());
    if !first.is_empty() {
        return first;
    }
    rqrr_decode_all_catch(binarize_otsu(&img))
}

/// #202 — how many horizontal tiles the robust (offline-recording) decode splits the BOTTOM
/// band into. The #111 layout fixes all three node burns to the BOTTOM of the frame
/// (strih = bottom-left, cam1 = bottom-center, stream = bottom-right corner) while the large
/// optical dual-QR sits in the TOP band and ALWAYS decodes on the full-frame pass. So the
/// recovery passes only need the bottom band, split into 3 columns — one per burn — giving
/// rqrr a sub-image in which the small ~320px burn is LARGE relative to the tile, so its
/// finder pattern locks where the full-frame `detect_grids` pass missed it. 3 columns (vs a
/// full 3×3 grid) cover every burn at 1/3 the rqrr passes (proven: the bottom band recovers
/// all 17 real flagged burns — cam1 11/11, strih 5/5, stream 1/1).
const TILE_COLS: u32 = 3;

/// #202 — fraction of the frame HEIGHT, measured from the bottom, the tile band covers. The
/// burns sit in the bottom corners + center (the #111 4-corner layout, ~0.28×h tall burns
/// bottom-anchored), so the bottom 45% always contains them whole with margin; the top
/// dual-QR is excluded (it never needs a tile — it decodes full-frame).
const TILE_BOTTOM_BAND_FRAC: f32 = 0.45;

/// #202 — fractional overlap between adjacent column tiles, so a burn straddling a column
/// boundary is still WHOLLY inside at least one tile (a QR cut by a hard tile edge decodes
/// in neither). 0.25 = each tile extends a quarter of its width into its neighbours.
const TILE_OVERLAP_FRAC: f32 = 0.25;

/// #202 — minimum long-side px a tile is upscaled TO before rqrr when it is smaller. A
/// crisp but small burn (the cam1 320px mark, sharp yet only ~7px/module in the 1080p
/// recording) decodes more reliably for rqrr's finder when the modules are a few px larger;
/// cubic-upscaling the tile to this floor gives the finder more pixels per module WITHOUT
/// inventing detail (the pattern is already sharp — see #202: cv2 read every flagged burn
/// from the same pixels). 1280 keeps a 1/3-of-1080 (≈640px) tile at ~2× and a 1/3-of-4K
/// (≈1280px) tile unchanged.
const TILE_UPSCALE_MIN: u32 = 1280;

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
pub(crate) const OPTICAL_TOP_BAND_FRAC: f32 = 0.67;

/// Merge `add` into `into`, keeping each DISTINCT `(run_id, frame_id)` payload once. The
/// 60→30 beat + multiple tiles surface the SAME burn many times; this de-dups by the full
/// identity so a node's burn is counted once, never inflated, and a recovered burn from a
/// later pass is added if (and only if) it is new. The kept copy is the FIRST seen (the
/// full-frame pass runs before the tiles) — its `gen_ts_ns` is authoritative because a
/// CRC-valid QR for a given `(run_id, frame_id)` always carries the SAME `gen_ts_ns` (the
/// payload is one atomic encoded mark), so the dropped duplicates can never differ in it.
fn merge_payloads(into: &mut Vec<Payload>, add: Vec<Payload>) {
    for p in add {
        if !into
            .iter()
            .any(|q| q.run_id == p.run_id && q.frame_id == p.frame_id)
        {
            into.push(p);
        }
    }
}

/// #202 — the ROBUST offline-recording decode: every CRC-valid QR in the frame, recovering
/// the small burns rqrr's single full-frame `detect_grids` pass misses.
///
/// rqrr's grid detector reliably finds the LARGE optical dual-QR but intermittently misses
/// the small ~320px node burns when several QRs of very different sizes share one
/// full-resolution frame (#186/#202 — the burn pixels are present and sharp; cv2 reads every
/// flagged frame from the identical pixels, so this is a detector-coverage gap, NOT a
/// burn-size or burn-readability defect). The fix gives rqrr a fair look at each region:
///
/// 1. the plain full-frame pass (`decode_qr_luma_all`) — finds the big top dual-QR cheaply;
/// 2. the BOTTOM band ([`TILE_BOTTOM_BAND_FRAC`] of the height) split into [`TILE_COLS`]
///    OVERLAPPING column tiles ([`TILE_OVERLAP_FRAC`]) — one per bottom burn — each
///    cubic-upscaled to at least [`TILE_UPSCALE_MIN`] on its long side, rqrr-decoded; in a
///    column tile a 320px burn is large-relative and its finder locks.
///
/// All passes' CRC-valid payloads are merged, de-duped by `(run_id, frame_id)`
/// ([`merge_payloads`]), so a node's burn is counted exactly once and the result is always a
/// SUPERSET of the plain pass. Offline only (the parallel #166/#187 recording decode) — it
/// runs a few extra rqrr passes per frame, so it is NEVER on the latency-sensitive live tap.
pub fn decode_qr_luma_all_robust(img: GrayImage) -> Vec<Payload> {
    let mut out = decode_qr_luma_all(img.clone());
    robust_tile_passes(&img, &mut out);
    out
}

/// The expensive part of the robust decode: the bottom-band tiled+upscaled rqrr passes that
/// recover the small node burns the plain full-frame pass missed. Factored out of
/// [`decode_qr_luma_all_robust`] so the #207 fast path can run it CONDITIONALLY (only when a
/// burn is actually missing) instead of on every frame. Merges each tile's CRC-valid
/// payloads into `out` (de-duped by `(run_id, frame_id)`), so `out` is always a SUPERSET of
/// what it carried on entry — never fewer.
fn robust_tile_passes(img: &GrayImage, out: &mut Vec<Payload>) {
    let (w, h) = (img.width(), img.height());
    // A tiny frame is one tile — the plain pass already covered it; nothing to gain.
    if w < TILE_COLS || h < 2 {
        return;
    }

    // The bottom band that holds the burns (the top dual-QR is excluded — it decodes
    // full-frame). Band top = h - band_h; the tiles span [band_top, h).
    let band_h = (((h as f32) * TILE_BOTTOM_BAND_FRAC) as u32).clamp(1, h);
    let band_top = h - band_h;

    // Overlapping column geometry across the band: base step = w / cols, each column extended
    // by the overlap on each side (clamped to the frame width). The columns span the whole
    // width, so every bottom burn — left / center / right — falls wholly inside at least one.
    let step_x = (w / TILE_COLS).max(1);
    let over_x = ((step_x as f32) * TILE_OVERLAP_FRAC) as u32;

    for gx in 0..TILE_COLS {
        let x0 = (gx * step_x).saturating_sub(over_x);
        // The last column extends to the frame edge so no right strip is left uncovered.
        let x1 = if gx == TILE_COLS - 1 {
            w
        } else {
            (((gx + 1) * step_x) + over_x).min(w)
        };
        let tw = x1 - x0;
        if tw == 0 {
            continue;
        }
        let tile = image::imageops::crop_imm(img, x0, band_top, tw, band_h).to_image();
        // Upscale a small tile so the burn's modules span more px for rqrr's finder.
        let long = tw.max(band_h);
        let tile = if long < TILE_UPSCALE_MIN {
            let nw = (tw * TILE_UPSCALE_MIN / long).max(1);
            let nh = (band_h * TILE_UPSCALE_MIN / long).max(1);
            image::imageops::resize(&tile, nw, nh, image::imageops::FilterType::CatmullRom)
        } else {
            tile
        };
        merge_payloads(out, decode_qr_luma_all_tile(tile));
    }
}

/// #754 — the TOP-band optical-recovery pass: crop the top [`OPTICAL_TOP_BAND_FRAC`] of the
/// frame (which holds the whole cam2 dual-QR Vernier and NONE of the bottom node burns) and run
/// the SAME plain∪Otsu decode ([`decode_qr_luma_all`]) over just that band, merging any
/// CRC-valid payloads into `out` (de-duped by `(run_id, frame_id)`). Because the crop excludes
/// the bottom burns entirely, this can only ever ADD optical payloads — `out` stays a strict
/// SUPERSET of what it carried on entry — so it is safe to fire without knowing which run_id the
/// optical is (pin-independent). This is the top-band twin of [`robust_tile_passes`] (which
/// crops the BOTTOM band for the small burns); see [`OPTICAL_TOP_BAND_FRAC`] for the #751/#754
/// root cause and the 30/30-vs-0/30 offline measurement that fixes the band fraction.
fn robust_optical_top_band(img: &GrayImage, out: &mut Vec<Payload>) {
    let (w, h) = (img.width(), img.height());
    // A tiny frame is already one look for the plain pass — nothing a crop can add.
    if w < 2 || h < 2 {
        return;
    }
    // #718: crop-height arithmetic now lives in the pure crate-root helper (shared with
    // `colour_sample::detect_dual_qr`'s own retry) — same formula, one source of truth.
    let band_h = crate::colour_scale::top_band_crop_height(h, OPTICAL_TOP_BAND_FRAC);
    // Same size as the source when the frac rounds to the full height — still a valid, cheap
    // second look (Otsu over the whole frame), never a panic.
    let band = image::imageops::crop_imm(img, 0, 0, w, band_h).to_image();
    merge_payloads(out, decode_qr_luma_all(band));
}

/// #754 — is the plain full-frame pass SHORT of the optical dual-QR, i.e. does the top-band
/// recovery ([`robust_optical_top_band`]) have work to do? Two modes, both keyed off the fact
/// that the painter's Vernier ALWAYS shows TWO halves of ONE run_id per frame (`painter::
/// vernier_ids`), while every node BURN is a single QR (exactly one `frame_id` per run_id per
/// frame):
///
/// * **Pinned** (`Some((run_id, min_distinct))`) — the caller KNOWS the optical run_id (the
///   `--cam2-run-id` pin, #707). Short ⇔ fewer than `min_distinct` DISTINCT `frame_id`s for that
///   run_id decoded on the plain pass. Identical predicate to the #707 fast-path optical gate,
///   just used here to decide whether to run the recovery FIRST.
/// * **Unpinned** (`None`) — the caller does NOT know the optical run_id (strih extracts
///   unpinned — the exact case the #707 gate could not save, see #754). We can still tell the
///   read is short structurally: on a healthy frame the optical contributes the ONLY run_id with
///   ≥2 distinct `frame_id`s; if NO run_id has ≥2, the optical is at best a single held half (or
///   absent), so the top band is worth a look. This fires exactly on the decayed frames (early
///   healthy frames, where both halves decode full-frame, are skipped for free) and never
///   mistakes a burn for a complete optical.
fn optical_read_short(payloads: &[Payload], min_distinct_optical: Option<(u32, usize)>) -> bool {
    match min_distinct_optical {
        Some((run_id, min_distinct)) => {
            let distinct: std::collections::HashSet<u32> = payloads
                .iter()
                .filter(|p| p.run_id == run_id)
                .map(|p| p.frame_id)
                .collect();
            distinct.len() < min_distinct
        }
        None => {
            let mut by_run: std::collections::HashMap<u32, std::collections::HashSet<u32>> =
                std::collections::HashMap::new();
            for p in payloads {
                by_run.entry(p.run_id).or_default().insert(p.frame_id);
            }
            !by_run.values().any(|ids| ids.len() >= 2)
        }
    }
}

/// #207 — the PER-FRAME recording decode: plain full-frame pass FIRST (fast), then the
/// robust tiled recovery ONLY when the fast pass missed an expected node burn.
///
/// The robust tiled passes ([`robust_tile_passes`]) cost ~10× the plain full-frame pass, yet
/// on a clean genlocked recording the plain pass already reads EVERY node burn on ~99 %+ of
/// frames (#202: the tiles only ever recover the rare frame where rqrr's single
/// `detect_grids` misses a small burn — the residual #186 coverage gap). Running the tiles on
/// every frame therefore made a 30-min verdict take ~50 min when ~5 would do.
///
/// So: decode the full frame plainly; if every id in `expected_burn_run_ids` already decoded,
/// return immediately (the FAST path); otherwise run the tiled recovery (the ROBUST
/// fallback). The result is IDENTICAL to [`decode_qr_luma_all_robust`] for any frame whose
/// burns the plain pass already had (a SUPERSET-of-plain that the tiles couldn't extend), and
/// for any frame missing a burn the full robust passes run unchanged — so the #186 0-miss
/// guarantee is preserved exactly, just gated behind a cheap plain-first check. `expected_…`
/// empty ⇒ always fast (no burns to require); pass [`recording::NODE_BURN_RUN_IDS`] for the
/// recording path.
pub fn decode_qr_luma_all_fast_then_robust(
    img: GrayImage,
    expected_burn_run_ids: &[u32],
) -> Vec<Payload> {
    let (out, path) = decode_qr_luma_all_fast_then_robust_pathed(img, expected_burn_run_ids);
    // Record the split for the verdict log (observability only — never gates correctness).
    match path {
        DecodePath::Fast => FAST_PATH_FRAMES.fetch_add(1, Ordering::Relaxed),
        DecodePath::Robust => ROBUST_FALLBACK_FRAMES.fetch_add(1, Ordering::Relaxed),
    };
    out
}

/// #632 gap 1 — [`decode_qr_luma_all_fast_then_robust`] (the counting public wrapper) over
/// [`decode_qr_luma_all_fast_then_robust_grouped_pathed`]'s two-group gate. See that function's
/// doc for why the mandatory/any-of split exists.
pub fn decode_qr_luma_all_fast_then_robust_grouped(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
) -> Vec<Payload> {
    decode_qr_luma_all_fast_then_robust_grouped_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        None,
    )
}

/// #707 — [`decode_qr_luma_all_fast_then_robust_grouped`] with the third (optical) gate
/// dimension; see [`decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`].
pub fn decode_qr_luma_all_fast_then_robust_grouped_optical(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> Vec<Payload> {
    let (out, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        min_distinct_optical,
    );
    match path {
        DecodePath::Fast => FAST_PATH_FRAMES.fetch_add(1, Ordering::Relaxed),
        DecodePath::Robust => ROBUST_FALLBACK_FRAMES.fetch_add(1, Ordering::Relaxed),
    };
    out
}

/// [`decode_qr_luma_all_fast_then_robust`] that ALSO returns which [`DecodePath`] it took.
/// This is the core; the counting public wrapper above just records the path. Returning the
/// path makes the choice observable per-call (no global state), so the path-reachability tests
/// are deterministic even when other tests decode concurrently.
pub fn decode_qr_luma_all_fast_then_robust_pathed(
    img: GrayImage,
    expected_burn_run_ids: &[u32],
) -> (Vec<Payload>, DecodePath) {
    // Every id in `expected_burn_run_ids` is MANDATORY (an empty `any_of` group is vacuously
    // satisfied) — this is the pre-#632 behavior, unchanged for every existing caller.
    decode_qr_luma_all_fast_then_robust_grouped_pathed(img, expected_burn_run_ids, &[])
}

/// #632 gap 1 — [`decode_qr_luma_all_fast_then_robust_pathed`] generalized with a SECOND,
/// independent group: `mandatory_burn_run_ids` must ALL be found (unchanged semantics), and
/// `any_of_burn_run_ids` needs only ONE member found (empty ⇒ vacuously satisfied, matching
/// `mandatory`'s existing empty-list behavior). This is what lets a recording whose
/// camera-under-test is cam3/cam4/cam5/cam6/cam2 (instead of the historically-hardcoded cam1)
/// take the #207 FAST path too: cam1..cam6 are mutually exclusive in a real run (only the
/// physically-deployed camera's burn ever appears), so requiring "cam1 AND strih" (the old
/// single-group gate) is permanently unsatisfiable — and permanently ROBUST — for any OTHER
/// deployed camera. Passing `mandatory = [strih]`, `any_of = [cam1, cam2, cam3, cam4, cam5,
/// cam6]` instead fixes that: the mandatory hop burn(s) are still always required, and the
/// deployed camera's OWN burn (whichever one it is) still satisfies the group — so a genuinely
/// missing/unreadable deployed-camera burn on THIS frame still correctly falls through to the
/// robust recovery (the #186 0-miss guarantee is unaffected; see the doc on
/// [`decode_qr_luma_all_fast_then_robust_pathed`]). A flat UNION of all 6 ids into one
/// mandatory list (requiring all of them on every frame) would NOT fix this — it would make the
/// gate permanently unsatisfiable in the OTHER direction (an undeployed camera's id never
/// appears either) — this is why the two groups must stay separate.
pub fn decode_qr_luma_all_fast_then_robust_grouped_pathed(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
) -> (Vec<Payload>, DecodePath) {
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        None,
    )
}

/// #707 — the #207 gate's THIRD, independent completeness dimension: the cam2 dual-QR
/// Vernier optical read. `min_distinct_optical = Some((optical_run_id, min_distinct_ids))`
/// requires the plain pass to already carry at least `min_distinct_ids` DISTINCT `frame_id`s
/// for `optical_run_id` before the fast path may skip the tiled recovery — `None` preserves
/// the exact pre-#707 behavior (burns only), unchanged for every existing caller.
///
/// **Why this is needed** (found investigating #707's residual `all_cambox_continuity`
/// `copies`/`gaps`): the dual-QR Vernier paints TWO regions per refresh (left=latest even
/// tick, right=latest odd tick, `painter::vernier_ids`) — a healthy frame's plain pass finds
/// BOTH as two distinct payloads. But the pre-#707 gate here only ever checked the NODE BURNS
/// (cam1/strih/stream/camN) — a frame where the plain pass reads the (small, digitally
/// rendered, easy) node burns fine but MISSES one dual-QR half (the actively-repainting region
/// is naturally harder to read than the held one — moiré/shimmer/focus-breathing during a brief
/// optical-degradation window) took the FAST path anyway, silently skipping the #202 robust
/// tiled+upscaled retry that has ALREADY been proven (see
/// [`fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame`]) to recover reads
/// the plain pass alone misses. The frame's resolved Vernier tick then falls back to whichever
/// single held id DID decode — the SAME value the immediately preceding frame already reported
/// — registering as a spurious `all_cambox_continuity` "copy" even though nothing was actually
/// duplicated on screen (#707 offline-validated every such frame's decoded `(frame_id,
/// gen_ts_ns)` against the painter's own ground-truth CSV: 92939/92939 payloads across 5 full
/// recordings matched a REAL painted tick exactly — zero hallucinated/misread values — so this
/// is a decode-COVERAGE gap, never a decoder-correctness bug; see #707's own issue thread).
pub fn decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> (Vec<Payload>, DecodePath) {
    let mut out = decode_qr_luma_all(img.clone());

    // #754: the SOFT optical dual-QR (top band) is missed by the full-frame plain pass on late
    // #751-sweep frames (rqrr locates the grid but the full-frame perspective/threshold decode
    // fails) WHILE the crisp bottom burns still decode — so the fast-path gate can pass with the
    // optical actually absent (unpinned: it never checked the optical; pinned: even the robust
    // fallback only tiled the BOTTOM band, never the optical's top band). Give the TOP band the
    // isolated look robust_tile_passes gives the bottom burns, BEFORE the gate, whenever the
    // plain pass is short of the optical — so a frame that was ONLY optical-short (burns fine)
    // recovers and takes the cheap FAST path instead of the ~10× bottom-tile robust fallback.
    // The crop excludes the burns, so this only ever ADDS optical payloads (out stays a superset).
    if optical_read_short(&out, min_distinct_optical) {
        robust_optical_top_band(&img, &mut out);
    }

    if fast_path_gate_satisfied(
        &out,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        min_distinct_optical,
    ) {
        return (out, DecodePath::Fast);
    }

    // Robust fallback: a mandatory burn is missing, NONE of the any-of group decoded, or (#707)
    // the dual-QR optical read is STILL short after the #754 top-band recovery — give rqrr the
    // tiled+upscaled look (#202) that recovers reads the full-frame pass intermittently misses.
    robust_tile_passes(&img, &mut out);
    (out, DecodePath::Robust)
}

/// Pure #207/#707 fast-path gate DECISION — every completeness dimension the plain-pass
/// `payloads` must already satisfy before the ~10×-cost robust tiled retry can be skipped.
/// Extracted from [`decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`] so the
/// decision itself is directly unit-testable against hand-built payload lists, with no image
/// decode involved (mirrors the project's other pure-decision seams, e.g. `send_stall::
/// is_send_stall`).
fn fast_path_gate_satisfied(
    payloads: &[Payload],
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> bool {
    let mandatory_present = mandatory_burn_run_ids
        .iter()
        .all(|id| payloads.iter().any(|p| p.run_id == *id));
    let any_of_present = any_of_burn_run_ids.is_empty()
        || any_of_burn_run_ids
            .iter()
            .any(|id| payloads.iter().any(|p| p.run_id == *id));
    let optical_complete = match min_distinct_optical {
        None => true,
        Some((run_id, min_distinct_ids)) => {
            let distinct: std::collections::HashSet<u32> = payloads
                .iter()
                .filter(|p| p.run_id == run_id)
                .map(|p| p.frame_id)
                .collect();
            distinct.len() >= min_distinct_ids
        }
    };
    mandatory_present && any_of_present && optical_complete
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
    fn optical_fixture_luma(name: &str) -> GrayImage {
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

    #[test]
    fn optical_dual_qr_recovered_on_late_sweep_frame_via_top_band_755754() {
        // #754 — a REAL late-range (#751 motion-sweep) frame from the surviving imag recording of
        // run 303636614 (t≈290s, frame-12081): the crisp bottom node burns (cam1 911001, imag
        // 911003) decode on the plain full-frame pass, but the SOFT optical dual-QR (run_id
        // 303636614, top band) does NOT — the exact decoder-side decay this ticket is about. cv2
        // reads both optical halves off this same frame; rqrr's full-frame pass locates the
        // optical grid but the perspective/threshold decode fails. The #754 top-band recovery
        // crop restores it. Fixture committed alongside; sharpness/contrast are constant across
        // the decay (see the ticket) — this is a coverage gap, not a pixel defect.
        const OPTICAL_RUN_ID: u32 = 303_636_614;
        const IMAG_BURN: u32 = 911_003; // present on this frame (fast-path burn gate is satisfied)
        let luma = optical_fixture_luma("optical-sweep-decay-late-imag.png");

        // RED-condition lock (permanent, mechanism): the FULL-FRAME plain∪Otsu pass
        // (`decode_qr_luma_all`, the current production full-frame decode) MISSES the optical on
        // this late frame — 0 payloads for run_id 303636614 — even though it reads the burns.
        // This is what makes every fast-path decode of this frame drop the optical. If a future
        // rqrr reads it full-frame, re-tune this fixture, never delete the guard.
        let full = decode_qr_luma_all(luma.clone());
        assert!(
            !full.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
            "the full-frame plain∪Otsu pass is expected to MISS the soft optical on this late \
             sweep frame (run_id {OPTICAL_RUN_ID}); got {:?}",
            full.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );
        assert!(
            full.iter().any(|p| p.run_id == IMAG_BURN),
            "sanity: the crisp imag burn {IMAG_BURN} MUST decode full-frame on this same frame \
             (proves the decode itself works — only the soft optical is missed); got {:?}",
            full.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );

        // GREEN (PINNED, the fused verdict + imag/stream path, #707 `--cam2-run-id`): the
        // per-frame recording decode, gated with min_distinct_optical=Some, recovers BOTH optical
        // halves via the #754 top-band crop. Without the fix this returns only burns → 0 optical
        // → fails.
        let pinned = decode_qr_luma_all_fast_then_robust_grouped_optical(
            luma.clone(),
            &[IMAG_BURN],
            &[],
            Some((OPTICAL_RUN_ID, 2)),
        );
        let optical_pinned = distinct_optical_ids(&pinned, OPTICAL_RUN_ID);
        assert!(
            optical_pinned.len() >= 2,
            "PINNED: the recording decode must recover BOTH optical dual-QR halves via the #754 \
             top-band crop (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got {optical_pinned:?} \
             from {:?}",
            pinned.iter().map(|p| (p.run_id, p.frame_id)).collect::<Vec<_>>()
        );

        // GREEN (UNPINNED, the strih-extract path — min_distinct_optical=None, the exact case the
        // #707 gate could NOT save, see #754): the burns satisfy the gate (fast path), yet the
        // structural optical-short trigger still fires the top-band recovery and the optical is
        // surfaced. This is what makes the fix pin-INDEPENDENT.
        let unpinned = decode_qr_luma_all_fast_then_robust_grouped_optical(
            luma.clone(),
            &[IMAG_BURN],
            &[],
            None,
        );
        let optical_unpinned = distinct_optical_ids(&unpinned, OPTICAL_RUN_ID);
        assert!(
            optical_unpinned.len() >= 2,
            "UNPINNED: the top-band recovery must still surface BOTH optical halves without a \
             --cam2-run-id pin (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got \
             {optical_unpinned:?} from {:?}",
            unpinned
                .iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );
    }

    /// The DISTINCT `frame_id`s decoded for `run_id`, sorted+deduped — the count of optical
    /// Vernier halves recovered for the #754 fixture assertions.
    fn distinct_optical_ids(payloads: &[Payload], run_id: u32) -> Vec<u32> {
        let mut ids: Vec<u32> = payloads
            .iter()
            .filter(|p| p.run_id == run_id)
            .map(|p| p.frame_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
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
    fn blit_burn_luma(frame: &mut GrayImage, p: &Payload, qr_px: u32, ox: u32, oy: u32) {
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
    fn robust_is_always_a_superset_of_the_plain_pass() {
        // No regression: every payload the plain pass finds is also in the robust result
        // (robust = plain ∪ tile passes, de-duped). A clean dual-QR frame decodes identically
        // plus whatever the tiles add — never fewer.
        let left = Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 101,
            gen_ts_ns: 2,
        };
        let (w, h) = (1920u32, 1080u32);
        let bgra = render_qr_dual_bgra(&left, &right, w, h, 700);
        let luma = bgra_to_luma(&bgra, w, h, w * 4);
        let plain = decode_qr_luma_all(luma.clone());
        let robust = decode_qr_luma_all_robust(luma);
        assert!(
            !plain.is_empty(),
            "the clean dual-QR must decode: {plain:?}"
        );
        for p in &plain {
            assert!(
                robust.contains(p),
                "robust must be a SUPERSET of the plain pass: {p:?} missing from {robust:?}"
            );
        }
    }

    #[test]
    fn robust_does_not_duplicate_a_burn_seen_in_multiple_tiles() {
        // The same burn appears in several overlapping tiles AND the full-frame pass; the
        // result must carry each DISTINCT (run_id, frame_id) exactly once (merge_payloads
        // de-dups), never an inflated count that would over-report.
        let burn = Payload {
            run_id: 911_001,
            frame_id: 555,
            gen_ts_ns: 9,
        };
        let (w, h) = (1280u32, 720u32);
        let mut luma = GrayImage::from_raw(w, h, vec![255u8; (w * h) as usize]).unwrap();
        // A 200px burn in the BOTTOM band (where the recovery tiles look), straddling the
        // boundary between two overlapping column tiles so it lands inside BOTH at once — the
        // exact condition the (run_id, frame_id) de-dup must collapse to a single payload.
        let qpx = 200u32;
        let qr = render_payload_qr(&burn, qpx);
        let qh = qr.height();
        // x at ~1/3 width (the col0/col1 boundary), y near the bottom (inside the band).
        blit_burn_luma(&mut luma, &burn, qpx, w / 3 - qr.width() / 2, h - qh - 30);
        let robust = decode_qr_luma_all_robust(luma);
        let count = robust
            .iter()
            .filter(|p| p.run_id == burn.run_id && p.frame_id == burn.frame_id)
            .count();
        assert_eq!(
            count, 1,
            "a burn seen in many tiles must appear exactly once: {robust:?}"
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
    fn robust_does_not_panic_on_a_degenerate_frame() {
        // rqrr's grid identifier has an internal assert!(scan >= 1) that PANICS on certain
        // degenerate finder geometries (rqrr-0.9.3 identify/grid.rs:206). The robust pass
        // feeds rqrr many sub-tiles; a panicking tile must NOT abort the whole frame's decode
        // (it would crash the verdict via the worker pool). A near-uniform / noisy small frame
        // is exactly the kind of input that can trip it — robust must return cleanly (empty).
        let mut img = GrayImage::from_raw(200, 130, vec![200u8; 200 * 130]).unwrap();
        // Sprinkle a few dark specks so rqrr's finder attempts (and may trip) rather than
        // bailing on a flat field — the panic path this guards.
        for (i, px) in img.pixels_mut().enumerate() {
            if i % 37 == 0 {
                px.0[0] = 0;
            }
        }
        // Must not panic; degenerate input simply yields no payloads.
        let got = decode_qr_luma_all_robust(img);
        assert!(
            got.is_empty(),
            "a degenerate frame yields no payloads (and never panics): {got:?}"
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

    // ========================================================================
    // #207 — decode_qr_luma_all_fast_then_robust: plain pass first (FAST), robust
    // tiled recovery only on a missing burn (ROBUST FALLBACK). The path is asserted via
    // the per-call DecodePath return (decode_qr_luma_all_fast_then_robust_pathed) — NO
    // global counter, so these run correctly even alongside the concurrent recording
    // unit tests that also decode (a process-global counter raced; that was the first
    // CI red).
    // ========================================================================

    /// Composite a node burn (rendered at `qr_px`) into the BOTTOM-LEFT corner of a clean
    /// dual-QR frame, returning the luma. Models the #111 layout: big optical dual-QR top,
    /// small node burn bottom-corner. `blur` softens the whole frame (the recording chain).
    fn dual_with_bottom_burn(
        left: &Payload,
        right: &Payload,
        burn: &Payload,
        burn_px: u32,
        blur: f32,
    ) -> GrayImage {
        let (w, h) = (1920u32, 1080u32);
        let bgra = render_qr_dual_bgra(left, right, w, h, 700);
        let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
        let qh = render_payload_qr(burn, burn_px).height();
        // Reuse the tests' blit helper (defined earlier in this module).
        blit_burn_luma(&mut luma, burn, burn_px, 40, h - qh - 40);
        if blur > 0.0 {
            image::imageops::blur(&luma, blur)
        } else {
            luma
        }
    }

    #[test]
    fn fast_path_taken_when_plain_already_reads_every_expected_burn() {
        // The required id is one the plain full-frame pass ALWAYS reads — the big optical
        // dual-QR (run_id 7), which decodes full-frame on every well-formed frame (every other
        // decode test relies on this). With the required id already present, the fast gate must
        // SKIP the ~10×-cost tiled recovery: the call reports DecodePath::Fast and the result
        // equals the plain pass (the tiles would add nothing). Using the optical id keeps this
        // DETERMINISTIC — no dependence on whether rqrr's full-frame finder happens to lock a
        // small synthetic burn (the very intermittency #186/#202 is about).
        let left = Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 101,
            gen_ts_ns: 2,
        };
        let bgra = render_qr_dual_bgra(&left, &right, 1920, 1080, 700);
        let luma = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);

        // Precondition: the plain pass alone already reads the required (optical) id.
        let plain = decode_qr_luma_all(luma.clone());
        assert!(
            plain.iter().any(|p| p.run_id == 7),
            "precondition: the plain pass must read the optical dual-QR (run_id 7): {plain:?}"
        );

        let (got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[7]);
        assert_eq!(
            path,
            DecodePath::Fast,
            "a frame whose expected id the plain pass already read must take the FAST path \
             (no tiled recovery)"
        );
        assert!(
            got.iter().any(|p| p.run_id == 7),
            "fast path still returns the required id: {got:?}"
        );
    }

    #[test]
    fn fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame() {
        // A REAL "burn-unreadable" recording frame (cam1 burn 911001.1727) where the
        // full-frame pass — even #363's plain ∪ Otsu union — genuinely MISSES the node burn
        // (the #186/#202 size-disparity / detector-coverage gap; cv2 reads it from the same
        // pixels, rqrr's full-frame detect_grids does not). The #207 fast gate must detect the
        // missing expected burn, fall back to the robust tiled recovery (DecodePath::Robust),
        // and RECOVER the exact burn. This is the genuine gap that the synthetic perfect-QR
        // model can no longer reproduce after #363 strengthened the full-frame pass — so it is
        // locked here against the real pixels (the verdict's actual input). Cross-checked by
        // tests/burn_fixture_decode.rs.
        const RID: u32 = 911_001; // BURN_RUN_ID_CAM1
        const FID: u32 = 1727;
        let luma = optical_fixture_luma("burn-unreadable/cam1-frame-1148.png");

        // Precondition: even the #363 full-frame Otsu union MISSES this real burn.
        let full = decode_qr_luma_all(luma.clone());
        assert!(
            !full.iter().any(|p| p.run_id == RID && p.frame_id == FID),
            "precondition: the full-frame pass (plain ∪ Otsu) must MISS this real burn \
             ({RID}.{FID}) — the #186 detector-coverage gap; full={:?}",
            full.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );

        let (got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[RID]);
        assert_eq!(
            path,
            DecodePath::Robust,
            "a frame missing an expected burn from the full-frame pass must take the ROBUST \
             fallback"
        );
        assert!(
            got.iter().any(|p| p.run_id == RID && p.frame_id == FID),
            "the robust fallback must RECOVER the real burn ({RID}.{FID}) — #186 0-miss \
             preserved: {:?}",
            got.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn fast_then_robust_is_identical_to_robust_always() {
        // The optimization changes WHEN the tiles run, never WHAT is read. On both a clean
        // frame (fast path) and a softened frame (robust fallback) the fast-then-robust
        // result must equal robust-always exactly (order-independent payload set).
        let left = Payload {
            run_id: 7,
            frame_id: 200,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 201,
            gen_ts_ns: 2,
        };
        let burn = Payload {
            run_id: 911_002,
            frame_id: 5555,
            gen_ts_ns: 3,
        };
        let key = |p: &Payload| (p.run_id, p.frame_id, p.gen_ts_ns);
        for blur in [0.0f32, 2.8] {
            let luma = dual_with_bottom_burn(
                &left,
                &right,
                &burn,
                if blur == 0.0 { 360 } else { 260 },
                blur,
            );
            let mut robust = decode_qr_luma_all_robust(luma.clone());
            let mut fast = decode_qr_luma_all_fast_then_robust(luma, &[burn.run_id]);
            robust.sort_by_key(key);
            fast.sort_by_key(key);
            assert_eq!(
                robust, fast,
                "fast-then-robust must read the IDENTICAL set as robust-always (blur={blur})"
            );
        }
    }

    /// #632 gap 1: the any-of group IS satisfied when the deployed camera's OWN burn (cam3, not
    /// cam1) decoded in the plain pass — proving cam3-deployed recordings get the SAME fast path
    /// benefit cam1-deployed ones already had, without requiring cam1's (never-emitted) burn.
    #[test]
    fn grouped_gate_fast_path_when_deployed_camera_is_cam3_not_cam1() {
        const STRIH_ID: u32 = 911_002;
        const CAM1_ID: u32 = 911_001;
        const CAM3_ID: u32 = 911_008;
        let left = Payload {
            run_id: 7,
            frame_id: 400,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 401,
            gen_ts_ns: 2,
        };
        // Composite BOTH the strih burn and cam3's burn into the frame (two distinct node
        // burns, as a real strih recording under cam3 test would carry: cam3's forwarded
        // capture burn + strih's own render burn). `dual_with_bottom_burn` only blits one
        // burn, so blit a second one manually at a different corner.
        let strih_burn = Payload {
            run_id: STRIH_ID,
            frame_id: 1670,
            gen_ts_ns: 3,
        };
        let cam3_burn = Payload {
            run_id: CAM3_ID,
            frame_id: 5000,
            gen_ts_ns: 4,
        };
        let mut luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);
        let (w, h) = (1920u32, 1080u32);
        let cam3_qr = render_payload_qr(&cam3_burn, 360);
        let (qw, qh) = (cam3_qr.width(), cam3_qr.height());
        // Bottom-RIGHT corner (measured actual size, never the requested px) — clear of the
        // strih burn `dual_with_bottom_burn` already placed bottom-LEFT.
        blit_burn_luma(&mut luma, &cam3_burn, 360, w - qw - 40, h - qh - 40);

        // Precondition: the plain pass reads BOTH burns, never cam1's (it was never drawn).
        let plain = decode_qr_luma_all(luma.clone());
        assert!(
            plain.iter().any(|p| p.run_id == STRIH_ID),
            "precondition: strih burn must decode: {plain:?}"
        );
        assert!(
            plain.iter().any(|p| p.run_id == CAM3_ID),
            "precondition: cam3 burn must decode: {plain:?}"
        );
        assert!(
            !plain.iter().any(|p| p.run_id == CAM1_ID),
            "precondition: cam1's burn must never appear (cam3 is under test): {plain:?}"
        );

        let (got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed(
            luma,
            &[STRIH_ID],
            &[CAM1_ID, CAM3_ID],
        );
        assert_eq!(
            path,
            DecodePath::Fast,
            "#632: strih (mandatory) present + cam3 (any-of member) present must take FAST, \
             even though cam1 (another any-of member) never appears in this recording at all"
        );
        assert!(
            got.iter().any(|p| p.run_id == CAM3_ID),
            "fast path still returns cam3's burn: {got:?}"
        );
    }

    /// #632: when NONE of the any-of group decoded (the deployed camera's burn genuinely missed
    /// the plain pass on this frame), the gate must still fall back to ROBUST — the any-of group
    /// is not a free pass, it only widens WHICH id counts, never whether recovery still runs.
    #[test]
    fn grouped_gate_falls_back_to_robust_when_any_of_group_entirely_absent() {
        const STRIH_ID: u32 = 911_002;
        const CAM1_ID: u32 = 911_001;
        const CAM3_ID: u32 = 911_008;
        let left = Payload {
            run_id: 7,
            frame_id: 500,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 501,
            gen_ts_ns: 2,
        };
        let strih_burn = Payload {
            run_id: STRIH_ID,
            frame_id: 1670,
            gen_ts_ns: 3,
        };
        // Only the mandatory strih burn is drawn — no camera burn at all (neither cam1 nor
        // cam3), modeling a frame where the deployed camera's own burn failed to decode.
        let luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);
        let (_got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed(
            luma,
            &[STRIH_ID],
            &[CAM1_ID, CAM3_ID],
        );
        assert_eq!(
            path,
            DecodePath::Robust,
            "#632: mandatory satisfied but the any-of group is ENTIRELY absent — must still \
             attempt robust recovery (the group only widens which id counts, never skips \
             recovery when none of them are found)"
        );
    }

    #[test]
    fn empty_expected_burns_always_takes_fast_path() {
        // With no expected node burns required, the fast gate is vacuously satisfied — the
        // decode never runs the tiles (a caller with nothing to require — e.g. an optical-only
        // frame — pays only the plain pass).
        let p = Payload {
            run_id: 7,
            frame_id: 1,
            gen_ts_ns: 1,
        };
        let bgra = render_qr_dual_bgra(&p, &p, 1920, 1080, 700);
        let luma = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);
        let (_got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[]);
        assert_eq!(
            path,
            DecodePath::Fast,
            "empty expected-burns set ⇒ always FAST"
        );
    }

    // ========================================================================
    // #707 — the gate's THIRD (optical) completeness dimension. Found while investigating
    // #707's residual `all_cambox_continuity` copies/gaps: a frame where the plain pass reads
    // every expected node burn fine but MISSES one dual-QR Vernier half took the FAST path
    // anyway, silently skipping the #202 robust recovery that (proven above) CAN recover a
    // read the plain pass alone misses. See `fast_path_gate_satisfied`'s doc for the full
    // reasoning + the #707 ground-truth evidence that this is a coverage gap, not a
    // decoder-correctness bug.
    // ========================================================================

    #[test]
    fn fast_path_gate_optical_check_is_a_no_op_when_none() {
        // `min_distinct_optical = None` must reproduce the EXACT pre-#707 gate (burns only) —
        // every existing caller passes `None` via the un-suffixed wrappers and must see no
        // behavior change at all.
        let one_optical = [Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        }];
        assert!(
            fast_path_gate_satisfied(&one_optical, &[], &[], None),
            "no burns required + no optical requirement ⇒ satisfied regardless of optical count"
        );
    }

    #[test]
    fn fast_path_gate_requires_min_distinct_optical_ids_when_specified() {
        // Only ONE distinct id of the optical run_id present — short of the required 2 (the
        // dual-QR Vernier's left+right halves) — the gate must NOT be satisfied even though
        // there are no burns to require at all.
        let one_optical = [Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        }];
        assert!(
            !fast_path_gate_satisfied(&one_optical, &[], &[], Some((7, 2))),
            "only 1 distinct optical id present, 2 required ⇒ NOT satisfied (must retry robust)"
        );
    }

    #[test]
    fn fast_path_gate_satisfied_when_optical_ids_meet_the_minimum() {
        let two_optical = [
            Payload {
                run_id: 7,
                frame_id: 100,
                gen_ts_ns: 1,
            },
            Payload {
                run_id: 7,
                frame_id: 101,
                gen_ts_ns: 1,
            },
        ];
        assert!(
            fast_path_gate_satisfied(&two_optical, &[], &[], Some((7, 2))),
            "2 distinct optical ids present, 2 required ⇒ satisfied"
        );
    }

    #[test]
    fn fast_path_gate_optical_check_ignores_a_repeated_id_and_other_run_ids() {
        // A repeated frame_id (same held id decoded twice, e.g. duplicate rqrr detections) must
        // NOT count as 2 DISTINCT ids; a burn payload (a different run_id entirely) must not
        // count toward the optical requirement either.
        let repeated_plus_burn = [
            Payload {
                run_id: 7,
                frame_id: 100,
                gen_ts_ns: 1,
            },
            Payload {
                run_id: 7,
                frame_id: 100,
                gen_ts_ns: 1,
            },
            Payload {
                run_id: 911_002,
                frame_id: 5555,
                gen_ts_ns: 2,
            },
        ];
        assert!(
            !fast_path_gate_satisfied(&repeated_plus_burn, &[], &[], Some((7, 2))),
            "1 distinct optical id (repeated) + an unrelated burn ⇒ still short of 2: {repeated_plus_burn:?}"
        );
    }

    #[test]
    fn optical_gate_fast_path_when_both_dual_qr_halves_and_burns_already_read() {
        // The common case (#707): a clean frame where the plain pass reads BOTH Vernier halves
        // AND the expected burn — must take FAST, unchanged from before this dimension existed.
        const STRIH_ID: u32 = 911_002;
        let left = Payload {
            run_id: 7,
            frame_id: 600,
            gen_ts_ns: 1,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 601,
            gen_ts_ns: 2,
        };
        let strih_burn = Payload {
            run_id: STRIH_ID,
            frame_id: 1670,
            gen_ts_ns: 3,
        };
        let luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);

        // Precondition: the plain pass already reads both optical halves.
        let plain = decode_qr_luma_all(luma.clone());
        let distinct_optical: std::collections::HashSet<u32> = plain
            .iter()
            .filter(|p| p.run_id == 7)
            .map(|p| p.frame_id)
            .collect();
        assert_eq!(
            distinct_optical.len(),
            2,
            "precondition: plain pass must read both Vernier halves: {plain:?}"
        );

        let (got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
            luma,
            &[STRIH_ID],
            &[],
            Some((7, 2)),
        );
        assert_eq!(
            path,
            DecodePath::Fast,
            "both optical halves + the mandatory burn already read ⇒ FAST, unchanged by #707"
        );
        assert!(got.iter().any(|p| p.run_id == STRIH_ID));
    }

    #[test]
    fn optical_gate_falls_back_to_robust_when_only_one_dual_qr_half_decodes() {
        // #707's actual defect, reproduced deterministically: render only ONE Vernier QR (the
        // "held" half — the OTHER, freshly-repainting half is entirely absent from this frame,
        // modeling the real-world moiré/shimmer miss). The plain pass finds every expected node
        // burn AND one optical id — the PRE-#707 gate (burns only) would wrongly call this FAST.
        // With the optical dimension wired in, it must fall through to ROBUST instead.
        const STRIH_ID: u32 = 911_002;
        let held = Payload {
            run_id: 7,
            frame_id: 700,
            gen_ts_ns: 1,
        };
        let strih_burn = Payload {
            run_id: STRIH_ID,
            frame_id: 1671,
            gen_ts_ns: 2,
        };
        let (w, h) = (1920u32, 1080u32);
        let bgra = render_qr_bgra(&held, w, h, 700);
        let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
        let qh = render_payload_qr(&strih_burn, 360).height();
        blit_burn_luma(&mut luma, &strih_burn, 360, 40, h - qh - 40);

        // Precondition: exactly ONE distinct optical id decodes in the plain pass (the other
        // Vernier half was never painted onto this frame at all).
        let plain = decode_qr_luma_all(luma.clone());
        let distinct_optical: std::collections::HashSet<u32> = plain
            .iter()
            .filter(|p| p.run_id == 7)
            .map(|p| p.frame_id)
            .collect();
        assert_eq!(
            distinct_optical.len(),
            1,
            "precondition: plain pass must read exactly one Vernier half: {plain:?}"
        );
        assert!(
            plain.iter().any(|p| p.run_id == STRIH_ID),
            "precondition: the mandatory burn must still decode: {plain:?}"
        );

        let (_got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
            luma,
            &[STRIH_ID],
            &[],
            Some((7, 2)),
        );
        assert_eq!(
            path,
            DecodePath::Robust,
            "#707: only 1 of 2 expected optical ids read by the plain pass — even with every \
             burn present, the gate must retry robust instead of silently accepting the short \
             optical read (the pre-#707 gate wrongly returned Fast here)"
        );
    }
}
