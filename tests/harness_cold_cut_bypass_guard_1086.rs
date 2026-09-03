//! #1086 — the ARM-TIME guard + LOUD banner for the deliberate keepalive-bypass cold cut, and the
//! full-path-e2e.yml wiring that sources COLD_CUT_BYPASS_CAM / COLD_CUT_BYPASS_INPUT from
//! repository VARIABLES.
//!
//! `scripts/lib/cold-cut-bypass-guard.sh` is a pure sourced lib (sibling of the runtime
//! `scripts/lib/cold-cut-step.sh`). Because a repository variable is GLOBAL — it applies to EVERY
//! dev→main PR gate run until cleared — a stuck/typo'd value would silently idle a LIVE strih
//! receiver run after run; the guard makes an armed bypass LOUD and fail-CLOSED before ~30 min of
//! rig time is spent.
//!
//! These tests pin (1) the lib's guard state machine FUNCTIONALLY, by sourcing it and calling
//! `cold_cut_bypass_arm_check` under each state — silent no-op when unset, loud ARMED banner when
//! armed, fail-closed on an out-of-set target or a set-CAM/empty-INPUT, and an INERT-warning when
//! only INPUT is set; and (2) the static wiring in `.github/workflows/full-path-e2e.yml` (both env
//! vars on the recording step, plus a preceding arm-check step that sources the lib and calls the
//! orchestrator BEFORE the recording step). No rig, no OBS.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn read(rel: &str) -> String {
    fs::read_to_string(rel).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn read_workflow() -> String {
    read(&format!(
        "{}/.github/workflows/full-path-e2e.yml",
        env!("CARGO_MANIFEST_DIR")
    ))
}

// ---------------------------------------------------------------------------
// Functional: source the guard lib and drive cold_cut_bypass_arm_check.
// ---------------------------------------------------------------------------

/// Source scripts/lib/cold-cut-bypass-guard.sh under the caller's real `set -euo pipefail`, call
/// `cold_cut_bypass_arm_check`, and return (combined stdout+stderr, success). The lib must NEVER
/// abort the sourcing shell on a safe state (both empty / INERT) — it is called as a bare CI step.
fn arm_check(env: &[(&str, &str)]) -> (String, bool) {
    let lib = PathBuf::from("scripts/lib/cold-cut-bypass-guard.sh")
        .canonicalize()
        .unwrap();
    // The real full-path-e2e.yml arm-check step runs the guard under the step's own bash
    // `set -euo pipefail`; reproduce that exact context so a benign no-op that leaked a non-zero
    // return would be caught (the #1133 report-only-probe-aborts-under-set-e class).
    let body = format!(
        "set -euo pipefail\n. \"{}\"\ncold_cut_bypass_arm_check\n",
        lib.display()
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&body);
    // Start from a clean slate so the host's own environment can't leak either variable in.
    cmd.env_remove("COLD_CUT_BYPASS_CAM");
    cmd.env_remove("COLD_CUT_BYPASS_INPUT");
    cmd.env_remove("COLD_CUT_BYPASS_VALID_TARGETS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (combined, out.status.success())
}

#[test]
fn both_unset_is_a_silent_no_op() {
    let (out, ok) = arm_check(&[]);
    assert!(
        ok,
        "#1086: with both variables unset the guard must exit 0; out:\n{out}"
    );
    assert!(
        out.trim().is_empty(),
        "#1086: with both variables unset the guard must print NOTHING (a normal gate run must be \
         byte-for-byte unaffected); got:\n{out}"
    );
}

#[test]
fn valid_target_and_input_on_all_cambox_run_prints_armed_banner_and_succeeds() {
    // ALL_CAMBOX=1: the cold-cut hooks only fire inside recording-e2e.sh's fused sweep, so an ARMED
    // banner (rather than an INERT warning) is only honest when that sweep will actually run.
    let (out, ok) = arm_check(&[
        ("COLD_CUT_BYPASS_CAM", "CAM1"),
        ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
        ("ALL_CAMBOX", "1"),
    ]);
    assert!(ok, "#1086: a valid armed config must exit 0; out:\n{out}");
    assert!(
        out.contains("ARMED") && out.contains("CAM1") && out.contains("NDI cam1"),
        "#1086: a valid armed config on an ALL_CAMBOX run must print a LOUD banner naming BOTH \
         values; got:\n{out}"
    );
}

#[test]
fn armed_but_not_all_cambox_run_is_inert_and_warns() {
    // CAM+INPUT set but ALL_CAMBOX != 1 (a workflow_dispatch single-camera soak): the cold-cut
    // hooks never fire, so the bypass is INERT. The guard must WARN LOUDLY (naming both values) —
    // not print the ARMED banner and not fail-closed — so an operator is not fooled into thinking a
    // genuine cold cut happened.
    let (out, ok) = arm_check(&[
        ("COLD_CUT_BYPASS_CAM", "CAM1"),
        ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
        ("ALL_CAMBOX", "0"),
    ]);
    assert!(
        ok,
        "#1086: an armed config on a NON-ALL_CAMBOX run is INERT (safe) — must exit 0; out:\n{out}"
    );
    assert!(
        out.contains("::warning::")
            && out.contains("INERT")
            && out.contains("CAM1")
            && out.contains("NDI cam1"),
        "#1086: an armed-but-inert (non-ALL_CAMBOX) run must warn LOUDLY naming both values; got:\n{out}"
    );
    assert!(
        !out.contains(">>> #1086 cold-cut keepalive-bypass ARMED"),
        "#1086: a non-ALL_CAMBOX run must NOT print the ARMED banner (nothing is armed); got:\n{out}"
    );
}

#[test]
fn out_of_set_target_on_all_cambox_run_is_rejected_fail_closed() {
    // CAM7 is a real camera but gets NO 2nd program cut in the current sweep, so it can never yield
    // a genuine cold-cut onset — on the ALL_CAMBOX sweep run the guard must REJECT it (fail-closed),
    // not run a meaningless sweep.
    let (out, ok) = arm_check(&[
        ("COLD_CUT_BYPASS_CAM", "CAM7"),
        ("COLD_CUT_BYPASS_INPUT", "NDI cam7"),
        ("ALL_CAMBOX", "1"),
    ]);
    assert!(
        !ok,
        "#1086: an out-of-set COLD_CUT_BYPASS_CAM must FAIL the arm check (fail-closed); out:\n{out}"
    );
    assert!(
        out.contains("::error::")
            && out.contains("CAM7")
            && out.contains("not a valid bypass target"),
        "#1086: the rejection must be a loud ::error:: naming the bad target; got:\n{out}"
    );
    // It must STILL have printed the ARMED banner first (so the operator sees it WAS armed).
    assert!(
        out.contains("ARMED"),
        "#1086: even a rejected armed config must print the ARMED banner first; got:\n{out}"
    );
}

#[test]
fn set_cam_but_empty_input_on_all_cambox_run_is_rejected_fail_closed() {
    // Mirrors cold_cut_reset_state's own refusal — but caught at arm time, before ~30 min of rig.
    let (out, ok) = arm_check(&[("COLD_CUT_BYPASS_CAM", "CAM1"), ("ALL_CAMBOX", "1")]);
    assert!(
        !ok,
        "#1086: a set CAM with an empty INPUT must FAIL the arm check (never guess the receiver); \
         out:\n{out}"
    );
    assert!(
        out.contains("::error::") && out.contains("COLD_CUT_BYPASS_INPUT"),
        "#1086: the empty-INPUT rejection must be a loud ::error:: naming COLD_CUT_BYPASS_INPUT; \
         got:\n{out}"
    );
}

#[test]
fn input_only_is_inert_and_warns_but_does_not_fail() {
    // INPUT set, CAM empty: cold-cut-step.sh keys arming on CAM, so the bypass is INERT (idles
    // nothing) — a loud warning, never a hard failure (an inert bypass is safe). Independent of
    // ALL_CAMBOX (CAM empty is INERT on every run).
    let (out, ok) = arm_check(&[("COLD_CUT_BYPASS_INPUT", "NDI cam1"), ("ALL_CAMBOX", "1")]);
    assert!(
        ok,
        "#1086: INPUT-only (CAM empty) is INERT and safe — the guard must exit 0, not fail; out:\n{out}"
    );
    assert!(
        out.contains("::warning::") && out.contains("INERT"),
        "#1086: INPUT-only must warn LOUDLY that the bypass is INERT (not armed); got:\n{out}"
    );
}

#[test]
fn valid_target_match_is_whole_token_not_substring_or_case() {
    // A refactor to a substring/case-insensitive match (grep -q / case *"$want"*) would silently
    // widen the guard — pin that CAM10 / cam1 / a multi-token value are all REJECTED on the sweep.
    for bad in ["CAM10", "cam1", "CAM1 CAM2", "CAM"] {
        let (out, ok) = arm_check(&[
            ("COLD_CUT_BYPASS_CAM", bad),
            ("COLD_CUT_BYPASS_INPUT", "NDI cam1"),
            ("ALL_CAMBOX", "1"),
        ]);
        assert!(
            !ok && out.contains("not a valid bypass target"),
            "#1086: {bad:?} must be REJECTED as a whole-token mismatch (never a substring/case \
             match); out:\n{out}"
        );
    }
}

#[test]
fn valid_targets_override_widens_the_accepted_set() {
    // COLD_CUT_BYPASS_VALID_TARGETS is the ONE env-overridable source of truth (a future sweep that
    // gives more boxes a 2nd cut is a one-line widen, no code hunt) — pin that it actually takes.
    let (out, ok) = arm_check(&[
        ("COLD_CUT_BYPASS_CAM", "CAM4"),
        ("COLD_CUT_BYPASS_INPUT", "NDI cam4"),
        ("ALL_CAMBOX", "1"),
        ("COLD_CUT_BYPASS_VALID_TARGETS", "CAM1 CAM2 CAM3 CAM4"),
    ]);
    assert!(
        ok && out.contains("is valid"),
        "#1086: a target added via COLD_CUT_BYPASS_VALID_TARGETS must be accepted; out:\n{out}"
    );
}

#[test]
fn valid_targets_default_is_the_current_sweep_second_cut_set() {
    // The ONE source of truth for the valid set — the current-sweep 2nd-cut cameras.
    let lib = PathBuf::from("scripts/lib/cold-cut-bypass-guard.sh")
        .canonicalize()
        .unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -euo pipefail\n. \"{}\"\ncold_cut_bypass_valid_targets\n",
            lib.display()
        ))
        .env_remove("COLD_CUT_BYPASS_VALID_TARGETS")
        .output()
        .expect("run bash");
    let got = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        got.trim(),
        "CAM1 CAM2 CAM3",
        "#1086: the default valid bypass-target set must be exactly the current-sweep 2nd-cut \
         cameras CAM1 CAM2 CAM3; got: {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Static wiring in .github/workflows/full-path-e2e.yml
// ---------------------------------------------------------------------------

/// Both env vars must live in the recording step's OWN env: block (between its `name:` and its
/// `run:` line) so recording-e2e.sh actually sees them — sourced from repository variables so a
/// future genuine-cold run is a variable flip with no code change.
#[test]
fn recording_step_sources_cold_cut_bypass_vars_from_repository_variables() {
    let s = read_workflow();
    let step_pos = s
        .find("name: Recording-based 4-node cam2")
        .expect("the recording step must exist");
    let run_pos = s[step_pos..]
        .find("run: exec bash scripts/recording-e2e.sh")
        .map(|p| p + step_pos)
        .expect("the recording step must invoke recording-e2e.sh");
    let step_block = &s[step_pos..run_pos];
    assert!(
        step_block.contains("COLD_CUT_BYPASS_CAM: ${{ vars.COLD_CUT_BYPASS_CAM }}"),
        "#1086: the recording step's env: block must source COLD_CUT_BYPASS_CAM from the repository \
         variable vars.COLD_CUT_BYPASS_CAM. step_block:\n{step_block}"
    );
    assert!(
        step_block.contains("COLD_CUT_BYPASS_INPUT: ${{ vars.COLD_CUT_BYPASS_INPUT }}"),
        "#1086: the recording step's env: block must source COLD_CUT_BYPASS_INPUT from the \
         repository variable vars.COLD_CUT_BYPASS_INPUT. step_block:\n{step_block}"
    );
}

/// A preceding arm-check step must exist BEFORE the recording step, source the guard lib, call the
/// orchestrator, and carry both repository variables in its OWN env: block — so a stuck/typo'd
/// variable fails LOUDLY and fail-CLOSED before the ~30-min recording begins.
#[test]
fn arm_check_step_runs_before_recording_and_calls_the_guard() {
    let s = read_workflow();
    let arm_pos = s
        .find("name: Cold-cut keepalive-bypass arm check")
        .expect("#1086: a 'Cold-cut keepalive-bypass arm check' step must exist");
    let recording_pos = s
        .find("name: Recording-based 4-node cam2")
        .expect("the recording step must exist");
    let busy_gate_pos = s
        .find("run: bash scripts/rig-busy-gate.sh")
        .expect("the rig-busy gate step must exist");
    // The guard is PURE (no rig, no lease, no network), so it must fail-closed BEFORE the ~30-min
    // rig-busy poll / lease acquire, not just before the recording — catching a stuck variable
    // without wasting rig time at all.
    assert!(
        arm_pos < busy_gate_pos && busy_gate_pos < recording_pos,
        "#1086: the arm-check step must run BEFORE the rig-busy gate (fail-closed before ANY rig \
         time) — arm_pos={arm_pos}, busy_gate_pos={busy_gate_pos}, recording_pos={recording_pos}"
    );
    // Slice the arm-check step's block (from its name to the next step's `- name:`).
    let after = &s[arm_pos..];
    let step_end = after
        .find("\n      - name:")
        .map(|p| p + arm_pos)
        .unwrap_or(recording_pos);
    let step_block = &s[arm_pos..step_end];
    // Anchor on the SOURCE form `. scripts/lib/...` (call-site-unique), never the bare script name
    // — the step's own comment also mentions the path, which a bare-name .contains() would match.
    assert!(
        step_block.contains(". scripts/lib/cold-cut-bypass-guard.sh")
            && step_block.contains("cold_cut_bypass_arm_check"),
        "#1086: the arm-check step must SOURCE scripts/lib/cold-cut-bypass-guard.sh and call \
         cold_cut_bypass_arm_check. step_block:\n{step_block}"
    );
    assert!(
        step_block.contains("COLD_CUT_BYPASS_CAM: ${{ vars.COLD_CUT_BYPASS_CAM }}")
            && step_block.contains("COLD_CUT_BYPASS_INPUT: ${{ vars.COLD_CUT_BYPASS_INPUT }}"),
        "#1086: the arm-check step's OWN env: block must carry both repository variables (a step \
         only sees env vars set on itself). step_block:\n{step_block}"
    );
    // The guard needs ALL_CAMBOX (the SAME ternary the recording step uses) to tell an ARMED sweep
    // run from an INERT single-camera / workflow_dispatch run — so the honest banner is possible.
    assert!(
        step_block.contains("ALL_CAMBOX: ${{ github.event_name == 'pull_request' && '1' || '0' }}"),
        "#1086: the arm-check step's env: block must carry ALL_CAMBOX (same ternary as the \
         recording step) so the guard can tell an armed sweep run from an inert one. \
         step_block:\n{step_block}"
    );
}
