//! Regression guard for `scripts/lib/capture-rate-guard.sh` (#656 prevention item 2) — the
//! shared "source camera's captured fps has sustained a real rate defect" journal signal, and
//! its wiring into `scripts/recording-e2e.sh`'s preflight (before any deploy/record step, so a
//! doomed 30-minute E2E run is never even started against a defective grabber).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn lib_script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/capture-rate-guard.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source the shared lib and run `body`, returning stdout. Asserts the harness itself exited 0.
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

#[test]
fn capture_rate_defect_grep_pattern_is_the_exact_656_signal() {
    let out = run_sourced("capture_rate_defect_grep_pattern");
    assert_eq!(out.trim(), "#656 capture-delivery-rate DEFECTIVE");
}

/// The pattern must actually MATCH the real WARN line src/main.rs's capture loop emits
/// (src/capture_rate_health.rs's message, verbatim shape).
#[test]
fn capture_rate_defect_grep_pattern_matches_the_real_warn_line() {
    let pattern = run_sourced("capture_rate_defect_grep_pattern")
        .trim()
        .to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'WARN #656 capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps configured/negotiated (>1.0% deviation sustained for 6 consecutive report windows, ~30s) — USB-reset the capture device (see #656)' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        out.status.success(),
        "capture_rate_defect_grep_pattern must match the real #656 WARN line. Pattern: {pattern}"
    );
}

/// A perfectly healthy "Streaming: ..." line must NOT match — this preflight must never
/// false-fail a healthy box.
#[test]
fn capture_rate_defect_grep_pattern_does_not_match_a_healthy_line() {
    let pattern = run_sourced("capture_rate_defect_grep_pattern")
        .trim()
        .to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' 'Streaming: 59.9 fps emitted / 60.02 fps captured (1798 sent, 1801 captured, 0 capture-dropped)' | grep -E -- '{pattern}'"
        ))
        .output()
        .expect("failed to run grep");
    assert!(
        !out.status.success(),
        "capture_rate_defect_grep_pattern must NOT match a healthy Streaming: line. Pattern: {pattern}"
    );
}

#[test]
fn preflight_message_extracts_captured_and_configured_fps() {
    let out = run_sourced(
        "capture_rate_preflight_message cam1 'WARN #656 capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps configured/negotiated (>1.0% deviation sustained for 6 consecutive report windows, ~30s) — USB-reset the capture device (see #656)'",
    );
    assert_eq!(
        out.trim(),
        "cam1 capture rate defective (~63.20fps, expected 60.00fps) — USB-reset the grabber (see #656)"
    );
}

#[test]
fn preflight_message_falls_back_gracefully_on_an_unparseable_line() {
    // The signal is present (grep matched something) but the message shape doesn't parse the
    // fps values out — never silently swallow the signal, echo the raw line instead.
    let out = run_sourced(
        "capture_rate_preflight_message cam3 'a #656 capture-delivery-rate DEFECTIVE line in an unexpected shape'",
    );
    assert_eq!(
        out.trim(),
        "cam3 capture rate defective (see #656): a #656 capture-delivery-rate DEFECTIVE line in an unexpected shape"
    );
}

// ---- Wiring into recording-e2e.sh -----------------------------------------------------------

#[test]
fn recording_e2e_sources_the_capture_rate_guard_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/capture-rate-guard.sh\""),
        "recording-e2e.sh must source scripts/lib/capture-rate-guard.sh"
    );
}

#[test]
fn recording_e2e_runs_the_capture_rate_preflight_before_any_deploy_or_record_step() {
    let s = read("scripts/recording-e2e.sh");
    let preflight_pos = s
        .find("capture-delivery-rate preflight")
        .expect("the #656 capture-rate preflight step must exist");
    let grep_call_pos = s
        .find("capture_rate_defect_grep_pattern)")
        .expect("the preflight must actually call capture_rate_defect_grep_pattern");
    assert!(
        grep_call_pos > preflight_pos,
        "the grep-pattern call must be part of the preflight step block"
    );

    // Must run strictly BEFORE [2/8] (the SOURCE-camera deploy step) so a defective grabber is
    // caught before any binary is even pushed, never mind a 30-minute recording. Anchored on the
    // ACTUAL step-header echo (not a bare "[2/8]" substring — that also appears earlier in
    // unrelated comments, e.g. the #24 item-1 SOURCE-camera-role note near the top of the file).
    let deploy_pos = s
        .find("echo \"[2/8] $CAMERA_NAME (")
        .expect("recording-e2e.sh must still have a [2/8] deploy step");
    assert!(
        preflight_pos < deploy_pos,
        "the #656 capture-rate preflight must run BEFORE [2/8] deploy (fail fast, before \
         burning any deploy/record time)"
    );

    // A matched defect line must abort the run (exit 1), never just warn and continue — this is
    // a HARD fail-fast gate, not an advisory. Scoped tightly to JUST this preflight's own block
    // (preflight_pos .. its own "ok:" success echo) so an unrelated exit-1 elsewhere in the file
    // (there are several other preflight gates before [2/8]) can never make this assertion
    // vacuously pass.
    let preflight_ok_pos = s
        .find("no sustained capture-rate defect in $CAMERA_NAME's recent journal")
        .expect("the preflight must print a success line when no defect is found");
    assert!(
        preflight_ok_pos > preflight_pos,
        "the success echo must come after the preflight step header"
    );
    let this_preflight_block = &s[preflight_pos..preflight_ok_pos];
    assert!(
        this_preflight_block.contains("exit 1"),
        "the #656 capture-rate preflight must exit 1 on a matched defect line"
    );
}

#[test]
fn recording_e2e_preflight_uses_the_shared_message_formatter() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("capture_rate_preflight_message \"$CAMERA_NAME\" \"$CAPTURE_RATE_DEFECT_LINE\""),
        "the preflight must format its fail message via the shared, unit-tested \
         capture_rate_preflight_message helper (never a second ad-hoc string built inline)"
    );
}

// ---------------------------------------------------------------------------------------------
// #693 — `journalctl -u camera-box -n 200` spans ACROSS service restarts (not scoped to the
// CURRENTLY RUNNING process). Live incident 2026-07-11: a routine cleanup() restart bounced
// cam1's camera-box.service; the OLD process instance's stale #656 DEFECTIVE WARN (logged 2s
// before the restart) was still inside the NEW instance's 200-line lookback and false-failed the
// required merge gate's preflight even though the new process's own captured rate was already
// healthy. Same journal-freshness bug class already fixed for DanteSync reads (#550/#591/#595/
// #607/#686). Fix: scope the read to the CURRENT process instance via
// `_SYSTEMD_INVOCATION_ID=<uuid>` (from `systemctl show -p InvocationID --value camera-box`).
// ---------------------------------------------------------------------------------------------

#[test]
fn capture_rate_journalctl_cmd_scopes_to_the_given_invocation_id() {
    let out = run_sourced("capture_rate_journalctl_cmd 'abc-123-def'");
    let cmd = out.trim();
    assert!(
        cmd.contains("_SYSTEMD_INVOCATION_ID=abc-123-def"),
        "#693: with a non-empty invocation id, the journalctl command must scope to it \
         (_SYSTEMD_INVOCATION_ID=<id>) so a stale line from a KILLED prior instance can never \
         leak into the lookback window. Got: {cmd}"
    );
    assert!(
        !cmd.contains("-u camera-box"),
        "#693: an invocation-id-scoped read must not ALSO use the unscoped -u camera-box form \
         (that would defeat the scoping). Got: {cmd}"
    );
}

#[test]
fn capture_rate_journalctl_cmd_falls_back_to_the_unscoped_unit_read_when_invocation_id_is_empty() {
    // systemctl show can fail/return empty (e.g. an older systemd, a transient SSH hiccup) --
    // never silently skip the WHOLE preflight just because the invocation id couldn't be read.
    let out = run_sourced("capture_rate_journalctl_cmd ''");
    let cmd = out.trim();
    assert!(
        cmd.contains("-u camera-box"),
        "#693: with an empty invocation id, must fall back to the original -u camera-box form \
         (never silently produce no command at all). Got: {cmd}"
    );
}

// ---------------------------------------------------------------------------------------------
// #694 — deploy-fleet.sh, verify-device.sh, and upgrade-fleet-ndi.sh have the SAME
// stale-journal-across-restart exposure #693 fixed for recording-e2e.sh's preflight, but each
// reads a DIFFERENT lookback window (200 or 300 lines) than the hardcoded "-n 200" this function
// originally emitted. Extend it with an optional LINES arg (default 200, so the existing
// recording-e2e.sh call site — which passes only the invocation id — is unaffected) so every
// caller can reuse ONE scoped-journalctl builder instead of duplicating the scoping logic.
// ---------------------------------------------------------------------------------------------

#[test]
fn capture_rate_journalctl_cmd_defaults_to_200_lines_when_lines_arg_omitted() {
    let out = run_sourced("capture_rate_journalctl_cmd 'abc-123'");
    let cmd = out.trim();
    assert!(
        cmd.contains("-n 200"),
        "#694: omitting the LINES arg must default to the original -n 200 behavior. Got: {cmd}"
    );
}

#[test]
fn capture_rate_journalctl_cmd_accepts_a_custom_line_count_scoped() {
    let out = run_sourced("capture_rate_journalctl_cmd 'abc-123' 300");
    let cmd = out.trim();
    assert!(
        cmd.contains("_SYSTEMD_INVOCATION_ID=abc-123"),
        "#694: a custom line count must not lose the invocation-id scoping. Got: {cmd}"
    );
    assert!(
        cmd.contains("-n 300"),
        "#694: passing 300 as the LINES arg must produce '-n 300', not the hardcoded default. \
         Got: {cmd}"
    );
    assert!(
        !cmd.contains("-n 200"),
        "#694: a custom line count must replace, not append to, the default. Got: {cmd}"
    );
}

#[test]
fn capture_rate_journalctl_cmd_accepts_a_custom_line_count_unscoped_fallback() {
    let out = run_sourced("capture_rate_journalctl_cmd '' 300");
    let cmd = out.trim();
    assert!(
        cmd.contains("-u camera-box"),
        "#694: an empty invocation id must still fall back to the unscoped -u camera-box form \
         even with a custom line count. Got: {cmd}"
    );
    assert!(
        cmd.contains("-n 300"),
        "#694: the unscoped fallback must also honor a custom line count. Got: {cmd}"
    );
}

#[test]
fn capture_rate_journalctl_cmd_output_is_valid_remote_shell() {
    let out = run_sourced("capture_rate_journalctl_cmd 'abc-123'");
    let checker = Command::new("bash")
        .arg("-n")
        .arg("-c")
        .arg(out.trim())
        .output()
        .expect("run bash -n");
    assert!(
        checker.status.success(),
        "#693: capture_rate_journalctl_cmd's output must be syntactically valid shell text"
    );
}

/// recording-e2e.sh must resolve the CURRENT camera-box.service invocation id BEFORE reading the
/// journal for the #656 defect line, and use capture_rate_journalctl_cmd instead of the old bare
/// `journalctl -u camera-box --no-pager -n 200` literal (which read across restarts, #693).
#[test]
fn recording_e2e_preflight_scopes_the_journal_read_to_the_current_invocation() {
    let s = read("scripts/recording-e2e.sh");
    let invocation_pos = s
        .find("InvocationID")
        .expect("#693: the preflight must resolve camera-box's CURRENT InvocationID");
    let cmd_call_pos = s.find("capture_rate_journalctl_cmd").expect(
        "#693: the preflight must build its journalctl read via capture_rate_journalctl_cmd",
    );
    assert!(
        invocation_pos < cmd_call_pos,
        "#693: the invocation id must be resolved BEFORE it is used to scope the journalctl read"
    );
    assert!(
        !s.contains("journalctl -u camera-box --no-pager -n 200 2>/dev/null | grep"),
        "#693: the OLD unscoped-across-restarts journalctl literal must be gone from the \
         preflight (replaced by the invocation-id-scoped capture_rate_journalctl_cmd)"
    );
}

// ---------------------------------------------------------------------------------------------
// #705 — the [0/8] preflight above only proves the source camera was clean BEFORE a run starts;
// the #656/#663 ShadowCast judder is confirmed to RECUR mid-session (PR #704's own real-verdict
// CI run: cam1's own recurrence_heal_count=30), so a clean preflight does not guarantee the
// recording stayed clean for its whole duration. These cover the NEW mid-recording (POST
// StartRecord..StopRecord window) re-check: a window-bounded journalctl builder
// (capture_rate_window_journalctl_cmd, journalctl's own native --since=@N/--until=@N absolute-
// time filtering -- no bash-side timestamp parsing needed) and a distinct diagnostic message
// (capture_rate_recurrence_message) so a recurrence during the recording never gets confused
// with a fresh chain-loss/zero-loss regression surfaced elsewhere in the verdict.
// ---------------------------------------------------------------------------------------------

#[test]
fn capture_rate_window_journalctl_cmd_scopes_to_invocation_id_and_time_window() {
    let out = run_sourced("capture_rate_window_journalctl_cmd 'abc-123-def' 1000 2000");
    let cmd = out.trim();
    assert!(
        cmd.contains("_SYSTEMD_INVOCATION_ID=abc-123-def"),
        "#705: with a non-empty invocation id, must scope to it (mirrors #693's scoping) so a \
         stale line from a KILLED prior instance can never leak into the window. Got: {cmd}"
    );
    assert!(
        cmd.contains("--since=@1000"),
        "#705: must bound the read to the recording window's START via journalctl's native \
         absolute-time --since=@epoch form. Got: {cmd}"
    );
    assert!(
        cmd.contains("--until=@2000"),
        "#705: must bound the read to the recording window's END via journalctl's native \
         absolute-time --until=@epoch form. Got: {cmd}"
    );
    assert!(
        !cmd.contains("-u camera-box"),
        "#705: an invocation-id-scoped read must not ALSO use the unscoped -u camera-box form \
         (defeats the scoping). Got: {cmd}"
    );
}

#[test]
fn capture_rate_window_journalctl_cmd_falls_back_to_unscoped_unit_read_when_invocation_id_empty() {
    let out = run_sourced("capture_rate_window_journalctl_cmd '' 1000 2000");
    let cmd = out.trim();
    assert!(
        cmd.contains("-u camera-box"),
        "#705: an empty invocation id must fall back to the unscoped -u camera-box form (mirrors \
         capture_rate_journalctl_cmd's own fallback contract). Got: {cmd}"
    );
    assert!(
        cmd.contains("--since=@1000") && cmd.contains("--until=@2000"),
        "#705: the unscoped fallback must still honor the time window. Got: {cmd}"
    );
}

#[test]
fn capture_rate_window_journalctl_cmd_output_is_valid_remote_shell() {
    let out = run_sourced("capture_rate_window_journalctl_cmd 'abc-123' 1000 2000");
    let checker = Command::new("bash")
        .arg("-n")
        .arg("-c")
        .arg(out.trim())
        .output()
        .expect("run bash -n");
    assert!(
        checker.status.success(),
        "#705: capture_rate_window_journalctl_cmd's output must be syntactically valid shell text"
    );
}

#[test]
fn recurrence_message_extracts_captured_and_configured_fps_and_reads_distinctly_from_preflight() {
    let out = run_sourced(
        "capture_rate_recurrence_message cam1 'WARN #656 capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps configured/negotiated (>1.0% deviation sustained for 6 consecutive report windows, ~30s) — USB-reset the capture device (see #656)'",
    );
    let msg = out.trim();
    assert!(
        msg.contains("63.20") && msg.contains("60.00"),
        "#705: recurrence message must extract the real captured/configured fps. Got: {msg}"
    );
    assert!(
        msg.contains("RECURRED DURING"),
        "#705: the recurrence message must read distinctly from capture_rate_preflight_message's \
         'capture rate defective' wording, so a human/CI reader can tell a mid-recording \
         recurrence apart from the pre-recording preflight failure. Got: {msg}"
    );
    assert!(
        msg.contains("#705"),
        "#705: the recurrence message should point at this ticket for context. Got: {msg}"
    );
    assert_ne!(
        msg,
        "cam1 capture rate defective (~63.20fps, expected 60.00fps) — USB-reset the grabber (see #656)",
        "#705: must NOT be byte-identical to capture_rate_preflight_message's output"
    );
}

#[test]
fn recurrence_message_falls_back_gracefully_on_an_unparseable_line() {
    let out = run_sourced(
        "capture_rate_recurrence_message cam3 'a #656 capture-delivery-rate DEFECTIVE line in an unexpected shape'",
    );
    let msg = out.trim();
    assert!(
        msg.contains("cam3"),
        "#705: fallback message must still name the camera. Got: {msg}"
    );
    assert!(
        msg.contains("a #656 capture-delivery-rate DEFECTIVE line in an unexpected shape"),
        "#705: fallback must echo the raw matched line, never silently swallow the signal. Got: {msg}"
    );
}

// ---- Wiring the mid-recording check into recording-e2e.sh -------------------------------------

#[test]
fn recording_e2e_captures_the_recording_window_start_and_end_epoch() {
    let s = read("scripts/recording-e2e.sh");
    let start_record_pos = s
        .find("echo \"[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    let window_start_pos = s
        .find("CAPTURE_RATE_WINDOW_START_EPOCH=")
        .expect("#705: recording-e2e.sh must snapshot the recording window's START epoch");
    assert!(
        window_start_pos > start_record_pos,
        "#705: the window START epoch must be captured AFTER StartRecord actually runs"
    );

    let stop_record_pos = s
        .find("echo \"[7/8] StopRecord")
        .expect("[7/8] StopRecord step must exist");
    let window_end_pos = s
        .find("CAPTURE_RATE_WINDOW_END_EPOCH=")
        .expect("#705: recording-e2e.sh must snapshot the recording window's END epoch");
    assert!(
        window_end_pos > stop_record_pos,
        "#705: the window END epoch must be captured AFTER the [7/8] StopRecord step begins"
    );
    assert!(
        window_end_pos > window_start_pos,
        "#705: the window END epoch capture must textually follow the window START capture"
    );
}

#[test]
fn recording_e2e_runs_the_post_recording_capture_rate_check_after_stoprecord() {
    let s = read("scripts/recording-e2e.sh");
    let window_end_pos = s
        .find("CAPTURE_RATE_WINDOW_END_EPOCH=")
        .expect("#705: window END epoch capture must exist");
    let check_header_pos = s
        .find("capture-delivery-rate POST-recording check")
        .expect("#705: the post-recording capture-rate check step must exist");
    assert!(
        check_header_pos > window_end_pos,
        "#705: the post-recording check must run AFTER the window END epoch is captured"
    );

    let window_cmd_pos = s
        .find("capture_rate_window_journalctl_cmd \"")
        .expect("#705: the post-recording check must call capture_rate_window_journalctl_cmd");
    assert!(
        window_cmd_pos > check_header_pos,
        "#705: the journalctl-window call must be part of the post-recording check block"
    );

    let msg_call_pos = s
        .find("capture_rate_recurrence_message \"$CAMERA_NAME\"")
        .expect(
            "#705: the post-recording check must format its fail message via the shared \
             capture_rate_recurrence_message helper (never a second ad-hoc string built inline)",
        );
    assert!(
        msg_call_pos > check_header_pos,
        "#705: the recurrence-message call must be part of the post-recording check block"
    );

    // A matched recurrence must abort the run (exit 1) BEFORE the ~5-10 min decode step below
    // ever launches — scoped tightly to just this check's own block so an unrelated exit 1
    // elsewhere in the file can never make this assertion vacuously pass.
    let check_ok_pos = s
        .find("no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording")
        .expect("#705: the post-recording check must print a success line when clean");
    assert!(
        check_ok_pos > check_header_pos,
        "#705: the success echo must come after the check step header"
    );
    let this_check_block = &s[check_header_pos..check_ok_pos];
    assert!(
        this_check_block.contains("exit 1"),
        "#705: the post-recording capture-rate check must exit 1 on a matched recurrence line"
    );

    // Must run BEFORE the decode step launches (#193's VERDICT_ON_STREAM branch) — the whole
    // point is saving the ~5-10 min decode budget, not just diagnosing after the fact.
    let decode_pos = s
        .find("#193: by DEFAULT decode ON stream.lan")
        .expect("the #193 decode-on-stream branch comment must exist");
    assert!(
        check_ok_pos < decode_pos,
        "#705: the post-recording capture-rate check must complete BEFORE the decode step \
         launches, so a recurrence is caught before the expensive decode budget is spent"
    );
}

#[test]
fn recording_e2e_post_check_reresolves_invocation_id_after_start_record_not_the_stale_preflight_one(
) {
    let s = read("scripts/recording-e2e.sh");
    let start_record_pos = s
        .find("echo \"[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    let window_invocation_pos = s.find("CAPTURE_RATE_WINDOW_INVOCATION_ID=").expect(
        "#705: the post-recording check must re-resolve a FRESH InvocationID (the [0/8] \
         preflight's own $CAPTURE_RATE_INVOCATION_ID is stale by this point -- [2/8] deploys \
         and restarts camera-box in between, killing that process instance)",
    );
    assert!(
        window_invocation_pos > start_record_pos,
        "#705: the fresh invocation id must be resolved AFTER [5/8] StartRecord (so it reflects \
         the process instance that was actually running DURING the recording)"
    );

    let window_cmd_call = s
        .find("capture_rate_window_journalctl_cmd \"$CAPTURE_RATE_WINDOW_INVOCATION_ID\"")
        .expect(
            "#705: the post-recording check must pass the FRESH $CAPTURE_RATE_WINDOW_INVOCATION_ID \
             into capture_rate_window_journalctl_cmd, never the stale [0/8] $CAPTURE_RATE_INVOCATION_ID",
        );
    assert!(
        window_cmd_call > window_invocation_pos,
        "#705: the fresh invocation id must be resolved BEFORE it is used"
    );
}
