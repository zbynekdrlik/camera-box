//! issue 1367 (strih burn id jump) -- the verdict contract behind the 8 h soak's one-per-window
//! strih `real_drop`, pinned on REAL frames through the REAL `recording-verdict --merge-partials`
//! process.
//!
//! The fixture is the stream partial of window slot-005 of the 8 h run of 28.9.2026
//! (`~/.camera-box/av-soak/8h-20260928T0147Z` on dev1), trimmed to recorded frames 150..=190, with
//! that window's own switch schedule. The strih measurement burn (run 911002) is one burn filter
//! PER strih camera input (`vendor/distroav/src/ndi-burn-filter.cpp`), each with its own
//! `frame_id` counter. The soak started that recording while the strih program still showed the
//! previous window's last sweep camera, so frames 150..=159 carry that input's counter
//! (298341..=298350), and at frame 160 -- the first sweep cut, which opens schedule window 0 --
//! the ids continue on Cam 1's counter (87926, 87927, ...). The frames before window 0 belong to
//! no window, so the verdict charges the backward counter change there as one REAL DROP (by
//! design: a crossing is excused only between two KNOWN windows). The fix is in the soak
//! (`scripts/av-soak.sh` cuts to the first sweep scene before StartRecord, as the E2E's [4/8]
//! does); the verdict stays strict.
//!
//! What this file pins, all on the same real frames:
//! - as recorded (another input before window 0): exactly one real drop, id 87926, frame 160;
//! - as the fixed soak records it (the pre-window frames on the first sweep input's counter): none;
//! - a backward jump INSIDE window 0 is still a real drop;
//! - a backward jump on the SAME counter exactly at the first cut is still a real drop.
//!
//! `recording-verdict` is `required-features = ["probe"]`, so this file runs under
//! `--features probe` only (CI).

#![cfg(feature = "probe")]

use camera_box::probe::recording_latency::BURN_RUN_ID_STRIH;
use camera_box::probe::recording_partial::RecordingPartial;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The permanent cam2 painter's QR run id in that window (its `frame-probe start:` journal line).
const PAINTER_RUN_ID: &str = "1790548508";
/// The first recorded frame of schedule window 0 (the first sweep cut) in the fixture.
const FIRST_CUT_FRAME: u64 = 160;
/// A frame well inside window 0.
const INSIDE_WINDOW0_FRAME: u64 = 180;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/soak-first-cut-1367")
        .join(name)
}

fn window0_start_ns() -> i64 {
    let text = std::fs::read_to_string(fixture("switch-schedule-slot005.json")).unwrap();
    let schedule: serde_json::Value = serde_json::from_str(&text).unwrap();
    schedule[0]["start_ns"].as_i64().unwrap()
}

fn recorded() -> RecordingPartial {
    RecordingPartial::load(&fixture("stream-partial-slot005.json")).unwrap()
}

/// The fixture as the fixed soak records it: the recording starts on the first sweep scene, so the
/// frames before window 0 carry the first sweep input's own counter, which then simply continues
/// into window 0. Shifts the pre-window strih ids so their last one is window 0's first id - 1.
fn on_first_sweep_input(mut p: RecordingPartial) -> RecordingPartial {
    let w0 = window0_start_ns();
    let strih = |f: &camera_box::probe::recording::RecordingFrame| {
        f.payloads
            .iter()
            .filter(|pl| pl.run_id == BURN_RUN_ID_STRIH)
            .map(|pl| (pl.frame_id, pl.gen_ts_ns))
            .collect::<Vec<_>>()
    };
    let pre_last = p
        .frames
        .iter()
        .flat_map(strih)
        .filter(|&(_, ts)| ts < w0)
        .map(|(id, _)| i64::from(id))
        .max()
        .expect("the fixture has strih frames before window 0");
    let first_post = p
        .frames
        .iter()
        .flat_map(strih)
        .filter(|&(_, ts)| ts >= w0)
        .map(|(id, _)| i64::from(id))
        .min()
        .expect("the fixture has strih frames in window 0");
    let shift = first_post - 1 - pre_last;
    for f in &mut p.frames {
        for pl in &mut f.payloads {
            if pl.run_id == BURN_RUN_ID_STRIH && pl.gen_ts_ns < w0 {
                pl.frame_id = u32::try_from(i64::from(pl.frame_id) + shift).unwrap();
            }
        }
    }
    p
}

/// Moves the strih id of recorded frame `frame_index` back by `by`.
fn step_back(mut p: RecordingPartial, frame_index: u64, by: u32) -> RecordingPartial {
    let f = p
        .frames
        .iter_mut()
        .find(|f| f.frame_index == frame_index)
        .expect("the frame is in the fixture");
    let mut hit = false;
    for pl in &mut f.payloads {
        if pl.run_id == BURN_RUN_ID_STRIH {
            pl.frame_id -= by;
            hit = true;
        }
    }
    assert!(hit, "frame {frame_index} carries a strih burn");
    p
}

/// Merge `partial` as the stream partial with the window's own schedule and the soak's fps and
/// painter arguments (`scripts/lib/av-soak.sh` `av_soak_merge_argv`, without the strih partial and
/// `--offline-ack-cams`: the strih hop's loss is read from the stream recording), and return
/// `full_chain.loss.strih`.
fn strih_loss(tag: &str, partial: &RecordingPartial) -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let partial_path = dir.join("stream-partial.json");
    partial.save(&partial_path).unwrap();
    let json_path = dir.join("verdict.json");
    let pixel_dir = dir.join("pixel-proof");
    let schedule = fixture("switch-schedule-slot005.json");
    let stream_spec = format!("stream={}", partial_path.display());
    let out = Command::new(env!("CARGO_BIN_EXE_recording-verdict"))
        .args([
            "--merge-partials",
            stream_spec.as_str(),
            "--min-secs",
            "0",
            "--capture-fps",
            "30",
            "--strih-emit-fps",
            "30",
            "--stream-capture-fps",
            "30",
            "--cam2-run-id",
            PAINTER_RUN_ID,
            "--switch-schedule",
            schedule.to_str().unwrap(),
            "--out-dir",
            pixel_dir.to_str().unwrap(),
            "--json",
            json_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn recording-verdict --merge-partials");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(json_path)
        .unwrap_or_else(|e| panic!("{tag}: verdict JSON not written ({e}): {text}"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["full_chain"]["loss"]["strih"].clone()
}

fn missing_ids(loss: &serde_json::Value) -> Vec<u64> {
    loss["missing_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect()
}

fn classified(loss: &serde_json::Value) -> Vec<(u64, u64, String)> {
    loss["classified"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_u64().unwrap(),
                c["frame_index"].as_u64().unwrap(),
                c["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn the_fixture_is_the_real_first_cut_1367() {
    let p = recorded();
    let w0 = window0_start_ns();
    let before: Vec<u32> = p
        .frames
        .iter()
        .flat_map(|f| f.payloads.iter())
        .filter(|pl| pl.run_id == BURN_RUN_ID_STRIH && pl.gen_ts_ns < w0)
        .map(|pl| pl.frame_id)
        .collect();
    let first_cut = p
        .frames
        .iter()
        .find(|f| f.frame_index == FIRST_CUT_FRAME)
        .unwrap();
    let first_cut_id = first_cut
        .payloads
        .iter()
        .find(|pl| pl.run_id == BURN_RUN_ID_STRIH)
        .unwrap();
    assert_eq!(before.first(), Some(&298_341));
    assert_eq!(before.last(), Some(&298_350));
    assert!(
        first_cut_id.gen_ts_ns >= w0,
        "frame 160 is window 0's first frame"
    );
    assert_eq!(first_cut_id.frame_id, 87_926);
}

#[test]
fn a_recording_started_on_another_input_reads_one_real_drop_at_the_first_cut_1367() {
    let loss = strih_loss("recorded", &recorded());
    assert_eq!(missing_ids(&loss), [87_926], "{loss}");
    assert_eq!(
        classified(&loss),
        [(87_926, FIRST_CUT_FRAME, "real_drop".to_string())],
        "{loss}"
    );
    assert_eq!(loss["zero_loss"], serde_json::json!(false), "{loss}");
}

#[test]
fn a_recording_started_on_the_first_sweep_input_reads_no_drop_1367() {
    let loss = strih_loss("fixed", &on_first_sweep_input(recorded()));
    assert!(missing_ids(&loss).is_empty(), "{loss}");
    assert_eq!(loss["real_drops"], serde_json::json!(0), "{loss}");
    assert_eq!(loss["zero_loss"], serde_json::json!(true), "{loss}");
}

#[test]
fn a_backward_jump_inside_window0_is_still_a_real_drop_1367() {
    let p = step_back(on_first_sweep_input(recorded()), INSIDE_WINDOW0_FRAME, 5);
    let loss = strih_loss("back-inside", &p);
    assert_eq!(
        classified(&loss),
        [(87_941, INSIDE_WINDOW0_FRAME, "real_drop".to_string())],
        "{loss}"
    );
}

#[test]
fn a_same_counter_backward_jump_at_the_first_cut_is_still_a_real_drop_1367() {
    // the verdict stays strict at window 0's start: the pre-window frames sit in no schedule
    // window, so a counter going backward there is never excused as a program cut
    let p = step_back(on_first_sweep_input(recorded()), FIRST_CUT_FRAME, 3);
    let loss = strih_loss("back-at-cut", &p);
    assert_eq!(
        classified(&loss),
        [(87_923, FIRST_CUT_FRAME, "real_drop".to_string())],
        "{loss}"
    );
}
