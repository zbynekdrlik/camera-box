//! Recorded-file → rqrr QR analysis (offline, #106 / part of #105 Step 1).
//!
//! The loss/delivery verdict for the zero-loss proof (#105 acceptance #1) must be
//! counted ONLY from the **recorded OBS program-output file** — never an NDI tap
//! and never the lz4 spool (both add their own sampling artifacts that contaminate
//! the measurement). This module is that recorded-file path:
//!
//! ```text
//!   recording.mkv/.mp4
//!        │  ffprobe → native width×height
//!        ▼
//!   ffmpeg -i <file> -f rawvideo -pix_fmt gray pipe:1   (gray8/luma, native res)
//!        │  one width*height luma buffer per frame
//!        ▼
//!   decode_qr_luma_all  (the EXISTING rqrr decoder in src/probe/qr.rs)
//!        │  every CRC-valid Payload in the frame (both dual-QR halves in ONE pass)
//!        ▼
//!   RecordingFrame { frame_index, payloads, tick }   → CSV / stdout for #107
//! ```
//!
//! opencv was NEVER in this repo; the prior "flaky opencv" was an external tool.
//! Decoding a sharp QR is trivial for rqrr — this path simply feeds the recorded
//! frames into the already-unit-tested rqrr decoder, no opencv anywhere.
//!
//! The per-frame decode (`decode_recording_frame`) is pure and unit-tested. The
//! ffmpeg/ffprobe spawning glue (`analyze_recording`, `probe_dimensions`,
//! `read_frames`) is an external-process boundary — excluded from coverage like
//! the other hardware/process glue (`multi_reader`, `reader`, `run`).

use crate::probe::payload::Payload;
use crate::probe::qr::decode_qr_luma_all;
use anyhow::{Context, Result};
use image::GrayImage;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// One analyzed frame of a recording, in file (capture) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingFrame {
    /// 0-based index of this frame within the recording.
    pub frame_index: u64,
    /// Every CRC-valid QR payload found in the frame (both dual-QR halves, decoded
    /// in one rqrr pass). Empty when no readable QR is present in the frame.
    pub payloads: Vec<Payload>,
    /// Effective Vernier tick = the highest `frame_id` among the decoded payloads
    /// (left QR carries the latest even tick, right the latest odd; the freshest
    /// sharp region wins — matches `decode_capture_dual`'s `max_by_key(frame_id)`).
    /// `None` when nothing decoded.
    pub tick: Option<u32>,
}

/// Decode one native-resolution luma frame into a `RecordingFrame`.
///
/// PURE (no I/O, no ffmpeg): feeds the luma image straight into the existing rqrr
/// decoder, which finds BOTH side-by-side dual-QR codes in one prepare+detect pass.
/// `frame_index` is the caller-supplied position in the recording.
pub fn decode_recording_frame(frame_index: u64, luma: GrayImage) -> RecordingFrame {
    let payloads = decode_qr_luma_all(luma);
    let tick = payloads.iter().map(|p| p.frame_id).max();
    RecordingFrame {
        frame_index,
        payloads,
        tick,
    }
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
    if !out.status.success() {
        anyhow::bail!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
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
    tracing::info!(
        file = %path.display(), width, height,
        "recording native resolution probed"
    );
    Ok((width, height))
}

/// Stream every native-resolution gray8 (luma) frame out of `path` via ffmpeg,
/// invoking `on_frame(frame_index, luma)` for each. Frames are read by exact
/// byte count (`width * height`), so a truncated trailing frame is ignored.
///
/// ffmpeg's stderr is INHERITED (not piped): on a 30-min / 54k-frame clip a piped
/// stderr we only drain after the stdout loop could fill its ~64 KB OS pipe buffer
/// while we block reading stdout — ffmpeg then blocks writing stderr and we block
/// reading stdout, a classic two-pipe deadlock. Inheriting routes any `-v error`
/// output straight to this process's stderr/logs with no buffer to fill. A
/// non-zero ffmpeg exit fails the call (the inherited error text is already in the
/// log).
fn read_frames(
    path: &Path,
    width: u32,
    height: u32,
    mut on_frame: impl FnMut(u64, GrayImage),
) -> Result<u64> {
    let frame_bytes = (width as usize) * (height as usize);
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg (install ffmpeg: apt install ffmpeg)")?;

    let mut stdout = child
        .stdout
        .take()
        .context("ffmpeg stdout pipe unavailable")?;

    let mut buf = vec![0u8; frame_bytes];
    let mut frame_index: u64 = 0;
    loop {
        match stdout.read_exact(&mut buf) {
            Ok(()) => {
                // Move `buf` into the GrayImage (no per-frame clone) and replace it
                // with a fresh buffer for the next read — on a 54k-frame clip this
                // avoids 54k redundant width*height copies.
                let owned = std::mem::replace(&mut buf, vec![0u8; frame_bytes]);
                let luma = GrayImage::from_raw(width, height, owned)
                    .context("luma buffer sized width*height")?;
                on_frame(frame_index, luma);
                frame_index += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("read ffmpeg rawvideo stdout"),
        }
    }

    let status = child.wait().context("wait for ffmpeg")?;
    if !status.success() {
        anyhow::bail!(
            "ffmpeg decode failed on {} (status {:?}); see ffmpeg stderr above",
            path.display(),
            status.code(),
        );
    }
    tracing::info!(
        file = %path.display(), frames = frame_index,
        "recording fully decoded via ffmpeg gray8 pipe"
    );
    Ok(frame_index)
}

/// Analyze a recorded OBS program-output file end-to-end: probe its native
/// resolution, stream every frame as gray8 luma via ffmpeg, decode each with the
/// rqrr decoder, and return one [`RecordingFrame`] per frame in capture order.
///
/// This is the recorded-file analysis entrypoint (#106). It NEVER uses an NDI tap
/// or the lz4 spool — the loss/delivery verdict (#105 acceptance #1, computed
/// downstream in #107) must be derived only from the recorded file.
pub fn analyze_recording(path: &Path) -> Result<Vec<RecordingFrame>> {
    let (width, height) = probe_dimensions(path)?;
    let mut frames = Vec::new();
    read_frames(path, width, height, |idx, luma| {
        let f = decode_recording_frame(idx, luma);
        tracing::debug!(
            frame = f.frame_index, decoded = f.payloads.len(), tick = ?f.tick,
            "recording frame analyzed"
        );
        frames.push(f);
    })?;
    let decoded: usize = frames.iter().filter(|f| !f.payloads.is_empty()).count();
    tracing::info!(
        file = %path.display(), total = frames.len(), with_qr = decoded,
        "recording analysis complete"
    );
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::luma::bgra_to_luma;
    use crate::probe::qr::render_qr_dual_bgra;

    fn dual_qr_luma(left_id: u32, right_id: u32) -> GrayImage {
        let (cw, ch, qs) = (960u32, 540u32, 260u32);
        let l = Payload {
            run_id: 6519,
            frame_id: left_id,
            gen_ts_ns: 1,
        };
        let r = Payload {
            run_id: 6519,
            frame_id: right_id,
            gen_ts_ns: 2,
        };
        let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
        bgra_to_luma(&bgra, cw, ch, cw * 4)
    }

    #[test]
    fn decode_recording_frame_returns_both_dual_qrs() {
        // A strih-6519-type frame (two sharp QRs) decodes to BOTH — the exact case
        // opencv silently returned 0/1 on.
        let f = decode_recording_frame(0, dual_qr_luma(6518, 6519));
        assert_eq!(f.frame_index, 0);
        assert_eq!(f.payloads.len(), 2, "both QRs decode: {:?}", f.payloads);
        let ids: Vec<u32> = f.payloads.iter().map(|p| p.frame_id).collect();
        assert!(ids.contains(&6518) && ids.contains(&6519), "ids {ids:?}");
    }

    #[test]
    fn tick_is_max_frame_id() {
        // Effective Vernier tick = max(left, right).
        let f = decode_recording_frame(7, dual_qr_luma(200, 201));
        assert_eq!(f.frame_index, 7);
        assert_eq!(f.tick, Some(201));
    }

    #[test]
    fn blank_frame_decodes_to_zero_and_none_tick() {
        let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
        let f = decode_recording_frame(3, blank);
        assert!(f.payloads.is_empty(), "no QR in a blank frame");
        assert_eq!(f.tick, None);
    }

    #[test]
    fn deterministic_same_frame_same_result() {
        // STRICT acceptance: same frame → same result.
        let a = decode_recording_frame(0, dual_qr_luma(10, 11));
        let b = decode_recording_frame(0, dual_qr_luma(10, 11));
        assert_eq!(a, b);
    }
}
