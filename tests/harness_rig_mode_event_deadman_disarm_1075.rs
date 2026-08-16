//! #1075 — rig-mode EVENT must also tear down the transient cam2-painter deadman timer.
//!
//! Root cause: `scripts/lib/cam2-painter-deadman.sh` defines a TRANSIENT, PERIODIC systemd-run
//! timer (`cam2-painter-deadman`, every 5 min) that `systemctl start cam2-painter` whenever no
//! frame-probe is running. `recording-e2e.sh` arms it before stopping cam2-painter and disarms it
//! in cleanup(); a SIGKILLed run leaves it armed. `rig-mode.sh event` → `painter_stop_remote` then
//! stops+DISABLES cam2-painter.service (#892) but never touches that transient timer — and
//! `systemctl disable` does not stop a manual `systemctl start`, so within ≤5 min of switching to
//! EVENT the deadman resurrects the QR painter ON AIR (live incident 2026-08-15). This wires:
//!   (1) EVENT disarms the deadman (painter_stop_remote, via cam2_painter_deadman_disarm_cmds);
//!   (2) TEST re-arms it as the standing "never dark" net (do_test, via cam2_painter_deadman_arm_cmds);
//!   (3) event_mode_assert catches a still-armed deadman as a STRAY unit (event-assert.sh's
//!       STRAY_UNITS glob widened to also match cam2-painter-deadman*, which event_assert.py's
//!       services_healthy_ok already fails on).
//!
//! Pure-shell / content tests — no rig, no ssh. Mirrors tests/rig_mode.rs's own run_sourced model:
//! source the REAL scripts (their BASH_SOURCE!=$0 guards skip main) and assert the pure builders'
//! output + the source-text wiring. NO test runs test/event end-to-end (that would ssh the rig).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rig_mode() -> PathBuf {
    manifest_dir().join("scripts/rig-mode.sh")
}

fn event_assert_lib() -> PathBuf {
    manifest_dir().join("scripts/lib/event-assert.sh")
}

fn read(p: &PathBuf) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source rig-mode.sh (guard skips main) + run `body`, returning stdout. Asserts the harness itself
/// exited 0 (the pure builders never fail). Mirrors tests/rig_mode.rs::run_sourced.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", rig_mode())
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

/// The EVENT-mode remote bash for cam2 (stop painter via pidfile, disable+disarm, restart camera-box).
fn painter_stop() -> String {
    run_sourced("painter_stop_remote /run/rig-painter.pid")
}

// ================================================================================================
// (0) rig-mode.sh must SOURCE the deadman lib so both the executed flow and these unit tests get
//     the arm/disarm builders.
// ================================================================================================

#[test]
fn rig_mode_sources_the_cam2_painter_deadman_lib() {
    let s = read(&rig_mode());
    assert!(
        s.contains("lib/cam2-painter-deadman.sh"),
        "#1075: rig-mode.sh must source scripts/lib/cam2-painter-deadman.sh (single source of the \
         arm/disarm builders)"
    );
}

// ================================================================================================
// (1) EVENT disarms the transient deadman timer (the core fix).
// ================================================================================================

#[test]
fn event_mode_disarms_the_deadman_timer_1075() {
    let p = painter_stop();
    assert!(
        p.contains("systemctl stop cam2-painter-deadman.timer"),
        "#1075: EVENT mode must STOP the transient cam2-painter-deadman.timer (else it resurrects \
         the QR painter within ~5 min of going live). Got:\n{p}"
    );
    assert!(
        p.contains("cam2-painter-deadman.service"),
        "#1075: EVENT mode must reset-failed the transient cam2-painter-deadman.service. Got:\n{p}"
    );
}

#[test]
fn event_mode_disarm_runs_after_the_892_disable_1075() {
    // The disarm belongs at step (2.5), right after the #892 stop+disable of the permanent painter
    // — the same place EVENT already tears down the painter it must never leave running.
    let p = painter_stop();
    let disable_pos = p
        .find("disabling")
        .expect("#892: expected the permanent-painter stop+disable text");
    let disarm_pos = p
        .find("systemctl stop cam2-painter-deadman.timer")
        .expect("#1075: expected the deadman disarm");
    assert!(
        disable_pos < disarm_pos,
        "#1075: the deadman disarm must run alongside/after the #892 permanent-painter disable. \
         Got:\n{p}"
    );
}

// ================================================================================================
// (2) TEST re-arms the deadman as the standing net (symmetric with EVENT's disarm).
// ================================================================================================

#[test]
fn test_mode_arms_the_deadman_timer_1075() {
    let s = read(&rig_mode());
    // Slice do_test's body the same way tests/rig_mode.rs does — everything between the FIRST
    // do_test()/do_event() function markers.
    let do_test = s
        .split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("");
    assert!(
        do_test.contains("cam2_painter_deadman_arm_cmds"),
        "#1075: do_test must arm the deadman (cam2_painter_deadman_arm_cmds) as the standing \
         'never dark' net for the handed-off permanent painter. Got do_test body:\n{do_test}"
    );
}

// ================================================================================================
// (3) event_mode_assert catches a still-armed deadman as a STRAY unit — via the fleet check's
//     STRAY_UNITS glob (event_assert.py's services_healthy_ok already fails on any stray unit).
// ================================================================================================

#[test]
fn event_assert_fleet_check_covers_the_deadman_timer_1075() {
    let s = read(&event_assert_lib());
    // The STRAY_UNITS list-units glob must ALSO match the transient deadman timer, so a live
    // cam2-painter-deadman.timer in EVENT mode surfaces as a stray unit and fails the contract.
    assert!(
        s.contains("cam2-painter-deadman"),
        "#1075: event_assert_fleet_check_cmds's STRAY_UNITS glob must also match \
         cam2-painter-deadman* (a live deadman timer in EVENT mode is a stray unit). Got:\n{s}"
    );
    // Sanity: the original burn-unit glob must remain (this is a WIDENING, never a replacement).
    assert!(
        s.contains("camera-box-burn-*"),
        "#1075: the existing camera-box-burn-* stray-unit glob must be preserved. Got:\n{s}"
    );
}
