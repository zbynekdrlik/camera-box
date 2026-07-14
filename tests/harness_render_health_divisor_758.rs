//! #758 [0/8]/[1/8] preflight — two more items the user demanded after finding the fused gate
//! caught neither an imag render regression nor a missing MV render-divisor capability BEFORE
//! burning a run ("ako to ze to nezachytili preflight testy!!!! — because they didn't exist"):
//!
//! 1. imag RENDER-HEALTH: with the Multiview projector verified OPEN (the earlier #758 item),
//!    the PROGRAM compositor must still hold its OWN 60fps frame budget BEFORE any recording
//!    starts. Reuses the EXISTING #405 `render-budget-gate.py` + `render-budget-gate` Rust
//!    binary (`render_budget::classify`) rather than inventing a parallel threshold — its own
//!    60fps calibration (2fps tolerance, 1000/60≈16.67ms budget) already IS the "activeFps >= 58
//!    / averageFrameRenderTime < 16.7ms" bar from the spec. Checked across SEVERAL independent
//!    windows (never one averaged window) so a transient dip cannot be averaged away.
//! 2. MV render-DIVISOR CAPABILITY (a #756 follow-up item): whether the deployed imag frontend
//!    binary was actually built with the #276/#278 multiview render-divisor decouple symbol
//!    (`obs_display_set_render_divisor`) — the same read-only `nm -D -u` check
//!    `scripts/setup-imag.sh` already runs at provisioning time (#499). WARN-only for now (a
//!    known gap: even the unified genlock-parity build does not carry this symbol on imag's
//!    Linux frontend yet) — the render-health item above is the hard gate on the OUTCOME. The
//!    flip to FAIL (once the Linux divisor symbol ships, a separate #756 PR) must be exactly one
//!    line to change: `IMAG_DIVISOR_CAPABILITY_FAIL` defaults to `"0"`.
//!
//! Structural, source-text assertions (same discipline as the rest of this repo's harness suite
//! — see tests/harness_e2e_execute_verdict_703.rs) since both items are read-only preflight
//! probes against LIVE OBS-WS / a live imag SSH host that only the rig itself can exercise
//! end-to-end.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn render_health_preflight_invokes_the_existing_405_render_budget_gate_for_imag() {
    let s = recording_e2e();
    assert!(
        s.contains("imag render-health preflight"),
        "#758: recording-e2e.sh must name the imag render-health preflight step"
    );
    assert!(
        s.contains("render-budget-gate.py") && s.contains("--box") && s.contains("imag=${IMAG_IP}"),
        "#758: the render-health preflight must reuse the EXISTING render-budget-gate.py \
         against imag, not invent a parallel measurement path"
    );
    assert!(
        s.contains("RENDER_TARGET_FPS_IMAG"),
        "#758: must reuse the SAME RENDER_TARGET_FPS_IMAG env var the later [4d/8] gate uses \
         (single source of truth for imag's target fps)"
    );
    assert!(
        s.contains("--verdict-bin \"$PROBE_BIN_DIR/render-budget-gate\""),
        "#758: must reuse the Rust render_budget::classify verdict binary, not a new threshold"
    );
}

#[test]
fn render_health_preflight_fails_loud_with_the_specified_slovak_message() {
    let s = recording_e2e();
    assert!(
        s.contains("imag render pod budgetom s MV otvoreným")
            && s.contains("skontroluj divisor/projektory/zataz"),
        "#758: the render-health FAIL message must match the specified operator-facing text \
         exactly (Slovak, names the fix path)"
    );
    // The FAIL block must actually abort the run, not just warn.
    let fail_idx = s
        .find("imag render pod budgetom s MV otvoreným")
        .expect("FAIL message must be present");
    let after = &s[fail_idx..fail_idx + 200.min(s.len() - fail_idx)];
    assert!(
        after.contains("exit 1"),
        "#758: the render-health FAIL path must exit 1 (a hard gate on the outcome), got: {after}"
    );
}

#[test]
fn render_health_preflight_polls_multiple_sustained_windows_not_one_average() {
    let s = recording_e2e();
    assert!(
        s.contains("RENDER_HEALTH_WINDOWS") && s.contains("RENDER_HEALTH_WINDOW_S"),
        "#758: must poll across several independent windows (SUSTAINED), never a single \
         averaged measurement — a transient mid-window dip must not be averaged away"
    );
    assert!(
        s.contains("for _rhw in $(seq 1 \"$RENDER_HEALTH_WINDOWS\")"),
        "#758: the render-health check must loop over RENDER_HEALTH_WINDOWS independent calls"
    );
}

#[test]
fn divisor_capability_check_reads_the_same_symbol_setup_imag_checks_at_provisioning() {
    let s = recording_e2e();
    assert!(
        s.contains("imag MV render-divisor capability check"),
        "#758: recording-e2e.sh must name the divisor capability check step"
    );
    assert!(
        s.contains("nm -D -u /usr/bin/obs") && s.contains("obs_display_set_render_divisor"),
        "#758: must read the SAME exported symbol scripts/setup-imag.sh already checks at \
         provisioning time (#499) — a read-only query of the deployed binary, no config mutation"
    );
}

#[test]
fn divisor_capability_check_warns_by_default_never_fails_until_flipped() {
    let s = recording_e2e();
    assert!(
        s.contains(r#"IMAG_DIVISOR_CAPABILITY_FAIL="${IMAG_DIVISOR_CAPABILITY_FAIL:-0}""#),
        "#758: the divisor capability check must default to WARN (0), never FAIL, until the \
         Linux divisor symbol actually ships (a separate #756 follow-up)"
    );
    assert!(
        s.contains("[preflight] WARN: ${_divisor_msg}"),
        "#758: an absent symbol must print a loud WARN line, never silently pass"
    );
    // The flip to FAIL must be a single conditional on the ONE env var above — never a second,
    // independently-hardcoded FAIL path that the "one-line change" flip would miss. Count LINES
    // referencing the var (not raw substring matches — the default-assignment line itself
    // legitimately mentions the name twice: `VAR="${VAR:-0}"`).
    let fail_lines = s
        .lines()
        .filter(|l| l.contains("IMAG_DIVISOR_CAPABILITY_FAIL"))
        .count();
    assert_eq!(
        fail_lines, 2,
        "#758: IMAG_DIVISOR_CAPABILITY_FAIL must be referenced on exactly two LINES (the \
         default-assignment line + the one `if` check that flips WARN->FAIL) — got {fail_lines}, \
         which means the flip is no longer a one-line change"
    );
}

#[test]
fn both_new_preflight_items_are_all_cambox_only_and_ordered_between_mv_liveness_and_step_2() {
    let s = recording_e2e();
    let mv_liveness_idx = s
        .find("PREFLIGHT_MV_SOURCES")
        .expect("the #758 MV-clone liveness check must exist");
    let render_health_idx = s
        .find("imag render-health preflight")
        .expect("render-health preflight must exist");
    let divisor_idx = s
        .find("imag MV render-divisor capability check")
        .expect("divisor capability check must exist");
    let step2_idx = s
        .find("echo \"[2/8]")
        .expect("[2/8] deploy step must exist");

    assert!(
        mv_liveness_idx < render_health_idx,
        "#758: render-health preflight must come AFTER the MV-clone liveness check (needs \
         PROBE_BIN_DIR/render-budget-gate already built, same constraint as frozen-camera-gate)"
    );
    assert!(
        render_health_idx < divisor_idx,
        "#758: the divisor capability WARN check must come after the render-health hard gate \
         (render-health is the OUTCOME gate; divisor is only a capability marker)"
    );
    assert!(
        divisor_idx < step2_idx,
        "#758: both new preflight items must complete BEFORE [2/8]'s deploy step starts — \
         these are preflight, not mid-run, checks"
    );
}
