//! issue 888 — RE-GATE: temporary, user-directed (2026-07-30) relaxation of the `[4d/8]`
//! render-budget gate's IMAG term to REPORT-ONLY, while strih + stream stay STRICT — RESTORED to
//! STRICT here once real measured data (10 independent `Full-path E2E` runs, 2026-07-30 19:20
//! through 2026-08-03, imag comfortably at 4.8-6.4ms against its 16.67ms budget with burns
//! confirmed ON) showed the relaxation's own restore bar met many times over. See the design
//! comment on issue 888 for the full dataset and the reasoning.
//!
//! Original root cause (issue 886 / issue 865, measured 2026-07-30): the measurement burn cost
//! ~11.5ms of imag's 16.67ms (60fps) frame budget — the instrument's own cost, not a product
//! regression. Three consecutive gate runs failed on this one term, blocking PR #704 (37 bundled,
//! otherwise-finished tickets) from ever reaching a verdict, which is why the term was relaxed to
//! report-only in the first place (`cdfd1fd4d`).
//!
//! Locks BOTH directions of the RESTORED state so a future edit can't silently re-relax (or
//! silently merge back) the gate:
//! - imag's render-budget-gate.py call stays its OWN, separate invocation (positioned after the
//!   strih/stream call's closing `fi`, before `[4e/8]`, exactly where the relaxation put it) but
//!   is STRICT again: `exit 1` on failure, same shape strih/stream already use — no more
//!   WARN-without-abort branch, no env-var bypass knob.
//! - strih + stream remain in their OWN, separate, still-STRICT invocation (same `--box` args,
//!   still `exit 1` on failure) — and imag must NOT be back in that same call/window.
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
/// call must be STRICT AGAIN (restored by issue 888, 2026-08-03): `exit 1` on failure, same shape
/// strih/stream already use — no more WARN-without-abort branch, no env-var bypass knob.
#[test]
fn imag_render_budget_call_is_strict_again_after_888_restore() {
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
        region.contains("exit 1"),
        "issue 888 (restored 2026-08-03): imag's render-budget term must be STRICT again — an \
         `exit 1` must exist in this region, same as strih/stream. Got:\n{region}"
    );
    assert!(
        !region.to_uppercase().contains("REPORT-ONLY") && !region.contains("NOT aborting"),
        "issue 888 (restored 2026-08-03): no report-only / non-aborting language should remain in \
         this region — the term is strict again. Got:\n{region}"
    );
    assert!(
        region.contains("issue 888") || region.contains("#888"),
        "issue 888: the abort message should still name issue 888 for history/traceability. \
         Got:\n{region}"
    );
    let region_upper = region.to_uppercase();
    assert!(
        !region_upper.contains("SKIP_IMAG_RENDER")
            && !region_upper.contains("IMAG_RENDER_GATE_SKIP"),
        "issue 888: must never gain a new env-var bypass knob (a silent env default is exactly \
         how a relaxation quietly comes back). Got:\n{region}"
    );
}

/// The `[4d/8]` step's own BANNER echo (printed at the top of the step, before ANY box is
/// measured) must not lie about imag's strictness. The banner sits ~1760 chars BEFORE the
/// `--box "strih=` anchor every other test in this file/`harness_imag_topology.rs` scopes its
/// window from, so it was outside every existing assertion and stayed stale through the restore
/// (2026-08-03 supervisor-found defect on PR #957): it still read
/// `imag is measured but REPORT-ONLY (issue 888, temporary — see below)` even though the actual
/// gate call below it was already restored to `exit 1`.
#[test]
fn banner_no_longer_advertises_imag_as_report_only_or_temporary() {
    let s = read("scripts/recording-e2e.sh");
    let banner_start = s
        .find("[4d/8] #405/#406/#462 render-budget gate")
        .expect("recording-e2e.sh must have the [4d/8] render-budget gate banner echo");
    let line_end = s[banner_start..]
        .find('\n')
        .expect("the [4d/8] banner echo must be a single line ending in a newline");
    let banner_line = &s[banner_start..(banner_start + line_end)];
    let upper = banner_line.to_uppercase();
    assert!(
        !upper.contains("REPORT-ONLY") && !upper.contains("REPORT ONLY"),
        "issue 888 (restored 2026-08-03): the [4d/8] banner must not advertise imag as \
         report-only any more -- all three boxes (strih/stream/imag) are strict now. \
         Got:\n{banner_line}"
    );
    assert!(
        !banner_line.to_lowercase().contains("temporary"),
        "issue 888 (restored 2026-08-03): the [4d/8] banner must not call imag's strictness \
         temporary any more. Got:\n{banner_line}"
    );
    assert!(
        !banner_line.to_lowercase().contains("non-aborting"),
        "issue 888 (restored 2026-08-03): the [4d/8] banner must not describe imag's term as \
         non-aborting under any wording. Got:\n{banner_line}"
    );
}

// The `strih_stream_render_budget_call_stays_strict_and_excludes_imag` test above still proves
// the split itself (imag measured by its OWN call, not re-merged into the strih/stream one) — no
// separate test needed here; restoring strictness only changes imag's OWN call's abort behavior.
