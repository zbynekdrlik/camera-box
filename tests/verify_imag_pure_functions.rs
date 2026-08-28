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

/// #1183: `ps -L -o psr= -C obs` RIGHT-PADS its single column to the widest value's width, so a
/// healthy box's REAL output is "  6" / " 11" (verified `cat -A`: "  6$"), NOT the bare "6" the
/// pre-#1183 `^[0-9]+$` greps assumed. Padded lines matched ZERO -> total=0 -> the function
/// false-FAILED on a perfectly healthy box. This proves the padded REAL format is tolerated, and
/// that a genuine pileup STILL fails even when padded (normalisation must not weaken the bound).
#[test]
fn imag_obs_thread_concentration_ok_tolerates_space_padded_psr_output_1183() {
    // A HEALTHY spread, each core number RIGHT-PADDED to width 3 exactly as `ps -L -o psr=` emits
    // it on a box whose highest core is two digits: "  6" (2 leading spaces) ... " 11" (1 space).
    // 12+10+14+16+9+11 = 72 threads across 6 cores, max 16 -> 16/72 = 22% (well under the 60% bound).
    let padded: String = std::iter::repeat_n("  6\n", 12)
        .chain(std::iter::repeat_n("  7\n", 10))
        .chain(std::iter::repeat_n("  8\n", 14))
        .chain(std::iter::repeat_n("  9\n", 16))
        .chain(std::iter::repeat_n(" 10\n", 9))
        .chain(std::iter::repeat_n(" 11\n", 11))
        .collect();
    let (code, out, err) = run_sourced(&format!(
        r#"LIST="{padded}"
if imag_obs_thread_concentration_ok "$LIST"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "YES",
        "space-padded `ps -L -o psr=` output (the REAL format) must be tolerated, not false-FAIL: {out:?}"
    );

    // A genuine single-core pileup must STILL fail even when padded -- normalisation strips the
    // padding, it does not weaken the concentration bound. 60 of 72 on core "  8" -> 83% > 60%.
    let padded_pileup: String = std::iter::repeat_n("  8\n", 60)
        .chain(std::iter::repeat_n(" 11\n", 12))
        .collect();
    let (code, out, err) = run_sourced(&format!(
        r#"LIST="{padded_pileup}"
if imag_obs_thread_concentration_ok "$LIST"; then echo YES; else echo NO; fi"#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "NO",
        "a padded single-core pileup must still FAIL -- padding-normalisation must not weaken the bound: {out:?}"
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

/// #1183: OBS logs carry raw invalid-UTF-8 bytes (DistroAV mojibake). In a UTF-8 locale, GNU grep
/// WITHOUT `-a` fails to match a marker that IS present. The fix adds `-a` + `LC_ALL=C` so matching
/// is deterministic byte-literal regardless of ambient locale or embedded invalid bytes. The
/// fixture embeds a real invalid-byte sequence (`\x83?\xdd` on the distroav line, `\xe2\x82` in the
/// genlock marker's `.*` gap) alongside genuine marker lines; `export LC_ALL=C.UTF-8` makes the
/// pre-fix miss deterministic on any runner (the fixed function carries its OWN `LC_ALL=C`, which
/// overrides this for the actual match, so the GREEN direction is locale-independent).
#[test]
fn imag_obs_log_matchers_survive_invalid_utf8_bytes_1183() {
    let (code, out, err) = run_sourced(
        r#"export LC_ALL=C.UTF-8
LOG="$(printf 'info: [obs-websocket] Server started successfully\ninfo: [distroav] plugin loaded (full NDI features) \x83?\xdd (version 6.3.2)\ninfo: NDI library initialized\ninfo: genlock: wall-clock-slaved \xe2\x82render tick ENABLED (latency = 3 ms)\n')"
if imag_obs_log_shows_genlock_tick "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_no_version_mismatch "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_shows_distroav_loaded "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_shows_ndi_loaded "$LOG"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "YES", "YES", "YES"],
        "invalid-UTF-8 bytes in the log must not suppress a marker match (#1183): {out:?}"
    );

    // The #824 'compiled with newer libobs' mismatch must STILL be caught when invalid bytes sit in
    // the SAME log -- the -a/LC_ALL=C audit fix must not blind the negative check either.
    let (code, out, err) = run_sourced(
        r#"export LC_ALL=C.UTF-8
LOG="$(printf 'warning: [distroav] recv \x83?\xdd frame\nwarning: Module obs-websocket.so \xe2\x82compiled with newer libobs 32.2\n')"
if imag_obs_log_no_version_mismatch "$LOG"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim(),
        "NO",
        "the #824 'compiled with newer libobs' mismatch must be caught despite invalid bytes (#1183): {out:?}"
    );
}

/// #1183 residual: `verify-imag.sh` runs under `set -euo pipefail`, and a matcher fed via
/// `printf '%s' "$1" | grep -q` SIGPIPEs the writer (rc=141) the instant `grep -q` exits early on
/// a match -- which it always does, because the genlock marker is a startup line at the TOP while
/// live OBS logs are 173 KB-40 MB (far over the 64 KiB pipe capacity). `pipefail` then promotes
/// that 141 to the pipeline status and the matcher false-FAILs a healthy box DESPITE the match.
/// The sanctioned issue-1047 fix is a here-string (`<<<"$1"`): bash writes the whole body to a
/// temp file, so there is no live writer to SIGPIPE at ANY size. RED against the pipe form (the
/// genlock matcher returns NO on an over-capacity log), GREEN after all four (h) matchers feed
/// grep from a here-string. The small woven fixtures in the sibling invalid-UTF-8 test pass BOTH
/// ways, which is why the first RED->GREEN missed this -- a >64 KiB body with the marker at the TOP
/// is the missing coverage (the issue-1047 fixture recipe).
#[test]
fn imag_obs_log_matchers_are_sigpipe_immune_over_pipe_capacity_1183() {
    // >64 KiB body (~200 KB of filler) with the DistroAV/NDI/genlock markers at the very TOP, so
    // an early-exiting `grep -q` closes the pipe long before a `printf` writer finishes -> SIGPIPE.
    let (code, out, err) = run_sourced(
        r#"FILLER="$(head -c 200000 /dev/zero | tr '\0' x)"
LOG="info: [distroav] plugin loaded (full NDI features) (version 6.3.2)
info: NDI library initialized
info: genlock: wall-clock-slaved render tick ENABLED (latency = 3 ms)
$FILLER"
if imag_obs_log_shows_genlock_tick "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_no_version_mismatch "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_shows_distroav_loaded "$LOG"; then echo YES; else echo NO; fi
if imag_obs_log_shows_ndi_loaded "$LOG"; then echo YES; else echo NO; fi"#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["YES", "YES", "YES", "YES"],
        "an over-64-KiB OBS log with the markers at the TOP must not SIGPIPE-false-FAIL any (h) \
         matcher under pipefail (#1183 residual): {out:?}"
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
// (l) dantesync phase_slew ENABLED (#1215) -- imag-nb shipped with no /etc/dantesync/config.json
// at all and stepped the clock (16x/hour, ~4-min hitch) instead of slewing it; the config gap is
// closed in setup-imag.sh, and this check makes a future regression to no-config fail loud rather
// than shipping the same silent hitch again.
// ---------------------------------------------------------------------------------------------

#[test]
fn phase_slew_check_composes_correctly_when_reused_from_clock_offset_guard_sh_1215() {
    let json = "{\"phase_slew_enabled\":false,\"mode\":\"PROD\",\"ntp_offset_us\":2924}";
    let (code, out, err) = run_sourced(&format!(
        r#"
        JSON='{json}'
        PS="$(phase_slew_enabled_from_pipe_json "$JSON")"
        echo "$PS"
        set +e
        phase_slew_check imag "$PS"
        echo "rc=$?"
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("false") && out.contains("PHASE-SLEW DISABLED") && out.contains("rc=2"),
        "imag-nb's pre-#1215 disabled state must be caught when composed inside verify-imag.sh: {out:?}"
    );
}

#[test]
fn verify_imag_wires_phase_slew_check_into_the_live_flow_1215() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("phase_slew_check imag"),
        "verify-imag.sh must CALL phase_slew_check on the imag-nb dantesync status (#1215) -- a \
         pure function that is only ever defined (in clock-offset-guard.sh) and never invoked \
         here provides zero acceptance coverage for the phase_slew provisioning gap"
    );
    assert!(
        body.contains("phase_slew_enabled_from_pipe_json"),
        "verify-imag.sh must parse phase_slew_enabled out of the SAME $DS_HTTP_STATUS blob check \
         (l) already fetches for ptp_locked/offset/gm_source_ip (#1215)"
    );
}

#[test]
fn clock_offset_guard_defines_phase_slew_enabled_from_pipe_json_and_phase_slew_check_1215() {
    let guard_path = manifest_dir().join("scripts/clock-offset-guard.sh");
    let body = std::fs::read_to_string(&guard_path).unwrap();
    for needle in ["phase_slew_enabled_from_pipe_json() {", "phase_slew_check() {"] {
        assert!(
            body.contains(needle),
            "scripts/clock-offset-guard.sh must define {needle} (#1215), following the EXACT \
             shape of gm_source_ip_from_pipe_json()/gm_check() in the same file"
        );
    }
}

#[test]
fn verify_imag_fails_loud_on_phase_slew_when_the_journal_fallback_path_cannot_read_it_1215() {
    // check (l)'s journal-fallback branch (HTTP status unreachable) has NO phase_slew_enabled
    // field to read (journald carries no such field, same as gm_source_ip per #834) -- it must
    // FAIL LOUD naming phase_slew, never silently skip the check (imag-ssh-remote-tool-preflight
    // discipline: an unreadable signal is a hard FAIL, never a silent pass).
    let body = std::fs::read_to_string(script()).unwrap();
    let l_start = body
        .find("# (l) dantesync PTP LOCKED")
        .expect("check (l) marker must exist");
    let m_start = body[l_start..]
        .find("# (m) dantesync is the SOLE")
        .map(|off| l_start + off)
        .expect("check (m) marker must exist, bounding the (l) block");
    let l_region = &body[l_start..m_start];
    assert!(
        l_region.to_lowercase().contains("phase_slew") && l_region.contains("fail \""),
        "check (l)'s journal-fallback branch must fail loud naming phase_slew when :8898/status \
         is unreachable (#1215), mirroring the existing grandmaster-unreadable fail in the same \
         branch: {l_region}"
    );
}

// ---------------------------------------------------------------------------------------------
// (n) scenes present + Multiview populated (imag_scenes.py, bare)
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_scenes_output_ok_requires_both_sets_complete() {
    // #791: imag_scenes_output_ok now takes an EXPECTED_COUNT parameter (cam7 widened the fleet
    // from 6 to 7; the count must never be re-hardcoded as a literal "6" here again).
    //
    // #843: the OUT/SHORT fixtures below use imag_scenes.py's REAL printed line --
    // "MV scenes: N/N (multiview, low-bw) OK" -- not the old assumed "MV scenes: N/N OK" shape.
    // The regex must match the ACTUAL producer output, confirmed live on 10.77.9.187 2026-07-28
    // (see #843), never an assumed format.
    let (code, out, err) = run_sourced(
        r#"
        OUT="video: 1920x1080@60/1 OK
scenes: 7/7 OK
MV scenes: 7/7 (multiview, low-bw) OK"
        if imag_scenes_output_ok "$OUT" 7; then echo YES; else echo NO; fi

        SHORT="video: 1920x1080@60/1 OK
scenes: 6/7 MISSING ['Cam 7']
MV scenes: 7/7 (multiview, low-bw) OK"
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
        "a healthy real MV-scenes line must PASS, a short scene set must fail the gate, and a \
         missing count must fail closed: {out:?}"
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
// (2) restart OBS and re-count, to actually prove PERSISTENCE.
// #890: the restart must go through the SERVICE (systemctl --user restart imag-obs.service),
// NEVER a direct `imag-obs-stop.sh && imag-obs-start.sh` ssh call -- since #882 imag-obs-start.sh
// blocks on `wait "$OBS_PID"`, so a DIRECT ssh invocation never returns and hangs the whole gate
// forever. systemd (Type=simple) owns that blocking wait, so a service restart returns promptly;
// it must be wrapped in a hard execution timeout and followed by a BOUNDED poll (fail loud on
// expiry), never an unbounded wait.

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
fn verify_imag_restarts_obs_via_the_service_never_a_blocking_direct_script_call_890() {
    let body = std::fs::read_to_string(script()).unwrap();
    // #890: a DIRECT `imag-obs-stop.sh && imag-obs-start.sh` ssh call hangs forever on #882's
    // blocking `wait "$OBS_PID"`. It must be gone.
    assert!(
        !body.contains(r#"/usr/local/bin/imag-obs-stop.sh && /usr/local/bin/imag-obs-start.sh"#),
        "verify-imag.sh check (o) must NOT restart OBS via a DIRECT `imag-obs-stop.sh && \
         imag-obs-start.sh` ssh call -- that hangs the gate forever on #882's blocking wait (#890)"
    );
    // It must restart through the service (systemd owns the wait -> returns promptly + keeps the
    // new obs supervised) via the pure `imag_obs_service_restart_cmd` helper, both DEFINED and
    // CALLED, and wrap the ssh call in a hard execution timeout.
    assert!(
        body.matches("imag_obs_service_restart_cmd").count() >= 2,
        "verify-imag.sh must both DEFINE and CALL imag_obs_service_restart_cmd (#890) -- a pure \
         function only ever defined and never invoked provides zero acceptance coverage"
    );
    assert!(
        body.contains(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#),
        "verify-imag.sh check (o) must wrap the service restart in a bounded execution timeout \
         (ssh_box_timeout \"$IMAG_OBS_RESTART_TIMEOUT\") so the gate can NEVER hang again (#890)"
    );
}

#[test]
fn verify_imag_counts_projectors_before_and_after_a_bounded_service_restart_890() {
    let body = std::fs::read_to_string(script()).unwrap();
    let restart = body
        .find(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#)
        .expect("the bounded service-restart call must be present (#890)");
    // The wmctrl projector-count read (grep -c 'Projector - Multiview') must appear on BOTH
    // sides of the restart call -- once to prove the box's OWN startup path already established
    // them (no self-establish, #840), and again afterward to prove they came back.
    let counts: Vec<_> = body
        .match_indices("grep -c 'Projector - Multiview'")
        .map(|(i, _)| i)
        .collect();
    assert!(
        counts.len() >= 2,
        "verify-imag.sh must count the Multiview projector window BOTH before and after the \
         restart (#840) -- found {} occurrence(s)",
        counts.len()
    );
    assert!(
        counts[0] < restart,
        "the FIRST projector count must happen BEFORE the restart (proving the box's own \
         startup path already had them, never self-established by this gate, #840)"
    );
    assert!(
        counts.iter().any(|&i| i > restart),
        "a projector count must ALSO happen AFTER the restart (proving persistence, #840)"
    );
    // #890: the post-restart re-count must be a BOUNDED poll that FAILs loud on expiry, never an
    // unbounded wait -- keyed on the IMAG_OBS_PROJECTOR_POLL_S deadline knob.
    assert!(
        body.contains("IMAG_OBS_PROJECTOR_POLL_S"),
        "the post-restart projector re-count must be a bounded poll (IMAG_OBS_PROJECTOR_POLL_S \
         deadline), never an unbounded wait (#890)"
    );
}

// #890: the pure command-builder for check (o)'s restart. It must target the SERVICE (systemd
// owns #882's blocking wait, so the ssh call returns promptly + the new obs stays supervised),
// export XDG_RUNTIME_DIR so a non-graphical ssh session can reach the user bus, and NEVER invoke
// imag-obs-start.sh directly (that hangs forever on `wait "$OBS_PID"`).
#[test]
fn imag_obs_service_restart_cmd_targets_the_service_never_the_blocking_start_script_890() {
    let (code, out, err) = run_sourced(r#"imag_obs_service_restart_cmd"#);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("systemctl --user restart imag-obs.service"),
        "imag_obs_service_restart_cmd must restart OBS through imag-obs.service (#890): {out:?}"
    );
    assert!(
        !out.contains("imag-obs-start.sh"),
        "imag_obs_service_restart_cmd must NOT invoke imag-obs-start.sh directly -- that script \
         blocks on `wait \"$OBS_PID\"` (#882) and hangs the gate forever over ssh (#890): {out:?}"
    );
    assert!(
        out.contains("XDG_RUNTIME_DIR"),
        "the restart command must export XDG_RUNTIME_DIR so a non-graphical ssh session can reach \
         the user bus (imag-obs-supervision.md): {out:?}"
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

// #1095: the #785 menu.xml (<menu id="root-menu">) is only REACHABLE if the openbox rc.xml binds
// the desktop right-click (Root mouse context, Right button) to `ShowMenu root-menu`. On a fresh
// box the stock /etc/xdg/openbox/rc.xml holds that binding; a box carrying a STALE hand-placed
// ~/.config/openbox/rc.xml (the "hand-placed, not provisioned" class #785 exists to close) could
// bind the desktop click elsewhere, silently orphaning the menu. verify-imag.sh must ASSERT the
// binding (design decision (b): assert-only, never rewrite operator rc.xml).
#[test]
fn imag_openbox_root_menu_bound_requires_root_context_right_click_to_root_menu() {
    let (code, out, err) = run_sourced(
        r#"
        # stock-style: Root context, Right button -> ShowMenu root-menu -> BOUND
        GOOD='<mouse><context name="Root"><mousebind button="Middle" action="Press"><action name="ShowMenu"><menu>client-list-combined-menu</menu></action></mousebind><mousebind button="Right" action="Press"><action name="ShowMenu"><menu>root-menu</menu></action></mousebind></context></mouse>'
        # stale operator rc.xml: Root right-click bound to a DIFFERENT menu -> NOT bound (the target failure)
        STALE='<mouse><context name="Root"><mousebind button="Right" action="Press"><action name="ShowMenu"><menu>my-custom-menu</menu></action></mousebind></context></mouse>'
        # decoy: root-menu referenced only in a KEYBIND; Root right-click bound elsewhere -> NOT bound (scoping matters)
        DECOY='<keyboard><keybind key="A-space"><action name="ShowMenu"><menu>root-menu</menu></action></keybind></keyboard><mouse><context name="Root"><mousebind button="Right" action="Press"><action name="ShowMenu"><menu>apps-menu</menu></action></mousebind></context></mouse>'
        # single-quoted attributes (valid XML, openbox/libxml2 accepts) -> BOUND
        SQ="<mouse><context name='Root'><mousebind button='Right' action='Press'><action name='ShowMenu'><menu>root-menu</menu></action></mousebind></context></mouse>"
        # [review #1095] the real root-menu bind is COMMENTED OUT and Right rebound to apps-menu ->
        # NOT bound (a disabled <!-- --> binding must never false-PASS an actually-orphaned menu)
        COMMENTED='<mouse><context name="Root"><!-- <mousebind button="Right" action="Press"><action name="ShowMenu"><menu>root-menu</menu></action></mousebind> --><mousebind button="Right" action="Press"><action name="ShowMenu"><menu>apps-menu</menu></action></mousebind></context></mouse>'
        # [review #1095] XML attribute order is not significant: button not first, action name not
        # first -> still BOUND (must not false-FAIL a legal reordered rc.xml)
        REORDER='<mouse><context name="Root"><mousebind action="Press" button="Right"><action enabled="true" name="ShowMenu"><menu>root-menu</menu></action></mousebind></context></mouse>'
        for name in GOOD STALE DECOY SQ COMMENTED REORDER; do
          if imag_openbox_root_menu_bound "${!name}"; then echo BOUND; else echo NOTBOUND; fi
        done
        # empty / absent rc.xml text -> NOT bound
        if imag_openbox_root_menu_bound ""; then echo BOUND; else echo NOTBOUND; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["BOUND", "NOTBOUND", "NOTBOUND", "BOUND", "NOTBOUND", "BOUND", "NOTBOUND"],
        "rc.xml Root right-click binding must be scoped to the Root context + Right button + \
         ShowMenu root-menu (a stale bind-elsewhere fails, a keybind-only root-menu does NOT \
         count, a COMMENTED-OUT binding does NOT count, and attribute ORDER is irrelevant): {out:?}"
    );
}

#[test]
fn verify_imag_wires_the_rc_menu_binding_check_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.matches("imag_openbox_root_menu_bound").count() >= 2,
        "verify-imag.sh must both DEFINE and CALL imag_openbox_root_menu_bound in its live flow \
         (#1095) -- a pure function that is only ever defined and never invoked provides zero \
         acceptance coverage that the #785 menu is reachable"
    );
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

// ---------------------------------------------------------------------------------------------
// (t) cont'd — the running obs PID must genuinely live INSIDE imag-obs.service's cgroup, not just
// systemd's own is-enabled/is-active bookkeeping (#1015, the #840 claim-vs-reality class). Live
// finding (2026-08-13): setup-imag.sh step 21 genuinely installs+enables the unit, but every
// actual recovery this ticket investigated launched OBS via a direct imag-obs-start.sh call
// instead — outside the unit's cgroup entirely — so Restart=on-failure supervised nothing even
// while the unit itself sat correctly enabled. A per-PID cgroup read is the independent,
// can't-be-faked-by-stale-bookkeeping proof that the LIVE process is the supervised one.
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_obs_cgroup_shows_service_unit_requires_the_real_unit_component() {
    let (code, out, err) = run_sourced(
        r#"
        # cgroup v2 unified hierarchy -- the real shape on this box's systemd --user session
        if imag_obs_cgroup_shows_service_unit "0::/user.slice/user-1000.slice/user@1000.service/app.slice/imag-obs.service"; then echo YES; else echo NO; fi
        # cgroup v1 hybrid -- multiple controller lines, unit component on one of them
        if imag_obs_cgroup_shows_service_unit "$(printf '12:pids:/user.slice/user-1000.slice/user@1000.service/app.slice/imag-obs.service\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/imag-obs.service\n')"; then echo YES; else echo NO; fi
        # the #1015 live-observed BAD state -- launched directly, outside any unit's cgroup
        if imag_obs_cgroup_shows_service_unit "0::/user.slice/user-1000.slice/session-2.scope"; then echo YES; else echo NO; fi
        # a DIFFERENT unit must not false-match a bare substring/prefix of the real name
        if imag_obs_cgroup_shows_service_unit "0::/user.slice/user-1000.slice/user@1000.service/app.slice/imag-obs.service-old"; then echo YES; else echo NO; fi
        if imag_obs_cgroup_shows_service_unit "0::/user.slice/user-1000.slice/user@1000.service/app.slice/not-imag-obs.service"; then echo YES; else echo NO; fi
        if imag_obs_cgroup_shows_service_unit ""; then echo YES; else echo NO; fi
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "YES", "NO", "NO", "NO", "NO"],
        "the live obs PID's /proc/<pid>/cgroup must show a genuine imag-obs.service path \
         component (component-boundary matched, never a bare substring/prefix) — the #1015 proof \
         that the RUNNING process is actually supervised, not merely that systemd's own \
         is-enabled/is-active bookkeeping claims it is: {out:?}"
    );
}

/// Live-caught on 10.77.9.182 (#884): check (o)'s restart-proof (#840) RESTARTS obs, which
/// REPLACES the tracked obs process with a fresh one. The #884 checks (unit enabled+active,
/// Restart=, autostart wiring, core-dump enablement) must read the box's BOOT-TIME state BEFORE
/// that restart runs, or they observe the post-restart process instead and can falsely FAIL a
/// genuinely healthy, correctly-provisioned box. (#890 changed the restart from a direct
/// imag-obs-start.sh ssh call -- which hung forever on #882's blocking wait -- to a bounded
/// `systemctl --user restart imag-obs.service`; the ordering constraint is unchanged.)
#[test]
fn verify_imag_reads_884_service_state_before_the_840_restart_wipes_it() {
    let body = std::fs::read_to_string(script()).unwrap();
    let service_check = body
        .find("systemctl --user is-enabled imag-obs.service")
        .expect("the imag-obs.service enabled/active check must exist (#884)");
    let restart_call = body
        .find(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#)
        .expect("check (o)'s bounded service-restart call must exist (#890)");
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
fn verify_imag_wires_the_1015_cgroup_check_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.matches("imag_obs_cgroup_shows_service_unit").count() >= 2,
        "verify-imag.sh must both DEFINE and CALL imag_obs_cgroup_shows_service_unit in its live \
         flow (#1015) — a pure function that is only ever defined and never invoked provides \
         zero acceptance coverage"
    );
}

/// Same ordering constraint #884 already established for the enabled/active/Restart checks —
/// this new per-PID cgroup read must ALSO run BEFORE check (o)'s direct restart-proof call, or it
/// would observe the fresh, untracked post-restart process instead of the box's normal boot-time
/// one (#1015, same reasoning as verify_imag_reads_884_service_state_before_the_840_restart_wipes_it).
#[test]
fn verify_imag_reads_1015_cgroup_before_the_840_restart_wipes_it() {
    let body = std::fs::read_to_string(script()).unwrap();
    let cgroup_check = body
        .find("imag_obs_cgroup_shows_service_unit \"")
        .expect("the #1015 cgroup check must actually be CALLED (not just defined)");
    let restart_call = body
        .find(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#)
        .expect("check (o)'s bounded service-restart call must exist (#890)");
    assert!(
        cgroup_check < restart_call,
        "the #1015 cgroup check must run BEFORE check (o)'s restart-proof (#840) -- reading it \
         afterward would observe the fresh, untracked post-restart process instead of the box's \
         normal boot-time state"
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

// ---------------------------------------------------------------------------------------------
// (u) power/thermal envelope (#1040) — sources + wires the SHARED verdict, and runs BEFORE (o)
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_imag_sources_the_shared_power_envelope_lib_1040() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("lib/imag-power-envelope.sh"),
        "verify-imag.sh must SOURCE the shared power-envelope lib (one verdict, never a copy)"
    );
}

#[test]
fn verify_imag_wires_the_1040_power_envelope_check_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    // The shared gather + verdict must both be CALLED in the live flow (a sourced-but-never-called
    // function provides zero acceptance coverage — same discipline as the #884/#1015 wiring tests).
    for needle in [
        "imag_power_envelope_gather_remote_snippet",
        "imag_power_envelope_verdict",
    ] {
        assert!(
            body.matches(needle).count() >= 1,
            "verify-imag.sh must CALL {needle} in its live flow (#1040)"
        );
    }
    // It must also assert TCPU is below the step-down ceiling and the guard tag is readable.
    assert!(
        body.contains("step-down ceiling") && body.contains("imag-power-envelope journald tag"),
        "verify-imag.sh check (u) must assert TCPU below the ceiling AND the guard tag is readable"
    );
}

/// Same ordering constraint #884/#1015 already established: check (u)'s SSH reads must run BEFORE
/// check (o)'s direct restart-proof call, or (u) would observe the fresh, untracked post-restart
/// process/state instead of the box's normal boot-time envelope (#1040, same reasoning as
/// verify_imag_reads_884_service_state_before_the_840_restart_wipes_it).
#[test]
fn verify_imag_reads_1040_power_envelope_before_the_840_restart_wipes_it() {
    let body = std::fs::read_to_string(script()).unwrap();
    let power_check = body
        .find("imag_power_envelope_gather_remote_snippet")
        .expect("the #1040 power-envelope gather must actually be CALLED in the live flow");
    let restart_call = body
        .find(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#)
        .expect("check (o)'s bounded service-restart call must exist (#890)");
    assert!(
        power_check < restart_call,
        "the #1040 power-envelope check (u) must run BEFORE check (o)'s restart-proof (#840) -- \
         reading it afterward would observe the fresh, untracked post-restart process/state \
         instead of the box's normal boot-time envelope"
    );
}

// ---------------------------------------------------------------------------------------------
// (#1058) EVERY ssh read is bounded by a per-class execution timeout, not just check (o).
//
// Issue 890 added the `ssh_box_timeout SECONDS CMD` primitive but scoped it to check (o); every
// other `ssh_box "…"` read stayed unbounded (only `-o ConnectTimeout`, which bounds the connect
// phase, never remote command runtime). A wedged X / stuck remote read would hang that check
// forever. The fix bounds every read BY CONSTRUCTION: `ssh_box` delegates to `ssh_box_timeout`
// with the general read budget, the genuinely-slow reads get an explicit longer budget, and the
// raw `sshpass … ssh` primitive exists in exactly ONE place (the bounded helper). These are
// text-scan guards over the script itself, the same model as the #890 / #884 / #1040 checks above.
// ---------------------------------------------------------------------------------------------

/// The single read helper must be bounded by construction: `ssh_box` delegates to the
/// execution-timeout-wrapped `ssh_box_timeout` with the general read budget (never a raw ssh).
#[test]
fn verify_imag_ssh_box_delegates_to_bounded_helper_1058() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#"ssh_box_timeout "$IMAG_READ_TIMEOUT" "$1""#),
        "verify-imag.sh ssh_box() must delegate to `ssh_box_timeout \"$IMAG_READ_TIMEOUT\" \"$1\"` \
         (#1058) so every ssh_box read is bounded by an execution timeout, not just the connect phase"
    );
}

/// The raw `sshpass … ssh` primitive must appear EXACTLY ONCE — inside the bounded
/// `ssh_box_timeout` helper — so no unbounded raw-ssh read can exist anywhere in the script.
#[test]
fn verify_imag_raw_ssh_primitive_is_bounded_and_singular_1058() {
    let body = std::fs::read_to_string(script()).unwrap();
    let n = body.matches(r#"sshpass -p "$IMAG_PW" ssh"#).count();
    assert_eq!(
        n, 1,
        "verify-imag.sh must have EXACTLY ONE raw `sshpass -p \"$IMAG_PW\" ssh` primitive (inside \
         the execution-timeout-wrapped ssh_box_timeout, #1058) -- found {n}. More than one means a \
         second, UNBOUNDED ssh read path exists (the exact hazard #1058 closes)."
    );
}

/// Both per-class read-budget knobs must be defined with the module's `${VAR:-default}` idiom —
/// a general read budget and a longer slow-read budget (a blanket connect-cap would false-FAIL a
/// healthy box on the legitimately-slower dpkg/apt/journal/gather reads, which the ticket forbids).
#[test]
fn verify_imag_defines_per_class_read_budgets_1058() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#"IMAG_READ_TIMEOUT="${IMAG_READ_TIMEOUT:-"#),
        "verify-imag.sh must define IMAG_READ_TIMEOUT (general read budget, #1058)"
    );
    assert!(
        body.contains(r#"IMAG_SLOW_READ_TIMEOUT="${IMAG_SLOW_READ_TIMEOUT:-"#),
        "verify-imag.sh must define IMAG_SLOW_READ_TIMEOUT (slow-read budget, #1058)"
    );
}

/// The slow-read budget must actually be USED — the genuinely-slow reads (dpkg/apt under a held
/// lock, the dantesync journal, the timesync/power-envelope gathers) are wrapped with the longer
/// explicit budget, proving the design is per-class (not a single blanket cap).
#[test]
fn verify_imag_uses_slow_read_budget_for_slow_reads_1058() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#"ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT""#),
        "verify-imag.sh must wrap its genuinely-slow reads (dpkg/apt/journal/gather) with \
         `ssh_box_timeout \"$IMAG_SLOW_READ_TIMEOUT\"` (#1058) -- per-class budgets, not a blanket cap"
    );
}

// ---------------------------------------------------------------------------------------------
// (v) power-button + lid + sleep protection (#727)
//
// imag-nb is a PRODUCTION box; a short accidental power-button press suspended it during the
// 2026-07-12 live event. setup-imag.sh step 5 persists the logind drop-ins + masks the sleep
// targets; verify-imag.sh must PROVE that protection is EFFECTIVE on the running box, so a
// re-provision that silently lost step 5 FAILS the acceptance gate instead of passing it.
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_powerkey_protection_ok_requires_all_keys_ignored_and_targets_masked_727() {
    // A healthy production imag-nb, with the real distractor lines `loginctl show-seat` also
    // prints (LongPress variants, reboot key) -- every power/suspend/hibernate/lid key ignored,
    // every sleep target masked.
    let good_login = "HandlePowerKey=ignore\nHandlePowerKeyLongPress=ignore\nHandleRebootKey=reboot\nHandleSuspendKey=ignore\nHandleSuspendKeyLongPress=hibernate\nHandleHibernateKey=ignore\nHandleLidSwitch=ignore\nHandleLidSwitchExternalPower=ignore\nHandleLidSwitchDocked=ignore";
    let good_masked = "sleep.target=masked\nsuspend.target=masked\nhibernate.target=masked\nhybrid-sleep.target=masked";
    // Bare HandlePowerKey NOT ignored (poweroff) -- must FAIL even though HandlePowerKeyLongPress
    // IS =ignore (proves whole-line matching, never a HandlePowerKey substring hit).
    let bad_key = "HandlePowerKey=poweroff\nHandlePowerKeyLongPress=ignore\nHandleSuspendKey=ignore\nHandleHibernateKey=ignore\nHandleLidSwitch=ignore";
    // Only the LongPress variant present, no bare HandlePowerKey=ignore line -> FAIL.
    let only_longpress = "HandlePowerKeyLongPress=ignore\nHandleSuspendKey=ignore\nHandleHibernateKey=ignore\nHandleLidSwitch=ignore";
    // A sleep target left unmasked -> FAIL.
    let bad_masked = "sleep.target=masked\nsuspend.target=disabled\nhibernate.target=masked\nhybrid-sleep.target=masked";
    let (code, out, err) = run_sourced(&format!(
        r#"
        GOOD_LOGIN='{good_login}'
        GOOD_MASK='{good_masked}'
        BAD_KEY='{bad_key}'
        ONLY_LP='{only_longpress}'
        BAD_MASK='{bad_masked}'
        if imag_powerkey_protection_ok "$GOOD_LOGIN" "$GOOD_MASK"; then echo YES; else echo NO; fi
        if imag_powerkey_protection_ok "$BAD_KEY" "$GOOD_MASK"; then echo YES; else echo NO; fi
        if imag_powerkey_protection_ok "$ONLY_LP" "$GOOD_MASK"; then echo YES; else echo NO; fi
        if imag_powerkey_protection_ok "$GOOD_LOGIN" "$BAD_MASK"; then echo YES; else echo NO; fi
        "#
    ));
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["YES", "NO", "NO", "NO"],
        "a healthy box PASSES; a non-ignore power key, an only-LongPress dump, or any unmasked \
         sleep target each FAIL (#727): {out:?}"
    );
}

/// The #727 pure check must be DEFINED and CALLED in the live flow (a sourced-but-never-called
/// pure fn provides zero acceptance coverage -- same discipline as the #884/#1040 wiring tests),
/// it must read the EFFECTIVE reloaded logind policy via `loginctl show-seat` (not merely a file
/// on disk), and it must verify the four sleep targets are masked.
#[test]
fn verify_imag_wires_the_727_powerkey_check_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    // Anchor on the CALL form (function name + a quoted argument) -- true ONLY at the invocation
    // site, never at the doc comment (`imag_powerkey_protection_ok LOGINCTL MASKED`) or the
    // definition (`imag_powerkey_protection_ok() {`). A `.count() >= 2` on the bare name would
    // still pass with the call DELETED (doc comment + definition already = 2), defeating this
    // test's own "defined but never called" guard. Precedent: imag_obs_cgroup_shows_service_unit.
    assert!(
        body.contains("imag_powerkey_protection_ok \""),
        "verify-imag.sh must CALL imag_powerkey_protection_ok (with its read args) in its live flow (#727)"
    );
    assert!(
        body.contains("loginctl show-seat"),
        "verify-imag.sh check (v) must read the EFFECTIVE logind key-handling via `loginctl show-seat` (#727)"
    );
    assert!(
        body.contains("hybrid-sleep.target"),
        "verify-imag.sh check (v) must verify sleep/suspend/hibernate/hybrid-sleep targets are masked (#727)"
    );
}

// ---------------------------------------------------------------------------------------------
// #779 — touchpad usability reprovision-durability gate (w). The pure `imag_touchpad_conf_ok`
// classifies the live /etc/X11/xorg.conf.d/30-touchpad-tap.conf content read back over SSH: it
// must ACCEPT the full live InputClass and REJECT a conf missing an option OR carrying the wrong
// scroll-distance VALUE (a reprovision that regenerated a partial/wrong file must FAIL the gate,
// not just an absent one — the issue-840 "check the file the provisioner writes" pairing).
// ---------------------------------------------------------------------------------------------

const GOOD_TOUCHPAD_CONF: &str = r#"Section "InputClass"
    Identifier "touchpad tap-to-click"
    MatchIsTouchpad "on"
    Driver "libinput"
    Option "Tapping" "on"
    Option "TappingDrag" "on"
    Option "NaturalScrolling" "on"
    Option "ScrollPixelDistance" "50"
EndSection"#;

#[test]
fn imag_touchpad_conf_ok_accepts_the_full_live_inputclass_779() {
    let harness = format!(
        "CONF={q}{good}{q}\nimag_touchpad_conf_ok \"$CONF\" && echo ACCEPT || echo REJECT",
        q = "'",
        good = GOOD_TOUCHPAD_CONF
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("ACCEPT"),
        "imag_touchpad_conf_ok must ACCEPT the full live 30-touchpad-tap.conf (#779): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_touchpad_conf_ok_rejects_a_conf_missing_tap_to_click_779() {
    // Same conf with the Tapping option removed — a reprovision that dropped tap-to-click must FAIL.
    let bad = GOOD_TOUCHPAD_CONF.replace("    Option \"Tapping\" \"on\"\n", "");
    let harness = format!(
        "CONF={q}{bad}{q}\nimag_touchpad_conf_ok \"$CONF\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_touchpad_conf_ok must REJECT a conf missing Option Tapping (#779): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_touchpad_conf_ok_rejects_the_wrong_scroll_distance_value_779() {
    // The sensitivity value MATTERS: the user tuned ScrollPixelDistance to 50; a reprovision that
    // regenerated the libinput default (15) or any other value must FAIL, not pass on presence.
    let bad = GOOD_TOUCHPAD_CONF.replace(
        "\"ScrollPixelDistance\" \"50\"",
        "\"ScrollPixelDistance\" \"15\"",
    );
    let harness = format!(
        "CONF={q}{bad}{q}\nimag_touchpad_conf_ok \"$CONF\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_touchpad_conf_ok must REJECT the wrong ScrollPixelDistance value (#779): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_touchpad_conf_ok_rejects_a_conf_missing_the_touchpad_selector_779() {
    // WITHOUT MatchIsTouchpad the InputClass never binds any device, so a file that kept the four
    // Options but dropped the selector is functionally inert -- it must FAIL, not pass on option
    // presence (the function's "PARTIAL file must FAIL" contract covers the selector too).
    let bad = GOOD_TOUCHPAD_CONF.replace("    MatchIsTouchpad \"on\"\n", "");
    let harness = format!(
        "CONF={q}{bad}{q}\nimag_touchpad_conf_ok \"$CONF\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_touchpad_conf_ok must REJECT a conf missing the MatchIsTouchpad selector (#779): out={out:?} err={err:?}"
    );
}

/// #779 — the pure fn is only useful if the live flow CALLS it. Mirror the #884/#1015
/// DEFINE-and-CALL and ordering discipline (this file's own `imag_obs_cgroup_shows_service_unit`
/// and #884 tests) so a future edit cannot delete check (w) with every other test still green.
#[test]
fn verify_imag_wires_the_779_touchpad_check_into_the_live_flow() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.matches("imag_touchpad_conf_ok").count() >= 2,
        "verify-imag.sh must both DEFINE and CALL imag_touchpad_conf_ok (#779) -- a sourced-but-never-called pure fn gives zero acceptance coverage"
    );
    assert!(
        body.contains("cat /etc/X11/xorg.conf.d/30-touchpad-tap.conf"),
        "verify-imag.sh check (w) must read /etc/X11/xorg.conf.d/30-touchpad-tap.conf back over SSH (#779)"
    );
    // Check (w) must run BEFORE check (o)'s OBS restart (#884 ordering) -- a static read is
    // side-effect-free, but the ordering must hold so a future reorder can't hide it post-restart.
    let call = body
        .find("imag_touchpad_conf_ok \"$TOUCHPAD_CONF\"")
        .expect(
            "verify-imag.sh check (w) must CALL imag_touchpad_conf_ok on the ssh-read conf (#779)",
        );
    let restart = body
        .find("ssh_box_timeout \"$IMAG_OBS_RESTART_TIMEOUT\"")
        .expect("check (o)'s bounded OBS restart must exist (#890)");
    assert!(
        call < restart,
        "verify-imag.sh check (w) (#779) must run BEFORE check (o)'s OBS restart (#884 ordering)"
    );
}

// ---------------------------------------------------------------------------------------------
// #791 — imag-maxperf runtime STATE parity (check (y))
// ---------------------------------------------------------------------------------------------
// `imag_maxperf_state_ok STATE_TEXT` returns 0 iff the gathered performance state reads
// performance. STATE_TEXT is labelled KNOB=VALUE lines gathered over SSH:
//   GOVERNOR=<value>          (mandatory — always exists; must be `performance`)
//   EPP=<value|absent>        (optional; if present must be `performance`)
//   NO_TURBO=<value|absent>   (optional; if present must be `0`)
//   PLATFORM_PROFILE=<value|absent>  (optional; if present must be `performance`)
// The optional-knob tolerance keeps the check hardware-agnostic (#816) — a box without
// intel_pstate/platform_profile simply omits those knobs, exactly as imag-maxperf.sh only writes
// the knobs that exist (`[ -f ]` guarded). The governor is the mandatory backbone.

const GOOD_MAXPERF_STATE: &str =
    "GOVERNOR=performance\nEPP=performance\nNO_TURBO=0\nPLATFORM_PROFILE=performance\n";

#[test]
fn imag_maxperf_state_ok_accepts_full_performance_state_791() {
    let harness = format!(
        "S={q}{good}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        good = GOOD_MAXPERF_STATE
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("ACCEPT"),
        "imag_maxperf_state_ok must ACCEPT a full performance state (#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_rejects_powersave_governor_791() {
    let bad = GOOD_MAXPERF_STATE.replace("GOVERNOR=performance", "GOVERNOR=powersave");
    let harness = format!(
        "S={q}{bad}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_maxperf_state_ok must REJECT a powersave governor (#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_rejects_non_performance_epp_791() {
    let bad = GOOD_MAXPERF_STATE.replace("EPP=performance", "EPP=power");
    let harness = format!(
        "S={q}{bad}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_maxperf_state_ok must REJECT a present-but-non-performance EPP (#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_rejects_turbo_disabled_791() {
    // no_turbo=1 means turbo is DISABLED — the opposite of max performance.
    let bad = GOOD_MAXPERF_STATE.replace("NO_TURBO=0", "NO_TURBO=1");
    let harness = format!(
        "S={q}{bad}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        bad = bad
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_maxperf_state_ok must REJECT no_turbo=1 (turbo disabled) (#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_tolerates_absent_optional_knobs_791() {
    // A hardware-agnostic box may lack intel_pstate/platform_profile — governor performance alone,
    // with the optional knobs reported `absent`, must still PASS (the #816 principle).
    let state = "GOVERNOR=performance\nEPP=absent\nNO_TURBO=absent\nPLATFORM_PROFILE=absent\n";
    let harness = format!(
        "S={q}{s}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        s = state
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("ACCEPT"),
        "imag_maxperf_state_ok must TOLERATE absent optional knobs when governor is performance (#816/#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_rejects_missing_governor_791() {
    // The governor line is the mandatory backbone — its absence (an unreadable gather) must FAIL,
    // never silently pass (the #833 measured-zero class).
    let state = "EPP=performance\nNO_TURBO=0\nPLATFORM_PROFILE=performance\n";
    let harness = format!(
        "S={q}{s}{q}\nimag_maxperf_state_ok \"$S\" && echo ACCEPT || echo REJECT",
        q = "'",
        s = state
    );
    let (code, out, err) = run_sourced(&harness);
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("REJECT"),
        "imag_maxperf_state_ok must REJECT a missing GOVERNOR line (unreadable gather, #833/#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_maxperf_state_ok_is_defined_791() {
    // The reject-path tests above print REJECT even when the function is UNDEFINED (a bare
    // `cmd-not-found || echo REJECT`), so they alone do not prove the impl exists. This asserts the
    // function is actually defined in verify-imag.sh — the genuine RED signal for the impl (#791 review).
    let (code, out, err) =
        run_sourced("type imag_maxperf_state_ok >/dev/null 2>&1 && echo DEFINED || echo MISSING");
    assert_eq!(code, 0, "harness/source failed: out={out:?} err={err:?}");
    assert!(
        out.contains("DEFINED"),
        "imag_maxperf_state_ok must be defined in verify-imag.sh (#791): out={out:?} err={err:?}"
    );
}

#[test]
fn imag_powerkey_protection_ok_survives_oversized_loginctl_input_1163() {
    // SIGPIPE-under-pipefail regression (#1163): `printf '%s\n' | grep -q` misgrades a HEALTHY
    // box as unprotected when grep -q exits at the first match before printf's write completes
    // (pipefail turns printf's EPIPE into the pipeline rc, `|| return 1` reads it as "absent").
    // Deterministic repro shape: the matching keys FIRST, then >64KB of distractor lines, so
    // printf must block past the pipe buffer while grep has already matched and exited. The
    // here-string form (no pipe) grades this fixture PASS every time. Also loops the exact
    // 9-line CI fixture 500x — the probabilistic small-input form of the same race (measured
    // 6/5000 false FAILs pre-fix on dev1).
    let (code, out, err) = run_sourced(
        r#"
        GOOD_KEYS='HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore'
        PAD=$(seq 1 20000 | sed 's/^/Distractor=line/')
        BIG="$GOOD_KEYS
$PAD"
        GOOD_MASK='sleep.target=masked
suspend.target=masked
hibernate.target=masked
hybrid-sleep.target=masked'
        if imag_powerkey_protection_ok "$BIG" "$GOOD_MASK"; then echo BIG-PASS; else echo BIG-FAIL; fi
        fails=0
        for _ in $(seq 1 500); do
          imag_powerkey_protection_ok "$GOOD_KEYS" "$GOOD_MASK" || fails=$((fails+1))
        done
        echo "small-fails=$fails"
        "#,
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["BIG-PASS", "small-fails=0"],
        "a healthy box must grade PASS regardless of loginctl dump size or scheduling — \
         no SIGPIPE-under-pipefail false negative (#1163): {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// (u) power/thermal-envelope ACCEPTANCE reclassification (guard-state-aware, #1188)
//
// The SHARED imag_power_envelope_verdict is deliberately guard-BLIND (a pl1 DRIFT on any live !=
// pinned value — correct for drift-guard's strict [0/8] preflight). verify-imag downgrades that to
// OK-with-note ONLY when the guard's own /run state proves a LEGITIMATE thermal step-down. These
// pure functions encode that acceptance-only policy.
// ---------------------------------------------------------------------------------------------

#[test]
fn pl1_guard_reclassify_only_downgrades_a_genuine_stepdown_1188() {
    // Signature: imag_power_pl1_guard_reclassify OBSERVED_UW ENABLED GUARD_STATE STEPDOWN_WATTS
    // stepdown-ok ONLY when guard==stepped AND observed uW == 25W-in-uW AND enabled==1.
    let cases = [
        // legitimate step-down: 25W == 25000000uW, enabled, guard stepped -> stepdown-ok
        ("25000000 1 stepped 25", "stepdown-ok"),
        // guard NOT stepped (foreign 25W write) -> drift (never masked)
        ("25000000 1 not-stepped 25", "drift"),
        // guard state unknown (unreadable state file) -> drift (never mask on uncertainty)
        ("25000000 1 unknown 25", "drift"),
        // stepped but the constraint is DISABLED -> drift (not a normal guard step-down)
        ("25000000 0 stepped 25", "drift"),
        // stepped but the observed value is NOT the step-down value (a wrong/foreign clamp) -> drift
        ("30000000 1 stepped 25", "drift"),
    ];
    for (args, want) in cases {
        let (_c, out, err) = run_sourced(&format!("imag_power_pl1_guard_reclassify {args}"));
        assert_eq!(
            out.trim(),
            want,
            "imag_power_pl1_guard_reclassify {args} -> want {want:?}: out={out:?} err={err:?}"
        );
    }
}

#[test]
fn tcpu_guard_verdict_is_stepdown_aware_at_the_ceiling_1188() {
    // Signature: imag_power_tcpu_guard_verdict TCPU CEIL GUARD_STATE
    let cases = [
        ("92 93 stepped", "ok"), // below ceiling -> ok regardless of guard
        ("92 93 not-stepped", "ok"),
        ("93 93 stepped", "ok-stepdown"), // at ceiling + guard stepped -> the #1162 steady state
        ("95 93 stepped", "ok-stepdown"),
        ("93 93 not-stepped", "over-ceiling"), // at ceiling, guard NOT stepped -> live clamp, FAIL
        ("93 93 unknown", "over-ceiling"),     // unknown guard -> FAIL (never mask)
        ("'' 93 stepped", "unreadable"),       // empty TCPU -> unreadable (existing FAIL path)
        ("abc 93 stepped", "unreadable"),      // non-numeric -> unreadable
    ];
    for (args, want) in cases {
        let (_c, out, err) = run_sourced(&format!("imag_power_tcpu_guard_verdict {args}"));
        assert_eq!(
            out.trim(),
            want,
            "imag_power_tcpu_guard_verdict {args} -> want {want:?}: out={out:?} err={err:?}"
        );
    }
}

#[test]
fn verify_imag_wires_the_1188_guard_state_awareness_into_check_u() {
    // The pure fns are only useful if check (u) actually READS the guard state file and CALLS the
    // reclassify/tcpu-verdict fns — a defined-but-uncalled fn provides zero acceptance coverage
    // (same discipline as the #884/#1015/#1040 wiring tests).
    let body = std::fs::read_to_string(script()).unwrap();
    for needle in [
        "imag_power_guard_stepped_from_state",
        "imag_power_guard_stepdown_w_from_state",
        "imag_power_pl1_guard_reclassify",
        "imag_power_tcpu_guard_verdict",
        "IMAG_POWER_GUARD_STATE_FILE",
    ] {
        assert!(
            body.contains(needle),
            "verify-imag.sh check (u) must reference {needle} to become guard-state-aware (#1188)"
        );
    }
    // It must READ the guard's /run state over SSH (a `cat` of the state path).
    assert!(
        body.contains("imag-power-envelope-guard.state") || body.contains("IMAG_POWER_GUARD_STATE"),
        "verify-imag.sh check (u) must read the guard's /run state file (#1188)"
    );
    // The legitimate-step-down OK path must be a LOUD note, not a silent pass.
    assert!(
        body.contains("guard thermal step-down active"),
        "the reclassified pl1 OK path must carry a LOUD 'guard thermal step-down active' note (#1188)"
    );
}

/// The #1188 guard-state reads (the state-file cat) must run BEFORE check (o)'s OBS restart —
/// same #884/#1015/#1040 ordering constraint (check (u) already lives above (o); the new reads sit
/// inside it, so this just re-pins that the whole power block precedes the restart).
#[test]
fn verify_imag_reads_1188_guard_state_before_the_840_restart_wipes_it() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard_read = body
        .find("imag_power_guard_stepped_from_state")
        .expect("check (u) must read the guard state (#1188)");
    // Anchor the restart on the actual CALL literal — the exact string the sibling #884/#1015/#1040
    // ordering tests use. (Do NOT anchor on `imag_obs_service_restart_cmd`: THAT fn name first
    // appears at its DEFINITION far above check (u), so `.find` would resolve above the guard read.
    // `imag_power_guard_stepped_from_state` above is safe — it is defined in the lib and appears in
    // verify-imag.sh exactly once, as the check-(u) call.)
    let restart_call = body
        .find(r#"ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT""#)
        .expect("check (o)'s restart must exist");
    assert!(
        guard_read < restart_call,
        "the #1188 guard-state read must run BEFORE check (o)'s restart-proof (#840 ordering)"
    );
}
