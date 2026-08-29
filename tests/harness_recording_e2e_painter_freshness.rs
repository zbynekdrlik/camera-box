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

/// #747 — the [3/8] cam2 painter must stay ALIVE from its launch, through the pre-record
/// warm-up/gate budget AND the whole DURATION recording, self-exiting (writing its ground-truth
/// CSV) only AFTER [7/8] StopRecord. The old fixed `--duration-secs $((DURATION+60))` slack was
/// sized BEFORE the #747 frozen-camera-gate warm-up ([4c/8]) and scene warm-up ([4f/8]) phases
/// were added between the launch and [5/8] StartRecord (~50s+, plus up to ~120s of frozen-gate
/// retries on a marginal rig); with them the painter self-exited ~47s BEFORE StopRecord, so the
/// last ~1.5 verdict windows went dark (windows 8-9 all-undecodable). Two things this locks:
/// (1) the slack is a NAMED, generously-sized constant covering the worst-case pre-record budget;
/// (2) the painter `--duration-secs` and the PAINTER_EXIT_DEADLINE self-exit wait use the SAME
/// constant — two independent `DURATION+60` literals silently drift, and a shorter deadline than
/// the painter's real lifetime makes the wait give up before self-exit → a STALE CSV is pulled.
#[test]
fn painter_duration_slack_covers_prerecord_warmup_and_is_lockstep() {
    let s = read();
    // The slack must be a single named constant, not two bare `DURATION+60` literals.
    let def = s
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("PAINTER_PRE_RECORD_SLACK_SECS="))
        .expect(
            "#747: the painter slack must be a NAMED constant (PAINTER_PRE_RECORD_SLACK_SECS=…) \
             referenced by BOTH the painter --duration-secs and PAINTER_EXIT_DEADLINE — not two \
             independent DURATION+60 literals that can silently drift",
        );
    // Extract the numeric default from `${PAINTER_PRE_RECORD_SLACK_SECS:-NNN}`.
    let n: u32 = def
        .split(":-")
        .nth(1)
        .and_then(|rest| {
            rest.trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .expect("#747: PAINTER_PRE_RECORD_SLACK_SECS must have a numeric default (${…:-NNN})");
    assert!(
        n >= 180,
        "#747: the painter slack (got {n}s) must cover the worst-case pre-record warm-up + \
         frozen-gate-retry budget so the painter self-exits AFTER StopRecord — the old +60s let \
         it die ~47s early, darkening the last ~1.5 verdict windows"
    );
    // Lock-step: the painter --duration-secs must add the named constant …
    assert!(
        s.contains("--duration-secs $((DURATION + PAINTER_PRE_RECORD_SLACK_SECS))"),
        "#747: the painter --duration-secs must be $((DURATION + PAINTER_PRE_RECORD_SLACK_SECS)), \
         not a bare DURATION+60"
    );
    // … and PAINTER_EXIT_DEADLINE (the self-exit wait) must add the SAME constant, so the wait
    // matches the painter's real lifetime (no early give-up → stale CSV).
    assert!(
        s.contains("DURATION + PAINTER_PRE_RECORD_SLACK_SECS ))")
            || s.contains("DURATION + PAINTER_PRE_RECORD_SLACK_SECS))"),
        "#747: PAINTER_EXIT_DEADLINE must add the SAME PAINTER_PRE_RECORD_SLACK_SECS as the \
         painter --duration-secs, so the self-exit wait matches the painter's real lifetime"
    );
}

/// #1223 — two of three overnight E2E aborts (2026-08-29/30) were the painter's pre-record slack
/// (240s, #747 sizing) being too short for today's worst-case pre-record budget: the
/// frozen-camera gate (~180s worst case) + the issue-1221 settle-wait (up to a 180s budget) +
/// render/MV gates + align/heal steps can together exceed 9 minutes on a degraded attempt, so the
/// painter self-exits (blanking /dev/fb0, issue 660) BEFORE StartRecord even runs. The later
/// switch-sweep self-check then reads the dark monitor as "cambox not delivering" (a mis-attributed
/// abort — live evidence: runs 1089165656 and 1136341935 on issue 1223). 240s is no longer enough
/// margin; 600s is the floor this test locks so the default cannot silently regress back down.
#[test]
fn painter_pre_record_slack_covers_2026_08_worst_case_1223() {
    let s = read();
    let def = s
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("PAINTER_PRE_RECORD_SLACK_SECS="))
        .expect("#1223: PAINTER_PRE_RECORD_SLACK_SECS default definition must still exist");
    let n: u32 = def
        .split(":-")
        .nth(1)
        .and_then(|rest| {
            rest.trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .expect("#1223: PAINTER_PRE_RECORD_SLACK_SECS must have a numeric default (${…:-NNN})");
    assert!(
        n >= 600,
        "#1223: the painter pre-record slack (got {n}s) must cover today's worst-case pre-record          budget (frozen-cam gate ~180s + issue-1221 settle-wait up to 180s + render/MV/align/heal)          — 240s was live-exceeded twice overnight 2026-08-29/30, expiring the painter before          StartRecord and darkening the monitor mid-run"
    );
}

/// #1223 fix 2: after the [7/8] StopRecord phase, the harness must send the painter a GRACEFUL
/// `pkill -TERM -x frame-probe` before the existing #359 self-exit wait loop. Since issue 1186,
/// frame-probe's SIGTERM handler runs the same teardown as its clean self-exit (writes the
/// ground-truth CSV + marker log, blanks fb0) — live-proven by a systemd `stop` of the permanent
/// painter unit producing the identical teardown sequence. This makes the #359 wait loop's own
/// condition (process gone + fresh CSV) true within seconds regardless of how large the slack in
/// the test above is, so raising that slack never actually lengthens a normal run's tail.
#[test]
fn painter_gets_graceful_term_after_stoprecord_before_exit_wait_1223() {
    let s = read();
    let stop_record = s
        .find("echo \"[7/8] StopRecord")
        .expect("#1223: the [7/8] StopRecord phase banner must still exist");
    let deadline = s
        .find("PAINTER_EXIT_DEADLINE=")
        .expect("#1223: PAINTER_EXIT_DEADLINE must still exist");
    assert!(
        stop_record < deadline,
        "#1223: PAINTER_EXIT_DEADLINE must come AFTER the [7/8] StopRecord phase"
    );
    let region = &s[stop_record..deadline];
    assert!(
        region.contains("pkill -TERM -x frame-probe"),
        "#1223: a graceful `pkill -TERM -x frame-probe` must run on the painter box between the \
         [7/8] StopRecord phase and PAINTER_EXIT_DEADLINE, so frame-probe's issue-1186 SIGTERM \
         teardown (CSV + markers, live-proven) fires the #359 wait loop's condition within \
         seconds instead of waiting out the enlarged pre-record slack"
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
