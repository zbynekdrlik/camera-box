//! #895 -- the mid-recording self-heal-RESET scan (`scripts/lib/self-heal-attribution.sh`) and its
//! wiring into `recording-e2e.sh` + `recording-verdict.rs`, so a `capture_rate_selfheal` (#663)
//! USB reset firing during an E2E measurement is never again misreported as `frozen_leg` (a camera
//! fault). See `src/self_heal_attribution.rs` for the pure Rust correlation logic this feeds.
//!
//! These tests (a) source the REAL `scripts/lib/self-heal-attribution.sh` for its pure parser/
//! builder functions (the `harness_udev_camera_box_894.rs` "fake the remote, not the ssh"
//! pattern), and (b) assert `recording-e2e.sh` actually wires the scan for every active camera and
//! threads its output into the merge/verdict call.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/self-heal-attribution.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the real lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// -------------------------------------------------------------------------------------------
// grep pattern + journalctl command builder
// -------------------------------------------------------------------------------------------

#[test]
fn grep_pattern_matches_the_real_663_reset_success_line() {
    let pattern = run_sourced("self_heal_reset_grep_pattern");
    let pattern = pattern.trim();
    // The exact text src/main.rs logs on capture_rate_selfheal's SelfHealDecision::Heal success
    // arm (both jitter-band #656 AND sustained-band #717 triggers share this ONE line).
    let sample =
        "#663 self-heal: USB reset attempt #3 succeeded -- will exit (code 77) after graceful shutdown";
    let matched = Command::new("bash")
        .arg("-c")
        .arg(format!("echo {sample:?} | grep -E {pattern:?}"))
        .output()
        .expect("grep");
    assert!(
        matched.status.success(),
        "grep_pattern must match the real #663 reset-success line: {pattern}"
    );
}

#[test]
fn grep_pattern_does_not_match_either_upstream_warn_band() {
    let pattern = run_sourced("self_heal_reset_grep_pattern");
    let pattern = pattern.trim();
    for sample in [
        "#656 capture-delivery-rate DEFECTIVE: 64.02 fps captured vs 60.00 fps configured",
        "#717 capture-delivery-rate SUSTAINED defect: 61.5 fps captured vs 60.00 fps configured",
        "#663 self-heal rate-limited: the last USB reset attempt was too recent",
    ] {
        let matched = Command::new("bash")
            .arg("-c")
            .arg(format!("echo {sample:?} | grep -E {pattern:?}"))
            .output()
            .expect("grep");
        assert!(
            !matched.status.success(),
            "grep_pattern must NOT match an upstream WARN band or a rate-limited (non-reset) line: {sample}"
        );
    }
}

#[test]
fn journalctl_cmd_scopes_by_invocation_id_and_window_with_short_unix_output() {
    let cmd = run_sourced("self_heal_reset_window_journalctl_cmd abc-123 1785439000 1785439999");
    assert!(cmd.contains("_SYSTEMD_INVOCATION_ID=abc-123"), "{cmd}");
    assert!(cmd.contains("--since=@1785439000"), "{cmd}");
    assert!(cmd.contains("--until=@1785439999"), "{cmd}");
    assert!(cmd.contains("-o short-unix"), "{cmd}");
}

#[test]
fn journalctl_cmd_falls_back_to_unscoped_unit_query_when_invocation_id_empty() {
    let cmd = run_sourced("self_heal_reset_window_journalctl_cmd '' 1 2");
    assert!(cmd.contains("-u camera-box"), "{cmd}");
    assert!(!cmd.contains("_SYSTEMD_INVOCATION_ID"), "{cmd}");
}

// -------------------------------------------------------------------------------------------
// events_from_output -- the -o short-unix timestamp parser
// -------------------------------------------------------------------------------------------

#[test]
fn events_from_output_extracts_epoch_ns_from_short_unix_lines() {
    let harness = "SCRIPT_OUT=$(cat <<'JOURNAL'\n\
1785439475.449374 cam1 camera-box[1234]: WARN #663 self-heal: USB reset attempt #3 succeeded -- will exit (code 77)\n\
JOURNAL\n\
)\nself_heal_reset_events_from_output \"$SCRIPT_OUT\"";
    let out = run_sourced(harness);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["1785439475449374000"], "raw output: {out:?}");
}

#[test]
fn events_from_output_ignores_non_matching_lines() {
    let harness = "SCRIPT_OUT=$(cat <<'JOURNAL'\n\
1785439470.000000 cam1 camera-box[1234]: INFO capture loop started\n\
1785439471.111111 cam1 camera-box[1234]: WARN #656 capture-delivery-rate DEFECTIVE: 64.0 fps\n\
1785439475.449374 cam1 camera-box[1234]: WARN #663 self-heal: USB reset attempt #1 succeeded -- ok\n\
1785439480.222222 cam1 camera-box[1234]: INFO capture loop resumed\n\
JOURNAL\n\
)\nself_heal_reset_events_from_output \"$SCRIPT_OUT\"";
    let out = run_sourced(harness);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["1785439475449374000"], "raw output: {out:?}");
}

#[test]
fn events_from_output_extracts_multiple_resets_in_order() {
    let harness = "SCRIPT_OUT=$(cat <<'JOURNAL'\n\
1785439475.449374 cam1 camera-box[1234]: WARN #663 self-heal: USB reset attempt #1 succeeded -- ok\n\
1785439600.100000 cam1 camera-box[1235]: WARN #663 self-heal: USB reset attempt #2 succeeded -- ok\n\
JOURNAL\n\
)\nself_heal_reset_events_from_output \"$SCRIPT_OUT\"";
    let out = run_sourced(harness);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec!["1785439475449374000", "1785439600100000000"],
        "raw output: {out:?}"
    );
}

#[test]
fn events_from_output_empty_input_yields_nothing() {
    let out = run_sourced("self_heal_reset_events_from_output ''");
    assert!(out.trim().is_empty(), "raw output: {out:?}");
}

// -------------------------------------------------------------------------------------------
// scan message -- distinctly labeled, never reads as a camera fault
// -------------------------------------------------------------------------------------------

#[test]
fn scan_message_is_loud_and_distinctly_labeled() {
    let out = run_sourced("self_heal_reset_scan_message CAM1 1785439475449374588");
    assert!(out.contains("CAM1"), "{out}");
    assert!(out.contains("1785439475449374588"), "{out}");
    assert!(out.contains("self-heal RESET detected"), "{out}");
    assert!(out.contains("self_heal_reset"), "{out}");
    assert!(
        out.contains("NOT frozen_leg"),
        "must explicitly disclaim a camera-fault accusation, mirroring the #894 burn-unit \
         integrity message's own 'NOT a frozen camera' reassurance: {out}"
    );
}

// -------------------------------------------------------------------------------------------
// Wiring -- recording-e2e.sh actually sources the lib, scans every active camera, and threads
// the result into recording-verdict.rs's --self-heal-reset flag.
// -------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_lib_and_wires_the_scan_into_merge_args() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("lib/self-heal-attribution.sh"),
        "recording-e2e.sh must source the new lib"
    );
    assert!(
        s.contains("self_heal_reset_window_journalctl_cmd"),
        "recording-e2e.sh must call the window-scoped journalctl builder"
    );
    assert!(
        s.contains("self_heal_reset_events_from_output"),
        "recording-e2e.sh must parse the scan output for epoch-ns events"
    );
    assert!(
        s.contains("--self-heal-reset"),
        "recording-e2e.sh must thread detected events into recording-verdict.rs via --self-heal-reset"
    );
}

#[test]
fn recording_e2e_scans_all_active_cameras_not_just_cam1() {
    let s = read("scripts/recording-e2e.sh");
    // Mirrors the #894 burn-unit-integrity-check's OWN ALL_CAMBOX loop shape (CAMBOX_SECONDARY_DEPLOY) --
    // the self-heal scan must sweep the same set, not just CAM1_IP.
    let scan_start = s
        .find("self_heal_reset_window_journalctl_cmd")
        .expect("scan call site must exist");
    let region = &s[scan_start.saturating_sub(2000)..(scan_start + 2000).min(s.len())];
    assert!(
        region.contains("CAMBOX_SECONDARY_DEPLOY") || region.contains("ALL_CAMBOX"),
        "the self-heal-reset scan must sweep every active camera (ALL_CAMBOX), not just CAM1: {region}"
    );
}
