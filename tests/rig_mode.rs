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
        // Clear painter overrides from the ambient env so the tests assert the script's PINNED
        // defaults (e.g. PAINTER_FPS default = 60), not whatever the test runner happened to export.
        .env_remove("PAINTER_FPS")
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
        .env_remove("PAINTER_FPS")
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

/// #290: TEST mode must launch the painter at the FULL 60 fps capture rate — the chain moved to
/// 60fps (#11) but the painter was launched without forcing a rate, so on the fbdev-fallback path it
/// fell to the 12 fps coverage default and the optical tick advanced far too slowly to resolve
/// 60fps timing. The invocation must pass an explicit `--paint-fps 60` so the painter paints 60
/// distinct ticks/s (under KMS it is vblank-locked at the monitor refresh and the flag is a no-op;
/// on the fbdev fallback it is what makes the rate correct).
#[test]
fn test_mode_painter_runs_at_60fps_capture_rate() {
    let p = painter_launch();
    assert!(
        p.contains("--paint-fps 60"),
        "#290: TEST mode must launch the painter at the 60fps capture rate (--paint-fps 60) so it \
         paints 60 distinct ticks/s — got:\n{p}"
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
    // cam2 as a measurable camera). The only acceptable "stop" is the display output. Check
    // EXECUTABLE lines only (a future explanatory comment may legitimately mention the old command).
    let stops_service = p.lines().any(|l| {
        let code = l.trim_start();
        !code.starts_with('#') && code.contains("systemctl stop camera-box")
    });
    assert!(
        !stops_service,
        "#291: TEST mode must NOT fully stop camera-box on an executable line — that kills cam2's \
         capture+emit. Switch it to no-display instead. Got:\n{p}"
    );
    // It installs a transient drop-in (in /run — tmpfs, so a reboot auto-reverts) overriding
    // ExecStart to camera-box WITHOUT --display, then reloads + restarts to apply it. Assert the
    // FULL write-redirect + the FULL no-display override line (not loose substrings that a comment
    // could satisfy).
    assert!(
        p.contains("> \"/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf\""),
        "#291: TEST mode must write a transient no-display systemd drop-in to /run. Got:\n{p}"
    );
    assert!(
        p.contains("echo 'ExecStart='")
            && p.contains("echo \"ExecStart=/usr/local/bin/camera-box\""),
        "#291: the drop-in must reset ExecStart and set the no-display camera-box command \
         (no --display). Got:\n{p}"
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
    // It must FAIL LOUD (not silently pass) if camera-box did not come back up — otherwise a green
    // result would be reported while cam2's capture+emit is dead.
    assert!(
        p.contains("camera-box not active after switching to no-display mode"),
        "#291: TEST mode must fail loud if camera-box is not active after the no-display switch. \
         Got:\n{p}"
    );
    assert!(
        p.contains(
            "systemctl show -p ExecStart --value camera-box 2>/dev/null | grep -q -- '--display'"
        ),
        "#291: TEST mode must verify the EFFECTIVE ExecStart no longer has --display (so a \
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

/// #420: the QPSK audio-marker emitter is a THREAD inside the SAME `frame-probe --paint-only`
/// process (src/probe/qpsk_emit.rs) — not a separate process — so TEST mode must launch the
/// painter WITH `--audio-marker` (+ device/cadence/log) or no marker is ever emitted at all. Live
/// evidence (#420): rig TEST mode started only `--paint-only --dual-qr` (video), no audio flags,
/// so the whole A/V-sync measurement was silently unmeasured (no marker in the recording).
#[test]
fn test_mode_starts_qpsk_audio_marker_alongside_painter() {
    let p = painter_launch();
    assert!(
        p.contains("--audio-marker --audio-marker-device hw:CARD=PCH,DEV=3"),
        "#420: TEST mode must launch the painter with --audio-marker targeting the cam2 BenQ HDMI \
         audio out (hw:CARD=PCH,DEV=3 — the connected-speaker device, confirmed live). Got:\n{p}"
    );
    assert!(
        p.contains("--audio-marker-cadence-ticks 180"),
        "#420: default cadence must be 180 ticks (~3s @ 60Hz painter ticks — the av-sync skill \
         recipe). Got:\n{p}"
    );
    assert!(
        p.contains("--marker-log /run/rig-qpsk-markers.csv"),
        "#420: TEST mode must write the emitted-marker CSV so it can be pulled off cam2 for \
         recording-verdict --av-sync. Got:\n{p}"
    );
    // The audio-marker flags must be on the SAME nohup'd frame-probe launch line as the painter —
    // proving it's the SAME process, not a second daemon that could independently die/desync.
    let launch_line = p
        .lines()
        .find(|l| l.contains("nohup") && l.contains("--paint-only"))
        .expect("#420: expected a single nohup'd frame-probe --paint-only launch line");
    assert!(
        launch_line.contains("--dual-qr") && launch_line.contains("--audio-marker"),
        "#420: --dual-qr (video) and --audio-marker (audio) must be on the SAME launch line \
         (same process, in lock-step via the shared frame_id/refresh tick). Got:\n{launch_line}"
    );
}

/// #420: TEST mode is env-overridable for the audio device/cadence/log, mirroring every other
/// pinned constant in this script (QR_SIZE, PAINTER_FPS, ...) — never a hardcoded rig assumption.
#[test]
fn test_mode_audio_marker_params_are_positional_overrides() {
    let p = run_sourced(
        "painter_launch_remote /usr/local/bin/frame-probe 7200 700 /run/rig-painter.pid '' 60 \
         /usr/local/bin/camera-box /run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf \
         hw:CARD=USB,DEV=0 60 /tmp/markers.csv",
    );
    assert!(
        p.contains("--audio-marker-device hw:CARD=USB,DEV=0"),
        "#420: an explicit audio-marker-device override must be honoured. Got:\n{p}"
    );
    assert!(
        p.contains("--audio-marker-cadence-ticks 60"),
        "#420: an explicit cadence override must be honoured. Got:\n{p}"
    );
    assert!(
        p.contains("--marker-log /tmp/markers.csv"),
        "#420: an explicit marker-log override must be honoured. Got:\n{p}"
    );
    // The self-check below must follow the OVERRIDDEN device, not the pinned default.
    assert!(
        p.contains("/proc/asound/USB/pcm0p/sub0/status"),
        "#420: the audible self-check must derive its ALSA status path from the (possibly \
         overridden) --audio-marker-device, not a hardcoded default. Got:\n{p}"
    );
}

/// #420 root cause, part 2: a started emitter is not a PROVEN emitter. The rig evidence showed the
/// audio path was silently dead (no process at all), so TEST mode must VERIFY the QPSK marker's
/// ALSA PCM is actually RUNNING on the target device — a REAL kernel-reported signal (never a
/// stub/no-op check) — and FAIL LOUD (killing the just-started painter) if it is silent, mirroring
/// the [4b/8] pre-record burn-ON gate's fail-fast-before-wasting-a-run shape.
#[test]
fn test_mode_verifies_audio_marker_pcm_running_fail_loud_if_silent() {
    let p = painter_launch();
    assert!(
        p.contains("/proc/asound/PCH/pcm3p/sub0/status"),
        "#420: the self-check must read the REAL ALSA PCM status file for hw:CARD=PCH,DEV=3 \
         (card id PCH, playback device 3) — a genuine kernel signal, not a stub. Got:\n{p}"
    );
    assert!(
        p.contains("state: RUNNING"),
        "#420: the self-check must assert the PCM is in the RUNNING state (actively streaming — \
         not just opened/prepared). Got:\n{p}"
    );
    assert!(
        p.contains("is NOT RUNNING") && p.contains("#420"),
        "#420: a silent marker must FAIL LOUD with a message identifying the problem (never a \
         silent pass-through). Got:\n{p}"
    );
    // The audible-check must come AFTER the painter is confirmed alive/painting (step 5) — it
    // extends that same verification, it doesn't replace or race it.
    let painter_alive_pos = p
        .find("painting /dev/fb0")
        .expect("#420: expected the existing fb0-painting verification to still be present");
    let audio_check_pos = p
        .find("/proc/asound/PCH/pcm3p/sub0/status")
        .expect("#420: expected the audio PCM self-check");
    assert!(
        audio_check_pos > painter_alive_pos,
        "#420: the audio-marker self-check must run AFTER the painter/fb0 verification. Got:\n{p}"
    );
}

/// #420: a silent marker must not be reported as a healthy TEST-mode switch — the self-check kills
/// the painter it just verified was alive, so a caller cannot mistake "process up" for "marker
/// audible" (the exact confusion #420 documents: the process WAS up, painting video, with no audio).
#[test]
fn test_mode_audio_check_kills_painter_on_silent_marker() {
    let p = painter_launch();
    let fail_pos = p
        .find("is NOT RUNNING")
        .expect("#420: expected the silent-marker FAIL branch");
    let kill_pos = p[fail_pos..]
        .find("kill \"$PAINTER_PID\"")
        .map(|i| i + fail_pos)
        .expect(
            "#420: the silent-marker FAIL branch must kill the just-started painter (a run with \
             no audio marker is wasted — don't leave it running unmeasured)",
        );
    let exit_pos = p[kill_pos..]
        .find("exit 1")
        .map(|i| i + kill_pos)
        .expect("#420: the silent-marker branch must exit non-zero after killing the painter");
    assert!(kill_pos > fail_pos && exit_pos > kill_pos);
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
    // Assert the FULL removal command + the same effective --display check TEST uses (symmetric).
    assert!(
        p.contains(
            "rm -f \"/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf\""
        ),
        "#291: EVENT mode must remove the transient no-display drop-in installed by TEST mode. \
         Got:\n{p}"
    );
    assert!(
        p.contains("systemctl daemon-reload") && p.contains("systemctl restart camera-box"),
        "#291: EVENT mode must daemon-reload + RESTART to revert ExecStart back to --display"
    );
    assert!(
        p.contains(
            "systemctl show -p ExecStart --value camera-box 2>/dev/null | grep -q -- '--display'"
        ),
        "#291: EVENT mode must verify --display restored via the EFFECTIVE ExecStart (symmetric \
         with TEST, not a `systemctl cat` that could false-pass on the base unit). Got:\n{p}"
    );
    assert!(
        p.contains("is-active camera-box") && p.contains("not active after restart"),
        "#247: EVENT mode must verify the camera-box service is active (fail loud otherwise)"
    );
    assert!(
        p.contains("grep -q -- '--display'") && p.contains("no --display"),
        "#247: EVENT mode must verify --display is restored (the interkom monitor path)"
    );
}

/// #420: EVENT mode needs NO separate step to stop the QPSK audio marker — it is a THREAD inside
/// the same `frame-probe --paint-only` process TEST mode launches (src/probe/qpsk_emit.rs), so
/// killing that one process (already covered above: pidfile kill + `pkill -x frame-probe`) stops
/// BOTH the video painter and the audio marker together. This test locks that invariant so a
/// future split of the emitter into its own process/service does not silently leave it running.
#[test]
fn event_mode_stopping_painter_process_stops_the_audio_marker_too() {
    let p = painter_stop();
    assert!(
        p.contains("kill \"$PID\"") && p.contains("pkill -x frame-probe"),
        "#420: EVENT mode must stop the SAME frame-probe process the audio marker runs inside — \
         no separate audio-marker process/service exists to stop. Got:\n{p}"
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
