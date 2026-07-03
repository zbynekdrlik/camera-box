//! Behavioral guard for the canary-first NDI Linux runtime upgrade tool
//! `scripts/upgrade-fleet-ndi.sh` (#132).
//!
//! ## Why this script exists (#132 — fleet not uniform/newest)
//!
//! The cameras (cam1-4) run NDI Linux runtime **6.2.1.0**; the production OBS boxes strih +
//! stream already run **6.3.2.0** (live-validated, see the #132 issue thread). Cross-version NDI
//! interop works today, but the initial requirement was "newest NDI everywhere" — so the cams
//! need `libndi.so.6` bumped to match. The NDI v6 Linux SDK/runtime is a public, unauthenticated
//! download (`vendor/distroav/CI/libndi-get.sh` curls `downloads.ndi.tv` directly) — NOT a
//! license-gated blocker — so the actual gap is a SAFE fleet-deploy tool, not a missing asset.
//!
//! This is genuinely risky WORK (swap a shared-library dependency under a running service, on
//! embedded appliance hardware with no easy physical recovery) — a bad swap could leave a
//! camera emitting no NDI at all. So the tool is canary-first (prove ONE camera survives the
//! swap + still emits NDI before touching the rest), always backs up the previous runtime
//! before repointing symlinks (never deletes it), and rolls a camera back automatically on any
//! verification failure.
//!
//! Same PURE-PLANNER model as tests/rig_mode.rs / tests/av_stack_update.rs: these tests source
//! the REAL script (its `BASH_SOURCE != $0` guard skips the network/ssh-mutating main flow) and
//! exercise its pure functions — version parsing, version ordering, canary selection, and the
//! exact remote command text — directly. NO test here ever ssh's a real camera.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/upgrade-fleet-ndi.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script (BASH_SOURCE!=$0 guard skips the network/mutating main flow) and run
/// `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source + run `body` WITHOUT asserting success — for pure functions that intentionally return
/// non-zero (e.g. resolve_canary on an override not in the set).
fn run_sourced_status(body: &str) -> (i32, String, String) {
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

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run upgrade-fleet-ndi.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The script must be SOURCE-SAFE — sourcing it (the unit-test harness) must not run the
/// network/ssh-mutating main flow.
#[test]
fn script_is_source_safe() {
    let out = run_sourced("echo SOURCED_OK");
    assert!(
        out.contains("SOURCED_OK"),
        "#132: the script must be source-safe (BASH_SOURCE != $0 guard) — sourcing ran main"
    );
}

/// `--help` prints usage and exits 0 — never touches ssh/sshpass.
#[test]
fn help_exits_zero_and_documents_so_path() {
    let (code, out, _) = run_script(&["--help"]);
    assert_eq!(code, 0, "#132: --help must exit 0");
    assert!(
        out.contains("upgrade-fleet-ndi.sh") && out.contains("--so-path"),
        "#132: --help must document the script name and --so-path. Got:\n{out}"
    );
}

/// Missing --so-path is a clean usage error — must fail BEFORE any ssh/sshpass check.
#[test]
fn missing_so_path_is_a_clean_usage_error() {
    let (code, _out, err) = run_script(&[]);
    assert_eq!(
        code, 1,
        "#132: a missing --so-path must exit 1 (usage/env error)"
    );
    assert!(
        err.contains("--so-path"),
        "#132: the usage error must mention --so-path. Got stderr: {err:?}"
    );
}

/// A --so-path pointing at a nonexistent file is a clean usage error (never proceeds to ssh).
#[test]
fn nonexistent_so_path_is_a_clean_usage_error() {
    let (code, _out, err) = run_script(&["--so-path", "/nonexistent/libndi.so.6.3.2"]);
    assert_eq!(code, 1, "#132: a nonexistent --so-path must exit 1");
    assert!(
        err.contains("not found"),
        "#132: the error must say the file was not found. Got stderr: {err:?}"
    );
}

/// An unrecognised extra argument is a usage error (exit 2), distinct from the exit-1 env errors.
#[test]
fn unknown_argument_is_usage_error_exit_2() {
    let (code, _out, err) = run_script(&["--bogus-flag"]);
    assert_eq!(code, 2, "#132: an unknown argument must exit 2");
    assert!(err.contains("unknown arg"), "got stderr: {err:?}");
}

/// ndi_version_from_strings_output extracts the trailing X.Y.Z.W token from the exact
/// `strings libndi.so.6 | grep 'NDI SDK LINUX'` banner text (live-verified format on cam2:
/// "NDI SDK LINUX 10:24:11 Aug 21 2025 6.2.1.0"; on dev1's newer copy:
/// "NDI SDK LINUX 12:51:52 Apr 13 2026 6.3.2.0").
#[test]
fn version_from_strings_output_extracts_trailing_semver() {
    let out =
        run_sourced("ndi_version_from_strings_output 'NDI SDK LINUX 12:51:52 Apr 13 2026 6.3.2.0'");
    assert_eq!(
        out.trim(),
        "6.3.2.0",
        "#132: expected the trailing dotted-quad. Got:\n{out}"
    );

    let out2 =
        run_sourced("ndi_version_from_strings_output 'NDI SDK LINUX 10:24:11 Aug 21 2025 6.2.1.0'");
    assert_eq!(out2.trim(), "6.2.1.0", "Got:\n{out2}");
}

/// An empty/garbage banner (a failed `strings|grep`, or a non-NDI file) must yield an EMPTY
/// string, never a guess — callers gate on emptiness to refuse an unverified blob.
#[test]
fn version_from_strings_output_empty_on_no_match() {
    let out = run_sourced("ndi_version_from_strings_output ''");
    assert_eq!(
        out.trim(),
        "",
        "#132: no banner -> empty version, never a guess"
    );

    let out2 = run_sourced("ndi_version_from_strings_output 'some unrelated garbage text'");
    assert_eq!(
        out2.trim(),
        "",
        "#132: non-NDI text -> empty version. Got:\n{out2}"
    );
}

/// ndi_version_status orders dotted-quad versions correctly (NEWER/SAME/OLDER), and reports
/// UNKNOWN — never a guessed ordering — when either side is empty (a failed version read).
#[test]
fn version_status_orders_dotted_quads_and_flags_unknown() {
    assert_eq!(
        run_sourced("ndi_version_status 6.2.1.0 6.3.2.0").trim(),
        "NEWER",
        "#132: 6.3.2.0 is newer than 6.2.1.0"
    );
    assert_eq!(
        run_sourced("ndi_version_status 6.3.2.0 6.3.2.0").trim(),
        "SAME"
    );
    assert_eq!(
        run_sourced("ndi_version_status 6.3.2.0 6.2.1.0").trim(),
        "OLDER",
        "#132: refusing a downgrade must be detectable"
    );
    assert_eq!(
        run_sourced("ndi_version_status '' 6.3.2.0").trim(),
        "UNKNOWN",
        "#132: an unreadable CURRENT version must never be treated as any ordering"
    );
    assert_eq!(
        run_sourced("ndi_version_status 6.2.1.0 ''").trim(),
        "UNKNOWN",
        "#132: an unreadable CANDIDATE version must never be treated as any ordering"
    );
}

/// resolve_canary defaults to the FIRST camera in the set when no override is given.
#[test]
fn resolve_canary_defaults_to_first_in_set() {
    let out = run_sourced("resolve_canary 'cam1 cam2 cam3 cam4' ''");
    assert_eq!(
        out.trim(),
        "cam1",
        "#132: default canary must be the first cam in the set"
    );
}

/// An explicit --canary override is honoured when it IS a member of the set.
#[test]
fn resolve_canary_honors_override_when_member_of_set() {
    let out = run_sourced("resolve_canary 'cam1 cam2 cam3 cam4' cam3");
    assert_eq!(out.trim(), "cam3");
}

/// An override that is NOT a member of the set is REJECTED (never silently substituted) —
/// the fleet upgrade must never guess which camera the operator meant.
#[test]
fn resolve_canary_rejects_override_not_in_set() {
    let (code, out, err) = run_sourced_status("resolve_canary 'cam1 cam2 cam3 cam4' cam9");
    assert_ne!(
        code, 0,
        "#132: an unknown canary override must fail, not fall back silently"
    );
    assert!(
        out.trim().is_empty(),
        "#132: no camera name printed on failure. Got:\n{out}"
    );
    assert!(
        err.contains("cam9") && err.contains("not a member"),
        "#132: the error must name the bad override. Got stderr: {err:?}"
    );
}

/// remaining_after_canary returns SET minus CANARY, preserving order.
#[test]
fn remaining_after_canary_excludes_canary_preserves_order() {
    let out = run_sourced("remaining_after_canary 'cam1 cam2 cam3 cam4' cam3");
    assert_eq!(out.trim(), "cam1 cam2 cam4", "Got:\n{out}");
}

/// ndi_swap_remote's generated remote command: backs up the CURRENTLY ACTIVE runtime (by its
/// real, symlink-resolved path) BEFORE repointing anything, re-points BOTH `libndi.so` and
/// `libndi.so.6` symlinks at the new basename, ldconfig's, prints the old basename (so the
/// caller can roll back), and restarts camera-box — in that order. It must NEVER delete the
/// previous runtime file (the whole point of the backup — recoverable rollback).
#[test]
fn ndi_swap_remote_backs_up_before_repointing_never_deletes_old_runtime() {
    let p = run_sourced("ndi_swap_remote /usr/lib/ndi libndi.so.6.3.2");
    assert!(
        p.contains("readlink -f") && p.contains("/usr/lib/ndi/libndi.so.6"),
        "#132: must resolve the CURRENTLY active runtime via readlink -f. Got:\n{p}"
    );
    let backup_pos = p
        .find("cp -a")
        .expect("#132: expected a backup copy (cp -a) of the old runtime");
    assert!(
        p.contains("ln -sf libndi.so.6.3.2 \"$dest/libndi.so.6\"")
            || p.contains("ln -sf libndi.so.6.3.2 \"/usr/lib/ndi/libndi.so.6\""),
        "#132: must re-point libndi.so.6 at the new basename. Got:\n{p}"
    );
    assert!(
        p.contains("ln -sf libndi.so.6.3.2 \"$dest/libndi.so\"")
            || p.contains("ln -sf libndi.so.6.3.2 \"/usr/lib/ndi/libndi.so\""),
        "#132: must re-point the bare libndi.so symlink too. Got:\n{p}"
    );
    let relink_pos = p
        .find("ln -sf libndi.so.6.3.2")
        .expect("#132: expected the re-point commands");
    assert!(
        backup_pos < relink_pos,
        "#132: must back up the OLD runtime BEFORE re-pointing symlinks. Got:\n{p}"
    );
    assert!(
        p.contains("ldconfig"),
        "#132: must run ldconfig after repointing. Got:\n{p}"
    );
    assert!(
        p.contains("OLD_BASE="),
        "#132: must print the previous runtime's basename so the caller can roll back. Got:\n{p}"
    );
    let old_base_pos = p.find("OLD_BASE=").unwrap();
    let restart_pos = p
        .find("systemctl restart camera-box")
        .expect("#132: expected a camera-box restart to load the new runtime");
    assert!(
        old_base_pos < restart_pos,
        "#132: OLD_BASE must be captured/printed BEFORE the restart. Got:\n{p}"
    );
    // Never delete the previous runtime file — that is the whole rollback safety net.
    assert!(
        !p.contains("rm -f \"$OLD_REAL\"") && !p.contains("rm \"$OLD_REAL\""),
        "#132: must NEVER delete the previous runtime — rollback depends on it still existing. \
         Got:\n{p}"
    );
}

/// ndi_rollback_remote re-points both symlinks back at the pre-swap basename (which was never
/// deleted by ndi_swap_remote) and restarts camera-box — the exact inverse of the swap.
#[test]
fn ndi_rollback_remote_repoints_symlinks_back_and_restarts() {
    let p = run_sourced("ndi_rollback_remote /usr/lib/ndi libndi.so.6.2.1");
    assert!(
        p.contains("ln -sf libndi.so.6.2.1 \"$dest/libndi.so.6\"")
            || p.contains("ln -sf libndi.so.6.2.1 \"/usr/lib/ndi/libndi.so.6\""),
        "#132: rollback must re-point libndi.so.6 at the OLD basename. Got:\n{p}"
    );
    assert!(
        p.contains("ln -sf libndi.so.6.2.1 \"$dest/libndi.so\"")
            || p.contains("ln -sf libndi.so.6.2.1 \"/usr/lib/ndi/libndi.so\""),
        "#132: rollback must re-point libndi.so too. Got:\n{p}"
    );
    assert!(p.contains("ldconfig"), "Got:\n{p}");
    assert!(
        p.contains("systemctl restart camera-box"),
        "#132: rollback must restart camera-box to load the restored runtime. Got:\n{p}"
    );
}

/// ndi_active_version_remote reads the version via the SAME symlink-resolution + strings/grep
/// technique used to read the candidate file locally — so "current" and "candidate" versions
/// are always compared apples-to-apples.
#[test]
fn ndi_active_version_remote_resolves_symlink_then_reads_banner() {
    let p = run_sourced("ndi_active_version_remote /usr/lib/ndi");
    assert!(
        p.contains("readlink -f") && p.contains("/usr/lib/ndi/libndi.so.6"),
        "Got:\n{p}"
    );
    assert!(
        p.contains("strings") && p.contains("NDI SDK LINUX"),
        "#132: must grep the exact 'NDI SDK LINUX' banner. Got:\n{p}"
    );
}

/// The post-swap emit check must key on the EXACT same genlock-report signal deploy-fleet.sh
/// already uses ('fps emitted .* fps captured') — one established "NDI is alive" signal across
/// the fleet tooling, not a second, possibly-inconsistent one.
#[test]
fn emit_ok_grep_pattern_matches_deploy_fleet_signal() {
    let out = run_sourced("emit_ok_grep_pattern");
    assert_eq!(out.trim(), "fps emitted .* fps captured");
}

/// The post-swap crash scan must key on the EXACT same signatures deploy-fleet.sh uses.
#[test]
fn fatal_grep_pattern_matches_deploy_fleet_signatures() {
    let out = run_sourced("fatal_grep_pattern");
    assert_eq!(
        out.trim(),
        "panic|thread '.*' panicked|SIGSEGV|SIGABRT|core dumped|FATAL"
    );
}

/// Static ordering guard: the main flow must upgrade the CANARY strictly before iterating the
/// REMAINING cameras — the whole point of "canary first, never touch the rest on failure".
#[test]
fn canary_is_upgraded_before_the_remaining_fleet() {
    let s = fs::read_to_string(script()).expect("read upgrade-fleet-ndi.sh");
    let canary_pos = s
        .find("upgrade_one_camera \"$CANARY\"")
        .expect("#132: expected an explicit canary-first upgrade call");
    let rest_loop_pos = s
        .find("for cam in $REST")
        .expect("#132: expected a loop over the remaining (non-canary) cameras");
    assert!(
        canary_pos < rest_loop_pos,
        "#132: the canary MUST be upgraded before the loop over the rest of the fleet"
    );
}

/// The script must be idempotent-safe: a candidate that is already the ACTIVE version is a
/// no-op (never re-swaps/restarts a camera that is already on the target version).
#[test]
fn script_source_treats_same_version_as_noop() {
    let s = fs::read_to_string(script()).expect("read upgrade-fleet-ndi.sh");
    assert!(
        s.contains("SAME") && s.contains("nothing to do"),
        "#132: the SAME-version case must be a documented no-op, not a needless re-swap"
    );
}
