//! #882 — the imag render-health preflight's restart-and-settle warm-up window.
//!
//! Background: after imag-nb's OBS was restarted at 09:21 (recovering from the segfault this
//! ticket investigates), the very next E2E gate run failed `[1/8]` render-health at window 1/5 —
//! yet the SAME gate binary measured the box at a clean 60.00fps/4.47ms/0% skip twenty minutes
//! later. The box was still settling (NDI receivers locking, shaders warming up) when window 1
//! was sampled. `render_health_window_outcome` (scripts/lib/render-health-warmup.sh) decides
//! whether a failed window aborts the sweep: window 1 is a non-counting warm-up (a failure there
//! is tolerated), windows 2..N stay exactly as strict as before (any failure there aborts).
//!
//! Pure-shell tests — no rig, no OBS. Mirrors the sourcing pattern in
//! tests/harness_obs_liveness_watchdog.rs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/render-health-warmup.sh")
}

fn outcome(window_index: &str, rc: &str) -> HashMap<String, String> {
    let script = format!(
        r#"set -u
. "$LIB"
render_health_window_outcome {window_index} {rc}
"#
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("LIB", lib_path())
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "render_health_window_outcome must exit 0\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

#[test]
fn window_1_failure_is_tolerated_as_warmup() {
    let m = outcome("1", "1");
    assert_eq!(
        m.get("outcome").map(String::as_str),
        Some("WARMUP"),
        "window 1 failing must be a non-counting WARMUP, never FAIL — got {m:?}"
    );
}

#[test]
fn window_1_pass_is_still_a_plain_pass() {
    let m = outcome("1", "0");
    assert_eq!(
        m.get("outcome").map(String::as_str),
        Some("PASS"),
        "window 1 passing must report PASS, not WARMUP — got {m:?}"
    );
}

#[test]
fn window_2_failure_is_a_genuine_fail_never_tolerated() {
    let m = outcome("2", "1");
    assert_eq!(
        m.get("outcome").map(String::as_str),
        Some("FAIL"),
        "a real (non-warm-up) window's failure must still abort the sweep — got {m:?}"
    );
}

#[test]
fn window_5_of_5_failure_is_also_a_genuine_fail() {
    let m = outcome("5", "1");
    assert_eq!(
        m.get("outcome").map(String::as_str),
        Some("FAIL"),
        "the LAST window failing must still be FAIL, not tolerated — got {m:?}"
    );
}

#[test]
fn any_window_passing_is_pass_regardless_of_index() {
    for idx in ["1", "2", "3", "4", "5"] {
        let m = outcome(idx, "0");
        assert_eq!(
            m.get("outcome").map(String::as_str),
            Some("PASS"),
            "window {idx} passing (rc=0) must be PASS — got {m:?}"
        );
    }
}
