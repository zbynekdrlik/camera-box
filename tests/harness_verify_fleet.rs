//! Regression guard for `scripts/verify-fleet.sh` — the fleet-wide drift-guard loop over
//! `scripts/verify-device.sh` (#552, remaining #547 "keeping it converged" work).
//!
//! `verify-device.sh` certifies ONE box at a time. Before #552 there was no fleet-wide runner —
//! confirming the whole fleet stayed converged meant re-running `verify-device.sh` by hand for
//! every camera and eyeballing the results. `verify-fleet.sh` composes the ALREADY-TESTED
//! `verify-device.sh` per box (never reinventing its checks) and rolls the per-box verdicts up
//! into ONE fleet-wide report + exit status.
//!
//! The load-bearing contract this file pins:
//!   1. it exists, is executable-looking, sources the single camera-set.sh source of truth (no
//!      re-baked IP map) — same discipline as `deploy-fleet.sh` (`harness_deploy_fleet.rs`);
//!   2. `box_status()` (the pure verdict function) treats an unreachable box as SKIPPED — NEVER a
//!      hard FAIL (an offline box, e.g. cam7 during the 2026-07-06 convergence, could simply be
//!      mid-reboot/deploy) — while a REACHABLE box that fails verify-device.sh's own acceptance
//!      gate IS a fleet FAIL;
//!   3. driven end-to-end under stubs (stubbed `sshpass` controlling reachability + a stubbed
//!      `VERIFY_CMD` standing in for `verify-device.sh`), the real script's exit status and
//!      PASS/FAIL/SKIPPED report reflect that contract — not a re-spelling of it.
//!
//! RED before `scripts/verify-fleet.sh` exists; GREEN after.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/verify-fleet.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------------------------
// Existence + sourcing contract
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_fleet_script_exists_and_is_executable_looking() {
    let p = script();
    assert!(p.exists(), "scripts/verify-fleet.sh must exist (#552)");
    let bytes = fs::read(&p).unwrap();
    assert!(
        bytes.starts_with(b"#!"),
        "verify-fleet.sh must start with a shebang"
    );
}

#[test]
fn verify_fleet_sources_shared_camera_set() {
    let s = read("scripts/verify-fleet.sh");
    assert!(
        s.contains("camera-set.sh"),
        "verify-fleet.sh must source scripts/camera-set.sh (single source of truth for the \
         cam1-7 IP map), not re-bake device IPs."
    );
    for ip in [
        "10.77.9.61",
        "10.77.9.62",
        "10.77.9.63",
        "10.77.9.64",
        "10.77.9.65",
        "10.77.9.66",
        "10.77.9.67",
    ] {
        assert!(
            !s.contains(ip),
            "verify-fleet.sh hard-codes device IP {ip}; resolve it via camera-set.sh instead."
        );
    }
}

#[test]
fn verify_fleet_calls_verify_device_per_box_not_reinventing_its_checks() {
    let s = read("scripts/verify-fleet.sh");
    assert!(
        s.contains("VERIFY_CMD"),
        "verify-fleet.sh must call scripts/verify-device.sh (via an overridable VERIFY_CMD) per \
         box -- it VERIFIES, it must not reimplement verify-device.sh's own acceptance checks."
    );
    assert!(
        s.contains("verify-device.sh"),
        "verify-fleet.sh's default VERIFY_CMD must resolve to verify-device.sh."
    );
}

#[test]
fn verify_fleet_uses_set_euo_pipefail() {
    let s = read("scripts/verify-fleet.sh");
    assert!(
        s.lines().any(|l| l.trim() == "set -euo pipefail"),
        "verify-fleet.sh must use `set -euo pipefail` (script-failure-policy)"
    );
}

// ---------------------------------------------------------------------------------------------
// box_status() pure function -- source the real script (its BASH_SOURCE guard skips the live
// flow) and call it directly, same convention as verify-device.sh / setup-device.sh.
// ---------------------------------------------------------------------------------------------

fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn box_status_unreachable_is_skipped_never_fail() {
    // An offline box must be SKIPPED regardless of what a hypothetical verify_rc would have
    // been -- never run/blamed as a FAIL. This is the cam7-offline-during-convergence case.
    for verify_rc in [0, 1] {
        let (code, out, err) = run_sourced(&format!("box_status 1 {verify_rc}"));
        assert_eq!(code, 0, "box_status must not crash. stderr: {err}");
        assert_eq!(
            out.trim(),
            "SKIPPED",
            "an unreachable box (reachable_rc=1) must be SKIPPED regardless of verify_rc={verify_rc}"
        );
    }
}

#[test]
fn box_status_reachable_and_verify_ok_is_pass() {
    let (code, out, err) = run_sourced("box_status 0 0");
    assert_eq!(code, 0, "box_status must not crash. stderr: {err}");
    assert_eq!(out.trim(), "PASS");
}

#[test]
fn box_status_reachable_and_verify_failed_is_fail() {
    let (code, out, err) = run_sourced("box_status 0 1");
    assert_eq!(code, 0, "box_status must not crash. stderr: {err}");
    assert_eq!(
        out.trim(),
        "FAIL",
        "a REACHABLE box that fails verify-device.sh must be a fleet FAIL, not skipped/passed"
    );
}

// ---------------------------------------------------------------------------------------------
// Full live-flow drive under stubs -- stubbed `sshpass` (controls reachability per IP) + a
// stubbed VERIFY_CMD (stands in for verify-device.sh, returns a controlled exit code per NAME).
// Same technique as harness_deploy_fleet.rs::run_fleet.
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
fn set_exec(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}
#[cfg(not(unix))]
fn set_exec(_p: &std::path::Path) {}

struct RunResult {
    success: bool,
    output: String,
}

/// Drive the REAL verify-fleet.sh against a `CAMERA_SET` of fake cameras, with a stubbed
/// `sshpass` (offline_ips fail the reachability probe; every other IP "succeeds") and a stubbed
/// `VERIFY_CMD` script (fail_cams exit 1, every other cam exits 0).
fn run_fleet(camera_set: &str, offline_ips: &[&str], fail_cams: &[&str]) -> RunResult {
    let tmp = std::env::temp_dir().join(format!(
        "verifyfleet_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // sshpass stub: reachability probe is `sshpass -p <pw> ssh -o ... -o ... -o ... user@ip true`
    // (the real invocation carries THREE `-o` options before `user@ip`, then the trailing remote
    // command `true` — so `user@ip` is NOT simply "the last arg", it's second-to-last, and the
    // exact offset shifts if the real script's -o count ever changes). Scan ALL remaining args for
    // the one containing `@` instead of indexing by position — robust to that shift, and this is
    // exactly the bug a positional `${@: -1}` extraction had (it grabbed the trailing `true`
    // instead of `user@ip`, so the offline branch below NEVER matched and this stub silently
    // reported every box reachable regardless of `offline_ips` — verified by reproducing the real
    // invocation shape against the old extraction by hand before fixing it here).
    // Fail (exit 1) when the target ip is in the offline list; succeed otherwise. The real
    // script's ONLY use of sshpass is this reachability probe (VERIFY_CMD is invoked directly,
    // not through ssh).
    let offline_pattern = offline_ips.join("|");
    let sshpass_body = format!(
        r#"#!/usr/bin/env bash
shift 2  # drop -p <pass>
shift    # drop ssh
target=""
for arg in "$@"; do
  case "$arg" in *@*) target="${{arg#*@}}" ;; esac
done
case "$target" in
  {offline_pattern_case}) exit 1 ;;
  *) exit 0 ;;
esac
"#,
        offline_pattern_case = if offline_pattern.is_empty() {
            "__none__".to_string()
        } else {
            offline_pattern
        }
    );
    let sshpass_path = bin.join("sshpass");
    fs::write(&sshpass_path, sshpass_body).unwrap();
    set_exec(&sshpass_path);

    // VERIFY_CMD stub: `verify-device-stub.sh <name>` exits 1 for a name in fail_cams, else 0.
    let fail_pattern = fail_cams.join("|");
    let verify_body = format!(
        r#"#!/usr/bin/env bash
name="$1"
case "$name" in
  {fail_pattern_case}) echo "[FAIL] simulated"; exit 1 ;;
  *) echo "[OK] simulated ALL CLEAR"; exit 0 ;;
esac
"#,
        fail_pattern_case = if fail_pattern.is_empty() {
            "__none__".to_string()
        } else {
            fail_pattern
        }
    );
    let verify_path = tmp.join("verify-device-stub.sh");
    fs::write(&verify_path, verify_body).unwrap();
    set_exec(&verify_path);

    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new("bash")
        .arg(script())
        .env("PATH", &path_env)
        .env("CAMERA_SET", camera_set)
        .env("VERIFY_CMD", &verify_path)
        .env("CAM_PW", "x")
        .output()
        .expect("failed to run verify-fleet.sh under stubs");

    let _ = fs::remove_dir_all(&tmp);
    RunResult {
        success: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn verify_fleet_exits_zero_when_every_reachable_box_passes() {
    let r = run_fleet("cam1 cam2", &[], &[]);
    assert!(
        r.success,
        "exited nonzero when every reachable box passed verify-device.sh. output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("FLEET CONVERGED"),
        "success but no FLEET CONVERGED line; output:\n{}",
        r.output
    );
}

#[test]
fn verify_fleet_exits_nonzero_when_a_reachable_box_fails() {
    let r = run_fleet("cam1 cam2", &[], &["cam2"]);
    assert!(
        !r.success,
        "exited 0 despite a reachable box failing verify-device.sh (no-false-green broken). \
         output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("FLEET DRIFT"),
        "failed, but no FLEET DRIFT summary line; output:\n{}",
        r.output
    );
}

/// Find the summary line CONTAINING `label` (e.g. "PASS:", "FAIL:", "SKIPPED:") and return the
/// text after it, trimmed. The real lines carry an ANSI color prefix (`info()`'s `\x1b[0;34m[*]
/// \x1b[0m` before the label), so this searches for the label as a substring rather than
/// requiring the line to literally START with it. Panics if the line is missing -- a missing
/// summary line is itself a bug worth failing loudly on, not silently treating as "doesn't
/// contain X".
fn summary_line<'a>(output: &'a str, label: &str) -> &'a str {
    output
        .lines()
        .find_map(|l| l.split_once(label).map(|(_, after)| after))
        .unwrap_or_else(|| panic!("no '{label}' summary line in output:\n{output}"))
        .trim()
}

#[test]
fn verify_fleet_treats_an_offline_box_as_skipped_and_still_exits_zero() {
    // cam7 (10.77.9.67) offline -- must be SKIPPED, and since no OTHER box fails, the fleet
    // exit status must still be 0 (an offline box alone is not a fleet failure, #552).
    let r = run_fleet("cam1 cam7", &["10.77.9.67"], &[]);
    assert!(
        r.success,
        "exited nonzero solely because one box was offline -- an offline box must be SKIPPED, \
         not a fleet FAIL. output:\n{}",
        r.output
    );
    // Precise checks on the actual per-label summary lines (not a loose substring match that a
    // vacuous "SKIPPED: none" could also satisfy) -- cam1 genuinely PASSED, cam7 genuinely
    // SKIPPED, nothing FAILED.
    assert_eq!(
        summary_line(&r.output, "PASS:"),
        "cam1",
        "cam1 (reachable, verify succeeds) must be reported PASS; output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "SKIPPED:"),
        "cam7",
        "cam7 (offline) must be reported SKIPPED, by name, not just the label present with an \
         empty list; output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "FAIL:"),
        "none",
        "cam7 (offline) must NOT appear in the FAIL summary; output:\n{}",
        r.output
    );
}

#[test]
fn verify_fleet_offline_box_does_not_mask_a_real_failure_elsewhere() {
    // Mixed fleet: cam7 offline (SKIPPED) AND cam2 reachable-but-failing (FAIL) -- the offline
    // box must not swallow the real failure; overall exit must still be nonzero.
    let r = run_fleet("cam1 cam2 cam7", &["10.77.9.67"], &["cam2"]);
    assert!(
        !r.success,
        "an offline box masked a real failure elsewhere -- must still exit nonzero. output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "PASS:"),
        "cam1",
        "output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "FAIL:"),
        "cam2",
        "output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "SKIPPED:"),
        "cam7",
        "output:\n{}",
        r.output
    );
    assert!(r.output.contains("FLEET DRIFT"), "output:\n{}", r.output);
}

// --- Hardening from code review (2026-07-06): unknown-arg rejection, case-insensitivity, empty
// CAMERA_SET floor, and explicit env export to VERIFY_CMD ------------------------------------

#[test]
fn verify_fleet_rejects_an_unknown_positional_argument() {
    // verify-device.sh takes a positional NAME; an operator might reasonably try
    // `verify-fleet.sh cam1` expecting the same per-box scoping. It must FAIL LOUD (never
    // silently ignore the arg and check the whole fleet instead).
    let out = Command::new("bash")
        .arg(script())
        .arg("cam1")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .expect("failed to run verify-fleet.sh with a positional arg");
    assert!(
        !out.status.success(),
        "verify-fleet.sh must exit nonzero on an unrecognized positional argument"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("unknown argument") || combined.contains("CAMERA_SET"),
        "error message should explain to use CAMERA_SET instead; got:\n{combined}"
    );
}

#[test]
fn verify_fleet_camera_set_is_case_insensitive() {
    // setup-device.sh / verify-device.sh both advertise (and accept) case-insensitive NAMEs
    // (CAM5 / cam5 both work) -- CAMERA_SET must honor the same convention, not silently reject
    // uppercase entries as "invalid".
    let r = run_fleet("CAM1 Cam2", &[], &[]);
    assert!(
        r.success,
        "uppercase/mixed-case CAMERA_SET entries must resolve like their lowercase form. \
         output:\n{}",
        r.output
    );
    assert!(
        !r.output.contains("invalid"),
        "no camera should be reported invalid purely from case; output:\n{}",
        r.output
    );
    assert_eq!(
        summary_line(&r.output, "PASS:"),
        "CAM1 Cam2",
        "output:\n{}",
        r.output
    );
}

#[test]
fn verify_fleet_fails_loud_on_blank_camera_set() {
    // CAMERA_SET set to whitespace-only bypasses the `:-` default (it's non-empty) and word-splits
    // to zero loop iterations. Without a floor this would print a vacuous "FLEET CONVERGED" for a
    // run that checked NOTHING -- must fail loud instead.
    let r = run_fleet("   ", &[], &[]);
    assert!(
        !r.success,
        "a blank CAMERA_SET must fail loud, not silently report FLEET CONVERGED with zero boxes \
         checked. output:\n{}",
        r.output
    );
    assert!(
        !r.output.contains("FLEET CONVERGED"),
        "must not claim convergence when nothing was actually checked; output:\n{}",
        r.output
    );
}

#[test]
fn verify_fleet_exports_ssh_env_vars_to_verify_cmd() {
    // SSH_USER/CAM_PW/SSH_TIMEOUT must be visible to VERIFY_CMD as a genuine child-process
    // environment variable (not merely "happens to work because the shell already had it"). A
    // stub VERIFY_CMD that checks its OWN environment, rather than trusting an inherited default,
    // proves the explicit `export` actually propagates.
    let tmp = std::env::temp_dir().join(format!(
        "verifyfleet_export_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // sshpass stub: always "reachable" (exit 0) regardless of target.
    let sshpass_path = bin.join("sshpass");
    fs::write(&sshpass_path, "#!/usr/bin/env bash\nexit 0\n").unwrap();
    set_exec(&sshpass_path);

    // VERIFY_CMD stub: FAILS unless it sees the EXACT SSH_USER/CAM_PW/SSH_TIMEOUT this test set,
    // proving they arrived via the environment rather than being silently empty/default.
    let verify_path = tmp.join("verify-device-stub.sh");
    fs::write(
        &verify_path,
        r#"#!/usr/bin/env bash
[ "$SSH_USER" = "customuser" ] || { echo "SSH_USER not propagated: '$SSH_USER'"; exit 1; }
[ "$CAM_PW" = "custompw" ] || { echo "CAM_PW not propagated: '$CAM_PW'"; exit 1; }
[ "$SSH_TIMEOUT" = "42" ] || { echo "SSH_TIMEOUT not propagated: '$SSH_TIMEOUT'"; exit 1; }
exit 0
"#,
    )
    .unwrap();
    set_exec(&verify_path);

    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .env("PATH", &path_env)
        .env("CAMERA_SET", "cam1")
        .env("VERIFY_CMD", &verify_path)
        .env("SSH_USER", "customuser")
        .env("CAM_PW", "custompw")
        .env("SSH_TIMEOUT", "42")
        .output()
        .expect("failed to run verify-fleet.sh under export-test stubs");
    let _ = fs::remove_dir_all(&tmp);

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "SSH_USER/CAM_PW/SSH_TIMEOUT were not all visible to VERIFY_CMD's environment. output:\n{combined}"
    );
}
