//! #732 — pure-function guard for `scripts/lib/bundle-state-health.sh`, the SHARED decision core
//! for the dev1-side `:8899` BundleStateServer active health-check watchdog.
//!
//! Root cause (issue 732, four live recurrences through 2026-08-13): the strih/stream
//! `BundleStateServer` Scheduled Task dies with `SCHED_S_TASK_TERMINATED` (`0x40010004`) — an
//! informational/SUCCESS class — on session/parent teardown (dominant post-reboot), so Windows
//! Task Scheduler's restart-on-failure (`RestartCount`) never engages; it also failed to restart a
//! real `0xC000013A` crash (silent 3 days) and cannot cover a cold-start that never fired at all.
//! Nothing off the box probes `:8899`, so the version-integrity E2E gate reads the box UNKNOWN and
//! blames itself. A passive Task-Scheduler policy can never cover a non-failure termination.
//!
//! The dev1-side network-reachability watchdog (issue 1001) already probes `:8899` — but ONLY as
//! one of three OR-signals for "is the box on the network at all", so a `:8899`-only death while
//! the box is otherwise fully up (ping + OBS-WS `:4455` answering) classifies the box REACHABLE and
//! never pages. THIS watchdog closes that specific gap: box up but `:8899` down → restart the task
//! (`schtasks /run`, session-agnostic) + a throttled alert.
//!
//! This file pins the PURE core the dev1-side `bundle-state-alert-watchdog.sh` consumes — the
//! box-reachability signal (deliberately excluding `:8899`), the HEALTHY/DOWN/BOX_UNREACHABLE
//! classification, the alert-detail string, and the exact session-agnostic restart command — so
//! they are correct regardless of any live rig.
//!
//! Same convention as `tests/harness_network_reach_health_1001.rs`: source the REAL lib
//! (source-only, no side effects) and exercise the pure functions directly. RED before the lib
//! exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bundle-state-health.sh");
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
        "bundle_state_box_reachable",
        "bundle_state_classify",
        "bundle_state_alert_detail",
        "bundle_state_restart_remote_cmd",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// bundle_state_box_reachable <ping_ok> <ws_ok> -> 1 | 0
//   Is the BOX itself up, INDEPENDENT of :8899? 1 iff ping OR OBS-WS :4455. Deliberately EXCLUDES
//   :8899 (the service under test) so a dead :8899 can never be mistaken for a dead box.
// ---------------------------------------------------------------------------------------------
fn box_reachable(ping: &str, ws: &str) -> String {
    stdout_of(&format!("bundle_state_box_reachable {ping} {ws}"))
}

#[test]
fn box_reachable_true_if_ping_or_ws_up() {
    assert_eq!(box_reachable("1", "0"), "1"); // ping answers
    assert_eq!(box_reachable("0", "1"), "1"); // OBS-WS answers (ICMP firewalled but box up)
    assert_eq!(box_reachable("1", "1"), "1");
}

#[test]
fn box_reachable_false_if_both_down() {
    assert_eq!(box_reachable("0", "0"), "0");
}

#[test]
fn box_reachable_excludes_8899_by_construction() {
    // The signature takes only ping + ws — there is no :8899 argument to accidentally pass. A box
    // whose ONLY live signal is :8899 is not what this function measures; it measures "is the box
    // reachable so a `:8899`-only failure is a genuine service death (not a dead box)".
    assert_eq!(box_reachable("0", "0"), "0"); // box dark → NOT reachable, regardless of :8899
}

#[test]
fn box_reachable_non_one_values_treated_as_down_defensively() {
    assert_eq!(box_reachable("\"\"", "\"\""), "0");
    assert_eq!(box_reachable("2", "x"), "0");
    assert_eq!(box_reachable("0", "\"\""), "0");
    assert_eq!(box_reachable("\"\"", "1"), "1"); // the one real "1" still wins
}

// ---------------------------------------------------------------------------------------------
// bundle_state_classify <box_reachable> <bundle_healthy> -> HEALTHY | DOWN | BOX_UNREACHABLE
//   HEALTHY        : box up AND :8899 answered 200/JSON.
//   DOWN           : box up but :8899 did NOT answer — the target failure; this watchdog acts.
//   BOX_UNREACHABLE: box itself down — defer to the network-reachability watchdog; nothing here.
// ---------------------------------------------------------------------------------------------
fn classify(box_reachable: &str, bundle_healthy: &str) -> String {
    stdout_of(&format!(
        "bundle_state_classify {box_reachable} {bundle_healthy}"
    ))
}

#[test]
fn classify_box_up_and_bundle_up_is_healthy() {
    assert_eq!(classify("1", "1"), "HEALTHY");
}

#[test]
fn classify_box_up_but_bundle_down_is_down() {
    // The exact target failure: SCHED_S_TASK_TERMINATED / cold-start-never-happened /
    // wedged-but-listening — the box answers ping+:4455 but :8899 does not serve.
    assert_eq!(classify("1", "0"), "DOWN");
}

#[test]
fn classify_box_down_is_box_unreachable_regardless_of_bundle() {
    // A dark box defers to the network-reachability watchdog — never a pointless restart / double
    // page. Even a stale "bundle_healthy=1" reading (impossible when the box is dark, but defended)
    // must not override the box-unreachable verdict.
    assert_eq!(classify("0", "0"), "BOX_UNREACHABLE");
    assert_eq!(classify("0", "1"), "BOX_UNREACHABLE");
}

#[test]
fn classify_non_one_values_treated_as_down_defensively() {
    assert_eq!(classify("\"\"", "\"\""), "BOX_UNREACHABLE"); // box not provably up → defer
    assert_eq!(classify("1", "\"\""), "DOWN"); // box up, bundle not provably up → act
    assert_eq!(classify("1", "x"), "DOWN");
}

// ---------------------------------------------------------------------------------------------
// bundle_state_alert_detail <box> <ip> <ping_ok> <ws_ok> <bundle_ok> -> human signal breakdown
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_names_box_ip_and_marks_the_bundle_port_down() {
    let d = stdout_of("bundle_state_alert_detail strih 10.77.9.202 1 1 0");
    assert!(d.contains("strih"), "detail must name the box: {d}");
    assert!(d.contains("10.77.9.202"), "detail must name the ip: {d}");
    assert!(
        d.contains("8899 DOWN"),
        "detail must mark the bundle port down: {d}"
    );
    assert!(d.contains("ping up"), "ping should read up: {d}");
    assert!(
        d.contains("4455 up") || d.contains("OBS-WS:4455 up"),
        "ws should read up: {d}"
    );
}

#[test]
fn alert_detail_renders_all_down_branch() {
    let d = stdout_of("bundle_state_alert_detail stream 10.77.9.204 0 0 0");
    assert!(d.contains("stream"), "detail must name the box: {d}");
    assert!(d.contains("ping DOWN"), "ping should read DOWN: {d}");
    assert!(d.contains("8899 DOWN"), "bundle should read DOWN: {d}");
}

// ---------------------------------------------------------------------------------------------
// bundle_state_restart_remote_cmd -> the exact session-agnostic recovery command
//   Pins that the remedy is `schtasks /run` of the hidden headless task — NEVER the `/it`
//   interactive form (a documented DEAD END on these boxes, and a desktop-session op the headless
//   watchdog must never attempt — win-ssh-vs-mcp.md).
// ---------------------------------------------------------------------------------------------
#[test]
fn restart_cmd_uses_schtasks_run_of_the_named_task() {
    let c = stdout_of("bundle_state_restart_remote_cmd");
    assert!(c.contains("schtasks /run"), "must use schtasks /run: {c}");
    assert!(c.contains("BundleStateServer"), "must name the task: {c}");
}

#[test]
fn restart_cmd_never_uses_the_interactive_it_form() {
    let c = stdout_of("bundle_state_restart_remote_cmd");
    // `/it` (interactive) is a desktop-session op the headless dev1 watchdog must never issue, and
    // is a documented DEAD END on strih/stream. Guard against it explicitly.
    assert!(
        !c.to_lowercase().contains("/it"),
        "restart command must NOT use the interactive /it form: {c}"
    );
}
