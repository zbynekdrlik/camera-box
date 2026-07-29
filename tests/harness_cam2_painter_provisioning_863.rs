//! #863 — "V devel režime má na cam2 monitore trvale bežať QR — cam2-painter.service nie je
//! vôbec nainštalovaná".
//!
//! `scripts/rig-mode.sh` (#440) and `scripts/recording-e2e.sh`'s cleanup() have ALWAYS assumed
//! `cam2-painter.service` exists (guarded stop/start calls, tested by `tests/rig_mode.rs` /
//! `tests/harness_cam2_painter_coordination.rs`) — but `scripts/setup-device.sh` never installed
//! it, so every one of those calls has been a silent no-op. These tests pin:
//!
//! 1. `scripts/setup-device.sh` installs the permanent painter + a permanent camera-box
//!    no-display drop-in, gated to cam2 ONLY (`cam2_is_painter_box`).
//! 2. `scripts/verify-device.sh` carries a cam2-only acceptance check for the installed unit.
//! 3. `scripts/lib/cam2-painter-restore-verify.sh` — the NEW cleanup()-time verification helper —
//!    actually behaves correctly against fake `systemctl`/`journalctl`/`fuser` stand-ins, and
//!    NEVER calls `exit` (cleanup() is the bash EXIT trap and must always run to completion).
//!
//! RED before this work exists (the pure functions / check / lib are absent, every test fails);
//! GREEN after.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn setup_device_script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup-device.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn verify_device_text() -> String {
    let p = manifest_dir().join("scripts/verify-device.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/cam2-painter-restore-verify.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source `setup-device.sh` (its `BASH_SOURCE != $0` guard skips the destructive provisioning
/// flow) and run `body` against its pure functions. Same convention as
/// `tests/setup_device_pure_functions.rs::run_sourced`.
fn run_sourced_setup_device(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", setup_device_script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// setup-device.sh: gating + unit/dropin content
// ---------------------------------------------------------------------------------------------

#[test]
fn cam2_is_painter_box_true_only_for_cam2() {
    for (name, expect) in [
        ("CAM2", true),
        ("CAM1", false),
        ("CAM3", false),
        ("CAM4", false),
        ("CAM5", false),
        ("cam2", false), // resolve_device_name always uppercases first -- lowercase must NOT match
        ("", false),
    ] {
        let (code, out, err) = run_sourced_setup_device(&format!(
            "cam2_is_painter_box '{name}' && echo YES || echo NO"
        ));
        assert_eq!(code, 0, "stderr: {err}");
        let want = if expect { "YES" } else { "NO" };
        assert_eq!(out.trim(), want, "name={name:?} stderr: {err}");
    }
}

#[test]
fn painter_no_display_dropin_sets_the_opt_out_env_var() {
    let (code, out, err) = run_sourced_setup_device("cam2_painter_no_display_dropin_content");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("[Service]"),
        "must be a valid [Service] drop-in: {out}"
    );
    assert!(
        out.contains("Environment=CAMERA_BOX_NO_DISPLAY=1"),
        "#863: must set the SAME opt-out env var rig-mode.sh's transient drop-in uses \
         (permanently, for cam2's fixed painter role): {out}"
    );
}

#[test]
fn painter_service_unit_content_pins_the_expected_flags() {
    let (code, out, err) = run_sourced_setup_device("cam2_painter_service_unit_content");
    assert_eq!(code, 0, "stderr: {err}");
    for needle in [
        "[Unit]",
        "[Service]",
        "[Install]",
        "ExecStart=/usr/local/bin/frame-probe",
        "--paint-only",
        "--dual-qr",
        "--qr-size 700",
        "--paint-fps 60",
        "--duration-secs",
        "Restart=always",
        "WantedBy=multi-user.target",
    ] {
        assert!(
            out.contains(needle),
            "unit content missing {needle:?}:\n{out}"
        );
    }
    // #420: this is a passive visual health display, NOT a measurement run -- it must NEVER
    // carry the QPSK audio marker (that's the TEST-mode transient painter's job, rig-mode.sh).
    assert!(
        !out.contains("--audio-marker"),
        "the permanent devel-mode painter must NOT emit the QPSK audio marker: {out}"
    );
}

#[test]
fn setup_device_installs_cam2_painter_only_when_the_box_is_cam2() {
    let s = fs::read_to_string(setup_device_script()).expect("read setup-device.sh");
    let step_marker = "STEP 3b: cam2 ONLY";
    let step_pos = s
        .find(step_marker)
        .expect("#863: expected a cam2-only STEP 3b in setup-device.sh");
    let tail = &s[step_pos..];
    let gate_pos = tail
        .find("if cam2_is_painter_box \"$DEVICE_NAME\"; then")
        .expect("#863: STEP 3b must be gated on cam2_is_painter_box, never unconditional");
    let block_end = tail[gate_pos..]
        .find("\nfi\n")
        .map(|i| gate_pos + i)
        .expect("STEP 3b's gate must close with a plain `fi`");
    let block = &tail[gate_pos..block_end];
    assert!(
        block.contains("cam2_painter_no_display_dropin_content"),
        "STEP 3b must install the permanent no-display drop-in: {block}"
    );
    assert!(
        block.contains("cam2_painter_service_unit_content"),
        "STEP 3b must install the painter unit: {block}"
    );
    assert!(
        block.contains("systemctl enable cam2-painter.service"),
        "STEP 3b must enable the unit so it survives the next reboot: {block}"
    );
    // #863: this script never STARTS services live (see camera-box.service's own STEP 7,
    // enable-only) -- everything takes effect on the box's next reboot.
    assert!(
        !block.contains("systemctl start cam2-painter")
            && !block.contains("systemctl restart cam2-painter"),
        "STEP 3b must not start/restart the service live -- this provisioner only enables \
         (reboot picks it up), matching STEP 7's own camera-box.service convention: {block}"
    );
}

// ---------------------------------------------------------------------------------------------
// verify-device.sh: the cam2-only acceptance check exists, is gated, and never runs on other boxes
// ---------------------------------------------------------------------------------------------

/// The (v) check's own block -- from its header marker to the next `# (q)` block (the
/// documented last check, confirmed to come after it by
/// `verify_device_v_check_runs_before_the_last_q_check`).
fn v_check_block(s: &str) -> String {
    let marker = "# (v) cam2-only: the PERMANENT devel-mode dual-QR painter";
    let pos = s
        .find(marker)
        .expect("#863: expected a (v) cam2-only painter acceptance check in verify-device.sh");
    let tail = &s[pos..];
    let end = tail
        .find("# (q) .bak cruft drift")
        .expect("(v) check must be followed by the (q) check");
    tail[..end].to_string()
}

#[test]
fn verify_device_gates_the_cam2_painter_check_to_cam2_only() {
    let s = verify_device_text();
    let block = v_check_block(&s);
    assert!(
        block.contains("if [ \"$NAME_UPPER\" = \"CAM2\" ]; then"),
        "the (v) check must be gated on NAME_UPPER == CAM2, never run unconditionally on every \
         box:\n{block}"
    );
    for needle in [
        "systemctl list-unit-files cam2-painter.service",
        "systemctl is-active cam2-painter.service",
        "journalctl -u cam2-painter.service",
        "CAMERA_BOX_NO_DISPLAY=1",
    ] {
        assert!(block.contains(needle), "(v) check missing {needle:?}");
    }
}

#[test]
fn verify_device_v_check_runs_before_the_last_q_check() {
    // #453's (q) check is documented (and tested by tests/verify_device_pure_functions.rs) as
    // "the LAST check before the ALL CLEAR/VERIFY FAILED summary" -- the (v) check added here
    // MUST NOT become the new last check, or it would silently get folded into (q)'s own
    // "runs to end-of-file" test slice. Confirm ordering instead of re-deriving it by hand.
    let s = verify_device_text();
    let v_pos = s
        .find("# (v) cam2-only: the PERMANENT devel-mode dual-QR painter")
        .expect("(v) check present");
    let q_pos = s
        .rfind("# (q) .bak cruft drift")
        .expect("(q) check present");
    assert!(
        v_pos < q_pos,
        "(v) check (byte {v_pos}) must come BEFORE the (q) check (byte {q_pos}), which is the \
         documented last check before the summary"
    );
}

// ---------------------------------------------------------------------------------------------
// recording-e2e.sh: the new lib is sourced + wired in right after the existing (guarded,
// unverified) `systemctl start cam2-painter` call inside cleanup().
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_new_restore_verify_lib() {
    let s = recording_e2e_text();
    assert!(
        s.contains(". \"$HERE/lib/cam2-painter-restore-verify.sh\""),
        "recording-e2e.sh must source scripts/lib/cam2-painter-restore-verify.sh"
    );
}

#[test]
fn cleanup_calls_the_restore_verify_right_after_starting_cam2_painter() {
    let s = recording_e2e_text();
    let start_pos = s
        .find("systemctl start cam2-painter 2>/dev/null || true")
        .expect("existing #367/#440 restore call must be unchanged (never edit the anchored line)");
    let verify_pos = s
        .find("$(cam2_painter_restore_verify_cmds)")
        .expect("#863: the new verification call must be wired in");
    assert!(
        verify_pos > start_pos && verify_pos - start_pos < 80,
        "the verify call (byte {verify_pos}) must immediately follow the existing start call \
         (byte {start_pos}), not be relocated elsewhere"
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/lib/cam2-painter-restore-verify.sh: functional behavior against fake systemctl/
// journalctl/fuser stand-ins (never a live ssh/rig dependency).
// ---------------------------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cam2-painter-restore-verify-863-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_fake_bin(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/usr/bin/env bash\n{script}\n")).expect("write fake bin");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&path, perms).unwrap();
}

/// Run `cam2_painter_restore_verify_cmds`'s OWN generated text through `eval` inside a harness
/// whose PATH is restricted to fake `systemctl`/`journalctl`/`fuser`/`sleep` stand-ins -- this
/// simulates "what cam2 would run" without any ssh/network/rig dependency. `sleep` is faked to a
/// no-op so the ~8s poll loops in the generated text run instantly.
fn run_against_fakes(bin_dir: &Path) -> (i32, String, String) {
    let harness = format!(
        r#"set -uo pipefail
. "$LIB"
export PATH="{bin}:$PATH"
eval "$(cam2_painter_restore_verify_cmds)"
"#,
        bin = bin_dir.display(),
    );
    write_fake_bin(bin_dir, "sleep", "exit 0"); // never actually wait in tests
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn unit_not_installed_is_a_guarded_no_op() {
    let dir = scratch("not-installed");
    write_fake_bin(&dir, "systemctl", "exit 1"); // list-unit-files fails -> "not installed"
    let (code, out, err) = run_against_fakes(&dir);
    assert_eq!(code, 0, "must never exit non-zero: stderr={err}");
    assert!(out.contains("not installed on this box"), "stdout={out}");
    assert!(
        err.is_empty(),
        "a box without the unit must never WARN: {err}"
    );
}

#[test]
fn active_kms_painting_reports_success_with_no_warning() {
    let dir = scratch("kms-ok");
    write_fake_bin(
        &dir,
        "systemctl",
        r#"
case "$1 $2" in
  "list-unit-files cam2-painter.service") exit 0 ;;
  "is-active cam2-painter.service") echo active; exit 0 ;;
esac
exit 0
"#,
    );
    write_fake_bin(
        &dir,
        "journalctl",
        r#"echo "presenter: using DRM/KMS page-flip (/dev/dri/card0)"
echo "KmsPresenter: 1920x1080@60.000Hz, double-buffered DRM page-flip, vblank-locked 1:1""#,
    );
    write_fake_bin(&dir, "fuser", "exit 0"); // device held
    let (code, out, err) = run_against_fakes(&dir);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("genuinely painting"),
        "expected a success line: {out}"
    );
    assert!(
        err.is_empty(),
        "a healthy KMS run must never WARN to stderr: {err}"
    );
}

#[test]
fn service_never_becomes_active_warns_but_never_exits() {
    let dir = scratch("never-active");
    write_fake_bin(
        &dir,
        "systemctl",
        r#"
case "$1 $2" in
  "list-unit-files cam2-painter.service") exit 0 ;;
  "is-active cam2-painter.service") echo inactive; exit 3 ;;
esac
exit 0
"#,
    );
    write_fake_bin(&dir, "journalctl", "exit 0");
    write_fake_bin(&dir, "fuser", "exit 1");
    let (code, out, err) = run_against_fakes(&dir);
    assert_eq!(
        code, 0,
        "cleanup()'s EXIT trap must ALWAYS run to completion -- a restore-verify failure must \
         WARN, never `exit`. stdout={out} stderr={err}"
    );
    assert!(
        err.contains("WARNING #863") && err.contains("FAILED to come back active"),
        "expected a loud WARNING naming the failure: {err}"
    );
}

#[test]
fn active_but_no_painting_signal_warns_but_never_exits() {
    let dir = scratch("no-signal");
    write_fake_bin(
        &dir,
        "systemctl",
        r#"
case "$1 $2" in
  "list-unit-files cam2-painter.service") exit 0 ;;
  "is-active cam2-painter.service") echo active; exit 0 ;;
esac
exit 0
"#,
    );
    // No presenter-selection line at all, and fuser never reports anything held.
    write_fake_bin(&dir, "journalctl", "echo 'some unrelated log line'");
    write_fake_bin(&dir, "fuser", "exit 1");
    let (code, out, err) = run_against_fakes(&dir);
    assert_eq!(code, 0, "must never exit: stdout={out} stderr={err}");
    assert!(
        err.contains("WARNING #863") && err.contains("no painting signal found"),
        "expected the no-painting-signal WARNING: {err}"
    );
}

#[test]
fn lib_never_contains_a_bare_exit_call() {
    // cleanup() is the bash EXIT trap (per .claude/rules/recording-e2e-cleanup-composition.md);
    // a helper spliced into it must NEVER call `exit` -- that would abort the trap itself,
    // unlike scripts/lib/presenter-liveness-check.sh's painter_liveness_check_cmds (used only at
    // TEST-mode LAUNCH time, where aborting is correct). Scan only the heredoc BODY the function
    // generates (never the surrounding doc-comment prose, which legitimately discusses "exit
    // code" in English).
    let s = fs::read_to_string(lib_script()).expect("read lib");
    let body_start = s
        .find("cat <<'VERIFY'")
        .expect("expected the generated heredoc body");
    let body_end = s[body_start..]
        .rfind("\nVERIFY")
        .map(|i| body_start + i)
        .expect("expected the heredoc to close with VERIFY");
    let body = &s[body_start..body_end];
    assert!(
        !body.contains("exit "),
        "cam2-painter-restore-verify.sh's generated body must be WARN-only -- found a bare \
         `exit` call:\n{body}"
    );
}
