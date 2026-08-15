//! #828 item 2 — the fleet preflight must name the PHYSICAL cause "no capture card present"
//! instead of the generic "camera-box.service is <state>, not active" when that is detectable.
//!
//! Detected binary-independently from the box's own `/dev/video*` nodes (the issue's own repro
//! signal: `ls /dev/video*` -> "No such file or directory"): the remote gather reports a
//! `VIDEO_NODES=<list>` field, and the verdict, when that field is PRESENT but EMPTY, reports the
//! precise cause. Field ABSENCE (older fixtures) stays healthy/skip so nothing false-fails.
//!
//! Mirrors `tests/harness_preflight_fleet_check_758.rs`'s sourced-bash / fake-bins convention.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/preflight-fleet-check.sh")
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
// preflight_fleet_check_verdict — #828 no-capture-card decision.
// ---------------------------------------------------------------------------

#[test]
fn verdict_names_no_capture_card_when_video_nodes_present_but_empty() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=5 DISK_TMP_PCT=5 RSYSLOGD_CPU=0 JOURNALD_CPU=0 VIDEO_NODES='",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("no capture card present"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_names_no_capture_card_even_when_service_also_not_active() {
    // The physical cause is the most specific/actionable — it wins over the generic
    // "service is <state>, not active" when both are true.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=activating EMITTER_COUNT=0 STRAY_UNITS= \
         VIDEO_NODES='",
    );
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("no capture card present"),
        "stdout={}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("not active"),
        "no-capture-card cause must win over the generic message, stdout={}",
        r.stdout
    );
}

#[test]
fn verdict_stays_generic_when_service_down_but_video_nodes_present() {
    // Honest UNKNOWN: nodes DO exist, the service is down for some OTHER reason -> keep the
    // generic message, do NOT claim "no capture card".
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=inactive EMITTER_COUNT=0 STRAY_UNITS= \
         VIDEO_NODES=/dev/video1,/dev/video2'",
    );
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("not active"), "stdout={}", r.stdout);
    assert!(!r.stdout.contains("no capture card"), "stdout={}", r.stdout);
}

#[test]
fn verdict_passes_a_healthy_box_that_reports_video_nodes() {
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS= \
         DISK_LOG_PCT=5 DISK_TMP_PCT=5 RSYSLOGD_CPU=0 JOURNALD_CPU=0 \
         VIDEO_NODES=/dev/video1,/dev/video2'",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "",
        "a healthy box with a grabber must PASS (empty verdict)"
    );
}

#[test]
fn verdict_back_compat_missing_video_nodes_field_never_false_fails() {
    // A fixture line predating #828 has no VIDEO_NODES field at all -> must not be mistaken for
    // "no capture card" (empty), it defaults to healthy/skip.
    let r = run_sourced(
        "preflight_fleet_check_verdict 'SERVICE_ACTIVE=active EMITTER_COUNT=1 STRAY_UNITS='",
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(
        r.stdout, "",
        "absent VIDEO_NODES must default to healthy, not FAIL, got: {}",
        r.stdout
    );
}

// ---------------------------------------------------------------------------
// preflight_fleet_check_cmds — real execution against a fake `ls`.
// ---------------------------------------------------------------------------

#[test]
fn gather_reports_video_nodes_when_present() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo active; exit 0;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 1; exit 0"),
            ("df", "printf 'Use%%\\n1%%\\n'"),
            ("ps", "printf 'bash 0.1\\n'"),
            ("ls", "printf '/dev/video1\\n/dev/video2\\n'"),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("VIDEO_NODES=/dev/video1,/dev/video2"),
        "stdout={}",
        r.stdout
    );
}

#[test]
fn gather_reports_empty_video_nodes_when_absent() {
    let r = run_with_fake_bins(
        "eval \"$(preflight_fleet_check_cmds)\"",
        &[
            (
                "systemctl",
                "case \"$1\" in is-active) echo activating; exit 0;; list-units) exit 0;; esac",
            ),
            ("pgrep", "echo 0; exit 1"),
            ("df", "printf 'Use%%\\n1%%\\n'"),
            ("ps", "printf 'bash 0.1\\n'"),
            // No capture nodes: ls prints nothing and exits non-zero (glob unmatched).
            ("ls", "exit 2"),
        ],
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("VIDEO_NODES="),
        "the field must always be present, stdout={}",
        r.stdout
    );
    // present-but-empty -> the verdict over this same line must name the physical cause.
    let v = run_sourced(&format!("preflight_fleet_check_verdict '{}'", r.stdout));
    assert!(
        v.stdout.contains("no capture card present"),
        "verdict over gathered line stdout={}",
        v.stdout
    );
}
