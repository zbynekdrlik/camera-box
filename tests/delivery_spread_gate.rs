//! issue 1033 — the ALL-CAMBOX cross-camera DELIVERY-latency spread gate must ship REPORT-ONLY
//! first (the fleet data is not tight-green — cam1's delivery lottery, ~66–81 ms spreads on recent
//! green runs).
//!
//! Tier-0 (default features): pins the report-only contract of `camera_box::delivery_spread_gate`,
//! the pure fold seam `recording-verdict.rs` (probe-gated, no local type-check) calls. RED before
//! the GREEN flip proves the seam ships report-only (`false`), NOT the naive would-gate-LIVE value
//! that would immediately red the 10+ recent green runs whose delivery spread sits at ~66–81 ms.

use camera_box::delivery_spread_gate as gate;

#[test]
fn delivery_spread_gate_ships_report_only_1033() {
    // The delivery-spread term flows + is surfaced, but does NOT gate overall_pass yet — because
    // the current fleet data is not tight-green (recent green runs sit at ~66–81 ms spread against
    // the 24 ms bound, driven by cam1's delivery lottery / issue-909 grabber class). A follow-up
    // flips this to `true` once cam1's lottery is killed and ~5 consecutive green runs hold the
    // spread ≤ ~10 ms. Shipping `true` here would red those recent green runs.
    assert!(
        !gate::gates_overall_pass(),
        "issue 1033: the delivery-spread gate must ship REPORT-ONLY (gates_overall_pass()==false); \
         the fleet data is not tight-green — flipping it blocking is a separate follow-up ticket"
    );
}

#[test]
fn report_only_never_reds_a_run_even_when_the_spread_is_wide_1033() {
    // Report-only fold (gates_overall == false): a FAILING (wide) delivery spread still passes
    // overall — the whole point today (surface, never red).
    assert!(
        gate::fold(false, false),
        "report-only: a wide delivery spread must NOT red the run"
    );
    assert!(
        gate::fold(true, false),
        "report-only: a tight delivery spread passes"
    );
    // …and via the LIVE-seam call-site helper (the exact call `recording-verdict.rs` makes):
    assert!(
        gate::folds_into_overall_pass(false),
        "with the seam report-only today, even a wide delivery spread must fold to pass"
    );
    assert!(gate::folds_into_overall_pass(true));
}

#[test]
fn blocking_seam_would_gate_a_wide_spread_1033() {
    // The follow-up's target state: once flipped to blocking (gates_overall == true) a wide
    // delivery spread reds the run, a tight one passes. The pure `fold` lets us pin BOTH seam
    // states without touching the live toggle — proving the flip is correct in both directions.
    assert!(
        !gate::fold(false, true),
        "blocking: a wide delivery spread reds the run"
    );
    assert!(
        gate::fold(true, true),
        "blocking: a tight delivery spread passes"
    );
}

#[test]
fn bound_reuses_the_switch_latency_spread_threshold_1033() {
    // No new, drifting constant — the delivery-spread bound IS the existing
    // switch_latency::SPREAD_THRESHOLD_MS (recalibrated to 24 ms by issue 1120; was the 16 ms
    // half-frame of issue 624). Both spreads are driven by the SAME CAM1 grabber (issue 1110)
    // and re-tighten on the SAME grabber swap, so they deliberately share ONE constant.
    assert_eq!(
        gate::DELIVERY_SPREAD_BOUND_MS,
        camera_box::switch_latency::SPREAD_THRESHOLD_MS,
        "issue 1033: the gate must reuse the existing bound, not define a second one"
    );
    assert_eq!(gate::DELIVERY_SPREAD_BOUND_MS, 24.0);
}
