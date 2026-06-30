//! #364 — probe-gated colour-sampling glue for the per-camera COLOUR gate.
//!
//! All the JUDGEMENT lives in the pure, Tier-0-tested [`crate::colour_verify`] module. This file is
//! the thin probe-gated I/O that the verdict needs: (1) the per-node BURN-EXCLUSION rectangles to
//! dodge (derived from the SAME geometry the burns are written with), (2) a one-frame adapter from
//! an `image::RgbImage` to a [`CameraColourVerdict`], and (3) the ffmpeg colour pass that pulls a
//! few RGB frames from a recording and reduces them to one [`NodeColourSummary`]. It is gated
//! behind `feature = "probe"` (it pulls `image`), so it is built + tested on CI only — never
//! locally (#185). The pure module it calls is verified locally.
//!
//! ## Why the burns are dodged (and why every patch survives)
//!
//! The #367 colour scale is a VERTICAL column in the central gap between the two dual-QR halves
//! ([`crate::colour_scale`]), spanning the QR's vertical extent (y ≈ 24..724 on 1080). The four
//! burns all sit at the BOTTOM (#111 4-corner layout): the cam1 capture burn CENTER-bottom
//! ([`qr::cam1_burn_origin`], 320px, top row ≈ 736), strih bottom-LEFT and stream bottom-RIGHT
//! (`burn_geom::corner_placement`, ~0.28·h ≈ 302px on 1080, top row ≈ 738). Because the column ends
//! at the QR bottom (≈724) and every burn starts below it, the burns no longer overlap any patch
//! at all — so the burn-exclusion rects ([`node_burn_exclusions`]) are now belt-and-braces: the
//! sampler ([`crate::colour_verify::sample_patch_means`]) still dodges them, but no patch loses a
//! pixel. Confirmed on the rig by the supervisor when the real fixture lands; the rects here match
//! the writers' geometry exactly.

use crate::colour_scale::Rect;
use crate::colour_verify::{
    fit_affine, summarize_node_colour, verify_rgb_frame, verify_rgb_frame_affine,
    CameraColourVerdict, NodeColourSummary,
};
use crate::probe::payload::Payload;
use crate::probe::qr;
use anyhow::{Context, Result};
use image::{GrayImage, RgbImage};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// The cam2 PAINTER canvas the colour column + dual-QR are rendered on (the cam2 monitor is 1080p).
/// The #364 colour gate maps these CANVAS coordinates into each recording through the affine fit
/// from the detected dual-QR corners — so a camera-of-a-monitor recording (shifted / scaled / not
/// pixel-identical, possibly a different resolution) is sampled where the content actually landed,
/// not at fixed canvas coords. If the cam2 monitor resolution changes, update these (same
/// convention as `colour_scale`'s test constants; the painter `qr_size`/`top_margin` are passed in).
pub const PAINTER_CANVAS_W: u32 = 1920;
pub const PAINTER_CANVAS_H: u32 = 1080;

/// A few px of pad added around every burn-exclusion rectangle, covering the QR quiet zone and any
/// integer rounding in the canvas-relative geometry, so a burn module never bleeds into a sample.
/// Over-excluding by a few px is safe — the colour column sits in the central gap, well above the
/// bottom-anchored burns, so no patch loses pixels regardless.
const BURN_EXCLUSION_PAD_PX: u32 = 6;

/// Canvas-height fraction of the strih/stream corner burn QR (mirrors
/// `burn_geom::BURN_QR_HEIGHT_FRACTION`).
const CORNER_BURN_HEIGHT_FRACTION: f64 = 0.28;

/// Canvas-height fraction of the corner burn edge margin (mirrors `burn_geom::BURN_MARGIN_FRACTION`
/// = 40/1080).
const CORNER_BURN_MARGIN_FRACTION: f64 = 40.0 / 1080.0;

/// Default number of frames sampled across a recording for the colour gate. The colour scale is
/// static reference content and a real colour defect is persistent, so a handful of frames spread
/// across the clip is sufficient and bounds the cost independent of the recording's length.
pub const DEFAULT_COLOUR_SAMPLES: usize = 12;

/// Grow `r` by `pad` px on every side, clamped to the canvas (no underflow / no off-canvas).
fn pad_rect(r: Rect, pad: u32, canvas_w: u32, canvas_h: u32) -> Rect {
    let x = r.x.saturating_sub(pad);
    let y = r.y.saturating_sub(pad);
    let x_end = (r.x + r.w + pad).min(canvas_w);
    let y_end = (r.y + r.h + pad).min(canvas_h);
    Rect {
        x,
        y,
        w: x_end.saturating_sub(x),
        h: y_end.saturating_sub(y),
    }
}

/// The burn rectangles to DODGE when sampling the colour column on a `canvas_w`×`canvas_h` frame:
/// the cam1 capture burn (center-bottom) and the strih/stream corner burns (bottom-left /
/// bottom-right). Each is computed from the SAME geometry the writers use (`qr::cam1_burn_origin`
/// and `burn_geom::corner_placement`), then padded by [`BURN_EXCLUSION_PAD_PX`]. Empty for a canvas
/// too small to carry the burns.
pub fn node_burn_exclusions(canvas_w: u32, canvas_h: u32) -> Vec<Rect> {
    if canvas_w == 0 || canvas_h == 0 {
        return Vec::new();
    }
    let mut rects = Vec::with_capacity(3);

    // cam1 capture burn — horizontally centered, bottom-anchored (qr::cam1_burn_origin geometry).
    let cam1_px = qr::CAM1_BURN_QR_PX.min(canvas_w).min(canvas_h);
    let (cx, cy) = qr::cam1_burn_origin(canvas_w, canvas_h, cam1_px, cam1_px);
    rects.push(Rect {
        x: cx,
        y: cy,
        w: cam1_px,
        h: cam1_px,
    });

    // strih (bottom-left) + stream (bottom-right) corner burns — burn_geom::corner_placement.
    let margin = ((CORNER_BURN_MARGIN_FRACTION * canvas_h as f64) as u32).max(8);
    let mut side = (CORNER_BURN_HEIGHT_FRACTION * canvas_h as f64) as u32;
    side = side.max(64);
    let max_w = canvas_w.saturating_sub(2 * margin).max(1);
    let max_h = canvas_h.saturating_sub(2 * margin).max(1);
    side = side.min(max_w).min(max_h).max(1);
    // Bottom edge sits at canvas_h - margin; top = bottom - side.
    let top = canvas_h.saturating_sub(margin).saturating_sub(side);
    // bottom-left
    rects.push(Rect {
        x: margin,
        y: top,
        w: side,
        h: side,
    });
    // bottom-right
    let right_x = canvas_w.saturating_sub(margin).saturating_sub(side);
    rects.push(Rect {
        x: right_x,
        y: top,
        w: side,
        h: side,
    });

    rects
        .into_iter()
        .map(|r| pad_rect(r, BURN_EXCLUSION_PAD_PX, canvas_w, canvas_h))
        .collect()
}

/// Sample + classify ONE recorded frame's colour scale. `img` is the native-resolution RGB frame;
/// `qr_size`/`top_margin` are the dual-QR layout the column was painted with (so the gate samples
/// the same central-gap rects the painter wrote); `exclusions` are the per-node burn rects from
/// [`node_burn_exclusions`]. Pure handoff to [`verify_rgb_frame`] — `RgbImage` is packed RGB8,
/// exactly what the sampler expects.
pub fn colour_verdict_from_rgb_image(
    img: &RgbImage,
    qr_size: u32,
    top_margin: u32,
    exclusions: &[Rect],
) -> CameraColourVerdict {
    verify_rgb_frame(
        img.as_raw(),
        img.width(),
        img.height(),
        qr_size,
        top_margin,
        exclusions,
    )
}

/// Rec.601 luma of a packed-RGB8 frame — what `rqrr` ingests to find the dual-QR finder patterns.
fn rgb_to_luma(img: &RgbImage) -> GrayImage {
    let (w, h) = (img.width(), img.height());
    let src = img.as_raw();
    let mut out = GrayImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let j = i * 3;
        let (r, g, b) = (src[j] as f32, src[j + 1] as f32, src[j + 2] as f32);
        *px = image::Luma([(0.299 * r + 0.587 * g + 0.114 * b).round() as u8]);
    }
    out
}

/// Shoelace area of a quad given its 4 corners (winding-agnostic, always ≥ 0).
fn quad_area(p: &[(f64, f64); 4]) -> f64 {
    let mut a = 0.0;
    for i in 0..4 {
        let (x1, y1) = p[i];
        let (x2, y2) = p[(i + 1) % 4];
        a += x1 * y2 - x2 * y1;
    }
    (a / 2.0).abs()
}

/// Reorder 4 quad corners into canonical TL, TR, BR, BL by the sum/difference rule — robust to the
/// winding `rqrr` reports AND to small camera rotation: TL has min(x+y), BR max(x+y), TR max(x−y),
/// BL min(x−y). Pairing these with [`canvas_dual_qr_corners`] (also TL,TR,BR,BL) gives correct
/// correspondences for the affine fit regardless of detection order.
fn order_corners(p: [(f64, f64); 4]) -> [(f64, f64); 4] {
    let by = |f: fn((f64, f64)) -> f64, max: bool| -> (f64, f64) {
        let mut best = p[0];
        let mut bv = f(p[0]);
        for &c in &p[1..] {
            let v = f(c);
            if (max && v > bv) || (!max && v < bv) {
                bv = v;
                best = c;
            }
        }
        best
    };
    let tl = by(|c| c.0 + c.1, false);
    let br = by(|c| c.0 + c.1, true);
    let tr = by(|c| c.0 - c.1, true);
    let bl = by(|c| c.0 - c.1, false);
    [tl, tr, br, bl]
}

/// Pick the two dual-QR halves from candidate grids described by `(area, centroid_y, centroid_x)`,
/// returning their indices `[left, right]` ordered left→right by centroid x. The dual-QRs are the two
/// LARGEST grids in the TOP half of the frame: the four node burns are all bottom-anchored (centroid
/// below mid-height) and smaller, so they are excluded. `None` when fewer than two top-half
/// candidates exist (the frame cannot be localized → it is skipped). Pure (no rqrr) — unit-tested.
fn select_two_dual_qr(items: &[(f64, f64, f64)], frame_h: f64) -> Option<[usize; 2]> {
    let mut top: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].1 < frame_h / 2.0)
        .collect();
    if top.len() < 2 {
        return None;
    }
    // Two LARGEST top-half candidates are the dual-QR halves.
    top.sort_by(|&a, &b| items[b].0.total_cmp(&items[a].0));
    top.truncate(2);
    // Order them left → right by centroid x.
    top.sort_by(|&a, &b| items[a].2.total_cmp(&items[b].2));
    Some([top[0], top[1]])
}

/// A `canvas_w*canvas_h` BGRA buffer → packed-RGB8 `RgbImage` (drops alpha, B/G/R → R/G/B). Used to
/// feed a re-rendered reference canvas back through `rqrr`.
fn bgra_to_rgb(bgra: &[u8], canvas_w: u32, canvas_h: u32) -> RgbImage {
    let mut img = RgbImage::new(canvas_w, canvas_h);
    for (i, px) in img.pixels_mut().enumerate() {
        let j = i * 4;
        *px = image::Rgb([bgra[j + 2], bgra[j + 1], bgra[j]]);
    }
    img
}

/// The two dual-QR halves located in a frame: `(left payload, right payload, 8 corners)` — the
/// corners are left-half then right-half, each TL,TR,BR,BL.
type DualQr = (Payload, Payload, Vec<(f64, f64)>);

/// Detect the two dual-QR halves in an RGB frame: returns their decoded payloads `(left, right)` and
/// their 8 corners (left then right, each TL,TR,BR,BL). The halves are the two LARGEST top-half QR
/// grids that DECODE — the four node burns are bottom-anchored (excluded by position). `None` when
/// fewer than two such grids are found (frame cannot be localized → skipped).
fn detect_dual_qr(img: &RgbImage) -> Option<DualQr> {
    let luma = rgb_to_luma(img);
    let frame_h = luma.height() as f64;
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    let mut corners: Vec<[(f64, f64); 4]> = Vec::new();
    let mut payloads: Vec<Payload> = Vec::new();
    let mut items: Vec<(f64, f64, f64)> = Vec::new();
    for grid in prepared.detect_grids() {
        let b = grid.bounds;
        let c = [
            (b[0].x as f64, b[0].y as f64),
            (b[1].x as f64, b[1].y as f64),
            (b[2].x as f64, b[2].y as f64),
            (b[3].x as f64, b[3].y as f64),
        ];
        if let Ok((_meta, content)) = grid.decode() {
            if let Some(p) = Payload::decode(&content) {
                let area = quad_area(&c);
                let cy = c.iter().map(|q| q.1).sum::<f64>() / 4.0;
                let cx = c.iter().map(|q| q.0).sum::<f64>() / 4.0;
                items.push((area, cy, cx));
                corners.push(c);
                payloads.push(p);
            }
        }
    }
    let [li, ri] = select_two_dual_qr(&items, frame_h)?;
    let mut pts = Vec::with_capacity(8);
    pts.extend_from_slice(&order_corners(corners[li]));
    pts.extend_from_slice(&order_corners(corners[ri]));
    Some((payloads[li], payloads[ri], pts))
}

/// Sample + classify ONE recorded frame's colour scale, LOCALIZED to where the camera actually
/// landed the painter canvas — the #364 camera-of-a-monitor fix. The recording is a broadcast camera
/// filming the cam2 MONITOR, so the painted column does NOT sit at fixed canvas coords.
///
/// The canvas→recording affine is derived from the dual-QR fiducials WITHOUT coupling to the QR
/// renderer's internals (quiet zone, integer-module rounding, sub-`qr_size` render): decode the
/// recording's two dual-QR payloads, RE-RENDER the reference canvas from those exact payloads via the
/// same painter path (`render_qr_dual_bgra`), detect the reference's dual-QR corners (the true
/// CANVAS-frame positions of the very same symbols), and fit reference-corners → recording-corners.
/// Pairing like-with-like (symbol↔symbol of identical geometry) makes the fit the PURE camera
/// transform, so sampling the colour column ([`crate::colour_scale::colour_scale_patches`], same
/// canvas geometry the painter used) through it lands exactly where the camera put it. Returns
/// `None` when the two
/// dual-QRs can't be decoded/located or the fit is degenerate — the caller SKIPS the frame
/// (fail-closed; no fixed-coord fallback, since a mis-located sample is a false signal).
pub fn colour_verdict_from_rgb_image_localized(
    img: &RgbImage,
    canvas_w: u32,
    canvas_h: u32,
    qr_size: u32,
    top_margin: u32,
    exclusions: &[Rect],
) -> Option<CameraColourVerdict> {
    let (left, right, rec_corners) = detect_dual_qr(img)?;
    // Reference canvas: the SAME painter render for these exact payloads. Its detected dual-QR
    // corners are the true canvas-frame positions of the identical symbols, so the fit is the pure
    // camera transform (no box-vs-symbol / quiet-zone mismatch).
    let ref_bgra = qr::render_qr_dual_bgra(&left, &right, canvas_w, canvas_h, qr_size);
    let ref_rgb = bgra_to_rgb(&ref_bgra, canvas_w, canvas_h);
    let (_, _, ref_corners) = detect_dual_qr(&ref_rgb)?;
    let affine = fit_affine(&ref_corners, &rec_corners)?;
    Some(verify_rgb_frame_affine(
        img.as_raw(),
        img.width(),
        img.height(),
        affine,
        canvas_w,
        canvas_h,
        qr_size,
        top_margin,
        exclusions,
    ))
}

/// Native width×height of a recording's first video stream, via `ffprobe`.
fn probe_dimensions(path: &Path) -> Result<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(path)
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffprobe (install ffmpeg: apt install ffmpeg)")?;
    anyhow::ensure!(
        out.status.success(),
        "ffprobe failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s
        .lines()
        .next()
        .with_context(|| format!("ffprobe returned no video stream for {}", path.display()))?
        .trim();
    let (w, h) = line
        .split_once('x')
        .with_context(|| format!("ffprobe dimensions not WxH: {line:?}"))?;
    let width: u32 = w
        .parse()
        .with_context(|| format!("ffprobe width not a number: {w:?}"))?;
    let height: u32 = h
        .parse()
        .with_context(|| format!("ffprobe height not a number: {h:?}"))?;
    anyhow::ensure!(
        width > 0 && height > 0,
        "ffprobe returned zero dimension ({width}x{height}) for {}",
        path.display()
    );
    Ok((width, height))
}

/// Duration (seconds) of a recording's container, via `ffprobe`. Used to space the colour samples
/// evenly across the clip.
fn probe_duration_secs(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffprobe for duration")?;
    anyhow::ensure!(
        out.status.success(),
        "ffprobe duration failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let v: f64 = s
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .with_context(|| format!("ffprobe duration not a number for {}", path.display()))?;
    anyhow::ensure!(
        v.is_finite() && v > 0.0,
        "ffprobe non-positive duration {v}"
    );
    Ok(v)
}

/// Decode ONE native-resolution RGB frame at timestamp `ts_secs` (input seek) via ffmpeg. Returns
/// `Ok(None)` when the seek landed past the last frame (no bytes) — the caller simply takes one
/// fewer sample rather than failing the whole pass.
fn decode_one_rgb_frame_at(
    path: &Path,
    ts_secs: f64,
    width: u32,
    height: u32,
) -> Result<Option<RgbImage>> {
    let frame_bytes = (width as usize) * (height as usize) * 3;
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &format!("{ts_secs:.3}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg (install ffmpeg: apt install ffmpeg)")?;
    let mut stdout = child
        .stdout
        .take()
        .context("ffmpeg stdout pipe unavailable")?;
    let mut buf = vec![0u8; frame_bytes];
    let read_res = stdout.read_exact(&mut buf);
    // Drain anything remaining + reap the child so it never lingers.
    let _ = std::io::copy(&mut stdout, &mut std::io::sink());
    let status = child.wait().context("wait for ffmpeg")?;
    match read_res {
        Ok(()) => {
            anyhow::ensure!(
                status.success(),
                "ffmpeg failed extracting colour frame at {ts_secs:.3}s from {}",
                path.display()
            );
            let img = RgbImage::from_raw(width, height, buf)
                .context("rgb24 buffer sized width*height*3")?;
            Ok(Some(img))
        }
        // Fewer than a full frame's bytes ⇒ the seek was past EOF; not an error, just no sample.
        Err(_) => Ok(None),
    }
}

/// Pull up to `samples` evenly-spaced RGB frames from `path` and reduce their colour-scale readings
/// to ONE [`NodeColourSummary`] (the per-camera colour verdict for this recording). Frames are
/// sampled by input-seek so the cost is bounded by `samples`, independent of the recording length.
/// `qr_size`/`top_margin` are the dual-QR layout the painter rendered (so the central-gap column is
/// sampled where it was painted). The burn rects are derived from the recording's own native
/// dimensions. Errors out if NO frame could be read (the colour gate must never silently pass when
/// it could not look).
pub fn extract_recording_colour_summary(
    path: &Path,
    samples: usize,
    qr_size: u32,
    top_margin: u32,
) -> Result<NodeColourSummary> {
    let samples = samples.max(1);
    let (width, height) = probe_dimensions(path)?;
    let duration = probe_duration_secs(path)?;
    // Burn exclusions are in CANVAS space — the affine maps canvas→recording and the localized
    // sampler tests exclusions against the inverse-mapped canvas coordinate.
    let exclusions = node_burn_exclusions(PAINTER_CANVAS_W, PAINTER_CANVAS_H);

    let mut verdicts: Vec<CameraColourVerdict> = Vec::with_capacity(samples);
    let mut localize_failures = 0usize;
    for i in 0..samples {
        // Sample at the CENTER of each of `samples` equal time slices (avoids the very first/last
        // frame, which can be a partial teardown frame).
        let ts = duration * ((i as f64) + 0.5) / (samples as f64);
        if let Some(img) = decode_one_rgb_frame_at(path, ts, width, height)? {
            // Localize via the frame's own dual-QR corners; a frame whose dual-QRs can't be located
            // is SKIPPED (never sampled at the wrong canvas coords) rather than mis-measured.
            match colour_verdict_from_rgb_image_localized(
                &img,
                PAINTER_CANVAS_W,
                PAINTER_CANVAS_H,
                qr_size,
                top_margin,
                &exclusions,
            ) {
                Some(v) => verdicts.push(v),
                None => localize_failures += 1,
            }
        }
    }
    anyhow::ensure!(
        !verdicts.is_empty(),
        "colour gate: could not localize the dual-QR colour scale in ANY of {} sampled frames of \
         {} ({} frames had no locatable dual-QR pair) — cannot verify colour",
        samples,
        path.display(),
        localize_failures
    );
    tracing::info!(
        file = %path.display(),
        localized = verdicts.len(),
        localize_failures,
        requested = samples,
        "colour-scale sampling complete (affine-localized)"
    );
    Ok(summarize_node_colour(&verdicts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour_scale::{
        colour_scale_patches, Rgb, DEFAULT_QR_SIZE, PATCH_COLOURS, TOP_MARGIN_PX,
    };
    use crate::colour_verify::Affine;
    use crate::probe::payload::Payload;

    const W: u32 = 1920;
    const H: u32 = 1080;
    const QR: u32 = DEFAULT_QR_SIZE;
    const TM: u32 = TOP_MARGIN_PX;

    /// Paint a packed-RGB8 `RgbImage` with the reference colour scale (central-gap column) through
    /// `f` (identity = a correct frame). Non-patch pixels stay black.
    fn paint_rgb_image(f: impl Fn(Rgb) -> Rgb) -> RgbImage {
        let mut img = RgbImage::new(W, H);
        for (rect, rgb) in colour_scale_patches(W, H, QR, TM) {
            let c = f(rgb);
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    img.put_pixel(x, y, image::Rgb([c.r, c.g, c.b]));
                }
            }
        }
        img
    }

    fn to_gray(c: Rgb) -> Rgb {
        let y = (0.299 * c.r as f64 + 0.587 * c.g as f64 + 0.114 * c.b as f64).round() as u8;
        Rgb::new(y, y, y)
    }

    #[test]
    fn burn_exclusions_cover_the_three_burns_yet_leave_every_patch_samplable() {
        let ex = node_burn_exclusions(W, H);
        assert_eq!(ex.len(), 3, "cam1 + strih + stream burns");
        // Each excluded rect is bottom-anchored and within the canvas.
        for r in &ex {
            assert!(r.x + r.w <= W && r.y + r.h <= H, "in canvas: {r:?}");
            assert!(
                r.y + r.h <= H && r.y + r.h >= H - 80,
                "bottom-anchored: {r:?}"
            );
        }
        // Every colour patch must still have ≥1 samplable pixel after dodging — otherwise the gate
        // would lose that patch. (The burns leave a clear strip at the very bottom of the band.)
        let correct = paint_rgb_image(|c| c);
        let v = colour_verdict_from_rgb_image(&correct, QR, TM, &ex);
        assert_eq!(
            v.checked_count(),
            PATCH_COLOURS.len(),
            "every patch keeps samplable pixels after burn-dodge"
        );
        assert!(
            v.is_pass(),
            "a correct frame passes even with the burns dodged"
        );
    }

    #[test]
    fn adapter_passes_correct_and_fails_grayscale_through_the_real_burn_dodge() {
        let ex = node_burn_exclusions(W, H);
        let correct = colour_verdict_from_rgb_image(&paint_rgb_image(|c| c), QR, TM, &ex);
        assert!(correct.is_pass(), "correct camera passes");

        let gray = colour_verdict_from_rgb_image(&paint_rgb_image(to_gray), QR, TM, &ex);
        assert!(
            !gray.is_pass(),
            "grayscale camera FAILS even with the burns dodged"
        );
        assert!(
            gray.wrong_count() >= 6,
            "all chromatic patches wrong: {}",
            gray.wrong_count()
        );
    }

    // ---- #364 affine LOCALIZATION glue (camera-of-a-monitor) ----

    /// Render the full painter canvas (dual-QR + colour column) to a packed-RGB8 image — exactly
    /// what the cam2 monitor shows (before any camera warp).
    fn render_canvas_rgb() -> RgbImage {
        let left = Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1_700_000_000_000_000_000,
        };
        let right = Payload {
            run_id: 7,
            frame_id: 101,
            gen_ts_ns: 1_700_000_000_000_000_001,
        };
        let mut bgra = qr::render_qr_dual_bgra(&left, &right, W, H, QR);
        qr::blit_colour_scale_bgra(&mut bgra, W, H, QR, TM);
        bgra_to_rgb(&bgra, W, H)
    }

    /// Warp an RGB image through `a` (canvas→recording) — simulating the camera filming the monitor.
    fn warp_rgb(src: &RgbImage, a: Affine) -> RgbImage {
        let inv = a.inverse().unwrap();
        let (w, h) = (src.width(), src.height());
        let mut out = RgbImage::new(w, h);
        for ry in 0..h {
            for rx in 0..w {
                let (cx, cy) = inv.apply(rx as f64 + 0.5, ry as f64 + 0.5);
                if cx < 0.0 || cy < 0.0 {
                    continue;
                }
                let (ux, uy) = (cx as u32, cy as u32);
                if ux >= w || uy >= h {
                    continue;
                }
                out.put_pixel(rx, ry, *src.get_pixel(ux, uy));
            }
        }
        out
    }

    #[test]
    fn order_corners_canonicalizes_any_winding() {
        // A near-axis quad's corners in canonical TL, TR, BR, BL.
        let canon = [(10.0, 20.0), (110.0, 22.0), (108.0, 122.0), (12.0, 120.0)];
        // Fed in any rotation/reversal, it must come back canonical.
        let rotated = [canon[2], canon[3], canon[0], canon[1]];
        assert_eq!(order_corners(rotated), canon);
        let reversed = [canon[3], canon[2], canon[1], canon[0]];
        assert_eq!(order_corners(reversed), canon);
    }

    #[test]
    fn select_two_dual_qr_picks_largest_top_half_left_to_right() {
        // (area, centroid_y, centroid_x): two big top-half QRs + a bottom-anchored burn (excluded by
        // position) + a small top-half noise grid (excluded by area).
        let items = vec![
            (490_000.0, 374.0, 1440.0), // right QR
            (90_000.0, 889.0, 191.0),   // bottom burn — centroid below mid-height ⇒ excluded
            (490_000.0, 374.0, 480.0),  // left QR
            (400.0, 100.0, 910.0),      // top-half noise — smaller than the two QRs ⇒ excluded
        ];
        let [li, ri] = select_two_dual_qr(&items, 1080.0).expect("two top-half QRs");
        assert_eq!(items[li].2, 480.0, "left QR (smaller cx) first");
        assert_eq!(items[ri].2, 1440.0, "right QR second");

        // Fewer than two top-half candidates ⇒ None (cannot localize).
        assert!(select_two_dual_qr(&[(490_000.0, 374.0, 480.0)], 1080.0).is_none());
        assert!(
            select_two_dual_qr(&[(9e4, 889.0, 191.0), (9e4, 900.0, 1700.0)], 1080.0).is_none(),
            "all bottom-anchored ⇒ no dual-QR pair"
        );
    }

    #[test]
    fn localized_gate_passes_a_warped_correct_frame_and_fails_grayscale() {
        // Render the real monitor canvas, then WARP it (shift + scale + slight shear) as a camera
        // filming the monitor would. rqrr must find the two real dual-QRs, the affine must localize
        // the colour column, and a CORRECT frame must PASS — proving the fixed-coord false-fail is
        // cured end-to-end through real QR detection.
        let canvas = render_canvas_rgb();
        let ex = node_burn_exclusions(W, H);

        // Sanity: rqrr must locate the two dual-QRs on the un-warped render, and the (near-identity)
        // localized gate passes — isolates real QR DETECTION from the warp.
        let v0 = colour_verdict_from_rgb_image_localized(&canvas, W, H, QR, TM, &ex)
            .expect("dual-QRs detected on the un-warped render");
        assert!(
            v0.is_pass(),
            "un-warped correct frame passes: {:?}",
            v0.failures().collect::<Vec<_>>()
        );

        // A clean INTEGER translation (camera framing offset) — keeps the QR modules pixel-aligned so
        // rqrr detects reliably; the full scale/shear fit is proven in colour_verify's Tier-0 test.
        let a = Affine {
            a: 1.0,
            b: 0.0,
            tx: 40.0,
            c: 0.0,
            d: 1.0,
            ty: 30.0,
        };
        let warped = warp_rgb(&canvas, a);
        let v = colour_verdict_from_rgb_image_localized(&warped, W, H, QR, TM, &ex)
            .expect("dual-QRs detected + localized in the shifted frame");
        assert!(
            v.is_pass(),
            "a correct shifted frame PASSES once localized: {:?}",
            v.failures().collect::<Vec<_>>()
        );
        assert_eq!(
            v.checked_count(),
            PATCH_COLOURS.len(),
            "every patch sampled after localization"
        );

        // Grayscale the warped frame (QRs stay black/white so they still detect; the colour column
        // collapses) — the gate must still FAIL. Localization fixes WHERE we sample, never the verdict.
        let mut gray = warped.clone();
        for px in gray.pixels_mut() {
            let y =
                (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32).round() as u8;
            *px = image::Rgb([y, y, y]);
        }
        let vg = colour_verdict_from_rgb_image_localized(&gray, W, H, QR, TM, &ex)
            .expect("dual-QRs still detected on the grayscale frame");
        assert!(
            !vg.is_pass(),
            "a grayscale camera FAILS even when correctly localized"
        );
    }
}
