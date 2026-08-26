//! #1053 -- post-wake reachability verify for the WoL sender (the "after wake, verify availability"
//! half of remote recovery). `scripts/wake-box.sh` could SEND a magic packet but had no way to
//! confirm the box actually came back -- that was only a manual runbook step. This adds
//! `--wait[=SECS]`: after the send, poll the target's reachability until it responds (exit 0,
//! `WAKE-VERIFY UP`) or the budget elapses (exit 4, `WAKE-VERIFY STILL-DOWN`), so a recovery loop
//! (issue 1001 detect-down -> wake -> confirm-up) is one composable command.
//!
//! Pure host resolution (`wol_verify_host`, scripts/lib/wol.sh) is tested BEHAVIORALLY by sourcing
//! the lib (no network); the wake-box.sh poll loop is tested BEHAVIORALLY with a stub `python3`
//! (so NO real UDP packet is ever broadcast) plus a deterministic `WOL_PING_CMD` (`true`/`false`),
//! and STRUCTURALLY. RED without the impl (no `wol_verify_host`, no `--wait`), GREEN with it.

use std::path::PathBuf;
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source scripts/lib/wol.sh and run `code`, returning (stdout, ok). Pure -- no network.
fn run_wol(code: &str) -> (String, bool) {
    let out = Command::new("bash")
        .current_dir(manifest_dir())
        .arg("-c")
        .arg(format!(". scripts/lib/wol.sh; {code}"))
        .output()
        .expect("spawn bash");
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        out.status.success(),
    )
}

/// A temp dir holding a stub `python3` (drains stdin + args, sends NOTHING) so a non-dry-run
/// wake-box.sh run performs NO real UDP broadcast. Returns the kept TempDir guard (drop it to clean
/// up -- keep it alive across the wake_box call) plus a PATH prepending the dir to the real PATH.
/// Uses tempfile::tempdir() (a kernel-atomic unique name, the repo's #975 convention) rather than a
/// hand-rolled pid path, so there is no collision or leak.
fn stub_python_path() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let py = dir.path().join("python3");
    std::fs::write(&py, "#!/bin/sh\ncat >/dev/null 2>&1 || true\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&py, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (dir, path)
}

fn wake_box(args: &[&str], envs: &[(&str, &str)], path_override: Option<&str>) -> Output {
    let mut c = Command::new("bash");
    c.current_dir(manifest_dir());
    c.arg("scripts/wake-box.sh").args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    if let Some(p) = path_override {
        c.env("PATH", p);
    }
    c.output().expect("run wake-box.sh")
}

// A three-box table matching wol-targets.txt (as a printf '%b' setup so \n expands).
const TBL: &str = "strih 10.77.9.202 5C:6A:80:F6:6C:F7\\nstream 10.77.9.204 E8:9C:25:CE:B6:EA\\nimag-nb 10.77.9.182 6C:1F:F7:66:15:4B";

// ---------------------------------------------------------------------------------------------
// 1. scripts/lib/wol.sh -- wol_verify_host, BEHAVIORAL (pure)
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_host_resolves_a_box_to_its_table_ip() {
    let setup = format!("TBL=$(printf '%b' \"{TBL}\"); ");
    for (target, want) in [
        ("strih", "10.77.9.202"),
        ("stream", "10.77.9.204"),
        ("imag-nb", "10.77.9.182"),
    ] {
        let (out, ok) = run_wol(&format!("{setup}wol_verify_host \"$TBL\" {target} ''"));
        assert!(ok, "#1053: verify-host for box {target} should succeed");
        assert_eq!(out, want, "#1053: {target} -> its table ip");
    }
}

#[test]
fn verify_host_override_wins_over_the_table() {
    let setup = format!("TBL=$(printf '%b' \"{TBL}\"); ");
    let (out, ok) = run_wol(&format!("{setup}wol_verify_host \"$TBL\" strih 10.0.0.9"));
    assert!(ok, "#1053: an explicit override should resolve");
    assert_eq!(
        out, "10.0.0.9",
        "#1053: a --wait-host override must win over the table ip"
    );
}

#[test]
fn verify_host_raw_mac_needs_an_override() {
    let setup = format!("TBL=$(printf '%b' \"{TBL}\"); ");
    // a raw MAC is not a box, carries no IP; with no override there is nothing safe to poll -> fail.
    let (_o, ok) = run_wol(&format!(
        "{setup}wol_verify_host \"$TBL\" 5c6a80f66cf7 '' 2>/dev/null"
    ));
    assert!(
        !ok,
        "#1053: a raw-MAC target with no override must FAIL (never poll a wrong/no host)"
    );
    // ... but WITH an override it resolves to that host.
    let (out, ok2) = run_wol(&format!(
        "{setup}wol_verify_host \"$TBL\" 5c6a80f66cf7 10.0.0.9"
    ));
    assert!(
        ok2 && out == "10.0.0.9",
        "#1053: --wait-host lets a raw-MAC wake be verified, got ok={ok2} out={out:?}"
    );
}

#[test]
fn verify_host_unknown_box_fails_loud() {
    let setup = format!("TBL=$(printf '%b' \"{TBL}\"); ");
    let (_o, ok) = run_wol(&format!(
        "{setup}wol_verify_host \"$TBL\" nosuch '' 2>/dev/null"
    ));
    assert!(!ok, "#1053: an unknown box with no override must fail loud");
}

// ---------------------------------------------------------------------------------------------
// 2. scripts/wake-box.sh -- STRUCTURAL contract for the verify path
// ---------------------------------------------------------------------------------------------

#[test]
fn wake_box_offers_the_wait_verify_option() {
    let s = read("scripts/wake-box.sh");
    assert!(s.contains("--wait"), "#1053: wake-box.sh must offer --wait");
    assert!(
        s.contains("wol_verify_host"),
        "#1053: must resolve the poll host via the pure lib fn"
    );
    assert!(
        s.contains("WOL_PING_CMD"),
        "#1053: the reachability probe must be env-injectable (WOL_PING_CMD)"
    );
    assert!(
        s.contains("WAKE-VERIFY"),
        "#1053: must report a WAKE-VERIFY UP/STILL-DOWN verdict"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. scripts/wake-box.sh -- BEHAVIORAL (dry-run plan, UP, STILL-DOWN, raw-MAC guard, bad budget)
// ---------------------------------------------------------------------------------------------

#[test]
fn wait_dry_run_prints_the_verify_plan_and_sends_nothing() {
    let out = wake_box(&["strih", "--dry-run", "--wait=30"], &[], None);
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "#1053: --dry-run --wait must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        so.contains("10.77.9.202"),
        "#1053: the verify plan must name the poll host (strih ip):\n{so}"
    );
    assert!(
        so.contains("30"),
        "#1053: the verify plan must state the budget (30s):\n{so}"
    );
    assert!(
        so.contains("DRY-RUN: no packet sent"),
        "#1053: dry-run must send nothing:\n{so}"
    );
    assert!(
        !so.contains("WAKE-VERIFY UP") && !so.contains("WAKE-VERIFY STILL-DOWN"),
        "#1053: dry-run must NOT actually poll:\n{so}"
    );
}

#[test]
fn wait_reports_up_when_the_probe_succeeds() {
    let (_stub, path) = stub_python_path();
    let out = wake_box(
        &["strih", "--wait=6"],
        &[("WOL_PING_CMD", "true"), ("WOL_WAIT_INTERVAL", "1")],
        Some(&path),
    );
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "#1053: an immediately-reachable box -> exit 0 (UP): out={so} err={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        so.contains("WAKE-VERIFY UP") && so.contains("10.77.9.202"),
        "#1053: must report UP with the host:\n{so}"
    );
}

#[test]
fn wait_reports_still_down_and_exits_4_on_timeout() {
    let (_stub, path) = stub_python_path();
    let out = wake_box(
        &["strih", "--wait=2"],
        &[("WOL_PING_CMD", "false"), ("WOL_WAIT_INTERVAL", "1")],
        Some(&path),
    );
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "#1053: a never-reachable box must exit non-zero:\n{so}\n{se}"
    );
    assert_eq!(
        out.status.code(),
        Some(4),
        "#1053: STILL-DOWN uses the distinct exit code 4 (vs 2 for bad args)"
    );
    assert!(
        (so.to_string() + &se).contains("WAKE-VERIFY STILL-DOWN"),
        "#1053: must report STILL-DOWN:\n{so}\n{se}"
    );
}

#[test]
fn wait_on_a_raw_mac_requires_wait_host() {
    // no --wait-host: a raw MAC carries no IP -> config error, fail before any send (dry-run).
    let out = wake_box(&["5C:6A:80:F6:6C:F7", "--wait=5", "--dry-run"], &[], None);
    assert!(
        !out.status.success(),
        "#1053: --wait on a raw MAC without --wait-host must fail loud"
    );
    // with --wait-host: resolves, and the dry-run plan names the override host.
    let out2 = wake_box(
        &[
            "5C:6A:80:F6:6C:F7",
            "--wait=5",
            "--wait-host",
            "10.0.0.9",
            "--dry-run",
        ],
        &[],
        None,
    );
    let so2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        out2.status.success(),
        "#1053: --wait-host lets a raw-MAC wake be verified: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(
        so2.contains("10.0.0.9"),
        "#1053: the verify plan must name the --wait-host:\n{so2}"
    );
}

#[test]
fn wait_rejects_a_non_numeric_budget() {
    let out = wake_box(&["strih", "--wait=abc", "--dry-run"], &[], None);
    assert!(
        !out.status.success(),
        "#1053: --wait=<non-integer> must fail loud, never default silently"
    );
}
