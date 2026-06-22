//! Behavioral guard for `scripts/dantesync-gate.sh` — the recording-E2E NTP+PTP precondition
//! gate (#7). The recording-based 4-node E2E measures cross-node per-hop latency and aligns
//! per-frame timestamps; those numbers are ONLY meaningful when every measured node is BOTH
//! NTP-synced AND PTP-locked (µs-grade fine servo, GM 10.77.9.184 up — not the ±1 ms NTP
//! sawtooth fallback). This gate runs FIRST and must FAIL FAST otherwise, so a meaningless run
//! never reaches the recording step.
//!
//! The gate REUSES the unit-tested pure parsers in clock-offset-guard.sh (tested separately in
//! tests/clock_offset_guard.rs); these tests pin the gate's own FLOW: its `node_verdict`
//! combiner (a node passes only when BOTH NTP and PTP pass) and its end-to-end exit-code
//! contract over Windows status FILES (the path that needs no live nodes), plus the live-SSH
//! branch failing closed when a Linux node is unreachable.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/dantesync-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the gate (its BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the gate as a subprocess; return (exit_code, stdout, stderr).
fn run_gate(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("run dantesync-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write `json` to a temp file and return its path (kept alive by the returned tempdir-like).
fn write_status(name: &str, json: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dante-gate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

#[test]
fn node_verdict_passes_only_when_both_ntp_and_ptp_pass() {
    // OK iff offset rc 0 AND ptp rc 0. A DRIFT/DEGRADED (rc 2) on EITHER => BAD (hard failure
    // dominates). An UNKNOWN (rc 3) with no hard failure => UNKNOWN. This is the core safety
    // property: a node never passes the gate on a half-check.
    let cases = [
        ("0", "0", "OK"),
        ("2", "0", "BAD"), // NTP drift
        ("0", "2", "BAD"), // PTP degraded
        ("2", "2", "BAD"),
        ("3", "0", "UNKNOWN"),
        ("0", "3", "UNKNOWN"),
        ("2", "3", "BAD"), // hard failure dominates unknown
    ];
    for (off, ptp, want) in cases {
        let out = run_sourced(
            "node_verdict \"$OFF\" \"$PTP\"",
            &[("OFF", off), ("PTP", ptp)],
        );
        assert_eq!(
            out.trim(),
            want,
            "node_verdict(off={off}, ptp={ptp}) must be {want}: {out:?}"
        );
    }
}

#[test]
fn gate_passes_when_a_windows_node_is_ntp_and_ptp_locked() {
    // A real locked strih status file (NTP 154us within bound + is_locked NANO) -> GATE PASS (0).
    let locked = "{\"gm_source_ip\":\"10.77.9.184\",\"is_locked\":true,\"ntp_offset_us\":154,\
                  \"mode\":\"NANO\",\"ntp_failed\":false}";
    let p = write_status("strih_ok", locked);
    let (code, stdout, _e) = run_gate(&[
        "--linux",
        "",
        "--win-status",
        &format!("strih={}", p.display()),
    ]);
    assert_eq!(code, 0, "locked+synced node must PASS: {stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn gate_fails_fast_when_a_node_is_ptp_degraded() {
    // is_locked=false (NTP-only sawtooth) must FAIL the gate with code 20, even though NTP is OK.
    let degraded = "{\"is_locked\":false,\"mode\":\"NTP\",\"ntp_offset_us\":154}";
    let p = write_status("stream_degraded", degraded);
    let (code, _o, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-status",
        &format!("stream={}", p.display()),
    ]);
    assert_eq!(
        code, 20,
        "PTP-degraded node must FAIL (20). stderr: {stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
}

#[test]
fn gate_fails_when_a_node_exceeds_the_offset_bound() {
    // is_locked NANO but NTP offset 50 ms >> 2 ms bound -> DRIFT -> FAIL (20).
    let drifted = "{\"is_locked\":true,\"mode\":\"NANO\",\"ntp_offset_us\":50000}";
    let p = write_status("strih_drift", drifted);
    let (code, _o, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-status",
        &format!("strih={}", p.display()),
    ]);
    assert_eq!(
        code, 20,
        "NTP-drifted node must FAIL (20). stderr: {stderr}"
    );
}

#[test]
fn gate_incomplete_when_a_windows_status_file_is_missing() {
    // No status file for a Windows node -> UNKNOWN -> exit 11 (incomplete, NOT a silent pass).
    let (code, _o, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-status",
        "stream=/tmp/definitely-not-a-real-dante-status.json",
    ]);
    assert_eq!(
        code, 11,
        "missing status -> INCOMPLETE (11). stderr: {stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
}

#[test]
fn gate_with_no_nodes_refuses_to_pass() {
    // Zero nodes to check must be a usage error (1), never "all clear".
    let (code, _o, stderr) = run_gate(&["--linux", ""]);
    assert_eq!(code, 1, "zero nodes -> usage error (1). stderr: {stderr}");
    assert!(
        stderr.contains("zero nodes") || stderr.contains("no nodes"),
        "stderr: {stderr}"
    );
}

#[test]
fn help_describes_the_ntp_and_ptp_requirement() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("ptp") && low.contains("ntp"),
        "help must describe BOTH the NTP and PTP requirement: {stdout}"
    );
}
