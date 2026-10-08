#![cfg(feature = "probe")]
//! Issue 1404 (the CI timeout of `tests/av_run_recording_1404.rs`): the `--av-sync` decode request
//! on the measurement clip, through the REAL per-frame recording decode. CI only (probe); the pure
//! plan is unit-tested Tier-0 in `src/av_sync_decode_plan.rs`.
//! - `--av-run 911016` asks for what the clip carries, so a clip frame takes the #207 FAST path;
//! - the painter path's full-decode set (cam1/strih/stream) can never be met by the clip, so the
//!   same frame goes ROBUST: the issue-423 class that ran three CI tests into nextest's 480 s kill;
//! - the painter head asks for nothing, so a clip frame stays on the fast path there too;
//! - the painter set is the `recording_latency` cam1/strih/stream ids.

use camera_box::av_sync_decode_plan::{
    av_decode_request, painter_head_request, AvDecodeRequest, PAINTER_PATH_NODE_BURNS,
};
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical, DecodePath,
};
use camera_box::probe::recording_latency::{
    BURN_RUN_ID_CAM1, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH, MEASUREMENT_CLIP_RUN_ID,
};
use image::GrayImage;
use std::path::PathBuf;

/// Frame 1801 of the generated clip (both halves: ticks 3602 and 3601).
fn clip_frame() -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "measurement-clip-1404",
        "clip-v1-frame-1801.png",
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

fn decode(req: &AvDecodeRequest) -> (Vec<Payload>, DecodePath) {
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        clip_frame(),
        &req.mandatory_burns,
        &[],
        req.min_distinct_optical,
    )
}

fn clip_ids(payloads: &[Payload]) -> Vec<u32> {
    let mut ids: Vec<u32> = payloads
        .iter()
        .filter(|p| p.run_id == MEASUREMENT_CLIP_RUN_ID)
        .map(|p| p.frame_id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn av_run_takes_the_fast_path_on_a_clip_frame_1404() {
    let (payloads, path) = decode(&av_decode_request(Some(MEASUREMENT_CLIP_RUN_ID)));
    assert_eq!(
        path,
        DecodePath::Fast,
        "--av-run must ask only for what the clip carries: {payloads:?}"
    );
    assert_eq!(clip_ids(&payloads), [3601, 3602]);
}

#[test]
fn the_painter_request_sends_a_clip_frame_robust_1404() {
    let (payloads, path) = decode(&av_decode_request(None));
    assert_eq!(
        path,
        DecodePath::Robust,
        "the clip carries no cam1/strih/stream burn, so the painter set can never be met"
    );
    assert_eq!(clip_ids(&payloads), [3601, 3602]);
}

#[test]
fn the_painter_head_takes_the_fast_path_on_a_clip_frame_1404() {
    let (payloads, path) = decode(&painter_head_request());
    assert_eq!(path, DecodePath::Fast, "{payloads:?}");
    assert_eq!(clip_ids(&payloads), [3601, 3602]);
}

#[test]
fn the_painter_set_is_the_recording_latency_cam1_strih_stream_1404() {
    assert_eq!(
        PAINTER_PATH_NODE_BURNS,
        [BURN_RUN_ID_CAM1, BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM]
    );
}
