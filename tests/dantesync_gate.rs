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

/// #686: a sentinel path that never exists on disk. Pointing a Linux node's
/// DANTESYNC_GATE_LINUX_HTTP_<NAME> override at this makes read_linux_node_http_status's `cat`
/// fail (silently, `2>/dev/null || true`) and return "" deterministically -- forcing the
/// journal-fallback path WITHOUT any live network dependency. Every pre-#686 Linux-journal-only
/// test below must pass this (or an equivalent override) so it never attempts a REAL curl to the
/// live rig (cam1/cam2/imag-nb on the LAN) when run locally on a LAN-connected dev box.
const NO_HTTP: &str = "/nonexistent-686-linux-http-fixture";

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

// ---------------------------------------------------------------------------------------------
// #835 — the `--win-status NAME=FILE` file-relay path (the Windows-node status-pipe JSON a
// human/agent pre-fetched via the win-* MCP) is REMOVED outright, not guarded. It had ZERO live
// callers left in this repo (recording-e2e.sh has passed only --win-http since #648) and was
// deliberately AGE-BLIND (no updated_ts/mtime check — the exact "load a stale clock reading"
// hazard a stale runbook could walk an operator into, #598/#835). The --win-http path already
// covers the identical two Windows nodes with a strictly better mechanism (no MCP, no pre-fetch,
// DOES grade freshness) — the four tests this block replaces
// (gate_passes_when_a_windows_node_is_ntp_and_ptp_locked / gate_fails_fast_when_a_node_is_ptp_
// degraded / gate_fails_when_a_node_exceeds_the_offset_bound /
// gate_incomplete_when_a_windows_status_file_is_missing) had their coverage duplicated 1:1 by
// gate_passes_a_win_http_node_that_is_fresh_locked_and_in_bound /
// gate_fails_when_a_win_http_node_is_stale / gate_fails_when_a_win_http_node_updated_ts_field_is_
// absent / gate_fails_when_a_win_http_node_is_unreachable below, so nothing goes untested.
// ---------------------------------------------------------------------------------------------

#[test]
fn help_no_longer_documents_win_status_835() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        !stdout.contains("--win-status"),
        "the removed file-relay flag must no longer appear in --help: {stdout}"
    );
}

#[test]
fn win_status_flag_is_rejected_as_an_unknown_option_835() {
    // Passing the removed flag must fail the SAME way any other unrecognized flag does (usage
    // error, exit 1) -- never silently accepted, never treated as a node with no data.
    let (code, _o, stderr) = run_gate(&["--linux", "", "--win-status", "stream=/tmp/x.json"]);
    assert_eq!(
        code, 1,
        "--win-status must be an unrecognized option now. stderr: {stderr}"
    );
    assert!(
        stderr.contains("unknown option"),
        "stderr should name it an unknown option: {stderr}"
    );
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
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // #686: force journal fallback, no live curl
        ],
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
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // #686: force journal fallback, no live curl
        ],
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
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // #686: force journal fallback, no live curl
        ],
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
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // #686: force journal fallback, no live curl
        ],
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
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // #686: force journal fallback, no live curl
            ("DANTESYNC_GATE_LINUX_HTTP_CAM2", NO_HTTP),
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

// ---------------------------------------------------------------------------------------------
// #648 — the --win-http path: strih/stream queried LIVE over HTTP from dantesync#47's own
// network status endpoint (http://<box>:8898/status), instead of a human/agent pre-fetching the
// status pipe's JSON to a file (--win-status above). No live boxes in CI, so these tests feed
// the REAL captured payload shape (curled from strih/stream on 2026-07-10) via
// DANTESYNC_GATE_WIN_HTTP_<NAME> -- the same "caller pre-fetches to a file, keyed by node name"
// fixture-injection convention as DANTESYNC_GATE_LINUX_JOURNAL_<NAME> above (#608).
// ---------------------------------------------------------------------------------------------

/// Real strih (10.77.9.202) DanteSync HTTP status payload, curled 2026-07-10. NANO-locked,
/// ntp_offset_us=0 (well within any sane bound), fresh (updated_ts must be paired with a "now"
/// close to it by the caller in each test).
const HTTP_STRIH_OK: &str = "{\"offset_ns\":164707,\"drift_ppm\":-7.68,\"gm_source_ip\":\"10.77.9.184\",\
\"settled\":true,\"updated_ts\":1783647854,\"is_locked\":true,\"ntp_offset_us\":0,\"mode\":\"NANO\",\
\"ntp_failed\":false}";

/// Write `json` to a temp file and return its path -- the DANTESYNC_GATE_WIN_HTTP_<NAME> fixture.
fn write_win_http_fixture(name: &str, json: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dante-gate-http-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

#[test]
fn gate_passes_a_win_http_node_that_is_fresh_locked_and_in_bound() {
    // The gate computes "now" internally via `date +%s` (not injectable), so a fixture proving
    // the PASS path needs a genuinely current updated_ts -- unlike gate_fails_when_a_win_http_node_is_stale
    // below (which relies on a FIXED past capture staying stale forever), this one must be built
    // with the real wall clock at test-run time.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let fresh = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{now},\
         \"is_locked\":true,\"ntp_offset_us\":0,\"mode\":\"NANO\",\"ntp_failed\":false}}"
    );
    let p = write_win_http_fixture("strih_http_ok", &fresh);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "", "--win-http", "strih=10.77.9.202"],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "fresh+locked+in-bound --win-http node must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_win_http_node_is_stale() {
    // dantesync-gate.sh's own flow computes "now" internally via `date +%s`, so a fixture whose
    // updated_ts is deliberately far in the PAST (year ~2026-07-10 real capture, well behind
    // whenever this test actually runs) is always graded STALE against the real wall clock --
    // simulates the box's HTTP server serving a cached snapshot after dantesync itself died.
    let p = write_win_http_fixture("strih_http_stale", HTTP_STRIH_OK);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "", "--win-http", "strih=10.77.9.202"],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "a stale updated_ts must be INCOMPLETE (11), never a silent PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("NTP STALE"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_win_http_node_is_unreachable() {
    // No DANTESYNC_GATE_WIN_HTTP_STRIH override and no real box to curl (the TEST-ONLY
    // unroutable 192.0.2.0/24 range, RFC 5737 TEST-NET-1) -> UNREACHABLE -> UNKNOWN -> GATE
    // INCOMPLETE (11), never a silent pass. CLOCK_GUARD_HTTP_TIMEOUT=1 keeps the test fast
    // instead of waiting out the gate's real default (10s) connect timeout.
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "", "--win-http", "strih=192.0.2.1"],
        &[("CLOCK_GUARD_HTTP_TIMEOUT", "1")],
    );
    assert_eq!(
        code, 11,
        "unreachable --win-http node must be INCOMPLETE (11). stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("UNREACHABLE"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_win_http_node_updated_ts_field_is_absent() {
    // A payload missing "updated_ts" entirely (an incompatible/older endpoint) must be UNKNOWN,
    // never silently graded on offset/PTP alone with no freshness proof at all.
    let no_ts = "{\"ntp_offset_us\":0,\"is_locked\":true,\"mode\":\"NANO\"}";
    let p = write_win_http_fixture("strih_http_no_ts", no_ts);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "", "--win-http", "strih=10.77.9.202"],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "no updated_ts field must be INCOMPLETE (11). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("NTP UNKNOWN"), "stdout: {stdout}");
}

#[test]
fn gate_with_only_win_http_nodes_still_refuses_zero_nodes() {
    // --linux "" and --win-status absent, but --win-http also absent -> still zero nodes -> usage
    // error (1). Proves the "no nodes" guard was extended to cover --win-http, not just the two
    // pre-existing arrays.
    let (code, _o, stderr) = run_gate(&["--linux", ""]);
    assert_eq!(code, 1, "zero nodes -> usage error (1). stderr: {stderr}");
}

#[test]
fn help_describes_win_http_and_its_freshness_requirement() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("--win-http"),
        "help must document --win-http (#648): {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("updated_ts"),
        "help must document the --win-http freshness field (updated_ts, #648): {stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// #686 — regression introduced by #679's dantesync log throttling (v1.8.19 fleet-wide):
// ptp_locked_from_journal() decides LOCKED vs DEGRADED by comparing the journal POSITION of the
// last `[PTP] (NANO|LOCK) Drift:` servo line against the last `[NTP] offset:` line -- sound when
// servo lines tick ~1/s, but after the 1-in-30 throttle they land at nearly the SAME cadence as
// the NTP offset lines, so whichever happened to log last wins: a genuinely LOCKED node can read
// DEGRADED with roughly coin-flip odds. The fix: for Linux nodes, try dantesync#47's own network
// status endpoint (http://<ip>:8898/status, the SAME authoritative signal --win-http already
// reads for Windows boxes, deployed fleet-wide and verified responding on cam1-cam6, 2026-07-11)
// FIRST; the journal parser is now a FALLBACK for a node whose HTTP endpoint is unreachable/
// disabled -- never a second opinion when HTTP answered (a reachable-but-STALE HTTP payload must
// fail the gate, not silently fall through to a possibly-misleading journal read).
// ---------------------------------------------------------------------------------------------

/// Write `json` to a temp file and return its path -- the DANTESYNC_GATE_LINUX_HTTP_<NAME>
/// fixture (mirrors write_win_http_fixture above, #648).
fn write_linux_http_fixture(name: &str, json: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("dante-gate-linux-http-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

#[test]
fn gate_passes_a_throttled_linux_node_via_http_status_first() {
    // The exact #686 regression: a THROTTLED journal whose last [NTP] offset: line is NEWER
    // (later position) than the last [PTP] NANO Drift: servo line -- ptp_locked_from_journal
    // alone would read this as DEGRADED even though the node is genuinely locked. A healthy,
    // fresh, in-bound HTTP payload must make the gate PASS regardless of what the (misleading)
    // journal says, because HTTP is now tried FIRST.
    let throttled_journal = write_dante_journal(
        "cam1_throttled",
        "2026-07-11T10:00:00+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n\
2026-07-11T10:00:29+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n",
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let http_ok = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{now},\
         \"is_locked\":true,\"ntp_offset_us\":150,\"mode\":\"NANO\",\"ntp_failed\":false}}"
    );
    let http_fixture = write_linux_http_fixture("cam1_throttled_http", &http_ok);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &throttled_journal.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &http_fixture.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "#686: a throttled journal that WOULD read DEGRADED must not sink the gate when the \
         HTTP status is healthy+fresh+locked -- HTTP must be tried FIRST. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_linux_node_http_payload_is_stale_even_if_its_journal_looks_fine() {
    // The HTTP payload is reachable but STALE (old updated_ts) -- must FAIL the gate (never
    // silently fall back to the journal, even though the journal fixture here is healthy/LOCKED).
    // Proves: once HTTP answers, it is authoritative -- fallback is ONLY for HTTP being
    // unreachable, never for HTTP being stale.
    let healthy_journal = write_dante_journal(
        "cam1_healthy_but_http_stale",
        "2026-07-11T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-11T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    // A genuinely old capture (well behind "now" no matter when this test runs).
    let stale_http = "{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":1783647854,\
                       \"is_locked\":true,\"ntp_offset_us\":0,\"mode\":\"NANO\",\"ntp_failed\":false}";
    let http_fixture = write_linux_http_fixture("cam1_http_stale", stale_http);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &healthy_journal.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &http_fixture.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 11,
        "#686: a STALE Linux HTTP payload must be INCOMPLETE (11), never silently pass by \
         falling back to a healthy-looking journal. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("NTP STALE"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_linux_node_http_payload_is_fresh_but_ptp_degraded() {
    // Review follow-up (PR #692): the new Linux-HTTP branch's plain FAIL path — a fresh,
    // reachable payload whose daemon itself reports is_locked:false (NTP-only sawtooth) must
    // FAIL the gate with 20 through the NEW branch, exactly like the --win-http equivalent
    // (gate_fails_fast_when_a_node_is_ptp_degraded above). The underlying parsers are already
    // unit-tested in clock_offset_guard.rs; this pins the WIRING (rc_ptp -> node_verdict ->
    // exit code) for the Linux-HTTP path specifically.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let degraded = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":false,\"updated_ts\":{now},\
         \"is_locked\":false,\"ntp_offset_us\":150,\"mode\":\"NTP\",\"ntp_failed\":false}}"
    );
    let p = write_linux_http_fixture("cam1_http_degraded", &degraded);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[("DANTESYNC_GATE_LINUX_HTTP_CAM1", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "#686: a fresh Linux HTTP payload with is_locked:false must FAIL the gate (20). \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(stdout.contains("PTP DEGRADED"), "stdout: {stdout}");
}

#[test]
fn gate_fails_when_a_linux_node_http_payload_is_fresh_but_offset_drifted() {
    // Review follow-up (PR #692): the other plain FAIL path through the new Linux-HTTP branch —
    // locked servo but an out-of-bound NTP offset (50 ms >> the 2 ms default bound) must DRIFT
    // -> FAIL (20), mirroring gate_fails_when_a_node_exceeds_the_offset_bound's --win-status
    // equivalent.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let drifted = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{now},\
         \"is_locked\":true,\"ntp_offset_us\":50000,\"mode\":\"NANO\",\"ntp_failed\":false}}"
    );
    let p = write_linux_http_fixture("cam1_http_drift", &drifted);
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[("DANTESYNC_GATE_LINUX_HTTP_CAM1", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "#686: a fresh Linux HTTP payload with an out-of-bound offset must FAIL the gate (20). \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_falls_back_to_the_journal_when_a_linux_node_http_endpoint_is_unreachable() {
    // No DANTESYNC_GATE_LINUX_HTTP_CAM1 override and an unroutable IP (TEST-NET-1) -> HTTP
    // unreachable -> FALL BACK to the journal fixture, which is healthy -> PASS. Proves the
    // fallback path still works for a node whose HTTP endpoint is genuinely down/disabled.
    let healthy_journal = write_dante_journal(
        "cam1_http_unreachable_journal_ok",
        "2026-07-11T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-11T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=192.0.2.1"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &healthy_journal.display().to_string(),
            ),
            ("CLOCK_GUARD_HTTP_TIMEOUT", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "#686: an unreachable Linux HTTP endpoint must FALL BACK to the (healthy) journal, not \
         fail the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn help_describes_linux_nodes_trying_http_first() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("linux") && low.contains("http") && low.contains("first"),
        "#686: help must specifically describe that LINUX nodes now try the HTTP status \
         endpoint FIRST (the journal is a fallback) -- not just the pre-existing generic \
         mentions of 'http'/'fallback' unrelated to this: {stdout}"
    );
}

#[test]
fn gate_linux_journal_override_maps_a_hyphenated_node_name_to_a_valid_env_var() {
    // #608 review follow-up: the NAME -> ENV_VAR mapping uppercases AND maps "-" to "_" (a bare
    // uppercase of "imag-nb" would be "IMAG-NB", not a valid shell variable name). This node isn't
    // one of today's real Linux gate nodes (cam1/cam2), but the mapping must not silently break on
    // any hyphenated name a future --linux invocation could pass.
    let j = write_dante_journal(
        "imag_nb_ok",
        "2026-07-08T10:00:00+02:00 imag-nb dantesync[1]: [NTP] offset:+150us (threshold:520us, adaptive)\n\
2026-07-08T10:00:05+02:00 imag-nb dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "imag-nb=10.77.9.182"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_IMAG_NB",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_IMAG_NB", NO_HTTP), // #686: force journal fallback, no live curl
        ],
    );
    assert_eq!(
        code, 0,
        "DANTESYNC_GATE_LINUX_JOURNAL_IMAG_NB must be honored for node name \"imag-nb\". \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}
