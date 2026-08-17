//! #1023 — pure-function guard for `scripts/lib/asio-starve-health.sh`, the SHARED decision core
//! for the dev1-side STREAM ASIO-STARVED alert watchdog.
//!
//! Root cause (issue 1023): when stream OBS starts BEFORE its ASIO device/matrix is ready, an ASIO
//! source connects but its audio callback perpetually STARVES (no samples) → the source is silent
//! and only an OBS reset fixes it. The vendored genlock build prints, once per ASRC_LOG_INTERVAL_S
//! (=60 s), `asrc: source '<name>' … starved_blocks=N (#803/#806/#960)` to the stream OBS log;
//! `starved_blocks=N` is PER-INTERVAL (reset-on-read, asrc-compensator.c), so the newest line's value
//! is a self-contained 60 s measurement. A healthy source reads 0; a starved one reads ~2946 (≈100 %
//! of ~2900 callbacks/60 s) SUSTAINED while a sibling source stays 0. Reproduced LIVE 2026-08-17
//! ('ASIO Input Capture' ≈2946 for 11.5 h; 'mbc' = 0). This file pins the PURE parse + classification
//! the `asio-starve-alert-watchdog.sh` consumes, so it is correct regardless of any live rig.
//!
//! Same convention as `tests/harness_frozen_input_health_1052.rs` /
//! `tests/harness_cadence_health_794.rs`: source the REAL lib (source-only, no side effects) and
//! exercise the pure functions directly. RED before the lib exists (sourcing fails, every test
//! fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/asio-starve-health.sh");
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

// A realistic multi-source stream-OBS asrc fixture (the exact live format, two report intervals). It
// deliberately also carries a `source 'mbc2'` line so the parser's exact-name anchoring is tested.
const FIXTURE: &str = r#"LOG=$(cat <<'FIX'
08:01:07.536: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2948 (#803/#806/#960)
08:01:08.657: asrc: source 'mbc' estimated=-8.28ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=0 (#803/#806/#960)
08:01:08.680: asrc: source 'mbc2' estimated=0.00ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=1234 (#803/#806/#960)
08:02:07.551: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2949 (#803/#806/#960)
08:02:08.660: asrc: source 'mbc' estimated=-10.32ppm applied=-10.32ppm outer_bias=0.00ppm cumulative_correction=0.805ms/60s starved_blocks=0 (#803/#806/#960)
FIX
)
"#;

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "asio_starve_parse_blocks",
        "asio_starve_is_healthy",
        "asio_starve_classify",
        "asio_starve_recovery_decision",
        "asio_starve_alert_detail",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// asio_starve_parse_blocks <source> (stdin log) -> newest starved_blocks | empty
// ---------------------------------------------------------------------------------------------
#[test]
fn parse_returns_newest_starved_blocks_for_the_named_source() {
    // 'ASIO Input Capture' appears twice (2948 then 2949) -> the NEWEST (last) value.
    assert_eq!(
        stdout_of(&format!(
            "{FIXTURE}printf '%s\\n' \"$LOG\" | asio_starve_parse_blocks 'ASIO Input Capture'"
        )),
        "2949"
    );
}

#[test]
fn parse_returns_zero_for_a_healthy_source() {
    assert_eq!(
        stdout_of(&format!(
            "{FIXTURE}printf '%s\\n' \"$LOG\" | asio_starve_parse_blocks 'mbc'"
        )),
        "0"
    );
}

#[test]
fn parse_anchors_the_exact_name_never_a_prefix_sibling() {
    // Parsing 'mbc' must NOT latch onto the `source 'mbc2'` line (starved_blocks=1234): the trailing
    // quote in the fixed-string match anchors the exact name. So 'mbc' -> 0, 'mbc2' -> 1234.
    assert_eq!(
        stdout_of(&format!(
            "{FIXTURE}printf '%s\\n' \"$LOG\" | asio_starve_parse_blocks 'mbc'"
        )),
        "0"
    );
    assert_eq!(
        stdout_of(&format!(
            "{FIXTURE}printf '%s\\n' \"$LOG\" | asio_starve_parse_blocks 'mbc2'"
        )),
        "1234"
    );
}

#[test]
fn parse_absent_source_is_empty() {
    // A source with no asrc line -> empty output (the caller treats it as a blind tap -> UNKNOWN).
    assert_eq!(
        stdout_of(&format!(
            "{FIXTURE}printf '%s\\n' \"$LOG\" | asio_starve_parse_blocks 'nope'"
        )),
        ""
    );
}

// ---------------------------------------------------------------------------------------------
// asio_starve_is_healthy <blocks> <threshold> -> 1 | 0
// ---------------------------------------------------------------------------------------------
fn is_healthy(blocks: &str, thr: &str) -> String {
    stdout_of(&format!("asio_starve_is_healthy {blocks} {thr}"))
}

#[test]
fn is_healthy_below_threshold_is_one() {
    assert_eq!(is_healthy("0", "1000"), "1"); // the healthy sibling in the live incident
    assert_eq!(is_healthy("999", "1000"), "1");
}

#[test]
fn is_healthy_at_or_above_threshold_is_zero() {
    assert_eq!(is_healthy("1000", "1000"), "0");
    assert_eq!(is_healthy("2948", "1000"), "0"); // the starved source
}

#[test]
fn is_healthy_missing_or_non_numeric_is_zero() {
    // A missing / unreadable value is NOT proof of health.
    assert_eq!(is_healthy("\"\"", "1000"), "0");
    assert_eq!(is_healthy("x", "1000"), "0");
}

// ---------------------------------------------------------------------------------------------
// asio_starve_classify <blocks> <threshold> <healthy_sibling> <expected_live>
//   -> STARVED | OK | UNKNOWN | SKIP
// ---------------------------------------------------------------------------------------------
fn classify(blocks: &str, thr: &str, sib: &str, live: &str) -> String {
    stdout_of(&format!("asio_starve_classify {blocks} {thr} {sib} {live}"))
}

#[test]
fn classify_starved_with_healthy_sibling_is_starved() {
    // The core incident: this source starving hard while a sibling is healthy -> the per-source
    // startup-order defect (OBS reset cures it).
    assert_eq!(classify("2948", "1000", "1", "1"), "STARVED");
    assert_eq!(classify("1000", "1000", "1", "1"), "STARVED"); // exactly at threshold counts
}

#[test]
fn classify_below_threshold_is_ok() {
    assert_eq!(classify("0", "1000", "1", "1"), "OK");
    assert_eq!(classify("999", "1000", "1", "1"), "OK"); // just under the threshold
}

#[test]
fn classify_starved_without_healthy_sibling_is_unknown_never_double_page() {
    // Every watched source starving (no healthy sibling) is a box-wide audio outage owned by
    // obs-liveness #391 / audio-presence -> UNKNOWN here, never a false / double page, and never
    // turning the precise per-source discriminator into a box-wide alarm.
    assert_eq!(classify("2948", "1000", "0", "1"), "UNKNOWN");
}

#[test]
fn classify_no_sample_is_unknown() {
    // The source's asrc line was absent / the read failed -> nothing to page on.
    assert_eq!(classify("\"\"", "1000", "1", "1"), "UNKNOWN");
    assert_eq!(classify("x", "1000", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_not_expected_live_is_skip() {
    // Box down (issue-1001 owns the page) / source out of scope -> SKIP, regardless of the value.
    assert_eq!(classify("2948", "1000", "1", "0"), "SKIP");
    assert_eq!(classify("0", "1000", "1", "0"), "SKIP");
}

#[test]
fn classify_defensive_non_one_expected_live_is_skip() {
    assert_eq!(classify("2948", "1000", "1", "yes"), "SKIP");
}

// ---------------------------------------------------------------------------------------------
// asio_starve_recovery_decision <was_alerted> <now_ok> -> recover=0|1
// ---------------------------------------------------------------------------------------------
fn recover(was_alerted: &str, now_ok: &str) -> String {
    stdout_of(&format!(
        "asio_starve_recovery_decision {was_alerted} {now_ok}"
    ))
}

#[test]
fn recovery_only_when_paged_source_reads_ok_again() {
    assert_eq!(recover("1", "1"), "recover=1"); // we paged, it recovered -> one recovery ping
    assert_eq!(recover("0", "1"), "recover=0"); // never paged -> no recovery ping
    assert_eq!(recover("1", "0"), "recover=0"); // still starved -> no recovery yet
    assert_eq!(recover("0", "0"), "recover=0");
}

// ---------------------------------------------------------------------------------------------
// asio_starve_alert_detail <source> <blocks> <threshold> -> one human line
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_names_source_and_value() {
    let out = stdout_of("asio_starve_alert_detail 'ASIO Input Capture' 2948 1000");
    assert!(
        out.contains("ASIO Input Capture"),
        "detail missing source: {out}"
    );
    assert!(out.contains("2948"), "detail missing blocks value: {out}");
    assert!(
        out.contains("starving"),
        "detail missing the human signal: {out}"
    );
}
