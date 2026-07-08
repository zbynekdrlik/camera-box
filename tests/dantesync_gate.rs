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

/// Run the gate as a subprocess WITH extra env (the #608 fixture-injection seam). Mirrors
/// clock_offset_painter_gate.rs's run_gate(args, extra_env).
fn run_gate_env(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run dantesync-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a `journalctl -o short-iso` DanteSync journal fixture and return its path (#608). Mirrors
/// clock_offset_painter_gate.rs's write_journal/write_journal_stale/write_journal_no_offset —
/// same "caller pre-fetches the status to a file" pattern, extended to dantesync-gate.sh's Linux
/// SSH-gather path (which, unlike the painter gate, ALSO grades the PTP-lock signal from the same
/// journal text).
fn write_dante_journal(name: &str, lines: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dante-gate-journal-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.log"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(lines.as_bytes()).unwrap();
    path
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
    // #595: help must document the freshness knob, not just the offset bound -- a stale-but-
    // in-bound reading is graded differently than a plain out-of-bound one (never a silent OK).
    assert!(
        low.contains("fresh"),
        "help must describe the offset FRESHNESS requirement (#550/#595), not just the bound: {stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// #608 — the Linux SSH-gather path has no offline fixture-injection seam, unlike
// clock-offset-painter-gate.sh's DEV1_DANTE_JOURNAL/PAINTER_DANTE_JOURNAL. These tests feed
// pre-captured journald text via DANTESYNC_GATE_LINUX_JOURNAL_<NAME> (NAME uppercased) instead of
// live SSH, and prove the full ok/drift/stale/absent -> gate exit-code mapping end-to-end for a
// Linux node -- previously only exercised indirectly via the dantesync_offset_verdict unit tests
// in tests/clock_offset_guard.rs, never at the GATE (case-statement -> rc_off -> node_verdict ->
// exit code) level the way the Windows status-file path already was above.
// ---------------------------------------------------------------------------------------------

#[test]
fn gate_passes_on_a_linux_node_with_ok_fresh_offset_and_ptp_locked() {
    // Fresh in-bound offset (+150us, default bound 2000us) followed by a NANO servo line (PTP
    // LOCKED, still the most recent event) -> node OK -> GATE PASS (0).
    let j = write_dante_journal(
        "cam1_ok",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[(
            "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
            &j.display().to_string(),
        )],
    );
    assert_eq!(
        code, 0,
        "fresh in-bound offset + PTP LOCKED must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(stdout.contains("NTP OK"), "stdout: {stdout}");
}

#[test]
fn gate_fails_on_a_linux_node_with_a_fresh_offset_that_exceeds_the_bound() {
    // The exact cam5/6 #591 magnitude (+5280959us) as a FRESH reading -> DRIFT -> BAD -> FAIL (20).
    let j = write_dante_journal(
        "cam1_drift",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+5280959us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[(
            "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
            &j.display().to_string(),
        )],
    );
    assert_eq!(
        code, 20,
        "fresh out-of-bound offset must FAIL (20). stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(stdout.contains("NTP DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_incomplete_on_a_linux_node_with_a_stale_offset_line() {
    // #550/#595-class staleness: the ONLY [NTP] offset: line sits ~1h behind the newer [PTP]
    // servo lines -- well past the default 300s freshness window -- even though the VALUE is
    // in-bound. Must be graded STALE (not OK), which is an UNKNOWN -> GATE INCOMPLETE (11).
    let j = write_dante_journal(
        "cam1_stale",
        "2026-07-08T09:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[(
            "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
            &j.display().to_string(),
        )],
    );
    assert_eq!(
        code, 11,
        "a stale-but-in-bound offset line must be INCOMPLETE (11), never a silent PASS. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("NTP STALE"), "stdout: {stdout}");
}

#[test]
fn gate_incomplete_on_a_linux_node_with_no_ntp_offset_line_at_all() {
    // No `[NTP] offset:` line anywhere in the journal -> "absent" -> the case statement's `*)`
    // catch-all -> NTP UNKNOWN -> GATE INCOMPLETE (11). (The `*)` fallthrough already fails
    // closed on a typo'd case label per #608's own scope text -- this pins the REAL "absent"
    // verdict reaches the identical safe outcome, not just the typo-safety net.)
    let j = write_dante_journal(
        "cam1_absent",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[(
            "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
            &j.display().to_string(),
        )],
    );
    assert_eq!(
        code, 11,
        "no [NTP] offset line at all must be INCOMPLETE (11), never a silent PASS. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("NTP UNKNOWN"), "stdout: {stdout}");
}

#[test]
fn gate_linux_journal_override_is_keyed_per_node_name_not_a_single_shared_var() {
    // Two Linux nodes, each with its OWN override var -- proves the seam is keyed BY NODE NAME
    // (mirrors dantesync-gate.sh's existing --win-status NAME=FILE per-node convention) rather
    // than a single global override that would make a 2-node gate untestable.
    let cam1_ok = write_dante_journal(
        "cam1_multi_ok",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let cam2_drift = write_dante_journal(
        "cam2_multi_drift",
        "2026-07-08T10:00:00+02:00 cam2 dantesync[1]: [NTP] offset:+9999999us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 cam2 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61 cam2=10.77.9.62"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &cam1_ok.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM2",
                &cam2_drift.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "cam1 OK + cam2 DRIFT must still fail the whole gate (20). stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("cam1") && stdout.contains("NTP OK"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("cam2") && stdout.contains("NTP DRIFT"),
        "stdout: {stdout}"
    );
}
