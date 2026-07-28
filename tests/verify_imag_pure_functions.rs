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
// (d) kernel cmdline: preempt=full + DERIVED isolation (#289/#303/#816)
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

#[test]
fn imag_cmdline_has_derived_isolation_matches_the_derived_plan_never_a_literal() {
    // Real derived plan for a 12-thread (6 P-core-pair + ... ) box, byte-for-byte what
    // imag_cpu_isolation_plan would compute -- this test proves the CHECK correctly matches a
    // DERIVED plan, not a fixed cam-fleet-style literal like isolcpus=3.
    let cmdline = "BOOT_IMAGE=/vmlinuz quiet isolcpus=2,4,6,8,10 nohz_full=10 irqaffinity=0,12,13,14,15 preempt=full";
    let (code, out, err) = run_sourced(&format!(
        r#"if imag_cmdline_has_derived_isolation "{cmdline}" "2,4,6,8,10" "10" "0,12,13,14,15"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "YES",
        "must match the exact derived plan: {out:?}"
    );

    // A DIFFERENT (e.g. stale/cam-fleet) plan must NOT match.
    let (code, out, err) = run_sourced(&format!(
        r#"if imag_cmdline_has_derived_isolation "{cmdline}" "3" "10,11" "0-2"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "NO", "a mismatched plan must fail: {out:?}");

    // An empty derived value (topology gather failed) must never silently pass.
    let (code, out, err) = run_sourced(&format!(
        r#"if imag_cmdline_has_derived_isolation "{cmdline}" "" "10" "0,12,13,14,15"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "NO",
        "empty derived isolated-cpu set must fail: {out:?}"
    );
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
    let (code, out, err) = run_sourced(
        r#"
        OUT="video: 1920x1080@60/1 OK
scenes: 6/6 OK
MV scenes: 6/6 OK"
        if imag_scenes_output_ok "$OUT"; then echo YES; else echo NO; fi

        SHORT="video: 1920x1080@60/1 OK
scenes: 5/6 MISSING ['Cam 6']
MV scenes: 6/6 OK"
        if imag_scenes_output_ok "$SHORT"; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO"],
        "a short scene set must fail the gate: {out:?}"
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

#[test]
fn verify_imag_exits_nonzero_on_any_failed_check() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("exit 1") || body.contains("exit \"$FAILS\""),
        "verify-imag.sh must exit non-zero when any check fails (test-strictness)"
    );
}
