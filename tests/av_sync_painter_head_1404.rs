#![cfg(feature = "probe")]
//! Issue 1404 (the CI timeout of `tests/av_run_recording_1404.rs`): the `--av-sync` painter path
//! reads a cheap HEAD of the recording before its full decode
//! (`probe::recording::analyze_recording_head`, `src/av_sync_decode_plan.rs`). CI only (probe).
//! - a head of N frames decodes exactly the recording's first N frames, in order, and stops there;
//! - a head of 0 frames decodes nothing.
//!
//! The measurement clip's frame f shows the 911016 dual-QR ticks 2f and 2f - 1 (frame 0 shows tick
//! 0 in both halves), and 911016 never becomes the cam2 tick.

use camera_box::av_sync_decode_plan::painter_head_request;
use camera_box::probe::recording::analyze_recording_head;
use camera_box::probe::recording_latency::MEASUREMENT_CLIP_RUN_ID;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn clip() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "measurement-clip-1404",
        "clip-v1-4s.mp4",
    ]
    .iter()
    .collect()
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
