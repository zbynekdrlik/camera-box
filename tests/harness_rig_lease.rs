//! #830 — the CAMERA-BOX half of the shared cross-repo rig lease. camera-box's full-path-e2e.yml
//! and restreamer's Rust CI both drive the SAME physical rig (strih/stream OBS) from the SAME
//! self-hosted dev1 runner, with no mutual exclusion. Live collision (2026-07-27): our gate burnt
//! its full 30-minute rig-busy budget and died OUTCOME=RIG_BUSY while restreamer's soak held
//! stream OBS. Design settled on the issue (owner comment): a lockdir lease on dev1 --
//! `/var/tmp/rig-lease/` (atomic mkdir=acquire) + `holder.json` (repo/run_id/run_url/job/
//! acquired_at/expected_release_at) + `heartbeat` (mtime bumped while the holder works).
//!
//! These tests exercise the REAL `scripts/lib/rig-lease.sh` (sourced, never re-implemented) --
//! acquire/release round-trip, refusal while a live foreign holder is named, stale reclaim via
//! either signal (heartbeat too old, or the holder's own run confirmed no-longer-in-progress), and
//! that releasing never destroys a DIFFERENT holder's lease. No test touches the real rig — every
//! test points `RIG_LEASE_DIR` at its own tempdir.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/rig-lease.sh");
    assert!(p.exists(), "{} not found (#830)", p.display());
    p
}

struct RunResult {
    status: i32,
    stdout: String,
    stderr: String,
}

/// Source the real lib, then run `body`. Returns stdout/stderr/exit separately (unlike the
/// v4l2-neutral pattern's single merged blob) because several assertions here need to tell a
/// stdout outcome line apart from a stderr `::warning` annotation.
fn run_sourced(lease_dir: &Path, body: &str, extra_env: &[(&str, &str)]) -> RunResult {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("RIG_LEASE_DIR", lease_dir);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    RunResult {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn rig_lease_sh_exists() {
    let _ = lib_script();
}

/// A fresh (empty) lease dir path must acquire immediately, write holder.json + heartbeat, and
/// then be releasable by the SAME run_id — the basic round trip.
#[test]
fn acquire_then_release_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");

    let r = run_sourced(
        &lease_dir,
        r#"rig_lease_acquire "camera-box-repo" "run-1" "https://example/run/1" "full-path" "2026-07-27T22:00:00Z""#,
        &[],
    );
    assert_eq!(
        r.status, 0,
        "fresh acquire must succeed: {} / {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stdout.contains("RIG_LEASE_ACQUIRED"),
        "must report RIG_LEASE_ACQUIRED: {}",
        r.stdout
    );
    assert!(
        lease_dir.join("holder.json").exists(),
        "holder.json must exist after acquire"
    );
    assert!(
        lease_dir.join("heartbeat").exists(),
        "heartbeat must exist after acquire"
    );

    let holder: SerdeJsonLite = parse_holder(&lease_dir);
    assert_eq!(holder.repo, "camera-box-repo");
    assert_eq!(holder.run_id, "run-1");

    let r2 = run_sourced(&lease_dir, r#"rig_lease_release "run-1""#, &[]);
    assert_eq!(
        r2.status, 0,
        "release must succeed: {} / {}",
        r2.stdout, r2.stderr
    );
    assert!(
        !lease_dir.exists(),
        "the lease dir must be gone after a matching-run_id release"
    );
}

/// A live foreign holder (fresh heartbeat, no dead-run signal) must refuse our acquire AND name
/// the holder in the printed outcome — the whole point of #830 over a bare "RIG_BUSY".
#[test]
fn refuses_while_held_by_live_foreign_holder_and_names_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    seed_holder(
        &lease_dir,
        "zbynekdrlik/restreamer",
        "999",
        "https://github.com/zbynekdrlik/restreamer/actions/runs/999",
        "e2e-obs-to-youtube",
        "2026-07-27T23:00:00Z",
        0, // fresh heartbeat
    );

    let r = run_sourced(
        &lease_dir,
        r#"rig_lease_acquire "camera-box" "run-2" "https://example/run/2" "full-path" "2026-07-27T22:30:00Z""#,
        &[],
    );

    assert_eq!(
        r.status, 1,
        "acquire must be REFUSED while a live foreign holder exists: {} / {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stdout
            .contains("RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#999"),
        "must name the actual foreign holder (repo#run_id), not a bare RIG_BUSY: {}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("expected_release_at=2026-07-27T23:00:00Z"),
        "must surface the holder's own expected_release_at: {}",
        r.stdout
    );
    // Our own holder.json must be UNCHANGED — never overwritten by a refused acquire attempt.
    let holder = parse_holder(&lease_dir);
    assert_eq!(
        holder.run_id, "999",
        "a refused acquire must never mutate the existing lease"
    );
}

/// A heartbeat far older than the stale threshold must be reclaimed — logged loudly (citing
/// #830) — even though nothing else is checked (no RIG_LEASE_RUN_STATUS_CMD configured).
#[test]
fn stale_by_heartbeat_is_reclaimed_loudly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    seed_holder(
        &lease_dir,
        "zbynekdrlik/restreamer",
        "111",
        "https://github.com/zbynekdrlik/restreamer/actions/runs/111",
        "e2e-fb-push",
        "2026-07-27T20:00:00Z",
        99999, // ancient heartbeat
    );

    let r = run_sourced(
        &lease_dir,
        r#"rig_lease_acquire "camera-box" "run-3" "https://example/run/3" "full-path" "2026-07-27T23:30:00Z" 100"#,
        &[],
    );

    assert_eq!(
        r.status, 0,
        "a stale (heartbeat-expired) foreign lease must be RECLAIMABLE, not a deadlock: {} / {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stdout.contains("RIG_LEASE_RECLAIMED"),
        "must report RIG_LEASE_RECLAIMED: {}",
        r.stdout
    );
    assert!(
        r.stderr.contains("::warning") && r.stderr.contains("#830"),
        "the stale reclaim must be logged LOUDLY citing #830: {}",
        r.stderr
    );
    let holder = parse_holder(&lease_dir);
    assert_eq!(holder.repo, "camera-box", "the lease must now belong to us");
    assert_eq!(holder.run_id, "run-3");
}

/// A holder whose run status checker reports "not_in_progress" must be reclaimed EVEN WITH a
/// fresh heartbeat — the run finishing/crashing without releasing (the actual #830 self-heal
/// path for a CI job that died) is a faster signal than waiting out the heartbeat threshold.
#[test]
fn stale_by_dead_holder_run_status_is_reclaimed_even_with_fresh_heartbeat() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    seed_holder(
        &lease_dir,
        "zbynekdrlik/restreamer",
        "222",
        "https://github.com/zbynekdrlik/restreamer/actions/runs/222",
        "e2e-streaming-test",
        "2026-07-27T23:00:00Z",
        0, // FRESH heartbeat — must not matter once the run-status checker says dead
    );
    let checker = tmp.path().join("fake_status_checker.sh");
    fs::write(&checker, "#!/usr/bin/env bash\necho not_in_progress\n").unwrap();
    fs::set_permissions(&checker, fs::Permissions::from_mode(0o755)).unwrap();

    let r = run_sourced(
        &lease_dir,
        r#"rig_lease_acquire "camera-box" "run-4" "https://example/run/4" "full-path" "2026-07-27T23:45:00Z" 5400"#,
        &[("RIG_LEASE_RUN_STATUS_CMD", checker.to_str().unwrap())],
    );

    assert_eq!(
        r.status, 0,
        "a confirmed-dead holder run must be reclaimable even with a fresh heartbeat: {} / {}",
        r.stdout, r.stderr
    );
    assert!(r.stdout.contains("RIG_LEASE_RECLAIMED"), "{}", r.stdout);
}

/// The default (no RIG_LEASE_RUN_STATUS_CMD configured) must treat "unknown" as ALIVE — never
/// reclaim purely because we have no external status source. Staleness then rests solely on the
/// heartbeat, which stays fresh here.
#[test]
fn unknown_run_status_with_fresh_heartbeat_is_never_reclaimed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    seed_holder(
        &lease_dir,
        "zbynekdrlik/restreamer",
        "333",
        "https://github.com/zbynekdrlik/restreamer/actions/runs/333",
        "e2e-obs-to-youtube",
        "2026-07-27T23:00:00Z",
        0,
    );

    let r = run_sourced(
        &lease_dir,
        r#"rig_lease_acquire "camera-box" "run-5" "https://example/run/5" "full-path" "2026-07-27T23:45:00Z""#,
        &[],
    );

    assert_eq!(
        r.status, 1,
        "unknown holder-run-status (no checker configured) must never itself trigger a reclaim: {} / {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stdout
            .contains("RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#333"),
        "{}",
        r.stdout
    );
}

/// Releasing must be a NO-OP when the current holder's run_id does not match ours — never
/// destroy a DIFFERENT (later, or already-reclaimed) holder's lease out from under it.
#[test]
fn release_never_destroys_a_different_holders_lease() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    seed_holder(
        &lease_dir,
        "camera-box",
        "theirs",
        "https://example/run/theirs",
        "full-path",
        "2026-07-27T23:00:00Z",
        0,
    );

    let r = run_sourced(&lease_dir, r#"rig_lease_release "ours""#, &[]);
    assert_eq!(
        r.status, 0,
        "release itself must exit 0 even when it declines to act: {} / {}",
        r.stdout, r.stderr
    );
    assert!(
        lease_dir.exists() && lease_dir.join("holder.json").exists(),
        "a release with a MISMATCHED run_id must leave the current holder's lease intact"
    );
    let holder = parse_holder(&lease_dir);
    assert_eq!(
        holder.run_id, "theirs",
        "the foreign holder's lease must be untouched"
    );
}

/// Releasing an ALREADY-ABSENT lease dir must be a safe no-op (idempotent) — e.g. a gate run
/// that failed fast before ever acquiring, whose cleanup step still unconditionally calls release.
#[test]
fn release_of_absent_lease_is_a_safe_noop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("never-created");

    let r = run_sourced(&lease_dir, r#"rig_lease_release "whatever""#, &[]);
    assert_eq!(
        r.status, 0,
        "releasing an absent lease must exit 0: {} / {}",
        r.stdout, r.stderr
    );
}

// --- tiny fixture helpers -----------------------------------------------------------------

/// Write a holder.json + heartbeat directly (bypassing rig_lease_acquire) so a test can seed an
/// EXISTING foreign lease with a specific heartbeat age (in seconds).
fn seed_holder(
    lease_dir: &Path,
    repo: &str,
    run_id: &str,
    run_url: &str,
    job: &str,
    expected_release_at: &str,
    heartbeat_age_secs: u64,
) {
    fs::create_dir_all(lease_dir).expect("create lease dir");
    let holder = format!(
        r#"{{"repo": "{repo}", "run_id": "{run_id}", "run_url": "{run_url}", "job": "{job}", "acquired_at": "2026-07-27T19:00:00Z", "expected_release_at": "{expected_release_at}"}}"#
    );
    fs::write(lease_dir.join("holder.json"), holder).expect("write holder.json");
    let hb = lease_dir.join("heartbeat");
    fs::write(&hb, "").expect("write heartbeat");
    if heartbeat_age_secs > 0 {
        let out = Command::new("touch")
            .arg("-d")
            .arg(format!("-{heartbeat_age_secs} seconds"))
            .arg(&hb)
            .output()
            .expect("touch -d");
        assert!(out.status.success(), "touch -d failed: {:?}", out);
    }
}

struct SerdeJsonLite {
    repo: String,
    run_id: String,
}

fn parse_holder(lease_dir: &Path) -> SerdeJsonLite {
    let text = fs::read_to_string(lease_dir.join("holder.json")).expect("read holder.json");
    // Minimal hand-rolled extraction — this repo has no serde_json dependency in Cargo.toml and
    // these tests only ever need two string fields out of a flat, well-known JSON shape.
    let field = |name: &str| -> String {
        let pat = format!("\"{name}\": \"");
        let start = text.find(&pat).map(|p| p + pat.len());
        let start = match start {
            Some(s) => s,
            None => return String::new(),
        };
        let end = text[start..]
            .find('"')
            .map(|e| start + e)
            .unwrap_or(text.len());
        text[start..end].to_string()
    };
    SerdeJsonLite {
        repo: field("repo"),
        run_id: field("run_id"),
    }
}
