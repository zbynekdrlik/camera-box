//! issue 899 (lane 3) — pure-function guard for `scripts/lib/rt-kernel-plan.sh`, the low-latency
//! kernel provisioning DECISION logic (defect 1: the fleet runs a stock PREEMPT_DYNAMIC kernel with
//! no full preemption). Lane 1 (merged: `src/affinity.rs` capture-IRQ routing) already fixed
//! defect 3.
//!
//! OWNER DECISION (2026-08-20): Ubuntu Pro is REJECTED. STEP 1 is the FREE official-archive
//! `linux-lowlatency-hwe-24.04` (a config meta dropping preempt=full via `lowlatency-kernel` — the
//! imag-nb precedent). This file was reworked from the pro-attach PREEMPT_RT design: the Ubuntu Pro
//! axis is gone, the flavour is the lowlatency meta, and `blocked:no-rt-candidate` is KEPT as the
//! fail-closed shape for a genuinely-missing package.
//!
//! Same convention as `tests/verify_device_pure_functions.rs` / `tests/clock_offset_guard.rs`:
//! every pure function is sourced + called directly. The library is side-effect-free (no `$0`
//! guard needed — sourcing it defines functions and prints nothing). The reboot-class APPLY is the
//! supervisor's step (`scripts/rt-kernel-upgrade.sh` only PLANS, read-only), so it is deliberately
//! NOT exercised here — only the pure planner is.
//!
//! RED before `scripts/lib/rt-kernel-plan.sh` carries the lowlatency logic (the flavour/tokens
//! differ, tests fail); GREEN after.

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
fn flavour_is_the_single_decided_lowlatency_package() {
    let (code, out, err) = run_sourced("rt_kernel_flavour");
    assert_eq!(code, 0, "harness must not crash. stderr: {err}");
    // The FREE Ubuntu main-archive low-latency meta (no Pro) — NOT linux-image-realtime.
    assert_eq!(out.trim(), "linux-lowlatency-hwe-24.04");
}

// --- rt_kernel_readiness_verdict ---------------------------------------------------------------

#[test]
fn readiness_covers_all_three_states() {
    // Args: RUNNING_LOWLAT LOWLAT_INSTALLED CANDIDATE_PRESENT. The INSTALLED axis keeps the verdict
    // consistent with the plan's own blocked condition (`!inst && !cand`), so an installed box whose
    // candidate has aged out reads `ready`, never a spurious `no-rt-candidate` while the plan proceeds.
    for (args, want) in [
        ("1 0 0", "already-lowlatency"), // running preempt=full already dominates
        ("1 1 1", "already-lowlatency"), // running dominates every other axis
        ("0 0 1", "ready"),              // candidate present, not installed (no Pro needed)
        ("0 1 0", "ready"), // installed but candidate aged out -> still ready, no false block
        ("0 1 1", "ready"), // installed AND candidate
        ("0 0 0", "no-rt-candidate"), // neither installed nor a candidate (fail-closed)
    ] {
        let (code, out, err) = run_sourced(&format!("rt_kernel_readiness_verdict {args}"));
        assert_eq!(code, 0, "harness must not crash. stderr: {err}");
        assert_eq!(out.trim(), want, "readiness_verdict({args})");
    }
}

// --- rt_kernel_upgrade_plan --------------------------------------------------------------------

#[test]
fn plan_is_noop_when_already_lowlatency() {
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 1 0 0 0 1");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "noop:already-lowlatency");
}

#[test]
fn plan_is_blocked_when_no_candidate_and_not_installed() {
    // The fail-closed shape kept from the pro-attach design: package not resolvable + not installed.
    // (5th axis cand=0.) The plan must agree with the readiness verdict and block, never print a
    // full install sequence that would apt-get install a package with no candidate.
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 0 1 saved 0");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "blocked:no-rt-candidate");
}

#[test]
fn plan_full_sequence_reboots_before_purging_superseded_generic() {
    // cam2 shape: not running preempt=full, not installed, superseded-generic will remain (HWE meta
    // absent), GRUB_DEFAULT=saved, candidate present.
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 0 1 saved 1");
    assert_eq!(code, 0, "stderr: {err}");
    let steps: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        steps,
        vec![
            "install-lowlatency",
            "verify-lowlatency-config",
            "grub-pin:saved",
            "safe-grub-regen",
            "reboot-into-lowlatency",
            "confirm-running-lowlatency",
            "purge-superseded-generic",
            "verify-single-kernel",
            "post-verify",
        ],
        "the SAFE atomic order: reboot INTO lowlatency, confirm, THEN purge the now-superseded generic"
    );
    // The purge must come strictly AFTER the reboot+confirm (never removes the running kernel).
    let ir = steps
        .iter()
        .position(|s| *s == "reboot-into-lowlatency")
        .unwrap();
    let ic = steps
        .iter()
        .position(|s| *s == "confirm-running-lowlatency")
        .unwrap();
    let ip = steps
        .iter()
        .position(|s| *s == "purge-superseded-generic")
        .unwrap();
    assert!(ir < ic && ic < ip, "purge must follow reboot+confirm");
}

#[test]
fn plan_skips_install_when_installed_and_skips_purge_when_no_superseded() {
    // imag-like shape: not running preempt=full yet, lowlatency config ALREADY installed, NO
    // superseded generic (the HWE meta is present → config-only install, no new image),
    // GRUB_DEFAULT=0. Candidate axis irrelevant once installed (pass 0 to prove it does not block).
    let (code, out, err) = run_sourced("rt_kernel_upgrade_plan 0 1 0 0 0");
    assert_eq!(code, 0, "stderr: {err}");
    let steps: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !steps.contains(&"install-lowlatency"),
        "already installed => no install step"
    );
    assert!(
        !steps.contains(&"purge-superseded-generic"),
        "no superseded generic => no purge step"
    );
    assert!(
        steps.contains(&"grub-pin:menuentry"),
        "numeric GRUB_DEFAULT => menuentry pin"
    );
    assert!(!steps.contains(&"grub-pin:saved"));
    // The config guard is ALWAYS present, even when install is skipped (verify the drop landed).
    assert!(steps.contains(&"verify-lowlatency-config"));
}

// --- rt_kernel_step_command --------------------------------------------------------------------

#[test]
fn step_command_maps_known_tokens_and_flags_unknown() {
    let (code, out, err) = run_sourced("rt_kernel_step_command install-lowlatency");
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("apt-get install")
            && out.contains("linux-lowlatency-hwe-24.04")
            && out.contains("--allow-change-held-packages"),
        "install command installs the lowlatency meta with --allow-change-held-packages: {out}"
    );
    assert!(
        out.contains("remount,rw") && out.contains("remount,ro"),
        "wraps the ro remount"
    );

    // The purge MUST be a supervisor note that never blanket-purges generic (that would remove the
    // new running kernel), and must call out the --allow-change-held-packages requirement.
    let (_c, purge, _e) = run_sourced("rt_kernel_step_command purge-superseded-generic");
    assert!(
        purge.trim_start().starts_with('#'),
        "purge is a SUPERVISOR note (per-box exact version), not a blind command: {purge}"
    );
    assert!(
        !purge.contains("linux-image-*generic"),
        "must never blanket-purge generic — that removes the new running kernel: {purge}"
    );
    assert!(
        purge.contains("--allow-change-held-packages"),
        "the held pre-upgrade image needs --allow-change-held-packages: {purge}"
    );

    let (_c, reboot, _e) = run_sourced("rt_kernel_step_command reboot-into-lowlatency");
    assert!(
        reboot.trim_start().starts_with('#'),
        "reboot is a SUPERVISOR note, not a command"
    );

    // The confirm step must check preempt=full, NOT a *-lowlatency uname (the config meta keeps the
    // generic image), matching the imag-nb reality.
    let (_c, confirm, _e) = run_sourced("rt_kernel_step_command confirm-running-lowlatency");
    assert!(
        confirm.contains("preempt=full"),
        "confirm checks preempt=full is active: {confirm}"
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

/// The load-bearing property: the driver sources this lib under `set -euo pipefail`, so no
/// function may abort the caller (unbound var / a falsy `_rt_truthy` reaching a non-condition
/// context / a pipefail). Turn on `-e` and drive every function down its truthy AND falsy
/// branches; if any aborts, the trailing `ALIVE` never prints.
#[test]
fn functions_never_abort_a_set_e_caller() {
    let (code, out, err) = run_sourced(
        "set -euo pipefail; \
         rt_kernel_flavour >/dev/null; \
         rt_kernel_readiness_verdict 1 1 1 >/dev/null; \
         rt_kernel_readiness_verdict 0 0 0 >/dev/null; \
         rt_kernel_upgrade_plan 1 0 0 0 1 >/dev/null; \
         rt_kernel_upgrade_plan 0 0 1 saved 0 >/dev/null; \
         rt_kernel_upgrade_plan 0 0 1 saved 1 >/dev/null; \
         rt_kernel_step_command purge-superseded-generic >/dev/null; \
         rt_kernel_step_command not-a-real-token >/dev/null; \
         echo ALIVE",
    );
    assert_eq!(
        code, 0,
        "a function aborted the set -e caller. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "ALIVE",
        "the -e caller must survive every function call"
    );
}
