//! issue 798 (path A) — the imag-leg recording verdict must ship REPORT-ONLY first.
//!
//! Tier-0 (default features): pins the report-only contract of `camera_box::imag_leg_gate`, the
//! pure fold seam `recording-verdict.rs` (probe-gated, no local type-check) calls. RED before the
//! GREEN flip proves the seam ships report-only (`false`), NOT the naive blocking value that would
//! immediately red the first run that ever produces an imag partial.

use camera_box::imag_leg_gate;

#[test]
fn imag_leg_gate_ships_report_only() {
    // Path A: the imag leg flows + is surfaced, but does NOT gate overall_pass yet. A follow-up
    // ticket flips this to `true` (and folds in the issue-887 produced-vs-presented deficit) once
    // healthy imag runs accumulate. Shipping `true` here would red the first imag run (0/76 today).
    assert!(
        !imag_leg_gate::gates_overall_pass(),
        "issue 798 path A: imag leg must ship REPORT-ONLY (gates_overall_pass()==false); \
         flipping it blocking is a separate follow-up ticket"
    );
}

#[test]
fn report_only_never_reds_a_run_even_when_the_imag_leg_fails() {
    // Report-only fold (gates_overall == false): a FAILING imag leg still passes overall — the
    // whole point of path A (surface, never red).
    assert!(
        imag_leg_gate::fold(false, false),
        "report-only: a failing imag leg must NOT red the run"
    );
    assert!(
        imag_leg_gate::fold(true, false),
        "report-only: a passing imag leg passes"
    );
    // …and via the LIVE-seam call-site helper (the exact call `recording-verdict.rs` makes):
    assert!(
        imag_leg_gate::folds_into_overall_pass(false),
        "with the seam report-only today, even a failing imag leg must fold to pass"
    );
    assert!(imag_leg_gate::folds_into_overall_pass(true));
}

#[test]
fn blocking_seam_would_gate_a_failing_imag_leg() {
    // The follow-up's target state: once flipped to blocking (gates_overall == true) a failing imag
    // leg reds the run, a passing one passes. The pure `fold` lets us pin BOTH seam states without
    // touching the live toggle.
    assert!(
        !imag_leg_gate::fold(false, true),
        "blocking: a failing imag leg reds the run"
    );
    assert!(
        imag_leg_gate::fold(true, true),
        "blocking: a passing imag leg passes"
    );
}
