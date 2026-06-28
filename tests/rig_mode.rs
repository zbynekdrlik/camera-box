//! Behavioral guard for the deterministic rig TEST/EVENT-mode switch `scripts/rig-mode.sh` (#247).
//!
//! ## Why this script exists (#247 — the #246 live-event disaster)
//!
//! Switching the rig between TEST mode (QR/E2E measurement) and EVENT mode (clean prod broadcast)
//! used to be AD-HOC — which QR, what size, capture settings, burns on/off, genlock config all
//! depended on the operator's/agent's context. That left burns ON in the prod Machine env during a
//! LIVE event (QR painted on the broadcast) and genlock in a test state. `rig-mode.sh` is the SINGLE
//! SOURCE OF TRUTH: identical PINNED settings every time, no improvisation.
//!
//! The CAM side is automated over ssh (ssh to the cam boxes is ALLOWED); the Windows OBS side is
//! PRINTED as the exact `launch-obs-genlock.sh --mode {test|event}` step (ssh to Windows is denied —
//! the agent drives the win-* MCP). Same PURE-PLANNER model as tests/launch_obs_genlock.rs: these
//! tests source the REAL script (its `BASH_SOURCE != $0` guard skips main), call its pure remote-
//! command builders, and assert the PINNED painter flags + the safety properties (free /dev/fb0,
//! fail loud on a missing binary, the PID-file stop that avoids the `pkill -f` self-match footgun,
//! the camera-box `--display` restore). The invalid-mode contract is checked end-to-end (it exits
//! before any ssh). NO test runs `test`/`event` end-to-end — that would ssh the live rig.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/rig-mode.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script (BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout. Asserts
/// the harness itself exited 0 (the pure builders never fail).
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

/// Source the script + run `body`, returning (exit_code, stdout) WITHOUT asserting success — for
/// pure functions that intentionally return non-zero (e.g. burn_action_for_mode on an unknown mode).
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

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run rig-mode.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The TEST-mode remote bash for cam2 (pinned painter @ 700px, 7200s, default binary/pidfile).
fn painter_launch() -> String {
    run_sourced("painter_launch_remote /usr/local/bin/frame-probe 7200 700 /run/rig-painter.pid ''")
}

/// The EVENT-mode remote bash for cam2 (stop painter via pidfile, restart camera-box, verify).
fn painter_stop() -> String {
    run_sourced("painter_stop_remote /run/rig-painter.pid")
}

/// An invalid or missing mode MUST be a CLEAN usage error (exit 2) — and crucially it must fail
/// BEFORE any ssh to the rig (the validation is the first thing main does).
#[test]
fn invalid_or_missing_mode_is_usage_error_exit_2() {
    let (code_bad, _out, err) = run_script(&["bogus"]);
    assert_eq!(
        code_bad, 2,
        "#247: an invalid mode must exit 2 (usage error)"
    );
    assert!(
        err.contains("mode must be 'test' or 'event'"),
        "#247: an invalid mode must print a clear error (got stderr: {err:?})"
    );

    let (code_none, _, _) = run_script(&[]);
    assert_eq!(
        code_none, 2,
        "#247: a missing mode must exit 2 (usage error)"
    );
}

/// `--help` prints usage and exits 0 (never tries to touch the rig).
#[test]
fn help_exits_zero() {
    let (code, out, _) = run_script(&["--help"]);
    assert_eq!(code, 0, "#247: --help must exit 0");
    assert!(
        out.contains("rig-mode.sh") && out.contains("test") && out.contains("event"),
        "#247: --help must document both modes"
    );
}

/// TEST mode launches the EXACT pinned dual-QR vernier painter — `--paint-only --dual-qr
/// --qr-size 700 --duration-secs <N>` — no improvisation (#247: the switch is deterministic, the
/// 700px vernier is the validated size). The duration + binary are the resolved values.
#[test]
fn test_mode_launches_pinned_painter() {
    let p = painter_launch();
    assert!(
        p.contains(
            "/usr/local/bin/frame-probe --paint-only --dual-qr --qr-size 700 --duration-secs 7200"
        ),
        "#247: TEST mode must launch the PINNED painter flags (--paint-only --dual-qr --qr-size 700 \
         --duration-secs N) — got:\n{p}"
    );
}

/// #291: TEST mode frees /dev/fb0 WITHOUT a full `systemctl stop camera-box` (which killed cam2's
/// capture+emit too). It installs a TRANSIENT systemd drop-in that overrides ExecStart to run
/// camera-box WITHOUT `--display`, reloads + restarts, then WAITS until fb0 is actually free
/// (fbdev teardown is async), failing loud if it stays held. Display output is the ONLY thing that
/// grabs fb0; capture (/dev/video0) + NDI emit do not.
#[test]
fn test_mode_frees_fb0_via_no_display_dropin_not_full_stop() {
    let p = painter_launch();
    // The #291 bug: TEST mode must NOT stop the WHOLE service (that kills capture+emit, dropping
    // cam2 as a measurable camera). The only acceptable "stop" is the display output.
    assert!(
        !p.contains("systemctl stop camera-box"),
        "#291: TEST mode must NOT fully stop camera-box — that kills cam2's capture+emit. \
         Switch it to no-display instead. Got:\n{p}"
    );
    // It installs a transient drop-in (in /run — tmpfs, so a reboot auto-reverts) overriding
    // ExecStart to camera-box WITHOUT --display, then reloads + restarts to apply it.
    assert!(
        p.contains("camera-box.service.d") && p.contains("zz-rig-test-no-display.conf"),
        "#291: TEST mode must install a transient no-display systemd drop-in. Got:\n{p}"
    );
    assert!(
        p.contains("ExecStart=") && p.contains("ExecStart=/usr/local/bin/camera-box"),
        "#291: the drop-in must reset ExecStart and set the no-display camera-box command. Got:\n{p}"
    );
    assert!(
        p.contains("systemctl daemon-reload") && p.contains("systemctl restart camera-box"),
        "#291: TEST mode must daemon-reload + restart to apply the no-display drop-in. Got:\n{p}"
    );
    assert!(
        p.contains("fuser -s /dev/fb0"),
        "#247: TEST mode must WAIT until /dev/fb0 is free (fbdev teardown is async)"
    );
    assert!(
        p.contains("/dev/fb0 still held"),
        "#247: TEST mode must FAIL LOUD if /dev/fb0 stays held after releasing the display"
    );
}

/// #291 headline: TEST mode KEEPS cam2 capturing + emitting NDI while the painter owns /dev/fb0 —
/// the whole point of the fix. After switching to no-display it verifies the service is STILL active
/// (capture+emit alive) and that the effective ExecStart no longer carries `--display` (so the
/// unit's Restart=always can never respawn a process that re-grabs fb0).
#[test]
fn test_mode_keeps_camera_box_capturing_and_emitting() {
    let p = painter_launch();
    assert!(
        p.contains("is-active camera-box"),
        "#291: TEST mode must verify camera-box is STILL active (capture+emit must keep running). \
         Got:\n{p}"
    );
    assert!(
        p.contains("systemctl show -p ExecStart") && p.contains("--display"),
        "#291: TEST mode must verify the effective ExecStart no longer has --display (so a \
         Restart=always respawn cannot re-grab fb0). Got:\n{p}"
    );
}

/// TEST mode records the painter PID to a PID FILE — that is what lets EVENT mode stop it cleanly
/// without the `pkill -f frame-probe` self-match footgun.
#[test]
fn test_mode_records_painter_pidfile() {
    let p = painter_launch();
    assert!(
        p.contains("> \"/run/rig-painter.pid\""),
        "#247: TEST mode must write the painter PID to the pidfile (for a clean event-mode stop)"
    );
    assert!(
        p.contains("kill -0") && p.contains("painting /dev/fb0"),
        "#247: TEST mode must verify the painter is UP and actually writing /dev/fb0"
    );
}

/// If the painter binary is absent, TEST mode FAILS LOUD telling the operator to deploy the CI
/// probe-tools-linux-amd64 artifact (never a silent no-op that leaves the monitor blank).
#[test]
fn test_mode_fails_loud_when_painter_binary_absent() {
    let p = painter_launch();
    assert!(
        p.contains("[ ! -x \"/usr/local/bin/frame-probe\" ]"),
        "#247: TEST mode must check the painter binary exists before launching"
    );
    assert!(
        p.contains("probe-tools-linux-amd64"),
        "#247: a missing painter binary must tell the operator to deploy the CI \
         probe-tools-linux-amd64 artifact"
    );
}

/// EVENT mode stops the painter via its PID FILE and ALSO `pkill -x frame-probe` (exact NAME match —
/// can never self-match the remote shell's cmdline), then RESTORES the deployed --display camera-box
/// and VERIFIES the service is active AND `--display` is restored (the interkom monitor is back).
/// #291: TEST mode no longer STOPS camera-box (it switches it to no-display via a drop-in), so EVENT
/// mode must REMOVE that drop-in and RESTART (not just `start`) to revert ExecStart to --display.
#[test]
fn event_mode_stops_via_pidfile_restores_display() {
    let p = painter_stop();
    assert!(
        p.contains("/run/rig-painter.pid") && p.contains("kill \"$PID\""),
        "#247: EVENT mode must stop the painter via its PID file (the self-match-safe path)"
    );
    assert!(
        p.contains("pkill -x frame-probe"),
        "#247: EVENT mode's belt-and-suspenders kill must be `pkill -x` (exact name, never self-match)"
    );
    // #291: remove the transient no-display drop-in TEST mode installed, then RESTART (the unit was
    // never fully stopped — it was reconfigured to no-display — so a plain `start` would not revert).
    assert!(
        p.contains("zz-rig-test-no-display.conf") && p.contains("rm -f"),
        "#291: EVENT mode must remove the transient no-display drop-in installed by TEST mode. \
         Got:\n{p}"
    );
    assert!(
        p.contains("systemctl daemon-reload") && p.contains("systemctl restart camera-box"),
        "#291: EVENT mode must daemon-reload + RESTART to revert ExecStart back to --display"
    );
    assert!(
        p.contains("is-active camera-box") && p.contains("not active after start"),
        "#247: EVENT mode must verify the camera-box service is active (fail loud otherwise)"
    );
    assert!(
        p.contains("grep -q -- '--display'") && p.contains("no --display"),
        "#247: EVENT mode must verify --display is restored (the interkom monitor path)"
    );
}

/// The `pkill -f` self-match footgun (a remote shell whose own cmdline contains the pattern gets
/// killed, stranding the rest of the cleanup — see tests/harness_remote_kill_safety.rs) must never
/// appear on an EXECUTABLE line of rig-mode.sh. Comment lines that EXPLAIN the footgun are allowed;
/// every executable `pkill` must be the exact-name `pkill -x` form.
#[test]
fn no_cmdline_matching_pkill_on_executable_lines() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    for line in s.lines() {
        let code = line.trim_start();
        if code.starts_with('#') {
            continue; // explanatory comment (docs the footgun) — not executed
        }
        assert!(
            !code.contains("pkill -f") && !code.contains("pgrep -f"),
            "rig-mode.sh: executable line uses full-cmdline matching (self-match footgun): {line:?} \
             — use exact-name `pkill -x <name>`"
        );
        if code.contains("pkill") {
            assert!(
                code.contains("pkill -x"),
                "rig-mode.sh: non `-x` pkill on an executable line: {line:?}"
            );
        }
    }
}

/// #257: the burn is toggled over OBS WebSocket (no --mode relaunch). `obs_burn_targets` lists the
/// strih + stream program inputs; `burn_action_for_mode` maps test->add (burn ON), event->remove
/// (burn OFF). The genlock relaunch note (printed, ssh denied) is env-free — no --mode.
#[test]
fn burn_targets_cover_both_boxes() {
    let targets = run_sourced("obs_burn_targets");
    assert!(
        targets.contains("10.77.9.202") && targets.contains("strih"),
        "#257: obs_burn_targets must include the strih box. got=\n{targets}"
    );
    assert!(
        targets.contains("10.77.9.204") && targets.contains("stream"),
        "#257: obs_burn_targets must include the stream box. got=\n{targets}"
    );
}

#[test]
fn burn_action_maps_mode_to_add_or_remove() {
    let (code, out) = run_sourced_status("burn_action_for_mode test");
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "add",
        "#257: test mode -> obs_burn_filter.py add (burn ON)"
    );
    let (code, out) = run_sourced_status("burn_action_for_mode event");
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "remove",
        "#257: event mode -> obs_burn_filter.py remove (burn OFF)"
    );
    let (code, _out) = run_sourced_status("burn_action_for_mode bogus");
    assert_ne!(
        code, 0,
        "#257: an unknown mode must fail (no silent wrong action)"
    );
}

#[test]
fn genlock_relaunch_note_is_env_free_no_mode() {
    for mode in ["test", "event"] {
        let note = run_sourced(&format!("print_genlock_relaunch_note {mode}"));
        assert!(
            note.contains("--box strih") && note.contains("--box stream"),
            "#257: the relaunch note must cover strih AND stream. mode={mode} note=\n{note}"
        );
        // #257: env-free relaunch — NO --mode and NO OBS_GENLOCK_*/OBS_BURN_* env.
        assert!(
            !note.contains("--mode"),
            "#257: the genlock relaunch is env-free with NO --mode (burn is a WS toggle). note=\n{note}"
        );
        assert!(
            !note.contains("OBS_BURN") && !note.contains("OBS_GENLOCK"),
            "#257: the relaunch note must not reference any OBS_BURN_*/OBS_GENLOCK_* env. note=\n{note}"
        );
    }
}

/// The script must be SOURCE-SAFE: sourcing it (the unit-test harness) must NOT execute main (the
/// `BASH_SOURCE != $0` guard) — otherwise every source would try to act on a missing/invalid mode.
#[test]
fn script_is_source_safe() {
    let out = run_sourced("echo SOURCED_OK");
    assert!(
        out.contains("SOURCED_OK"),
        "#247: the script must be source-safe (BASH_SOURCE != $0 guard) — sourcing ran main"
    );
}
