//! #835 — `.claude/skills/e2e/SKILL.md` still told an operator to hand-pre-fetch each Windows
//! box's DanteSync status into `/tmp/recording-e2e-<RUN_ID>/dante-{strih,stream}.json` before
//! launching the harness. False since #648 (`e2cfeb3d7`, 2026-07-10): `recording-e2e.sh` fetches
//! `:8898` itself via `--win-http`, and nothing in this harness has written a `dante-*.json` file
//! since. Following the stale runbook tonight dropped a 21-day-old cached snapshot into a live
//! run's `$OUTDIR` — harmless only because nothing reads it, but the artifact of a stale runbook
//! should announce itself instead of lurking.
//!
//! `scripts/lib/stale-artifact-guard.sh`'s `stale_dante_artifact_warn OUTDIR` is a pure, directly-
//! callable local function (not a remote-command-string builder — `$OUTDIR` lives on dev1, no ssh
//! involved): it prints a loud, non-fatal WARNING to stderr naming any `dante-*.json` file already
//! sitting in `OUTDIR`, and prints nothing when there is none. These tests exercise it directly
//! (source the lib, call the function against a real tempdir) plus a static-read assertion that
//! `recording-e2e.sh` actually wires it in, right after `mkdir -p "$OUTDIR"` creates the run dir.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Source the lib and run `stale_dante_artifact_warn "$1"` against `outdir`. Returns
/// (exit_status_success, stdout, stderr) — the function must NEVER fail the caller's shell
/// (advisory-only), so a non-zero exit here means the guard itself is broken, not a "found a
/// stray file" signal (that signal lives in stderr text, not the exit code).
fn run_guard(outdir: &std::path::Path) -> (bool, String, String) {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\nstale_dante_artifact_warn \"$OUTDIR\"\necho DONE";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", repo("scripts/lib/stale-artifact-guard.sh"))
        .env("OUTDIR", outdir)
        .output()
        .expect("run bash harness");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "stale-dante-guard-test-{name}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn warns_loudly_when_a_stray_dante_json_is_already_present() {
    let dir = tempdir("stray");
    fs::write(
        dir.join("dante-stream.json"),
        r#"{"updated_ts":1783389040}"#,
    )
    .unwrap();

    let (ok, stdout, stderr) = run_guard(&dir);
    assert!(
        ok,
        "the guard must never fail the caller's shell. stderr={stderr}"
    );
    assert!(
        stdout.contains("DONE"),
        "the harness must reach past the call: {stdout}"
    );
    assert!(
        stderr.contains("WARNING"),
        "a stray dante-*.json must produce a loud WARNING on stderr: {stderr}"
    );
    assert!(
        stderr.contains("dante-stream.json"),
        "the warning must NAME the stray file: {stderr}"
    );
    assert!(
        stderr.contains("#648") || stderr.contains("#835"),
        "the warning should explain why this file is unexpected (references #648/#835): {stderr}"
    );
}

#[test]
fn warns_for_each_stray_file_when_both_strih_and_stream_are_present() {
    let dir = tempdir("both");
    fs::write(dir.join("dante-strih.json"), "{}").unwrap();
    fs::write(dir.join("dante-stream.json"), "{}").unwrap();

    let (ok, _stdout, stderr) = run_guard(&dir);
    assert!(ok, "stderr={stderr}");
    assert!(
        stderr.contains("dante-strih.json"),
        "must name strih's stray file too: {stderr}"
    );
    assert!(
        stderr.contains("dante-stream.json"),
        "must name stream's stray file too: {stderr}"
    );
}

#[test]
fn silent_when_the_run_directory_has_no_dante_json() {
    let dir = tempdir("clean");
    // A genuinely fresh run dir: no dante-*.json, just an unrelated artifact.
    fs::write(dir.join("verdict-123.json"), "{}").unwrap();

    let (ok, stdout, stderr) = run_guard(&dir);
    assert!(ok, "stderr={stderr}");
    assert!(stdout.contains("DONE"), "stdout: {stdout}");
    assert!(
        !stderr.contains("WARNING"),
        "a clean run dir must produce NO warning: {stderr}"
    );
}

#[test]
fn silent_and_safe_when_the_run_directory_does_not_exist_yet() {
    // The guard must never itself fail (or `set -e`-abort the caller) when handed a directory
    // that hasn't been created — a defensive no-op, not a hard requirement of caller ordering.
    let dir = std::env::temp_dir().join(format!(
        "stale-dante-guard-test-missing-{}-does-not-exist",
        std::process::id()
    ));
    let (ok, _stdout, stderr) = run_guard(&dir);
    assert!(ok, "must not fail on a missing directory. stderr={stderr}");
    assert!(!stderr.contains("WARNING"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------------------------------
// Static-read wiring assertions — recording-e2e.sh must actually call the guard, right after it
// creates $OUTDIR, not just define/source it somewhere unused.
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_stale_artifact_guard() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("stale-artifact-guard.sh"),
        "#835: recording-e2e.sh must source scripts/lib/stale-artifact-guard.sh"
    );
    assert!(
        s.contains(". \"$HERE/lib/stale-artifact-guard.sh\""),
        "#835: recording-e2e.sh must actually `source` (not just mention) stale-artifact-guard.sh"
    );
}

#[test]
fn recording_e2e_calls_the_guard_right_after_creating_outdir() {
    let s = read("scripts/recording-e2e.sh");
    let mkdir_at = s
        .find("mkdir -p \"$OUTDIR\"")
        .expect("recording-e2e.sh must still create $OUTDIR (unchanged original line)");
    let call_at = s
        .find("stale_dante_artifact_warn")
        .expect("#835: recording-e2e.sh must call stale_dante_artifact_warn");
    assert!(
        call_at > mkdir_at,
        "#835: the stray-artifact check must run AFTER $OUTDIR exists (mkdir at {mkdir_at}, call at {call_at})"
    );
    // Keep it close to the mkdir -- not buried hundreds of lines later where a run could do real
    // work (deploys, recording) before the operator ever sees the warning.
    assert!(
        call_at - mkdir_at < 4000,
        "#835: the check should run shortly after $OUTDIR is created, not deep into the script \
         (mkdir at {mkdir_at}, call at {call_at})"
    );
}
