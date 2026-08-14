//! #1052 — pure-function guard for `scripts/lib/frozen-input-health.sh`, the SHARED decision core
//! for the dev1-side STREAM FROZEN-INPUT alert watchdog (network-reach watchdog phase 2).
//!
//! Root cause (issue 1052, phase-2 of issue 1001): reachability alone cannot see a box that is
//! fully REACHABLE while its NDI input is silently frozen on the last frame (the issue-767
//! receiver-rebind class: the cambox is fine but stream's DistroAV receiver froze). The stream OBS
//! log prints `genlock-fifo audit '<src>': received=N …` ~every 5 s; a frozen input stops advancing
//! `received=`. The dev1-side watchdog samples the newest `received=` per watched source each pass,
//! persists it, and compares to the prior pass. This file pins the PURE classification the
//! `frozen-input-alert-watchdog.sh` consumes, so it is correct regardless of any live rig.
//!
//! Same convention as `tests/harness_network_reach_health_1001.rs` /
//! `tests/harness_optical_chain_health_860.rs`: source the REAL lib (source-only, no side effects)
//! and exercise the pure functions directly. RED before the lib exists (sourcing fails, every test
//! fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/frozen-input-health.sh");
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
// lib shape — the pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "frozen_input_classify",
        "frozen_input_recovery_decision",
        "frozen_input_alert_detail",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// frozen_input_classify <prev_received> <curr_received> <expected_live> <sender_reachable>
//   -> FROZEN | ADVANCING | UNKNOWN | SKIP
// ---------------------------------------------------------------------------------------------
fn classify(prev: &str, curr: &str, live: &str, reach: &str) -> String {
    stdout_of(&format!("frozen_input_classify {prev} {curr} {live} {reach}"))
}

#[test]
fn classify_counter_unchanged_is_frozen() {
    // The core incident: received= held identical across a pass while the box is reachable and the
    // program input is expected live -> input frozen on last frame.
    assert_eq!(classify("123456", "123456", "1", "1"), "FROZEN");
    assert_eq!(classify("0", "0", "1", "1"), "FROZEN"); // never received a single frame this session
}

#[test]
fn classify_counter_advancing_is_advancing() {
    assert_eq!(classify("123456", "123999", "1", "1"), "ADVANCING");
    assert_eq!(classify("0", "1", "1", "1"), "ADVANCING");
}

#[test]
fn classify_no_prior_sample_is_unknown() {
    // First pass: no previous reading to compare -> seed state, never page.
    assert_eq!(classify("\"\"", "123456", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_unreadable_current_is_unknown() {
    // Log read failed / the source's audit line was absent this pass -> nothing to compare, never
    // page on a missing reading (a false page on a transient log-tail miss is banned).
    assert_eq!(classify("123456", "\"\"", "1", "1"), "UNKNOWN");
    assert_eq!(classify("123456", "x", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_counter_reset_is_unknown_not_frozen() {
    // curr < prev means the cumulative counter reset -> OBS restarted. Reseed; never page.
    assert_eq!(classify("500000", "12", "1", "1"), "UNKNOWN");
}

#[test]
fn classify_not_expected_live_is_skip() {
    // An idle input (not in program / sender paused) may legitimately stop -> out of scope, SKIP,
    // regardless of counter values.
    assert_eq!(classify("123456", "123456", "0", "1"), "SKIP");
    assert_eq!(classify("100", "200", "0", "1"), "SKIP");
}

#[test]
fn classify_sender_unreachable_is_skip_never_double_page() {
    // When the sender box is CONFIRMED unreachable, issue-1001 already owns the page. A frozen input
    // there is a downstream symptom -> SKIP, never a second alert for the same root cause.
    assert_eq!(classify("123456", "123456", "1", "0"), "SKIP");
    assert_eq!(classify("123456", "123456", "1", "\"\""), "SKIP");
}

#[test]
fn classify_defensive_non_one_flags_are_skip() {
    // Any expected_live / sender_reachable value other than "1" is treated conservatively as
    // out-of-scope (never a false FROZEN page).
    assert_eq!(classify("1", "1", "yes", "1"), "SKIP");
    assert_eq!(classify("1", "1", "1", "up"), "SKIP");
}

// ---------------------------------------------------------------------------------------------
// frozen_input_recovery_decision <was_alerted> <now_advancing> -> recover=0|1
//   Fire ONE recovery ping only when a source we PAGED for starts advancing again.
// ---------------------------------------------------------------------------------------------
fn recover(was_alerted: &str, now_advancing: &str) -> String {
    stdout_of(&format!(
        "frozen_input_recovery_decision {was_alerted} {now_advancing}"
    ))
}

#[test]
fn recovery_only_when_previously_alerted_and_now_advancing() {
    assert_eq!(recover("1", "1"), "recover=1"); // paged, now advancing -> recovery ping
    assert_eq!(recover("1", "0"), "recover=0"); // still frozen -> no recovery yet
    assert_eq!(recover("0", "1"), "recover=0"); // never paged (healthy all along) -> no ping
    assert_eq!(recover("0", "0"), "recover=0"); // frozen but never paged -> no ping
    assert_eq!(recover("1", "x"), "recover=0"); // defensive: non-"1" advancing -> no ping
}

// ---------------------------------------------------------------------------------------------
// frozen_input_alert_detail <source> <received> -> human line for the Discord body
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_names_source_and_stuck_counter() {
    let d = stdout_of("frozen_input_alert_detail \"NDI 2ME PGM\" 123456");
    assert!(d.contains("NDI 2ME PGM"), "detail must name the source: {d}");
    assert!(d.contains("123456"), "detail must show the stuck received counter: {d}");
    assert!(
        d.to_lowercase().contains("frozen") || d.to_lowercase().contains("not advancing"),
        "detail must state the input is frozen: {d}"
    );
}
