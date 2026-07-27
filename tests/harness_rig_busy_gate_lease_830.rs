//! #830 — scripts/rig-busy-gate.sh's integration with the shared cross-repo rig lease
//! (scripts/lib/rig-lease.sh). These tests execute the REAL bash gate script end-to-end against
//! a fake obs_phase2.py stub, each pointed at its own isolated `RIG_LEASE_DIR` tempdir (never the
//! real `/var/tmp/rig-lease` — this repo's shared dev1 checkout runs tests from multiple
//! workers/parallel threads, see CLAUDE.md's shared-checkout GOTCHA).
//!
//! Covers: the lease is acquired and left HELD across a RIG_FREE exit (the recording step that
//! follows in the same CI job needs it); a failure path (RIG_BUSY/RIG_UNREACHABLE) releases the
//! lease it just acquired; a genuinely busy rig is STILL refused even when nobody else holds the
//! lease (the "no-lease fallback" the issue explicitly requires stays intact); a live foreign
//! holder whose estimate is far away is FAILED FAST (never grinds the whole OBS busy-check
//! budget); a live foreign holder that releases mid-wait lets us proceed.

use std::fs;
use std::path::Path;
use std::process::Command;

fn script_path() -> String {
    format!("{}/scripts/rig-busy-gate.sh", env!("CARGO_MANIFEST_DIR"))
}

fn write_fake_obs_phase2(dir: &Path, body: &str, code: i32) -> String {
    let path = dir.join("fake_obs_phase2.py");
    let contents = format!("import sys\nprint({body:?})\nsys.exit({code})\n");
    fs::write(&path, contents).expect("write fake obs_phase2.py");
    path.to_string_lossy().to_string()
}

/// A fake obs_phase2.py that COUNTS how many times it was invoked (via a call-log file) — used
/// to prove the "fail fast" path never even polls OBS state.
fn write_counting_fake_obs_phase2(dir: &Path, call_log: &Path) -> String {
    let path = dir.join("fake_obs_phase2_counting.py");
    let contents = format!(
        "import sys\nwith open({call_log:?}, 'a') as f:\n    f.write('call\\n')\nprint('{{\"busy\": false, \"reasons\": []}}')\nsys.exit(0)\n",
    );
    fs::write(&path, contents).expect("write counting fake obs_phase2.py");
    path.to_string_lossy().to_string()
}

struct RunResult {
    status: i32,
    stdout: String,
}

#[allow(clippy::too_many_arguments)]
fn run_gate(
    lease_dir: &Path,
    fake_py: &str,
    iterations: &str,
    sleep_secs: &str,
    run_id: &str,
    extra_env: &[(&str, &str)],
) -> RunResult {
    let mut cmd = Command::new("bash");
    cmd.arg(script_path())
        .env("OBS_PHASE2_PY", fake_py)
        .env("RIG_BUSY_GATE_ITERATIONS", iterations)
        .env("RIG_BUSY_GATE_SLEEP_SECS", sleep_secs)
        .env("RIG_LEASE_DIR", lease_dir)
        .env("RIG_LEASE_REPO", "zbynekdrlik/camera-box")
        .env("RIG_LEASE_RUN_ID", run_id)
        .env("RIG_LEASE_RUN_URL", format!("https://example/run/{run_id}"))
        .env("RIG_LEASE_JOB", "full-path")
        .env_remove("GITHUB_STEP_SUMMARY")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_RUN_ID")
        .env_remove("GITHUB_JOB");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run rig-busy-gate.sh");
    RunResult {
        status: out.status.code().unwrap_or(-1),
        stdout: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn seed_foreign_holder(lease_dir: &Path, run_id: &str, expected_release_at: &str) {
    fs::create_dir_all(lease_dir).expect("create lease dir");
    let holder = format!(
        r#"{{"repo": "zbynekdrlik/restreamer", "run_id": "{run_id}", "run_url": "https://github.com/zbynekdrlik/restreamer/actions/runs/{run_id}", "job": "e2e-obs-to-youtube", "acquired_at": "2026-07-27T18:00:00Z", "expected_release_at": "{expected_release_at}"}}"#
    );
    fs::write(lease_dir.join("holder.json"), holder).expect("write holder.json");
    fs::write(lease_dir.join("heartbeat"), "").expect("write heartbeat");
}

/// When the rig is free AND nobody else holds the lease, the gate must ACQUIRE it, exit 0, and
/// leave it HELD (not release it) — the recording step that follows in the SAME job needs it.
#[test]
fn rig_free_acquires_lease_and_leaves_it_held_across_the_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let fake = write_fake_obs_phase2(dir.path(), r#"{"busy": false, "reasons": []}"#, 0);

    let r = run_gate(&lease_dir, &fake, "5", "0", "run-free", &[]);

    assert_eq!(r.status, 0, "rig-free must still exit 0: {}", r.stdout);
    assert!(r.stdout.contains("OUTCOME=RIG_FREE"), "{}", r.stdout);
    assert!(
        r.stdout.contains("RIG_LEASE_ACQUIRED"),
        "the gate must acquire the lease before proceeding: {}",
        r.stdout
    );
    assert!(
        lease_dir.join("holder.json").exists(),
        "#830: the lease must be LEFT HELD after a RIG_FREE exit (a later workflow step releases \
         it, not this script's own success path)"
    );
}

/// A failure path (rig stays busy the whole budget) must RELEASE the lease it just acquired —
/// never leave a dangling lease behind after a failed gate run.
#[test]
fn rig_busy_failure_releases_the_lease_it_acquired() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let fake = write_fake_obs_phase2(
        dir.path(),
        r#"{"busy": true, "reasons": ["strih is streaming (GetStreamStatus.outputActive=true)"]}"#,
        0,
    );

    let r = run_gate(&lease_dir, &fake, "2", "0", "run-busy", &[]);

    assert_eq!(
        r.status, 42,
        "still-busy must exit 42 as before: {}",
        r.stdout
    );
    assert!(r.stdout.contains("OUTCOME=RIG_BUSY"), "{}", r.stdout);
    // Prove this is a genuine acquire-THEN-release round trip, not merely "the lease dir was
    // never touched" (which would trivially also leave it absent).
    assert!(
        r.stdout.contains("RIG_LEASE_ACQUIRED"),
        "must have actually acquired the lease first: {}",
        r.stdout
    );
    assert!(
        !lease_dir.exists(),
        "#830: a failed gate run must RELEASE the lease it acquired, not leave it dangling"
    );
}

/// Same for the unreachable-rig failure path.
#[test]
fn rig_unreachable_failure_releases_the_lease_it_acquired() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let fake = write_fake_obs_phase2(
        dir.path(),
        r#"{"busy": null, "reasons": ["unreachable"]}"#,
        3,
    );

    let r = run_gate(&lease_dir, &fake, "3", "0", "run-unreachable", &[]);

    assert_eq!(
        r.status, 43,
        "unreachable must exit 43 as before: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("RIG_LEASE_ACQUIRED"),
        "must have actually acquired the lease first: {}",
        r.stdout
    );
    assert!(
        !lease_dir.exists(),
        "#830: a failed (unreachable) gate run must release the lease it acquired"
    );
}

/// #830's explicit requirement: "the existing OBS-state polling stays as the FALLBACK ... a busy
/// rig with no lease present must still be refused exactly as today." With NO foreign holder at
/// all (we trivially acquire the lease ourselves), a genuinely busy rig must still fail RIG_BUSY
/// — the lease layer must never weaken the pre-existing OBS-state gate.
#[test]
fn no_lease_conflict_busy_rig_is_still_refused_fallback_unweakened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let fake = write_fake_obs_phase2(
        dir.path(),
        r#"{"busy": true, "reasons": ["stream is recording (GetRecordStatus.outputActive=true)"]}"#,
        0,
    );

    let r = run_gate(&lease_dir, &fake, "2", "0", "run-fallback", &[]);

    assert_eq!(
        r.status, 42,
        "no lease conflict at all, but OBS reports busy -- must still refuse (fallback intact): {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("RIG_LEASE_ACQUIRED"),
        "we must have acquired trivially: {}",
        r.stdout
    );
    assert!(r.stdout.contains("OUTCOME=RIG_BUSY"), "{}", r.stdout);
}

/// A live foreign holder whose own expected_release_at is FAR beyond our max-wait budget must be
/// refused IMMEDIATELY — never grind the whole OBS busy-check budget for certain failure. Proven
/// by asserting the OBS busy-check stub was NEVER even invoked.
#[test]
fn foreign_lease_far_from_release_fails_fast_without_polling_obs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let call_log = dir.path().join("obs_calls.log");
    let fake = write_counting_fake_obs_phase2(dir.path(), &call_log);

    // Foreign holder's own estimate: ~40 minutes from now -- far beyond our 60s max-wait budget.
    let far_future = chrono_like_plus_seconds(2400);
    seed_foreign_holder(&lease_dir, "999", &far_future);

    let r = run_gate(
        &lease_dir,
        &fake,
        "5",
        "1",
        "run-failfast",
        &[("RIG_LEASE_MAX_WAIT_SECS", "60")],
    );

    assert_eq!(
        r.status, 44,
        "a foreign lease whose estimate outlasts our wait budget must fail FAST with a distinct \
         exit code (44), not grind toward RIG_BUSY/42: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("OUTCOME=RIG_LEASE_HELD"),
        "must report the distinct RIG_LEASE_HELD outcome: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("RIG_HELD_BY=zbynekdrlik/restreamer#999"),
        "must name the actual foreign holder: {}",
        r.stdout
    );
    assert!(
        !call_log.exists(),
        "#830: failing FAST must never even poll OBS state — the whole point is avoiding the \
         wasted 30-minute busy-check budget against a soak that will obviously outlast it"
    );
}

/// A live foreign holder whose lease is released SHORTLY (within our wait budget) must let us
/// wait for it and then proceed — never an instant, unconditional refusal.
#[test]
fn foreign_lease_released_within_wait_budget_lets_us_proceed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    let fake = write_fake_obs_phase2(dir.path(), r#"{"busy": false, "reasons": []}"#, 0);

    // Foreign holder's own estimate is soon, well within our wait budget.
    let soon = chrono_like_plus_seconds(30);
    seed_foreign_holder(&lease_dir, "888", &soon);

    // Simulate the foreign holder releasing shortly after we start waiting.
    let lease_dir_clone = lease_dir.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = fs::remove_dir_all(&lease_dir_clone);
    });

    let r = run_gate(
        &lease_dir,
        &fake,
        "5",
        "1",
        "run-waiter",
        &[("RIG_LEASE_MAX_WAIT_SECS", "30")],
    );
    releaser.join().expect("releaser thread");

    assert_eq!(
        r.status, 0,
        "once the foreign holder releases within our wait budget, we must acquire and proceed: {}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("rig lease held by zbynekdrlik/restreamer#888"),
        "must have logged the wait against the named foreign holder: {}",
        r.stdout
    );
    assert!(r.stdout.contains("RIG_LEASE_ACQUIRED"), "{}", r.stdout);
    assert!(r.stdout.contains("OUTCOME=RIG_FREE"), "{}", r.stdout);
}

/// Tiny helper: an ISO8601 UTC timestamp `secs` in the future, via the system `date` binary (no
/// chrono dependency in this repo's Cargo.toml) — named for readability at call sites only.
fn chrono_like_plus_seconds(secs: u64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("+{secs} seconds"),
            "+%Y-%m-%dT%H:%M:%SZ",
        ])
        .output()
        .expect("date");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
