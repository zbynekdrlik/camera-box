//! Direct unit tests for `scripts/lib/win-status-args.sh`'s `win_status_parse_entry()` — the
//! shared "--win-status NAME=FILE" parse + missing-file guard extracted from
//! `scripts/w32time-gate.sh` and `scripts/dantesync-gate.sh` (#622; #598's own review flagged this
//! exact block as character-for-character duplicated between the two gates: the same
//! `name="${entry%%=*}"; file="${entry#*=}"` split, the same
//! `[ -z "$file" ] || [ ! -s "$file" ]` guard, the same printf wording/width, the same
//! `cat "$file" 2>/dev/null || true` read).
//!
//! `tests/w32time_gate.rs` and `tests/dantesync_gate.rs` already exercise this logic INDIRECTLY,
//! end-to-end, through each gate script's own `--win-status` handling — including the
//! "missing status file -> exit 11 (incomplete)" case both suites already pin, unchanged after
//! this refactor (proven by re-running both suites green post-extraction). This file pins the
//! EXTRACTED FUNCTION directly, so the shared behavior itself — not just its two callers — carries
//! its own regression guard, per #622's TDD requirement.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/win-status-args.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source `win-status-args.sh` (source-only: defines one function, no top-level execution -- no
/// `BASH_SOURCE` guard needed) and run `body`. Uses `set -uo pipefail` (no `-e`), mirroring
/// `tests/w32time_gate.rs`'s / `tests/dantesync_gate.rs`'s own `run_sourced` harness, so a
/// non-zero return from `win_status_parse_entry` itself doesn't abort the harness before its
/// stdout/exit code can be inspected. Returns (harness_exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a status-text fixture and return its path.
fn write_status(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("win-status-args-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.txt"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
    path
}

#[test]
fn win_status_parse_entry_is_defined_after_sourcing() {
    let (code, stdout, stderr) =
        run_sourced("type win_status_parse_entry >/dev/null && echo defined");
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(stdout.trim(), "defined", "stdout: {stdout}");
}

#[test]
fn a_valid_entry_populates_name_and_text_and_returns_zero() {
    let p = write_status("strih_ok", "some real status text\n");
    let (code, stdout, stderr) = run_sourced(&format!(
        "win_status_parse_entry 'strih={}'; echo \"rc=$?\"; \
         printf 'NAME=%s\\n' \"$WIN_STATUS_NAME\"; \
         printf 'TEXT=%s\\n' \"$WIN_STATUS_TEXT\"",
        p.display()
    ));
    assert_eq!(code, 0, "harness itself must not abort. stderr={stderr}");
    assert!(
        stdout.contains("rc=0"),
        "a valid NAME=FILE entry must return 0: {stdout}"
    );
    assert!(stdout.contains("NAME=strih"), "stdout: {stdout}");
    assert!(
        stdout.contains("TEXT=some real status text"),
        "WIN_STATUS_TEXT must hold the file's content (same `cat` read as the original inline \
         block): {stdout}"
    );
}

#[test]
fn a_malformed_entry_with_a_missing_file_fails_loud() {
    // The exact "no status file" case both gates' own end-to-end tests already pin at the gate
    // level (exit 11) -- this pins the SHARED function's own contract directly: a missing file
    // must return 1 (fail loud) and print the standard diagnostic, never silently succeed.
    let (code, stdout, stderr) = run_sourced(
        "win_status_parse_entry 'stream=/tmp/definitely-not-a-real-win-status-file.txt'; \
         echo \"rc=$?\"",
    );
    assert_eq!(code, 0, "harness itself must not abort. stderr={stderr}");
    assert!(
        stdout.contains("rc=1"),
        "a missing status file must return 1, never silently succeed: {stdout}"
    );
    assert!(
        stdout.contains("stream")
            && stdout.contains("UNKNOWN")
            && stdout.contains("no status file"),
        "must print the standard per-box UNKNOWN diagnostic naming the box: {stdout}"
    );
}

#[test]
fn a_malformed_entry_with_an_empty_file_also_fails_loud() {
    // The `[ ! -s "$file" ]` half of the guard: a PRESENT but EMPTY status file is exactly as
    // unusable as a missing one and must also fail, not silently pass through an empty read.
    let p = write_status("stream_empty", "");
    let (code, stdout, stderr) = run_sourced(&format!(
        "win_status_parse_entry 'stream={}'; echo \"rc=$?\"",
        p.display()
    ));
    assert_eq!(code, 0, "harness itself must not abort. stderr={stderr}");
    assert!(
        stdout.contains("rc=1"),
        "an empty status file must return 1, never silently succeed: {stdout}"
    );
    assert!(stdout.contains("UNKNOWN"), "stdout: {stdout}");
}

#[test]
fn multiple_win_status_entries_accumulate_independently_across_sequential_calls() {
    // Mirrors a gate's `for entry in "${win_status[@]}"` loop calling win_status_parse_entry once
    // per box: two sequential entries in the SAME shell session must each land the CURRENT entry's
    // own name/text into WIN_STATUS_NAME/WIN_STATUS_TEXT -- no stale leakage from the previous
    // call's globals into the next iteration.
    let strih = write_status("multi_strih", "strih status text");
    let stream = write_status("multi_stream", "stream status text");
    let (code, stdout, stderr) = run_sourced(&format!(
        "for e in 'strih={}' 'stream={}'; do \
           win_status_parse_entry \"$e\"; \
           printf '%s:%s\\n' \"$WIN_STATUS_NAME\" \"$WIN_STATUS_TEXT\"; \
         done",
        strih.display(),
        stream.display()
    ));
    assert_eq!(code, 0, "harness itself must not abort. stderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout:?}");
    assert_eq!(lines[0], "strih:strih status text", "stdout: {stdout:?}");
    assert_eq!(lines[1], "stream:stream status text", "stdout: {stdout:?}");
}
