//! #977/#958 — obs64/AHK Windows-session-visibility probe + pure message parser
//! (scripts/lib/obs-session-visibility.sh), and its wiring into `scripts/recording-e2e.sh`'s
//! `[0/8]` preflight.
//!
//! Root cause (issue 958): a session-0 obs64 (launched via ssh+Invoke-CimMethod) answers OBS
//! WebSocket, serves NDI, and writes a normal log -- so it sails through every EXISTING `[0/8]`
//! term while being invisible to the operator on the console. This gate closes that gap: it reads
//! `Get-Process obs64`/`AutoHotkey64` over `win_ssh_run` (scripts/lib/win-ssh-exec.sh, #703) and
//! FAILS LOUD (exit 1) on anything short of SessionId=1 + a visible window.
//!
//! The lib mirrors `scripts/lib/imag-obs-reachability.sh`'s established shape (a probe-cmd
//! builder + a pure message parser returning "" on healthy) so #977 (this gate) and #979 (the
//! dev1 watchdog) share ONE detector.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/obs-session-visibility.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `body` (which may call its pure functions). Returns stdout.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
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

// ================================================================================================
// obs_session_visibility_probe_ps -- the emitted PowerShell probe text (embedded via $(...) into
// win_ssh_run's 4th arg). Read-only: no writes, no relaunch.
// ================================================================================================

#[test]
fn probe_ps_reads_obs64_session_and_title() {
    let p = run_sourced("obs_session_visibility_probe_ps 1");
    assert!(p.contains("Get-Process obs64"), "must probe obs64. Program:\n{p}");
    assert!(p.contains("SessionId"), "must read SessionId. Program:\n{p}");
    assert!(
        p.contains("MainWindowTitle"),
        "must read MainWindowTitle. Program:\n{p}"
    );
}

#[test]
fn probe_ps_has_ahk_1_also_probes_autohotkey() {
    let p = run_sourced("obs_session_visibility_probe_ps 1");
    assert!(
        p.contains("Get-Process AutoHotkey64"),
        "has_ahk=1 must also probe AutoHotkey64. Program:\n{p}"
    );
}

#[test]
fn probe_ps_has_ahk_0_never_probes_autohotkey() {
    let p = run_sourced("obs_session_visibility_probe_ps 0");
    assert!(
        !p.contains("AutoHotkey64"),
        "has_ahk=0 (stream) must never probe AutoHotkey64. Program:\n{p}"
    );
}

// ================================================================================================
// obs_session_visibility_message -- pure parser. "" = fully visible; anything else = a diagnosis.
// ================================================================================================

// probe_out is passed via a TEMP FILE, never inlined as a bash literal -- an inlined multi-line
// string round-tripped through Rust's `{:?}` Debug escaping does not reconstruct real newlines
// inside bash double quotes (`\n` stays two literal characters, not a newline), which would break
// the sed-based per-line parsing in obs_session_visibility_message. A real file has real bytes.
fn message(probe_out: &str, has_ahk: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = dir.path().join("probe.txt");
    fs::write(&f, probe_out).expect("write probe fixture");
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nprobe_out=\"$(cat \"$PROBE_FILE\")\"\nobs_session_visibility_message \"$probe_out\" {has_ahk}"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("PROBE_FILE", &f)
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

#[test]
fn healthy_strih_probe_is_fully_visible() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=OBS 30.2.3 - Profile: strih\nAHK_COUNT=1\nAHK_SESSION=1\n";
    let msg = message(probe, "1");
    assert_eq!(msg.trim(), "", "a fully healthy strih probe must return an empty message");
}

#[test]
fn healthy_stream_probe_no_ahk_lines_is_fully_visible() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=OBS 30.2.3 - Profile: stream\n";
    let msg = message(probe, "0");
    assert_eq!(msg.trim(), "", "a fully healthy stream (no-AHK) probe must return an empty message");
}

#[test]
fn session_zero_obs_is_invisible() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=0\nOBS_TITLE=OBS 30.2.3\n";
    let msg = message(probe, "0");
    assert!(
        msg.contains("SessionId") && msg.contains("958"),
        "a SessionId=0 obs64 must be reported INVISIBLE, referencing issue 958. msg={msg:?}"
    );
}

#[test]
fn zero_obs_processes_is_invisible() {
    let probe = "OBS_COUNT=0\n";
    let msg = message(probe, "0");
    assert!(
        msg.contains("0") && msg.to_lowercase().contains("count"),
        "obs64 count=0 must be reported. msg={msg:?}"
    );
}

#[test]
fn two_obs_processes_is_invisible() {
    let probe = "OBS_COUNT=2\nOBS_SESSION=1\nOBS_TITLE=x\n";
    let msg = message(probe, "0");
    assert!(
        msg.contains("2"),
        "obs64 count=2 (want exactly 1) must be reported. msg={msg:?}"
    );
}

#[test]
fn empty_window_title_is_invisible() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=\n";
    let msg = message(probe, "0");
    assert!(
        msg.to_lowercase().contains("mainwindowtitle") || msg.to_lowercase().contains("window"),
        "SessionId=1 but no window title must be reported. msg={msg:?}"
    );
}

#[test]
fn strih_ahk_session_zero_is_invisible_even_when_obs_is_fine() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=OBS\nAHK_COUNT=1\nAHK_SESSION=0\n";
    let msg = message(probe, "1");
    assert!(
        msg.contains("AutoHotkey64") && msg.contains("958"),
        "a session-0 AHK on strih must be reported even when OBS itself is healthy. msg={msg:?}"
    );
}

#[test]
fn strih_ahk_missing_is_invisible() {
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=OBS\nAHK_COUNT=0\n";
    let msg = message(probe, "1");
    assert!(
        msg.contains("AutoHotkey64"),
        "a missing AHK on strih must be reported. msg={msg:?}"
    );
}

#[test]
fn stream_ignores_ahk_fields_even_if_present() {
    // has_ahk=0 must never look at AHK_* fields at all -- a healthy obs64 + garbage AHK fields
    // must still read as fully visible on stream.
    let probe = "OBS_COUNT=1\nOBS_SESSION=1\nOBS_TITLE=OBS\nAHK_COUNT=0\nAHK_SESSION=0\n";
    let msg = message(probe, "0");
    assert_eq!(msg.trim(), "", "has_ahk=0 must ignore AHK_* fields entirely. msg={msg:?}");
}

#[test]
fn empty_probe_output_is_invisible_never_a_silent_pass() {
    // #833's "missing tool != measured zero" class, applied here: an ssh/connectivity failure
    // must NEVER be read as VISIBLE by this pure parser -- #977's E2E gate wants to fail loud on
    // it (a probe/connectivity failure at this late a preflight stage is itself a real signal).
    let msg = message("", "0");
    assert!(
        !msg.trim().is_empty(),
        "empty probe output must produce a non-empty diagnosis, never a silent pass"
    );
}

// ================================================================================================
// Wiring into scripts/recording-e2e.sh's [0/8] preflight -- new lines only, no anchor edits.
// ================================================================================================

#[test]
fn recording_e2e_sources_the_lib_and_gates_before_dantesync() {
    let body = read("scripts/recording-e2e.sh");
    assert!(
        body.contains("lib/obs-session-visibility.sh"),
        "recording-e2e.sh must source the new lib"
    );
    let banner_pos = body
        .find("obs64/AHK session-visibility gate")
        .expect("a [0/8] banner announcing the session-visibility gate must exist");
    let dantesync_pos = body
        .find("[0/8] DanteSync NTP+PTP gate")
        .expect("the pre-existing DanteSync banner must still exist");
    assert!(
        banner_pos < dantesync_pos,
        "the session-visibility gate must run before the DanteSync gate (early fail-fast)"
    );
}

#[test]
fn recording_e2e_calls_both_boxes_and_exits_1_on_failure() {
    let body = read("scripts/recording-e2e.sh");
    let window = &body[body
        .find("obs64/AHK session-visibility gate")
        .expect("banner must exist")..];
    let window = &window[..window.len().min(2500)];
    assert!(
        window.contains("obs_session_visibility_probe_ps 1")
            && window.contains("obs_session_visibility_probe_ps 0"),
        "must probe strih (has_ahk=1) AND stream (has_ahk=0). Window:\n{window}"
    );
    assert!(
        window.contains("obs_session_visibility_message"),
        "must parse the probe via the pure message function. Window:\n{window}"
    );
    assert!(
        window.contains("exit 1"),
        "an INVISIBLE box must fail the whole preflight (exit 1). Window:\n{window}"
    );
    assert!(
        window.contains("launch-obs-genlock.sh"),
        "the failure text must embed the exact recovery command. Window:\n{window}"
    );
}
