//! #405 / EPIC #406 — render-budget strict gate: cross-boundary + wiring guards.
//!
//! Locks (a) that the render-budget-gate binary decides pass/fail via
//! `render_budget::classify` exactly as the rig E2E needs, and (b) that
//! `scripts/recording-e2e.sh` actually WIRES the gate as a pre-record step (so it can
//! never be silently dropped — the exact class of miss that let the 2026-07-02
//! 60→27fps choke ship undetected).

use std::fs;
use std::path::Path;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The Rust gate binary source must exist.
#[test]
fn render_budget_gate_bin_src_exists() {
    let path = format!(
        "{}/src/bin/render-budget-gate.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/render-budget-gate.rs not found (#405)."
    );
}

/// recording-e2e.sh MUST invoke the render-budget gate as a pre-record step, while burns
/// are ON and the Multiview is open — the exact state that choked on 2026-07-02. Guards
/// against the gate being removed or never wired.
#[test]
fn recording_e2e_sh_wires_render_budget_gate() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("render-budget-gate"),
        "scripts/recording-e2e.sh must invoke the render-budget-gate before StartRecord \
         (#405 strict render-fps gate — burns-ON + multiview-open state must hold the frame budget)."
    );
}

/// The 2026-07-02 choke sample (strih 27fps/36ms burns-on) MUST fail the gate (exit 1),
/// while a healthy strih(60/11) + stream(30/1.4) pair passes (exit 0). This is the
/// cross-boundary behavioural lock through the actual built binary.
#[test]
fn gate_binary_fails_the_choke_and_passes_healthy() {
    let bin = env!("CARGO_BIN_EXE_render-budget-gate");

    let choke = r#"{"strih":{"active_fps":27.5,"avg_render_time_ms":36.0,"render_skipped_frac":0.55,"target_fps":60.0}}"#;
    let out = run(bin, choke);
    assert_eq!(
        out, 1,
        "27fps/36ms choke MUST exit 1 (fail the render budget), got exit {out}"
    );

    let healthy = r#"{"strih":{"active_fps":60.0,"avg_render_time_ms":11.3,"render_skipped_frac":0.0,"target_fps":60.0},"stream":{"active_fps":30.0,"avg_render_time_ms":1.4,"render_skipped_frac":0.0,"target_fps":30.0}}"#;
    let out = run(bin, healthy);
    assert_eq!(
        out, 0,
        "healthy 60/30fps MUST exit 0 (pass), got exit {out}"
    );
}

/// Malformed input fails closed (exit 2), never silently passes.
#[test]
fn gate_binary_bad_json_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_render-budget-gate");
    let out = run(bin, "not json");
    assert_eq!(out, 2, "bad JSON must exit 2 (fail closed), got exit {out}");
}

fn run(bin: &str, stdin: &str) -> i32 {
    use std::io::Write;
    let mut child = Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn render-budget-gate");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait().expect("wait").code().expect("exit code")
}
