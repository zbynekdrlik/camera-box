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
//! PRINTED as the exact `launch-obs-genlock.sh --mode {test|event}` step (a GUI relaunch is what
//! the win-* MCP is for — #701 proved plain scp/ssh reaches strih/stream too, but that doesn't
//! drive/verify a GUI app). Same PURE-PLANNER model as tests/launch_obs_genlock.rs: these
//! tests source the REAL script (its `BASH_SOURCE != $0` guard skips main), call its pure remote-
//! command builders, and assert the PINNED painter flags + the safety properties (free /dev/fb0,
//! fail loud on a missing binary, the PID-file stop that avoids the `pkill -f` self-match footgun,
//! the camera-box HDMI-preview restore via the CAMERA_BOX_NO_DISPLAY=1 opt-out, #528). The
//! invalid-mode contract is checked end-to-end (it exits before any ssh). NO test runs
//! `test`/`event` end-to-end — that would ssh the live rig.

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

/// #1135: source the script with a specific CAMERA_ACTIVE_SET (exported BEFORE the source, so the
/// source-role derivation at the top of rig-mode.sh — RIG_SOURCE_BOX etc — sees it) and run `body`,
/// returning stdout. Asserts success: the derivation resolves cleanly for any active set that has a
/// strih-routable member (cam1-first legacy sets AND the cam1-retired "cam2 cam3" default).
fn run_sourced_with_active_set(active_set: &str, body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .env("CAMERA_ACTIVE_SET", active_set)
        .env_remove("PAINTER_FPS")
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness (CAMERA_ACTIVE_SET={active_set:?}) exited non-zero.\nstdout={:?}\nstderr={:?}",
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

/// #531 + #789: the imag-nb genlock-staleness check is WIRED on the EVENT path as an ADVISORY
/// alert (never hard-blocks going live) keyed on drift-guard's exact "genlock STALE" DRIFT phrase.
/// The TEST path was ADVISORY too until #789 — per the owner's 2026-08-19 HARD-BLOCK decision it is
/// now a fail-closed gate, covered by test_entry_hard_blocks_on_imag_genlock_fleet_drift_789 below.
/// Recurrence guard for #530 (imag-nb ran a stale genlock build at a live event -> 45fps). The
/// compute LOGIC itself is unit-tested deterministically in tests/drift_guard.rs
/// (imag_build_drift_report_*).
#[test]
fn pre_event_genlock_staleness_check_wired_advisory_on_event_path_531() {
    let src = std::fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        src.contains("warn_imag_genlock_stale()"),
        "#531: rig-mode.sh must define warn_imag_genlock_stale (the advisory drift alert)"
    );
    assert!(
        src.contains("drift-guard.sh --check-imag"),
        "#531: the check must drive scripts/drift-guard.sh --check-imag (the dynamic box-vs-main compare)"
    );
    assert!(
        src.contains("genlock STALE"),
        "#531: the banner must key on drift-guard's 'genlock STALE' DRIFT phrase (cross-script contract)"
    );
    assert!(
        src.contains("advisory: a drift check must NEVER fail rig-mode"),
        "#531: the advisory event check must never hard-block a live event"
    );
    // Wired on the EVENT entry point (the distinctive echo line — advisory for going live).
    assert!(
        src.contains("never blocks going live"),
        "#531: do_event (EVENT path) must run the advisory pre-event genlock-staleness check"
    );
}

/// The text of do_test's body — from its own definition to do_event's (brace-anchored, the #901
/// lesson: never a bare `funcname()` that a comment could also carry).
fn do_test_body(src: &str) -> &str {
    src.split("\ndo_test() {")
        .nth(1)
        .and_then(|s| s.split("\ndo_event() {").next())
        .expect("rig-mode.sh must define do_test() then do_event()")
}

/// The text of do_event's body — from its own definition to main's.
fn do_event_body(src: &str) -> &str {
    src.split("\ndo_event() {")
        .nth(1)
        .and_then(|s| s.split("\nmain() {").next())
        .expect("rig-mode.sh must define do_event() then main()")
}

/// Run the pure #789 verdict function over a captured `--check-imag` OUTPUT + its exit RC.
fn genlock_verdict(out: &str, rc: &str) -> String {
    run_sourced(&format!("imag_genlock_gate_verdict '{out}' {rc}"))
        .trim()
        .to_string()
}

/// #789 (owner ROZHODNUTÉ 2026-08-19, comment 5337986800: HARD-BLOCK, gates maximálne striktné): the
/// `rig-mode.sh test` ENTRY must REFUSE to enter TEST mode when imag-nb's deployed genlock OBS build
/// is not provably current — no WARN-and-proceed. The EVENT path stays advisory (blocking a live
/// event on a drift check is worse than the drift). Content-asserted for the wiring + fail-closed
/// exit + the named operator remedy, and DETERMINISTICALLY unit-tested (Tier-0, #477) for the pure
/// genlock_build-facet verdict over every state: OK -> PASS, DRIFT/UNKNOWN/absent -> BLOCK.
#[test]
fn test_entry_hard_blocks_on_imag_genlock_fleet_drift_789() {
    let src = std::fs::read_to_string(script()).expect("read rig-mode.sh");

    // The two new functions are defined.
    assert!(
        src.contains("imag_genlock_gate_verdict()"),
        "#789: rig-mode.sh must define the pure verdict function imag_genlock_gate_verdict"
    );
    assert!(
        src.contains("require_imag_genlock_current()"),
        "#789: rig-mode.sh must define the fail-closed gate require_imag_genlock_current"
    );

    // TEST path hard-blocks (calls the gate, no longer the advisory warn).
    let dt = do_test_body(&src);
    assert!(
        dt.contains("require_imag_genlock_current"),
        "#789: do_test (TEST path) must call the fail-closed gate require_imag_genlock_current"
    );
    assert!(
        !dt.contains("warn_imag_genlock_stale"),
        "#789: do_test must NO LONGER run the advisory warn_imag_genlock_stale (it is now HARD-BLOCK)"
    );

    // EVENT path stays advisory (never hard-block going live).
    let de = do_event_body(&src);
    assert!(
        de.contains("warn_imag_genlock_stale"),
        "#789: do_event (EVENT path) must keep the advisory warn_imag_genlock_stale"
    );
    assert!(
        !de.contains("require_imag_genlock_current"),
        "#789: do_event must NEVER hard-block going live on a drift check"
    );

    // The gate reuses --check-imag, keys on the genlock_build PIN facet, fails closed with a
    // distinct exit, and names the ONE fleet deploy remedy for the operator.
    assert!(
        src.contains("drift-guard.sh --check-imag"),
        "#789: the gate must reuse drift-guard.sh --check-imag (the imag-vs-origin/main PIN check)"
    );
    assert!(
        src.contains("$1==\"genlock_build\""),
        "#789: the verdict must key on the genlock_build facet line (genlock-scoped, not aggregate rc)"
    );
    assert!(
        src.contains("exit 30"),
        "#789: refusing TEST entry must use the distinct non-clean exit 30"
    );
    assert!(
        src.contains("deploy-genlock-fleet.sh"),
        "#789: the hard-block message must name the ONE fleet deploy remedy (deploy-genlock-fleet.sh)"
    );

    // Deterministic verdict over every genlock_build-facet state (Tier-0 pure function).
    let ok = "  genlock_build          OK       (box=abc1234 is current with origin/main vendored-genlock HEAD)";
    let drift = "  genlock_build          DRIFT    (imag-nb genlock STALE: box=abc1234 is 3 genlock-commit(s) behind origin/main [a,b,c]; deploy the latest build via setup-imag.sh step-12 at a safe off-event time)";
    let unknown = "  genlock_build          UNKNOWN  (genlock build SHA not read on imag-nb)";
    let absent = "  distroav_sha           OK       (matches pin)\n  genlock_capability     OK       (marker present)";

    assert!(
        genlock_verdict(ok, "0").starts_with("PASS"),
        "#789: a genlock_build OK facet (box current with origin/main) must PASS"
    );
    assert!(
        genlock_verdict(drift, "20").starts_with("BLOCK"),
        "#789: a genlock_build DRIFT facet (imag STALE) must BLOCK"
    );
    assert!(
        genlock_verdict(unknown, "11").starts_with("BLOCK"),
        "#789: a genlock_build UNKNOWN facet (SHA unread) must BLOCK (fail-closed)"
    );
    assert!(
        genlock_verdict(absent, "0").starts_with("BLOCK"),
        "#789: an ABSENT genlock_build facet must BLOCK (fail-closed — the check never reported)"
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
/// capture+emit too). It installs a TRANSIENT systemd drop-in that sets
/// `Environment=CAMERA_BOX_NO_DISPLAY=1` (#528: the HDMI cameraman preview is unconditional now,
/// so a bare/plain ExecStart no longer means "no display thread" — this dedicated env-var opt-out
/// is what src/main.rs checks first), reloads + restarts, then WAITS until fb0 is actually free
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
    // It installs a transient drop-in (in /run — tmpfs, so a reboot auto-reverts) setting
    // CAMERA_BOX_NO_DISPLAY=1, then reloads + restarts to apply it. Assert the FULL write-redirect
    // + the FULL env-var override line (not loose substrings that a comment could satisfy).
    assert!(
        p.contains("> \"/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf\""),
        "#291: TEST mode must write a transient no-display systemd drop-in to /run. Got:\n{p}"
    );
    assert!(
        p.contains("echo 'Environment=CAMERA_BOX_NO_DISPLAY=1'"),
        "#528: the drop-in must set Environment=CAMERA_BOX_NO_DISPLAY=1 (the display-thread \
         opt-out src/main.rs checks first). Got:\n{p}"
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
/// (capture+emit alive) and that the effective Environment carries CAMERA_BOX_NO_DISPLAY=1 (#528 —
/// so the unit's Restart=always can never respawn a process that re-grabs fb0).
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
            "systemctl show -p Environment --value camera-box 2>/dev/null | grep -q -- 'CAMERA_BOX_NO_DISPLAY=1'"
        ),
        "#528: TEST mode must verify the EFFECTIVE Environment carries CAMERA_BOX_NO_DISPLAY=1 \
         (so a Restart=always respawn cannot re-grab fb0). Got:\n{p}"
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
         /run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf \
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
/// can never self-match the remote shell's cmdline), then RESTORES the unconditional-preview
/// camera-box and VERIFIES the service is active AND the CAMERA_BOX_NO_DISPLAY=1 opt-out is gone
/// (#528 — the interkom monitor is back). #291: TEST mode no longer STOPS camera-box (it switches
/// it to no-display via a drop-in), so EVENT mode must REMOVE that drop-in and RESTART (not just
/// `start`) to revert the Environment.
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
    // Assert the FULL removal command + the same effective Environment check TEST uses (symmetric).
    assert!(
        p.contains(
            "rm -f \"/run/systemd/system/camera-box.service.d/zz-rig-test-no-display.conf\""
        ),
        "#291: EVENT mode must remove the transient no-display drop-in installed by TEST mode. \
         Got:\n{p}"
    );
    assert!(
        p.contains("systemctl daemon-reload") && p.contains("systemctl restart camera-box"),
        "#291: EVENT mode must daemon-reload + RESTART to revert the Environment back to plain"
    );
    assert!(
        p.contains(
            "systemctl show -p Environment --value camera-box 2>/dev/null | grep -q -- 'CAMERA_BOX_NO_DISPLAY=1'"
        ),
        "#528: EVENT mode must verify the opt-out is gone via the EFFECTIVE Environment (symmetric \
         with TEST, not a `systemctl cat` that could false-pass on the base unit). Got:\n{p}"
    );
    assert!(
        p.contains("is-active camera-box") && p.contains("not active after restart"),
        "#247: EVENT mode must verify the camera-box service is active (fail loud otherwise)"
    );
    assert!(
        p.contains("CAMERA_BOX_NO_DISPLAY=1") && p.contains("interkom monitor not restored"),
        "#528: EVENT mode must fail loud with a clear message if the opt-out is still present \
         (the interkom monitor path). Got:\n{p}"
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

/// #440: TEST mode must stop the PERMANENT `cam2-painter.service` (systemd, always-on dual-QR
/// painter) BEFORE launching its own transient emitter-painter — otherwise BOTH processes write
/// /dev/fb0 and the displayed QR alternates between the two painters' run_ids, so the QPSK audio
/// marker's frame_id can't reliably match the displayed video QR (the #440 root cause: the #420
/// A/V-sync measurement was broken this way live, requiring a manual `systemctl stop
/// cam2-painter.service`). Guarded — a box without the unit installed must not fail the switch.
#[test]
fn test_mode_stops_cam2_painter_service_before_launching_emitter() {
    let p = painter_launch();
    assert!(
        p.contains("systemctl list-unit-files cam2-painter.service"),
        "#440: TEST mode must GUARD the cam2-painter.service stop (only act if the unit exists) so \
         a box without the unit is unaffected. Got:\n{p}"
    );
    assert!(
        p.contains("systemctl stop cam2-painter.service"),
        "#440: TEST mode must stop the permanent cam2-painter.service (avoids racing /dev/fb0 with \
         the transient emitter-painter). Got:\n{p}"
    );
    assert!(
        p.contains("[#440]"),
        "#440: the cam2-painter.service stop must be logged clearly. Got:\n{p}"
    );
    // Must happen BEFORE the emitter-painter's own nohup launch line — it is the whole point.
    let stop_pos = p
        .find("systemctl stop cam2-painter.service")
        .expect("#440: expected the guarded cam2-painter.service stop");
    let launch_pos = p
        .find("nohup")
        .expect("#440: expected the emitter-painter's nohup launch line");
    assert!(
        stop_pos < launch_pos,
        "#440: cam2-painter.service must be stopped BEFORE the emitter-painter launches (nohup). \
         Got:\n{p}"
    );
}

/// #892: EVENT mode must NEVER (re)start the PERMANENT `cam2-painter.service` — the old #440
/// "symmetric guard, restore what TEST stopped" assumption is WRONG for EVENT mode: `rig-mode.sh
/// event` can be invoked on an already-clean rig (no TEST session preceded it), and starting the
/// permanent painter in that case puts a QR straight onto the live broadcast (cam2's monitor is
/// what the whole cambox fleet films via the splitter) — live-caught 2026-07-30. EVENT mode must
/// instead STOP (idempotent) and DISABLE the unit, so neither `rig-mode.sh event` itself nor a
/// later reboot (the unit's own `enabled` + `Restart=always` state, the second incident on this
/// ticket) can ever bring the QR back on its own.
#[test]
fn event_mode_disables_cam2_painter_service_892() {
    let p = painter_stop();
    assert!(
        p.contains("systemctl list-unit-files cam2-painter.service"),
        "#892: EVENT mode must GUARD the cam2-painter.service stop+disable (only act if the unit \
         exists). Got:\n{p}"
    );
    assert!(
        !p.contains("systemctl start cam2-painter.service")
            && !p.contains("systemctl restart cam2-painter.service"),
        "#892: EVENT mode must NEVER (re)start the permanent cam2-painter.service — that is \
         exactly the live-broadcast QR hazard this ticket fixes. Got:\n{p}"
    );
    assert!(
        p.contains("systemctl stop cam2-painter.service"),
        "#892: EVENT mode must ensure cam2-painter.service is stopped. Got:\n{p}"
    );
    assert!(
        p.contains("systemctl disable cam2-painter.service"),
        "#892: EVENT mode must DISABLE cam2-painter.service so a reboot cannot re-arm the QR \
         painter on its own (the second incident on #892: a bare reboot restarted it 3h after a \
         manual stop). Got:\n{p}"
    );
    assert!(
        p.contains("#892"),
        "#892: the stop+disable must be logged clearly, referencing this ticket. Got:\n{p}"
    );
}

/// #440: TEST mode must WARN (never fail loud) when the deployed painter binary looks stale — live
/// evidence: cam2's `/usr/local/bin/frame-probe` was a pre-#431 build, which writes the marker-log
/// CSV only on shutdown, so the #431 emission self-check FAILED even though the marker was actually
/// running. Auto-deploying the fresh CI artifact is out of scope here — the deliverable is a clear
/// operator warning pointing at the redeploy command, printed BEFORE the #431 emission check runs
/// (so the operator sees the "may be stale" hint before, not just after, a confusing failure).
#[test]
fn test_mode_warns_on_stale_painter_binary_freshness() {
    let p = painter_launch();
    assert!(
        p.contains("WARNING") && p.contains("[#440]"),
        "#440: TEST mode must print a WARNING (not a hard fail) about painter-binary freshness. \
         Got:\n{p}"
    );
    assert!(
        p.contains("mtime"),
        "#440: the freshness warning must include the deployed binary's build/deploy date (mtime). \
         Got:\n{p}"
    );
    assert!(
        p.contains("#431") && p.contains("probe-tools-linux-amd64"),
        "#440: the freshness warning must reference the #431 emission check + tell the operator to \
         redeploy the fresh CI probe-tools-linux-amd64 artifact. Got:\n{p}"
    );
    // WARN only — must never be wired to an `exit 1` (that would turn an advisory into a hard gate,
    // which the issue explicitly scopes OUT: "Do NOT try to auto-download ... a clear operator
    // warning is the deliverable").
    let warn_pos = p
        .find("WARNING: [#440]")
        .expect("#440: expected the exact WARNING: [#440] prefix");
    let warn_line = p[warn_pos..].lines().next().unwrap_or("");
    assert!(
        !warn_line.contains("exit 1"),
        "#440: the freshness warning must be advisory only, never exit 1 on its own line. Got:\n{warn_line}"
    );
    // Must appear BEFORE the #431 emission self-check (the marker-log growth poll) so the operator
    // sees the "may be stale" hint before a confusing failure, not buried after it.
    let emission_check_pos = p
        .find("marker log")
        .expect("#440: expected the #431 marker-log emission check to still be present");
    assert!(
        warn_pos < emission_check_pos,
        "#440: the freshness WARNING must print before the #431 emission self-check runs. Got:\n{p}"
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
/// (burn OFF). The genlock relaunch note (printed, via win-* MCP) is env-free — no --mode.
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

/// #462 (EPIC #466 Topology v2): `obs_burn_targets` must ALSO cover imag-nb — the third box the
/// #252 design comment anticipated ("a third box ... can never green-light a set the cleanup does
/// not clear"). This is the SAME array the [4b/8]-equivalent pre-record gate and the #246 cleanup
/// burn-clear loop in recording-e2e.sh iterate (see tests/harness_imag_topology.rs for that side).
#[test]
fn burn_targets_cover_imag_too() {
    let targets = run_sourced("obs_burn_targets");
    // #832: IMAG_IP now defaults to the ACTIVE imag host resolved by scripts/imag-host.sh — the
    // replacement notebook, not the retired incumbent (#831 HDMI disconnected). Since the
    // 2026-07-29 IP swap the imag ROLE permanently holds .182 and the retired box sits at .189,
    // so this asserts .182 = the LIVE imag box. See tests/harness_imag_host.rs for the
    // reversibility proof.
    assert!(
        targets.contains("10.77.9.182") && targets.contains("imag"),
        "#462/#832: obs_burn_targets must include imag-nb at its ACTIVE resolved host. got=\n{targets}"
    );
}

/// #462/#1135: imag-nb's program-feeding NDI source + scene (the burn target + the routed program
/// scene) are named, overridable constants — mirroring STRIH_PROG_SOURCE/STREAM_PROG_SOURCE. #1135:
/// their DEFAULTS are no longer the literal cam1 'NDI CAM1'/'Cam 1'; they DERIVE from the resolved
/// source box (camera_source_box, camera-set.sh) via scripts/lib/imag-scene-route.sh, so a cam1-first
/// legacy set still yields 'NDI CAM1'/'Cam 1' (back-compat) while a cam1-retired set (source=cam3)
/// yields 'NDI CAM3'/'Cam 3'.
#[test]
fn imag_prog_constants_derive_off_source_box_1135() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    // #832: IMAG_IP's default is no longer an independently hardcoded literal here -- it is
    // derived by sourcing scripts/imag-host.sh (see tests/harness_imag_host.rs).
    assert!(
        s.contains(". \"$RIG_MODE_DIR/imag-host.sh\""),
        "#832: rig-mode.sh must source scripts/imag-host.sh to derive IMAG_IP."
    );
    // #1135: the imag routing must be DERIVED from the resolved source role, not hard-pinned to
    // cam1 — the literal cam1 defaults must be gone.
    assert!(
        !s.contains("IMAG_PROG_SOURCE=\"${IMAG_PROG_SOURCE:-NDI CAM1}\""),
        "#1135: IMAG_PROG_SOURCE must no longer hard-default to the literal 'NDI CAM1' — it must \
         derive from the resolved source box (imag_source_for_camera)."
    );
    assert!(
        !s.contains("IMAG_PROG_SCENE=\"${IMAG_PROG_SCENE:-Cam 1}\""),
        "#1135: IMAG_PROG_SCENE must no longer hard-default to the literal 'Cam 1' — it must \
         derive from the resolved source box (imag_scene_for_camera)."
    );
    // Back-compat: a cam1-first active set (legacy default) still resolves imag to cam1's 1:1 pins.
    let cam1first = run_sourced_with_active_set(
        "cam1 cam2 cam3",
        "printf 'SRC=%s\\nSCENE=%s\\n' \"$IMAG_PROG_SOURCE\" \"$IMAG_PROG_SCENE\"",
    );
    assert!(
        cam1first.contains("SRC=NDI CAM1") && cam1first.contains("SCENE=Cam 1"),
        "#1135: a cam1-first set must resolve imag to 'NDI CAM1'/'Cam 1' (back-compat). got=\n{cam1first}"
    );
    // The derivation: with cam1 retired (source=cam3), imag routes to cam3's 1:1 pins.
    let cam1retired = run_sourced_with_active_set(
        "cam2 cam3",
        "printf 'SRC=%s\\nSCENE=%s\\n' \"$IMAG_PROG_SOURCE\" \"$IMAG_PROG_SCENE\"",
    );
    assert!(
        cam1retired.contains("SRC=NDI CAM3") && cam1retired.contains("SCENE=Cam 3"),
        "#1135: with cam1 retired (source=cam3) imag must route to 'NDI CAM3'/'Cam 3', never the \
         stale cam1 pins. got=\n{cam1retired}"
    );
}

/// #1135: the SOURCE-camera role in rig-mode.sh is DERIVED from camera_source_box (camera-set.sh),
/// never hard-pinned to cam1. RIG_SOURCE_BOX / RIG_SOURCE_IP / STRIH_PROG_SOURCE all follow the
/// active set: a cam1-first set resolves cam1 (back-compat, byte-identical to the pre-#1135 pins),
/// a cam1-retired "cam2 cam3" set resolves cam3.
#[test]
fn source_role_derives_off_camera_source_box_1135() {
    // cam1-first legacy set: source=cam1 (full back-compat with the old hard-pin).
    let cam1first = run_sourced_with_active_set(
        "cam1 cam2 cam3",
        "printf 'BOX=%s\\nIP=%s\\nSTRIH=%s\\n' \"$RIG_SOURCE_BOX\" \"$RIG_SOURCE_IP\" \"$STRIH_PROG_SOURCE\"",
    );
    assert!(
        cam1first.contains("BOX=cam1")
            && cam1first.contains("IP=10.77.9.61")
            && cam1first.contains("STRIH=NDI cam1"),
        "#1135: a cam1-first set must resolve source=cam1 / 10.77.9.61 / 'NDI cam1'. got=\n{cam1first}"
    );
    // cam1 retired: source moves to cam3 (the next strih-routable member), and every derived value
    // follows — this is the whole point of the ticket.
    let cam1retired = run_sourced_with_active_set(
        "cam2 cam3",
        "printf 'BOX=%s\\nIP=%s\\nSTRIH=%s\\n' \"$RIG_SOURCE_BOX\" \"$RIG_SOURCE_IP\" \"$STRIH_PROG_SOURCE\"",
    );
    assert!(
        cam1retired.contains("BOX=cam3")
            && cam1retired.contains("IP=10.77.9.63")
            && cam1retired.contains("STRIH=NDI cam3"),
        "#1135: with cam1 retired, source must resolve to cam3 / 10.77.9.63 / 'NDI cam3', never the \
         retired cam1. got=\n{cam1retired}"
    );
}

/// #1135: the EVENT-mode fleet sweep (event_mode_assert) must base its target list on the RESOLVED
/// source box (RIG_SOURCE_BOX / RIG_SOURCE_IP), never the literal `cam1=$CAM1_IP` — else a cam1-
/// retired rig sweeps the broken/absent cam1 and MISSES the real source (cam3).
#[test]
fn event_sweep_targets_resolved_source_not_literal_cam1_1135() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        !s.contains("EVENT_ASSERT_TARGETS=(\"cam1=$CAM1_IP\""),
        "#1135: the EVENT sweep must not hard-pin its base target to the literal cam1=$CAM1_IP."
    );
    assert!(
        s.contains("EVENT_ASSERT_TARGETS=(\"${RIG_SOURCE_BOX}=$RIG_SOURCE_IP\""),
        "#1135: the EVENT sweep base must be the resolved source (${{RIG_SOURCE_BOX}}=$RIG_SOURCE_IP) \
         + cam2, so cam1's retirement moves the swept source automatically."
    );
    // The whole active fleet is still covered: with cam1 retired, source=cam3, so cam3 is swept
    // (via the RIG_SOURCE_BOX base) even though camera_active_secondary_set is then empty.
    let secondary = run_sourced_with_active_set("cam2 cam3", "camera_active_secondary_set; echo");
    assert!(
        secondary.trim().is_empty(),
        "#1135 sanity: with source=cam3 the secondary set is empty; cam3 must be swept via the \
         RIG_SOURCE_BOX base, not the (empty) secondary loop. got secondary=\n{secondary}"
    );
}

/// #462: TEST mode must route imag-nb's PROGRAM to the cam1 scene (`set_imag_test_program`) — the
/// camera that also proves cam→imag zero-loss — via the SAME `obs_phase2.py switch` action the
/// all-cambox sweep uses (SetCurrentProgramScene + its non-black self-check). Must be CALLED from
/// do_test (not just defined).
#[test]
fn test_mode_routes_imag_program_to_cam1_scene() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        s.contains("set_imag_test_program"),
        "#462: rig-mode.sh must define + call the set_imag_test_program helper."
    );
    let do_test = s
        .split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("");
    assert!(
        do_test.contains("set_imag_test_program"),
        "#462: do_test must call set_imag_test_program. Got:\n{do_test}"
    );

    // The helper itself must use the `switch` action (not `setup`/`prod-scene` — those are the
    // heavier strih/stream-specific mechanisms) against imag's IP + program scene. Static check
    // only (like enforce_strih_ndi_mapping's sibling tests in harness_rig_ndi_mapping.rs) — this
    // function makes a LIVE OBS-websocket call, so it must never be EXECUTED by a unit test.
    let def = s
        .find("set_imag_test_program()")
        .expect("set_imag_test_program must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let body = &s[def..body_end];
    assert!(
        body.contains(
            "obs_phase2.py\" switch --host \"$IMAG_IP\" --program-scene \"$IMAG_PROG_SCENE\""
        ),
        "#462: set_imag_test_program must invoke `obs_phase2.py switch --host $IMAG_IP \
         --program-scene $IMAG_PROG_SCENE`. Got:\n{body}"
    );
}

/// #462: EVENT mode must NOT force any imag scene switch (mirrors strih/stream — rig-mode never
/// scene-switches those in EVENT mode either); only the burn toggle (already covered by
/// `burn_targets_cover_imag_too` + `toggle_burn`) turns imag's measurement burn OFF.
#[test]
fn event_mode_does_not_scene_switch_imag() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    let do_event = s
        .split("do_event()")
        .nth(1)
        .unwrap_or("")
        .split("\nmain()")
        .next()
        .unwrap_or("");
    assert!(
        !do_event.contains("set_imag_test_program"),
        "#462: do_event must NOT call set_imag_test_program (EVENT mode never scene-switches, \
         mirroring strih/stream). Got:\n{do_event}"
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

/// #524: EVENT mode must guard against a STRAY recording left running on strih/stream — a runaway
/// recording filled ~266 GiB (~11h to full) during a live event, and a SECOND stray recording
/// (18.57 GiB) started the SAME event day. Both were only caught + stopped manually. `do_event`
/// must call the pre-event guard EARLY — before the painter-stop/broadcast-restore steps — so a
/// stray recording is stopped (file KEPT) and disk is freed before anything else in EVENT mode
/// proceeds.
#[test]
fn event_mode_calls_stop_stray_recordings_guard() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    let do_event = s
        .split("do_event()")
        .nth(1)
        .unwrap_or("")
        .split("\nmain()")
        .next()
        .unwrap_or("");
    assert!(
        do_event.contains("stop_stray_recordings"),
        "#524: do_event must call stop_stray_recordings (pre-event stray-recording guard). \
         Got:\n{do_event}"
    );
    // Must run EARLY — before the painter-stop / broadcast-restore steps (stop the stray
    // recording + free disk before anything else in EVENT mode proceeds).
    let guard_pos = do_event
        .find("stop_stray_recordings")
        .expect("#524: expected the stop_stray_recordings call");
    let painter_stop_pos = do_event
        .find("painter_stop_remote")
        .expect("#524: expected the existing painter-stop step to still be present");
    assert!(
        guard_pos < painter_stop_pos,
        "#524: stop_stray_recordings must run BEFORE the painter-stop/broadcast-restore steps. \
         Got:\n{do_event}"
    );
}

/// #524: `stray_recording_targets` lists ONLY strih + stream (the two boxes named in the incident)
/// — NOT imag-nb, which has no OBS recording output in this topology.
#[test]
fn stray_recording_targets_covers_strih_and_stream_only() {
    let targets = run_sourced("stray_recording_targets");
    assert!(
        targets.contains("10.77.9.202") && targets.contains("strih"),
        "#524: stray_recording_targets must include strih. got=\n{targets}"
    );
    assert!(
        targets.contains("10.77.9.204") && targets.contains("stream"),
        "#524: stray_recording_targets must include stream. got=\n{targets}"
    );
    assert!(
        !targets.contains("10.77.9.182") && !targets.contains("imag"),
        "#524: stray_recording_targets must NOT include imag-nb (no recording output there). \
         got=\n{targets}"
    );
}

/// #524: `stop_stray_recordings` must invoke the `obs_phase2.py record --action guard` WS helper
/// (GetRecordStatus -> StopRecord-if-active) for each target — reusing the SAME recording-control
/// script the harness already uses (`record --action start|stop|status`), extended with the new
/// `guard` action. Static check only — this function makes a LIVE OBS-websocket call, so it must
/// never be EXECUTED by a unit test (mirrors enforce_strih_ndi_mapping / set_imag_test_program).
#[test]
fn stop_stray_recordings_invokes_record_guard_action() {
    let s = fs::read_to_string(script()).expect("read rig-mode.sh");
    let def = s
        .find("stop_stray_recordings()")
        .expect("#524: stop_stray_recordings must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let body = &s[def..body_end];
    assert!(
        body.contains("obs_phase2.py\" record --action guard"),
        "#524: stop_stray_recordings must invoke `obs_phase2.py record --action guard`. \
         Got:\n{body}"
    );
    assert!(
        body.contains("stray_recording_targets"),
        "#524: stop_stray_recordings must iterate stray_recording_targets (both boxes). \
         Got:\n{body}"
    );
    assert!(
        body.contains("OBS_WS_PASSWORD"),
        "#524: stop_stray_recordings must pass the shared OBS_WS_PASSWORD (never hardcode a WS \
         password). Got:\n{body}"
    );
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

/// #868 — on the PAINTER box, EVENT mode must NOT demand that `CAMERA_BOX_NO_DISPLAY=1` has
/// disappeared, and must NOT demand that camera-box owns `/dev/fb0`.
///
/// #863 made `/etc/systemd/system/camera-box.service.d/cam2-no-display.conf` PERMANENT on cam2,
/// because `cam2-painter.service` is the sole owner of that box's monitor. EVENT mode's step (5)
/// still encoded the pre-#863 contract, so on cam2 it could only ever FAIL:
///
/// ```text
/// FAIL: camera-box Environment still carries CAMERA_BOX_NO_DISPLAY=1 — interkom monitor not restored
/// ```
///
/// That is not cosmetic: the non-zero exit stranded EVENT mode's own burn-clear sweep, leaving
/// `genlock_burn=true` on strih's `NDI cam1`, which then made the next Full-path E2E gate run refuse
/// in its `#758` preflight. The verify must branch on whether this box HAS a dedicated painter unit
/// (the same condition `cam2_painter_service_start_cmds` already guards on), asserting the state that
/// is actually correct there: transient drop-in gone, `camera-box` active, `cam2-painter` active, and
/// `/dev/fb0` held (by the painter).
#[test]
fn event_mode_painter_box_expects_the_permanent_no_display_dropin_868() {
    let p = painter_stop();
    // The NO_DISPLAY assert must be REACHED ONLY on a box with no painter unit — i.e. it sits inside
    // a branch labelled for this ticket, not at the top level of step (5). Anchoring on the `#868`
    // label rather than on `systemctl list-unit-files cam2-painter.service` is deliberate: that
    // string ALREADY appears earlier (the #440 stop/start guards), so a position check against it
    // would pass vacuously without any branch existing.
    let branch_pos = p
        .find("#868")
        .expect("#868: expected the painter-box verify branch, labelled #868");
    let nodisplay_assert = p
        .find("interkom monitor not restored")
        .expect("#868: the non-painter-box NO_DISPLAY assert must still exist (#528)");
    assert!(
        branch_pos < nodisplay_assert,
        "#868: on the painter box the permanent #863 drop-in MUST remain, so the NO_DISPLAY assert \
         has to sit behind the painter-box branch — not run unconditionally. Got:\n{p}"
    );
}

/// #892 superseded the #868-era invariant here: on the painter box EVENT mode used to verify
/// cam2-painter came back ACTIVE (restored). Per #892 it must now verify the OPPOSITE — the
/// permanent painter is INACTIVE (never restored) — while camera-box's own permanent #863
/// NO_DISPLAY drop-in stays untouched (a separate, static, provisioning-time config), so fb0
/// ends up correctly UNHELD (blank monitor) rather than either painting a QR or fighting
/// camera-box for the framebuffer.
#[test]
fn event_mode_painter_box_verifies_the_painter_service_is_stopped_892() {
    let p = painter_stop();
    assert!(
        p.contains("is-active cam2-painter"),
        "#892: on the painter box EVENT mode must verify cam2-painter's active state. Got:\n{p}"
    );
    assert!(
        p.contains("#868") || p.contains("#892"),
        "#868/#892: the painter-box branch must be labelled so a future reader sees why the \
         asserts differ per box. Got:\n{p}"
    );
}

/// #868 — a FAILED cam-side restore must never strand a measurement burn ON.
///
/// `do_event` runs the cam-side restore BEFORE `toggle_burn event`, under `set -euo pipefail`. So a
/// non-zero cam_ssh aborted the whole function and the burn-clear never ran — live cost: `NDI cam1`
/// on strih kept `genlock_burn=true`, and the next Full-path E2E gate run refused in its `#758`
/// preflight ("genlock_burn is still ON from a prior run"). The failure is now RECORDED, the
/// burn-clear still runs, and the recorded failure is folded into the final exit status — so
/// deferring it cannot SWALLOW it either.
#[test]
fn event_mode_cam_restore_failure_does_not_strand_the_burn_clear_868() {
    let whole = fs::read_to_string(script()).expect("read rig-mode.sh");
    // Bound to do_event's BODY. Anchoring on the whole file is what this repo's own gotcha warns
    // about: prose ABOVE a call site can contain the same literal as the call, so `.find()` grabs
    // the comment instead of the code (it did, on the first attempt at this test).
    let body_start = whole
        .find("\ndo_event() {")
        .expect("#868: expected do_event to exist");
    let body_end = whole[body_start..]
        .find("\nmain() {")
        .map(|i| body_start + i)
        .expect("#868: expected main() to bound do_event's body");
    let s = &whole[body_start..body_end];
    let cam_call = s
        .find("cam_ssh \"$(painter_stop_remote")
        .expect("#868: expected do_event's cam-side restore call");
    // The real call site, never the explanatory comment above it: the invocation is indented and
    // starts the line.
    let burn_clear = s
        .find("\n  toggle_burn event")
        .expect("#868: expected do_event's burn-clear invocation");
    assert!(
        cam_call < burn_clear,
        "#868: this test assumes the documented order (cam restore, then burn clear) — if that \
         changed, revisit the fix's premise"
    );
    // The cam-side failure must be CAPTURED, not allowed to abort before the burn-clear.
    let capture = s
        .find("|| _cam_restore_rc=$?")
        .expect("#868: the cam-side restore's failure must be captured, not left to `set -e`");
    assert!(
        capture < burn_clear,
        "#868: the capture must happen BEFORE the burn-clear, or `set -e` still aborts first"
    );
    // ...and must still make EVENT mode fail: the recorded rc has to reach the final exit decision.
    let tail = &s[burn_clear..];
    assert!(
        tail.contains("_cam_restore_rc:-0") && tail.contains("exit 1"),
        "#868: the recorded cam-side failure must be folded into the final exit status — deferring \
         it past the burn-clear must not swallow it. Got tail:\n{tail}"
    );
}

// ---------------------------------------------------------------------------------------------- //
// issue 1171 — the #789 imag-genlock TEST-entry HARD-BLOCK must consult the issue-1013 offline-ack
// before fail-closing. A legitimately absent (acked-offline + unreachable) imag must SKIP the gate
// (a loud named skip), not exit 30 and abort TEST entry — which left cam2 with no painter
// (2026-08-22). An acked-but-REACHABLE imag is a STALE ack and is NOT exempted (mirrors the
// recording-e2e.sh [0/8] reachability probe): it falls through to the real gate.
// ---------------------------------------------------------------------------------------------- //

/// Run the pure #1171 offline-ack decision function over (reason, reachable).
fn offline_ack_action(reason: &str, reachable: &str) -> String {
    run_sourced(&format!(
        "imag_genlock_gate_offline_ack_action '{reason}' {reachable}"
    ))
    .trim()
    .to_string()
}

/// Deterministic decision matrix (Tier-0 pure function, #477): only acked+unreachable skips.
#[test]
fn imag_genlock_gate_offline_ack_action_decision_matrix_1171() {
    assert_eq!(
        offline_ack_action("", "0"),
        "proceed",
        "1171: imag NOT acked -> run the #789 gate normally"
    );
    assert_eq!(
        offline_ack_action("", "1"),
        "proceed",
        "1171: imag NOT acked (reachable) -> run the gate normally"
    );
    assert_eq!(
        offline_ack_action("notebook-replacement-2026-08-22", "0"),
        "skip",
        "1171: acked + UNREACHABLE (issue-1013 legit offline) -> SKIP the gate"
    );
    assert_eq!(
        offline_ack_action("notebook-replacement-2026-08-22", "1"),
        "proceed",
        "1171: acked but REACHABLE = STALE ack, NOT exempted -> fall through to the real gate"
    );
}

/// The gate must be wired to the offline-ack seam: rig-mode.sh sources the offline-ack lib, defines
/// the pure decision function, and require_imag_genlock_current consults it (reading the ack reason,
/// probing reachability, naming issue 1013 in the skip) BEFORE the existing fail-closed body — which
/// stays intact (drift-guard --check-imag + exit 30).
#[test]
fn require_imag_genlock_current_consults_offline_ack_1171() {
    let src = std::fs::read_to_string(script()).expect("read rig-mode.sh");
    assert!(
        src.contains("lib/cambox-offline-ack.sh"),
        "1171: rig-mode.sh must source the offline-ack lib"
    );
    assert!(
        src.contains("imag_genlock_gate_offline_ack_action()"),
        "1171: rig-mode.sh must define the pure decision function imag_genlock_gate_offline_ack_action"
    );
    // The gate body (from its own definition to the next function) must consult the ack + probe.
    let gate = src
        .split("require_imag_genlock_current() {")
        .nth(1)
        .and_then(|s| s.split("\nburn_action_for_mode() {").next())
        .expect(
            "1171: rig-mode.sh must define require_imag_genlock_current() then burn_action_for_mode()",
        );
    assert!(
        gate.contains("cambox_offline_ack_reason"),
        "1171: the gate must read the imag offline-ack reason: {gate}"
    );
    assert!(
        gate.contains("imag_genlock_gate_offline_ack_action"),
        "1171: the gate must delegate the skip/proceed decision to the pure function: {gate}"
    );
    assert!(
        gate.contains("ping "),
        "1171: the gate must probe imag reachability (stale-ack protection): {gate}"
    );
    assert!(
        gate.contains("1013"),
        "1171: the loud skip must name issue 1013 (the offline-ack mechanism): {gate}"
    );
    // The existing #789 fail-closed body must remain intact.
    assert!(
        gate.contains("drift-guard.sh --check-imag") && gate.contains("exit 30"),
        "1171: the existing fail-closed gate (drift-guard --check-imag + exit 30) must stay: {gate}"
    );
}
