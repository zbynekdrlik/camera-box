//! #406/#312 item5 — belt-and-braces rig-busy re-check immediately before recording-e2e.sh
//! reroutes strih/stream's PRODUCTION program scenes.
//!
//! The CI-level scripts/rig-busy-gate.sh check (run by the automatic pull_request-triggered
//! full-path-e2e.yml gate) can pass tens of minutes before the [4/8] prod-scene reroute
//! actually happens ([1/8]-[3/8] build/deploy/painter-start take a while) — a real broadcast
//! could start in that window. These are content-assert guards (not live-rig execution) that
//! pin: the re-check exists, runs BEFORE the reroute, and aborts on busy=true.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn recording_e2e_sh_references_rig_busy_check_before_prod_scene_reroute() {
    let s = read("scripts/recording-e2e.sh");
    let recheck = s
        .find("rig-busy-check")
        .expect("recording-e2e.sh must invoke obs_phase2.py rig-busy-check (#406/#312 item5 belt-and-braces re-check)");
    let reroute = s
        .find("[4/8] OBS prod-scene routing")
        .expect("recording-e2e.sh must still have the [4/8] prod-scene reroute step");
    assert!(
        recheck < reroute,
        "#406/#312 item5: the rig-busy-check re-check MUST run BEFORE the [4/8] prod-scene \
         reroute (never reroute a rig that just went busy)."
    );
}

/// The re-check must actually ABORT the run (never just warn) when the rig reports busy — a
/// silent proceed would defeat the whole point of the gate.
#[test]
fn recording_e2e_sh_aborts_when_busy_recheck_reports_busy() {
    let s = read("scripts/recording-e2e.sh");
    let recheck_pos = s
        .find("rig-busy-check")
        .expect("rig-busy-check must be referenced");
    // Look at the ~600-char window right after the recheck call for an abort (`exit 1`) guarded
    // on the parsed busy field — not just anywhere later in the (very long) script.
    let window_end = (recheck_pos + 600).min(s.len());
    let window = &s[recheck_pos..window_end];
    assert!(
        window.contains("exit 1"),
        "#406/#312 item5: a BUSY rig-busy-check result must abort the run (exit 1) before the \
         reroute, not just warn. Nearby text:\n{window}"
    );
    assert!(
        window.contains("d[\"busy\"]") || window.contains("busy"),
        "#406/#312 item5: the abort must be conditioned on the busy-check's parsed 'busy' field: \
         \n{window}"
    );
}

/// The re-check must pass the SAME strih/stream hosts the rest of the harness already resolved
/// (not a hardcoded/duplicate host pair that could silently drift from $STRIH/$STREAM).
#[test]
fn recording_e2e_sh_rig_busy_recheck_uses_the_resolved_strih_stream_vars() {
    let s = read("scripts/recording-e2e.sh");
    let recheck_pos = s
        .find("rig-busy-check")
        .expect("rig-busy-check must be referenced");
    let window_end = (recheck_pos + 300).min(s.len());
    let window = &s[recheck_pos.saturating_sub(200)..window_end];
    assert!(
        window.contains("\"$STRIH\"") && window.contains("\"$STREAM\""),
        "the belt-and-braces re-check must use the harness's own $STRIH/$STREAM vars, not a \
         hardcoded duplicate: \n{window}"
    );
}
