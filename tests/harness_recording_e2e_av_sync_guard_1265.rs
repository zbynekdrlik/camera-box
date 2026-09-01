//! #1265 task 3 — `scripts/recording-e2e.sh` must GUARD the #856 rig-wide A/V apply: HOLD it when
//! THIS run's audio timeline was unstable, instead of walking the prod pin toward a flapping mbc
//! ts_lag band (the 926->976 walk). Structural source-text assertions (same discipline as
//! `harness_recording_e2e_av_sync_apply_856.rs`, since only the live rig exercises the apply
//! end-to-end); the pure decision has its own pytest + the sourced lib its own harness.
//!
//! Tier-0 (camera-box #477/#557): RUNS ON CI only; `cargo fmt --all --check` proves it parses, and
//! the wiring was cross-checked locally by simulating these same `.find`/window assertions in python.

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
fn sources_the_apply_guard_lib() {
    let s = recording_e2e();
    assert!(
        s.contains(". \"$HERE/lib/av-sync-apply-guard.sh\""),
        "#1265: recording-e2e.sh must source the #856 apply-guard sourced helper"
    );
}

#[test]
fn band_verdict_declared_before_the_cleanup_trap() {
    let s = recording_e2e();
    let decl = s
        .find("AV_SYNC_BAND_VERDICT=\"${AV_SYNC_BAND_VERDICT:-}\"")
        .expect("#1265: AV_SYNC_BAND_VERDICT must be declared with a safe empty default");
    let trap = s
        .find("\ntrap cleanup EXIT HUP INT TERM")
        .expect("cleanup trap must install");
    assert!(
        decl < trap,
        "#1265: AV_SYNC_BAND_VERDICT must be declared BEFORE the cleanup trap (cleanup() reads it)"
    );
}

#[test]
fn band_verdict_gathered_at_8_8g_from_the_stream_facet() {
    let s = recording_e2e();
    // gathered right after the combiner ([8/8g]), into the pre-trap var, via the sourced helper.
    assert!(
        s.contains("AV_SYNC_BAND_VERDICT=\"$(av_sync_stream_band_verdict \"$STREAM\""),
        "#1265: [8/8g] must gather the stream mbc band verdict into AV_SYNC_BAND_VERDICT"
    );
    let combine = s.find("av_sync_combine_offsets.py").expect("combiner call");
    let gather = s
        .find("av_sync_stream_band_verdict")
        .expect("band gather call");
    assert!(
        combine < gather,
        "#1265: the band gather must run AFTER the combiner computed AV_SYNC_APPLY_OFFSET_MS"
    );
}

#[test]
fn cleanup_guards_the_apply_before_it_and_can_clear_the_offset() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let guard = body
        .find("av_sync_apply_guard_decide")
        .expect("#1265: cleanup() must call av_sync_apply_guard_decide");
    let apply = body
        .find("av_sync_calibrate.py")
        .expect("cleanup() must call av_sync_calibrate.py (the #856 apply)");
    assert!(
        guard < apply,
        "#1265: the apply-guard must run BEFORE the av_sync_calibrate.py apply (a HOLD clears the \
         offset so the apply is skipped)"
    );
    // the guard's HOLD path clears the offset (so the byte-unchanged apply `if [ -n ... ]` skips it)
    // and persists the reason.
    let gwin = &body[guard..(apply).min(body.len())];
    assert!(
        gwin.contains("AV_SYNC_APPLY_OFFSET_MS=\"\""),
        "#1265: a HOLD must clear AV_SYNC_APPLY_OFFSET_MS so the apply is skipped: {gwin}"
    );
    assert!(
        gwin.contains("av-sync-apply-hold-"),
        "#1265: a HOLD must persist its reason to a file: {gwin}"
    );
    // #856 discipline preserved: the guard/apply must NOT touch $GATE.
    assert!(
        !gwin.contains("exit 1") && !gwin.contains("GATE=1"),
        "#1265: the apply-guard must never affect the run's exit code: {gwin}"
    );
}

#[test]
fn cleanup_guard_runs_after_the_stream_teardown_restore() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let teardown = body
        .find("timeout \"$OBS_CLEANUP_TIMEOUT\" python3 \"$HERE/obs_phase2.py\" \"${_stream_teardown_args[@]}\"")
        .expect("cleanup() must call the stream teardown restore");
    let guard = body
        .find("av_sync_apply_guard_decide")
        .expect("cleanup() must call the apply-guard");
    assert!(
        teardown < guard,
        "#1265: the guard (like the apply it protects) runs AFTER the #358/#691 stream teardown \
         restore, composing with it"
    );
}

#[test]
fn cleanup_persists_the_applied_offset_after_the_apply() {
    let s = recording_e2e();
    let body = cleanup_body(&s);
    let apply = body.find("av_sync_calibrate.py").expect("apply call");
    let persist = body.find("av_sync_persist_applied_offset").expect(
        "#1265: cleanup() must persist the applied offset for the next run's jump baseline",
    );
    assert!(
        apply < persist,
        "#1265: the persist must run AFTER the apply (it records what landed, gated on the OUTDIR \
         success file)"
    );
}
