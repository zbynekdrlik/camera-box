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
///
/// #758 fix: anchors on the literal function DEFINITION `"cleanup() {"`, not the bare `"cleanup()"`
/// substring. The bare substring matches the FIRST occurrence anywhere in the file — which, well
/// before this fix, was actually a PROSE COMMENT ("...cleanup() clears any leftover drop-in...",
/// line 57 in the file this was written against), not the real `cleanup() {` function definition
/// ~550 lines later. That mis-anchor happened to be harmless only because no unguarded
/// obs_phase2/obs_burn_filter call existed anywhere in the (accidentally too-wide) byte range
/// between that comment and the real trap line — until #758 added one in the [0/8]/[1/8] preflight
/// section (which runs ONCE per run, long before cleanup() and completely outside the EXIT/HUP/
/// INT/TERM trap this file is actually about), which the mis-anchored slice then wrongly swept in
/// and flagged. `"cleanup() {"` (with the opening brace) is unambiguous — it is real bash syntax
/// nothing else in this file's prose ever writes verbatim.
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup() {")
        .expect("recording-e2e.sh must define cleanup() {");
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
    // Target only EXECUTABLE invocations (`python3 "$HERE/<script>"`), not advisory echo/comment
    // text that merely names a script. Every such invocation must be guarded by `timeout`.
    for line in body.lines().filter(|l| {
        l.contains("python3 \"$HERE/obs_phase2.py\"")
            || l.contains("python3 \"$HERE/obs_burn_filter.py\"")
    }) {
        assert!(
            line.contains("timeout "),
            "#328: every blocking OBS call in cleanup() must be wrapped in `timeout` so a hung \
             obs-websocket op can't block the trap and strand a cam device. Unbounded line: {line:?}"
        );
    }
}

/// The cam-device free step itself must FORCE-kill the burn binary so /dev/video0 is reliably
/// released even if it is mid-write (the #328 incident needed a manual `kill -9`).
///
/// #626/#640: the pattern must be anchored (`camera-box-burn-[a-z0-9]`), not the bare
/// `camera-box-burn-` this test used to require — the bare pattern self-matches the invoking
/// remote shell's own cmdline (it literally contains the pkill argument text) and SIGKILLs the
/// shell running the whole cleanup command before `systemctl restart camera-box` ever executes.
/// (Widened from the original digit-only `[0-9]` to `[a-z0-9]` by #640 — see
/// `cleanup_kills_the_cam_name_infixed_all_cambox_burn_binaries` for why digit-only wasn't enough.)
#[test]
fn cleanup_force_kills_the_cam1_burn_binary() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    assert!(
        body.contains("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
            || body.contains("pkill -9 -f \"camera-box-burn-[a-z0-9]\""),
        "#328/#626/#640: cleanup() must force-kill (pkill -9 -f) the cam1 burn binary with an \
         anchored pattern ('camera-box-burn-[a-z0-9]') so it reliably releases /dev/video0 \
         (a plain TERM can leave it mid-write holding the device) WITHOUT the pattern \
         self-matching the invoking ssh shell's own cmdline and killing it before the restart \
         runs (#626), AND without missing the camname-infixed cam2-6 form (#640)."
    );
}

/// #626's own comment claimed the digit-anchored pattern ALSO matches the camname-infixed form
/// (`/tmp/camera-box-burn-cam3-1783530925`) that cam2/cam3/cam4/cam5/cam6 actually use (#312/#624
/// ALL_CAMBOX deploy, `_cbin="/tmp/camera-box-burn-${_cn}-${RUN_ID}"`) -- it does NOT: the
/// character right after the hyphen there is a LETTER ('c' of "cam3"), not a digit, so
/// `camera-box-burn-[0-9]` never matches it. Empirically confirmed live on the rig (2026-07-09):
/// cam2/cam3/cam4/cam5/cam6 orphaned burn processes survived cleanup() across multiple runs,
/// crash-looping camera-box ("Device or resource busy") until manually killed. The pattern must
/// widen to `camera-box-burn-[a-z0-9]` at BOTH the cam3/4/5/6 loop and the cam2/painter restore.
#[test]
fn cleanup_kills_the_cam_name_infixed_all_cambox_burn_binaries() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));

    let loop_region_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312: cleanup() must have the cam3/4/5/6 ALL_CAMBOX restore loop");
    // Code-review finding: bound loop_region's RIGHT edge at the cam2/painter block's own start —
    // an unbounded `&body[loop_region_start..]` would run to the end of cleanup() and swallow the
    // painter block's own (also-widened) pkill line, so the first assertion below would still
    // pass even if ONLY the loop's own line (not the painter's) regressed back to the narrower
    // digit-only pattern. Isolating the two regions is the whole point of asserting them
    // separately.
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    let loop_region = &body[loop_region_start..painter_region_start];
    assert!(
        loop_region.contains("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
            || loop_region.contains("pkill -9 -f \"camera-box-burn-[a-z0-9]\""),
        "#624/#312: the cam3/4/5/6 restore must use the WIDENED pattern \
         ('camera-box-burn-[a-z0-9]') -- the digit-only pattern never matches their \
         'camera-box-burn-<camname>-<run_id>' naming, orphaning the burn process and \
         crash-looping camera-box: {loop_region}"
    );

    let painter_region = &body[painter_region_start..];
    assert!(
        painter_region.contains("pkill -9 -f 'camera-box-burn-[a-z0-9]'")
            || painter_region.contains("pkill -9 -f \"camera-box-burn-[a-z0-9]\""),
        "#312: the cam2/painter restore (under ALL_CAMBOX=1) must use the WIDENED pattern too -- \
         cam2's own manually-deployed burn binary is ALSO camname-infixed \
         ('camera-box-burn-cam2-<run_id>'): {painter_region}"
    );
}

/// Pure regex-behavior lock (no rig, no ssh): the widened pattern matches BOTH real target forms
/// (cam1's plain `camera-box-burn-<run_id>` AND the camname-infixed
/// `camera-box-burn-<camname>-<run_id>`) while STILL not matching the literal invoking pkill
/// argument text itself -- preserving the #626 self-match fix (immediately after the hyphen in
/// the invoking text is the regex's own `[` bracket character, never a letter or digit).
#[test]
fn widened_pattern_matches_both_burn_naming_forms_but_not_the_invoking_command() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let matches = |haystack: &str| -> bool {
        let mut child = Command::new("grep")
            .arg("-qE")
            .arg("camera-box-burn-[a-z0-9]")
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn grep");
        child
            .stdin
            .take()
            .expect("grep stdin")
            .write_all(haystack.as_bytes())
            .expect("write to grep stdin");
        child.wait().expect("wait for grep").success()
    };
    assert!(
        matches("root 123 /tmp/camera-box-burn-1783530925"),
        "cam1's plain form must match"
    );
    assert!(
        matches("root 123 /tmp/camera-box-burn-cam3-1783530925"),
        "cam3/4/5/6/cam2's camname-infixed form must match"
    );
    assert!(
        !matches("pkill -9 -f 'camera-box-burn-[a-z0-9]' 2>/dev/null"),
        "the pattern must NOT self-match the invoking pkill argument's own literal text \
         (the #626 self-match property must be preserved by the widened pattern)"
    );
}
