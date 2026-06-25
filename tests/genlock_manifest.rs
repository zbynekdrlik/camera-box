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

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the executed flow) and run `body`
/// against its pure functions. Returns (exit_code, stdout, stderr) — used to drive the
/// completeness-guard helper directly with controlled counts (a generate bug can't be injected from
/// the outside, so the guard is exercised as the unit it is). Same convention as
/// tests/launch_obs_genlock.rs::run_sourced.
fn run_sourced(body: &str) -> (i32, String, String) {
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

#[test]
fn empty_stage_generates_and_self_checks_clean_under_set_e() {
    // Robustness (review #1): a degenerate stage with NO files must not abort the script under
    // `set -euo pipefail` (the empty grep/find no-match). It produces a manifest with an empty
    // files[] and self-checks clean — never a spurious exit-1 that would fail the build.
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path().join("stage");
    fs::create_dir_all(&stage).unwrap();
    let out = stage.join("BUNDLE_MANIFEST.json");

    let (gen_code, gout, gerr) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--build-sha",
        "abc",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        gen_code, 0,
        "empty-stage generate must exit 0.\nstdout={gout}\nstderr={gerr}"
    );

    let m = fs::read_to_string(&out).unwrap();
    assert!(
        m.contains("\"files\": ["),
        "empty manifest still has a files[] array:\n{m}"
    );

    let (chk_code, cout, cerr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        chk_code, 0,
        "empty-stage manifest must self-check clean.\nstdout={cout}\nstderr={cerr}"
    );
}

/// Build a synthetic stage with `n` files spread across the real bundle's nested layout
/// (bin/64bit/*.dll + data/libobs/*.effect), mirroring the ~2000-file windows-genlock bundle.
fn make_large_stage(n: usize) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path().join("stage");
    fs::create_dir_all(stage.join("bin/64bit")).unwrap();
    fs::create_dir_all(stage.join("data/libobs")).unwrap();
    fs::write(stage.join("GENLOCK_BUILD_SHA.txt"), "deadbeef\n").unwrap();
    let half = n / 2;
    for i in 0..half {
        fs::write(
            stage.join(format!("bin/64bit/lib{i}.dll")),
            format!("dll-{i}"),
        )
        .unwrap();
    }
    for i in half..n {
        fs::write(
            stage.join(format!("data/libobs/eff{i}.effect")),
            format!("eff-{i}"),
        )
        .unwrap();
    }
    (tmp, stage)
}

#[test]
fn check_is_consistent_on_a_large_real_sized_bundle() {
    // REGRESSION (#239): the real windows-genlock bundle is ~2000 files. The original
    // check_consistency ran `printf '%s\n' "$listed" | grep -qxF "$rel"` ONCE PER staged file;
    // `grep -q` exits early on a match, sending SIGPIPE to the upstream `printf`, and under
    // `set -euo pipefail` that SIGPIPE nondeterministically poisoned the pipeline's exit status —
    // so ~half the files were falsely reported "staged file not in manifest" and the build failed
    // with exit 21 on a PERFECTLY VALID, complete manifest. The tiny 5-file synthetic stages above
    // never hit the race; a real-sized stage does. This locks the fix: a freshly-generated manifest
    // over a ~1500-file bundle MUST self-check clean, repeatably (the race was nondeterministic, so
    // check several times to catch a regression that only fails some of the time).
    let (_tmp, stage) = make_large_stage(1500);
    let out = stage.join("BUNDLE_MANIFEST.json");
    let (gen_code, gout, gerr) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--build-sha",
        "abc",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        gen_code, 0,
        "large-stage generate must exit 0.\nstdout={gout}\nstderr={gerr}"
    );

    for attempt in 0..5 {
        let (code, cout, cerr) = run_script(&[
            "--check",
            out.to_str().unwrap(),
            "--stage",
            stage.to_str().unwrap(),
        ]);
        assert_eq!(
            code, 0,
            "large-bundle self-check must be clean (attempt {attempt}); the SIGPIPE/pipefail \
             race (#239) made this fail nondeterministically.\nstdout={cout}\nstderr={cerr}"
        );
    }
}

#[test]
fn check_still_catches_an_extra_file_in_a_large_bundle() {
    // The fix must not weaken the gate: even on a large bundle, an un-manifested extra file is still
    // flagged INCONSISTENT (exit 21) — the anti-#119 "no un-audited bytes ship" property holds at scale.
    let (_tmp, stage) = make_large_stage(1500);
    let out = stage.join("BUNDLE_MANIFEST.json");
    let (gen_code, _g, _e) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--build-sha",
        "abc",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(gen_code, 0, "large-stage generate must exit 0");

    fs::write(stage.join("bin/64bit/rogue.dll"), "unlisted").unwrap();
    let (code, _c, cerr) = run_script(&[
        "--check",
        out.to_str().unwrap(),
        "--stage",
        stage.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 21,
        "an un-manifested extra file in a large bundle must still fail"
    );
    assert!(
        cerr.contains("not in manifest"),
        "expected a not-in-manifest report, got:\n{cerr}"
    );
}

#[test]
fn generate_verifies_manifest_completeness_at_generation_loud() {
    // HARDENING (#239 follow-up): the failed Windows build's reported symptom looked like the
    // generate loop had stopped partway and the manifest was missing files. It hadn't (the real
    // cause was the SIGPIPE checker race, fixed above) — but a TRUNCATED generate is a real risk on
    // git-bash (a process-substitution / find stream cut short) and MUST fail LOUD at generation, so
    // a partial manifest can never silently reach the later --check. The generator now counts the
    // staged files it intends to manifest vs the files[] entries it actually wrote, and emits a
    // completeness line; a healthy generate reports N == N.
    let (_tmp, stage) = make_large_stage(1500);
    let out = stage.join("BUNDLE_MANIFEST.json");
    let (gen_code, gout, gerr) = run_script(&[
        "--stage",
        stage.to_str().unwrap(),
        "--build-sha",
        "abc",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        gen_code, 0,
        "complete-bundle generate must exit 0.\nstdout={gout}\nstderr={gerr}"
    );
    // The completeness guard prints both counts; on a healthy bundle they are equal and non-zero.
    // 1500 files + GENLOCK_BUILD_SHA.txt = 1501 staged (the manifest never lists itself).
    let combined = format!("{gout}{gerr}");
    assert!(
        combined.contains("manifest completeness")
            && combined.contains("1501 staged")
            && combined.contains("1501 manifested"),
        "generate must report a verified completeness count (1501 == 1501).\nstdout={gout}\nstderr={gerr}"
    );
}

#[test]
fn completeness_guard_fails_loud_on_a_count_mismatch() {
    // The guard's TEETH: a generate bug can't be injected from outside, so drive the pure
    // completeness helper directly with a mismatch (expected 1501, wrote 800 — a truncated stream).
    // It MUST exit non-zero AND name both counts on stderr, never silently continue. This is the
    // independent backstop the dispatch asked for: a partial manifest dies AT generation, loud.
    let (code, sout, serr) = run_sourced("assert_manifest_complete 1501 800");
    assert_ne!(
        code, 0,
        "a 1501-vs-800 completeness mismatch must FAIL the generate.\nstdout={sout}\nstderr={serr}"
    );
    let combined = format!("{sout}{serr}");
    assert!(
        combined.contains("1501") && combined.contains("800"),
        "the failure must name both counts so the truncation is debuggable.\nstdout={sout}\nstderr={serr}"
    );
    // A matching count passes (the helper is not a constant failure).
    let (ok_code, _o, _e) = run_sourced("assert_manifest_complete 1501 1501");
    assert_eq!(ok_code, 0, "a matching completeness count must pass");
}

#[test]
fn generate_fails_loud_when_a_per_file_sha_or_size_is_empty() {
    // BUG (#239 review, cmdsub set -e suppression): main() calls generate as
    // `manifest="$(generate_manifest …)"`. A command substitution on the RHS of an assignment
    // SUPPRESSES `set -e` inside the substituted command, so a mid-loop `sha256_of`/`wc -c` failure
    // (a staged file briefly unreadable — concurrent process, Windows AV lock, transient I/O) does
    // NOT abort: the loop emits `{ "path": "x", "sha256": "", "size":  }` (empty sha AND a value-less
    // size = INVALID JSON), `written` still increments, the count guard sees expected==written and
    // says OK, and the corrupt manifest is written with exit 0. The completeness count is blind to it
    // (every PATH is present; only the sha/size are empty). Each emitted entry must therefore be
    // validated explicitly — a non-empty 64-hex sha + a numeric size — and a bad one must FAIL LOUD,
    // independent of the suppressed `set -e`.
    let (bad_sha, sout, serr) = run_sourced("assert_entry_valid 'b.txt' '' '3'");
    assert_ne!(
        bad_sha, 0,
        "an empty sha256 for a staged file must FAIL the generate.\nstdout={sout}\nstderr={serr}"
    );
    assert!(
        format!("{sout}{serr}").contains("b.txt"),
        "the failure must name the offending file.\nstdout={sout}\nstderr={serr}"
    );
    let (bad_size, _o2, _e2) = run_sourced("assert_entry_valid 'b.txt' '5695d82a086b677962a0b0428ed1a213208285b7b40d7d3604876d36a710302a' ''");
    assert_ne!(bad_size, 0, "an empty size for a staged file must FAIL the generate");
    let (bad_size2, _o3, _e3) = run_sourced("assert_entry_valid 'b.txt' '5695d82a086b677962a0b0428ed1a213208285b7b40d7d3604876d36a710302a' 'notanumber'");
    assert_ne!(bad_size2, 0, "a non-numeric size must FAIL the generate");
    // A valid 64-hex sha + numeric size passes (the helper is not a constant failure).
    let (ok, _o4, _e4) = run_sourced(
        "assert_entry_valid 'b.txt' '5695d82a086b677962a0b0428ed1a213208285b7b40d7d3604876d36a710302a' '4'",
    );
    assert_eq!(ok, 0, "a valid sha+size must pass");
}
