//! #889 (2026-07-30, user decision on #883) — the per-cambox-window `#186` zero-loss verdict's
//! `copies`/`gaps` terms became REPORT-ONLY: still computed, still printed, still written to the
//! verdict JSON, but no longer forced a window (or the run) to FAIL. This was the HEAVIEST
//! relaxation in this repo's history — it relaxed the core zero-loss CLAIM itself, not a
//! measurement cost (contrast #888, which only relaxed imag's render-budget term). See #889 for
//! the full decision record (the failing evidence, run 30547146285) and the 3-part restore path
//! (root-cause #883 item 4, two consecutive clean STRICT runs, delete this relaxation).
//!
//! ## 2026-08-05 RE-GATE — [`WINDOW_COPIES_GAPS_TOLERANCE`] (ticket 889 comment 5196190653)
//!
//! Root-cause condition 1 of the restore path above was met: the issue-971 dupe-preferring
//! decimation fix (PR #993) directly attacks the over-rate-grabber duplicate/gap injection this
//! relaxation existed for. Its MEASURED residual under live over-rate (run 31033239950 attempt 1)
//! was max 1 copy AND max 1 gap per window — the decimation's bounded one-deferral-per-boundary
//! design admits occasional singletons BY DESIGN. Per the user's standing 2026-07-31 directive
//! ("jedna stratená snímka nie je problém — one lost frame per window is explicitly acceptable"),
//! `copies`/`gaps` are re-gated with a per-window SINGLETON tolerance instead of either extreme:
//! absolute zero (which would re-create a permanently-red gate on this bounded residual — the
//! issue-909 class of mistake, just relocated) or leaving the terms permanently report-only (which
//! would give up real regression protection against a return of the issue-971 10-45-per-window
//! failure class). `relaxed_pass` now requires `copies <= WINDOW_COPIES_GAPS_TOLERANCE && gaps <=
//! WINDOW_COPIES_GAPS_TOLERANCE` in addition to its prior terms — `strict_pass` (absolute zero)
//! stays byte-for-byte unchanged, so the singleton residual stays fully VISIBLE (never silent)
//! even though it no longer fails the run.
//!
//! **Not relaxed by this module:** `frame_count > 0` and the whole-run duration floor — those are
//! computed and folded in exactly as before. The `#881` calibrated optical-`undecodable` floor
//! (per-window here, run-wide in `probe::recording_segments::segment_continuity`) WAS in that
//! "never relaxed" set until issue 915 (2026-08-01, user decision) made it report-only too, using
//! the exact same shape this module already established for `copies`/`gaps` — see
//! `crate::optical_floor::gates_overall_pass` for the seam and restore path (issue 905).
//!
//! ## 2026-08-06 RECALIBRATION — tolerance 1 → 2 (issue 889 comment 5198131539)
//!
//! The tolerance above was calibrated from n=2 hardware runs (max 1 copy AND max 1 gap per
//! window each). A THIRD valid run (same commit, same rig, the transient rig anomaly from a
//! failed first attempt having cleared) measured the SAME order of total residual burden per run
//! (~5-7 copies + ~5-7 gaps) but showed it can randomly CLUSTER onto fewer windows — per-window
//! max across the three healthy runs is `{1, 1, 2}`, not a flat `1`. At tolerance=1 the gate was
//! flaky by construction: a healthy run has a real chance of a window landing on 2 defects purely
//! by how the same fixed total happens to distribute, and would FAIL a run that is not actually
//! anomalous. Tolerance=2 absorbs that measured clustering while preserving full discrimination —
//! the genuine anomaly (the failed first attempt) measured 9-15 copies and 8-15 gaps on EVERY
//! window, 4.5x and more over the new threshold, so the re-gate still catches it every time. The
//! rejected alternative (leave tolerance at 1 and re-run until a healthy run happens to pass) is
//! the banned blind-rerun pattern — gambling on clustering instead of measuring it, ~40 min per
//! rig cycle, and a gate that stays flaky for every future PR. See ticket 889 comment 5198131539
//! for the full evidence (three run IDs, per-window breakdowns) this recalibration is based on.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as `optical_floor.rs` / `av_window.rs`'s `#861` relaxation: the whole `probe`
//! module is `#[cfg(feature = "probe")]` (CLAUDE.md's Local Build Policy — heavy deps balloon the
//! shared dev1 `target/`), so a change confined to `probe::recording_segments` has ZERO local
//! verification path, not even a compile check. This module is the PURE strict-vs-relaxed
//! decision seam; `probe::recording_segments::window_segment` only calls it and wires the result
//! onto `CamboxSegment`.
//!
//! ## The #861 precedent this mirrors
//!
//! `av_window::av_offset_gate_pass` stayed UNCHANGED in meaning when its gate went report-only —
//! only the CALLER stopped folding it into `overall_pass` ("the pure decision function stays
//! unchanged, still measured, still fails CLOSED on thin data — only the caller stopped folding
//! its result"). This module follows the identical shape: [`decide`] still computes the pre-#889
//! STRICT verdict ([`WindowGateDecision::strict_pass`], byte-for-byte the same boolean
//! `probe::recording_segments::CamboxSegment::pass` has always held) alongside the NEW relaxed
//! verdict the caller actually folds into `overall_pass`.

/// The per-window tolerance applied to `copies`/`gaps` when folding them back into
/// `relaxed_pass`/`overall_pass` (2026-08-05 re-gate, ticket 889 comment 5196190653; recalibrated
/// 1 → 2 on 2026-08-06, ticket 889 comment 5198131539). Hardcoded, no env knob — a silent env
/// default is exactly how "temporary" becomes permanent (the original issue-889 requirement 4),
/// and this repo's standing rule is that a needed capability is always ON by default, never a
/// forgettable toggle (`features-default-on-never-forgettable-toggle` in the project memory). A
/// window with `copies > WINDOW_COPIES_GAPS_TOLERANCE` OR `gaps > WINDOW_COPIES_GAPS_TOLERANCE`
/// fails [`WindowGateDecision::relaxed_pass`] again; at or under the tolerance it is absorbed
/// (still visible via `strict_pass` / the #889 per-window WARN, never silent).
///
/// **Calibration basis (issue 889 comment 5198131539):** three valid hardware runs on the same
/// fix (PR #993's dupe-preferring decimation) measured a per-window max of `{1, 1, 2}` copies/gaps
/// while carrying the same ~5-7-per-run total residual burden, randomly clustered — tolerance=1
/// was calibrated on only two of those runs and is flaky by construction (a healthy run can land
/// 2 defects on one window purely by clustering). The genuine anomaly run measured 9-15 copies and
/// 8-15 gaps on EVERY window (4.5x+ over this threshold), so tolerance=2 keeps full discrimination
/// against a real regression while no longer flaking on healthy clustering. See the module doc's
/// 2026-08-06 RECALIBRATION section above for the full rationale and the rejected alternative.
pub const WINDOW_COPIES_GAPS_TOLERANCE: u32 = 2;

/// The strict-vs-relaxed decision for one cambox window, given its already-computed counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGateDecision {
    /// The pre-#889 verdict — UNCHANGED semantics: `frame_count > 0 && <undecodable within the
    /// #881 calibrated floor> && copies == 0 && gaps == 0`. Still computed, still exposed as
    /// `CamboxSegment::pass`, and drives the #889 loud per-window WARN — never silently dropped.
    pub strict_pass: bool,
    /// The verdict actually folded into `overall_pass`. Issue 889 (2026-07-30): `frame_count > 0
    /// && <undecodable within the #881 floor>`. 2026-08-05 RE-GATE: gained `&& copies <=
    /// WINDOW_COPIES_GAPS_TOLERANCE && gaps <= WINDOW_COPIES_GAPS_TOLERANCE` — `copies`/`gaps` are
    /// no longer fully report-only, they are tolerated up to the calibrated per-window threshold
    /// and gate again above it.
    pub relaxed_pass: bool,
}

impl WindowGateDecision {
    /// `true` exactly when THIS window's `copies`/`gaps` term being WITHIN the per-window
    /// tolerance (2026-08-05 re-gate, recalibrated 2026-08-06) and/or the optical undecodable
    /// floor (issue 915) is the reason `strict_pass` and `relaxed_pass` disagree — i.e. some
    /// report-only/tolerance relaxation, and only that, is doing work on this window.
    /// `frame_count == 0` fails BOTH verdicts unconditionally (an absent cambox proves nothing
    /// either way, never rescued) — see `zero_frames_fails_both_verdicts_889` below. A window
    /// whose `copies`/`gaps` EXCEED the tolerance fails both verdicts too (not rescued) — see the
    /// `..._over_tolerance_..` tests below.
    pub fn relaxed_by_889(&self) -> bool {
        !self.strict_pass && self.relaxed_pass
    }
}

/// Decide both verdicts for one window from its already-computed counts (`probe::
/// recording_segments::window_segment` supplies these — this function re-derives nothing about
/// frame contents, it only combines counts that are already known).
pub fn decide(frame_count: u32, undecodable: u32, copies: u32, gaps: u32) -> WindowGateDecision {
    let undecodable_ok = crate::optical_floor::window_within_floor(undecodable, frame_count);
    // Issue 915 (2026-08-01, user decision): the optical undecodable floor (issue 881) becomes
    // report-only while cam1's grabber (issue 909) + the 120Hz monitor (issue 881) are unresolved
    // -- `undecodable_ok` above is UNCHANGED (still fully computed, still feeds `strict_pass`
    // below byte-for-byte) but no longer participates in the RELAXED verdict that feeds
    // `overall_pass` when `crate::optical_floor::gates_overall_pass()` is false. Restore: flip
    // that one function back to `true` (see its own doc for the full restore path on issue 905).
    //
    // 2026-08-05 RE-GATE: `copies`/`gaps` re-join the relaxed verdict, but only above the
    // per-window tolerance -- see `WINDOW_COPIES_GAPS_TOLERANCE`'s own doc for the decision
    // record (recalibrated 1 -> 2 on 2026-08-06, ticket 889 comment 5198131539).
    let within_tolerance =
        copies <= WINDOW_COPIES_GAPS_TOLERANCE && gaps <= WINDOW_COPIES_GAPS_TOLERANCE;
    let relaxed_pass = frame_count > 0
        && (undecodable_ok || !crate::optical_floor::gates_overall_pass())
        && within_tolerance;
    // `strict_pass` keeps its pre-889-AND-pre-915 meaning byte-for-byte: frame_count>0 &&
    // undecodable within floor && copies==0 && gaps==0 -- computed directly (no longer derived
    // from `relaxed_pass`, since issue 915 decoupled the floor from that derivation).
    let strict_pass = frame_count > 0 && undecodable_ok && copies == 0 && gaps == 0;
    WindowGateDecision {
        strict_pass,
        relaxed_pass,
    }
}

/// The reason(s) a single cambox window fails its RELAXED verdict (`WindowGateDecision::
/// relaxed_pass == false`) -- extracted (issue-889 re-gate deep-review findings 1+2) so
/// `src/bin/recording-verdict.rs`'s per-window WARN print (issue 889 visibility requirement 1,
/// extended by issue 915 and the 2026-08-05 re-gate) matches on this instead of re-deriving the
/// same conditions inline. The inline version being replaced misclassified a `frame_count == 0`
/// window as an exceeded optical floor (`crate::optical_floor::window_within_floor`'s defensive
/// `frame_count == 0` clause is not itself a "floor exceeded" signal), and worded the floor
/// reason as "currently gates overall_pass" unconditionally even while
/// `crate::optical_floor::gates_overall_pass()` is hardcoded `false` today.
///
/// Call this ONLY on a window whose `relaxed_pass` has already failed -- a window that PASSED its
/// relaxed verdict has no "failure reason" (see the separate WITHIN-TOLERANCE / REPORT-ONLY
/// prints `recording-verdict.rs` uses for that case). Calling it on a passing window is harmless
/// (it re-derives the same conditions and returns an empty `Vec`, or a `FloorWithinReportOnly`
/// reason that did not actually fail anything), but is not this function's intended use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaxedFailureReason {
    /// `frame_count == 0` -- an absent/non-emitting cambox. Never rescued by any relaxation, and
    /// takes priority over every other reason: a 0-frame window's `copies`/`gaps`/`undecodable`
    /// counts carry no meaningful signal.
    EmptyWindow,
    /// `copies` and/or `gaps` exceed the per-window tolerance (the 2026-08-05 re-gate,
    /// recalibrated 2026-08-06, [`WINDOW_COPIES_GAPS_TOLERANCE`]).
    OverCopiesGapsTolerance,
    /// The issue-881 optical undecodable floor is exceeded AND it currently gates `overall_pass`
    /// (`crate::optical_floor::gates_overall_pass() == true` -- the issue-905 restore-path state).
    /// Hardcoded `false` today, so this variant cannot fire yet -- kept so the seam stays correct
    /// once issue 905 restores the floor's gating.
    FloorExceededGating,
    /// The issue-881 optical undecodable floor is exceeded but stays REPORT-ONLY
    /// (`crate::optical_floor::gates_overall_pass() == false`, today's state) -- worth printing
    /// for context even on a window that already fails for another reason, but not itself why
    /// the window fails.
    FloorWithinReportOnly,
}

/// See [`RelaxedFailureReason`]'s own doc for the full rationale and call-site discipline.
///
/// `frame_count == 0` is checked FIRST and short-circuits every other reason (finding 1 of the
/// issue-889 re-gate review) -- the optical floor / tolerance checks below are meaningless on an
/// empty window and must never be consulted for one. The floor reason's severity (`Gating` vs
/// `WithinReportOnly`) is decided by `crate::optical_floor::gates_overall_pass()` (finding 2) --
/// never worded as "currently gates overall_pass" unconditionally.
pub fn relaxed_failure_reasons(
    frames: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
) -> Vec<RelaxedFailureReason> {
    if frames == 0 {
        return vec![RelaxedFailureReason::EmptyWindow];
    }
    let mut reasons = Vec::new();
    if copies > WINDOW_COPIES_GAPS_TOLERANCE || gaps > WINDOW_COPIES_GAPS_TOLERANCE {
        reasons.push(RelaxedFailureReason::OverCopiesGapsTolerance);
    }
    let floor_ok = crate::optical_floor::window_within_floor(undecodable, frames);
    if !floor_ok {
        if crate::optical_floor::gates_overall_pass() {
            reasons.push(RelaxedFailureReason::FloorExceededGating);
        } else {
            reasons.push(RelaxedFailureReason::FloorWithinReportOnly);
        }
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_is_calibrated_at_two_889_recalibration() {
        // Pins the calibrated NUMBER itself, not just its use through the const (2026-08-06
        // recalibration, ticket 889 comment 5198131539): three valid hardware runs measured a
        // per-window max of {1, 1, 2} copies/gaps on the same fix, so tolerance=1 (calibrated on
        // only two of those runs) was flaky by construction. See the const's own doc and the
        // module doc's 2026-08-06 RECALIBRATION section for the full evidence.
        assert_eq!(WINDOW_COPIES_GAPS_TOLERANCE, 2);
    }

    #[test]
    fn copies_at_tolerance_fails_strict_but_passes_relaxed_889() {
        // Finding 4 of the issue-889 re-gate review: renamed from
        // `copies_alone_fails_strict_but_passes_relaxed_889` -- that name/message implied "copies
        // alone can never fail relaxed", which was true pre-re-gate but is no longer true (over
        // the tolerance fails relaxed again, see `copies_over_tolerance_fails_relaxed_889_regate`
        // below). 2026-08-06 recalibration: renamed again from
        // `copies_at_singleton_tolerance_...` -- "singleton" (implying exactly one) stopped being
        // an accurate description once the tolerance became 2. The fixture value tracks the const
        // (not a literal) so this test self-adjusts with any future recalibration; it is
        // load-bearing that it sits AT the tolerance, not under it.
        let d = decide(100, 0, WINDOW_COPIES_GAPS_TOLERANCE, 0);
        assert!(
            !d.strict_pass,
            "a copy must still fail the strict verdict: {d:?}"
        );
        assert!(
            d.relaxed_pass,
            "re-gate: copies AT the tolerance boundary, not over it, must not fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn gap_alone_at_tolerance_fails_strict_but_passes_relaxed_889() {
        // 2026-08-05 re-gate (issue 889 ROZHODNUTÉ), recalibrated 2026-08-06 (comment 5198131539):
        // a gap AT the tolerance boundary -- still must not fail relaxed.
        let d = decide(100, 0, 0, WINDOW_COPIES_GAPS_TOLERANCE);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "re-gate: a gap AT the tolerance boundary must not fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_over_tolerance_fails_relaxed_889_regate() {
        // 2026-08-06 recalibration: renamed from `copies_over_singleton_tolerance_...` -- the
        // fixture now sits at tolerance+1 (3) so this test tracks whatever the const is
        // calibrated to instead of hardcoding the pre-recalibration boundary (2). The window must
        // FAIL the relaxed verdict again (this is the whole point of the re-gate: a return of the
        // issue-971 regression class, 10-45 copies/window, must fail loudly).
        let d = decide(100, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: copies over the tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "an over-tolerance window is not rescued by any report-only relaxation: {d:?}"
        );
    }

    #[test]
    fn gaps_over_tolerance_fails_relaxed_889_regate() {
        // 2026-08-06 recalibration: renamed from `gaps_over_singleton_tolerance_...`; fixture
        // tracks the const (tolerance+1) instead of a hardcoded pre-recalibration literal.
        let d = decide(100, 0, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: gaps over the tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_both_at_tolerance_pass_relaxed_889_regate() {
        // Mirrors the measured residual the ORIGINAL (2026-08-05) threshold decision was
        // calibrated against (run 31033239950 attempt 1, comment id 5195798868): windows with
        // copies AND gaps simultaneously AT the tolerance, fully absorbed. 2026-08-06
        // recalibration: renamed from `..._both_at_singleton_tolerance_...`, fixture now tracks
        // the const (2, not the pre-recalibration 1).
        let d = decide(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE,
            WINDOW_COPIES_GAPS_TOLERANCE,
        );
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "re-gate: copies AND gaps together must still pass relaxed (both at tolerance): {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_together_over_tolerance_fail_relaxed_889_regate() {
        // 2026-08-05 re-gate: both terms over tolerance together must fail relaxed -- this is the
        // exact pre-fix regression shape (the original issue-889 failing evidence). 2026-08-06
        // recalibration: fixtures now sit at tolerance+1/tolerance+2 so this stays "both terms
        // over, asymmetric" regardless of the calibrated tolerance value.
        let d = decide(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE + 1,
            WINDOW_COPIES_GAPS_TOLERANCE + 2,
        );
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "re-gate: copies AND gaps both exceed the tolerance -- must fail relaxed: {d:?}"
        );
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn zero_frames_fails_both_verdicts_889() {
        // #889 does not touch `frame_count > 0` — an absent cambox proves nothing either way.
        let d = decide(0, 0, 0, 0);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "a 0-frame window must still fail the relaxed verdict: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "not #889's doing -- frame_count==0 fails both: {d:?}"
        );
    }

    #[test]
    fn undecodable_over_floor_now_passes_relaxed_but_fails_strict_915() {
        // Issue 915 (2026-08-01, user decision): the optical undecodable floor is now
        // report-only -- an over-floor undecodable count no longer fails the RELAXED verdict
        // (only frame_count==0 does), even though the STRICT verdict still fails on it exactly
        // as before.
        let d = decide(10, 5, 0, 0); // 5 undecodable of 10 frames -- past the #881 per-window floor (4)
        assert!(
            !d.strict_pass,
            "the optical floor still fails the STRICT verdict, unchanged: {d:?}"
        );
        assert!(
            d.relaxed_pass,
            "#915: undecodable over floor no longer fails the relaxed verdict: {d:?}"
        );
        assert!(
            d.relaxed_by_889(),
            "#915's floor relaxation is now what's rescuing this window: {d:?}"
        );
    }

    #[test]
    fn clean_window_passes_both_verdicts() {
        let d = decide(100, 0, 0, 0);
        assert!(d.strict_pass);
        assert!(d.relaxed_pass);
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn undecodable_within_floor_and_clean_copies_gaps_passes_both_881() {
        // Mirrors `optical_floor`'s own calibrated floor -- unaffected by #889.
        let d = decide(847, 1, 0, 0);
        assert!(d.strict_pass);
        assert!(d.relaxed_pass);
    }

    // --- relaxed_failure_reasons (issue-889 re-gate deep-review findings 1+2) -------------------

    #[test]
    fn relaxed_failure_reasons_frames_zero_is_empty_window_889_review() {
        // Finding 1 of the issue-889 re-gate review: a frame_count==0 window must classify as
        // EmptyWindow, never as an exceeded optical floor --
        // `optical_floor::window_within_floor`'s defensive `frame_count == 0` clause is not
        // itself a "floor exceeded" signal. The RED commit for this test proved a prior version
        // of this function (which checked `over_tolerance`/`floor_ok` before ever asking "is this
        // window even empty") got this wrong, reporting `[FloorExceededGating]` instead.
        let reasons = relaxed_failure_reasons(0, 0, 0, 0);
        assert_eq!(
            reasons,
            vec![RelaxedFailureReason::EmptyWindow],
            "a 0-frame window must classify as EmptyWindow alone, never a floor reason: {reasons:?}"
        );
    }

    #[test]
    fn relaxed_failure_reasons_over_copies_tolerance_889_regate() {
        // 2026-08-06 recalibration: fixture tracks tolerance+1 (3) instead of the
        // pre-recalibration hardcoded 2. undecodable=0 stays within the floor -- the ONLY reason
        // is OverCopiesGapsTolerance.
        let reasons = relaxed_failure_reasons(100, 0, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert_eq!(reasons, vec![RelaxedFailureReason::OverCopiesGapsTolerance]);
    }

    #[test]
    fn relaxed_failure_reasons_within_tolerance_and_floor_is_empty_889_regate() {
        // Finding 6 coverage: copies AND gaps sit AT the tolerance (not over it), and
        // undecodable=0 is within the floor -- a window with these counts does not actually fail
        // `relaxed_pass` (see `copies_and_gaps_both_at_tolerance_pass_relaxed_889_regate` above),
        // so it has NO failure reason. This function is only ever called by
        // `recording-verdict.rs` on a window that already failed `relaxed_pass` -- this test just
        // pins the seam's own boundary behavior independent of that call-site discipline.
        // 2026-08-06 recalibration: fixture tracks the const instead of the pre-recalibration
        // hardcoded 1.
        let reasons = relaxed_failure_reasons(
            100,
            0,
            WINDOW_COPIES_GAPS_TOLERANCE,
            WINDOW_COPIES_GAPS_TOLERANCE,
        );
        assert!(
            reasons.is_empty(),
            "copies/gaps both AT tolerance -- no failure reason applies: {reasons:?}"
        );
    }

    #[test]
    fn relaxed_failure_reasons_over_floor_is_report_only_today_889_regate() {
        // Mirrors `undecodable_over_floor_now_passes_relaxed_but_fails_strict_915` above:
        // undecodable=5 over the per-window floor (4), frames=10. `gates_overall_pass()` is
        // hardcoded false today (issue 905's restore path is not yet landed), so this is
        // FloorWithinReportOnly, never FloorExceededGating.
        let reasons = relaxed_failure_reasons(10, 5, 0, 0);
        assert_eq!(reasons, vec![RelaxedFailureReason::FloorWithinReportOnly]);
    }

    #[test]
    fn relaxed_failure_reasons_over_tolerance_and_over_floor_both_reported_889_regate() {
        // Finding 2's "else-arm" scenario: a window can fail overall_pass via OverCopiesGapsTolerance
        // while ALSO carrying an over-floor undecodable count that is merely report-only today --
        // both reasons are returned so the caller can print both, instead of the old code's
        // misleading "currently gates overall_pass" wording for the floor half. 2026-08-06
        // recalibration: fixture tracks tolerance+1 (3) instead of the pre-recalibration
        // hardcoded 2.
        let reasons = relaxed_failure_reasons(10, 5, WINDOW_COPIES_GAPS_TOLERANCE + 1, 0);
        assert_eq!(
            reasons,
            vec![
                RelaxedFailureReason::OverCopiesGapsTolerance,
                RelaxedFailureReason::FloorWithinReportOnly,
            ]
        );
    }
}
