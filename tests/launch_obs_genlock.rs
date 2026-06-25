//! Behavioral guard for the genlock OBS (re)launch wrapper `scripts/launch-obs-genlock.sh` (#128).
//!
//! ## Why this wrapper exists (#128 — the recurring "stale-env trap")
//!
//! Every OBS relaunch on the genlock boxes (deploy, crash-recovery, reboot, a reserve/config change)
//! MUST come up carrying the four genlock env vars — OBS_GENLOCK_WALL_CLOCK / _RESERVE_MS / _TS_ALIGN
//! / _PRELOAD_FRAMES. When OBS is spawned from a LONG-LIVED win-* MCP shell whose environment
//! snapshot PREDATES the Machine-scope env write, the child obs64 inherits the STALE snapshot, the
//! var is UNSET, and the wall-clock render tick is silently OFF — the whole genlock guarantee gone,
//! invisibly (the #126 deploy near-miss). A `$env:` read in that stale shell agrees with the wrong
//! value, so the AUTHORITATIVE source is the persistent Machine-scope (HKLM) env, and the only proof
//! the child actually inherited it is the launched process's own PEB env.
//!
//! `scripts/launch-obs-genlock.sh` is the PURE, testable PLANNER (same model as
//! scripts/recording-verdict-on-stream.sh — scp/ssh to Windows is denied, so the agent drives the
//! win-* MCP): given the box + OBS dir it emits the exact self-verifying PowerShell program to paste
//! into the box's MCP Shell. These tests source the REAL script (its `BASH_SOURCE != $0` guard skips
//! the executed flow), call its pure `build_launch_program` builder, and assert the emitted program
//! carries the env from Machine, sets it explicit in the spawning shell, launches cwd=bin\64bit,
//! verifies the child PEB env + the OBS-log render-tick line, and FAILS LOUD otherwise — the exact
//! properties that make a relaunch genlock-safe. If anyone weakens the wrapper (drops a var, the
//! Machine read, the PEB verify, the fail-loud exit, or the cwd), the stale-env trap can bite again
//! and one of these fails.
//!
//! The convention (source the script, exercise pure functions; run it end-to-end for the CLI
//! contract) matches tests/drift_guard.rs and tests/harness_recording_e2e_paths.rs.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/launch-obs-genlock.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
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
        .expect("failed to run launch-obs-genlock.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The launch program built for a normal (no-force) launch with the default OBS dir.
fn program_default() -> String {
    run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 0")
}

/// The launch program built with --force (a wedged-OBS recovery launch).
fn program_force() -> String {
    run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 1")
}

/// The four genlock env vars that MUST be carried on every launch (#128). A relaunch missing ANY of
/// these is the silent half-genlocked box this wrapper exists to prevent.
const GENLOCK_VARS: [&str; 4] = [
    "OBS_GENLOCK_WALL_CLOCK",
    "OBS_GENLOCK_RESERVE_MS",
    "OBS_GENLOCK_TS_ALIGN",
    "OBS_GENLOCK_PRELOAD_FRAMES",
];

/// `genlock_var_list` is the single source of truth for the four vars and lists EXACTLY them — no
/// more, no fewer. A drift here means the wrapper carries/verifies the wrong set.
#[test]
fn genlock_var_list_is_exactly_the_four() {
    let out = run_sourced("genlock_var_list");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        got, GENLOCK_VARS,
        "#128: genlock_var_list must be exactly the four OBS_GENLOCK_* vars carried on launch — got {got:?}"
    );
}

/// The emitted program reads EACH genlock var from MACHINE scope (the persistent HKLM source of
/// truth, survives reboot) — not from the spawning shell's `$env:` (which can be a stale snapshot).
#[test]
fn program_reads_every_var_from_machine_scope() {
    let p = program_default();
    assert!(
        p.contains("[System.Environment]::GetEnvironmentVariable($n, 'Machine')"),
        "#128: program must read genlock vars from Machine scope (missing the Machine read)"
    );
    for v in GENLOCK_VARS {
        assert!(
            p.contains(v),
            "#128: program must reference genlock var {v} (carried + verified set)"
        );
    }
}

/// The emitted program SETS each var EXPLICITLY in the spawning shell BEFORE Start-Process, so the
/// child obs64 inherits the correct value regardless of any stale env snapshot (#128 core fix).
#[test]
fn program_sets_env_explicit_before_launch() {
    let p = program_default();
    let set_pos = p.find("Set-Item -Path \"Env:$n\"").expect(
        "#128: program must set each genlock var explicit in the spawning shell (Set-Item Env:)",
    );
    let launch_pos = p
        .find("Start-Process")
        .expect("#128: program must launch obs64 via Start-Process");
    assert!(
        set_pos < launch_pos,
        "#128: the explicit env-set MUST happen BEFORE Start-Process — otherwise obs64 inherits the \
         stale snapshot (the exact trap this wrapper prevents)"
    );
}

/// A Machine var that is UNSET must FAIL LOUD (non-zero exit), never launch a half-genlocked OBS.
#[test]
fn program_fails_loud_when_machine_var_unset() {
    let p = program_default();
    assert!(
        p.contains("is UNSET") && p.contains("exit 4"),
        "#128: program must fail loud (non-zero exit) if a genlock Machine var is UNSET — never \
         launch OBS with the genlock env missing"
    );
}

/// OBS MUST be launched with cwd = bin\64bit — a wrong cwd produces a broken ~37MB OBS with no
/// WebSocket / no NDI / no genlock ("Failed to find locale/en-US.ini").
#[test]
fn program_launches_with_bin64_cwd() {
    let p = program_default();
    assert!(
        p.contains("Start-Process -FilePath $exe -WorkingDirectory 'C:\\Program Files\\obs-studio\\bin\\64bit'"),
        "#128: obs64 must be launched with cwd = bin\\64bit (wrong cwd => broken OBS, no genlock)"
    );
}

/// Stale crash sentinels are cleared before launch so the "Crash Detected" modal cannot hang a
/// headless relaunch (which would land in safe mode — DistroAV + genlock disabled).
#[test]
fn program_clears_crash_sentinels() {
    let p = program_default();
    assert!(
        p.contains("Remove-Item \"$env:APPDATA\\obs-studio\\.sentinel\\*\""),
        "#128: program must clear %APPDATA%\\obs-studio\\.sentinel\\* before launch (else the crash \
         modal hangs the headless relaunch into safe mode, disabling genlock)"
    );
}

/// The verify step reads the launched child's PEB env (NtQueryInformationProcess + ReadProcessMemory)
/// and FAILS LOUD on a mismatch — the MCP `$env:` read is a stale snapshot, only the child's own PEB
/// proves what obs64 actually inherited (#128).
#[test]
fn program_verifies_child_peb_env_and_fails_on_mismatch() {
    let p = program_default();
    assert!(
        p.contains("NtQueryInformationProcess") && p.contains("ReadProcessMemory"),
        "#128: program must read the launched obs64's CHILD PEB env to prove inheritance (the MCP \
         $env: read is a stale snapshot and cannot be trusted)"
    );
    assert!(
        p.contains("PEB MISMATCH") && p.contains("$pebOk = $false"),
        "#128: a PEB env that does not match Machine must fail the verify (stale-env trap caught)"
    );
}

/// The verify step also asserts the OBS log shows the render tick ENABLED (the authoritative runtime
/// signal — same line drift-guard.sh genlock_from_log keys on) AND looks for the #235 single-knob
/// genlock-latency line ('genlock: latency = N ms (≈ M frames @ Ffps)' — ms primary, frames in parens).
#[test]
fn program_verifies_render_tick_and_latency_log_lines() {
    let p = program_default();
    assert!(
        p.contains("genlock:.*render tick ENABLED") || p.contains("render tick ENABLED"),
        "#128: program must verify the OBS log shows 'render tick ENABLED' (genlock master gate on)"
    );
    assert!(
        p.contains("genlock: latency = "),
        "#235: program must check the OBS log 'genlock: latency = N ms' single-knob line"
    );
}

/// #235: the wrapper carries the canonical OBS_GENLOCK_LATENCY_MS knob OPTIONALLY — only when it is
/// set in Machine (a re-pinned box). A legacy box on the OBS_GENLOCK_RESERVE_MS alias still launches.
#[test]
fn program_carries_optional_canonical_latency_knob() {
    // The pure list helper exposes exactly the canonical var.
    let out = run_sourced("genlock_optional_var_list");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        got,
        ["OBS_GENLOCK_LATENCY_MS"],
        "#235: genlock_optional_var_list must be exactly [OBS_GENLOCK_LATENCY_MS] — got {got:?}"
    );
    let p = program_default();
    assert!(
        p.contains("OBS_GENLOCK_LATENCY_MS"),
        "#235: the program must reference the canonical OBS_GENLOCK_LATENCY_MS knob"
    );
    // It is OPTIONAL — carried only when set in Machine (no `exit 4` on unset, unlike the required set).
    assert!(
        p.contains("$genlockOptionalVars") && p.contains("$genlockOptionalSet"),
        "#235: the program must carry the canonical knob via the OPTIONAL (set-if-present) path"
    );
}

/// The FINAL verdict gates exit 0 on BOTH the child PEB carrying every var AND the render tick
/// ENABLED; anything short of that exits non-zero (fail loud — never a silent half-genlocked box).
#[test]
fn program_final_verdict_is_fail_loud() {
    let p = program_default();
    assert!(
        p.contains("if ($pebOk -and $tickOk)") && p.contains("exit 0"),
        "#128: success (exit 0) must require BOTH the PEB env carried AND render tick ENABLED"
    );
    assert!(
        p.contains("#128 FAIL") && p.contains("exit 1"),
        "#128: a failed verify must exit non-zero (fail loud) so the box is never trusted silently"
    );
}

/// --force inserts the documented obs-ops force-kill of a wedged obs64 before relaunch; the no-force
/// path instead REFUSES to double-launch a running obs64. The two must be distinct.
#[test]
fn force_inserts_kill_and_noforce_refuses_double_launch() {
    let forced = program_force();
    let normal = program_default();
    assert!(
        forced.contains("Stop-Process -Id $_.Id -Force"),
        "#128: --force must force-kill a wedged obs64 first (documented obs-ops recovery)"
    );
    assert!(
        !normal.contains("Stop-Process -Id $_.Id -Force"),
        "#128: a no-force launch must NOT kill — relaunch deliberately"
    );
    assert!(
        normal.contains("obs64 already running") && normal.contains("exit 3"),
        "#128: a no-force launch must refuse to double-launch a running obs64"
    );
}

/// CLI contract: `--box strih` targets the win-strih MCP (10.77.9.202); `--box stream` targets
/// win-stream-snv (10.77.9.204). Both emit the full launch program. An unknown/missing box errors.
#[test]
fn cli_box_selects_correct_mcp_and_emits_program() {
    let (code_s, out_s, _) = run_script(&["--box", "strih"]);
    assert_eq!(code_s, 0, "strih plan should print (exit 0)");
    assert!(
        out_s.contains("win-strih") && out_s.contains("10.77.9.202"),
        "#128: --box strih must target the win-strih MCP (10.77.9.202)"
    );
    assert!(
        out_s.contains("deterministic genlock OBS (re)launch + verify"),
        "#128: --box strih must emit the full launch+verify program"
    );

    let (code_st, out_st, _) = run_script(&["--box", "stream"]);
    assert_eq!(code_st, 0, "stream plan should print (exit 0)");
    assert!(
        out_st.contains("win-stream-snv") && out_st.contains("10.77.9.204"),
        "#128: --box stream must target the win-stream-snv MCP (10.77.9.204)"
    );

    let (code_bad, _, err_bad) = run_script(&["--box", "nope"]);
    assert_eq!(code_bad, 2, "an unknown box must be a usage error (exit 2)");
    assert!(
        err_bad.contains("must be 'strih' or 'stream'"),
        "#128: an unknown --box must error clearly"
    );

    let (code_none, _, _) = run_script(&[]);
    assert_eq!(code_none, 2, "missing --box must be a usage error (exit 2)");
}

/// A value-taking flag given with NO value (trailing `--box` / `--obs-dir`) must be a CLEAN usage
/// error: exit 2 with a message — NOT a silent `shift 2` abort (exit 1, no diagnostic) under set -e.
#[test]
fn trailing_flag_without_value_is_usage_error_exit_2() {
    for flag in ["--box", "--obs-dir"] {
        let (code, _out, err) = run_script(&[flag]);
        assert_eq!(
            code, 2,
            "#128: a trailing `{flag}` with no value must exit 2 (usage error), not a silent exit 1"
        );
        assert!(
            err.contains("needs a value"),
            "#128: a trailing `{flag}` must print a clear 'needs a value' error (got stderr: {err:?})"
        );
    }
}

/// An OBS dir containing a single quote must NOT break out of the PowerShell single-quoted strings:
/// the quote is doubled ('' the PowerShell escape) wherever obs_dir is interpolated into '...'.
#[test]
fn obs_dir_single_quote_is_escaped_in_powershell() {
    let p = run_sourced("build_launch_program \"C:\\\\Te'st\\\\obs-studio\" 0");
    assert!(
        p.contains("$obsDir = 'C:\\Te''st\\obs-studio'"),
        "#128: a single quote in --obs-dir must be doubled ('') so it can't break out of the \
         PowerShell single-quoted string — got program:\n{p}"
    );
    assert!(
        p.contains("$exe    = 'C:\\Te''st\\obs-studio\\bin\\64bit\\obs64.exe'"),
        "#128: the exe path must carry the same '' escape"
    );
}

/// The script must be SOURCE-SAFE: sourcing it (the unit-test harness) must NOT execute the CLI flow
/// (the BASH_SOURCE != $0 guard), otherwise every source would try to parse the sourcing shell's args.
#[test]
fn script_is_source_safe() {
    // If sourcing executed main, this would error on the missing --box. It returns clean instead.
    let out = run_sourced("echo SOURCED_OK");
    assert!(
        out.contains("SOURCED_OK"),
        "#128: the script must be source-safe (BASH_SOURCE != $0 guard) — sourcing ran the CLI flow"
    );
}

// ===== #247 rig TEST/EVENT-mode burn handling (--mode {test|event}) ===============================
//
// The deterministic rig switch (#247) drives this wrapper with --mode test|event. TEST mode launches
// OBS with the #111 burn ON (OBS_BURN_QR=1 + OBS_BURN_QR_PX=300 + the per-box run_id) set ONLY in the
// launch shell; EVENT mode clears the burns AND asserts the Machine scope carries NONE (the #246
// guard — a Machine-scope burn survives reboot and paints QR on the LIVE broadcast). The default
// no-mode launch is byte-identical to the pre-#247 wrapper (all the tests above still pass).

/// Source the script and run `body`; return (exit_code, stdout) WITHOUT asserting success — for the
/// pure functions that intentionally return non-zero (e.g. burn_run_id_for_box on an unknown box).
fn run_sourced_status(body: &str) -> (i32, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The launch program built for --mode test with the given per-box burn run_id.
fn program_test(run_id: &str) -> String {
    run_sourced(&format!(
        "build_launch_program 'C:\\Program Files\\obs-studio' 0 test {run_id}"
    ))
}

/// The launch program built for --mode event.
fn program_event() -> String {
    run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 0 event")
}

/// `burn_run_id_for_box` is the single source of truth for the per-box #111 burn run_id and mirrors
/// the Rust consts src/probe/recording_latency.rs::BURN_RUN_ID_{STRIH,STREAM} (strih 911002 / stream
/// 911004). An unknown box is a HARD error (non-zero) — never a silent wrong/zero run_id in a burn.
#[test]
fn burn_run_id_for_box_maps_strih_and_stream() {
    let strih = run_sourced("burn_run_id_for_box strih");
    assert_eq!(
        strih.trim(),
        "911002",
        "#247: burn_run_id_for_box strih must be 911002 (mirrors BURN_RUN_ID_STRIH)"
    );
    let stream = run_sourced("burn_run_id_for_box stream");
    assert_eq!(
        stream.trim(),
        "911004",
        "#247: burn_run_id_for_box stream must be 911004 (mirrors BURN_RUN_ID_STREAM)"
    );
    let (code, _out) = run_sourced_status("burn_run_id_for_box nope");
    assert_ne!(
        code, 0,
        "#247: burn_run_id_for_box on an unknown box must FAIL (non-zero) — never emit a wrong run_id"
    );
}

/// `burn_var_list` lists EXACTLY the three #111 burn env vars — the set EVENT mode clears + asserts
/// absent. A drift here means the #246 guard checks/clears the wrong set.
#[test]
fn burn_var_list_is_exactly_the_three() {
    let out = run_sourced("burn_var_list");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        got,
        ["OBS_BURN_QR", "OBS_BURN_QR_PX", "OBS_BURN_RUN_ID"],
        "#247: burn_var_list must be exactly the three burn vars — got {got:?}"
    );
}

/// TEST mode sets the burns EXPLICITLY in the LAUNCH SHELL (Set-Item Env:) with OBS_BURN_QR=1 +
/// OBS_BURN_QR_PX=300 + the per-box run_id — and NEVER in Machine scope (a Machine-scope burn
/// survives reboot and paints QR on the LIVE broadcast: the #246 incident). The set must precede
/// Start-Process so the child obs64 inherits it.
#[test]
fn test_mode_sets_burns_in_launch_shell_not_machine() {
    let p = program_test("911002");
    assert!(
        p.contains("Set-Item -Path Env:OBS_BURN_QR     -Value '1'"),
        "#247: TEST mode must set OBS_BURN_QR=1 in the launch shell"
    );
    assert!(
        p.contains("Set-Item -Path Env:OBS_BURN_QR_PX  -Value '300'"),
        "#247: TEST mode must set OBS_BURN_QR_PX=300 in the launch shell"
    );
    assert!(
        p.contains("Set-Item -Path Env:OBS_BURN_RUN_ID -Value '911002'"),
        "#247: TEST mode must set the per-box OBS_BURN_RUN_ID in the launch shell"
    );
    // The burns must NEVER be written to Machine scope (that is the #246 contamination).
    assert!(
        !p.contains("SetEnvironmentVariable('OBS_BURN")
            && !p.contains("setx /M OBS_BURN")
            && !p.contains("'OBS_BURN_QR', '1', 'Machine'"),
        "#246/#247: TEST mode must NEVER write a burn var to Machine scope (it survives reboot)"
    );
    // Set before launch (so obs64 inherits the burn env).
    let set_pos = p
        .find("Set-Item -Path Env:OBS_BURN_QR ")
        .expect("burn set present");
    let launch_pos = p.find("Start-Process").expect("Start-Process present");
    assert!(
        set_pos < launch_pos,
        "#247: the burn env-set must happen BEFORE Start-Process so obs64 inherits it"
    );
}

/// TEST mode's per-box run_id is wired end-to-end through the CLI: --box strih → 911002,
/// --box stream → 911004 in the emitted program.
#[test]
fn cli_test_mode_run_id_per_box() {
    let (code_s, out_s, _) = run_script(&["--box", "strih", "--mode", "test"]);
    assert_eq!(code_s, 0, "strih --mode test plan should print (exit 0)");
    assert!(
        out_s.contains("Set-Item -Path Env:OBS_BURN_RUN_ID -Value '911002'"),
        "#247: --box strih --mode test must wire OBS_BURN_RUN_ID=911002"
    );

    let (code_st, out_st, _) = run_script(&["--box", "stream", "--mode", "test"]);
    assert_eq!(code_st, 0, "stream --mode test plan should print (exit 0)");
    assert!(
        out_st.contains("Set-Item -Path Env:OBS_BURN_RUN_ID -Value '911004'"),
        "#247: --box stream --mode test must wire OBS_BURN_RUN_ID=911004"
    );
}

/// TEST mode also proves the burns in the launched child's PEB (belt-and-suspenders vs a stale
/// launch-shell snapshot) and FOLDS $burnOk into the final verdict.
#[test]
fn test_mode_verifies_burns_in_child_peb_and_verdict() {
    let p = program_test("911002");
    assert!(
        p.contains("^(OBS_BURN_[^=]+)=(.*)$") && p.contains("$wantBurn"),
        "#247: TEST mode must verify the child PEB carries the burn vars"
    );
    assert!(
        p.contains("if ($pebOk -and $tickOk -and $burnOk)"),
        "#247: TEST mode's final verdict must require the burn state too ($burnOk)"
    );
}

/// EVENT mode FAILS LOUD if a burn env would leak: it clears OBS_BURN_* in the launch shell AND
/// asserts the Machine scope carries NONE — refusing to launch (non-zero exit) if any is set. This
/// is the #246 readiness guard (a Machine-scope burn survives reboot → QR on the LIVE broadcast).
#[test]
fn event_mode_clears_burns_and_fails_loud_on_machine_leak() {
    let p = program_event();
    // Clear in the launch shell.
    assert!(
        p.contains("Remove-Item \"Env:$bn\" -ErrorAction SilentlyContinue"),
        "#247: EVENT mode must clear OBS_BURN_* in the launch shell"
    );
    // Assert Machine carries none, fail loud (the #246 guard).
    assert!(
        p.contains("GetEnvironmentVariable($bn, 'Machine')") && p.contains("#246 BURN LEAK"),
        "#246: EVENT mode must assert the Machine scope carries NO burn var (fail loud on leak)"
    );
    assert!(
        p.contains("exit 8"),
        "#246: a Machine burn leak must exit non-zero (refuse to launch) — exit 8"
    );
    // Post-launch PEB proof the child carries none.
    assert!(
        p.contains("carries NO OBS_BURN_*"),
        "#246: EVENT mode must prove the launched obs64 carries NO OBS_BURN_* (post-launch guard)"
    );
    assert!(
        p.contains("if ($pebOk -and $tickOk -and $burnOk)"),
        "#247: EVENT mode's final verdict must require the burn-free state too ($burnOk)"
    );
}

/// The DEFAULT (no --mode) launch is byte-identical in BEHAVIOR to the pre-#247 wrapper: NO burn
/// handling at all, and the original `if ($pebOk -and $tickOk)` verdict (no $burnOk). This proves
/// the #247 feature is purely additive and cannot affect a normal genlock relaunch.
#[test]
fn default_mode_has_no_burn_handling() {
    let p = program_default();
    assert!(
        !p.contains("OBS_BURN"),
        "#247: a default (no --mode) launch must contain NO burn handling — got:\n{p}"
    );
    // The verdict line itself must be the plain pre-#247 form — no `-and $burnOk` folded in.
    // (A doc-comment may mention $burnOk; the gate is the executable verdict, so match the operator.)
    assert!(
        p.contains("if ($pebOk -and $tickOk)") && !p.contains("-and $burnOk"),
        "#247: a default launch must keep the original verdict (no `-and $burnOk`) — pre-#247 behavior"
    );
}

/// CLI: an invalid --mode is a usage error (exit 2); a valid --mode reflects in the plan header.
#[test]
fn cli_mode_flag_validates_and_reflects_in_header() {
    let (code_bad, _, err_bad) = run_script(&["--box", "strih", "--mode", "bogus"]);
    assert_eq!(
        code_bad, 2,
        "#247: an invalid --mode must be a usage error (exit 2)"
    );
    assert!(
        err_bad.contains("must be 'test' or 'event'"),
        "#247: an invalid --mode must error clearly (got stderr: {err_bad:?})"
    );

    let (code_ok, out_ok, _) = run_script(&["--box", "strih", "--mode", "event"]);
    assert_eq!(code_ok, 0, "#247: --mode event plan should print (exit 0)");
    assert!(
        out_ok.contains("EVENT mode"),
        "#247: the plan header must reflect the selected mode"
    );
}
