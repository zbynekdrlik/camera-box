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

fn driver() -> PathBuf {
    let s = manifest_dir().join("scripts/rt-kernel-upgrade.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Run the DRY-RUN driver `scripts/rt-kernel-upgrade.sh` with `args` (offline `--facts` mode, no
/// ssh). Returns (exit_code, stdout, stderr).
fn run_driver(args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(driver())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run rt-kernel-upgrade.sh");
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

// --- issue 899 planner gap: purge on the OBSERVED superseded-generic set, not the prediction ----
// cam5 (2026-09-03) ran preempt=full (run=1) with a stale 6.8.0-134-generic image + the
// linux-image-generic meta STILL installed, yet GEN read 0 (HWE meta present -> the pre-install
// prediction said "no purge"). The old plan collapsed to `noop:already-lowlatency` and never
// emitted the purge -> the single-kernel invariant silently stayed violated. The 6th `STALE` arg
// is the OBSERVED stale set gather_facts read off the box (comma-joined `<ver>` entries + the
// literal `linux-image-generic` meta; `-`/empty = none).

#[test]
fn plan_purges_observed_superseded_generic_when_already_lowlatency_899() {
    // run=1 inst=1 gen=0 grub=0 cand=1, observed stale = {6.8.0-134-generic, the generic meta}.
    let (code, out, err) =
        run_sourced("rt_kernel_upgrade_plan 1 1 0 0 1 6.8.0-134-generic,linux-image-generic");
    assert_eq!(code, 0, "stderr: {err}");
    let steps: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        steps.contains(&"purge-superseded-generic"),
        "an OBSERVED stale generic on an already-lowlatency box MUST still be purged: {out}"
    );
    assert!(
        steps.contains(&"verify-single-kernel"),
        "and the single-kernel invariant (check (k)) re-verified: {out}"
    );
    assert!(
        !steps.contains(&"install-lowlatency"),
        "already lowlatency => never re-install: {out}"
    );
    assert!(
        !steps.contains(&"noop:already-lowlatency"),
        "stale present => NOT a plain noop: {out}"
    );
}

#[test]
fn plan_stays_noop_when_already_lowlatency_and_no_stale_899() {
    // run=1 with NO observed stale set (both the empty and the `-` sentinel) => unchanged noop.
    for stale in ["", "-"] {
        let (code, out, err) = run_sourced(&format!("rt_kernel_upgrade_plan 1 1 0 0 1 {stale}"));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            "noop:already-lowlatency",
            "already lowlatency + no observed stale generic => noop (stale={stale:?})"
        );
    }
}

#[test]
fn stale_set_does_not_affect_the_pre_install_branch_899() {
    // The observed-stale set ("installed image != uname -r") is ONLY meaningful once the box has
    // rebooted into the new kernel (run=1, uname -r IS the new kernel). BEFORE the install (run=0,
    // uname -r is still the OLD kernel), a "!= uname -r" reading would flag the NEW desired image
    // as stale -> the plan must NOT consult it in the pre-install branch; GEN (the prediction)
    // decides the purge there, exactly as before.
    let with_stale = run_sourced("rt_kernel_upgrade_plan 0 0 1 saved 1 7.0.0-30-generic").1;
    let without = run_sourced("rt_kernel_upgrade_plan 0 0 1 saved 1").1;
    assert_eq!(
        with_stale, without,
        "a stale arg must not change the pre-install (run=0) plan"
    );
    let steps: Vec<&str> = without.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        steps.contains(&"purge-superseded-generic"),
        "run=0 gen=1 still purges on the prediction (unchanged): {without}"
    );
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
    // #899 lane 4: the cam-box appliance image mounts /var/cache (512M) and /tmp (100M) as tmpfs.
    // The lowlatency-hwe install (~242MB archives + a new HWE generic image) overflows /var/cache,
    // and the kernel postinst's in-line initramfs build (~78MB initrd) overflows /tmp -- the
    // supervisor hit both live on cam1/2/3 (issue-899 comment 2026-08-22). The generated command
    // must cache .debs and build the initrd on the ample rootfs so a copy-paste run on cam4 (the one
    // box left to upgrade) does not fail the same way.
    assert!(
        out.contains("Dir::Cache::archives=/root/apt-tmp"),
        "install caches .debs on the rootfs (/var/cache is a 512M tmpfs): {out}"
    );
    assert!(
        out.contains("TMPDIR=/root/tmpbig"),
        "install builds the initrd on the rootfs (/tmp is a 100M tmpfs): {out}"
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

    // #899 lane 4: safe-grub-regen runs update-initramfs + update-grub, both of which build in /tmp
    // (a 100M tmpfs on the appliance) -- must redirect TMPDIR to the ample rootfs so the initrd
    // build does not overflow /tmp (supervisor finding 2026-08-22).
    let (_c, regen, _e) = run_sourced("rt_kernel_step_command safe-grub-regen");
    assert!(
        regen.contains("TMPDIR=/root/tmpbig"),
        "safe-grub-regen builds the initrd on the rootfs (/tmp is a 100M tmpfs): {regen}"
    );
    assert!(
        regen.contains("update-grub"),
        "safe-grub-regen still runs update-grub: {regen}"
    );

    let (_c, bogus, _e) = run_sourced("rt_kernel_step_command not-a-real-token");
    assert_eq!(
        bogus.trim(),
        "unknown-token",
        "unknown token fails loud, never empty"
    );
}

#[test]
fn purge_command_names_the_observed_stale_packages_899() {
    // With the OBSERVED stale set (2nd arg), the purge note names the CONCRETE packages the
    // supervisor purges: image + modules + modules-extra for each stale ver, plus the generic meta.
    let (_c, cmd, _e) = run_sourced(
        "rt_kernel_step_command purge-superseded-generic 6.8.0-134-generic,linux-image-generic",
    );
    for pkg in [
        "linux-image-6.8.0-134-generic",
        "linux-modules-6.8.0-134-generic",
        "linux-modules-extra-6.8.0-134-generic",
        "linux-image-generic",
    ] {
        assert!(cmd.contains(pkg), "purge command must name {pkg}: {cmd}");
    }
    assert!(
        cmd.contains("--allow-change-held-packages"),
        "the held pre-upgrade packages need --allow-change-held-packages: {cmd}"
    );
    assert!(
        cmd.contains("remount,rw") && cmd.contains("remount,ro"),
        "wraps the ro remount: {cmd}"
    );
    assert!(
        cmd.trim_start().starts_with('#'),
        "still a SUPERVISOR note (reboot-class, supervisor applies): {cmd}"
    );
    assert!(
        !cmd.contains("linux-image-*generic"),
        "never a wildcard generic purge — that removes the new running kernel: {cmd}"
    );
    // Back-compat: with NO observed set, the step keeps the generic <OLD_VER> supervisor note.
    let (_c, note, _e) = run_sourced("rt_kernel_step_command purge-superseded-generic");
    assert!(
        note.contains("linux-image-<OLD_VER>"),
        "no-arg keeps the placeholder note: {note}"
    );
    assert!(!note.contains("linux-image-6.8.0-134-generic"), "{note}");
}

// --- driver (scripts/rt-kernel-upgrade.sh) offline --facts wiring ------------------------------

#[test]
fn driver_facts_accepts_legacy_4_and_5_field_facts_899() {
    // Back-compat: a pre-#899 --facts with only 4 or 5 fields (no observed-stale 6th field) still
    // parses; the new field defaults to none, so the run=0 plan is unchanged (GEN still purges).
    for facts in ["0 0 1 saved", "0 0 1 saved 1"] {
        let (code, out, err) = run_driver(&["--facts", facts]);
        assert_eq!(code, 0, "facts={facts:?} stderr: {err}");
        assert!(out.contains("install-lowlatency"), "facts={facts:?}: {out}");
        assert!(
            out.contains("purge-superseded-generic"),
            "facts={facts:?}: {out}"
        );
        assert!(
            out.contains("superseded_installed=-"),
            "legacy facts default the observed-stale field to none: {out}"
        );
    }
}

#[test]
fn driver_facts_carries_the_observed_stale_set_end_to_end_899() {
    // cam5 shape through the driver: run=1 inst=1 gen=0 cand=1 + the observed stale 6th field.
    let (code, out, err) = run_driver(&[
        "--facts",
        "1 1 0 0 1 6.8.0-134-generic,linux-image-generic",
        "--commands",
    ]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("superseded_installed=6.8.0-134-generic,linux-image-generic"),
        "the facts line surfaces the observed stale set: {out}"
    );
    assert!(out.contains("purge-superseded-generic"), "{out}");
    assert!(
        out.contains("linux-image-6.8.0-134-generic"),
        "--commands names the concrete observed stale image: {out}"
    );
    assert!(!out.contains("noop:already-lowlatency"), "{out}");
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
         rt_kernel_upgrade_plan 1 1 0 0 1 6.8.0-134-generic,linux-image-generic >/dev/null; \
         rt_kernel_upgrade_plan 1 1 0 0 1 - >/dev/null; \
         rt_kernel_step_command purge-superseded-generic 6.8.0-134-generic,linux-image-generic >/dev/null; \
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
