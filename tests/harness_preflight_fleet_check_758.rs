//! #758 item 1 — `scripts/lib/preflight-fleet-check.sh`: the per-box minute-0 preflight check
//! (`systemctl is-active camera-box` + exact-name `pgrep -cx camera-box` emitter count + stray
//! `camera-box-burn-*` unit survival AFTER the self-heal sweep). Proven for real by executing the
//! REMOTE command against a fake `systemctl`/`pgrep` on PATH (mirrors
//! `tests/harness_event_assert_wired_722.rs`'s `run_with_fake_bins` convention), plus the PURE
//! verdict decision tested directly with hand-built input lines.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/preflight-fleet-check.sh")
}

struct Run {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_sourced(body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run_with_fake_bins(body: &str, fakes: &[(&str, &str)]) -> Run {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    for (name, script_body) in fakes {
        let p = bin_dir.join(name);
        fs::write(&p, format!("#!/usr/bin/env bash\n{script_body}\n")).unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        fs::set_permissions(&p, perm).unwrap();
    }
    let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PATH", path_env)
        .output()
        .expect("run with fake bins");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// preflight_fleet_check_cmds — real execution against fake systemctl/pgrep.
// ---------------------------------------------------------------------------

#[test]
fn check_reports_active_service_one_emitter_no_stray_units() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 1; exit 0"),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("SERVICE_ACTIVE=active"),
        "stdout={}",
        r.stdout
    );
    assert!(r.stdout.contains("EMITTER_COUNT=1"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("STRAY_UNITS="), "stdout={}", r.stdout);
}

#[test]
fn check_reports_a_stray_unit_when_one_survives() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; \
                 list-units) echo 'camera-box-burn-911002.service'; exit 0;; esac",
            ),
            ("pgrep", "echo 1; exit 0"),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout
            .contains("STRAY_UNITS=camera-box-burn-911002.service"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn check_reports_zero_emitters_when_the_service_is_down() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo inactive; exit 3;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 0; exit 1"), // pgrep -c prints 0 AND exits non-zero on no match
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("SERVICE_ACTIVE=inactive"),
        "stdout={}",
        r.stdout
    );
    assert!(r.stdout.contains("EMITTER_COUNT=0"), "stdout={}", r.stdout);
}

// ---------------------------------------------------------------------------
// preflight_fleet_check_verdict — pure decision, no I/O.
// ---------------------------------------------------------------------------

#[test]
fn verdict_is_empty_pass_on_a_clean_line() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "", "a clean box must PASS (empty verdict)");
}

#[test]
fn verdict_fails_on_an_inactive_service() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=inactive EMITTER_COUNT=0 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("not active"), "stdout={}", r.stdout);
}

#[test]
fn verdict_fails_on_zero_emitters() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=0 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("exactly ONE"), "stdout={}", r.stdout);
}

#[test]
fn verdict_fails_on_two_emitters() {
    // A "double-launched" camera-box (e.g. a wedged prior instance never reaped) is JUST as
    // wrong as zero -- exactly ONE, not "at least one".
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=2 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("exactly ONE") && r.stdout.contains("found 2"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_fails_on_a_surviving_stray_unit() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 \
         STRAY_UNITS=camera-box-burn-911002.service'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("camera-box-burn-911002.service"),
        "stdout={}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------------------------
// #762 item 2 — disk-fill / log-storm-signature preflight, added to the SAME one-round-trip line.
// ---------------------------------------------------------------------------------------------

#[test]
fn check_reports_disk_and_cpu_fields_alongside_the_758_fields() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 1; exit 0"),
            (
                "df",
                "case \"$*\" in *'/var/log'*) printf 'Use%%\\n12%%\\n';; *'/tmp'*) printf 'Use%%\\n7%%\\n';; esac",
            ),
            (
                "ps",
                "printf 'rsyslogd 0.0\\nsystemd-journal 1.5\\nbash 0.1\\n'",
            ),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("DISK_LOG_PCT=12"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("DISK_TMP_PCT=7"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("RSYSLOGD_CPU=0"), "stdout={}", r.stdout);
    assert!(r.stdout.contains("JOURNALD_CPU=1"), "stdout={}", r.stdout);
}

#[test]
fn check_sums_cpu_across_multiple_matching_processes() {
    // A storm can spawn/thread such that more than one matching row appears -- sum them, don't
    // just take the first.
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 1; exit 0"),
            ("df", "printf 'Use%%\\n1%%\\n'"),
            ("ps", "printf 'rsyslogd 20.5\\nrsyslogd 22.3\\n'"),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("RSYSLOGD_CPU=42"), "stdout={}", r.stdout);
}

// ---------------------------------------------------------------------------------------------
// preflight_fleet_check_verdict — #762 disk/CPU thresholds (pure decision, no I/O).
// ---------------------------------------------------------------------------------------------

#[test]
fn verdict_is_still_empty_pass_when_disk_and_cpu_fields_are_healthy() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=12 DISK_TMP_PCT=7 RSYSLOGD_CPU=0 JOURNALD_CPU=1'",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, "", "a healthy box must PASS (empty verdict)");
}

#[test]
fn verdict_fails_on_var_log_over_80_percent() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=92 DISK_TMP_PCT=5 RSYSLOGD_CPU=0 JOURNALD_CPU=0'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("/var/log at 92%") && r.stdout.contains("#762"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_fails_on_var_log_at_exactly_the_threshold() {
    // >= threshold, not strictly >, matches cam_disk_alert_needed's own boundary convention.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=80 DISK_TMP_PCT=5 RSYSLOGD_CPU=0 JOURNALD_CPU=0'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("/var/log at 80%"), "stdout={}", r.stdout);
}

#[test]
fn verdict_fails_on_tmp_over_80_percent() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=5 DISK_TMP_PCT=85 RSYSLOGD_CPU=0 JOURNALD_CPU=0'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("/tmp at 85%"), "stdout={}", r.stdout);
}

#[test]
fn verdict_fails_on_rsyslogd_cpu_over_20_percent_the_storm_signature() {
    // The exact live cam1 signal (42.8% CPU) -- catches the storm BEFORE the tmpfs is full.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=30 DISK_TMP_PCT=5 RSYSLOGD_CPU=43 JOURNALD_CPU=0'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("rsyslogd CPU at 43%") && r.stdout.contains("storm signature"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_fails_on_journald_cpu_over_20_percent() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=5 DISK_TMP_PCT=5 RSYSLOGD_CPU=0 JOURNALD_CPU=25'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("systemd-journald CPU at 25%"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_checks_service_health_before_disk_and_cpu() {
    // Ordering: an inactive service is a more urgent/actionable failure than a disk warning on
    // the SAME box -- the existing (#758) checks must still win when both are present.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=inactive EMITTER_COUNT=0 STRAY_UNITS= \
         DISK_LOG_PCT=99 DISK_TMP_PCT=99 RSYSLOGD_CPU=99 JOURNALD_CPU=99'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("not active"), "stdout={}", r.stdout);
}

#[test]
fn verdict_defaults_missing_disk_and_cpu_fields_to_zero_never_a_false_fail() {
    // A hand-built fixture line predating #762 (e.g. an older test, or a caller that hasn't
    // upgraded its remote gather yet) must not spuriously FAIL just because the new fields are
    // absent -- absence defaults to 0 (healthy), matching the existing STRAY_UNITS convention.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "",
        "missing #762 fields must default to healthy, not FAIL, got: {}",
        r.stdout
    );
}
