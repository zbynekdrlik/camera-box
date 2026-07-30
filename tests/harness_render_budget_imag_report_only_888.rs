//! issue 888 — RE-GATE: temporary, user-directed (2026-07-30) relaxation of the `[4d/8]`
//! render-budget gate's IMAG term to REPORT-ONLY, while strih + stream stay STRICT.
//!
//! Root cause (issue 886, measured, reproduces to within 0.2ms across 3 runs): the measurement
//! burn costs ~11.5ms of imag's 16.67ms (60fps) frame budget — the instrument's own cost, not a
//! product regression (burn OFF: imag renders 4.5-5.9ms, zero skips, every window — the STILL
//! STRICT `[1/8]` render-health preflight above proves this). Three consecutive gate runs failed
//! on this one term, blocking PR #704 (37 bundled, otherwise-finished tickets) from ever reaching
//! a verdict. The user's directive: release this ONE term, keep everything else strict, iterate.
//!
//! Locks BOTH directions so a future edit can't silently widen (or narrow) the relaxation:
//! - imag's render-budget-gate.py call must be its OWN, separate, NON-aborting invocation that
//!   logs a loud WARN (pass or fail) naming issue 888 and issue 886 in words, with no `exit 1`
//!   anywhere in its branch and no new env-var bypass knob.
//! - strih + stream must remain in their OWN, separate, still-STRICT invocation (same `--box`
//!   args, still `exit 1` on failure) — and imag must NOT be in that same call any more.
//!
//! Pure static (`fs::read_to_string` + substring/ordering asserts) — mirrors the style of
//! tests/harness_imag_topology.rs and tests/harness_render_budget_gate.rs; no OBS, no ssh, no
//! live rig.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// strih + stream MUST stay in their OWN render-budget-gate.py call, and that call must still
/// abort (`exit 1`) on failure — unchanged strict semantics. imag must NOT be part of this same
/// call/window any more (the split itself, locked from this direction).
#[test]
fn strih_stream_render_budget_call_stays_strict_and_excludes_imag() {
    let s = read("scripts/recording-e2e.sh");
    // Anchor on `--box "strih=` (unique to this call site — the [1/8] preflight never measures
    // strih, only imag; see tests/harness_imag_topology.rs's own note on this anchor choice).
    let call = s
        .find("--box \"strih=")
        .expect("recording-e2e.sh must invoke render-budget-gate.py with a strih box");
    let fi_rel = s[call..]
        .find("\nfi\n")
        .expect("the strih/stream render-budget-gate call must close with its own `fi`");
    let fi = call + fi_rel;
    let window = &s[call.saturating_sub(200)..fi];
    assert!(
        window.contains("--box \"strih=${STRIH}:${RENDER_TARGET_FPS_STRIH:-30}\""),
        "render-budget-gate call must keep the strih=…:30 box. Got:\n{window}"
    );
    assert!(
        window.contains("--box \"stream=${STREAM}:${RENDER_TARGET_FPS_STREAM:-30}\""),
        "render-budget-gate call must keep the stream=…:30 box. Got:\n{window}"
    );
    assert!(
        window.contains("exit 1"),
        "issue 888: strih/stream must remain STRICT (their call must still abort with exit 1). \
         Got:\n{window}"
    );
    assert!(
        !window.contains("--box \"imag="),
        "issue 888: imag must NOT be part of the same call/window as strih+stream any more — it \
         must be measured by its OWN separate, report-only call. Got:\n{window}"
    );
}

/// imag's render-budget term must be measured by its OWN separate render-budget-gate.py call,
/// positioned after the strih/stream call's closing `fi` and before the `[4e/8]` step, and that
/// call must be REPORT-ONLY: no `exit 1` anywhere in its region, a loud WARN logged either way,
/// and the WARN text names both issue 888 (this relaxation) and issue 886 (the measured root
/// cause) in words — plus no new env-var bypass knob (a hardcoded, one-line-deletable branch).
#[test]
fn imag_render_budget_call_is_report_only_and_names_888_and_886() {
    let s = read("scripts/recording-e2e.sh");
    let strih_call = s
        .find("--box \"strih=")
        .expect("recording-e2e.sh must invoke render-budget-gate.py with a strih box");
    let strih_fi_rel = s[strih_call..]
        .find("\nfi\n")
        .expect("the strih/stream render-budget-gate call must close with its own `fi`");
    let after_strih_fi = strih_call + strih_fi_rel + "\nfi\n".len();
    let next_step_rel = s[after_strih_fi..]
        .find("[4e/8]")
        .expect("recording-e2e.sh must still have a [4e/8] step after the render-budget gate(s)");
    let region = &s[after_strih_fi..(after_strih_fi + next_step_rel)];

    assert!(
        region.contains("--box \"imag=${IMAG_IP}:${RENDER_TARGET_FPS_IMAG:-60}\""),
        "issue 888: imag must still be MEASURED (its own render-budget-gate.py call), just \
         separately from strih/stream. Got:\n{region}"
    );
    assert!(
        !region.contains("exit 1"),
        "issue 888: imag's render-budget term must be REPORT-ONLY — no exit 1 anywhere in this \
         region. Got:\n{region}"
    );
    assert!(
        region.contains("WARN"),
        "issue 888: imag's render-budget result (pass OR fail) must be logged loudly (WARN), so \
         nobody reads silence as strictness. Got:\n{region}"
    );
    assert!(
        region.contains("issue 888"),
        "issue 888: the WARN text must name issue 888 (this relaxation ticket) in words. \
         Got:\n{region}"
    );
    assert!(
        region.contains("issue 886"),
        "issue 888: the WARN text must name issue 886 (the measured root cause) in words. \
         Got:\n{region}"
    );
    let region_upper = region.to_uppercase();
    assert!(
        !region_upper.contains("SKIP_IMAG_RENDER")
            && !region_upper.contains("IMAG_RENDER_GATE_SKIP"),
        "issue 888: must be a hardcoded, one-line-deletable branch — NEVER a new env-var bypass \
         knob (a silent env default is exactly how 'temporary' becomes permanent). Got:\n{region}"
    );
}
