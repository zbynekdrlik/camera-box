//! issue 1351 — after the M4 cut-over (strih role moved from the Windows STRIH-SNV PC to the
//! Linux notebook strih-lx, 10.77.9.202, `strih-obs.service` on GNOME/Wayland), the full-path E2E
//! gate must learn a per-box strih PLATFORM (`scripts/lib/strih-platform.sh`) and branch its two
//! Windows-only touch-points in `scripts/recording-e2e.sh`: the `[0/8]` obs64/AHK
//! session-visibility gate, and the `[8/8a]` strih record-extraction path (a NEW Linux sibling,
//! `scripts/recording-verdict-on-strih-lx.sh`, mirroring recording-verdict-on-imag.sh's
//! always-execute plain-ssh shape). The Windows path stays byte-identical (parallel-run
//! tolerant) — this file's negative-anchor tests pin that.
//!
//! Design-by: main (issue 1351, comment 5751246658) — Approach 1.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/strih-platform.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn onstrihlx_script() -> PathBuf {
    manifest_dir().join("scripts/recording-verdict-on-strih-lx.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `body` (which may call its pure functions). Returns stdout. Mirrors
/// tests/harness_obs_session_visibility_977.rs's `run_sourced` shape exactly.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
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

/// Run `body` with extra env vars set, source the lib, return stdout.
fn run_sourced_env(body: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", lib_script());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to run bash harness")
}

// ================================================================================================
// strih_platform — pure resolver.
// ================================================================================================

#[test]
fn strih_lx_ip_resolves_to_linux() {
    let out = run_sourced_env("strih_platform 10.77.9.202", &[]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "linux");
}

#[test]
fn an_unknown_host_defaults_to_windows() {
    let out = run_sourced_env("strih_platform 10.77.9.99", &[]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "windows");
}

#[test]
fn empty_host_defaults_to_windows() {
    let out = run_sourced_env("strih_platform ''", &[]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "windows");
}

#[test]
fn strih_platform_env_override_wins_even_for_the_lx_ip() {
    // an explicit STRIH_PLATFORM=windows must win outright, even against the known strih-lx IP —
    // this is the escape hatch for testing/rollback the design calls for.
    let out = run_sourced_env(
        "strih_platform 10.77.9.202",
        &[("STRIH_PLATFORM", "windows")],
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "windows");
}

#[test]
fn strih_platform_env_override_can_force_linux_for_any_host() {
    let out = run_sourced_env("strih_platform 10.77.9.99", &[("STRIH_PLATFORM", "linux")]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "linux");
}

#[test]
fn a_garbage_env_value_is_ignored_never_aborts_a_live_run() {
    // A typo'd env value must fall through to the normal host-based resolution, never abort.
    let out = run_sourced_env("strih_platform 10.77.9.202", &[("STRIH_PLATFORM", "bogus")]);
    assert!(
        out.status.success(),
        "a garbage STRIH_PLATFORM value must never abort: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "linux");
}

#[test]
fn strih_lx_host_env_can_repoint_the_known_linux_address() {
    let out = run_sourced_env(
        "strih_platform 10.99.99.99",
        &[("STRIH_LX_HOST", "10.99.99.99")],
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "linux");
}

// ================================================================================================
// strih_linux_visibility_probe_cmd / strih_linux_visibility_message — pure probe + parser, the
// Linux analogue of obs_session_visibility_probe_ps / obs_session_visibility_message. MUST NEVER
// probe obs64/AutoHotkey64 (the Windows CIM signature) — that is the whole point of this gate.
// ================================================================================================

#[test]
fn probe_cmd_checks_strih_obs_service_never_the_windows_cim_signature() {
    let p = run_sourced("strih_linux_visibility_probe_cmd");
    assert!(
        p.contains("systemctl --user is-active strih-obs.service"),
        "must probe strih-obs.service under the operator's own user session. Program:\n{p}"
    );
    assert!(
        !p.contains("obs64") && !p.contains("AutoHotkey64") && !p.contains("Get-Process"),
        "a Linux strih probe must NEVER reference the Windows obs64/AHK CIM signature. Program:\n{p}"
    );
}

fn message(probe_out: &str, ws_ok: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = dir.path().join("probe.txt");
    fs::write(&f, probe_out).expect("write probe fixture");
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\nprobe_out=\"$(cat \"$PROBE_FILE\")\"\nstrih_linux_visibility_message \"$probe_out\" {ws_ok}"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("PROBE_FILE", &f)
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

#[test]
fn active_service_plus_healthy_ws_is_fully_visible() {
    let msg = message("SVC_ACTIVE=active\n", "1");
    assert_eq!(
        msg.trim(),
        "",
        "an active strih-obs.service + a healthy WS round-trip must be fully visible"
    );
}

#[test]
fn inactive_service_is_invisible() {
    let msg = message("SVC_ACTIVE=inactive\n", "1");
    assert!(
        msg.contains("strih-obs.service") && msg.contains("1351"),
        "an inactive strih-obs.service must be reported, referencing issue 1351. msg={msg:?}"
    );
}

#[test]
fn active_service_but_dead_ws_is_invisible() {
    let msg = message("SVC_ACTIVE=active\n", "0");
    assert!(
        msg.to_lowercase().contains("websocket") || msg.contains(":4455"),
        "an active service but unreachable obs-websocket must be reported. msg={msg:?}"
    );
}

#[test]
fn empty_probe_output_is_invisible_never_a_silent_pass() {
    // Mirrors #833's "missing tool != measured zero" class — an ssh/connectivity failure must
    // never read as VISIBLE.
    let msg = message("", "0");
    assert!(
        !msg.trim().is_empty(),
        "empty probe output must produce a non-empty diagnosis, never a silent pass"
    );
}

#[test]
fn crlf_line_endings_do_not_cause_a_false_invisible() {
    let msg = message("SVC_ACTIVE=active\r\n", "1");
    assert_eq!(
        msg.trim(),
        "",
        "CRLF must parse identically to LF (defensive parity with the Windows parser's own \
         #977 CRLF fix, even though a Linux ssh session normally returns LF). msg={msg:?}"
    );
}

// ================================================================================================
// scripts/recording-verdict-on-strih-lx.sh — the Linux [8/8a] extraction sibling.
// ================================================================================================

#[test]
fn recording_verdict_on_strih_lx_script_exists_and_is_executable() {
    let meta = fs::metadata(onstrihlx_script())
        .unwrap_or_else(|e| panic!("issue 1351: recording-verdict-on-strih-lx.sh missing: {e}"));
    assert!(meta.is_file(), "must be a file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "recording-verdict-on-strih-lx.sh must be executable"
        );
    }
}

/// Mirrors on_imag_helper_targets_imag_over_plain_ssh_not_mcp — strih-lx is reached over plain
/// ssh/scp, NEVER the win-* MCP plan-printing shape.
#[test]
fn on_strih_lx_helper_targets_plain_ssh_never_mcp() {
    let s = read("scripts/recording-verdict-on-strih-lx.sh");
    assert!(
        s.contains("10.77.9.202"),
        "issue 1351: the on-strih-lx helper must reference the strih-lx box IP 10.77.9.202."
    );
    assert!(
        s.contains("sshpass") && s.contains("ssh"),
        "issue 1351: the on-strih-lx helper must use plain ssh/scp."
    );
    assert!(
        !s.contains("FileUpload") && !s.contains("FileDownload") && !s.contains("MCP Shell:"),
        "issue 1351: the on-strih-lx helper must not print an MCP plan (it EXECUTES directly \
         over ssh/scp, unlike the win-strih Windows sibling)."
    );
}

#[test]
fn build_onstrihlx_command_quotes_safely_and_sets_rust_log() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; build_onstrihlx_command \"$2\" --strih \"$3\" --out \"$4\"")
        .arg("bash")
        .arg(onstrihlx_script())
        .arg("/home/newlevel/recording-verdict")
        .arg("/home/newlevel/strih REC.mkv") // a path WITH a space
        .arg("/home/newlevel/verdict-out/strih-partial-1.json")
        .output()
        .expect("run build_onstrihlx_command");
    assert!(
        out.status.success(),
        "build_onstrihlx_command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cmd = String::from_utf8_lossy(&out.stdout);
    assert!(
        cmd.contains("RUST_LOG=info"),
        "must set RUST_LOG=info. Got: {cmd:?}"
    );
    assert!(
        cmd.contains("/home/newlevel/recording-verdict"),
        "must run the deployed verdict binary. Got: {cmd:?}"
    );
    assert!(
        cmd.contains("--strih") && cmd.contains("--out"),
        "must forward the --strih/--out args. Got: {cmd:?}"
    );
}

/// review finding (issue 1351): onstrihlx_upload_decision must mirror
/// recording-verdict-on-imag.sh's issue-1118 sha256 VERSION GATE exactly — a present-but-stale
/// binary (differing sha256) must be re-uploaded, never silently reused.
#[test]
fn onstrihlx_upload_decision_mirrors_the_imag_sha256_version_gate() {
    fn decide(force: &str, present: &str, local_sha: &str, remote_sha: &str) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$1\"; onstrihlx_upload_decision \"$2\" \"$3\" \"$4\" \"$5\"")
            .arg("bash")
            .arg(onstrihlx_script())
            .arg(force)
            .arg(present)
            .arg(local_sha)
            .arg(remote_sha)
            .output()
            .expect("run onstrihlx_upload_decision");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    assert_eq!(
        decide("1", "1", "abc", "abc"),
        "upload",
        "force always wins"
    );
    assert_eq!(decide("0", "0", "", ""), "upload", "absent -> upload");
    assert_eq!(
        decide("0", "1", "", "abc"),
        "upload",
        "present but local sha unknown -> fail-safe upload"
    );
    assert_eq!(
        decide("0", "1", "abc", "def"),
        "upload",
        "present but DIFFERING sha256 (stale/schema-drifted binary) -> re-upload"
    );
    assert_eq!(
        decide("0", "1", "abc", "abc"),
        "skip",
        "present AND identical sha256 -> skip (fast idempotent path)"
    );
}

// ================================================================================================
// Wiring into scripts/recording-e2e.sh — new lines only; the Windows path stays byte-identical.
// ================================================================================================

#[test]
fn recording_e2e_sources_the_new_lib() {
    let body = read("scripts/recording-e2e.sh");
    assert!(
        body.contains("lib/strih-platform.sh"),
        "recording-e2e.sh must source scripts/lib/strih-platform.sh"
    );
}

#[test]
fn zero_eight_gate_branches_on_strih_platform_before_the_windows_probe() {
    let body = read("scripts/recording-e2e.sh");
    let banner_pos = body
        .find("obs64/AHK session-visibility gate")
        .expect("the [0/8] banner must still exist");
    let window = &body[banner_pos..(banner_pos + 3200).min(body.len())];
    assert!(
        window.contains("strih_platform \"$STRIH\""),
        "the [0/8] gate must call strih_platform to decide the branch. Window:\n{window}"
    );
    assert!(
        window.contains("strih_linux_visibility_check"),
        "the [0/8] gate must call the Linux visibility check on a linux strih. Window:\n{window}"
    );
    // The Windows-only probe call must still be present too — reached in the `else` branch.
    assert!(
        window.contains("obs_session_visibility_probe_ps 1")
            && window.contains("obs_session_visibility_probe_ps 0"),
        "the Windows obs64/AHK probe (strih AND stream) must still be present, reached for a \
         non-Linux strih and always for stream. Window:\n{window}"
    );
}

/// NEGATIVE ANCHOR: the Windows stream leg (never branched — stream stays Windows-only per the
/// design) must be byte-identical to its pre-1351 text. Pinning this whole block verbatim proves
/// the edit added new lines around the strih leg without touching the stream leg at all.
#[test]
fn stream_leg_of_zero_eight_gate_is_byte_identical() {
    let body = read("scripts/recording-e2e.sh");
    let expected = r#"echo "    ok: strih obs64/AHK visible on the console (SessionId=1, window present)"
_svg_stream_out="$(timeout "$SVG_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
  "$HERE/lib/win-ssh-exec.sh" "$STREAM_USER" "$STREAM_PW" "$STREAM" "$(obs_session_visibility_probe_ps 0)" 2>/dev/null || true)"
_svg_stream_msg="$(obs_session_visibility_message "$_svg_stream_out" 0)"
if [ -n "$_svg_stream_msg" ]; then
  echo "ERROR: [0/8] stream INVISIBLE: $_svg_stream_msg" >&2
  echo "       Recovery: bash scripts/launch-obs-genlock.sh --box stream --force   # paste into the win-stream-snv MCP Shell (session 1, never ssh+CIM — issue 958)" >&2
  exit 1
fi
echo "    ok: stream obs64 visible on the console (SessionId=1, window present)"
"#;
    assert!(
        body.contains(expected),
        "the stream leg of the [0/8] gate must be byte-identical (stream stays Windows-only per \
         the issue-1351 design) — it was never touched by this change."
    );
}

/// NEGATIVE ANCHOR: the Windows-only STRIH probe (`else` branch of the new platform check) must
/// be byte-identical to its pre-1351 text — proven by pinning the exact original 3-line block.
#[test]
fn windows_strih_probe_branch_is_byte_identical_to_the_original() {
    let body = read("scripts/recording-e2e.sh");
    let expected = r#"  _svg_strih_out="$(timeout "$SVG_SSH_TIMEOUT" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
    "$HERE/lib/win-ssh-exec.sh" "$STRIH_USER" "$STRIH_PW" "$STRIH" "$(obs_session_visibility_probe_ps 1)" 2>/dev/null || true)"
  _svg_strih_msg="$(obs_session_visibility_message "$_svg_strih_out" 1)"
fi
"#;
    assert!(
        body.contains(expected),
        "the Windows-only strih probe branch must be byte-identical to its pre-1351 text \
         (only its INDENTATION context changed by being wrapped in the new if/else)."
    );
}

#[test]
fn run_strih_extract_branches_on_platform_and_calls_the_linux_sibling() {
    let body = read("scripts/recording-e2e.sh");
    let fn_pos = body
        .find("run_strih_extract() {")
        .expect("run_strih_extract must still be defined");
    let window = &body[fn_pos..(fn_pos + 1400).min(body.len())];
    assert!(
        window.contains("strih_platform \"$STRIH\""),
        "run_strih_extract must branch on strih_platform. Window:\n{window}"
    );
    assert!(
        window.contains("recording-verdict-on-strih-lx.sh"),
        "run_strih_extract must call the new Linux sibling on a linux strih. Window:\n{window}"
    );
    assert!(
        window.contains("recording-verdict-on-strih.sh"),
        "run_strih_extract must still call the (unchanged) Windows script too. Window:\n{window}"
    );
}

/// NEGATIVE ANCHOR: the Windows-only branch of run_strih_extract (the original call, now reached
/// after the linux-branch `return`) must be byte-identical to the pre-1351 text.
#[test]
fn run_strih_extract_windows_branch_is_byte_identical_to_the_original() {
    let body = read("scripts/recording-e2e.sh");
    let expected = r#"    "$HERE/recording-verdict-on-strih.sh" --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" \
      --strih-rec "$strih_rec_win" \
      -- --extract-partial strih --strih "$strih_rec_win" --capture-fps "$STRIH_CAPTURE_FPS" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$ZL_BURN_STRIH_RUN_ID" \
         --out "$strih_partial_win"
    echo "    pull back to dev1: $strih_partial  (win-strih FileDownload $strih_partial_win -> $strih_partial)"

    echo "    --- [$label 8b] extract the STREAM partial ON the stream box (win-stream-snv), in place ---"
    "$HERE/recording-verdict-on-stream.sh" --verdict-exe "$VERDICT_EXE_WIN" --out-dir "$OUT_DIR_WIN" \
      --stream-rec "$stream_rec_win" \
      -- --extract-partial stream --stream "$stream_rec_win" --capture-fps "$STREAM_CAPTURE_FPS" \
         --strih-emit-fps "$STRIH_CAPTURE_FPS" --stream-capture-fps "$STREAM_CAPTURE_FPS" \
         --cam2-run-id "$RUN_ID" \
         --burn-cam1-run-id "$BURN_CAM1_RUN_ID" --burn-strih-run-id "$ZL_BURN_STRIH_RUN_ID" \
         --burn-stream-run-id "$ZL_BURN_STREAM_RUN_ID" \
         --out "$stream_partial_win"
    echo "    pull back to dev1: $stream_partial  (win-stream-snv FileDownload $stream_partial_win -> $stream_partial)"

    local out_json="$OUTDIR/zero-loss-restart-${label}-${RUN_ID}.json"
    local merge_bin
    merge_bin="$(cd "$PROBE_BIN_DIR" && pwd)/recording-verdict"
    echo "    --- [$label 8c] MERGE the two small partials ON dev1 -> the '$label' zero-loss verdict JSON ---"
    printf '      %q --merge-partials %q --merge-partials %q --min-secs 300 --capture-fps %q --strih-emit-fps %q --stream-capture-fps %q --cam2-run-id %q --burn-cam1-run-id %q --burn-strih-run-id %q --burn-stream-run-id %q --json %q\n' \
      "$merge_bin" "strih=$strih_partial" "stream=$stream_partial" "$STRIH_CAPTURE_FPS" \
      "$STRIH_CAPTURE_FPS" "$STREAM_CAPTURE_FPS" "$RUN_ID" \
      "$BURN_CAM1_RUN_ID" "$ZL_BURN_STRIH_RUN_ID" "$ZL_BURN_STREAM_RUN_ID" "$out_json"
    echo "    -> once pulled back + merged, writes the '$label' zero-loss verdict JSON: $out_json"
  }
"#;
    assert!(
        body.contains(expected),
        "run_strih_extract's Windows branch must be byte-identical to its pre-1351 text."
    );
}

/// #1351 hotfix (supervisor, live-rig E2E finding): the strih-lx ssh/scp opts MUST carry
/// `-o UserKnownHostsFile=/dev/null`. The M4 cut-over gave 10.77.9.202 a new host key, and
/// `StrictHostKeyChecking=no` does NOT override a CHANGED key — without this the [0/8] Linux
/// visibility probe AND the record extraction fail with an empty read ("strih-lx unreachable"),
/// aborting the E2E. Matches the fleet-standard ssh shape used everywhere else.
#[test]
fn strih_lx_ssh_opts_ignore_stale_known_hosts_1351() {
    let lib = read("scripts/lib/strih-platform.sh");
    assert!(
        lib.contains("-o UserKnownHostsFile=/dev/null"),
        "strih_linux_visibility_check ssh must set UserKnownHostsFile=/dev/null (M4 .202 host-key change)"
    );
    let ext = read("scripts/recording-verdict-on-strih-lx.sh");
    assert!(
        ext.contains("-o UserKnownHostsFile=/dev/null"),
        "recording-verdict-on-strih-lx.sh SSH_OPTS must set UserKnownHostsFile=/dev/null (ssh + scp)"
    );
}

/// #1351 hotfix (live-rig E2E finding): the linux visibility branch MUST initialize
/// `_svg_strih_out` (empty). The downstream #1295 zombie-note read references it unconditionally,
/// and `set -u` aborts ("unbound variable") on a Linux strih otherwise (the Windows branch sets it,
/// the linux branch previously did not).
#[test]
fn linux_visibility_branch_inits_svg_strih_out_for_set_u_1351() {
    let body = read("scripts/recording-e2e.sh");
    let pos = body
        .find("_svg_strih_msg=\"$(strih_linux_visibility_check")
        .expect("the linux visibility branch must exist");
    let window = &body[pos.saturating_sub(220)..pos];
    assert!(
        window.contains("_svg_strih_out=\"\""),
        "the linux visibility branch must init _svg_strih_out=\"\" (set -u safety for the #1295 znote read)"
    );
}

// ================================================================================================
// issue 1351 follow-up — the `[0/8]` version-integrity gate invocation passes `--strih-linux` iff
// strih_platform resolves linux; the Windows path stays byte-identical.
// ================================================================================================

/// The `[0/8]` version-integrity gate region resolves `strih_platform "$STRIH"` into a flag
/// variable and threads it into BOTH the imag-acked and non-acked `version-integrity-gate.sh`
/// invocation shapes via the SAME `${VAR:+--flag}` convention `AUTO_WIN_MANIFEST`/`IMAG_SO_CSV`
/// already use elsewhere in this exact block -- never a bare unconditional literal.
#[test]
fn zero_eight_version_integrity_gate_threads_strih_linux_flag_conditionally_1351() {
    let body = read("scripts/recording-e2e.sh");
    let banner_pos = body
        .find("[0/8] version-integrity gate")
        .expect("the [0/8] version-integrity banner must still exist");
    let end_pos = body[banner_pos..]
        .find("dantesync fleet-wide VERSION-PARITY gate")
        .map(|p| banner_pos + p)
        .expect("the following dantesync fleet-wide banner must still exist");
    let window = &body[banner_pos..end_pos];
    assert!(
        window.contains("strih_platform \"$STRIH\""),
        "the [0/8] version-integrity region must resolve strih_platform to decide the flag. \
         Window:\n{window}"
    );
    assert!(
        window.contains("STRIH_LINUX_GATE_ARG=\"1\""),
        "a linux strih must set the flag var to a non-empty value. Window:\n{window}"
    );
    // The flag must be threaded via the established conditional-arg convention, never a bare
    // unconditional literal -- count exactly TWO occurrences (one per invocation shape: the
    // imag-acked branch and the non-acked branch).
    let occurrences = window
        .matches("${STRIH_LINUX_GATE_ARG:+--strih-linux}")
        .count();
    assert_eq!(
        occurrences, 2,
        "--strih-linux must be threaded conditionally into BOTH version-integrity-gate.sh \
         invocation shapes (imag-acked + non-acked), never a bare literal. Window:\n{window}"
    );
}

/// NEGATIVE ANCHOR: the Windows `--win-state` invocation lines (both shapes) stay byte-identical
/// to their pre-1351 text -- the new flag is APPENDED, never inserted between/replacing existing
/// args.
#[test]
fn zero_eight_version_integrity_gate_windows_invocation_args_are_byte_identical_1351() {
    let body = read("scripts/recording-e2e.sh");
    assert!(
        body.contains(
            "--win-state \"strih=$VERSION_STRIH_STATE\" \\\n    --win-state \"stream=$VERSION_STREAM_STATE\" \\\n    --imag-acked-offline \"$IMAG_OFFLINE_ACK_REASON\""
        ),
        "the imag-acked version-integrity-gate.sh invocation's core args must be byte-identical \
         to their pre-1351 text (the new flag is appended after, never inserted inside)"
    );
    assert!(
        body.contains(
            "--win-state \"strih=$VERSION_STRIH_STATE\" \\\n  --win-state \"stream=$VERSION_STREAM_STATE\" \\\n  --genlock-sha \"imag=$IMAG_GENLOCK_SHA\""
        ),
        "the non-acked version-integrity-gate.sh invocation's core args must be byte-identical to \
         their pre-1351 text (the new flag is appended after, never inserted inside)"
    );
}
