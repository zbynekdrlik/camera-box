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
fn delivery_spread_gate_is_blocking_since_1142() {
    // #1142 (owner mandate 2026-08-19): the delivery-spread term now BLOCKS overall_pass — the
    // "green" runs it used to pass were FALSELY green (the phase lottery, 3.97 vs 85 ms, hid a real
    // delivery-spread failure behind a green gate). At the existing SPREAD_THRESHOLD_MS=24 bound a
    // wide spread now REDs the run. Was report-only (issue 1033); the [red] commit keeps the seam at
    // `false` while this test already asserts the blocking contract, so it FAILS there; the [green]
    // commit flips it.
    assert!(
        gate::gates_overall_pass(),
        "#1142: the delivery-spread gate must be BLOCKING (gates_overall_pass()==true)"
    );
}

#[test]
fn blocking_fold_reds_a_wide_spread_since_1142() {
    // Blocking fold (gates_overall == true): a FAILING (wide) delivery spread now REDs overall.
    // The pure `fold` pins both seam states, and the LIVE call-site helper the verdict makes.
    assert!(
        !gate::fold(false, true),
        "blocking: a wide spread reds the run"
    );
    assert!(gate::fold(true, true), "blocking: a tight spread passes");
    assert!(
        !gate::folds_into_overall_pass(false),
        "#1142: with the seam blocking, a wide delivery spread must fold to FAIL"
    );
    assert!(gate::folds_into_overall_pass(true));
    // The report-only fold direction is still pinned (a hypothetical revert): fold(_, false) passes.
    assert!(
        gate::fold(false, false),
        "report-only direction: a wide spread would not red"
    );
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
