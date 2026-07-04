//! Functional (execution) guard for `scripts/setup-imag.sh`'s pure #460 integrity-check helpers
//! (`manifest_sha_for_path` / `verify_file_sha`), #458.
//!
//! `tests/setup_imag_guards.rs` pins the CONTRACT textually (the right call sites, the right
//! paths, the right fail-loud messages) but — as three independent review passes on this exact
//! PR pointed out — a purely textual guard cannot catch a silent LOGIC inversion (e.g.
//! `[ "$got" = "$want" ]` flipped to `!=`, or a broken `jq` `select`) that leaves every pinned
//! string unchanged. This file closes that gap by actually SOURCING the real script and RUNNING
//! its pure functions against synthetic fixtures — same convention as
//! `tests/genlock_manifest.rs::run_sourced` / `tests/launch_obs_genlock.rs::run_sourced`.
//!
//! setup-imag.sh's `BASH_SOURCE[0] != $0` guard (near the top of the file, right after the pure
//! function definitions) makes sourcing safe: everything below the guard (the destructive
//! one-shot provisioning flow — `[ "$EUID" -eq 0 ] || fail ...`, `apt-get`, network downloads,
//! etc.) is skipped, and only `fail()` + the two pure functions are defined in the sourcing shell.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup-imag.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the destructive provisioning
/// flow) and run `body` against its pure functions. Returns (exit_code, stdout, stderr).
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

/// Build a synthetic manifest + staged files: two real files with KNOWN content, a manifest.json
/// listing each with its true sha256. Returns (tempdir guard, manifest path, libobs path,
/// distroav path, libobs sha, distroav sha).
fn make_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path().join("stage");
    fs::create_dir_all(&stage).unwrap();

    let libobs = stage.join("libobs.so.30");
    let distroav = stage.join("distroav.so");
    fs::write(&libobs, b"fake-genlock-libobs-bytes").unwrap();
    fs::write(&distroav, b"fake-genlock-distroav-bytes").unwrap();

    let libobs_sha = sha256_hex(&fs::read(&libobs).unwrap());
    let distroav_sha = sha256_hex(&fs::read(&distroav).unwrap());

    let manifest = stage.join("BUNDLE_MANIFEST.json");
    fs::write(
        &manifest,
        format!(
            r#"{{"files":[
                {{"path":"lib/x86_64-linux-gnu/libobs.so.30","sha256":"{libobs_sha}","size":26}},
                {{"path":"lib/x86_64-linux-gnu/obs-plugins/distroav.so","sha256":"{distroav_sha}","size":28}}
            ]}}"#
        ),
    )
    .unwrap();

    (tmp, manifest, libobs, distroav, libobs_sha, distroav_sha)
}

/// Minimal, dependency-free sha256 via the real `sha256sum` binary — avoids pulling a crypto
/// crate into dev-dependencies just for test fixture setup; `sha256sum` is already a hard
/// dependency of the script under test.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sha256sum");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(bytes)
        .expect("write to sha256sum stdin");
    let out = child.wait_with_output().expect("sha256sum output");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output has a hash field")
        .to_string()
}

#[test]
fn manifest_sha_for_path_returns_the_correct_sha_for_a_real_entry() {
    let (_tmp, manifest, _libobs, _distroav, libobs_sha, _distroav_sha) = make_fixture();
    let (code, stdout, stderr) = run_sourced(&format!(
        "manifest_sha_for_path {:?} 'lib/x86_64-linux-gnu/libobs.so.30'",
        manifest.to_str().unwrap()
    ));
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        stdout.trim(),
        libobs_sha,
        "manifest_sha_for_path must return the EXACT sha256 recorded in the manifest for that path"
    );
}

#[test]
fn manifest_sha_for_path_fails_loud_on_an_unlisted_path() {
    let (_tmp, manifest, ..) = make_fixture();
    let (code, stdout, stderr) = run_sourced(&format!(
        "manifest_sha_for_path {:?} 'lib/x86_64-linux-gnu/nonexistent-file.so'",
        manifest.to_str().unwrap()
    ));
    assert_ne!(
        code, 0,
        "must fail loud (nonzero exit) when the manifest has no entry for the path"
    );
    assert!(
        stderr.contains("manifest lists no entry"),
        "must print the specific 'manifest lists no entry' diagnostic, not a silent/generic failure:\n{stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "must print NOTHING to stdout on failure — a caller doing `X=\"$(manifest_sha_for_path ...)\"` \
         must never receive a bogus non-empty value on the failure path"
    );
}

#[test]
fn verify_file_sha_passes_on_a_byte_correct_file() {
    let (_tmp, _manifest, libobs, _distroav, libobs_sha, _distroav_sha) = make_fixture();
    let (code, _stdout, stderr) = run_sourced(&format!(
        "verify_file_sha {:?} {:?} 'test libobs'",
        libobs.to_str().unwrap(),
        libobs_sha
    ));
    assert_eq!(
        code, 0,
        "byte-correct file must verify clean: stderr={stderr}"
    );
}

/// THE core regression this test file exists to catch: a corrupted/truncated/tampered download
/// must be rejected. This is the exact scenario 3 independent review agents flagged as completely
/// unverified before this PR's fix (distroav.so had zero content-integrity check).
#[test]
fn verify_file_sha_fails_loud_on_a_corrupted_file() {
    let (_tmp, _manifest, libobs, _distroav, libobs_sha, _distroav_sha) = make_fixture();
    // Corrupt the file in place — same path, different bytes (simulates a truncated/tampered
    // download landing at the expected location).
    fs::write(&libobs, b"CORRUPTED-not-the-real-bytes-at-all").unwrap();
    let (code, stdout, stderr) = run_sourced(&format!(
        "verify_file_sha {:?} {:?} 'test corrupted libobs'",
        libobs.to_str().unwrap(),
        libobs_sha
    ));
    assert_ne!(
        code, 0,
        "a corrupted file (sha mismatch) MUST fail loud, never pass silently"
    );
    assert!(
        stderr.contains("sha256 mismatch"),
        "must print the specific 'sha256 mismatch' diagnostic:\n{stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "verify_file_sha must print nothing to stdout on failure"
    );
}

#[test]
fn verify_file_sha_fails_loud_when_file_is_missing() {
    let (_tmp, _manifest, _libobs, _distroav, libobs_sha, _distroav_sha) = make_fixture();
    let (code, _stdout, stderr) = run_sourced(&format!(
        "verify_file_sha /nonexistent/path/libobs.so.30 {:?} 'test missing file'",
        libobs_sha
    ));
    assert_ne!(code, 0, "a missing file must fail loud");
    assert!(
        stderr.contains("file missing at"),
        "must print the specific 'file missing at' diagnostic:\n{stderr}"
    );
}

/// End-to-end: the EXACT two-step call pattern used in the real script (`WANT_SHA="$(manifest_sha_for_path
/// ...)"` then `verify_file_sha "$FILE" "$WANT_SHA" "label"`), proving the full integration works
/// for BOTH swapped files (libobs.so.30 AND distroav.so, the file that previously had zero
/// integrity check at all).
#[test]
fn full_lookup_then_verify_pattern_passes_for_both_swapped_files() {
    let (_tmp, manifest, libobs, distroav, ..) = make_fixture();
    let (code, stdout, stderr) = run_sourced(&format!(
        r#"
        WANT_LIBOBS_SHA="$(manifest_sha_for_path {manifest:?} 'lib/x86_64-linux-gnu/libobs.so.30')"
        verify_file_sha {libobs:?} "$WANT_LIBOBS_SHA" "libobs.so.30"
        echo "libobs OK"
        WANT_DISTROAV_SHA="$(manifest_sha_for_path {manifest:?} 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
        verify_file_sha {distroav:?} "$WANT_DISTROAV_SHA" "distroav.so"
        echo "distroav OK"
        "#,
        manifest = manifest.to_str().unwrap(),
        libobs = libobs.to_str().unwrap(),
        distroav = distroav.to_str().unwrap(),
    ));
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("libobs OK") && stdout.contains("distroav OK"));
}

/// The real regression this whole file guards against: if a future edit silently inverted the
/// comparison (`[ "$got" = "$want" ]` -> `!=`), this test would flip from PASS to FAIL, catching
/// a bug that leaves every pinned string in tests/setup_imag_guards.rs completely unchanged.
#[test]
fn full_lookup_then_verify_pattern_rejects_a_tampered_distroav() {
    let (_tmp, manifest, _libobs, distroav, ..) = make_fixture();
    fs::write(&distroav, b"a tampered plugin binary, wrong bytes entirely").unwrap();
    let (code, stdout, stderr) = run_sourced(&format!(
        r#"
        WANT_DISTROAV_SHA="$(manifest_sha_for_path {manifest:?} 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
        verify_file_sha {distroav:?} "$WANT_DISTROAV_SHA" "distroav.so (cross-checked against bundle manifest)"
        echo "SHOULD NOT REACH HERE"
        "#,
        manifest = manifest.to_str().unwrap(),
        distroav = distroav.to_str().unwrap(),
    ));
    assert_ne!(
        code, 0,
        "a tampered distroav.so must be rejected: stdout={stdout}"
    );
    assert!(!stdout.contains("SHOULD NOT REACH HERE"));
    assert!(stderr.contains("sha256 mismatch"), "stderr={stderr}");
}
