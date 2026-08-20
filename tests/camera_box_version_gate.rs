//! Behavioral guard for `scripts/camera-box-version-gate.sh` — the camera-box BINARY version gate.
//! The user's requirement: a fleet whose cam boxes run a camera-box build OTHER than main's release
//! (a single box behind — live 2026-07-29 cam4 three builds behind — OR the WHOLE fleet uniformly
//! stale — live: dev.462 for a week while main carried the #1111 fix) must never be discoverable only
//! by eye or by post-mortem; the gate must REFUSE (fail-closed).
//!
//! TWO comparison layers (issue 1136 added the pin on top of the original #875 parity):
//!   * PIN-to-origin/main (DEFAULT) — every active box must run the version in origin/main's
//!     Cargo.toml. This catches a UNIFORMLY-stale fleet, which parity alone misses. The pin is a
//!     MOVING reference (read from origin/main, advanced automatically by the push-to-main
//!     auto-deploy), not the spurious-fail-prone FIXED pin the #875 header once rejected. Fail
//!     CLOSED when the pin itself is unreadable.
//!   * RELATIVE cross-box parity (SUPPLEMENT, `--no-main-pin`) — the legacy #875 model: every active
//!     box agrees with every other (no fixed value), `drift-guard.sh`'s genlock_build_sha shape. The
//!     dormant path for a deliberate pre-merge / operator soak where the fleet is knowingly not yet
//!     on main.
//!
//! These tests pin the gate's PURE functions (version extraction, the modal reference, the per-box
//! parity AND pin verdicts, the fleet-wide roll-ups + table prints) and its end-to-end exit-code
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
            // #1136: the pin-to-main layer is now ON by default; this test exercises the parity
            // SUPPLEMENT in isolation, so it runs with --no-main-pin (the legacy #875 behaviour).
            "--no-main-pin",
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
            // #1136: the pin-to-main layer is now ON by default; this test exercises the parity
            // SUPPLEMENT in isolation, so it runs with --no-main-pin (the legacy #875 behaviour).
            "--no-main-pin",
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

// ---------------------------------------------------------------------------
// #1136 — PIN-to-main layer. Relative parity alone lets a UNIFORMLY-STALE fleet
// pass (every box agrees on an OLD build, live: dev.462 fleet for a week while
// main carried the #1111 fix). With the origin/main Cargo.toml pin available, a
// uniformly-stale fleet MUST be REFUSED — the owner's exact requirement. The pin
// is injected via the CAMERA_BOX_VERSION_GATE_MAIN_PIN fixture seam (production
// reads it from `git show origin/main:Cargo.toml`).
// ---------------------------------------------------------------------------

#[test]
fn cli_uniformly_stale_fleet_is_refused_against_the_main_pin_1136() {
    // Every active box AGREES on an OLD version -> parity-only PASSES. The main
    // pin is NEWER -> the pin layer must REFUSE (exit 20), naming the pin.
    let stale_a = write_version_fixture("stale1136-a", "camera-box 1.7.0-dev.100\n");
    let stale_b = write_version_fixture("stale1136-b", "camera-box 1.7.0-dev.100\n");
    let stale_c = write_version_fixture("stale1136-c", "camera-box 1.7.0-dev.100\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--linux",
            "cam1=x cam2=x cam3=x",
        ],
        &[
            // Explicit empty ack (mirrors the sibling cli_* tests) so a polluted ambient
            // CAMBOX_OFFLINE_ACK can never mask a box as excluded in this refusal test.
            ("CAMBOX_OFFLINE_ACK", ""),
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.487"),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &stale_a.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &stale_b.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM3",
                &stale_c.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a uniformly-stale fleet must be REFUSED against the main pin (20), never a silent PASS.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.to_uppercase().contains("PIN"),
        "the refusal table must name the pin model.\nstdout={stdout}"
    );
    assert!(
        stdout.contains("1.7.0-dev.487"),
        "the refusal must show the expected main pin 1.7.0-dev.487.\nstdout={stdout}"
    );
}

// ---------------------------------------------------------------------------
// #1136 PURE pin functions — camera_box_version_from_cargo_toml (Cargo.toml
// parse), camera_box_pin_verdict (per-box vs the pin), camera_box_fleet_report_pinned
// (fleet roll-up vs the pin). Mirror the parity pure-function tests above.
// ---------------------------------------------------------------------------

#[test]
fn cargo_toml_version_extracts_package_version_ignoring_deps() {
    let out = run_sourced(
        r#"camera_box_version_from_cargo_toml "$(printf '[package]\nname = "camera-box"\nversion = "1.7.0-dev.487"\n\n[dependencies]\nserde = { version = "1.0.200" }\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "1.7.0-dev.487");
}

#[test]
fn cargo_toml_version_empty_when_no_package_version() {
    let out = run_sourced(
        r#"camera_box_version_from_cargo_toml "$(printf '[dependencies]\nserde = { version = "1.0" }\n')""#,
        &[],
    );
    assert_eq!(out.trim(), "");
}

#[test]
fn pin_verdict_ok_when_version_matches_the_main_pin() {
    let out = run_sourced(
        r#"camera_box_pin_verdict cam1 "1.7.0-dev.487" "1.7.0-dev.487"; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam1") && out.contains("OK"));
    assert!(out.contains("RC=0"));
}

#[test]
fn pin_verdict_drift_when_version_differs_from_the_main_pin() {
    let out = run_sourced(
        r#"camera_box_pin_verdict cam1 "1.7.0-dev.100" "1.7.0-dev.487" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam1") && out.contains("PIN-DRIFT") && out.contains("1.7.0-dev.487"));
    assert!(out.contains("RC=20"));
}

#[test]
fn pin_verdict_unknown_when_version_unread() {
    let out = run_sourced(
        r#"camera_box_pin_verdict cam3 "" "1.7.0-dev.487"; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("cam3") && out.to_uppercase().contains("UNKNOWN"));
    assert!(out.contains("RC=11"));
}

#[test]
fn pinned_fleet_report_uniformly_stale_fails_proving_pin_not_parity() {
    // The PARITY report PASSES this exact fleet (fleet_report_uniformly_newer_fleet_still_passes_...);
    // the PIN report must FAIL it — the whole point of #1136.
    let out = run_sourced(
        r#"camera_box_fleet_report_pinned "1.7.0-dev.487" "cam1=1.7.0-dev.100" "cam2=1.7.0-dev.100" "cam3=1.7.0-dev.100" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(
        out.contains("GATE FAILED"),
        "uniformly-stale fleet must FAIL the pin: {out:?}"
    );
    assert!(out.contains("RC=20"));
}

#[test]
fn pinned_fleet_report_all_on_pin_passes() {
    let out = run_sourced(
        r#"camera_box_fleet_report_pinned "1.7.0-dev.487" "cam1=1.7.0-dev.487" "cam2=1.7.0-dev.487"; echo "RC=$?""#,
        &[],
    );
    assert!(out.contains("GATE PASS"));
    assert!(out.contains("RC=0"));
}

#[test]
fn pinned_fleet_report_unread_box_is_unknown_not_a_silent_pass() {
    let out = run_sourced(
        r#"camera_box_fleet_report_pinned "1.7.0-dev.487" "cam1=1.7.0-dev.487" "cam3=" 2>&1; echo "RC=$?""#,
        &[],
    );
    assert!(out.to_uppercase().contains("UNKNOWN"));
    assert!(out.contains("RC=11"));
}

#[test]
fn pinned_fleet_report_acked_offline_box_is_excluded_not_judged() {
    // cam3 is OFF the pin but acked offline -> EXCLUDED, must NOT fail the pin gate.
    let out = run_sourced(
        r#"camera_box_fleet_report_pinned "1.7.0-dev.487" "cam1=1.7.0-dev.487" "cam3=1.7.0-dev.100"; echo "RC=$?""#,
        &[("CAMBOX_OFFLINE_ACK", "cam3:powered-off")],
    );
    assert!(
        out.contains("EXCLUDED"),
        "acked box must be excluded: {out:?}"
    );
    assert!(
        out.contains("RC=0"),
        "acked-offline off-pin box must not fail: {out:?}"
    );
}

// ---------------------------------------------------------------------------
// #1138 — frame-probe (cam2 painter) sha-pin, REPORT-ONLY + DORMANT-unless-supplied.
// /usr/local/bin/frame-probe had no --version, no gate reading it, and no auto-deploy, so a stale
// painter's staleness was detected by nothing (the early-gate-pin-doctrine "frame-probe UNPINNABLE"
// row). This folds a report-only sha-pin into camera-box-version-gate.sh (same cam boxes, same ssh):
// each active box's DEPLOYED frame-probe sha256 vs the current CI build's sha (#1118 pattern). It
// NEVER flips the gate exit, and is DORMANT (no output, no behaviour change) unless an expected sha
// is supplied — so it becomes noise-free live only when the supervisor enables it with the deploy.
// ---------------------------------------------------------------------------

#[test]
fn frame_probe_pin_verdict_ok_alarm_unknown() {
    let ok = run_sourced(
        r#"o="$(frame_probe_pin_verdict cam2 "d47e43f8" "d47e43f8")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        ok.contains("RC=0") && ok.contains("OK"),
        "matching painter -> OK: {ok}"
    );
    let alarm = run_sourced(
        r#"o="$(frame_probe_pin_verdict cam2 "d47e43f8" "beef1234")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        alarm.contains("RC=30") && alarm.contains("ALARM"),
        "lagging painter -> ALARM(30): {alarm}"
    );
    assert!(
        alarm.contains("beef1234"),
        "must name the expected current-build sha: {alarm}"
    );
    let dep_empty = run_sourced(
        r#"o="$(frame_probe_pin_verdict cam2 "" "beef")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        dep_empty.contains("RC=31") && dep_empty.contains("UNKNOWN"),
        "unread deployed -> UNKNOWN(31): {dep_empty}"
    );
    let exp_empty = run_sourced(
        r#"o="$(frame_probe_pin_verdict cam2 "d47e" "")"; rc=$?; printf '%s\n' "$o"; echo "RC=$rc""#,
        &[],
    );
    assert!(
        exp_empty.contains("RC=31") && exp_empty.contains("UNKNOWN"),
        "unresolved expected -> UNKNOWN(31): {exp_empty}"
    );
}

#[test]
fn frame_probe_section_is_dormant_when_no_expected_sha_supplied() {
    // No --frame-probe-expected-sha => the section prints NOTHING and the gate behaves exactly as
    // before (a clean pinned box passes with no frame-probe output). This is the no-regression
    // guarantee for the existing gate.
    let v = write_version_fixture("fp_dormant", "camera-box 1.7.0-dev.497\n");
    let (code, out, _err) = run_gate_env(
        &["--linux", "cam2=root@10.77.9.62"],
        &[
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.497"),
            ("CAMERA_BOX_VERSION_GATE_VERSION_CAM2", v.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "clean pinned box must pass: {out}");
    assert!(out.contains("GATE PASS"), "{out}");
    assert!(
        !out.contains("frame-probe"),
        "the frame-probe section must be DORMANT (silent) when no expected sha is supplied: {out}"
    );
}

#[test]
fn frame_probe_pin_alarm_is_report_only_does_not_block_a_clean_gate() {
    // Expected sha supplied + deployed differs => ALARM report-only; the gate STILL passes (exit 0).
    let v = write_version_fixture("fp_report_only", "camera-box 1.7.0-dev.497\n");
    let (code, out, err) = run_gate_env(
        &[
            "--linux",
            "cam2=root@10.77.9.62",
            "--frame-probe-expected-sha",
            "beefface00000000",
        ],
        &[
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.497"),
            ("CAMERA_BOX_VERSION_GATE_VERSION_CAM2", v.to_str().unwrap()),
            ("FRAME_PROBE_GATE_SHA_CAM2", "d47e43f896917dca"),
        ],
    );
    assert_eq!(
        code, 0,
        "a lagging frame-probe must be report-only (gate still passes). out={out} err={err}"
    );
    assert!(out.contains("GATE PASS"), "{out}");
    assert!(
        out.contains("frame-probe (cam2 painter) sha-pin") && out.contains("ALARM"),
        "the frame-probe section must SCREAM the drift: {out}"
    );
    assert!(
        err.contains("FRAME-PROBE PIN ALARM"),
        "a loud stderr banner must fire: {err}"
    );
}

#[test]
fn frame_probe_pin_ok_when_deployed_matches_current_build() {
    let v = write_version_fixture("fp_ok", "camera-box 1.7.0-dev.497\n");
    let (code, out, _err) = run_gate_env(
        &[
            "--linux",
            "cam2=root@10.77.9.62",
            "--frame-probe-expected-sha",
            "d47e43f896917dca",
        ],
        &[
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.497"),
            ("CAMERA_BOX_VERSION_GATE_VERSION_CAM2", v.to_str().unwrap()),
            ("FRAME_PROBE_GATE_SHA_CAM2", "d47e43f896917dca"),
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("frame-probe") && out.contains("OK"),
        "matching painter must report OK: {out}"
    );
}

// ---------------------------------------------------------------------------
// #1136 addendum: --candidate-pin — the pre-merge bootstrap escape that does
// NOT reopen the uniformly-stale hole. The pull_request E2E deploys THIS run's
// merge-candidate build to the fleet to measure it; the gate accepts a fleet
// uniformly on that candidate (or uniformly on main) and refuses everything
// else exactly as before.
// ---------------------------------------------------------------------------

#[test]
fn cli_candidate_pin_accepts_a_fleet_uniformly_on_the_candidate_build_1136() {
    let a = write_version_fixture("cand1136-a", "camera-box 1.7.0-dev.496\n");
    let b = write_version_fixture("cand1136-b", "camera-box 1.7.0-dev.496\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--candidate-pin",
            "1.7.0-dev.496",
            "--linux",
            "cam1=x cam2=x",
        ],
        &[
            ("CAMBOX_OFFLINE_ACK", ""),
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.481"),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &a.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &b.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 0,
        "a fleet uniformly on the candidate build must PASS when --candidate-pin names it.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("candidate"),
        "the PASS must say it accepted the CANDIDATE build (never a silent main-pin pass).\nstdout={stdout}"
    );
}

#[test]
fn cli_candidate_pin_still_refuses_a_uniformly_stale_fleet_1136() {
    let a = write_version_fixture("candstale1136-a", "camera-box 1.7.0-dev.100\n");
    let b = write_version_fixture("candstale1136-b", "camera-box 1.7.0-dev.100\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--candidate-pin",
            "1.7.0-dev.496",
            "--linux",
            "cam1=x cam2=x",
        ],
        &[
            ("CAMBOX_OFFLINE_ACK", ""),
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.481"),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &a.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &b.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a uniformly-stale fleet (neither main nor candidate) must STAY refused (20) — \
         --candidate-pin must not reopen the issue-1136 hole.\nstdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn cli_candidate_pin_refuses_a_mixed_main_and_candidate_fleet_1136() {
    let a = write_version_fixture("candmix1136-a", "camera-box 1.7.0-dev.481\n");
    let b = write_version_fixture("candmix1136-b", "camera-box 1.7.0-dev.496\n");
    let (code, stdout, stderr) = run_gate_env(
        &[
            "--fleet-file",
            "/dev/null",
            "--candidate-pin",
            "1.7.0-dev.496",
            "--linux",
            "cam1=x cam2=x",
        ],
        &[
            ("CAMBOX_OFFLINE_ACK", ""),
            ("CAMERA_BOX_VERSION_GATE_MAIN_PIN", "1.7.0-dev.481"),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM1",
                &a.display().to_string(),
            ),
            (
                "CAMERA_BOX_VERSION_GATE_VERSION_CAM2",
                &b.display().to_string(),
            ),
        ],
    );
    assert_eq!(
        code, 20,
        "a MIXED fleet (some on main, some on the candidate) must be refused (20) — \
         uniformity stays mandatory under --candidate-pin.\nstdout={stdout}\nstderr={stderr}"
    );
}

// ---------------------------------------------------------------------------
// #1138 (residual) — `--frame-probe-only` mode. The merged frame_probe_pin_report is DORMANT
// because the [0/8] gate call cannot supply an expected bin (the local CI frame-probe is BUILT at
// [1/8], AFTER [0/8]). This mode lets recording-e2e engage the report from [1/8] — where the
// current-build frame-probe exists — WITHOUT re-running the camera-box parity read (no second
// version-parity table). It is ALWAYS report-only (exit 0), cam2-scoped by its caller.
// ---------------------------------------------------------------------------

#[test]
fn frame_probe_only_mode_reports_ok_and_skips_the_parity_read() {
    // --frame-probe-only + a matching expected sha: prints the frame-probe OK row, exits 0, and
    // does NOT run the camera-box parity read (no "GATE PASS", no per-box camera-box version table)
    // — proving the mode is distinct from the full gate and needs no camera-box version fixture.
    let (code, out, _err) = run_gate_env(
        &[
            "--frame-probe-only",
            "--linux",
            "cam2=root@10.77.9.62",
            "--frame-probe-expected-sha",
            "d47e43f896917dca",
        ],
        &[("FRAME_PROBE_GATE_SHA_CAM2", "d47e43f896917dca")],
    );
    assert_eq!(
        code, 0,
        "frame-probe-only must always exit 0 (report-only): {out}"
    );
    assert!(
        out.contains("frame-probe (cam2 painter) sha-pin") && out.contains("OK"),
        "the frame-probe report must run + report OK on a match: {out}"
    );
    assert!(
        !out.contains("GATE PASS"),
        "frame-probe-only must NOT run the camera-box parity layer: {out}"
    );
}

#[test]
fn frame_probe_only_mode_alarm_is_report_only() {
    // A lagging painter under --frame-probe-only: ALARM + stderr banner, but STILL exit 0.
    let (code, out, err) = run_gate_env(
        &[
            "--frame-probe-only",
            "--linux",
            "cam2=root@10.77.9.62",
            "--frame-probe-expected-sha",
            "beefface00000000",
        ],
        &[("FRAME_PROBE_GATE_SHA_CAM2", "d47e43f896917dca")],
    );
    assert_eq!(
        code, 0,
        "frame-probe-only ALARM must be report-only (exit 0): out={out} err={err}"
    );
    assert!(
        out.contains("ALARM"),
        "the drift must be screamed in the report: {out}"
    );
    assert!(
        err.contains("FRAME-PROBE PIN ALARM"),
        "a loud stderr banner must fire: {err}"
    );
}

#[test]
fn frame_probe_only_mode_is_dormant_without_an_expected_sha() {
    // No expected sha => the report prints nothing (dormant), and the mode still exits 0.
    let (code, out, _err) = run_gate_env(
        &["--frame-probe-only", "--linux", "cam2=root@10.77.9.62"],
        &[("FRAME_PROBE_GATE_SHA_CAM2", "d47e43f896917dca")],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("frame-probe (cam2 painter) sha-pin"),
        "with no expected sha the report must stay dormant (silent): {out}"
    );
}
