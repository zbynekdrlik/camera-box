//! #889 (2026-07-30, user decision on #883) — the per-cambox-window `#186` zero-loss verdict's
//! `copies`/`gaps` terms become REPORT-ONLY: still computed, still printed, still written to the
//! verdict JSON, but no longer force a window (or the run) to FAIL. This is the HEAVIEST
//! relaxation in this repo's history — it relaxes the core zero-loss CLAIM itself, not a
//! measurement cost (contrast #888, which only relaxed imag's render-budget term). See #889 for
//! the full decision record (the failing evidence, run 30547146285) and the 3-part restore path
//! (root-cause #883 item 4, two consecutive clean STRICT runs, delete this relaxation).
//!
//! **Not relaxed by this module:** `frame_count > 0` and the whole-run duration floor — those are
//! computed and folded in exactly as before. The `#881` calibrated optical-`undecodable` floor
//! (per-window here, run-wide in `probe::recording_segments::segment_continuity`) WAS in that
//! "never relaxed" set until issue 915 (2026-08-01, user decision) made it report-only too, using
//! the exact same shape this module already established for `copies`/`gaps` — see
//! `crate::optical_floor::gates_overall_pass` for the seam and restore path (issue 905).
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

/// The strict-vs-relaxed decision for one cambox window, given its already-computed counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGateDecision {
    /// The pre-#889 verdict — UNCHANGED semantics: `frame_count > 0 && <undecodable within the
    /// #881 calibrated floor> && copies == 0 && gaps == 0`. Still computed, still exposed as
    /// `CamboxSegment::pass`, and drives the #889 loud per-window WARN — never silently dropped.
    pub strict_pass: bool,
    /// #889: the verdict actually folded into `overall_pass` — `copies`/`gaps` do NOT
    /// participate: `frame_count > 0 && <undecodable within the #881 floor>`.
    pub relaxed_pass: bool,
}

impl WindowGateDecision {
    /// `true` exactly when THIS window's `copies`/`gaps` term (issue 889) and/or the optical
    /// undecodable floor (issue 915) is the reason `strict_pass` and `relaxed_pass` disagree —
    /// i.e. some report-only relaxation, and only a report-only relaxation, is doing work on this
    /// window. `frame_count == 0` fails BOTH verdicts unconditionally (an absent cambox proves
    /// nothing either way, never relaxed) — see `zero_frames_fails_both_verdicts_889` below.
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
    let relaxed_pass =
        frame_count > 0 && (undecodable_ok || !crate::optical_floor::gates_overall_pass());
    // `strict_pass` keeps its pre-889-AND-pre-915 meaning byte-for-byte: frame_count>0 &&
    // undecodable within floor && copies==0 && gaps==0 -- computed directly (no longer derived
    // from `relaxed_pass`, since issue 915 decoupled the floor from that derivation).
    let strict_pass = frame_count > 0 && undecodable_ok && copies == 0 && gaps == 0;
    WindowGateDecision {
        strict_pass,
        relaxed_pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_alone_fails_strict_but_passes_relaxed_889() {
        let d = decide(100, 0, 1, 0);
        assert!(
            !d.strict_pass,
            "a copy must still fail the strict verdict: {d:?}"
        );
        assert!(
            d.relaxed_pass,
            "#889: copies alone must not fail the relaxed verdict: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn gap_alone_within_tolerance_fails_strict_but_passes_relaxed_889() {
        // 2026-08-05 re-gate (issue 889 ROZHODNUTÉ): a single gap sits AT the singleton
        // tolerance -- still must not fail relaxed.
        let d = decide(100, 0, 0, 1);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "889 re-gate: a single gap (at the singleton tolerance) must not fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_over_singleton_tolerance_fails_relaxed_889_regate() {
        // 2026-08-05 re-gate: 2 copies exceeds the singleton tolerance (<=1) -- the window must
        // FAIL the relaxed verdict again (this is the whole point of the re-gate: a return of the
        // issue-971 regression class, 10-45 copies/window, must fail loudly).
        let d = decide(100, 0, 2, 0);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "889 re-gate: 2 copies exceeds the singleton tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "an over-tolerance window is not rescued by any report-only relaxation: {d:?}"
        );
    }

    #[test]
    fn gaps_over_singleton_tolerance_fails_relaxed_889_regate() {
        // 2026-08-05 re-gate: 2 gaps exceeds the singleton tolerance (<=1) -- must fail relaxed.
        let d = decide(100, 0, 0, 2);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "889 re-gate: 2 gaps exceeds the singleton tolerance -- must fail relaxed again: {d:?}"
        );
        assert!(!d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_both_at_singleton_tolerance_pass_relaxed_889_regate() {
        // Mirrors the measured residual the threshold decision was calibrated against (run
        // 31033239950 attempt 1, comment id 5195798868): windows with 1 copy AND 1 gap
        // simultaneously, fully absorbed by the singleton tolerance.
        let d = decide(100, 0, 1, 1);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "889 re-gate: copies=1 AND gaps=1 together must still pass relaxed (both at tolerance): {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_together_over_tolerance_fail_relaxed_889_regate() {
        // 2026-08-05 re-gate: both terms over tolerance together must fail relaxed -- this is the
        // exact pre-fix regression shape (the original issue-889 failing evidence).
        let d = decide(100, 0, 2, 3);
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "889 re-gate: copies=2 AND gaps=3 both exceed the singleton tolerance -- must fail relaxed: {d:?}"
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
}
