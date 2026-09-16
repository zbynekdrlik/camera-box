//! issue 1242 — the WALK-BACK step for the residual FIFO copy churn, after issues 1318/1320
//! root-caused + fixed the strih PROGRAM render-freeze → stream FIFO underrun → relock storm that
//! produced it (cure = genlock bundle `02b53180b`). Data-first (mining tool
//! `scripts/window_gate_walkdown.py` over the live verdict corpus): the ONLY post-fix run on the
//! cure bundle (180691712) is strict-clean (every window 0 copies / 0 gaps, worst beat-corrected
//! uniformity 0.9988), but that is n=1 — too thin to restore absolute strict-zero on one run. So
//! this step TIGHTENS two blocking-gate constants a data-supported amount and leaves the full
//! strict-zero restore as the explicit NEXT step:
//!
//!   - `WINDOW_COPIES_GAPS_TOLERANCE` 5 → 2  (interim; the tolerance CHANNEL still governs the fold
//!     via `copies_gaps_tolerance_gates_overall_pass() == true`, per #1220; strict-zero mechanism
//!     stays wired-but-dormant — the `gate-allowance-restore-red-green` pattern).
//!   - `UNIFORM_FRACTION_MIN` 0.90 → 0.95  (the issue-1142 cadence floor, gated on
//!     `beat_corrected_uniform_fraction` since #1250; post-fix worst beat-corrected 0.9976–0.9988,
//!     the churny pre-fix run 0.9481 correctly REDs).
//!   - `WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX` CAM2 → 25 kept (no post-16.9 splitter-fed run
//!     exists yet; removal precondition PINNED below, data-conditional).
//!
//! Default-feature test (no `#![cfg(feature = "probe")]`) — both modules are crate-root pub.
//! Tier-0 #557 bans even `cargo test --no-run` locally; the fold logic was verified RED→GREEN via
//! a std-only `rustc --test` replica of `decide_with_tolerance` + `cadence_uniformity_gate_pass`.

use camera_box::presentation_cadence::{cadence_uniformity_gate_pass, UNIFORM_FRACTION_MIN};
use camera_box::window_gate::{
    copies_gaps_tolerance_gates_overall_pass, decide, decide_for_cambox,
    segment_singleton_allowance_gates_overall_pass, WINDOW_COPIES_GAPS_TOLERANCE,
    WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX,
};

// --- the two walked constants (RED against the pre-change 5 / 0.90) ---

#[test]
fn copies_gaps_tolerance_walked_to_two() {
    assert_eq!(
        WINDOW_COPIES_GAPS_TOLERANCE, 2,
        "issue 1242 interim walk 5 -> 2 (post-fix n=1 too thin for strict-zero)"
    );
}

#[test]
fn uniformity_floor_restored_to_point_ninety_five() {
    assert_eq!(
        UNIFORM_FRACTION_MIN, 0.95,
        "issue 1242 restore 0.90 -> 0.95; post-fix beat-corrected worst >= 0.9976"
    );
}

// --- the fold behaviour at the new tolerance boundary ---

#[test]
fn window_over_two_now_reds_the_fold() {
    // 3 copies exceeded the OLD tol=5 fold (passed); at tol=2 it must RED overall_pass_term.
    assert!(
        !decide(100, 0, 3, 0).overall_pass_term,
        "copies=3 > tol 2 must fail the fold"
    );
    // 2 copies sit exactly at the new tolerance and still pass.
    assert!(
        decide(100, 0, 2, 0).overall_pass_term,
        "copies=2 == tol 2 still passes"
    );
}

#[test]
fn clean_window_passes_both_verdicts() {
    let d = decide(100, 0, 0, 0);
    assert!(d.strict_pass && d.overall_pass_term && d.relaxed_pass);
}

#[test]
fn single_copy_is_the_documented_interim_gap() {
    // HONEST interim limitation: a single-copy window still passes the tol=2 fold — only the
    // strict-zero restore (the explicit NEXT step) catches the ticket's exact churn signature.
    // The single copy stays VISIBLE as a strict failure, never silent.
    let d = decide(100, 0, 1, 0);
    assert!(
        d.overall_pass_term,
        "copies=1 absorbed at tol 2 (interim; strict restore is next)"
    );
    assert!(
        !d.strict_pass,
        "copies=1 still fails strict_pass — visible, never masked"
    );
}

// --- the uniformity floor at the restored value, on the BEAT-corrected reading ---

#[test]
fn floor_reds_the_prefix_churn_passes_postfix() {
    // pre-fix churny run 25635487 worst beat-corrected 0.9481 -> RED at floor 0.95.
    assert!(!cadence_uniformity_gate_pass(
        Some(0.9481),
        Some(UNIFORM_FRACTION_MIN)
    ));
    // post-fix run 180691712 worst beat-corrected 0.9988, adjacent clean 0.9976 -> PASS.
    assert!(cadence_uniformity_gate_pass(
        Some(0.9988),
        Some(UNIFORM_FRACTION_MIN)
    ));
    assert!(cadence_uniformity_gate_pass(
        Some(0.9976),
        Some(UNIFORM_FRACTION_MIN)
    ));
}

// --- CAM2 override kept; its removal precondition PINNED (data-conditional, report-only) ---

#[test]
fn cam2_override_kept_until_a_post_16_9_splitter_fed_run() {
    // KEPT at 25: no post-16.9 splitter-fed run exists yet (cam2 was the imag-HDMI projection tap
    // until ~16.9 13:00; the post-fix run 180691712 at 15.9 22:31 predates the swap). REMOVAL
    // PRECONDITION for the next step: the first post-16.9 splitter-fed E2E whose CAM2 windows sit
    // within the DEFAULT tolerance -> set this map to `&[]`. Until then it stays exactly here.
    assert_eq!(WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX, &[("CAM2", 25)]);
    // The override still absorbs CAM2's over-rate band while the default gate is now tighter.
    assert!(
        decide_for_cambox("CAM2", 100, 0, 20, 0).overall_pass_term,
        "CAM2 tol 25 absorbs 20"
    );
    assert!(
        !decide_for_cambox("CAM3", 100, 0, 20, 0).overall_pass_term,
        "CAM3 uses default tol 2"
    );
}

// --- strict-zero restore is the EXPLICIT next step, NOT taken here (both seams stay armed) ---

#[test]
fn strict_restore_is_the_next_step_not_this_one() {
    // Interim: the tolerance channel still governs the fold (NOT strict-zero). The full restore =
    // flip BOTH seams to `false`, gated on >= 2 more consecutive `02b53180b`-or-later runs with
    // windows_failed_report_only == 0. Both stay wired/armed here (dormant-mechanism pattern).
    assert!(
        copies_gaps_tolerance_gates_overall_pass(),
        "tolerance channel still governs (interim)"
    );
    assert!(
        segment_singleton_allowance_gates_overall_pass(),
        "singleton fallback stays wired"
    );
}
