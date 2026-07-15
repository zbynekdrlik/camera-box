//! #128/#257 — deterministic OBS (re)launch wrapper (scripts/launch-obs-genlock.sh).
//!
//! #257 HARD-LOCKED the genlock build: render tick + ts-align are ALWAYS ON and the latency is a
//! BUILD CONST (3 ms, floor 3) — there is NO OBS_GENLOCK_* env, and the measurement burn is a
//! per-source genlock_burn bool toggled over OBS WebSocket (no relaunch). So the wrapper no longer
//! carries or verifies ANY env (the #128 stale-env trap is structurally gone) and has no --mode.
//! Its job: (force-kill →) clear crash sentinels → Start-Process obs64 cwd=bin\64bit → log-verify
//! the genlock render tick ENABLED + DistroAV loaded, failing LOUD otherwise.
//!
//! These guards source the script (never the executed flow), call its pure `build_launch_program`
//! builder, and assert the emitted PowerShell is well-formed — so a regression (a re-introduced env
//! read, a missing log-verify, a wrong cwd, a non-fail-loud exit) is caught without a Windows host.

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

/// #257: the wrapper must carry NO OBS_GENLOCK_* / OBS_BURN_* env — the genlock build is env-free
/// (render tick + ts-align build defaults, latency build const, burn a per-source WS bool).
#[test]
fn program_carries_no_genlock_or_burn_env() {
    let p = program_default();
    for env in [
        "OBS_GENLOCK_WALL_CLOCK",
        "OBS_GENLOCK_RESERVE_MS",
        "OBS_GENLOCK_TS_ALIGN",
        "OBS_GENLOCK_PRELOAD_FRAMES",
        "OBS_GENLOCK_LATENCY_MS",
        "OBS_BURN_QR",
        "OBS_BURN_RUN_ID",
        "OBS_BURN_QR_PX",
    ] {
        assert!(
            !p.contains(env),
            "#257: the env-free launch wrapper must NOT reference {env} — the genlock build needs \
             no env, and the burn is a per-source WS bool. Program:\n{p}"
        );
    }
    // It must NOT read any Machine-scope env (there is none to carry).
    assert!(
        !p.contains("GetEnvironmentVariable"),
        "#257: the wrapper no longer reads Machine-scope env (the genlock build is env-free)."
    );
}

/// The program clears stale crash sentinels so OBS does not pop the "Crash Detected" modal headless.
#[test]
fn program_clears_crash_sentinels() {
    let p = program_default();
    assert!(
        p.contains(".sentinel"),
        "#128: the relaunch must clear %APPDATA%\\obs-studio\\.sentinel\\* (else OBS hangs on the \
         Crash Detected modal headless). Program:\n{p}"
    );
}

/// The program launches obs64 with cwd = bin\64bit (wrong cwd => broken-locale OBS).
#[test]
fn program_launches_with_bin64_cwd() {
    let p = program_default();
    assert!(
        p.contains("Start-Process") && p.contains("-WorkingDirectory") && p.contains("bin\\64bit"),
        "#128: obs64 must be Start-Process'd with -WorkingDirectory bin\\64bit. Program:\n{p}"
    );
}

/// The verify step asserts the OBS log shows the genlock render tick ENABLED (the #257 build-default
/// proof — the same line drift-guard + the launch verify key on) + a DistroAV-loaded check.
#[test]
fn program_verifies_render_tick_and_distroav_log_lines() {
    let p = program_default();
    assert!(
        p.contains("render tick ENABLED"),
        "#257: the program must verify the OBS log shows 'render tick ENABLED' (genlock build default)."
    );
    assert!(
        p.to_lowercase().contains("distroav"),
        "#257: the program must also check DistroAV loaded. Program:\n{p}"
    );
}

/// #786 — the emitted program carries the AUDIO-BUFFERING LAUNCH-GATE: a bad ASIO launch draw
/// (libobs ratcheting its one-way global audio buffering to the 960 ms max within the first
/// seconds, sticky until restart → whole-session A/V off by ~0.9 s) must be detected from the
/// fresh log and answered with a bounded kill+relaunch redraw, failing LOUD (exit 7) when every
/// attempt draws bad. A wrapper without this gate relaunches OBS blind into the 2026-07-15
/// incident.
#[test]
fn program_gates_on_audio_buffering_and_redraws_786() {
    let p = program_default();
    assert!(
        p.contains("total audio buffering is now (\\d+) milliseconds"),
        "#786: the program must read the fresh log's audio-buffering peak. Program:\n{p}"
    );
    assert!(
        p.contains("Max audio buffering reached"),
        "#786: the program must detect the maxed-out ratchet marker. Program:\n{p}"
    );
    assert!(
        p.contains("exit 7"),
        "#786: exhausting the relaunch attempts must fail LOUD with the distinct exit 7. Program:\n{p}"
    );
}

/// The FINAL verdict gates exit 0 on the render tick being ENABLED (the genlock build proof) and
/// fails LOUD (non-zero exit) otherwise — never a silent stock/wrong-build launch.
#[test]
fn program_final_verdict_is_fail_loud() {
    let p = program_default();
    assert!(
        p.contains("if (\\$tickOk)") || p.contains("if ($tickOk)"),
        "#257: the final verdict must gate on the render tick being ENABLED. Program:\n{p}"
    );
    assert!(
        p.contains("exit 0") && p.contains("exit 1"),
        "#257: success must be exit 0 and the failure path a non-zero exit (fail loud). Program:\n{p}"
    );
}

/// --force inserts a documented force-kill of a wedged obs64; without it the program REFUSES to
/// double-launch a running obs64 (relaunch deliberately).
#[test]
fn force_inserts_kill_and_noforce_refuses_double_launch() {
    let forced = program_force();
    assert!(
        forced.contains("Stop-Process") && forced.contains("obs64"),
        "#128: --force must force-kill a wedged obs64 first. Program:\n{forced}"
    );
    let plain = program_default();
    assert!(
        plain.contains("already running") && plain.contains("exit 3"),
        "#128: without --force the program must refuse to double-launch a running obs64. Program:\n{plain}"
    );
}

/// The CLI selects the correct win-* MCP per box and emits the program in the plan.
#[test]
fn cli_box_selects_correct_mcp_and_emits_program() {
    let (code, out, _err) = run_script(&["--box", "strih"]);
    assert_eq!(code, 0, "--box strih must print the plan (exit 0)");
    assert!(
        out.contains("win-strih") && out.contains("10.77.9.202"),
        "strih -> win-strih plan"
    );
    assert!(
        out.contains("render tick ENABLED"),
        "the plan embeds the launch+verify program"
    );

    let (code, out, _err) = run_script(&["--box", "stream"]);
    assert_eq!(code, 0, "--box stream must print the plan (exit 0)");
    assert!(
        out.contains("win-stream-snv") && out.contains("10.77.9.204"),
        "stream -> win-stream-snv plan"
    );
    // #257: the plan points at the WS burn toggle (obs_burn_filter.py), NOT a --mode relaunch.
    assert!(
        out.contains("obs_burn_filter.py") && !out.contains("--mode"),
        "#257: the plan toggles the burn over WebSocket (obs_burn_filter.py), no --mode relaunch. out=\n{out}"
    );
}

/// A trailing value-taking flag with no value is a clean usage error (exit 2), not a set -e abort.
#[test]
fn trailing_flag_without_value_is_usage_error_exit_2() {
    let (code, _out, err) = run_script(&["--box"]);
    assert_eq!(
        code, 2,
        "--box with no value must exit 2 (usage error). stderr={err}"
    );
    let (code, _out, err) = run_script(&["--box", "strih", "--obs-dir"]);
    assert_eq!(code, 2, "--obs-dir with no value must exit 2. stderr={err}");
}

/// An unknown --box is a usage error (exit 2).
#[test]
fn unknown_box_is_usage_error_exit_2() {
    let (code, _out, err) = run_script(&["--box", "nope"]);
    assert_eq!(
        code, 2,
        "an unknown box must exit 2 (usage error). stderr={err}"
    );
}

/// A single quote in the OBS dir is doubled ('' ) for the PowerShell single-quoted string so it
/// cannot break out of the '...' literal.
#[test]
fn obs_dir_single_quote_is_escaped_in_powershell() {
    let p = run_sourced("build_launch_program \"C:\\\\Te'st\\\\obs-studio\" 0");
    assert!(
        p.contains("C:\\Te''st\\obs-studio"),
        "a single quote in the OBS dir must be doubled for the PowerShell literal. Program:\n{p}"
    );
}

/// Sourcing the script (the unit-test path) must NOT execute the flow (the source-guard returns).
#[test]
fn script_is_source_safe() {
    let out = run_sourced("echo SOURCED_OK");
    assert!(
        out.contains("SOURCED_OK"),
        "sourcing must not run main()/print a plan"
    );
    assert!(
        !out.contains("genlock OBS (re)launch plan"),
        "sourcing must stop at the source-guard, not print the plan. out=\n{out}"
    );
}
