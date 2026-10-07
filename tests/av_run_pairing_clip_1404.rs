//! Issue 1404 Task 5 part b — the A/V of the measurement clip through its OWN tick, on the committed
//! fixture cut from the generated clip (`tests/fixtures/measurement-clip-1404/clip-v1-4s.mp4`, 4 s,
//! `scripts/gen_measurement_clip.py`), with no probe build (Tier-0, default features).
//!
//! The picture side is the clip's run-scoped tick map, decoded from that fixture by the YouTube-leg
//! tick decoder (`clip-v1-4s.ticks.tsv`, pinned to a fresh decode by
//! `tests/python/test_youtube_leg_runs_1404.py`): each frame's two halves become `(run, frame_id)`
//! payloads for `av_run_pairing::run_frame_tick`. The sound side is the fixture's REAL audio
//! (ffmpeg, every channel kept), decoded by the crate-root demod with the probe glue's own channel
//! pick. The pairing is the same sequence of crate-root calls as
//! `probe::av_sync_recording::av_sync_from_recording` with `av_run = Some(911016)`. Results:
//! - the clean clip reads 0 within one frame;
//! - a copy whose audio comes 100 ms EARLY (the picture lags the sound) reads +100 ± 17 ms;
//! - a copy whose audio comes 100 ms LATE reads -100 ± 17 ms;
//! - another run (the painter's, a burn origin) finds no tick on the clip and measures nothing.
//!
//! The probe glue itself (the rqrr decode of the same fixture through `recording-verdict --av-sync
//! --av-run 911016`) is covered in CI by `tests/av_run_recording_1404.rs`. Needs ffmpeg/ffprobe.

use camera_box::av_run_pairing::{
    check_av_run, run_frame_tick, run_tick_samples, SELF_MARKED_RUN_IDS,
};
use camera_box::qpsk_channel_select::{
    decode_best_channel, f32le_to_channels, ffmpeg_extract_args, ffprobe_channels_args,
    parse_ffprobe_channels,
};
use camera_box::qpsk_marker::{
    av_offset_candidates_deduped, cluster_offset_ms, marker_coverage_overlaps_video_ticks,
    parse_ffprobe_start_time, parse_qpsk_marker_log, AudioParams, AvOffset,
    DEDUPE_SAME_FID_WINDOW_S,
};
use camera_box::qpsk_probe_decision::{ClusterParams, DEFAULT_MIN_CLUSTERS};
use std::path::{Path, PathBuf};
use std::process::Command;

const CLIP_RUN: u32 = 911_016;
const FPS: f64 = 30.0;
/// `recording-verdict --av-sync` defaults: threshold, min matched, cluster half-width.
const THRESHOLD: f64 = 0.35;
const MIN_MATCHED: usize = 4;
const CLUSTER_TOL_MS: f64 = 25.0;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/measurement-clip-1404")
        .join(name)
}

fn ffprobe(args: &[String], path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(args)
        .arg(path)
        .output()
        .expect("spawn ffprobe (install ffmpeg)");
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn start_time(path: &Path, selector: &str) -> f64 {
    let args: Vec<String> = [
        "-v",
        "error",
        "-select_streams",
        selector,
        "-show_entries",
        "stream=start_time",
        "-of",
        "default=nw=1:nk=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    parse_ffprobe_start_time(ffprobe(&args, path).lines().next().unwrap_or(""))
}

/// The fixture's audio track, every channel, through `audio_filter` (None = as recorded).
fn audio_channels(path: &Path, audio_filter: Option<&str>) -> Vec<Vec<f32>> {
    let channels = parse_ffprobe_channels(&ffprobe(&ffprobe_channels_args(0), path))
        .expect("the clip's channel count");
    assert_eq!(channels, 2, "the clip is stereo, L == R");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]).arg(path);
    if let Some(f) = audio_filter {
        cmd.args(["-af", f]);
    }
    let out = cmd
        .args(ffmpeg_extract_args(
            0,
            AudioParams::rig60().sample_rate,
            channels,
        ))
        .output()
        .expect("spawn ffmpeg (install ffmpeg)");
    assert!(
        out.status.success(),
        "ffmpeg audio extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    f32le_to_channels(&out.stdout, channels).expect("whole f32 frames")
}

/// `(frame_index, [(run, frame_id)])` of every fixture frame, from its run-scoped tick map.
fn frame_payloads() -> Vec<(u64, Vec<(u32, u32)>)> {
    let text = std::fs::read_to_string(fixture("clip-v1-4s.ticks.tsv")).expect("the tick map");
    let mut out = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let c: Vec<&str> = line.split('\t').collect();
        assert_eq!(c.len(), 7, "a run-scoped tick map row: {line:?}");
        let mut payloads = Vec::new();
        if let Ok(run) = c[6].parse::<u32>() {
            for half in [c[4], c[5]] {
                if let Ok(frame_id) = half.parse::<u32>() {
                    payloads.push((run, frame_id));
                }
            }
        }
        out.push((c[0].parse().expect("frame index"), payloads));
    }
    out
}

/// The A/V offset of the fixture paired through `run`'s own tick, the audio through `audio_filter`.
fn measure(run: u32, audio_filter: Option<&str>) -> Option<AvOffset> {
    let clip = fixture("clip-v1-4s.mp4");
    let emit_log = parse_qpsk_marker_log(
        &std::fs::read_to_string(fixture("clip-v1-4s.mp4.markers.csv")).expect("the marker log"),
    );
    assert_eq!(emit_log.len(), 7, "markers at 0.5 .. 3.5 s");
    let frames: Vec<(u64, Option<u32>)> = frame_payloads()
        .into_iter()
        .map(|(i, p)| (i, run_frame_tick(p, run)))
        .collect();
    let ticks =
        run_tick_samples(&frames, FPS, start_time(&clip, "v:0")).expect("one play of the clip");
    if !marker_coverage_overlaps_video_ticks(&emit_log, &ticks) {
        return None;
    }
    let best = decode_best_channel(
        &audio_channels(&clip, audio_filter),
        &AudioParams::rig60(),
        THRESHOLD,
        ClusterParams::default(),
        DEFAULT_MIN_CLUSTERS,
    )
    .expect("a channel");
    let audio_start = start_time(&clip, "a:0");
    let audio: Vec<(f64, u8)> = best
        .markers
        .iter()
        .map(|&(ts, idx)| (audio_start + ts, idx))
        .collect();
    let candidates =
        av_offset_candidates_deduped(&emit_log, &audio, &ticks, DEDUPE_SAME_FID_WINDOW_S);
    cluster_offset_ms(&candidates, MIN_MATCHED, CLUSTER_TOL_MS)
}

#[test]
fn the_clip_is_the_only_self_marked_run_1404() {
    assert_eq!(SELF_MARKED_RUN_IDS, [CLIP_RUN]);
    assert!(check_av_run(CLIP_RUN).is_ok());
    // the Rust reserved id it mirrors
    let rl = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/probe/recording_latency.rs"),
    )
    .expect("recording_latency.rs");
    assert!(rl.contains("pub const MEASUREMENT_CLIP_RUN_ID: u32 = 911016;"));
}

#[test]
fn every_fixture_frame_shows_the_clips_tick_2f_1404() {
    let frames = frame_payloads();
    assert_eq!(frames.len(), 120);
    for (i, payloads) in frames {
        assert_eq!(
            run_frame_tick(payloads, CLIP_RUN),
            Some(2 * i as u32),
            "frame {i}"
        );
    }
}

#[test]
fn the_clean_clip_reads_zero_within_one_frame_1404() {
    let off = measure(CLIP_RUN, None).expect("the clip pairs through its own tick");
    assert!(
        off.offset_ms.abs() <= 1000.0 / FPS,
        "the clean clip's A/V must read 0 within one frame, got {off:?}"
    );
    assert!(
        off.matched >= 6,
        "nearly every one of the 7 markers pairs: {off:?}"
    );
}

#[test]
fn audio_100_ms_early_reads_plus_100_1404() {
    // the sound 100 ms ahead of the picture: the picture LAGS the sound, video - audio = +100 ms
    let off = measure(CLIP_RUN, Some("atrim=start=0.1,asetpts=PTS-STARTPTS"))
        .expect("the shifted clip still pairs");
    assert!(
        (off.offset_ms - 100.0).abs() <= 17.0,
        "audio 100 ms early must read +100 +/- 17 ms, got {off:?}"
    );
}

#[test]
fn audio_100_ms_late_reads_minus_100_1404() {
    let off = measure(CLIP_RUN, Some("adelay=100:all=1")).expect("the shifted clip still pairs");
    assert!(
        (off.offset_ms + 100.0).abs() <= 17.0,
        "audio 100 ms late must read -100 +/- 17 ms, got {off:?}"
    );
}

#[test]
fn another_run_finds_no_tick_on_the_clip_and_measures_nothing_1404() {
    for run in [123_456_789u32, 911_014, 911_015] {
        assert_eq!(
            measure(run, None),
            None,
            "run {run} has no tick on the clip"
        );
    }
}
