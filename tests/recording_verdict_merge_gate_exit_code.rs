//! #703 — proves the REAL `recording-verdict --merge-partials` CLI PROCESS actually EXITS
//! non-zero when the cross-camera SOURCE-side latency-spread gate (#624, the `all_cambox_latency`
//! block — a DIFFERENT block from the #286 `all_cambox_delivery_latency` one, which #1142 also
//! flipped blocking) fails — not
//! just that the in-process `build_and_print_verdict()` function RETURNS `all_pass=false`
//! (already covered, in-process, by
//! `all_cambox_latency_measures_per_camera_windowed_latency_and_fails_a_wide_spread_624` in
//! `src/bin/recording-verdict.rs`'s own `#[cfg(test)] mod tests`).
//!
//! **#861 (2026-08-06): the A/V-offset gate (#641/#624 deliverable 4) is BLOCKING again** —
//! re-armed after ASRC (#803) proved stable (was report-only 2026-07-29 to 2026-08-06, user
//! decision on #856). A failing/Unknown A/V-sync verdict once again makes this process exit
//! non-zero, mirroring the in-process
//! `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861_rearmed` /
//! `all_cambox_av_sync_measures_a_real_camera_and_fails_closed_on_the_rest_312` in
//! `src/bin/recording-verdict.rs`. The two AV-specific tests below are INVERTED BACK to their
//! ORIGINAL pre-#861 form (git `4743e2e9b`'s
//! `merge_subprocess_exits_nonzero_when_av_offset_gate_fails_703` /
//! `merge_subprocess_exits_nonzero_when_av_sync_is_unknown_from_silent_audio_703`) — same
//! fixtures, same non-zero-exit-code assertions, renamed with the `_861_rearmed` suffix to keep
//! the ticket history visible.
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

/// #861 (2026-08-06, re-armed after ASRC #803 proved stable) — a REAL, cleanly-measured av-offset
/// (500ms) far outside the default `--av-expected-ms 0` gate tolerance MUST make the
/// `--merge-partials` PROCESS exit non-zero again: the AV-offset gate (#641/#624 deliverable 4)
/// is BLOCKING again, so a failing measurement here forces the merge's overall verdict to fail —
/// proven through the REAL subprocess. This INVERTS
/// `merge_subprocess_exit_code_unaffected_by_av_offset_gate_failure_861` (the 2026-07-29
/// report-only proof this test replaces) and restores the ORIGINAL pre-#861 invariant
/// `merge_subprocess_exits_nonzero_when_av_offset_gate_fails_703` once asserted (git
/// `4743e2e9b`) — same fixture, same non-zero-exit-code assertion; mirrors the in-process fixture
/// `all_cambox_av_sync_gate_failure_forces_the_overall_verdict_to_fail_861_rearmed` (10
/// candidates, all at exactly 500ms — same data shape) through the real CLI end-to-end.
#[test]
fn merge_subprocess_exits_nonzero_when_av_offset_gate_fails_861_rearmed() {
    let dir = tempdir("av-fail-861-rearmed");
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;
    let frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
    let sched = write_switch_schedule(&dir, &["CAM1"], base, win);

    // 10 candidates: video_ts(1000+k) - audio_ts(k/30 - 0.5) = 0.5s = 500ms constant offset,
    // matching the emit-log frame_ids (1000..1010) the CAM1 window's optical ticks carry.
    let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
    let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0 - 0.5, k)).collect();
    let av = AvMarkerInputs {
        fps: 30.0,
        video_start_s: 0.0,
        emit_log,
        audio_preamble_screens_passed: audio_markers.len() as u64,
        audio_markers,
    };

    let stream_partial = write_stream_partial(&dir, frames, Some(av));
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
            "--av-expected-ms",
            "0",
            "--min-secs",
            "0",
            "--out-dir",
            pixel_proof_dir.to_str().unwrap(),
        ],
    );

    assert_ne!(
        code, 0,
        "#861: a 500ms av-offset (far outside the default gate tolerance) MUST make the merge \
         PROCESS exit non-zero again (re-armed after ASRC #803): exit={code} output={text}"
    );
    let v = json.unwrap_or_else(|| panic!("verdict JSON not written at {json_path:?}: {text}"));
    assert_eq!(
        v["all_cambox_av_sync"]["cam1"]["gate_pass"],
        serde_json::json!(false),
        "#624: cam1's measured 500ms offset must fail the gate tolerance: {v}"
    );
    assert_eq!(
        v["all_cambox_av_sync"]["gate_pass"],
        serde_json::json!(false),
        "#624: the overall av_sync gate must be false: {v}"
    );
    assert_eq!(
        v["all_cambox_av_sync"]["gates_overall_pass"],
        serde_json::json!(true),
        "#861: the JSON must say plainly this failing term DOES decide overall_pass again: {v}"
    );
    assert_eq!(
        v["overall_pass"],
        serde_json::json!(false),
        "#861: overall_pass must be false when the av_sync gate fails (re-armed): {v}"
    );
}

/// #861 vacuity control (review finding on PR #1002): every test in this file asserts a NON-zero
/// exit, which proves nothing if the baseline single-window merge already exits non-zero for an
/// unrelated reason. This is the exit-0 proof: the identical fixture shape with the markers
/// aligned to a 0ms offset and the never-scheduled cameras operator-acked (cam3..cam7, the #855
/// mechanism — without the acks their absent+unacked Unknown verdicts fail the gate closed by
/// design) must make the merge process exit 0 with `overall_pass=true`. If THIS test goes red,
/// the baseline is not green and the non-zero assertions above have no bite.
#[test]
fn merge_subprocess_exits_zero_when_av_offset_gate_passes_861_control() {
    let dir = tempdir("av-pass-861-control");
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;
    let frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
    let sched = write_switch_schedule(&dir, &["CAM1"], base, win);

    // Markers land exactly ON each video tick — a clean 0ms offset, dead-center in the gate.
    let emit_log: Vec<(u8, u32, i64)> = (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect();
    let audio_markers: Vec<(f64, u8)> = (0..10u8).map(|k| (k as f64 / 30.0, k)).collect();
    let av = AvMarkerInputs {
        fps: 30.0,
        video_start_s: 0.0,
        emit_log,
        audio_preamble_screens_passed: audio_markers.len() as u64,
        audio_markers,
    };

    let stream_partial = write_stream_partial(&dir, frames, Some(av));
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
            "--av-expected-ms",
            "0",
            "--min-secs",
            "0",
            "--offline-ack-cams",
            "cam3:control-ack,cam4:control-ack,cam5:control-ack,cam6:control-ack,cam7:control-ack",
            "--out-dir",
            pixel_proof_dir.to_str().unwrap(),
        ],
    );

    let v = json.unwrap_or_else(|| panic!("verdict JSON not written at {json_path:?}: {text}"));
    assert_eq!(
        v["all_cambox_av_sync"]["cam1"]["gate_pass"],
        serde_json::json!(true),
        "control: cam1's clean 0ms offset must pass the gate: {v}"
    );
    assert_eq!(
        v["all_cambox_av_sync"]["gate_pass"],
        serde_json::json!(true),
        "control: with cam1+cam2 passing and every absent camera acked, the overall av_sync \
         gate must pass: {v}"
    );
    assert_eq!(
        v["overall_pass"],
        serde_json::json!(true),
        "control: the baseline single-window merge must be GREEN end-to-end — if this fails, \
         the non-zero-exit assertions in this file prove nothing: {v}"
    );
    assert_eq!(
        code, 0,
        "control: a fully passing run must exit 0 through the real CLI: exit={code} \
         output={text}"
    );
}

/// #861 (2026-08-06, re-armed) — "Unknown from silent audio": the cam2 emit log still exists (the
/// marker was EMITTED), but the QPSK decode found ZERO audio markers to pair against it — the
/// exact #689/#690 real-rig failure mode (marker tone bytes are written to the HDMI audio device,
/// but the recorded track is silent downstream). Too-thin/zero candidates MUST verdict `Unknown`,
/// and Unknown NEVER passes the gate (never a fabricated pass on thin data) — so the merge
/// subprocess exits non-zero again. Inverts
/// `merge_subprocess_exit_code_unaffected_by_av_sync_unknown_from_silent_audio_861` and restores
/// the ORIGINAL pre-#861 `merge_subprocess_exits_nonzero_when_av_sync_is_unknown_from_silent_
/// audio_703` (git `4743e2e9b`).
#[test]
fn merge_subprocess_exits_nonzero_when_av_sync_unknown_from_silent_audio_861_rearmed() {
    let dir = tempdir("av-unknown-861-rearmed");
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;
    let frames = window_frames(0, base, Some((BURN_RUN_ID_CAM1, 0)));
    let sched = write_switch_schedule(&dir, &["CAM1"], base, win);

    let av = AvMarkerInputs {
        fps: 30.0,
        video_start_s: 0.0,
        emit_log: (0..10u8).map(|k| (k, 1000 + k as u32, 0)).collect(),
        audio_markers: vec![], // silent audio track — zero decoded candidates
        audio_preamble_screens_passed: 0, // #748: no preamble energy ⇒ silent chain
    };

    let stream_partial = write_stream_partial(&dir, frames, Some(av));
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
            "--av-expected-ms",
            "0",
            "--min-secs",
            "0",
            "--out-dir",
            pixel_proof_dir.to_str().unwrap(),
        ],
    );

    assert_ne!(
        code, 0,
        "#861: an Unknown av_sync verdict (silent audio, zero candidates) MUST make the merge \
         PROCESS exit non-zero again (re-armed after ASRC #803) — Unknown never passes the gate: \
         exit={code} output={text}"
    );
    let v = json.unwrap_or_else(|| panic!("verdict JSON not written at {json_path:?}: {text}"));
    assert_eq!(
        v["all_cambox_av_sync"]["cam1"]["verdict"],
        serde_json::json!("unknown"),
        "#624: zero audio candidates must verdict unknown, never a fabricated offset: {v}"
    );
    assert_eq!(
        v["all_cambox_av_sync"]["cam1"]["gate_pass"],
        serde_json::json!(false),
        "#624: Unknown must fail the gate closed: {v}"
    );
    assert_eq!(
        v["overall_pass"],
        serde_json::json!(false),
        "#861: overall_pass must be false when av_sync is Unknown (re-armed): {v}"
    );
}

/// #624 — the cross-camera DELIVERY-LATENCY-SPREAD gate (`all_cambox_latency.spread_gate_pass`
/// — a DIFFERENT measurement than the AV-sync gate above: `cam2->camera` capture latency, not
/// audio/video offset) ALSO folds into `all_pass` via the SAME `all_pass &= sv.pass` pattern
/// (`src/bin/recording-verdict.rs`, the `all_cambox_latency` block) — proven here the same way:
/// two cameras whose measured p50 latencies differ by far more than the
/// `switch_latency::SPREAD_THRESHOLD_MS` bound (24ms since issue 1120) MUST make the merge
/// subprocess exit non-zero.
///
/// (A DIFFERENT block from the #286/#1033 `all_cambox_delivery_latency`/`cross_camera_spread_ms`
/// one — though #1142 flipped the DELIVERY-side `delivery_spread_gate` seam BLOCKING too, so BOTH
/// spreads now gate at the SAME `SPREAD_THRESHOLD_MS` bound; the delivery side is pinned by
/// `all_cambox_delivery_latency_spread_folds_blocking_since_1142` in
/// `src/bin/recording-verdict.rs`'s own test module. This test targets the #624 SOURCE-side
/// `all_cambox_latency` block specifically, which has always gated hard.)
#[test]
fn merge_subprocess_exits_nonzero_when_cross_camera_latency_spread_gate_fails_703() {
    let dir = tempdir("spread-fail");
    let base = 5_000 * ONE_S_NS;
    let win = 5 * ONE_S_NS;
    // CAM1 at 0ms injected latency, CAM3 at 50ms -- spread=50ms, far over the 24ms bound.
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
            "--min-secs",
            "0",
            "--out-dir",
            pixel_proof_dir.to_str().unwrap(),
        ],
    );

    assert_ne!(
        code, 0,
        "#703/#624: a 50ms cross-camera delivery-latency spread (>> the 24ms bound) MUST \
         make the merge PROCESS exit non-zero: exit={code} output={text}"
    );
    let v = json.unwrap_or_else(|| panic!("verdict JSON not written at {json_path:?}: {text}"));
    assert_eq!(
        v["all_cambox_latency"]["spread_gate_pass"],
        serde_json::json!(false),
        "#624: the 50ms spread must fail the 24ms gate: {v}"
    );
    assert_eq!(
        v["overall_pass"],
        serde_json::json!(false),
        "#703: overall_pass must be false when the cross-camera spread gate fails: {v}"
    );
}
