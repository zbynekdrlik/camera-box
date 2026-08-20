//! #1128 — driver-level guard for `scripts/grabber-stuck-alert-watchdog.sh`'s `main()`: the
//! per-box probe -> parse -> classify -> action wiring, the 2-pass confirm, the ONE-ping-per-episode
//! throttle (discord-volume-near-zero), and the recovery ping on return-to-OK. The pure lib is
//! pinned by `tests/harness_grabber_stuck_health_1128.rs`; this file pins the IMPURE driver that
//! composes it.
//!
//! Method (same as `tests/harness_splitter_port_watchdog_739.rs`): the driver guards `main` behind
//! `[[ "${BASH_SOURCE[0]}" == "$0" ]]`, so sourcing it only DEFINES its functions. We source it in
//! `--dry-run`, override `probe_box` with a canned per-IP output and `sshpass` with a no-op (so the
//! tool preflight passes with no real ssh), run `main` N times against a per-test temp state file,
//! and assert on the log (stderr). No rig, no network, no notify.

use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/grabber-stuck-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the watchdog in dry-run, override `probe_box`/`sshpass`, run a scripted sequence of
/// `main` calls (`seq` is raw bash inserted after the overrides — e.g. `"main\nmain\n"`, optionally
/// flipping a state var between passes). Returns stderr (the `log()` stream). A per-test tempdir
/// isolates the state file (never a shared host path).
fn run_driver(probe_cases: &str, seq: &str) -> String {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("grabber-stuck.state");
    let body = format!(
        "set -uo pipefail\n\
         export CAMERA_ACTIVE_SET='cam1'\n\
         export GRABBER_STUCK_WATCH_STATE_FILE='{state}'\n\
         . \"$SCRIPT\" --dry-run\n\
         sshpass() {{ :; }}\n\
         probe_box() {{ {cases}; }}\n\
         {seq}",
        state = state.display(),
        cases = probe_cases,
        seq = seq,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&body)
        .env("SCRIPT", script())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "watchdog main() exited non-zero:\n{stderr}"
    );
    stderr
}

// A canned box output: PROBE_OK + a `#1128 grabber STUCK` marker, PROBE_OK + a healthy Streaming
// line, or empty (ssh fail). `$STUCK` (set in the seq) flips STUCK vs OK.
const STUCK_CASE: &str = r"printf 'PROBE_OK\nWARN #1128 grabber STUCK: /dev/video0 captured 62.50 fps (>= 61.5 fps over-rate floor) WITH persistent corrupted frames (4/window) sustained for 6 consecutive report windows (~30s)\n'";
const OK_CASE: &str =
    r"printf 'PROBE_OK\nINFO Streaming: 30.0 fps emitted / 60.0 fps captured (0 corrupted)\n'";
const SSH_FAIL: &str = r"printf ''";

#[test]
fn stuck_box_pages_once_after_the_two_pass_confirm() {
    // Pass 1: STUCK but not yet confirmed (act=0) -> holds, no alert.
    // Pass 2: confirmed (act=1) -> pages ONCE.
    let log = run_driver(STUCK_CASE, "main\nmain\n");
    assert!(
        log.contains("holding"),
        "first STUCK pass must hold (2-pass confirm): {log}"
    );
    assert_eq!(
        log.matches("WOULD alert").count(),
        1,
        "exactly one alert after the confirm: {log}"
    );
    assert!(
        log.contains("~62.50 fps"),
        "alert carries the captured fps parsed from the marker: {log}"
    );
}

#[test]
fn a_chronic_stuck_box_never_re_pings_one_ping_per_episode() {
    // discord-volume-near-zero: the SAME box staying stuck for many passes pages exactly ONCE (the
    // huge throttle suppresses every subsequent pass), never a repeated alert of a chronic state.
    let log = run_driver(STUCK_CASE, "main\nmain\nmain\nmain\nmain\n");
    assert_eq!(
        log.matches("WOULD alert").count(),
        1,
        "chronic stuck must page exactly once across many passes: {log}"
    );
    assert!(
        log.contains("suppressed by throttle"),
        "later passes are throttle-suppressed: {log}"
    );
}

#[test]
fn recovery_ping_fires_once_when_a_paged_box_returns_to_ok() {
    // STUCK (paged) then a healthy window -> one recovery ping; a subsequent OK pass does not re-fire.
    // Two stuck passes page once; then probe_box is redefined to the healthy output and two OK passes
    // fire exactly one recovery (the second OK pass has already cleared the recovery latch).
    let seq = format!("main\nmain\nprobe_box() {{ {OK_CASE}; }}\nmain\nmain\n");
    let log = run_driver(STUCK_CASE, &seq);
    assert_eq!(
        log.matches("WOULD alert").count(),
        1,
        "paged once while stuck: {log}"
    );
    assert_eq!(
        log.matches("WOULD send recovery").count(),
        1,
        "exactly one recovery ping on return-to-OK: {log}"
    );
}

#[test]
fn a_transient_nodata_blip_mid_episode_does_not_produce_a_second_page() {
    // #1128 review 🔵3 (discord-volume-near-zero): a chronically-stuck box that suffers ONE
    // transient ssh failure (NODATA) between stuck passes must still page exactly ONCE for the
    // ongoing episode — NODATA clears only the confirm counter, never the one-ping episode latch.
    // stuck x2 (page) -> NODATA blip -> stuck x2 (re-confirms) -> still ONE alert total.
    let seq = format!("main\nmain\nprobe_box() {{ printf ''; }}\nmain\nprobe_box() {{ {STUCK_CASE}; }}\nmain\nmain\n");
    let log = run_driver(STUCK_CASE, &seq);
    assert!(
        log.contains("verdict=NODATA"),
        "the blip pass reads NODATA: {log}"
    );
    assert_eq!(
        log.matches("WOULD alert").count(),
        1,
        "a mid-episode NODATA blip must not produce a second page: {log}"
    );
}

#[test]
fn unreachable_box_never_pages_and_never_false_recovers() {
    // NODATA (ssh fail) is "nothing to decide" — never an alert, never a recovery.
    let log = run_driver(SSH_FAIL, "main\nmain\n");
    assert!(log.contains("verdict=NODATA"), "ssh fail -> NODATA: {log}");
    assert_eq!(
        log.matches("WOULD alert").count(),
        0,
        "an unreachable box must never page: {log}"
    );
    assert_eq!(
        log.matches("WOULD send recovery").count(),
        0,
        "an unreachable box must never emit a recovery: {log}"
    );
}
