//! #406/#312 item5 — the automatic CI-blocking rig-busy gate.
//!
//! `scripts/rig-busy-gate.sh` is the belt that stops the automatic `pull_request`-triggered
//! full-path-e2e CI run from rerouting strih/stream's PRODUCTION OBS program scenes while a
//! REAL broadcast is live. These tests execute the REAL bash script (not just grep it) against
//! a fake `obs_phase2.py` stub (via the `OBS_PHASE2_PY` override), driving all three outcomes:
//! rig free, rig busy past its budget, and rig unreachable — each with its own distinct exit
//! code so a human/agent can tell "retry later" apart from "fix the code" apart from
//! "runner/rig unreachable, fail closed".

use std::fs;
use std::path::Path;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn script_path() -> String {
    format!("{}/scripts/rig-busy-gate.sh", env!("CARGO_MANIFEST_DIR"))
}

/// Write a tiny fake `obs_phase2.py` stub that ALWAYS prints `body` and exits `code`,
/// regardless of the `rig-busy-check ...` args it's called with (the real CLI parses and
/// dispatches; this stub is a pure canned-response double for the gate's OWN loop logic).
fn write_fake_obs_phase2(dir: &Path, body: &str, code: i32) -> String {
    let path = dir.join("fake_obs_phase2.py");
    let contents = format!(
        "import sys\nprint({body:?})\nsys.exit({code})\n",
        body = body,
        code = code
    );
    fs::write(&path, contents).expect("write fake obs_phase2.py");
    path.to_string_lossy().to_string()
}

struct RunResult {
    status: i32,
    stdout: String,
}

fn run_gate(fake_py: &str, iterations: &str, sleep_secs: &str) -> RunResult {
    let out = Command::new("bash")
        .arg(script_path())
        .env("OBS_PHASE2_PY", fake_py)
        .env("RIG_BUSY_GATE_ITERATIONS", iterations)
        .env("RIG_BUSY_GATE_SLEEP_SECS", sleep_secs)
        .env_remove("GITHUB_STEP_SUMMARY")
        .output()
        .expect("run rig-busy-gate.sh");
    RunResult {
        status: out.status.code().unwrap_or(-1),
        stdout: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn rig_busy_gate_sh_exists_and_is_executable() {
    let path = format!("{}/scripts/rig-busy-gate.sh", env!("CARGO_MANIFEST_DIR"));
    assert!(
        Path::new(&path).exists(),
        "scripts/rig-busy-gate.sh not found (#406/#312 item5)."
    );
}

/// The rig reporting free (busy=false) on the FIRST check must exit 0 immediately — no
/// wasted retries, no failure annotation.
#[test]
fn rig_free_on_first_check_exits_zero_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_fake_obs_phase2(dir.path(), r#"{"busy": false, "reasons": []}"#, 0);

    let r = run_gate(&fake, "5", "0");

    assert_eq!(r.status, 0, "rig-free must exit 0: {}", r.stdout);
    assert!(
        r.stdout.contains("OUTCOME=RIG_FREE") || r.stdout.contains("rig is free"),
        "must report the rig-free outcome: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("::error"),
        "a clean rig-free run must not emit an error annotation: {}",
        r.stdout
    );
}

/// The rig staying busy for the ENTIRE budget must exit 42 (distinct from unreachable=43) and
/// emit a "RIG BUSY" annotation — a human/agent must be able to tell this apart from a real
/// code regression or an unreachable rig.
#[test]
fn rig_busy_for_entire_budget_exits_42_with_busy_annotation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_fake_obs_phase2(
        dir.path(),
        r#"{"busy": true, "reasons": ["strih is streaming (GetStreamStatus.outputActive=true)"]}"#,
        0,
    );

    let r = run_gate(&fake, "2", "0");

    assert_eq!(
        r.status, 42,
        "still-busy-after-budget must exit 42: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("::error") && r.stdout.contains("RIG BUSY"),
        "must emit a RIG BUSY error annotation: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("OUTCOME=RIG_BUSY"),
        "must report the OUTCOME=RIG_BUSY outcome line: {}",
        r.stdout
    );
}

/// An unreachable rig (obs_phase2.py rig-busy-check exits 3) must exit 43 (distinct from
/// busy=42) and emit a "RIG UNREACHABLE" annotation, failing CLOSED rather than proceeding as
/// if the rig were free.
#[test]
fn rig_unreachable_exits_43_with_unreachable_annotation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_fake_obs_phase2(
        dir.path(),
        r#"{"busy": null, "reasons": ["strih (10.77.9.202) unreachable: connection refused"]}"#,
        3,
    );

    let r = run_gate(&fake, "3", "0");

    assert_eq!(
        r.status, 43,
        "unreachable rig must exit 43 (fail closed): {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("::error") && r.stdout.contains("RIG UNREACHABLE"),
        "must emit a RIG UNREACHABLE error annotation: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("OUTCOME=RIG_UNREACHABLE"),
        "must report the OUTCOME=RIG_UNREACHABLE outcome line: {}",
        r.stdout
    );
    // Must abort on the VERY FIRST unreachable check — never burn the whole 3-iteration
    // budget retrying an unreachable box (that's the busy-retry path, not this one). Match the
    // loop's own log prefix (not the bare substring "check ", which also occurs inside the
    // error annotation's "obs_phase2.py rig-busy-check could not reach..." text).
    assert_eq!(
        r.stdout.matches("[rig-busy-gate] check ").count(),
        1,
        "must fail fast on the first unreachable check, not retry: {}",
        r.stdout
    );
}

/// An UNEXPECTED non-{0,3} exit from obs_phase2.py (e.g. a Python crash) must ALSO fail
/// closed as RIG_UNREACHABLE (exit 43) — never silently treated as "busy" (which would retry
/// for the full budget) or, worse, as "free" (which would proceed to reroute prod).
#[test]
fn unexpected_nonzero_exit_from_busy_check_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_fake_obs_phase2(dir.path(), "Traceback (most recent call last): boom", 1);

    let r = run_gate(&fake, "3", "0");

    assert_eq!(
        r.status, 43,
        "an unexpected non-zero exit must fail closed (43), never proceed: {}",
        r.stdout
    );
}

/// The gate must eventually give up (never loop forever) — bounded by RIG_BUSY_GATE_ITERATIONS.
#[test]
fn rig_busy_gate_is_bounded_never_infinite() {
    let s = read("scripts/rig-busy-gate.sh");
    assert!(
        s.contains("MAX_ITERATIONS"),
        "the poll loop must be bounded by a max-iterations budget, not an unbounded while-true"
    );
    assert!(
        !s.contains("while true") && !s.contains("while :"),
        "must not use an unbounded while-true poll loop"
    );
}

/// The three outcomes must use DISTINCT exit codes so a caller (CI, a re-dispatch agent) can
/// branch on them — busy-retry-later (42) vs unreachable-fail-closed (43) vs free (0).
#[test]
fn rig_busy_gate_uses_distinct_exit_codes_for_busy_vs_unreachable() {
    let s = read("scripts/rig-busy-gate.sh");
    assert!(s.contains("exit 42"), "RIG_BUSY must exit 42");
    assert!(s.contains("exit 43"), "RIG_UNREACHABLE must exit 43");
}
