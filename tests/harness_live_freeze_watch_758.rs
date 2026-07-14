//! #758 item 3 — `scripts/lib/live-freeze-watch.sh`: the in-run freeze watch. Polls the SAME
//! "MV NDI camN" + frozen-camera-gate.py mechanism the [0/8]/[1/8] preflight and [2/8]/[2b/8]
//! sender-bounce re-verify already use, in a BACKGROUND loop during the recording window, and
//! writes a poison-file line per frozen verdict. Proven for real: starts the actual background
//! loop against a FAKE `python3` on PATH (controllable exit code), waits a bounded amount of
//! real wall-clock time, and asserts the poison file / process lifecycle behave correctly.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/live-freeze-watch.sh")
}

fn write_fake_python3(bin_dir: &std::path::Path, body: &str) {
    fs::create_dir_all(bin_dir).unwrap();
    let p = bin_dir.join("python3");
    fs::write(&p, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).unwrap();
}

struct Harness {
    tmp: tempfile::TempDir,
    pid_file: PathBuf,
    poison_file: PathBuf,
}

impl Harness {
    fn new(fake_python3_body: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("bin");
        write_fake_python3(&bin_dir, fake_python3_body);
        Harness {
            pid_file: tmp.path().join("freeze-watch.pid"),
            poison_file: tmp.path().join("freeze-watch-poison.txt"),
            tmp,
        }
    }

    fn start(&self, poll_interval_s: &str) {
        let bin_dir = self.tmp.path().join("bin");
        let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
        let harness = format!(
            "set -uo pipefail\nHERE={:?}\n. {:?}\nlive_freeze_watch_start {:?} {:?} strih-host \
             'MV NDI cam1,MV NDI cam2' /probe-bin-dir {poll_interval_s}",
            self.tmp.path(),
            script(),
            self.pid_file,
            self.poison_file,
        );
        let status = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .env("PATH", path_env)
            .status()
            .expect("start the watch");
        assert!(status.success(), "live_freeze_watch_start must exit 0");
    }

    fn stop(&self) {
        let harness = format!(
            "set -uo pipefail\n. {:?}\nlive_freeze_watch_stop {:?}",
            script(),
            self.pid_file
        );
        let status = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .status()
            .expect("stop the watch");
        assert!(status.success(), "live_freeze_watch_stop must exit 0");
    }

    fn verdict(&self) -> String {
        let harness = format!(
            "set -uo pipefail\n. {:?}\nlive_freeze_watch_verdict {:?}",
            script(),
            self.poison_file
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .output()
            .expect("read verdict");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn a_healthy_run_never_writes_a_poison_line() {
    // Fake frozen-camera-gate.py always PASSES (exit 0).
    let h = Harness::new("exit 0");
    h.start("0.2");
    sleep(Duration::from_millis(700)); // several poll cycles at 0.2s
    h.stop();
    assert_eq!(
        h.verdict(),
        "",
        "a healthy run (gate always PASS) must never write a poison line"
    );
}

#[test]
fn a_frozen_camera_writes_a_named_poison_line_within_one_poll_cycle() {
    // Fake frozen-camera-gate.py always reports FAIL (exit 1) naming a frozen camera.
    let h = Harness::new("echo 'MV NDI cam5' >&2; exit 1");
    h.start("0.2");
    sleep(Duration::from_millis(500)); // several poll cycles well past detection
    h.stop();
    let verdict = h.verdict();
    assert!(
        verdict.contains("[freeze-watch]") && verdict.contains("FROZEN"),
        "verdict={verdict:?}"
    );
    assert!(
        verdict.contains("MV NDI cam5"),
        "poison line must name the frozen camera: {verdict:?}"
    );
}

#[test]
fn stop_actually_kills_the_background_loop() {
    let h = Harness::new("exit 0");
    h.start("0.2");
    let pid: i32 = fs::read_to_string(&h.pid_file)
        .expect("pid file written")
        .trim()
        .parse()
        .expect("pid file contains a PID");
    // Confirm the process is alive right after start (best-effort — /proc is Linux-only, matches
    // this repo's own target platform).
    assert!(
        std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "the background loop's PID must be alive right after start"
    );
    h.stop();
    sleep(Duration::from_millis(200));
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "live_freeze_watch_stop must actually terminate the background loop"
    );
    assert!(!h.pid_file.exists(), "stop must remove the pid file");
}

#[test]
fn stop_is_a_safe_noop_when_the_pid_file_is_already_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing_pid_file = tmp.path().join("never-started.pid");
    let harness = format!(
        "set -uo pipefail\n. {:?}\nlive_freeze_watch_stop {:?}",
        script(),
        missing_pid_file
    );
    let status = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .status()
        .expect("stop with no pid file");
    assert!(
        status.success(),
        "stopping a watch that was never started must be a safe no-op"
    );
}

#[test]
fn verdict_is_empty_when_the_poison_file_is_missing_entirely() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("never-written.txt");
    let harness = format!(
        "set -uo pipefail\n. {:?}\nlive_freeze_watch_verdict {:?}",
        script(),
        missing
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("read verdict on a missing file");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}
