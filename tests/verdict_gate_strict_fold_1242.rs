//! issue 1242 — the WALK-DOWN step (2026-09-17, ROZHODNUTÉ 5706131227) for the residual FIFO copy
//! churn. After issues 1318/1320 fixed the render-freeze churn SOURCE, task-1 attribution (comment
//! 5706065579) proved the residual `<=1/<=1` copy/gap churn is a DOWNSTREAM structural floor of the
//! 60->30 decimation + genlock FIFO on the splitter topology, NOT a fixable source fault. So the
//! walk-down landed on the calibrated `<=1/<=1` SINGLETON band (the owner's 22.8. issue-1169 bar),
//! NOT absolute strict-zero (which RED-ed run 443513281 on one surviving FIFO hold and was DROPPED):
//!
//!   - `copies_gaps_tolerance_gates_overall_pass()` `true` -> `false` (seam 2 DISARMED), so `decide`'s
//!     `if`/`else if` fold falls through to the still-armed seam 4 singleton band.
//!   - `segment_singleton_allowance_gates_overall_pass()` stays `true` and now GOVERNS the fold: a
//!     window with `copies <= 1 && gaps <= 1` passes (with the loud singleton-consumed note),
//!     `>= 2` of either REDs `overall_pass`.
//!   - `WINDOW_COPIES_GAPS_TOLERANCE` stays 2 as the REPORT-ONLY observability lens (shapes
//!     `relaxed_pass` only) — a `relaxed_pass==true`/`overall_pass_term==false` window (copies/gaps
//!     in `[2, 2]`) is the disarmed rescue visibly doing nothing (the #1132 masking guard).
//!   - `WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX` CAM2->25 override DROPPED (`&[]`): cam2 became
//!     splitter-fed ~16.9 13:00 and all post-16.9 splitter-fed runs read CAM2 within the default.
//!
//! Evidence (band safety): the five post-cure splitter-fed runs (977889848, 2019585820, 443513281,
//! 605445038, 1249662438) read per-window worst copies=1 / gaps=1 — inside the band; a 2 reds.
//!
//! Default-feature test (no `#![cfg(feature = "probe")]`) — both modules are crate-root pub.
//! Tier-0 #557 bans even `cargo test --no-run` locally; the fold logic was verified RED->GREEN via
//! a std-only `rustc --test` replica of `decide_with_tolerance` at the armed-vs-disarmed seams.

use camera_box::presentation_cadence::{cadence_uniformity_gate_pass, UNIFORM_FRACTION_MIN};
use camera_box::window_gate::{
    copies_gaps_tolerance_gates_overall_pass, decide, decide_for_cambox,
    segment_singleton_allowance_gates_overall_pass, WINDOW_COPIES_GAPS_TOLERANCE,
    WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX,
};

// --- the seam state the walk-down lands on ---

#[test]
fn tolerance_seam_disarmed_singleton_band_governs() {
    // The core walk-down: seam 2 DISARMED, seam 4 (the `<=1/<=1` singleton band) still armed and
    // now governing the fold. The full strict-zero restore (BOTH `false`) was DROPPED as
    // unattainable on an irreducible downstream floor.
    assert!(
        !copies_gaps_tolerance_gates_overall_pass(),
        "issue 1242: the `<=2` tolerance seam is DISARMED"
    );
    assert!(
        segment_singleton_allowance_gates_overall_pass(),
        "issue 1242: the `<=1/<=1` singleton band stays armed and governs the fold"
    );
}

#[test]
fn tolerance_const_stays_two_as_the_observability_lens() {
    assert_eq!(
        WINDOW_COPIES_GAPS_TOLERANCE, 2,
        "issue 1242: the const stays 2 as the report-only lens (not 0), keeping the #1132 masking guard"
    );
}

#[test]
fn uniformity_floor_stays_point_ninety_five() {
    assert_eq!(
        UNIFORM_FRACTION_MIN, 0.95,
        "issue 1242 (interim) restored 0.95; the walk-down does not touch the uniformity floor"
    );
}

// --- the fold behaviour at the singleton-band boundary ---

#[test]
fn window_over_one_now_reds_the_fold() {
    // 2 copies exceeded the `<=1/<=1` singleton band -> must RED overall_pass_term (they DID pass
    // under the interim tol-2 fold, so this is the walk-down's tightening).
    assert!(
        !decide(100, 0, 2, 0).overall_pass_term,
        "copies=2 > the `<=1` singleton band must fail the fold"
    );
    assert!(
        !decide(100, 0, 0, 2).overall_pass_term,
        "gaps=2 > the `<=1` singleton band must fail the fold"
    );
    // 1 copy/gap sits exactly at the band and still passes.
    assert!(
        decide(100, 0, 1, 0).overall_pass_term,
        "copies=1 == the singleton band still passes (absorbed)"
    );
    assert!(
        decide(100, 0, 0, 1).overall_pass_term,
        "gaps=1 == the singleton band still passes (absorbed)"
    );
}

#[test]
fn two_reds_the_fold_but_stays_within_the_relaxed_lens_divergence() {
    // The KEY divergence: copies=2 REDs the blocking fold (`<=1` singleton) yet the report-only
    // tol-2 lens still absorbs it (`relaxed_pass == true`) -- the disarmed rescue visibly doing
    // nothing (the #1132 masking guard).
    let d = decide(100, 0, 2, 0);
    assert!(
        !d.overall_pass_term,
        "copies=2 REDs the singleton fold: {d:?}"
    );
    assert!(
        d.relaxed_pass,
        "copies=2 stays within the tol-2 observability lens (non-masking): {d:?}"
    );
}

#[test]
fn clean_window_passes_both_verdicts() {
    let d = decide(100, 0, 0, 0);
    assert!(d.strict_pass && d.overall_pass_term && d.relaxed_pass);
}

#[test]
fn single_copy_is_absorbed_by_the_calibrated_singleton_band() {
    // A single copy is the designed irreducible `<=1/<=1` downstream churn: absorbed into the
    // blocking verdict by the calibrated singleton band (the owner's issue-1169 bar), LOUDLY
    // (`singleton_allowance_consumed`), while it stays a visible strict failure -- never masked.
    let d = decide(100, 0, 1, 0);
    assert!(
        d.overall_pass_term,
        "copies=1 absorbed by the `<=1/<=1` singleton band"
    );
    assert!(
        d.singleton_allowance_consumed,
        "the absorption fires the loud singleton note/count"
    );
    assert!(
        !d.strict_pass,
        "copies=1 still fails strict_pass -- visible, never masked"
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

// --- CAM2 override DROPPED: cam2 is a normal splitter leg now ---

#[test]
fn cam2_override_dropped_normal_splitter_leg_1242() {
    // The #1251 CAM2->25 override is DROPPED (map `&[]`): cam2 became splitter-fed ~16.9 13:00 and
    // all post-16.9 splitter-fed runs read CAM2 within the default. So CAM2 now uses the default
    // tolerance and the singleton fold like every box -- a 2-copy CAM2 window REDs.
    assert!(
        WINDOW_COPIES_GAPS_TOLERANCE_PER_CAMBOX.is_empty(),
        "issue 1242: the CAM2 override is dropped (empty map, the tested walk-back state)"
    );
    assert!(
        !decide_for_cambox("CAM2", 100, 0, 2, 0).overall_pass_term,
        "CAM2 copies=2 REDs the fold now (no override to absorb it)"
    );
    assert!(
        decide_for_cambox("CAM2", 100, 0, 1, 0).overall_pass_term,
        "CAM2 copies=1 is absorbed by the singleton band, exactly like any box"
    );
    // Byte-identical to the default `decide` for every count now.
    assert_eq!(
        decide_for_cambox("CAM2", 100, 0, 8, 0),
        decide(100, 0, 8, 0),
        "CAM2 == the default decide (override dropped)"
    );
}
