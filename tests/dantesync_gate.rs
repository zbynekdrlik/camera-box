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

#[test]
fn gate_fails_on_a_linux_journal_node_with_scattered_but_in_bound_median_837() {
    // #837 -- the journal-fallback twin of #836's HTTP spread check. Three FRESH offset samples
    // whose MEDIAN (+50us) sits inside the 2000us bound but whose SPREAD (2540us) exceeds the
    // 2000us stability bound. Pre-#837 the journal fallback graded the median alone and rated this
    // node NTP OK -> GATE PASS, silently. It must now grade the spread too -> UNSTABLE -> BAD ->
    // GATE FAIL (20). PTP is LOCKED (NANO the newest line) so only the NTP spread verdict decides.
    let j = write_dante_journal(
        "cam1_scattered_837",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+50us (threshold:520us, adaptive)\n\
2026-07-08T10:00:10+02:00 cam1 dantesync[1]: [NTP] offset:+2500us (threshold:520us, adaptive)\n\
2026-07-08T10:00:20+02:00 cam1 dantesync[1]: [NTP] offset:-40us (threshold:520us, adaptive)\n\
2026-07-08T10:00:25+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP), // force journal fallback, no live curl
        ],
    );
    assert_eq!(
        code, 20,
        "a scattered-but-in-bound-median journal node must now FAIL the gate (20), not pass \
         silently. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(
        stdout.contains("UNSTABLE"),
        "the journal-fallback report must name the UNSTABLE verdict: {stdout}"
    );
}

#[test]
fn gate_fails_drift_unstable_on_a_linux_journal_node_837() {
    // #837: median OUT of bound (+2600us) AND spread (2600us) past the 2000us stability bound ->
    // "drift_unstable" -> BAD -> GATE FAIL (20), reported as NTP DRIFT+UNSTABLE. Pins that the
    // both-fail verdict reaches the same hard-fail exit as plain drift/unstable at the gate level.
    let j = write_dante_journal(
        "cam1_drift_unstable_837",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+2600us (threshold:520us, adaptive)\n\
2026-07-08T10:00:10+02:00 cam1 dantesync[1]: [NTP] offset:+5000us (threshold:520us, adaptive)\n\
2026-07-08T10:00:20+02:00 cam1 dantesync[1]: [NTP] offset:+2400us (threshold:520us, adaptive)\n\
2026-07-08T10:00:25+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP),
        ],
    );
    assert_eq!(
        code, 20,
        "a drift+unstable journal node must FAIL the gate (20). stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(
        stdout.contains("DRIFT+UNSTABLE"),
        "the journal-fallback report must name the DRIFT+UNSTABLE verdict: {stdout}"
    );
}

#[test]
fn gate_still_passes_a_linux_journal_node_with_a_tight_in_bound_spread_837() {
    // Non-regression companion: a healthy node whose fresh samples are tight (spread 130us) and
    // in-bound (median +50us) must STILL pass the journal fallback -- the spread check must not
    // false-fail a genuinely healthy multi-sample journal.
    let j = write_dante_journal(
        "cam1_tight_837",
        "2026-07-08T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+50us (threshold:520us, adaptive)\n\
2026-07-08T10:00:10+02:00 cam1 dantesync[1]: [NTP] offset:+100us (threshold:520us, adaptive)\n\
2026-07-08T10:00:20+02:00 cam1 dantesync[1]: [NTP] offset:-30us (threshold:520us, adaptive)\n\
2026-07-08T10:00:25+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
    );
    let (code, stdout, stderr) = run_gate_env(
        &["--linux", "cam1=10.77.9.61"],
        &[
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", NO_HTTP),
        ],
    );
    assert_eq!(
        code, 0,
        "a tight in-bound multi-sample journal node must still PASS (0). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(stdout.contains("NTP OK"), "stdout: {stdout}");
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
    // #836: the gate now samples each node multiple times and requires a MINIMUM number of
    // DISTINCT (by updated_ts) samples before it will grade at all. A STATIC single-value fixture
    // never varies its updated_ts across repeated reads, so it can never satisfy the production
    // default (min-distinct=3) -- that is deliberate (see
    // gate_fails_a_win_http_node_with_too_few_distinct_samples_by_default below). This test is
    // specifically about the ORIGINAL single-good-read concern (fresh+locked+in-bound), so it
    // overrides --samples 1 --min-distinct 1 (one read is enough to grade) and --window-s 0
    // (no real sampling delay needed for a single read).
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 keeps this test fast (no real sampling
    // delay, and --min-distinct must never exceed --samples) -- the staleness check runs on the
    // LAST gathered payload regardless of sample count, so a single read is sufficient.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 keeps this test fast (--min-distinct must
    // never exceed --samples); the absent-updated_ts check runs on the LAST gathered payload
    // regardless of sample count.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 -- this test is about the HTTP-vs-journal
    // precedence (#686), not multi-sampling; a single good read is sufficient to grade PASS.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 keeps this test fast (--min-distinct must
    // never exceed --samples); staleness is graded on the LAST gathered payload regardless of
    // sample count.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 (--min-distinct must never exceed
    // --samples) -- PTP-lock grading is independent of offset sampling and still reads from the
    // LAST gathered payload, so a single read is sufficient here.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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
    // #836: --samples 1 --min-distinct 1 --window-s 0 -- a single grossly out-of-bound read is
    // sufficient to reach a drift verdict; without the override, a STATIC single-value fixture
    // could never satisfy the production default min-distinct (3) and would grade INSUFFICIENT
    // instead of DRIFT -- this test is specifically about the drift-detection wiring.
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
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

// ---------------------------------------------------------------------------------------------
// #836 — a SINGLE pipe-json read is close to a coin flip on a noisy node (live data: 2/22 stream
// box reads land inside the 2000us bound). The gate now samples each --win-http / Linux-HTTP-
// first node MULTIPLE times over a short window and grades the MEDIAN (against the existing,
// UNCHANGED bound) AND the SPREAD (a NEW stability check, #836 point 3) of the DISTINCT samples.
// ---------------------------------------------------------------------------------------------

/// Write an EXECUTABLE fixture script that returns a DIFFERENT response on each successive
/// invocation -- unlike write_win_http_fixture/write_linux_http_fixture's static `cat` of one
/// fixed file, this exercises gather_http_samples' ability to observe genuinely varying reads
/// across N samples, entirely offline (no network, no real sleep needed since --window-s 0
/// skips spacing). `responses` is the ordered list of raw JSON payloads to return, one per call;
/// calls beyond the list length keep returning the LAST response, so a caller need not know the
/// exact number of times it will be invoked.
fn write_multi_read_fixture(name: &str, responses: &[String]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dante-gate-multi-http-test-{}-{}",
        std::process::id(),
        name
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let responses_path = dir.join("responses.txt");
    let mut body = responses.join("\n");
    body.push('\n');
    std::fs::write(&responses_path, body).unwrap();
    let counter_path = dir.join("counter.txt");
    std::fs::write(&counter_path, "0").unwrap();
    let script_path = dir.join("fixture.sh");
    let script = format!(
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         counter_file=\"{counter}\"\n\
         responses_file=\"{responses}\"\n\
         n=$(wc -l < \"$responses_file\")\n\
         i=$(cat \"$counter_file\" 2>/dev/null || echo 0)\n\
         i=$((i + 1))\n\
         echo \"$i\" > \"$counter_file\"\n\
         [ \"$i\" -gt \"$n\" ] && i=\"$n\"\n\
         sed -n \"${{i}}p\" \"$responses_file\"\n",
        counter = counter_path.display(),
        responses = responses_path.display(),
    );
    std::fs::write(&script_path, &script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();
    script_path
}

/// A DanteSync HTTP status-pipe JSON payload with the given (updated_ts, ntp_offset_us),
/// otherwise healthy/locked -- the shape gather_http_samples/sampled_offset_check consume.
fn http_status(ts: u64, offset_us: i64) -> String {
    format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{ts},\
         \"is_locked\":true,\"ntp_offset_us\":{offset_us},\"mode\":\"NANO\",\"ntp_failed\":false}}"
    )
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn help_describes_the_new_multi_sample_flags_836() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    for flag in [
        "--samples",
        "--window-s",
        "--min-distinct",
        "--stability-us",
    ] {
        assert!(
            stdout.contains(flag),
            "#836: help must document the new sampling flag {flag}: {stdout}"
        );
    }
    let low = stdout.to_lowercase();
    assert!(
        low.contains("median") && low.contains("spread"),
        "#836: help must describe that the gate now grades both the median AND the spread of \
         multiple samples: {stdout}"
    );
}

#[test]
fn gate_fails_a_win_http_node_with_too_few_distinct_samples_by_default() {
    // NO --samples / --min-distinct override -- this proves the PRODUCTION DEFAULTS themselves
    // are strictly stricter than the old single-read gate: a fixture that never varies its
    // updated_ts (the exact shape of a node whose refresh interval is longer than the sampling
    // window, or a stuck/cached HTTP server) can never reach the default min-distinct and must
    // NEVER silently pass, even though the one value it does return is comfortably in-bound
    // (#836 point 5, second half). --window-s 0 only removes the real sampling DELAY -- min-
    // distinct and sample-count stay at their real production defaults.
    let now = now_epoch();
    let fresh = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{now},\
         \"is_locked\":true,\"ntp_offset_us\":50,\"mode\":\"NANO\",\"ntp_failed\":false}}"
    );
    let p = write_win_http_fixture("strih_http_static_default_sampling", &fresh);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "#836: a node whose reads never vary (updated_ts never advances) must be INCOMPLETE (11) \
         under the real default min-distinct, never a silent PASS on one lucky in-bound value. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    // #1130: scope this to the strih node's OWN verdict line (the first strih-prefixed line, the
    // offset grade printed before PTP/GM/PHASE-SLEW) AND anchor on the distinct-samples wording --
    // a bare stdout.contains("UNKNOWN") is now tautologically satisfied by the always-printed
    // report-first "strih PHASE-SLEW UNKNOWN" line, so it could no longer fail for its intended
    // reason (the insufficient-distinct verdict). Mirrors the node-line scoping at ~:2976.
    let strih_verdict = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("strih"))
        .unwrap_or("");
    assert!(
        strih_verdict.contains("UNKNOWN") && strih_verdict.contains("distinct sample"),
        "the strih node's OWN verdict must be the insufficient-distinct-samples UNKNOWN, not \
         satisfied by an unrelated report-only line: {stdout}"
    );
}

#[test]
fn gate_fails_a_win_http_node_whose_median_is_fine_but_samples_scatter_beyond_stability() {
    // THE new failure mode (#836 point 3): a node whose MEDIAN offset is comfortably in-bound
    // (200us, well under the 2000us default) but whose individual readings scatter across
    // +-20ms -- exactly the shape the issue describes ("a node scattering +-20ms around a median
    // of 200us must also fail"). A single-read gate could never see this; it only ever grades
    // whichever ONE value it happened to draw, and several of these values ARE individually
    // in-bound.
    //
    // Uses "stream" (a CLIENT node), not "strih" -- #1014 made "strih" (the default
    // --ntp-master) grade on median+freshness ONLY, so a scattered-but-in-bound-median "strih"
    // fixture would now PASS instead of proving this generic full-grading stability check. See
    // gate_a_win_http_ntp_master_ignores_spread_but_still_grades_median_1014 below for the
    // master-specific counterpart of this exact fixture shape.
    let base = now_epoch();
    let responses = vec![
        http_status(base, -19800),
        http_status(base + 1, 20100),
        http_status(base + 2, 200),
        http_status(base + 3, -19500),
        http_status(base + 4, 19900),
    ];
    let p = write_multi_read_fixture("stream_unstable", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            // #1014 review follow-up: only "stream" is configured, no "strih" -- opt OUT of the
            // master-name validation (this test cares about generic client grading only).
            "--ntp-master",
            "",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "#836: a scattered-but-in-bound-median node must FAIL the gate (20) on the NEW stability \
         check -- never any easier to pass than a plain DRIFT. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    assert!(
        stdout.contains("UNSTABLE"),
        "stdout must name the failure as instability, not drift, so a red says which kind of \
         bad it is (#836 point 4): {stdout}"
    );
}

#[test]
fn gate_passes_a_win_http_node_with_enough_stable_distinct_samples() {
    // The full multi-sample GREEN path end-to-end: several DISTINCT (differing updated_ts),
    // tight, in-bound readings must PASS, proving gather_http_samples + sampled_offset_check
    // wire together correctly, not just the pure functions in isolation (already covered in
    // tests/clock_offset_guard.rs).
    let base = now_epoch();
    let responses = vec![
        http_status(base, 120),
        http_status(base + 1, 150),
        http_status(base + 2, 130),
        http_status(base + 3, 140),
        http_status(base + 4, 125),
    ];
    let p = write_multi_read_fixture("strih_stable", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "#836: several distinct, tight, in-bound samples must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        stdout.contains("OK") && stdout.contains("distinct samples"),
        "the status line must report the distinct-sample count: {stdout}"
    );
}

#[test]
fn gate_status_line_reports_median_and_spread_regardless_of_outcome() {
    // #836 point 4: "a red says which kind of bad it is" -- the status line must carry BOTH
    // numbers even on a FAILING verdict, not just on a pass.
    let base = now_epoch();
    let responses = vec![
        http_status(base, 25000),
        http_status(base + 1, 24800),
        http_status(base + 2, 25200),
    ];
    let p = write_multi_read_fixture("strih_drift_reported", &responses);
    let (code, stdout, _stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(code, 20, "stdout={stdout}");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("median") && low.contains("spread"),
        "a DRIFT status line must still report both median and spread: {stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// #836 review follow-up -- sampling a node now takes real wall-clock time (up to --window-s
// seconds), so grading multiple independent nodes ONE AFTER ANOTHER would multiply that window
// by the node count. The gate instead samples every node CONCURRENTLY (grade_http_node run in a
// background subshell per node, joined via `wait`). These tests prove that refactor is correct
// (multiple nodes' reports and verdicts are neither dropped nor mixed up) and that it genuinely
// runs in parallel (real wall-clock time stays close to ONE window, not node_count x window).
// ---------------------------------------------------------------------------------------------

#[test]
fn gate_reports_and_tallies_multiple_concurrent_nodes_independently() {
    // Two --win-http nodes: "strih" is tight+in-bound (OK), "stream" scatters beyond stability
    // (UNSTABLE). The combined gate must FAIL (one BAD node dominates), and the report must show
    // EACH node's own correct verdict -- neither dropped, swapped, nor bled into the other's line.
    let base = now_epoch();
    let ok_responses = vec![
        http_status(base, 100),
        http_status(base + 1, 120),
        http_status(base + 2, 110),
    ];
    let unstable_responses = vec![
        http_status(base, -19800),
        http_status(base + 1, 200),
        http_status(base + 2, 20100),
    ];
    let p_strih = write_multi_read_fixture("concurrent_strih_ok", &ok_responses);
    let p_stream = write_multi_read_fixture("concurrent_stream_unstable", &unstable_responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            // #1022: strih is the default NTP master with a "stream" client also configured, so
            // main() now does a priming read of strih's own status to derive stream's chase
            // envelope. Point it at the #686 NO_HTTP sentinel so the test never attempts a real
            // curl to the live rig -- this test's own fixtures carry no ntp_deadband_us field at
            // all, so the priming read (had it succeeded) would have been a no-op anyway.
            ("DANTESYNC_GATE_MASTER_DEADBAND_STATUS", NO_HTTP),
        ],
    );
    assert_eq!(
        code, 20,
        "one BAD node among several must fail the whole gate. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
    // Each node's OWN line must carry its OWN correct verdict -- not the other's. Match on the
    // node NAME as the line's leading token (per-node report lines are `  <name>   VERDICT ...`)
    // rather than a bare substring, since the gate's own banner line ("NTP master = strih; ...")
    // also contains the literal text "strih".
    let strih_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("strih"))
        .unwrap_or_else(|| panic!("no strih report line in stdout: {stdout}"));
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        strih_line.contains("OK"),
        "strih (tight, in-bound) must report OK: {strih_line:?}"
    );
    assert!(
        stream_line.contains("UNSTABLE"),
        "stream (scattered) must report UNSTABLE, not strih's OK: {stream_line:?}"
    );
}

#[test]
fn gate_samples_multiple_nodes_concurrently_not_sequentially() {
    // Real proof of the performance fix: 2 nodes, each needing 2 samples spread across a REAL
    // 4-second window (spacing = window/(n-1) = 4s -- gather_http_samples' own real `sleep`, not
    // skippable via --window-s 0 here since we need to actually MEASURE elapsed time). If the
    // gate graded nodes one after another, total wall time would be ~2 x 4s = 8s; if concurrent,
    // it stays close to ONE node's ~4s. The assertion threshold (6.5s) sits safely between the
    // two, comfortably above scheduling/process-spawn overhead and comfortably below the
    // sequential total.
    let base = now_epoch();
    let responses_a = vec![http_status(base, 100), http_status(base + 4, 110)];
    let responses_b = vec![http_status(base, 200), http_status(base + 4, 210)];
    let p_a = write_multi_read_fixture("concurrent_timing_strih", &responses_a);
    let p_b = write_multi_read_fixture("concurrent_timing_stream", &responses_b);

    let start = std::time::Instant::now();
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "2",
            "--min-distinct",
            "2",
            "--window-s",
            "4",
        ],
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STRIH", &p_a.display().to_string()),
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p_b.display().to_string()),
            // #1022: same NO_HTTP-sentinel reasoning as the sibling test above -- avoid a real
            // curl to the live rig, and the instant `cat` failure adds no measurable delay to
            // this test's own timing assertion.
            ("DANTESYNC_GATE_MASTER_DEADBAND_STATUS", NO_HTTP),
        ],
    );
    let elapsed = start.elapsed();
    assert_eq!(
        code, 0,
        "both nodes are healthy -> PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(
        elapsed.as_secs_f64() < 6.5,
        "#836 review follow-up: 2 nodes each needing a real 4s sampling window took {:.1}s -- \
         sequential grading would take ~8s, so this proves the nodes were NOT sampled one after \
         another. stdout={stdout}",
        elapsed.as_secs_f64()
    );
}

#[test]
fn gate_refuses_when_min_distinct_exceeds_samples() {
    // A min-distinct higher than the sample count could NEVER be satisfied by any node --
    // refuse at argument-parse time (usage error, 1) rather than let every node silently grade
    // "insufficient" no matter how healthy it is.
    let (code, _stdout, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-http",
        "strih=10.77.9.202",
        "--samples",
        "2",
        "--min-distinct",
        "3",
    ]);
    assert_eq!(
        code, 1,
        "--min-distinct > --samples must be a usage error. stderr: {stderr}"
    );
    assert!(
        stderr.contains("--min-distinct") && stderr.contains("--samples"),
        "stderr must name both flags: {stderr}"
    );
}

// ---------------------------------------------------------------------------------------------
// #1014 -- the --win-http path had NO staleness check on the NTP MEASUREMENT itself (only the
// general, PTP-driven updated_ts), and graded the NTP master (strih) with the same
// median+spread/stability bar as a client node even though the master's spread is a by-design
// UTC-residual correction-lag sawtooth since dantesync v1.8.30 (dantesync issue 71). These tests
// cover all four fixture classes: FRESH (new fields present, in window), STALE (new fields
// present, past window / ntp_failed), ABSENT (pre-1.8.30 payload, no ntp_age_s field at all --
// the frozen-sample fallback), and the MASTER-SAWTOOTH case (median-only grading).
// ---------------------------------------------------------------------------------------------

/// A DanteSync HTTP status-pipe JSON payload carrying the v1.8.30 NTP-freshness fields
/// (ntp_updated_ts/ntp_age_s/ntp_failed) alongside the pre-existing ones. `ntp_age_s_raw` is
/// either a plain integer string or the literal "null".
fn http_status_ntp(ts: u64, offset_us: i64, ntp_age_s_raw: &str, ntp_failed: bool) -> String {
    format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{ts},\
         \"is_locked\":true,\"ntp_offset_us\":{offset_us},\"mode\":\"NANO\",\
         \"ntp_failed\":{ntp_failed},\"ntp_updated_ts\":{ts},\"ntp_age_s\":{ntp_age_s_raw}}}"
    )
}

/// http_status_ntp (above) plus the dantesync PR #84/#86 "ntp_deadband_us" field (#1021): the
/// NTP master's own currently-active PTP-locked step-deferral threshold. `deadband_raw` is a
/// plain integer string or the literal "null" (a client node reports explicit null).
fn http_status_ntp_deadband(
    ts: u64,
    offset_us: i64,
    ntp_age_s_raw: &str,
    ntp_failed: bool,
    deadband_raw: &str,
) -> String {
    format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{ts},\
         \"is_locked\":true,\"ntp_offset_us\":{offset_us},\"mode\":\"NANO\",\
         \"ntp_failed\":{ntp_failed},\"ntp_updated_ts\":{ts},\"ntp_age_s\":{ntp_age_s_raw},\
         \"ntp_deadband_us\":{deadband_raw}}}"
    )
}

// --- Fixture class 1: FRESH (new fields present, in window) -- the happy path is unchanged ---

#[test]
fn gate_win_http_master_fresh_ntp_age_s_grades_ok_1014() {
    let now = now_epoch();
    let fresh = http_status_ntp(now, 100, "4", false);
    let p = write_win_http_fixture("strih_1014_fresh", &fresh);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "fresh ntp_age_s, in-bound offset, master node -> PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

// --- Fixture class 2: STALE (new fields present, past window, or ntp_failed) -- THE core fix -

#[test]
fn gate_win_http_stale_ntp_age_s_is_never_graded_as_drift_1014() {
    // #1014's ORIGINAL live incident, reproduced exactly: an offset far out of bound (-34718us,
    // the real captured value) whose ntp_age_s proves the reading is stale (99999s old). Before
    // this fix the gate had no way to see this and graded a false DRIFT; after the fix it must
    // refuse honestly as STALE/UNKNOWN, never DRIFT.
    let now = now_epoch();
    let stale = http_status_ntp(now, -34718, "99999", false);
    let p = write_win_http_fixture("strih_1014_stale_age", &stale);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "a stale NTP measurement must be GATE INCOMPLETE (11), NEVER GATE FAILED (20) -- the \
         exact false-DRIFT this ticket exists to fix. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(
        stdout.contains("NTP STALE"),
        "stdout must name it stale, not drift: {stdout}"
    );
    assert!(
        !stdout.contains("DRIFT"),
        "stdout must never mention DRIFT for a stale measurement, regardless of how far out of \
         bound the frozen value looks: {stdout}"
    );
}

#[test]
fn gate_win_http_ntp_failed_true_refuses_even_with_fresh_age_1014() {
    // dantesync issue 68 widened ntp_failed to ALSO mean "no fresh measurement within window" --
    // must refuse even when ntp_age_s itself looks fresh, proving the two signals are checked
    // independently.
    let now = now_epoch();
    let failed = http_status_ntp(now, 100, "2", true);
    let p = write_win_http_fixture("strih_1014_ntp_failed", &failed);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "ntp_failed:true must refuse (11) even with a fresh ntp_age_s. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("NTP STALE"), "stdout: {stdout}");
}

#[test]
fn gate_win_http_ntp_age_s_null_is_unknown_never_measured_1014() {
    let now = now_epoch();
    let never = http_status_ntp(now, 999999, "null", false);
    let p = write_win_http_fixture("strih_1014_never_measured", &never);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "ntp_age_s:null (never measured) must be GATE INCOMPLETE (11), never DRIFT. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("NTP UNKNOWN") && stdout.to_lowercase().contains("never"),
        "stdout must say never-measured, not just generically stale: {stdout}"
    );
}

// --- Fixture class 3: ABSENT (pre-1.8.30 payload, no ntp_age_s field) -- frozen-sample fallback

#[test]
fn gate_win_http_pre_1_8_30_frozen_offset_is_stale_not_drift_1014() {
    // Same #1014 incident shape, but as it would have looked BEFORE dantesync v1.8.30 shipped
    // ntp_age_s at all: several distinct-by-updated_ts reads that all report the SAME
    // ntp_offset_us -- the frozen-sample heuristic this ticket's own comments endorsed as the
    // interim fix for payloads lacking the new fields.
    let base = now_epoch();
    let responses = vec![
        http_status(base, -34718),
        http_status(base + 10, -34718),
        http_status(base + 20, -34718),
    ];
    let p = write_multi_read_fixture("strih_1014_frozen_legacy", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 11,
        "a frozen ntp_offset_us across all distinct samples, on a pre-1.8.30 payload with no \
         ntp_age_s, must be INCOMPLETE (11), never DRIFT (20). stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("INCOMPLETE"), "stderr: {stderr}");
    assert!(stdout.contains("NTP STALE"), "stdout: {stdout}");
    assert!(
        !stdout.contains("DRIFT"),
        "must never report DRIFT for a frozen legacy reading: {stdout}"
    );
}

#[test]
fn gate_win_http_pre_1_8_30_non_frozen_still_grades_and_can_still_drift_1014() {
    // A pre-1.8.30 payload (no ntp_age_s) whose samples genuinely VARY must fall through to the
    // unchanged legacy grading -- including still catching a REAL drift, proving the backward-
    // compat fallback is not a blanket pass.
    let base = now_epoch();
    let responses = vec![
        http_status(base, 8000),
        http_status(base + 10, 8200),
        http_status(base + 20, 8100),
    ];
    let p = write_multi_read_fixture("strih_1014_legacy_drift", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "a genuinely varying (non-frozen) pre-1.8.30 reading whose median is out of bound must \
         still DRIFT (20) -- the fallback never masks a real problem. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
    assert!(
        stdout.contains("pre-1.8.30") && stdout.contains("legacy"),
        "the line should note it graded via the legacy path (documented WARN, #1014): {stdout}"
    );
}

// --- Fixture class 4: MASTER-SAWTOOTH -- median+freshness only, spread never gates ------------

#[test]
fn gate_win_http_ntp_master_ignores_spread_but_still_grades_median_1014() {
    // The live dantesync issue 71 shape: strih's median is perfect (0) but its samples sawtooth
    // across a wide spread purely from correction lag -- must PASS, never UNSTABLE.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2500, "3", false),
        http_status_ntp(base + 10, 900, "2", false),
        http_status_ntp(base + 15, 0, "4", false),
        http_status_ntp(base + 20, 1800, "2", false),
        http_status_ntp(base + 25, 0, "3", false),
    ];
    let p = write_multi_read_fixture("strih_1014_master_sawtooth", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "6",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "the NTP master's by-design correction-lag sawtooth must PASS -- median in-bound, \
         spread never gated. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(
        !stdout.contains("UNSTABLE"),
        "the master must never be reported UNSTABLE for its own by-design sawtooth: {stdout}"
    );
    assert!(
        stdout.contains("NTP MASTER"),
        "the OK line should note this node was graded as the NTP master: {stdout}"
    );
}

#[test]
fn gate_win_http_ntp_master_still_fails_on_genuine_drift_1014() {
    // median-only mode is not "the master can never fail" -- a genuinely drifted master (tight
    // samples, but all clearly out of bound) must still DRIFT.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 25000, "2", false),
        http_status_ntp(base + 5, 25100, "3", false),
        http_status_ntp(base + 10, 24900, "2", false),
    ];
    let p = write_multi_read_fixture("strih_1014_master_genuine_drift", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "the master must still fail on a genuine median drift, median-only mode only skips the \
         spread/stability check. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_ntp_master_name_is_configurable_via_ntp_master_flag_1014() {
    // Prove the master designation is genuinely NAME-based and caller-configurable, not
    // hardcoded to the literal "strih": with --ntp-master stream, a "stream" node showing the
    // same scattered-but-in-bound-median shape that fails as UNSTABLE for an ordinary client
    // must now PASS.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2500, "3", false),
        http_status_ntp(base + 10, 900, "2", false),
    ];
    let p = write_multi_read_fixture("stream_1014_as_configured_master", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            "--ntp-master",
            "stream",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "--ntp-master stream must make \"stream\" grade as the master (median-only). \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(!stdout.contains("UNSTABLE"), "stdout: {stdout}");
}

// --- #1014 review follow-up: --ntp-master must match a CONFIGURED node, or refuse loudly -------

#[test]
fn gate_refuses_when_ntp_master_name_matches_no_configured_node_1014() {
    // A typo'd --win-http NAME= for the box meant to be the master (e.g. "strhi" instead of
    // "strih") must NEVER silently fall back to grading the intended master with the full
    // spread/stability bar -- that is #1014's exact false-DRIFT bug, reachable again through a
    // misspelling instead of an old payload shape. Default --ntp-master is "strih"; configuring
    // only a differently-named win-http node must refuse at usage-check time (1), never proceed.
    let now = now_epoch();
    let fresh = http_status_ntp(now, 100, "4", false);
    let p = write_win_http_fixture("strhi_typo_1014", &fresh);
    let (code, _stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strhi=10.77.9.202", // typo'd node name -- never matches the default "strih" master
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRHI", &p.display().to_string())],
    );
    assert_eq!(
        code, 1,
        "a --ntp-master name matching no configured node must be a usage error (1), never a \
         silent full-grading fallback: stderr={stderr}"
    );
    assert!(
        stderr.contains("--ntp-master") && stderr.contains("strih"),
        "stderr must name the mismatch clearly: {stderr}"
    );
}

#[test]
fn gate_ntp_master_empty_string_opts_out_of_master_validation_1014() {
    // --ntp-master "" explicitly disables the master concept for this invocation -- a node set
    // with no "strih" at all must proceed normally (no usage-error refusal), grading every node
    // with the full spread/stability bar.
    let now = now_epoch();
    let fresh = http_status_ntp(now, 100, "4", false);
    let p = write_win_http_fixture("stream_no_master_opt_out_1014", &fresh);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            "--ntp-master",
            "",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "--ntp-master \"\" must opt out of the validation and proceed normally. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn gate_ntp_master_validation_skipped_for_linux_only_invocations_1014() {
    // A pure --linux invocation (--win-http omitted entirely, so the win_http array stays
    // EMPTY -- unlike --linux "", a bare --win-http "" would append ONE empty-name element, not
    // zero) has no master concept in play -- the default "strih" master name matching nothing
    // among cam1/cam2 must NOT refuse.
    let j = write_dante_journal(
        "cam1_1014_linux_only_no_master",
        "2026-08-11T10:00:00+02:00 cam1 dantesync[1]: [NTP] offset:+100us (threshold:520us, adaptive)\n\
2026-08-11T10:00:05+02:00 cam1 dantesync[1]: [PTP] NANO  Drift:   +12ns/s  Adj: +6.10ppm\n",
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
        "a Linux-only invocation must never be blocked by the master-name validation (no \
         --win-http node is configured at all). stdout={stdout} stderr={stderr}"
    );
}

// --- #1021: the master's fixed median bound must adapt to a live PTP-locked deadband -----------
//
// dantesync PR #84/#86 (closes dantesync issue 83): a genuinely PTP-locked master now DEFERS its
// periodic UTC-phase step to a deadband (live-tuned to 2500us, the #1021 supervisor comment
// 2026-08-12) instead of a tight ~200us threshold, and reports the currently-active threshold as
// "ntp_deadband_us" in its own /status. The pre-#1021 gate grades the master's median against the
// FIXED GATE_BOUND_US (2000us) regardless -- a healthy master's own ramp toward the deadband would
// then false-DRIFT purely from where in its correction cycle a 30s sample window landed. Confirmed
// live 2026-08-12: curl http://10.77.9.202:8898/status returned ntp_offset_us=2398,
// ntp_deadband_us=2500 while genuinely PTP-locked (mode NANO, is_locked true) -- ALREADY above the
// fixed 2000us bound.

#[test]
fn gate_win_http_ntp_master_with_deadband_widens_bound_past_false_drift_1021() {
    // The live shape: a healthy master's median (~2850us) sits between the fixed 2000us bound and
    // its own reported 2500us deadband. Before #1021 this false-DRIFTs; after, it must PASS.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 2800, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2900, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2850, "2", false, "2500"),
    ];
    let p = write_multi_read_fixture("strih_1021_deadband_widens_bound", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "a master reporting ntp_deadband_us=2500 with a ~2850us median (a healthy by-design ramp) \
         must PASS, not false-DRIFT against the fixed 2000us bound. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(!stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_win_http_ntp_master_without_deadband_field_keeps_the_fixed_bound_unchanged_1021() {
    // Backward compat: a pre-dantesync-#84 master payload (no ntp_deadband_us field at all) must
    // grade EXACTLY as before -- the same ~2850us median that PASSES with the field present (see
    // the sibling test above) must still DRIFT here, against the unmodified fixed GATE_BOUND_US.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 2800, "2", false),
        http_status_ntp(base + 5, 2900, "3", false),
        http_status_ntp(base + 10, 2850, "2", false),
    ];
    let p = write_multi_read_fixture("strih_1021_no_deadband_field", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "no ntp_deadband_us field -> the unmodified fixed bound -> must still DRIFT exactly like \
         before #1021 (rollout-window backward compat). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_win_http_ntp_master_with_null_deadband_keeps_the_fixed_bound_unchanged_1021() {
    // A client-shaped payload explicitly reports ntp_deadband_us:null (dantesync PR #84/#86) --
    // must fall back to the unmodified fixed bound exactly like an absent field.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 2800, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 2900, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 2850, "2", false, "null"),
    ];
    let p = write_multi_read_fixture("strih_1021_null_deadband", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "ntp_deadband_us:null must fall back to the unmodified fixed bound, same as absent. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_win_http_ntp_master_still_drifts_beyond_deadband_plus_margin_1021() {
    // #1021 is not "the master can never fail once it reports a deadband" -- a genuine drift far
    // beyond deadband+margin must still DRIFT.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 10000, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 10100, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 9900, "2", false, "2500"),
    ];
    let p = write_multi_read_fixture("strih_1021_genuine_drift_with_deadband", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "a genuine ~10ms drift far beyond deadband+margin (2500+1000=3500us default) must still \
         DRIFT. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_win_http_client_node_ignores_its_own_deadband_field_1021() {
    // Client rows are byte-for-byte untouched by #1021 -- even if a non-master node's payload
    // somehow carried a numeric ntp_deadband_us (never expected live -- clients report null), the
    // deadband widening must ONLY ever apply to the GATE_NTP_MASTER_NAME/median-only node.
    // "stream" here is graded as an ordinary client (--ntp-master "" opts out of the master
    // concept entirely for this invocation, since "stream" is the only configured node).
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 2800, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2900, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2850, "2", false, "2500"),
    ];
    let p = write_multi_read_fixture("stream_1021_client_ignores_deadband", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--ntp-master",
            "",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "a client node's own ntp_deadband_us must never widen ITS bound -- must still DRIFT \
         against the fixed 2000us bound. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_deadband_margin_us_zero_still_drifts_1021() {
    // --deadband-margin-us 0 -> effective bound == the bare deadband (2500us) -> the same
    // ~2850-2900us median that PASSES with the default 1000us margin (see the sibling test below)
    // must DRIFT here, proving the flag genuinely controls the margin rather than being ignored.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 2800, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2900, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2850, "2", false, "2500"),
    ];
    let p = write_multi_read_fixture("strih_1021_margin_zero", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--deadband-margin-us",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "--deadband-margin-us 0 -> effective bound == deadband (2500us) -> a ~2850-2900us median \
         must still DRIFT. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_deadband_margin_us_default_covers_observed_overshoot_1021() {
    // The default margin (1000us) must be wide enough to cover the live-observed step overshoot
    // (peak 2987us on a 2500us deadband, the #1021 supervisor comment) without any explicit flag.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp_deadband(base, 2950, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2987, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2900, "2", false, "2500"),
    ];
    let p = write_multi_read_fixture("strih_1021_margin_default_overshoot", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "the default 1000us margin must cover the live-observed ~487us peak overshoot with room \
         to spare. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
}

#[test]
fn help_describes_the_deadband_margin_flag_1021() {
    let (code, stdout, _stderr) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("--deadband-margin-us"),
        "usage text must document the new flag: {stdout}"
    );
    assert!(
        stdout.contains("ntp_deadband_us"),
        "usage text must explain what the flag widens the master's bound against: {stdout}"
    );
}

// --- #1022: client rows ALSO false-DRIFT during the master's own deadband step-chase window ----
//
// #1021 (above) widens ONLY the GATE_NTP_MASTER_NAME/median-only row. Live evidence filed on
// #1022 (camera-box PR #1020, run 31617253261, dantesync v1.8.41 fleet-wide) showed a CLIENT
// node ALSO false-DRIFTs during the SAME master step-chase window, via a DIFFERENT mechanism (a
// client always reports its own "ntp_deadband_us":null -- it mirrors the master's sawtooth via
// the LAN NTP measurement instead): "stream" graded DRIFT with median 2589us > the fixed 2000us
// bound while its spread was only 82us across 6 samples -- tight, not the #836 scatter/noise
// class, i.e. a genuinely elevated-but-healthy chase window, not a real desync.
//
// These tests exercise `main()`'s new priming read of the CONFIGURED master's own /status (via
// the NEW DANTESYNC_GATE_MASTER_DEADBAND_STATUS override -- deliberately SEPARATE from
// DANTESYNC_GATE_WIN_HTTP_<NAME>/DANTESYNC_GATE_LINUX_HTTP_<NAME>, which the master's OWN
// gather_http_samples calls already consume via the #836 executable-fixture counter; reusing
// that same override for the priming read would silently shift the master's own sampled
// sequence) and the new client_chase_bound_us-driven widening it feeds into every CLIENT row's
// grading.

#[test]
fn gate_client_row_within_master_deadband_envelope_passes_instead_of_false_drift_1022() {
    // The exact live shape from #1022's own filed evidence: master (strih) reports a healthy
    // 2500us deadband; client (stream) shows a TIGHT (spread 80us) but elevated median (2589us)
    // -- a healthy step-chase window, not a real desync. Before #1022 this false-DRIFTs on
    // stream; after, the whole gate must PASS.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1022_chase_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 2550, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 2589, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 2630, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1022_chase_client", &client_responses);
    // The priming read of the master's OWN status (used SOLELY to derive the client envelope) --
    // a static single payload is enough, it needs no freshness/sample-count shape at all.
    let p_priming = write_win_http_fixture(
        "strih_1022_priming_deadband",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "a client's tight, elevated median inside the master's own deadband+margin envelope \
         (max(2000, 2500+1000)=3500us; median 2589us) must PASS, not false-DRIFT against the \
         fixed 2000us bound. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(!stdout.contains("DRIFT"), "stdout: {stdout}");
}

#[test]
fn gate_client_row_still_drifts_beyond_the_widened_chase_envelope_1022() {
    // #1022 is not "a client can never fail once a master deadband is present" -- a genuine
    // ~8ms client drift far beyond the widened envelope (max(2000, 2500+1000)=3500us) must still
    // DRIFT.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1022_genuine_drift_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 8000, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 8100, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 7900, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1022_genuine_drift_client", &client_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_genuine_drift_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a genuine ~8ms client drift far beyond the widened 3500us envelope must still DRIFT. \
         stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("DRIFT"),
        "stream (genuine drift) must report DRIFT: {stream_line:?}"
    );
}

#[test]
fn gate_client_row_keeps_the_fixed_bound_unchanged_when_the_master_priming_read_fails_1022() {
    // Same tight/elevated client sample as the PASS test above, but this time
    // DANTESYNC_GATE_MASTER_DEADBAND_STATUS points nowhere (the #686 NO_HTTP sentinel) -- the
    // priming read must fail closed (empty), never a real curl to the live rig from a test
    // sandbox, and the client bound must fall back to the UNMODIFIED fixed bound -- exactly the
    // "cannot prove it -> do not widen" discipline every other fallback in this file follows.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp(base, 2400, "2", false),
        http_status_ntp(base + 5, 2500, "3", false),
        http_status_ntp(base + 10, 2450, "2", false),
    ];
    let p_master = write_multi_read_fixture("strih_1022_no_priming_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 2550, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 2589, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 2630, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1022_no_priming_client", &client_responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            ("DANTESYNC_GATE_MASTER_DEADBAND_STATUS", NO_HTTP),
        ],
    );
    assert_eq!(
        code, 20,
        "an unreadable master priming status must fall back to the unmodified 2000us bound, so \
         the same 2589us median must still DRIFT (never a silent, unproven widen). \
         stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("DRIFT"),
        "stream_line: {stream_line:?}"
    );
}

#[test]
fn gate_client_row_scatter_still_fails_via_stability_despite_the_widened_median_bound_1022() {
    // The widened bound only ever relaxes the MEDIAN (location) check -- the pre-existing #836
    // spread/stability check stays fully active for client rows. A client whose median sits
    // inside the widened envelope (2589us <= 3500us) but whose samples SCATTER beyond the
    // stability bound must still fail, just via UNSTABLE instead of DRIFT.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1022_scatter_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 1500, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 2589, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 3600, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1022_scatter_client", &client_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_scatter_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--client-step-threshold-fallback-us",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "median in-envelope (2589us <= 3500us) but spread 2100us > the 2000us stability bound \
         must still fail. stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("UNSTABLE") && !stream_line.contains("DRIFT"),
        "the widened median bound must clear the DRIFT flag, leaving ONLY UNSTABLE (the \
         pre-existing #836 scatter class, unaffected by #1022): {stream_line:?}"
    );
}

#[test]
fn gate_client_chase_ceiling_us_caps_an_absurd_master_deadband_1022() {
    // #1021's own master-row widening is deliberately UNCAPPED (it only ever widens the ONE
    // master row). #1022 widens potentially MANY client rows from the SAME live-read deadband --
    // an absurdly large/misconfigured ntp_deadband_us must not blindly widen every client's
    // bound to match it. The default --client-chase-ceiling-us (5000) caps the deadband
    // component BEFORE the margin is added, so a client median that would PASS under an
    // uncapped (blind) widen still DRIFTs here.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 100, "2", false, "50000"),
        http_status_ntp_deadband(base + 5, 120, "3", false, "50000"),
        http_status_ntp_deadband(base + 10, 110, "2", false, "50000"),
    ];
    let p_master = write_multi_read_fixture("strih_1022_absurd_deadband_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 6400, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 6500, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 6600, "2", false, "null"),
    ];
    let p_client =
        write_multi_read_fixture("stream_1022_absurd_deadband_client", &client_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_absurd_deadband_priming",
        &http_status_ntp_deadband(base, 100, "2", false, "50000"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--client-step-threshold-fallback-us",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "max(2000, min(50000,5000)+1000)=6000us -- a 6500us client median must still DRIFT \
         against the CAPPED envelope, even though it would PASS under an uncapped \
         50000+1000=51000us blind widen. stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("DRIFT"),
        "stream_line: {stream_line:?}"
    );
}

#[test]
fn gate_client_chase_ceiling_us_flag_is_configurable_1022() {
    // Same fixtures as the PASS test above (master deadband 2500, client median 2589us, which
    // PASSES under the default 5000us ceiling) -- an explicit lower --client-chase-ceiling-us
    // narrows the envelope back down and must make the SAME client DRIFT again, proving the flag
    // genuinely controls the cap rather than being ignored.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1022_low_ceiling_master", &master_responses);
    let client_responses = vec![
        http_status_ntp_deadband(base, 2550, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 2589, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 2630, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1022_low_ceiling_client", &client_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_low_ceiling_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--client-chase-ceiling-us",
            "1000",
            "--client-step-threshold-fallback-us",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "max(2000, min(2500,1000)+1000)=2000us -- --client-chase-ceiling-us 1000 must narrow the \
         envelope enough that the same 2589us median DRIFTs again. stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("DRIFT"),
        "stream_line: {stream_line:?}"
    );
}

#[test]
fn gate_client_chase_ceiling_us_malformed_is_a_usage_error_1022() {
    let (code, _stdout, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-http",
        "strih=10.77.9.202",
        "--client-chase-ceiling-us",
        "abc",
    ]);
    assert_eq!(
        code, 1,
        "a non-numeric --client-chase-ceiling-us must be a usage error (1). stderr: {stderr}"
    );
    assert!(
        stderr.contains("--client-chase-ceiling-us"),
        "stderr must name the flag: {stderr}"
    );
}

#[test]
fn help_describes_the_client_chase_ceiling_flag_1022() {
    let (code, stdout, _stderr) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("--client-chase-ceiling-us"),
        "usage text must document the new flag: {stdout}"
    );
}

// --- #1041: client chase envelope under-derived -- the gate must fetch EACH client's OWN -------
// --- journal to include its real adaptive step threshold, falling back to a conservative -------
// --- constant for a Windows client (no journald) or an unreadable journal ----------------------

#[test]
fn gate_client_step_threshold_fallback_us_malformed_is_a_usage_error_1041() {
    let (code, _stdout, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-http",
        "strih=10.77.9.202",
        "--client-step-threshold-fallback-us",
        "abc",
    ]);
    assert_eq!(
        code, 1,
        "a non-numeric --client-step-threshold-fallback-us must be a usage error (1). \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("--client-step-threshold-fallback-us"),
        "stderr must name the flag: {stderr}"
    );
}

#[test]
fn help_describes_the_client_step_threshold_fallback_flag_1041() {
    let (code, stdout, _stderr) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("--client-step-threshold-fallback-us"),
        "usage text must document the new flag: {stdout}"
    );
}

#[test]
fn gate_client_row_prefers_its_own_journal_threshold_over_the_fallback_1041() {
    // The exact live cam3 incident (E2E run 31691870165): master deadband 2500, cam3's own
    // journal carries threshold:665us. 6 samples: 2 baseline (+23us) + 4 elevated (+3680us) --
    // the OLD 3500us bound (2500+1000 margin, no threshold term) false-DRIFTs this; the NEW
    // envelope (2500+665+1000=4165us), derived from cam3's OWN journal (never the fallback
    // constant), must PASS via the (unchanged) bimodal chase-signature exclusion.
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2450, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2460, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2470, "2", false, "2500"),
        http_status_ntp_deadband(base + 15, 2480, "2", false, "2500"),
        http_status_ntp_deadband(base + 20, 2490, "2", false, "2500"),
        http_status_ntp_deadband(base + 25, 2500, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1041_cam3_master", &master_responses);
    let p_priming = write_win_http_fixture(
        "strih_1041_cam3_priming",
        &http_status_ntp_deadband(base, 2500, "2", false, "2500"),
    );
    let cam3_responses = vec![
        http_status_ntp(base, 23, "2", false),
        http_status_ntp(base + 5, 23, "3", false),
        http_status_ntp(base + 10, 3680, "2", false),
        http_status_ntp(base + 15, 3680, "2", false),
        http_status_ntp(base + 20, 3680, "2", false),
        http_status_ntp(base + 25, 3680, "2", false),
    ];
    let p_cam3 = write_multi_read_fixture("cam3_1041_incident", &cam3_responses);
    let j_cam3 = write_dante_journal(
        "cam3_1041_incident_journal",
        "11:16:58 [NTP] burst offset:+3680us spread:16us samples:3/5\n\
11:16:58 [NTP] Stepped +3680us\n\
11:17:29 [NTP] offset:+23us\n\
11:18:59 [NTP] burst offset:+2701us step candidate +2701us (threshold:665us)\n",
    );

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam3=10.77.9.63",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "6",
            "--min-distinct",
            "6",
            "--window-s",
            "0",
            // A deliberately WRONG (way too small) fallback proves the gate used cam3's OWN
            // journal, NOT the fallback -- if it silently fell back, this would still DRIFT.
            "--client-step-threshold-fallback-us",
            "1",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM3",
                &p_cam3.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM3",
                &j_cam3.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "cam3's real 665us journal threshold must widen the envelope to 4165us and PASS the \
         genuine +3680us chase excursion -- even with a deliberately tiny fallback (1us) that \
         would still DRIFT if the fallback were used instead of the real journal value. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    let cam3_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("cam3"))
        .unwrap_or_else(|| panic!("no cam3 report line in stdout: {stdout}"));
    assert!(
        cam3_line.contains("its own journal (665us)"),
        "the report line must show cam3's threshold came from ITS OWN journal, not the \
         fallback: {cam3_line:?}"
    );
}

#[test]
fn gate_win_http_client_never_fetches_a_journal_uses_the_fallback_1041() {
    // A Windows client (no journald at all) must use CLIENT_STEP_FALLBACK_US directly -- never
    // attempt a journal read (there is nothing to read via SSH for a --win-http node).
    let base = now_epoch();
    let master_responses = vec![
        http_status_ntp_deadband(base, 2450, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2480, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2500, "2", false, "2500"),
    ];
    let p_master = write_multi_read_fixture("strih_1041_win_fallback_master", &master_responses);
    let p_priming = write_win_http_fixture(
        "strih_1041_win_fallback_priming",
        &http_status_ntp_deadband(base, 2500, "2", false, "2500"),
    );
    // median 2900us, worst-case pre-#1041 envelope (2500+1000=3500us) already covers it, so the
    // interesting assertion is the REPORT LINE's own attribution: it must name the fallback
    // (never a journal -- a Windows box has no journald at all).
    let stream_responses = vec![
        http_status_ntp(base, 2900, "2", false),
        http_status_ntp(base + 5, 2900, "3", false),
        http_status_ntp(base + 10, 2900, "2", false),
    ];
    let p_stream = write_multi_read_fixture("stream_1041_win_fallback", &stream_responses);

    // Low fallback (100us): max(2000, 2500+100+1000)=3600us -> 2900 <= 3600 PASSES.
    let (code_low, stdout_low, _stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--client-step-threshold-fallback-us",
            "100",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code_low, 0,
        "median 2900us must fit inside max(2000, 2500+100+1000)=3600us via the fallback term: \
         stdout={stdout_low}"
    );
    let stream_line_low = stdout_low
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line: {stdout_low}"));
    assert!(
        stream_line_low.contains("fallback(100us)"),
        "a Windows client must report its threshold source as the FALLBACK, never a journal \
         (it has no journald): {stream_line_low:?}"
    );
}

// --- #1022 spread-side completion: a single master step can trip MULTIPLE clients' SPREAD -----
//
// The median fix (client_chase_bound_us) works correctly -- a live E2E rerun on the merged round
// showed every client's MEDIAN graded against the widened envelope exactly as designed. But the
// SAME master step also inflates a client's SPREAD, left at the fixed 2000us stability bound by
// design (client_chase_bound_us only ever widens the median/location check). Because the step is
// on ONE clock shared by the whole fleet, the SAME step can land inside MULTIPLE clients' sampling
// windows in the SAME run -- the live rerun tripped cam1, cam2, AND stream simultaneously:
//
//   cam1   UNSTABLE (median 0us <= 3500us bound; spread 2682us > 2000us stability)
//   cam2   UNSTABLE (median 0us <= 3500us bound; spread 2577us > 2000us stability)
//   strih  OK       (median 1220us <= 3500us bound; spread 384us)
//   stream UNSTABLE (median 2822us <= 3500us bound; spread 2837us > 2000us stability)
//   !! GATE FAILED: 3 node(s) DRIFTED or PTP-DEGRADED.  (exit 20)
//
// The fix: a CLIENT row whose verdict is "unstable" (median in bound, spread not) AND whose
// worst sample still fits the SAME bound gets ONE fresh resample round before failing --
// dantesync-gate.sh's grade_http_node calls should_resample_for_chase, and on "yes" re-gathers
// via gather_http_samples and grades THAT round instead. These tests reproduce the EXACT live
// numbers above (each client's fixture serves 3 "chase" responses for the first round, then 3
// "recovered" responses for the resample round -- the SAME write_multi_read_fixture counter
// mechanism naturally serves them in that order across the two gather_http_samples calls).

#[test]
fn gate_reproduces_the_live_three_client_simultaneous_chase_shape_and_passes_via_exclusion_1022() {
    // Supersedes the OLD resample-based recovery: a live rerun proved resample-once is a coin
    // flip (the fixed delay can land inside the SAME excursion). The bimodal chase-signature
    // exclusion instead grades the ORIGINAL window directly -- these are the EXACT live numbers
    // (E2E run 31640853894/31633417530), and NONE of the three clients need a resample at all;
    // every one is explained on its FIRST round.
    let base = now_epoch();
    // strih (master): median 1220us, spread 384us (reference only, never gated) -- the master's
    // own median-only row never has a spread verdict or a chase-signature check at all.
    let strih_responses = vec![
        http_status_ntp_deadband(base, 1200, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 1220, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 1584, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_spread_live_shape", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_spread_live_priming",
        &http_status_ntp_deadband(base, 1220, "2", false, "2500"),
    );

    // cam1: reproduces "median 0us; spread 2682us" EXACTLY (0, 0, 2682 -> sorted median
    // position 2 = 0, spread = 2682-0) -- a tight baseline pair + one tight elevated sample.
    let cam1_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 0, "3", false),
        http_status_ntp(base + 10, 2682, "2", false),
    ];
    let p_cam1 = write_multi_read_fixture("cam1_1022_spread_live_shape", &cam1_responses);

    // cam2: reproduces "median 0us; spread 2577us" EXACTLY.
    let cam2_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 0, "3", false),
        http_status_ntp(base + 10, 2577, "2", false),
    ];
    let p_cam2 = write_multi_read_fixture("cam2_1022_spread_live_shape", &cam2_responses);

    // stream: reproduces "median 2822us; spread 2837us" EXACTLY (0, 2822, 2837 -> sorted median
    // position 2 = 2822, spread = 2837-0) -- a single baseline sample + a tight elevated pair.
    let stream_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2822, "3", false),
        http_status_ntp(base + 10, 2837, "2", false),
    ];
    let p_stream = write_multi_read_fixture("stream_1022_spread_live_shape", &stream_responses);

    // #1041 network safety: this environment can reach the real rig (cam1/cam2 IPs below are
    // genuine, reachable boxes) -- WITHOUT an explicit journal override, grade_http_node's new
    // per-client threshold read (client_step_threshold_us_from_journal, #1041) would attempt a
    // REAL live SSH read here, making this test non-deterministic. An empty journal (no
    // "threshold:" match) makes the term fall back to the flag's own default, unaffected by
    // whatever the LIVE rig's journal happens to contain right now.
    let j_cam1 = write_dante_journal("cam1_1022_spread_live_shape_journal", "");
    let j_cam2 = write_dante_journal("cam2_1022_spread_live_shape_journal", "");

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61 cam2=10.77.9.62",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &p_cam1.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM2",
                &p_cam2.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j_cam1.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM2",
                &j_cam2.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "the exact live 3-simultaneous-UNSTABLE shape must be explained by the chase signature \
         and PASS -- deterministically, on the first round. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout: {stdout}");
    assert!(!stdout.contains("UNSTABLE"), "stdout: {stdout}");
    assert!(!stdout.contains("DRIFT"), "stdout: {stdout}");
    assert!(
        !stdout.contains("resampled once"),
        "none of the three clients should need a resample -- exclusion explains the FIRST \
         round directly: stdout={stdout}"
    );
    for name in ["cam1", "cam2", "stream"] {
        let line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("no {name} report line in stdout: {stdout}"));
        assert!(
            line.contains("explained by master step-chase"),
            "{name} must report the chase-signature exclusion, not a raw pass: {line:?}"
        );
    }
}

#[test]
fn gate_resample_with_no_further_distinct_data_reports_unknown_never_a_stale_or_false_pass_1022() {
    // Real edge case: a resample-eligible node whose EXCLUSION declines (mixed-sign elevated
    // samples -- not a clean chase signature, so genuinely needs the resample fallback) but
    // whose fixture has NOTHING further to give -- only 3 responses total, so
    // write_multi_read_fixture CLAMPS every call past the 3rd to the LAST entry -- gets a
    // resample round whose 3 reads are all BYTE-IDENTICAL, including the SAME updated_ts.
    // distinct_offset_samples_us's own "the daemon re-serving its cached value" dedup (#836
    // point 5) then collapses those to a SINGLE distinct sample -- fewer than --min-distinct, so
    // the FINAL grade is "insufficient" -> UNKNOWN, never a silent PASS and never the stale
    // original "unstable" verdict either. The resample note must still appear -- the resample
    // WAS attempted, it just could not produce enough independent data to grade.
    let base = now_epoch();
    let strih_responses = vec![
        http_status_ntp_deadband(base, 1200, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 1220, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 1584, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_spread_baseline", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_spread_baseline_priming",
        &http_status_ntp_deadband(base, 1220, "2", false, "2500"),
    );
    // Mixed-sign elevated (2600, -2200): median in bound (unstable), worst sample (2600) within
    // the envelope (resample-eligible), but NOT a coherent chase signature (exclusion declines).
    let cam1_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2600, "3", false),
        http_status_ntp(base + 10, -2200, "2", false),
    ];
    let p_cam1 = write_multi_read_fixture("cam1_1022_spread_baseline", &cam1_responses);

    // #1041 network safety: see the sibling 3-client test's own comment above -- an explicit
    // empty journal override avoids a real live SSH read against the reachable cam1 IP below.
    let j_cam1 = write_dante_journal("cam1_1022_spread_baseline_journal", "");

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &p_cam1.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j_cam1.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 11,
        "a resample that cannot gather enough independent data must be INCOMPLETE (11), never a \
         silent pass. stdout={stdout} stderr={stderr}"
    );
    let cam1_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("cam1"))
        .unwrap_or_else(|| panic!("no cam1 report line in stdout: {stdout}"));
    assert!(
        cam1_line.contains("UNKNOWN") && cam1_line.contains("only 1 distinct sample"),
        "the clamped-repeat resample must collapse to a single distinct sample -> UNKNOWN, never \
         a false PASS (spread 0) or the stale original UNSTABLE verdict: {cam1_line:?}"
    );
    assert!(
        cam1_line.contains("resampled once"),
        "the resample must still be noted as attempted, even though it couldn't gather enough \
         new data: {cam1_line:?}"
    );
}

#[test]
fn gate_client_row_persistent_scatter_still_fails_after_the_resample_1022() {
    // resample-once is a literal ONE-SHOT, never a retry loop: a node whose CHASE round declines
    // exclusion (mixed-sign elevated -- genuine scatter, not a coherent phase offset) resamples,
    // and whose RESAMPLE round is ALSO mixed-sign/unstable must still FAIL, grading the
    // RESAMPLED numbers -- "sustained ... must still fail" even under #1022's exclusion.
    let base = now_epoch();
    let strih_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_persistent_master", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_persistent_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    // Chase round: median 0, spread 4800, mixed-sign elevated (2600, -2200) -> unstable,
    // exclusion declines (not a coherent chase), resample fires (worst sample 2600 <= 3500
    // bound). Resample round: median 100, spread 4900, ALSO mixed-sign elevated (2600, -2300) --
    // STILL unstable, STILL declines exclusion. The two rounds' spread numbers (4800 vs 4900)
    // are deliberately different so the assertion can prove the FINAL report reflects round 2's
    // own fresh numbers, not round 1's stale ones.
    let stream_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2600, "3", false),
        http_status_ntp(base + 10, -2200, "2", false),
        http_status_ntp(base + 15, 100, "4", false),
        http_status_ntp(base + 20, 2600, "5", false),
        http_status_ntp(base + 25, -2300, "6", false),
    ];
    let p_stream = write_multi_read_fixture("stream_1022_persistent_client", &stream_responses);

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a resample that is ALSO unstable must still fail the gate. stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("UNSTABLE") && stream_line.contains("spread 4900us"),
        "must report the RESAMPLED round's own numbers (spread 4900us), not the original chase \
         round's (spread 4800us) -- proves the final verdict grades the fresh data: {stream_line:?}"
    );
    assert!(
        stream_line.contains("resampled once"),
        "must still note a resample was attempted, even though it did not rescue the node: \
         {stream_line:?}"
    );
}

#[test]
fn gate_chase_resample_delay_flag_value_is_reflected_in_the_report_note_1022() {
    let base = now_epoch();
    let strih_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_delay_flag_master", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_delay_flag_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    // Chase round: mixed-sign elevated (2600, -2200) -> unstable, exclusion declines (genuinely
    // needs the resample fallback, so the delay flag's own note is actually exercised).
    // Resample round: clean/healthy -> OK, carrying the resample note.
    let stream_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2600, "3", false),
        http_status_ntp(base + 10, -2200, "2", false),
        http_status_ntp(base + 15, 10, "4", false),
        http_status_ntp(base + 20, 20, "5", false),
        http_status_ntp(base + 25, 15, "6", false),
    ];
    let p_stream = write_multi_read_fixture("stream_1022_delay_flag_client", &stream_responses);

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("resampled once after a 0s delay"),
        "the --chase-resample-delay-s value must be reflected verbatim in the note: {stream_line:?}"
    );
}

#[test]
fn gate_chase_resample_delay_s_malformed_is_a_usage_error_1022() {
    let (code, _stdout, stderr) = run_gate(&[
        "--linux",
        "",
        "--win-http",
        "strih=10.77.9.202",
        "--chase-resample-delay-s",
        "abc",
    ]);
    assert_eq!(
        code, 1,
        "a non-numeric --chase-resample-delay-s must be a usage error (1). stderr: {stderr}"
    );
    assert!(
        stderr.contains("--chase-resample-delay-s"),
        "stderr must name the flag: {stderr}"
    );
}

#[test]
fn help_describes_the_chase_resample_delay_flag_1022() {
    let (code, stdout, _stderr) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("--chase-resample-delay-s"),
        "usage text must document the new flag: {stdout}"
    );
}

#[test]
fn gate_resample_reports_stale_when_the_ntp_measurement_ages_out_during_the_delay_1022() {
    // Review finding: the resample takes real wall-clock time (the delay + another full sampling
    // window) -- long enough for a borderline-fresh NTP measurement to cross into staleness
    // during that gap (the #1014 "frozen/free-running measurement graded as live" class). The
    // resampled round's OWN freshness must be re-verified before its median/spread are trusted --
    // never silently grade a now-stale measurement just because the ORIGINAL round was fresh.
    let base = now_epoch();
    let strih_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_resample_stale_master", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_resample_stale_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    // Chase round: fresh, unstable, mixed-sign elevated (2600, -2200) -> exclusion declines
    // (genuinely needs the resample fallback), worst sample within the envelope -> resample
    // fires. Resample round: ntp_failed=true on every payload -> the resampled data is STALE.
    let stream_responses = vec![
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2600, "3", false),
        http_status_ntp(base + 10, -2200, "2", false),
        http_status_ntp(base + 15, 10, "2", true),
        http_status_ntp(base + 20, 20, "3", true),
        http_status_ntp(base + 25, 15, "2", true),
    ];
    let p_stream = write_multi_read_fixture("stream_1022_resample_stale_client", &stream_responses);

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 11,
        "a resample whose NTP measurement went stale must be INCOMPLETE (11), never a silent \
         grade of stale data. stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("STALE"),
        "must report STALE for the now-aged-out resampled measurement, never grade its \
         median/spread as if it were still live: {stream_line:?}"
    );
    let _ = stderr;
}

#[test]
fn gate_resampled_round_that_shows_a_clean_chase_signature_is_also_excused_1022() {
    // Review finding: the "excused-after-resample" composition (grade_http_node retries
    // chase_bimodal_exclusion_verdict on the FRESH round, not just the original one) had zero
    // test coverage. Chase round: mixed-sign elevated (declines exclusion, but resample-eligible,
    // same shape as the sibling resample tests). Resample round: a CLEAN bimodal signature (tight
    // baseline pair + one tight same-sign elevated sample, all within the envelope) -- must be
    // excused via exclusion on the SECOND round, not graded raw as "unstable".
    let base = now_epoch();
    let strih_responses = vec![
        http_status_ntp_deadband(base, 2400, "2", false, "2500"),
        http_status_ntp_deadband(base + 5, 2500, "3", false, "2500"),
        http_status_ntp_deadband(base + 10, 2450, "2", false, "2500"),
    ];
    let p_strih = write_multi_read_fixture("strih_1022_resample_excused_master", &strih_responses);
    let p_priming = write_win_http_fixture(
        "strih_1022_resample_excused_priming",
        &http_status_ntp_deadband(base, 2450, "2", false, "2500"),
    );
    let stream_responses = vec![
        // Chase round: mixed-sign elevated -- declines exclusion, resample-eligible (worst
        // sample 2600 <= 3500 bound).
        http_status_ntp(base, 0, "2", false),
        http_status_ntp(base + 5, 2600, "3", false),
        http_status_ntp(base + 10, -2200, "2", false),
        // Resample round: clean bimodal signature (0, 0, 2600) -- must be excused.
        http_status_ntp(base + 15, 0, "4", false),
        http_status_ntp(base + 20, 0, "5", false),
        http_status_ntp(base + 25, 2600, "6", false),
    ];
    let p_stream =
        write_multi_read_fixture("stream_1022_resample_excused_client", &stream_responses);

    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_strih.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_stream.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "a resampled round that shows a clean chase signature must be excused, not graded raw. \
         stdout={stdout} stderr={stderr}"
    );
    let stream_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("stream"))
        .unwrap_or_else(|| panic!("no stream report line in stdout: {stdout}"));
    assert!(
        stream_line.contains("resampled once")
            && stream_line.contains("explained by master step-chase"),
        "must show BOTH the resample note (round 1 declined exclusion) AND the exclusion note \
         (round 2 -- the resampled data -- was excused): {stream_line:?}"
    );
    assert!(
        !stream_line.contains("UNSTABLE"),
        "stream_line: {stream_line:?}"
    );
    let _ = stderr;
}

// --- #1055: slew-aware CLIENT rescue via journal step-correlation (end-to-end) ----------------
//
// A CLIENT sampled by HTTP whose 30 s window lands in a master deadband-slew plateau reads a
// majority-elevated set -> the median DRIFTs. When the master's own /status is unreadable at
// gate-prime time (the ~50% intermittency), the #1022 widening never applies and the client
// false-DRIFTs. The client's OWN journal proves the spikes are step-correlated transients and its
// step-excluded baseline is us-grade -> the gate must PASS. A genuine sustained desync (the
// step-excluded baseline is itself elevated, or nothing survives) must still FAIL. Journals are
// injected via DANTESYNC_GATE_LINUX_JOURNAL_CAM1 (the pre-captured live 2026-08-14 shape).

const GATE_JOURNAL_SLEW_TRANSIENT_CAM1: &str = "\
2026-08-14T12:49:30+00:00 CAM1 dantesync[703]: [NTP] offset:+2740us (threshold:585us, adaptive)
2026-08-14T12:49:30+00:00 CAM1 dantesync[703]: [NTP] step candidate +2740us (threshold:585us) — awaiting 1 agreeing sample(s)
2026-08-14T12:50:00+00:00 CAM1 dantesync[703]: [NTP] offset:+2776us (threshold:585us, adaptive)
2026-08-14T12:50:00+00:00 CAM1 dantesync[703]: [NTP] Stepped +2776us
2026-08-14T12:50:30+00:00 CAM1 dantesync[703]: [NTP] offset:-29us
2026-08-14T12:51:00+00:00 CAM1 dantesync[703]: [NTP] offset:-32us
2026-08-14T12:51:30+00:00 CAM1 dantesync[703]: [NTP] offset:-36us (threshold:515us, adaptive)
2026-08-14T12:52:00+00:00 CAM1 dantesync[703]: [NTP] offset:+3229us (threshold:535us, adaptive)
2026-08-14T12:52:00+00:00 CAM1 dantesync[703]: [NTP] step candidate +3229us (threshold:535us) — awaiting 1 agreeing sample(s)
2026-08-14T12:52:31+00:00 CAM1 dantesync[703]: [NTP] offset:+3331us (threshold:535us, adaptive)
2026-08-14T12:52:31+00:00 CAM1 dantesync[703]: [NTP] Stepped +3331us
2026-08-14T12:53:01+00:00 CAM1 dantesync[703]: [NTP] offset:-114us
2026-08-14T12:53:31+00:00 CAM1 dantesync[703]: [NTP] offset:-111us
2026-08-14T12:54:01+00:00 CAM1 dantesync[703]: [NTP] offset:-113us (threshold:505us, adaptive)
2026-08-14T12:54:07+00:00 CAM1 dantesync[703]: [PTP] LOCK  Drift:  -0.5us/s  Adj: -14.4ppm
";

// Genuine sustained desync: every offset ~+3000us, the daemon step-candidating/Stepping every
// cycle -- so every sample is step-adjacent and NOTHING survives exclusion.
const GATE_JOURNAL_SUSTAINED_DRIFT_CAM1: &str = "\
2026-08-14T13:00:00+00:00 CAM1 dantesync[703]: [NTP] offset:+3200us (threshold:520us, adaptive)
2026-08-14T13:00:00+00:00 CAM1 dantesync[703]: [NTP] step candidate +3200us (threshold:520us) — awaiting 1 agreeing sample(s)
2026-08-14T13:00:30+00:00 CAM1 dantesync[703]: [NTP] offset:+3205us (threshold:520us, adaptive)
2026-08-14T13:00:30+00:00 CAM1 dantesync[703]: [NTP] Stepped +3205us
2026-08-14T13:01:00+00:00 CAM1 dantesync[703]: [NTP] offset:+3210us (threshold:520us, adaptive)
2026-08-14T13:01:00+00:00 CAM1 dantesync[703]: [NTP] step candidate +3210us (threshold:520us) — awaiting 1 agreeing sample(s)
2026-08-14T13:01:30+00:00 CAM1 dantesync[703]: [NTP] offset:+3215us (threshold:520us, adaptive)
2026-08-14T13:01:30+00:00 CAM1 dantesync[703]: [NTP] Stepped +3215us
2026-08-14T13:02:00+00:00 CAM1 dantesync[703]: [NTP] offset:+3220us (threshold:520us, adaptive)
2026-08-14T13:02:00+00:00 CAM1 dantesync[703]: [NTP] step candidate +3220us (threshold:520us) — awaiting 1 agreeing sample(s)
";

#[test]
fn gate_rescues_a_client_master_slew_transient_via_journal_step_correlation_1055() {
    // 6 HTTP samples land in the slew plateau: 1 baseline (+14) + 5 spike -> median +2759us ->
    // drift_unstable at the bare 2000us bound. --ntp-master "" opts OUT of the master concept
    // (equivalent to the master's /status being unreadable -> no #1022 widening). WITHOUT the fix
    // this DRIFTs (code 20); WITH the journal step-correlation rescue the gate PASSES (0), because
    // cam1's own journal proves the spikes are step-transients and the baseline is us-grade.
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 14, "25", false),
        http_status_ntp(base + 1, 2740, "25", false),
        http_status_ntp(base + 2, 2776, "25", false),
        http_status_ntp(base + 3, 3229, "25", false),
        http_status_ntp(base + 4, 3254, "25", false),
        http_status_ntp(base + 5, 2759, "25", false),
    ];
    let p = write_multi_read_fixture("cam1_slew_transient_1055", &responses);
    let j = write_dante_journal("cam1_slew_transient_1055", GATE_JOURNAL_SLEW_TRANSIENT_CAM1);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--ntp-master",
            "",
            "--samples",
            "6",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", &p.display().to_string()),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "#1055: a client whose HTTP median lands in a master step-chase plateau, whose own \
         journal proves the spikes are step-correlated transients over a us-grade baseline, must \
         PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("cam1") && stdout.contains("OK"),
        "cam1 must report OK: {stdout}"
    );
}

#[test]
fn gate_still_fails_a_genuine_sustained_client_desync_with_the_slew_rescue_1055() {
    // Adversarial: 6 HTTP samples ALL elevated (~+3200us) -> median drifts, AND cam1's journal is
    // a sustained desync (stepping every cycle, nothing survives exclusion). The rescue must NOT
    // mask it -> the gate still FAILS (20).
    let base = now_epoch();
    let responses = vec![
        http_status_ntp(base, 3200, "25", false),
        http_status_ntp(base + 1, 3205, "25", false),
        http_status_ntp(base + 2, 3210, "25", false),
        http_status_ntp(base + 3, 3215, "25", false),
        http_status_ntp(base + 4, 3220, "25", false),
        http_status_ntp(base + 5, 3225, "25", false),
    ];
    let p = write_multi_read_fixture("cam1_sustained_drift_1055", &responses);
    let j = write_dante_journal(
        "cam1_sustained_drift_1055",
        GATE_JOURNAL_SUSTAINED_DRIFT_CAM1,
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--ntp-master",
            "",
            "--samples",
            "6",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_LINUX_HTTP_CAM1", &p.display().to_string()),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "#1055: a genuine sustained desync (no step-excluded baseline survives) must still FAIL, \
         never be masked by the slew rescue. stdout={stdout} stderr={stderr}"
    );
    assert!(stderr.contains("GATE FAILED"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------------------------------
// #834 — grandmaster IDENTITY. A node PTP-locked to a FOREIGN grandmaster (stream box, 2026-07-28
// and still live 2026-08-15: gm_source_ip=10.77.7.109, is_locked=true) reads every LOCAL health
// indicator green and, when its instantaneous offset is small, PASSES the offset+PTP-only grade.
// gm_check now compares each HTTP-graded node's gm_source_ip against the rig grandmaster
// (10.77.9.184). REPORT-FIRST: the GM status line is ALWAYS printed loudly, but only feeds the
// node's OK/BAD verdict when DANTESYNC_GATE_GM_ENFORCE=1 (default off) -- so wiring it cannot brick
// the standing E2E gate while the stream box still elects a foreign GM (a rig/dantesync fix tracked
// separately). Mirrors verify-imag.sh:948's gm_check call for the imag path.
// ---------------------------------------------------------------------------------------------

/// A single-read HTTP fixture for the GM tests: fresh (updated_ts = now), locked, in-bound offset,
/// so offset+PTP both PASS -- isolating the grandmaster-identity behavior. `gm_prefix` is spliced
/// in verbatim at the front of the JSON object, so pass either e.g.
/// `"\"gm_source_ip\":\"10.77.7.109\","` or `""` (field entirely absent).
fn write_gm_fixture(name: &str, gm_prefix: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let json = format!(
        "{{{gm_prefix}\"settled\":true,\"updated_ts\":{now},\"is_locked\":true,\
         \"ntp_offset_us\":0,\"mode\":\"NANO\",\"ntp_failed\":false}}"
    );
    write_win_http_fixture(name, &json)
}

#[test]
fn gate_reports_foreign_grandmaster_but_stays_report_only_by_default_834() {
    // The stream box's real 2026-07-28/2026-08-15 fault: gm_source_ip=10.77.7.109 while locked +
    // in-bound. Report-only default -> the gate PASSES (does not brick E2E) but names the fault.
    let p = write_gm_fixture("stream_foreign_gm", "\"gm_source_ip\":\"10.77.7.109\",");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            // only "stream" is configured, no "strih" -> opt OUT of the master-name validation.
            "--ntp-master",
            "",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "report-only default: a foreign GM must NOT block the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("GM FOREIGN"),
        "the foreign grandmaster must be reported loudly. stdout={stdout}"
    );
    assert!(
        stdout.contains("10.77.7.109"),
        "the foreign GM ip must be named. stdout={stdout}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_fails_a_foreign_grandmaster_node_when_gm_enforce_is_set_834() {
    // DANTESYNC_GATE_GM_ENFORCE=1 flips the report-only check to a hard gate: a foreign GM is BAD
    // (exit 20), exactly like a DRIFT/PTP-degraded node -- the future state once the rig is fixed.
    let p = write_gm_fixture(
        "stream_foreign_gm_enforced",
        "\"gm_source_ip\":\"10.77.7.109\",",
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            // only "stream" is configured, no "strih" -> opt OUT of the master-name validation.
            "--ntp-master",
            "",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_GM_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 20,
        "enforce: a foreign GM must be a hard failure (20). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM FOREIGN"), "stdout={stdout}");
    assert!(stderr.contains("FAILED"), "stderr={stderr}");
}

#[test]
fn gate_reports_gm_ok_for_a_node_on_the_rig_grandmaster_834() {
    // A node on the rig grandmaster (10.77.9.184) is confirmed OK by name, and passes.
    let p = write_gm_fixture("strih_gm_ok", "\"gm_source_ip\":\"10.77.9.184\",");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "a node on the rig GM passes. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM OK"), "stdout={stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_reports_gm_unknown_when_gm_source_ip_absent_and_enforce_set_834() {
    // A payload with NO gm_source_ip field: the grandmaster is UNREADABLE, which must never look
    // correct (test-strictness: unreachable = fail). Under enforce that is INCOMPLETE (11), never
    // a silent pass -- the same "unreadable is not OK" contract offset/PTP already follow.
    let p = write_gm_fixture("stream_gm_absent", "");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            // only "stream" is configured, no "strih" -> opt OUT of the master-name validation.
            "--ntp-master",
            "",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_GM_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 11,
        "enforce: an unreadable GM must be INCOMPLETE (11), never a silent pass. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM UNKNOWN"), "stdout={stdout}");
    assert!(stderr.contains("INCOMPLETE"), "stderr={stderr}");
}

#[test]
fn help_documents_the_grandmaster_identity_check_834() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.to_lowercase().contains("grandmaster"),
        "help must document the #834 grandmaster-identity check: {stdout}"
    );
    assert!(
        stdout.contains("DANTESYNC_GATE_GM_ENFORCE"),
        "help must document the report-first enforce flag: {stdout}"
    );
}

#[test]
fn gate_reports_gm_unknown_report_only_still_passes_834() {
    // #834 review 🔵: absent gm_source_ip with enforce OFF (default) -- the GM is unreadable and
    // reported UNKNOWN, but report-only must NOT change the verdict; the node still passes on
    // offset+PTP. Complements the absent+enforce case (which is INCOMPLETE/11).
    let p = write_gm_fixture("stream_gm_absent_reportonly", "");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "stream=10.77.9.204",
            // only "stream" is configured, no "strih" -> opt OUT of the master-name validation.
            "--ntp-master",
            "",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "report-only: an unreadable GM must NOT block the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM UNKNOWN"), "stdout={stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_passes_a_node_on_the_rig_grandmaster_under_enforce_834() {
    // #834 review 🔵: the enforce HAPPY path -- a node on the rig GM passes even with
    // DANTESYNC_GATE_GM_ENFORCE=1, locking the pass side of the enforce path (complements the
    // enforce-FAIL cases: foreign=>20, unknown=>11).
    let p = write_gm_fixture("strih_gm_ok_enforced", "\"gm_source_ip\":\"10.77.9.184\",");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "1",
            "--min-distinct",
            "1",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string()),
            ("DANTESYNC_GATE_GM_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "enforce + rig GM must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM OK"), "stdout={stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

// --- #1119: master-scope the median bound (step-cap ceiling) + step-storm guard ---------------
//
// v1.8.46 reports ntp_deadband_us=1000 (the no-step threshold), NOT the ≤2500us bounded per-step
// CAP the master's own UTC offset actually sawtooths toward under a slow grandmaster (root-caused
// 2026-08-18). deadband(1000)+margin(1000)=2000 gives NO widening (#1021), so a healthy sawtooth
// median (live failed run: 2699us) false-DRIFTs the bare 2000us bound -- a per-window coin flip.
// The gate now grades the master's median against the step-cap ceiling (2500+1000=3500us) AND
// hard-fails on dantesync's own ntp_step_storm=true. CLIENT rows' 2000us bound is UNTOUCHED.

/// The v1.8.46 master `/status` shape: adds ntp_step_storm + ntp_steps_last_hour to the #1021
/// deadband payload, and lets is_locked/mode/gm_source_ip vary for the failure fixtures.
#[allow(clippy::too_many_arguments)]
fn http_status_master_1119(
    ts: u64,
    offset_us: i64,
    deadband_raw: &str,
    storm: bool,
    steps_raw: &str,
    is_locked: bool,
    mode: &str,
    gm_ip: &str,
) -> String {
    format!(
        "{{\"gm_source_ip\":\"{gm_ip}\",\"settled\":true,\"updated_ts\":{ts},\
         \"is_locked\":{is_locked},\"ntp_offset_us\":{offset_us},\"mode\":\"{mode}\",\
         \"ntp_failed\":false,\"ntp_updated_ts\":{ts},\"ntp_age_s\":0,\
         \"ntp_deadband_us\":{deadband_raw},\"ntp_step_storm\":{storm},\
         \"ntp_steps_last_hour\":{steps_raw}}}"
    )
}

#[test]
fn gate_win_http_master_bounded_step_sawtooth_passes_via_step_cap_1119() {
    // The exact round-33 failed-run shape: median 2699us, deadband 1000, LOCK, GM OK, storm false.
    // Pre-#1119 this false-DRIFTs the bare 2000us bound (proven live 2026-08-19); after, the
    // step-cap floor (3500us) must let it PASS.
    let base = now_epoch();
    let responses = vec![
        http_status_master_1119(base, 2400, "1000", false, "85", true, "LOCK", "10.77.9.184"),
        http_status_master_1119(
            base + 5,
            2600,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 10,
            2699,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 15,
            2900,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 20,
            3100,
            "1000",
            false,
            "86",
            true,
            "LOCK",
            "10.77.9.184",
        ),
    ];
    let p = write_multi_read_fixture("strih_1119_sawtooth", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "5",
            "--min-distinct",
            "5",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "median 2699us with deadband 1000 must PASS via the step-cap floor (3500us), not \
         false-DRIFT the bare 2000us bound. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
    assert!(!stdout.contains("DRIFT"), "stdout={stdout}");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("step-cap") || low.contains("bounded-step"),
        "the widened-bound note must say WHY the master is graded on the step-cap, not raw median: {stdout}"
    );
}

#[test]
fn gate_win_http_master_step_storm_fails_even_with_in_bound_median_1119() {
    // A thrashing master (ntp_step_storm=true, steps/h past its 120 alarm) whose median sits
    // in-bound must be a HARD fail regardless of median -- the step-cap widening must never let a
    // storming master through. Pre-#1119 this PASSES (proven live 2026-08-19); after, it FAILS.
    let base = now_epoch();
    let responses = vec![
        http_status_master_1119(base, 1400, "1000", true, "240", true, "LOCK", "10.77.9.184"),
        http_status_master_1119(
            base + 5,
            1500,
            "1000",
            true,
            "240",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 10,
            1600,
            "1000",
            true,
            "245",
            true,
            "LOCK",
            "10.77.9.184",
        ),
    ];
    let p = write_multi_read_fixture("strih_1119_storm", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "an affirmative ntp_step_storm must FAIL even with an in-bound median. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("STORM"),
        "stdout must name the storm: {stdout}"
    );
}

#[test]
fn gate_win_http_master_huge_offset_still_fails_under_step_cap_1119() {
    // The step-cap floor is a numeric gross-desync ceiling, not a blanket pass: a 15ms offset
    // (a genuine desync the bounded step cannot produce) must still DRIFT far beyond 3500us.
    let base = now_epoch();
    let responses = vec![
        http_status_master_1119(
            base,
            15000,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 5,
            15100,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 10,
            14900,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.9.184",
        ),
    ];
    let p = write_multi_read_fixture("strih_1119_huge_offset", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "a genuine 15ms offset must still DRIFT beyond the step-cap ceiling (3500us). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("DRIFT"), "stdout={stdout}");
}

#[test]
fn gate_win_http_master_unlocked_still_fails_1119() {
    // is_locked=false / mode not NANO|LOCK -> PTP DEGRADED -> node BAD, regardless of the widened
    // offset bound. The step-cap median widening must not shadow the PTP-lock gate.
    let base = now_epoch();
    let responses = vec![
        http_status_master_1119(base, 500, "1000", false, "85", false, "NTP", "10.77.9.184"),
        http_status_master_1119(
            base + 5,
            550,
            "1000",
            false,
            "85",
            false,
            "NTP",
            "10.77.9.184",
        ),
        http_status_master_1119(
            base + 10,
            520,
            "1000",
            false,
            "85",
            false,
            "NTP",
            "10.77.9.184",
        ),
    ];
    let p = write_multi_read_fixture("strih_1119_unlocked", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string())],
    );
    assert_eq!(
        code, 20,
        "an unlocked master (PTP degraded) must FAIL even with a tiny in-bound offset. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("PTP DEGRADED"), "stdout={stdout}");
}

#[test]
fn gate_win_http_master_foreign_gm_still_fails_under_enforce_1119() {
    // With DANTESYNC_GATE_GM_ENFORCE=1, a master PTP-locked to a FOREIGN grandmaster must FAIL
    // even with a tiny offset and LOCKED -- the step-cap widening must not shadow the #834 GM gate.
    let base = now_epoch();
    let responses = vec![
        http_status_master_1119(base, 500, "1000", false, "85", true, "LOCK", "10.77.7.109"),
        http_status_master_1119(
            base + 5,
            550,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.7.109",
        ),
        http_status_master_1119(
            base + 10,
            520,
            "1000",
            false,
            "85",
            true,
            "LOCK",
            "10.77.7.109",
        ),
    ];
    let p = write_multi_read_fixture("strih_1119_foreign_gm", &responses);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--samples",
            "3",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STRIH", &p.display().to_string()),
            ("DANTESYNC_GATE_GM_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 20,
        "a foreign-GM master under enforce must FAIL despite a tiny offset + LOCK. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GM FOREIGN"), "stdout={stdout}");
}

#[test]
fn help_describes_the_master_step_cap_1119() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("step-cap") || low.contains("step cap"),
        "#1119: usage must document the master step-cap median treatment: {stdout}"
    );
    assert!(
        low.contains("ntp_step_storm") || low.contains("storm"),
        "#1119: usage must document the master step-storm guard: {stdout}"
    );
}

// --- #1123: client STABILITY (spread) bound is step-aware ---------------------------------------
//
// The issue-1022/1041 median widening learned the step-chase envelope; the STABILITY (spread, fixed
// 2000us) bound did not. A linux client whose own bounded step lands mid-window straddles the step
// -> spread ~= its step magnitude (live cam1: 2938us) -> false UNSTABLE while PTP LOCKED + GM OK.
// The stability bound now widens to the client's own journal step envelope (max threshold + margin).

/// The real cam1 journal shape around its 2026-08-19T01:34 failure: adaptive thresholds jitter up
/// to 6860us while the offset ramps toward a +2938us step.
const CAM1_STRADDLE_JOURNAL_1123: &str = "\
2026-08-19T01:33:11+00:00 CAM1 dantesync[1450558]: [NTP] offset:+1869us (threshold:2640us, adaptive)\n\
2026-08-19T01:33:42+00:00 CAM1 dantesync[1450558]: [NTP] offset:+1848us (threshold:6860us, adaptive)\n\
2026-08-19T01:34:12+00:00 CAM1 dantesync[1450558]: [PTP] NANO Drift: 3 ns\n\
2026-08-19T01:34:30+00:00 CAM1 dantesync[1450558]: [NTP] offset:+2938us (threshold:775us, adaptive)\n\
2026-08-19T01:34:45+00:00 CAM1 dantesync[1450558]: [NTP] Stepped +2938us\n";

/// A stable client's journal -- only tiny adaptive thresholds, so a big spread cannot be excused.
const CAM1_STABLE_JOURNAL_1123: &str = "\
2026-08-19T01:33:11+00:00 CAM1 dantesync[1450558]: [NTP] offset:+120us (threshold:500us, adaptive)\n\
2026-08-19T01:34:12+00:00 CAM1 dantesync[1450558]: [PTP] NANO Drift: 3 ns\n\
2026-08-19T01:34:30+00:00 CAM1 dantesync[1450558]: [NTP] offset:+140us (threshold:520us, adaptive)\n";

/// A healthy master (strih) priming/status fixture: deadband 1000, LOCK, small median.
fn strih_master_1123(base: u64) -> Vec<String> {
    vec![
        http_status_ntp_deadband(base, 428, "2", false, "1000"),
        http_status_ntp_deadband(base + 5, 512, "3", false, "1000"),
        http_status_ntp_deadband(base + 10, 470, "2", false, "1000"),
    ]
}

#[test]
fn gate_linux_client_step_straddle_spread_passes_via_stability_widening_1123() {
    // cam1's exact failed shape: samples 0/1848/1924/2900/2938 -> median 1924, spread 2938. Its own
    // journal shows an adaptive threshold up to 6860us -> stability floor max(2000,6860+1000)=7860,
    // so the straddle spread grades tight. Pre-#1123 this false-UNSTABLE (proven live 2026-08-19).
    let base = now_epoch();
    let p_master = write_multi_read_fixture("strih_1123_straddle_master", &strih_master_1123(base));
    let client = vec![
        http_status_ntp_deadband(base, 0, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 1848, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 1924, "2", false, "null"),
        http_status_ntp_deadband(base + 15, 2900, "3", false, "null"),
        http_status_ntp_deadband(base + 20, 2938, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("cam1_1123_straddle_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1123_straddle_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let j = write_dante_journal("cam1_1123_straddle", CAM1_STRADDLE_JOURNAL_1123);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--win-http",
            "strih=10.77.9.202",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "a client step-straddle (median 1924us, spread 2938us) whose own journal step envelope is \
         6860us must PASS, not false-UNSTABLE against the fixed 2000us stability. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
    assert!(!stdout.contains("UNSTABLE"), "stdout={stdout}");
    let low = stdout.to_lowercase();
    assert!(
        low.contains("stability") && (low.contains("step") || low.contains("threshold")),
        "the widened note must say the stability bound is step-aware and WHY: {stdout}"
    );
}

#[test]
fn gate_linux_client_genuine_scatter_still_fails_stability_1123() {
    // A genuinely-scattered client (spread 15000us) whose own journal shows only a tiny threshold
    // (500us) must STILL FAIL -- the step-aware widening is bounded by the client's OWN envelope.
    let base = now_epoch();
    let p_master = write_multi_read_fixture("strih_1123_scatter_master", &strih_master_1123(base));
    let client = vec![
        http_status_ntp_deadband(base, 0, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 500, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 900, "2", false, "null"),
        http_status_ntp_deadband(base + 15, 8000, "3", false, "null"),
        http_status_ntp_deadband(base + 20, 15000, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("cam1_1123_scatter_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1123_scatter_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let j = write_dante_journal("cam1_1123_scatter", CAM1_STABLE_JOURNAL_1123);
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--win-http",
            "strih=10.77.9.202",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_JOURNAL_CAM1",
                &j.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a genuine 15000us-spread scatter whose own journal envelope is only 500us must still FAIL. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("UNSTABLE") || stdout.contains("DRIFT"),
        "must name the scatter failure: {stdout}"
    );
}

#[test]
fn gate_linux_client_unreadable_journal_keeps_fixed_stability_1123() {
    // "Cannot prove the step envelope -> do not widen": an unreadable journal (the #686 NO_HTTP
    // sentinel) means no journal threshold, so the straddle spread grades against the fixed 2000us
    // stability and still FAILS -- the widening never widens on an unproven envelope.
    let base = now_epoch();
    let p_master =
        write_multi_read_fixture("strih_1123_nojournal_master", &strih_master_1123(base));
    let client = vec![
        http_status_ntp_deadband(base, 0, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 1848, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 1924, "2", false, "null"),
        http_status_ntp_deadband(base + 15, 2900, "3", false, "null"),
        http_status_ntp_deadband(base + 20, 2938, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("cam1_1123_nojournal_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1123_nojournal_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let (code, stdout, _stderr) = run_gate_env(
        &[
            "--linux",
            "cam1=10.77.9.61",
            "--win-http",
            "strih=10.77.9.202",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_LINUX_HTTP_CAM1",
                &p_client.display().to_string(),
            ),
            ("DANTESYNC_GATE_LINUX_JOURNAL_CAM1", NO_HTTP),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "no readable journal -> no step envelope -> the fixed 2000us stability still fails the 2938us spread. stdout={stdout}"
    );
    assert!(stdout.contains("UNSTABLE"), "stdout={stdout}");
}

// --- #1129: a WINDOWS client's step envelope comes from /status, not a journal ------------------
//
// The #1123 client STABILITY (spread) widening reads the step envelope from the client's own
// journal -- but grade_http_node reads a journal ONLY for kind="linux". A Windows client (stream)
// therefore fell back to the fixed 700us step term, so its stability bound stayed the base 2000us
// and a healthy step-straddle spread (~3.4ms on strih/stream) false-UNSTABLE'd the whole E2E
// (PR #1125 attempt 4, "client step threshold via fallback(700us)"). dantesync now exposes the
// client's OWN currently-active adaptive step threshold in /status (ntp_step_threshold_us, the
// SAME quantity a Linux journal logs as "threshold:NNNus"); the win-http branch reads its window
// MAX and feeds it into the SAME median + spread widening cam2 gets from its journal.

/// http_status_ntp_deadband plus the #1129 dantesync "ntp_step_threshold_us" field (the client's
/// own currently-active adaptive step threshold; a plain integer string or the literal "null").
fn http_status_ntp_step_threshold(
    ts: u64,
    offset_us: i64,
    ntp_age_s_raw: &str,
    ntp_failed: bool,
    deadband_raw: &str,
    step_threshold_raw: &str,
) -> String {
    format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{ts},\
         \"is_locked\":true,\"ntp_offset_us\":{offset_us},\"mode\":\"NANO\",\
         \"ntp_failed\":{ntp_failed},\"ntp_updated_ts\":{ts},\"ntp_age_s\":{ntp_age_s_raw},\
         \"ntp_deadband_us\":{deadband_raw},\"ntp_step_threshold_us\":{step_threshold_raw}}}"
    )
}

#[test]
fn gate_win_http_client_step_straddle_spread_passes_via_status_step_threshold_1129() {
    // stream's exact PR #1125 attempt-4 shape: samples 0/1848/1924/2900/2938 -> median 1924,
    // spread 2938. Its /status reports ntp_step_threshold_us=3400 -> stability floor
    // max(2000, 3400+1000)=4400, so the straddle spread grades tight. Pre-#1129 a win client had
    // no journal, fell back to 700us, kept the 2000us stability, and false-UNSTABLE'd (exit 20).
    let base = now_epoch();
    let master = vec![
        http_status_ntp_deadband(base, 428, "2", false, "1000"),
        http_status_ntp_deadband(base + 5, 512, "3", false, "1000"),
        http_status_ntp_deadband(base + 10, 470, "2", false, "1000"),
    ];
    let p_master = write_multi_read_fixture("strih_1129_straddle_master", &master);
    let client = vec![
        http_status_ntp_step_threshold(base, 0, "2", false, "null", "3400"),
        http_status_ntp_step_threshold(base + 5, 1848, "3", false, "null", "3400"),
        http_status_ntp_step_threshold(base + 10, 1924, "2", false, "null", "3400"),
        http_status_ntp_step_threshold(base + 15, 2900, "3", false, "null", "3400"),
        http_status_ntp_step_threshold(base + 20, 2938, "2", false, "null", "3400"),
    ];
    let p_client = write_multi_read_fixture("stream_1129_straddle_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1129_straddle_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "a Windows client step-straddle (median 1924us, spread 2938us) whose /status step \
         threshold is 3400us must PASS, not false-UNSTABLE on the 700us fallback. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
    assert!(
        !stdout.contains("UNSTABLE"),
        "stream must not be UNSTABLE after the /status step widening: {stdout}"
    );
    let low = stdout.to_lowercase();
    assert!(
        low.contains("/status") && low.contains("3400"),
        "the widened note must admit the /status step threshold source + value: {stdout}"
    );
}

#[test]
fn gate_win_http_client_genuine_scatter_still_fails_stability_1129() {
    // A genuinely-scattered Windows client (spread 15000us) whose /status step threshold is only
    // 500us must STILL FAIL -- the widening is bounded by the client's OWN envelope, not blanket.
    let base = now_epoch();
    let master = vec![
        http_status_ntp_deadband(base, 428, "2", false, "1000"),
        http_status_ntp_deadband(base + 5, 512, "3", false, "1000"),
        http_status_ntp_deadband(base + 10, 470, "2", false, "1000"),
    ];
    let p_master = write_multi_read_fixture("strih_1129_scatter_master", &master);
    let client = vec![
        http_status_ntp_step_threshold(base, 0, "2", false, "null", "500"),
        http_status_ntp_step_threshold(base + 5, 500, "3", false, "null", "500"),
        http_status_ntp_step_threshold(base + 10, 900, "2", false, "null", "500"),
        http_status_ntp_step_threshold(base + 15, 8000, "3", false, "null", "500"),
        http_status_ntp_step_threshold(base + 20, 15000, "2", false, "null", "500"),
    ];
    let p_client = write_multi_read_fixture("stream_1129_scatter_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1129_scatter_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a genuine 15000us-spread scatter whose /status envelope is only 500us must still FAIL. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("UNSTABLE") || stdout.contains("DRIFT"),
        "must name the scatter failure: {stdout}"
    );
}

#[test]
fn gate_win_http_client_missing_step_threshold_field_falls_back_to_700_1129() {
    // Graceful backward-compat: a Windows client on a box NOT yet serving ntp_step_threshold_us
    // (no field at all) falls back to the 700us step term exactly as before #1129 -> the 2938us
    // straddle spread still fails the fixed 2000us stability. Nothing regresses pre-fleet-upgrade;
    // the note keeps admitting the fallback.
    let base = now_epoch();
    let master = vec![
        http_status_ntp_deadband(base, 428, "2", false, "1000"),
        http_status_ntp_deadband(base + 5, 512, "3", false, "1000"),
        http_status_ntp_deadband(base + 10, 470, "2", false, "1000"),
    ];
    let p_master = write_multi_read_fixture("strih_1129_nofield_master", &master);
    let client = vec![
        http_status_ntp_deadband(base, 0, "2", false, "null"),
        http_status_ntp_deadband(base + 5, 1848, "3", false, "null"),
        http_status_ntp_deadband(base + 10, 1924, "2", false, "null"),
        http_status_ntp_deadband(base + 15, 2900, "3", false, "null"),
        http_status_ntp_deadband(base + 20, 2938, "2", false, "null"),
    ];
    let p_client = write_multi_read_fixture("stream_1129_nofield_client", &client);
    let p_priming = write_win_http_fixture(
        "strih_1129_nofield_priming",
        &http_status_ntp_deadband(base, 470, "2", false, "1000"),
    );
    let (code, stdout, _stderr) = run_gate_env(
        &[
            "--linux",
            "",
            "--win-http",
            "strih=10.77.9.202",
            "--win-http",
            "stream=10.77.9.204",
            "--ntp-master",
            "strih",
            "--samples",
            "5",
            "--min-distinct",
            "3",
            "--window-s",
            "0",
            "--chase-resample-delay-s",
            "0",
        ],
        &[
            (
                "DANTESYNC_GATE_WIN_HTTP_STRIH",
                &p_master.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_WIN_HTTP_STREAM",
                &p_client.display().to_string(),
            ),
            (
                "DANTESYNC_GATE_MASTER_DEADBAND_STATUS",
                &p_priming.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "no ntp_step_threshold_us field -> 700us fallback -> the fixed 2000us stability still fails the 2938us spread. stdout={stdout}"
    );
    assert!(stdout.contains("UNSTABLE"), "stdout={stdout}");
    assert!(
        stdout.contains("fallback(700us)"),
        "the note must still admit the 700us fallback on a box not serving the field: {stdout}"
    );
}

// ---------------------------------------------------------------------------------------------
// #1130 — phase_slew (dantesync issue 97) is the fleet-wide CURE for the chronic NTP step storm
// this ticket tracks: a bounded rate-slew that absorbs UTC phase error instead of stepping it.
// It is a per-box config toggle, so a box that silently reverts to phase_slew=off re-introduces
// the storm (uncaught until dantesync's own >120/h ntp_step_storm alarm, far above the visible-
// judder threshold). grade_http_node now checks it REPORT-FIRST, exactly like the #834 gm_check:
// the PHASE-SLEW ENABLED/DISABLED/UNKNOWN line is ALWAYS printed per HTTP-graded node, but only
// feeds the node's OK/BAD verdict when DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1 (default off) -- so
// wiring it cannot brick the standing E2E gate before the enforce flip. Reuses the #1215 pure
// functions (phase_slew_enabled_from_pipe_json/phase_slew_check) already used by verify-imag.sh.
// ---------------------------------------------------------------------------------------------

/// A single-read HTTP fixture for the phase_slew tests: fresh (updated_ts = now), locked, in-bound
/// offset, on the rig grandmaster -- so offset+PTP+GM all PASS, isolating the phase_slew behavior.
/// `ps_fragment` is spliced verbatim, so pass e.g. `",\"phase_slew_enabled\":false"`,
/// `",\"phase_slew_enabled\":true"`, or `""` (field entirely absent).
fn write_phase_slew_fixture(name: &str, ps_fragment: &str) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let json = format!(
        "{{\"gm_source_ip\":\"10.77.9.184\",\"settled\":true,\"updated_ts\":{now},\
         \"is_locked\":true,\"ntp_offset_us\":0,\"mode\":\"NANO\",\"ntp_failed\":false{ps_fragment}}}"
    );
    write_win_http_fixture(name, &json)
}

/// The stream node args used by these tests: only "stream" is configured (no "strih"), so
/// --ntp-master "" opts OUT of the master-name validation -- grading stream as a plain client,
/// the same isolation the gm foreign-GM test uses.
fn phase_slew_stream_args() -> Vec<&'static str> {
    vec![
        "--linux",
        "",
        "--win-http",
        "stream=10.77.9.204",
        "--ntp-master",
        "",
        "--samples",
        "1",
        "--min-distinct",
        "1",
        "--window-s",
        "0",
    ]
}

#[test]
fn gate_reports_phase_slew_disabled_but_stays_report_only_by_default_1130() {
    // phase_slew=off (the box would STEP -> visible judder), report-only default -> the gate names
    // the fault loudly but PASSES (does not brick E2E), byte-identical verdict to pre-#1130.
    let p = write_phase_slew_fixture("stream_ps_disabled", ",\"phase_slew_enabled\":false");
    let (code, stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "report-only default: disabled phase_slew must NOT block the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("PHASE-SLEW DISABLED"),
        "the disabled phase_slew must be reported loudly. stdout={stdout}"
    );
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_fails_a_phase_slew_disabled_node_when_enforce_is_set_1130() {
    // DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1 flips the report-only check to a hard gate: a box that
    // would STEP is BAD (exit 20), exactly like a DRIFT/PTP-degraded node -- the future enforce state.
    let p = write_phase_slew_fixture(
        "stream_ps_disabled_enforced",
        ",\"phase_slew_enabled\":false",
    );
    let (code, stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_PHASE_SLEW_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 20,
        "enforce: a disabled phase_slew must be a hard failure (20). stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("PHASE-SLEW DISABLED"), "stdout={stdout}");
    assert!(stderr.contains("FAILED"), "stderr={stderr}");
}

#[test]
fn gate_passes_a_phase_slew_enabled_node_under_enforce_1130() {
    // The enforce HAPPY path -- a box on phase_slew (the live fleet state, 2026-09-01) passes even
    // with enforce on, locking the pass side (complements the enforce-FAIL cases: disabled=>20).
    let p = write_phase_slew_fixture("stream_ps_enabled_enforced", ",\"phase_slew_enabled\":true");
    let (code, stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_PHASE_SLEW_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "enforce + phase_slew enabled must PASS. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("PHASE-SLEW ENABLED"), "stdout={stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_reports_phase_slew_unknown_report_only_still_passes_1130() {
    // A payload with NO phase_slew_enabled field: unreadable, which must never look correct
    // (test-strictness). Report-only default -> reported UNKNOWN but the node still passes on
    // offset+PTP+GM (complements the absent+enforce INCOMPLETE case below).
    let p = write_phase_slew_fixture("stream_ps_absent_reportonly", "");
    let (code, stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string())],
    );
    assert_eq!(
        code, 0,
        "report-only: an unreadable phase_slew must NOT block the gate. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("PHASE-SLEW UNKNOWN"), "stdout={stdout}");
    assert!(stdout.contains("GATE PASS"), "stdout={stdout}");
}

#[test]
fn gate_reports_phase_slew_unknown_when_absent_and_enforce_set_1130() {
    // Absent phase_slew_enabled under enforce is INCOMPLETE (11), never a silent pass -- the same
    // "unreadable is not OK" contract offset/PTP/GM already follow.
    let p = write_phase_slew_fixture("stream_ps_absent_enforced", "");
    let (code, stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_PHASE_SLEW_ENFORCE", "1"),
        ],
    );
    assert_eq!(
        code, 11,
        "enforce: an unreadable phase_slew must be INCOMPLETE (11), never a silent pass. stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("PHASE-SLEW UNKNOWN"), "stdout={stdout}");
    assert!(stderr.contains("INCOMPLETE"), "stderr={stderr}");
}

#[test]
fn gate_rejects_a_mis_set_phase_slew_enforce_flag_1130() {
    // Mirrors the #834 GM guard: a typo'd enforce value must fail loud, not silently be treated as OFF.
    let p = write_phase_slew_fixture("stream_ps_badflag", ",\"phase_slew_enabled\":true");
    let (code, _stdout, stderr) = run_gate_env(
        &phase_slew_stream_args(),
        &[
            ("DANTESYNC_GATE_WIN_HTTP_STREAM", &p.display().to_string()),
            ("DANTESYNC_GATE_PHASE_SLEW_ENFORCE", "true"),
        ],
    );
    assert_eq!(
        code, 1,
        "a mis-set enforce flag must fail loud (1). stderr={stderr}"
    );
    assert!(
        stderr.contains("DANTESYNC_GATE_PHASE_SLEW_ENFORCE must be 0 or 1"),
        "stderr={stderr}"
    );
}

#[test]
fn help_documents_the_phase_slew_enforce_flag_1130() {
    let (code, stdout, _e) = run_gate(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("DANTESYNC_GATE_PHASE_SLEW_ENFORCE"),
        "help must document the #1130 report-first phase_slew enforce flag: {stdout}"
    );
}

#[test]
fn node_verdict_folds_the_optional_phase_slew_rc_1130() {
    // node_verdict gained an OPTIONAL 4th [PS_RC] arg (default 0): ps=2 => BAD, ps=3 => UNKNOWN,
    // ps=0/omitted => unchanged. Locks backward-compat (2-arg + 3-arg callers) AND the new fold.
    for (args, want) in [
        ("0 0 0 2", "BAD"),
        ("0 0 0 3", "UNKNOWN"),
        ("0 0 0 0", "OK"),
        ("0 0", "OK"),   // pre-#834 2-arg caller unchanged
        ("0 0 0", "OK"), // #834 3-arg caller unchanged
        ("2 0 0 0", "BAD"),
    ] {
        let out = run_sourced(&format!("node_verdict {args}"), &[]);
        assert_eq!(
            out.trim(),
            want,
            "node_verdict {args} must be {want}: {out:?}"
        );
    }
}
