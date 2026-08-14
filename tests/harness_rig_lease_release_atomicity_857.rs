//! #857 — `rig_lease_release()` (scripts/lib/rig-lease.sh) must tear the lockdir down ATOMICALLY
//! from any concurrent observer's point of view. The pre-fix release ran a plain `rm -rf "$d"`,
//! whose recursive delete removes the directory CONTENTS (holder.json, heartbeat) BEFORE the
//! directory inode itself — so there is a real interval where `$d` exists but `holder.json` does
//! not. In that window a concurrent reader logs an unnamed holder (issue #857), and — worse — a
//! concurrent `rig_lease_acquire` sees its `mkdir "$d"` guard fail EEXIST against a holder-LESS
//! dir, reclaims into `$d`, and has that fresh lease deleted by the departing release's still-
//! running `rm -rf "$d"` — two runs momentarily both believing they hold the rig.
//!
//! The read-side #970/#980 work made the READER atomic; this proves the WRITER is too. Both tests
//! are DETERMINISTIC (no timing): a PATH `rm` shim faithfully reproduces `rm -rf`'s internal order
//! (contents first, dir last) and pins the exact worst-case instant. The atomic rename-then-delete
//! fix means the shim only ever fires on the already-detached `.releasing.*` copy while `$d` is
//! gone, so neither harm can occur.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/rig-lease.sh");
    assert!(p.exists(), "{} not found (#857)", p.display());
    p
}

/// Resolve the genuine `rm` BEFORE our shim goes on PATH, so the shim can call it without
/// recursing into itself.
fn real_rm() -> String {
    let out = Command::new("bash")
        .args(["-c", "command -v rm"])
        .output()
        .expect("resolve rm");
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!p.is_empty(), "rm must be on PATH for this test");
    p
}

/// Seed holder.json + heartbeat directly (bypassing rig_lease_acquire) so the release under test
/// has a real, matching-run_id lease to tear down.
fn seed_holder(lease_dir: &Path, repo: &str, run_id: &str) {
    fs::create_dir_all(lease_dir).expect("create lease dir");
    let holder = format!(
        r#"{{"repo": "{repo}", "run_id": "{run_id}", "run_url": "https://example/run/{run_id}", "job": "full-path", "acquired_at": "2026-07-27T19:00:00Z", "expected_release_at": "2099-01-01T00:00:00Z"}}"#
    );
    fs::write(lease_dir.join("holder.json"), holder).expect("write holder.json");
    fs::write(lease_dir.join("heartbeat"), "").expect("write heartbeat");
}

fn set_exec(p: &Path) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

/// Write the shared `rm` shim. When `rm` is invoked on a path under the lease dir (the release's
/// teardown target — `$d` before the fix, `$d.releasing.$$` after), it faithfully emulates
/// `rm -rf`'s internal order: delete the target's holder.json+heartbeat FIRST, then (optionally)
/// let a concurrent acquirer run at that exact instant, then delete the directory. A `probe` file
/// records whether the ACTIVE lease path (`$RIG_LEASE_DIR`) is ever observable as "dir present,
/// holder.json absent". Any other `rm` passes straight through to the real binary.
fn write_rm_shim(
    shim: &Path,
    probe: &Path,
    acq_out: &Path,
    real_rm: &str,
    clean_path: &str,
    script: &Path,
) {
    let body = format!(
        "#!/usr/bin/env bash\n\
         target=\"${{@: -1}}\"\n\
         if [ -z \"${{SHIM_PASSTHROUGH:-}}\" ] && [[ \"$target\" == \"$RIG_LEASE_DIR\"* ]]; then\n\
         \x20 {real:?} -f \"$target/holder.json\" \"$target/heartbeat\" 2>/dev/null\n\
         \x20 if [ -d \"$RIG_LEASE_DIR\" ] && [ ! -f \"$RIG_LEASE_DIR/holder.json\" ]; then echo TORN >> {probe:?}; fi\n\
         \x20 if [ -n \"${{ACQUIRE_DURING:-}}\" ]; then\n\
         \x20\x20 PATH={clean:?} SHIM_PASSTHROUGH=1 bash -c 'set -uo pipefail; . \"$1\"; rig_lease_acquire \"camera-box\" \"new\" \"https://example/run/new\" \"full-path\" \"2099-01-01T00:00:00Z\"' _ {script:?} > {acq_out:?} 2>&1\n\
         \x20 fi\n\
         \x20 {real:?} -rf \"$target\"\n\
         else\n\
         \x20 {real:?} \"$@\"\n\
         fi\n",
        real = real_rm,
        probe = probe,
        acq_out = acq_out,
        clean = clean_path,
        script = script,
    );
    fs::write(shim, body).expect("write rm shim");
    set_exec(shim);
}

struct ReleaseSetup {
    _tmp: tempfile::TempDir,
    lease_dir: PathBuf,
    probe: PathBuf,
    acq_out: PathBuf,
    path_env: String,
}

fn setup() -> ReleaseSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lease_dir = tmp.path().join("lease");
    let probe = tmp.path().join("probe");
    let acq_out = tmp.path().join("acq_out");
    let shim_dir = tmp.path().join("shim");
    fs::create_dir_all(&shim_dir).expect("create shim dir");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    write_rm_shim(
        &shim_dir.join("rm"),
        &probe,
        &acq_out,
        &real_rm(),
        &orig_path,
        &lib_script(),
    );
    let path_env = format!("{}:{}", shim_dir.display(), orig_path);

    ReleaseSetup {
        _tmp: tmp,
        lease_dir,
        probe,
        acq_out,
        path_env,
    }
}

fn run_release(s: &ReleaseSetup, run_id: &str, acquire_during: bool) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(
            "set -uo pipefail\n. \"$SCRIPT\"\nrig_lease_release {run_id:?}"
        ))
        .env("SCRIPT", lib_script())
        .env("RIG_LEASE_DIR", &s.lease_dir)
        .env("PATH", &s.path_env)
        .env("PROBE", &s.probe)
        .env("ACQ_OUT", &s.acq_out);
    if acquire_during {
        cmd.env("ACQUIRE_DURING", "1");
    }
    let out = cmd.output().expect("run rig_lease_release");
    assert!(
        out.status.success(),
        "release must exit 0: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read_holder_run_id(lease_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(lease_dir.join("holder.json")).ok()?;
    let pat = "\"run_id\": \"";
    let start = text.find(pat)? + pat.len();
    let end = text[start..].find('"')? + start;
    Some(text[start..end].to_string())
}

/// A concurrent observer must NEVER see the active lease dir present without its holder.json.
/// RED against `rm -rf "$d"` (the recursive delete exposes exactly that window); GREEN once the
/// release renames `$d` aside atomically before deleting.
#[test]
fn release_never_exposes_a_holderless_lease_dir() {
    let s = setup();
    seed_holder(&s.lease_dir, "camera-box", "old");

    run_release(&s, "old", false);

    let torn = fs::read_to_string(&s.probe).unwrap_or_default();
    assert!(
        !torn.contains("TORN"),
        "#857: release must be atomic — the active lease dir ($d) must never be observable as \
         present-but-holder.json-absent (a recursive `rm -rf` exposes that window). probe={torn:?}"
    );
    assert!(
        !s.lease_dir.exists(),
        "the lease dir must be gone after a matching-run_id release"
    );
}

/// A run that acquires the lease at the exact instant the previous holder is releasing must keep
/// its lease intact — never have it deleted out from under it by the departing `rm -rf "$d"`.
/// RED: the acquirer reclaims into `$d`, then the release's still-running `rm -rf "$d"` destroys
/// that fresh lease (holder.json gone though the acquirer believes it holds). GREEN: the release
/// renamed `$d` aside first, so the acquirer's `mkdir "$d"` succeeds on a fresh dir the release
/// never touches.
#[test]
fn release_never_destroys_a_concurrently_acquired_lease() {
    let s = setup();
    seed_holder(&s.lease_dir, "camera-box", "old");

    run_release(&s, "old", true);

    let acq = fs::read_to_string(&s.acq_out).unwrap_or_default();
    assert!(
        acq.contains("RIG_LEASE_ACQUIRED") || acq.contains("RIG_LEASE_RECLAIMED"),
        "the concurrent acquirer must have taken the lease during the release window: {acq:?}"
    );
    assert_eq!(
        read_holder_run_id(&s.lease_dir).as_deref(),
        Some("new"),
        "#857: the concurrently-acquired lease (run_id=new) must survive the departing release — \
         a non-atomic `rm -rf \"$d\"` deletes it out from under the new holder. acq_out={acq:?}"
    );
}
