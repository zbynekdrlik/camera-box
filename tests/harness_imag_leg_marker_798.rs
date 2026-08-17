//! issue 798 — `scripts/lib/imag-leg-marker.sh`'s `imag_leg_run_marker` emits ONE distinct,
//! greppable run-log line declaring whether the imag leg was VERIFIED (a partial reached dev1) or
//! SILENTLY skipped, and — when skipped — the REASON. Tier-0: sources the real lib + calls the
//! function with a temp path, zero network. Mirrors `tests/harness_cbox_burn_log_persist.rs`'s
//! source-and-call pattern.

use std::process::Command;

/// Source the real lib (relative to the crate root, `cargo test`'s cwd — same as every other
/// static-anchor harness here) and call `imag_leg_run_marker <partial> <host>`; return trimmed
/// stdout. Asserts the function itself exits 0 (it is pure/read-only).
fn marker(partial: &str, host: &str) -> String {
    let script =
        format!(". scripts/lib/imag-leg-marker.sh; imag_leg_run_marker {partial:?} {host:?}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert!(
        out.status.success(),
        "imag_leg_run_marker exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn verified_when_the_partial_json_exists_on_dev1() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("imag-partial-1.json");
    std::fs::write(&p, "{}").unwrap();
    let m = marker(p.to_str().unwrap(), "/home/newlevel/imag-REC.mkv");
    assert!(
        m.starts_with("IMAG-LEG-VERIFIED"),
        "expected VERIFIED, got: {m}"
    );
    assert!(m.contains("issue 798"), "must cite the ticket: {m}");
}

#[test]
fn not_verified_names_the_no_recording_path_reason() {
    // No partial AND no host path (imag StopRecord returned none) → the specific "no recording
    // path" reason, distinct from an extract failure.
    let m = marker("", "");
    assert!(
        m.starts_with("IMAG-LEG-NOT-VERIFIED"),
        "expected NOT-VERIFIED, got: {m}"
    );
    assert!(m.contains("no imag recording path"), "wrong reason: {m}");
    assert!(
        m.contains("hidden partial"),
        "must name the hidden-partial doctrine: {m}"
    );
}

#[test]
fn not_verified_names_the_extract_failed_reason() {
    // A host path exists (StopRecord succeeded) but no partial reached dev1 → the extract failed,
    // a DIFFERENT reason than "no recording path" (the value of doing this at the run level).
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("imag-partial-missing.json");
    let m = marker(missing.to_str().unwrap(), "/home/newlevel/imag-REC.mkv");
    assert!(
        m.starts_with("IMAG-LEG-NOT-VERIFIED"),
        "expected NOT-VERIFIED, got: {m}"
    );
    assert!(m.contains("extract failed"), "wrong reason: {m}");
}

#[test]
fn marker_is_exactly_one_line() {
    // A run-log marker must be a single line, whichever branch it takes.
    assert_eq!(marker("", "").lines().count(), 1);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("p.json");
    std::fs::write(&p, "{}").unwrap();
    assert_eq!(marker(p.to_str().unwrap(), "x").lines().count(), 1);
}
