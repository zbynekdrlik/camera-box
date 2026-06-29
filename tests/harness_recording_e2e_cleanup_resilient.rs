//! #328 — recording-e2e.sh cleanup() must FREE the cam capture devices even if OBS teardown hangs.
//!
//! ## The bug (RUN_ID 312001)
//!
//! The #312 all-cambox run hung ~28 min on `obs_phase2.py prod-scene --host <stream>`; on SIGINT
//! the cleanup trap ran `obs_phase2.py teardown --host <stream>` which ALSO hung on the same stream
//! WS path. Because the OBS teardown ran BEFORE — and with no bound — the cam restore, the trap
//! never reached the cam1 device restore, so cam1's `/tmp/camera-box-burn-*` binary kept holding
//! /dev/video0 and the prod camera-box crash-looped ("Device or resource busy"). A stuck OBS op
//! must NEVER leave a cam box's capture device held.
//!
//! ## The fix these tests lock (static read of the shell script — no rig, no ssh)
//!
//! cleanup() (1) runs the cam1/cam2 DEVICE restore FIRST, before the OBS record-stop/teardown, so
//! freeing /dev/video0 is never gated behind a hung OBS op, and (2) wraps every blocking
//! obs_phase2/obs_burn_filter invocation in `timeout` so even a hung OBS op cannot block the trap.
//! Same PURE-string model as tests/harness_rig_test_dropin_cleanup.rs: read the real script, assert
//! ordering + structure.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (same slice the
/// sibling cleanup tests use).
fn cleanup_body(s: &str) -> String {
    let start = s.find("cleanup()").expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

/// Headline #328 guard: inside cleanup(), the cam1 capture-device free step (removing the
/// `/tmp/camera-box-burn-*` binary that holds /dev/video0) MUST come BEFORE the `obs_phase2.py
/// teardown` calls — so a hung OBS teardown can never strand the cam device. (cam-device-free is
/// the safety-critical action; it is not gated behind OBS.)
#[test]
fn cleanup_frees_cam_device_before_obs_teardown() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let free = body
        .find("rm -f /tmp/camera-box-burn-*")
        .expect("#328: cleanup() must free /dev/video0 (rm -f /tmp/camera-box-burn-*) on cam1");
    let teardown = body
        .find("obs_phase2.py\" teardown")
        .or_else(|| body.find("obs_phase2.py teardown"))
        .expect("#328: cleanup() must still run obs_phase2 teardown");
    assert!(
        free < teardown,
        "#328: cleanup() must FREE the cam capture device (rm -f /tmp/camera-box-burn-*) BEFORE \
         the obs_phase2 teardown — otherwise a hung OBS teardown (the #328 hang) strands \
         /dev/video0 and the prod camera-box crash-loops. Order the cam restore first."
    );
}

/// Every blocking obs_phase2 invocation in cleanup() (record stop + teardown) MUST be wrapped in
/// `timeout` so a hung obs-websocket op (the #328 hang) cannot block the trap even if it runs.
#[test]
fn cleanup_obs_calls_are_timeout_bounded() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    for line in body
        .lines()
        .filter(|l| l.contains("obs_phase2.py") || l.contains("obs_burn_filter.py"))
    {
        let code = line.trim_start();
        // Only executable invocations (skip comments); a real invocation begins the command
        // (possibly via $(...) capture) and must be guarded by `timeout`.
        if code.starts_with('#') {
            continue;
        }
        assert!(
            code.contains("timeout "),
            "#328: every blocking OBS call in cleanup() must be wrapped in `timeout` so a hung \
             obs-websocket op can't block the trap and strand a cam device. Unbounded line: {line:?}"
        );
    }
}

/// The cam-device free step itself must FORCE-kill the burn binary so /dev/video0 is reliably
/// released even if it is mid-write (the #328 incident needed a manual `kill -9`).
#[test]
fn cleanup_force_kills_the_cam1_burn_binary() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    assert!(
        body.contains("pkill -9 -f 'camera-box-burn-'")
            || body.contains("pkill -9 -f \"camera-box-burn-\""),
        "#328: cleanup() must force-kill (pkill -9 -f) the cam1 burn binary so it reliably \
         releases /dev/video0 (a plain TERM can leave it mid-write holding the device)."
    );
}
