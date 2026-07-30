//! #882 — behavioral tests for imag-wallpaper-refresh.sh's alert_obs_down / clear_alert_state.
//! Sources the REAL script (its `[[ "${BASH_SOURCE[0]}" == "$0" ]]` guard skips `main`, mirroring
//! scripts/obs-liveness-watchdog.sh's convention) and drives the functions directly with a fake
//! `python3` stub standing in for `airuleset.py notify` — no rig, no OBS, no real Discord call.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/imag-wallpaper-refresh.sh")
}

/// A fake `python3` on PATH that just appends its own argv to a marker file every time it's
/// "called" (standing in for `python3 $NOTIFY notify --body ...`) and always exits 0.
fn fake_python_dir(marker: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("python3");
    fs::write(
        &p,
        format!(
            "#!/bin/sh\necho \"CALLED: $*\" >> {}\nexit 0\n",
            marker.display()
        ),
    )
    .expect("write python3 stub");
    let mut perm = fs::metadata(&p).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).expect("chmod stub");
    dir
}

struct Harness {
    _tmp: tempfile::TempDir,
    state_file: PathBuf,
    marker_file: PathBuf,
    fake_bin: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_file = tmp.path().join("state");
        let marker_file = tmp.path().join("notify-calls.log");
        let fake_bin = fake_python_dir(&marker_file);
        Harness {
            _tmp: tmp,
            state_file,
            marker_file,
            fake_bin,
        }
    }

    /// Run `body` (a bash snippet) after sourcing the real script with this harness's fixture
    /// env — PATH prepended with the fake python3 stub so the real one on this dev box is never
    /// invoked for the notify call.
    fn run(&self, body: &str) -> (i32, String, String) {
        let script_text = format!(
            "set -uo pipefail\n. \"$SCRIPT\"\n{body}\n"
        );
        let path = format!(
            "{}:{}",
            self.fake_bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&script_text)
            .env("SCRIPT", script())
            .env("IMAG_WALLPAPER_STATE_FILE", &self.state_file)
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
fn sourcing_the_script_never_runs_main() {
    let h = Harness::new();
    let (code, _out, err) = h.run("echo sourced-ok");
    assert_eq!(code, 0, "sourcing must not error\nstderr={err}");
    assert_eq!(h.notify_call_count(), 0, "sourcing alone must never fire a notify call");
}

#[test]
fn first_obs_down_pass_alerts_immediately_confirm_threshold_is_1() {
    // Unlike issue 391's 2-consecutive-pass confirm (a 4s tight-poll cadence needing debounce),
    // this timer's own cadence is 5 minutes — a SINGLE miss is already real downtime, so the
    // very first alert_obs_down call must fire, not wait for a second pass.
    let h = Harness::new();
    let (code, _out, err) = h.run("alert_obs_down");
    assert_eq!(code, 0, "alert_obs_down must exit 0\nstderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "the FIRST down-pass must alert immediately (confirm threshold 1, not #391's 2)"
    );
}

#[test]
fn repeated_down_passes_are_throttled_not_re_alerted_every_time() {
    let h = Harness::new();
    for _ in 0..5 {
        let (code, _out, err) = h.run("alert_obs_down");
        assert_eq!(code, 0, "alert_obs_down must exit 0\nstderr={err}");
    }
    assert_eq!(
        h.notify_call_count(),
        1,
        "5 consecutive down-passes with a large throttle window must alert only ONCE, not spam \
         every pass (this is exactly the 14-times-in-70-minutes silence #882 investigates -- the \
         fix is ONE alert, not silence AND not a flood)"
    );
}

#[test]
fn recovery_clears_state_so_the_next_outage_alerts_promptly() {
    let h = Harness::new();
    // First outage: alerts once.
    h.run("alert_obs_down");
    assert_eq!(h.notify_call_count(), 1);
    // Recovery.
    let (code, _out, err) = h.run("clear_alert_state");
    assert_eq!(code, 0, "clear_alert_state must exit 0\nstderr={err}");
    // A SECOND, later outage must alert again immediately -- not stay silent because of a stale
    // throttle counter inherited from the first (already-recovered) episode.
    let (code2, _out2, err2) = h.run("alert_obs_down");
    assert_eq!(code2, 0, "stderr={err2}");
    assert_eq!(
        h.notify_call_count(),
        2,
        "a NEW outage after a recovery must alert again, not stay silently throttled forever"
    );
}
