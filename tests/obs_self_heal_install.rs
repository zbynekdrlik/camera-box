//! #411 — structural/content tests for the Windows-local unattended self-heal mechanism
//! (`scripts/obs-self-heal-install.sh`).
//!
//! These are pure-shell / content tests — NO rig, NO OBS, NO Windows host, mirroring
//! `tests/launch_obs_genlock.rs`'s shape exactly. They source the script (never execute the real
//! flow), call its pure `build_recovery_script` / `build_task_xml` builders, and assert:
//!
//! - the wedge verdict is REUSED via `obs-watchdog-gate.exe` (never a re-derived threshold — the
//!   emitted PowerShell must NOT contain the magic numbers `obs_watchdog::classify` uses),
//! - the AHK-race-safe step order is preserved verbatim in the generated PowerShell (StopAhk
//!   before the obs64 kill, RestartAhk after the post-recovery verify),
//! - the kill+relaunch step REUSES `launch-obs-genlock.sh`'s `build_launch_program` byte-for-byte
//!   (never a second hand-rolled launch path),
//! - the confirm/throttle/stale-lock numbers passed through match
//!   `camera_box::obs_self_heal`'s own `DEFAULT_*` constants (true cross-language lock-step, since
//!   this test file imports those constants directly rather than hardcoding a copy),
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

fn recovery_script_strih() -> String {
    run_sourced(&format!(
        "build_recovery_script strih '{OBS_DIR}' 60 {DEFAULT_CONFIRM_THRESHOLD} {DEFAULT_MIN_RECOVERY_INTERVAL_S} {DEFAULT_STALE_LOCK_S}"
    ))
}

fn recovery_script_stream() -> String {
    run_sourced(&format!(
        "build_recovery_script stream '{OBS_DIR}' 30 {DEFAULT_CONFIRM_THRESHOLD} {DEFAULT_MIN_RECOVERY_INTERVAL_S} {DEFAULT_STALE_LOCK_S}"
    ))
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

/// #411: the AHK-race fix — strih's script must stop AutoHotkey64 BEFORE ever touching obs64,
/// and restart it only AFTER the post-recovery verify.
#[test]
fn strih_recovery_script_stops_ahk_before_kill_and_restarts_after_verify() {
    let p = recovery_script_strih();

    let stop_ahk_pos = p
        .find("Stop-Process -Name AutoHotkey64")
        .expect("strih script must contain a real AutoHotkey64 stop command");
    let kill_obs_pos = p
        .find("Stop-Process -Id $_.Id -Force")
        .expect("strih script must contain the obs64 force-kill (from build_launch_program)");
    let verify_pos = p
        .find("VerifyRecovered:")
        .expect("strih script must contain the explicit VerifyRecovered step");
    let start_ahk_pos = p
        .find("Start-Process -FilePath 'AutoHotkey64.exe'")
        .expect("strih script must contain a real AutoHotkey64 restart command");

    assert!(
        stop_ahk_pos < kill_obs_pos,
        "#411 AHK-race fix: AutoHotkey64 must be stopped BEFORE obs64 is ever touched. \
         stop_ahk@{stop_ahk_pos} kill_obs@{kill_obs_pos}"
    );
    assert!(
        kill_obs_pos < verify_pos,
        "the kill+relaunch must happen before the explicit post-recovery verify"
    );
    assert!(
        verify_pos < start_ahk_pos,
        "#411 AHK-race fix: AutoHotkey64 must be restarted only AFTER the post-recovery \
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
        !p.contains("Start-Process -FilePath 'AutoHotkey64.exe'"),
        "stream has no AHK watcher — must not emit a real AutoHotkey64 start command. \
         Program:\n{p}"
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

/// RestartAhk must run unconditionally after VerifyRecovered (see obs_self_heal.rs doc: AHK's
/// crash-respawn duty is more valuable always-on than conditionally withheld on a failed verify).
#[test]
fn restart_ahk_runs_regardless_of_verify_outcome() {
    let p = recovery_script_strih();
    let verify_pos = p
        .find("$verified  = ")
        .expect("verify assignment must exist");
    let restart_pos = p
        .find("Start-Process -FilePath 'AutoHotkey64.exe'")
        .expect("restart command must exist");
    // No `if ($verified)` gate wraps the restart — the restart line appears unconditionally
    // right after the verify block, not inside a conditional branch on $verified.
    let between = &p[verify_pos..restart_pos];
    assert!(
        !between.contains("if ($verified)") && !between.contains("if (-not $verified)"),
        "#411: RestartAhk must NOT be gated on $verified — it always runs. Between:\n{between}"
    );
}

/// The confirm/throttle/stale-lock numbers must be threaded through into the generated script
/// literally (true cross-language lock-step: this test imports the Rust DEFAULT_* constants).
#[test]
fn confirm_throttle_stale_lock_constants_match_the_rust_kernel() {
    let p = recovery_script_strih();
    assert!(
        p.contains(&format!("$ConfirmThreshold = {DEFAULT_CONFIRM_THRESHOLD}")),
        "#411: ConfirmThreshold must match camera_box::obs_self_heal::DEFAULT_CONFIRM_THRESHOLD \
         ({DEFAULT_CONFIRM_THRESHOLD}). Program:\n{p}"
    );
    assert!(
        p.contains(&format!(
            "$MinIntervalS     = {DEFAULT_MIN_RECOVERY_INTERVAL_S}"
        )),
        "#411: MinIntervalS must match camera_box::obs_self_heal::DEFAULT_MIN_RECOVERY_INTERVAL_S \
         ({DEFAULT_MIN_RECOVERY_INTERVAL_S}). Program:\n{p}"
    );
    assert!(
        p.contains(&format!("$StaleLockS       = {DEFAULT_STALE_LOCK_S}")),
        "#411: StaleLockS must match camera_box::obs_self_heal::DEFAULT_STALE_LOCK_S \
         ({DEFAULT_STALE_LOCK_S}). Program:\n{p}"
    );
}

/// The lock is set BEFORE obs64 is ever touched (fail-safe: a crash mid-recovery must leave the
/// lock held, never silently cleared) and only cleared AFTER the full plan completes.
#[test]
fn recovery_lock_is_set_before_acting_and_cleared_after_plan_completes() {
    let p = recovery_script_strih();
    let lock_set_pos = p
        .find("$state.recovery_in_progress = $true")
        .expect("lock-set line must exist");
    let stop_ahk_pos = p
        .find("Stop-Process -Name AutoHotkey64")
        .expect("StopAhk step must exist");
    // NB: `$state.recovery_in_progress = $false` also appears earlier for the STALE-LOCK clear
    // (a different code path) — rfind the LAST occurrence, which is the real post-recovery clear.
    let lock_clear_pos = p
        .rfind("$state.recovery_in_progress = $false")
        .expect("lock-clear line must exist");
    let restart_ahk_pos = p
        .find("Start-Process -FilePath 'AutoHotkey64.exe'")
        .expect("RestartAhk step must exist");
    assert!(
        lock_set_pos < stop_ahk_pos,
        "the lock must be set BEFORE the recovery plan's first step (StopAhk)"
    );
    assert!(
        restart_ahk_pos < lock_clear_pos,
        "the lock must be cleared only AFTER the recovery plan's last step (RestartAhk)"
    );
}

/// Fail-loud when the gate binary is missing — never silently guess a healthy/wedged verdict.
#[test]
fn missing_gate_binary_fails_loud_never_guesses() {
    let p = recovery_script_strih();
    assert!(
        p.contains("if (-not (Test-Path $GateBin))") && p.contains("exit 5"),
        "#411: a missing obs-watchdog-gate.exe must fail loud (non-zero exit), never silently \
         assume a healthy or wedged verdict. Program:\n{p}"
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
        out.contains("$TargetFps        = 60"),
        "strih targets 60fps (final mixed 60+30 topology). out=\n{out}"
    );
    assert!(
        out.contains("schtasks /Create"),
        "the plan must include the schtasks registration command"
    );

    let (code, out, _err) = run_script(&["--box", "stream"]);
    assert_eq!(code, 0, "--box stream must print the plan (exit 0)");
    assert!(out.contains("win-stream-snv") && out.contains("10.77.9.204"));
    assert!(
        out.contains("$TargetFps        = 30"),
        "stream targets 30fps (final mixed 60+30 topology). out=\n{out}"
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
