//! issue 1290 — pure classifier for `scripts/lib/rig-mode-state.sh`:
//! `rig_mode_from_painter_snapshot` maps the cam2 painter probe snapshot to EVENT / TEST / UNKNOWN.
//!
//! The dev1 splitter-port watchdog (issue 739) pages a DEAD_PORT the instant one cambox reads
//! grayscale while a sibling reads colour — a TEST-rig premise (ONE camera through an HDMI splitter
//! to every cambox). In EVENT/production each cambox has its OWN camera, so a camera-less cambox is
//! legitimately black and the sibling anchor is false. This lib is the shared, durable EVENT-mode
//! discriminator (reused from the optical-chain painter signal: pidfile OR `cam2-painter.service`
//! is-enabled), with a 3-state fail-safe: UNKNOWN (cam2 unreadable) must behave as today (never
//! silence a real TEST-mode fault), EVENT must never page.
//!
//! Method: the lib is source-only (self-sources optical-chain-health.sh so painter_expected has ONE
//! definition), so we source it and call the pure function with a canned snapshot. No rig, no ssh.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let l = manifest_dir().join("scripts/lib/rig-mode-state.sh");
    assert!(l.exists(), "{} not found", l.display());
    l
}

/// Source the lib and classify `snapshot`; returns the verdict token (stdout, trimmed).
fn classify(snapshot: &str) -> String {
    // `$0` is the placeholder "bash", `$1` is the snapshot (may be multi-line — one arg).
    let body = "set -uo pipefail\n. \"$LIB\"\nrig_mode_from_painter_snapshot \"$1\"\n";
    let out = Command::new("bash")
        .arg("-c")
        .arg(body)
        .arg("bash")
        .arg(snapshot)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "rig_mode_from_painter_snapshot exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// Snapshots have the shape the probe snippet emits: a reachability sentinel + four KEY|value lines.
const EVENT_SNAP: &str =
    "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0";
const TEST_SNAP_SVC: &str =
    "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|1\nSVC_ACTIVE|1";
const TEST_SNAP_PID: &str =
    "RIG_MODE_PROBE_OK\nPID_PRESENT|1\nPID_ALIVE|1\nSVC_ENABLED|0\nSVC_ACTIVE|0";
// A painter that is ACTIVE but DISABLED (an E2E `systemctl start` on a rig last set to EVENT leaves
// the unit active+disabled until a reboot / `rig-mode.sh test`) -- a running painter, so TEST.
const TEST_SNAP_ACTIVE_DISABLED: &str =
    "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|1";

#[test]
fn event_mode_when_painter_disabled_and_pidfile_absent() {
    // rig-mode.sh event STOPS+DISABLES cam2-painter.service (#892) and removes the pidfile -> a
    // provable EVENT mode. TEST-premise verdicts must be suppressed there.
    assert_eq!(classify(EVENT_SNAP), "EVENT");
}

#[test]
fn test_mode_when_cam2_painter_service_enabled() {
    // The DURABLE, non-staling TEST signal: the permanent service is enabled (rig-mode.sh test's
    // steady state since #1008/#937), even if momentarily inactive during a restart.
    assert_eq!(classify(TEST_SNAP_SVC), "TEST");
}

#[test]
fn test_mode_when_transient_pidfile_present() {
    // The at-mode-set verification window: the transient painter pidfile is present.
    assert_eq!(classify(TEST_SNAP_PID), "TEST");
}

#[test]
fn unknown_when_cam2_unreachable_empty_snapshot() {
    // ssh failed / box off -> empty -> no reachability sentinel -> UNKNOWN. The whole point of the
    // 3-state design: an unreadable mode must NEVER be read as EVENT (which would silence a real
    // TEST-mode dead port), and must behave exactly as today (page).
    assert_eq!(classify(""), "UNKNOWN");
}

#[test]
fn unknown_when_snapshot_missing_reachability_sentinel() {
    // Defensive: a partial read that somehow carried painter lines but not the RIG_MODE_PROBE_OK
    // sentinel is UNKNOWN, never a false EVENT.
    assert_eq!(classify("PID_PRESENT|0\nSVC_ENABLED|0"), "UNKNOWN");
}

#[test]
fn test_mode_when_painter_active_but_service_disabled() {
    // The active-but-DISABLED painter (a running QR after an E2E `systemctl start` on a rig last set
    // to EVENT). A running painter is never a clean broadcast (#892), so it must read TEST -- NOT a
    // false EVENT that would silence real DEAD_PORTs for days. painter_expected=0 but painter_alive=1.
    assert_eq!(classify(TEST_SNAP_ACTIVE_DISABLED), "TEST");
}

#[test]
fn unknown_when_sentinel_present_but_all_painter_lines_missing() {
    // The sentinel alone is NOT proof of a readable mode: an ssh that connected (echoed the sentinel)
    // but was torn down before the probe body ran carries no painter fields -> UNKNOWN, never EVENT.
    assert_eq!(classify("RIG_MODE_PROBE_OK"), "UNKNOWN");
}

#[test]
fn unknown_when_partial_snapshot_missing_the_service_lines() {
    // A truncated read: sentinel + pidfile lines present, but ssh was torn down before the
    // `systemctl` reads -> SVC_ENABLED / SVC_ACTIVE missing -> UNKNOWN, never a false EVENT.
    assert_eq!(
        classify("RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0"),
        "UNKNOWN"
    );
}

#[test]
fn unknown_when_systemctl_hiccup_emits_question_mark() {
    // A systemd manager no-answer hiccup (empty is-enabled / is-active output) emits `?`, not a
    // definite 0/1 -> UNKNOWN. A hiccup must never be misread as a provable EVENT (fail-DEADLY);
    // this is the `?`-on-empty state the shared optical-chain snippet now emits (#1290).
    assert_eq!(
        classify("RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|?\nSVC_ACTIVE|?"),
        "UNKNOWN"
    );
}
