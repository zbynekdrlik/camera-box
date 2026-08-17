//! #772 -- the PRODUCTION `camera-box.service` needs the same on-box dead-man restore that
//! cam2-painter got in #872/#1072, adapted for a genuinely different device lifecycle.
//!
//! ## The bug
//!
//! `scripts/recording-e2e.sh` STOPS `camera-box.service` and launches a probe-featured capture
//! BURN as a transient `systemd-run --unit=camera-box-burn-<id> --property=Restart=on-failure`
//! unit at FOUR sites (cam1 [2/8], the [2b/8] ALL_CAMBOX loop, cam2 non-sweep [3/8], AV_RESTART),
//! and restarts production ONLY in `cleanup()` (the bash EXIT trap). A cancel-in-progress SIGKILL
//! (routine: any push to `dev` cancels an in-flight run) never runs the trap -- camera-box is left
//! STOPPED and the burn unit (systemd-owned, NO `--duration-secs`, so it runs FOREVER) keeps
//! holding `/dev/videoN`. The operator's multiview freezes BETWEEN runs; the eventual
//! `systemctl start camera-box` crash-loops on "Device or resource busy". Live re-occurrence
//! 2026-08-03 on this ticket.
//!
//! ## The fix
//!
//! An on-box dead-man (`scripts/lib/camera-box-deadman.sh`) armed before each stop. Because the
//! burn (unlike the painter's `frame-probe`) never self-terminates, a process-presence guard would
//! keep it permanently disarmed -- so its FIRST fire is DELAYED past this run's entire window
//! (`--on-active`, computed from the real DURATION + margin), so it can NEVER fire during a live
//! measurement (worst case is slower recovery, never a corrupted verdict); it re-fires PERIODICALLY
//! (`--on-unit-active`) and SELF-DISARMS once production is confirmed active. Its action stops the
//! stray burn UNIT (not just `pkill`, which would trip Restart=on-failure), pkills the burn, starts
//! camera-box, and NEVER touches frame-probe (the cam2 painter, a different device).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB: &str = "scripts/lib/camera-box-deadman.sh";
const HARNESS: &str = "scripts/recording-e2e.sh";

// ---------------------------------------------------------------------------------------------- //
// Lib shape
// ---------------------------------------------------------------------------------------------- //

#[test]
fn deadman_first_fire_is_delayed_and_then_periodic_772() {
    let s = read(LIB);
    assert!(
        s.contains("--on-active=") && s.contains("--on-unit-active="),
        "#772: the camera-box dead-man must DELAY its first fire past the run window (--on-active) \
         AND re-fire periodically (--on-unit-active) so a run killed after the first fire still \
         recovers -- the burn never self-terminates, so a short one-shot cannot work like the \
         painter's frame-probe-guarded one"
    );
}

#[test]
fn deadman_action_stops_the_stray_burn_unit_not_just_pkill_772() {
    let s = read(LIB);
    // Stopping the UNIT is load-bearing: the burn runs under Restart=on-failure, so a bare pkill
    // just makes it respawn and re-steal /dev/video (the #894 fight).
    assert!(
        s.contains("camera-box-burn-*") && s.contains("systemctl stop"),
        "#772: the action must STOP the stray camera-box-burn-* UNIT before starting production, \
         not merely pkill it (Restart=on-failure would respawn a pkilled burn)"
    );
    assert!(
        s.contains("pkill -9 -x camera-box-burn"),
        "#772: the action must ALSO pkill the burn by its EXACT 15-char comm (-x camera-box-burn), \
         never `pkill -f` (self-match footgun) and never the 10-char production `camera-box`"
    );
}

#[test]
fn deadman_action_starts_production_only_when_inactive_772() {
    let s = read(LIB);
    assert!(
        s.contains("is-active --quiet camera-box || systemctl start camera-box"),
        "#772: the action must start camera-box only if it is not already active"
    );
}

#[test]
fn deadman_action_self_disarms_once_production_is_back_772() {
    let s = read(LIB);
    // No cleanup() disarm wiring exists; the action self-disarms once camera-box is confirmed
    // active, so a normal run leaves no lingering timer.
    assert!(
        s.contains(".timer"),
        "#772: the action must self-disarm (stop its own .timer) once production is confirmed back"
    );
    let start = s
        .find("systemctl start camera-box")
        .expect("#772: expected the guarded start");
    let disarm = s
        .rfind("systemctl stop ${CAMERA_BOX_DEADMAN_UNIT}.timer")
        .or_else(|| s.rfind("systemctl stop camera-box-deadman.timer"))
        .expect("#772: expected the self-disarm stop of the dead-man's own timer inside the action");
    assert!(
        disarm > start,
        "#772: self-disarm must come AFTER the start (start {start}, disarm {disarm}) -- a failed \
         start must leave the timer armed to retry on the next re-fire"
    );
}

#[test]
fn deadman_never_touches_frame_probe_772() {
    // frame-probe (the cam2 fb0 painter, a DIFFERENT device) must never be a COMMAND here -- killing
    // it would darken the operator's cam2 QR monitor on every camera-box start. The header comments
    // legitimately DISCUSS the distinction, so the check is "every frame-probe line is a comment",
    // not a blanket absence. The functional test above additionally proves the running action never
    // invokes it.
    let s = read(LIB);
    for (i, line) in s.lines().enumerate() {
        if line.contains("frame-probe") {
            assert!(
                line.trim_start().starts_with('#'),
                "#772: line {} references frame-probe outside a comment -- the dead-man must never \
                 touch the cam2 painter: {line}",
                i + 1
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------- //
// Functional -- re-exec the generated action under fake systemctl/pkill (fake the remote, not ssh)
// ---------------------------------------------------------------------------------------------- //

/// Sources the real lib, generates the arm (first=17), extracts the `/bin/bash -c '...'` action,
/// and runs it under a PATH-restricted fake `systemctl`/`pkill`. Returns the fake bins' call log.
/// Simulates a killed run: camera-box inactive on first check, active after `start` -- so the
/// action must stop the stray burn unit, pkill the burn, start production, then self-disarm.
fn run_action_call_log() -> String {
    // Raw string (no format! brace-escaping); the lib path is passed via $SCRIPT env. The fake
    // `systemctl`/`pkill` heredocs are UNQUOTED so $LOG/$FAKE expand at write time while \$* stays
    // literal for the fake's own runtime -- the proven scripts/lib fake-the-remote pattern.
    let script = r#"
set -uo pipefail
. "$SCRIPT"
FAKE="$(mktemp -d)"
LOG="$FAKE/calls.log"
cat > "$FAKE/systemctl" <<FAKESC
#!/usr/bin/env bash
echo "systemctl \$*" >> "$LOG"
case "\$1 \$2" in
  "list-units --all") echo "camera-box-burn-99001.service" ;;
esac
case "\$*" in
  "is-active --quiet camera-box") if [ -f "$FAKE/started" ]; then exit 0; else exit 3; fi ;;
  "start camera-box") : > "$FAKE/started" ;;
esac
exit 0
FAKESC
chmod +x "$FAKE/systemctl"
cat > "$FAKE/pkill" <<FAKEPK
#!/usr/bin/env bash
echo "pkill \$*" >> "$LOG"
exit 0
FAKEPK
chmod +x "$FAKE/pkill"
armtext="$(camera_box_deadman_arm_cmds 17)"
action="${armtext#*/bin/bash -c \'}"
action="${action%%\' 2>/dev/null*}"
PATH="$FAKE:/usr/bin:/bin" /usr/bin/env bash -c "$action"
cat "$LOG"
rm -rf "$FAKE"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("SCRIPT", manifest_dir().join(LIB))
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "action harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn deadman_action_restores_a_killed_run_and_self_disarms_772() {
    let log = run_action_call_log();
    assert!(
        log.contains("systemctl stop camera-box-burn-99001.service"),
        "#772: the action must STOP the stray burn unit. Got:\n{log}"
    );
    assert!(
        log.contains("pkill -9 -x camera-box-burn"),
        "#772: the action must pkill the stray burn. Got:\n{log}"
    );
    assert!(
        log.contains("systemctl start camera-box"),
        "#772: the action must start production when it was inactive. Got:\n{log}"
    );
    assert!(
        log.contains("systemctl stop camera-box-deadman.timer"),
        "#772: the action must self-disarm once production is active. Got:\n{log}"
    );
    assert!(
        !log.contains("frame-probe"),
        "#772: the action must NEVER touch frame-probe (the cam2 painter). Got:\n{log}"
    );
}

// ---------------------------------------------------------------------------------------------- //
// Wiring into recording-e2e.sh
// ---------------------------------------------------------------------------------------------- //

#[test]
fn harness_sources_the_deadman_lib_772() {
    let s = read(HARNESS);
    assert!(
        s.contains("lib/camera-box-deadman.sh"),
        "#772: recording-e2e.sh must source scripts/lib/camera-box-deadman.sh -- the arm text is \
         single-sourced there, never duplicated inline at the four stop sites"
    );
}

#[test]
fn harness_computes_first_fire_from_the_run_duration_772() {
    let s = read(HARNESS);
    let line = s
        .lines()
        .find(|l| l.trim_start().starts_with("CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN="))
        .expect("#772: recording-e2e.sh must compute CAMERA_BOX_DEADMAN_FIRST_FIRE_MIN");
    assert!(
        line.contains("DURATION") && line.contains("OVERHEAD"),
        "#772: the first-fire delay must be derived from the real DURATION plus an overhead margin \
         (so it always exceeds THIS run's window), not a fixed literal. Got: {line}"
    );
}

#[test]
fn harness_arms_the_deadman_at_exactly_the_four_production_stop_sites_772() {
    let s = read(HARNESS);
    let n = s.matches("camera_box_deadman_arm_cmds").count();
    assert_eq!(
        n, 4,
        "#772: the dead-man must be armed at all FOUR camera-box-stop sites (cam1 [2/8], the [2b/8] \
         ALL_CAMBOX loop, cam2 non-sweep [3/8], AV_RESTART) -- found {n}"
    );
}

/// For a bounded window `s[start..end]`, assert the dead-man is armed BEFORE the production stop.
fn assert_armed_before_stop(s: &str, start_anchor: &str, end_anchor: &str, label: &str) {
    let start = s
        .find(start_anchor)
        .unwrap_or_else(|| panic!("#772 [{label}]: start anchor {start_anchor:?} not found"));
    let end = s[start..]
        .find(end_anchor)
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("#772 [{label}]: end anchor {end_anchor:?} not found after start"));
    let win = &s[start..end];
    let armed = win.find("camera_box_deadman_arm_cmds").unwrap_or_else(|| {
        panic!("#772 [{label}]: the dead-man must be armed in this site. Window:\n{win}")
    });
    let stopped = win.find("systemctl stop camera-box").unwrap_or_else(|| {
        panic!("#772 [{label}]: expected the production camera-box stop in this site. Window:\n{win}")
    });
    assert!(
        armed < stopped,
        "#772 [{label}]: the dead-man must be ARMED BEFORE the stop (armed {armed}, stopped {stopped})"
    );
}

#[test]
fn cam1_deploy_arms_the_deadman_before_stopping_camera_box_772() {
    let s = read(HARNESS);
    assert_armed_before_stop(
        &s,
        "root@\"$CAM1_IP\":\"$CAM1_BURN_BIN\"",
        "chmod +x $CAM1_BURN_BIN",
        "cam1 [2/8]",
    );
}

#[test]
fn all_cambox_loop_arms_the_deadman_before_stopping_camera_box_772() {
    let s = read(HARNESS);
    assert_armed_before_stop(
        &s,
        "root@\"$_cip\":\"$_cbin\"",
        "chmod +x $_cbin",
        "[2b/8] ALL_CAMBOX loop",
    );
}

#[test]
fn non_sweep_prep_arms_the_deadman_before_stopping_camera_box_772() {
    let s = read(HARNESS);
    // The non-sweep `_cam2_prep` else arm is the ONLY site putting the arm on the same line between
    // `systemctl stop cam2-painter` and `systemctl stop camera-box; pkill -x camera-box ...; rm -f
    // /tmp/painter.csv`.
    assert_armed_before_stop(
        &s,
        "systemctl stop cam2-painter 2>/dev/null || true; $(camera_box_deadman_arm_cmds",
        "rm -f /tmp/painter.csv;\"",
        "cam2 non-sweep [3/8]",
    );
}

#[test]
fn av_restart_arms_the_deadman_before_stopping_camera_box_772() {
    let s = read(HARNESS);
    assert_armed_before_stop(
        &s,
        "av_restart_record_and_emit_plan()",
        "rm -f /tmp/av-restart-markers.csv;",
        "AV_RESTART",
    );
}
