//! #797 — offline integration guard for `scripts/netcfg-drift-alert-watchdog.sh`'s DECISION
//! COMPOSITION (`main`): CLEAN clears + fires recovery-when-previously-alerted, DRIFT confirms across
//! N passes before paging, a CONFIRMED drift then WOULD-alert (dry-run), and a gather ERROR (rc != 0
//! and != 3) is "nothing to decide" (never a page — reachability is #1001's job). The pure classify
//! core is covered by `tests/harness_netcfg_audit_797.rs`; this covers the CALLER GLUE that drives
//! the alert/recovery latches (per #414 — an unattended production timer's novel wiring is tested,
//! weighted like a correctness bug).
//!
//! Fully offline + deterministic: the read-only ssh audit is replaced via `NETCFG_DRIFT_AUDIT_CMD`
//! (a stub whose exit code + summary the test rewrites per pass), the run is `--dry-run` (never a
//! real notify), and the state file is a per-test tempdir path. Same fixture-shim shape as
//! `tests/harness_asio_starve_alert_watchdog_1023.rs` / `tests/harness_bundle_state_watchdog_732.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/netcfg-drift-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// A per-test rig: a tempdir with a stub audit that ignores `--check` and exits with a code + summary
/// the test rewrites per pass; plus the alert state file. Reused across passes within one test so the
/// confirm / alert / recovery latches persist exactly as they would on the real hourly timer.
struct Rig {
    _dir: tempfile::TempDir,
    audit: PathBuf,
    rc_file: PathBuf,
    summary_file: PathBuf,
    state: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let audit = dir.path().join("audit-stub.sh");
        // Ignores its `--check` arg; prints the current summary, exits with the current rc.
        write_exec(
            &audit,
            "#!/usr/bin/env bash\ncat \"$NC_STUB_SUMMARY\" 2>/dev/null\nexit \"$(cat \"$NC_STUB_RC\" 2>/dev/null || echo 0)\"\n",
        );
        Rig {
            audit,
            rc_file: dir.path().join("rc"),
            summary_file: dir.path().join("summary"),
            state: dir.path().join("netcfg-drift.state"),
            _dir: dir,
        }
    }

    /// Set what the stub audit will do on the NEXT pass.
    fn set_audit(&self, rc: i32, summary: &str) {
        fs::write(&self.rc_file, rc.to_string()).unwrap();
        fs::write(&self.summary_file, summary).unwrap();
    }

    /// Run ONE `--dry-run` watchdog pass; returns its stderr (the log lines).
    fn pass(&self) -> String {
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env("NETCFG_DRIFT_AUDIT_CMD", &self.audit)
            .env("NETCFG_DRIFT_STATE_FILE", &self.state)
            .env("NC_STUB_RC", &self.rc_file)
            .env("NC_STUB_SUMMARY", &self.summary_file)
            // point notify at a guaranteed-absent path so even a bug that reached the non-dry-run
            // branch could not actually post (belt-and-braces; --dry-run already never notifies)
            .env("AIRULESET_NOTIFY", self._dir.path().join("no-such-notify.py"))
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run watchdog");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

// ------------------------------------------------------------------------------------------------
#[test]
fn help_and_bad_arg() {
    let help = Command::new("bash").arg(watchdog()).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("netcfg-drift-alert-watchdog"),
        "--help should print the header");

    let bad = Command::new("bash").arg(watchdog()).arg("--bogus").output().unwrap();
    assert_eq!(bad.status.code(), Some(2), "an unknown arg exits 2");
}

#[test]
fn clean_pass_does_not_alert() {
    let rig = Rig::new();
    rig.set_audit(0, "NETCFG-CLEAN: venue switch chain matches baseline");
    let log = rig.pass();
    assert!(log.contains("audit rc=0"), "log:\n{log}");
    assert!(!log.contains("WOULD alert"), "a CLEAN pass must never alert:\n{log}");
}

#[test]
fn drift_confirms_across_two_passes_then_would_alert() {
    let rig = Rig::new();
    rig.set_audit(3, "NETCFG-DRIFT: venue switch chain drifted from baseline (1 finding(s))");

    let p1 = rig.pass();
    assert!(p1.contains("audit rc=3"), "p1:\n{p1}");
    assert!(p1.contains("holding"), "pass 1 must HOLD (not yet confirmed):\n{p1}");
    assert!(!p1.contains("WOULD alert"), "pass 1 must not alert:\n{p1}");

    let p2 = rig.pass();
    assert!(p2.contains("WOULD alert"), "pass 2 must alert once confirmed:\n{p2}");
}

#[test]
fn drift_then_clean_fires_recovery() {
    let rig = Rig::new();
    // two DRIFT passes -> confirmed + alerted latch set
    rig.set_audit(3, "NETCFG-DRIFT: venue switch chain drifted from baseline (1 finding(s))");
    rig.pass();
    let p2 = rig.pass();
    assert!(p2.contains("WOULD alert"), "p2:\n{p2}");
    // now CLEAN -> recovery ping (because we were alerted)
    rig.set_audit(0, "NETCFG-CLEAN: venue switch chain matches baseline");
    let p3 = rig.pass();
    assert!(p3.contains("WOULD send recovery"), "a CLEAN after an alert must recover:\n{p3}");
    // and a subsequent CLEAN is silent (latch cleared)
    let p4 = rig.pass();
    assert!(!p4.contains("WOULD send recovery"), "recovery fires ONCE, not every clean pass:\n{p4}");
}

#[test]
fn audit_error_is_nothing_to_decide() {
    let rig = Rig::new();
    rig.set_audit(2, ""); // usage/gather error
    let log = rig.pass();
    assert!(log.contains("nothing to decide"), "an audit error must not page:\n{log}");
    assert!(!log.contains("WOULD alert"), "an audit error must never alert:\n{log}");
}

#[test]
fn single_drift_pass_is_not_enough_to_alert() {
    // a transient one-pass drift (e.g. a mid-reconfig read) must never page on its own
    let rig = Rig::new();
    rig.set_audit(3, "NETCFG-DRIFT: venue switch chain drifted from baseline (1 finding(s))");
    let p1 = rig.pass();
    assert!(!p1.contains("WOULD alert"), "one drift pass alone must not alert:\n{p1}");
}
