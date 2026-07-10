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
