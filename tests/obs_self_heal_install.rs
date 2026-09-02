//! #411 — structural/content tests for the Windows-local unattended self-heal mechanism
//! (`scripts/obs-self-heal-install.sh`).
//!
//! These are pure-shell / content tests — NO rig, NO OBS, NO Windows host, mirroring
//! `tests/launch_obs_genlock.rs`'s shape exactly. They source the script (never execute the real
//! flow), call its pure `build_recovery_script` / `build_task_xml` builders, and assert:
//!
//! - NEITHER decision the recovery script needs is reimplemented in PowerShell: the wedge
//!   VERDICT is reused via `obs-watchdog-gate.exe` (never a re-derived threshold — the emitted
//!   PowerShell must NOT contain the magic numbers `obs_watchdog::classify` uses), and the
//!   RECOVERY decision (confirm/throttle/lock) is reused via `obs-self-heal-gate.exe`, which this
//!   test suite ALSO invokes directly (via `CARGO_BIN_EXE_obs-self-heal-gate`) to prove the real
//!   compiled binary's behavior — not just that the PowerShell text mentions it.
//! - the AHK-race-safe step order is preserved verbatim in the generated PowerShell (StopAhk
//!   before the obs64 kill, RestartAhk after the post-recovery verify),
//! - the kill+relaunch step REUSES `launch-obs-genlock.sh`'s `build_launch_program` byte-for-byte
//!   (never a second hand-rolled launch path),
//! - an OMITTED threshold/interval/stale-lock override becomes a JSON `null` in the generated
//!   script, so `obs-self-heal-gate.exe`'s own `camera_box::obs_self_heal::DEFAULT_*` Rust
//!   constants are the single actual source of default truth — verified both at the
//!   `build_recovery_script` level AND through `main()`'s real `--box strih` CLI invocation (the
//!   path an operator actually runs), closing the "bash default silently drifts from the Rust
//!   kernel" gap a prior review found.
//! - unexpected exit codes from EITHER gate binary fail loud (never silently read as healthy),
//! - the Task Scheduler XML ships `Enabled=false`, uses `InteractiveToken` logon (obs64 is a GUI
//!   app — a SYSTEM/Session-0 task could not launch it into the visible desktop) and
//!   `IgnoreNew` multiple-instances policy (defense in depth alongside the script's own lock).

use camera_box::obs_self_heal::{
    DEFAULT_CONFIRM_THRESHOLD, DEFAULT_MIN_RECOVERY_INTERVAL_S, DEFAULT_STALE_LOCK_S,
};
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/obs-self-heal-install.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn launch_genlock_script() -> PathBuf {
    manifest_dir().join("scripts/launch-obs-genlock.sh")
}

/// Source the script and run `body` (which may call its pure functions). Returns stdout.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
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

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run obs-self-heal-install.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const OBS_DIR: &str = "C:\\Program Files\\obs-studio";

/// `build_recovery_script` with NO threshold/interval/stale-lock override args — the default,
/// mirroring what `main()`'s own (unset-by-default) CLI flags now produce. `30` is strih's real
/// target fps since Topology v2 (#459, was 60 pre-#459 -- the 60fps IMAG role moved to imag-nb).
fn recovery_script_strih() -> String {
    run_sourced(&format!("build_recovery_script strih '{OBS_DIR}' 30"))
}

fn recovery_script_stream() -> String {
    run_sourced(&format!("build_recovery_script stream '{OBS_DIR}' 30"))
}

/// The launch-obs-genlock.sh planner's OWN force=1 program (the ground truth this module must
/// reuse verbatim for the KillAndRelaunchObs step).
fn expected_kill_relaunch_program() -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nbuild_launch_program '{OBS_DIR}' 1");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", launch_genlock_script())
        .output()
        .expect("failed to run launch-obs-genlock.sh harness");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Sourcing must not execute the flow (source-guard).
#[test]
fn script_is_source_safe() {
    let out = run_sourced("echo SOURCED_OK");
    assert!(out.contains("SOURCED_OK"), "sourcing must not run main()");
    assert!(
        !out.contains("obs-self-heal install plan"),
        "sourcing must stop at the source-guard, not print a plan. out=\n{out}"
    );
}

/// #411: the wedge verdict must be REUSED via obs-watchdog-gate.exe, never re-derived. The
/// emitted PowerShell must reference the gate binary and must NOT contain the magic threshold
/// numbers `obs_watchdog::classify` uses internally (120% CPU peg, 5% render-skip tolerance,
/// 2x render-time budget multiplier) — those constants exist in exactly ONE place.
#[test]
fn recovery_script_reuses_classify_via_gate_binary_never_reinvents_thresholds() {
    let p = recovery_script_strih();
    assert!(
        p.contains("obs-watchdog-gate.exe"),
        "#411: the local wedge check MUST pipe through obs-watchdog-gate.exe (the classify \
         verdict), never reimplement it. Program:\n{p}"
    );
    assert!(
        p.contains("& $GateBin") || p.contains("| & $GateBin"),
        "#411: the sample JSON must actually be piped INTO the gate binary. Program:\n{p}"
    );
    for magic in ["120.0", "= 120", ">= 120"] {
        assert!(
            !p.contains(magic),
            "#411: the recovery script must NOT hand-roll obs_watchdog::classify's CPU-pegged \
             threshold ({magic}) — that logic lives ONLY in Rust, reused via the gate binary. \
             Program:\n{p}"
        );
    }
    assert!(
        !p.to_lowercase().contains("getstats"),
        "#411: the LOCAL self-heal sample must come from process signals only (Get-Process), \
         never an OBS WebSocket GetStats round-trip (that's the #391 remote watchdog's job). \
         Program:\n{p}"
    );
}

/// #411 (architectural fix): the RECOVERY decision (confirm/throttle/lock) must ALSO be reused
/// via a gate binary — `obs-self-heal-gate.exe`, calling `camera_box::obs_self_heal::decide`
/// directly — never a hand-rolled PowerShell re-derivation of that state machine. This is the
/// same "reuse, don't reinvent" discipline the wedge verdict already had; a prior review found
/// the confirm/throttle/lock policy was the ONE piece still being re-implemented inline.
#[test]
fn recovery_decision_reuses_self_heal_gate_binary_never_reimplements_state_machine() {
    let p = recovery_script_strih();
    assert!(
        p.contains("obs-self-heal-gate.exe"),
        "#411: the recovery decision MUST pipe through obs-self-heal-gate.exe. Program:\n{p}"
    );
    assert!(
        p.contains("& $SelfHealGateBin"),
        "#411: the decision JSON must actually be piped INTO the self-heal gate binary. \
         Program:\n{p}"
    );
    // The OLD hand-rolled confirm/throttle logic must be GONE — no manual increment/threshold
    // comparison left duplicating decide()'s own branching.
    assert!(
        !p.contains("$state.confirm_count = $state.confirm_count + 1"),
        "#411: the confirm-counter increment must live ONLY in camera_box::obs_self_heal::decide \
         (via the gate binary) — a hand-rolled increment here would be exactly the duplicated \
         state machine the reuse fix removes. Program:\n{p}"
    );
    assert!(
        !p.contains("-lt $ConfirmThreshold") && !p.contains("-lt $MinIntervalS"),
        "#411: no hand-rolled threshold/throttle COMPARISON may remain in PowerShell — that \
         branching belongs ONLY to decide(). Program:\n{p}"
    );
}

/// #411 AHK-race fix, preserved through the issue-1273 single-owner restructure: AutoHotkey64 is
/// stopped (inside the reused embedded launch program, the single owner of the bracket) BEFORE
/// obs64 is ever killed, and the outer failure-path AHK backstop comes only AFTER the
/// post-recovery verify.
#[test]
fn strih_recovery_script_stops_ahk_before_kill_and_restarts_after_verify() {
    let p = recovery_script_strih();

    // The ONLY AutoHotkey64 stop is now the embedded launch program's own (single-owner, issue
    // 1273); it still sits before the obs64 force-kill within that program.
    let stop_ahk_pos = p
        .find("Stop-Process -Name AutoHotkey64")
        .expect("script must contain the embedded launch program's AutoHotkey64 stop");
    let kill_obs_pos = p
        .find("Stop-Process -Id $_.Id -Force")
        .expect("strih script must contain the obs64 force-kill (from build_launch_program)");
    let verify_pos = p
        .find("VerifyRecovered:")
        .expect("strih script must contain the explicit VerifyRecovered step");
    // Anchor on the unique OUTER `RestartAhk backstop` marker rather than the raw relaunch text —
    // the same ahk_resolve_and_relaunch_ps() block is ALSO embedded earlier (inside the reused
    // launch-obs-genlock.sh program), so a bare text anchor would be ambiguous (#867/#1272).
    let backstop_pos = p
        .find("RestartAhk backstop")
        .expect("strih script must contain the outer failure-path RestartAhk backstop");

    assert!(
        stop_ahk_pos < kill_obs_pos,
        "#411 AHK-race fix: AutoHotkey64 must be stopped (by the embedded launch program) BEFORE \
         obs64 is killed. stop_ahk@{stop_ahk_pos} kill_obs@{kill_obs_pos}"
    );
    assert!(
        kill_obs_pos < verify_pos,
        "the kill+relaunch must happen before the explicit post-recovery verify"
    );
    assert!(
        verify_pos < backstop_pos,
        "issue 1273: the outer failure-path AHK backstop must come only AFTER the post-recovery \
         verify, never before"
    );
}

/// stream has no AutoHotkey64 auto-respawn watcher (per `.claude/skills/obs-ops` "AHK on
/// strih" — only strih runs it) — the generated script must document that as a no-op, never
/// guess at an AHK script path that doesn't exist on stream.
#[test]
fn stream_recovery_script_never_touches_ahk() {
    let p = recovery_script_stream();
    assert!(
        !p.contains("Stop-Process -Name AutoHotkey64"),
        "stream has no AHK watcher — must not emit a real AutoHotkey64 stop command. \
         Program:\n{p}"
    );
    assert!(
        !p.contains("$ahkScriptPath"),
        "stream has no AHK watcher — must not embed the AutoHotkey64 resolve+relaunch machinery \
         (#867's ahk_resolve_and_relaunch_ps) at all. Program:\n{p}"
    );
    assert!(
        p.contains("no-op") && p.contains("AutoHotkey64"),
        "stream's script must document the AHK steps as documented no-ops. Program:\n{p}"
    );
}

/// #411 spec: "REUSE build_launch_program... ONE idempotent self-verifying launch path" — the
/// KillAndRelaunchObs step must embed launch-obs-genlock.sh's OWN force=1 program verbatim,
/// never a second hand-rolled kill+relaunch.
#[test]
fn kill_and_relaunch_step_reuses_launch_obs_genlock_program_verbatim() {
    let p = recovery_script_strih();
    let expected = expected_kill_relaunch_program();
    assert!(
        !expected.is_empty(),
        "sanity: launch-obs-genlock.sh's build_launch_program must produce non-empty output"
    );
    assert!(
        p.contains(expected.trim()),
        "#411: the self-heal recovery script must embed launch-obs-genlock.sh's build_launch_program \
         (--force) output VERBATIM for the kill+relaunch step — no second launch path. \
         Missing expected program body.\nGot:\n{p}"
    );
    // The genlock build-proof marker (from the reused program) must be present — proves this
    // is the REAL launch-obs-genlock.sh program, not a paraphrase.
    assert!(
        p.contains("render tick ENABLED"),
        "the reused launch program's own log-verify marker must be present"
    );
}

/// Post-recovery verify rule matches `obs_self_heal::recovery_verified`: exactly one obs64 AND
/// the reused launch program's own exit code (which gates on render-tick-ENABLED).
#[test]
fn verify_step_checks_exactly_one_obs64_and_relaunch_exit_code() {
    let p = recovery_script_strih();
    assert!(
        p.contains("$postCount -eq 1") && p.contains("$relaunchExit -eq 0"),
        "#411: VerifyRecovered must check exactly-one-obs64 AND the relaunch program's exit \
         code (which itself gates on render tick ENABLED). Program:\n{p}"
    );
}

/// The AutoHotkey64 restart is NEVER withheld on a false `$verified` (obs_self_heal.rs doc: AHK's
/// crash-respawn duty is more valuable always-on). Issue 1273: on a clean recovery the embedded
/// program restarts it (independent of obs64's own render-verify); on a failure the outer backstop
/// does — gated on `$relaunchExit`, never on `$verified`.
#[test]
fn restart_ahk_runs_regardless_of_verify_outcome() {
    let p = recovery_script_strih();
    let verify_pos = p
        .find("$verified  = ")
        .expect("verify assignment must exist");
    let backstop_pos = p
        .find("RestartAhk backstop")
        .expect("backstop step must exist");
    // No `if ($verified)` gate wraps the AHK restart — the backstop between the verify and the
    // lock-clear gates on $relaunchExit, never on $verified.
    let between = &p[verify_pos..backstop_pos];
    assert!(
        !between.contains("if ($verified)") && !between.contains("if (-not $verified)"),
        "issue 1273: the AHK restart must NOT be gated on $verified. Between:\n{between}"
    );
    assert!(
        p.contains("if ($relaunchExit -ne 0)"),
        "issue 1273: the failure-path backstop gates on the embedded program's exit code, never \
         on $verified. Program:\n{p}"
    );
}

/// #867: the AutoHotkey64 restart must NEVER rely on a bare `-FilePath 'AutoHotkey64.exe'` launch
/// — AHK v2 is installed user-scoped under `%LOCALAPPDATA%\Programs\AutoHotkey\v2\` and is NOT on
/// PATH (confirmed live on strih via comment #5121884098 on #867), so a bare exe-name launch can
/// never resolve. This is the exact shape that root-caused a real strih outage.
#[test]
fn strih_ahk_restart_never_uses_bare_exe_name_867() {
    let p = recovery_script_strih();
    assert!(
        !p.contains("-FilePath 'AutoHotkey64.exe'")
            && !p.contains("-FilePath \"AutoHotkey64.exe\""),
        "#867: must never launch AutoHotkey64 by a bare exe name relying on PATH. Program:\n{p}"
    );
    assert!(
        p.contains("LOCALAPPDATA") && p.contains("AutoHotkey\\v2\\AutoHotkey64.exe"),
        "#867: must probe the user-scoped %LOCALAPPDATA%\\Programs\\AutoHotkey\\v2\\ install \
         location. Program:\n{p}"
    );
}

/// #867 discipline preserved through the issue-1273 restructure: the OUTER failure-path AHK
/// restart (backstop) must be VERIFIED (poll `Get-Process AutoHotkey64` / `$ahkRelaunchVerified`)
/// and log an explicit FATAL line — never a blind success claim — when AutoHotkey64 does not come
/// back.
#[test]
fn strih_ahk_restart_is_verified_and_logs_fatal_on_failure_867() {
    let p = recovery_script_strih();
    assert!(
        p.contains("Get-Process AutoHotkey64") && p.contains("$ahkRelaunchVerified"),
        "#867: the AHK restart must poll Get-Process AutoHotkey64 and record whether it \
         verified. Program:\n{p}"
    );
    let backstop_pos = p
        .find("RestartAhk backstop")
        .expect("RestartAhk backstop marker must exist");
    let success_pos = p[backstop_pos..]
        .find("RestartAhk backstop: AutoHotkey64 relaunched via")
        .map(|off| off + backstop_pos)
        .expect("a success log line naming the relaunch target must exist");
    let fatal_pos = p[backstop_pos..]
        .find("FATAL: RestartAhk backstop failed")
        .map(|off| off + backstop_pos)
        .expect("an explicit FATAL log line must exist when the relaunch is not verified");
    assert!(
        success_pos < fatal_pos,
        "#867: success log line must precede the failure log line (both inside the same \
         if/else on $ahkRelaunchVerified). Program:\n{p}"
    );
}

/// #411 (closes a prior review gap): an OMITTED override becomes a literal PowerShell `$null`,
/// which `obs-self-heal-gate.exe` then defaults to its OWN `camera_box::obs_self_heal::
/// DEFAULT_*` Rust constant — never a second hardcoded bash/PowerShell literal that could drift.
#[test]
fn omitted_overrides_become_powershell_null_so_the_rust_kernel_defaults_apply() {
    let p = recovery_script_strih();
    assert!(
        p.contains("$ConfirmThresholdOverride = $null"),
        "#411: an omitted --confirm-threshold must emit $null (not a hardcoded number), so \
         obs-self-heal-gate.exe applies DEFAULT_CONFIRM_THRESHOLD itself. Program:\n{p}"
    );
    assert!(
        p.contains("$MinIntervalSOverride     = $null"),
        "#411: an omitted --min-interval-s must emit $null. Program:\n{p}"
    );
    assert!(
        p.contains("$StaleLockSOverride       = $null"),
        "#411: an omitted --stale-lock-s must emit $null. Program:\n{p}"
    );
    // And those override variables must actually be threaded into the JSON sent to the gate.
    assert!(
        p.contains("threshold            = $ConfirmThresholdOverride")
            && p.contains("min_interval_s       = $MinIntervalSOverride")
            && p.contains("stale_lock_s         = $StaleLockSOverride"),
        "#411: the override variables must flow into the decision JSON payload. Program:\n{p}"
    );
}

/// An EXPLICIT override (e.g. a supervisor tuning the cadence at generation time) must flow
/// through as a real number, not be silently dropped to null.
#[test]
fn explicit_overrides_flow_through_as_numbers() {
    let p = run_sourced(&format!(
        "build_recovery_script strih '{OBS_DIR}' 30 {DEFAULT_CONFIRM_THRESHOLD} {DEFAULT_MIN_RECOVERY_INTERVAL_S} {DEFAULT_STALE_LOCK_S}"
    ));
    assert!(
        p.contains(&format!(
            "$ConfirmThresholdOverride = {DEFAULT_CONFIRM_THRESHOLD}"
        )),
        "an explicit confirm-threshold override must flow through as a literal number. \
         Program:\n{p}"
    );
    assert!(
        p.contains(&format!(
            "$MinIntervalSOverride     = {DEFAULT_MIN_RECOVERY_INTERVAL_S}"
        )),
        "an explicit min-interval override must flow through as a literal number. Program:\n{p}"
    );
    assert!(
        p.contains(&format!(
            "$StaleLockSOverride       = {DEFAULT_STALE_LOCK_S}"
        )),
        "an explicit stale-lock override must flow through as a literal number. Program:\n{p}"
    );
}

/// The lock-state MERGE (from the gate binary's `next_state`) happens BEFORE the recovery steps
/// run (fail-safe: a crash mid-recovery must leave the lock HELD, never silently cleared), and
/// the EXPLICIT clear only happens AFTER the full plan completes.
#[test]
fn recovery_lock_merge_precedes_steps_and_explicit_clear_follows_them() {
    let p = recovery_script_strih();
    let merge_pos = p
        .find("$state.recovery_in_progress = $decision.next_state.recovery_in_progress")
        .expect("the next_state merge must exist");
    // The recovery plan's first real step is the embedded launch program's own AutoHotkey64 stop
    // (single owner, issue 1273).
    let stop_ahk_pos = p
        .find("Stop-Process -Name AutoHotkey64")
        .expect("the embedded launch program's AutoHotkey64 stop must exist");
    let lock_clear_pos = p
        .find("$state.recovery_in_progress = $false")
        .expect("explicit lock-clear line must exist");
    let backstop_pos = p
        .find("RestartAhk backstop")
        .expect("RestartAhk backstop step must exist");
    assert!(
        merge_pos < stop_ahk_pos,
        "the next_state merge (which sets the lock when decide() returns Recover) must happen \
         BEFORE the recovery plan's first step (the embedded launch program's AutoHotkey64 stop)"
    );
    assert!(
        backstop_pos < lock_clear_pos,
        "the explicit lock-clear must happen only AFTER the recovery plan's last step \
         (the RestartAhk backstop)"
    );
}

/// Fail-loud when the wedge-verdict gate binary is missing — never silently guess a
/// healthy/wedged verdict, and PERSIST state before exiting (a prior review found the exit-5
/// path skipped saving, which could strand an in-memory stale-lock clear unpersisted).
#[test]
fn missing_gate_binary_fails_loud_and_saves_state_first() {
    let p = recovery_script_strih();
    let missing_check_pos = p
        .find("if (-not (Test-Path $GateBin))")
        .expect("missing-binary check must exist");
    let exit5_pos = p[missing_check_pos..]
        .find("exit 5")
        .map(|off| off + missing_check_pos)
        .expect("must exit 5 on a missing gate binary");
    let save_pos = p[missing_check_pos..exit5_pos]
        .find("Save-SelfHealState")
        .map(|off| off + missing_check_pos);
    assert!(
        save_pos.is_some() && save_pos.unwrap() < exit5_pos,
        "#411: a missing obs-watchdog-gate.exe must Save-SelfHealState BEFORE exiting 5, never \
         skip persisting whatever state was already computed this pass. Program:\n{p}"
    );
}

/// Fail-loud when the RECOVERY-decision gate binary is missing — same discipline as the wedge
/// gate: never guess, always persist state first.
#[test]
fn missing_self_heal_gate_binary_fails_loud_and_saves_state_first() {
    let p = recovery_script_strih();
    let missing_check_pos = p
        .find("if (-not (Test-Path $SelfHealGateBin))")
        .expect("missing self-heal-gate-binary check must exist");
    let exit9_pos = p[missing_check_pos..]
        .find("exit 9")
        .map(|off| off + missing_check_pos)
        .expect("must exit 9 on a missing self-heal gate binary");
    let save_pos = p[missing_check_pos..exit9_pos]
        .find("Save-SelfHealState")
        .map(|off| off + missing_check_pos);
    assert!(
        save_pos.is_some() && save_pos.unwrap() < exit9_pos,
        "#411: a missing obs-self-heal-gate.exe must Save-SelfHealState BEFORE exiting 9. \
         Program:\n{p}"
    );
}

/// #411: a gate exit code of 2 is a TOOLING error in the payload the self-heal script itself
/// built (bad JSON, wrong field type) — that is a self-heal bug, NOT evidence of a wedge, and
/// must NEVER be conflated with `wedged=true` (which would force-kill a possibly-healthy box off
/// our OWN bug). Exit 1 (a real classify verdict) is the ONLY input that sets `$wedged = $true`.
#[test]
fn gate_exit_two_is_a_tooling_error_never_treated_as_wedged() {
    let p = recovery_script_strih();
    assert!(
        p.contains("$wedged = ($gateExit -eq 1)"),
        "#411: wedged must be derived ONLY from gate exit == 1, never from \"!= 0\" (which would \
         conflate a tooling error (exit 2) with a real wedge). Program:\n{p}"
    );
    assert!(
        p.contains("if ($gateExit -eq 2)") && p.contains("exit 6"),
        "#411: gate exit 2 must be handled as a distinct FATAL tooling-error path (skip the pass, \
         never act), not fall through into the wedge decision. Program:\n{p}"
    );
}

/// #411 (fixes a review finding): an UNEXPECTED exit code from `obs-watchdog-gate.exe` (a
/// crash/panic — e.g. 101 — is neither 0, 1, nor 2) must NEVER silently fall through to
/// `$wedged = $false` (which would stop detecting a real wedge). It must fail loud instead.
#[test]
fn unexpected_watchdog_gate_exit_code_fails_loud_never_reads_as_healthy() {
    let p = recovery_script_strih();
    assert!(
        p.contains("if ($gateExit -ne 0 -and $gateExit -ne 1)") && p.contains("exit 8"),
        "#411: an obs-watchdog-gate.exe exit code outside {{0,1,2}} (e.g. a panic) must be \
         handled as a distinct FATAL path — never silently coerced to healthy via \
         '$wedged = ($gateExit -eq 1)' evaluating false. Program:\n{p}"
    );
}

/// Same discipline for the recovery-decision gate binary: a truly unexpected exit code (neither
/// 0, 1, nor the distinct tooling-error 2) must fail loud, never be silently treated as "no
/// action needed".
#[test]
fn unexpected_self_heal_gate_exit_code_fails_loud() {
    let p = recovery_script_strih();
    assert!(
        p.contains("if ($decisionExit -ne 0 -and $decisionExit -ne 1)") && p.contains("exit 10"),
        "#411: an obs-self-heal-gate.exe exit code outside {{0,1,2}} must fail loud, never be \
         silently parsed as a decision. Program:\n{p}"
    );
}

/// #411 (review round 2): obs-self-heal-gate.exe's exit 2 is a TOOLING error in the payload
/// THIS script built — the same distinction obs-watchdog-gate.exe's exit 2 already gets — and
/// must be a DISTINCT FATAL path from a true crash/panic, so an operator's log line pinpoints
/// "our own JSON was malformed" instead of the generic "possible crash" message.
#[test]
fn self_heal_gate_exit_two_is_a_distinct_tooling_error_path() {
    let p = recovery_script_strih();
    let exit2_pos = p
        .find("if ($decisionExit -eq 2)")
        .expect("obs-self-heal-gate.exe exit 2 must have its own distinct check");
    let exit11_pos = p[exit2_pos..]
        .find("exit 11")
        .map(|off| off + exit2_pos)
        .expect("the distinct tooling-error path must exit with its own code (11)");
    let generic_pos = p
        .find("if ($decisionExit -ne 0 -and $decisionExit -ne 1)")
        .expect("the generic unexpected-exit-code check must still exist");
    assert!(
        exit2_pos < generic_pos && exit11_pos < generic_pos,
        "the distinct exit-2 tooling-error check must run BEFORE the generic unexpected-exit-code \
         check, so exit 2 never falls into the generic 'possible crash' path. Program:\n{p}"
    );
}

/// #411 (review round 2): a cleared stale lock must be LOGGED distinctly — an operator
/// diagnosing an incident needs to tell "fresh confirm cycle" apart from "recovering from an
/// abandoned lock", a diagnostic signal a prior refactor silently dropped.
#[test]
fn stale_lock_cleared_is_logged_distinctly() {
    let p = recovery_script_strih();
    assert!(
        p.contains("$decision.stale_lock_cleared") && p.contains("STALE LOCK CLEARED"),
        "#411: the recovery script must check obs-self-heal-gate.exe's stale_lock_cleared field \
         and log it distinctly. Program:\n{p}"
    );
}

/// #411 (review round 2): the live-verify display text must never hardcode the Rust kernel's
/// default threshold as a bare number — that is exactly the second-literal drift risk this
/// design otherwise eliminates.
#[test]
fn confirm_threshold_display_never_hardcodes_a_bare_default_number() {
    let (_, out, _) = run_script(&["--box", "strih"]);
    assert!(
        !out.contains("confirm-threshold=obs-self-heal-gate.exe default (2)"),
        "#411: the STEP 3 display text must not hardcode a bare default number — it must \
         reference the Rust constant by name instead. out=\n{out}"
    );
    assert!(
        out.contains("DEFAULT_CONFIRM_THRESHOLD"),
        "#411: with no override, STEP 3 must point at DEFAULT_CONFIRM_THRESHOLD by name, never a \
         duplicated literal. out=\n{out}"
    );
}

/// A non-finite or negative CPU-percent computation must never be sent to the gate as a number
/// (NaN/Infinity are not valid JSON and could poison the parse) — it degrades to "not sampled".
#[test]
fn non_finite_or_negative_cpu_percent_is_never_sent_as_a_number() {
    let p = recovery_script_strih();
    assert!(
        p.contains("[double]::IsFinite($computed)") && p.contains("$computed -ge 0"),
        "#411: the CPU% computation must be guarded finite+non-negative before being kept — a \
         PID-reuse negative delta or a non-finite result must degrade to null, never be sent as \
         a bogus number. Program:\n{p}"
    );
}

/// Corrupt/missing state file must fall back to SAFE (all-zero/false) defaults, never crash and
/// never silently assume a value that could suppress detection.
#[test]
fn corrupt_state_file_falls_back_to_safe_defaults() {
    let p = recovery_script_strih();
    assert!(
        p.contains("catch") && p.contains("starting fresh"),
        "#411: a corrupt state file must be caught and reset to safe defaults, not crash the \
         whole recovery pass. Program:\n{p}"
    );
}

// ─── issue 1273: single-owner AHK bracket + failure-path backstop ─────────────────────────────

/// issue 1273: the embedded launch-obs-genlock program (built with has_ahk=1 on strih) is the
/// SINGLE OWNER of the AutoHotkey64 stop/restart bracket — it stops AHK before killing obs64,
/// restarts + verifies it after the launch, then runs its own #978 session gate. Because that
/// embedded program runs in a SEPARATE `powershell.exe -File` child, an OUTER pre-stop left its
/// own `$ahkStopped` false, so its restart never fired and its #978 session gate exit-8'd on a
/// clean recovery — force-falsing the outer `$verified`. After the fix, the ONLY AutoHotkey64
/// stop in the whole emitted program is the ONE inside the embedded launch program itself.
#[test]
fn outer_self_heal_never_pre_stops_ahk_embedded_program_owns_the_bracket_1273() {
    let p = recovery_script_strih();
    let embedded = expected_kill_relaunch_program();
    assert!(
        p.contains(embedded.trim()),
        "sanity: the embedded launch program must be embedded verbatim so it can be stripped"
    );
    // Strip the embedded launch program (which legitimately owns the AHK stop); any AutoHotkey64
    // stop left in the OUTER remainder is an illegitimate outer pre-stop (the issue-1273 bug).
    let outer_only = p.replace(embedded.trim(), "<<EMBEDDED LAUNCH PROGRAM>>");
    assert!(
        !outer_only.contains("Stop-Process -Name AutoHotkey64"),
        "issue 1273: the OUTER self-heal script must not pre-stop AutoHotkey64 — the embedded \
         launch-obs-genlock program owns the whole AHK bracket. Outer remainder (embedded \
         stripped):\n{outer_only}"
    );
}

/// issue 1273: the outer script's ONLY AutoHotkey64 action is a FAILURE-PATH backstop — if the
/// embedded launch program exited non-zero (it may have aborted before its own restart point,
/// e.g. an audio-buffering exit 7 that sits before it) AND AutoHotkey64 is genuinely down,
/// best-effort relaunch it so a wedged box never ends with no respawn watcher. It is gated on
/// `$relaunchExit -ne 0` (never on `$verified`, never unconditional) and idempotent AHK-present
/// (only acts when AutoHotkey64 is down, never double-launching what the embedded program restored).
#[test]
fn strih_failure_path_ahk_backstop_is_gated_and_idempotent_1273() {
    let p = recovery_script_strih();
    assert!(
        p.contains("RestartAhk backstop"),
        "issue 1273: the outer script must carry a failure-path RestartAhk backstop. Program:\n{p}"
    );
    assert!(
        p.contains("if ($relaunchExit -ne 0)"),
        "issue 1273: the backstop must be gated on the embedded program's non-zero exit — never \
         unconditional, never on $verified. Program:\n{p}"
    );
    assert!(
        p.contains("if (-not (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue))"),
        "issue 1273: the backstop must be idempotent AHK-present — only relaunch when AutoHotkey64 \
         is genuinely down, never double-launching what the embedded program already restored. \
         Program:\n{p}"
    );
}

// ─── build_task_xml ──────────────────────────────────────────────────────────────────────────

fn task_xml() -> String {
    run_sourced("build_task_xml camera-box-obs-self-heal-strih 'C:\\ProgramData\\camera-box\\obs-self-heal.ps1' 2")
}

/// #411: ships DISABLED. Never auto-enable — the supervisor enables only after live-verify.
#[test]
fn task_xml_ships_disabled() {
    let xml = task_xml();
    assert!(
        xml.contains("<Enabled>false</Enabled>"),
        "#411: the Task Scheduler XML MUST ship with Enabled=false. XML:\n{xml}"
    );
}

/// obs64 is a GUI app — a SYSTEM/Session-0 task cannot launch it into the visible desktop
/// session, so the task MUST run as the interactive logged-on user's own token.
#[test]
fn task_xml_uses_interactive_token_logon_and_highest_run_level() {
    let xml = task_xml();
    assert!(
        xml.contains("<LogonType>InteractiveToken</LogonType>"),
        "#411: must use InteractiveToken logon (obs64 needs the visible desktop session). \
         XML:\n{xml}"
    );
    assert!(
        xml.contains("<RunLevel>HighestAvailable</RunLevel>"),
        "#411: must request HighestAvailable to reliably force-kill/relaunch obs64. XML:\n{xml}"
    );
    assert!(
        xml.contains("__RIG_USER__"),
        "#411: the UserId must be a clearly-marked placeholder for the supervisor to fill in \
         with the box's real account — never a guessed/hardcoded username. XML:\n{xml}"
    );
}

/// Defense in depth alongside the script's own state-file lock: the Task Scheduler task itself
/// must never run two overlapping instances.
#[test]
fn task_xml_never_allows_overlapping_instances() {
    let xml = task_xml();
    assert!(
        xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"),
        "#411: MultipleInstancesPolicy must be IgnoreNew (never overlap two recovery runs). \
         XML:\n{xml}"
    );
}

/// The repetition interval reflects the requested cadence.
#[test]
fn task_xml_repetition_interval_matches_requested_cadence() {
    let xml = run_sourced(
        "build_task_xml camera-box-obs-self-heal-stream 'C:\\ProgramData\\camera-box\\obs-self-heal.ps1' 5",
    );
    assert!(
        xml.contains("<Interval>PT5M</Interval>"),
        "the repetition interval must reflect the requested 5-minute cadence. XML:\n{xml}"
    );
}

// ─── CLI (main()) ────────────────────────────────────────────────────────────────────────────

/// The CLI selects the correct win-* MCP + target fps per box and emits a full plan.
#[test]
fn cli_box_selects_correct_mcp_and_target_fps() {
    let (code, out, _err) = run_script(&["--box", "strih"]);
    assert_eq!(code, 0, "--box strih must print the plan (exit 0)");
    assert!(out.contains("win-strih") && out.contains("10.77.9.202"));
    assert!(
        out.contains("$TargetFps       = 30"),
        "strih targets 30fps (Topology v2, #459 -- cut-to-stream only; the 60fps IMAG role \
         moved to imag-nb, #458/#463). out=\n{out}"
    );
    assert!(
        out.contains("schtasks /Create"),
        "the plan must include the schtasks registration command"
    );

    let (code, out, _err) = run_script(&["--box", "stream"]);
    assert_eq!(code, 0, "--box stream must print the plan (exit 0)");
    assert!(out.contains("win-stream-snv") && out.contains("10.77.9.204"));
    assert!(
        out.contains("$TargetFps       = 30"),
        "stream targets 30fps (final mixed 60+30 topology). out=\n{out}"
    );
}

/// #411 (closes the top review finding, verified via the REAL `main()` invocation path — not
/// just `build_recovery_script` called directly): with NO override flags, the plan an operator
/// actually runs (`--box strih`, no `--confirm-threshold`/etc) must install `$null` overrides —
/// i.e. obs-self-heal-gate.exe's own DEFAULT_* Rust constants apply, never a stale bash literal.
#[test]
fn main_cli_default_omits_overrides_so_rust_kernel_defaults_apply() {
    let (code, out, _err) = run_script(&["--box", "strih"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("$ConfirmThresholdOverride = $null")
            && out.contains("$MinIntervalSOverride     = $null")
            && out.contains("$StaleLockSOverride       = $null"),
        "#411: `scripts/obs-self-heal-install.sh --box strih` with NO override flags — the exact \
         command the install plan tells the supervisor to run — must emit $null overrides so the \
         Rust kernel's DEFAULT_* constants are authoritative. A stale hardcoded bash literal here \
         would silently diverge from src/obs_self_heal.rs if the Rust consts are ever retuned. \
         out=\n{out}"
    );
}

/// An explicit `--confirm-threshold` (etc) flag on the REAL CLI invocation must flow through as
/// a concrete override, not be silently dropped.
#[test]
fn main_cli_explicit_override_flows_through_the_real_invocation() {
    let (code, out, _err) = run_script(&[
        "--box",
        "strih",
        "--confirm-threshold",
        "5",
        "--min-interval-s",
        "120",
        "--stale-lock-s",
        "600",
    ]);
    assert_eq!(code, 0);
    assert!(
        out.contains("$ConfirmThresholdOverride = 5")
            && out.contains("$MinIntervalSOverride     = 120")
            && out.contains("$StaleLockSOverride       = 600"),
        "#411: explicit override flags on the real CLI invocation must flow through as literal \
         numbers, not silently drop back to $null. out=\n{out}"
    );
}

/// An unknown --box is a usage error (exit 2), never a silent guess.
#[test]
fn unknown_box_is_usage_error_exit_2() {
    let (code, _out, err) = run_script(&["--box", "nope"]);
    assert_eq!(
        code, 2,
        "an unknown box must exit 2 (usage error). stderr={err}"
    );
}

/// A trailing value-taking flag with no value is a clean usage error, not a set -e abort.
#[test]
fn trailing_flag_without_value_is_usage_error_exit_2() {
    let (code, _out, err) = run_script(&["--box"]);
    assert_eq!(code, 2, "--box with no value must exit 2. stderr={err}");
}

/// The plan states the live-verify procedure MUST run before enabling — never skip straight to
/// enable.
#[test]
fn plan_documents_mandatory_live_verify_before_enable() {
    let (_, out, _) = run_script(&["--box", "strih"]);
    assert!(
        out.contains("LIVE-VERIFY") && out.contains("Healthy-box dry run"),
        "the plan must document the mandatory live-verify sequence. out=\n{out}"
    );
    assert!(
        out.contains("schtasks /Change") && out.contains("/ENABLE"),
        "enabling must be an explicit LAST step, never automatic. out=\n{out}"
    );
}

/// STEP 0 of the install plan must mention deploying BOTH gate binaries — a supervisor following
/// only the old wording would deploy obs-watchdog-gate.exe and miss obs-self-heal-gate.exe,
/// leaving the recovery decision unable to run (fail-loud exit 9, per the missing-binary test).
#[test]
fn plan_step_zero_mentions_both_gate_binaries() {
    let (_, out, _) = run_script(&["--box", "strih"]);
    assert!(
        out.contains("obs-watchdog-gate.exe") && out.contains("obs-self-heal-gate.exe"),
        "STEP 0 must instruct the supervisor to deploy BOTH gate binaries. out=\n{out}"
    );
}

// ─── #89: GPU device-removed — local OBS-log DXGI audit + cause + reboot opt-in ────────────────

/// The recovery script must audit the box's OWN OBS log locally for the DXGI device-lost
/// signature (#89) — no MCP/ssh needed on the self-heal box itself — and feed the result into
/// BOTH the wedge-verdict sample (obs-watchdog-gate.exe) and the recovery cause (obs-self-heal-
/// gate.exe), never re-implementing the DXGI code match (it must reference the same codes
/// `camera_box::dxgi_device_lost::DXGI_DEVICE_LOST_CODES` uses).
#[test]
fn recovery_script_performs_local_obs_log_dxgi_audit() {
    let p = recovery_script_strih();
    assert!(
        p.contains("887A0005") && p.contains("887A0006") && p.contains("887A0007"),
        "#89: the recovery script must check the OBS log for all three DXGI device-lost codes. \
         Program:\n{p}"
    );
    assert!(
        p.contains("dxgiDeviceLost") || p.contains("DxgiDeviceLost"),
        "#89: the script must compute a dxgi-device-lost signal. Program:\n{p}"
    );
    assert!(
        p.contains("dxgi_device_lost"),
        "#89: the dxgi audit result must flow into the sample sent to obs-watchdog-gate.exe. \
         Program:\n{p}"
    );
}

/// The computed cause ("GpuDeviceRemoved" when the DXGI audit found the signature, else
/// "ProcessWedge") must flow into the decision JSON sent to obs-self-heal-gate.exe.
#[test]
fn recovery_script_computes_cause_and_sends_it_to_the_self_heal_gate() {
    let p = recovery_script_strih();
    assert!(
        p.contains("GpuDeviceRemoved") && p.contains("ProcessWedge"),
        "#89: the script must select between the two WedgeCause values. Program:\n{p}"
    );
    assert!(
        p.contains("cause") && p.contains("reboot_enabled"),
        "#89: both cause and reboot_enabled must be sent in the decision JSON payload. \
         Program:\n{p}"
    );
}

/// `--enable-reboot` defaults OFF — an omitted flag must emit `$false`, never `$true` (a host
/// reboot is a destructive, approval-gated action per no-destructive-remote-actions.md).
#[test]
fn enable_reboot_defaults_to_false_when_omitted() {
    let (code, out, _err) = run_script(&["--box", "strih"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("$RebootEnabledOverride    = $false"),
        "#89: an omitted --enable-reboot must install $false, never $true or $null (this is a \
         plain boolean opt-in, not a tunable magic number). out=\n{out}"
    );
}

/// An explicit `--enable-reboot` flag must flow through as `$true`.
#[test]
fn enable_reboot_flag_flows_through_as_true() {
    let (code, out, _err) = run_script(&["--box", "strih", "--enable-reboot"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("$RebootEnabledOverride    = $true"),
        "#89: --enable-reboot must install $true. out=\n{out}"
    );
}

/// The Recover switch arm must handle a `RebootPc`-only plan (executes a real reboot) AND an
/// EMPTY plan (GpuDeviceRemoved confirmed, reboot disabled — alert-only, no automatic action) —
/// distinct from the original 4-step process-wedge branch, which must still be present verbatim.
#[test]
fn recover_arm_handles_reboot_pc_and_empty_plan_branches() {
    let p = recovery_script_strih();
    assert!(
        p.contains("RebootPc") && p.contains("Restart-Computer"),
        "#89: a RebootPc-only plan must actually execute a reboot. Program:\n{p}"
    );
    assert!(
        p.to_lowercase().contains("auto-reboot") && p.to_lowercase().contains("disabled"),
        "#89: an empty plan (GpuDeviceRemoved, reboot disabled) must log that auto-reboot is \
         disabled and no action was taken. Program:\n{p}"
    );
    // The ORIGINAL 4-step process-wedge branch must still be reachable and unchanged.
    assert!(
        p.contains("KillAndRelaunchObs"),
        "#89: the original process-wedge plan branch must still be present. Program:\n{p}"
    );
}

// ─── behavioral cross-check: the REAL compiled obs-self-heal-gate binary ───────────────────────
//
// The tests above only prove the PowerShell TEXT calls obs-self-heal-gate.exe correctly. These
// tests invoke the ACTUAL compiled binary (built as a normal cargo test dependency via
// CARGO_BIN_EXE_*) with the exact JSON shape the PowerShell sends, proving the real end-to-end
// contract works — not just that the two sides' textual claims agree.

fn self_heal_gate_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_obs-self-heal-gate"))
}

fn run_self_heal_gate(json: &str) -> (i32, String) {
    use std::io::Write;
    let mut child = Command::new(self_heal_gate_bin())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn obs-self-heal-gate");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("failed to wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The EXACT JSON shape `build_recovery_script`'s PowerShell sends (field names/order don't
/// matter for JSON, but every field it emits must be accepted) — a healthy pass.
#[test]
fn real_binary_accepts_the_exact_powershell_payload_shape_healthy() {
    let json = r#"{"confirm_count":0,"last_attempt_epoch_s":null,"recovery_in_progress":false,
        "wedged":false,"now_epoch_s":1700000000,"threshold":null,"min_interval_s":null,
        "stale_lock_s":null}"#;
    let (code, out) = run_self_heal_gate(json);
    assert_eq!(
        code, 0,
        "healthy pass must exit 0 to skip the recovery steps. out={out}"
    );
    assert!(out.contains("\"decision\":\"Healthy\""));
    assert!(
        out.contains("\"next_state\""),
        "next_state must always be present so PowerShell can persist it. out={out}"
    );
    assert!(
        out.contains("\"stale_lock_cleared\":false"),
        "stale_lock_cleared must always be present (false here — nothing was abandoned) so the \
         PowerShell side can log it distinctly when true. out={out}"
    );
}

/// The EXACT JSON shape for a confirmed wedge — exit 1 tells PowerShell to actually run the
/// switch's 'Recover' arm, and the returned `steps` array matches `recovery_plan()`'s order.
#[test]
fn real_binary_confirmed_wedge_returns_recover_with_the_ahk_safe_step_order() {
    let json = r#"{"confirm_count":1,"last_attempt_epoch_s":null,"recovery_in_progress":false,
        "wedged":true,"now_epoch_s":1700000000,"threshold":null,"min_interval_s":null,
        "stale_lock_s":null}"#;
    let (code, out) = run_self_heal_gate(json);
    assert_eq!(
        code, 1,
        "a confirmed wedge must exit 1 so PowerShell's switch runs 'Recover'. out={out}"
    );
    assert!(out.contains("\"decision\":\"Recover\""));
    assert!(
        out.contains(r#""steps":["StopAhk","KillAndRelaunchObs","VerifyRecovered","RestartAhk"]"#),
        "the returned step order must be the AHK-race-safe order PowerShell's switch depends on \
         (StopAhk first, RestartAhk last). out={out}"
    );
}
