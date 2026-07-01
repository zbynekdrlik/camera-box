//! #188 A/V-sync offset from a recording — the phone-free verdict.
//!
//! Decodes the cam2 dual-QR from the recording's VIDEO (recorded frame → optical tick → video time)
//! and the QPSK marker from the recording's AUDIO track (the hand-mic'd cam2 HDMI marker, captured
//! into the stream OBS as the "mbc" input), then pairs them via the cam2 emit log
//! (`index → frame_id`) to compute the video↔audio offset. Cross-platform (ffmpeg + the pure
//! `qpsk_marker` decode + `recording::analyze_recording`) so it runs ON stream.lan alongside the
//! zero-loss `recording-verdict` (#193). All the JUDGEMENT (decode, pair, offset, interpolation)
//! is pure Tier-0 in `crate::qpsk_marker`; this module is only the ffmpeg I/O glue.

use crate::probe::recording::analyze_recording;
use crate::qpsk_marker::{
    av_offset_candidates, cluster_offset_ms, decode_markers, parse_ffprobe_start_time,
    parse_qpsk_marker_log, AudioParams, AvOffset,
};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Result of an A/V-sync recording measurement.
#[derive(Debug, Clone)]
pub struct AvSyncReport {
    /// The A/V offset (video − audio) in ms; > 0 = video LAGS audio.
    pub offset: AvOffset,
    /// Video frame rate used to convert frame_index → time.
    pub fps: f64,
    /// QPSK markers decoded from the audio track.
    pub audio_markers: usize,
    /// Distinct cam2 dual-QR ticks read from the video.
    pub video_ticks: usize,
    /// Emit-log rows parsed.
    pub emit_rows: usize,
    /// Candidate offsets formed (audio marker × matching emit frame_id, video-interpolable). The
    /// offset is the median of the densest cluster of these; `candidates − matched` is the rejected
    /// scatter (false audio decodes + wrong-lap matches).
    pub candidates: usize,
}

/// Video frame rate of `path`'s first video stream (`r_frame_rate` "num/den"), via ffprobe.
fn probe_video_fps(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffprobe (install ffmpeg)")?;
    anyhow::ensure!(
        out.status.success(),
        "ffprobe fps failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s
        .lines()
        .next()
        .with_context(|| format!("ffprobe returned no fps for {}", path.display()))?
        .trim();
    let fps = match line.split_once('/') {
        Some((n, d)) => {
            let n: f64 = n.trim().parse().with_context(|| format!("fps num {n:?}"))?;
            let d: f64 = d.trim().parse().with_context(|| format!("fps den {d:?}"))?;
            anyhow::ensure!(d != 0.0, "ffprobe fps denominator 0 for {}", path.display());
            n / d
        }
        None => line.parse().with_context(|| format!("fps {line:?}"))?,
    };
    anyhow::ensure!(fps > 0.0, "ffprobe fps {fps} for {}", path.display());
    Ok(fps)
}

/// `start_time` (s) of one stream (`selector` e.g. `"v:0"` / `"a:1"`) via ffprobe. Video
/// `frame_index/fps` and audio `sample/rate` each count from their OWN stream's first
/// packet; a non-zero per-stream `start_time` (mux edit list, encoder priming) would shift the
/// measured offset by the DIFFERENCE silently, so both timelines are rebased onto the shared
/// container origin. Missing/`N/A` parses as 0.0 (stream at the origin).
fn probe_stream_start_time(path: &Path, selector: &str) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            selector,
            "-show_entries",
            "stream=start_time",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffprobe (install ffmpeg)")?;
    anyhow::ensure!(
        out.status.success(),
        "ffprobe start_time ({selector}) failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(parse_ffprobe_start_time(s.lines().next().unwrap_or("")))
}

/// Extract audio track `track` of `path` as mono f32 @ 48 kHz via ffmpeg (channels mixed to mono —
/// the marker survives the mix, and the QPSK decode is amplitude-tolerant).
fn extract_audio_mono_f32(path: &Path, track: u32, sample_rate: u32) -> Result<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:a:{track}"),
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffmpeg for audio extract (install ffmpeg)")?;
    anyhow::ensure!(
        out.status.success(),
        "ffmpeg audio extract failed on {} (track {track}): {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let bytes = out.stdout;
    anyhow::ensure!(
        bytes.len() >= 4 && bytes.len() % 4 == 0,
        "ffmpeg returned {} bytes (not whole f32) for {} track {track}",
        bytes.len(),
        path.display()
    );
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(samples)
}

/// Measure the A/V-sync offset of `recording` (which carries BOTH the cam2 dual-QR video and the
/// mbc audio marker) using the cam2 `marker_log_csv` (the emitter's `index,frame_id,emit_ts_ns`).
///
/// Steps: ffprobe fps → decode every recorded frame's dual-QR tick → (tick, video_ts) samples;
/// extract audio track → QPSK-decode (audio_ts, index); form a candidate `video − audio` offset per
/// index-match (`av_offset_candidates`); the offset is the median of the DENSEST cluster of those
/// candidates (`cluster_offset_ms`) — robust to false audio decodes and the index wrap.
pub fn av_sync_from_recording(
    recording: &Path,
    marker_log_csv: &str,
    params: &AudioParams,
    audio_track: u32,
    threshold: f64,
    min_matched: usize,
    cluster_tol_ms: f64,
) -> Result<AvSyncReport> {
    let fps = probe_video_fps(recording)?;
    let emit_log = parse_qpsk_marker_log(marker_log_csv);
    anyhow::ensure!(
        !emit_log.is_empty(),
        "emit log is empty (no markers logged)"
    );

    // Rebase both timelines onto the shared container origin (per-stream start_time can differ).
    let video_start = probe_stream_start_time(recording, "v:0")?;
    let audio_start = probe_stream_start_time(recording, &format!("a:{audio_track}"))?;

    // Video: decode every frame's optical tick → sorted (tick, video_ts) samples, first per tick.
    let frames = analyze_recording(recording)
        .with_context(|| format!("decode video {}", recording.display()))?;
    let mut ticks: Vec<(u32, f64)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fr in &frames {
        if let Some(t) = fr.tick {
            if seen.insert(t) {
                ticks.push((t, video_start + fr.frame_index as f64 / fps));
            }
        }
    }
    ticks.sort_by_key(|&(t, _)| t);
    let video_ticks = ticks.len();

    // Audio: extract the mbc track → QPSK markers (audio_ts, index), on the container origin.
    let audio = extract_audio_mono_f32(recording, audio_track, params.sample_rate)?;
    let audio_markers: Vec<(f64, u8)> = decode_markers(&audio, params, threshold)
        .into_iter()
        .map(|(ts, idx)| (audio_start + ts, idx))
        .collect();
    let n_audio = audio_markers.len();

    // Pair: index-match every audio marker to its emit-log frame_id(s), interpolate the video time
    // of that frame, and form a `video − audio` candidate offset. The true offset is the median of
    // the densest cluster of these candidates (false decodes + wrong-lap matches scatter and are
    // rejected). This is robust to the massive false-decode rate CRC-4 lets through on a music mix.
    let candidates = av_offset_candidates(&emit_log, &audio_markers, &ticks);
    let n_cand = candidates.len();

    let offset =
        cluster_offset_ms(&candidates, min_matched, cluster_tol_ms).with_context(|| {
            format!(
                "too few clustered A/V pairs to estimate (audio markers {n_audio}, video ticks \
             {video_ticks}, candidates {n_cand}, need {min_matched} within ±{cluster_tol_ms} ms) — \
             check the audio track index and the emit log"
            )
        })?;

    Ok(AvSyncReport {
        offset,
        fps,
        audio_markers: n_audio,
        video_ticks,
        emit_rows: emit_log.len(),
        candidates: n_cand,
    })
}
