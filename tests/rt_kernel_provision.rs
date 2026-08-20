//! issue 899 (lane 2) — pure-function guard for `scripts/lib/rt-kernel-plan.sh`, the PREEMPT_RT
//! kernel provisioning DECISION logic (defect 1: the fleet runs a stock PREEMPT_DYNAMIC kernel,
//! not PREEMPT_RT). Lane 1 (merged: `src/affinity.rs` capture-IRQ routing) already fixed defect 3.
//!
//! Same convention as `tests/verify_device_pure_functions.rs` / `tests/clock_offset_guard.rs`:
//! every pure function is sourced + called directly. The library is side-effect-free (no `$0`
//! guard needed — sourcing it defines functions and prints nothing). The reboot-class APPLY is the
//! supervisor's step (`scripts/rt-kernel-upgrade.sh` only PLANS, read-only), so it is deliberately
//! NOT exercised here — only the pure planner is.
//!
//! RED before `scripts/lib/rt-kernel-plan.sh` exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/rt-kernel-plan.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the pure library and run `body`. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// --- rt_kernel_flavour -------------------------------------------------------------------------

#[test]
fn flavour_is_the_single_decided_realtime_package() {
    let (code, out, err) = run_sourced("rt_kernel_flavour");
    assert_eq!(code, 0, "harness must not crash. stderr: {err}");
    assert_eq!(out.trim(), "linux-image-realtime");
}

// --- rt_kernel_readiness_verdict ---------------------------------------------------------------

#[test]
fn readiness_covers_all_four_states() {
    for (args, want) in [
        ("1 1 1", "already-realtime"), // running RT already
        ("1 0 0", "already-realtime"), // running RT dominates every other input
        ("0 1 1", "ready"),            // candidate + pro attached
        ("0 1 0", "needs-pro-attach"), // candidate but pro not attached (today's fleet)
        ("0 0 0", "no-rt-candidate"),  // no realtime package resolvable
        ("0 0 1", "no-rt-candidate"),  // candidate absence dominates pro state
    ] {
        let (code, out, err) = run_sourced(&format!("rt_kernel_readiness_verdict {args}"));
        assert_eq!(code, 0, "harness must not crash. stderr: {err}");
        assert_eq!(out.trim(), want, "readiness_verdict({args})");
    }
}

// --- rt_kernel_upgrade_plan --------------------------------------------------------------------

#[test]
fn plan_is_noop_when_already_realtime() {
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 1 0 0 0 0");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "noop:already-realtime");
}

#[test]
fn plan_is_blocked_when_no_pro_and_not_installed() {
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 0 0 1 saved");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "blocked:need-pro-attach");
}

#[test]
fn plan_full_sequence_reboots_into_rt_before_purging_generic() {
    // cam2 shape: non-rt, not installed, pro attached, generic present, GRUB_DEFAULT=saved.
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 0 1 1 saved");
    assert_eq!(code, 0, "stderr: {err}");
    let steps: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        steps,
        vec![
            "install-rt-kernel",
            "verify-rt-initrd",
            "grub-pin:saved",
            "update-grub",
            "reboot-into-rt",
            "confirm-running-realtime",
            "purge-generic",
            "verify-single-kernel",
            "post-verify",
        ],
        "the SAFE atomic order: reboot INTO rt, confirm, THEN purge the now-unused generic"
    );
    // The purge must come strictly AFTER the reboot+confirm (never removes the running kernel).
    let ir = steps.iter().position(|s| *s == "reboot-into-rt").unwrap();
    let ic = steps
        .iter()
        .position(|s| *s == "confirm-running-realtime")
        .unwrap();
    let ip = steps.iter().position(|s| *s == "purge-generic").unwrap();
    assert!(ir < ic && ic < ip, "purge must follow reboot+confirm");
}

#[test]
fn plan_skips_install_when_already_installed_and_skips_purge_when_no_generic() {
    // cam1 shape: non-rt, RT already installed, pro attached, NO generic meta, GRUB_DEFAULT=0.
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 1 1 0 0");
    assert_eq!(code, 0, "stderr: {err}");
    let steps: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !steps.contains(&"install-rt-kernel"),
        "already installed => no install step"
    );
    assert!(
        !steps.contains(&"purge-generic"),
        "no generic meta => no purge step"
    );
    assert!(
        steps.contains(&"grub-pin:menuentry"),
        "numeric GRUB_DEFAULT => menuentry pin"
    );
    assert!(!steps.contains(&"grub-pin:saved"));
}

// --- rt_kernel_step_command --------------------------------------------------------------------

#[test]
fn step_command_maps_known_tokens_and_flags_unknown() {
    let (code, out, err) = run_sourced("rt_kernel_step_command install-rt-kernel");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("apt-get install -y linux-image-realtime"),
        "got: {out}"
    );
    assert!(
        out.contains("remount,rw") && out.contains("remount,ro"),
        "wraps the ro remount"
    );

    let (_c, purge, _e) = run_sourced("rt_kernel_step_command purge-generic");
    assert!(purge.contains("apt-get purge"), "got: {purge}");

    let (_c, reboot, _e) = run_sourced("rt_kernel_step_command reboot-into-rt");
    assert!(
        reboot.trim_start().starts_with('#'),
        "reboot is a SUPERVISOR note, not a command"
    );

    let (_c, bogus, _e) = run_sourced("rt_kernel_step_command not-a-real-token");
    assert_eq!(
        bogus.trim(),
        "unknown-token",
        "unknown token fails loud, never empty"
    );
}

// --- sourcing has no side effects --------------------------------------------------------------

#[test]
fn sourcing_the_library_prints_nothing() {
    let (code, out, err) = run_sourced("true");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out, "",
        "a pure sourced library must emit nothing on source"
    );
}
