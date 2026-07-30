//! #821 — pure-function guard for `scripts/verify-imag.sh`, the POST-PROVISION runtime acceptance
//! gate for the imag notebook (the imag twin of `scripts/verify-device.sh`, #454).
//!
//! Every pure function is sourced + called directly — same convention as
//! `tests/verify_device_pure_functions.rs` / `tests/clock_offset_guard.rs`. `verify-imag.sh`
//! REUSES rather than reinvents:
//!
//! - `scripts/setup-imag.sh`: `imag_cpu_isolation_plan()` / `imag_has_discrete_nvidia()` (#816)
//! - `scripts/verify-device.sh`: `ndi_symlink_target()` / `ndi_symlink_chain_ok()` /
//!   `ndi_symlink_version()` / `ndi_version_matches()` (#454/#132/#547)
//! - `scripts/lib/timesync-authority.sh`: `dpkg_status_installed()` (#591/#596)
//! - `scripts/clock-offset-guard.sh`: `ptp_locked_from_pipe_json()`, `offset_check()`, and the
//!   NEW (#834) `gm_source_ip_from_pipe_json()` / `gm_matches_expected()` / `gm_check()`
//!
//! so this file also proves the COMPOSITION (sourcing all of the above from verify-imag.sh)
//! actually works, not just that verify-imag.sh's OWN new functions are correct in isolation.
//!
//! RED before `scripts/verify-imag.sh` exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/verify-imag.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the live SSH/WS flow) and run
/// `body` against its pure functions (and, transitively, everything it sources). Returns
/// (exit_code, stdout, stderr).
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
// script exists, is a bash script, sources everything it claims to reuse, has a source-guard
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_imag_script_exists_and_is_a_bash_script() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.starts_with("#!/usr/bin/env bash") || body.starts_with("#!/bin/bash"),
        "scripts/verify-imag.sh must start with a bash shebang"
    );
    assert!(
        body.lines().any(|l| l.trim() == "set -euo pipefail"),
        "scripts/verify-imag.sh must use `set -euo pipefail` (script-failure-policy)"
    );
    assert!(
        body.contains(r#"[ "${BASH_SOURCE[0]}" != "${0}" ]"#),
        "scripts/verify-imag.sh must guard its live flow behind a BASH_SOURCE != $0 check so the \
         pure functions can be sourced + unit-tested offline"
    );
}

#[test]
fn verify_imag_reuses_setup_imag_and_verify_device_and_shared_libs() {
    let body = std::fs::read_to_string(script()).unwrap();
    for needle in [
        "lib/cli-log.sh",
        "lib/timesync-authority.sh",
        "clock-offset-guard.sh",
        "setup-imag.sh",
        "verify-device.sh",
        "imag-host.sh",
    ] {
        let sourced = body
            .lines()
            .any(|l| l.trim_start().starts_with('.') && l.contains(needle));
        assert!(
            sourced,
            "scripts/verify-imag.sh must source {needle} (reuse mandate) -- expected a \
             `. \"$HERE/...\"` line"
        );
    }
}

#[test]
fn verify_imag_reuses_imag_cpu_isolation_plan_not_a_literal() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("imag_cpu_isolation_plan"),
        "verify-imag.sh must call the shared imag_cpu_isolation_plan() (#816) to derive the \
         expected isolcpus/nohz_full/irqaffinity plan from THIS box's own topology -- never a \
         hardcoded cam-fleet-style literal"
    );
}

// ---------------------------------------------------------------------------------------------
// (a) hostname + static IP
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_hostname_matches_true_only_on_exact_nonempty_match() {
    let (code, out, err) = run_sourced(
        r#"
        for a in "imag-nb:imag-nb:YES" "imag-nb:imag-old:NO" ":imag-nb:NO" "imag-nb::NO"; do
          actual="${a%%:*}"; rest="${a#*:}"; expected="${rest%%:*}"; want="${rest#*:}"
          if imag_hostname_matches "$actual" "$expected"; then got=YES; else got=NO; fi
          [ "$got" = "$want" ] && echo "OK $a" || echo "MISMATCH $a got=$got"
        done
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("MISMATCH"),
        "imag_hostname_matches mismatch: {out}"
    );
}

#[test]
fn imag_static_ip_present_matches_whole_token_not_substring() {
    let (code, out, err) = run_sourced(
        r#"
        TEXT="10.77.9.187
2: eno1    inet 10.77.9.187/23 brd 10.77.9.255 scope global eno1"
        if imag_static_ip_present "$TEXT" "10.77.9.187"; then echo YES; else echo NO; fi
        if imag_static_ip_present "$TEXT" "10.77.9.18"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO"],
        "must whole-token match, never a substring: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) ssh.service (not ssh.socket)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_sshd_via_service_requires_service_enabled_and_socket_not() {
    let cases = [
        ("enabled", "disabled", "YES"),
        ("enabled", "not-found", "YES"),
        ("enabled", "enabled", "NO"), // noble's socket-activation default -- must be rejected
        ("disabled", "disabled", "NO"),
        ("", "", "NO"),
    ];
    for (svc, sock, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_sshd_via_service "{svc}" "{sock}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_sshd_via_service(svc={svc:?}, sock={sock:?}) expected {want}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// (c) kernel on the HWE line (#819) -- reuses dpkg_status_installed
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_hwe_kernel_installed_reuses_dpkg_status_installed() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_hwe_kernel_installed "install ok installed"; then echo YES; else echo NO; fi
        if imag_hwe_kernel_installed "purge ok not-installed"; then echo YES; else echo NO; fi
        if imag_hwe_kernel_installed ""; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO"],
        "must reuse dpkg_status_installed's own contract: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (d) kernel cmdline: preempt=full + NO kernel isolcpus/nohz_full isolation (#289/#482/#784/#842)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_cmdline_has_preempt_full_whole_token() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_cmdline_has_preempt_full "BOOT_IMAGE=/vmlinuz quiet preempt=full splash"; then echo YES; else echo NO; fi
        if imag_cmdline_has_preempt_full "BOOT_IMAGE=/vmlinuz quiet preempt=voluntary splash"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["YES", "NO"], "{out:?}");
}

/// #842 (recurrence of #784): `isolcpus=`/`nohz_full=` on the kernel cmdline disables scheduler
/// load balancing for the CPUs listed -- measured live to pile 114 of OBS's 119 threads onto ONE
/// core while sibling cores in the SAME affinity mask sat at 0% busy (60fps -> ~53fps NDI
/// receive). `scripts/setup-imag.sh` must stop writing them; this is the acceptance-gate guard
/// that fails LOUD if a box (old or newly provisioned) still carries either token -- #784's own
/// outstanding item, deferred between #780/#791 since 2026-07-15 and the direct cause of the
/// #842 recurrence on the replacement notebook.
#[test]
fn imag_cmdline_free_of_kernel_isolation_fails_on_isolcpus_or_nohz_full() {
    let cases = [
        ("BOOT_IMAGE=/vmlinuz quiet preempt=full rcu_nocbs=all", "YES"), // .182's real clean cmdline
        (
            "BOOT_IMAGE=/vmlinuz quiet isolcpus=2,3,4,5,6,7 nohz_full=6,7 irqaffinity=0,1,8,9,10,11 preempt=full",
            "NO",
        ), // .187's real #842 defect cmdline
        ("BOOT_IMAGE=/vmlinuz quiet isolcpus=2-7 preempt=full", "NO"), // isolcpus alone must still fail
        ("BOOT_IMAGE=/vmlinuz quiet nohz_full=6,7 preempt=full", "NO"), // nohz_full alone must still fail
    ];
    for (cmdline, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_cmdline_free_of_kernel_isolation "{cmdline}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_cmdline_free_of_kernel_isolation({cmdline:?}) expected {want}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// OBS thread distribution: the #842 DIRECT SYMPTOM check -- no single CPU core may hold a
// majority of OBS's live threads (a future variant of the isolcpus-class bug must not pass
// silently just because it doesn't happen to write a kernel-cmdline token).
// ---------------------------------------------------------------------------------------------

/// `imag_obs_thread_concentration_ok` takes the raw per-thread processor numbers (`ps -L -o psr=
/// -C obs` output, one CPU number per OBS thread, one per line) and fails when a single core
/// holds more than ~60% of the live thread count -- the #842 signature was 114/119 (96%) on one
/// core. Live-verified reference numbers used directly: 114 on cpu5, 19/16/24/26/12/17 spread
/// after the fix (comment 5105280323 on #842).
#[test]
fn imag_obs_thread_concentration_ok_flags_a_single_core_pileup() {
    // The FIXED distribution (post-#842, real numbers): 19+16+24+26+12+17 = 114 threads spread
    // across 6 cores, max 26 -> 26/114 = 22.8%, well under the 60% bound.
    let spread: String = std::iter::repeat_n("2\n", 19)
        .chain(std::iter::repeat_n("3\n", 16))
        .chain(std::iter::repeat_n("4\n", 24))
        .chain(std::iter::repeat_n("5\n", 26))
        .chain(std::iter::repeat_n("6\n", 12))
        .chain(std::iter::repeat_n("7\n", 17))
        .collect();
    let (code, out, err) = run_sourced(&format!(
        r#"LIST="{spread}"
if imag_obs_thread_concentration_ok "$LIST"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES", "spread distribution must pass: {out:?}");

    // The #842 DEFECT distribution (real numbers): 114 threads on cpu5, 5 threads elsewhere (119
    // total) -- 114/119 = 95.8%, far past the 60% bound.
    let pileup: String = std::iter::repeat_n("5\n", 114)
        .chain(std::iter::repeat_n("7\n", 5))
        .collect();
    let (code, out, err) = run_sourced(&format!(
        r#"LIST="{pileup}"
if imag_obs_thread_concentration_ok "$LIST"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO", "single-core pileup must fail: {out:?}");

    // An empty/unreadable thread list (OBS not running, or `ps` failed) must never silently pass.
    let (code, out, err) = run_sourced(
        r#"LIST=""
if imag_obs_thread_concentration_ok "$LIST"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO", "empty thread list must fail: {out:?}");
}

// ---------------------------------------------------------------------------------------------
// (e) display-manager -> lightdm + autologin; gdm3 absent
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_autologin_conf_ok_requires_both_lines() {
    let full = "autologin-user=newlevel\nautologin-user-timeout=0\nautologin-session=openbox\n";
    let (code, out, err) = run_sourced(&format!(
        r#"TEXT="{full}"
if imag_autologin_conf_ok "$TEXT" "newlevel"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "YES", "{out:?}");

    // Missing the session=openbox line -> must fail (a subtly wrong config that logs in but into
    // the wrong session is NOT the kiosk).
    let (code, out, err) = run_sourced(
        r#"TEXT="autologin-user=newlevel
autologin-user-timeout=0"
if imag_autologin_conf_ok "$TEXT" "newlevel"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO", "{out:?}");
}

#[test]
fn imag_pkg_absent_is_the_inverse_of_dpkg_status_installed() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_pkg_absent ""; then echo YES; else echo NO; fi
        if imag_pkg_absent "purge ok not-installed"; then echo YES; else echo NO; fi
        if imag_pkg_absent "install ok installed"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "YES", "NO"],
        "gdm3 must be genuinely gone: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (f) zero failed systemd units
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_failed_units_ok_blank_only() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_failed_units_ok ""; then echo YES; else echo NO; fi
        if imag_failed_units_ok "   "; then echo YES; else echo NO; fi
        if imag_failed_units_ok "obs-websocket-hint.service loaded failed failed Foo"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["YES", "YES", "NO"], "{out:?}");
}

// ---------------------------------------------------------------------------------------------
// (g) openbox autostart placeholder / regular-executable-file / process-running
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_autostart_placeholders_resolved_catches_a_leftover_literal() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_autostart_placeholders_resolved "PYBIN=/usr/bin/python3"; then echo YES; else echo NO; fi
        if imag_autostart_placeholders_resolved "PYBIN=__PYBIN__"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO"],
        "a leftover __PYBIN__ means the sed step silently no-op'd: {out:?}"
    );
}

#[test]
fn imag_regular_file_and_executable_checks() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_regular_file_present "-rw-r--r--"; then echo YES; else echo NO; fi
        if imag_regular_file_present "drwxr-xr-x"; then echo YES; else echo NO; fi
        if imag_regular_file_present ""; then echo YES; else echo NO; fi
        if imag_regular_executable_file "-rwxr-xr-x"; then echo YES; else echo NO; fi
        if imag_regular_executable_file "-rw-r--r--"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "YES", "NO"],
        "regular-file / executable-bit checks: {out:?}"
    );
}

#[test]
fn imag_proc_running_exact_line_never_substring() {
    let (code, out, err) = run_sourced(
        r#"
        PS="openbox
obs-plugin-helper
obs"
        if imag_proc_running "$PS" "obs"; then echo YES; else echo NO; fi
        if imag_proc_running "$PS" "openbox"; then echo YES; else echo NO; fi
        if imag_proc_running "$PS" "obs-websocket"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "YES", "NO"],
        "'obs' must not substring-match 'obs-plugin-helper': {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (h) OBS log: genlock tick, version-mismatch, DistroAV + NDI loaded
// ---------------------------------------------------------------------------------------------

const OBS_LOG_HEALTHY: &str = "\
info: [obs-websocket] Server started successfully
info: [distroav] plugin loaded (full NDI features) (version 6.3.2)
info: NDI library initialized
info: genlock: wall-clock-slaved render tick ENABLED (latency = 3 ms)
";

const OBS_LOG_824_REGRESSION: &str = "\
warning: Module '/usr/lib/x86_64-linux-gnu/obs-plugins/obs-websocket.so' compiled with newer libobs 32.2
info: [distroav] plugin loaded (full NDI features) (version 6.3.2)
";

#[test]
fn imag_obs_log_checks_on_a_healthy_capture() {
    let (code, out, err) = run_sourced(&format!(
        r#"
        LOG="{OBS_LOG_HEALTHY}"
        if imag_obs_log_shows_genlock_tick "$LOG"; then echo YES; else echo NO; fi
        if imag_obs_log_no_version_mismatch "$LOG"; then echo YES; else echo NO; fi
        if imag_obs_log_shows_distroav_loaded "$LOG"; then echo YES; else echo NO; fi
        if imag_obs_log_shows_ndi_loaded "$LOG"; then echo YES; else echo NO; fi
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["YES", "YES", "YES", "YES"], "{out:?}");
}

#[test]
fn imag_obs_log_catches_the_824_version_mismatch_regression() {
    let (code, out, err) = run_sourced(&format!(
        r#"
        LOG="{OBS_LOG_824_REGRESSION}"
        if imag_obs_log_no_version_mismatch "$LOG"; then echo YES; else echo NO; fi
        if imag_obs_log_shows_genlock_tick "$LOG"; then echo YES; else echo NO; fi
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["NO", "NO"],
        "the #824 regression (obs-websocket refused, no genlock tick without websocket) must be \
         caught, not silently pass: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (j) OBS base version pin + apt-mark hold (#824)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_obs_base_version_matches_exact_nonempty_only() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_obs_base_version_matches "32.1.2-0obsproject1~noble" "32.1.2-0obsproject1~noble"; then echo YES; else echo NO; fi
        if imag_obs_base_version_matches "32.2.0-0obsproject1~noble" "32.1.2-0obsproject1~noble"; then echo YES; else echo NO; fi
        if imag_obs_base_version_matches "" "32.1.2-0obsproject1~noble"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO"],
        "a moved-on PPA base (#824) must fail the pin check: {out:?}"
    );
}

#[test]
fn imag_pkg_is_held_exact_line() {
    let (code, out, err) = run_sourced(
        r#"
        HOLD="obs-studio
lowlatency-kernel
linux-lowlatency-hwe-24.04"
        if imag_pkg_is_held "$HOLD" "obs-studio"; then echo YES; else echo NO; fi
        if imag_pkg_is_held "$HOLD" "obs"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO"],
        "'obs' must not substring-match 'obs-studio': {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (k) NDI runtime pinned -- proves the REUSED verify-device.sh functions actually compose here
// ---------------------------------------------------------------------------------------------

#[test]
fn ndi_symlink_functions_are_reused_from_verify_device_sh() {
    // A canonical `ls -la /usr/lib/ndi` listing (mirrors verify-device.sh's own fixture shape).
    let ls = "\
total 8
drwxr-xr-x 2 root root 4096 Jul 27 10:00 .
lrwxrwxrwx 1 root root   17 Jul 27 10:00 libndi.so -> libndi.so.6
lrwxrwxrwx 1 root root   19 Jul 27 10:00 libndi.so.6 -> libndi.so.6.3.2
-rwxr-xr-x 1 root root 999 Jul 27 10:00 libndi.so.6.3.2
";
    let (code, out, err) = run_sourced(&format!(
        r#"
        LS="{ls}"
        if ndi_symlink_chain_ok "$LS"; then echo YES; else echo NO; fi
        ndi_symlink_version "$LS"
        if ndi_version_matches "$(ndi_symlink_version "$LS")" "6.3.2"; then echo YES; else echo NO; fi
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "6.3.2", "YES"],
        "verify-device.sh's NDI-symlink parsers must compose correctly when reused here: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (k2) NVIDIA dGPU: driver + prime-select when present, correctly skipped when absent (#816)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_nvidia_verdict_na_ok_fail() {
    let (code, out, err) = run_sourced(
        r#"
        imag_nvidia_verdict "no" "" ""
        imag_nvidia_verdict "yes" "install ok installed" "nvidia"
        imag_nvidia_verdict "yes" "purge ok not-installed" "nvidia"
        imag_nvidia_verdict "yes" "install ok installed" "intel"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["na", "ok", "fail", "fail"],
        "a box with no dGPU must be 'na' (correctly skipped, #816), never 'fail': {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (l) dantesync PTP LOCKED + FRESH offset + SAME grandmaster (#834) -- composition proof
// ---------------------------------------------------------------------------------------------

#[test]
fn gm_check_composes_correctly_when_reused_from_clock_offset_guard_sh() {
    // Real captured #834 incident payload (mirrors tests/clock_offset_guard.rs's own fixture).
    let stream_json = "{\"is_locked\":true,\"mode\":\"NANO\",\"ntp_offset_us\":189,\
                        \"gm_source_ip\":\"10.77.7.109\"}";
    let (code, out, err) = run_sourced(&format!(
        r#"
        JSON='{stream_json}'
        GM="$(gm_source_ip_from_pipe_json "$JSON")"
        echo "$GM"
        set +e
        gm_check imag "$GM" "10.77.9.184"
        echo "rc=$?"
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("10.77.7.109") && out.contains("GM FOREIGN") && out.contains("rc=2"),
        "a foreign grandmaster (#834) must be caught when composed inside verify-imag.sh: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (n) scenes present + Multiview populated (imag_scenes.py, bare)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_scenes_output_ok_requires_both_sets_complete() {
    // #791: imag_scenes_output_ok now takes an EXPECTED_COUNT parameter (cam7 widened the fleet
    // from 6 to 7; the count must never be re-hardcoded as a literal "6" here again).
    let (code, out, err) = run_sourced(
        r#"
        OUT="video: 1920x1080@60/1 OK
scenes: 7/7 OK
MV scenes: 7/7 OK"
        if imag_scenes_output_ok "$OUT" 7; then echo YES; else echo NO; fi

        SHORT="video: 1920x1080@60/1 OK
scenes: 6/7 MISSING ['Cam 7']
MV scenes: 7/7 OK"
        if imag_scenes_output_ok "$SHORT" 7; then echo YES; else echo NO; fi

        # A stale caller that forgot to pass the count at all must fail closed, not silently
        # match on an empty pattern.
        if imag_scenes_output_ok "$OUT" ""; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO"],
        "a short scene set must fail the gate, and a missing count must fail closed: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (q) canonical scene ORDER + NDI-source bindings (imag_scenes.py --verify-parity, #791)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_parity_output_ok_requires_both_lines_ok() {
    let (code, out, err) = run_sourced(
        r#"
        OK_OUT="scene order: OK
ndi sources: OK"
        if imag_parity_output_ok "$OK_OUT"; then echo YES; else echo NO; fi

        BAD_ORDER="scene order: MISMATCH -- missing ['Cam 7', 'MV Cam 7']
ndi sources: OK"
        if imag_parity_output_ok "$BAD_ORDER"; then echo YES; else echo NO; fi

        BAD_NDI="scene order: OK
ndi sources: MISSING 'NDI resolume imag'"
        if imag_parity_output_ok "$BAD_NDI"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO"],
        "a scene-order OR ndi-source mismatch must fail the gate: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (r) OBS stats dock persistence: DockState in global.ini (#791)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_dockstate_present_requires_a_non_empty_dockstate_line() {
    let (code, out, err) = run_sourced(
        r#"
        WITH_DOCK="[OBSWebSocket]

[BasicWindow]
CloseExistingProjectors=true
geometry=AdnQywADAAAA
DockState=AAAA/wAAAAD9AAAA
SaveProjectors=false"
        if imag_dockstate_present "$WITH_DOCK"; then echo YES; else echo NO; fi

        NO_DOCK="[OBSWebSocket]

[BasicWindow]
CloseExistingProjectors=true
SaveProjectors=false"
        if imag_dockstate_present "$NO_DOCK"; then echo YES; else echo NO; fi

        EMPTY_DOCK="[BasicWindow]
DockState="
        if imag_dockstate_present "$EMPTY_DOCK"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO"],
        "DockState must be present AND non-empty (matches the exact confirmed-live shape of \
         BOTH known imag boxes' global.ini, which lack the key entirely -- #791): {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (o) projector count -- exactly 1 Program + 1 Multiview (#756/#758)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_projector_counts_ok_requires_exactly_one_each() {
    let cases = [
        ("1", "1", "YES"),
        ("0", "1", "NO"),
        ("2", "1", "NO"),
        ("1", "0", "NO"),
    ];
    for (mv, pgm, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_projector_counts_ok "{mv}" "{pgm}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_projector_counts_ok(mv={mv}, pgm={pgm}) expected {want}"
        );
    }
}

// #840: check (o) used to call `obs_phase2.py open-projectors` ITSELF before counting via
// wmctrl -- self-establishing the very condition it then asserted, so it would pass even on a
// box that comes up blank every single boot (the #840 root cause). The gate must instead (1)
// read the CURRENT projector counts with no side effect and FAIL if they're not already 1+1, and
// (2) restart OBS through the box's OWN operator scripts (imag-obs-stop.sh + imag-obs-start.sh --
// the SAME path a real reboot/manual restart uses) and re-count, to actually prove PERSISTENCE.

#[test]
fn verify_imag_no_longer_self_establishes_projectors_before_counting_840() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        !body.contains("open-projectors"),
        "verify-imag.sh must NOT call `obs_phase2.py open-projectors` any more (#840) -- that \
         call OPENS the projectors itself, so check (o) would self-establish the very condition \
         it then asserts, passing even on a box that comes up with zero projectors every boot"
    );
}

#[test]
fn verify_imag_restarts_obs_via_its_own_scripts_to_prove_persistence_840() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("/usr/local/bin/imag-obs-stop.sh")
            && body.contains("/usr/local/bin/imag-obs-start.sh"),
        "verify-imag.sh check (o) must restart OBS via imag-obs-stop.sh + imag-obs-start.sh (the \
         box's OWN operator scripts, the same path a real reboot/manual restart uses) to prove \
         the projectors PERSIST across a real restart (#840), not just that they can be opened \
         once from dev1"
    );
}

#[test]
fn verify_imag_counts_projectors_before_and_after_the_restart_840() {
    let body = std::fs::read_to_string(script()).unwrap();
    let restart = body
        .find("/usr/local/bin/imag-obs-stop.sh")
        .expect("the restart invocation must be present");
    // The wmctrl projector-count read (grep -c 'Projector - Multiview') must appear on BOTH
    // sides of the restart call -- once to prove the box's OWN startup path already established
    // them (no self-establish), and again afterward to prove they came back.
    let counts_before: Vec<_> = body
        .match_indices("grep -c 'Projector - Multiview'")
        .map(|(i, _)| i)
        .collect();
    assert!(
        counts_before.len() >= 2,
        "verify-imag.sh must count the Multiview projector window BOTH before and after the \
         restart (#840) -- found {} occurrence(s)",
        counts_before.len()
    );
    assert!(
        counts_before[0] < restart,
        "the FIRST projector count must happen BEFORE the restart (proving the box's own \
         startup path already had them, never self-established by this gate)"
    );
    assert!(
        counts_before.iter().any(|&i| i > restart),
        "a projector count must ALSO happen AFTER the restart (proving persistence, #840)"
    );
}

// ---------------------------------------------------------------------------------------------
// (p) operator scaffolding present (#791)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_openbox_menu_looks_valid_rejects_empty_or_non_xml() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_openbox_menu_looks_valid "<openbox_menu><menu id=\"root-menu\"></menu></openbox_menu>"; then echo YES; else echo NO; fi
        if imag_openbox_menu_looks_valid ""; then echo YES; else echo NO; fi
        if imag_openbox_menu_looks_valid "not xml at all"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["YES", "NO", "NO"], "{out:?}");
}

#[test]
fn imag_watchdog_installed_but_disabled_requires_all_three_facts() {
    let (code, out, err) = run_sourced(
        r#"
        # present, unit installed, disabled -> ok (the #791 agreed model)
        if imag_watchdog_installed_but_disabled "-rwxr-xr-x" "imag-obs-watchdog.service" "disabled"; then echo YES; else echo NO; fi
        # unit not installed at all -> fail (missing is NOT disabled)
        if imag_watchdog_installed_but_disabled "-rwxr-xr-x" "" "not-found"; then echo YES; else echo NO; fi
        # script missing -> fail
        if imag_watchdog_installed_but_disabled "" "imag-obs-watchdog.service" "disabled"; then echo YES; else echo NO; fi
        # enabled to auto-start -> fail (must stay disabled per the agreed model until #788)
        if imag_watchdog_installed_but_disabled "-rwxr-xr-x" "imag-obs-watchdog.service" "enabled"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO"],
        "installed-but-disabled requires ALL three facts: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (t) imag-obs.service supervision (#884, follow-up to #882) — the openbox-autostart-vs-live-box
// divergence: the live box (10.77.9.182) already runs the boot launch through the supervised
// systemd unit (enabled+active, Restart=on-failure), but setup-imag.sh still wrote the OLD direct
// script call and verify-imag.sh had ZERO checks for any of this — so a fresh reprovision would
// silently regress to the unsupervised state that produced the 2026-07-30 ~70-minute OBS outage,
// and the acceptance gate would certify that regression as ALL CLEAR.
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_obs_service_state_ok_requires_enabled_and_active() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_obs_service_state_ok "enabled" "active"; then echo YES; else echo NO; fi
        if imag_obs_service_state_ok "disabled" "active"; then echo YES; else echo NO; fi
        if imag_obs_service_state_ok "enabled" "inactive"; then echo YES; else echo NO; fi
        if imag_obs_service_state_ok "not-found" "inactive"; then echo YES; else echo NO; fi
        # whitespace from a real `systemctl --user is-enabled` reply (trailing newline) must not
        # break the comparison
        if imag_obs_service_state_ok "$(printf 'enabled\n')" "$(printf 'active\n')"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO", "YES"],
        "imag-obs.service must be BOTH enabled AND active — a re-provisioned box with the unit \
         merely installed (not enabled) or enabled-but-not-running must fail: {out:?}"
    );
}

#[test]
fn imag_obs_service_restart_is_on_failure_rejects_always() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_obs_service_restart_is_on_failure "Restart=on-failure"; then echo YES; else echo NO; fi
        # issue 788's operator-fighting bug: an always-restart fights a deliberate manual quit
        if imag_obs_service_restart_is_on_failure "Restart=always"; then echo YES; else echo NO; fi
        if imag_obs_service_restart_is_on_failure "Restart=no"; then echo YES; else echo NO; fi
        if imag_obs_service_restart_is_on_failure ""; then echo YES; else echo NO; fi
        if imag_obs_service_restart_is_on_failure "$(printf 'Restart=on-failure\n')"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO", "YES"],
        "Restart must be EXACTLY on-failure — never 'always' (issue 788's operator-fighting bug, \
         an always-restart fights a deliberate manual quit): {out:?}"
    );
}

#[test]
fn imag_autostart_launches_via_service_not_script_884() {
    let (code, out, err) = run_sourced(
        r#"
        # #884: the healthy, current form
        GOOD='sleep 1
export IMAG_ISOLATED_CPUS="2,3,4,5,6,7"
systemctl --user start imag-obs.service || true'
        if imag_autostart_launches_via_service_not_script "$GOOD"; then echo YES; else echo NO; fi

        # the OLD, regressed form (pre-#884) — direct script call, no systemd supervision
        BAD='sleep 1
export IMAG_ISOLATED_CPUS="2,3,4,5,6,7"
/usr/local/bin/imag-obs-start.sh >>/tmp/imag-seed.log 2>&1 || true'
        if imag_autostart_launches_via_service_not_script "$BAD"; then echo YES; else echo NO; fi

        # a header COMMENT mentioning imag-obs-start.sh in prose (as this repo's own comments do)
        # must NOT be mistaken for a direct CODE call — only real code lines matter (the exact
        # anchor-collision class this repo's CLAUDE.md GOTCHA warns about)
        COMMENTED='# imag-obs-start.sh is invoked BY the unit below (#840/#884), not called here
systemctl --user start imag-obs.service || true'
        if imag_autostart_launches_via_service_not_script "$COMMENTED"; then echo YES; else echo NO; fi

        # neither call present at all -> fail closed
        if imag_autostart_launches_via_service_not_script "sleep 1"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "YES", "NO"],
        "the autostart must launch via imag-obs.service, never a direct imag-obs-start.sh call — \
         a prose comment mentioning the script name must not be mistaken for the call: {out:?}"
    );
}

#[test]
fn imag_core_pattern_captures_dumps_requires_a_piped_collector() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_core_pattern_captures_dumps "|/usr/lib/systemd/systemd-coredump %P %u %g %s %t 9223372036854775808 %h %d"; then echo YES; else echo NO; fi
        if imag_core_pattern_captures_dumps "core"; then echo YES; else echo NO; fi
        if imag_core_pattern_captures_dumps "core.%p"; then echo YES; else echo NO; fi
        if imag_core_pattern_captures_dumps ""; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO"],
        "kernel.core_pattern must be a PIPED collector (systemd-coredump/apport) — a bare/relative \
         pattern can silently drop a core (wrong cwd, read-only rootfs) even with an unlimited \
         ulimit: {out:?}"
    );
}

#[test]
fn imag_obs_core_dumps_enabled_requires_unlimited_both_columns() {
    let (code, out, err) = run_sourced(
        r#"
        if imag_obs_core_dumps_enabled "Max core file size        unlimited            unlimited            bytes"; then echo YES; else echo NO; fi
        # the #882 root cause: ulimit -c was 0, so the 2026-07-30 segfault left nothing debuggable
        if imag_obs_core_dumps_enabled "Max core file size        0                    0                    bytes"; then echo YES; else echo NO; fi
        if imag_obs_core_dumps_enabled "Max core file size        unlimited            0                    bytes"; then echo YES; else echo NO; fi
        if imag_obs_core_dumps_enabled ""; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO"],
        "the LIVE obs process's own /proc/<pid>/limits must show Max core file size unlimited on \
         BOTH the soft and hard column — proof LimitCORE=infinity is actually applied to the \
         running process, not just configured in the unit file: {out:?}"
    );
}

/// Live-caught on 10.77.9.182 (#884): check (o)'s restart-proof (#840) calls
/// imag-obs-stop.sh/imag-obs-start.sh DIRECTLY over SSH, bypassing systemctl entirely -- which
/// leaves imag-obs.service `inactive (dead)` (systemd loses track of the main process once the
/// wrapper's own blocking `wait` returns) and starts a fresh, UNTRACKED obs process with NO
/// LimitCORE applied (confirmed live: the post-restart process showed `Max core file size = 0`,
/// not unlimited -- LimitCORE is a systemd-applied cgroup property, never inherited by a bare SSH
/// invocation). The #884 checks MUST read the box's state BEFORE this restart runs, or they
/// falsely FAIL a genuinely healthy, correctly-provisioned box every single time this gate runs.
#[test]
fn verify_imag_reads_884_service_state_before_the_840_restart_wipes_it() {
    let body = std::fs::read_to_string(script()).unwrap();
    let service_check = body
        .find("systemctl --user is-enabled imag-obs.service")
        .expect("the imag-obs.service enabled/active check must exist (#884)");
    let restart_call = body
        .find(r#"/usr/local/bin/imag-obs-stop.sh && /usr/local/bin/imag-obs-start.sh"#)
        .expect("check (o)'s restart-proof call must exist (#840)");
    assert!(
        service_check < restart_call,
        "the #884 imag-obs.service checks must run BEFORE check (o)'s restart-proof (#840) -- \
         reading them afterward would falsely FAIL a genuinely healthy, correctly-provisioned box \
         (live-confirmed on 10.77.9.182, see this test's own doc comment)"
    );
}

#[test]
fn verify_imag_wires_the_new_884_checks_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    for needle in [
        "imag_obs_service_state_ok",
        "imag_obs_service_restart_is_on_failure",
        "imag_autostart_launches_via_service_not_script",
        "imag_core_pattern_captures_dumps",
        "imag_obs_core_dumps_enabled",
    ] {
        assert!(
            body.matches(needle).count() >= 2,
            "verify-imag.sh must both DEFINE and CALL {needle} in its live flow (#884) — a pure \
             function that is only ever defined and never invoked provides zero acceptance \
             coverage"
        );
    }
}

#[test]
fn verify_imag_exits_nonzero_on_any_failed_check() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("exit 1") || body.contains("exit \"$FAILS\""),
        "verify-imag.sh must exit non-zero when any check fails (test-strictness)"
    );
}
