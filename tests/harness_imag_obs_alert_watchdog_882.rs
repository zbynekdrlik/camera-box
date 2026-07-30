//! #882 — imag-nb OBS liveness alert, DEV1-SIDE (scripts/imag-obs-alert-watchdog.sh).
//!
//! Background: imag-nb's own imag-wallpaper-refresh.sh detects "obs not running" every 5 minutes
//! but structurally CANNOT alert anyone from there — imag-nb has no ~/devel/airuleset checkout
//! and no Discord credentials. Confirmed LIVE (2026-07-30): firing `airuleset.py notify` directly
//! from imag-nb failed every time. This script moves the alert to DEV1 (which has both), mirroring
//! scripts/obs-liveness-watchdog.sh's own #391 topology, but targets imag-nb over SSH via the
//! #882 reachability probe (scripts/lib/imag-obs-reachability.sh) instead of OBS WebSocket
//! GetStats — no render-wedge classification needed, just "is OBS running at all".
//!
//! Pure-shell / content tests — no rig, no OBS, no real ssh (measure() is exercised via a
//! `PROBE_OUT`-shaped fixture passed to the decision path directly, mirroring the #391 sibling's
//! own test style for the parts that don't need a live network call).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/imag-obs-alert-watchdog.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SERVICE_UNIT: &str = "systemd/imag-obs-alert-watchdog.service";
const TIMER_UNIT: &str = "systemd/imag-obs-alert-watchdog.timer";

// ================================================================================================
// Content: reuses the EXISTING #391 decision lib + #882 reachability lib, never a third mechanism.
// ================================================================================================

#[test]
fn watchdog_sources_the_shared_decision_lib_never_a_third_mechanism() {
    let body = read("scripts/imag-obs-alert-watchdog.sh");
    assert!(
        body.contains("lib/obs-watchdog-decision.sh"),
        "must reuse the #391 pure decision functions (obs_watchdog_confirm / \
         obs_watchdog_alert_throttle) -- never invent a second/third alerting mechanism"
    );
    assert!(
        body.contains("lib/imag-obs-reachability.sh"),
        "must reuse the #882 reachability probe (imag_obs_reachability_probe_cmd / \
         imag_obs_reachability_message) -- the SAME lib the [0/8] preflight uses"
    );
}

#[test]
fn watchdog_confirm_threshold_defaults_to_1_not_391s_2() {
    let body = read("scripts/imag-obs-alert-watchdog.sh");
    assert!(
        body.contains("IMAG_OBS_ALERT_CONFIRM_THRESHOLD:-1"),
        "confirm threshold must default to 1 (this timer's own 5-min cadence already means a \
         single miss is real downtime) -- not #391's 2-pass debounce for its 4s tight poll"
    );
}

#[test]
fn watchdog_fires_through_the_same_airuleset_notify_path_as_391() {
    let body = read("scripts/imag-obs-alert-watchdog.sh");
    assert!(
        body.contains("airuleset.py") && body.contains("notify --body"),
        "must fire through the SAME airuleset.py notify path #391 already uses"
    );
}

// ================================================================================================
// Behavioral: run main() with a stubbed `sshpass`/`ssh` on PATH that always returns a fixed probe
// line (simulating imag-nb's real answer without any network call), and a fake python3 stub
// standing in for the real notify call.
// ================================================================================================

fn fake_bin_dir(ssh_reply: &str, notify_marker: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // Fake `sshpass` — the real script invokes `sshpass -p PW ssh ... HOST "<remote script>"`.
    // The stub ignores all args and just echoes the fixed reply, simulating imag-nb's answer.
    let sshpass = dir.path().join("sshpass");
    fs::write(&sshpass, format!("#!/bin/sh\necho '{ssh_reply}'\nexit 0\n")).expect("write sshpass");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();

    // Fake `python3` — stands in for `python3 $NOTIFY notify --body ...`.
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
            .env("IMAG_OBS_ALERT_STATE_FILE", &self.state_file)
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

#[test]
fn obs_reachable_never_alerts() {
    let h = Harness::new("OBS_REACHABLE");
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "a healthy imag-nb must never alert"
    );
}

#[test]
fn obs_process_absent_alerts_on_the_first_pass() {
    let h = Harness::new("OBS_PROCESS_ABSENT");
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "confirm threshold 1 means the FIRST down-pass must alert immediately"
    );
}

#[test]
fn repeated_down_passes_are_throttled() {
    let h = Harness::new("OBS_PROCESS_ABSENT");
    for _ in 0..4 {
        h.run_main();
    }
    assert_eq!(
        h.notify_call_count(),
        1,
        "4 consecutive down-passes with a large throttle window must alert only ONCE"
    );
}

#[test]
fn recovery_then_a_new_outage_alerts_again() {
    let h = Harness::new("OBS_PROCESS_ABSENT");
    h.run_main();
    assert_eq!(h.notify_call_count(), 1);

    // Recovery: obs comes back -> obs_watchdog_confirm resets the confirm counter to 0 on any
    // non-wedged pass (its own documented behavior), so a NEW outage alerts again, not staying
    // silently throttled from the first episode.
    let h2 = Harness::new("OBS_REACHABLE");
    // Reuse the SAME state file to simulate the sequence within one real timer's state.
    std::fs::copy(&h.state_file, &h2.state_file).ok();
    h2.run_main();
    assert_eq!(h2.notify_call_count(), 0, "a healthy pass must never alert");

    let h3 = Harness::new("OBS_PROCESS_ABSENT");
    std::fs::copy(&h2.state_file, &h3.state_file).ok();
    h3.run_main();
    assert_eq!(
        h3.notify_call_count(),
        1,
        "a NEW outage after recovery must alert again"
    );
}

#[test]
fn dry_run_never_calls_notify() {
    let h = Harness::new("OBS_PROCESS_ABSENT");
    let path = format!(
        "{}:{}",
        h.fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"\nDRY_RUN=1\nmain")
        .env("SCRIPT", script())
        .env("IMAG_OBS_ALERT_STATE_FILE", &h.state_file)
        .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
        .env("PATH", path)
        .output()
        .expect("run bash harness");
    assert!(out.status.success());
    assert_eq!(
        h.notify_call_count(),
        0,
        "DRY_RUN=1 must never fire a real notify call"
    );
}

#[test]
fn empty_probe_output_ssh_failure_never_falsely_alerts() {
    let h = Harness::new("");
    let (code, _out, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "an ssh/connectivity failure (empty probe output) must never be read as a false \
         'OBS is down' alert -- the fleet preflight is the authority for connectivity, not this \
         watchdog"
    );
}

// ================================================================================================
// systemd/imag-obs-alert-watchdog.{service,timer} — dev1-side unit files
// ================================================================================================

#[test]
fn unit_files_exist_and_are_wired_correctly() {
    let service = read(SERVICE_UNIT);
    let timer = read(TIMER_UNIT);
    assert!(
        service.contains("imag-obs-alert-watchdog.sh"),
        "the service unit must ExecStart the watchdog script"
    );
    assert!(
        timer.contains("OnUnitActiveSec=5min"),
        "the timer must fire every 5 min, matching imag-wallpaper-refresh.timer's own cadence"
    );
    assert!(
        timer.contains("[Install]") && timer.contains("WantedBy=timers.target"),
        "the timer must be installable"
    );
}
