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
    summarize_node_colour, verify_rgb_frame, CameraColourVerdict, NodeColourSummary,
};
use crate::probe::qr;
use anyhow::{Context, Result};
use image::RgbImage;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

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
    let exclusions = node_burn_exclusions(width, height);

    let mut verdicts: Vec<CameraColourVerdict> = Vec::with_capacity(samples);
    for i in 0..samples {
        // Sample at the CENTER of each of `samples` equal time slices (avoids the very first/last
        // frame, which can be a partial teardown frame).
        let ts = duration * ((i as f64) + 0.5) / (samples as f64);
        if let Some(img) = decode_one_rgb_frame_at(path, ts, width, height)? {
            verdicts.push(colour_verdict_from_rgb_image(
                &img,
                qr_size,
                top_margin,
                &exclusions,
            ));
        }
    }
    anyhow::ensure!(
        !verdicts.is_empty(),
        "colour gate: could not read ANY frame from {} (cannot verify colour)",
        path.display()
    );
    tracing::info!(
        file = %path.display(),
        sampled = verdicts.len(),
        requested = samples,
        "colour-scale sampling complete"
    );
    Ok(summarize_node_colour(&verdicts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour_scale::{
        colour_scale_patches, Rgb, DEFAULT_QR_SIZE, PATCH_COLOURS, TOP_MARGIN_PX,
    };

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
}
