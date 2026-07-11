//! #674 — pure-function tests for `scripts/lib/imag-jitter-state.sh`, the byte-offset bookkeeping
//! `scripts/imag-jitter-monitor.sh` (imag-nb's periodic genlock-FIFO audit delta reporter, deployed
//! for the NEXT natural judder occurrence per the #674 telemetry deliverable) uses to resume
//! reading only the NEW OBS log content since its last run.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-jitter-state.sh")
}

/// Source the lib and evaluate `body`. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn next_offset(stored: &str, current_size: &str) -> String {
    let (code, out, err) = run_sourced(&format!(
        "imag_jitter_next_offset '{stored}' '{current_size}'"
    ));
    assert_eq!(code, 0, "harness crashed. stderr: {err}");
    out.trim().to_string()
}

#[test]
fn resumes_from_the_stored_offset_when_the_log_only_grew() {
    assert_eq!(next_offset("1000", "5000"), "1000");
}

#[test]
fn fresh_state_starts_at_zero() {
    assert_eq!(next_offset("0", "5000"), "0");
}

#[test]
fn resets_to_zero_when_the_log_shrank_rotated_or_was_replaced() {
    // Stored offset (5000) exceeds the CURRENT size (200) — the file was rotated/truncated/a
    // fresh OBS process started a new log. Never error, never silently skip the whole file.
    assert_eq!(next_offset("5000", "200"), "0");
}

#[test]
fn resets_to_zero_on_a_corrupt_or_missing_state_value() {
    for bad in ["", "abc", "-5", "12.3", "1000 "] {
        assert_eq!(
            next_offset(bad, "5000"),
            "0",
            "a malformed stored offset '{bad}' must fail closed to 0, never crash or misread"
        );
    }
}

#[test]
fn equal_offset_and_size_means_no_new_content_but_is_not_treated_as_shrunk() {
    // offset == size is the normal "caught up, nothing new yet" steady state — must resume FROM
    // that offset (0 new bytes to read), not reset to 0 (which would re-read the whole file).
    assert_eq!(next_offset("5000", "5000"), "5000");
}
