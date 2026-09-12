//! Behavioral guard for the REPORT-ONLY maintenance-tier dantesync health gate
//! `scripts/dantesync-maintenance-gate.sh` (issue 1297).
//!
//! ## Why this gate exists (issue 1297)
//!
//! RESOLUME-SNV is a TRAVELING CG box that runs dantesync but is NOT a measured source in the
//! cam->strih->stream recording path, so it must never be wired into `recording-e2e.sh`'s blocking
//! `[0/8]` precondition set (that would fail-CLOSE every E2E whenever the box is away). With no
//! check at all it silently drifted: `ntp_server` pointed at `strih.lan` (unresolvable while strih
//! is off) so NTP phase discipline died (`ntp_failed`, 0 samples, a -14ms accumulated phase walk),
//! and `phase_slew` was off so the box STEPS the clock -- the issue-1130 storm the rig boxes + mbc
//! already cured. `dantesync-version-gate.sh` checks only the daemon VERSION; `dantesync-gate.sh`
//! is the blocking `[0/8]` gate a traveling box cannot join. So this is a SEPARATE, standalone,
//! maintenance-cadence gate that asserts version pin + live lock/NTP/phase on the SAME cadence the
//! fleet asserts strih/stream -- REPORT-ONLY, printing ONE honest row: SKIP when away (never a
//! false red), OK / ALARM / UNKNOWN when home.
//!
//! Same PURE-planner model as tests/dantesync_fleet_upgrade.rs / tests/dantesync_gate.rs: these
//! tests source the REAL script (its `BASH_SOURCE != $0` guard skips the network flow) and exercise
//! its pure `dantesync_maintenance_verdict` directly, plus a few full-script runs driven entirely
//! by the `DANTESYNC_MAINT_STATUS_<NAME>` / `DANTESYNC_MAINT_VERSION_<NAME>` / `OBS_FLEET_HOME`
//! fixture seams. NO test here ever curls, ssh's, or MCP's a real box.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/dantesync-maintenance-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script (its `BASH_SOURCE != $0` guard skips the flow) and run `body`. The sourced
/// sub-gates re-enable `set -e`, so a pure fn that returns non-zero is captured via `|| rc=$?`
/// (the `||` suppresses `-e`), never by letting the harness abort. Returns (captured_rc, stdout).
fn verdict(args: &str) -> (i32, String) {
    let body = format!("rc=0\ndantesync_maintenance_verdict {args} || rc=$?\necho \"RC=$rc\"");
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let rc = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RC="))
        .and_then(|n| n.trim().parse::<i32>().ok())
        .expect("no RC= line in harness stdout");
    (rc, stdout)
}

/// Run the script as a program with the given env fixtures + args. Returns (exit_code, stdout, stderr).
fn run_script(env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run script");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const GOOD: &str = r#"{"mode":"NANO","is_locked":true,"ntp_failed":false,"ntp_age_s":12,"ntp_offset_us":-5,"phase_slew_enabled":true}"#;
// the LIVE broken state the ticket reported (supervisor read-back 2026-09-12).
const BROKEN: &str = r#"{"mode":"NANO","is_locked":true,"ntp_failed":true,"ntp_age_s":null,"ntp_offset_us":-14020,"phase_slew_enabled":false}"#;

// ---------------------------------------------------------------- pure verdict: away -> SKIP
#[test]
fn away_traveling_box_is_skip_never_a_false_red() {
    let (rc, out) = verdict(r#"resolume 0 "" "" 1.8.53 120 2000"#);
    assert_eq!(rc, 0, "away must be rc 0 (never a false red): {out}");
    assert!(out.contains("SKIP"), "expected a SKIP row: {out}");
}

// ---------------------------------------------------------------- pure verdict: home + healthy
#[test]
fn home_and_healthy_is_ok() {
    let (rc, out) = verdict(&format!(
        r#"resolume 1 "dantesync 1.8.53" '{GOOD}' 1.8.53 120 2000"#
    ));
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("OK"), "{out}");
    assert!(out.contains("PTP LOCKED"), "{out}");
    assert!(out.contains("NTP fresh"), "{out}");
    assert!(out.contains("phase-slew ENABLED"), "{out}");
}

// ---------------------------------------------------------------- pure verdict: the live broken state
#[test]
fn home_and_broken_ntp_and_phase_is_alarm() {
    let (rc, out) = verdict(&format!(
        r#"resolume 1 "dantesync 1.8.53" '{BROKEN}' 1.8.53 120 2000"#
    ));
    assert_eq!(rc, 30, "a read, wrong field must ALARM (30): {out}");
    assert!(out.contains("ALARM"), "{out}");
    assert!(out.contains("phase-slew DISABLED"), "{out}");
    // ntp_failed=true + ntp_age_s null -> the "never measured" signal.
    assert!(
        out.contains("NTP never") || out.contains("NTP stale"),
        "{out}"
    );
}

// ---------------------------------------------------------------- pure verdict: version drift
#[test]
fn version_drift_is_alarm() {
    let (rc, out) = verdict(&format!(
        r#"resolume 1 "dantesync 1.8.41" '{GOOD}' 1.8.53 120 2000"#
    ));
    assert_eq!(rc, 30, "{out}");
    assert!(out.contains("1.8.41") && out.contains("1.8.53"), "{out}");
}

// ---------------------------------------------------------------- pure verdict: unreadable -> UNKNOWN
#[test]
fn unreadable_status_is_unknown_never_a_silent_pass() {
    let (rc, out) = verdict(r#"resolume 1 "dantesync 1.8.53" "" 1.8.53 120 2000"#);
    assert_eq!(
        rc, 11,
        "an unread field is UNKNOWN (11), never a silent OK: {out}"
    );
    assert!(out.contains("UNKNOWN"), "{out}");
}

// ---------------------------------------------------------------- pure verdict: offset over bound
#[test]
fn offset_over_bound_is_alarm() {
    let oob = r#"{"mode":"NANO","is_locked":true,"ntp_failed":false,"ntp_age_s":12,"ntp_offset_us":5000,"phase_slew_enabled":true}"#;
    let (rc, out) = verdict(&format!(
        r#"resolume 1 "dantesync 1.8.53" '{oob}' 1.8.53 120 2000"#
    ));
    assert_eq!(rc, 30, "{out}");
    assert!(out.contains("5000us>2000"), "{out}");
}

// ---------------------------------------------------------------- full script via fixtures
#[test]
fn full_script_away_exits_zero() {
    // resolume not in the OBS_FLEET_HOME list -> away -> SKIP -> exit 0 (report-only, never a red).
    let (code, out, _err) = run_script(
        &[("OBS_FLEET_HOME", "strih stream")],
        &["--box", "resolume"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("SKIP"), "{out}");
}

#[test]
fn full_script_home_broken_exits_30() {
    let (code, _out, err) = run_script(
        &[
            ("OBS_FLEET_HOME", "resolume"),
            ("DANTESYNC_MAINT_STATUS_RESOLUME", BROKEN),
            ("DANTESYNC_MAINT_VERSION_RESOLUME", "dantesync 1.8.53"),
        ],
        &["--box", "resolume"],
    );
    assert_eq!(
        code, 30,
        "home + broken fixtures must exit 30; stderr={err}"
    );
    assert!(err.contains("MAINT ALARM"), "{err}");
}

#[test]
fn full_script_home_healthy_exits_zero() {
    let (code, out, _err) = run_script(
        &[
            ("OBS_FLEET_HOME", "resolume"),
            ("DANTESYNC_MAINT_STATUS_RESOLUME", GOOD),
            ("DANTESYNC_MAINT_VERSION_RESOLUME", "dantesync 1.8.53"),
        ],
        &["--box", "resolume"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("OK"), "{out}");
}

#[test]
fn full_script_unknown_box_exits_11() {
    let (code, out, _err) = run_script(&[], &["--box", "nosuchbox"]);
    assert_eq!(code, 11, "{out}");
    assert!(out.contains("not in the OBS_FLEET table"), "{out}");
}
