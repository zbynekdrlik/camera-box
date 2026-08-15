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

// -------------------------------------------------------------------------------------------
// (#992) the [7b/8] #705 post-recording check is JOURNALD-BLIND during a real E2E run: the
// harness stops camera-box.service and launches the source camera's capture as a transient
// systemd-run unit whose stdout/stderr are redirected DIRECTLY to /tmp/cbox-burn.log --
// journald never sees a line of it. Live false-negative (gate run 19150595): the check printed
// "ok" while cam1's burn instance captured at 63.5-64.0fps for the whole window. Fix: ALSO grep
// the burn instance's own log file.
//
// ROZHODNUTÉ (supervisor, gate rerun 31028767542 evidence, see issue 992 comment
// https://github.com/zbynekdrlik/camera-box/issues/992#issuecomment-5195254731): the detection
// itself works, but hard-failing on the #717 SUSTAINED band recreates the issue-909 mistake one
// layer up -- that band is INFORMATIONAL BY DESIGN (the genlock decimation gate absorbs the
// over-rate into exact NDI output), and cam1's ShadowCast 2 over-rate is CHRONIC, so a hard
// #717 fail here would abort every run before the verdict is ever computed. The pattern is
// therefore SPLIT: capture_rate_defect_grep_pattern_hard (#656/#971/#663 -- still exit 1) and
// capture_rate_sustained_band_grep_pattern (#717 only -- report-only WARN, never exit 1). The
// old union capture_rate_defect_grep_pattern_all is REMOVED (no caller needs the union once both
// call sites grep the two bands separately).
// -------------------------------------------------------------------------------------------

#[test]
fn defect_grep_pattern_hard_matches_hard_bands_only() {
    let pattern = run_sourced("capture_rate_defect_grep_pattern_hard")
        .trim()
        .to_string();

    let chronic_line = "#971 capture-delivery-rate CHRONIC sustained-band DEFECTIVE: 64.00 fps \
         captured vs 60.00 fps configured/negotiated (>2.0% deviation held for 180 consecutive \
         report windows, ~900s -- beyond the 900s chronic bar) -- USB-reset the capture device \
         (see #971, #909, #717)";
    let self_heal_line = "#663 self-heal: USB reset attempt #1 succeeded";
    let jitter_line = "#656 capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps \
         configured/negotiated (>1.0% deviation sustained for 6 consecutive report windows, \
         ~30s) -- USB-reset the capture device (see #656)";
    let sustained_line = "#717 capture-delivery-rate SUSTAINED band confirmed (informational at THIS tier -- a USB reset AUTO-ESCALATES \
         once this sustained deviation becomes chronic, see #971): 63.90 fps captured vs \
         60.00 fps configured/negotiated (>2.0% deviation sustained for 12 consecutive report \
         windows, ~60s) -- inside the ShadowCast 2 wide 9.0% jitter-tolerant envelope; the genlock \
         decimation gate absorbs this over-rate into exact NDI output by design, so NO USB \
         reset is triggered yet";
    let healthy_line =
        "Streaming: 59.9 fps emitted / 60.02 fps captured (1798 sent, 1801 captured, 0 \
         capture-dropped)";

    for (label, line, must_match) in [
        ("chronic", chronic_line, true),
        ("self-heal reset", self_heal_line, true),
        ("jitter (still matched, unchanged)", jitter_line, true),
        (
            "sustained (must NOT match the HARD pattern -- report-only per issue 992 ROZHODNUTÉ)",
            sustained_line,
            false,
        ),
        ("healthy", healthy_line, false),
    ] {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!("printf '%s\\n' '{line}' | grep -E -- '{pattern}'"))
            .output()
            .expect("failed to run grep");
        assert_eq!(
            out.status.success(),
            must_match,
            "capture_rate_defect_grep_pattern_hard: '{label}' line match expectation failed \
             (want match={must_match}). Pattern: {pattern}"
        );
    }
}

#[test]
fn capture_rate_defect_grep_pattern_hard_is_the_exact_three_band_alternation() {
    let out = run_sourced("capture_rate_defect_grep_pattern_hard");
    assert_eq!(
        out.trim(),
        "#656 capture-delivery-rate DEFECTIVE|#971 capture-delivery-rate CHRONIC sustained-band \
         DEFECTIVE|#663 self-heal: USB reset attempt",
        "issue 992 ROZHODNUTÉ: capture_rate_defect_grep_pattern_hard must be exactly the three \
         HARD bands -- #656 jitter, #971 chronic escalation, #663 self-heal reset. Never the \
         #717 sustained band (that one is report-only)."
    );
}

#[test]
fn sustained_band_grep_pattern_matches_only_717() {
    let pattern = run_sourced("capture_rate_sustained_band_grep_pattern")
        .trim()
        .to_string();

    let sustained_line = "#717 capture-delivery-rate SUSTAINED band confirmed (informational at THIS tier -- a USB reset AUTO-ESCALATES \
         once this sustained deviation becomes chronic, see #971): 63.90 fps captured vs \
         60.00 fps configured/negotiated (>2.0% deviation sustained for 12 consecutive report \
         windows, ~60s) -- inside the ShadowCast 2 wide 9.0% jitter-tolerant envelope; the genlock \
         decimation gate absorbs this over-rate into exact NDI output by design, so NO USB \
         reset is triggered yet";
    let chronic_line = "#971 capture-delivery-rate CHRONIC sustained-band DEFECTIVE: 64.00 fps \
         captured vs 60.00 fps configured/negotiated (>2.0% deviation held for 180 consecutive \
         report windows, ~900s -- beyond the 900s chronic bar) -- USB-reset the capture device \
         (see #971, #909, #717)";
    let self_heal_line = "#663 self-heal: USB reset attempt #1 succeeded";
    let jitter_line = "#656 capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps \
         configured/negotiated (>1.0% deviation sustained for 6 consecutive report windows, \
         ~30s) -- USB-reset the capture device (see #656)";
    let healthy_line =
        "Streaming: 59.9 fps emitted / 60.02 fps captured (1798 sent, 1801 captured, 0 \
         capture-dropped)";

    for (label, line, must_match) in [
        ("sustained", sustained_line, true),
        (
            "chronic (hard band, must NOT match the sustained-only pattern)",
            chronic_line,
            false,
        ),
        (
            "self-heal reset (hard band, must NOT match)",
            self_heal_line,
            false,
        ),
        ("jitter (hard band, must NOT match)", jitter_line, false),
        ("healthy", healthy_line, false),
    ] {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!("printf '%s\\n' '{line}' | grep -E -- '{pattern}'"))
            .output()
            .expect("failed to run grep");
        assert_eq!(
            out.status.success(),
            must_match,
            "capture_rate_sustained_band_grep_pattern: '{label}' line match expectation failed \
             (want match={must_match}). Pattern: {pattern}"
        );
    }
}

#[test]
fn capture_rate_sustained_band_grep_pattern_is_the_exact_717_signal() {
    let out = run_sourced("capture_rate_sustained_band_grep_pattern");
    assert_eq!(
        out.trim(),
        "#717 capture-delivery-rate SUSTAINED band confirmed",
        "issue 992 ROZHODNUTÉ: capture_rate_sustained_band_grep_pattern must be exactly the \
         #717 SUSTAINED-band signal, and nothing else."
    );
}

#[test]
fn capture_rate_defect_grep_pattern_all_was_removed_in_favor_of_the_hard_sustained_split() {
    let s = read("scripts/lib/capture-rate-guard.sh");
    assert!(
        !s.contains("capture_rate_defect_grep_pattern_all"),
        "issue 992 ROZHODNUTÉ: the union pattern is superseded by \
         capture_rate_defect_grep_pattern_hard + capture_rate_sustained_band_grep_pattern -- it \
         must be removed as dead code, not left alongside the split."
    );
}

#[test]
fn burn_log_grep_cmd_greps_the_given_path_with_the_given_pattern() {
    let out = run_sourced(
        "capture_rate_burn_log_grep_cmd '/tmp/cbox-burn.log' \
         \"$(capture_rate_defect_grep_pattern_hard)\"",
    );
    let cmd = out.trim();
    assert!(
        cmd.contains("/tmp/cbox-burn.log"),
        "#992: must grep the caller-supplied log path. Got: {cmd}"
    );
    assert!(
        cmd.contains("#656 capture-delivery-rate DEFECTIVE"),
        "#992: must embed whatever pattern the caller supplies (the HARD pattern, in this \
         call) -- never a pattern hardcoded inside the function. Got: {cmd}"
    );
    assert!(
        !cmd.contains("#717 capture-delivery-rate SUSTAINED band confirmed"),
        "issue 992 ROZHODNUTÉ: a call passing the HARD pattern must never also embed the \
         sustained-band literal. Got: {cmd}"
    );
    assert!(
        cmd.starts_with("grep -E"),
        "#992: must be a grep command (embeds via $(...) into a larger ssh command string). \
         Got: {cmd}"
    );
}

#[test]
fn burn_log_grep_cmd_accepts_the_sustained_pattern_too() {
    let out = run_sourced(
        "capture_rate_burn_log_grep_cmd '/tmp/cbox-burn.log' \
         \"$(capture_rate_sustained_band_grep_pattern)\"",
    );
    let cmd = out.trim();
    assert!(
        cmd.contains("#717 capture-delivery-rate SUSTAINED band confirmed"),
        "issue 992 ROZHODNUTÉ: must embed the sustained-band pattern when the caller passes it \
         -- the function is a generic PATTERN + LOG_PATH grep builder, not hardcoded to one \
         band. Got: {cmd}"
    );
}

#[test]
fn burn_log_grep_cmd_output_is_valid_remote_shell() {
    let out = run_sourced(
        "capture_rate_burn_log_grep_cmd '/tmp/cbox-burn.log' \
         \"$(capture_rate_defect_grep_pattern_hard)\"",
    );
    let cmd = out.trim();
    let check = Command::new("bash")
        .arg("-n")
        .arg("-c")
        .arg(format!(
            "ssh dummy \"{cmd}\"",
            cmd = cmd.replace('"', "\\\"")
        ))
        .output()
        .expect("failed to run bash -n");
    assert!(
        check.status.success(),
        "#992: capture_rate_burn_log_grep_cmd's output must be syntactically valid shell text.\n\
         stderr={:?}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn burn_log_recurrence_message_names_the_log_file_never_journal() {
    let out = run_sourced(
        "capture_rate_burn_log_recurrence_message cam1 'WARN #971 capture-delivery-rate \
         CHRONIC sustained-band DEFECTIVE: 64.00 fps captured vs 60.00 fps configured/negotiated'",
    );
    let msg = out.trim();
    assert!(
        msg.contains("cam1") && msg.contains("burn-instance log"),
        "#992: must name the camera and clearly identify the burn-instance LOG as the source. \
         Got: {msg}"
    );
    // The message MAY explain that journald was blind (that's useful context) but must never
    // misattribute the SOURCE of the match as "journal" -- distinct from
    // capture_rate_recurrence_message, whose surrounding call site (recording-e2e.sh) labels its
    // own success line "... in $CAMERA_NAME's journal ...".
    assert!(
        !msg.contains("in cam1's journal"),
        "#992: must never claim the match came FROM the journal -- this message is for a defect \
         found in the burn instance's OWN log file. Got: {msg}"
    );
}

#[test]
fn sustained_band_warn_message_is_loud_names_the_journal_and_never_exits() {
    let out = run_sourced(
        "capture_rate_sustained_band_warn_message cam1 'WARN #717 capture-delivery-rate \
         SUSTAINED band confirmed: 63.90 fps captured vs 60.00 fps configured/negotiated'",
    );
    let msg = out.trim();
    assert!(
        msg.starts_with("WARNING #992:"),
        "issue 992 ROZHODNUTÉ: the sustained-band message must be a loud WARNING, clearly \
         tagged #992 so a log reader can find the design decision. Got: {msg}"
    );
    assert!(
        msg.contains("cam1") && msg.contains("journal"),
        "must name the camera and identify the journal as the source. Got: {msg}"
    );
    assert!(
        msg.contains("63.90 fps captured"),
        "issue 992 ROZHODNUTÉ: must print the matched line verbatim so every run's log carries \
         the over-rate evidence. Got: {msg}"
    );
    assert!(
        msg.contains("909"),
        "issue 992 ROZHODNUTÉ: should point at issue 909 (why this band is informational by \
         design), so a reader lands on the rationale, not just the fact of the match. Got: {msg}"
    );
}

#[test]
fn burn_log_sustained_band_warn_message_is_loud_and_names_the_burn_log() {
    let out = run_sourced(
        "capture_rate_burn_log_sustained_band_warn_message cam1 'WARN #717 \
         capture-delivery-rate SUSTAINED band confirmed: 63.90 fps captured vs 60.00 fps \
         configured/negotiated'",
    );
    let msg = out.trim();
    assert!(
        msg.starts_with("WARNING #992:"),
        "issue 992 ROZHODNUTÉ: must be a loud WARNING, clearly tagged #992. Got: {msg}"
    );
    assert!(
        msg.contains("cam1") && msg.contains("burn-instance log"),
        "must name the camera and identify the burn-instance log as the source (never \
         \"journal\" -- same discriminator as capture_rate_burn_log_recurrence_message). \
         Got: {msg}"
    );
    assert!(
        !msg.contains("in cam1's journal"),
        "must never misattribute the source as the journal. Got: {msg}"
    );
}

#[test]
fn recording_e2e_post_check_also_scans_the_burn_instance_log_when_journald_is_clean() {
    let s = read("scripts/recording-e2e.sh");
    let check_header_pos = s
        .find("capture-delivery-rate POST-recording check")
        .expect("#705: the post-recording capture-rate check step must exist");
    let check_ok_pos = s
        .find("no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording")
        .expect("#705: the post-recording check must print a success line when clean");
    assert!(
        check_ok_pos > check_header_pos,
        "the success echo must come after the check step header"
    );
    let this_check_block = &s[check_header_pos..check_ok_pos];

    assert!(
        this_check_block.contains("capture_rate_burn_log_grep_cmd"),
        "#992: the post-recording check must ALSO grep the burn instance's own log file (the \
         harness stops camera-box.service and redirects the burn's stdout/stderr straight to a \
         file, never journald) -- journald alone false-passed a live over-rate recurrence \
         (gate run 19150595). Block:\n{this_check_block}"
    );
    assert!(
        this_check_block.contains("/tmp/cbox-burn.log"),
        "#992: must scan the SOURCE camera's own burn log path. Block:\n{this_check_block}"
    );
    assert!(
        this_check_block.contains("capture_rate_burn_log_recurrence_message"),
        "#992: a HARD burn-log match must be reported via the dedicated burn-log message \
         formatter (never silently reusing the journald-only message, and never a third ad-hoc \
         string). Block:\n{this_check_block}"
    );
    assert!(
        this_check_block.contains("exit 1"),
        "#992: a HARD defect found in the burn log must still abort the run (exit 1), same as \
         a journald-sourced match"
    );
}

#[test]
fn recording_e2e_post_check_uses_the_hard_pattern_at_both_call_sites_never_the_removed_all_pattern()
{
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains("capture_rate_defect_grep_pattern_all"),
        "issue 992 ROZHODNUTÉ: capture_rate_defect_grep_pattern_all was removed in favor of the \
         split hard/sustained patterns -- recording-e2e.sh must not reference it any more."
    );
    let check_header_pos = s
        .find("capture-delivery-rate POST-recording check")
        .expect("#705: the post-recording capture-rate check step must exist");
    let check_ok_pos = s
        .find("no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording")
        .expect("#705: the post-recording check must print a success line when clean");
    let this_check_block = &s[check_header_pos..check_ok_pos];
    let hard_calls = this_check_block
        .matches("capture_rate_defect_grep_pattern_hard")
        .count();
    assert_eq!(
        hard_calls, 2,
        "issue 992 ROZHODNUTÉ: the HARD pattern must be grepped at BOTH call sites (journald \
         window + burn log). Block:\n{this_check_block}"
    );
}

#[test]
fn recording_e2e_post_check_sustained_band_is_report_only_never_aborts() {
    let s = read("scripts/recording-e2e.sh");
    let check_header_pos = s
        .find("capture-delivery-rate POST-recording check")
        .expect("#705: the post-recording capture-rate check step must exist");
    let check_ok_pos = s
        .find("no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording")
        .expect("#705: the post-recording check must print a success line when clean");
    let this_check_block = &s[check_header_pos..check_ok_pos];

    let sustained_calls = this_check_block
        .matches("capture_rate_sustained_band_grep_pattern")
        .count();
    assert_eq!(
        sustained_calls, 2,
        "issue 992 ROZHODNUTÉ: the sustained-band pattern must be grepped at BOTH call sites \
         (journald window + burn log), separately from the hard pattern. \
         Block:\n{this_check_block}"
    );

    assert!(
        this_check_block.contains("capture_rate_sustained_band_warn_message"),
        "issue 992 ROZHODNUTÉ: a journald-side sustained-band match must be reported via the \
         dedicated WARN formatter. Block:\n{this_check_block}"
    );
    assert!(
        this_check_block.contains("capture_rate_burn_log_sustained_band_warn_message"),
        "issue 992 ROZHODNUTÉ: a burn-log-side sustained-band match must be reported via its \
         OWN dedicated WARN formatter (never reusing the journald one, never a third ad-hoc \
         string). Block:\n{this_check_block}"
    );

    // Scope tightly: from the journald-side sustained-band grep call through the start of the
    // burn-log section (the burn-log HARD call, which begins that section) -- prove that region
    // never exits, and does print the loud WARNING.
    let first_sustained_pos = this_check_block
        .find("capture_rate_sustained_band_grep_pattern")
        .expect("sustained-band grep call must exist");
    let burn_log_section_pos = this_check_block
        .find("capture_rate_burn_log_grep_cmd")
        .expect("burn-log grep call must exist");
    assert!(
        burn_log_section_pos > first_sustained_pos,
        "the journald-side sustained-band check must come before the burn-log section begins"
    );
    let journald_sustained_region = &this_check_block[first_sustained_pos..burn_log_section_pos];
    assert!(
        !journald_sustained_region.contains("exit 1"),
        "issue 992 ROZHODNUTÉ: a #717 SUSTAINED-band match must NEVER exit 1 -- it is \
         informational by design (issue 909: absorbed by the genlock decimation gate). \
         Region:\n{journald_sustained_region}"
    );
    // The "WARNING #992:" text itself lives inside capture_rate_sustained_band_warn_message's
    // OWN echo (scripts/lib/capture-rate-guard.sh), not in recording-e2e.sh's source text -- so
    // the caller-side region check is for the CALL to that formatter, not the literal string
    // (that literal-text contract is pinned separately, on the formatter itself, by
    // sustained_band_warn_message_is_loud_names_the_journal_and_never_exits above).
    assert!(
        journald_sustained_region.contains("capture_rate_sustained_band_warn_message"),
        "issue 992 ROZHODNUTÉ: a journald-side sustained-band match must be reported via the \
         dedicated WARN formatter, in this same tightly-scoped region (not just somewhere in \
         the wider check block). Region:\n{journald_sustained_region}"
    );
}

// -------------------------------------------------------------------------------------------
// (#994) Extend the [7b/8] capture-delivery-rate POST-recording check to the SECONDARY cameras.
//
// The source-camera check (above) only ever read $CAMERA_NAME / $CAM1_IP / /tmp/cbox-burn.log.
// Under ALL_CAMBOX=1 every active secondary camera runs its OWN capture burn ([2b/8], logging to
// /tmp/cbox-burn-<camname>.log) and is cut into strih program, so a capture-rate defect on a
// secondary during the recording (issue 889: cam1 AND cam2 over-rate at once -- cam2 is a
// secondary) was invisible. Option 2 of the ticket -- the reset-EVENT sweep across secondaries --
// is already done by the #910 unified restart-event scan; this closes option 1 for the capture-
// rate defect-DECLARATION signal.
//
// SEVERITY: REPORT-ONLY for a secondary (a loud "WARNING #994:", never exit 1). Hard-failing on a
// secondary would recreate the issue-909/914 permanently-red-gate mistake (frozen_leg/
// self_heal_reset are report-only precisely so a chronic secondary quirk doesn't abort every run),
// and conflicts with the owner's "green gate first, tighten via tickets" directive. The source-
// camera check keeps its HARD (exit 1) behavior unchanged.
// -------------------------------------------------------------------------------------------

#[test]
fn secondary_recurrence_warn_message_is_loud_report_only_and_names_the_journal() {
    let out = run_sourced(
        "capture_rate_secondary_recurrence_warn_message cam4 'WARN #971 capture-delivery-rate \
         CHRONIC sustained-band DEFECTIVE: 64.00 fps captured vs 60.00 fps configured/negotiated'",
    );
    let msg = out.trim();
    assert!(
        msg.starts_with("WARNING #994:"),
        "#994: a secondary-camera capture-rate match must be a loud report-only WARNING tagged \
         #994 (never an ERROR/exit-1 line). Got: {msg}"
    );
    assert!(
        msg.contains("cam4") && msg.contains("64.00") && msg.contains("60.00"),
        "#994: must name the camera and extract the captured/configured fps. Got: {msg}"
    );
    assert!(
        msg.contains("journal"),
        "#994: the journald-sourced formatter must identify the journal as the source. Got: {msg}"
    );
    assert!(
        msg.contains("#994"),
        "#994: the message must point at this ticket for context. Got: {msg}"
    );
}

#[test]
fn secondary_recurrence_warn_message_falls_back_gracefully_on_an_unparseable_line() {
    let out = run_sourced(
        "capture_rate_secondary_recurrence_warn_message cam5 'a #656 capture-delivery-rate \
         DEFECTIVE line in an unexpected shape'",
    );
    let msg = out.trim();
    assert!(
        msg.starts_with("WARNING #994:") && msg.contains("cam5"),
        "#994: fallback must still be a loud WARNING naming the camera. Got: {msg}"
    );
    assert!(
        msg.contains("a #656 capture-delivery-rate DEFECTIVE line in an unexpected shape"),
        "#994: fallback must echo the raw matched line, never silently swallow it. Got: {msg}"
    );
}

#[test]
fn secondary_burn_log_recurrence_warn_message_is_report_only_and_names_the_burn_log() {
    let out = run_sourced(
        "capture_rate_secondary_burn_log_recurrence_warn_message cam4 'WARN #656 \
         capture-delivery-rate DEFECTIVE: 63.20 fps captured vs 60.00 fps configured/negotiated'",
    );
    let msg = out.trim();
    assert!(
        msg.starts_with("WARNING #994:"),
        "#994: must be a loud report-only WARNING tagged #994. Got: {msg}"
    );
    assert!(
        msg.contains("cam4") && msg.contains("burn-instance log"),
        "#994: must name the camera and identify the burn-instance LOG as the source. Got: {msg}"
    );
    assert!(
        !msg.contains("in cam4's journal"),
        "#994: must never misattribute the source as the journal (same journal/burn-log \
         discriminator as the #992 formatters). Got: {msg}"
    );
}

#[test]
fn recording_e2e_sweeps_every_secondary_camera_for_capture_rate_after_the_source_check() {
    let s = read("scripts/recording-e2e.sh");

    // The source-camera check block must be untouched: its success line still exists, and the new
    // secondary sweep must come STRICTLY AFTER it (so the new grep calls never land inside the
    // region the source block's hard==2 / sustained==2 anchor counts measure).
    let source_ok_pos = s
        .find("no capture-rate defect recurrence in $CAMERA_NAME's journal during the recording")
        .expect("#705/#992: the source-camera post-recording check success line must still exist");
    let sweep_pos = s
        .find("secondary-camera capture-delivery-rate POST-recording sweep (#994)")
        .expect("#994: the secondary-camera capture-rate sweep step must exist");
    assert!(
        sweep_pos > source_ok_pos,
        "#994: the secondary sweep must come AFTER the source-camera check block, so it never \
         disturbs that block's own hard/sustained ==2 anchor counts"
    );

    let sweep_end_pos = s
        .find("secondary-camera capture-rate sweep complete (#994")
        .expect("#994: the secondary sweep must print a completion line");
    let sweep_block = &s[sweep_pos..sweep_end_pos];

    // Gated on ALL_CAMBOX=1 (the plain single-camera path has no secondary cameras).
    assert!(
        sweep_block.contains("if [ \"${ALL_CAMBOX:-0}\" = \"1\" ]"),
        "#994: the secondary sweep loop must be gated behind ALL_CAMBOX=1. Block:\n{sweep_block}"
    );

    // Loops over every secondary (cam2 + camera_active_secondary_set()) via the SAME deploy list
    // #910's restart-event scan already sweeps -- never a literal cam-number range.
    assert!(
        sweep_block.contains("CAMBOX_SECONDARY_DEPLOY"),
        "#994: must sweep every active secondary via CAMBOX_SECONDARY_DEPLOY (never a literal \
         cam range). Block:\n{sweep_block}"
    );
    // Reads each secondary box's OWN burn log (the journald-blind sibling, issue 992/910).
    assert!(
        sweep_block.contains("/tmp/cbox-burn-${_cn}.log"),
        "#994: must read each secondary's own burn-instance log path. Block:\n{sweep_block}"
    );
    // Both sources (journald window + burn log) and both bands (HARD + SUSTAINED), mirroring the
    // source-camera check.
    assert!(
        sweep_block.contains("capture_rate_window_journalctl_cmd"),
        "#994: must read the secondary's journald window. Block:\n{sweep_block}"
    );
    assert!(
        sweep_block.contains("capture_rate_burn_log_grep_cmd"),
        "#994: must read the secondary's burn-instance log. Block:\n{sweep_block}"
    );
    assert!(
        sweep_block.contains("capture_rate_defect_grep_pattern_hard"),
        "#994: must grep the HARD defect band on secondaries. Block:\n{sweep_block}"
    );
    assert!(
        sweep_block.contains("capture_rate_sustained_band_grep_pattern"),
        "#994: must grep the SUSTAINED band on secondaries too. Block:\n{sweep_block}"
    );
    // Report-only via the dedicated #994 formatters (never the source's exit-1 ERROR formatters).
    assert!(
        sweep_block.contains("capture_rate_secondary_recurrence_warn_message"),
        "#994: a journald-side HARD match on a secondary must be reported via the dedicated \
         report-only #994 formatter. Block:\n{sweep_block}"
    );
    assert!(
        sweep_block.contains("capture_rate_secondary_burn_log_recurrence_warn_message"),
        "#994: a burn-log-side HARD match on a secondary must be reported via its own dedicated \
         report-only #994 formatter. Block:\n{sweep_block}"
    );
    // The whole point: a secondary defect is REPORT-ONLY -- it must NEVER abort the run.
    assert!(
        !sweep_block.contains("exit 1"),
        "#994: a secondary-camera capture-rate match must NEVER exit 1 (report-only -- \
         hard-failing recreates the issue-909/914 permanently-red-gate mistake; cam2 is a \
         secondary). Block:\n{sweep_block}"
    );
}
