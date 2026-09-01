//! #771 — mv-fps-gate binary exit-code contract (mirrors tests/harness_render_budget_gate.rs).
//!
//! Locks the cross-boundary behaviour of the built `mv-fps-gate` binary: a healthy
//! `multiview-audit:` log passes (exit 0), a projector whose window-MEDIAN cadence fell below its
//! printed floor fails (exit 1, #1212), and a log with no audit line fails CLOSED (exit 2) — never
//! a silent pass. The decision itself lives in `camera_box::mv_audit::gate_log` (unit-tested
//! Tier-0); this proves the binary wires it to the right exit codes.

use std::path::Path;
use std::process::Command;

fn run(bin: &str, stdin: &str) -> i32 {
    use std::io::Write;
    let mut child = Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mv-fps-gate");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait().expect("wait").code().expect("exit code")
}

/// The gate binary source must exist.
#[test]
fn mv_fps_gate_bin_src_exists() {
    let path = format!("{}/src/bin/mv-fps-gate.rs", env!("CARGO_MANIFEST_DIR"));
    assert!(
        Path::new(&path).exists(),
        "src/bin/mv-fps-gate.rs not found (#771)."
    );
}

/// A healthy multiview-audit log (rendered_fps at/above floor) passes with exit 0.
#[test]
fn gate_binary_passes_a_healthy_log() {
    let bin = env!("CARGO_BIN_EXE_mv-fps-gate");
    let healthy = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080\n\
                   20:15:08.456: multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720\n";
    assert_eq!(run(bin, healthy), 0, "healthy MV fps must exit 0 (pass)");
}

/// A projector whose window-MEDIAN cadence fell below its floor fails with exit 1 (#1212).
#[test]
fn gate_binary_fails_a_below_floor_collapse() {
    let bin = env!("CARGO_BIN_EXE_mv-fps-gate");
    // monitor=1's 2-sample window median is 19.5 ((30 + 9)/2) < floor 28.0 (freeze / starvation).
    let breach = "multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080\n\
                  multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=28.0 cx=1920 cy=1080\n";
    assert_eq!(
        run(bin, breach),
        1,
        "a below-floor multiview collapse must exit 1 (alarm)"
    );
}

/// A log with no multiview-audit line fails CLOSED (exit 2), never a silent pass.
#[test]
fn gate_binary_no_audit_line_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_mv-fps-gate");
    let none =
        "some unrelated obs log line\ngenlock-fifo audit 'Cam 1': received=300 consumed=150\n";
    assert_eq!(
        run(bin, none),
        2,
        "no multiview-audit line must exit 2 (fail closed)"
    );
}
