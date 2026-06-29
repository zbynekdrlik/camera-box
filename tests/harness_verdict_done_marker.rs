//! #281 Part B — done-marker (resumable decode) for recording-verdict-on-strih.sh +
//! recording-verdict-on-stream.sh.
//!
//! The two per-box planner scripts emit the MCP instructions (upload/run/download) that the
//! agent drives to decode each recording in place. If the small partial JSON (#208 "durable
//! state") already exists on dev1 from a PREVIOUS run, a re-invocation must SKIP rather than
//! re-decode (re-decoding is slow and wasteful; the partial is already correct). This is the
//! "resumable decode" property.
//!
//! ## Properties tested (RED→GREEN per regression-test-first.md)
//!
//! 1. --skip-if-exists <path>: when the file exists, script exits 0 + prints "SKIP".
//! 2. --skip-if-exists <path>: when the file does NOT exist, plan is emitted normally.
//! 3. Without --skip-if-exists, existing partial files do not suppress the plan.
//!
//! Same pure-string / subprocess model as the existing harness tests — no rig, no ssh.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn strih_planner() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-strih.sh")
}

fn stream_planner() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-stream.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("verdict-done-marker-{}-{}", std::process::id(), name));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run a planner script with the given args; return (exit_code, stdout, stderr).
fn run_planner(script: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script)
        .args(args)
        .output()
        .expect("run planner script");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ─── strih planner done-marker ───────────────────────────────────────────────

#[test]
fn strih_skip_if_partial_exists() {
    let dir = scratch("strih-skip");
    let partial = dir.join("strih-partial-1234.json");
    // Write a fake partial JSON (the "durable state" from a previous run).
    fs::write(&partial, r#"{"strih":"done"}"#).unwrap();

    let (code, stdout, stderr) = run_planner(
        &strih_planner(),
        &["--skip-if-exists", partial.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "#281 strih: --skip-if-exists with existing partial must exit 0\nstderr: {stderr}"
    );
    let output = stdout.to_lowercase();
    assert!(
        output.contains("skip"),
        "#281 strih: output must say SKIP when partial already exists\nstdout: {stdout}"
    );
    // Must NOT emit the decode plan (STEP 2 is the on-box decode).
    assert!(
        !stdout.contains("STEP 2"),
        "#281 strih: skip output must NOT include the decode plan (STEP 2)\nstdout: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn strih_no_skip_when_partial_absent() {
    let dir = scratch("strih-no-skip");
    let partial = dir.join("strih-partial-9999.json"); // does not exist

    let (code, stdout, _stderr) = run_planner(
        &strih_planner(),
        &[
            "--skip-if-exists",
            partial.to_str().unwrap(),
            "--strih-rec",
            r"C:\rec\strih.mkv",
            "--",
            "--extract-partial",
            "strih",
            "--strih",
            r"C:\rec\strih.mkv",
            "--out",
            r"C:\out\strih-partial.json",
        ],
    );
    assert_eq!(
        code, 0,
        "#281 strih: normal run must succeed\n"
    );
    assert!(
        stdout.contains("STEP 2") || stdout.contains("STEP 1"),
        "#281 strih: plan must be emitted when partial does NOT exist\nstdout: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── stream planner done-marker ──────────────────────────────────────────────

#[test]
fn stream_skip_if_partial_exists() {
    let dir = scratch("stream-skip");
    let partial = dir.join("stream-partial-1234.json");
    fs::write(&partial, r#"{"stream":"done"}"#).unwrap();

    let (code, stdout, stderr) = run_planner(
        &stream_planner(),
        &["--skip-if-exists", partial.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "#281 stream: --skip-if-exists with existing partial must exit 0\nstderr: {stderr}"
    );
    let output = stdout.to_lowercase();
    assert!(
        output.contains("skip"),
        "#281 stream: output must say SKIP when partial already exists\nstdout: {stdout}"
    );
    assert!(
        !stdout.contains("STEP 2"),
        "#281 stream: skip output must NOT include the decode plan\nstdout: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stream_no_skip_when_partial_absent() {
    let dir = scratch("stream-no-skip");
    let partial = dir.join("stream-partial-9999.json"); // does not exist

    let (code, stdout, _stderr) = run_planner(
        &stream_planner(),
        &[
            "--skip-if-exists",
            partial.to_str().unwrap(),
            "--stream-rec",
            r"C:\rec\stream.mp4",
            "--",
            "--extract-partial",
            "stream",
            "--stream",
            r"C:\rec\stream.mp4",
            "--out",
            r"C:\out\stream-partial.json",
        ],
    );
    assert_eq!(code, 0, "#281 stream: normal run must succeed");
    assert!(
        stdout.contains("STEP 2") || stdout.contains("STEP 1"),
        "#281 stream: plan must be emitted when partial does NOT exist\nstdout: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── backward compatibility: no flag → behavior unchanged ───────────────────

#[test]
fn strih_without_skip_flag_emits_plan_regardless() {
    // Without --skip-if-exists, the script must emit the plan as it always did.
    let (code, stdout, _stderr) = run_planner(
        &strih_planner(),
        &[
            "--strih-rec",
            r"C:\rec\strih.mkv",
            "--",
            "--extract-partial",
            "strih",
            "--strih",
            r"C:\rec\strih.mkv",
            "--out",
            r"C:\out\strih-partial.json",
        ],
    );
    assert_eq!(code, 0, "#281: strih planner without --skip-if-exists must succeed");
    assert!(
        stdout.contains("STEP 1"),
        "#281: without --skip-if-exists the plan must always be emitted\nstdout: {stdout}"
    );
}
