//! #1265 -- `scripts/recording-e2e.sh` must DAMP the #856 rig-wide A/V correction with the fixed
//! loop gain BEFORE the guard + the +/-50/run clamp, and record the per-run controller history.
//! Structural source-text assertions (same discipline as `harness_recording_e2e_av_sync_apply_856.rs`
//! / `harness_recording_e2e_av_sync_guard_1265.rs`, since only the live rig exercises the apply
//! end-to-end); the pure gain math + the history append have their own pytest suites.
//!
//! Tier-0 (camera-box #477/#557): RUNS ON CI only; `cargo fmt --all --check` proves it parses, and
//! the wiring was cross-checked locally by simulating these same `.find`/window assertions in python
//! plus a real `bash` run of the sourced-lib functions under `set -euo pipefail`.

use std::fs;
use std::path::PathBuf;

fn recording_e2e() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn cleanup_body(s: &str) -> String {
    let start = s.find("cleanup()").expect("cleanup() must exist");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("cleanup trap must install after cleanup()");
    s[start..end].to_string()
}

#[test]
fn loop_gain_context_vars_declared_before_the_cleanup_trap() {
    // cleanup() reads all four (the calibrate gain args + the history append), so an early abort
    // before these lines would `set -u`-abort the trap -- declare them pre-trap with empty defaults.
    let s = recording_e2e();
    let trap = s
        .find("\ntrap cleanup EXIT HUP INT TERM")
        .expect("cleanup trap must install");
    for var in [
        "AV_SYNC_COMBINED_OFFSET_MS_RAW=\"${AV_SYNC_COMBINED_OFFSET_MS_RAW:-}\"",
        "AV_SYNC_LOOP_GAIN_VALUE=\"${AV_SYNC_LOOP_GAIN_VALUE:-}\"",
        "AV_SYNC_PROPOSED_OFFSET_MS=\"${AV_SYNC_PROPOSED_OFFSET_MS:-}\"",
        "AV_SYNC_HELD_REASON=\"${AV_SYNC_HELD_REASON:-}\"",
    ] {
        let pos = s
            .find(var)
            .unwrap_or_else(|| panic!("#1265: {var} must be declared with a safe empty default"));
        assert!(
            pos < trap,
            "#1265: {var} must be declared BEFORE the cleanup trap"
        );
    }
}

#[test]
fn gain_damps_the_offset_at_8_8g_after_combine_before_the_band_gather() {
    // The gain multiplies the combined offset (via the sourced helper) and REASSIGNS
    // AV_SYNC_APPLY_OFFSET_MS to the damped value, so both the guard and the +/-50 clamp downstream
    // see the damped number. It runs AFTER the combiner and BEFORE the band gather.
    let s = recording_e2e();
    let combine = s.find("av_sync_combine_offsets.py").expect("combiner call");
    let damp = s
        .find("av_sync_apply_loop_gain \"$AV_SYNC_APPLY_OFFSET_MS\" \"$HERE/av_sync_loop_gain.py\"")
        .expect("#1265: [8/8g] must damp via av_sync_apply_loop_gain");
    let gather = s
        .find("av_sync_stream_band_verdict")
        .expect("band gather call");
    assert!(
        combine < damp && damp < gather,
        "#1265: the gain damp must run AFTER the combiner and BEFORE the band gather \
         (combine={combine} damp={damp} gather={gather})"
    );
    // the damped value is captured back into AV_SYNC_APPLY_OFFSET_MS (so the guard + clamp see it).
    assert!(
        s.contains("AV_SYNC_APPLY_OFFSET_MS=\"$(printf '%s' \"$_avs_gain_pair\" | cut -f1)\""),
        "#1265: the damped offset (cut -f1) must be captured back into AV_SYNC_APPLY_OFFSET_MS"
    );
    // the raw median is preserved for the calibrate gain line + persist.
    assert!(
        s.contains("AV_SYNC_COMBINED_OFFSET_MS_RAW=\"$AV_SYNC_APPLY_OFFSET_MS\""),
        "#1265: the raw combined median must be stashed before damping"
    );
}

#[test]
fn calibrate_call_passes_the_loop_gain_context() {
    // the gain log line at apply time + the persisted loop_gain/combined_offset_ms_raw keys need
    // the gain + raw median passed to av_sync_calibrate.py.
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let idx = body
        .find("av_sync_calibrate.py")
        .expect("cleanup apply call");
    let window = &body[idx..(idx + 600).min(body.len())];
    assert!(
        window.contains("--loop-gain \"$AV_SYNC_LOOP_GAIN_VALUE\"")
            && window.contains("--combined-offset-ms \"$AV_SYNC_COMBINED_OFFSET_MS_RAW\""),
        "#1265: the cleanup apply must pass --loop-gain + --combined-offset-ms so the gain line \
         logs at apply time and the keys persist. Window:\n{window}"
    );
}

#[test]
fn history_append_runs_last_after_the_applied_offset_persist() {
    // the append reads the pin that LANDED (av-sync-last.json) + this run's residual-last, so it
    // must run AFTER av_sync_persist_applied_offset and carry the proposed offset + hold reason.
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let persist_applied = body
        .find("av_sync_persist_applied_offset")
        .expect("applied-offset persist");
    let history = body
        .find("av_sync_append_history \"$RUN_ID\" \"$AV_SYNC_PROPOSED_OFFSET_MS\"")
        .expect("#1265: cleanup() must append the per-run controller history");
    assert!(
        persist_applied < history,
        "#1265: the history append must run AFTER the applied-offset persist (it reads the landed \
         pin): persist_applied={persist_applied} history={history}"
    );
    // the damped proposed offset is captured (for the history) BEFORE the guard can clear it.
    assert!(
        body.contains("AV_SYNC_PROPOSED_OFFSET_MS=\"$AV_SYNC_APPLY_OFFSET_MS\""),
        "#1265: the proposed offset must be captured before a HOLD clears AV_SYNC_APPLY_OFFSET_MS"
    );
    // a HOLD carries its reason into the history via AV_SYNC_HELD_REASON.
    assert!(
        body.contains("AV_SYNC_HELD_REASON=\"$_avs_hold\""),
        "#1265: a HOLD must record its reason for the history append"
    );
    // the history append must never affect $GATE (best-effort, like the rest of the #856 block).
    let hwin = &body[history..(history + 400).min(body.len())];
    assert!(
        !hwin.contains("exit 1") && !hwin.contains("GATE=1"),
        "#1265: the history append must never affect the run's exit code: {hwin}"
    );
}
