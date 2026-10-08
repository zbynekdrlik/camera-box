#![cfg(feature = "probe")]
//! Issue 1404 (the CI timeout of `tests/av_run_recording_1404.rs`): the `--av-sync` painter path
//! reads a cheap HEAD of the recording before its full decode
//! (`probe::recording::analyze_recording_head`, `src/av_sync_decode_plan.rs`). CI only (probe).
//! - a head of N frames holds exactly the recording's first N frames, in order;
//! - a head of 0 frames decodes nothing;
//! - every single real rig frame read with the head request carries both a cam2 tick and a rig
//!   node burn, so one rig frame anywhere in the head sends the recording on to the unchanged full
//!   decode (a head with no self-marked run, such as a QR-less pre-roll, never stops it either);
//! - the painter path stops a burn-free clip after its head, with the "no cam2 tick" reason.
//!
//! The measurement clip's frame f shows the 911016 dual-QR ticks 2f and 2f - 1 (frame 0 shows tick
//! 0 in both halves), and 911016 never becomes the cam2 tick.

use camera_box::av_sync_decode_plan::{
    painter_head_request, painter_head_verdict, PainterHead, PAINTER_HEAD_FRAMES,
    PAINTER_PATH_NODE_BURNS,
};
use camera_box::probe::av_sync_recording::av_sync_from_recording;
use camera_box::probe::recording::{
    analyze_recording_head, decode_recording_frame_with_grouped_burns_optical,
};
use camera_box::probe::recording_latency::MEASUREMENT_CLIP_RUN_ID;
use camera_box::qpsk_marker::AudioParams;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn fixture(parts: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    for part in parts {
        p.push(part);
    }
    p
}

fn clip() -> PathBuf {
    fixture(&["measurement-clip-1404", "clip-v1-4s.mp4"])
}

#[test]
fn a_head_decodes_exactly_the_first_frames_in_order_1404() {
    let request = painter_head_request();
    let head = analyze_recording_head(
        &clip(),
        3,
        &request.mandatory_burns,
        request.min_distinct_optical,
    )
    .expect("head of the clip");
    let indices: Vec<u64> = head.iter().map(|f| f.frame_index).collect();
    assert_eq!(indices, [0, 1, 2], "the first three frames, in order");
    for f in &head {
        // distinct ids: frame 0 shows tick 0 in both halves
        let ids: BTreeSet<u32> = f
            .payloads
            .iter()
            .filter(|p| p.run_id == MEASUREMENT_CLIP_RUN_ID)
            .map(|p| p.frame_id)
            .collect();
        let want: BTreeSet<u32> = match f.frame_index as u32 {
            0 => [0].into(),
            n => [2 * n - 1, 2 * n].into(),
        };
        assert_eq!(ids, want, "frame {}", f.frame_index);
        assert_eq!(f.tick, None, "911016 is never the cam2 tick");
    }
}

#[test]
fn a_zero_frame_head_decodes_nothing_1404() {
    let head = analyze_recording_head(&clip(), 0, &[], None).expect("empty head");
    assert!(head.is_empty(), "{head:?}");
}

/// Real 1080p rig frames (a stream recording with cam3 deployed, another stream run, a cam1 grab):
/// read alone with the head request, each one carries a cam2 tick AND a rig node burn, and either
/// signal by itself sends the painter path to the full decode. So one rig frame in the head is
/// enough, even when the optical or the burns are unreadable on every other head frame.
#[test]
fn every_real_rig_frame_alone_sends_the_painter_path_to_the_full_decode_1404() {
    let request = painter_head_request();
    for parts in [
        ["tear-781", "stream-1700989544-frame-8497-healthy.png"],
        ["tear-781", "stream-2099068429-frame-1399.png"],
        ["burn-unreadable", "cam1-frame-225.png"],
    ] {
        let path = fixture(&parts);
        let luma = image::open(&path)
            .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
            .to_luma8();
        let f = decode_recording_frame_with_grouped_burns_optical(
            0,
            luma,
            &request.mandatory_burns,
            &[],
            request.min_distinct_optical,
        );
        let runs: Vec<u32> = f.payloads.iter().map(|p| p.run_id).collect();
        let what = format!("{}: tick {:?}, runs {runs:?}", parts[1], f.tick);
        assert!(f.tick.is_some(), "a cam2 tick on {what}");
        assert!(
            runs.iter().any(|r| PAINTER_PATH_NODE_BURNS.contains(r)),
            "a rig node burn on {what}"
        );
        assert_eq!(
            painter_head_verdict(&[(f.tick, runs.clone())]),
            PainterHead::FullDecode,
            "{what}"
        );
        assert_eq!(
            painter_head_verdict(&[(None, runs.clone())]),
            PainterHead::FullDecode,
            "the burns alone: {what}"
        );
        assert_eq!(
            painter_head_verdict(&[(f.tick, vec![])]),
            PainterHead::FullDecode,
            "the tick alone: {what}"
        );
    }
}

/// The painter path on a burn-free clip shorter than the head (the clip's first 10 frames, stream
/// copied) stops after its head with the head's reason: before issue 1404 it decoded every frame
/// through the robust recovery and failed later on the coverage guard. ffmpeg's rawvideo output
/// pads such a cut to its container length (dev1's ffmpeg 6.1.1 reads 13 frames), so the count is
/// checked as a range; the head cap itself is pinned by the 3-frame head above.
#[test]
fn the_painter_path_stops_a_burn_free_clip_with_the_no_tick_reason_1404() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let cut = dir.path().join("clip-first-10-frames.mp4");
    let ff = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(clip())
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-frames:v",
            "10",
            "-c",
            "copy",
        ])
        .arg(&cut)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        ff.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&ff.stderr)
    );
    let markers = std::fs::read_to_string(fixture(&[
        "measurement-clip-1404",
        "clip-v1-4s.mp4.markers.csv",
    ]))
    .expect("marker log");
    // the recording-verdict --av-sync defaults: track 0, threshold 0.35, 4 matched, 25 ms cluster
    let err = av_sync_from_recording(
        &cut,
        &markers,
        &AudioParams::rig60(),
        0,
        0.35,
        4,
        25.0,
        None,
    )
    .expect_err("the painter path measures nothing on the clip");
    let msg = format!("{err:#}");
    assert!(msg.contains("no cam2 painter tick"), "{msg}");
    let frames: u64 = msg
        .split("on any of the first ")
        .nth(1)
        .and_then(|rest| rest.split(" frames").next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the head's frame count in: {msg}"));
    assert!(
        (10..=PAINTER_HEAD_FRAMES).contains(&frames),
        "the whole short cut is the head: {msg}"
    );
}
