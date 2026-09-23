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
fn watchdog_roster_derives_from_the_obs_session_fleet_facet_1317() {
    // issue 1317 (M4): the production strih is the LINUX strih-lx at .202. The old literal
    // `process_box strih ... 10.77.9.202 1` probed it with the Windows PowerShell session probe
    // and failed on every pass ("strih: ERROR: no probe output"). The roster now derives from
    // the fleet list's `obs-session` facet (windows-genlock boxes only).
    let body = read("scripts/obs-session-watchdog.sh");
    assert!(
        body.contains("lib/obs-fleet.sh") && body.contains("obs_fleet_boxes obs-session"),
        "the roster must derive from the obs-fleet `obs-session` facet"
    );
    assert!(
        body.contains("OBS_SESSION_WATCHDOG_BOXES"),
        "the roster keeps a byte-compatible env override (the fleet <X>_BOXES convention)"
    );
    for gone in ["process_box strih", "10.77.9.202", "STRIH_PW", "STRIH_HOST"] {
        assert!(
            !body.contains(gone),
            "the retired Windows strih literal `{gone}` must be gone from the watchdog"
        );
    }
}

/// Source the watchdog (main is guarded) and print its resolved probe targets, one
/// `name host has_ahk` line per box that will be probed this pass.
fn targets(env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(". \"$SCRIPT\"\nobs_session_targets")
        .env("SCRIPT", script())
        .env_remove("OBS_SESSION_WATCHDOG_BOXES")
        .env_remove("STREAM_HOST")
        .env_remove("RESOLUME_HOST")
        .env_remove("OBS_FLEET");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "obs_session_targets failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn targets_are_windows_genlock_only_with_per_box_ahk_1317() {
    // stream has no AHK watcher; resolume runs the NL_STARTUP.ahk v2 safe-loop (has_ahk=1, the
    // same fact deploy-genlock-fleet.sh / launch-obs-genlock.sh carry).
    assert_eq!(
        targets(&[("OBS_FLEET_HOME", "stream resolume")]),
        "stream 10.77.9.204 0\nresolume resolume.lan 1"
    );
}

#[test]
fn targets_skip_the_traveling_resolume_while_away_1317() {
    // A traveling box that is away is never probed (never a false "invisible" verdict).
    assert_eq!(
        targets(&[("OBS_FLEET_HOME", "stream")]),
        "stream 10.77.9.204 0"
    );
}

#[test]
fn targets_never_include_a_linux_genlock_box_even_via_override_1317() {
    // Defense in depth: even an override naming strih-lx never sends the PowerShell probe to it.
    assert_eq!(
        targets(&[
            (
                "OBS_SESSION_WATCHDOG_BOXES",
                "strih-lx|10.77.9.202 stream|10.77.9.204"
            ),
            ("OBS_FLEET_HOME", "strih-lx stream"),
        ]),
        "stream 10.77.9.204 0"
    );
}

#[test]
fn targets_keep_the_per_box_host_env_overrides_1317() {
    assert_eq!(
        targets(&[
            ("OBS_FLEET_HOME", "stream resolume"),
            ("STREAM_HOST", "192.0.2.9"),
            ("RESOLUME_HOST", "192.0.2.7"),
        ]),
        "stream 192.0.2.9 0\nresolume 192.0.2.7 1"
    );
}

#[test]
fn main_never_ssh_probes_the_linux_strih_1317() {
    // Behavioral: a stub sshpass records every target it is asked to reach. A full pass must reach
    // stream + the home resolume, and NEVER the Linux strih-lx address.
    let tmp = tempfile::tempdir().expect("tempdir");
    let calls = tmp.path().join("sshpass-calls.log");
    let sshpass = tmp.path().join("sshpass");
    fs::write(
        &sshpass,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\nprintf '%b' '{HEALTHY}'\nexit 0\n",
            calls.display()
        ),
    )
    .expect("write sshpass stub");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();
    let path = format!(
        "{}:{}",
        tmp.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$SCRIPT\"\nDRY_RUN=1\nmain")
        .env("SCRIPT", script())
        .env("OBS_SESSION_WATCHDOG_STATE_FILE", tmp.path().join("state"))
        .env("OBS_FLEET_HOME", "stream resolume")
        .env("AIRULESET_NOTIFY", "/dev/null/does-not-matter")
        .env("PATH", path)
        .output()
        .expect("run bash harness");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr={err}");
    let log = fs::read_to_string(&calls).unwrap_or_default();
    assert!(log.contains("10.77.9.204"), "stream must be probed: {log}");
    assert!(
        log.contains("resolume.lan"),
        "a home resolume must be probed: {log}"
    );
    assert!(
        !log.contains("10.77.9.202"),
        "the Linux strih-lx must NEVER get the Windows PowerShell probe: {log}"
    );
    assert!(
        !err.contains("strih"),
        "no strih box may appear in a pass any more: {err}"
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
            // issue 1317: the roster is the fleet `obs-session` facet (stream + resolume when
            // home) -- force both home so the pass never depends on live resolume.lan I/O.
            .env("OBS_FLEET_HOME", "stream resolume")
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

// #958 follow-up: this watchdog ALWAYS probes over win_ssh_run (ssh from dev1) -- i.e. ALWAYS
// cross-session from obs64's real console session. HEALTHY therefore uses an EMPTY title (the
// real ssh-probe shape, per the supervisor's live root-cause on issue 958: MainWindowTitle is
// structurally unreadable cross-session even on a perfectly healthy box) to prove the watchdog no
// longer false-alerts forever on a healthy fleet. INVISIBLE keeps a genuine SessionId mismatch
// (0 vs the active session 1), which must still alert regardless of title/session context.
const HEALTHY: &str =
    "ACTIVE_SESSION=1\\nOWN_SESSION=0\\nOBS_COUNT=1\\nOBS_SESSION=1\\nOBS_TITLE=\\nAHK_COUNT=1\\nAHK_SESSION=1\\n";
const INVISIBLE: &str =
    "ACTIVE_SESSION=1\\nOWN_SESSION=0\\nOBS_COUNT=1\\nOBS_SESSION=0\\nOBS_TITLE=OBS\\n";

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
            .env("OBS_FLEET_HOME", "stream resolume")
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
