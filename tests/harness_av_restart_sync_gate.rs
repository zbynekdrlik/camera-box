//! #137 — OBS-restart A/V-sync-survival gate: cross-boundary + wiring guards.
//!
//! Locks (a) that the av-restart-sync-gate binary decides PASS/FAIL/UNKNOWN via
//! `av_restart_sync::classify` from two real `recording-verdict --av-sync` JSON
//! files exactly as the rig E2E needs, and (b) that `scripts/recording-e2e.sh` wires
//! the optional restart-survival step OFF by default, documents the OBS restart as an
//! operator/supervisor action it never executes itself, and invokes the gate binary
//! (mirrors `harness_render_budget_gate.rs`'s guard against the gate being silently
//! dropped or never wired).

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
    let dir = std::env::temp_dir().join(format!("av-restart-gate-test-{}", std::process::id()));
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
        .expect("spawn av-restart-sync-gate");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The Rust gate binary source must exist.
#[test]
fn av_restart_sync_gate_bin_src_exists() {
    let path = format!(
        "{}/src/bin/av-restart-sync-gate.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/av-restart-sync-gate.rs not found (#137)."
    );
}

/// A trusted before/after pair within tolerance MUST exit 0 PASS.
#[test]
fn gate_binary_passes_a_healthy_restart() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let before = write_json(
        "before-healthy",
        r#"{"av_offset_ms": -70.2, "matched": 32, "mad_ms": 8.0}"#,
    );
    let after = write_json(
        "after-healthy",
        r#"{"av_offset_ms": -64.5, "matched": 30, "mad_ms": 9.5}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 0,
        "healthy in-tolerance restart MUST exit 0, got {code} ({out})"
    );
    assert!(
        out.starts_with("PASS"),
        "stdout should start with PASS, got {out:?}"
    );
}

/// The exact #137 user-reported failure — a 200-300ms drift across the restart — MUST
/// exit 1 FAIL (never silently pass).
#[test]
fn gate_binary_fails_the_reported_200ms_drift() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let before = write_json(
        "before-drift",
        r#"{"av_offset_ms": -70.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    let after = write_json(
        "after-drift",
        r#"{"av_offset_ms": -270.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 1,
        "200ms restart drift MUST exit 1 (FAIL), got {code} ({out})"
    );
    assert!(
        out.starts_with("FAIL"),
        "stdout should start with FAIL, got {out:?}"
    );
}

/// An untrustworthy measurement (too few clustered markers) MUST exit 1 (never 0 PASS),
/// and is reported as UNKNOWN, not a silent pass.
#[test]
fn gate_binary_never_passes_an_untrustworthy_measurement() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let before = write_json(
        "before-untrusted",
        r#"{"av_offset_ms": -70.0, "matched": 1, "mad_ms": 8.0}"#,
    );
    let after = write_json(
        "after-untrusted",
        r#"{"av_offset_ms": -70.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 1,
        "untrustworthy measurement must never exit 0, got {code} ({out})"
    );
    assert!(
        out.starts_with("UNKNOWN"),
        "stdout should start with UNKNOWN, got {out:?}"
    );
}

/// An explicit tolerance override (3rd positional arg) is honoured.
#[test]
fn gate_binary_honours_explicit_tolerance_override() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let before = write_json(
        "before-tol",
        r#"{"av_offset_ms": 0.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    let after = write_json(
        "after-tol",
        r#"{"av_offset_ms": 40.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    // Default tolerance (50ms) would PASS a 40ms delta; an explicit 10ms override must FAIL it.
    let out = Command::new(bin)
        .args([
            before.as_os_str(),
            after.as_os_str(),
            std::ffi::OsStr::new("10"),
        ])
        .output()
        .expect("spawn av-restart-sync-gate");
    let code = out.status.code().expect("exit code");
    assert_eq!(
        code, 1,
        "40ms delta with a 10ms override MUST FAIL, got {code}"
    );
}

/// Missing file / malformed JSON fails closed (exit 2), never silently passes.
#[test]
fn gate_binary_missing_file_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let missing = PathBuf::from("/tmp/does-not-exist-av-restart-sync-gate.json");
    let after = write_json(
        "after-for-missing",
        r#"{"av_offset_ms": 0.0, "matched": 32, "mad_ms": 8.0}"#,
    );
    let (code, _) = run(bin, &[&missing, &after]);
    assert_eq!(
        code, 2,
        "a missing input file must exit 2 (fail closed), got {code}"
    );
}

#[test]
fn gate_binary_too_few_args_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let out = Command::new(bin).output().expect("spawn");
    assert_eq!(
        out.status.code().expect("exit code"),
        2,
        "no args must exit 2 (fail closed)"
    );
}

/// A float-encoded non-negative integer `matched` (e.g. `32.0`) is accepted, not rejected
/// as a "missing field" — a hand-edited / alternately-serialized partial JSON with an
/// unambiguous value must still gate (robustness, not a spurious exit-2).
#[test]
fn gate_binary_accepts_float_encoded_matched() {
    let bin = env!("CARGO_BIN_EXE_av-restart-sync-gate");
    let before = write_json(
        "before-floatmatched",
        r#"{"av_offset_ms": -70.0, "matched": 32.0, "mad_ms": 8.0}"#,
    );
    let after = write_json(
        "after-floatmatched",
        r#"{"av_offset_ms": -66.0, "matched": 30.0, "mad_ms": 9.0}"#,
    );
    let (code, out) = run(bin, &[&before, &after]);
    assert_eq!(
        code, 0,
        "float-encoded matched (32.0/30.0) must be accepted and PASS, got {code} ({out})"
    );
    assert!(
        out.starts_with("PASS"),
        "stdout should start with PASS, got {out:?}"
    );
}

// ---------------------------------------------------------------------------------------
// scripts/recording-e2e.sh wiring guards
// ---------------------------------------------------------------------------------------

/// recording-e2e.sh MUST invoke the av-restart-sync-gate binary somewhere (never silently
/// dropped after being added).
#[test]
fn recording_e2e_sh_wires_av_restart_sync_gate() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("av-restart-sync-gate"),
        "scripts/recording-e2e.sh must invoke av-restart-sync-gate (#137 restart-survival gate)."
    );
}

/// The restart-survival step MUST be OFF by default — a normal zero-loss run is
/// UNCHANGED unless the operator opts in (mirrors --colour-gate/COLOUR_GATE's
/// default-on-but-overridable shape, but this gate defaults OFF since it needs a real
/// OBS restart, which a plain zero-loss run never performs).
#[test]
fn av_restart_gate_step_defaults_off() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("AV_RESTART_GATE:-0"),
        "the #137 restart-survival step must default OFF (AV_RESTART_GATE:-0) so normal \
         recording-e2e.sh runs are unchanged."
    );
}

/// The OBS restart itself MUST be documented as an operator/supervisor action that this
/// script does NOT execute — locks the #137 scope boundary (this PR ships the gate +
/// wiring; the live two-recording rig proof with a REAL OBS stop->start is
/// supervisor-driven, never automated inside recording-e2e.sh).
#[test]
fn obs_restart_is_documented_as_operator_action_never_executed() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("OPERATOR/SUPERVISOR ACTION"),
        "the #137 step must clearly mark the OBS restart as an operator/supervisor action."
    );
    assert!(
        s.contains("does NOT execute it") || s.contains("never stops/starts OBS itself"),
        "the #137 step must explicitly state the script never executes the OBS restart itself."
    );
}

/// REACHABILITY: the AV_RESTART_GATE mode MUST appear BEFORE the main `[5/8] StartRecord`
/// step (and thus before the `VERDICT_ON_STREAM=1` `exit 0` inside [8/8], which fires on
/// the DEFAULT path). Placing it after that early exit — as the first cut of this PR did —
/// makes the whole gate silently unreachable when a user runs `AV_RESTART_GATE=1` without
/// also flipping the legacy `VERDICT_ON_STREAM=0`. Lock the ordering so it can't regress.
#[test]
fn av_restart_gate_mode_is_reachable_before_the_main_record_step() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let start_record_pos = s
        .find("[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    assert!(
        gate_pos < start_record_pos,
        "#137 AV_RESTART_GATE mode must be positioned BEFORE [5/8] StartRecord (and thus \
         before the VERDICT_ON_STREAM=1 `exit 0`) or it is unreachable on the default path."
    );
    // It is an early-exit MODE: the block must exit on its own so it never falls through
    // into the normal single-recording zero-loss verdict.
    let block = &s[gate_pos..start_record_pos];
    assert!(
        block.contains("exit \"$GATE\""),
        "#137 AV_RESTART_GATE block must be an early-exit mode (exit \"$GATE\" before [5/8])."
    );
}

/// FAIL-LOUD confirmation: the restart-confirmation gate MUST abort (never silently
/// proceed) when it cannot confirm the OBS restart happened. A non-interactive run with
/// no AV_RESTART_CONFIRM=1 must hard-fail — otherwise the 'after' measurement is taken
/// with no real restart and the gate reports a SPURIOUS PASS, masking the #137 regression.
#[test]
fn av_restart_confirmation_fails_loud_when_not_confirmable() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let start_record_pos = s.find("[5/8] StartRecord").expect("[5/8] step must exist");
    let block = &s[gate_pos..start_record_pos];
    // Honours an explicit out-of-band confirmation for non-interactive supervisor runs...
    assert!(
        block.contains("AV_RESTART_CONFIRM"),
        "#137 must honour AV_RESTART_CONFIRM for non-interactive confirmation."
    );
    // ...but a non-TTY run WITHOUT that confirmation must ABORT, not silently continue.
    assert!(
        block.contains("not a TTY") && block.contains("exit 1"),
        "#137 confirmation must ABORT (exit 1) when stdin is not a TTY and the restart is \
         unconfirmed — never silently take a spurious 'after' measurement."
    );
}

/// HONEST messaging (no-overstatement): the wrapper MUST distinguish the gate binary's
/// exit codes and NOT report a bad-JSON error (exit 2) or an UNKNOWN verdict as a
/// confirmed A/V drift. Locks the exit-code-distinguished branch so a future edit can't
/// regress to the single "drifted ... lipsync broken" claim for every failure.
#[test]
fn av_restart_gate_wrapper_reports_verdict_honestly_per_exit_code() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let start_record_pos = s.find("[5/8] StartRecord").expect("[5/8] step must exist");
    let block = &s[gate_pos..start_record_pos];
    // Captures the gate's own exit code (not a single unconditional if!-branch)...
    assert!(
        block.contains("av_rc"),
        "#137 wrapper must capture the gate binary's exit code to report per-code."
    );
    // ...distinguishes the bad/missing-JSON error (exit 2) from a real drift...
    assert!(
        block.contains("could NOT evaluate"),
        "#137 wrapper must surface a bad/missing-JSON gate error (exit 2) as 'could NOT \
         evaluate', never as a confirmed A/V drift (no-overstatement)."
    );
    // ...and acknowledges UNKNOWN is not a confirmed pass (never a silent PASS).
    assert!(
        block.contains("UNKNOWN"),
        "#137 wrapper must acknowledge an UNKNOWN verdict (untrustworthy measurement) is \
         never a confirmed pass."
    );
}
