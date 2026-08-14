//! #860 — pure-function guard for `scripts/lib/optical-chain-health.sh`, the SHARED decision core
//! for the cam2 optical-injection-leg (painter → cam2 monitor → cam1 camera) health check.
//!
//! Root cause (issue 860, live incident 2026-08-14): a chain of FAILED E2E runs whose cleanups each
//! logged `WARNING #712: cam2/painter restore failed/timed out` left the painter DEAD — cam2's
//! monitor pitch black — and the next gate run's optical hop reported UNAVAILABLE / breached the
//! undecodable floor, with NO alert firing anywhere. A dead painter must page immediately (the
//! standing rig-degradation-alert rule) AND fail-fast the harness before it burns a ~40-min run.
//!
//! This file pins the PURE core the TWO dev1-side surfaces share — the standing
//! `optical-chain-alert-watchdog.sh` and the recording-e2e.sh [0/8] preflight — so the
//! TEST/EVENT-mode discriminator, the optical-probe classification, and the alert decision are
//! correct regardless of any live rig.
//!
//! Same convention as `tests/harness_imag_power_envelope_1040.rs`: source the REAL lib (source-only,
//! no side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/optical-chain-health.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
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

// ---------------------------------------------------------------------------------------------
// lib shape — the four pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "optical_chain_classify_nonblack_probe",
        "optical_chain_painter_expected_from_snapshot",
        "optical_chain_painter_alive_from_snapshot",
        "optical_chain_alert_condition",
        "optical_chain_painter_probe_remote_snippet",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// optical_chain_classify_nonblack_probe <rc> <output> -> OK | BLACK | UNKNOWN
// ---------------------------------------------------------------------------------------------
#[test]
fn classify_probe_rc0_is_ok() {
    assert_eq!(
        stdout_of("optical_chain_classify_nonblack_probe 0 \"PASS: 10.77.9.202 program scene 'X' NON-BLACK\""),
        "OK"
    );
}

#[test]
fn classify_probe_black_message_is_black() {
    // assert-program-nonblack raises SystemExit with "renders BLACK" on a genuine black program.
    assert_eq!(
        stdout_of("optical_chain_classify_nonblack_probe 1 \"[obs] 10.77.9.202: #901 chain-verify self-check FAIL — program scene 'X' renders BLACK (luma peak 0)\""),
        "BLACK"
    );
}

#[test]
fn classify_probe_other_error_is_unknown() {
    // A WS/connectivity failure (not a black verdict) must be UNKNOWN, never BLACK — a probe we
    // could not run is "nothing to decide", not proof of a dark monitor.
    assert_eq!(
        stdout_of("optical_chain_classify_nonblack_probe 1 \"ConnectionRefusedError: could not reach OBS WebSocket\""),
        "UNKNOWN"
    );
    assert_eq!(stdout_of("optical_chain_classify_nonblack_probe 1 \"\""), "UNKNOWN");
}

// ---------------------------------------------------------------------------------------------
// painter_expected / painter_alive from a cam2 probe snapshot
//   snapshot lines: PID_PRESENT|0|1  PID_ALIVE|0|1  SVC_ENABLED|0|1  SVC_ACTIVE|0|1
// ---------------------------------------------------------------------------------------------
const SNAP_TEST_HEALTHY: &str = "PID_PRESENT|1\nPID_ALIVE|1\nSVC_ENABLED|0\nSVC_ACTIVE|0\n";
const SNAP_TEST_DEAD: &str = "PID_PRESENT|1\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0\n";
const SNAP_SVC_HEALTHY: &str = "PID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|1\nSVC_ACTIVE|1\n";
const SNAP_SVC_DEAD: &str = "PID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|1\nSVC_ACTIVE|0\n";
const SNAP_EVENT: &str = "PID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0\n";

fn expected(snap: &str) -> String {
    stdout_of(&format!(
        "printf '%s' {} | optical_chain_painter_expected_from_snapshot \"$(cat)\"",
        shell_quote(snap)
    ))
}
fn alive(snap: &str) -> String {
    stdout_of(&format!(
        "printf '%s' {} | optical_chain_painter_alive_from_snapshot \"$(cat)\"",
        shell_quote(snap)
    ))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[test]
fn painter_expected_is_pid_present_or_service_enabled() {
    assert_eq!(expected(SNAP_TEST_HEALTHY), "1"); // rig-mode painter pidfile present
    assert_eq!(expected(SNAP_TEST_DEAD), "1"); // stale pidfile (crashed painter) still = expected
    assert_eq!(expected(SNAP_SVC_HEALTHY), "1"); // permanent service enabled
    assert_eq!(expected(SNAP_SVC_DEAD), "1"); // service enabled but not active = still expected
    assert_eq!(expected(SNAP_EVENT), "0"); // EVENT mode: no pidfile, service disabled -> not expected
}

#[test]
fn painter_alive_is_pid_alive_or_service_active() {
    assert_eq!(alive(SNAP_TEST_HEALTHY), "1");
    assert_eq!(alive(SNAP_TEST_DEAD), "0"); // pidfile present but PID dead
    assert_eq!(alive(SNAP_SVC_HEALTHY), "1");
    assert_eq!(alive(SNAP_SVC_DEAD), "0"); // service enabled but inactive
    assert_eq!(alive(SNAP_EVENT), "0");
}

// ---------------------------------------------------------------------------------------------
// optical_chain_alert_condition <painter_expected> <painter_alive> <optical>
//   -> skip | alert:PAINTER-DEAD | alert:OPTICAL-BLACK | healthy | healthy-unverified
// ---------------------------------------------------------------------------------------------
fn cond(exp: &str, alv: &str, opt: &str) -> String {
    stdout_of(&format!("optical_chain_alert_condition {exp} {alv} {opt}"))
}

#[test]
fn condition_skip_when_no_painter_expected() {
    // EVENT mode / never-tested: a dark monitor is CORRECT -> never alert, regardless of optical.
    assert_eq!(cond("0", "0", "BLACK"), "skip");
    assert_eq!(cond("0", "0", "OK"), "skip");
    assert_eq!(cond("0", "1", "UNKNOWN"), "skip");
}

#[test]
fn condition_painter_dead_alerts() {
    // TEST mode, painter expected but dead -> the 2026-08-14 incident. Alert regardless of optical
    // (a dead painter needs no OBS confirmation).
    assert_eq!(cond("1", "0", "UNKNOWN"), "alert:PAINTER-DEAD");
    assert_eq!(cond("1", "0", "BLACK"), "alert:PAINTER-DEAD");
    assert_eq!(cond("1", "0", "OK"), "alert:PAINTER-DEAD");
}

#[test]
fn condition_optical_black_alerts_even_when_painter_alive() {
    // The #901/#754 class: process alive, pidfile correct, but the rendered monitor is BLACK.
    assert_eq!(cond("1", "1", "BLACK"), "alert:OPTICAL-BLACK");
}

#[test]
fn condition_healthy_when_alive_and_nonblack() {
    assert_eq!(cond("1", "1", "OK"), "healthy");
}

#[test]
fn condition_unverified_when_alive_but_optical_unreadable() {
    // Painter alive but OBS-WS unreachable: nothing to decide about the optical read -> never a
    // false alert, but not a clean "healthy" either.
    assert_eq!(cond("1", "1", "UNKNOWN"), "healthy-unverified");
}

// ---------------------------------------------------------------------------------------------
// the remote-probe snippet builder emits the 4 markers and reads the right pidfile + service
// ---------------------------------------------------------------------------------------------
#[test]
fn remote_snippet_emits_the_four_markers_and_reads_the_right_sources() {
    let snip = stdout_of("optical_chain_painter_probe_remote_snippet /run/rig-painter.pid cam2-painter.service");
    for marker in ["PID_PRESENT|", "PID_ALIVE|", "SVC_ENABLED|", "SVC_ACTIVE|"] {
        assert!(snip.contains(marker), "snippet missing marker {marker}:\n{snip}");
    }
    assert!(snip.contains("/run/rig-painter.pid"), "snippet must read the painter pidfile:\n{snip}");
    assert!(snip.contains("cam2-painter.service"), "snippet must read the permanent service:\n{snip}");
}
