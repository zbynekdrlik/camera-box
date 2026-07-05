//! Regression guard for `scripts/lib/ndi-alive.sh` (#451) — the ONE shared "camera-box is alive
//! and streaming NDI" signal, extracted out of `scripts/upgrade-fleet-ndi.sh` so
//! `scripts/deploy-fleet.sh` can share the exact same pattern instead of keeping its own copy
//! that silently drifts out of sync (the #445 broadening was applied to upgrade-fleet-ndi.sh
//! only, leaving deploy-fleet.sh's narrower pattern behind — the "latent re-break" #451 fixes).
//!
//! These tests exercise the shared file DIRECTLY (source it, call the functions) so it has its
//! own coverage independent of which script sources it.

use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/ndi-alive.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the shared lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn emit_ok_grep_pattern_is_the_exact_broadened_signal() {
    let out = run_sourced("emit_ok_grep_pattern");
    assert_eq!(
        out.trim(),
        "fps emitted .* fps captured|Streaming: [0-9.]+ fps|NDI sender ready"
    );
}

#[test]
fn fatal_grep_pattern_is_the_exact_crash_signatures() {
    let out = run_sourced("fatal_grep_pattern");
    assert_eq!(
        out.trim(),
        "panic|thread '.*' panicked|SIGSEGV|SIGABRT|core dumped|FATAL"
    );
}

/// The pattern must actually MATCH the genlock decimation report via a real `grep -E`.
#[test]
fn emit_ok_grep_pattern_matches_genlock_report_line() {
    let pattern = run_sourced("emit_ok_grep_pattern").trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'genlock: 60.02 fps emitted 60.01 fps captured' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "emit_ok_grep_pattern must match the genlock decimation report. Pattern: {pattern}"
    );
}

/// The pattern must also match the older, non-decimating "Streaming: X.Y fps" shape a box with
/// no genlock decimation configured logs (src/main.rs: only logs "fps emitted / fps captured"
/// when decimation is active, else the plain "Streaming: X fps" form).
#[test]
fn emit_ok_grep_pattern_matches_older_streaming_fps_shape() {
    let pattern = run_sourced("emit_ok_grep_pattern").trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'Streaming: 59.8 fps (299 frames, 0 capture-dropped)' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "emit_ok_grep_pattern must match the older non-decimating 'Streaming: X fps' shape. \
         Pattern: {pattern}"
    );
}

/// The pattern must also match the generic NDI-sender-ready line.
#[test]
fn emit_ok_grep_pattern_matches_ndi_sender_ready_line() {
    let pattern = run_sourced("emit_ok_grep_pattern").trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' \"NDI sender ready, streaming as 'CAM1'\" | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "emit_ok_grep_pattern must match the generic NDI-sender-ready line. Pattern: {pattern}"
    );
}
