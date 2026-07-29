//! #703 — proves the REAL `recording-verdict --merge-partials` CLI PROCESS actually EXITS
//! non-zero when the cross-camera delivery-latency-spread gate (#624, the `all_cambox_latency`
//! block — NOT the #286 `all_cambox_delivery_latency` one, which is report-only) fails — not
//! just that the in-process `build_and_print_verdict()` function RETURNS `all_pass=false`
//! (already covered, in-process, by
//! `all_cambox_latency_measures_per_camera_windowed_latency_and_fails_a_wide_spread_624` in
//! `src/bin/recording-verdict.rs`'s own `#[cfg(test)] mod tests`).
//!
//! **#861 (2026-07-29, user decision on #856): the A/V-offset gate (#641/#624 deliverable 4) is
//! now report-only** — a failing/Unknown A/V-sync verdict no longer makes this process exit
//! non-zero by itself (mirrors the in-process
//! `all_cambox_av_sync_gate_failure_no_longer_forces_the_overall_verdict_to_fail_861` /
//! `all_cambox_av_sync_measures_a_real_camera_and_fails_closed_on_the_rest_312` in
//! `src/bin/recording-verdict.rs`). The two AV-specific tests below were REWRITTEN from proving
//! "AV failure ⇒ nonzero exit" to proving the opposite invariant through the SAME real subprocess
//! path — this is the pair #861 (re-arm after ASRC, #803) will need to invert back.
//!
//! WHY A SEPARATE SUBPROCESS TEST: `run_merge()` (`src/bin/recording-verdict.rs`) calls
//! `std::process::exit(1)` directly on a failing verdict — an IN-PROCESS `#[test]` that fed it
//! failing data would kill the TEST BINARY itself (`run_merge_accepts_an_imag_partial_461`'s own
//! doc comment says exactly this, which is why every existing `run_merge()` test only ever
//! exercises the PASSING case in-process). #703's root cause was that the REQUIRED CI merge gate
//! went GREEN without the verdict's exit code ever being computed at all — so what actually needs
//! proving here is the PROCESS EXIT CODE, reached through the REAL CLI argument-parsing + partial
//! JSON (de)serialization path, exactly as `scripts/recording-e2e.sh`'s `[8/8d]` merge step
//! invokes it in production. Spawning the compiled binary
//! (`CARGO_BIN_EXE_recording-verdict`, the same pattern `tests/harness_av_restart_sync_gate.rs`
//! already uses for a smaller gate binary) is the only way to observe that without killing the
//! test runner.
//!
//! `recording-verdict` is `required-features = ["probe"]` — this whole file compiles/runs ONLY
//! under `--features probe` (CI-only, per this repo's Tier-0 local-build policy in CLAUDE.md;
//! cannot be compile-verified locally).

#![cfg(feature = "probe")]

use camera_box::probe::av_sync_recording::AvMarkerInputs;
use camera_box::probe::payload::Payload;
use camera_box::probe::recording::RecordingFrame;
use camera_box::probe::recording_latency::{
    BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM3, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_partial::RecordingPartial;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A non-burn run_id for cam2's OPTICAL dual-QR payload — mirrors the `const CAM2: u32 = 7;`
/// convention `src/bin/recording-verdict.rs`'s own in-process fixtures already use (any value
/// outside the reserved node-burn ids works; `NODE_BURN_RUN_IDS` is what excludes burns from
/// `RecordingFrame::tick`, not this specific value).
const CAM2_RUN_ID: u32 = 7;
const ONE_S_NS: i64 = 1_000_000_000;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cb-703-merge-gate-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build the `--switch-schedule` JSON: one `window_ns`-wide window per `labels` entry, back to
/// back starting at `base_ns`. Mirrors `build_all_cambox_av_sync_fixture`'s /
/// `build_all_cambox_latency_fixture`'s schedule shape (both in `src/bin/recording-verdict.rs`'s
/// own `#[cfg(test)] mod tests`) exactly.
fn write_switch_schedule(dir: &Path, labels: &[&str], base_ns: i64, window_ns: i64) -> PathBuf {
    let windows: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(wi, label)| {
            let start_ns = base_ns + (wi as i64) * window_ns;
            let end_ns = start_ns + window_ns;
            format!(r#"{{"cambox":"{label}","start_ns":{start_ns},"end_ns":{end_ns}}}"#)
        })
        .collect();
    let path = dir.join("switch-schedule.json");
    std::fs::write(&path, format!("[{}]", windows.join(","))).unwrap();
    path
}

/// One window's worth of stream frames (20 frames, 0.2s apart): each carries the strih
/// render-tick burn + cam2's optical tick, and (when `camera_burn` is `Some`) a
/// camera-under-test capture burn offset by `latency_ns` from cam2's paint. `start_idx` is the
/// running frame index so multiple windows concatenate into one monotone optical sequence — the
/// SAME per-frame payload shape `build_all_cambox_av_sync_fixture`/
/// `build_all_cambox_latency_fixture` build.
fn window_frames(
    start_idx: u64,
    wstart_ns: i64,
    camera_burn: Option<(u32, i64)>,
) -> Vec<RecordingFrame> {
    (0..20i64)
        .map(|j| {
            let idx = start_idx + j as u64;
            let paint_ts = wstart_ns + (j + 1) * (ONE_S_NS / 5);
            let optical = 1000u32 + idx as u32;
            let mut payloads = vec![
                Payload {
                    run_id: BURN_RUN_ID_STRIH,
                    frame_id: 1670 + idx as u32,
                    gen_ts_ns: paint_ts,
                },
                Payload {
                    run_id: CAM2_RUN_ID,
                    frame_id: optical,
                    gen_ts_ns: paint_ts,
                },
            ];
            if let Some((burn_id, latency_ns)) = camera_burn {
                payloads.push(Payload {
                    run_id: burn_id,
                    frame_id: 5000 + idx as u32,
                    gen_ts_ns: paint_ts + latency_ns,
                });
            }
            RecordingFrame {
                frame_index: idx,
                payloads,
                tick: Some(optical),
            }
        })
        .collect()
}

/// Write a "stream" `RecordingPartial` JSON (the ONLY box `av_sync` is ever carried on, #312
/// item 2 PR A) to `dir/stream-partial.json` and return its path. `av_sync` is `Some` to
/// exercise the AV gate, `None` to exercise the delivery-latency-spread gate alone.
fn write_stream_partial(
    dir: &Path,
    frames: Vec<RecordingFrame>,
    av_sync: Option<AvMarkerInputs>,
) -> PathBuf {
    let path = dir.join("stream-partial.json");
    let rec_path = PathBuf::from("stream-REC.mp4");
    let expected_burns = [BURN_RUN_ID_CAM1, BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM];
    let partial = RecordingPartial::from_frames("stream", &rec_path, &expected_burns, frames)
        .with_av_sync(av_sync);
    partial.save(&path).unwrap();
    path
}

/// Spawn the COMPILED `recording-verdict` binary with `--merge-partials stream=<partial>` + the
/// given extra args; returns `(exit_code, combined stdout+stderr, the --json body once parsed)`.
fn run_merge_subprocess(
    stream_partial: &Path,
    json_path: &Path,
    extra_args: &[&str],
) -> (i32, String, Option<serde_json::Value>) {
    let bin = env!("CARGO_BIN_EXE_recording-verdict");
    let spec = format!("stream={}", stream_partial.display());
    let mut args: Vec<String> = vec![
        "--merge-partials".to_string(),
        spec,
        "--json".to_string(),
        json_path.display().to_string(),
    ];
    args.extend(extra_args.iter().map(|s| s.to_string()));
    let out = Command::new(bin)
        .args(&args)
        .output()
        .expect("spawn recording-verdict --merge-partials");
    let code = out.status.code().expect("recording-verdict exit code");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(json_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    (code, text, json)
}

/// #861 (2026-07-29 user decision on #856) — a REAL, cleanly-measured av-offset (500ms) far
/// outside the default `--av-expected-ms 0` +/-20ms tolerance must NOT change the
/// `--merge-partials` PROCESS exit code: the AV-offset gate (#641/#624 deliverable 4) is now
/// report-only, so a failing measurement here is a no-op on the merge's overall verdict — proven
/// through the REAL subprocess by comparing the exit code + `overall_pass` of a WITH-av run
/// against an otherwise-identical WITHOUT-av run. **This SUPERSEDES the old
/// `merge_subprocess_exits_nonzero_when_av_offset_gate_fails_703`**, which asserted the opposite
/// when the gate was still wired into `all_pass`; mirrors the in-process fixture
/// `all_cambox_av_sync_gate_failure_no_longer_forces_the_overall_verdict_to_fail_861` (10
/// candidates, all at exactly 500ms — same data shape) through the real CLI end-to-end. The JSON
/// still shows the AV gate ITSELF failing (`gate_pass: false`) and unambiguously report-only
/// (`gates_overall_pass: false`) — it just no longer decides the exit code.
#[test]
fn merge_subprocess_exit_code_unaffected_by_av_offset_gate_failure_861() {
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;

    // 10 candidates: video_ts(1000+k) - audio_ts(k/30 - 0.5) = 0.5s = 500ms constant offset,
    // matching the emit-log frame_ids (1000..1010) the CAM1 window's optical ticks carry.
    let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
    let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0 - 0.5, k)).collect();
    let av = AvMarkerInputs {
        fps: 30.0,
        video_start_s: 0.0,
        emit_log,
        audio_markers,
    };

    let run = |tag: &str, av: Option<AvMarkerInputs>| {
        let dir = tempdir(tag);
        let frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
        let sched = write_switch_schedule(&dir, &["CAM1"], base, win);
        let stream_partial = write_stream_partial(&dir, frames, av);
        let json_path = dir.join("verdict.json");
        let pixel_proof_dir = dir.join("pixel-proof");
        run_merge_subprocess(
            &stream_partial,
            &json_path,
            &[
                "--switch-schedule",
                sched.to_str().unwrap(),
                "--switch-guard-ns",
                "0",
                "--switch-expected-step",
                "2",
                "--av-expected-ms",
                "0",
                "--out-dir",
                pixel_proof_dir.to_str().unwrap(),
            ],
        )
    };

    let (with_code, with_text, with_json) = run("av-fail-with-861", Some(av));
    let (without_code, without_text, without_json) = run("av-fail-without-861", None);

    let with_v =
        with_json.unwrap_or_else(|| panic!("with-av verdict JSON not written: {with_text}"));
    let without_v = without_json
        .unwrap_or_else(|| panic!("without-av verdict JSON not written: {without_text}"));

    assert_eq!(
        with_v["all_cambox_av_sync"]["cam1"]["gate_pass"],
        serde_json::json!(false),
        "sanity: cam1's measured 500ms offset must still fail the +/-20ms gate: {with_v}"
    );
    assert_eq!(
        with_v["all_cambox_av_sync"]["gate_pass"],
        serde_json::json!(false),
        "sanity: the overall av_sync gate must still measure false: {with_v}"
    );
    assert_eq!(
        with_v["all_cambox_av_sync"]["gates_overall_pass"],
        serde_json::json!(false),
        "#861: the JSON must say plainly this failing term does not decide overall_pass: {with_v}"
    );
    assert_eq!(
        with_code, without_code,
        "#861: a failing A/V-offset gate must NOT change the merge PROCESS exit code (report-only, \
         pending ASRC #803) -- with={with_code} ({with_text}), without={without_code} ({without_text})"
    );
    assert_eq!(
        with_v["overall_pass"], without_v["overall_pass"],
        "#861: a failing A/V-offset gate must be a no-op on overall_pass through the real CLI too \
         -- with={with_v}, without={without_v}"
    );
}

/// #861 — "Unknown from silent audio": the cam2 emit log still exists (the marker was EMITTED),
/// but the QPSK decode found ZERO audio markers to pair against it — the exact #689/#690 real-rig
/// failure mode (marker tone bytes are written to the HDMI audio device, but the recorded track
/// is silent downstream). Too-thin/zero candidates still verdict `Unknown` and Unknown still
/// fails the gate CLOSED (never a fabricated pass on thin data) — but since #861 that failure is
/// report-only too, so it must NOT change the merge subprocess's exit code either.
/// **This SUPERSEDES the old `merge_subprocess_exits_nonzero_when_av_sync_is_unknown_from_silent_
/// audio_703`**, which asserted the opposite when the gate was still wired into `all_pass`.
#[test]
fn merge_subprocess_exit_code_unaffected_by_av_sync_unknown_from_silent_audio_861() {
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;

    let av = AvMarkerInputs {
        fps: 30.0,
        video_start_s: 0.0,
        emit_log: (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect(),
        audio_markers: vec![], // silent audio track — zero decoded candidates
    };

    let run = |tag: &str, av: Option<AvMarkerInputs>| {
        let dir = tempdir(tag);
        let frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
        let sched = write_switch_schedule(&dir, &["CAM1"], base, win);
        let stream_partial = write_stream_partial(&dir, frames, av);
        let json_path = dir.join("verdict.json");
        let pixel_proof_dir = dir.join("pixel-proof");
        run_merge_subprocess(
            &stream_partial,
            &json_path,
            &[
                "--switch-schedule",
                sched.to_str().unwrap(),
                "--switch-guard-ns",
                "0",
                "--switch-expected-step",
                "2",
                "--av-expected-ms",
                "0",
                "--out-dir",
                pixel_proof_dir.to_str().unwrap(),
            ],
        )
    };

    let (with_code, with_text, with_json) = run("av-unknown-with-861", Some(av));
    let (without_code, without_text, without_json) = run("av-unknown-without-861", None);

    let with_v =
        with_json.unwrap_or_else(|| panic!("with-av verdict JSON not written: {with_text}"));
    let without_v = without_json
        .unwrap_or_else(|| panic!("without-av verdict JSON not written: {without_text}"));

    assert_eq!(
        with_v["all_cambox_av_sync"]["cam1"]["verdict"],
        serde_json::json!("unknown"),
        "sanity: zero audio candidates must still verdict unknown, never a fabricated offset: {with_v}"
    );
    assert_eq!(
        with_v["all_cambox_av_sync"]["cam1"]["gate_pass"],
        serde_json::json!(false),
        "sanity: Unknown must still fail the gate closed: {with_v}"
    );
    assert_eq!(
        with_code, without_code,
        "#861: an Unknown av_sync verdict must NOT change the merge PROCESS exit code (report-only) \
         -- with={with_code} ({with_text}), without={without_code} ({without_text})"
    );
    assert_eq!(
        with_v["overall_pass"], without_v["overall_pass"],
        "#861: an Unknown av_sync verdict must be a no-op on overall_pass through the real CLI too \
         -- with={with_v}, without={without_v}"
    );
}

/// #624 — the cross-camera DELIVERY-LATENCY-SPREAD gate (`all_cambox_latency.spread_gate_pass`
/// — a DIFFERENT measurement than the AV-sync gate above: `cam2->camera` capture latency, not
/// audio/video offset) ALSO folds into `all_pass` via the SAME `all_pass &= sv.pass` pattern
/// (`src/bin/recording-verdict.rs`, the `all_cambox_latency` block) — proven here the same way:
/// two cameras whose measured p50 latencies differ by far more than the 16ms
/// `switch_latency::SPREAD_THRESHOLD_MS` MUST make the merge subprocess exit non-zero.
///
/// (NOT the SAME gate as the #286 `all_cambox_delivery_latency`/`cross_camera_spread_ms` block —
/// that one is DELIBERATELY report-only per its own code comment ("does NOT fold into all_pass")
/// and per `all_cambox_delivery_latency_spread_never_gates_all_pass_286` in
/// `src/bin/recording-verdict.rs`'s own test module; this test targets the #624
/// `all_cambox_latency` block specifically, which DOES gate — asserting the #286 one gates here
/// would misrepresent an intentionally-advisory signal as a hard gate.)
#[test]
fn merge_subprocess_exits_nonzero_when_cross_camera_latency_spread_gate_fails_703() {
    let dir = tempdir("spread-fail");
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;
    // CAM1 at 0ms injected latency, CAM3 at 50ms -- spread=50ms, far over the 16ms threshold.
    let mut frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
    frames.extend(window_frames(
        20,
        base + win,
        Some((BURN_RUN_ID_CAM3, 50 * 1_000_000)),
    ));
    let sched = write_switch_schedule(&dir, &["CAM1", "CAM3"], base, win);

    let stream_partial = write_stream_partial(&dir, frames, None);
    let json_path = dir.join("verdict.json");
    let pixel_proof_dir = dir.join("pixel-proof");
    let (code, text, json) = run_merge_subprocess(
        &stream_partial,
        &json_path,
        &[
            "--switch-schedule",
            sched.to_str().unwrap(),
            "--switch-guard-ns",
            "0",
            "--switch-expected-step",
            "2",
            "--out-dir",
            pixel_proof_dir.to_str().unwrap(),
        ],
    );

    assert_ne!(
        code, 0,
        "#703/#624: a 50ms cross-camera delivery-latency spread (>> the 16ms threshold) MUST \
         make the merge PROCESS exit non-zero: exit={code} output={text}"
    );
    let v = json.unwrap_or_else(|| panic!("verdict JSON not written at {json_path:?}: {text}"));
    assert_eq!(
        v["all_cambox_latency"]["spread_gate_pass"],
        serde_json::json!(false),
        "#624: the 50ms spread must fail the 16ms gate: {v}"
    );
    assert_eq!(
        v["overall_pass"],
        serde_json::json!(false),
        "#703: overall_pass must be false when the cross-camera spread gate fails: {v}"
    );
}
