//! Issue 1409: no test harness may hand-roll a pid-named scratch path under the OS temp dir.
//!
//! `temp_dir().join(format!("…-{}", process::id()))` is the same path for every thread of one
//! test binary (two tests can share it), and a helper that never removes it leaves one dir per
//! run in /tmp on dev1 and on the CI runners. A test takes its scratch dir from
//! `tempfile::tempdir()` and keeps the `TempDir` alive for the test, so Drop removes it.
//!
//! The ALLOWLIST names the files that still build such a path but remove it themselves (a Drop
//! guard, or a `remove_dir_all` / `remove_file` at the end of the test), or that must stay
//! std-only with the `{tag}-{pid}-{counter}` pattern. A file leaves the list once it no longer
//! matches; the second test fails on a stale entry.
//!
//! Std-only on purpose: it reads the test sources as text and runs no other code.

use std::fs;
use std::path::{Path, PathBuf};

/// Files allowed to keep a pid-named temp path, as `"<file under tests/>: <why it does not leak>"`.
const ALLOWLIST: &[&str] = &[
    "aux_sender_budget_879.rs: every workdir() caller removes it at the end",
    "av_run_recording_1404.rs: the test removes its dir at the end",
    "av_stack_update.rs: each test removes its temp README",
    "av_sync_dock_audio_worker_1381.rs: the test removes its dir at the end",
    "av_sync_dock_audit_log.rs: the test removes its workdir at the end",
    "av_sync_dock_cpp_mirror_gate.rs: the test removes its self-test binary",
    "av_sync_dock_decode_mailbox_1367.rs: the test removes its self-test binary",
    "av_sync_dock_demod_bench_1381.rs: a Drop guard removes the dir",
    "av_sync_dock_latency_display_999.rs: the test removes its workdir at the end",
    "av_sync_dock_lock_926.rs: the test removes its workdir at the end",
    "av_sync_dock_lock_churn_999.rs: the test removes its workdir at the end",
    "av_sync_dock_outcome_955.rs: the test removes its workdir at the end",
    "av_sync_dock_suggested_target_953.rs: the test removes its workdir at the end",
    "av_sync_dock_video_decode_921.rs: the test removes its workdir at the end",
    "av_sync_dock_video_ts_valid_1005.rs: the test removes its workdir at the end",
    "burn_regions_cpp_parity_1370.rs: cpp_placements() removes its dir",
    "deploy_genlock_fleet.rs: the one pid dir is removed by its test",
    "distroav_sender_port_linger_1363.rs: a Drop guard removes the dir",
    "drm_output_view_1346.rs: compile_and_run() removes its dir",
    "drm_output_view_mv_budget_1346.rs: the test removes its workdir at the end",
    "drm_output_vk_direct_1346.rs: compile_and_run() removes its dir",
    "genlock_audio_buffering_wiring_1367.rs: the lift removes its scratch dir",
    "genlock_audio_mix_guard_1381.rs: the lift removes its scratch dir",
    "genlock_audio_relabel_ingest_1381.rs: c_trace() removes its scratch dir",
    "genlock_fifo_relabel_wiring_1372.rs: the test removes its dir at the end",
    "genlock_render_tick_pin_1357.rs: std-only lift: pid + counter, a Drop guard removes it",
    "harness_cam2_paint_signal_1148.rs: the test removes its scratch dir",
    "harness_cam_disk_guard.rs: the test removes its scratch dir",
    "harness_camera_set.rs: the injection marker file is removed after the test",
    "harness_cg_chain_hops_1302.rs: the test removes its dir at the end",
    "harness_e2e_execute_verdict_703.rs: the test removes its dir at the end",
    "harness_frozen_input_enum_1069.rs: a Drop guard removes the dir",
    "harness_genlock_jitter_report.rs: each test removes its temp log",
    "harness_genlock_settle_1221.rs: the test removes its stub dir",
    "harness_imag_cmdline_isolation_preflight_1105.rs: the test removes its fake-ssh dir",
    "harness_manifest_autosource_1082.rs: every tmpdir() caller removes it",
    "harness_mgmt_liveness_1309.rs: the test removes its generated script",
    "harness_mv_reverify_escalate_1093.rs: the dir and the stub files are removed",
    "harness_ps_encoded_fleet_1259.rs: the only tmp_dir() caller removes it",
    "harness_received_tap_byte_safety_1258.rs: every tmp_dir() caller removes it",
    "harness_received_tap_encoded_command_1258.rs: probe_raw_invocation() removes its dir",
    "harness_recording_verdict_on_imag.rs: the test removes its dir at the end",
    "harness_remote_env_quoting.rs: the injection marker file is removed after the test",
    "harness_rig_restore_watchdog.rs: every scratch() caller removes it",
    "harness_transport_sampler_707.rs: the test removes its dir at the end",
    "harness_verdict_done_marker.rs: every scratch() caller removes it",
    "harness_verify_fleet.rs: both callers remove their dir",
    "harness_with_rig_restore.rs: every scratch() caller removes it",
    "intercom_target_dropin_1345.rs: the test removes its workspace at the end",
    "mv_audit_cell_name_sanitizer_1260.rs: the test removes its .c and binary",
    "obs_crash_handler_no_modal_1378.rs: a Drop guard removes the dir",
    "obs_display_budget.rs: the test removes its workdir at the end",
    "obs_titlebar_newlevel_sha_parse.rs: the test removes its workdir at the end",
    "os_clock_discipline_parity_1372.rs: std-only lift: pid + counter, a Drop guard removes it",
    "provisioning_defects_1311.rs: pid + counter, both callers remove their dir",
    "qpsk_channel_pick_parity_1367.rs: a Drop guard removes the dir",
    "recording_decode.rs: both tests remove their dir",
    "recording_verdict.rs: one test removes its dir, the other never creates one",
    "setup_imag_hardware_agnostic.rs: every test removes its stub dir",
    "strih_drm_output_provision_1346.rs: every scratch() caller removes it",
    "strih_provision_pure_functions.rs: the one pid dir is removed by its test",
    "support/ndi_source_lift_1367.rs: std-only lift: pid + counter, the files are removed",
    "vban_pacing_parity_1372.rs: c_trace() removes its dir",
];

/// Split an allowlist entry into (file, reason).
fn entry(e: &str) -> (&str, &str) {
    e.split_once(": ")
        .expect("an allowlist entry reads \"<file>: <reason>\"")
}

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Every `.rs` file under `tests/`, as a path relative to `tests/` with `/` separators.
fn test_sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![tests_dir()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read the tests dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(tests_dir())
                    .expect("under tests/")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = fs::read_to_string(&path).expect("read a test source");
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

/// True when the source builds a pid-keyed path and also reaches the OS temp dir: a code line
/// (not a `//` comment) calls `process::id()`, and some code line names `temp_dir`.
fn builds_pid_temp_path(src: &str) -> bool {
    // Spelled in two pieces so this file never matches itself.
    let pid = concat!("process", "::id()");
    let tmp = concat!("temp", "_dir");
    let code: Vec<&str> = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    code.iter().any(|l| l.contains(pid)) && code.iter().any(|l| l.contains(tmp))
}

#[test]
fn no_test_hand_rolls_a_pid_named_temp_dir_1409() {
    let offenders: Vec<String> = test_sources()
        .into_iter()
        .filter(|(rel, text)| {
            builds_pid_temp_path(text) && !ALLOWLIST.iter().any(|e| entry(e).0 == rel)
        })
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        offenders.is_empty(),
        "issue 1409: these tests/ files build a pid-named path under temp_dir() — take the \
         scratch dir from tempfile::tempdir() and keep the TempDir alive for the test \
         (or, for a std-only lift, use pid + counter with a Drop cleanup and add it to the \
         allowlist with its reason): {offenders:?}"
    );
}

#[test]
fn the_allowlist_names_only_files_that_still_need_it_1409() {
    let sources = test_sources();
    let mut stale = Vec::new();
    for e in ALLOWLIST {
        let (name, reason) = entry(e);
        assert!(
            !reason.trim().is_empty(),
            "issue 1409: {name} needs a reason"
        );
        match sources.iter().find(|(rel, _)| rel == name) {
            Some((_, text)) if builds_pid_temp_path(text) => {}
            _ => stale.push(name),
        }
    }
    assert!(
        stale.is_empty(),
        "issue 1409: these allowlist entries are gone or no longer build a pid temp path — \
         remove them from the allowlist: {stale:?}"
    );
}

#[test]
fn the_detector_sees_the_hand_rolled_shape_and_ignores_comments_1409() {
    let pid = concat!("process", "::id()");
    let tmp = concat!("temp", "_dir");
    let leaking = format!("let d = std::env::{tmp}().join(format!(\"x-{{}}\", std::{pid}));");
    assert!(builds_pid_temp_path(&leaking));
    let commented =
        format!("// never std::env::{tmp}() + std::{pid}\nlet d = tempfile::tempdir();");
    assert!(!builds_pid_temp_path(&commented));
    let tag_only = format!("let tag = format!(\"t-{{}}\", std::{pid});");
    assert!(!builds_pid_temp_path(&tag_only));
}
