//! #675 — recording-e2e.sh cleanup()'s `systemctl restart camera-box` restore (cam1's SOURCE
//! role, the cam3/4/5/6 ALL_CAMBOX loop, and cam2/painter) used to swallow a failed restart
//! silently via a trailing `2>/dev/null; true` / `|| true` — no verification, no visible warning.
//!
//! ## The bug (live incidents, 2026-07-11)
//!
//! During the #466 restart-survival dispatch, cam2's PRODUCTION `camera-box.service` was left
//! INACTIVE after BOTH a `recording-e2e.sh` run's cleanup completed normally (no error anywhere
//! in the harness's own log). Escalated: the SAME thing hit cam4 (the resolved SOURCE camera)
//! and cam1 across further runs in the same session — 4 occurrences total, twice breaking the
//! REQUIRED "Full-path E2E (rig zero-loss gate)" CI check on an unrelated PR (#676) because the
//! gate correctly detected the now-dark camera's NDI feed as FROZEN. A manual reproduction of the
//! exact restart one-liner in isolation succeeded instantly — the command itself is not broken;
//! something about the busy harness-execution context (suspected ssh/connection contention)
//! intermittently drops it, with the old code giving zero visibility either way.
//!
//! ## The fix these tests lock (static read of the shell script — no rig, no ssh)
//!
//! Every `systemctl restart camera-box` restore site in cleanup() (cam1's SOURCE-camera restore,
//! the cam3/4/5/6 ALL_CAMBOX loop, and cam2/painter's restore) is now immediately followed by a
//! call to the shared `camera_box_verify_active_cmds` builder
//! (scripts/lib/camera-box-restart-verify.sh): it polls `systemctl is-active camera-box`, retries
//! the restart ONCE (inside the SAME already-open ssh session) if it isn't active yet, and — only
//! if still not active after the retry — echoes a WARNING #675 line to stderr that is NEVER
//! swallowed (the outer `timeout ... ssh ...` call is not itself redirected, so it reaches the
//! harness's own console/CI log). It never aborts the remote command chain — the #649 flag
//! discipline (warn, don't kill the trap) applies here too. Same PURE-string static-read model as
//! the sibling `harness_recording_e2e_*` cleanup tests.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (same slice every
/// sibling cleanup test uses).
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

/// recording-e2e.sh must source the new shared restart-verify helper.
#[test]
fn recording_e2e_sources_the_restart_verify_helper() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("camera-box-restart-verify.sh"),
        "#675: recording-e2e.sh must source scripts/lib/camera-box-restart-verify.sh"
    );
    assert!(
        s.contains(". \"$HERE/lib/camera-box-restart-verify.sh\""),
        "#675: recording-e2e.sh must actually `source` (not just mention) camera-box-restart-verify.sh"
    );
}

/// cam1's SOURCE-camera restore block (before the ALL_CAMBOX loop) must call
/// `camera_box_verify_active_cmds` immediately after its `systemctl restart camera-box`, in the
/// SAME remote ssh command (so no new ssh connection is opened for the check).
#[test]
fn cleanup_verifies_cam1_restart() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312: cleanup() must have the cam3/4/5/6 ALL_CAMBOX restore loop");
    let cam1_block = &body[..loop_start];

    let restart = cam1_block
        .find("systemctl restart camera-box 2>/dev/null; true")
        .expect("#675: cam1's restore must still restart camera-box (unchanged original line)");
    let verify = cam1_block
        .find("camera_box_verify_active_cmds")
        .expect("#675: cam1's restore must call camera_box_verify_active_cmds after the restart");
    assert!(
        restart < verify,
        "#675: cam1's restart must come BEFORE its verify call. Block:\n{cam1_block}"
    );
}

/// The cam3/4/5/6 ALL_CAMBOX restore loop must ALSO call `camera_box_verify_active_cmds` after
/// its own `systemctl restart camera-box` — the escalation comment on #675 makes clear cam4 (a
/// non-cam1 SOURCE camera routed through this exact loop shape) hit the identical bug.
#[test]
fn cleanup_verifies_all_cambox_loop_restart() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let loop_start = body
        .find("for _ccn in $(camera_active_secondary_set)")
        .expect("#624/#312: cleanup() must have the cam3/4/5/6 ALL_CAMBOX restore loop");
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    let loop_region = &body[loop_start..painter_region_start];

    let restart = loop_region
        .find("systemctl restart camera-box 2>/dev/null; true")
        .expect(
            "#675: the ALL_CAMBOX loop must still restart camera-box (unchanged original line)",
        );
    let verify = loop_region.find("camera_box_verify_active_cmds").expect(
        "#675: the ALL_CAMBOX loop must call camera_box_verify_active_cmds after the restart",
    );
    assert!(
        restart < verify,
        "#675: the ALL_CAMBOX loop's restart must come BEFORE its verify call. Loop region:\n{loop_region}"
    );
}

/// cam2/painter's restore block must ALSO call `camera_box_verify_active_cmds` after its own
/// `systemctl restart camera-box` — this is the box the ORIGINAL #675 report hit first.
#[test]
fn cleanup_verifies_cam2_painter_restart() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    let painter_region_start = body
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("cleanup() must have the cam2/painter restore block");
    let painter_region = &body[painter_region_start..];

    let restart = painter_region
        .find("systemctl restart camera-box 2>/dev/null || true")
        .expect(
            "#675: cam2/painter's restore must still restart camera-box (unchanged original line)",
        );
    let verify = painter_region.find("camera_box_verify_active_cmds").expect(
        "#675: cam2/painter's restore must call camera_box_verify_active_cmds after the restart",
    );
    assert!(
        restart < verify,
        "#675: cam2/painter's restart must come BEFORE its verify call. Painter region:\n{painter_region}"
    );
}

/// The shared builder itself: polls `systemctl is-active camera-box`, retries the restart exactly
/// once, and prints a loud (non-swallowed) WARNING referencing #675 if still not active — but
/// NEVER aborts (no bare `exit`) so a genuinely down box can't kill the rest of cleanup()'s
/// teardown (mirrors the #649 StopRecord WARNING discipline: warn, never abort the trap).
#[test]
fn restart_verify_helper_polls_retries_and_warns_loud_without_aborting() {
    let s = read("scripts/lib/camera-box-restart-verify.sh");
    assert!(
        s.contains("camera_box_verify_active_cmds()"),
        "#675: the helper must define camera_box_verify_active_cmds()"
    );
    assert!(
        s.contains("systemctl is-active camera-box"),
        "#675: the helper must poll `systemctl is-active camera-box` after the restart"
    );
    // The retry: a SECOND `systemctl restart camera-box` call inside the function body.
    let restart_count = s.matches("systemctl restart camera-box").count();
    assert!(
        restart_count >= 1,
        "#675: the helper must retry `systemctl restart camera-box` at least once on failure. Got {restart_count} occurrences"
    );
    assert!(
        s.contains("WARNING #675"),
        "#675: the helper must print a WARNING referencing #675 when camera-box is still not \
         active after the retry, visible in the harness's own (non-redirected) log"
    );
    assert!(
        !s.contains("\nexit ") && !s.contains(" exit "),
        "#675: the helper must never itself `exit` — cleanup()'s trap must always run to \
         completion even when a restore fails (mirrors the #649 warn-only discipline). Got:\n{s}"
    );
}

/// Pure structural lock: the label argument is substituted LOCALLY (so the warning names the
/// actual camera) while every remote-side `$(...)`/`$var` stays literally escaped in the
/// function's OUTPUT — i.e. camera_box_verify_active_cmds must never be evaluated on dev1, only
/// spliced as text into the remote ssh command. Verified by actually invoking the shell function
/// exactly like the harness will (no ssh, no rig — pure bash).
#[test]
fn restart_verify_helper_output_is_valid_remote_shell_and_preserves_dollar_signs() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let path = format!(
        "{}/scripts/lib/camera-box-restart-verify.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let script =
        format!("source '{path}'\ncamera_box_verify_active_cmds \"cam1 (source, 10.77.9.61)\"\n");
    let child = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash to invoke the helper");
    let output = child.wait_with_output().expect("wait for bash");
    assert!(
        output.status.success(),
        "#675: invoking camera_box_verify_active_cmds must succeed. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        out.contains("$(systemctl is-active camera-box"),
        "#675: the function's OUTPUT must contain a LITERAL (unexpanded) remote `$(systemctl \
         is-active camera-box ...)` — evaluating it locally instead of preserving it for the \
         remote shell would silently check dev1's own systemd, not the camera box's. Got:\n{out}"
    );
    assert!(
        out.contains("cam1 (source, 10.77.9.61)"),
        "#675: the label argument must be substituted locally into the output text. Got:\n{out}"
    );

    // The produced text must itself be syntactically valid bash (as the remote host will parse
    // it once spliced into the full ssh command string).
    let mut checker = Command::new("bash")
        .arg("-n")
        .arg("/dev/stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash -n");
    checker
        .stdin
        .take()
        .expect("bash -n stdin")
        .write_all(out.as_bytes())
        .expect("write candidate script to bash -n");
    let check_status = checker.wait().expect("wait for bash -n");
    assert!(
        check_status.success(),
        "#675: camera_box_verify_active_cmds output must be valid bash syntax on its own"
    );
}
