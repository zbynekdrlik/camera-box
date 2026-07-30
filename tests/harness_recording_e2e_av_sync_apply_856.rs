//! #856 -- `scripts/recording-e2e.sh` must ACTUALLY APPLY the A/V correction it already
//! measures (`all_cambox_av_sync`), not just report it. `scripts/av_sync_calibrate.py` (#427/
//! #188) already implements the full apply (measured offset -> genlock video-delay on
//! 'NDI 2ME PGM', read-back verified, rolled back on mismatch, persisted) but was never wired
//! into the fused run.
//!
//! ## Why the apply lands in cleanup(), not at [8/8g] where it's computed
//!
//! `cleanup()` (the bash `EXIT` trap, always runs) calls `obs_phase2.py teardown --host
//! "$STREAM"`, which restores `NDI 2ME PGM`'s `genlock_latency_ms_src` back to whatever it was
//! snapshotted at the START of this run (the #358/#691 delivery-verify snapshot/restore).
//! Applying the NEW correction at [8/8g] (post-verdict) and then letting the script exit would
//! have this teardown restore silently overwrite it a few lines later, inside the SAME
//! cleanup() -- so the apply must happen strictly AFTER that restore call, composing with it
//! instead of being fought by it.
//!
//! Structural, source-text assertions -- same discipline as the rest of this repo's harness
//! suite (`tests/harness_recording_e2e_latency_pins_756.rs`,
//! `tests/harness_recording_e2e_latency_stomp_691.rs`) since this is a step against a live rig
//! that only the rig itself can exercise end-to-end. The pure combining decision
//! (`scripts/av_sync_combine_offsets.py`) has its own pytest suite
//! (`tests/python/test_av_sync_combine_offsets.py`).

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of cleanup() -- from `cleanup()` to the `\ntrap ` that installs it (same slice
/// every sibling cleanup test in this repo uses, e.g. harness_recording_e2e_latency_stomp_691.rs).
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

// ---------------------------------------------------------------------------------------------
// (1) AV_SYNC_APPLY_OFFSET_MS declared BEFORE the cleanup trap, safe empty default.
// ---------------------------------------------------------------------------------------------

#[test]
fn av_sync_apply_offset_ms_is_declared_before_the_cleanup_trap() {
    let s = recording_e2e();
    let decl_pos = s
        .find("AV_SYNC_APPLY_OFFSET_MS=\"${AV_SYNC_APPLY_OFFSET_MS:-}\"")
        .expect(
            "#856: recording-e2e.sh must declare AV_SYNC_APPLY_OFFSET_MS with a safe empty \
             default (mirrors AV_SYNC_CALIBRATED_MS/IMAG_PREV_SCENE in this same file)",
        );
    let trap_pos = s
        .find("\ntrap cleanup EXIT HUP INT TERM")
        .expect("recording-e2e.sh must install the cleanup trap");
    assert!(
        decl_pos < trap_pos,
        "#856: AV_SYNC_APPLY_OFFSET_MS must be declared BEFORE `trap cleanup EXIT ...` \
         installs -- cleanup() reads it, so an early abort before this line would otherwise \
         `set -u`-abort the trap."
    );
}

// ---------------------------------------------------------------------------------------------
// (2) [8/8g]: the combiner is invoked with THIS run's own verdict JSON, right after it exists.
// ---------------------------------------------------------------------------------------------

#[test]
fn av_sync_combine_offsets_is_invoked_with_this_runs_verdict_json() {
    let s = recording_e2e();
    assert!(
        s.contains("av_sync_combine_offsets.py"),
        "recording-e2e.sh must invoke scripts/av_sync_combine_offsets.py"
    );
    assert!(
        s.contains("av_sync_combine_offsets.py\" --verdict-json \"$REPORT_JSON\""),
        "the combiner must be fed THIS run's own verdict JSON, not a stale/different one"
    );
}

#[test]
fn av_sync_combine_runs_after_the_merge_verdict_is_computed() {
    let s = recording_e2e();
    let verdict_idx = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\"")
        .expect("the merge recording-verdict execution must exist");
    let combine_idx = s
        .find("av_sync_combine_offsets.py")
        .expect("the combiner call must exist");
    assert!(
        verdict_idx < combine_idx,
        "#856: the combiner must run AFTER the verdict is computed (it needs THIS run's own \
         all_cambox_av_sync measurements, not a stale value)."
    );
}

#[test]
fn av_sync_combine_sets_the_apply_offset_var_never_calls_calibrate_directly() {
    let s = recording_e2e();
    let idx = s
        .find("av_sync_combine_offsets.py")
        .expect("the combiner call must exist");
    let window = &s[idx.saturating_sub(600)..(idx + 700).min(s.len())];
    assert!(
        window.contains("AV_SYNC_APPLY_OFFSET_MS=\"$(python3 \"$HERE/av_sync_combine_offsets.py\""),
        "#856: the combiner's stdout must be captured into AV_SYNC_APPLY_OFFSET_MS (the var \
         cleanup() later reads to decide whether/what to apply). Window:\n{window}"
    );
    assert!(
        !window.contains("av_sync_calibrate.py"),
        "#856: [8/8g] must only COMPUTE the offset, never call av_sync_calibrate.py directly \
         here -- the actual OBS apply belongs in cleanup(), AFTER the #358/#691 restore (see \
         module doc). Window:\n{window}"
    );
}

#[test]
fn av_sync_combine_failure_resets_the_apply_offset_var_and_never_touches_gate() {
    let s = recording_e2e();
    let idx = s
        .find("av_sync_combine_offsets.py")
        .expect("the combiner call must exist");
    let block = &s[idx.saturating_sub(200)..(idx + 500).min(s.len())];
    assert!(
        block.contains("AV_SYNC_APPLY_OFFSET_MS=\"\""),
        "#856: on a combiner refusal, AV_SYNC_APPLY_OFFSET_MS must be reset to empty (so \
         cleanup()'s later `if [ -n ... ]` correctly skips the apply): {block}"
    );
    assert!(
        !block.contains("exit 1") && !block.contains("GATE=1"),
        "#856: a combiner refusal must NEVER affect the run's own exit code / $GATE \
         (best-effort, same discipline as the pins-snapshot / Discord report steps): {block}"
    );
}

// ---------------------------------------------------------------------------------------------
// (3) cleanup(): the REAL apply happens strictly AFTER the #358/#691 stream teardown restore.
// ---------------------------------------------------------------------------------------------

#[test]
fn cleanup_applies_the_av_sync_correction_after_the_stream_teardown_restore() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let teardown_idx = body
        .find("timeout \"$OBS_CLEANUP_TIMEOUT\" python3 \"$HERE/obs_phase2.py\" \"${_stream_teardown_args[@]}\"")
        .expect("cleanup() must call obs_phase2.py teardown with _stream_teardown_args");
    let apply_idx = body.find("av_sync_calibrate.py").expect(
        "cleanup() must call scripts/av_sync_calibrate.py to apply this run's own #856 correction",
    );
    assert!(
        teardown_idx < apply_idx,
        "#856: the av_sync_calibrate.py --apply call must come AFTER the stream teardown \
         restore -- that restore ALWAYS runs on exit and would otherwise silently overwrite \
         whatever [8/8g] computed (composing with the restore instead of fighting it)."
    );
}

#[test]
fn cleanup_av_sync_apply_is_guarded_and_uses_the_computed_offset() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let idx = body
        .find("av_sync_calibrate.py")
        .expect("cleanup() must call scripts/av_sync_calibrate.py");
    let window = &body[idx.saturating_sub(300)..(idx + 500).min(body.len())];
    assert!(
        body.contains("if [ -n \"$AV_SYNC_APPLY_OFFSET_MS\" ]; then"),
        "#856: cleanup() must only apply when AV_SYNC_APPLY_OFFSET_MS was actually computed \
         this run (empty by default -- an early abort or a combiner refusal must never touch \
         the stream box)."
    );
    assert!(
        window.contains("--host \"$STREAM\"")
            && window.contains("--source \"$STREAM_PROG_SOURCE\"")
            && window.contains("--offset-ms \"$AV_SYNC_APPLY_OFFSET_MS\"")
            && window.contains("--apply"),
        "#856: the apply call must target the stream box's program source with THIS run's own \
         computed offset, and pass --apply (not a dry run). Window:\n{window}"
    );
}
