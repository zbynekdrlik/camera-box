//! #359 — recording-e2e.sh must never SILENTLY pull a STALE cam2 painter ground-truth CSV.
//!
//! ## The bug (run 354002)
//!
//! frame-probe writes the painter ground-truth `/tmp/painter.csv` ONLY on its clean
//! `--duration-secs` self-exit (src/probe/run.rs). The harness `pkill -x frame-probe`'d the
//! painter at ~DURATION — BEFORE that self-exit — so it never wrote a fresh CSV, and the
//! best-effort `scp ... || warn` then pulled a STALE leftover from a prior run. The verdict
//! trusted it → a fake catastrophic FAIL (cam2→cam1 latency 14.9h, 86% "undecodable") that was
//! a pure measurement artifact, not real frame loss.
//!
//! ## The fix these tests lock (pure static read of the shell script — no rig, no ssh)
//!
//! 1. `rm -f /tmp/painter.csv` BEFORE launching the painter (a leftover can never be pulled).
//! 2. WAIT for the painter to self-exit (process gone) instead of killing it early, so a FRESH
//!    CSV is actually written this run.
//! 3. After the pull, a FAIL-LOUD freshness gate (non-zero exit) that rejects a missing/empty,
//!    too-short, or hours-offset CSV — never feeding a stale ground truth to the verdict.

use std::fs;

fn read() -> String {
    let path = format!("{}/scripts/recording-e2e.sh", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// #359 fix 1: a STALE /tmp/painter.csv must be removed BEFORE the painter is launched, so a
/// leftover from a prior run can never be pulled and trusted as this run's ground truth.
#[test]
fn stale_painter_csv_is_removed_before_painter_launch() {
    let s = read();
    let rm = s.find("rm -f /tmp/painter.csv").expect(
        "#359: recording-e2e.sh must `rm -f /tmp/painter.csv` on the painter box before launching \
         the painter (a killed painter leaves a STALE CSV; frame-probe writes only on self-exit)",
    );
    let launch = s
        .find("--paint-log /tmp/painter.csv")
        .expect("recording-e2e.sh must launch the painter with --paint-log /tmp/painter.csv");
    assert!(
        rm < launch,
        "#359: the stale-CSV `rm -f /tmp/painter.csv` must come BEFORE the painter launch, not after"
    );
}

/// #359 fix 3 (root cause): the harness must WAIT for the painter to self-exit (when frame-probe
/// writes the CSV) instead of `pkill`-ing it early at ~DURATION. The wait keys off the painter's
/// launch epoch + its --duration-secs lifetime and confirms the process has exited.
#[test]
fn painter_is_waited_for_self_exit_not_killed_early() {
    let s = read();
    assert!(
        s.contains("PAINTER_LAUNCH_EPOCH"),
        "#359: the harness must record the painter launch epoch (PAINTER_LAUNCH_EPOCH) so it can \
         wait for the painter's --duration-secs self-exit before pulling the CSV"
    );
    assert!(
        s.contains("pgrep -x frame-probe"),
        "#359: before pulling, the harness must confirm the painter PROCESS has self-exited \
         (pgrep -x frame-probe) — frame-probe writes the ground-truth CSV only on clean self-exit"
    );
}

/// #359 fix 2 (the core): after pulling the painter CSV the harness must FAIL LOUD (non-zero
/// exit) when it is missing/empty, too short, or its gen_ts is hours off — never feed a stale
/// ground truth to the verdict.
#[test]
fn painter_csv_freshness_gate_fails_loud_after_pull() {
    let s = read();
    let pull = s
        .find(":/tmp/painter.csv \"$PAINTER_CSV\"")
        .expect("recording-e2e.sh must scp the painter CSV to $PAINTER_CSV");
    let after = &s[pull..];
    // The #359 freshness gate must come AFTER the pull.
    let gate = after
        .find("FATAL #359")
        .expect("#359: a freshness gate (FATAL #359 ...) must follow the painter-CSV pull");
    let gate_region = &after[gate..];
    assert!(
        gate_region.contains("exit 1"),
        "#359: the freshness gate must FAIL LOUD with a non-zero exit (exit 1), never just warn"
    );
    // It must validate run-relative freshness: span vs DURATION and gen_ts offset vs the run
    // start (RUN_START_EPOCH) — not merely 'the file exists'.
    assert!(
        after.contains("RUN_START_EPOCH") && after.contains("DURATION"),
        "#359: the freshness gate must validate the CSV span (≈DURATION) and gen_ts offset vs \
         RUN_START_EPOCH, not just that the file exists"
    );
}

/// The run-start wall-clock epoch the freshness gate compares against must be captured.
#[test]
fn run_start_epoch_is_captured() {
    let s = read();
    assert!(
        s.contains("RUN_START_EPOCH=") && s.contains("date +%s"),
        "#359: the harness must capture RUN_START_EPOCH=$(date +%s) for the painter-CSV gen_ts \
         freshness check"
    );
}

/// Characterization: the edited script must still pass `bash -n` (no syntax break).
#[test]
fn recording_e2e_passes_bash_syntax_check() {
    let path = format!("{}/scripts/recording-e2e.sh", env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("run bash -n");
    assert!(
        out.status.success(),
        "scripts/recording-e2e.sh must pass `bash -n`\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
