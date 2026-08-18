//! #788 — the imag-obs-alert-watchdog must distinguish a DELIBERATE operator quit from a real
//! crash and NEVER page the crew for an OBS the operator quit on purpose.
//!
//! Live incident (2026-07-16): the operator quit OBS on imag to test latency and got 4 consecutive
//! false "OBS crashed — relaunched it" alarms + 4 auto-relaunches under their hands. The relaunch
//! half is already solved by the #882 systemd model (imag-obs.service Restart=on-failure leaves a
//! clean exit(0) alone; deliberate stops route through `systemctl --user stop`; the old
//! relaunch-everything imag-obs-watchdog.py stays disabled). The RESIDUAL this file drives is the
//! dev1-side ALERT path (scripts/imag-obs-alert-watchdog.sh), which fires "OBS is DOWN" on ANY
//! OBS_PROCESS_ABSENT with zero operator-quit discrimination.
//!
//! Two PURE seams (in scripts/lib/imag-obs-reachability.sh, the #882 lib the watchdog already
//! sources), Tier-0 testable exactly like obs_watchdog_confirm:
//!   - imag_obs_deliberate_down_probe_cmd [pause_file] [pause_window_s] — a remote bash-snippet
//!     BUILDER (same always-exit-0 command-builder shape as imag_obs_reachability_probe_cmd).
//!   - imag_obs_down_is_deliberate PROBE2_OUT — a PURE token classifier -> deliberate=0|1 + reason.
//!
//! The discriminator SOURCE is systemd's own Restart=on-failure verdict: a clean quit / operator
//! `systemctl stop` reads `LoadState=loaded ActiveState=inactive Result=success`; a crash-loop ends
//! `failed`. The classifier requires LoadState=loaded because a not-found unit ALSO shows
//! inactive/success (live-confirmed systemd quirk) and must NOT be read as a clean quit.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-obs-reachability.sh")
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/imag-obs-alert-watchdog.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ================================================================================================
// PURE classifier: imag_obs_down_is_deliberate PROBE2_OUT -> "deliberate=0|1\nreason=..."
// ================================================================================================

/// Source the lib (its own `set -euo pipefail` runs during the source; `set +e` immediately after,
/// per ci-testing-gotchas.md's sourced-harness rule) and call the classifier with the fixture.
fn deliberate_flag(probe2: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#". "$LIB"; set +e; imag_obs_down_is_deliberate "$PROBE2""#)
        .env("LIB", lib_path())
        .env("PROBE2", probe2)
        .output()
        .expect("run classifier");
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("deliberate="))
        .unwrap_or("")
        .trim()
        .to_string()
}

const CLEAN_QUIT: &str = "OPERATOR_PAUSE=0\nLoadState=loaded\nActiveState=inactive\nResult=success";
const CRASH_FAILED: &str = "OPERATOR_PAUSE=0\nLoadState=loaded\nActiveState=failed\nResult=signal";
const AUTO_RESTART: &str =
    "OPERATOR_PAUSE=0\nLoadState=loaded\nActiveState=activating\nResult=exit-code";
const NOT_FOUND: &str =
    "OPERATOR_PAUSE=0\nLoadState=not-found\nActiveState=inactive\nResult=success";
const PAUSED_OVER_CRASH: &str =
    "OPERATOR_PAUSE=1\nLoadState=loaded\nActiveState=failed\nResult=signal";
const UNIT_QUERY_FAILED: &str = "OPERATOR_PAUSE=0\nUNIT_QUERY=FAILED";

#[test]
fn clean_quit_inactive_success_is_deliberate() {
    // imag-obs.service left inactive/success by Restart=on-failure = a clean exit(0) / operator
    // `systemctl stop` -> a DELIBERATE quit -> suppress the alert.
    assert_eq!(deliberate_flag(CLEAN_QUIT), "1");
}

#[test]
fn crash_failed_is_not_deliberate() {
    // A crash-loop that exhausted Restart= ends `failed` -> a real outage -> ALARM.
    assert_eq!(deliberate_flag(CRASH_FAILED), "0");
}

#[test]
fn auto_restart_in_progress_is_not_deliberate() {
    // Mid Restart=on-failure backoff (activating/exit-code) is a crash in progress, not a quit.
    assert_eq!(deliberate_flag(AUTO_RESTART), "0");
}

#[test]
fn not_found_unit_is_not_deliberate() {
    // systemd quirk (live-confirmed): a not-found unit ALSO shows inactive/success. Requiring
    // LoadState=loaded is what stops a missing/uninstalled unit being misread as a clean quit.
    assert_eq!(deliberate_flag(NOT_FOUND), "0");
}

#[test]
fn operator_pause_overrides_even_a_crash_shaped_state() {
    // The explicit operator override wins regardless of unit state.
    assert_eq!(deliberate_flag(PAUSED_OVER_CRASH), "1");
}

#[test]
fn unit_query_failed_is_not_deliberate() {
    // Could not read the unit (no user bus, etc.) -> fail-safe: alarm ("bez clean markera = pád").
    assert_eq!(deliberate_flag(UNIT_QUERY_FAILED), "0");
}

#[test]
fn empty_probe_is_not_deliberate() {
    assert_eq!(deliberate_flag(""), "0");
}

#[test]
fn bare_reachability_token_is_not_deliberate() {
    // The existing #882 test stub echoes the SAME reachability token for the 2nd probe too; that
    // must classify as NOT deliberate (fail-safe alarm), keeping the #882 tests green.
    assert_eq!(deliberate_flag("OBS_PROCESS_ABSENT"), "0");
}

// ================================================================================================
// PURE probe builder: imag_obs_deliberate_down_probe_cmd [pause_file] [pause_window_s]
// ================================================================================================

fn build_probe(pause_file: &str, window: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#". "$LIB"; set +e; imag_obs_deliberate_down_probe_cmd "$PF" "$WIN""#)
        .env("LIB", lib_path())
        .env("PF", pause_file)
        .env("WIN", window)
        .output()
        .expect("run probe builder");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn probe_cmd_substitutes_params_and_reads_the_user_unit() {
    let body = build_probe("/tmp/my-pause-file", "1800");
    assert!(
        body.contains("/tmp/my-pause-file"),
        "the pause-file path must be substituted at build time; got:\n{body}"
    );
    assert!(
        body.contains("1800"),
        "the pause window must be substituted at build time; got:\n{body}"
    );
    assert!(
        body.contains("systemctl --user show imag-obs.service"),
        "must read the #882 supervised user unit's state; got:\n{body}"
    );
    assert!(
        body.contains("XDG_RUNTIME_DIR"),
        "a non-login ssh session needs XDG_RUNTIME_DIR to reach the user bus; got:\n{body}"
    );
    assert!(
        body.contains("OPERATOR_PAUSE="),
        "must emit the operator-pause token; got:\n{body}"
    );
}

/// Run the emitted remote snippet LOCALLY (no network) and assert the pause-file freshness logic
/// end to end. The snippet must ALWAYS exit 0 (a probe never aborts the caller).
fn run_probe_snippet(pause_file: &str, window: &str) -> (i32, String) {
    let snippet = build_probe(pause_file, window);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .output()
        .expect("run probe snippet");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn pause_line(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("OPERATOR_PAUSE="))
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn probe_snippet_no_pause_file_reports_zero_and_exits_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pf = dir.path().join("absent-pause");
    let (code, out) = run_probe_snippet(pf.to_str().unwrap(), "3600");
    assert_eq!(code, 0, "probe must always exit 0; out:\n{out}");
    assert_eq!(
        pause_line(&out),
        "0",
        "no pause file -> OPERATOR_PAUSE=0; out:\n{out}"
    );
    assert!(
        out.contains("LoadState=") || out.contains("UNIT_QUERY=FAILED"),
        "must emit a unit-state or a UNIT_QUERY=FAILED line; out:\n{out}"
    );
}

#[test]
fn probe_snippet_fresh_pause_file_reports_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pf = dir.path().join("fresh-pause");
    fs::write(&pf, "").expect("touch pause file"); // mtime = now
    let (code, out) = run_probe_snippet(pf.to_str().unwrap(), "3600");
    assert_eq!(code, 0, "probe must always exit 0; out:\n{out}");
    assert_eq!(
        pause_line(&out),
        "1",
        "fresh pause file -> OPERATOR_PAUSE=1; out:\n{out}"
    );
}

#[test]
fn probe_snippet_stale_pause_file_reports_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pf = dir.path().join("stale-pause");
    fs::write(&pf, "").expect("touch pause file");
    // Backdate mtime well beyond a 60 s window so a FORGOTTEN pause can never mask a real crash.
    let touched = Command::new("touch")
        .args(["-d", "@1000000000", pf.to_str().unwrap()])
        .status()
        .expect("backdate");
    assert!(touched.success(), "touch -d must succeed");
    let (code, out) = run_probe_snippet(pf.to_str().unwrap(), "60");
    assert_eq!(code, 0, "probe must always exit 0; out:\n{out}");
    assert_eq!(
        pause_line(&out),
        "0",
        "stale pause file -> OPERATOR_PAUSE=0; out:\n{out}"
    );
}

// ================================================================================================
// Behavioral: run the watchdog main() with a SMART sshpass stub that answers the reachability
// probe and the deliberate-down probe DIFFERENTLY (keyed on the remote command text).
// ================================================================================================

/// The reachability probe's remote text contains OBS_PROCESS_ABSENT + pgrep; the deliberate-down
/// probe's remote text contains OPERATOR_PAUSE. The stub keys on that to return each reply.
fn fake_bin_dir(
    reach_reply: &str,
    probe2_reply: &str,
    notify_marker: &std::path::Path,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    let sshpass = dir.path().join("sshpass");
    fs::write(
        &sshpass,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\ncase \"$last\" in\n  *OPERATOR_PAUSE*) printf '%s\\n' '{probe2_reply}' ;;\n  *) printf '%s\\n' '{reach_reply}' ;;\nesac\nexit 0\n"
        ),
    )
    .expect("write sshpass");
    let mut perm = fs::metadata(&sshpass).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&sshpass, perm).unwrap();

    // python3 stub: latency verify -> exit 0 silently; notify -> record one line to the marker.
    let python3 = dir.path().join("python3");
    fs::write(
        &python3,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *latency_pins_verify*) exit 0 ;;\n  *) echo \"CALLED: $*\" >> {} ;;\nesac\nexit 0\n",
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
    fn new(reach_reply: &str, probe2_reply: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_file = tmp.path().join("state");
        let marker_file = tmp.path().join("notify-calls.log");
        let fake_bin = fake_bin_dir(reach_reply, probe2_reply, &marker_file);
        Harness {
            _tmp: tmp,
            state_file,
            marker_file,
            fake_bin,
        }
    }

    fn run_main(&self) -> (i32, String) {
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
fn deliberate_operator_quit_does_not_alert() {
    // OBS process absent, but the supervised unit is inactive/success -> a deliberate quit.
    let h = Harness::new(
        "OBS_PROCESS_ABSENT",
        "OPERATOR_PAUSE=0\nLoadState=loaded\nActiveState=inactive\nResult=success",
    );
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "a deliberate operator quit (clean imag-obs.service exit) must NOT page the crew (issue 788)"
    );
}

#[test]
fn operator_pause_file_suppresses_the_alert() {
    let h = Harness::new(
        "OBS_PROCESS_ABSENT",
        "OPERATOR_PAUSE=1\nLoadState=loaded\nActiveState=failed\nResult=signal",
    );
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        0,
        "a fresh operator pause file must suppress the alert regardless of unit state (issue 788)"
    );
}

#[test]
fn a_real_crash_still_alerts() {
    // OBS absent AND the unit is `failed` = a genuine crash -> the existing alarm path must fire.
    let h = Harness::new(
        "OBS_PROCESS_ABSENT",
        "OPERATOR_PAUSE=0\nLoadState=loaded\nActiveState=failed\nResult=signal",
    );
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "a genuine crash (unit failed) must still alert exactly once"
    );
}

#[test]
fn port_not_listening_still_alerts_process_is_up_not_a_quit() {
    // OBS process is UP but WS isn't bound -> not a quit -> alarm as before (the deliberate check
    // is only for OBS_PROCESS_ABSENT). The probe2 reply is irrelevant here (never consulted).
    let h = Harness::new("OBS_PORT_NOT_LISTENING", "irrelevant");
    let (code, err) = h.run_main();
    assert_eq!(code, 0, "stderr={err}");
    assert_eq!(
        h.notify_call_count(),
        1,
        "OBS_PORT_NOT_LISTENING (process up) must still alert -- it is not an operator quit"
    );
}

#[test]
fn watchdog_sources_the_reachability_lib_that_defines_the_discriminator() {
    let body = read("scripts/imag-obs-alert-watchdog.sh");
    assert!(
        body.contains("imag_obs_down_is_deliberate"),
        "the watchdog must call the #788 discriminator before alerting"
    );
}
