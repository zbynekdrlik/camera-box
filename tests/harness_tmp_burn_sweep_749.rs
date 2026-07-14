//! #749 — `recording-e2e.sh`'s [2/8]/[2b/8] steps scp the probe-featured camera-box binary to a
//! per-RUN_ID-unique `/tmp/camera-box-burn-<RUN_ID>` (cam1) / `/tmp/camera-box-burn-<camname>-
//! <RUN_ID>` (cam2/3/4/5/6 under ALL_CAMBOX) path on the SOURCE box. `cleanup()`'s own end-of-run
//! `rm -f /tmp/camera-box-burn-*` is best-effort inside a single `timeout`-bounded ssh call — if
//! that ssh round-trip never lands cleanly (a real, recurring condition on cam boxes with flaky
//! storage/SSH, #737), the binary is orphaned permanently. Each box's `/tmp` is a 100MB tmpfs;
//! live evidence (#749) found CAM1 and CAM6 both already at 100% (32-33 accumulated files),
//! failing the very next run's scp deploy outright.
//!
//! Fix: `scripts/lib/tmp-burn-sweep.sh`'s `tmp_burn_sweep_stale_cmds()` — an age-gated
//! (`-mmin +60`) sweep run on the box BEFORE its scp, independent of whether the prior run's own
//! cleanup succeeded. These tests (a) source the REAL lib and pin its exact output text (never
//! re-implement the find command), and (b) assert recording-e2e.sh sources the lib and calls the
//! sweep at BOTH deploy sites (cam1's [2/8], and the ALL_CAMBOX [2b/8] loop) BEFORE their
//! respective scp lines — a sweep wired AFTER the scp would still let a full tmpfs fail that same
//! run's own upload.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/tmp-burn-sweep.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the shared lib and call `tmp_burn_sweep_stale_cmds`, returning its stdout.
fn sweep_cmd_text() -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\ntmp_burn_sweep_stale_cmds".to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "tmp_burn_sweep_stale_cmds exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn sweep_cmd_targets_the_camera_box_burn_glob_age_gated() {
    let cmd = sweep_cmd_text();
    assert!(
        cmd.contains("camera-box-burn-*"),
        "sweep command must target the camera-box-burn-* glob (covers both cam1's plain \
         <RUN_ID> naming and the ALL_CAMBOX <camname>-<RUN_ID> naming): {cmd}"
    );
    assert!(
        cmd.contains("-mmin +60"),
        "sweep command must be age-gated (+60 min) so it never touches a run genuinely still in \
         flight or the file THIS run is about to write: {cmd}"
    );
    assert!(
        cmd.contains("/tmp"),
        "sweep command must target /tmp (the tmpfs that filled, #749): {cmd}"
    );
}

#[test]
fn sweep_cmd_never_fails_the_remote_shell_on_its_own() {
    // Embedded into a larger remote command chain (or run standalone best-effort) — a `find`
    // failure (permission, races) must never abort the caller's remote shell.
    let cmd = sweep_cmd_text();
    assert!(
        cmd.trim_end().ends_with("|| true;") || cmd.trim_end().ends_with("|| true"),
        "sweep command must be `|| true` guarded so it can never fail the remote shell it's \
         embedded in: {cmd}"
    );
}

#[test]
fn regression_e2e_script_sources_the_tmp_burn_sweep_lib_749() {
    let text = recording_e2e_text();
    assert!(
        text.contains("lib/tmp-burn-sweep.sh"),
        "recording-e2e.sh must source scripts/lib/tmp-burn-sweep.sh (#749)"
    );
}

#[test]
fn regression_e2e_script_calls_the_sweep_at_both_deploy_sites_749() {
    let text = recording_e2e_text();
    let calls = text.matches("tmp_burn_sweep_stale_cmds").count();
    // cam1's [2/8] site calls it once; the [2b/8] ALL_CAMBOX loop calls it once per loop
    // iteration in the SCRIPT TEXT (the loop body appears once in the source even though it runs
    // 5 times at runtime) — so 2 occurrences in the file text proves both sites are wired.
    assert!(
        calls >= 2,
        "expected the sweep to be called at both [2/8] (cam1) and [2b/8] (ALL_CAMBOX loop) \
         deploy sites, found {calls} call(s) in recording-e2e.sh text"
    );
}

#[test]
fn regression_sweep_runs_before_cam1_scp_not_after_749() {
    // A sweep wired AFTER cam1's own scp would still let a full tmpfs fail THIS run's own
    // upload — the whole point is to free space before it, not after.
    let text = recording_e2e_text();
    let sweep_idx = text
        .find("tmp_burn_sweep_stale_cmds")
        .expect("sweep must be called somewhere in recording-e2e.sh");
    let cam1_scp_idx = text
        .find("root@\"$CAM1_IP\":\"$CAM1_BURN_BIN\"")
        .expect("cam1's scp destination line must be present");
    assert!(
        sweep_idx < cam1_scp_idx,
        "the sweep must run BEFORE cam1's scp deploy (sweep at byte {sweep_idx}, scp at byte \
         {cam1_scp_idx}) — a full /tmp must be freed before the upload it would otherwise block"
    );
}

#[test]
fn regression_sweep_runs_before_all_cambox_scp_not_after_749() {
    let text = recording_e2e_text();
    let cambox_scp_idx = text
        .find("root@\"$_cip\":\"$_cbin\"")
        .expect("the ALL_CAMBOX loop's scp destination line must be present");
    // The sweep call nearest to (and before) the ALL_CAMBOX scp line — find the LAST sweep
    // occurrence before it (cam1's own sweep call sits earlier in the file and is unrelated).
    let sweep_before = text[..cambox_scp_idx]
        .rfind("tmp_burn_sweep_stale_cmds")
        .expect("a sweep call must appear before the ALL_CAMBOX loop's scp line");
    assert!(
        sweep_before < cambox_scp_idx,
        "the ALL_CAMBOX loop must sweep BEFORE its own scp deploy"
    );
}
