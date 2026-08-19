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

/// issue 1094 — the pure command-builder takes a RESOLVED core-pin range (empty = run unpinned)
/// and produces a valid, safely-quoted shell command line running RUST_LOG=info + the verdict
/// binary against the given (imag-local) args — no dev1 path leaks in, and a path/arg is %q-quoted
/// so a value with spaces can never corrupt the command. The pin range is resolved FROM THE ACTUAL
/// BOX by main() (see onimag_decode_core_range) so a hardware swap can never mis-pin it again — the
/// retired 16-thread box's hardcoded `taskset -c 12-15` silently failed EVERY extract on the
/// 12-thread i5-13420H replacement (cores 12-15 don't exist), zeroing the imag leg in 0/76 runs.
#[test]
fn build_onimag_command_pins_to_the_resolved_range_and_quotes_safely() {
    let out = Command::new("bash")
        .arg("-c")
        // exe, RESOLVED range (8-11 = the i5-13420H E-cores), then the forwarded args
        .arg(". \"$1\"; build_onimag_command \"$2\" \"$3\" --imag \"$4\" --out \"$5\"")
        .arg("bash") // $0
        .arg(script()) // $1 — the script to source
        .arg("/home/newlevel/recording-verdict") // $2 — the remote verdict binary
        .arg("8-11") // $3 — the resolved core-pin range (i5-13420H E-cores)
        .arg("/home/newlevel/imag REC.mkv") // $4 — a path WITH a space (quoting must survive it)
        .arg("/home/newlevel/verdict-out/imag-partial-1.json") // $5
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
    // #767: the decode is a BATCH job — nice 19 (batch priority) keeps it off the production render.
    assert!(
        cmd.contains("nice -n 19"),
        "#767: the on-imag decode must run at nice 19 (batch priority). Got: {cmd:?}"
    );
    // issue 1094: the pin uses the RESOLVED range passed in (the box's E-cores), NOT a hardcoded
    // range. The retired box's `taskset -c 12-15` was the 0/76 root cause on the i5-13420H.
    assert!(
        cmd.contains("taskset -c 8-11"),
        "issue 1094: the on-imag decode must pin to the resolved core range (8-11 here). Got: {cmd:?}"
    );
    assert!(
        !cmd.contains("12-15"),
        "issue 1094: the retired 16-thread box's hardcoded `taskset -c 12-15` must be gone — those \
         cores do not exist on the 12-thread i5-13420H, so taskset aborts before the decode. Got: {cmd:?}"
    );
    // issue 1094 review (🔵): lock the EXACT one-trailing-space adjacency — a `taskset -c 8-11  env`
    // double-space regression would slip past the substring checks above.
    assert!(
        cmd.contains("taskset -c 8-11 env RUST_LOG=info"),
        "issue 1094: the pin must sit exactly `taskset -c 8-11 env RUST_LOG=info` (one space). Got: {cmd:?}"
    );
    // The space-containing path must be safely quoted — re-running the printed command line through
    // bash must parse without a syntax error, proving the %q-quoting round-trips.
    let roundtrip = Command::new("bash")
        .arg("-c")
        .arg(format!("printf '%s' {}", cmd.trim()))
        .output();
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

/// issue 1094 — FAIL-OPEN: when main() could NOT resolve a valid pin (empty range), the decode must
/// still run, UNPINNED (nice 19 alone), never emitting a bare/broken `taskset` that would abort the
/// whole extract. A pin error can never again silently zero the imag leg (the 0/76 failure mode).
#[test]
fn build_onimag_command_empty_range_runs_unpinned() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; build_onimag_command \"$2\" \"$3\" --imag \"$4\" --out \"$5\"")
        .arg("bash")
        .arg(script())
        .arg("/home/newlevel/recording-verdict")
        .arg("") // $3 — EMPTY resolved range => run unpinned
        .arg("/home/newlevel/imag REC.mkv")
        .arg("/home/newlevel/verdict-out/imag-partial-1.json")
        .output()
        .expect("run build_onimag_command");
    assert!(
        out.status.success(),
        "build_onimag_command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cmd = String::from_utf8_lossy(&out.stdout);
    assert!(
        cmd.contains("nice -n 19") && cmd.contains("RUST_LOG=info"),
        "issue 1094: an unpinned decode must still be nice 19 + RUST_LOG=info. Got: {cmd:?}"
    );
    assert!(
        !cmd.contains("taskset"),
        "issue 1094: an empty resolved range must run the decode UNPINNED (no taskset) — fail-open, \
         so a pin error never aborts the extract. Got: {cmd:?}"
    );
    // issue 1094 review (🔵): the empty-range command must be exactly `nice -n 19 env RUST_LOG=info`
    // (one space, no dangling/double token where the taskset prefix was elided).
    assert!(
        cmd.contains("nice -n 19 env RUST_LOG=info"),
        "issue 1094: unpinned command must read exactly `nice -n 19 env RUST_LOG=info` (one space). Got: {cmd:?}"
    );
}

/// issue 1094 — the pure pin-range formula: the top min(4, ncpus) online cores (the E-cores on
/// Intel hybrid). 12 threads (i5-13420H) -> 8-11; 16 threads (retired box) -> 12-15 (reproducing
/// #767's original pin there); tiny boxes clamp the low end to 0; a non-numeric/absent/zero count
/// -> empty (the caller then runs the decode unpinned). No network, no real cores needed — pure +
/// Tier-0 testable.
#[test]
fn onimag_decode_core_range_maps_cpu_count_to_top_four_cores() {
    let cases = [
        ("12", "8-11"),  // the live i5-13420H
        ("16", "12-15"), // the retired box — #767's original pin is reproduced
        ("8", "4-7"),
        ("4", "0-3"),
        ("2", "0-1"),
        ("1", "0-0"),
        ("08", "4-7"), // issue 1094 review: a leading-zero token must NOT be read as octal
        ("09", "5-8"), // (09 is an invalid octal digit -> a $(( )) error that would defeat
        ("012", "8-11"), // fail-open); base-10 forcing keeps them harmless.
        ("", ""),      // absent count -> unpinned
        ("bogus", ""), // non-numeric -> unpinned
        ("0", ""),     // zero -> unpinned
    ];
    for (n, want) in cases {
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$1\"; onimag_decode_core_range \"$2\"")
            .arg("bash")
            .arg(script())
            .arg(n)
            .output()
            .expect("run onimag_decode_core_range");
        assert!(
            out.status.success(),
            "onimag_decode_core_range {n:?} exited nonzero: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            got.trim(),
            want,
            "onimag_decode_core_range {n:?} => {:?}, want {want:?}",
            got.trim()
        );
    }
}

/// issue 1118 — STEP 1 must VERSION-GATE the on-imag binary upload. `onimag_upload_decision` is the
/// pure/sourced (no-network) decision: `--force-upload` always uploads; an absent binary uploads;
/// an already-present binary whose sha256 DIFFERS from the local one re-uploads (the fix — a stale
/// v3-emitting binary after the #1112 v3->v4 PARTIAL_SCHEMA_VERSION bump); an IDENTICAL sha keeps
/// the fast idempotent skip; a can't-hash-local case re-uploads (fail-safe). Before this, STEP 1
/// only re-uploaded when the binary was ABSENT/non-executable, so a schema bump left imag stale and
/// the fresh dev1 merge rejected its partial and (pre-1118) killed the whole verdict.
#[test]
fn onimag_upload_decision_version_gates_the_binary() {
    // force, present, local_sha, remote_sha  =>  expected decision
    let cases = [
        // --force-upload always wins, even when the shas match.
        ("1", "1", "aaaa", "aaaa", "upload"),
        ("1", "0", "", "", "upload"),
        // Absent / not-executable on imag -> upload (unchanged pre-1118 behaviour).
        ("0", "0", "aaaa", "aaaa", "upload"),
        // THE FIX: present but the on-imag sha DIFFERS from the local binary -> re-upload
        // (stale emitter after a schema bump).
        ("0", "1", "aaaa", "bbbb", "upload"),
        // Present AND identical sha -> keep the fast idempotent skip.
        ("0", "1", "aaaa", "aaaa", "skip"),
        // Present but the local binary could not be hashed -> re-upload (fail-safe, never skip blind).
        ("0", "1", "", "bbbb", "upload"),
        // Present, remote sha empty (couldn't read it) but local known -> differ -> upload.
        ("0", "1", "aaaa", "", "upload"),
    ];
    for (force, present, local_sha, remote_sha, want) in cases {
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$1\"; onimag_upload_decision \"$2\" \"$3\" \"$4\" \"$5\"")
            .arg("bash")
            .arg(script())
            .arg(force)
            .arg(present)
            .arg(local_sha)
            .arg(remote_sha)
            .output()
            .expect("run onimag_upload_decision");
        assert!(
            out.status.success(),
            "onimag_upload_decision({force},{present},{local_sha:?},{remote_sha:?}) exited nonzero: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            got.trim(),
            want,
            "onimag_upload_decision({force},{present},{local_sha:?},{remote_sha:?}) => {:?}, want {want:?}",
            got.trim()
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
