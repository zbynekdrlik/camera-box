//! issue 1242 — the FINAL step: the ABSOLUTE strict-zero per-segment fold is RESTORED, now that the
//! residual FIFO copy churn's source is fixed (issues 1318/1320 render-freeze cure, bundle
//! `02b53180b` / its descendant `7e8efff6a`) AND the interim lane's precondition is met: >= 2
//! consecutive strict-clean post-cure runs. Data-first (mining tool
//! `scripts/window_gate_walkdown.py 02b53180b 7e8efff6a` over the live verdict corpus, segregated by
//! the rig-verified `version-strih.json.genlock_build_sha`):
//!
//!   | run | genlock | w_fail_strict | worst BEAT-unif | CAM2 windows |
//!   |---|---|---|---|---|
//!   | 180691712 | 02b53180b | 0 | 0.9988 | 0/0 0/0 |
//!   | 977889848 | 7e8efff6a | 0 | 0.9988 | 0/0 0/0 |   (PR #1322 attempt 2)
//!   | 2019585820 | 7e8efff6a | 0 | 0.9976 | 0/0 0/0 |   (PR #1322 attempt 3)
//!   | 622403283 | 7e8efff6a | 1 | 0.9976 | 0/0 0/0 |   (attempt 1: ONE CAM1 gap 0/1)
//!
//! 977889848 + 2019585820 = >= 2 consecutive `02b53180b`-or-later runs with
//! `windows_failed_report_only == 0` (plus 180691712) → the strict-zero restore precondition. CAM2
//! reads 0/0 on every window across all four post-16.9 splitter-fed runs (cam2 became splitter-fed
//! ~16.9 13:00, the imag-HDMI projection tap retired with imag-nb, issue 1316) → the #1251 CAM2
//! per-cambox override removal precondition. Attempt 1's single CAM1 gap is exactly what strict-zero
//! now REDs (the owner's "copies=0 must block" directive); the guard rail for a healthy-chain false
//! red is the one-line report-only revert (re-arm `copies_gaps_tolerance_gates_overall_pass() ->
//! true`), never a silent widen.
//!
//! The restore is a ONE-FUNCTION flip per seam (`gate-allowance-restore-red-green` dormant-mechanism
//! pattern): DISARM both `copies_gaps_tolerance_gates_overall_pass()` AND
//! `segment_singleton_allowance_gates_overall_pass()` -> the `decide` `else` arm `copies==0 &&
//! gaps==0` governs `overall_pass_term`. `WINDOW_COPIES_GAPS_TOLERANCE` stays 2 as the #1132/#1220
//! dormant OBSERVABILITY lens (`relaxed_pass` still reports "the disarmed rescue would pass this,
//! strict blocks it" — the masking-guard visibility that run 622403283's CAM1 gap exercises); the
//! blocking fold is absolute strict-zero via the disarmed seams, not via the const. The CAM2
//! override map is emptied (`&[]`); the override machinery stays fully wired for a future per-box
//! need. `UNIFORM_FRACTION_MIN` was already restored to 0.95 by the interim lane — unchanged here.
//!
//! Default-feature test (no `#![cfg(feature = "probe")]`) — both modules are crate-root pub.
//! Tier-0 #557 bans even `cargo test --no-run` locally; the fold logic was verified RED→GREEN via a
//! std-only `rustc --test` replica of `decide_with_tolerance` (seams armed vs disarmed).

use camera_box::presentation_cadence::{cadence_uniformity_gate_pass, UNIFORM_FRACTION_MIN};
use camera_box::window_gate::{
    copies_gaps_tolerance_gates_overall_pass, decide, decide_for_cambox,
    segment_singleton_allowance_gates_overall_pass, WINDOW_COPIES_GAPS_TOLERANCE,
    WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX,
};

// --- the restore: BOTH copies/gaps seams are DISARMED (the absolute strict-zero fold) ---

#[test]
fn both_copies_gaps_seams_are_disarmed_for_the_strict_zero_fold() {
    // The FINAL step: disarm BOTH seams so `decide`'s `else` arm (`copies==0 && gaps==0`) governs
    // `overall_pass_term`. RED against the interim state (both `true`).
    assert!(
        !copies_gaps_tolerance_gates_overall_pass(),
        "issue 1242 strict-zero restore: the <=2 tolerance channel must NOT rescue the blocking fold"
    );
    assert!(
        !segment_singleton_allowance_gates_overall_pass(),
        "issue 1242 strict-zero restore: the <=1/<=1 singleton band must NOT rescue the blocking fold"
    );
}

#[test]
fn any_nonzero_copies_or_gaps_now_reds_the_blocking_fold() {
    // The core of the restore: ANY nonzero copies/gaps fails `overall_pass_term` — including the
    // single-copy/single-gap churn signature the interim tol=2 (and the singleton band) absorbed.
    // RED against the interim state (copies=1..2 passed the fold there).
    for &(c, g) in &[(1u32, 0u32), (0, 1), (1, 1), (2, 0), (0, 2), (2, 2)] {
        let d = decide(100, 0, c, g);
        assert!(
            !d.overall_pass_term,
            "issue 1242: copies={c} gaps={g} must RED the strict-zero blocking fold: {d:?}"
        );
        assert!(
            !d.strict_pass,
            "and still fails strict_pass — visible, never masked: {d:?}"
        );
    }
    // 622403283's exact shape (a single CAM1 gap) now reds — the restored gate working as designed.
    assert!(
        !decide(100, 0, 0, 1).overall_pass_term,
        "issue 1242: a single gap (run 622403283 CAM1 0/1) now REDs the fold"
    );
}

#[test]
fn a_clean_window_still_passes_all_three_verdicts() {
    let d = decide(100, 0, 0, 0);
    assert!(
        d.strict_pass && d.overall_pass_term && d.relaxed_pass,
        "a 0/0 window passes every verdict: {d:?}"
    );
}

#[test]
fn the_tolerance_const_stays_the_dormant_observability_lens_at_two() {
    // The const is NOT set to 0: it stays 2 as the #1132/#1220 dormant observability lens. The
    // blocking fold is strict-zero via the DISARMED seams (the `else` arm ignores the const), while
    // `relaxed_pass` still reports what the tol-2 rescue WOULD say — a `relaxed_pass==true`,
    // `overall_pass_term==false` window is the disarmed rescue visibly doing nothing (the #1132
    // masking guard). Run 622403283's CAM1 gap reads exactly this: within the tol-2 lens
    // (`windows_over_copies_gaps_tolerance=0`) yet strict-failing (`windows_failed_report_only=1`).
    assert_eq!(
        WINDOW_COPIES_GAPS_TOLERANCE, 2,
        "the const stays 2 as the observability lens; the fold is strict-zero via disarmed seams"
    );
    let d = decide(100, 0, 1, 0);
    assert!(
        d.relaxed_pass,
        "the tol-2 lens still absorbs a single copy (observability): {d:?}"
    );
    assert!(
        !d.overall_pass_term,
        "but the strict-zero blocking fold rejects it — visibly, never masked: {d:?}"
    );
}

// --- CAM2 per-cambox override DROPPED (removal precondition met) ---

#[test]
fn cam2_override_dropped_cam2_now_uses_the_default_strict_fold() {
    // RED against the interim `&[("CAM2", 25)]`: the map is now empty — CAM2 reads 0/0 on every
    // window across all four post-16.9 splitter-fed runs (180691712, 977889848, 2019585820,
    // 622403283), so the HW carve-out (issue 1249, the imag-HDMI-tap era) is no longer needed.
    assert_eq!(
        WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX,
        &[] as &[(&str, u32)],
        "issue 1242: the CAM2 tolerance-25 override is dropped (post-16.9 splitter-fed CAM2 is 0/0)"
    );
    // CAM2 now folds exactly like every other box: strict-zero, no per-box relaxation.
    let cam2 = decide_for_cambox("CAM2", 100, 0, 3, 0);
    let default = decide(100, 0, 3, 0);
    assert_eq!(
        cam2, default,
        "CAM2 decision == the default decide (override gone): {cam2:?}"
    );
    assert!(
        !cam2.overall_pass_term,
        "CAM2 copies=3 now REDs under the default strict-zero fold: {cam2:?}"
    );
}

// --- the uniformity floor at the restored value (already 0.95 from the interim lane) ---

#[test]
fn uniformity_floor_stays_at_the_restored_point_ninety_five() {
    assert_eq!(
        UNIFORM_FRACTION_MIN, 0.95,
        "the interim lane already restored 0.90 -> 0.95; this step leaves it there"
    );
    // pre-fix churny run 25635487 worst beat-corrected 0.9481 -> RED at floor 0.95.
    assert!(!cadence_uniformity_gate_pass(
        Some(0.9481),
        Some(UNIFORM_FRACTION_MIN)
    ));
    // post-fix runs 180691712 / 977889848 (0.9988) + 2019585820 / 622403283 (0.9976) -> PASS.
    assert!(cadence_uniformity_gate_pass(
        Some(0.9988),
        Some(UNIFORM_FRACTION_MIN)
    ));
    assert!(cadence_uniformity_gate_pass(
        Some(0.9976),
        Some(UNIFORM_FRACTION_MIN)
    ));
}
