//! #1001 — pure-function guard for `scripts/lib/network-reach-health.sh`, the SHARED decision core
//! for the dev1-side strih/stream network-reachability alert watchdog.
//!
//! Root cause (issue 1001, live 2026-08-06 50-min outage + 2026-08-13 recurrence): strih's optical
//! NIC died and the box fell fully off the network — no DHCP, ARP INCOMPLETE, OBS-WS + ssh + MCP all
//! dead — while stream's `NDI 2ME PGM` silently held the last frozen frame. NO Discord alert fired,
//! because every existing watchdog probes a box it assumes is UP (OBS-WS `GetStats`, ssh into the
//! box) and treats a total network outage as `no probe output → nothing to decide`. The reachability
//! question can only be answered by a prober that is UP while the target is DOWN — dev1 — probing the
//! box from OUTSIDE with a multi-signal check.
//!
//! This file pins the PURE core the dev1-side `network-reach-alert-watchdog.sh` consumes — the
//! multi-signal REACHABLE/UNREACHABLE classification, the reference-anchor aggregate (the dev1-side
//! outage guard), the recovery decision, and the alert detail string — so they are correct
//! regardless of any live rig.
//!
//! Same convention as `tests/harness_optical_chain_health_860.rs`: source the REAL lib (source-only,
//! no side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/network-reach-health.sh");
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
        "net_reach_classify_box",
        "net_reach_any_reachable",
        "net_reach_recovery_decision",
        "net_reach_alert_detail",
        "net_reach_box_is_report_only",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// net_reach_classify_box <ping_ok> <ws_ok> <bundle_ok> -> REACHABLE | UNREACHABLE
//   REACHABLE iff ANY signal succeeded (a box that firewalls ICMP but answers a TCP port is UP);
//   UNREACHABLE only iff ALL three failed — the real 50-min outage ("No route to host" everywhere).
// ---------------------------------------------------------------------------------------------
fn classify(ping: &str, ws: &str, bundle: &str) -> String {
    stdout_of(&format!("net_reach_classify_box {ping} {ws} {bundle}"))
}

#[test]
fn classify_all_signals_down_is_unreachable() {
    // the exact incident: ping 100% loss + :4455 "No route to host" + :8899 dead
    assert_eq!(classify("0", "0", "0"), "UNREACHABLE");
}

#[test]
fn classify_any_single_signal_up_is_reachable() {
    assert_eq!(classify("1", "0", "0"), "REACHABLE"); // ping answers
    assert_eq!(classify("0", "1", "0"), "REACHABLE"); // OBS-WS answers (ICMP firewalled but box up)
    assert_eq!(classify("0", "0", "1"), "REACHABLE"); // bundle-state answers
}

#[test]
fn classify_multiple_signals_up_is_reachable() {
    assert_eq!(classify("1", "1", "1"), "REACHABLE");
    assert_eq!(classify("1", "0", "1"), "REACHABLE");
    assert_eq!(classify("0", "1", "1"), "REACHABLE");
}

#[test]
fn classify_non_one_values_treated_as_down_defensively() {
    // Any value other than "1" (empty, garbage) counts as a failed signal — never a false REACHABLE.
    assert_eq!(classify("\"\"", "\"\"", "\"\""), "UNREACHABLE");
    assert_eq!(classify("2", "x", "-"), "UNREACHABLE");
    assert_eq!(classify("0", "\"\"", "1"), "REACHABLE"); // the one real "1" still wins
}

// ---------------------------------------------------------------------------------------------
// net_reach_any_reachable <flags...> -> 1 | 0  (the reference-anchor aggregate: dev1-side outage
//   guard — if NO reference rig node is reachable, dev1's own path to the rig subnet is down and
//   the pass is "nothing to decide", never a false "both OBS boxes down").
// ---------------------------------------------------------------------------------------------
#[test]
fn any_reachable_true_when_at_least_one_flag_is_one() {
    assert_eq!(stdout_of("net_reach_any_reachable 0 0 1"), "1");
    assert_eq!(stdout_of("net_reach_any_reachable 1 0 0"), "1");
    assert_eq!(stdout_of("net_reach_any_reachable 1 1 1"), "1");
}

#[test]
fn any_reachable_false_when_all_flags_down_or_empty() {
    assert_eq!(stdout_of("net_reach_any_reachable 0 0 0"), "0");
    // no reference host answered at all -> dev1-side path suspect
    assert_eq!(stdout_of("net_reach_any_reachable"), "0");
    assert_eq!(stdout_of("net_reach_any_reachable 0 x \"\""), "0");
}

// ---------------------------------------------------------------------------------------------
// net_reach_recovery_decision <was_alerted> <now_reachable> -> recover=0|1
//   Fire ONE recovery ping only when a box we PAGED for comes back reachable.
// ---------------------------------------------------------------------------------------------
fn recover(was_alerted: &str, now_reachable: &str) -> String {
    stdout_of(&format!(
        "net_reach_recovery_decision {was_alerted} {now_reachable}"
    ))
}

#[test]
fn recovery_only_when_previously_alerted_and_now_back() {
    assert_eq!(recover("1", "1"), "recover=1"); // paged, now back -> recover ping
    assert_eq!(recover("1", "0"), "recover=0"); // still down -> no recovery yet
    assert_eq!(recover("0", "1"), "recover=0"); // never paged (healthy all along) -> no ping
    assert_eq!(recover("0", "0"), "recover=0"); // down but never paged (not yet confirmed) -> no ping
}

// ---------------------------------------------------------------------------------------------
// net_reach_alert_detail <box> <ping_ok> <ws_ok> <bundle_ok> -> human signal breakdown
// ---------------------------------------------------------------------------------------------
#[test]
fn alert_detail_names_box_and_marks_each_failed_signal() {
    let d = stdout_of("net_reach_alert_detail strih 0 0 0");
    assert!(d.contains("strih"), "detail must name the box: {d}");
    assert!(d.contains("ping DOWN"), "detail must mark ping down: {d}");
    assert!(
        d.contains("4455 DOWN") || d.contains("OBS-WS:4455 DOWN"),
        "detail must mark the OBS-WS port down: {d}"
    );
    assert!(
        d.contains("8899 DOWN"),
        "detail must mark the bundle-state port down: {d}"
    );
}

#[test]
fn alert_detail_renders_ping_up_branch() {
    // Directly pin the `ping up` branch (the other detail tests all use ping=0).
    let d = stdout_of("net_reach_alert_detail strih 1 0 0");
    assert!(d.contains("ping up"), "ping should read up: {d}");
    assert!(d.contains("4455 DOWN"), "ws should read DOWN: {d}");
}

#[test]
fn alert_detail_distinguishes_up_from_down_signals() {
    // A partial-signal outage (ICMP firewalled but a port up) is REACHABLE and never alerts, but the
    // detail builder must still render mixed state honestly when called.
    let d = stdout_of("net_reach_alert_detail stream 0 1 0");
    assert!(d.contains("stream"), "detail must name the box: {d}");
    assert!(d.contains("ping DOWN"), "ping should read DOWN: {d}");
    assert!(
        d.contains("4455 up") || d.contains("OBS-WS:4455 up"),
        "ws should read up: {d}"
    );
    assert!(d.contains("8899 DOWN"), "bundle should read DOWN: {d}");
}

// ---------------------------------------------------------------------------------------------
// net_reach_box_is_report_only <box> <report_only_boxes> -> report_only=1 | report_only=0  (#811)
//   A box NAMED in the space-separated list is REPORT-ONLY: probed + logged + state-tracked like any
//   other, but it NEVER pages (nor recovery-pings) — for a TRAVELING box (resolume) whose absence is
//   the NORMAL state. Whole-word match on the box name; empty list / non-member -> report_only=0.
// ---------------------------------------------------------------------------------------------
fn report_only(boxname: &str, list: &str) -> String {
    stdout_of(&format!("net_reach_box_is_report_only {boxname} '{list}'"))
}

#[test]
fn report_only_true_for_a_listed_box() {
    assert_eq!(report_only("resolume", "resolume"), "report_only=1");
    assert_eq!(
        report_only("resolume", "strih stream resolume"),
        "report_only=1"
    );
    assert_eq!(
        report_only("stream", "strih stream resolume"),
        "report_only=1"
    );
}

#[test]
fn report_only_false_for_an_unlisted_box() {
    // The strih/stream default (empty report-only list means they page normally).
    assert_eq!(report_only("strih", "resolume"), "report_only=0");
    assert_eq!(report_only("stream", "resolume"), "report_only=0");
}

#[test]
fn report_only_false_for_empty_list() {
    assert_eq!(report_only("resolume", ""), "report_only=0");
    assert_eq!(report_only("strih", ""), "report_only=0");
}

#[test]
fn report_only_is_whole_word_not_substring() {
    // "resolume" must not match the different NIC row name "resolume-alt", and a name prefix must
    // not match either — otherwise a paging box could be silently muted by a look-alike list entry.
    assert_eq!(report_only("resolume-alt", "resolume"), "report_only=0");
    assert_eq!(report_only("resolume", "resolume-alt"), "report_only=0");
    assert_eq!(report_only("resol", "resolume"), "report_only=0");
    assert_eq!(
        report_only("resolume", "resolume-alt resolume"),
        "report_only=1"
    );
}
