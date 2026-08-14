//! #970 / #980 — `rig_lease_holder_summary()` (scripts/lib/rig-lease.sh) must read holder.json
//! ATOMICALLY. The live flake: while our gate reads the foreign holder's identity, that holder
//! (restreamer, or the test's releaser thread) `rm`s the lockdir. The pre-fix summary read
//! holder.json via FIVE separate `python3 open()+json.load()` calls (one per field), so a delete
//! landing mid-summary TORE the read — `repo` succeeded, then the file vanished and the remaining
//! fields read empty, producing the exact garbled `RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#
//! run_url= job= expected_release_at=` (#970) or `unknown (corrupt holder.json)` (#980) that broke
//! `foreign_lease_released_within_wait_budget_lets_us_proceed`.
//!
//! This is a DETERMINISTIC reproduction of that torn read (the sibling harness reproduces it only
//! statistically, under CPU load): a `python3` shim on PATH deletes holder.json immediately AFTER
//! its first invocation, so the pre-fix multi-read is guaranteed to tear on the very next field,
//! while the atomic single-read fix reads the whole file in one open() and is immune.

use std::fs;
use std::path::Path;
use std::process::Command;

fn rig_lease_lib() -> String {
    format!("{}/scripts/lib/rig-lease.sh", env!("CARGO_MANIFEST_DIR"))
}

fn real_python3() -> String {
    // Resolve the genuine python3 BEFORE our shim goes on PATH, so the shim can call it without
    // recursing into itself.
    let out = Command::new("bash")
        .args(["-c", "command -v python3"])
        .output()
        .expect("resolve python3");
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!p.is_empty(), "python3 must be on PATH for this test");
    p
}

const RUN_ID: &str = "888";
const RUN_URL: &str = "https://github.com/zbynekdrlik/restreamer/actions/runs/888";
const JOB: &str = "e2e-obs-to-youtube";
const EXP: &str = "2099-01-01T00:00:00Z";

/// The summary of a holder.json that is DELETED mid-read must be all-or-nothing: either the full
/// consistent line, or a clean placeholder — never a torn partial line with an empty run_id/fields.
#[test]
fn holder_summary_is_atomic_under_a_concurrent_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease_dir = dir.path().join("lease");
    fs::create_dir_all(&lease_dir).expect("create lease dir");
    let holder = lease_dir.join("holder.json");
    fs::write(
        &holder,
        format!(
            r#"{{"repo": "zbynekdrlik/restreamer", "run_id": "{RUN_ID}", "run_url": "{RUN_URL}", "job": "{JOB}", "acquired_at": "2026-07-27T18:00:00Z", "expected_release_at": "{EXP}"}}"#
        ),
    )
    .expect("write holder.json");

    // A `python3` shim that runs the real interpreter, then deletes holder.json right after its
    // FIRST call — deterministically simulating a foreign holder releasing the instant we begin
    // reading. The pre-fix summary (5 reads) tears on read #2; the atomic fix (1 read) is immune.
    let shim_dir = dir.path().join("shim");
    fs::create_dir_all(&shim_dir).expect("create shim dir");
    let counter = dir.path().join("py_calls");
    let shim = shim_dir.join("python3");
    let shim_body = format!(
        "#!/usr/bin/env bash\n\
         n=\"$(cat {counter:?} 2>/dev/null || echo 0)\"\n\
         echo $((n+1)) > {counter:?}\n\
         {real:?} \"$@\"; rc=$?\n\
         [ \"$n\" -eq 0 ] && rm -f {holder:?}\n\
         exit $rc\n",
        counter = counter,
        real = real_python3(),
        holder = holder,
    );
    fs::write(&shim, shim_body).expect("write shim");
    set_exec(&shim);

    let path_env = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new("bash")
        .args(["-c", ". \"$1\"; rig_lease_holder_summary", "_"])
        .arg(rig_lease_lib())
        .env("PATH", &path_env)
        .env("RIG_LEASE_DIR", &lease_dir)
        .output()
        .expect("run rig_lease_holder_summary");
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let full = format!("zbynekdrlik/restreamer#{RUN_ID} run_url={RUN_URL} job={JOB} expected_release_at={EXP}");
    assert!(
        summary == full
            || summary == "unknown (no holder.json present)"
            || summary == "unknown (corrupt holder.json)",
        "#970/#980: rig_lease_holder_summary tore under a concurrent holder.json delete — it must \
         read the file atomically so the result is EITHER the complete summary OR a clean \
         placeholder, never a partial line with empty fields. got: {summary:?}"
    );
}

#[cfg(unix)]
fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}
#[cfg(not(unix))]
fn set_exec(_p: &Path) {}
