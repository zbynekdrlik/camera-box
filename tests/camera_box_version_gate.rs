//! Behavioral guard for `scripts/camera-box-version-gate.sh` — the cross-box camera-box BINARY
//! version-parity precondition gate (#875). The user's requirement: a fleet whose cam boxes run
//! DIFFERENT camera-box builds (live 2026-07-29 cam4 was three builds behind cam1/2/3 and was the
//! only box missing the publish-30p fix) must never be discoverable only by eye or by post-mortem —
//! the gate must REFUSE (fail-closed) the moment ANY active box's camera-box version disagrees with
//! the others.
//!
//! This is a DELIBERATE follow-up split from `dantesync-version-gate.sh` (#862) because the two
//! signals need DIFFERENT comparison models: dantesync uses a FIXED PIN (upgrades rarely), while the
//! camera-box binary is deployed continuously (`1.7.0-dev.NNN` grows on almost every PR) and so has
//! no canonical value to pin against — the only checkable invariant is RELATIVE cross-box parity
//! (every active box agrees with every other), the same model `drift-guard.sh`'s genlock_build_sha
//! parity engine uses. These tests pin the gate's PURE functions (version extraction, the modal
//! reference, the per-box verdict, the fleet-wide roll-up + table print) and its end-to-end exit-code
//! contract over fixture files (the path that needs no live rig).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/camera-box-version-gate.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the gate (its BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.
/// `set +e` after the source neutralizes the sourced script's leaked `set -euo pipefail` (mirrors
/// tests/dantesync_version_gate.rs) — a `body` that calls a verdict function returning non-zero
/// (a DRIFT/UNKNOWN scenario, most of what this file asserts) must not abort the harness.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the gate as a subprocess WITH extra env (the fixture-injection seam); return
/// (exit_code, stdout, stderr).
fn run_gate_env(args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(script());
    cmd.args(args).current_dir(manifest_dir());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run camera-box-version-gate.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "camera-box-version-gate-test-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `text` to a fixture file and return its path (the CAMERA_BOX_VERSION_GATE_VERSION_<NAME>
/// seam cats the file, mirroring dantesync-version-gate.sh's own version fixture convention).
fn write_version_fixture(tag: &str, text: &str) -> PathBuf {
    let p = tmp_dir(tag).join("version.txt");
    std::fs::write(&p, text).unwrap();
    p
}

// ---------------------------------------------------------------------------
// camera_box_version_from_version_output — PURE extraction from `camera-box --version` stdout.
// ---------------------------------------------------------------------------

#[test]
fn version_from_output_extracts_dev_build() {
    let out = run_sourced(
        r#"camera_box_version_from_version_output "$(printf 'camera-box 1.7.0-dev.452\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.7.0-dev.452");
}

#[test]
fn version_from_output_last_match_wins_amid_banner_noise() {
    // A leading SSH banner/MOTD must never be read as the version — the LAST match wins.
    let out = run_sourced(
        r#"camera_box_version_from_version_output "$(printf 'Warning: unknown host key\ncamera-box 1.7.0-dev.403\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.7.0-dev.403");
}

#[test]
fn version_from_output_no_match_is_empty() {
    // Unreachable box (ssh error text, no version) -> "" (UNKNOWN downstream, never guessed).
    let out = run_sourced(
        r#"camera_box_version_from_version_output "$(printf 'ssh: connect to host: Connection refused\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "");
}

#[test]
fn version_from_output_empty_input_is_empty() {
    let out = run_sourced(r#"camera_box_version_from_version_output """#, &[]);
    assert_eq!(out.trim(), "");
}

#[test]
fn version_from_output_release_without_dev_suffix() {
    // A plain release build (no -dev.NNN) must still parse.
    let out = run_sourced(
        r#"camera_box_version_from_version_output "$(printf 'camera-box 1.7.0\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.7.0");
}

// ---------------------------------------------------------------------------
// camera_box_version_verdict — per-box PURE verdict + table row (RELATIVE compare vs the modal).
// ---------------------------------------------------------------------------

#[test]
fn verdict_ok_when_version_matches_the_fleet_majority() {
    let out = run_sourced(
        r#"camera_box_version_verdict cam1 1.7.0-dev.452 1.7.0-dev.452; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam1") && out.contains("1.7.0-dev.452") && out.contains("OK"));
    assert!(out.contains("RC=0"));
}

#[test]
fn verdict_drift_when_version_differs_from_the_majority() {
    // RELATIVE, not pin: DRIFT means "differs from the fleet majority", and the row names it.
    let out = run_sourced(
        r#"camera_box_version_verdict cam4 1.7.0-dev.403 1.7.0-dev.452; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam4") && out.contains("1.7.0-dev.403") && out.contains("DRIFT"));
    assert!(
        out.contains("1.7.0-dev.452"),
        "the drift row must name the majority version it disagrees with: {out:?}"
    );
    assert!(out.contains("RC=20"));
}

#[test]
fn verdict_unknown_when_version_unread() {
    let out = run_sourced(
        r#"camera_box_version_verdict cam3 "" 1.7.0-dev.452; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam3") && out.to_uppercase().contains("UNKNOWN"));
    assert!(out.contains("RC=11"));
}

// ---------------------------------------------------------------------------
// camera_box_fleet_report — fleet-wide roll-up, table print, CAMBOX_OFFLINE_ACK exclusion.
// ---------------------------------------------------------------------------

#[test]
fn fleet_report_all_agree_passes_and_prints_full_table() {
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.452" "cam2=1.7.0-dev.452" "cam3=1.7.0-dev.452"; echo "RC=$?""#,
        &[],
    );
    for name in ["cam1", "cam2", "cam3"] {
        assert!(out.contains(name), "table must list {name}: {out:?}");
    }
    assert!(out.contains("GATE PASS"));
    assert!(out.contains("RC=0"));
}

#[test]
fn fleet_report_one_drifted_box_fails_the_whole_fleet() {
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.452" "cam2=1.7.0-dev.452" "cam3=1.7.0-dev.403" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("DRIFT"),
        "the drifted box must be flagged: {out:?}"
    );
    assert!(out.contains("GATE FAILED"));
    assert!(out.contains("RC=20"));
}

#[test]
fn fleet_report_uniformly_newer_fleet_still_passes_proving_relative_not_pin() {
    // THE #875 property: there is NO fixed pin — a fleet uniformly on a NEWER build than any past
    // version must PASS (contrast dantesync's pin gate, where a stale-but-uniform fleet FAILS).
    // The only thing that matters is that the boxes agree with EACH OTHER.
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.999" "cam2=1.7.0-dev.999" "cam3=1.7.0-dev.999"; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("GATE PASS") && out.contains("RC=0"),
        "a uniformly-newer fleet must PASS (relative parity, no pin): {out:?}"
    );
}

#[test]
fn fleet_report_unread_box_is_unknown_not_a_silent_pass() {
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.452" "cam3=" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(out.to_uppercase().contains("UNKNOWN"));
    assert!(
        out.contains("RC=11"),
        "an unread box must make the gate INCOMPLETE, never a silent pass: {out:?}"
    );
}

#[test]
fn fleet_report_drift_takes_precedence_over_unknown() {
    // A DRIFT (20) must win over an UNKNOWN (11) in the same run — a genuine disagreement is the
    // louder, more actionable failure.
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.452" "cam2=1.7.0-dev.403" "cam3=" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("RC=20"),
        "drift must take precedence over unknown: {out:?}"
    );
}

#[test]
fn fleet_report_acked_offline_box_is_excluded_not_judged() {
    // The SAME CAMBOX_OFFLINE_ACK mechanism recording-e2e.sh already uses (#758/#827): a knowingly
    // offline box is reported EXCLUDED with its reason, never counted UNKNOWN/DRIFT and never a
    // reason to fail the gate.
    let out = run_sourced(
        r#"camera_box_fleet_report "cam1=1.7.0-dev.452" "cam2=1.7.0-dev.452" "cam3="; echo "RC=$?""#,
        &[("CAMBOX_OFFLINE_ACK", "cam3:powered-off-2026-08-14")],
    );
    assert!(
        out.contains("RC=0"),
        "an acked-offline box must not fail the gate: {out:?}"
    );
    assert!(
        out.contains("cam3") && out.to_uppercase().contains("EXCLUDED"),
        "the acked box must be visibly EXCLUDED in the table: {out:?}"
    );
    assert!(
        out.contains("powered-off-2026-08-14"),
        "the exclusion row must carry the ack REASON: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// End-to-end CLI: --linux reading `camera-box --version` output via the fixture-injection seam
// (no live rig). --fleet-file /dev/null keeps rig-fleet.txt out of the offline test.
// ---------------------------------------------------------------------------

#[test]
fn cli_fleet_that_disagrees_refuses_with_a_table() {
    let cam1 = write_version_fixture("cli-d1", "camera-box 1.7.0-dev.452\n");
    let cam2 = write_version_fixture("cli-d2", "camera-box 1.7.0-dev.452\n");
    let cam3 = write_version_fixture("cli-d3", "camera-box 1.7.0-dev.403\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--linux",
            "cam1=root@x cam2=root@y cam3=root@z",
        ],
        &[
            ("CAMBOX_OFFLINE_ACK", ""),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &cam1.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &cam2.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM3",
                &cam3.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a disagreeing fleet must exit 20.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("cam3") && stdout.contains("DRIFT"));
}

#[test]
fn cli_fleet_that_agrees_passes() {
    let cam1 = write_version_fixture("cli-a1", "camera-box 1.7.0-dev.452\n");
    let cam2 = write_version_fixture("cli-a2", "camera-box 1.7.0-dev.452\n");
    let cam3 = write_version_fixture("cli-a3", "camera-box 1.7.0-dev.452\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--linux",
            "cam1=root@x cam2=root@y cam3=root@z",
        ],
        &[
            ("CAMBOX_OFFLINE_ACK", ""),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &cam1.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &cam2.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM3",
                &cam3.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "an agreeing fleet must exit 0.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("GATE PASS"));
}

#[test]
fn cli_refuses_when_no_node_is_given() {
    let (code, _stdout, stderr) = run_gate_env(&["--fleet-file", "/dev/null"], &[]);
    assert_eq!(
        code, 1,
        "zero nodes must be a usage error, never a silent pass"
    );
    assert!(stderr.to_lowercase().contains("no node"));
}
