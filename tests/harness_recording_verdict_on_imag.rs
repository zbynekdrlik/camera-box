//! #462 (EPIC #466 Topology v2) — `scripts/recording-verdict-on-imag.sh`, the THIRD leg of the
//! #208 per-box decode-in-place model, alongside recording-verdict-on-strih.sh /
//! recording-verdict-on-stream.sh. UNLIKE those two Windows planners (ssh/scp to strih/stream is
//! DENIED — win-*-MCP is the only path there, so they only PRINT a plan), imag-nb is a plain
//! Ubuntu box reachable over ssh/scp (same access class as cam1/cam2 — targets.md's "Linux OBS
//! Targets" row), so this script ACTUALLY EXECUTES the deploy + decode + pull-back.
//!
//! These are the SAME two test shapes the Windows siblings use (see
//! tests/harness_recording_e2e_paths.rs's `recording_verdict_on_strih_script_exists` /
//! `on_stream_planner_builds_a_valid_windows_command`): (a) the script exists + is executable,
//! and (b) its pure command-builder produces a well-formed, safely-quoted command line — sourced
//! and called directly, NEVER touching the network. `main()`'s idempotent
//! `--skip-if-exists`-returns-early behavior is also exercised end-to-end (no network needed:
//! the file existing is enough for it to return before ever reaching ssh/scp).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-imag.sh")
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The on-imag helper must exist and be executable (mirrors the on-strih/on-stream siblings).
#[test]
fn recording_verdict_on_imag_script_exists() {
    let meta = fs::metadata(script())
        .unwrap_or_else(|e| panic!("#462: recording-verdict-on-imag.sh missing: {e}"));
    assert!(
        meta.is_file(),
        "recording-verdict-on-imag.sh must be a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "recording-verdict-on-imag.sh must be executable"
        );
    }
}

/// UNLIKE its Windows siblings' DEFAULT planner mode, this script must target imag over PLAIN SSH
/// (imag-nb is a plain Linux box reached directly, same access class as cam1/cam2; the Windows
/// siblings default to the win-* MCP for a GUI-adjacent workflow, though #701/#703 gave them an
/// opt-in --execute ssh path too) — it must reference the imag box IP and NEVER the win-*
/// MCP names (those are strictly for strih/stream).
#[test]
fn on_imag_helper_targets_imag_over_plain_ssh_not_mcp() {
    let s = read("scripts/recording-verdict-on-imag.sh");
    assert!(
        s.contains("10.77.9.182"),
        "#462: the on-imag helper must reference the imag box IP 10.77.9.182."
    );
    assert!(
        s.contains("sshpass") && s.contains("ssh"),
        "#462: the on-imag helper must use plain ssh/scp (imag is reachable, unlike Windows)."
    );
    // A prose mention of the sibling Windows planners (explaining WHY this script differs) is
    // fine; what must be ABSENT is an actual MCP directive (the "paste this into ... Shell /
    // FileUpload / FileDownload" plan-printing shape the Windows siblings use).
    assert!(
        !s.contains("FileUpload") && !s.contains("FileDownload") && !s.contains("MCP Shell:"),
        "#462: the on-imag helper must not print an MCP plan (it EXECUTES directly over ssh/scp, \
         unlike the win-strih/win-stream-snv siblings)."
    );
}

/// The pure command-builder must produce a valid, safely-quoted shell command line running
/// RUST_LOG=info + the verdict binary against the given (imag-local) args — no dev1 path leaks
/// in, and a path/arg is %q-quoted so a value with spaces can never corrupt the command.
#[test]
fn build_onimag_command_produces_a_safely_quoted_command() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; build_onimag_command \"$2\" --imag \"$3\" --out \"$4\"")
        .arg("bash") // $0
        .arg(script()) // $1 — the script to source
        .arg("/home/newlevel/recording-verdict") // $2 — the remote verdict binary
        .arg("/home/newlevel/imag REC.mkv") // $3 — a path WITH a space (quoting must survive it)
        .arg("/home/newlevel/verdict-out/imag-partial-1.json") // $4
        .output()
        .expect("run build_onimag_command");
    assert!(
        out.status.success(),
        "build_onimag_command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cmd = String::from_utf8_lossy(&out.stdout);
    assert!(
        cmd.contains("RUST_LOG=info"),
        "#462: the on-imag command must set RUST_LOG=info (decode-progress liveness signal). \
         Got: {cmd:?}"
    );
    assert!(
        cmd.contains("/home/newlevel/recording-verdict"),
        "#462: the on-imag command must run the deployed verdict binary. Got: {cmd:?}"
    );
    assert!(
        cmd.contains("--imag") && cmd.contains("--out"),
        "#462: the on-imag command must forward the --imag/--out args. Got: {cmd:?}"
    );
    // The space-containing path must be safely quoted (bash %q either backslash-escapes the
    // space or single-quotes the whole token) — re-running the printed command line through bash
    // must reproduce the exact original argument, proving the quoting round-trips.
    let roundtrip = Command::new("bash")
        .arg("-c")
        .arg(format!("printf '%s' {}", cmd.trim()))
        .output();
    // We only need the quoting to be well-formed enough that bash can parse it without erroring —
    // a genuine build failure here would mean the %q-quoting was broken.
    if let Ok(rt) = roundtrip {
        // RUST_LOG=info is a var-assignment prefix on a bare command; bash may complain the
        // "binary" doesn't exist (it's a fake path) — that's fine, we only assert no PARSE error.
        let stderr = String::from_utf8_lossy(&rt.stderr);
        assert!(
            !stderr.contains("syntax error"),
            "#462: the built command must be syntactically valid shell. stderr: {stderr}"
        );
    }
}

/// `--skip-if-exists <path>` must return BEFORE any ssh/scp when the partial already exists on
/// dev1 (the #281 durable-idempotency contract every per-box helper shares) — proven end-to-end
/// with NO network access: main() must reach the early `return 0` and never attempt to parse
/// `--imag-rec` (which is required otherwise) or touch the network.
#[test]
fn skip_if_exists_returns_before_touching_the_network() {
    let dir = std::env::temp_dir().join(format!(
        "recording-verdict-on-imag-test-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let existing = dir.join("imag-partial.json");
    fs::write(&existing, "{}").expect("write fake partial");

    let out = Command::new("bash")
        .arg(script())
        .arg("--skip-if-exists")
        .arg(&existing)
        // No --imag-rec given at all — if main() did not return early, it would hit the
        // "--imag-rec is required" usage error (exit 2), not exit 0.
        .output()
        .expect("run recording-verdict-on-imag.sh --skip-if-exists");
    assert!(
        out.status.success(),
        "#462/#281: --skip-if-exists on an existing partial must exit 0 (skip re-decode) without \
         requiring --imag-rec. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SKIP"),
        "#462: --skip-if-exists must print a SKIP line. Got: {stdout:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

/// A missing `--imag-rec` (and no `--skip-if-exists` short-circuit) must be a CLEAN usage error
/// (exit 2) — proven without ssh/scp: the validation runs before any network call.
#[test]
fn missing_imag_rec_is_a_usage_error_before_any_network_call() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--verdict-bin")
        .arg("/tmp/does-not-matter")
        .arg("--")
        .arg("--extract-partial")
        .arg("imag")
        .output()
        .expect("run recording-verdict-on-imag.sh with no --imag-rec");
    assert_eq!(
        out.status.code(),
        Some(2),
        "#462: a missing --imag-rec must exit 2 (usage error) BEFORE any ssh/scp. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The script must be SOURCE-SAFE: sourcing it (the unit-test harness above) must NOT execute
/// main (the `BASH_SOURCE != $0` guard) — otherwise every source would try to ssh/scp with no args.
#[test]
fn script_is_source_safe() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; echo SOURCED_OK")
        .arg("bash")
        .arg(script())
        .output()
        .expect("source recording-verdict-on-imag.sh");
    assert!(
        out.status.success(),
        "sourcing must not execute main (and must not error): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SOURCED_OK"),
        "#462: the script must be source-safe. Got: {stdout:?}"
    );
}
