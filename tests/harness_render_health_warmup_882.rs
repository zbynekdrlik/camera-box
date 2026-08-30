//! #882/#1232 — the imag render-health preflight's settle-adaptive warm-up PHASE.
//!
//! Background (#882, 2026-07-30): after imag-nb's OBS was restarted at 09:21 (recovering from a
//! segfault), the very next E2E gate run failed `[1/8]` render-health at window 1/5 — yet the SAME
//! gate binary measured the box at a clean 60.00fps/4.47ms/0% skip twenty minutes later. #882
//! shipped a FIXED rule: window 1 never counts (a non-counting warm-up), windows 2..N stay exactly
//! as strict as before.
//!
//! Why the fixed single warm-up window stopped being enough (#1232, 2026-08-30): the settle time
//! is not a constant — E2E run 33308636791 failed at window 2/5 (49.28fps/19.27ms/18.6% skip),
//! confirmed a genuine settle overrun (not a capacity ceiling) by a clean 60.0fps/2.9-4.5ms
//! measurement on the same box ~4 minutes later. The #1143 ensure-rec-encoder step restarts imag's
//! OBS right before these windows; with 7 active cameras (issue 1216 cam4+cam5 re-entry, vs 5
//! before) the settle routinely needs >=2 windows, so a fixed "only window 1 is warm-up" boundary
//! measured a still-settling box as a strict, gate-aborting failure.
//!
//! The fix: `render_health_phase_outcome` (scripts/lib/render-health-warmup.sh) generalizes "window
//! 1 doesn't count" into a settle-ADAPTIVE warm-up PHASE — the leading run of FAILED windows from
//! the start of the sweep up to (and including) the first PASS is the non-counting warm-up phase,
//! bounded by a wall-clock budget (`RENDER_HEALTH_SETTLE_BUDGET_S`, default 60s) so a genuinely
//! broken box (one that never settles) still fails loudly instead of retrying forever. The window
//! that achieves the first PASS ends the warm-up phase but — exactly like the old fixed window 1,
//! whether it happened to pass or fail — does NOT itself count toward the required strict total;
//! the strict windows are the ones that FOLLOW it (at least `RENDER_HEALTH_WINDOWS`, default 4, all
//! must pass). This is a strict generalization: when the box settles inside window 1 (the common
//! case), the phase machine behaves byte-for-byte like the old fixed rule.
//! `render_budget::classify` (src/render_budget.rs) itself is UNTOUCHED — this only decides
//! whether a FAILED window counts toward aborting the sweep.
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

/// Calls `render_health_phase_outcome <rc> <seen> <elapsed> <budget>` and parses its
/// `key=value` stdout lines into a map.
fn phase(rc: &str, seen: &str, elapsed: &str, budget: &str) -> HashMap<String, String> {
    let script = format!(
        r#"set -u
. "$LIB"
render_health_phase_outcome {rc} {seen} {elapsed} {budget}
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
        "render_health_phase_outcome must exit 0\nstdout={:?}\nstderr={:?}",
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

fn assert_phase(
    m: &HashMap<String, String>,
    want_outcome: &str,
    want_seen: &str,
    want_counts: &str,
    msg: &str,
) {
    assert_eq!(
        m.get("outcome").map(String::as_str),
        Some(want_outcome),
        "outcome mismatch: {msg} — got {m:?}"
    );
    assert_eq!(
        m.get("first_pass_seen").map(String::as_str),
        Some(want_seen),
        "first_pass_seen mismatch: {msg} — got {m:?}"
    );
    assert_eq!(
        m.get("counts_as_strict").map(String::as_str),
        Some(want_counts),
        "counts_as_strict mismatch: {msg} — got {m:?}"
    );
}

#[test]
fn window_1_immediate_pass_ends_warmup_but_never_counts_itself() {
    // The common case (no settle overrun): window 1 passes right away. This must behave
    // byte-for-byte like the OLD fixed rule — window 1 never counts toward the strict total, even
    // though it passed.
    let m = phase("0", "0", "0", "60");
    assert_phase(
        &m,
        "PASS",
        "1",
        "0",
        "an immediate window-1 PASS must end the warm-up phase without counting toward the \
         strict total (matches the old fixed window-1-never-counts rule)",
    );
}

#[test]
fn a_leading_failure_inside_the_settle_budget_is_a_tolerated_warmup() {
    let m = phase("1", "0", "5", "60");
    assert_phase(
        &m,
        "WARMUP",
        "0",
        "0",
        "a leading (pre-first-pass) failure well inside the settle budget must be tolerated",
    );
}

#[test]
fn a_leading_failure_just_under_the_settle_budget_is_still_a_tolerated_warmup() {
    let m = phase("1", "0", "59", "60");
    assert_phase(
        &m,
        "WARMUP",
        "0",
        "0",
        "a leading failure at elapsed=59s of a 60s budget must still be tolerated",
    );
}

#[test]
fn a_leading_failure_at_the_settle_budget_boundary_is_a_genuine_fail() {
    // Sustained failure PAST the whole settle budget must still abort loudly, exactly like today
    // — a box that never settles is a real regression, not something to retry forever.
    let m = phase("1", "0", "60", "60");
    assert_phase(
        &m,
        "FAIL",
        "0",
        "0",
        "a leading failure once elapsed has REACHED the settle budget must no longer be tolerated",
    );
}

#[test]
fn a_leading_failure_past_the_settle_budget_is_a_genuine_fail() {
    let m = phase("1", "0", "125", "60");
    assert_phase(
        &m,
        "FAIL",
        "0",
        "0",
        "a leading failure well past the settle budget must abort, never retry indefinitely",
    );
}

#[test]
fn a_multi_window_warmup_ending_late_still_does_not_count_its_own_first_pass() {
    // The window that FINALLY passes after several leading failures (a genuine multi-window
    // settle, the #1232 root cause) ends the phase but is STILL the boundary window, not a strict
    // one — exactly like window 1 in the #882 fixed rule.
    let m = phase("0", "0", "42", "60");
    assert_phase(
        &m,
        "PASS",
        "1",
        "0",
        "the first PASS of a multi-window warm-up ends the phase without itself counting, \
         regardless of how long the warm-up took (as long as it stayed inside the budget)",
    );
}

#[test]
fn a_strict_phase_pass_counts_toward_the_required_total() {
    // Once first_pass_seen is already 1 (some earlier window already ended the warm-up phase),
    // every subsequent PASS is a real strict-phase pass and counts.
    let m = phase("0", "1", "999", "60");
    assert_phase(
        &m,
        "PASS",
        "1",
        "1",
        "a PASS in the strict phase (first_pass_seen already 1) must count toward the required \
         strict total",
    );
}

#[test]
fn a_strict_phase_failure_is_never_tolerated_exactly_as_before() {
    let m = phase("1", "1", "999", "60");
    assert_phase(
        &m,
        "FAIL",
        "1",
        "0",
        "a strict-phase window's failure must still abort the sweep, never tolerated — exactly \
         as strict as the old fixed windows 2..N",
    );
}

#[test]
fn first_pass_seen_latches_permanently_once_set() {
    // Once a PASS has occurred, first_pass_seen must never go back to 0 — the strict phase, once
    // entered, is not re-enterable into warm-up even if this exact call is replayed.
    for elapsed in ["0", "1", "60", "999"] {
        let m = phase("1", "1", elapsed, "60");
        assert_eq!(
            m.get("first_pass_seen").map(String::as_str),
            Some("1"),
            "first_pass_seen must stay latched at 1 once a pass has occurred (elapsed={elapsed})"
        );
    }
}

#[test]
fn malformed_or_missing_inputs_fail_safe_never_infinite_tolerate() {
    // Missing/non-numeric args must never silently produce an infinitely-tolerated WARMUP —
    // fail-safe means FAIL, matching script-failure-policy's "fail loudly on errors".
    let script = format!(
        r#"set -u
. "{}"
render_health_phase_outcome
"#,
        lib_path().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "must still exit 0 (a decision, not a crash)"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    assert_eq!(
        map.get("outcome").map(String::as_str),
        Some("FAIL"),
        "malformed/missing inputs must fail SAFE (FAIL), never tolerate forever — got {map:?}"
    );
}

#[test]
fn any_window_passing_after_the_boundary_is_pass_regardless_of_how_late() {
    for elapsed in ["0", "10", "45", "600"] {
        let m = phase("0", "1", elapsed, "60");
        assert_eq!(
            m.get("outcome").map(String::as_str),
            Some("PASS"),
            "a strict-phase window passing (rc=0) must be PASS regardless of elapsed={elapsed}"
        );
    }
}
