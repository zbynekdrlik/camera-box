//! #1088 — duplication-masked 50→60 source-cadence detector (per-frame content-hash dup-rate).
//!
//! The #794 hard layer. `src/cadence-health` (#794) reads strih's genlock-fifo `received=`
//! counter and pages when a camera's DELIVERED rate sits away from 60 fps. That covers a camera
//! genuinely delivering a non-60 NDI rate (50/43 fps → `received=` advances at 50/43 per second).
//! It is STRUCTURALLY BLIND to the case where a grabber upconverts a 50 fps source to 60 by frame
//! DUPLICATION (5:6 pulldown): the grabber delivers a padded genuine 60 NDI frames/s, so
//! `received=` reads a clean 60 and the receiver-side rate tap sees nothing — even though 1 in
//! every 6 delivered frames is an exact content-duplicate of the one before it and the motion
//! judders at the real 50 fps.
//!
//! The ONLY signal that survives the duplication is per-frame CONTENT identity. This module is the
//! PURE (Tier-0, default-features) classifier for that signal: given a sequence of per-frame
//! content HASHES in recorded (delivery) order, it counts exact consecutive duplicates and
//! decides whether the pattern is the sustained, REGULARLY-SPACED duplication of a pulldown (a
//! real cadence defect) as opposed to the isolated, irregular content-duplication that healthy
//! hardware already produces for unrelated reasons.
//!
//! ## Mirrors the crate-root verdict-gate seam pattern
//!
//! Like `presentation_cadence.rs` / `optical_floor.rs`, the WHOLE `probe` module is
//! `#[cfg(feature = "probe")]` (CI-only, never compiled/tested locally per CLAUDE.md's Local Build
//! Policy), so the PURE decision logic lives here at the crate root where it unit-tests on DEFAULT
//! features. The probe-gated glue (`bin/recording-verdict.rs`) computes the per-frame content hash
//! from the offline recording's decoded luma frames — reusing the proven row-sampled FNV-1a
//! approach of `dupe_decimation::dupe_content_hash` (#889) — slices the hash sequence per cambox
//! window, calls [`measure_dup_cadence`], and surfaces the result REPORT-ONLY.
//!
//! ## Why the hashing runs OFFLINE (the design fork resolved)
//!
//! The receiver-side rate tap is blind; hashing every frame on the LIVE strih/stream box would
//! perturb a broadcast render, and hashing on the cam-box side is a rig write out of scope for the
//! dev1-side read-only #794 family. The offline `recording-verdict` worker path already decodes
//! every recorded frame, so the hash is computed there — on the worker, once per verdict — which
//! is neither a rig write nor a live-box perturbation.
//!
//! ## Distinguishing a pulldown from the baseline
//!
//! Two independent duplication phenomena already sit BELOW the pulldown and must not be confused
//! with it:
//! - the free-running-clock beat baseline (`#674` measured ~4.3% on a ShadowCast), and
//! - the over-rate grabber's isolated dupes (`dupe_decimation.rs` #889: a ~64 fps grabber repeats
//!   its buffer ~1-in-15 ≈ 6.7%, always ISOLATED pairs, and the cam-box decimation gate already
//!   SHEDS them — so they never reach 60 fps padded).
//!
//! A 5:6 pulldown sits at ≈16.7% and — unlike either baseline — its duplicates are REGULARLY
//! spaced (one every ~6 frames). [`measure_dup_cadence`] classifies `duplication_masked` on BOTH a
//! rate floor ([`DUP_RATE_PULLDOWN_MIN`], above both baselines and below the pulldown) AND spacing
//! regularity ([`DUP_GAP_CV_MAX`]), so an anomalous BURST of irregular dupes is not misread as a
//! pulldown.
//!
//! ## Report-only (calibration-first)
//!
//! [`gates_overall_pass`] is `false`: the metric ships REPORT-ONLY. [`DUP_RATE_PULLDOWN_MIN`] is a
//! PRINCIPLED first-cut (above the two measured baselines, below the pulldown), not yet calibrated
//! against a real 50→60-grabber run (no such rig data exists) nor against the healthy-run offline
//! content-dup distribution (which needs this very surface to run first). The first real runs
//! calibrate it before any thought of gating — the same discipline as #1036 / #915.

/// Target canvas rate the source is padded UP to. A duplication-masked source runs at
/// `TARGET_FPS * (1 - duplicate_fraction)`; for a 5:6 pulldown that is `60 * (1 - 1/6) = 50`.
pub const TARGET_FPS: f64 = 60.0;

/// The rate floor above which a sustained content-duplicate fraction is treated as a candidate
/// pulldown. `0.10` sits above BOTH known baselines — the `#674` free-running beat (~0.043 on a
/// ShadowCast) and the `#889` over-rate grabber's isolated dupes (~0.067) — and comfortably below
/// the 5:6 pulldown's ≈0.167. A first-cut PRINCIPLED bound (report-only), to be tightened once the
/// healthy offline content-dup distribution is measured from real verdict runs.
pub const DUP_RATE_PULLDOWN_MIN: f64 = 0.10;

/// The maximum coefficient of variation (stddev/mean) of the inter-duplicate spacing for the
/// pattern to count as a REGULAR pulldown. A perfect 5:6 pulldown places a duplicate every 6
/// frames → cv = 0; real multi-hop jitter widens it. `0.35` is a generous first-cut regularity
/// bound (report-only) that still separates the evenly-spaced pulldown from an irregular burst of
/// dupes. Calibrate against real runs before gating.
pub const DUP_GAP_CV_MAX: f64 = 0.35;

/// The minimum number of frames a window must carry before a dup-rate reading is meaningful. Below
/// this there are too few consecutive pairs for a fraction to be trustworthy (the issue samples a
/// ~60-frame window); [`measure_dup_cadence`] returns `None` under it, never a spurious verdict.
pub const MIN_SAMPLE_FRAMES: usize = 24;

/// Per-window duplication-masked-cadence classification, built from a sequence of per-frame
/// content HASHES in recorded (delivery) order.
// #1088: carries `f64` fractions (no `Eq` impl — NaN) + a `Vec`, so this derives `PartialEq`/
// `Debug`/`Clone`/`Serialize` only, never `Copy`/`Eq`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DupCadence {
    /// Number of per-frame hashes evaluated (`hashes.len()`).
    pub sample_frames: usize,
    /// Consecutive-frame pairs compared (`sample_frames - 1`).
    pub compared_pairs: usize,
    /// Pairs whose two frames hashed byte-identical — an exact content duplicate of the prior
    /// delivered frame.
    pub exact_duplicates: usize,
    /// `exact_duplicates / compared_pairs`. ≈0.167 for a 5:6 pulldown, ~0.043 for the `#674` beat.
    pub duplicate_fraction: f64,
    /// Spacing (index delta) between each pair of CONSECUTIVE duplicate positions — the raw signal
    /// the regularity check consumes. A clean 5:6 pulldown is all `6`s.
    pub duplicate_gaps: Vec<usize>,
    /// Mean of `duplicate_gaps`. `None` when there are fewer than two duplicates (no gap exists).
    pub gap_mean: Option<f64>,
    /// Coefficient of variation (population stddev / mean) of `duplicate_gaps` — the regularity
    /// measure. `0` = perfectly evenly spaced (a true pulldown); large = irregular. `None` when
    /// there are fewer than two duplicates (no spacing to characterize).
    pub gap_cv: Option<f64>,
    /// The source rate this duplicate fraction implies against a 60 fps target
    /// (`TARGET_FPS * (1 - duplicate_fraction)`) — the operator-facing "the camera is really at
    /// N fps" number. ≈50 for a 5:6 pulldown.
    pub inferred_source_fps: f64,
    /// The classification: a SUSTAINED (`duplicate_fraction >= DUP_RATE_PULLDOWN_MIN`) AND
    /// REGULARLY-SPACED (`gap_cv <= DUP_GAP_CV_MAX`, with at least two duplicates to measure
    /// spacing) content-duplication pattern — the duplication-masked non-60 cadence this module
    /// exists to catch. `false` for the healthy baselines (below the rate floor) and for an
    /// irregular burst of dupes (over the cv bound).
    pub duplication_masked: bool,
}

/// Classify the duplication-masked cadence of `hashes` (per-frame content hashes in recorded
/// order).
///
/// Returns `None` when there is not enough data to say anything (`hashes.len() <
/// MIN_SAMPLE_FRAMES`). A caller treats `None` as "not applicable to this window", never a
/// failure — exactly like [`crate::presentation_cadence::measure_cadence_evenness`]'s `None`
/// contract.
pub fn measure_dup_cadence(_hashes: &[u64]) -> Option<DupCadence> {
    // #1088 RED stub — the real classification is implemented in the GREEN commit.
    None
}

/// Does the run's WORST per-window duplicate fraction satisfy the `max` bound? Mirrors
/// [`crate::presentation_cadence::cadence_judder_gate_pass`] arm-for-arm (a per-window RATE, so a
/// single per-window-max term is honest — the pulldown saturates every affected window, no
/// "spread the budget across windows" loophole):
/// - `None` bound ⇒ report-only, always passes.
/// - `None` worst (no window produced a dup-rate reading) ⇒ PASS — per [`measure_dup_cadence`]'s
///   `None` contract this is "not applicable", and a run with no readable window is already
///   hard-failed elsewhere (no double-jeopardy).
/// - `Some` bound, `Some` worst ⇒ pass iff `worst <= bound` (strict `>`: a worst exactly at the
///   bound passes).
pub fn dup_cadence_gate_pass(worst_duplicate_fraction: Option<f64>, max: Option<f64>) -> bool {
    match (max, worst_duplicate_fraction) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(bound), Some(worst)) => worst <= bound,
    }
}

/// #1088 report-only / restore seam — mirrors [`crate::optical_floor::gates_overall_pass`] /
/// [`crate::presentation_cadence::gates_overall_pass`]. Whether [`dup_cadence_gate_pass`]'s result
/// folds into the fused verdict's `overall_pass`. `false` today: the metric ships REPORT-ONLY (the
/// bound is an uncalibrated first-cut and the offline content-dup distribution is not yet measured
/// on real runs). Flip to `true` for a one-line promotion once calibrated against real runs.
pub fn gates_overall_pass() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a hash sequence of `n` frames with a duplicate inserted every `period` frames
    /// (a clean M:(M+1) pulldown when `period == M+1`), all other frames unique. Frame i's hash
    /// is a fresh unique value unless it duplicates its predecessor.
    fn pulldown_hashes(n: usize, period: usize) -> Vec<u64> {
        let mut out: Vec<u64> = Vec::with_capacity(n);
        let mut next: u64 = 1;
        for i in 0..n {
            if i > 0 && period > 0 && i % period == 0 {
                // duplicate the previous frame's content
                let prev = *out.last().unwrap();
                out.push(prev);
            } else {
                out.push(next);
                next += 1;
            }
        }
        out
    }

    /// All-unique frames (no duplicates at all) — a smooth true-60 source.
    fn smooth_hashes(n: usize) -> Vec<u64> {
        (0..n as u64).collect()
    }

    // ---- degenerate inputs -------------------------------------------------------------

    #[test]
    fn too_few_frames_returns_none() {
        assert_eq!(measure_dup_cadence(&[]), None);
        assert_eq!(measure_dup_cadence(&[1, 2, 3]), None);
        // exactly one under the floor is still None
        let just_under = smooth_hashes(MIN_SAMPLE_FRAMES - 1);
        assert_eq!(measure_dup_cadence(&just_under), None);
    }

    #[test]
    fn at_the_sample_floor_produces_a_reading() {
        let at_floor = smooth_hashes(MIN_SAMPLE_FRAMES);
        assert!(measure_dup_cadence(&at_floor).is_some());
    }

    // ---- the reference patterns --------------------------------------------------------

    #[test]
    fn smooth_true_60_source_has_zero_duplicates_and_is_not_masked() {
        let v = measure_dup_cadence(&smooth_hashes(60)).expect("60 frames is plenty");
        assert_eq!(v.sample_frames, 60);
        assert_eq!(v.compared_pairs, 59);
        assert_eq!(v.exact_duplicates, 0);
        assert_eq!(v.duplicate_fraction, 0.0);
        assert!(v.duplicate_gaps.is_empty());
        assert_eq!(v.gap_mean, None);
        assert_eq!(v.gap_cv, None);
        assert!((v.inferred_source_fps - 60.0).abs() < 1e-9);
        assert!(!v.duplication_masked, "a smooth 60 source is not masked: {v:?}");
    }

    #[test]
    fn five_to_six_pulldown_is_detected_as_duplication_masked() {
        // 5:6 pulldown → a duplicate every 6th frame → ~1/6 ≈ 0.167 duplicate fraction, all gaps
        // exactly 6 (perfectly regular) → the duplication-masked signature.
        let v = measure_dup_cadence(&pulldown_hashes(120, 6)).expect("120 frames is plenty");
        // duplicates land at indices 6,12,...,114 → 19 duplicates over 119 pairs.
        assert_eq!(v.exact_duplicates, 19, "one dup every 6 frames over 120: {v:?}");
        assert!(
            (v.duplicate_fraction - 19.0 / 119.0).abs() < 1e-9,
            "≈16% dup fraction: {v:?}"
        );
        assert!(
            v.duplicate_fraction > DUP_RATE_PULLDOWN_MIN,
            "the pulldown fraction must clear the rate floor: {v:?}"
        );
        // every gap between consecutive duplicate positions is exactly 6 → cv 0.
        assert!(
            v.duplicate_gaps.iter().all(|&g| g == 6),
            "clean pulldown gaps are all 6: {v:?}"
        );
        assert_eq!(v.gap_cv, Some(0.0), "perfectly regular spacing → cv 0: {v:?}");
        assert!(
            (v.inferred_source_fps - 60.0 * (1.0 - 19.0 / 119.0)).abs() < 1e-6,
            "inferred source fps ≈ 50: {v:?}"
        );
        assert!(
            v.inferred_source_fps > 49.0 && v.inferred_source_fps < 51.0,
            "5:6 pulldown implies ~50 fps source: {v:?}"
        );
        assert!(
            v.duplication_masked,
            "a regular 5:6 pulldown MUST classify as duplication-masked: {v:?}"
        );
    }

    #[test]
    fn free_running_beat_baseline_below_the_floor_is_not_masked() {
        // #674 ~4.3% baseline: a duplicate roughly every ~23 frames (1/23 ≈ 0.043) — a real
        // free-running-clock beat, NOT a pulldown. Even though the synthetic spacing here is
        // regular, the RATE alone sits below the floor, so it must NOT be flagged.
        let v = measure_dup_cadence(&pulldown_hashes(240, 23)).expect("plenty");
        assert!(
            v.duplicate_fraction < DUP_RATE_PULLDOWN_MIN,
            "the ~4.3% beat fraction sits below the rate floor: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "the free-running beat baseline must NOT be masked (below the rate floor): {v:?}"
        );
    }

    #[test]
    fn over_rate_isolated_dupes_889_below_the_floor_is_not_masked() {
        // #889 over-rate grabber: ~6.7% isolated dupes (~1 in 15). Still below the 10% floor.
        let v = measure_dup_cadence(&pulldown_hashes(300, 15)).expect("plenty");
        assert!(
            v.duplicate_fraction < DUP_RATE_PULLDOWN_MIN,
            "the #889 over-rate ~6.7% fraction sits below the rate floor: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "the #889 over-rate isolated dupes must NOT be masked: {v:?}"
        );
    }

    #[test]
    fn irregular_burst_above_the_floor_is_not_masked_by_the_regularity_gate() {
        // A high-rate (>10%) but IRREGULARLY spaced clump of duplicates — e.g. a decode glitch or
        // a genuine stall burst, NOT a steady pulldown. The rate floor alone would flag it; the
        // regularity (cv) bound must veto it because the spacing is not that of a pulldown.
        // Build: dense dupes clustered at the start, none later → wildly uneven gaps.
        let mut h: Vec<u64> = Vec::new();
        let mut next = 1u64;
        // first 20 frames: alternate unique/dup → many dupes, tight spacing
        for i in 0..20 {
            if i > 0 && i % 2 == 0 {
                h.push(*h.last().unwrap());
            } else {
                h.push(next);
                next += 1;
            }
        }
        // then 40 frames all unique → the late region has zero dupes → gaps blow out
        for _ in 0..40 {
            h.push(next);
            next += 1;
        }
        let v = measure_dup_cadence(&h).expect("60 frames");
        assert!(
            v.duplicate_fraction > DUP_RATE_PULLDOWN_MIN,
            "the clustered burst clears the rate floor: {v:?}"
        );
        assert!(
            v.gap_cv.map_or(true, |cv| cv > DUP_GAP_CV_MAX),
            "clustered irregular dupes have a high cv: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "an irregular high-rate burst must NOT be classified a pulldown: {v:?}"
        );
    }

    #[test]
    fn single_isolated_duplicate_is_not_masked() {
        // Exactly one duplicate in a long clean run: below the rate floor AND there is no gap to
        // measure regularity from (fewer than two dups → gap_cv None).
        let mut h = smooth_hashes(60);
        h[30] = h[29]; // one duplicate
        let v = measure_dup_cadence(&h).expect("60 frames");
        assert_eq!(v.exact_duplicates, 1);
        assert_eq!(v.gap_cv, None, "one dup has no inter-dup gap: {v:?}");
        assert!(!v.duplication_masked, "a single dup is never a pulldown: {v:?}");
    }

    // ---- the report-only gate seam (both directions) -----------------------------------

    #[test]
    fn default_constants_are_the_calibrated_values() {
        assert_eq!(DUP_RATE_PULLDOWN_MIN, 0.10);
        assert_eq!(DUP_GAP_CV_MAX, 0.35);
        assert_eq!(TARGET_FPS, 60.0);
    }

    #[test]
    fn gate_is_report_only_today() {
        assert!(
            !gates_overall_pass(),
            "#1088 ships REPORT-ONLY until calibrated against real runs"
        );
    }

    #[test]
    fn none_bound_is_report_only_always_passes() {
        assert!(dup_cadence_gate_pass(Some(0.9), None));
        assert!(dup_cadence_gate_pass(None, None));
    }

    #[test]
    fn no_window_reading_is_not_applicable_pass() {
        assert!(dup_cadence_gate_pass(None, Some(DUP_RATE_PULLDOWN_MIN)));
    }

    #[test]
    fn worst_below_bound_passes_over_bound_fails() {
        assert!(dup_cadence_gate_pass(Some(0.05), Some(DUP_RATE_PULLDOWN_MIN)));
        assert!(
            dup_cadence_gate_pass(Some(0.10), Some(0.10)),
            "exactly at the bound passes (strict >)"
        );
        assert!(!dup_cadence_gate_pass(Some(0.1667), Some(DUP_RATE_PULLDOWN_MIN)));
    }

    #[test]
    fn pulldown_fraction_end_to_end_fails_the_bound() {
        // Wire the real measured pulldown fraction into the gate: it must FAIL the rate bound.
        let v = measure_dup_cadence(&pulldown_hashes(120, 6)).expect("plenty");
        assert!(
            !dup_cadence_gate_pass(Some(v.duplicate_fraction), Some(DUP_RATE_PULLDOWN_MIN)),
            "the pulldown fraction ({}) must fail the bound",
            v.duplicate_fraction
        );
    }
}
