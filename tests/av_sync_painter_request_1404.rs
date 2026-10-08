#![cfg(feature = "probe")]
//! Issue 1404 (ROZHODNUTÉ 6051603225): the `--av-sync` painter path asks its full decode for exactly
//! the node burns its head read (`av_sync_decode_plan::painter_full_request`). CI only (probe); the
//! pure plan is unit-tested Tier-0 in `src/av_sync_decode_plan.rs`.
//!
//! - **Burns off (the YouTube-leg windows of the 5.10 sessions).** The committed clip
//!   `tests/fixtures/youtube_leg_1404/base-rec-A-4s-540p.mp4` is the first 4 s of the re-made window
//!   A of the session-1 stream recording, scaled to 960x540 (libx264 crf 28) so the debug decode
//!   stays short. Every frame carries only the painter run and the aux pair. The compiled
//!   `recording-verdict --av-sync` decodes every frame of the full decode on the #207 fast path,
//!   and prints exactly the JSON that today's request (cam1/strih/stream mandatory, every frame
//!   robust) printed on the same clip: `base-rec-A-4s-540p.avsync.json`, written by the runner's
//!   release `av_sync_from_recording` before this change, with dev1's ffmpeg 6.1.1 and with CI's
//!   pinned ffmpeg N-126264 alike.
//! - **Burns on (real rig frames).** A stream frame with cam2 or cam3 deployed and a strih frame
//!   with cam1: read alone with the head request, each keeps the strih / stream burns it carries
//!   required and gets the camera group as the any-of group, so it now takes the fast path (today's
//!   request sends all three robust: none carries all of cam1, strih and stream) and reads the
//!   same cam2 tick.

use camera_box::av_sync_decode_plan::{
    av_decode_request, painter_full_request, painter_head_request,
};
use camera_box::probe::qr::{
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical, DecodePath,
};
use camera_box::probe::recording::{
    decode_recording_frame_with_grouped_burns_optical, NODE_BURN_RUN_IDS,
};
use camera_box::probe::recording_latency::{
    BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4, BURN_RUN_ID_CAM5,
    BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The camera capture burns in `NODE_BURN_RUN_IDS` order: the any-of group.
const CAMERAS: [u32; 7] = [
    BURN_RUN_ID_CAM1,
    BURN_RUN_ID_CAM2,
    BURN_RUN_ID_CAM3,
    BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5,
    BURN_RUN_ID_CAM6,
    BURN_RUN_ID_CAM7,
];

fn fixture(parts: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    for part in parts {
        p.push(part);
    }
    p
}

fn av_sync(clip: &Path, marker_log: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recording-verdict"))
        .arg("--av-sync")
        .arg(clip)
        .arg("--av-marker-log")
        .arg(marker_log)
        .output()
        .expect("spawn recording-verdict")
}

/// The JSON block `run_av_sync` prints (a line that is exactly `{` through its closing brace).
fn json_of(text: &str) -> serde_json::Value {
    let at = text
        .lines()
        .scan(0usize, |pos, line| {
            let start = *pos;
            *pos += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| line.trim_end() == "{")
        .map(|(start, _)| start)
        .unwrap_or_else(|| panic!("no JSON block in: {text}"));
    serde_json::Deserializer::from_str(&text[at..])
        .into_iter::<serde_json::Value>()
        .next()
        .expect("a JSON value")
        .expect("valid JSON")
}

/// The log text with its ANSI colour escapes removed.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The unsigned value of `key=` on `line`.
fn field(line: &str, key: &str) -> u64 {
    line.split(&format!("{key}="))
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{key}= in {line}"))
}

#[test]
fn a_burns_off_recording_decodes_on_the_fast_path_with_todays_result_1404() {
    let clip = fixture(&["youtube_leg_1404", "base-rec-A-4s-540p.mp4"]);
    let log = fixture(&["youtube_leg_1404", "base-rec-A-4s-540p.mp4.markers.csv"]);
    let out = av_sync(&clip, &log);
    let text = plain(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));
    assert!(out.status.success(), "--av-sync failed: {text}");

    let want: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture(&[
            "youtube_leg_1404",
            "base-rec-A-4s-540p.avsync.json",
        ]))
        .expect("the pinned result"),
    )
    .expect("pinned JSON");
    assert_eq!(json_of(&text), want, "today's result, unchanged: {text}");

    // the last analysis is the full decode (the head is read first)
    let start = text
        .lines()
        .rfind(|l| l.contains("recording analysis start"))
        .unwrap_or_else(|| panic!("an analysis start line in {text}"));
    assert!(
        start.contains("mandatory_burns=[] any_of_burns=[]") && start.contains("max_frames=None"),
        "the burns-off head read no node burn, so the full decode requires none: {start}"
    );
    let done = text
        .lines()
        .rfind(|l| l.contains("recording analysis complete"))
        .unwrap_or_else(|| panic!("an analysis complete line in {text}"));
    let fast = field(done, "this_analysis_fast");
    let robust = field(done, "this_analysis_robust");
    assert_eq!(fast + robust, 120, "the clip's 120 frames: {done}");
    assert!(
        fast * 100 >= 95 * (fast + robust),
        "at least 95 % of the full decode on the fast path: {done}"
    );
}

/// Real burns-on rig frames: the plan keeps every strih / stream burn the frame carries required,
/// the camera group is the any-of group, and the frame now takes the fast path with the same tick.
#[test]
fn a_burns_on_rig_frame_keeps_its_hop_burns_required_1404() {
    let head = painter_head_request();
    let today = av_decode_request(None);
    for (parts, hops) in [
        (
            ["tear-781", "stream-1700989544-frame-8497-healthy.png"],
            vec![BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        ),
        (
            ["tear-781", "stream-2099068429-frame-1399.png"],
            vec![BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        ),
        (
            ["burn-unreadable", "cam1-frame-225.png"],
            vec![BURN_RUN_ID_STRIH],
        ),
    ] {
        let path = fixture(&parts);
        let luma = image::open(&path)
            .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
            .to_luma8();
        let seen = decode_recording_frame_with_grouped_burns_optical(
            0,
            luma.clone(),
            &head.mandatory_burns,
            &head.any_of_burns,
            head.min_distinct_optical,
        );
        let runs: Vec<u32> = seen.payloads.iter().map(|p| p.run_id).collect();
        let what = format!("{}: tick {:?}, runs {runs:?}", parts[1], seen.tick);
        let plan = painter_full_request(&[(seen.tick, runs.clone())], &NODE_BURN_RUN_IDS);
        assert_eq!(
            plan.mandatory_burns, hops,
            "the hop burns it carries: {what}"
        );
        assert_eq!(plan.any_of_burns, CAMERAS, "the camera group: {what}");
        assert_eq!(plan.min_distinct_optical, None, "{what}");

        // One decode per request (a robust 1080p decode is the expensive part of a CI debug run).
        let decode = |req: &camera_box::av_sync_decode_plan::AvDecodeRequest| {
            decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
                luma.clone(),
                &req.mandatory_burns,
                &req.any_of_burns,
                req.min_distinct_optical,
            )
        };
        let (plan_payloads, plan_path) = decode(&plan);
        let (today_payloads, today_path) = decode(&today);
        assert_eq!(plan_path, DecodePath::Fast, "the plan's request: {what}");
        assert_eq!(
            today_path,
            DecodePath::Robust,
            "today's request (none carries cam1, strih and stream): {what}"
        );
        assert_eq!(
            cam2_tick(&plan_payloads),
            cam2_tick(&today_payloads),
            "the same cam2 tick: {what}"
        );
        assert_eq!(cam2_tick(&plan_payloads), seen.tick, "{what}");
        assert!(seen.tick.is_some(), "{what}");
    }
}

/// The cam2 tick of a frame's payloads, derived like `RecordingFrame::tick`: the newest `frame_id`
/// of a QR that is not a reserved node-burn id.
fn cam2_tick(payloads: &[camera_box::probe::payload::Payload]) -> Option<u32> {
    payloads
        .iter()
        .filter(|p| !NODE_BURN_RUN_IDS.contains(&p.run_id))
        .map(|p| p.frame_id)
        .max()
}
