//! #454 — pure-function guard for `scripts/verify-device.sh`, the POST-REBOOT runtime acceptance
//! gate for a freshly-provisioned camera-box appliance.
//!
//! Distinct from `tests/setup_device_provisioner_hardening.rs` (which pins `setup-device.sh`
//! STEP 19's INSTALL-TIME, pre-reboot file-presence check): this file exercises
//! `verify-device.sh`'s own pure decision functions, which read REAL post-reboot signals
//! (systemd state, journald, `ls -la`, `avahi-browse`) gathered over SSH by the (untestable-here)
//! live flow. Every pure function is sourced + called directly — same convention as
//! `tests/setup_device_pure_functions.rs` / `tests/clock_offset_guard.rs`.
//!
//! `verify-device.sh` REUSES rather than reinvents:
//!
//! - `scripts/lib/ndi-alive.sh`: `emit_ok_grep_pattern()` / `fatal_grep_pattern()`
//! - `scripts/clock-offset-guard.sh`: `offset_us_from_journal()` / `offset_check()` /
//!   `ptp_locked_from_journal()`
//! - `scripts/camera-set.sh`: `camera_resolve()` (NAME -> IP / `CAMERA_GENLOCK_FPS`)
//!
//! so this file also proves the composition (`dantesync_locked_ok` / `dantesync_offset_ok` /
//! `ndi_emit_ok` / `ndi_journal_has_fatal`) works against real fixture text, not just that the
//! new script's OWN functions are correct in isolation.
//!
//! RED before `scripts/verify-device.sh` exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/verify-device.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the live SSH flow) and run `body`
/// against its pure functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// (a) version format / match
// ---------------------------------------------------------------------------------------------

#[test]
fn version_is_valid_format_accepts_dev_and_release_forms() {
    for v in ["1.7.0-dev.244", "1.7.0", "1.8.16"] {
        let (code, out, err) = run_sourced(&format!(
            r#"if version_is_valid_format "{v}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "harness itself must not crash. stderr: {err}");
        assert_eq!(
            out.trim(),
            "YES",
            "version_is_valid_format('{v}') should accept it"
        );
    }
}

#[test]
fn version_is_valid_format_rejects_garbage() {
    for v in ["", "unknown", "v1.7.0", "1.7-dev.244", "1.7.0-devX.1"] {
        let (code, out, err) = run_sourced(&format!(
            r#"if version_is_valid_format "{v}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "harness itself must not crash. stderr: {err}");
        assert_eq!(
            out.trim(),
            "NO",
            "version_is_valid_format('{v}') should reject it"
        );
    }
}

#[test]
fn version_matches_expected_true_only_on_exact_nonempty_match() {
    let (code, out, err) = run_sourced(
        r#"
        for a in "1.7.0-dev.244:1.7.0-dev.244:YES" "1.7.0-dev.244:1.7.0-dev.243:NO" ":1.7.0:NO" "1.7.0::NO"; do
          actual="${a%%:*}"; rest="${a#*:}"; expected="${rest%%:*}"; want="${rest#*:}"
          if version_matches_expected "$actual" "$expected"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $a" || echo "MISMATCH $a got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "version_matches_expected produced a mismatch: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) systemd service active
// ---------------------------------------------------------------------------------------------

#[test]
fn active_state_is_active_true_only_for_exact_active() {
    let (code, out, err) = run_sourced(
        r#"
        for s in "active" "inactive" "failed" "activating" ""; do
          if active_state_is_active "$s"; then echo "YES:$s"; else echo "NO:$s"; fi
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            "YES:active",
            "NO:inactive",
            "NO:failed",
            "NO:activating",
            "NO:"
        ],
        "active_state_is_active must accept ONLY the exact 'active' state"
    );
}

// ---------------------------------------------------------------------------------------------
// (c) NDI emit + FATAL scan (reuses scripts/lib/ndi-alive.sh)
// ---------------------------------------------------------------------------------------------

const CAMERA_BOX_JOURNAL_HEALTHY: &str = "\
Jul 05 10:00:01 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:01 CAM5 camera-box[812]: capture chroma: u_dev=14.2 v_dev=9.8 -> colour
Jul 05 10:00:06 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:06 CAM5 camera-box[812]: capture chroma: u_dev=13.9 v_dev=10.1 -> colour
";

const CAMERA_BOX_JOURNAL_PANIC: &str = "\
Jul 05 10:00:01 CAM5 camera-box[812]: Streaming: 60.0 fps emitted / 60.0 fps captured (300 sent, 300 captured, 0 capture-dropped)
Jul 05 10:00:07 CAM5 camera-box[812]: thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'
";

#[test]
fn ndi_emit_ok_true_on_genlock_report() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_emit_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn ndi_emit_ok_false_when_no_streaming_line() {
    let (code, out, err) = run_sourced(
        r#"TEXT='Jul 05 10:00:01 CAM5 camera-box[812]: starting up'
           if ndi_emit_ok "$TEXT"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn ndi_journal_has_fatal_detects_panic_and_ignores_healthy_log() {
    let (code, out, err) = run_sourced(&format!(
        "HEALTHY='{}'\nPANIC='{}'\n\
         if ndi_journal_has_fatal \"$HEALTHY\"; then echo HEALTHY_FATAL; else echo HEALTHY_OK; fi\n\
         if ndi_journal_has_fatal \"$PANIC\"; then echo PANIC_FATAL; else echo PANIC_OK; fi",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''"),
        CAMERA_BOX_JOURNAL_PANIC.replace('\'', "'\\''"),
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["HEALTHY_OK", "PANIC_FATAL"]);
}

// ---------------------------------------------------------------------------------------------
// (i) colour capture chroma metric (#299)
// ---------------------------------------------------------------------------------------------

#[test]
fn chroma_state_from_journal_picks_the_last_sample() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nchroma_state_from_journal \"$TEXT\"",
        CAMERA_BOX_JOURNAL_HEALTHY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "capture chroma: u_dev=13.9 v_dev=10.1 -> colour"
    );
}

#[test]
fn chroma_check_distinguishes_colour_grayscale_and_unknown() {
    let (code, out, err) = run_sourced(
        r#"
        rc=0; chroma_check "capture chroma: u_dev=1.0 v_dev=1.0 -> colour" || rc=$?; echo "colour=$rc"
        rc=0; chroma_check "capture chroma: u_dev=0.1 v_dev=0.1 -> grayscale (source likely monochrome)" || rc=$?; echo "gray=$rc"
        rc=0; chroma_check "" || rc=$?; echo "unknown=$rc"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "colour=0\ngray=2\nunknown=3");
}

// ---------------------------------------------------------------------------------------------
// (d) dantesync locked + offset (reuses scripts/clock-offset-guard.sh)
// ---------------------------------------------------------------------------------------------

const DANTESYNC_LOCKED_JOURNAL: &str = "\
Jul 05 10:00:01 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
Jul 05 10:00:02 CAM5 dantesync[900]: [NTP] offset:+300us (threshold:520us, adaptive)
Jul 05 10:00:03 CAM5 dantesync[900]: [PTP] NANO  Drift:   +253ns/s  Adj: +6.81ppm
";

const DANTESYNC_DEGRADED_JOURNAL: &str = "\
Jul 05 10:00:01 CAM5 dantesync[900]: [PTP] NANO  Drift:   -486ns/s  Adj: +6.82ppm
Jul 05 10:00:02 CAM5 dantesync[900]: [NTP] offset:+300us (threshold:520us, adaptive)
";

#[test]
fn dantesync_locked_ok_true_when_servo_is_the_most_recent_event() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif dantesync_locked_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        DANTESYNC_LOCKED_JOURNAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn dantesync_locked_ok_false_when_ntp_line_is_the_most_recent_event() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif dantesync_locked_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        DANTESYNC_DEGRADED_JOURNAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn dantesync_offset_ok_true_within_bound_false_outside() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\n\
         if dantesync_offset_ok \"$TEXT\" 2000; then echo WITHIN; else echo OUTSIDE; fi\n\
         if dantesync_offset_ok \"$TEXT\" 100; then echo WITHIN; else echo OUTSIDE; fi",
        DANTESYNC_LOCKED_JOURNAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "WITHIN\nOUTSIDE");
}

// ---------------------------------------------------------------------------------------------
// (e) genlock.conf drop-in FPS
// ---------------------------------------------------------------------------------------------

#[test]
fn genlock_dropin_fps_parses_the_value() {
    let (code, out, err) = run_sourced(
        r#"TEXT='[Service]
Environment=CAMERA_BOX_GENLOCK_FPS=60'
           genlock_dropin_fps "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "60");
}

#[test]
fn genlock_dropin_fps_empty_when_missing() {
    let (code, out, err) = run_sourced(r#"genlock_dropin_fps """#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "");
}

#[test]
fn genlock_fps_matches_true_only_on_exact_match() {
    let (code, out, err) = run_sourced(
        r#"
        if genlock_fps_matches "60" "60"; then echo YES; else echo NO; fi
        if genlock_fps_matches "30" "60"; then echo YES; else echo NO; fi
        if genlock_fps_matches "" "60"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES\nNO\nNO");
}

// ---------------------------------------------------------------------------------------------
// (f) cpu-affinity.conf drop-in
// ---------------------------------------------------------------------------------------------

#[test]
fn cpu_affinity_dropin_value_parses_the_value() {
    let (code, out, err) = run_sourced(
        r#"TEXT='[Service]
# #289: pin grab to the isolated core (isolcpus=3) so box load never starves capture/emit
CPUAffinity=3'
           cpu_affinity_dropin_value "$TEXT""#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "3");
}

// ---------------------------------------------------------------------------------------------
// (g) libndi root-owned symlink chain
// ---------------------------------------------------------------------------------------------

const NDI_LS_CANONICAL: &str = "\
total 556
drwxr-xr-x 2 root root   4096 Jul  5 10:00 .
drwxr-xr-x 3 root root   4096 Jul  5 10:00 ..
lrwxrwxrwx 1 root root     12 Jul  5 10:00 libndi.so -> libndi.so.6
lrwxrwxrwx 1 root root     20 Jul  5 10:00 libndi.so.6 -> libndi.so.6.3.2.0
-rwxr-xr-x 1 root root 545280 Jul  5 10:00 libndi.so.6.3.2.0
";

// The #445 cam3-outlier layout: real files, user-owned (its manual NDI upgrade never fit the
// fleet script) -- verify-device.sh certifies the CANONICAL build, so this must FAIL.
const NDI_LS_CAM3_OUTLIER: &str = "\
total 556
drwxr-xr-x 2 newlevel newlevel   4096 Jul  5 10:00 .
drwxr-xr-x 3 newlevel newlevel   4096 Jul  5 10:00 ..
-rwxr-xr-x 1 newlevel newlevel     12 Jul  5 10:00 libndi.so
-rwxr-xr-x 1 newlevel newlevel 545280 Jul  5 10:00 libndi.so.6
";

const NDI_LS_NON_ROOT_SYMLINK: &str = "\
total 556
drwxr-xr-x 2 root root   4096 Jul  5 10:00 .
drwxr-xr-x 3 root root   4096 Jul  5 10:00 ..
lrwxrwxrwx 1 newlevel newlevel 20 Jul  5 10:00 libndi.so.6 -> libndi.so.6.3.2.0
-rwxr-xr-x 1 root root 545280 Jul  5 10:00 libndi.so.6.3.2.0
";

#[test]
fn ndi_symlink_chain_ok_true_on_canonical_layout() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_CANONICAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn ndi_symlink_chain_ok_false_on_cam3_outlier_real_file_layout() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_CAM3_OUTLIER.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "NO",
        "the #445 cam3-outlier real-file layout must FAIL the canonical-build gate"
    );
}

#[test]
fn ndi_symlink_chain_ok_false_when_symlink_is_not_root_owned() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif ndi_symlink_chain_ok \"$TEXT\"; then echo YES; else echo NO; fi",
        NDI_LS_NON_ROOT_SYMLINK.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// ---------------------------------------------------------------------------------------------
// (h) avahi mDNS NDI discovery
// ---------------------------------------------------------------------------------------------

const AVAHI_BROWSE_WITH_CAM5: &str = "\
+;eth0;IPv4;CAM1 (usb);_ndi._tcp;local
+;eth0;IPv4;CAM5 (usb);_ndi._tcp;local
";

#[test]
fn avahi_ndi_discoverable_true_when_source_present() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif avahi_ndi_discoverable \"$TEXT\" \"CAM5\"; then echo YES; else echo NO; fi",
        AVAHI_BROWSE_WITH_CAM5.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES");
}

#[test]
fn avahi_ndi_discoverable_false_when_source_absent() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nif avahi_ndi_discoverable \"$TEXT\" \"CAM7\"; then echo YES; else echo NO; fi",
        AVAHI_BROWSE_WITH_CAM5.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn avahi_ndi_discoverable_false_on_empty_browse_output() {
    let (code, out, err) =
        run_sourced(r#"if avahi_ndi_discoverable "" "CAM5"; then echo YES; else echo NO; fi"#);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

// ---------------------------------------------------------------------------------------------
// (j)-(o) fleet-uniformity invariants (#547) — every cambox identical: ro-root, ONE kernel, no
// fwupd, wait-online masked, #289/#303 core-isolation cmdline, pinned NDI runtime.
// ---------------------------------------------------------------------------------------------

#[test]
fn root_mount_is_readonly_true_only_when_first_option_is_ro() {
    // A rw mount that carries "errors=remount-ro" in its options must NOT read as read-only —
    // the kernel always emits ro/rw as the FIRST comma-token, so only that decides.
    let (code, out, err) = run_sourced(
        r#"
        for o in "ro,relatime:YES" "rw,relatime:NO" "rw,errors=remount-ro:NO" "ro:YES" ":NO"; do
          opts="${o%%:*}"; want="${o##*:}"
          if root_mount_is_readonly "$opts"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $o" || echo "MISMATCH $o got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "root_mount_is_readonly mismatch: {out}"
    );
}

#[test]
fn kernels_uniform_ok_true_only_for_single_installed_matching_running() {
    let (code, out, err) = run_sourced(
        r#"
        ONE='/boot/vmlinuz-6.8.0-134-generic'
        TWO='/boot/vmlinuz-6.8.0-134-generic
/boot/vmlinuz-6.8.0-90-generic'
        if kernels_uniform_ok "$ONE" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "$TWO" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "$ONE" "6.8.0-90-generic"; then echo YES; else echo NO; fi
        if kernels_uniform_ok "" "6.8.0-134-generic"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "YES\nNO\nNO\nNO",
        "single-installed-kernel==running only; two kernels (cam4 drift) or a mismatch must FAIL"
    );
}

#[test]
fn fwupd_absent_true_only_when_purged() {
    // The fleet PURGES fwupd (it held a write handle blocking the ro remount). A unit still
    // present in ANY state — including masked — is not identical to a purged box, so FAILs.
    let (code, out, err) = run_sourced(
        r#"
        for s in "not-found:YES" ":YES" "static:NO" "enabled:NO" "masked:NO" "disabled:NO"; do
          st="${s%%:*}"; want="${s##*:}"
          if fwupd_absent "$st"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $s" || echo "MISMATCH $s got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(!out.contains("MISMATCH"), "fwupd_absent mismatch: {out}");
}

#[test]
fn waitonline_masked_true_only_when_masked() {
    let (code, out, err) = run_sourced(
        r#"
        for s in "masked:YES" "enabled:NO" "disabled:NO" ":NO" "not-found:NO"; do
          st="${s%%:*}"; want="${s##*:}"
          if waitonline_masked "$st"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $s" || echo "MISMATCH $s got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "waitonline_masked mismatch: {out}"
    );
}

const CMDLINE_FULL: &str = "BOOT_IMAGE=/boot/vmlinuz-6.8.0-134-generic root=UUID=abc ro quiet isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2";
const CMDLINE_PARTIAL: &str =
    "BOOT_IMAGE=/boot/vmlinuz-6.8.0-134-generic root=UUID=abc ro quiet isolcpus=3 nohz_full=3";

#[test]
fn cmdline_has_isolation_requires_all_four_flags() {
    let (code, out, err) = run_sourced(&format!(
        "FULL='{}'\nPARTIAL='{}'\n\
         if cmdline_has_isolation \"$FULL\"; then echo FULL_YES; else echo FULL_NO; fi\n\
         if cmdline_has_isolation \"$PARTIAL\"; then echo PARTIAL_YES; else echo PARTIAL_NO; fi\n\
         if cmdline_has_isolation \"\"; then echo EMPTY_YES; else echo EMPTY_NO; fi",
        CMDLINE_FULL, CMDLINE_PARTIAL,
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "FULL_YES\nPARTIAL_NO\nEMPTY_NO");
}

#[test]
fn cmdline_has_isolation_matches_whole_tokens_not_prefixes() {
    // nohz_full=3 must NOT be satisfied by nohz_full=30 (whole-token match).
    let (code, out, err) = run_sourced(
        r#"BOGUS='ro isolcpus=3 nohz_full=30 rcu_nocbs=3 irqaffinity=0-2'
           if cmdline_has_isolation "$BOGUS"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO");
}

#[test]
fn ndi_symlink_version_extracts_from_canonical_target() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nndi_symlink_version \"$TEXT\"",
        NDI_LS_CANONICAL.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "6.3.2.0");
}

#[test]
fn ndi_version_matches_accepts_pin_prefix_only() {
    // Pin "6.3.2" accepts the 3-part soname "6.3.2" and the 4-part SDK string "6.3.2.0", but
    // rejects "6.2.1" and the deceptive "6.3.20".
    let (code, out, err) = run_sourced(
        r#"
        if ndi_version_matches "6.3.2.0" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.3.2" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.2.1" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "6.3.20" "6.3.2"; then echo YES; else echo NO; fi
        if ndi_version_matches "" "6.3.2"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES\nYES\nNO\nNO\nNO");
}
