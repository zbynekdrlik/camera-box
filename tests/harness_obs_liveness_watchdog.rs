//! #391 — behavioral + content tests for the broadcast-OBS liveness/wedge watchdog.
//!
//! Background: stream OBS (10.77.9.204) was hung "(Not Responding)" for ~25 HOURS (obs64 pegged
//! ~168% CPU, Responding=False, 16.0% render-lag) and NOTHING detected it. This watchdog is the
//! Windows-OBS sibling of the #281/#350 rig-restore-watchdog: a dev1 systemd timer polls OBS
//! WebSocket `GetStats` on strih+stream (no ssh/MCP needed for DETECTION), runs the strict
//! `obs_watchdog::classify` verdict, and — once a wedge is CONFIRMED over 2 consecutive passes —
//! fires a Discord alert carrying the exact agent-driven recovery command (a dev1 timer cannot
//! itself force-kill/relaunch obs64 on Windows — ssh is denied, the win-* MCP is agent-only).
//!
//! These are pure-shell / content tests — NO rig, NO OBS, NO MCP. We test the pure decision
//! functions (obs_watchdog_confirm / obs_watchdog_alert_throttle) and the wrapper script's wiring +
//! syntax, mirroring tests/harness_rig_restore_watchdog.rs's shape.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn decision_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/obs-watchdog-decision.sh")
}

fn watchdog() -> PathBuf {
    manifest_dir().join("scripts/obs-liveness-watchdog.sh")
}

fn probe_py() -> PathBuf {
    manifest_dir().join("scripts/obs-liveness-probe.py")
}

fn gate_bin_src() -> PathBuf {
    manifest_dir().join("src/bin/obs-watchdog-gate.rs")
}

fn readme() -> PathBuf {
    manifest_dir().join("systemd/obs-liveness-watchdog.README.md")
}

fn service_unit() -> PathBuf {
    manifest_dir().join("systemd/obs-liveness-watchdog.service")
}

fn timer_unit() -> PathBuf {
    manifest_dir().join("systemd/obs-liveness-watchdog.timer")
}

fn run_bash(script: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("DECISION_LIB", decision_lib())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn parse_kv(stdout: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn confirm(prev: &str, wedged: &str, threshold: &str) -> HashMap<String, String> {
    let script = format!(
        r#"set -u
. "$DECISION_LIB"
obs_watchdog_confirm {prev} {wedged} {threshold}
"#
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "obs_watchdog_confirm must exit 0\nstdout:{stdout}\nstderr:{stderr}"
    );
    parse_kv(&stdout)
}

fn throttle(
    current_sig: &str,
    prior_sig: &str,
    prior_passes: &str,
    throttle_n: &str,
) -> HashMap<String, String> {
    let script = format!(
        r#"set -u
. "$DECISION_LIB"
obs_watchdog_alert_throttle '{current_sig}' '{prior_sig}' {prior_passes} {throttle_n}
"#
    );
    let (code, stdout, stderr) = run_bash(&script);
    assert_eq!(
        code, 0,
        "obs_watchdog_alert_throttle must exit 0\nstdout:{stdout}\nstderr:{stderr}"
    );
    parse_kv(&stdout)
}

// ── obs_watchdog_confirm ─────────────────────────────────────────────────────

#[test]
fn healthy_pass_resets_confirm_and_never_acts() {
    let d = confirm("1", "0", "2");
    assert_eq!(
        d["confirm"], "0",
        "a healthy (not-wedged) pass must reset the counter"
    );
    assert_eq!(d["act"], "0");
}

#[test]
fn single_wedged_pass_is_observe_only_below_threshold() {
    let d = confirm("0", "1", "2");
    assert_eq!(d["confirm"], "1");
    assert_eq!(
        d["act"], "0",
        "one transient wedge reading must NOT alert (threshold=2)"
    );
}

#[test]
fn second_consecutive_wedged_pass_confirms_and_acts() {
    let d = confirm("1", "1", "2");
    assert_eq!(d["confirm"], "2");
    assert_eq!(
        d["act"], "1",
        "2 consecutive wedge readings must confirm + act"
    );
}

#[test]
fn threshold_of_one_acts_on_first_sighting() {
    let d = confirm("0", "1", "1");
    assert_eq!(d["act"], "1");
}

#[test]
fn garbage_prev_confirm_is_treated_as_zero() {
    let d = confirm("bogus", "1", "2");
    assert_eq!(
        d["confirm"], "1",
        "a garbage/non-numeric prior counter must reset to 0, not crash"
    );
}

#[test]
fn already_confirmed_wedge_keeps_acting_every_pass() {
    // Once past threshold, EVERY subsequent wedged pass keeps act=1 (never drops back to
    // observe-only while the condition persists) — the throttle layer governs the ALERT
    // repetition rate, not this confirm layer.
    let d = confirm("5", "1", "2");
    assert_eq!(d["confirm"], "6");
    assert_eq!(d["act"], "1");
}

// ── obs_watchdog_alert_throttle ──────────────────────────────────────────────

#[test]
fn first_occurrence_always_alerts() {
    let t = throttle("stream:WEDGED-RENDER-LAG", "", "0", "10");
    assert_eq!(t["alert_now"], "1");
    assert_eq!(t["new_passes"], "1");
}

#[test]
fn same_signature_within_throttle_window_suppresses() {
    let t = throttle(
        "stream:WEDGED-RENDER-LAG",
        "stream:WEDGED-RENDER-LAG",
        "3",
        "10",
    );
    assert_eq!(
        t["alert_now"], "0",
        "same condition, only 3/10 passes elapsed -> suppress"
    );
    assert_eq!(t["new_passes"], "4");
}

#[test]
fn same_signature_past_throttle_window_re_alerts() {
    let t = throttle(
        "stream:WEDGED-RENDER-LAG",
        "stream:WEDGED-RENDER-LAG",
        "10",
        "10",
    );
    assert_eq!(
        t["alert_now"], "1",
        "10/10 passes elapsed -> re-alert (a reminder ping)"
    );
    assert_eq!(t["new_passes"], "1");
}

#[test]
fn changed_signature_always_alerts_even_mid_throttle_window() {
    // Escalation (e.g. WEDGED-RENDER-LAG -> OBS-COUNT-WRONG) must never be suppressed by the
    // throttle meant for the SAME persisting condition.
    let t = throttle(
        "stream:OBS-COUNT-WRONG",
        "stream:WEDGED-RENDER-LAG",
        "2",
        "10",
    );
    assert_eq!(t["alert_now"], "1");
}

// ── wrapper script wiring + content ───────────────────────────────────────────

#[test]
fn watchdog_script_has_valid_bash_syntax() {
    let out = Command::new("bash")
        .arg("-n")
        .arg(watchdog())
        .output()
        .expect("bash -n");
    assert!(
        out.status.success(),
        "syntax error:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn decision_lib_has_valid_bash_syntax() {
    let out = Command::new("bash")
        .arg("-n")
        .arg(decision_lib())
        .output()
        .expect("bash -n");
    assert!(
        out.status.success(),
        "syntax error:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn watchdog_sources_the_decision_lib() {
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(
        src.contains("obs-watchdog-decision.sh"),
        "#391: the watchdog must source the pure decision lib"
    );
    assert!(src.contains("obs_watchdog_confirm"));
    assert!(src.contains("obs_watchdog_alert_throttle"));
}

#[test]
fn watchdog_polls_both_broadcast_boxes_via_the_python_probe() {
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(src.contains("obs-liveness-probe.py"));
    assert!(
        src.contains("10.77.9.202"),
        "#391: strih must be a polled box"
    );
    assert!(
        src.contains("10.77.9.204"),
        "#391: stream must be a polled box"
    );
}

#[test]
fn watchdog_embeds_the_agent_driven_recovery_command() {
    // The auto-recover design decision (#391): a dev1 timer cannot itself force-kill/relaunch
    // obs64 (ssh denied, win-* MCP is agent-only). The alert MUST embed the exact recovery
    // command so recovery is a single paste-and-go for whichever agent sees the alert.
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(
        src.contains("launch-obs-genlock.sh"),
        "#391: the alert must carry the documented obs-ops recovery command"
    );
    assert!(
        src.contains("--force"),
        "#391: the recovery command must force-kill the wedged obs64"
    );
}

#[test]
fn watchdog_never_alerts_in_dry_run_mode() {
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(src.contains("--dry-run"));
    assert!(
        src.contains("DRY_RUN"),
        "#391: the watchdog must gate the actual notify call on a dry-run flag"
    );
}

#[test]
fn watchdog_dry_run_pass_produces_no_actual_alert_call_reachable() {
    // Structural check: within the loop body, the DRY_RUN branch must `continue` BEFORE the
    // real `python3 "$NOTIFY" notify` call is reachable — i.e. the notify invocation appears
    // strictly after a `continue` guarded by `[ "$DRY_RUN" -eq 1 ]` in the source text.
    let src = fs::read_to_string(watchdog()).unwrap();
    let dry_run_guard_pos = src
        .find("if [ \"$DRY_RUN\" -eq 1 ]")
        .expect("#391: must have an explicit DRY_RUN guard");
    let notify_call_pos = src
        .find("python3 \"$NOTIFY\" notify")
        .expect("#391: must call airuleset.py notify to alert");
    assert!(
        dry_run_guard_pos < notify_call_pos,
        "#391: the DRY_RUN guard (with its continue) must appear BEFORE the real notify call"
    );
}

#[test]
fn watchdog_calls_airuleset_notify_for_the_alert() {
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(
        src.contains("airuleset.py") || src.contains("$NOTIFY"),
        "#391: alerts must go through airuleset.py notify (milestone-notifications.md)"
    );
}

#[test]
fn watchdog_runs_main_only_when_executed_not_sourced() {
    let src = fs::read_to_string(watchdog()).unwrap();
    assert!(
        src.contains(r#"if [[ "${BASH_SOURCE[0]}" == "$0" ]]"#),
        "#391: sourcing the script for tests must not run a live pass (mirrors rig-restore-watchdog.sh #394)"
    );
}

// ── obs-watchdog-gate binary + obs_watchdog module wiring ───────────────────

#[test]
fn gate_binary_source_uses_the_pure_classify_function() {
    let src = fs::read_to_string(gate_bin_src()).unwrap();
    assert!(
        src.contains("camera_box::obs_watchdog::classify")
            || src.contains("obs_watchdog::{classify"),
        "#391: obs-watchdog-gate must call the single source-of-truth classify()"
    );
}

#[test]
fn gate_binary_is_registered_in_cargo_toml_without_probe_feature() {
    let cargo_toml = fs::read_to_string(manifest_dir().join("Cargo.toml")).unwrap();
    let idx = cargo_toml
        .find("name = \"obs-watchdog-gate\"")
        .expect("#391: obs-watchdog-gate must be a [[bin]] target");
    // The bin block for obs-watchdog-gate must NOT require the probe feature (default
    // features only — Tier-0 checkable, no probe deps, like frozen-camera-gate/render-budget-gate).
    // Bound the slice to THIS block only (up to the next `[[bin]]`), so a `required-features`
    // line belonging to the NEXT bin target is never mistaken for this one's.
    let rest = &cargo_toml[idx..];
    let block_end = rest.find("[[bin]]").unwrap_or(rest.len());
    let block = &rest[..block_end];
    assert!(
        !block.contains("required-features"),
        "#391: obs-watchdog-gate must build on default features (no probe deps)"
    );
}

#[test]
fn probe_script_lazily_imports_websocket() {
    // #399 lesson: a top-level `from websocket import ...` breaks any Rust-harness-style
    // exec_module of the pure helpers on a runner with no websocket-client installed.
    let src = fs::read_to_string(probe_py()).unwrap();
    assert!(
        !src.contains("\nfrom websocket import") && !src.contains("\ntry:\n    from websocket"),
        "#391/#399: obs-liveness-probe.py must NOT import websocket at module top level"
    );
    assert!(
        src.contains("def _ws("),
        "#391/#399: websocket-client must be imported lazily inside a _ws() helper"
    );
}

// ── ships-disabled systemd install (mirrors rig-restore-watchdog) ───────────

#[test]
fn systemd_units_exist_and_document_disabled_by_default() {
    let service = fs::read_to_string(service_unit()).unwrap();
    let timer = fs::read_to_string(timer_unit()).unwrap();
    assert!(
        service.contains("SHIPS DISABLED") || service.contains("ships disabled"),
        "#391: the service unit must document that it ships disabled"
    );
    assert!(
        timer.contains("SHIPS DISABLED") || timer.contains("ships disabled"),
        "#391: the timer unit must document that it ships disabled"
    );
    assert!(
        service.contains("ExecStart="),
        "#391: the service unit must define ExecStart"
    );
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "#391: the timer unit must be installable (WantedBy=timers.target)"
    );
}

#[test]
fn readme_documents_supervisor_install_and_live_verify_procedure() {
    let readme_text = fs::read_to_string(readme()).unwrap();
    assert!(
        readme_text.to_lowercase().contains("supervisor"),
        "#391: the README must state the SUPERVISOR installs + live-verifies before enabling"
    );
    assert!(
        readme_text.contains("--dry-run"),
        "#391: the README must document the --dry-run verification step"
    );
    assert!(
        readme_text.contains("systemctl --user enable"),
        "#391: the README must document how to enable the timer once verified"
    );
}

#[test]
fn readme_documents_the_auto_recover_design_decision() {
    let readme_text = fs::read_to_string(readme()).unwrap();
    assert!(
        readme_text.contains("launch-obs-genlock.sh"),
        "#391: the README must document the embedded agent-driven recovery command"
    );
}
