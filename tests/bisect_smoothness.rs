//! Behavioral guard for the #1150 visible-smoothness bisect driver
//! (`scripts/bisect-smoothness.sh` + `scripts/lib/bisect-smoothness.sh`).
//!
//! ## Why this tool exists (#1150 — the owner's controlled-bisect mandate, issue 1130 point 4)
//!
//! Visible juddering on strih/stream/imag worsened over recent weeks; the owner WITHDREW the
//! hardware conclusion, and the working hypothesis is a REGRESSION in the emit/receive stack. The
//! driver finds the breaking commit deterministically: for each candidate history point it deploys
//! that point's historical CI binary to CAM1+CAM2 ONLY, leaving CAM3 on the current build as the
//! measurement CONTROL, then STOPS — the E2E run + per-box uniformity read-out + the owner's visual
//! confirmation are the SUPERVISOR's manual step BETWEEN points (issue 1130 point 1).
//!
//! Same PURE-PLANNER model as tests/rig_mode.rs / tests/launch_obs_genlock.rs: these tests source
//! the REAL lib (its pure builders) and run the REAL driver in its DRY-RUN default — NO rig, NO
//! deploy. The load-bearing safety property asserted here is that the deploy command is always
//! `CAMERA_SET="cam1 cam2"` and NEVER names cam3 (the control box), and that DRY-RUN is the default.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bisect-smoothness.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn driver() -> PathBuf {
    let s = manifest_dir().join("scripts/bisect-smoothness.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the LIB (no main to guard — it is a pure lib) and run `body`, returning stdout. Asserts
/// the harness exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
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

/// Source the LIB + run `body`, returning (exit_code, stdout) WITHOUT asserting success — for the
/// pure functions that intentionally return non-zero (parse of a comment / bad run-id).
fn run_sourced_status(body: &str) -> (i32, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn camera_set_is_cam1_cam2_never_cam3() {
    // bisect_camera_set is the SINGLE source of truth used by BOTH the printed plan AND the real
    // deploy in the driver, so asserting it here covers the literal that actually deploys (#1150).
    let out = run_sourced("bisect_camera_set");
    assert_eq!(
        out, "cam1 cam2",
        "the bisect deploy set must be exactly cam1 cam2"
    );
    assert!(
        !out.contains("cam3"),
        "the bisect deploy set must NEVER contain cam3 (the control box): {out}"
    );
}

#[test]
fn deploy_plan_is_cam1_cam2_never_cam3() {
    let out = run_sourced("bisect_deploy_plan P3-bad462 31897259559 1.7.0-dev.462");
    assert_eq!(
        out, "CAMERA_SET=\"cam1 cam2\" scripts/deploy-fleet.sh --run 31897259559",
        "deploy plan must be the exact cam1+cam2 deploy-fleet command"
    );
    assert!(
        !out.contains("cam3"),
        "deploy plan must NEVER name cam3 (the control box): {out}"
    );
}

#[test]
fn parse_rejects_non_numeric_run_id() {
    let (rc, _) = run_sourced_status(
        "bisect_parse_point_line \"$(printf 'Pbad\\tNOTNUM\\t1.7.0-dev.1\\tx')\" >/dev/null 2>&1",
    );
    assert_eq!(rc, 2, "a non-numeric RUN_ID must be rejected with rc=2");
}

#[test]
fn parse_skips_comment_and_blank_lines() {
    let (rc_c, _) = run_sourced_status("bisect_parse_point_line '# comment' >/dev/null 2>&1");
    assert_eq!(rc_c, 1, "a comment line must be skipped (rc=1)");
    let (rc_b, _) = run_sourced_status("bisect_parse_point_line '   ' >/dev/null 2>&1");
    assert_eq!(rc_b, 1, "a blank line must be skipped (rc=1)");
}

#[test]
fn parse_valid_line_roundtrips_fields() {
    let out = run_sourced(
        "bisect_parse_point_line \"$(printf 'P2\\t31036919641\\t1.7.0-dev.432\\t#889 in')\"",
    );
    assert_eq!(out, "P2\t31036919641\t1.7.0-dev.432\t#889 in");
}

#[test]
fn driver_dry_run_is_default_and_never_deploys_cam3() {
    // Run the REAL driver in its DRY-RUN default against the shipped points file, with a marker log
    // in a throwaway path. DRY-RUN must print the cam1+cam2 plan, must say DRY-RUN, and must NOT
    // create the log (nothing deployed).
    let log = std::env::temp_dir().join("bisect-smoothness-test-1150.log");
    let _ = std::fs::remove_file(&log);
    let out = Command::new("bash")
        .arg(driver())
        .arg("--point")
        .arg("P8-cur507")
        .env("BISECT_LOG", &log)
        .output()
        .expect("failed to run driver");
    assert!(out.status.success(), "driver dry-run must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("CAMERA_SET=\"cam1 cam2\" scripts/deploy-fleet.sh --run 32359309599"),
        "dry-run must print the exact cam1+cam2 deploy plan: {stdout}"
    );
    assert!(
        stdout.contains("DRY-RUN (default)"),
        "dry-run must be the default: {stdout}"
    );
    assert!(
        !log.exists(),
        "DRY-RUN must NOT write the marker log (nothing deployed)"
    );
}
