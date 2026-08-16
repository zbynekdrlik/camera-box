//! #716 — a cam-box burn run's own `Streaming: X fps emitted / Y fps captured` telemetry is
//! written FILE-ONLY (`--property=StandardOutput=append:/tmp/cbox-burn.log` for cam1,
//! `/tmp/cbox-burn-<cn>.log` for each ALL_CAMBOX secondary), never journald, and is `rm -f`'d by
//! the NEXT run's deploy before the harness ever copies it back. Only the coarse
//! `cam1-capture-stats.txt` summary is persisted to `$OUTDIR` on dev1 — so at any moment only the
//! LATEST run's fine-grained fps log survives on the box, blocking capture-rate forensics against
//! any specific PAST recording window.
//!
//! Two layers locked here (all Tier-0 — no rig, no ssh; the scp is exercised via a fake `sshpass`
//! stand-in on PATH):
//!  1. the pure lib scripts/lib/cbox-burn-log-persist.sh — resolve each box's own burn-log path
//!     (cam1 bare vs the `-<cn>`-infixed secondaries), build the per-run `$OUTDIR` dest filename,
//!     and run the best-effort scp-back (WARN, never abort — same tolerance as the
//!     cam1-capture-stats sidecar it sits beside);
//!  2. recording-e2e.sh actually WIRES the persist: sources the lib and, right after the
//!     cam1-capture-stats scp, persists cam1's burn log plus (ALL_CAMBOX-gated) every secondary's
//!     via the existing CAMBOX_SECONDARY_DEPLOY list — before the [8/8] verdict step (a static
//!     read of the shell script, the same model as tests/harness_audio_presence_preflight.rs).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/cbox-burn-log-persist.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `snippet`, returning (exit_ok, stdout_trimmed). Uses `set -uo pipefail`
/// (never `-e`) so a best-effort helper's internal `|| ...` fallbacks never abort the harness.
fn run(snippet: &str) -> (bool, String) {
    let script = format!(
        "set -uo pipefail\n. \"{}\"\n{}",
        lib_script().display(),
        snippet
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn remote_path_cam1_is_the_bare_burn_log() {
    let (ok, p) = run("cbox_burn_log_remote_path cam1");
    assert!(ok, "cbox_burn_log_remote_path must succeed for cam1");
    assert_eq!(
        p, "/tmp/cbox-burn.log",
        "#716: cam1's own burn log is the bare /tmp/cbox-burn.log (matches recording-e2e.sh's \
         StandardOutput=append: target)"
    );
}

#[test]
fn remote_path_secondary_is_camname_infixed() {
    let (_ok, p2) = run("cbox_burn_log_remote_path cam2");
    assert_eq!(p2, "/tmp/cbox-burn-cam2.log");
    let (_ok, p3) = run("cbox_burn_log_remote_path cam3");
    assert_eq!(
        p3, "/tmp/cbox-burn-cam3.log",
        "#716: each secondary box writes to the -<cn>-infixed /tmp/cbox-burn-<cn>.log"
    );
}

#[test]
fn dest_name_matches_the_cam1_sidecar_convention() {
    let (ok, n1) = run("cbox_burn_log_dest_name cam1 12345");
    assert!(ok);
    assert_eq!(
        n1, "cam1-cbox-burn-12345.log",
        "#716: the per-run OUTDIR filename mirrors cam1-capture-stats.txt's <cam>-* convention"
    );
    let (_ok, n3) = run("cbox_burn_log_dest_name cam3 999");
    assert_eq!(n3, "cam3-cbox-burn-999.log");
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cbox-burn-persist-716-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Install a fake `sshpass` on PATH that appends its argv (space-joined) as one line to
/// `$ARGV_LOG` and exits with `$FAKE_EXIT` (default 0). This lets the scp-back be exercised with
/// zero network: the fake stands in for `sshpass -p <pw> scp ...`, capturing exactly the argv the
/// helper built (source `root@ip:remote` and dest `$OUTDIR/<dest>`), even though the helper
/// redirects the real command's stderr to /dev/null.
fn install_fake_sshpass(bin_dir: &std::path::Path) {
    let script = "#!/usr/bin/env bash\necho \"$@\" >> \"$ARGV_LOG\"\nexit \"${FAKE_EXIT:-0}\"\n";
    let p = bin_dir.join("sshpass");
    fs::write(&p, script).expect("write fake sshpass");
    let mut perms = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&p, perms).unwrap();
}

/// Run `cbox_burn_log_persist` against the fake sshpass; returns (exit_ok, argv_log, stderr).
fn persist(bin_dir: &std::path::Path, args: &str, fake_exit: &str) -> (bool, String, String) {
    let argv_log = bin_dir.join("argvlog");
    fs::write(&argv_log, "").expect("create argv log");
    let harness = format!(
        "set -uo pipefail\nexport PATH=\"{bin}:$PATH\"\nexport ARGV_LOG=\"{log}\"\nexport \
         FAKE_EXIT=\"{fe}\"\n. \"{lib}\"\ncbox_burn_log_persist {args}",
        bin = bin_dir.display(),
        log = argv_log.display(),
        fe = fake_exit,
        lib = lib_script().display(),
        args = args,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("run persist harness");
    (
        out.status.success(),
        fs::read_to_string(&argv_log).unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn persist_scps_cam1_burn_log_to_outdir() {
    let dir = scratch("cam1");
    install_fake_sshpass(&dir);
    let (ok, argv, _stderr) = persist(&dir, "testpw 1.2.3.4 cam1 12345 /some/outdir", "0");
    assert!(ok, "a successful scp-back must exit 0");
    assert!(argv.contains("scp"), "must invoke scp: {argv}");
    assert!(
        argv.contains("root@1.2.3.4:/tmp/cbox-burn.log"),
        "#716: must pull cam1's bare burn log from the box: {argv}"
    );
    assert!(
        argv.contains("/some/outdir/cam1-cbox-burn-12345.log"),
        "#716: must land at $OUTDIR/cam1-cbox-burn-<run>.log on dev1: {argv}"
    );
}

#[test]
fn persist_scps_secondary_infixed_burn_log() {
    let dir = scratch("cam2");
    install_fake_sshpass(&dir);
    let (ok, argv, _stderr) = persist(&dir, "testpw 5.6.7.8 cam2 777 /some/outdir", "0");
    assert!(ok);
    assert!(
        argv.contains("root@5.6.7.8:/tmp/cbox-burn-cam2.log"),
        "#716: a secondary's own -<cn>-infixed burn log must be pulled: {argv}"
    );
    assert!(
        argv.contains("/some/outdir/cam2-cbox-burn-777.log"),
        "#716: a secondary lands at $OUTDIR/<cn>-cbox-burn-<run>.log: {argv}"
    );
}

#[test]
fn persist_is_best_effort_warns_but_never_aborts() {
    let dir = scratch("fail");
    install_fake_sshpass(&dir);
    // FAKE_EXIT=1 simulates a failed/unreachable scp — the helper must still exit 0 (best-effort,
    // same tolerance as the cam1-capture-stats sidecar) and emit an operator WARNING to stderr.
    let (ok, _argv, stderr) = persist(&dir, "testpw 9.9.9.9 cam1 42 /some/outdir", "1");
    assert!(
        ok,
        "#716: a failed burn-log fetch must NEVER abort the harness this far into a run (best-effort)"
    );
    assert!(
        stderr.contains("WARNING") && stderr.to_lowercase().contains("burn"),
        "#716: a failed fetch must WARN (name the missing burn log): {stderr}"
    );
}

// --- static wiring guards on recording-e2e.sh ---

#[test]
fn recording_e2e_sources_the_cbox_burn_log_persist_lib() {
    let s = recording_e2e_text();
    assert!(
        s.contains("lib/cbox-burn-log-persist.sh"),
        "#716: recording-e2e.sh must source scripts/lib/cbox-burn-log-persist.sh"
    );
}

#[test]
fn recording_e2e_persists_the_cam1_and_secondary_burn_logs() {
    let s = recording_e2e_text();
    // cam1 (primary) is persisted unconditionally, and every secondary via the existing
    // CAMBOX_SECONDARY_DEPLOY list (never a literal cam range) — so at least two call sites.
    let calls = s.matches("cbox_burn_log_persist").count();
    assert!(
        calls >= 2,
        "#716: expected the primary + the ALL_CAMBOX secondary-loop persist calls (>=2), found \
         {calls}"
    );
    let block_start = s
        .find("#716: persist each cam-box burn-run fps log")
        .expect("#716: recording-e2e.sh must carry the persist block (its #716 marker comment)");
    let block = &s[block_start..];
    assert!(
        block.contains("cbox_burn_log_persist"),
        "#716: the persist block must call cbox_burn_log_persist"
    );
    assert!(
        block.contains("if [ \"${ALL_CAMBOX:-0}\" = \"1\" ]"),
        "#716: the secondary persist must be gated behind ALL_CAMBOX=1"
    );
    assert!(
        block.contains("CAMBOX_SECONDARY_DEPLOY"),
        "#716: the secondary persist must iterate the existing CAMBOX_SECONDARY_DEPLOY fleet list \
         (never a literal cam-number range)"
    );
}

#[test]
fn recording_e2e_persists_burn_logs_before_the_verdict_step() {
    let s = recording_e2e_text();
    let persist_pos = s
        .find("#716: persist each cam-box burn-run fps log")
        .expect("#716: the persist block must exist");
    let verdict_pos = s
        .find("[8/8] recording-verdict")
        .expect("recording-e2e.sh has the [8/8] recording-verdict step");
    assert!(
        persist_pos < verdict_pos,
        "#716: the burn logs must be pulled back BEFORE the [8/8] verdict step (which can `exit 0` \
         on the default VERDICT_ON_STREAM path), while the boxes are still reachable"
    );
}
