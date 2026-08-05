//! #979 — obs64/AHK Windows-session-visibility watchdog, DEV1-SIDE (scripts/obs-session-watchdog.sh).
//!
//! Background: #977's E2E gate only runs on a push -- the rig can degrade BETWEEN pushes (issue
//! 958's real incident: obs64 sat invisible in Windows session 0 for ~3.5h before the user found
//! it manually). This script is the #391/#882 dev1-timer topology applied to the SAME session-
//! visibility probe #977/#978 use (scripts/lib/obs-session-visibility.sh, reused verbatim -- never
//! a second detector), polling BOTH broadcast boxes over win_ssh_run every few minutes and firing
//! ONE deduped Discord alert per box the moment either goes invisible.
//!
//! Pure-shell / content tests -- no rig, no real ssh (win_ssh_run's own `sshpass` call is stubbed
//! on PATH, mirroring harness_imag_obs_alert_watchdog_882.rs's own test style).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/obs-session-watchdog.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SERVICE_UNIT: &str = "systemd/obs-session-watchdog.service";
const TIMER_UNIT: &str = "systemd/obs-session-watchdog.timer";

// ================================================================================================
// Content: reuses the EXISTING #391 decision lib + #977 session-visibility lib, never a third
// mechanism.
// ================================================================================================

#[test]
fn watchdog_sources_the_shared_libs_never_a_third_mechanism() {
    let body = read("scripts/obs-session-watchdog.sh");
    assert!(
        body.contains("lib/obs-watchdog-decision.sh"),
        "must reuse the #391 pure decision functions (obs_watchdog_confirm / \
         obs_watchdog_alert_throttle) -- never invent a second/third alerting mechanism"
    );
    assert!(
        body.contains("lib/obs-session-visibility.sh"),
        "must reuse the #977/#978 session-visibility probe -- the SAME detector the E2E gate uses"
    );
    assert!(
        body.contains("lib/win-ssh-exec.sh"),
        "must reuse win_ssh_run (#703) -- never a hand-rolled ssh invocation"
    );
}

#[test]
fn watchdog_fires_through_the_same_airuleset_notify_path_as_391() {
    let body = read("scripts/obs-session-watchdog.sh");
    assert!(
        body.contains("airuleset.py") && body.contains("notify --body"),
        "must fire through the SAME airuleset.py notify path #391/#882 already use"
    );
}

#[test]
fn watchdog_state_file_default_differs_from_391s_own() {
    let body = read("scripts/obs-session-watchdog.sh");
    assert!(
        body.contains("camera-box-obs-session-watchdog.state"),
        "must use its OWN default state file, distinct from #391's \
         camera-box-obs-watchdog.state -- otherwise the two watchdogs' per-box \
         '<box>_confirm'/'<box>_alert_sig' keys collide and corrupt each other's state"
    );
}

#[test]
fn watchdog_probes_both_boxes_with_correct_has_ahk() {
    let body = read("scripts/obs-session-watchdog.sh");
    assert!(
        body.contains("process_box strih") && body.contains("process_box stream"),
        "main() must process both strih and stream"
    );
    // strih=has_ahk=1, stream=has_ahk=0 -- find the two process_box call lines and check the
    // trailing arg on each.
    let strih_line = body
        .lines()
        .find(|l| l.trim_start().starts_with("process_box strih"))
        .expect("a process_box strih call line must exist");
    let stream_line = body
        .lines()
        .find(|l| l.trim_start().starts_with("process_box stream"))
        .expect("a process_box stream call line must exist");
    assert!(
        strih_line.trim_end().ends_with('1'),
        "strih must be called with has_ahk=1. line={strih_line:?}"
    );
    assert!(
        stream_line.trim_end().ends_with('0'),
        "stream must be called with has_ahk=0. line={stream_line:?}"
    );
}

// ================================================================================================
// Behavioral: run main() with a stubbed `sshpass`/`ssh` on PATH that always returns a fixed probe
// reply (simulating both boxes' real answer without any network call), and a fake python3 stub
// standing in for the real notify call.
// ================================================================================================

fn fake_bin_dir(ssh_reply: &str, notify_marker: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let sshpass = dir.path().join("sshpass");
    fs::write(
        &sshpass,
        format!("#!/bin/sh\nprintf '%b' '{ssh_reply}'\nexit 0\n"),
    )
    .expect("write sshpass");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();

    let python3 = dir.path().join("python3");
    fs::write(
        &python3,
        format!(
            "#!/bin/sh\necho \"CALLED: $*\" >> {}\nexit 0\n",
            notify_marker.display()
        ),
    )
    .expect("write python3 stub");
    let mut perm2 = fs::metadata(&python3).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm2, 0o755);
    fs::set_permissions(&python3, perm2).unwrap();

    dir
}

struct Harness {
    _tmp: tempfile::TempDir,
    state_file: PathBuf,
    marker_file: PathBuf,
    fake_bin: tempfile::TempDir,
}

impl Harness {
    fn new(ssh_reply: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_file = tmp.path().join("state");
        let marker_file = tmp.path().join("notify-calls.log");
        let fake_bin = fake_bin_dir(ssh_reply, &marker_file);
        Harness {
            _tmp: tmp,
            state_file,
            marker_file,
            fake_bin,
        }
    }

    fn run_main(&self) -> (i32, String, String) {
        let path = format!(
            "{}:{}",
            self.fake_bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$SCRIPT\"\nmain")
            .env("SCRIPT", script())
            .env("OBS_SESSION_WATCHDOG_STATE_FILE", &self.state_file)
            .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
            .env("PATH", path)
            .output()
            .expect("run bash harness");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn notify_call_count(&self) -> usize {
        fs::read_to_string(&self.marker_file)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

const HEALTHY: &str =
    "OBS_COUNT=1\\nOBS_SESSION=1\\nOBS_TITLE=OBS\\nAHK_COUNT=1\\nAHK_SESSION=1\\n";
const INVISIBLE: &str = "OBS_COUNT=1\\nOBS_SESSION=0\\nOBS_TITLE=OBS\\n";

#[test]
fn both_boxes_healthy_never_alerts() {
    let h = Harness::new(HEALTHY);
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "both boxes healthy must never alert"
    );
}

#[test]
fn both_invisible_alerts_after_confirm_threshold_default_2() {
    let h = Harness::new(INVISIBLE);
    // pass 1: confirm=1 for each box, no alert yet (default threshold 2)
    let (code1, _out1, err1) = h.run_main();
    assert_eq!(code1, 0, "stderr={err1}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "first invisible pass must not alert yet"
    );
    // pass 2 (same state file): confirm=2 for each box, both alert -- one call per box
    let (code2, _out2, err2) = h.run_main();
    assert_eq!(code2, 0, "stderr={err2}");
    assert_eq!(
        h.notify_call_count(),
        2,
        "the SECOND consecutive invisible pass must alert for BOTH boxes (2 calls)"
    );
}

#[test]
fn repeated_down_passes_are_throttled() {
    let h = Harness::new(INVISIBLE);
    for _ in 0..6 {
        h.run_main();
    }
    assert_eq!(
        h.notify_call_count(),
        2,
        "6 consecutive down-passes with a large throttle window must alert only ONCE per box (2 total)"
    );
}

#[test]
fn recovery_then_a_new_outage_alerts_again() {
    let h = Harness::new(INVISIBLE);
    h.run_main();
    h.run_main();
    assert_eq!(h.notify_call_count(), 2);

    let h2 = Harness::new(HEALTHY);
    std::fs::copy(&h.state_file, &h2.state_file).ok();
    h2.run_main();
    assert_eq!(h2.notify_call_count(), 0, "a healthy pass must never alert");

    let h3 = Harness::new(INVISIBLE);
    std::fs::copy(&h2.state_file, &h3.state_file).ok();
    h3.run_main();
    assert_eq!(
        h3.notify_call_count(),
        0,
        "confirm counter reset by the healthy pass -- a single new-outage pass must not yet alert"
    );
    let h4 = Harness::new(INVISIBLE);
    std::fs::copy(&h3.state_file, &h4.state_file).ok();
    h4.run_main();
    assert_eq!(
        h4.notify_call_count(),
        2,
        "the SECOND pass of the new outage must alert again for both boxes"
    );
}

#[test]
fn dry_run_never_calls_notify() {
    let h = Harness::new(INVISIBLE);
    let path = format!(
        "{}:{}",
        h.fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    for _ in 0..3 {
        Command::new("bash")
            .arg("-c")
            .arg(". \"$SCRIPT\"\nDRY_RUN=1\nmain")
            .env("SCRIPT", script())
            .env("OBS_SESSION_WATCHDOG_STATE_FILE", &h.state_file)
            .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
            .env("PATH", &path)
            .output()
            .expect("run bash harness");
    }
    assert_eq!(
        h.notify_call_count(),
        0,
        "DRY_RUN=1 must never fire a real notify call, even past the confirm threshold"
    );
}

#[test]
fn empty_probe_output_ssh_failure_never_falsely_alerts() {
    let h = Harness::new("");
    for _ in 0..3 {
        h.run_main();
    }
    assert_eq!(
        h.notify_call_count(),
        0,
        "an ssh/connectivity failure (empty probe output) must never be read as a false \
         INVISIBLE alert -- the fleet's own reachability preflight is the authority for \
         connectivity, not this watchdog"
    );
}

// ================================================================================================
// systemd/obs-session-watchdog.{service,timer} — dev1-side unit files, SHIPS DISABLED
// ================================================================================================

#[test]
fn unit_files_exist_and_are_wired_correctly() {
    let service = read(SERVICE_UNIT);
    let timer = read(TIMER_UNIT);
    assert!(
        service.contains("obs-session-watchdog.sh"),
        "the service unit must ExecStart the watchdog script"
    );
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "the timer must be installable"
    );
    assert!(
        service.contains("SHIPS DISABLED") || timer.contains("SHIPS DISABLED"),
        "must document that this ships disabled by default (supervisor installs + live-verifies)"
    );
}

#[test]
fn readme_documents_ships_disabled_and_install_procedure() {
    let readme = read("systemd/obs-session-watchdog.README.md");
    assert!(
        readme.contains("SHIPS DISABLED") || readme.to_lowercase().contains("ships disabled"),
        "README must state this ships disabled by default"
    );
    assert!(
        readme.contains("systemctl --user"),
        "README must document the supervisor install procedure"
    );
}
