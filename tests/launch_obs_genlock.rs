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
    // Pin the gate's LOGIC constants, not just its strings — a refactor that keeps the literals
    // but loosens the threshold / attempt count / clean-condition must go RED here.
    assert!(
        p.contains("$bufPeak -le 100"),
        "#786: the clean-draw threshold must stay 100 ms (box standard 64/85 ms + headroom) (peak above it = bad draw). Program:\n{p}"
    );
    assert!(
        p.contains("$maxLaunchAttempts = 3"),
        "#786: the redraw budget must stay bounded at 3 attempts. Program:\n{p}"
    );
    assert!(
        p.contains("(-not $bufMaxed) -and"),
        "#786: the clean condition must require BOTH not-maxed AND peak under threshold. Program:\n{p}"
    );
    // The AHK stop-first/restart-last bracket (strih: NL_STARTUP.ahk would respawn a BARE obs64
    // mid-redraw, dropping the shortcut params — the interkom "Permissions denied" failure).
    assert!(
        p.contains("Stop-Process -Name AutoHotkey64") && p.contains("$ahkStopped"),
        "#786: the redraw must stop AHK first and restart it after the loop. Program:\n{p}"
    );
    // On a box whose shortcut IS the on-box guarded launcher (obs-guarded-launch.ps1), the wrapper
    // must DELEGATE — verify-only, never a second concurrent redraw loop (double kill would race
    // the on-box guard into two obs64 instances).
    assert!(
        p.contains("obs-guarded-launch") && p.contains("$guardedLnk"),
        "#786: a guarded shortcut must switch the wrapper to verify-only delegation. Program:\n{p}"
    );
}

/// #786/#411 — a box with NO AutoHotkey64 watcher (stream; only strih runs NL_STARTUP.ahk) must get
/// a program WITHOUT any real AutoHotkey64 command: build_launch_program's third arg (has_ahk=0)
/// swaps the AHK stop/restart bracket for documented no-ops. Without this, the #411 self-heal
/// script (which embeds this program verbatim) carried a real `Stop-Process -Name AutoHotkey64`
/// onto stream — the exact thing its stream guard test forbids.
#[test]
fn program_without_ahk_watcher_carries_no_ahk_commands_786() {
    let p = run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 0 0");
    assert!(
        !p.contains("Stop-Process -Name AutoHotkey64"),
        "has_ahk=0 must not emit a real AutoHotkey64 stop command. Program:\n{p}"
    );
    assert!(
        p.contains("no-op") && p.contains("AutoHotkey64"),
        "has_ahk=0 must DOCUMENT the AHK bracket as a no-op, not silently vanish. Program:\n{p}"
    );
    // The redraw gate itself is unchanged — only the AHK bracket is swapped.
    assert!(
        p.contains("$bufPeak -le 100") && p.contains("exit 7"),
        "the #786 audio gate must survive has_ahk=0 untouched. Program:\n{p}"
    );
    // #867: a box with no AHK watcher must not embed the resolve+relaunch machinery at all.
    assert!(
        !p.contains("$ahkScriptPath"),
        "has_ahk=0 must not embed the #867 ahk_resolve_and_relaunch_ps machinery. Program:\n{p}"
    );
    // Default (2-arg) stays the strih behavior — the AHK bracket present (pinned above in
    // program_gates_on_audio_buffering_and_redraws_786).
}

/// #867: the strih AHK restart must NEVER rely on a bare `-FilePath 'AutoHotkey64.exe'` launch —
/// AutoHotkey v2 is installed user-scoped under `%LOCALAPPDATA%\Programs\AutoHotkey\v2\` and is
/// NOT on PATH (confirmed live on strih), so a bare exe-name launch can never resolve. The comment
/// on #867 root-caused a real outage to exactly this shape in a sibling script.
#[test]
fn program_never_launches_ahk_by_bare_exe_name_867() {
    let p = program_default();
    assert!(
        !p.contains("-FilePath 'AutoHotkey64.exe'")
            && !p.contains("-FilePath \"AutoHotkey64.exe\""),
        "#867: must never launch AutoHotkey64 by a bare exe name relying on PATH. Program:\n{p}"
    );
    // The robust resolve probes the user-scoped install locations, in order, before falling back
    // to the Startup shortcut / PATH.
    assert!(
        p.contains("LOCALAPPDATA") && p.contains("AutoHotkey\\v2\\AutoHotkey64.exe"),
        "#867: must probe the user-scoped %LOCALAPPDATA%\\Programs\\AutoHotkey\\v2\\ install \
         location. Program:\n{p}"
    );
}

/// #867: the restart must be VERIFIED (poll `Get-Process AutoHotkey64`), and a failed relaunch
/// must fail LOUD (Write-Error + non-zero exit) — never a blind unconditional success claim, which
/// is exactly what let strih run for hours with no live respawn watcher.
#[test]
fn program_ahk_restart_is_verified_and_fails_loud_867() {
    let p = program_default();
    assert!(
        p.contains("Get-Process AutoHotkey64") && p.contains("$ahkRelaunchVerified"),
        "#867: the AHK restart must poll Get-Process AutoHotkey64 and record whether it verified. \
         Program:\n{p}"
    );
    // #1272: ahk_relaunch_ps (which embeds "$ahkRelaunchVerified = $false") is now ALSO reused by
    // the two early-exit best-effort restart snippets (exe-not-found / obs64-never-started), so
    // this string is no longer unique in the program — anchor on the LAST occurrence, which is
    // guaranteed to be the real verify+fail-loud restart block (positioned after the whole
    // launch+audio-verify sequence, strictly later in the text than either early-exit site).
    let verified_pos = p
        .rfind("$ahkRelaunchVerified = $false")
        .expect("the verify flag must be initialized before the poll loop");
    let fail_branch = p[verified_pos..]
        .find("Write-Error")
        .map(|off| off + verified_pos)
        .expect("a Write-Error failure branch must exist after the verify flag");
    let exit_after_fail = p[fail_branch..]
        .find("exit 9")
        .expect("the failure branch must exit non-zero (9), not just warn");
    let _ = exit_after_fail;
    // The real restart block's own success-log marker must sit between the LAST verify-flag
    // occurrence and the fail branch, proving verified_pos genuinely anchored on the intended
    // (real restart) occurrence, not merely on the last of several unrelated ones by coincidence.
    let success_marker_pos = p[verified_pos..]
        .find("AHK watchdog restarted via")
        .map(|off| off + verified_pos)
        .expect(
            "the real restart block's success log line must follow the LAST verify-flag occurrence",
        );
    assert!(
        success_marker_pos < fail_branch,
        "#1272: the real restart block's success branch must precede its own fail branch \
         (verified_pos={verified_pos} success_marker_pos={success_marker_pos} fail_branch={fail_branch})"
    );
}

/// #978/#958 — SESSION-VISIBILITY GATE: an obs64 launched via ssh+Invoke-CimMethod lands in
/// Windows SessionId=0 (invisible on the console) yet passes every OTHER check in this program
/// (log render tick, audio buffering). The verify must re-query FRESH and fail LOUD (distinct
/// exit 8) unless exactly one obs64 has SessionId == the ACTIVE interactive session (derived from
/// explorer.exe, never hardcoded); on strih (has_ahk=1) it must ALSO assert AutoHotkey64 is in
/// that same active session (a session-0 AHK re-spawns obs64 into session 0 forever).
#[test]
fn program_gates_on_session_visibility_978() {
    let p = program_default(); // has_ahk=1 (strih)
    assert!(
        p.contains("$sessObsProcs.Count -ne 1"),
        "#978: must assert exactly one obs64 process. Program:\n{p}"
    );
    assert!(
        p.contains("$sessProc.SessionId -ne $activeSession"),
        "#978: must assert obs64's SessionId == the active interactive session. Program:\n{p}"
    );
    assert!(
        p.contains("MainWindowTitle"),
        "#978: must assert obs64 has a non-empty MainWindowTitle (same-session path). Program:\n{p}"
    );
    assert!(
        p.contains("exit 8"),
        "#978: the session-visibility gate must fail with a distinct exit code (8). Program:\n{p}"
    );
    assert!(
        p.contains("$ahkSessProcs") && p.contains("AutoHotkey64"),
        "#978: strih (has_ahk=1) must ALSO gate AutoHotkey64's SessionId. Program:\n{p}"
    );
    assert!(
        p.contains("$ahkSessProcs[0].SessionId -ne $activeSession"),
        "#978: the strih AHK check must assert SessionId == the active interactive session. Program:\n{p}"
    );
}

/// #978/#958 follow-up — the ACTIVE session must be DERIVED from explorer.exe, never hardcoded,
/// and an absent explorer.exe (no interactive desktop session at all) must fail loud rather than
/// silently comparing against an assumed session id.
#[test]
fn program_derives_active_session_from_explorer_never_hardcoded_958() {
    let p = program_default();
    assert!(
        p.contains("Get-Process explorer"),
        "#958: must derive the active session from explorer.exe. Program:\n{p}"
    );
    assert!(
        p.contains("$activeSession = $activeSessProcs[0].SessionId"),
        "#958: must assign the derived active session to $activeSession. Program:\n{p}"
    );
    assert!(
        p.contains("$activeSessProcs.Count -lt 1"),
        "#958: must fail loud when no explorer.exe process exists (no interactive desktop \
         session). Program:\n{p}"
    );
    assert!(
        !p.contains("SessionId -ne 1") && !p.contains("SessionId -eq 1"),
        "#958: no session comparison may hardcode the literal 1 any more -- everything must \
         compare against the derived $activeSession. Program:\n{p}"
    );
}

/// #978/#958 follow-up — the MainWindowTitle assertion is CONTEXT-GATED: this program's own
/// PowerShell session's SessionId ($PID-derived) is compared against obs64's before the title
/// check is enforced, so the #859 no-MCP CIM-breakaway ssh fallback (a DIFFERENT session from
/// obs64's) can't false-fail on a title Windows makes structurally unreadable cross-session.
#[test]
fn program_context_gates_the_title_check_on_own_vs_target_session_958() {
    let p = program_default();
    assert!(
        p.contains("$ownSession = (Get-Process -Id $PID).SessionId"),
        "#958: must derive this program's own running session via $PID. Program:\n{p}"
    );
    assert!(
        p.contains("$ownSession -eq $sessProc.SessionId"),
        "#958: the title check must be gated on own-session == target-session. Program:\n{p}"
    );
    assert!(
        p.to_lowercase().contains("title check skipped"),
        "#958: the cross-session branch must document that the title check is SKIPPED, not \
         silently omitted. Program:\n{p}"
    );
}

/// #978 — a box with NO AutoHotkey64 watcher (stream) must NOT carry a real AutoHotkey64
/// session-visibility check, only the obs64 one (mirrors the #786/#411 has_ahk=0 convention).
#[test]
fn program_without_ahk_watcher_session_gate_has_no_real_ahk_check_978() {
    let p = run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 0 0");
    assert!(
        p.contains("$sessObsProcs.Count -ne 1")
            && p.contains("$sessProc.SessionId -ne $activeSession"),
        "#978: the obs64 session check must survive has_ahk=0 untouched. Program:\n{p}"
    );
    assert!(
        !p.contains("$ahkSessProcs"),
        "#978: has_ahk=0 must not embed the real AutoHotkey64 session-visibility check. Program:\n{p}"
    );
    assert!(
        p.to_lowercase().contains("no ahk auto-respawn watcher"),
        "#978: has_ahk=0 must DOCUMENT the AHK session check as a no-op, not silently vanish. Program:\n{p}"
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

/// (#1057) The launch PLAN must include a MANDATORY dev1-side verify-at-start burn sweep-off step.
///
/// strih/stream have no on-box python/OBS-WebSocket client and `obs_burn_filter.py` is not deployed
/// there, so a saved `genlock_burn=true` reloaded at OBS start renders the QR burn onto the LIVE
/// program until the next gate run's `[0/8]` sweep. The burn toggle/sweep is a dev1-side WS
/// operation (`obs_burn_filter.py ... --host <ip>`, session-agnostic, per win-ssh-vs-mcp), so the
/// plan (printed by dev1) must direct a post-launch `sweep-off --host <box_ip>` that forces every
/// ndi_source input's burn OFF and reports LOUDLY -- positioned AFTER the on-box launch verify.
#[test]
fn plan_emits_verify_at_start_burn_sweep_off_1057() {
    for (box_arg, ip) in [("strih", "10.77.9.202"), ("stream", "10.77.9.204")] {
        let (code, out, _err) = run_script(&["--box", box_arg]);
        assert_eq!(code, 0, "--box {box_arg} must print the plan (exit 0)");
        assert!(
            out.contains("obs_burn_filter.py sweep-off"),
            "#1057: the {box_arg} plan must direct a dev1-side `obs_burn_filter.py sweep-off` \
             verify-at-start (force burns OFF at OBS start). plan=\n{out}"
        );
        let sweep_line = out
            .lines()
            .find(|l| l.contains("obs_burn_filter.py sweep-off"))
            .unwrap_or("");
        assert!(
            sweep_line.contains(&format!("--host {ip}")),
            "#1057: the {box_arg} sweep-off step must target --host {ip}. line=\n{sweep_line}"
        );
        // The verify-at-start burn sweep runs AFTER the on-box launch-verify STEP (never before OBS
        // is up) -- anchor on the STEP 2 marker, not the in-program "render tick ENABLED" text.
        let verify_step_pos = out
            .find("STEP 2")
            .expect("plan has the STEP 2 launch-verify marker");
        let sweep_pos = out.find("obs_burn_filter.py sweep-off").unwrap();
        assert!(
            verify_step_pos < sweep_pos,
            "#1057: the burn sweep-off verify-at-start must come AFTER the STEP 2 launch verify. plan=\n{out}"
        );
    }
}

/// (#1061) The launch PLAN must include a dev1-side latency-pin verify-at-start step.
///
/// Issue 866's latency half: per-source `genlock_latency_ms_src` persists to the scene collection
/// and RELOADS at OBS start (the #866/#707 unjustified restart revert). Unlike the #1057 burn
/// (forced OFF), per-source latency is the operator's A/V-align domain, so the start path may only
/// REPORT drift against the committed agreed-pins baseline (scripts/latency-pins-baseline.json),
/// NEVER overwrite. The plan (printed by dev1) must direct a post-launch
/// `latency_pins_verify.py --box <box> --host <ip>` -- REPORT-ONLY, positioned AFTER the STEP 2
/// launch verify.
#[test]
fn plan_emits_verify_at_start_latency_pins_1061() {
    for (box_arg, ip) in [("strih", "10.77.9.202"), ("stream", "10.77.9.204")] {
        let (code, out, _err) = run_script(&["--box", box_arg]);
        assert_eq!(code, 0, "--box {box_arg} must print the plan (exit 0)");
        assert!(
            out.contains("latency_pins_verify.py"),
            "#1061: the {box_arg} plan must direct a dev1-side `latency_pins_verify.py` \
             latency-pin verify-at-start. plan=\n{out}"
        );
        let lat_line = out
            .lines()
            .find(|l| l.contains("latency_pins_verify.py"))
            .unwrap_or("");
        assert!(
            lat_line.contains(&format!("--box {box_arg}")),
            "#1061: the {box_arg} latency-verify step must pass --box {box_arg}. line=\n{lat_line}"
        );
        assert!(
            lat_line.contains(&format!("--host {ip}")),
            "#1061: the {box_arg} latency-verify step must target --host {ip}. line=\n{lat_line}"
        );
        // REPORT-ONLY: per-source latency is the operator's A/V-align domain -- the plan must make
        // clear the step never overwrites (the opposite of the #1057 burn sweep-off).
        assert!(
            out.contains("REPORT-ONLY"),
            "#1061: the latency verify-at-start step must be worded REPORT-ONLY (never overwrite). plan=\n{out}"
        );
        // Positioned AFTER the on-box STEP 2 launch verify (never before OBS is up).
        let verify_step_pos = out
            .find("STEP 2")
            .expect("plan has the STEP 2 launch-verify marker");
        let lat_pos = out.find("latency_pins_verify.py").unwrap();
        assert!(
            verify_step_pos < lat_pos,
            "#1061: the latency verify-at-start must come AFTER the STEP 2 launch verify. plan=\n{out}"
        );
    }
}

/// #775 — OBS is (re)launched via the box's Start-Menu SHORTCUT (`OBS Studio.lnk`) as the PRIMARY
/// path; the bare exe is ONLY the guarded fallback (behind `if (Test-Path $lnk)` … `else`, with a
/// LOUD warning that the box-specific params are missing). The e496d4aab fix landed this behavior
/// but NOTHING pinned it — `program_launches_with_bin64_cwd` above asserts only the bare-exe
/// fallback, so a refactor back to a bare-exe-primary launch would pass CI silently and re-break
/// strih's interkom (`--enable-media-stream` dropped → VDO.ninja "Permissions denied").
#[test]
fn program_launches_lnk_as_primary_bare_exe_only_as_fallback_775() {
    let p = program_default();
    // The shortcut variable points at the ProgramData "OBS Studio.lnk".
    assert!(
        p.contains("OBS Studio.lnk"),
        "#775: the launch must reference the box's 'OBS Studio.lnk' shortcut. Program:\n{p}"
    );
    let lnk_pos = p
        .find("Start-Process -FilePath $lnk")
        .expect("#775: OBS must be launched via `Start-Process -FilePath $lnk` (the shortcut)");
    let bare_pos = p
        .find("Start-Process -FilePath $exe -WorkingDirectory")
        .expect("#775: the bare-exe fallback launch must still exist");
    // The shortcut launch is FIRST (primary); the bare exe comes later (fallback).
    assert!(
        lnk_pos < bare_pos,
        "#775: the .lnk launch must be the PRIMARY path, BEFORE the bare-exe fallback — not the \
         other way round. Program:\n{p}"
    );
    // The primary .lnk launch is gated on the shortcut existing.
    let gate_pos = p
        .find("if (Test-Path $lnk)")
        .expect("#775: the .lnk launch must be gated on `if (Test-Path $lnk)`");
    assert!(
        gate_pos < lnk_pos,
        "#775: the `if (Test-Path $lnk)` gate must precede the .lnk launch. Program:\n{p}"
    );
    // The bare-exe fallback carries a LOUD warning that the box-specific params are missing, and it
    // sits between the .lnk launch and … well, it IS the fallback branch after the warning.
    let warn_pos = p
        .find("shortcut params will be MISSING")
        .expect("#775: the bare-exe fallback must warn that box-specific params are MISSING");
    assert!(
        lnk_pos < warn_pos && warn_pos < bare_pos,
        "#775: the 'params MISSING' warning must sit in the fallback branch, between the primary \
         .lnk launch and the bare-exe launch. Program:\n{p}"
    );
}

/// #775 — the #786 redraw/relaunch loop must ALSO prefer the .lnk (a bad-ASIO redraw that dropped
/// back to a bare exe would re-break the interkom on every recovery). Both the initial launch and
/// the redraw relaunch go `Start-Process -FilePath $lnk`.
#[test]
fn redraw_relaunch_also_prefers_lnk_775() {
    let p = program_default();
    let lnk_launches = p.matches("Start-Process -FilePath $lnk").count();
    assert!(
        lnk_launches >= 2,
        "#775: both the initial launch AND the #786 redraw relaunch must use `Start-Process \
         -FilePath $lnk` (found {lnk_launches}). Program:\n{p}"
    );
    // Pin the redraw branch's OWN ordering too: its .lnk relaunch (the LAST $lnk launch) must
    // precede its bare-exe fallback (the LAST bare-exe launch) — so a refactor that inverted ONLY
    // the redraw branch to bare-exe-primary cannot slip through on the count alone.
    let redraw_lnk = p
        .rfind("Start-Process -FilePath $lnk")
        .expect("#775: a redraw .lnk relaunch must exist");
    let redraw_bare = p
        .rfind("Start-Process -FilePath $exe -WorkingDirectory")
        .expect("#775: a redraw bare-exe fallback must exist");
    assert!(
        redraw_lnk < redraw_bare,
        "#775: in the redraw branch too, the .lnk relaunch must be PRIMARY (precede the bare-exe \
         fallback). Program:\n{p}"
    );
}

/// #1272 — `--force` on strih (has_ahk=1): the fleet-deploy-restored AHK watchdog must be stopped
/// BEFORE the force-kill's own obs64 kill (else AHK respawns a duplicate obs64 within seconds of
/// the kill, landing #978's "expected exactly 1 obs64 process" fail — the live 2026-09-02 incident)
/// and restarted only AFTER the launch+audio-verify sequence completes, before the (3c) session
/// gate (which itself asserts AHK's own SessionId and would otherwise find it missing).
#[test]
fn force_stops_ahk_before_obs64_kill_and_restarts_after_verify_1272() {
    let p = program_force(); // has_ahk=1 default (strih)
    let ahk_stop_pos = p
        .find("Stop-Process -Name AutoHotkey64 -Force")
        .expect("#1272: --force must stop AHK before killing obs64. Program follows.");
    let obs64_kill_pos = p
        .find("Get-Process obs64 -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.Id -Force }")
        .expect("#1272: the --force obs64 kill line must exist");
    assert!(
        ahk_stop_pos < obs64_kill_pos,
        "#1272: AHK must be stopped BEFORE the --force obs64 kill (else the fleet-deploy-restored \
         AHK respawns a duplicate obs64 mid-kill -> #978 fail). ahk_stop_pos={ahk_stop_pos} \
         obs64_kill_pos={obs64_kill_pos}. Program:\n{p}"
    );

    let ahk_relaunch_pos = p
        .find("AHK watchdog restarted via")
        .expect("#1272: the AHK relaunch confirmation log line must exist");
    let session_gate_pos = p
        .find("# (3c) #978 SESSION-VISIBILITY GATE")
        .expect("the (3c) session-visibility gate comment must exist");
    assert!(
        obs64_kill_pos < ahk_relaunch_pos,
        "#1272: the AHK relaunch must come AFTER the obs64 kill+launch, not before it. \
         obs64_kill_pos={obs64_kill_pos} ahk_relaunch_pos={ahk_relaunch_pos}. Program:\n{p}"
    );
    assert!(
        ahk_relaunch_pos < session_gate_pos,
        "#1272: AHK must be restarted BEFORE the (3c) session-visibility gate (which asserts AHK's \
         own SessionId and would otherwise find it missing). ahk_relaunch_pos={ahk_relaunch_pos} \
         session_gate_pos={session_gate_pos}. Program:\n{p}"
    );
}

/// #1272 (review finding) — the fix moved the AHK stop from "only inside the #786 redraw loop" to
/// "unconditionally before the --force kill", which widened the window where a program that exits
/// EARLY (before the single shared restart point is reached) would leave AHK permanently stopped —
/// a regression these two specific exit sites never had before this fix (exe-not-found / obs64
/// never starting on the INITIAL launch). Both must attempt a best-effort AHK restart before
/// exiting, without stealing the exit 5 / exit 6 codes those failures must still report.
#[test]
fn early_exit_failures_attempt_best_effort_ahk_restart_before_exiting_1272() {
    let p = program_default(); // has_ahk=1, force=0 (the initial-launch path both early exits sit on)
    let exit5_pos = p
        .find("obs64 not found at")
        .expect("the exe-not-found exit 5 message must exist");
    let exit6_pos = p.find("obs64 did not start\"").expect(
        "the obs64-never-started exit 6 message must exist (first occurrence, initial launch)",
    );

    // A best-effort restart attempt must sit IMMEDIATELY before each early-exit message (within a
    // tight local window, never merely "somewhere earlier in the file").
    let before_exit5 = &p[exit5_pos.saturating_sub(400)..exit5_pos];
    assert!(
        before_exit5.contains("best-effort AHK restart"),
        "#1272: the exe-not-found exit 5 must attempt a best-effort AHK restart first (else the \
         --force AHK-stop this fix added leaves AHK dead on a failure that never touched it \
         before). Region:\n{before_exit5}"
    );
    let before_exit6 = &p[exit6_pos.saturating_sub(400)..exit6_pos];
    assert!(
        before_exit6.contains("best-effort AHK restart"),
        "#1272: the initial obs64-never-started exit 6 must attempt a best-effort AHK restart \
         first. Region:\n{before_exit6}"
    );

    // The primary exit code must still be the ONE reported — the best-effort attempt must be
    // silent about verification outcome (Write-Warning only), never fail-loud/exit itself (that
    // would steal the real exit 5/6 codes this failure must report).
    assert!(
        !p.contains("best-effort AHK restart") || !before_exit5.contains("exit 9"),
        "#1272: the best-effort restart before exit 5 must never itself exit — exit 5 stays the \
         reported code. Region:\n{before_exit5}"
    );
    assert!(
        !before_exit6.contains("exit 9"),
        "#1272: the best-effort restart before exit 6 must never itself exit — exit 6 stays the \
         reported code. Region:\n{before_exit6}"
    );
}

/// #1272 — a box with NO AutoHotkey64 watcher (stream) must NOT carry a real best-effort AHK
/// restart at the early-exit sites either — same #786/#411 has_ahk=0 no-op convention.
#[test]
fn program_without_ahk_watcher_early_exits_have_no_real_best_effort_restart_1272() {
    let p = run_sourced("build_launch_program 'C:\\Program Files\\obs-studio' 0 0");
    assert!(
        p.contains("AutoHotkey64 best-effort restart: no-op"),
        "has_ahk=0 must document the best-effort restart snippet as a no-op. Program:\n{p}"
    );
    assert!(
        !p.contains("$ahkRelaunchVerified"),
        "has_ahk=0 must not embed any real AHK relaunch machinery, including at the early-exit \
         best-effort sites. Program:\n{p}"
    );
}

/// #1272 — the AHK stop/restart must happen exactly ONCE per launch (not once around the initial
/// force-kill AND again around a #786 redraw) — a second `$ahkStopped = $false` declaration would
/// silently wipe out the force-kill's own stop flag before the single restart point runs, and a
/// second restart point risks two AHK relaunch attempts.
#[test]
fn ahk_stopped_declared_once_and_restart_emitted_once_1272() {
    let p = program_force();
    assert_eq!(
        p.matches("$ahkStopped = $false").count(),
        1,
        "#1272: $ahkStopped must be declared exactly ONCE (a second declaration right before the \
         #786 redraw loop would silently reset it to false after the force-kill's own stop). \
         Program:\n{p}"
    );
    assert_eq!(
        p.matches("AHK watchdog restarted via").count(),
        1,
        "#1272: the AHK relaunch-confirmation line must be emitted exactly ONCE (one restart point \
         covering both the plain force-kill launch and any #786 redraw), not duplicated. Program:\n{p}"
    );
}

/// #775 (item 2a) — the #411 self-heal recovery relaunch must inherit the SAME .lnk-primary launch
/// program, never re-derive its own. It does so by reusing `launch-obs-genlock.sh`'s
/// `build_launch_program` verbatim — pin that reuse so a self-heal refactor can't fork the launch
/// path (and thereby a bare-exe respawn) behind the wrapper's back.
#[test]
fn self_heal_reuses_wrapper_launch_program_775() {
    let self_heal =
        std::fs::read_to_string(manifest_dir().join("scripts/obs-self-heal-install.sh"))
            .expect("read obs-self-heal-install.sh");
    assert!(
        self_heal.contains("build_launch_program"),
        "#775: obs-self-heal-install.sh must reuse launch-obs-genlock.sh's build_launch_program \
         (so its recovery relaunch inherits the .lnk-primary contract), never re-derive its own launch."
    );
}
