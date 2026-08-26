//! #1128 — pure-function guard for `scripts/lib/grabber-stuck-health.sh`, the SHARED decision core
//! for the dev1-side fast-capture grabber STUCK alert.
//!
//! Root cause (issue 1128, live 2026-08-19 on CAM1): the GENKI ShadowCast 2 grabber can free-run at
//! ~62.5 fps AND deliver persistent corrupted frames — a state `systemctl restart camera-box` does
//! NOT clear (only a USB re-enumeration does). The camera-box appliance's crate-root detector
//! (`src/grabber_stuck.rs`) decides this and logs the report-only marker `#1128 grabber STUCK`
//! every 5s to its journal regardless of whether the in-process re-auth is enabled. This lib is the
//! ALERT half's decision core: given one cambox's raw ssh journal probe, classify STUCK / OK /
//! NODATA — ONE source of truth for the verdict (the Rust detector decides; the watchdog relays).
//!
//! Same convention as `tests/harness_splitter_port_health_739.rs`: source the REAL lib (source-only,
//! no side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/grabber-stuck-health.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

/// A realistic appliance STUCK marker line (bash-single-quoted for embedding in a heredoc `case`).
const MARKER: &str = "Aug 20 11:00:00 cam1 camera-box[1]: WARN #1128 grabber STUCK: /dev/video0 captured 62.50 fps (>= 61.5 fps over-rate floor) WITH persistent corrupted frames (4/window) sustained for 6 consecutive report windows (~30s)";
const STREAM: &str = "Aug 20 11:00:05 cam1 camera-box[1]: INFO Streaming: 30.0 fps emitted / 60.0 fps captured (150 sent, 300 captured, 0 capture-dropped, 0 corrupted)";

#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "grabber_stuck_parse_probe",
        "grabber_stuck_classify",
        "grabber_stuck_marker_fps",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---- grabber_stuck_parse_probe ---------------------------------------------------------------

#[test]
fn parse_empty_probe_is_unreachable_never_stuck() {
    // An ssh failure -> empty raw -> reachable=0 (NODATA), never a false STUCK.
    assert_eq!(
        stdout_of("grabber_stuck_parse_probe \"\""),
        "reachable=0 stuck=0"
    );
}

#[test]
fn parse_probe_ok_without_marker_is_reachable_not_stuck() {
    let body = format!("grabber_stuck_parse_probe \"$(printf 'PROBE_OK\\n{STREAM}\\n')\"");
    assert_eq!(stdout_of(&body), "reachable=1 stuck=0");
}

#[test]
fn parse_probe_ok_with_marker_is_reachable_and_stuck() {
    let body = format!("grabber_stuck_parse_probe \"$(printf 'PROBE_OK\\n{MARKER}\\n')\"");
    assert_eq!(stdout_of(&body), "reachable=1 stuck=1");
}

// ---- grabber_stuck_classify ------------------------------------------------------------------

#[test]
fn classify_unreachable_is_nodata() {
    assert_eq!(stdout_of("grabber_stuck_classify 0 0"), "verdict=NODATA");
    // even a (nonsensical) stuck=1 while unreachable stays NODATA — reachability gates everything.
    assert_eq!(stdout_of("grabber_stuck_classify 0 1"), "verdict=NODATA");
}

#[test]
fn classify_reachable_not_stuck_is_ok() {
    assert_eq!(stdout_of("grabber_stuck_classify 1 0"), "verdict=OK");
}

#[test]
fn classify_reachable_stuck_is_stuck() {
    assert_eq!(stdout_of("grabber_stuck_classify 1 1"), "verdict=STUCK");
}

// ---- grabber_stuck_marker_fps ----------------------------------------------------------------

#[test]
fn marker_fps_extracts_the_captured_rate() {
    let body = format!("grabber_stuck_marker_fps \"$(printf 'PROBE_OK\\n{MARKER}\\n')\"");
    assert_eq!(stdout_of(&body), "62.50");
}

#[test]
fn marker_fps_absent_marker_is_question_mark() {
    let body = format!("grabber_stuck_marker_fps \"$(printf 'PROBE_OK\\n{STREAM}\\n')\"");
    assert_eq!(stdout_of(&body), "?");
    assert_eq!(stdout_of("grabber_stuck_marker_fps \"\""), "?");
}

#[test]
fn marker_fps_uses_the_newest_marker_when_several_are_present() {
    // The device path carries no NN.NN, so the fps is the decimal after "captured " on the last
    // marker line — even when an earlier window logged a different rate.
    let older = "Aug 20 10:00:00 cam1 camera-box[1]: WARN #1128 grabber STUCK: /dev/video0 captured 61.90 fps (>= 61.5 fps over-rate floor) WITH persistent corrupted frames (3/window) sustained for 6 consecutive report windows (~30s)";
    let body = format!("grabber_stuck_marker_fps \"$(printf 'PROBE_OK\\n{older}\\n{MARKER}\\n')\"");
    assert_eq!(stdout_of(&body), "62.50");
}
