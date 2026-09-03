//! issue 1290 — the EVENT-mode gate on `scripts/splitter-port-alert-watchdog.sh`'s `main()`/`handle_box`.
//!
//! The sibling-anchor DEAD_PORT verdict (issue 739) is a TEST-rig premise (ONE camera through an HDMI
//! splitter to every cambox). In EVENT/production each cambox has its OWN camera, so a camera-less
//! cambox is legitimately black and a proven-good sibling proves nothing — the watchdog false-paged
//! the owner's phone 5× during a live show. The watchdog now probes the durable cam2 painter EVENT
//! signal once per pass and, in provable EVENT mode, logs every box's would-be verdict report-only +
//! never pages. TEST and UNKNOWN (cam2 unreadable) behave exactly as today.
//!
//! Method mirrors `tests/harness_splitter_port_watchdog_739.rs`: source the watchdog in --dry-run,
//! override `probe_box`/`sshpass` with canned data, ALSO override `rig_mode_probe` with a canned
//! painter snapshot, run `main` twice (past the 2-pass confirm), assert on the log (stderr).

use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/splitter-port-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the watchdog, override probe_box/sshpass + rig_mode_probe, run `main` `passes` times.
/// `rig_mode_probe_body` is the bash body of the `rig_mode_probe` override (its stdout = the canned
/// cam2 painter snapshot). Returns stderr (the log() stream). Per-test tempdir isolates state (#975).
fn run_driver(probe_cases: &str, rig_mode_probe_body: &str, passes: usize) -> String {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("splitter.state");
    let mut mains = String::new();
    for _ in 0..passes {
        mains.push_str("main\n");
    }
    let body = format!(
        "set -uo pipefail\n\
         export CAMERA_ACTIVE_SET='cam1 cam2 cam3'\n\
         export SPLITTER_WATCH_STATE_FILE='{state}'\n\
         . \"$SCRIPT\" --dry-run\n\
         sshpass() {{ :; }}\n\
         probe_box() {{ case \"$1\" in {cases} esac; }}\n\
         rig_mode_probe() {{ {rmp}; }}\n\
         {mains}",
        state = state.display(),
        cases = probe_cases,
        rmp = rig_mode_probe_body,
        mains = mains,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("SCRIPT", script())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "watchdog main() exited non-zero:\n{stderr}"
    );
    stderr
}

// Canned box chroma probe outputs (same as the 739 harness).
const COLOUR: &str = r"printf 'PROBE_OK\ncapture chroma: u_dev=6.1 v_dev=8.8 -> colour\n'";
const GREY: &str = r"printf 'PROBE_OK\ncapture chroma: u_dev=0.5 v_dev=0.4 -> grayscale (source likely monochrome)\n'";

// Canned rig_mode_probe snapshots.
const RIG_EVENT: &str =
    r"printf 'RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0\n'";
const RIG_TEST: &str =
    r"printf 'RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|1\nSVC_ACTIVE|1\n'";
const RIG_UNKNOWN: &str = r"printf ''"; // cam2 ssh failed -> empty -> UNKNOWN

// The mixed fleet that WOULD page a DEAD_PORT: cam2 grey, cam1+cam3 colour.
fn mixed_dead_port_fleet() -> String {
    format!("10.77.9.62) {GREY} ;; *) {COLOUR} ;;")
}

#[test]
fn event_mode_suppresses_dead_port_page_and_logs_report_only() {
    // The production-show false page: cam2 grey (no camera / blank monitor), cam1+cam3 colour. In
    // provable EVENT mode this must NOT page — it logs the would-be verdict report-only.
    let log = run_driver(&mixed_dead_port_fleet(), RIG_EVENT, 2);
    assert!(
        log.contains("rig mode") && log.contains("EVENT"),
        "the pass must log the detected EVENT mode: {log}"
    );
    assert!(
        log.contains("skip: rig in EVENT mode — TEST-premise verdict, no page"),
        "each box's TEST-premise verdict must be logged report-only in EVENT mode: {log}"
    );
    assert!(
        !log.contains("WOULD alert"),
        "EVENT mode must never page a TEST-premise verdict: {log}"
    );
}

#[test]
fn event_mode_does_not_page_even_the_grey_box() {
    // Specifically the cam2 grey box (the DEAD_PORT candidate) is suppressed in EVENT mode.
    let log = run_driver(&mixed_dead_port_fleet(), RIG_EVENT, 2);
    assert!(
        !log.contains("WOULD alert: cam2"),
        "the grey cam2 box must not page in EVENT mode: {log}"
    );
}

#[test]
fn test_mode_still_pages_dead_port_unchanged() {
    // In provable TEST mode (cam2-painter.service enabled) the behaviour is byte-unchanged: the grey
    // box still confirms + pages DEAD_PORT after 2 passes. The gate must not silence a real fault.
    let log = run_driver(&mixed_dead_port_fleet(), RIG_TEST, 2);
    assert!(
        log.contains("rig mode") && log.contains("TEST"),
        "the pass must log the detected TEST mode: {log}"
    );
    assert!(
        log.contains("WOULD alert: cam2 CONFIRMED DEAD_PORT"),
        "TEST mode must still page a real DEAD_PORT: {log}"
    );
    assert!(
        !log.contains("skip: rig in EVENT mode"),
        "TEST mode must not emit the EVENT skip line: {log}"
    );
}

#[test]
fn unknown_mode_pages_dead_port_fail_safe() {
    // cam2 unreachable -> UNKNOWN mode -> behave exactly as today (page). An unreadable mode must
    // NEVER silence a real TEST-mode dead port.
    let log = run_driver(&mixed_dead_port_fleet(), RIG_UNKNOWN, 2);
    assert!(
        log.contains("rig mode") && log.contains("UNKNOWN"),
        "the pass must log the UNKNOWN mode: {log}"
    );
    assert!(
        log.contains("WOULD alert: cam2 CONFIRMED DEAD_PORT"),
        "UNKNOWN mode must page as today (fail-safe): {log}"
    );
    assert!(
        !log.contains("skip: rig in EVENT mode"),
        "UNKNOWN mode must not emit the EVENT skip line: {log}"
    );
}
