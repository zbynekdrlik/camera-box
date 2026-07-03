//! #109 — zero-loss restart-survival gate: cross-boundary + wiring guards.
//!
//! Locks (a) that the zero-loss-restart-gate binary decides PASS/FAIL/UNKNOWN via
//! `zero_loss_restart_survival::classify` from two real `recording-verdict --json` files
//! exactly as the rig E2E needs, and (b) that `scripts/recording-e2e.sh` wires the optional
//! restart-survival step OFF by default, documents the OBS restart AND the PC restart as
//! operator/supervisor actions it never executes itself, and invokes the gate binary (mirrors
//! `harness_av_restart_sync_gate.rs`'s guard against the #137 gate being silently dropped or
//! never wired).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Write `json` to a uniquely-named temp file for this test process; returns its path.
fn write_json(name: &str, json: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zero-loss-restart-gate-test-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(json.as_bytes()).unwrap();
    path
}

fn run(bin: &str, args: &[&Path]) -> (i32, String) {
    let out = Command::new(bin)
        .args(args)
        .output()
        .expect("spawn zero-loss-restart-gate");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The Rust gate binary source must exist.
#[test]
fn zero_loss_restart_gate_bin_src_exists() {
    let path = format!(
        "{}/src/bin/zero-loss-restart-gate.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/zero-loss-restart-gate.rs not found (#109)."
    );
}

/// A clean before/after pair (both a genuine zero-loss PASS) MUST exit 0 PASS.
#[test]
fn gate_binary_passes_a_healthy_restart() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let before = write_json(
        "before-healthy",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let after = write_json(
        "after-healthy",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 0,
        "a healthy before/after pair MUST exit 0, got {code} ({out})"
    );
    assert!(
        out.starts_with("PASS"),
        "stdout should start with PASS, got {out:?}"
    );
}

/// The exact #109 failure mode — a restart that broke zero-loss delivery — MUST exit 1 FAIL
/// (never silently pass).
#[test]
fn gate_binary_fails_when_restart_breaks_zero_loss() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let before = write_json(
        "before-drift",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let after = write_json(
        "after-drift",
        r#"{"overall_pass": false, "full_chain": {"zero_loss": false, "real_drops": 3, "burn_unreadable": 0}}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 1,
        "a restart that broke zero-loss delivery MUST exit 1 (FAIL), got {code} ({out})"
    );
    assert!(
        out.starts_with("FAIL"),
        "stdout should start with FAIL, got {out:?}"
    );
}

/// A baseline ('before') that was never zero-loss MUST exit 1 FAIL — restart survival can
/// never be claimed from a dirty baseline.
#[test]
fn gate_binary_fails_a_dirty_baseline() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let before = write_json(
        "before-dirty",
        r#"{"overall_pass": false, "full_chain": {"zero_loss": false, "real_drops": 0, "burn_unreadable": 2}}"#,
    );
    let after = write_json(
        "after-clean",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 1,
        "a dirty baseline MUST exit 1 (FAIL), got {code} ({out})"
    );
    assert!(
        out.starts_with("FAIL"),
        "stdout should start with FAIL, got {out:?}"
    );
}

/// An internally-inconsistent measurement (a combination `recording-verdict` could never
/// actually produce) MUST exit 1 (never 0 PASS), and is reported as UNKNOWN, not a silent
/// pass and not a confirmed FAIL either.
#[test]
fn gate_binary_never_passes_an_inconsistent_measurement() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let before = write_json(
        "before-inconsistent",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": false, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let after = write_json(
        "after-clean-for-inconsistent",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 1,
        "an inconsistent measurement must never exit 0, got {code} ({out})"
    );
    assert!(
        out.starts_with("UNKNOWN"),
        "stdout should start with UNKNOWN, got {out:?}"
    );
}

/// Missing file / malformed JSON fails closed (exit 2), never silently passes.
#[test]
fn gate_binary_missing_file_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let missing = PathBuf::from("/tmp/does-not-exist-zero-loss-restart-gate.json");
    let after = write_json(
        "after-for-missing",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let (code, _) = run(bin, &[&missing, &after]);
    assert_eq!(
        code, 2,
        "a missing input file must exit 2 (fail closed), got {code}"
    );
}

/// A JSON with no `full_chain` object at all (e.g. a run with no full-chain burns configured)
/// fails closed (exit 2), never silently treated as zero-loss.
#[test]
fn gate_binary_missing_full_chain_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let before = write_json("before-no-full-chain", r#"{"overall_pass": true}"#);
    let after = write_json(
        "after-for-no-full-chain",
        r#"{"overall_pass": true, "full_chain": {"zero_loss": true, "real_drops": 0, "burn_unreadable": 0}}"#,
    );
    let (code, _) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 2,
        "missing 'full_chain' must exit 2 (fail closed), got {code}"
    );
}

#[test]
fn gate_binary_too_few_args_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_zero-loss-restart-gate");
    let out = Command::new(bin).output().expect("spawn");
    assert_eq!(
        out.status.code().expect("exit code"),
        2,
        "no args must exit 2 (fail closed)"
    );
}

// ---------------------------------------------------------------------------------------
// scripts/recording-e2e.sh wiring guards
// ---------------------------------------------------------------------------------------

/// recording-e2e.sh MUST invoke the zero-loss-restart-gate binary somewhere (never silently
/// dropped after being added).
#[test]
fn recording_e2e_sh_wires_zero_loss_restart_gate() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("zero-loss-restart-gate"),
        "scripts/recording-e2e.sh must invoke zero-loss-restart-gate (#109 restart-survival gate)."
    );
}

/// The restart-survival step MUST be OFF by default — a normal zero-loss run is UNCHANGED
/// unless the operator opts in (mirrors #137's AV_RESTART_GATE default-off shape).
#[test]
fn zero_loss_restart_gate_step_defaults_off() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("ZERO_LOSS_RESTART_GATE:-0"),
        "the #109 restart-survival step must default OFF (ZERO_LOSS_RESTART_GATE:-0) so \
         normal recording-e2e.sh runs are unchanged."
    );
}

/// The restart(s) themselves MUST be documented as operator/supervisor actions this script
/// does NOT execute — locks the #109 scope boundary (this PR ships the gate + wiring; the
/// live restart-survival rig proof, including the approval-gated PC reboot, is
/// supervisor-driven).
#[test]
fn restart_is_documented_as_operator_action_never_executed() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let block = &s[gate_pos..];
    assert!(
        block.contains("OPERATOR/SUPERVISOR ACTION"),
        "the #109 step must clearly mark the restart(s) as an operator/supervisor action."
    );
    assert!(
        block.contains("does NOT execute it")
            || block.contains("never stops/starts OBS itself")
            || block.contains("never executes"),
        "the #109 step must explicitly state the script never executes the restart itself."
    );
}

/// The #109 step must mention BOTH restart kinds the issue requires: an OBS restart AND a PC
/// restart/reboot — never only one of the two.
#[test]
fn restart_step_covers_both_obs_and_pc_restart() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let block = &s[gate_pos..];
    assert!(
        block.contains("OBS restart"),
        "#109 must mention the OBS restart step."
    );
    assert!(
        block.to_lowercase().contains("pc restart") || block.to_lowercase().contains("reboot"),
        "#109 must mention the PC restart/reboot step."
    );
}

/// REACHABILITY: the ZERO_LOSS_RESTART_GATE mode MUST appear BEFORE the main `[5/8]
/// StartRecord` step (and thus before the `VERDICT_ON_STREAM=1` `exit 0` inside `[8/8]`), or
/// it is unreachable on the default path — mirrors the #137 reachability lock.
#[test]
fn zero_loss_restart_gate_mode_is_reachable_before_the_main_record_step() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let start_record_pos = s
        .find("[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    assert!(
        gate_pos < start_record_pos,
        "#109 ZERO_LOSS_RESTART_GATE mode must be positioned BEFORE [5/8] StartRecord (and \
         thus before the VERDICT_ON_STREAM=1 `exit 0`) or it is unreachable on the default path."
    );
    let block = &s[gate_pos..start_record_pos];
    assert!(
        block.contains("exit \"$GATE\""),
        "#109 ZERO_LOSS_RESTART_GATE block must be an early-exit mode (exit \"$GATE\" before [5/8])."
    );
}

/// FAIL-LOUD confirmation: the restart-confirmation gate MUST abort (never silently proceed)
/// when it cannot confirm the restart happened.
#[test]
fn zero_loss_restart_confirmation_fails_loud_when_not_confirmable() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let start_record_pos = s.find("[5/8] StartRecord").expect("[5/8] step must exist");
    let block = &s[gate_pos..start_record_pos];
    assert!(
        block.contains("ZERO_LOSS_RESTART_CONFIRM"),
        "#109 must honour ZERO_LOSS_RESTART_CONFIRM for non-interactive confirmation."
    );
    assert!(
        block.contains("not a TTY") && block.contains("exit 1"),
        "#109 confirmation must ABORT (exit 1) when stdin is not a TTY and the restart is \
         unconfirmed — never silently take a spurious 'after' measurement."
    );
}

/// HONEST messaging (no-overstatement): the wrapper MUST distinguish the gate binary's exit
/// codes and NOT report a bad-JSON error (exit 2) or an UNKNOWN verdict as a confirmed
/// zero-loss regression.
#[test]
fn zero_loss_restart_gate_wrapper_reports_verdict_honestly_per_exit_code() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let start_record_pos = s.find("[5/8] StartRecord").expect("[5/8] step must exist");
    let block = &s[gate_pos..start_record_pos];
    assert!(
        block.contains("could NOT evaluate"),
        "#109 wrapper must surface a bad/missing-JSON gate error (exit 2) as 'could NOT \
         evaluate', never as a confirmed zero-loss regression (no-overstatement)."
    );
    assert!(
        block.contains("UNKNOWN"),
        "#109 wrapper must acknowledge an UNKNOWN verdict (an inconsistent measurement) is \
         never a confirmed pass."
    );
}

/// A previously-produced 'before' measurement can be reused via `ZERO_LOSS_RESTART_BEFORE_JSON`
/// (so the supervisor can chain baseline -> post-OBS-restart -> post-PC-restart as two
/// invocations of this SAME step without re-recording an already-clean baseline).
#[test]
fn zero_loss_restart_gate_supports_reusing_a_prior_before_json() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("ZERO_LOSS_RESTART_GATE:-0")
        .expect("#109 ZERO_LOSS_RESTART_GATE block must exist");
    let start_record_pos = s.find("[5/8] StartRecord").expect("[5/8] step must exist");
    let block = &s[gate_pos..start_record_pos];
    assert!(
        block.contains("ZERO_LOSS_RESTART_BEFORE_JSON"),
        "#109 must support ZERO_LOSS_RESTART_BEFORE_JSON to chain multiple restart passes \
         without re-recording an already-measured baseline."
    );
}
