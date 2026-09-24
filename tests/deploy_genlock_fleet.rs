//! issue 789 (bod 4 + bod 5) — one deploy path for the OBS genlock build across strih-lx + stream
//! (+ imag / resolume when named) from ONE CI run id, plus deploy-*/obs-backup-* retention. issue
//! 1317 part 3 RETIRED the Windows `strih` arm (the STRIH-SNV PC is gone; 10.77.9.202 is the Linux
//! strih-lx) and moved the per-box constant table into `scripts/lib/genlock-fleet-boxes.sh`.
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

fn boxes_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/genlock-fleet-boxes.sh");
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
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
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
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
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
        run_sourced(&script(), "fleet_normalize_boxes imag,strih-lx,imag").trim(),
        "strih-lx,imag",
        "canonical order with dedup; imag stays a VALID explicit target (issue 1316)"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_normalize_boxes ''").trim(),
        "strih-lx,stream",
        "issue 1317 part 3: empty selection defaults to strih-lx,stream — the Windows strih PC is \
         RETIRED; imag RETIRED too (issue 1316), dropped from the empty-default"
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

/// fleet_pick_run_at_sha selects the FIRST successful run whose headSha matches — the heart of
/// "same canonical SHA across the separate windows/linux workflows".
#[test]
fn pick_run_at_sha_selects_first_successful_same_sha_run() {
    let json = r#"[{"databaseId":111,"headSha":"aaaa","conclusion":"failure"},{"databaseId":222,"headSha":"bbbb","conclusion":"success"},{"databaseId":333,"headSha":"aaaa","conclusion":"success"},{"databaseId":444,"headSha":"aaaa","conclusion":"success"}]"#;
    let out = run_sourced(
        &script(),
        &format!("printf '%s' '{json}' | fleet_pick_run_at_sha aaaa"),
    );
    assert_eq!(
        out.trim(),
        "333",
        "must pick the FIRST successful run at the SHA (not the failed one, not a different SHA)"
    );
    let out2 = run_sourced(
        &script(),
        &format!("printf '%s' '{json}' | fleet_pick_run_at_sha zzzz"),
    );
    assert!(
        out2.trim().is_empty(),
        "no successful run at the SHA -> empty (caller fails loud):\n{out2}"
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
// build_windows_deploy_program — the emitted PowerShell for the Windows boxes (stream / resolume).
// ============================================================================================

#[test]
fn windows_program_is_a_fail_loud_powershell_deploy() {
    let p = win_program("resolume", "full", "1");
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
        p.to_uppercase().contains("VERIFY FAIL"),
        "a byte mismatch must fail loud (VERIFY FAIL + non-zero exit):\n{p}"
    );
    // robocopy 0-7 is success — only >= 8 is failure
    assert!(
        p.contains("$LASTEXITCODE -ge 8"),
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
    let p = win_program("resolume", "full", "1");
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
/// issue 1115 — Option A: the FULL deploy ALSO ships the bundle's genlock distroav.dll to the REAL
/// ProgramData OBS load path (backup + fail-closed byte verify), so the loaded plugin IS the
/// canonical build and the byte-parity gather/compare against the manifest becomes real.
#[test]
fn windows_full_deploys_distroav_to_programdata_load_path_1115() {
    let p = win_program("resolume", "full", "1");
    // OBS loads DistroAV ONLY from the ProgramData bin\64bit path — the deploy must write THERE.
    assert!(
        p.contains(r"C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"),
        "FULL deploy must ship the bundle distroav.dll to the ProgramData load path:\n{p}"
    );
    // sourced from the staged bundle's obs-plugins\64bit\distroav.dll via an explicit CODE-side path
    // map -- anchor on the code line (Join-Path $stage '...'), not the loose basename which also
    // appears in the adjacent comment (review 🔵: keep the anchor code-unique, #1115).
    assert!(
        p.contains(r"Join-Path $stage 'obs-plugins\64bit\distroav.dll'"),
        "the deployed distroav.dll is sourced from the staged bundle obs-plugins\\64bit:\n{p}"
    );
    // the pre-deploy ProgramData distroav.dll is backed up alongside obs.dll.pre-789 (instant rollback)
    assert!(
        p.contains("distroav.dll.pre-789"),
        "must back up the pre-deploy ProgramData distroav.dll (rollback):\n{p}"
    );
    // fail-closed sha256 verify of the DEPLOYED ProgramData distroav.dll vs the manifest distroav entry
    assert!(
        p.contains("VERIFY distroav.dll"),
        "must byte-verify the deployed ProgramData distroav.dll (mirrors the obs.dll verify):\n{p}"
    );
    assert!(
        p.contains("VERIFY FAIL: deployed distroav.dll"),
        "a distroav byte mismatch must fail loud (VERIFY FAIL + non-zero exit):\n{p}"
    );
    // the Program Files obs-plugins copy STAYS /XF-excluded (a copy there is the shadow drift-guard flags)
    assert!(
        p.contains("/XF distroav.dll"),
        "the Program Files obs-plugins shadow copy stays /XF-excluded:\n{p}"
    );
}

/// issue 1115 — the FAST (obs.dll-only) bundle carries no distroav.dll, so a FAST deploy must never
/// touch the ProgramData distroav (only a FULL bundle deploy ships the plugin).
#[test]
fn windows_fast_does_not_touch_programdata_distroav_1115() {
    let p = win_program("stream", "fast", "0");
    assert!(
        !p.contains(r"C:\ProgramData\obs-studio\plugins\distroav"),
        "FAST (obs.dll-only) deploy must NOT touch the ProgramData distroav:\n{p}"
    );
    assert!(
        !p.contains("distroav.dll.pre-789"),
        "FAST deploy does not back up distroav (no distroav in the fast bundle):\n{p}"
    );
    assert!(
        !p.contains("VERIFY distroav.dll"),
        "FAST deploy has no distroav byte verify:\n{p}"
    );
}

// issue 1295: the AHK stop/restart bracket is emitted for any has_ahk=1 box (resolume — the
// retired Windows strih was the other one, issue 1317), never for stream (has_ahk=0). This test
// pins the AHK-box-vs-stream split.
#[test]
fn windows_ahk_bracket_on_ahk_box_not_stream() {
    let ahk_box = win_program("resolume", "full", "1");
    let stream = win_program("stream", "full", "0");
    assert!(
        ahk_box.contains("Stop-Process -Name AutoHotkey64"),
        "an AHK box runs the AHK watchdog — must be stopped before the copy:\n{ahk_box}"
    );
    // #789 review #1: an AHK box MUST restart AHK verified before exiting (leaving it running so the
    // STEP-2 launch-obs-genlock.sh session gate passes) — launch only restarts AHK it stopped itself.
    assert!(
        ahk_box.contains("ahkRelaunchVerified") && ahk_box.contains("exit 9"),
        "an AHK box must restart AHK VERIFIED and fail loud if it doesn't come back (#789 review #1):\n{ahk_box}"
    );
    assert!(
        !stream.contains("Stop-Process -Name AutoHotkey64"),
        "stream has NO AHK watcher — its program must carry no real AutoHotkey64 stop:\n{stream}"
    );
    assert!(
        !stream.contains("ahkRelaunchVerified"),
        "stream must carry no AHK restart (no watcher on this box):\n{stream}"
    );
}

/// --yes wires the retention deletion confirm through the emitted Windows program.
#[test]
fn windows_confirm_wires_retention_deletion() {
    // 10th arg = confirm=1
    let confirmed = run_sourced(
        &script(),
        "build_windows_deploy_program resolume full 'C:\\st' 'C:\\Program Files\\obs-studio' 1 'C:\\obs-backup' 3 SHA DSHA 1",
    );
    assert!(
        confirmed.contains("$fleetConfirmRetention = $true"),
        "confirm=1 must set $fleetConfirmRetention = $true:\n{confirmed}"
    );
    let default = win_program("resolume", "full", "1"); // confirm defaults to 0
    assert!(
        default.contains("$fleetConfirmRetention = $false"),
        "default (no --yes) must keep retention print-only ($false):\n{default}"
    );
}

#[test]
fn windows_program_prints_retention_plan_never_silent_delete() {
    let p = win_program("resolume", "full", "1");
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

/// #1140 — the per-box source of the OBS keep-alive SCHEDULED-TASK names a deploy must disable so
/// none respawns obs64 mid-copy. stream runs the #812 avsync-keepalive (~10 min) AND the #411
/// obs-self-heal (~2 min, the actual obs64 respawner), so BOTH are listed; resolume's keep-alive is
/// the AHK watcher (the has_ahk path), so it lists none. Curated per box — never all of a box's
/// scheduled tasks.
#[test]
fn fleet_box_keepalive_tasks_lists_stream_obs_keepalives_1140() {
    let stream = run_sourced(&script(), "fleet_box_keepalive_tasks stream");
    assert!(
        stream.contains("avsync-keepalive"),
        "stream must list the #812 avsync-keepalive task (the named minimum):\n{stream}"
    );
    assert!(
        stream.contains("camera-box-obs-self-heal-stream"),
        "stream must list the #411 obs-self-heal task (the 2-min obs64 respawner):\n{stream}"
    );
    let ahk_box = run_sourced(&script(), "fleet_box_keepalive_tasks resolume");
    assert!(
        ahk_box.trim().is_empty(),
        "resolume lists NO keep-alive scheduled task (its keep-alive is the AHK watcher):\n{ahk_box:?}"
    );
    let unknown = run_sourced(&script(), "fleet_box_keepalive_tasks nope");
    assert!(
        unknown.trim().is_empty(),
        "an unknown box lists no keep-alive tasks:\n{unknown:?}"
    );
}

/// #1140 — the stream deploy program must DISABLE the OBS keep-alive scheduled tasks BEFORE it
/// stops obs64 (so none respawns obs64 mid-robocopy → the 2026-08-19 ERROR 32 sharing violation)
/// and RE-ENABLE + VERIFY exactly those it disabled at the end, mirroring the strih AHK
/// stop→verified-restart contract. strih carries no such block (AHK watcher path only).
#[test]
fn windows_stream_disables_and_restores_obs_keepalive_tasks_1140() {
    let stream = win_program("stream", "full", "0");
    // the named tasks appear (avsync-keepalive at minimum; the obs-self-heal respawner too)
    assert!(
        stream.contains("'avsync-keepalive'"),
        "stream program must disable the avsync-keepalive task:\n{stream}"
    );
    assert!(
        stream.contains("'camera-box-obs-self-heal-stream'"),
        "stream program must disable the obs-self-heal task (the real obs64 respawner):\n{stream}"
    );
    // the disable half: an schtasks /DISABLE, gated on the task being PRESENT and ENABLED first
    assert!(
        stream.contains("schtasks /Change /TN $t /DISABLE"),
        "stream must schtasks /DISABLE each keep-alive task:\n{stream}"
    );
    assert!(
        stream.contains("Scheduled Task State"),
        "only a PRESENT+ENABLED task is disabled+restored (reads its state first):\n{stream}"
    );
    // the restore half: re-enable exactly the tasks it disabled, verified, fail-loud on a miss
    assert!(
        stream.contains("$disabledKeepAlive") && stream.contains("schtasks /Change /TN $t /ENABLE"),
        "stream must re-enable exactly the tasks it disabled:\n{stream}"
    );
    assert!(
        stream.contains("exit 10"),
        "a keep-alive task that does not come back enabled must fail loud (exit 10), mirroring AHK exit 9:\n{stream}"
    );
    // a present-but-unreadable task state must FAIL LOUD, never silently fail-open (else a live
    // keep-alive respawns obs64 -- the exact #1140 incident with no warning).
    assert!(
        stream.contains("could not read the Scheduled Task State"),
        "an unreadable task state must fail loud, never be treated as already-disabled:\n{stream}"
    );
    // disabling also /End's any in-flight instance (parity with the AHK Stop-Process actor kill).
    assert!(
        stream.contains("schtasks /End /TN $t"),
        "the disable step must also terminate an in-flight keep-alive instance (schtasks /End):\n{stream}"
    );
    // ORDER: disable BEFORE stopping obs64; restore at the tail (after the AHK restart step).
    let disable_at = stream.find("# (1b)").expect("no # (1b) disable step");
    let obs_stop_at = stream
        .find("Get-Process obs64,obs-browser-page")
        .expect("no obs64 stop line");
    let restore_at = stream.find("# (8b)").expect("no # (8b) restore step");
    let ahk_restart_at = stream.find("# (8)").expect("no # (8) restart step");
    assert!(
        disable_at < obs_stop_at,
        "keep-alive tasks must be disabled BEFORE obs64 is stopped (else one respawns it):\n{stream}"
    );
    assert!(
        restore_at > ahk_restart_at,
        "keep-alive restore (8b) comes at the tail, after the AHK restart step (8):\n{stream}"
    );
    // an AHK box carries NO scheduled-task keep-alive block (its keep-alive is the AHK watcher).
    let ahk_box = win_program("resolume", "full", "1");
    assert!(
        !ahk_box.contains("avsync-keepalive") && !ahk_box.contains("$disabledKeepAlive"),
        "an AHK box must carry no scheduled-task keep-alive handling (AHK watcher path only):\n{ahk_box}"
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
fn imag_program_ships_full_bundle_and_verifies_bytes() {
    // #789 review #2 (issue 1026): the imag deploy must ship the WHOLE bundle (every obs-plugins/*.so
    // with libobs — a hand-picked subset over a new libobs is a latent SIGSEGV), NOT a 4-file install.
    let p = imag_program();
    assert!(
        p.contains("cp -a \"$BUNDLE/lib/x86_64-linux-gnu/.\""),
        "installs the WHOLE bundle lib dir (all obs-plugins/*.so), never a hand-picked subset:\n{p}"
    );
    assert!(
        !p.contains("install -m 0644 -o root -g root \"$STAGE/libobs.so.30\""),
        "must NOT be the old 4-file hand-picked install (issue 1026):\n{p}"
    );
    // #789 review #5: sha256-verify the staged bytes against the manifest before installing.
    assert!(
        p.contains("verify_sha") && p.contains("sha256sum") && p.contains("BUNDLE_MANIFEST.json"),
        "must sha256-verify the staged files against the bundle manifest (fail-closed):\n{p}"
    );
    // #789 review #2: the mandatory 1026 WS filter-enum survival check is directed after restart.
    assert!(
        p.contains("obs_burn_filter.py check"),
        "must direct the 1026 WS filter-enum survival check after restart:\n{p}"
    );
}

/// issue 1236: the emitted on-imag deploy runs `cp -a "$BUNDLE/lib/x86_64-linux-gnu/." "$LIBDIR/"`,
/// and GNU `cp -a` with the `src/.` operand stamps the SOURCE dir's mode+ownership onto the
/// DESTINATION -- a 0700 newlevel mktemp staging dir made /usr/lib/x86_64-linux-gnu itself
/// drwx------ newlevel:newlevel and installed 0700 root:root libs, so a runtime uid could not open
/// libobs.so.30. The program must NORMALIZE the just-installed payload after the copy, robustly,
/// regardless of the staging dir's perms.
#[test]
fn imag_program_normalizes_installed_perms_after_cp_a_1236() {
    let p = imag_program();
    // reset the clobbered destination libdir itself to root:root 0755
    assert!(
        p.contains("chown root:root \"$LIBDIR\"") && p.contains("chmod 0755 \"$LIBDIR\""),
        "must reset $LIBDIR to root:root 0755 after the cp -a clobber (issue 1236):\n{p}"
    );
    // normalize the just-installed set: files a+rX (world-readable), scoped by walking the bundle
    // source tree -- never a whole-libdir sweep.
    assert!(
        p.contains("chmod a+rX \"$dst\"") && p.contains("find . -mindepth 1 -printf '%P\\0'"),
        "must set files a+rX over the just-installed set, scoped to the bundle tree (issue 1236):\n{p}"
    );
    // the sibling share/obs install (same cp -a src/. shape) is normalized too
    assert!(
        p.contains("chmod 0755 /usr/share/obs"),
        "must normalize the share/obs install too (issue 1236):\n{p}"
    );
    // the whole-bundle install contract (issue 1026) is preserved -- cp -a stays, normalize after.
    assert!(
        p.contains("cp -a \"$BUNDLE/lib/x86_64-linux-gnu/.\""),
        "keeps the whole-bundle cp -a install (issue 1026) -- normalize after, do not drop it:\n{p}"
    );
}

/// issue 1236: after normalizing, the emitted program must FAIL CLOSED (refuse the supervised
/// restart) if the destination libdir is not root:root 0755 or any just-installed lib is
/// world-unreadable -- the same fail-loud spirit as the SONAME/manifest guards.
#[test]
fn imag_program_fail_closed_perms_assert_1236() {
    let p = imag_program();
    assert!(
        p.contains("stat -c '%U:%G' \"$LIBDIR\"") && p.contains("stat -c '%a' \"$LIBDIR\""),
        "must stat-assert $LIBDIR owner+mode after install (issue 1236):\n{p}"
    );
    assert!(
        p.contains("want root:root") && p.contains("want 755"),
        "must assert $LIBDIR is root:root 0755 (issue 1236):\n{p}"
    );
    // scan the just-installed set for any file lacking the world-read bit
    assert!(
        p.contains("-perm -o+r"),
        "must scan the just-installed set for world-unreadable files (issue 1236):\n{p}"
    );
    // the assert covers the WHOLE just-installed set: dirs world-traversable (o+rx) + ownership,
    // over BOTH the lib tree and the share/obs data subtree -- not just $LIBDIR + top-level file o+r.
    assert!(
        p.contains("-perm -o+rx"),
        "must assert installed dirs are world-traversable (issue 1236):\n{p}"
    );
    assert!(
        p.contains("assert_installed_perms \"$BUNDLE/lib/x86_64-linux-gnu\" \"$LIBDIR\"")
            && p.contains("assert_installed_perms \"$BUNDLE/share/obs\" /usr/share/obs"),
        "must run the fail-closed perms assert over BOTH the lib tree and the share/obs subtree (issue 1236):\n{p}"
    );
    // refuse the restart on any violation
    assert!(
        p.contains("post-install perms assert") && p.contains("exit 4"),
        "must exit 4 (refuse the restart) on a perms violation (issue 1236):\n{p}"
    );
}

#[test]
fn imag_program_keeps_the_abi_guards_and_graceful_restart() {
    let p = imag_program();
    assert!(
        p.contains("readelf") && p.contains("SONAME"),
        "SONAME sanity check — refuse a mismatched ABI:\n{p}"
    );
    // #789 review #5: BOTH libobs AND the separate libobs-opengl (#756) get a SONAME check.
    assert!(
        p.contains("libobs\\.so\\.30") && p.contains("libobs-opengl\\.so\\.30"),
        "SONAME check must cover BOTH libobs.so.30 and libobs-opengl.so.30 (#756):\n{p}"
    );
    assert!(
        p.contains("obs_display_set_render_divisor"),
        "nm build-proof — refuse a stock/wrong frontend (#499):\n{p}"
    );
    // #789 handoff residual: restart routes THROUGH the durable systemd user unit (a raw background
    // launch put OBS outside the unit cgroup and died in ~21s live, 2026-08-18), then records the
    // restarted obs for the #912 start-time race.
    assert!(
        p.contains("systemctl --user") && p.contains("stop imag-obs.service"),
        "graceful stop routes through the systemd user unit:\n{p}"
    );
    assert!(
        p.contains("restart imag-obs.service"),
        "start routes through the systemd user unit (restart), never a raw launch:\n{p}"
    );
    assert!(
        p.contains("lstart") || p.contains("etimes"),
        "still records the restarted obs process (start-time race, #912):\n{p}"
    );
}

/// #789 handoff residual: the first live fleet run (2026-08-18) raw-launched imag-obs-start.sh
/// OUTSIDE the imag-obs.service cgroup (no Restart=on-failure, ExecStop bypassed, launch tied to
/// the ssh session) and it died in ~21s. The imag deploy leg must instead HAND OFF to the durable
/// systemd USER unit + verify it (active + cgroup + render-tick), never a session-tied raw launch.
#[test]
fn imag_program_restarts_through_the_systemd_unit_not_a_raw_launch_789() {
    let p = imag_program();
    // relaunch = systemctl --user restart imag-obs.service (a USER unit, issue 998 -> XDG_RUNTIME_DIR).
    assert!(
        p.contains("systemctl --user") && p.contains("restart imag-obs.service"),
        "must relaunch through `systemctl --user restart imag-obs.service`:\n{p}"
    );
    assert!(
        p.contains("XDG_RUNTIME_DIR"),
        "a USER unit over non-login ssh needs XDG_RUNTIME_DIR exported (issue 998):\n{p}"
    );
    // NEVER a session-tied raw launch that escapes the unit cgroup.
    assert!(
        !p.contains("setsid") && !p.contains("nohup"),
        "must NOT raw-launch OBS with setsid/nohup (escapes systemd supervision):\n{p}"
    );
    // bounded active-verify + the cgroup discriminator (systemd bookkeeping can say active while the
    // real obs sits OUTSIDE the unit cgroup, launched by a bypassed path -- verify-imag.sh issue 1015).
    assert!(
        p.contains("is-active"),
        "must poll `systemctl --user is-active` after the restart (bounded active-verify):\n{p}"
    );
    assert!(
        p.contains("/proc/") && p.contains("cgroup") && p.contains("imag-obs.service"),
        "must verify the running obs pid lives inside the imag-obs.service cgroup:\n{p}"
    );
    // render-tick log verify the launch contract demands: the unit's ExecStart (imag-obs-start.sh)
    // prints 'OK: OBS bezi' to /tmp/imag-obs-start.log only after WS up + scenes seeded.
    assert!(
        p.contains("imag-obs-start.log") && p.contains("OK: OBS bezi"),
        "must render-tick verify via /tmp/imag-obs-start.log reaching 'OK: OBS bezi':\n{p}"
    );
    // fail-loud (never a silent WARN) if the supervised restart never comes up.
    assert!(
        p.contains("IMAG FAIL") && p.contains("exit 4"),
        "must fail loud (exit 4) if the supervised unit does not come up:\n{p}"
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
    // default (confirm=0) is print-only ("would delete"), never a silent rm.
    assert!(
        p.contains("would delete") && !p.contains("rm -rf \"$d\""),
        "default imag retention lists candidates, never deletes:\n{p}"
    );
}

/// --yes (confirm=1) wires the imag retention to actually delete the stale backup dirs.
#[test]
fn imag_program_confirm_deletes_stale_backups() {
    let p = run_sourced(
        &script(),
        "build_imag_deploy_program /tmp/genlock-stage-RUN /opt/obs-genlock /opt/obs-backup SHA789 DSHA789 3 1",
    );
    assert!(
        p.contains("rm -rf \"$d\""),
        "confirm=1 must actually delete the stale backup dirs:\n{p}"
    );
}

// ============================================================================================
// fleet_log_line + the --plan flow.
// ============================================================================================

#[test]
fn fleet_log_line_is_one_tab_separated_record() {
    let line = run_sourced(
        &script(),
        "fleet_log_line RUN123 abcdef1 strih-lx,stream,imag full",
    );
    let line = line.trim();
    assert!(line.contains('\t'), "tab-separated:\n{line}");
    for tok in ["RUN123", "abcdef1", "strih-lx,stream,imag", "full"] {
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
        "stream,imag",
        "--full",
    ]);
    assert_eq!(code, 0, "--plan must succeed.\nstdout={out}\nstderr={err}");
    // Windows (stream) program present
    assert!(
        out.contains("$ErrorActionPreference = 'Stop'"),
        "emits the stream PS program:\n{out}"
    );
    assert!(
        out.contains("win-stream-snv"),
        "names the stream MCP for the paste step:\n{out}"
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
    // resolume was NOT requested -> its MCP must not appear
    assert!(
        !out.contains("win-resolume"),
        "resolume not selected -> not in the plan:\n{out}"
    );
}

// #1303 part 4 — the report-only per-box forced-table AUDIO/yuv audit preflight is emitted BEFORE
// STEP 0 of each box's plan, references the classifier lib, and never writes/gates.
#[test]
fn plan_emits_forced_table_audit_preflight_before_step0_1303() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path();
    for boxes in ["resolume", "stream,imag"] {
        let (code, out, err) = run_script(&[
            "--plan",
            "--run-id",
            "RUN123",
            "--sha",
            "abcdef1234",
            "--stage",
            stage.to_str().unwrap(),
            "--boxes",
            boxes,
            "--full",
        ]);
        assert_eq!(
            code, 0,
            "--plan {boxes} must succeed.\nstdout={out}\nstderr={err}"
        );
        assert!(
            out.contains("PREFLIGHT (report-only, #1303 part 4)"),
            "emits the forced-table audit preflight for {boxes}:\n{out}"
        );
        assert!(
            out.contains("genlock_forced_table_audit"),
            "preflight pipes into the classifier for {boxes}:\n{out}"
        );
        assert!(
            out.contains("NEVER writes and NEVER gates"),
            "preflight is report-only for {boxes}:\n{out}"
        );
        // the preflight is a PRE-swap step: it must come before the box's REAL deploy program.
        // Anchor on the Windows program's first line ($ErrorActionPreference = 'Stop') -- both box
        // sets here include a Windows box and that string never appears in the preflight text, so
        // this is a genuine ordering check, not the preflight's own "STEP 0 below" phrase.
        let pf = out.find("PREFLIGHT (report-only, #1303 part 4)").unwrap();
        let deploy = out
            .find("$ErrorActionPreference = 'Stop'")
            .expect("plan carries a Windows deploy program");
        assert!(
            pf < deploy,
            "preflight must precede the deploy program for {boxes}:\n{out}"
        );
    }
    // one preflight per requested box (2 for stream,imag).
    let (_c, out2, _e) = run_script(&[
        "--plan",
        "--run-id",
        "RUN123",
        "--sha",
        "abcdef1234",
        "--stage",
        stage.to_str().unwrap(),
        "--boxes",
        "stream,imag",
        "--full",
    ]);
    assert_eq!(
        out2.matches("PREFLIGHT (report-only, #1303 part 4)")
            .count(),
        2,
        "one preflight per requested box:\n{out2}"
    );
}

// issue 1295 — a saved .ps1 of the --plan output is PARSED whole in file mode, so the plan must end
// on a clean `exit 0` and must NOT trail a bare (non-comment) tab-separated fleet-log record after
// the last box program's exit 0 (owner incident: "At C:\deploy2.ps1:170").
#[test]
fn plan_last_nonempty_line_is_exit_0_and_no_bare_log_record_1295() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path();
    // issue 1317 part 3: the strih-lx plan (all #-comment guidance) rides along the Windows boxes.
    for boxes in ["resolume", "strih-lx,stream,imag"] {
        let (code, out, err) = run_script(&[
            "--plan",
            "--run-id",
            "RUN123",
            "--sha",
            "abcdef1234",
            "--stage",
            stage.to_str().unwrap(),
            "--boxes",
            boxes,
            "--full",
        ]);
        assert_eq!(
            code, 0,
            "--plan {boxes} must succeed.\nstdout={out}\nstderr={err}"
        );
        let last = out
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .expect("plan has content");
        assert_eq!(
            last.trim(),
            "exit 0",
            "the plan's last non-empty line must be `exit 0` for {boxes}:\n...{}",
            &out[out.len().saturating_sub(400)..]
        );
        // the fleet-log record (a tab-separated line) must be COMMENTED — no bare record survives.
        for line in out.lines() {
            if line.contains('\t') {
                assert!(
                    line.trim_start().starts_with('#'),
                    "a tab-separated fleet-log record must be a #-comment (file-mode PS parse), got: {line:?}"
                );
            }
        }
        // the record itself is still present (run id + sha), just commented.
        assert!(
            out.contains("RUN123") && out.contains("abcdef1234"),
            "the fleet-log record stays visible:\n{out}"
        );
    }
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

// ============================================================================================
// #1295 -- RESOLUME-SNV (the traveling CG box) joins the genlock deploy fleet as a
// windows-genlock box: win-resolume MCP, has_ahk=0, hostname not a pinned IP, explicit-only
// (never the empty-default fleet), with a live box-identity confirm preamble.
// ============================================================================================
#[test]
fn normalize_accepts_resolume_explicit_only_not_default_1295() {
    // resolume is accepted + appended last in canonical order
    assert_eq!(
        run_sourced(
            &script(),
            "fleet_normalize_boxes strih-lx,stream,imag,resolume"
        )
        .trim(),
        "strih-lx,stream,imag,resolume"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_normalize_boxes resolume").trim(),
        "resolume"
    );
    // dedup
    assert_eq!(
        run_sourced(
            &script(),
            "fleet_normalize_boxes resolume,strih-lx,resolume"
        )
        .trim(),
        "strih-lx,resolume"
    );
    // the empty default is strih-lx,stream ONLY -- resolume (traveling maintenance box) is never
    // pulled into the whole-fleet default, and imag left it when imag-nb was RETIRED (issue 1316,
    // 16.9.2026); both deploy only when explicitly named.
    assert_eq!(
        run_sourced(&script(), "fleet_normalize_boxes ''").trim(),
        "strih-lx,stream"
    );
}

#[test]
fn resolume_box_constants_mcp_hostname_with_ahk_1295() {
    assert_eq!(
        run_sourced(&script(), "fleet_box_mcp resolume").trim(),
        "win-resolume"
    );
    // the "ip" is the HOSTNAME resolume.lan, NEVER a pinned literal IP (targets.md; .201 collides
    // with `bridge`).
    assert_eq!(
        run_sourced(&script(), "fleet_box_ip resolume").trim(),
        "resolume.lan"
    );
    // issue 1295 correction (supervisor pre-deploy inventory): RESOLUME-SNV RUNS an AutoHotkey v2
    // safe-loop (NL_STARTUP.ahk, SafeLoop:=1) that respawns OBS -- the SAME pattern the retired
    // Windows strih had -- so
    // has_ahk MUST be 1, or a deploy/relaunch that does not stop it first races a SECOND obs64.
    assert_eq!(
        run_sourced(&script(), "fleet_box_has_ahk resolume").trim(),
        "1"
    );
    // the per-box AHK identity: resolume's own v2 .ahk path (the traveling CG box), and it PREFERS
    // the Startup .lnk as the relaunch target (a future path move must not break the relaunch).
    assert_eq!(
        run_sourced(&script(), "fleet_box_ahk_script resolume").trim(),
        "C:\\Users\\Resolume\\Documents\\_NLMEDIA resolume\\_APPS\\NL_STARTUP.ahk"
    );
    assert_eq!(
        run_sourced(&script(), "fleet_box_ahk_prefer resolume").trim(),
        "lnk"
    );
    // issue 1317 part 3: the retired Windows strih has NO AHK identity any more, and a no-AHK box
    // (stream) never had one -- asking fails loud (rc 2, no output), never another box's script.
    for no_ahk in ["strih", "stream", "strih-lx"] {
        let (rc, out, _e) =
            run_sourced_status(&script(), &format!("fleet_box_ahk_script {no_ahk}"));
        assert_eq!(
            rc, 2,
            "fleet_box_ahk_script {no_ahk} must fail closed: {out:?}"
        );
        assert!(out.trim().is_empty(), "no AHK path for {no_ahk}: {out:?}");
        let (rc, out, _e) =
            run_sourced_status(&script(), &format!("fleet_box_ahk_prefer {no_ahk}"));
        assert_eq!(
            rc, 2,
            "fleet_box_ahk_prefer {no_ahk} must fail closed: {out:?}"
        );
    }
    // no OBS keep-alive SCHEDULED-TASK respawner on the CG box -- its respawner IS the AHK watcher
    // (handled by the has_ahk stop/restart path), so it lists no keepalive task.
    assert_eq!(
        run_sourced(&script(), "fleet_box_keepalive_tasks resolume").trim(),
        ""
    );
}

#[test]
fn resolume_windows_program_carries_ahk_and_no_keepalive_1295() {
    let p = win_program("resolume", "full", "1");
    // issue 1295: the AHK watcher MUST be stopped before the byte copy (it respawns obs64), and
    // restarted + VERIFIED afterward (the issue-789 restart guard).
    assert!(
        p.contains("Stop-Process -Name AutoHotkey64"),
        "resolume runs the AHK watcher -- its program must stop AutoHotkey64 before the copy:\n{p}"
    );
    assert!(
        p.contains("ahkRelaunchVerified") && p.contains("exit 9"),
        "resolume must restart AHK VERIFIED + fail loud if it doesn't come back (issue 789):\n{p}"
    );
    // the relaunch target is resolume's OWN .ahk path (never the retired strih's D:\_APPS path).
    assert!(
        p.contains(
            "$ahkScriptPath = 'C:\\Users\\Resolume\\Documents\\_NLMEDIA resolume\\_APPS\\NL_STARTUP.ahk'"
        ) && !p.contains("$ahkScriptPath = 'D:\\_APPS\\NL_STARTUP.ahk'"),
        "resolume's deploy program must relaunch via its OWN .ahk path, not the retired strih's:\n{p}"
    );
    assert!(
        !p.contains("schtasks /Change"),
        "resolume has no OBS keep-alive scheduled task -- no disable/enable:\n{p}"
    );
    // it is still a real fail-loud PowerShell deploy (the shared byte-verify / marker machinery)
    assert!(
        p.contains("$ErrorActionPreference = 'Stop'") && p.contains("VERIFY obs.dll"),
        "resolume rides the same fail-loud windows deploy program:\n{p}"
    );
}

#[test]
fn resolume_plan_emits_identity_confirm_and_win_resolume_mcp_1295() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (code, out, err) = run_script(&[
        "--plan",
        "--run-id",
        "R1",
        "--sha",
        "deadbeef",
        "--stage",
        tmp.path().to_str().unwrap(),
        "--boxes",
        "resolume",
        "--full",
    ]);
    assert_eq!(
        code, 0,
        "--plan resolume must succeed.\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("win-resolume"),
        "names the win-resolume MCP:\n{out}"
    );
    // the live box-IDENTITY confirm preamble (a traveling DHCP box colliding with `bridge` at .201)
    assert!(
        out.contains("box IDENTITY confirm") && out.contains("getent hosts resolume.lan"),
        "resolume plan emits the identity-confirm step:\n{out}"
    );
    // never a pinned IP in the resolume plan header
    assert!(
        out.contains("(win-resolume, resolume.lan)") && !out.contains("win-resolume, 10.77.9.201"),
        "resolume plan uses the hostname, never a pinned IP:\n{out}"
    );
}

#[test]
fn non_resolume_plan_has_no_identity_confirm_preamble_1295() {
    // the identity-confirm STEP -1 is resolume-only -- the stream / strih-lx plans must not carry it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_c, out, _e) = run_script(&[
        "--plan",
        "--run-id",
        "R1",
        "--sha",
        "deadbeef",
        "--stage",
        tmp.path().to_str().unwrap(),
        "--boxes",
        "strih-lx,stream",
        "--full",
    ]);
    assert!(
        out.contains("FLEET PLAN: box=stream") && out.contains("FLEET PLAN: box=strih-lx"),
        "the plan must actually carry the stream + strih-lx plans:\n{out}"
    );
    assert!(
        !out.contains("box IDENTITY confirm"),
        "stream / strih-lx plans must NOT carry the resolume-only identity-confirm preamble:\n{out}"
    );
}

/// #1295 follow-up C -- the #789 AHK-restart-failure Write-Error is emitted ONLY for has_ahk=1
/// boxes (resolume; the retired Windows strih was the other), which DO run an AHK respawn watcher;
/// the failure means the watcher did not come BACK after the deploy, not that the box lacks one.
/// The inherited text "<box> has NO respawn watcher" was wrong for every such box. It must say the
/// watcher failed to restart.
#[test]
fn ahk_restart_failure_message_does_not_falsely_claim_no_watcher_1295() {
    {
        let box_name = "resolume";
        let p = win_program(box_name, "full", "1");
        assert!(
            !p.contains("has NO respawn watcher"),
            "{box_name}: the #789 failure must not claim a has_ahk box 'has NO respawn watcher':\n{p}"
        );
        assert!(
            p.contains("failed to restart"),
            "{box_name}: the #789 failure must say the AHK respawn watcher failed to restart:\n{p}"
        );
        // the fail-loud exit is unchanged.
        assert!(
            p.contains("exit 9"),
            "{box_name}: the #789 AHK-restart failure must stay fail-loud (exit 9):\n{p}"
        );
    }
}

// issue 1317 (M4): strih-lx.lan has NO DNS entry on dev1, so the strih-lx dial default is the ONE
// fleet list's host for strih-lx (scripts/lib/obs-fleet.sh), never the unresolvable name.
// STRIH_LX_IP stays the explicit override.
#[test]
fn fleet_box_ip_strih_lx_defaults_to_the_fleet_list_host_1317() {
    let out = run_sourced(&script(), "unset STRIH_LX_IP; fleet_box_ip strih-lx");
    assert_eq!(out.trim(), "10.77.9.202");
    let out = run_sourced(&script(), "STRIH_LX_IP=10.1.2.3 fleet_box_ip strih-lx");
    assert_eq!(out.trim(), "10.1.2.3");
}

// issue 1317 review round 1: an unresolvable strih-lx fleet host fails CLOSED (rc 2, no output),
// never an empty dial address; the AHK fact is the shared obs-fleet one.
#[test]
fn fleet_box_ip_strih_lx_fails_closed_without_a_fleet_row_1317() {
    let (rc, out, _err) = run_sourced_status(
        &script(),
        "unset STRIH_LX_IP; OBS_FLEET='stream|10.77.9.204|windows-genlock|always'; fleet_box_ip strih-lx",
    );
    assert_eq!(rc, 2, "no strih-lx row must fail closed: out={out:?}");
    assert!(out.trim().is_empty(), "no empty dial address: {out:?}");
    let src = std::fs::read_to_string(boxes_lib()).unwrap();
    assert!(
        src.contains("obs_fleet_has_ahk"),
        "fleet_box_has_ahk must delegate to the shared obs_fleet_has_ahk fact"
    );
}

// ============================================================================================
// issue 1317 part 3 -- the Windows strih PC is RETIRED (M4 cut-over 20.9.2026; 10.77.9.202 is the
// Linux strih-lx). The deploy planner's box set comes from the fleet list, the Windows `strih` arm
// is gone, and the strih-lx plan is the provisioning recipe, never a Windows program.
// ============================================================================================

/// The retired Windows `strih` is refused BY NAME everywhere a Windows action could be emitted for
/// it: the box list, the per-box constants, the Windows builder, and the --plan CLI.
#[test]
fn retired_windows_strih_is_refused_everywhere_1317() {
    let (rc, out, err) = run_sourced_status(&script(), "fleet_normalize_boxes strih");
    assert_eq!(
        rc, 2,
        "a `strih` request must be a usage error: out={out:?}"
    );
    assert!(
        err.contains("RETIRED") && err.contains("strih-lx"),
        "the refusal must name the retirement + the replacement strih-lx: {err:?}"
    );
    for f in ["fleet_box_mcp", "fleet_box_ip"] {
        let (rc, out, _e) = run_sourced_status(&script(), &format!("{f} strih"));
        assert_eq!(
            rc, 2,
            "{f} strih must fail closed (no Windows strih row): {out:?}"
        );
        assert!(out.trim().is_empty(), "{f} strih prints nothing: {out:?}");
    }
    assert_eq!(
        run_sourced(&script(), "fleet_box_has_ahk strih").trim(),
        "0",
        "the retired strih carries no AHK fact any more"
    );
    // HAS_AHK=1 for a box with no AHK identity is a loud error, never a relaunch of another box's
    // script (the old builder silently fell back to the retired strih's D:\_APPS path).
    let (rc, out, err) = run_sourced_status(
        &script(),
        "build_windows_deploy_program stream full 'C:\\st' 'C:\\obs' 1 'C:\\obs-backup' 3 S D",
    );
    assert_eq!(rc, 2, "HAS_AHK=1 on a no-AHK box must fail: out={out:?}");
    assert!(
        err.contains("no AHK relaunch identity"),
        "the refusal names the missing identity: {err:?}"
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let (code, out, err) = run_script(&[
        "--plan",
        "--run-id",
        "R1",
        "--sha",
        "deadbeef",
        "--stage",
        tmp.path().to_str().unwrap(),
        "--boxes",
        "strih",
    ]);
    assert_eq!(
        code, 2,
        "--boxes strih must exit 2.\nstdout={out}\nstderr={err}"
    );
    assert!(!out.contains("win-strih"), "no Windows strih plan: {out}");
}

/// The DEFAULT deploy (no --boxes) is the production strih-lx + stream, and it never emits a
/// Windows program for 10.77.9.202 (the defect: the old default printed a win-strih robocopy/AHK
/// plan for the Linux notebook).
#[test]
fn default_plan_is_strih_lx_and_stream_never_a_windows_strih_1317() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (code, out, err) = run_script(&[
        "--plan",
        "--run-id",
        "R1",
        "--sha",
        "deadbeef",
        "--stage",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(
        code, 0,
        "default --plan must succeed.\nstdout={out}\nstderr={err}"
    );
    assert!(
        out.contains("boxes=strih-lx,stream"),
        "the default box set is strih-lx,stream:\n{out}"
    );
    assert!(
        out.contains("FLEET PLAN: box=strih-lx (ssh newlevel@10.77.9.202, linux-genlock)"),
        "the strih plan dials strih-lx at its fleet address:\n{out}"
    );
    assert!(
        out.contains("FLEET PLAN: box=stream (win-stream-snv, 10.77.9.204)"),
        "stream keeps its Windows plan:\n{out}"
    );
    assert!(
        !out.contains("win-strih") && !out.contains("box=strih ("),
        "no Windows strih plan in the default deploy:\n{out}"
    );
    // exactly ONE Windows deploy program (stream's) -- strih-lx gets none.
    assert_eq!(
        out.matches("$ErrorActionPreference = 'Stop'").count(),
        1,
        "only stream carries a Windows deploy program:\n{out}"
    );
}

/// The strih-lx plan prints the SAME steps the issue-1317 part-6 EXECUTE arm runs (built by the
/// same `scripts/lib/strih-lx-deploy.sh` builders): the same-SHA strih FULL artifact, the
/// obs-backup-retention `--local-sweep` of stale stages, the whole tree staged into
/// `/tmp/genlock-stage-<sha>/{bundle,repo}` BEFORE the graceful `strih-obs-stop.sh` stop,
/// setup-strih.sh with `STRIH_LX_BUNDLE_SRC=<stage>/bundle`, a supervised strih-obs.service start and
/// the fail-closed SHA read-back -- never the imag on-box program, which restarts imag-obs.service (a
/// unit strih-lx does not have). Every line is a #-comment so a saved whole-plan .ps1 still parses
/// (the issue-1295 rule).
#[test]
fn strih_lx_plan_is_the_setup_strih_recipe_not_the_imag_program_1317() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (code, out, err) = run_script(&[
        "--plan",
        "--run-id",
        "R1",
        "--sha",
        "deadbeef",
        "--stage",
        tmp.path().to_str().unwrap(),
        "--boxes",
        "strih-lx",
    ]);
    assert_eq!(
        code, 0,
        "--plan strih-lx must succeed.\nstdout={out}\nstderr={err}"
    );
    for want in [
        "scripts/deploy-genlock-fleet.sh --run-id R1 --boxes strih-lx",
        "obs-genlock-linux-x86_64-strih",
        "obs-backup-retention.sh",
        "--local-sweep --backup-root /opt/obs-backup --stage-parent /tmp --keep-runs 1 --keep-days 0 --execute",
        "touch '/tmp/genlock-stage-deadbeef'",
        "newlevel@10.77.9.202:/tmp/genlock-stage-deadbeef/bundle/",
        "newlevel@10.77.9.202:/tmp/genlock-stage-deadbeef/repo/",
        "STRIH_LX_BUNDLE_SRC=/tmp/genlock-stage-deadbeef/bundle",
        "/tmp/genlock-stage-deadbeef/repo/run-setup.sh",
        "ghtoken:",
        "/usr/local/bin/strih-obs-stop.sh",
        "systemctl --user start strih-obs.service",
        "/opt/obs-genlock/GENLOCK_BUILD_SHA.txt",
        "http://10.77.9.202:8899/bundle-state.json",
        "verify-strih.sh",
        "PREFLIGHT (report-only, #1303 part 4)",
    ] {
        assert!(
            out.contains(want),
            "the strih-lx plan must carry `{want}`:\n{out}"
        );
    }
    assert!(
        !out.contains("imag-obs.service") && !out.contains("box=imag"),
        "the strih-lx plan must never be the imag program:\n{out}"
    );
    assert!(
        !out.contains("nohup bash ")
            && !out.contains("kill -9")
            && !out.contains("strih-lx-deploy-repo"),
        "no `nohup bash` wrapper, no hard kill, no unswept fixed repo dir:\n{out}"
    );
    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() || t == "exit 0" {
            continue;
        }
        assert!(
            t.starts_with('#'),
            "every strih-lx plan line is #-comment guidance (file-mode PS parse), got: {line:?}"
        );
    }
    // STRIH_LX_IP stays the explicit dial override.
    let o = Command::new(script())
        .args([
            "--plan", "--run-id", "R1", "--sha", "deadbeef", "--stage", "/stage", "--boxes",
            "strih-lx",
        ])
        .env("STRIH_LX_IP", "10.9.9.9")
        .current_dir(manifest_dir())
        .output()
        .expect("run deploy-genlock-fleet.sh");
    let out2 = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out2.contains("ssh newlevel@10.9.9.9"),
        "the plan dials the STRIH_LX_IP override:\n{out2}"
    );
}

/// The per-box constant table lives in ONE sourced lib shared by the three Windows planners, so a
/// box is registered once (the deploy script was at 999 lines with the table inline).
#[test]
fn per_box_table_lives_in_the_shared_lib_1317() {
    let deploy = std::fs::read_to_string(script()).unwrap();
    let lib = std::fs::read_to_string(boxes_lib()).unwrap();
    for f in [
        "fleet_box_mcp()",
        "fleet_box_ip()",
        "fleet_strih_lx_ip()",
        "fleet_box_has_ahk()",
        "fleet_box_ahk_script()",
        "fleet_box_ahk_prefer()",
        "fleet_resolume_identity_confirm_note()",
        "fleet_box_keepalive_tasks()",
    ] {
        assert!(lib.contains(f), "the shared lib defines {f}");
        assert!(
            !deploy.contains(f),
            "deploy-genlock-fleet.sh must not redefine {f} inline"
        );
    }
    assert!(
        deploy.contains(". \"$HERE/lib/genlock-fleet-boxes.sh\""),
        "deploy-genlock-fleet.sh sources the shared per-box lib"
    );
    for other in [
        "scripts/launch-obs-genlock.sh",
        "scripts/obs-self-heal-install.sh",
    ] {
        let src = std::fs::read_to_string(manifest_dir().join(other)).unwrap();
        assert!(
            src.contains(". \"$HERE/lib/genlock-fleet-boxes.sh\""),
            "{other} reads the same per-box table"
        );
    }
    assert!(
        !lib.contains("win-strih") && !lib.contains("10.77.9.202"),
        "the shared table has no Windows strih entry and no literal .202"
    );
    assert!(
        deploy.lines().count() < 1000,
        "deploy-genlock-fleet.sh stays under the 1000-line budget"
    );
}
