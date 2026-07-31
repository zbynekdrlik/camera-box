//! #893 — phase-sync-active-floor-gate: cross-boundary guards.
//!
//! The decision itself is unit-tested in `src/phase_sync_active_floor.rs` (7 pure tests). These
//! tests lock the CLI boundary (correct JSON in/out, correct exit codes) through the actual
//! built binary. The recording-e2e.sh WIRING guard is added separately, alongside the preflight
//! itself (mirrors `harness_render_budget_gate.rs`'s own wiring guard for its gate).

use std::path::Path;
use std::process::Command;

#[test]
fn gate_bin_src_exists() {
    let path = format!(
        "{}/src/bin/phase-sync-active-floor-gate.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/phase-sync-active-floor-gate.rs not found (#893)."
    );
}

#[test]
fn gate_binary_passes_when_an_active_camera_is_at_the_floor() {
    let bin = env!("CARGO_BIN_EXE_phase-sync-active-floor-gate");
    let input = r#"{"active_set":["cam1","cam2","cam3","cam4"],"pins":{"cam1":21,"cam2":3,"cam3":26,"cam4":55,"cam5":13}}"#;
    let (code, out) = run(bin, input);
    assert_eq!(
        code, 0,
        "an active camera (cam2) at the floor must PASS, got exit {code}, out={out:?}"
    );
    assert!(
        out.contains("cam2"),
        "PASS output should name the floor camera: {out:?}"
    );
}

#[test]
fn gate_binary_fails_the_exact_live_893_regression() {
    let bin = env!("CARGO_BIN_EXE_phase-sync-active-floor-gate");
    // The exact live pin table from #893's own evidence: only the RETIRED cam5 sits at the
    // floor, every ACTIVE camera (cam1-4) drifted above it.
    let input = r#"{"active_set":["cam1","cam2","cam3","cam4"],"pins":{"cam1":21,"cam2":16,"cam3":26,"cam4":55,"cam5":3,"cam6":62,"cam7":41}}"#;
    let (code, out) = run(bin, input);
    assert_eq!(
        code, 1,
        "no ACTIVE camera at the floor must FAIL (a retired camera's stale pin must never \
         count), got exit {code}, out={out:?}"
    );
    assert!(
        !out.contains("cam5") || out.contains("FAIL"),
        "must not report cam5 as satisfying the term: {out:?}"
    );
}

#[test]
fn gate_binary_bad_json_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_phase-sync-active-floor-gate");
    let (code, _out) = run(bin, "not json");
    assert_eq!(code, 2, "bad JSON must exit 2 (fail closed)");
}

#[test]
fn gate_binary_empty_active_set_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_phase-sync-active-floor-gate");
    let (code, _out) = run(bin, r#"{"active_set":[],"pins":{"cam1":3}}"#);
    assert_eq!(
        code, 2,
        "an empty active_set must exit 2 (fail closed), never silently pass"
    );
}

fn run(bin: &str, stdin: &str) -> (i32, String) {
    use std::io::Write;
    let mut child = Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn phase-sync-active-floor-gate");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}
