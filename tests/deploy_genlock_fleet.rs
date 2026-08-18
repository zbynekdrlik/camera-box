//! issue 789 (bod 4 + bod 5) — one deploy path for the OBS genlock build across strih + stream +
//! imag from ONE CI run id, plus deploy-*/obs-backup-* retention.
//!
//! `scripts/deploy-genlock-fleet.sh` is a PLANNER + bounded ssh-executor in the exact shape of
//! `scripts/launch-obs-genlock.sh`: pure builder functions (no network, no MCP, no Windows) that
//! EMIT the per-box deploy program, a source-guard, then a `main` flow. The Windows program is
//! pasted into the box's `win-*` MCP Shell (a bash script cannot drive the MCP — win-ssh-vs-mcp);
//! the imag program runs over ssh. These guards source the script and its shared marker lib
//! (`scripts/lib/genlock-markers.sh`), call the pure builders, and assert the emitted programs are
//! well-formed — so a regression is caught with no rig, per the repo's Tier-0 discipline.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/deploy-genlock-fleet.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn markers_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/genlock-markers.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source `path` and run `body` (which may call the pure functions the file defines). Returns
/// stdout. Fails the test if the harness exits non-zero.
fn run_sourced(path: &PathBuf, body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", path)
        .current_dir(manifest_dir())
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

/// Source `path`, run `body`, and return (exit_code, stdout, stderr) WITHOUT asserting success —
/// for the error-path guards.
fn run_sourced_status(path: &PathBuf, body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", path)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run the fleet script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run deploy-genlock-fleet.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn win_program(box_name: &str, mode: &str, has_ahk: &str) -> String {
    run_sourced(
        &script(),
        &format!(
            "build_windows_deploy_program {box_name} {mode} 'C:\\stage-genlock-RUN' \
             'C:\\Program Files\\obs-studio' {has_ahk} 'C:\\obs-backup' 3 SHA789 DSHA789"
        ),
    )
}

fn imag_program() -> String {
    run_sourced(
        &script(),
        "build_imag_deploy_program /tmp/genlock-stage-RUN /opt/obs-genlock /opt/obs-backup \
         SHA789 DSHA789 3",
    )
}

// ============================================================================================
// scripts/lib/genlock-markers.sh — the shared marker helper (setup-imag.sh calls it too).
// ============================================================================================

/// genlock_write_markers writes all three markers atomically (temp-then-rename) with a trailing
/// newline, and leaves no *.tmp* file behind.
#[test]
fn genlock_write_markers_writes_all_three_atomically() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    run_sourced(
        &markers_lib(),
        &format!(
            "genlock_write_markers '{}' aaa111 bbb222 '2026-08-18T00:00:00+00:00'",
            dir.display()
        ),
    );
    let g = std::fs::read_to_string(dir.join("GENLOCK_BUILD_SHA.txt")).unwrap();
    let d = std::fs::read_to_string(dir.join("DISTROAV_BUILD_SHA.txt")).unwrap();
    let at = std::fs::read_to_string(dir.join("DEPLOYED_AT")).unwrap();
    assert_eq!(g, "aaa111\n", "GENLOCK_BUILD_SHA.txt content");
    assert_eq!(d, "bbb222\n", "DISTROAV_BUILD_SHA.txt content");
    assert_eq!(at, "2026-08-18T00:00:00+00:00\n", "DEPLOYED_AT content");
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp-then-rename must leave no .tmp file behind, found: {leftovers:?}"
    );
}

/// DEPLOYED_AT defaults to an ISO timestamp when the 4th arg is omitted.
#[test]
fn genlock_write_markers_defaults_deployed_at() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    run_sourced(
        &markers_lib(),
        &format!("genlock_write_markers '{}' aaa bbb", dir.display()),
    );
    let at = std::fs::read_to_string(dir.join("DEPLOYED_AT")).unwrap();
    assert!(
        at.contains("T") && at.len() > 10,
        "DEPLOYED_AT should default to an ISO-8601 timestamp, got {at:?}"
    );
}

/// An empty marker dir (or empty genlock sha) is a fail-loud usage error, never a silent no-op.
#[test]
fn genlock_write_markers_fails_loud_on_missing_args() {
    let (code, _out, err) = run_sourced_status(&markers_lib(), "genlock_write_markers '' aaa bbb");
    assert!(
        code != 0 && err.contains("genlock_write_markers"),
        "empty marker dir must fail loud (code={code}, err={err:?})"
    );
}

/// setup-imag.sh ships STANDALONE to the box (it cannot source the sibling lib), so it carries an
/// inline copy of genlock_write_markers. That copy must be BEHAVIORALLY IDENTICAL to the shared
/// scripts/lib/genlock-markers.sh — one behavior, two homes. Source both, write markers with the
/// same args (incl. an explicit DEPLOYED_AT so the timestamp is deterministic), assert the three
/// marker files are byte-for-byte identical.
#[test]
fn inline_genlock_write_markers_matches_the_shared_lib() {
    let setup = manifest_dir().join("scripts/setup-imag.sh");
    assert!(setup.exists(), "{} not found", setup.display());
    let a = tempfile::tempdir().expect("tempdir a");
    let b = tempfile::tempdir().expect("tempdir b");
    let at = "2026-08-18T12:00:00+00:00";
    run_sourced(
        &setup,
        &format!(
            "genlock_write_markers '{}' shaGGG shaDDD '{at}'",
            a.path().display()
        ),
    );
    run_sourced(
        &markers_lib(),
        &format!(
            "genlock_write_markers '{}' shaGGG shaDDD '{at}'",
            b.path().display()
        ),
    );
    for f in [
        "GENLOCK_BUILD_SHA.txt",
        "DISTROAV_BUILD_SHA.txt",
        "DEPLOYED_AT",
    ] {
        let ca = std::fs::read_to_string(a.path().join(f)).unwrap();
        let cb = std::fs::read_to_string(b.path().join(f)).unwrap();
        assert_eq!(
            ca, cb,
            "setup-imag.sh inline vs the shared lib genlock_write_markers differ on {f}"
        );
    }
}

// ============================================================================================
// deploy-genlock-fleet.sh — pure resolution helpers.
// ============================================================================================

#[test]
fn normalize_boxes_dedups_and_validates() {
    assert_eq!(
        run_sourced(&script(), "fleet_normalize_boxes imag,strih,imag").trim(),
        "strih,imag",
        "canonical order strih,stream,imag with dedup"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_normalize_boxes ''").trim(),
        "strih,stream,imag",
        "empty selection defaults to the whole fleet"
    );
    let (code, _o, err) = run_sourced_status(&script(), "fleet_normalize_boxes bogus");
    assert!(
        code != 0 && err.contains("bogus"),
        "an unknown box must fail loud naming it (code={code}, err={err:?})"
    );
}

#[test]
fn windows_artifact_and_workflow_by_mode() {
    assert_eq!(
        run_sourced(&script(), "fleet_windows_artifact full").trim(),
        "obs-genlock-windows-x64"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_windows_artifact fast").trim(),
        "obs-genlock-fast-dll"
    );
    assert!(run_sourced(&script(), "fleet_windows_workflow full").contains("windows-genlock.yml"));
    assert!(
        run_sourced(&script(), "fleet_windows_workflow fast").contains("windows-genlock-fast.yml")
    );
    assert_eq!(
        run_sourced(&script(), "fleet_linux_bundle_artifact").trim(),
        "obs-genlock-linux-x86_64"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_linux_distroav_artifact").trim(),
        "distroav-linux-fast-so"
    );
}

/// Retention keeps the newest KEEP dirs and prints the rest as delete candidates (never deletes).
#[test]
fn retention_delete_plan_keeps_newest_n() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path();
    // Create 5 backup dirs with strictly increasing mtimes (d0 oldest .. d4 newest).
    for i in 0..5 {
        let d = parent.join(format!("obs-backup-2026-08-1{i}"));
        std::fs::create_dir_all(&d).unwrap();
        // touch with an mtime ordered by i (via `-d` relative seconds)
        Command::new("touch")
            .args(["-d", &format!("2026-08-18 10:0{i}:00"), d.to_str().unwrap()])
            .status()
            .unwrap();
    }
    let out = run_sourced(
        &script(),
        &format!(
            "genlock_retention_delete_plan 3 '{}' 'obs-backup-*'",
            parent.display()
        ),
    );
    // The 2 OLDEST (d0, d1) are the delete candidates; the newest 3 (d2,d3,d4) are kept.
    assert!(
        out.contains("obs-backup-2026-08-10"),
        "oldest must be a delete candidate:\n{out}"
    );
    assert!(
        out.contains("obs-backup-2026-08-11"),
        "2nd-oldest must be a delete candidate:\n{out}"
    );
    assert!(
        !out.contains("obs-backup-2026-08-14"),
        "newest must be kept, not listed:\n{out}"
    );
    assert!(
        !out.contains("obs-backup-2026-08-12"),
        "3rd-newest must be kept:\n{out}"
    );
}

#[test]
fn retention_with_fewer_than_keep_lists_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path();
    for i in 0..2 {
        std::fs::create_dir_all(parent.join(format!("obs-backup-{i}"))).unwrap();
    }
    let out = run_sourced(
        &script(),
        &format!(
            "genlock_retention_delete_plan 3 '{}' 'obs-backup-*'",
            parent.display()
        ),
    );
    assert!(
        out.trim().is_empty(),
        "2 dirs, keep 3 -> nothing to delete, got:\n{out}"
    );
}

// ============================================================================================
// build_windows_deploy_program — the emitted PowerShell for strih / stream.
// ============================================================================================

#[test]
fn windows_program_is_a_fail_loud_powershell_deploy() {
    let p = win_program("strih", "full", "1");
    assert!(
        p.contains("$ErrorActionPreference = 'Stop'"),
        "must set Stop:\n{p}"
    );
    // clears crash sentinels + stops OBS before copying (locked files otherwise)
    assert!(p.contains(".sentinel"), "must clear crash sentinels:\n{p}");
    assert!(p.contains("obs64"), "must stop obs64 before the copy:\n{p}");
    // markers (BOTH) + DEPLOYED_AT written temp-then-rename (Move-Item -Force)
    assert!(
        p.contains("GENLOCK_BUILD_SHA.txt"),
        "writes the genlock marker:\n{p}"
    );
    assert!(
        p.contains("DISTROAV_BUILD_SHA.txt"),
        "writes the distroav marker:\n{p}"
    );
    assert!(p.contains("DEPLOYED_AT"), "writes DEPLOYED_AT:\n{p}");
    assert!(
        p.contains("Move-Item") && p.contains("-Force"),
        "markers must be written temp-then-rename via Move-Item -Force:\n{p}"
    );
    // sha256 verify of deployed bytes vs the artifact manifest, fail-closed
    assert!(
        p.contains("Get-FileHash") && p.contains("SHA256"),
        "sha256 verify:\n{p}"
    );
    assert!(
        p.contains("BUNDLE_MANIFEST.json"),
        "verify against the bundle manifest:\n{p}"
    );
    assert!(p.contains("match="), "prints per-component match=:\n{p}");
    assert!(
        p.to_uppercase().contains("VERIFY FAIL") || p.contains("exit 1"),
        "a byte mismatch must fail loud (non-zero exit):\n{p}"
    );
    // robocopy 0-7 is success — only >= 8 is failure
    assert!(
        p.contains("$LASTEXITCODE") && p.contains("8"),
        "robocopy exit >= 8 is the only failure (0-7 success):\n{p}"
    );
    // env-free (the genlock build carries no env)
    for env in [
        "OBS_GENLOCK_WALL_CLOCK",
        "OBS_GENLOCK_LATENCY_MS",
        "OBS_BURN_QR",
    ] {
        assert!(!p.contains(env), "must carry no {env}:\n{p}");
    }
}

#[test]
fn windows_full_program_does_the_three_surgical_robocopies() {
    let p = win_program("strih", "full", "1");
    assert!(p.contains("robocopy"), "full deploy uses robocopy:\n{p}");
    assert!(p.contains("bin\\64bit"), "copies bin\\64bit:\n{p}");
    assert!(p.contains("/XF *.pdb"), "PDBs are never deployed:\n{p}");
    assert!(
        p.contains("obs-virtualcam-module64.dll"),
        "data\\ copy must /XF the camera-frame-server-locked virtualcam dll:\n{p}"
    );
    assert!(
        p.contains("/XF distroav.dll"),
        "obs-plugins\\64bit copy must /XF distroav.dll (it lives in ProgramData):\n{p}"
    );
    assert!(
        p.contains("/R:2 /W:2"),
        "short retry cap so a locked file can't hang forever:\n{p}"
    );
}

#[test]
fn windows_fast_program_is_obs_dll_only() {
    let p = win_program("stream", "fast", "0");
    assert!(p.contains("obs.dll"), "fast deploy copies obs.dll:\n{p}");
    assert!(
        !p.contains("robocopy"),
        "fast (libobs-only) deploy must NOT robocopy data/ or obs-plugins/ — obs.dll only:\n{p}"
    );
    // markers are still written on a fast deploy
    assert!(
        p.contains("GENLOCK_BUILD_SHA.txt"),
        "fast deploy still writes the marker:\n{p}"
    );
}

#[test]
fn windows_ahk_bracket_only_on_strih() {
    let strih = win_program("strih", "full", "1");
    let stream = win_program("stream", "full", "0");
    assert!(
        strih.contains("Stop-Process -Name AutoHotkey64"),
        "strih runs the AHK watchdog — must be stopped before the copy:\n{strih}"
    );
    assert!(
        !stream.contains("Stop-Process -Name AutoHotkey64"),
        "stream has NO AHK watcher — its program must carry no real AutoHotkey64 stop:\n{stream}"
    );
}

#[test]
fn windows_program_prints_retention_plan_never_silent_delete() {
    let p = win_program("strih", "full", "1");
    assert!(
        p.to_uppercase().contains("RETENTION PLAN"),
        "prints a retention plan:\n{p}"
    );
    assert!(
        p.contains("Skip 3") || p.contains("-Skip 3"),
        "keeps the newest 3 backup dirs:\n{p}"
    );
    assert!(
        p.to_lowercase().contains("would delete"),
        "the retention plan lists what it WOULD delete (agent confirms):\n{p}"
    );
    // deletion must be gated behind an explicit confirm — never an unconditional Remove-Item of backups
    assert!(
        p.contains("fleetConfirmRetention"),
        "backup deletion must be gated behind an explicit confirm flag:\n{p}"
    );
}

// ============================================================================================
// build_imag_deploy_program — the emitted on-imag bash (run over ssh).
// ============================================================================================

#[test]
fn imag_program_installs_all_four_and_writes_markers_via_the_shared_helper() {
    let p = imag_program();
    assert!(
        p.contains("genlock_write_markers"),
        "imag deploy writes markers via the SHARED helper (single source of truth):\n{p}"
    );
    assert!(
        p.contains("genlock-markers.sh"),
        "the on-imag program sources the scp'd genlock-markers.sh lib:\n{p}"
    );
    // all four components installed
    assert!(p.contains("libobs.so.30"), "installs libobs.so.30:\n{p}");
    assert!(p.contains("distroav.so"), "installs distroav.so:\n{p}");
    assert!(
        p.contains("libobs-opengl.so.30"),
        "installs libobs-opengl.so.30 (#756):\n{p}"
    );
    assert!(
        p.contains("/usr/bin/obs"),
        "installs the frontend /usr/bin/obs (#499):\n{p}"
    );
    assert!(p.contains("ldconfig"), "runs ldconfig after the swap:\n{p}");
}

#[test]
fn imag_program_keeps_the_abi_guards_and_graceful_restart() {
    let p = imag_program();
    assert!(
        p.contains("readelf") && p.contains("SONAME"),
        "SONAME sanity check — refuse a mismatched ABI:\n{p}"
    );
    assert!(
        p.contains("obs_display_set_render_divisor"),
        "nm build-proof — refuse a stock/wrong frontend (#499):\n{p}"
    );
    // graceful restart via the EXISTING installed helpers, then verify the process actually restarted
    assert!(
        p.contains("imag-obs-stop.sh"),
        "graceful stop via the existing helper:\n{p}"
    );
    assert!(
        p.contains("imag-obs-start.sh"),
        "start via the existing helper:\n{p}"
    );
    assert!(
        p.contains("lstart") || p.contains("etimes"),
        "verify the obs process actually restarted (start-time race, #912):\n{p}"
    );
}

#[test]
fn imag_program_prints_retention_plan() {
    let p = imag_program();
    assert!(
        p.contains("/opt/obs-backup"),
        "retention runs over /opt/obs-backup:\n{p}"
    );
    assert!(
        p.to_uppercase().contains("RETENTION PLAN"),
        "prints a retention plan:\n{p}"
    );
}

// ============================================================================================
// fleet_log_line + the --plan flow.
// ============================================================================================

#[test]
fn fleet_log_line_is_one_tab_separated_record() {
    let line = run_sourced(
        &script(),
        "fleet_log_line RUN123 abcdef1 strih,stream,imag full",
    );
    let line = line.trim();
    assert!(line.contains('\t'), "tab-separated:\n{line}");
    for tok in ["RUN123", "abcdef1", "strih,stream,imag", "full"] {
        assert!(line.contains(tok), "log line must carry {tok}:\n{line}");
    }
    assert_eq!(line.lines().count(), 1, "exactly one line:\n{line}");
}

#[test]
fn plan_mode_emits_all_box_programs_without_network() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path();
    let (code, out, err) = run_script(&[
        "--plan",
        "--run-id",
        "RUN123",
        "--sha",
        "abcdef1234",
        "--stage",
        stage.to_str().unwrap(),
        "--boxes",
        "strih,imag",
        "--full",
    ]);
    assert_eq!(code, 0, "--plan must succeed.\nstdout={out}\nstderr={err}");
    // Windows (strih) program present
    assert!(
        out.contains("$ErrorActionPreference = 'Stop'"),
        "emits the strih PS program:\n{out}"
    );
    assert!(
        out.contains("win-strih"),
        "names the strih MCP for the paste step:\n{out}"
    );
    // imag program present
    assert!(
        out.contains("genlock_write_markers"),
        "emits the imag deploy program:\n{out}"
    );
    // fleet log line carrying the anchor run id + sha
    assert!(
        out.contains("RUN123") && out.contains("abcdef1234"),
        "emits the fleet log line with run id + sha:\n{out}"
    );
    // stream was NOT requested -> its MCP must not appear
    assert!(
        !out.contains("win-stream-snv"),
        "stream not selected -> not in the plan:\n{out}"
    );
}

#[test]
fn usage_errors_exit_two() {
    // --fast and --full are mutually exclusive
    let (c1, _o, _e) = run_script(&[
        "--plan", "--run-id", "R", "--sha", "S", "--stage", "/tmp", "--fast", "--full",
    ]);
    assert_eq!(c1, 2, "conflicting --fast --full");
    // an unknown box
    let (c2, _o, _e) = run_script(&[
        "--plan", "--run-id", "R", "--sha", "S", "--stage", "/tmp", "--boxes", "bogus",
    ]);
    assert_eq!(c2, 2, "unknown box");
    // --plan requires --stage and --sha (no network in plan mode)
    let (c3, _o, _e) = run_script(&["--plan", "--run-id", "R"]);
    assert_eq!(c3, 2, "--plan requires --stage and --sha");
    // no --run-id at all
    let (c4, _o, _e) = run_script(&["--plan", "--stage", "/tmp", "--sha", "S"]);
    assert_eq!(c4, 2, "missing --run-id");
}
