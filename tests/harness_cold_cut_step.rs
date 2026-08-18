//! #1086 — the deliberate keepalive-bypass COLD CUT step for the all-cambox sweep.
//!
//! Under the issue-767 keep-alive DistroAV build every strih NDI receiver keeps decoding
//! off-program, so a natural sweep cold cut is always WARM and the issue-768 report-only onset seam
//! can never redden on a 767 regression. `scripts/lib/cold-cut-step.sh` temporarily bypasses
//! keep-alive for ONE camera (idle its receiver after its first appearance -> genuinely cold for the
//! hidden window -> restore right before its next cut), so the seam measures a real cold-cut onset.
//!
//! These tests pin (1) the static wiring in `scripts/recording-e2e.sh` (the source line + the three
//! gated call sites in the sweep loop, in the right order) and (2) the lib's state machine
//! FUNCTIONALLY, by sourcing it and driving a fake sweep with a stub `obs_phase2.py` — asserting the
//! idle -> restore sequence, that it is a pure no-op when disabled, and that it fails loud when the
//! target input is missing. No rig, no OBS.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn read(rel: &str) -> String {
    fs::read_to_string(rel).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

// ---------------------------------------------------------------------------
// Static wiring in recording-e2e.sh
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_cold_cut_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/cold-cut-step.sh\""),
        "#1086: recording-e2e.sh must source scripts/lib/cold-cut-step.sh"
    );
}

#[test]
fn sweep_arms_then_restores_before_and_idles_after_each_switch() {
    let s = read("scripts/recording-e2e.sh");
    // The state machine is armed once, before the sweep loop.
    let reset = s
        .find("cold_cut_reset_state")
        .expect("#1086: the sweep must arm the cold-cut step via cold_cut_reset_state");
    // The restore hook runs BEFORE the switch (so the cut lands on a cold-restored receiver).
    let before = s
        .find("cold_cut_before_segment")
        .expect("#1086: the sweep must call cold_cut_before_segment before each switch");
    let switch = s
        .find("obs_phase2.py\" switch --host \"$STRIH\"")
        .expect("#1086: the sweep's switch call must still exist");
    // The idle hook runs AFTER the switch (once the target is off-program).
    let after = s
        .find("cold_cut_after_segment")
        .expect("#1086: the sweep must call cold_cut_after_segment after each switch");
    assert!(
        reset < before && before < switch && switch < after,
        "#1086: expected order arm < restore-before-cut < switch < idle-after-cut, got \
         reset={reset} before={before} switch={switch} after={after}"
    );
}

#[test]
fn cold_cut_call_sites_forward_host_and_obs_phase2_path() {
    let s = read("scripts/recording-e2e.sh");
    // Both hooks are handed the strih host + the real obs_phase2.py path so the idle/restore land
    // on the strih box (the only box the sweep cuts program on).
    for call in ["cold_cut_before_segment", "cold_cut_after_segment"] {
        let idx = s.find(call).expect("call site present");
        let line_end = s[idx..].find('\n').map(|i| idx + i).unwrap_or(s.len());
        let line = &s[idx..line_end];
        assert!(
            line.contains("\"$STRIH\"") && line.contains("$HERE/obs_phase2.py"),
            "#1086: {call} must forward $STRIH + $HERE/obs_phase2.py — got: {line}"
        );
    }
}

// ---------------------------------------------------------------------------
// Functional: drive the lib's state machine with a stub obs_phase2.py
// ---------------------------------------------------------------------------

/// Convenience wrapper: drive a sweep with a healthy stub (captures "CAM1 (usb)") and no trailing
/// cleanup. Returns (stdout, the stub's call log, success).
fn run_sweep(env: &[(&str, &str)], sweep: &[&str]) -> (String, String, bool) {
    run_scenario(env, sweep, false, "CAM1 (usb)")
}

/// Run a bash script that sources the lib and drives a fake sweep. `env` sets COLD_CUT_* for the
/// run; `sweep` is the label order. `call_cleanup` appends a `cold_cut_cleanup_restore` call (the
/// EXIT-trap path). `prev_ndi` is what the stub prints as the captured `PREV_NDI_NAME` on idle
/// (empty ⇒ a source-less / failed-read input).
fn run_scenario(
    env: &[(&str, &str)],
    sweep: &[&str],
    call_cleanup: bool,
    prev_ndi: &str,
) -> (String, String, bool) {
    // A kernel-atomic UNIQUE dir per invocation (#975: never hand-roll a pid+timestamp temp path —
    // the tests run in parallel in one process and would race). tempfile auto-removes on Drop.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let stub = dir.join("obs_stub.py");
    let calls = dir.join("calls.log");
    let state = dir.join("state");
    // The stub prints `PREV_NDI_NAME=<prev_ndi>` on idle (an empty value ⇒ just the bare prefix).
    fs::write(
        &stub,
        format!(
            "import sys\n\
             a=sys.argv[1:]\n\
             open(r'{calls}','a').write(' '.join(a)+'\\n')\n\
             if 'idle-receiver' in a and '--restore' not in a:\n    print('PREV_NDI_NAME={prev}')\n\
             print('stub ok')\n",
            calls = calls.display(),
            prev = prev_ndi
        ),
    )
    .unwrap();
    let lib = PathBuf::from("scripts/lib/cold-cut-step.sh")
        .canonicalize()
        .unwrap();
    // Simulate the sweep: for each label, before_segment -> [switch] -> after_segment.
    let mut body = String::new();
    body.push_str("set -euo pipefail\n");
    body.push_str("interruptible_sleep() { :; }\n"); // instant top-up in the test
    body.push_str(&format!(". \"{}\"\n", lib.display()));
    body.push_str("cold_cut_reset_state\n");
    for label in sweep {
        body.push_str(&format!(
            "cold_cut_before_segment \"{l}\" host \"\" \"{s}\"\n\
             echo \"[SWITCH {l}]\"\n\
             cold_cut_after_segment \"{l}\" host \"\" \"{s}\"\n",
            l = label,
            s = stub.display()
        ));
    }
    if call_cleanup {
        body.push_str(&format!(
            "cold_cut_cleanup_restore host \"\" \"{}\"\n",
            stub.display()
        ));
    }
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&body);
    cmd.env("COLD_CUT_STATE_FILE", &state);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash");
    // Combine stdout + stderr — the lib writes WARNING lines to stderr, and callers assert on them.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let call_log = fs::read_to_string(&calls).unwrap_or_default();
    (combined, call_log, out.status.success())
}

#[test]
fn enabled_bypass_idles_after_first_appearance_and_restores_before_second_cut() {
    // Target CAM1, 3-box sweep, CAM1 reappears at segment 4.
    let (stdout, calls, ok) = run_sweep(
        &[
            ("COLD_CUT_BYPASS_CAM", "CAM1"),
            ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
            ("COLD_CUT_HOLD_SECS", "1"),
        ],
        &["CAM1", "CAM2", "CAM3", "CAM1"],
    );
    assert!(
        ok,
        "the sweep must complete under set -e; stdout:\n{stdout}"
    );
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "#1086: exactly ONE idle + ONE restore for a single genuine cold cut; got:\n{calls}"
    );
    assert!(
        lines[0].contains("idle-receiver")
            && lines[0].contains("--input NDI cam1")
            && !lines[0].contains("--restore"),
        "#1086: the FIRST obs call must idle the target receiver; got: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("idle-receiver") && lines[1].contains("--restore CAM1 (usb)"),
        "#1086: the SECOND obs call must RESTORE the captured prev ndi name; got: {}",
        lines[1]
    );
}

#[test]
fn disabled_bypass_is_a_pure_no_op() {
    // No COLD_CUT_BYPASS_CAM -> the sweep must never touch obs_phase2.py.
    let (stdout, calls, ok) = run_sweep(&[], &["CAM1", "CAM2", "CAM3", "CAM1"]);
    assert!(ok, "the sweep must complete; stdout:\n{stdout}");
    assert!(
        calls.trim().is_empty(),
        "#1086: with the bypass OFF (default), NO obs idle/restore call must fire; got:\n{calls}"
    );
}

#[test]
fn active_bypass_without_input_fails_loud() {
    // COLD_CUT_BYPASS_CAM set but COLD_CUT_BYPASS_INPUT missing -> reset_state must fail (never
    // guess which live receiver to idle), so the sweep aborts under set -e.
    let (_stdout, calls, ok) = run_sweep(
        &[("COLD_CUT_BYPASS_CAM", "CAM1")],
        &["CAM1", "CAM2", "CAM3", "CAM1"],
    );
    assert!(
        !ok,
        "#1086: an active bypass with no COLD_CUT_BYPASS_INPUT must fail loud, not silently no-op"
    );
    assert!(
        calls.trim().is_empty(),
        "#1086: it must fail BEFORE any obs call; got:\n{calls}"
    );
}

#[test]
fn recording_e2e_cleanup_calls_cold_cut_cleanup_restore() {
    // The EXIT-trap cleanup() must call the best-effort final restore so an interrupted / single-
    // appearance run never leaves a strih receiver idled black.
    let s = read("scripts/recording-e2e.sh");
    let cleanup = s
        .find("cleanup() {")
        .map(|i| &s[i..])
        .expect("#1086: recording-e2e.sh must define cleanup()");
    assert!(
        cleanup.contains("cold_cut_cleanup_restore \"$STRIH\""),
        "#1086: cleanup() must call cold_cut_cleanup_restore with the strih host"
    );
}

#[test]
fn cleanup_restores_a_single_appearance_idled_receiver() {
    // The target is cut to program only ONCE, so before_segment's restore never fires — the
    // receiver is left idled. cold_cut_cleanup_restore (the EXIT-trap path) must re-point it.
    let (stdout, calls, ok) = run_scenario(
        &[
            ("COLD_CUT_BYPASS_CAM", "CAM1"),
            ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
            ("COLD_CUT_HOLD_SECS", "1"),
        ],
        &["CAM1", "CAM2", "CAM3"], // CAM1 appears once
        true,                      // then cleanup fires
        "CAM1 (usb)",
    );
    assert!(ok, "the run must complete under set -e; stdout:\n{stdout}");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "#1086: one idle (during the sweep) + one restore (in cleanup); got:\n{calls}"
    );
    assert!(
        lines[0].contains("idle-receiver") && !lines[0].contains("--restore"),
        "#1086: first the idle; got: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("--restore CAM1 (usb)"),
        "#1086: cleanup must restore the captured prev name; got: {}",
        lines[1]
    );
}

#[test]
fn empty_prev_ndi_never_restores_to_black() {
    // If the idle-time capture came back EMPTY, restoring with `--restore ""` would re-idle the
    // input black. The restore must be SKIPPED (with a loud warning), never issued.
    let (stdout, calls, ok) = run_scenario(
        &[
            ("COLD_CUT_BYPASS_CAM", "CAM1"),
            ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
            ("COLD_CUT_HOLD_SECS", "1"),
        ],
        &["CAM1", "CAM2", "CAM3", "CAM1"], // CAM1 reappears -> before_segment would try to restore
        true,
        "", // stub captures an EMPTY prev ndi name
    );
    assert!(ok, "the run must complete under set -e; stdout:\n{stdout}");
    // Exactly ONE obs call (the idle); NO restore call is ever issued.
    let restore_calls = calls.lines().filter(|l| l.contains("--restore")).count();
    assert_eq!(
        restore_calls, 0,
        "#1086: an empty captured prev name must NEVER produce a restore call (that would re-idle \
         the input black); got:\n{calls}"
    );
    assert!(
        stdout.contains("WARNING #1086"),
        "#1086: an empty captured prev name must warn loudly; stdout:\n{stdout}"
    );
}
