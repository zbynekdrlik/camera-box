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
//!    `scripts/setup-imag.sh` already runs at provisioning time (#499). Originally WARN-only (a
//!    believed "known gap": the log-marker grep this ticket's original investigation used found
//!    zero #276/#278/divisor/multiview lines — but no such log line has EVER existed anywhere in
//!    the vendored source, so that was never real evidence). **#756 (2026-07-15) flipped this to
//!    FAIL-by-default**: a direct `nm -D -u` against the live imag frontend proves the symbol
//!    genuinely IS referenced — see `tests/harness_projector_count_756.rs` for the full
//!    investigation (the real render-health defect was 7 stray accumulated Multiview/Program
//!    projector windows, unrelated to the divisor mechanism itself, also fixed there).
//!    `IMAG_DIVISOR_CAPABILITY_FAIL` now defaults to `"1"`.
//! 3. imag STUDIO MODE must be OFF (a live finding discovered WHILE proving item 1 against the
//!    real rig): Studio Mode must be ON on EVERY broadcast box, imag included (user hard rule
//!    2026-07-15 — without Studio the multiview Preview cell is dead). `obs_phase2.py
//!    ensure-studio-mode-on` ALWAYS (idempotently) turns it ON on imag before render-health is
//!    measured, so the measurement reflects the PRODUCTION state (this INVERTS the former #758
//!    force-OFF step: the old Studio-ON render collapse was the pre-#767 distroav.so receiver
//!    churn, fixed by the keep-alive build — imag measures 60fps/1.8ms with Studio ON now).
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
    // #1232: the fixed `for _rhw in $(seq 1 "$RENDER_HEALTH_WINDOWS")` loop (a fixed TOTAL window
    // count) became a `while` loop that keeps sampling through a settle-adaptive warm-up phase
    // (bounded by RENDER_HEALTH_SETTLE_BUDGET_S) until RENDER_HEALTH_WINDOWS STRICT windows have
    // passed — see tests/harness_render_health_warmup_882.rs for the phase-machine coverage. The
    // "several independent windows, never one average" property still holds: each iteration is
    // its own independent render-budget-gate.py call.
    assert!(
        s.contains("while :; do") && s.contains("render_health_phase_outcome"),
        "#1232: the render-health check must loop (settle-adaptively) calling \
         render_health_phase_outcome once per independent window"
    );
    assert!(
        s.contains("RENDER_HEALTH_SETTLE_BUDGET_S"),
        "#1232: the settle-adaptive warm-up phase must be bounded by a wall-clock budget so a \
         box that never settles still fails loudly instead of retrying forever"
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

// #756 (2026-07-15) SUPERSEDES this test's original assertion: the #758 WARN-only default was
// a hedge against a "known gap" belief (the Linux divisor symbol missing) that was reached by
// grepping the OBS frontend log for text markers that never existed anywhere in the vendored
// source — never real evidence either way. A direct nm -D -u check against the live imag
// frontend (GENLOCK_BUILD_SHA.txt=26de1c3c2) proves the symbol genuinely IS referenced
// (count=1) — see tests/harness_projector_count_756.rs for the full investigation. The check
// therefore now FAILS by default; the WARN *branch* itself is kept (still reachable by
// explicitly overriding the env var to 0) so an operator can temporarily downgrade it without a
// code change, but that is no longer the shipped default.
#[test]
fn divisor_capability_check_fails_by_default_now_756() {
    let s = recording_e2e();
    assert!(
        s.contains(r#"IMAG_DIVISOR_CAPABILITY_FAIL="${IMAG_DIVISOR_CAPABILITY_FAIL:-1}""#),
        "#756: the divisor capability check must now default to FAIL (1) — nm -D -u on the live \
         imag frontend confirms obs_display_set_render_divisor genuinely ships; this is no \
         longer a 'known gap' the #758 WARN-only stance was hedging against"
    );
    assert!(
        s.contains("[preflight] WARN: ${_divisor_msg}"),
        "#756: the WARN branch must still exist (reachable by explicitly overriding \
         IMAG_DIVISOR_CAPABILITY_FAIL=0), even though it is no longer the default"
    );
    // The flip is a single conditional on the ONE env var above — never a second,
    // independently-hardcoded FAIL path. Count LINES referencing the var (not raw substring
    // matches — the default-assignment line itself legitimately mentions the name twice:
    // `VAR="${VAR:-1}"`).
    let fail_lines = s
        .lines()
        .filter(|l| l.contains("IMAG_DIVISOR_CAPABILITY_FAIL"))
        .count();
    assert_eq!(
        fail_lines, 2,
        "#756: IMAG_DIVISOR_CAPABILITY_FAIL must be referenced on exactly two LINES (the \
         default-assignment line + the one `if` check) — got {fail_lines}, which means the flip \
         is no longer a one-line change"
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

// ---------------------------------------------------------------------------------------------
// imag Studio Mode must be forced OFF before render-health is measured (a live finding while
// proving item 1 against the real rig — see the module doc comment, item 3).
// ---------------------------------------------------------------------------------------------

#[test]
fn imag_studio_mode_is_forced_on_before_render_health_is_measured() {
    let s = recording_e2e();
    assert!(
        s.contains("obs_phase2.py\" ensure-studio-mode-on --host \"$IMAG_IP\""),
        "#767: recording-e2e.sh must call obs_phase2.py ensure-studio-mode-on against imag \
         (Studio is MUST-BE-ON on every broadcast box — the Preview cell needs it) before \
         measuring render health"
    );
    assert!(
        !s.contains("ensure-studio-mode-off"),
        "#767: the former #758 force-OFF step must be GONE — turning Studio off on imag is \
         banned (user hard rule 2026-07-15); render health is measured in the production \
         Studio-ON state"
    );
    let studio_idx = s
        .find("ensure-studio-mode-on")
        .expect("the studio-mode-on preflight call must exist");
    let render_health_idx = s
        .find("imag render-health preflight")
        .expect("render-health preflight must exist");
    let projectors_idx = s
        .find("imag-nb Multiview + Program projectors must be OPEN")
        .expect("the MV/Program projector-open preflight must exist");
    assert!(
        projectors_idx < studio_idx,
        "#758: Studio Mode must be forced on AFTER the MV/Program projectors are opened \
         (same [0/8] preflight block, projectors first per the user's original binding demand)"
    );
    assert!(
        studio_idx < render_health_idx,
        "#767: Studio Mode must be forced on BEFORE render-health is measured, so the \
         measurement reflects the real production state, not a stale Studio-OFF reading that \
         would hide a Studio-ON render regression"
    );
}

#[test]
fn obs_phase2_defines_and_wires_ensure_studio_mode_on() {
    let s = fs::read_to_string(manifest_dir().join("scripts/obs_phase2.py"))
        .expect("read scripts/obs_phase2.py");
    assert!(
        s.contains("def ensure_studio_mode_on(a):"),
        "#767: obs_phase2.py must define ensure_studio_mode_on"
    );
    assert!(
        s.contains("\"ensure-studio-mode-on\": ensure_studio_mode_on"),
        "#767: ensure_studio_mode_on must be wired into the subcommand dispatch table"
    );
    assert!(
        s.contains("GetStudioModeEnabled") && s.contains("SetStudioModeEnabled"),
        "#767: ensure_studio_mode_on must read THEN conditionally set Studio Mode (idempotent, \
         never an unconditional write)"
    );
}
