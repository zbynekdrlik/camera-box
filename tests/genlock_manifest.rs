//! Behavioral guard for the genlock-bundle SHA-manifest generator `scripts/genlock-manifest.sh` (#120).
//!
//! EPIC #125 / #119: the windows-genlock artifact once shipped a STALE prebuilt DistroAV (a pre-#97
//! build of the right *version* — the preload knob was inert) and the marketing-version pins (#45
//! drift-guard) could not catch it. #120's fix is a per-component SHA manifest shipped INSIDE the
//! bundle that records (a) the pinned SOURCE of every rebuilt component and (b) the per-file sha256
//! of every file actually in the bundle, SELF-CONSISTENT with what ships. #121 later byte/SHA-verifies
//! a deployed stack against this manifest.
//!
//! The 150-min windows-genlock.yml build is `workflow_dispatch`-only and cannot run per-PR, so the
//! manifest-generation LOGIC is proven HERE (the per-PR Linux `test` job): these tests source the
//! REAL script (its `BASH_SOURCE != $0` guard skips the executed flow), drive it against a synthetic
//! stage dir mirroring the real OBS rundir layout, and assert the produced manifest lists every
//! component with its SHA, is self-consistent with the bundle, and that the self-consistency check
//! has teeth (catches sha drift, extra files, missing files). Same convention as tests/drift_guard.rs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/genlock-manifest.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Run the script as a subprocess; return (exit_code, stdout, stderr).
fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run genlock-manifest.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Build a synthetic staged bundle mirroring the windows-genlock.yml stage/ layout: the OBS rundir
/// (bin/64bit/obs64.exe + obs.dll), the DistroAV plugin (obs-plugins/64bit/distroav.dll), its data,
/// and the existing GENLOCK_BUILD_SHA.txt. Returns the stage dir path (inside a tempdir kept alive
/// by the returned guard).
fn make_stage() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path().join("stage");
    let write = |rel: &str, content: &str| {
        let p = stage.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    };
    write("bin/64bit/obs64.exe", "fake-obs-frontend-binary");
    write("bin/64bit/obs.dll", "fake-libobs-core");
    write("obs-plugins/64bit/distroav.dll", "fake-distroav-plugin");
    write(
        "data/obs-plugins/distroav/locale/en-US.ini",
        "Name=\"DistroAV\"",
    );
    write("GENLOCK_BUILD_SHA.txt", "deadbeefcafef00d\n");
    (tmp, stage)
}

fn generate(stage: &Path) -> (i32, String, PathBuf) {
    let out = stage.join("BUNDLE_MANIFEST.json");
    let (code, stdout, stderr) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--build-sha",
        "0123456789abcdef0123456789abcdef01234567",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 0,
        "generate failed.\nstdout={stdout}\nstderr={stderr}"
    );
    (code, stdout, out)
}

#[test]
fn manifest_lists_every_component_with_pinned_version_and_commit() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);
    let m = fs::read_to_string(&out).unwrap();

    // The three components the EPIC names: OBS, DistroAV, NDI runtime.
    assert!(
        m.contains("\"name\": \"obs-studio\""),
        "OBS component missing:\n{m}"
    );
    assert!(
        m.contains("\"name\": \"distroav\""),
        "DistroAV component missing:\n{m}"
    );
    assert!(
        m.contains("\"name\": \"ndi-runtime\""),
        "NDI runtime component missing:\n{m}"
    );

    // Each rebuilt component carries its pinned version + the vendored subtree commit, read from the
    // REAL vendor/README.md pin table (single source of truth shared with drift-guard.sh). These are
    // the current pins; if a /update-av-stack bump re-pins them, the assertion documents the change.
    assert!(
        m.contains("\"pinned_version\": \"32.1.2\""),
        "OBS pinned_version 32.1.2 missing:\n{m}"
    );
    assert!(
        m.contains("\"pinned_commit\": \"fb4d98bf8\""),
        "OBS pinned_commit missing:\n{m}"
    );
    assert!(
        m.contains("\"pinned_version\": \"6.2.1\""),
        "DistroAV pinned_version 6.2.1 missing:\n{m}"
    );
    assert!(
        m.contains("\"pinned_commit\": \"038d9d6\""),
        "DistroAV pinned_commit missing:\n{m}"
    );
    assert!(
        m.contains("\"min_version\": \"6.3.0\""),
        "NDI min_version 6.3.0 missing:\n{m}"
    );

    // The bundle's monorepo build SHA is recorded (what #121 anchors the deployed bytes to).
    assert!(
        m.contains("\"build_sha\": \"0123456789abcdef0123456789abcdef01234567\""),
        "build_sha missing:\n{m}"
    );
    assert!(
        m.contains("\"rebuilt_from_source\": true"),
        "rebuilt-from-source flag missing:\n{m}"
    );
}

#[test]
fn manifest_lists_every_staged_file_with_a_sha256() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);
    let m = fs::read_to_string(&out).unwrap();

    // Every shipped file appears with a 64-hex sha256. The manifest never lists ITSELF.
    for f in [
        "bin/64bit/obs64.exe",
        "bin/64bit/obs.dll",
        "obs-plugins/64bit/distroav.dll",
        "data/obs-plugins/distroav/locale/en-US.ini",
        "GENLOCK_BUILD_SHA.txt",
    ] {
        assert!(
            m.contains(&format!("\"path\": \"{f}\"")),
            "file {f} not listed:\n{m}"
        );
    }
    assert!(
        !m.contains("\"path\": \"BUNDLE_MANIFEST.json\""),
        "manifest must not list itself:\n{m}"
    );

    // The sha256 recorded for distroav.dll must be the ACTUAL hash of the staged file (this is the
    // anti-#119 property: the manifest pins the BYTES, not just the version). Compute it independently.
    let real = {
        let (code, sout, serr) = run_script(&[]); // no-op to ensure script is executable; ignore
        let _ = (code, sout, serr);
        // sha256 of "fake-distroav-plugin" — compute via sha256sum to avoid hardcoding.
        let o = Command::new("sha256sum")
            .arg(stage.join("obs-plugins/64bit/distroav.dll"))
            .output()
            .expect("sha256sum");
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    };
    assert!(
        m.contains(&format!("\"sha256\": \"{real}\"")),
        "distroav.dll sha256 {real} not in manifest:\n{m}"
    );
}

#[test]
fn produced_manifest_is_self_consistent_with_the_bundle() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);

    let (code, sout, serr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 0,
        "freshly-generated manifest must self-check clean.\nstdout={sout}\nstderr={serr}"
    );
}

#[test]
fn check_fails_on_sha_drift_a_tampered_file() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);

    // Tamper one staged file AFTER the manifest was generated — the recorded sha256 no longer matches.
    fs::write(
        stage.join("obs-plugins/64bit/distroav.dll"),
        "STALE-PRE-97-DISTROAV",
    )
    .unwrap();

    let (code, _sout, serr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 21,
        "tampered bundle must be flagged INCONSISTENT (#119 anti-regression)"
    );
    assert!(
        serr.contains("sha256 drift"),
        "expected a sha256-drift report, got:\n{serr}"
    );
}

#[test]
fn check_fails_on_an_un_manifested_extra_file() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);

    // A file shows up in the bundle that the manifest does not list — un-audited bytes must fail.
    fs::write(stage.join("obs-plugins/64bit/rogue.dll"), "unlisted").unwrap();

    let (code, _sout, serr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 21,
        "an un-manifested staged file must be flagged INCONSISTENT"
    );
    assert!(
        serr.contains("not in manifest"),
        "expected a not-in-manifest report, got:\n{serr}"
    );
}

#[test]
fn check_fails_when_a_listed_file_is_missing_from_the_bundle() {
    let (_tmp, stage) = make_stage();
    let (_, _stdout, out) = generate(&stage);

    // A file the manifest lists is absent from the bundle — a truncated/partial bundle must fail.
    fs::remove_file(stage.join("GENLOCK_BUILD_SHA.txt")).unwrap();

    let (code, _sout, serr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 21,
        "a manifest entry with no matching bundle file must be flagged INCONSISTENT"
    );
    assert!(
        serr.contains("absent from the bundle"),
        "expected an absent-file report, got:\n{serr}"
    );
}

#[test]
fn distroav_version_comes_from_the_vendored_buildspec_not_just_the_readme() {
    // The anti-#119 source-of-truth: the DistroAV version recorded must be the VENDORED buildspec's
    // version (what actually built), not only the human-declared README pin. Point --buildspec at a
    // synthetic buildspec with a DIFFERENT version and assert the manifest follows the buildspec AND
    // records the README pin separately so a drift is visible in-manifest.
    let (_tmp, stage) = make_stage();
    let bs = stage.join("fake-buildspec.json");
    fs::write(
        &bs,
        "{\n    \"name\": \"distroav\",\n    \"version\": \"9.9.9\"\n}\n",
    )
    .unwrap();
    let out = stage.join("BUNDLE_MANIFEST.json");

    let (code, sout, serr) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--buildspec",
        bs.to_str().unwrap(),
        "--build-sha",
        "abc",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "generate failed.\nstdout={sout}\nstderr={serr}");
    let m = fs::read_to_string(&out).unwrap();
    assert!(
        m.contains("\"pinned_version\": \"9.9.9\""),
        "DistroAV version must come from the vendored buildspec (9.9.9):\n{m}"
    );
    assert!(
        m.contains("\"pinned_version_readme\": \"6.2.1\""),
        "the README pin (6.2.1) must still be recorded for drift visibility:\n{m}"
    );
}
