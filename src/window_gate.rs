//! #889 (2026-07-30, user decision on #883) — the per-cambox-window `#186` zero-loss verdict's
//! `copies`/`gaps` terms become REPORT-ONLY: still computed, still printed, still written to the
//! verdict JSON, but no longer force a window (or the run) to FAIL. This is the HEAVIEST
//! relaxation in this repo's history — it relaxes the core zero-loss CLAIM itself, not a
//! measurement cost (contrast #888, which only relaxed imag's render-budget term). See #889 for
//! the full decision record (the failing evidence, run 30547146285) and the 3-part restore path
//! (root-cause #883 item 4, two consecutive clean STRICT runs, delete this relaxation).
//!
//! **Not relaxed by this module, now or ever:** the `#881` calibrated optical-`undecodable`
//! floor, `frame_count > 0`, the whole-run duration floor, and the `#186` run-wide `undecodable`
//! cap — those are computed and folded in exactly as before; see `crate::optical_floor` and
//! `probe::recording_segments::segment_continuity`. This module touches ONLY the `copies`/`gaps`
//! terms.
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
    /// `true` exactly when THIS window's `copies`/`gaps` term is the reason `strict_pass` and
    /// `relaxed_pass` disagree — i.e. #889's relaxation, and only #889's relaxation, is doing
    /// work on this window. `frame_count == 0` and an over-floor `undecodable` fail BOTH verdicts
    /// (never just `strict_pass`), so this can never true-positive on those — see
    /// `undecodable_over_floor_fails_both_verdicts_even_with_clean_copies_gaps_889` and
    /// `zero_frames_fails_both_verdicts_889` below.
    pub fn relaxed_by_889(&self) -> bool {
        !self.strict_pass && self.relaxed_pass
    }
}

/// Decide both verdicts for one window from its already-computed counts (`probe::
/// recording_segments::window_segment` supplies these — this function re-derives nothing about
/// frame contents, it only combines counts that are already known).
pub fn decide(frame_count: u32, undecodable: u32, copies: u32, gaps: u32) -> WindowGateDecision {
    let undecodable_ok = crate::optical_floor::window_within_floor(undecodable, frame_count);
    // #889 RED-stub: no relaxation yet — `relaxed_pass` still requires `copies == 0 && gaps == 0`,
    // identical to `strict_pass`. The GREEN commit drops that requirement from `relaxed_pass`.
    let relaxed_pass = frame_count > 0 && undecodable_ok && copies == 0 && gaps == 0;
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
    fn gaps_alone_fails_strict_but_passes_relaxed_889() {
        let d = decide(100, 0, 0, 2);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "#889: gaps alone must not fail the relaxed verdict: {d:?}"
        );
        assert!(d.relaxed_by_889());
    }

    #[test]
    fn copies_and_gaps_together_fail_strict_but_pass_relaxed_889() {
        let d = decide(100, 0, 2, 3);
        assert!(!d.strict_pass);
        assert!(
            d.relaxed_pass,
            "#889: neither term alone nor together may fail relaxed: {d:?}"
        );
        assert!(d.relaxed_by_889());
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
    fn undecodable_over_floor_fails_both_verdicts_even_with_clean_copies_gaps_889() {
        // #889 does not touch the #881 optical floor -- the run-wide/per-window undecodable term
        // stays strict regardless of copies/gaps.
        let d = decide(10, 5, 0, 0); // 5 undecodable of 10 frames -- past the #881 per-window floor (4)
        assert!(!d.strict_pass);
        assert!(
            !d.relaxed_pass,
            "undecodable over floor must still fail relaxed too: {d:?}"
        );
        assert!(
            !d.relaxed_by_889(),
            "not #889's doing -- the optical floor fails both: {d:?}"
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
