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
    av_offset_candidates_deduped, cluster_offset_ms, decode_markers, decode_markers_with_stats,
    marker_coverage_gap_message, marker_coverage_overlaps_video_ticks, parse_ffprobe_start_time,
    parse_qpsk_marker_log, AudioParams, AvOffset, DEDUPE_SAME_FID_WINDOW_S,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    Ok(samples)
}

/// Measure the A/V-sync offset of `recording` (which carries BOTH the cam2 dual-QR video and the
/// mbc audio marker) using the cam2 `marker_log_csv` (the emitter's `index,frame_id,emit_ts_ns`).
///
/// Steps: ffprobe fps → decode every recorded frame's dual-QR tick → (tick, video_ts) samples;
/// extract audio track → QPSK-decode (audio_ts, index); form a candidate `video − audio` offset per
/// index-match, deduping near-simultaneous same-`frame_id` duplicates (`av_offset_candidates_deduped`,
/// #733); the offset is the median of the DENSEST cluster of those candidates (`cluster_offset_ms`)
/// — robust to false audio decodes and the index wrap.
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

    // #936 FAIL-CLOSED GUARD: the emit-log's frame_id coverage must overlap the recording's own
    // decoded video tick range, or REFUSE to measure — otherwise a marker thread that died before
    // (or after) this recording pairs decoded-audio-index-only against pure CRC-4 false decodes
    // and produces a plausible-looking but MEANINGLESS offset (live incident: av_offset_ms=-544.8
    // matched=41 mad_ms=12.0, clearing every existing plausibility threshold). Checked BEFORE the
    // (slow) audio decode below so a rejection fails fast. See
    // `qpsk_marker::marker_coverage_overlaps_video_ticks`'s doc comment for why a frame_id-range
    // check needs no wall-clock alignment between the two machines that produced these files.
    anyhow::ensure!(
        marker_coverage_overlaps_video_ticks(&emit_log, &ticks),
        "{}",
        marker_coverage_gap_message(&emit_log, &ticks)
    );

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
    // #733 — deduped: collapse near-simultaneous duplicate decodes of the SAME marker (a real-data
    // audit found 37-84ms-apart same-frame_id pairs, most plausibly an acoustic echo/reverb tail
    // off the mbc mastering chain the marker's audio rides) to one sample before clustering, so a
    // duplicate never inflates the matched count or skews the median/MAD.
    let candidates =
        av_offset_candidates_deduped(&emit_log, &audio_markers, &ticks, DEDUPE_SAME_FID_WINDOW_S);
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

/// #312 item 2 (PR A) — the shared ingredients a per-camera, per-`--switch-schedule`-window A/V
/// fusion needs from ONE recording: the emit log, the QPSK-decoded audio markers (rebased onto
/// the container origin, exactly like [`av_sync_from_recording`]'s `audio_markers`), the measured
/// video fps, and the video stream's `start_time`. Serializable so the #208 per-box
/// `--extract-partial stream` (the ONLY box that has both the audio marker track and the cam2
/// dual-QR video co-located) can carry it through the small partial JSON to the dev1 merge —
/// mirrors exactly how `RecordingPartial.colour` (#377) carries a per-recording computed summary
/// through the same JSON contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvMarkerInputs {
    /// Measured video frame rate (ffprobe `r_frame_rate` of the recording's first video stream).
    pub fps: f64,
    /// `start_time` (s) of the video stream — usually 0, carried so a non-zero mux edit list is
    /// never silently dropped (mirrors [`av_sync_from_recording`]'s `video_start`).
    pub video_start_s: f64,
    /// The cam2 emitter's parsed marker log: `(index, frame_id, emit_ts_ns)` rows.
    pub emit_log: Vec<(u8, u32, i64)>,
    /// QPSK-decoded audio markers: `(container-relative ts_s, index)` — the audio stream's
    /// `start_time` is ALREADY baked in (mirrors `av_sync_from_recording`'s `audio_markers`), so a
    /// caller pairs these directly against `(tick, video_ts)` samples built on the SAME container
    /// timeline (see `crate::av_window::window_ticks`).
    pub audio_markers: Vec<(f64, u8)>,
    /// #748: total QPSK preamble-onset count over the whole recording's audio
    /// ([`crate::qpsk_marker::DecodeStats::preamble_screens_passed`]). Zero means the demod never
    /// saw preamble energy (no/near-silent signal) — the discriminator the fused verdict uses to
    /// tell a silent mbc chain apart from a present-but-undecoded one when `candidates == 0`
    /// everywhere. `#[serde(default)]` so an older partial JSON (before this field existed)
    /// deserializes to 0 — the safe, loud default (treated as silent on an all-zero run).
    #[serde(default)]
    pub audio_preamble_screens_passed: u64,
}

/// Decode the [`AvMarkerInputs`] for `recording` — the exact SAME ffmpeg/ffprobe glue
/// [`av_sync_from_recording`] uses for its own front half (this function is a sibling, not a
/// replacement: `av_sync_from_recording`'s whole-recording `--av-sync` standalone mode is LEFT
/// COMPLETELY UNTOUCHED, per #312 item 2 PR A's explicit "zero regression risk" scope).
///
/// Deliberately stops SHORT of decoding the video track: the caller (the probe-gated fused/merge
/// glue in `bin/recording-verdict`) already has this recording's frames decoded — with `.tick`
/// per frame — from the zero-loss pass itself, so re-running `analyze_recording` here would be
/// pure waste. The caller builds its own per-window `(tick, video_ts)` samples from those
/// already-decoded frames (`crate::av_window::window_ticks`) and pairs them against the
/// `emit_log`/`audio_markers` this function returns via `qpsk_marker::av_offset_candidates`.
pub fn decode_av_marker_inputs(
    recording: &Path,
    marker_log_csv: &str,
    params: &AudioParams,
    audio_track: u32,
    threshold: f64,
) -> Result<AvMarkerInputs> {
    let fps = probe_video_fps(recording)?;
    let emit_log = parse_qpsk_marker_log(marker_log_csv);
    anyhow::ensure!(
        !emit_log.is_empty(),
        "emit log is empty (no markers logged)"
    );
    let video_start = probe_stream_start_time(recording, "v:0")?;
    let audio_start = probe_stream_start_time(recording, &format!("a:{audio_track}"))?;
    let audio = extract_audio_mono_f32(recording, audio_track, params.sample_rate)?;
    // #748: keep the decode STATS — `preamble_screens_passed` is the silent-vs-undecoded signal
    // the fused verdict emits (see `AvMarkerInputs::audio_preamble_screens_passed`). Same decode
    // as before, stats no longer discarded.
    let (decoded, stats) = decode_markers_with_stats(&audio, params, threshold);
    let audio_markers: Vec<(f64, u8)> = decoded
        .into_iter()
        .map(|(ts, idx)| (audio_start + ts, idx))
        .collect();
    Ok(AvMarkerInputs {
        fps,
        video_start_s: video_start,
        emit_log,
        audio_markers,
        audio_preamble_screens_passed: stats.preamble_screens_passed,
    })
}
