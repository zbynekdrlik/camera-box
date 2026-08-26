//! #703 — the required merge gate must ACTUALLY COMPUTE the zero-loss/A/V verdict, never just
//! print a plan and `exit 0`. This file locks the fix's four pieces (behavioral where possible,
//! structural where a live rig/ssh would otherwise be needed — same discipline as the rest of
//! this repo's harness test suite):
//!
//! 1. `scripts/lib/win-ssh-exec.sh` — the shared ssh/scp helpers (PowerShell `-EncodedCommand`
//!    base64/UTF-16LE encoding is a PURE function, tested behaviorally with no network).
//! 2. `scripts/recording-verdict-on-strih.sh` / `-on-stream.sh` — the new `--execute` mode that
//!    ACTUALLY ssh/scp's instead of only printing a plan (opt-in; default behavior unchanged).
//! 3. `scripts/recording-e2e.sh` — `E2E_EXECUTE_VERDICT=1` wiring: fail loud on a missing
//!    `WIN_VERDICT_EXE_LOCAL`, launch strih+stream extracts in PARALLEL (backgrounded), wait for
//!    both, then ACTUALLY RUN the merge and make the script's own exit code the merge's exit
//!    code (never a bare `exit 0` once real execution happened).
//! 4. `.github/workflows/full-path-e2e.yml` — the Windows-artifact fetch step, `E2E_EXECUTE_VERDICT`
//!    wired into the recording step's env, the fail-closed structural guard (verdict JSON exists
//!    + `overall_pass=true`), and the artifact-upload path scoped to THIS run's own RUN_ID.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------------------------
// scripts/lib/win-ssh-exec.sh
// ---------------------------------------------------------------------------------------------

#[test]
fn win_ssh_exec_lib_exists_and_defines_the_expected_helpers() {
    let s = read("scripts/lib/win-ssh-exec.sh");
    for func in [
        "win_ssh_ps_encoded_command",
        "win_ssh_run",
        "win_ssh_upload",
        "win_ssh_download",
        "win_ssh_download_dir",
        "win_ssh_path_exists",
    ] {
        assert!(
            s.contains(&format!("{func}()")),
            "#703: scripts/lib/win-ssh-exec.sh must define {func}()"
        );
    }
    // sshpass/ssh/scp — not a win-* MCP directive (this lib EXECUTES, it never prints a plan).
    assert!(
        s.contains("sshpass"),
        "#703: must use sshpass (targets.md password auth)"
    );
    assert!(
        !s.contains("FileUpload") && !s.contains("FileDownload"),
        "#703: win-ssh-exec.sh executes directly — it must never print an MCP plan directive"
    );
}

/// The library must be safely sourceable with NO side effects (no network call, no exit) — the
/// per-box scripts source it unconditionally at file scope, even when only being sourced by a
/// test to call `build_onbox_command`.
#[test]
fn win_ssh_exec_lib_is_sourceable_without_side_effects() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; echo SOURCED_OK")
        .arg("bash")
        .arg(manifest_dir().join("scripts/lib/win-ssh-exec.sh"))
        .output()
        .expect("source win-ssh-exec.sh");
    assert!(
        out.status.success(),
        "#703: sourcing win-ssh-exec.sh must not error: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("SOURCED_OK"));
}

/// `win_ssh_scp_source_path` — live-CI-run-discovered bug (2026-07-11, first real EXECUTE-mode
/// dispatch of this PR's own fix): `scp user@host:'C:\camera-box\...\x.json' dest` fails "No
/// such file or directory" even though the file genuinely exists on the box (live-reproduced on
/// strih); the IDENTICAL file via forward slashes (`C:/camera-box/.../x.json`) downloads fine.
/// win_ssh_download/_dir must convert backslashes before building the scp remote spec.
#[test]
fn win_ssh_scp_source_path_converts_backslashes_to_forward_slashes() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; win_ssh_scp_source_path \"$2\"")
        .arg("bash")
        .arg(manifest_dir().join("scripts/lib/win-ssh-exec.sh"))
        .arg(r"C:\camera-box\verdict-out\strih-partial-12345.json")
        .output()
        .expect("run win_ssh_scp_source_path");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "C:/camera-box/verdict-out/strih-partial-12345.json",
        "#703: win_ssh_scp_source_path must convert every backslash to a forward slash (the \
         live-verified scp-download fix)"
    );
}

#[test]
fn win_ssh_download_and_download_dir_use_the_converted_scp_source_path() {
    let s = read("scripts/lib/win-ssh-exec.sh");
    for func in ["win_ssh_download()", "win_ssh_download_dir()"] {
        let fn_start = s
            .find(func)
            .unwrap_or_else(|| panic!("#703: {func} must be defined"));
        let fn_end = s[fn_start..]
            .find("\n}\n")
            .map(|i| fn_start + i)
            .unwrap_or_else(|| panic!("#703: {func} must have a closing brace"));
        let body = &s[fn_start..fn_end];
        assert!(
            body.contains("win_ssh_scp_source_path"),
            "#703: {func} must convert the remote path via win_ssh_scp_source_path before \
             building the scp remote spec (the live-verified backslash-download fix): {body}"
        );
    }
    // win_ssh_upload (the OPPOSITE direction — a backslash DESTINATION) is NOT broken (live-
    // verified separately) and must stay UNCONVERTED — converting it would be an unrequested,
    // unverified change to a path that already works.
    let upload_start = s
        .find("win_ssh_upload()")
        .expect("#703: win_ssh_upload must be defined");
    let upload_end = s[upload_start..]
        .find("\n}\n")
        .map(|i| upload_start + i)
        .expect("win_ssh_upload must have a closing brace");
    assert!(
        !s[upload_start..upload_end].contains("win_ssh_scp_source_path"),
        "#703: win_ssh_upload's backslash DESTINATION already works live-verified — do not \
         convert it (that direction was never broken)"
    );
}

/// `win_ssh_basename` — live-CI-run-discovered bug (same run): plain bash `basename` splits
/// ONLY on `/`; fed a backslash Windows path it finds none and returns the WHOLE STRING
/// unchanged, producing a nonsense local destination
/// (`$LOCAL_OUT_DIR/C:\camera-box\verdict-out\...json`, live-observed in the failed run's log).
#[test]
fn win_ssh_basename_splits_on_backslash_not_only_forward_slash() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; win_ssh_basename \"$2\"")
        .arg("bash")
        .arg(manifest_dir().join("scripts/lib/win-ssh-exec.sh"))
        .arg(r"C:\camera-box\verdict-out\strih-partial-12345.json")
        .output()
        .expect("run win_ssh_basename");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "strih-partial-12345.json",
        "#703: win_ssh_basename must extract just the filename from a backslash Windows path — \
         a bare `basename` call returns the string UNCHANGED (no / found), the live-observed bug"
    );
}

#[test]
fn strih_and_stream_scripts_use_win_ssh_basename_not_bare_basename_for_the_pullback() {
    for path in [
        "scripts/recording-verdict-on-strih.sh",
        "scripts/recording-verdict-on-stream.sh",
    ] {
        let s = read(path);
        assert!(
            s.contains("win_ssh_basename"),
            "#703: {path} must use win_ssh_basename (not bare `basename`) to derive the pulled- \
             back local filename from the Windows --out path"
        );
        assert!(
            !s.contains(r#"partial_base="$(basename "$OUT_PARTIAL")""#),
            "#703: {path} must NOT use bare `basename` on a backslash Windows path (live-verified \
             bug — it returns the whole string unchanged)"
        );
    }
}

/// The A/V-sync marker CSV push to the stream box — live-CI-run-discovered bug (same run): the
/// push was PRINT-ONLY (an MCP-operator instruction) even in EXECUTE mode, so
/// `recording-verdict.exe --extract-partial stream --av-marker-log <path>` failed `os error 2 —
/// The system cannot find the file specified` reading a CSV that was never actually uploaded.
/// EXECUTE mode must actually win_ssh_upload it.
#[test]
fn recording_e2e_actually_uploads_the_av_marker_csv_in_execute_mode() {
    let s = read_e2e();
    let block_start = s
        .find("PUSH the cam2 A/V-sync marker log to the stream box")
        .expect("#703: the AV-marker push block must exist");
    let block_end = s[block_start..]
        .find("--- [8/8b] extract the STREAM partial")
        .map(|rel| block_start + rel)
        .expect("the AV-marker push block must be followed by the [8/8b] stream extract");
    let block = &s[block_start..block_end];
    assert!(
        block.contains(r#"if [ "$E2E_EXECUTE_VERDICT" = "1" ]"#),
        "#703: the AV-marker push block must branch on E2E_EXECUTE_VERDICT: {block}"
    );
    assert!(
        block
            .contains("win_ssh_upload \"$STREAM_USER\" \"$STREAM_PW\" \"$STREAM\" \"$MARKER_CSV\""),
        "#703: in EXECUTE mode the AV-marker CSV must be ACTUALLY uploaded via win_ssh_upload, \
         not just printed as an MCP instruction: {block}"
    );
}

/// `win_ssh_ps_encoded_command` — the PowerShell `-EncodedCommand` base64/UTF-16LE encoder. Pure
/// string function, no network. Round-trips (decoded with the SAME `base64`/`iconv` coreutils
/// the production path relies on, not a Rust base64 crate — avoids adding a new dependency for
/// one test and exercises the exact tool chain that matters) to reproduce the EXACT original
/// command text, including a Windows path with a SPACE and embedded quotes (the documented
/// three-layer quoting hazard this function exists to sidestep).
#[test]
fn win_ssh_ps_encoded_command_round_trips_utf16le_base64() {
    let ps_cmd = r#"$env:RUST_LOG="info"; & "C:\camera-box\recording-verdict.exe" "--strih" "D:\_REC\2026-07-10 17-10-31.mkv""#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            ". \"$1\"; enc=\"$(win_ssh_ps_encoded_command \"$2\")\"; \
             printf '%s' \"$enc\" | base64 -d | iconv -f UTF-16LE -t UTF-8",
        )
        .arg("bash")
        .arg(manifest_dir().join("scripts/lib/win-ssh-exec.sh"))
        .arg(ps_cmd)
        .output()
        .expect("run + round-trip win_ssh_ps_encoded_command");
    assert!(
        out.status.success(),
        "win_ssh_ps_encoded_command round-trip failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let decoded = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        decoded, ps_cmd,
        "#703: the EncodedCommand round-trip must reproduce the exact original PowerShell text \
         (including the space-bearing path) — this is the actual on-box command that will run"
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/recording-verdict-on-strih.sh / -on-stream.sh — the new --execute mode
// ---------------------------------------------------------------------------------------------

fn strih_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-strih.sh")
}
fn stream_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-stream.sh")
}

/// Both per-box scripts must accept --execute plus its companion flags — structural (source
/// text), the actual ssh/scp behavior needs the real rig and is out of reach for a unit test.
#[test]
fn strih_and_stream_scripts_accept_execute_mode_flags() {
    for (label, path) in [
        ("strih", "scripts/recording-verdict-on-strih.sh"),
        ("stream", "scripts/recording-verdict-on-stream.sh"),
    ] {
        let s = read(path);
        for flag in ["--execute", "--verdict-exe-local", "--local-out-dir"] {
            assert!(
                s.contains(flag),
                "#703: {label} script must accept {flag} for EXECUTE mode"
            );
        }
        assert!(
            s.contains("win_ssh_run")
                && s.contains("win_ssh_upload")
                && s.contains("win_ssh_download"),
            "#703: {label} script's execute mode must call the win-ssh-exec.sh helpers"
        );
        // Sourced from the shared lib, not reimplemented.
        assert!(
            s.contains("lib/win-ssh-exec.sh"),
            "#703: {label} script must source scripts/lib/win-ssh-exec.sh"
        );
    }
}

/// --execute mode must FAIL LOUD (exit 2, before any network call) when --local-out-dir is
/// missing — proven with NO --out in the forwarded args either, so the FIRST validation hit
/// (missing --out or missing --local-out-dir) determines the exit path; both are usage errors.
#[test]
fn strih_execute_without_local_out_dir_or_out_is_a_usage_error() {
    let out = Command::new("bash")
        .arg(strih_script())
        .arg("--execute")
        .arg("--strih-rec")
        .arg(r"D:\_REC\test.mkv")
        .arg("--")
        .arg("--extract-partial")
        .arg("strih")
        .output()
        .expect("run recording-verdict-on-strih.sh --execute with no --out/--local-out-dir");
    assert_eq!(
        out.status.code(),
        Some(2),
        "#703: --execute with no --out and no --local-out-dir must be a clean usage error (exit \
         2), before any ssh/scp attempt. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn stream_execute_without_local_out_dir_or_out_is_a_usage_error() {
    let out = Command::new("bash")
        .arg(stream_script())
        .arg("--execute")
        .arg("--stream-rec")
        .arg(r"D:\_REC\test.mp4")
        .arg("--")
        .arg("--extract-partial")
        .arg("stream")
        .output()
        .expect("run recording-verdict-on-stream.sh --execute with no --out/--local-out-dir");
    assert_eq!(
        out.status.code(),
        Some(2),
        "#703: --execute with no --out and no --local-out-dir must be a clean usage error (exit \
         2), before any ssh/scp attempt. stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// --skip-if-exists MUST still short-circuit BEFORE any --execute logic (the #281 durable-
/// idempotency contract must not regress now that --execute exists) — proven with a partial
/// that already exists AND --execute given AND no --local-out-dir (which would otherwise be a
/// usage error): the early return must win, so this must exit 0 with SKIP, not exit 2.
#[test]
fn skip_if_exists_wins_over_execute_mode() {
    let dir = std::env::temp_dir().join(format!("e2e-703-skip-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let partial = dir.join("strih-partial-703.json");
    fs::write(&partial, r#"{"strih":"done"}"#).unwrap();

    let out = Command::new("bash")
        .arg(strih_script())
        .arg("--skip-if-exists")
        .arg(&partial)
        .arg("--execute") // would otherwise require --local-out-dir/--out — must never be reached
        .output()
        .expect("run recording-verdict-on-strih.sh --skip-if-exists --execute");
    assert_eq!(
        out.status.code(),
        Some(0),
        "#703: --skip-if-exists must win over --execute (early return before any execute-mode \
         validation). stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("SKIP"));
    fs::remove_dir_all(&dir).ok();
}

/// Default (no --execute) mode must be COMPLETELY UNCHANGED — still prints the plan, never
/// attempts ssh/scp. Regression guard for the #703 addition (mirrors the pre-existing
/// `strih_without_skip_flag_emits_plan_regardless` test in harness_verdict_done_marker.rs).
#[test]
fn strih_default_mode_still_prints_the_plan_not_execute() {
    let out = Command::new("bash")
        .arg(strih_script())
        .arg("--strih-rec")
        .arg(r"C:\rec\strih.mkv")
        .arg("--")
        .arg("--extract-partial")
        .arg("strih")
        .arg("--strih")
        .arg(r"C:\rec\strih.mkv")
        .arg("--out")
        .arg(r"C:\out\strih-partial.json")
        .output()
        .expect("run recording-verdict-on-strih.sh in default (plan) mode");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("STEP 1") && stdout.contains("win-strih"),
        "#703: without --execute, the plan-print output must be unchanged. stdout={stdout:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/recording-e2e.sh — E2E_EXECUTE_VERDICT wiring
// ---------------------------------------------------------------------------------------------

fn read_e2e() -> String {
    read("scripts/recording-e2e.sh")
}

#[test]
fn recording_e2e_defaults_e2e_execute_verdict_to_0() {
    let s = read_e2e();
    assert!(
        s.contains(r#"E2E_EXECUTE_VERDICT="${E2E_EXECUTE_VERDICT:-0}""#),
        "#703: E2E_EXECUTE_VERDICT must default to 0 (unchanged plan-print behavior for manual/ \
         workflow_dispatch runs) — only the CI workflow's pull_request gate sets it to 1"
    );
}

/// E2E_EXECUTE_VERDICT=1 must fail LOUD (non-zero exit) when WIN_VERDICT_EXE_LOCAL is missing —
/// never silently fall back to plan-print or a stale on-box binary.
#[test]
fn recording_e2e_fails_loud_without_win_verdict_exe_local_in_execute_mode() {
    let s = read_e2e();
    let exec_block = s
        .find(r#"if [ "$E2E_EXECUTE_VERDICT" = "1" ]; then"#)
        .expect("#703: recording-e2e.sh must have an E2E_EXECUTE_VERDICT=1 branch");
    let window = &s[exec_block..(exec_block + 900).min(s.len())];
    assert!(
        window.contains("WIN_VERDICT_EXE_LOCAL"),
        "#703: the execute-mode branch must validate WIN_VERDICT_EXE_LOCAL: {window}"
    );
    assert!(
        window.contains("exit 1"),
        "#703: a missing WIN_VERDICT_EXE_LOCAL must exit non-zero (fail closed), not degrade \
         silently: {window}"
    );
}

/// The strih AND stream extracts must be BACKGROUNDED (launched with `&`, PID captured) in
/// EXECUTE mode — this is the actual "parallel ssh decode-in-place" fix item #1 requires. In
/// default mode the SAME functions must run in the FOREGROUND (no `&`) — proven by requiring
/// BOTH an `&`-launch line AND a plain foreground call to each `run_*_extract` function.
#[test]
fn strih_and_stream_extracts_run_in_parallel_only_when_executing() {
    let s = read_e2e();
    for func in ["run_strih_extract", "run_stream_extract"] {
        assert!(
            s.contains(&format!("{func}()")),
            "#703: recording-e2e.sh must define {func}() (function-wrapped so it can be \
             conditionally backgrounded)"
        );
        assert!(
            s.contains(&format!("{func} >")),
            "#703: {func} must be launched with its output redirected to a log file when \
             backgrounded (execute mode)"
        );
        // A backgrounded launch (the redirected-output line ends in `&`).
        let bg_line = s
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{func} >")))
            .unwrap_or_else(|| panic!("#703: no backgrounded launch line for {func}"));
        assert!(
            bg_line.trim_end().ends_with('&'),
            "#703: {func}'s backgrounded launch line must end in `&`: {bg_line:?}"
        );
        // A plain foreground call (the default/else branch) — same function name, called bare,
        // with nothing after it on the line (no `&`, no redirection).
        let plain_call_exists = s.lines().any(|l| l.trim() == func);
        assert!(
            plain_call_exists,
            "#703: {func} must ALSO be called in the FOREGROUND (default/else branch, unchanged \
             plan-print behavior) — a bare `{func}` line with nothing else"
        );
    }
}

/// PIDs from the backgrounded launches must actually be `wait`-ed on before the merge runs.
#[test]
fn recording_e2e_waits_for_both_backgrounded_extracts_before_merging() {
    let s = read_e2e();
    assert!(
        s.contains(r#"wait "$STRIH_EXTRACT_PID""#),
        "#703: must wait for the backgrounded strih extract"
    );
    assert!(
        s.contains(r#"wait "$STREAM_EXTRACT_PID""#),
        "#703: must wait for the backgrounded stream extract"
    );
    // The wait block must appear BEFORE the merge is actually invoked ("$VERDICT_BIN" call).
    let wait_pos = s
        .find(r#"wait "$STRIH_EXTRACT_PID""#)
        .expect("wait for strih must exist");
    let merge_exec_pos = s
        .find(r#""$VERDICT_BIN" "${MERGE_ARGS[@]}""#)
        .expect("#703: the merge binary must actually be INVOKED (not just printed)");
    assert!(
        wait_pos < merge_exec_pos,
        "#703: must wait for both extracts BEFORE running the merge (wait_pos={wait_pos} \
         merge_exec_pos={merge_exec_pos})"
    );
}

/// A strih or stream extract failure (non-zero rc) must abort BEFORE attempting the merge — the
/// merge cannot produce a real verdict missing either required leg.
#[test]
fn recording_e2e_aborts_the_merge_on_a_failed_extract() {
    let s = read_e2e();
    assert!(
        s.contains("STRIH_EXTRACT_RC") && s.contains("STREAM_EXTRACT_RC"),
        "#703: must capture both extracts' exit codes"
    );
    let rc_check = s
        .find(r#"if [ "$STRIH_EXTRACT_RC" != "0" ] || [ "$STREAM_EXTRACT_RC" != "0" ]"#)
        .expect("#703: must check both extract exit codes before merging");
    let window = &s[rc_check..(rc_check + 400).min(s.len())];
    assert!(
        window.contains("exit 1"),
        "#703: a failed extract must abort the run (exit 1), not silently proceed to merge \
         missing data: {window}"
    );
}

/// The actual FIX: in EXECUTE mode the merge recording-verdict binary is genuinely INVOKED, and
/// the harness's OWN exit code becomes the merge's exit code (`exit "$GATE"`) — never a bare
/// `exit 0` once real execution happened. This is the literal bug #703 reports: the old code
/// always printed the plan and unconditionally `exit 0`'d.
#[test]
fn recording_e2e_execute_mode_runs_the_merge_and_propagates_its_exit_code() {
    let s = read_e2e();
    let exec_merge_block = s
        .find(r#""$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?"#)
        .expect(
        "#703: the execute-mode branch must actually invoke the merge and capture its exit code",
    );
    // #758: widened from 1600 to 2500 bytes -- the freeze-watch verdict check (item 3) legitimately
    // added ~400 bytes between the merge call and the exit (a real, justified addition, not scope
    // creep); the window itself is an arbitrary "comfortably larger than the text between them"
    // implementation constant, never a correctness bound, so it grows with genuinely new code
    // between the two anchors rather than the anchors moving.
    // #756: widened from 2500 to 3800 bytes -- the Member 3 live-latency-pins snapshot step
    // (scripts/latency_pins_snapshot.py invocation + its fail-open comment block) legitimately
    // added ~800 bytes between the merge call and the exit, same justified-growth pattern as #758.
    // #827: widened from 3800 to 4800 bytes -- the fleet-preflight excluded_cams jq-merge step
    // (cambox_offline_ack_excluded_json + its fail-open comment block) legitimately added ~540
    // bytes between the merge call and the exit, same justified-growth pattern as #758/#756.
    // #856: widened from 4800 to 6200 bytes -- the [8/8g] rig-wide A/V-correction combine step
    // (av_sync_combine_offsets.py invocation + its fail-open comment block) legitimately added
    // ~1300 bytes between the merge call and the exit, same justified-growth pattern as above.
    // #894: widened from 6200 to 6600 bytes -- the burn-unit run-integrity GATE combinator
    // (reads BURN_UNIT_INTEGRITY_MSG from the new [7b/8] check and tightens $GATE) legitimately
    // added ~400 bytes between the merge call and the exit, same justified-growth pattern as above.
    // #761: widened from 6600 to 7500 bytes -- the [8/8f-mv] MV-clone-vs-main skew snapshot step
    // (scripts/mv_skew_snapshot.py invocation + its fail-open comment block) legitimately added
    // ~850 bytes between the merge call and the exit, same justified-growth pattern as above.
    // #1124: widened from 7500 to 9000 bytes -- the post-verdict report-only diagnostics
    // (measurement-eq staleness note + edge-oscillation classifier, both fail-open and
    // $GATE-untouched) legitimately added ~820 bytes between the merge call and the exit,
    // same justified-growth pattern as above (measured distance 8319).
    let window = &s[exec_merge_block..(exec_merge_block + 9000).min(s.len())];
    assert!(
        window.contains(r#"exit "$GATE""#),
        "#703: after running the real merge, the branch must `exit \"$GATE\"` (the merge's own \
         exit code) — never fall through to a bare `exit 0`: {window}"
    );
    // The plan-print (default) path's OWN historical `exit 0` must still exist further down
    // (unchanged default behavior) — both branches co-exist, gated by E2E_EXECUTE_VERDICT.
    assert!(
        s.contains("\n  exit 0\nfi"),
        "#703: the default (plan-print) path's original `exit 0` must remain intact for \
         workflow_dispatch / manual runs"
    );
}

/// RUN_ID must be surfaced to GITHUB_ENV (when running under GH Actions) so the workflow's
/// fail-closed guard + scoped artifact upload can find THIS run's verdict JSON without a
/// fragile "most-recently-modified /tmp dir" heuristic.
#[test]
fn recording_e2e_surfaces_run_id_to_github_env() {
    let s = read_e2e();
    assert!(
        s.contains("RECORDING_E2E_RUN_ID=$RUN_ID") && s.contains("$GITHUB_ENV"),
        "#703: recording-e2e.sh must write RECORDING_E2E_RUN_ID to GITHUB_ENV when present"
    );
    // Must appear exactly once (no duplicate block from a concurrent-edit collision).
    let count = s.matches("RECORDING_E2E_RUN_ID=$RUN_ID").count();
    assert_eq!(
        count, 1,
        "#703: the RUN_ID -> GITHUB_ENV line must appear exactly once, found {count}"
    );
}

// ---------------------------------------------------------------------------------------------
// .github/workflows/full-path-e2e.yml
// ---------------------------------------------------------------------------------------------

fn read_workflow() -> String {
    read(".github/workflows/full-path-e2e.yml")
}

#[test]
fn workflow_has_actions_read_permission() {
    let s = read_workflow();
    assert!(
        s.contains("actions: read"),
        "#703: the workflow needs `actions: read` to query ci.yml's runs (gh run list/download)"
    );
}

#[test]
fn workflow_fetches_the_matching_windows_verdict_artifact_for_pull_request() {
    let s = read_workflow();
    assert!(
        s.contains("probe-tools-windows-amd64"),
        "#703: the fetch step must download the probe-tools-windows-amd64 artifact"
    );
    assert!(
        s.contains("workflow ci.yml"),
        "#703: the fetch step must query ci.yml's runs (the workflow that builds the Windows exe)"
    );
    assert!(
        s.contains("WIN_VERDICT_EXE_LOCAL=") && s.contains("GITHUB_ENV"),
        "#703: the fetch step must export WIN_VERDICT_EXE_LOCAL via GITHUB_ENV for the recording \
         step to consume"
    );
    // Must run BEFORE the recording step, and must be scoped to pull_request only (workflow_dispatch
    // stays in plan-print mode, never needs this).
    let fetch_pos = s
        .find("Fetch the matching Windows recording-verdict.exe")
        .expect("#703: the fetch step must exist");
    let recording_pos = s
        .find("run: exec bash scripts/recording-e2e.sh")
        .expect("the recording step must exist");
    assert!(
        fetch_pos < recording_pos,
        "the fetch step must run BEFORE the recording step"
    );
}

#[test]
fn workflow_sets_e2e_execute_verdict_for_pull_request_runs_only() {
    let s = read_workflow();
    assert!(
        s.contains("E2E_EXECUTE_VERDICT: ${{ github.event_name == 'pull_request' && '1' || '0' }}"),
        "#703: the recording step must set E2E_EXECUTE_VERDICT=1 ONLY for pull_request-triggered \
         runs (the required merge gate) — workflow_dispatch (manual soak) must stay 0"
    );
    // Must be in the SAME step's env block as ALL_CAMBOX/DURATION (mirrors the existing
    // full_path_e2e_yml_all_cambox_is_in_the_recording_steps_env_block test's technique).
    let step_pos = s
        .find("name: Recording-based 4-node cam2")
        .expect("the recording step must exist");
    let run_pos = s[step_pos..]
        .find("run: exec bash scripts/recording-e2e.sh")
        .map(|p| p + step_pos)
        .expect("the recording step must invoke recording-e2e.sh");
    let step_block = &s[step_pos..run_pos];
    assert!(
        step_block.contains("E2E_EXECUTE_VERDICT:"),
        "#703: E2E_EXECUTE_VERDICT must be set inside the recording step's own env: block: \
         {step_block}"
    );
}

/// The fail-closed structural guard (#703 item 2) — MUST assert the verdict JSON exists AND
/// overall_pass=true, as an INDEPENDENT check after the recording step (not just trust the
/// script's own exit code) — this is the defense-in-depth backstop against a FUTURE regression
/// of this exact bug (a code path that silently exits 0 without computing a verdict).
#[test]
fn workflow_has_the_fail_closed_structural_guard() {
    let s = read_workflow();
    let guard_pos = s
        .find("Fail-closed structural guard")
        .expect("#703: the fail-closed structural guard step must exist");
    let window = &s[guard_pos..(guard_pos + 2200).min(s.len())];
    assert!(
        window.contains("overall_pass"),
        "#703: the guard must assert overall_pass=true: {window}"
    );
    assert!(
        window.contains("jq"),
        "#703: the guard must read the verdict JSON via jq: {window}"
    );
    assert!(
        window.contains("exit 1"),
        "#703: the guard must exit non-zero on ANY failure mode (missing RUN_ID, missing JSON, \
         overall_pass != true): {window}"
    );
    // Must run even if the E2E step already failed, so the diagnosis is always clear.
    assert!(
        window.contains("always()"),
        "#703: the guard must run with always() so it reports clearly even after an earlier \
         step failure: {window}"
    );
    // Scoped to pull_request only — workflow_dispatch legitimately never auto-produces a
    // verdict JSON (the operator runs the merge manually afterward via the win-* MCP).
    assert!(
        window.contains("github.event_name == 'pull_request'"),
        "#703: the guard must be scoped to pull_request (workflow_dispatch stays in plan-print \
         mode and would wrongly red-X under this guard): {window}"
    );
    // Must run AFTER the recording step.
    let recording_pos = s
        .find("run: exec bash scripts/recording-e2e.sh")
        .expect("the recording step must exist");
    assert!(
        recording_pos < guard_pos,
        "#703: the fail-closed guard must run AFTER the recording step"
    );
}

/// The artifact upload must be scoped to THIS run's own RECORDING_E2E_RUN_ID — the old unscoped
/// `/tmp/recording-e2e-*/...` glob picked up every stale verdict JSON left on the self-hosted
/// runner's /tmp from unrelated past sessions, actively misleading forensics (#703 item 4).
#[test]
fn workflow_artifact_upload_is_scoped_to_this_runs_run_id() {
    let s = read_workflow();
    assert!(
        s.contains("/tmp/recording-e2e-${{ env.RECORDING_E2E_RUN_ID }}/verdict-*.json"),
        "#703: the artifact upload path must be scoped to env.RECORDING_E2E_RUN_ID, not an \
         unscoped /tmp/recording-e2e-*/ glob"
    );
    assert!(
        !s.contains("/tmp/recording-e2e-*/verdict-*.json"),
        "#703: the OLD unscoped glob must be gone — it uploaded every stale verdict on the \
         runner's /tmp, not just this run's own evidence"
    );
}
