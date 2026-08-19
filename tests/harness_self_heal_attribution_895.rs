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
// Wiring -- recording-e2e.sh actually sources the lib, scans every active camera (journald AND
// each camera's burn-instance log), and threads the result into recording-verdict.rs's
// --restart-event flag.
// -------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_lib_and_wires_the_scan_into_merge_args() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("lib/self-heal-attribution.sh"),
        "recording-e2e.sh must source the lib"
    );
    assert!(
        s.contains("self_heal_reset_window_journalctl_cmd"),
        "recording-e2e.sh must call the window-scoped journalctl builder"
    );
    assert!(
        s.contains("restart_events_from_journal_output"),
        "recording-e2e.sh must parse the journald scan output for KIND:EPOCH_NS events"
    );
    assert!(
        s.contains("restart_events_from_burn_log_output")
            && s.contains("restart_event_burn_log_grep_cmd"),
        "recording-e2e.sh must ALSO read + parse each camera's burn-instance log (issue 910)"
    );
    assert!(
        s.contains("/tmp/cbox-burn.log") && s.contains("/tmp/cbox-burn-${_cn}.log"),
        "the restart-event scan must pass the source AND per-secondary burn-log paths (issue 910)"
    );
    assert!(
        s.contains("--restart-event"),
        "recording-e2e.sh must thread detected events into recording-verdict.rs via --restart-event"
    );
}

#[test]
fn recording_e2e_scans_all_active_cameras_not_just_cam1() {
    let s = read("scripts/recording-e2e.sh");
    // Mirrors the #894 burn-unit-integrity-check's OWN ALL_CAMBOX loop shape (CAMBOX_SECONDARY_DEPLOY) --
    // the self-heal scan must sweep the same set, not just CAM1_IP.
    // Anchor on the CALL-SITE form ("$(fn ...") -- the bare fn name also appears in a header
    // comment (line ~299), and .find() on it latched the comment; the +-2000 window around the
    // comment only coincidentally contained ALL_CAMBOX until the issue-1134 edits shifted text
    // (the #832 anchor-uniqueness gotcha).
    let scan_start = s
        .find("$(self_heal_reset_window_journalctl_cmd ")
        .expect("scan call site must exist");
    let region = &s[scan_start.saturating_sub(2000)..(scan_start + 2000).min(s.len())];
    assert!(
        region.contains("CAMBOX_SECONDARY_DEPLOY") || region.contains("ALL_CAMBOX"),
        "the self-heal-reset scan must sweep every active camera (ALL_CAMBOX), not just CAM1: {region}"
    );
}

// ===========================================================================================
// issue 946 + issue 910 — the unified recognised-event table + burn-log (RFC3339) parser
// ===========================================================================================
// One table maps each run-integrity restart KIND to its grep substring; the wedge (issue 945,
// exit 79) and emit-freeze (issue 944, exit 81) CRITICAL lines join the #663 self-heal reset in
// ONE recognised-event table, read from BOTH journald AND — during an E2E burn — the burn
// instance's own `/tmp/cbox-burn*.log` (whose lines are ANSI-wrapped, microsecond RFC3339-Z).

#[test]
fn restart_event_grep_pattern_alternates_all_three_kinds() {
    let pat = run_sourced("restart_event_grep_pattern");
    let pat = pat.trim();
    assert!(pat.contains("#663 self-heal: USB reset attempt"), "{pat}");
    assert!(
        pat.contains("CRITICAL #945: capture/emit thread WEDGED"),
        "{pat}"
    );
    assert!(pat.contains("CRITICAL #944: NDI output FROZEN"), "{pat}");
    assert!(pat.contains('|'), "must be a grep -E alternation: {pat}");
}

#[test]
fn restart_event_kind_for_line_classifies_each_kind() {
    for (line, want) in [
        (
            "2026-08-14T10:17:56.523683Z  WARN camera_box: #663 self-heal: USB reset attempt #6 succeeded -- ok",
            "self_heal_reset",
        ),
        (
            "2026-08-14T10:17:57.000000Z ERROR camera_box: CRITICAL #945: capture/emit thread WEDGED -- exiting (code 79)",
            "capture_wedge",
        ),
        (
            "2026-08-14T10:17:58.000000Z ERROR camera_box: CRITICAL #944: NDI output FROZEN -- exiting (code 81)",
            "emit_freeze",
        ),
    ] {
        let out = run_sourced(&format!("restart_event_kind_for_line {line:?}"));
        assert_eq!(out.trim(), want, "line: {line}");
    }
}

#[test]
fn restart_event_kind_for_line_ignores_unrelated_lines() {
    let out = run_sourced(
        "restart_event_kind_for_line '2026-08-14T10:17:56Z INFO camera_box: capture loop started'",
    );
    assert!(out.trim().is_empty(), "raw output: {out:?}");
}

#[test]
fn restart_events_from_journal_output_tags_kind_and_epoch_ns() {
    // -o short-unix journald lines: SEC.USEC leading field.
    let harness = "SCRIPT_OUT=$(cat <<'JOURNAL'\n\
1785439475.449374 cam1 camera-box[1234]: WARN #663 self-heal: USB reset attempt #3 succeeded -- ok\n\
1785439480.000000 cam1 camera-box[1234]: INFO capture loop resumed\n\
1785439600.100000 cam1 camera-box[1235]: ERROR CRITICAL #945: capture/emit thread WEDGED -- exiting (code 79)\n\
JOURNAL\n\
)\nrestart_events_from_journal_output \"$SCRIPT_OUT\"";
    let out = run_sourced(harness);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec![
            "self_heal_reset:1785439475449374000",
            "capture_wedge:1785439600100000000"
        ],
        "raw output: {out:?}"
    );
}

#[test]
fn restart_events_from_burn_log_output_strips_ansi_and_parses_rfc3339() {
    // The REAL burn-log line shape: ESC[2m<RFC3339-Z>ESC[0m ESC[33m LEVEL ESC[0m ... message.
    // date -u -d "2026-08-14T10:17:56.523683Z" +%s%N == 1786702676523683000.
    let harness = "SCRIPT_OUT=$(printf '\\033[2m2026-08-14T10:17:56.523683Z\\033[0m \\033[33m WARN\\033[0m \\033[2mcamera_box\\033[0m\\033[2m:\\033[0m #663 self-heal: USB reset attempt #6 succeeded -- ok\\n\\033[2m2026-08-14T10:17:57.000000Z\\033[0m \\033[31mERROR\\033[0m CRITICAL #944: NDI output FROZEN -- exiting (code 81)\\n')\nrestart_events_from_burn_log_output \"$SCRIPT_OUT\"";
    let out = run_sourced(harness);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec![
            "self_heal_reset:1786702676523683000",
            "emit_freeze:1786702677000000000"
        ],
        "raw output: {out:?}"
    );
}

#[test]
fn restart_events_from_burn_log_output_empty_input_yields_nothing() {
    let out = run_sourced("restart_events_from_burn_log_output ''");
    assert!(out.trim().is_empty(), "raw output: {out:?}");
}

#[test]
fn restart_event_burn_log_grep_cmd_greps_the_log_for_all_kinds() {
    let cmd = run_sourced("restart_event_burn_log_grep_cmd /tmp/cbox-burn.log");
    assert!(cmd.contains("/tmp/cbox-burn.log"), "{cmd}");
    assert!(cmd.contains("grep -E"), "{cmd}");
    assert!(
        cmd.contains("CRITICAL #945: capture/emit thread WEDGED"),
        "{cmd}"
    );
    assert!(cmd.contains("CRITICAL #944: NDI output FROZEN"), "{cmd}");
    assert!(cmd.contains("#663 self-heal: USB reset attempt"), "{cmd}");
}

#[test]
fn restart_event_scan_message_is_loud_and_kind_labeled() {
    let out = run_sourced("restart_event_scan_message capture_wedge CAM4 1786702677000000000");
    assert!(out.contains("CAM4"), "{out}");
    assert!(out.contains("1786702677000000000"), "{out}");
    assert!(out.contains("capture_wedge"), "{out}");
    assert!(
        out.contains("NOT frozen_leg"),
        "must disclaim a camera-fault accusation: {out}"
    );
}
