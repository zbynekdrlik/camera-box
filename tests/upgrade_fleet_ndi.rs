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
use std::os::unix::fs::PermissionsExt;
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
/// non-zero (e.g. resolve_canary_set on an override not in the set).
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

/// #452: ndi_camera_class distinguishes the cam3-class box (real-file layout, no `strings`,
/// older log shape — #445's findings) from every other camera's "standard" symlink layout. This
/// is a STATIC, KNOWN fleet fact resolve_canary_set() uses to pick class representatives —
/// never probed live over ssh.
#[test]
fn ndi_camera_class_distinguishes_cam3_from_standard_layout() {
    assert_eq!(
        run_sourced("ndi_camera_class cam3").trim(),
        "cam3class",
        "#452: cam3 must be classified distinctly from the standard symlink-layout cameras"
    );
    // #593: cam7 excluded -- it was never built and is not part of the active fleet.
    for cam in ["cam1", "cam2", "cam4", "cam5", "cam6"] {
        assert_eq!(
            run_sourced(&format!("ndi_camera_class {cam}")).trim(),
            "standard",
            "#452: {cam} must be classified as the standard symlink layout"
        );
    }
}

/// resolve_canary_set defaults to a single canary (the first camera in the set) when every
/// member of SET shares the same ndi_camera_class — unchanged from the original #132
/// single-canary behavior for a homogeneous fleet/subset.
#[test]
fn resolve_canary_set_defaults_to_first_when_set_is_single_class() {
    let out = run_sourced("resolve_canary_set 'cam1 cam2 cam4' ''");
    assert_eq!(
        out.trim(),
        "cam1",
        "#452: a single-class SET must still default to exactly one canary (the first member)"
    );
}

/// #452: the canary blind spot — a green canary on a symlink-layout box (e.g. cam1) proves
/// nothing about a real-file/no-strings box (cam3-class, the #132 history). When SET contains
/// more than one distinct ndi_camera_class, the DEFAULT canary set must include one
/// representative of EACH class present (first SET member of each newly-seen class), so a
/// full-fleet apply is never gated on a canary that only proved the majority class.
#[test]
fn resolve_canary_set_default_covers_every_distinct_class_present() {
    let out = run_sourced("resolve_canary_set 'cam1 cam2 cam3 cam4' ''");
    assert_eq!(
        out.trim(),
        "cam1 cam3",
        "#452: the default canary set must cover BOTH the standard class (cam1) and the \
         cam3-class (cam3) present in the set. Got:\n{out}"
    );
}

/// An explicit --canary override is honoured verbatim (space-separated, one or more names) when
/// every member IS in the set — the operator's explicit choice always wins over the
/// class-coverage default.
#[test]
fn resolve_canary_set_honors_multi_value_override_when_all_members_of_set() {
    let out = run_sourced("resolve_canary_set 'cam1 cam2 cam3 cam4' 'cam2 cam3'");
    assert_eq!(out.trim(), "cam2 cam3");

    let single = run_sourced("resolve_canary_set 'cam1 cam2 cam3 cam4' cam3");
    assert_eq!(single.trim(), "cam3");
}

/// An override containing a camera that is NOT a member of the set is REJECTED entirely (never
/// silently drops just the bad one) — the fleet upgrade must never guess which camera the
/// operator meant.
#[test]
fn resolve_canary_set_rejects_override_member_not_in_set() {
    let (code, out, err) = run_sourced_status("resolve_canary_set 'cam1 cam2 cam3 cam4' cam9");
    assert_ne!(
        code, 0,
        "#452: an unknown canary override must fail, not fall back silently"
    );
    assert!(
        out.trim().is_empty(),
        "#452: no camera names printed on failure. Got:\n{out}"
    );
    assert!(
        err.contains("cam9") && err.contains("not a member"),
        "#452: the error must name the bad override. Got stderr: {err:?}"
    );

    let (code2, _out2, err2) =
        run_sourced_status("resolve_canary_set 'cam1 cam2 cam3 cam4' 'cam2 cam9'");
    assert_ne!(
        code2, 0,
        "#452: ANY unknown member in a multi-value override must fail the whole override"
    );
    assert!(
        err2.contains("cam9") && err2.contains("not a member"),
        "#452: the error must name the specific bad override. Got stderr: {err2:?}"
    );
}

/// remaining_after_canary returns SET minus CANARY, preserving order — single-value canary
/// (back-compat with the original #132 shape).
#[test]
fn remaining_after_canary_excludes_canary_preserves_order() {
    let out = run_sourced("remaining_after_canary 'cam1 cam2 cam3 cam4' cam3");
    assert_eq!(out.trim(), "cam1 cam2 cam4", "Got:\n{out}");
}

/// #452: remaining_after_canary must also accept a multi-member canary SET (space-separated) —
/// resolve_canary_set() can now return more than one canary when the fleet spans multiple
/// ndi_camera_class values, and the fleet loop must exclude ALL of them, not just the first.
#[test]
fn remaining_after_canary_excludes_multiple_canaries_preserves_order() {
    let out = run_sourced("remaining_after_canary 'cam1 cam2 cam3 cam4' 'cam1 cam3'");
    assert_eq!(out.trim(), "cam2 cam4", "Got:\n{out}");
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

/// #451: emit_ok_grep_pattern/fatal_grep_pattern must come from the ONE shared
/// scripts/lib/ndi-alive.sh, not be defined locally here any more — so deploy-fleet.sh can
/// source the exact same signal instead of keeping its own copy that silently drifts (the
/// #445 broadening was applied here only, leaving deploy-fleet.sh's narrower copy behind).
#[test]
fn sources_shared_ndi_alive_lib() {
    let s = fs::read_to_string(script()).expect("read upgrade-fleet-ndi.sh");
    assert!(
        s.contains("lib/ndi-alive.sh"),
        "#451: upgrade-fleet-ndi.sh must source scripts/lib/ndi-alive.sh instead of defining \
         emit_ok_grep_pattern/fatal_grep_pattern locally."
    );
    assert!(
        !s.contains("emit_ok_grep_pattern() {"),
        "#451: emit_ok_grep_pattern must no longer be DEFINED in upgrade-fleet-ndi.sh — it \
         must come from the shared scripts/lib/ndi-alive.sh."
    );
    assert!(
        !s.contains("fatal_grep_pattern() {"),
        "#451: fatal_grep_pattern must no longer be DEFINED in upgrade-fleet-ndi.sh — it must \
         come from the shared scripts/lib/ndi-alive.sh."
    );
}

/// The post-swap emit check must key on the deploy-fleet.sh genlock-report signal
/// ('fps emitted .* fps captured') BROADENED to also accept the older per-camera log shape
/// ("Streaming: X.Y fps") and the sender-ready line — #445: cam3 runs an older camera-box build
/// that logs "Streaming: 60.0 fps" instead of the genlock report, and the narrow pattern
/// false-verify-failed a perfectly-good upgrade, triggering an automatic rollback.
#[test]
fn emit_ok_grep_pattern_matches_deploy_fleet_signal() {
    let out = run_sourced("emit_ok_grep_pattern");
    assert_eq!(
        out.trim(),
        "fps emitted .* fps captured|Streaming: [0-9.]+ fps|NDI sender ready"
    );
}

/// #445: the broadened pattern must ACTUALLY match cam3's older log line via a real `grep -E`
/// invocation — not just contain the substring in the exact-value test above.
#[test]
fn emit_ok_grep_pattern_also_matches_older_streaming_fps_log_line() {
    let pattern = run_sourced("emit_ok_grep_pattern").trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'Streaming: 60.0 fps' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "#445: emit_ok_grep_pattern must match cam3's older 'Streaming: 60.0 fps' log line so a \
         good upgrade is not false-verify-failed. Pattern: {pattern}"
    );
}

/// #445: the broadened pattern must still match the original genlock report line — broadening
/// must never regress the signal deploy-fleet.sh already relies on.
#[test]
fn emit_ok_grep_pattern_still_matches_original_genlock_report_line() {
    let pattern = run_sourced("emit_ok_grep_pattern").trim().to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'genlock: 60.02 fps emitted 60.01 fps captured' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "#445: broadening emit_ok_grep_pattern must not regress the original genlock signal. \
         Pattern: {pattern}"
    );
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

/// Static ordering guard: the main flow must upgrade every member of the CANARY SET strictly
/// before iterating the REMAINING cameras — the whole point of "canary(-set) first, never touch
/// the rest on failure". #452: the canary is now a SET (one per distinct box-class), not a
/// single camera, so this guards the loop-over-$CANARY_SET shape instead of a single direct call.
#[test]
fn canary_is_upgraded_before_the_remaining_fleet() {
    let s = fs::read_to_string(script()).expect("read upgrade-fleet-ndi.sh");
    let canary_loop_pos = s
        .find("for cam in $CANARY_SET")
        .expect("#452: expected a loop over the (possibly multi-member) canary set");
    let rest_loop_pos = s
        .find("for cam in $REST")
        .expect("#132: expected a loop over the remaining (non-canary) cameras");
    assert!(
        canary_loop_pos < rest_loop_pos,
        "#132/#452: the whole canary SET MUST be upgraded before the loop over the rest of the \
         fleet"
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

// --- #445: cam3 robustness gaps -------------------------------------------------------------
//
// Found while running the #132 tool live: cam3 (a) has no `strings` binary, causing an "unknown
// baseline" refusal; (b) runs an older camera-box build whose log line the emit-OK pattern above
// already covers; and (c) ships `libndi.so.6` + `libndi.so` as REAL FILES, not the symlink layout
// every other camera uses. These tests cover (a) and (c).

/// ndi_read_banner_local must extract the "NDI SDK LINUX ... X.Y.Z.W" banner via `strings` when
/// it is available (the historical, still-default path on cam1/2/4).
#[test]
fn ndi_read_banner_local_uses_strings_when_available() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let so_path = tmp.path().join("libndi.so.6.2.1.0");
    fs::write(
        &so_path,
        b"garbage\x00\x01NDI SDK LINUX 10:24:11 Aug 21 2025 6.2.1.0\x00\x02moregarbage",
    )
    .expect("write fake .so");

    let out = run_sourced(&format!("ndi_read_banner_local {}", so_path.display()));
    assert!(
        out.contains("NDI SDK LINUX") && out.contains("6.2.1.0"),
        "#445: expected the banner to be read via `strings`. Got:\n{out}"
    );
}

/// ndi_read_banner_local must fall back to `grep -a` and still extract the version when
/// `strings` is unavailable — confirmed necessary on cam3, which has no `strings` binary.
#[test]
fn ndi_read_banner_local_falls_back_to_grep_a_when_strings_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).expect("mkdir fake bin");
    let fake_strings = fake_bin.join("strings");
    fs::write(&fake_strings, "#!/bin/sh\nexit 127\n").expect("write fake strings");
    let mut perms = fs::metadata(&fake_strings)
        .expect("stat fake strings")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_strings, perms).expect("chmod fake strings");

    let so_path = tmp.path().join("libndi.so.6.2.1.0");
    fs::write(
        &so_path,
        b"garbage\x00\x01NDI SDK LINUX 10:24:11 Aug 21 2025 6.2.1.0\x00\x02moregarbage",
    )
    .expect("write fake .so");

    // Shadow PATH with the fake (always-failing) `strings` ahead of the real one, so
    // ndi_read_banner_local cannot rely on the real `strings` binary being present or absent on
    // whatever machine runs this test — it must exercise the fallback deterministically.
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nPATH=\"{}:$PATH\"\nndi_read_banner_local {}",
        fake_bin.display(),
        so_path.display()
    );
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("6.2.1.0"),
        "#445: with `strings` unavailable, ndi_read_banner_local must fall back to `grep -a` and \
         still extract the version. Got stdout:\n{stdout}"
    );

    // The extracted banner must still feed cleanly into ndi_version_from_strings_output.
    let ver_harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nPATH=\"{}:$PATH\"\nbanner=\"$(ndi_read_banner_local {})\"\nndi_version_from_strings_output \"$banner\"",
        fake_bin.display(),
        so_path.display()
    );
    let ver_out = Command::new("bash")
        .arg("-c")
        .arg(&ver_harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert_eq!(
        String::from_utf8_lossy(&ver_out.stdout).trim(),
        "6.2.1.0",
        "#445: the grep -a fallback banner must still parse to the bare version"
    );
}

/// ndi_read_banner_local yields empty ("") on a file with no NDI banner at all — never a guess,
/// under EITHER the strings path or the grep -a fallback.
#[test]
fn ndi_read_banner_local_empty_on_no_banner() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let so_path = tmp.path().join("not_an_ndi_lib.so");
    fs::write(&so_path, b"totally unrelated binary content\x00\x01\x02")
        .expect("write unrelated file");

    let out = run_sourced(&format!("ndi_read_banner_local {}", so_path.display()));
    assert!(
        out.trim().is_empty(),
        "#445: a file with no NDI banner must yield empty, not a guess. Got:\n{out}"
    );
}

/// ndi_link_kind_remote's generated remote check must correctly distinguish the symlink layout
/// (cam1/2/4: `libndi.so.6` -> `libndi.so.X.Y.Z.W`) from the real-file layout (cam3: `libndi.so.6`
/// is a regular file) and a missing runtime — executed here against real local temp files/links,
/// since the underlying `[ -L ... ]` / `[ -f ... ]` test is identical whether run locally or (as
/// it will be in production) over ssh on a camera.
#[test]
fn ndi_link_kind_remote_detects_symlink_vs_regular_vs_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    let link_path = dest.join("libndi.so.6");

    // missing: neither a symlink nor a regular file exists yet.
    let missing_out = run_sourced(&format!(
        "cmd=\"$(ndi_link_kind_remote {})\"\nbash -c \"$cmd\"",
        dest.display()
    ));
    assert_eq!(
        missing_out.trim(),
        "missing",
        "#445: no libndi.so.6 at all must report 'missing'"
    );

    // symlink layout (cam1/2/4).
    let target = dest.join("libndi.so.6.3.2.0");
    fs::write(&target, b"fake-so").expect("write fake target");
    std::os::unix::fs::symlink(&target, &link_path).expect("create symlink");
    let symlink_out = run_sourced(&format!(
        "cmd=\"$(ndi_link_kind_remote {})\"\nbash -c \"$cmd\"",
        dest.display()
    ));
    assert_eq!(
        symlink_out.trim(),
        "symlink",
        "#445: libndi.so.6 as a symlink must report 'symlink'"
    );
    fs::remove_file(&link_path).expect("remove symlink");

    // real-file layout (cam3, #445).
    fs::write(&link_path, b"fake-real-so").expect("write real file");
    let regular_out = run_sourced(&format!(
        "cmd=\"$(ndi_link_kind_remote {})\"\nbash -c \"$cmd\"",
        dest.display()
    ));
    assert_eq!(
        regular_out.trim(),
        "regular",
        "#445: libndi.so.6 as a REAL FILE (cam3's layout) must report 'regular', not fail or \
         misidentify it as a symlink"
    );
}

/// ndi_swap_remote given the "regular" link kind (cam3, #445) must back up the currently active
/// `libndi.so.6` file itself, then OVERWRITE (copy, not symlink) both `libndi.so.6` and
/// `libndi.so` with the new candidate content — the symlink repoint used for every other camera
/// does not apply when the runtime file is a real file, not a symlink.
///
/// #452: the backup name must be VERSION-SCOPED (`libndi.so.6.<old_version>.bak`), not the fixed
/// `libndi.so.6.bak` every prior upgrade overwrote — a cam3-class box needs the same
/// multi-generation rollback depth the symlink layout already gets for free (each symlink
/// backup's basename is the original versioned filename).
#[test]
fn ndi_swap_remote_regular_layout_backs_up_and_copies_both_names() {
    let p = run_sourced("ndi_swap_remote /usr/lib/ndi libndi.so.6.3.2 regular 6.2.1.0");
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6\" \"/usr/lib/ndi/libndi.so.6.6.2.1.0.bak\""),
        "#452: the regular-file layout must back up the active libndi.so.6 to a \
         VERSION-SCOPED .bak file (named after the OLD version) BEFORE overwriting it. Got:\n{p}"
    );
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.3.2\" \"/usr/lib/ndi/libndi.so.6\""),
        "#445: must COPY the new candidate content over libndi.so.6 (never a symlink) when the \
         layout is a real file. Got:\n{p}"
    );
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.3.2\" \"/usr/lib/ndi/libndi.so\""),
        "#445: must COPY the new candidate content over libndi.so too. Got:\n{p}"
    );
    let backup_pos = p
        .find("libndi.so.6.6.2.1.0.bak")
        .expect("#452: expected a version-scoped backup reference");
    let overwrite_pos = p
        .find("libndi.so.6.3.2\" \"/usr/lib/ndi/libndi.so.6\"")
        .expect("#445: expected the overwrite of libndi.so.6");
    assert!(
        backup_pos < overwrite_pos,
        "#445: must back up the OLD real-file runtime BEFORE overwriting it. Got:\n{p}"
    );
    assert!(
        !p.contains("ln -sf"),
        "#445: the real-file layout must never use a symlink repoint. Got:\n{p}"
    );
    assert!(p.contains("ldconfig"), "Got:\n{p}");
    assert!(
        p.contains("OLD_BASE=libndi.so.6.6.2.1.0.bak"),
        "#452: must print the VERSION-SCOPED OLD_BASE so the caller's rollback restores from the \
         right generation. Got:\n{p}"
    );
    assert!(p.contains("systemctl restart camera-box"), "Got:\n{p}");
}

/// #452: when NO old-version is supplied (defensive fallback — the real call site always has one
/// once the currently-active version was read), the regular-layout backup name falls back to the
/// original fixed `libndi.so.6.bak` rather than producing an ambiguous/empty-suffixed filename.
#[test]
fn ndi_swap_remote_regular_layout_falls_back_to_fixed_backup_name_when_old_version_omitted() {
    let p = run_sourced("ndi_swap_remote /usr/lib/ndi libndi.so.6.3.2 regular");
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6\" \"/usr/lib/ndi/libndi.so.6.bak\""),
        "#452: omitting old_version must fall back to the fixed backup name, not an \
         empty-suffixed one. Got:\n{p}"
    );
    assert!(p.contains("OLD_BASE=libndi.so.6.bak"), "Got:\n{p}");
}

/// ndi_swap_remote with NO third argument (existing callers/tests) must default to the symlink
/// layout — the #445 real-file branch is strictly additive, never a behavior change for the
/// fleet's existing symlink-layout cameras.
#[test]
fn ndi_swap_remote_defaults_to_symlink_layout_when_kind_omitted() {
    let with_default = run_sourced("ndi_swap_remote /usr/lib/ndi libndi.so.6.3.2");
    let explicit_symlink = run_sourced("ndi_swap_remote /usr/lib/ndi libndi.so.6.3.2 symlink");
    assert_eq!(
        with_default, explicit_symlink,
        "#445: omitting the link-kind argument must be identical to passing 'symlink' explicitly \
         (backward compatibility for the existing symlink-layout cameras)"
    );
}

/// ndi_rollback_remote given the "regular" link kind must restore `libndi.so.6` and `libndi.so`
/// from the `.bak` file ndi_swap_remote created — never a symlink repoint, since there is no
/// symlink to repoint on cam3's layout.
#[test]
fn ndi_rollback_remote_regular_layout_restores_from_backup_file() {
    let p = run_sourced("ndi_rollback_remote /usr/lib/ndi libndi.so.6.bak regular");
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.bak\" \"/usr/lib/ndi/libndi.so.6\""),
        "#445: regular-layout rollback must restore libndi.so.6 from the .bak file. Got:\n{p}"
    );
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.bak\" \"/usr/lib/ndi/libndi.so\""),
        "#445: regular-layout rollback must restore libndi.so from the .bak file too. Got:\n{p}"
    );
    assert!(
        !p.contains("ln -sf"),
        "#445: regular-layout rollback must never symlink-repoint. Got:\n{p}"
    );
    assert!(
        p.contains("ldconfig") && p.contains("systemctl restart camera-box"),
        "Got:\n{p}"
    );
}

/// #452: ndi_rollback_remote is the exact inverse of the version-scoped backup ndi_swap_remote
/// now produces — it needs NO code change (it already restores from whatever OLD_BASE string the
/// caller passes), but this proves the round-trip: a version-scoped backup name restores cleanly,
/// giving the regular (cam3-class) layout the same multi-generation rollback depth the symlink
/// layout already has (each of ITS backups is named after the original versioned filename).
#[test]
fn ndi_rollback_remote_regular_layout_restores_from_version_scoped_backup_name() {
    let p = run_sourced("ndi_rollback_remote /usr/lib/ndi libndi.so.6.6.2.1.0.bak regular");
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.6.2.1.0.bak\" \"/usr/lib/ndi/libndi.so.6\""),
        "#452: rollback must restore libndi.so.6 from the VERSION-SCOPED .bak file. Got:\n{p}"
    );
    assert!(
        p.contains("cp -a \"/usr/lib/ndi/libndi.so.6.6.2.1.0.bak\" \"/usr/lib/ndi/libndi.so\""),
        "#452: rollback must restore libndi.so from the VERSION-SCOPED .bak file too. Got:\n{p}"
    );
}

/// ndi_rollback_remote with NO third argument must default to the symlink layout — same
/// backward-compatibility guarantee as the swap side.
#[test]
fn ndi_rollback_remote_defaults_to_symlink_layout_when_kind_omitted() {
    let with_default = run_sourced("ndi_rollback_remote /usr/lib/ndi libndi.so.6.2.1");
    let explicit_symlink = run_sourced("ndi_rollback_remote /usr/lib/ndi libndi.so.6.2.1 symlink");
    assert_eq!(
        with_default, explicit_symlink,
        "#445: default must equal explicit 'symlink'"
    );
}

/// Static ordering guard: upgrade_one_camera must determine the actual remote layout
/// (ndi_link_kind_remote) BEFORE calling ndi_swap_remote — the swap needs to know which branch
/// to generate, so guessing or hard-coding "symlink" would silently corrupt cam3 again.
#[test]
fn upgrade_one_camera_determines_link_kind_before_swapping() {
    let s = fs::read_to_string(script()).expect("read upgrade-fleet-ndi.sh");
    let kind_pos = s.find("ndi_link_kind_remote").expect(
        "#445: expected upgrade_one_camera to query the real remote layout via ndi_link_kind_remote",
    );
    let swap_call_pos = s
        .find("ndi_swap_remote \"$NDI_DEST_DIR\"")
        .expect("#445: expected the swap call in upgrade_one_camera");
    assert!(
        kind_pos < swap_call_pos,
        "#445: the link kind must be determined BEFORE ndi_swap_remote is called, so the swap \
         script is built for the camera's ACTUAL layout, not an assumed one"
    );
}
