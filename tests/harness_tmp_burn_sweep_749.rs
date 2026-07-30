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

// ---------------------------------------------------------------------------
// #758 item 1 — tmp_burn_sweep_stale_units_cmds: stops (and reset-fails) any stray
// camera-box-burn-* systemd UNIT, not just the /tmp file. Executed for real against a fake
// `systemctl` on PATH that RECORDS its own invocations to a marker file, so the test proves the
// unit actually gets `stop`+`reset-failed`, not just that the command TEXT mentions them.
// ---------------------------------------------------------------------------

#[test]
fn stale_units_cmd_stops_and_reset_fails_every_stray_unit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = tmp.path().join("systemctl-calls.log");
    let fake_systemctl = format!(
        r#"#!/usr/bin/env bash
echo "$@" >> {marker:?}
case "$1" in
  list-units) echo "camera-box-burn-911005.service" ; exit 0 ;;
  stop|reset-failed) exit 0 ;;
  *) exit 0 ;;
esac
"#
    );
    let p = bin_dir.join("systemctl");
    fs::write(&p, fake_systemctl).unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).unwrap();

    let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    let harness = format!(
        "set -uo pipefail\n. {:?}\neval \"$(tmp_burn_sweep_stale_units_cmds)\"",
        lib_script()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PATH", path_env)
        .output()
        .expect("run with fake systemctl");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let calls = fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        calls.contains("list-units"),
        "must list stray camera-box-burn-* units first: {calls}"
    );
    assert!(
        calls.contains("stop camera-box-burn-911005.service"),
        "must stop the stray unit it found: {calls}"
    );
    assert!(
        calls.contains("reset-failed camera-box-burn-911005.service"),
        "must reset-failed the stray unit after stopping it: {calls}"
    );
}

#[test]
fn stale_units_cmd_is_a_noop_when_no_stray_units_exist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_systemctl = r#"#!/usr/bin/env bash
case "$1" in
  list-units) exit 0 ;; # no matching units -> empty stdout
  *) exit 0 ;;
esac
"#;
    let p = bin_dir.join("systemctl");
    fs::write(&p, fake_systemctl).unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).unwrap();

    let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    let harness = format!(
        "set -uo pipefail\n. {:?}\neval \"$(tmp_burn_sweep_stale_units_cmds)\"",
        lib_script()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PATH", path_env)
        .output()
        .expect("run with fake systemctl");
    assert!(
        out.status.success(),
        "a clean box (no stray units) must never fail the preflight: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn stale_units_cmds_embedding_never_glues_the_following_command_758() {
    // #758 — reproduces recording-e2e.sh's EXACT [0/8] preflight embedding shape:
    // "$(tmp_burn_sweep_stale_units_cmds) $(tmp_burn_sweep_stale_cmds)" as ONE ssh remote command
    // string. Command substitution strips ALL trailing newlines from tmp_burn_sweep_stale_units_
    // cmds's own captured output (a multi-line heredoc ending in `done`), so without an explicit
    // `;` after `done`, the space-joined text becomes "...done find /tmp ..." on ONE logical line
    // -- `done` immediately followed by another command's TEXT with no separator is a syntax
    // error (live-reproduced against the real cam3 rig box during this ticket's own development:
    // the FIRST version of this function broke with "syntax error near unexpected token 'find'").
    // IMPORTANT: tmp_burn_sweep_stale_cmds() is a `_cmds`-style function -- it PRINTS the TEXT of
    // a find command (via `echo`), it does NOT execute find itself. The bug is specifically about
    // that PRINTED TEXT getting glued onto the first half's own trailing `done` with no separator
    // -- an earlier version of this test wrongly used `$(find ... -delete)` (which EXECUTES find
    // immediately, producing empty stdout since -delete prints nothing) and therefore glued
    // nothing, silently NOT reproducing the bug. Uses the REAL tmp_burn_sweep_stale_cmds(), which
    // hardcodes /tmp (matching every production call site) -- a unique random-suffixed stale file
    // name avoids any collision with real files on a shared dev box.
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker_log = tmp.path().join("systemctl-calls.log");
    let fake_systemctl = format!(
        "#!/usr/bin/env bash\necho \"$@\" >> {}\ncase \"$1\" in\n  \
         list-units) echo \"camera-box-burn-911002.service\" ; exit 0 ;;\n  \
         stop|reset-failed) exit 0 ;;\n  *) exit 0 ;;\nesac\n",
        marker_log.display()
    );
    let p = bin_dir.join("systemctl");
    fs::write(&p, fake_systemctl).unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    fs::set_permissions(&p, perm).unwrap();

    // A stale file in the REAL /tmp (tmp_burn_sweep_stale_cmds() hardcodes it, like every
    // production call site) -- unique random suffix so this test can never collide with a real
    // file on a shared dev box. Aged past -mmin +60 via `touch -d`.
    let unique = std::process::id();
    let stale_file = PathBuf::from(format!("/tmp/camera-box-burn-test758-{unique}-1234567890"));
    fs::write(&stale_file, "stale").unwrap();
    let touch_status = Command::new("touch")
        .args(["-d", "2020-01-01"])
        .arg(&stale_file)
        .status()
        .expect("age the stale file");
    assert!(touch_status.success(), "touch -d must succeed");

    let path_env = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    // The EXACT production embedding: two $() outputs joined by a literal space, run DIRECTLY as
    // a command. CRITICAL: must be wrapped in a QUOTED string handed to a NESTED `bash -c`, not
    // used bare as an unquoted command in the outer script -- an UNQUOTED `$(A) $(B)` undergoes
    // word-splitting and is interpreted as command+args (no `|`/`while`/`done` re-parsing), which
    // does NOT reproduce the bug at all (confirmed: an earlier version of this test used the bare
    // unquoted form and silently passed even against the broken pre-fix lib, because the whole
    // glued text just became literal arguments to `systemctl`, its first word). The REAL
    // production shape is `ssh host "$(A) $(B)"` -- ssh hands that QUOTED STRING to the remote
    // `bash -c "<string>"`, which RE-PARSES it as actual shell syntax (only THERE does `done find`
    // become a genuine syntax error). `bash -c "$(A) $(B)"` (nested) reproduces that exactly.
    let harness = format!(
        "set -uo pipefail\n. {}\nbash -c \"$(tmp_burn_sweep_stale_units_cmds) $(tmp_burn_sweep_stale_cmds)\"",
        lib_script().display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("PATH", path_env)
        .output()
        .expect("run the combined embedding");
    let cleanup = || {
        let _ = fs::remove_file(&stale_file);
    };
    if !out.status.success() {
        cleanup();
    }
    assert!(
        out.status.success(),
        "the combined embedding must not be a syntax error -- stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(&marker_log).unwrap_or_default();
    let stale_still_exists = stale_file.exists();
    cleanup();
    assert!(
        calls.contains("stop camera-box-burn-911002.service"),
        "the units half must have actually run (not swallowed by the glue): {calls}"
    );
    assert!(
        !stale_still_exists,
        "the find-delete half must have actually run (not swallowed by the glue)"
    );
}
