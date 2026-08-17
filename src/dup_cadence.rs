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
//! decides whether the pattern is the sustained, REGULARLY-SPACED, window-SPANNING duplication of
//! a pulldown (a real cadence defect) as opposed to the isolated, irregular, or LOCALIZED
//! content-duplication that healthy hardware (or an unrelated freeze) already produces.
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
//! ## Distinguishing a pulldown from the non-pulldown patterns
//!
//! Several other duplication patterns must NOT be confused with a 50→60 pulldown:
//! - the free-running-clock beat baseline (`#674` measured ~4.3% on a ShadowCast), and the
//!   over-rate grabber's isolated dupes (`dupe_decimation.rs` #889: a ~64 fps grabber repeats its
//!   buffer ~1-in-15 ≈ 6.7%, ISOLATED pairs, already SHED by the cam-box decimation gate) — both
//!   sit BELOW the pulldown RATE and are rejected by [`DUP_RATE_PULLDOWN_MIN`];
//! - a genuine content FREEZE / stall — a run of identical frames CONCENTRATED in one part of the
//!   window (frozen_leg's job, #895) — which can carry a high local rate but does NOT span the
//!   window, and is rejected by [`DUP_COVERAGE_MIN`];
//! - an irregular decode-glitch burst — high rate but UNEVENLY spaced — rejected by
//!   [`DUP_GAP_CV_MAX`].
//!
//! A 5:6 pulldown alone is all three at once: rate ≈16.7%, duplicates evenly spaced (one every ~6
//! frames), spread across the WHOLE window. [`measure_dup_cadence`] sets `duplication_masked` only
//! when the rate floor AND the spacing-regularity AND the window-coverage checks all hold.
//!
//! ## Report-only (calibration-first)
//!
//! [`gates_overall_pass`] is `false`: the metric ships REPORT-ONLY. The constants are PRINCIPLED
//! first-cuts (above the two measured baselines, below the pulldown), not yet calibrated against a
//! real 50→60-grabber run (no such rig data exists) nor against the healthy-run offline
//! content-dup distribution (which needs this very surface to run first). The first real runs
//! calibrate them before any thought of gating — the same discipline as #1036 / #915.

/// Target canvas rate the source is padded UP to. A duplication-masked source runs at
/// `TARGET_FPS * (1 - duplicate_fraction)`; for a 5:6 pulldown that is `60 * (1 - 1/6) = 50`.
pub const TARGET_FPS: f64 = 60.0;

/// The rate floor above which a sustained content-duplicate fraction is treated as a candidate
/// pulldown. `0.10` sits above BOTH known baselines — the `#674` free-running beat (~0.043 on a
/// ShadowCast) and the `#889` over-rate grabber's isolated dupes (~0.067) — and comfortably below
/// the 5:6 pulldown's ≈0.167. A first-cut PRINCIPLED bound (report-only), to be tightened once the
/// healthy offline content-dup distribution is measured from real verdict runs.
pub const DUP_RATE_PULLDOWN_MIN: f64 = 0.10;

/// The maximum coefficient of variation (population stddev / mean) of the inter-duplicate spacing
/// for the pattern to count as a REGULAR pulldown. A perfect 5:6 pulldown places a duplicate every
/// 6 frames → cv = 0; real multi-hop jitter widens it. `0.35` is a generous first-cut regularity
/// bound (report-only) that still separates the evenly-spaced pulldown from an irregular burst of
/// dupes. Calibrate against real runs before gating.
pub const DUP_GAP_CV_MAX: f64 = 0.35;

/// The minimum fraction of the window the duplicates must SPAN (`(last_dup - first_dup) /
/// (frames - 1)`) for the pattern to count as a pulldown. A pulldown spreads its dupes across the
/// WHOLE window (≈1.0); a localized FREEZE or a clustered glitch — even one with a high local rate
/// and evenly-spaced local dupes — covers only a slice and is rejected here. `0.5` is a first-cut
/// bound (report-only): a genuine pulldown on even the smallest sampled window still clears it.
pub const DUP_COVERAGE_MIN: f64 = 0.5;

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
    /// Fraction of the window the duplicates span: `(last_dup - first_dup) / (sample_frames - 1)`.
    /// ≈1.0 for a window-wide pulldown; small for a localized freeze/glitch. `0.0` when there are
    /// fewer than two duplicates.
    pub coverage: f64,
    /// The source rate this duplicate fraction implies against a 60 fps target
    /// (`TARGET_FPS * (1 - duplicate_fraction)`) — the operator-facing "the camera is really at
    /// N fps" number. ≈50 for a 5:6 pulldown.
    pub inferred_source_fps: f64,
    /// The classification: a SUSTAINED (`duplicate_fraction >= DUP_RATE_PULLDOWN_MIN`),
    /// REGULARLY-SPACED (`gap_cv <= DUP_GAP_CV_MAX`) AND window-SPANNING (`coverage >=
    /// DUP_COVERAGE_MIN`) content-duplication pattern, with at least two duplicates to characterize
    /// — the duplication-masked non-60 cadence this module exists to catch. `false` for the healthy
    /// baselines (below the rate floor), a localized freeze (below coverage), and an irregular
    /// burst (over the cv bound).
    pub duplication_masked: bool,
}

/// Classify the duplication-masked cadence of `hashes` (per-frame content hashes in recorded
/// order).
///
/// Returns `None` when there is not enough data to say anything (`hashes.len() <
/// MIN_SAMPLE_FRAMES`). A caller treats `None` as "not applicable to this window", never a
/// failure — exactly like [`crate::presentation_cadence::measure_cadence_evenness`]'s `None`
/// contract.
pub fn measure_dup_cadence(hashes: &[u64]) -> Option<DupCadence> {
    let sample_frames = hashes.len();
    if sample_frames < MIN_SAMPLE_FRAMES {
        return None;
    }
    let compared_pairs = sample_frames - 1;

    // Positions (index `i` in `1..n`) where frame `i` is byte-identical to its predecessor.
    let dup_positions: Vec<usize> = (1..sample_frames)
        .filter(|&i| hashes[i] == hashes[i - 1])
        .collect();
    let exact_duplicates = dup_positions.len();
    let duplicate_fraction = exact_duplicates as f64 / compared_pairs as f64;

    // Inter-duplicate spacing (regularity) — needs at least two duplicates to have any gap.
    let duplicate_gaps: Vec<usize> = dup_positions.windows(2).map(|w| w[1] - w[0]).collect();
    let (gap_mean, gap_cv) = if duplicate_gaps.is_empty() {
        (None, None)
    } else {
        let n = duplicate_gaps.len() as f64;
        let mean = duplicate_gaps.iter().map(|&g| g as f64).sum::<f64>() / n;
        let variance = duplicate_gaps
            .iter()
            .map(|&g| {
                let d = g as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / n; // population variance (the whole gap set, not a sample of it)
        let cv = if mean > 0.0 {
            variance.sqrt() / mean
        } else {
            0.0
        };
        (Some(mean), Some(cv))
    };

    // Coverage — how much of the window the duplicates span. A pulldown spreads across the whole
    // window; a localized freeze covers only a slice. `0.0` with fewer than two duplicates.
    let coverage = match (dup_positions.first(), dup_positions.last()) {
        (Some(&first), Some(&last)) if last > first => {
            (last - first) as f64 / compared_pairs as f64
        }
        _ => 0.0,
    };

    let inferred_source_fps = TARGET_FPS * (1.0 - duplicate_fraction);

    let regular = gap_cv.is_some_and(|cv| cv <= DUP_GAP_CV_MAX);
    let spans_window = coverage >= DUP_COVERAGE_MIN;
    let duplication_masked = exact_duplicates >= 2
        && duplicate_fraction >= DUP_RATE_PULLDOWN_MIN
        && regular
        && spans_window;

    Some(DupCadence {
        sample_frames,
        compared_pairs,
        exact_duplicates,
        duplicate_fraction,
        duplicate_gaps,
        gap_mean,
        gap_cv,
        coverage,
        inferred_source_fps,
        duplication_masked,
    })
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

    /// `n` unique frames, then force each index in `dup_at` to duplicate its predecessor. Lets a
    /// test place duplicates at arbitrary positions (for the irregular-spacing / freeze cases).
    fn hashes_with_dups_at(n: usize, dup_at: &[usize]) -> Vec<u64> {
        let mut h: Vec<u64> = (0..n as u64).map(|x| x + 1).collect();
        for &i in dup_at {
            assert!(i >= 1 && i < n, "dup index in range");
            h[i] = h[i - 1];
        }
        h
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
        assert_eq!(v.coverage, 0.0);
        assert!((v.inferred_source_fps - 60.0).abs() < 1e-9);
        assert!(
            !v.duplication_masked,
            "a smooth 60 source is not masked: {v:?}"
        );
    }

    #[test]
    fn five_to_six_pulldown_is_detected_as_duplication_masked() {
        // 5:6 pulldown → a duplicate every 6th frame → ~1/6 ≈ 0.167 duplicate fraction, all gaps
        // exactly 6 (perfectly regular), spread across the whole window → the masked signature.
        let v = measure_dup_cadence(&pulldown_hashes(120, 6)).expect("120 frames is plenty");
        // duplicates land at indices 6,12,...,114 → 19 duplicates over 119 pairs.
        assert_eq!(
            v.exact_duplicates, 19,
            "one dup every 6 frames over 120: {v:?}"
        );
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
        assert_eq!(
            v.gap_cv,
            Some(0.0),
            "perfectly regular spacing → cv 0: {v:?}"
        );
        assert!(
            v.coverage >= DUP_COVERAGE_MIN,
            "the pulldown spans the window: {v:?}"
        );
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
            "a regular window-wide 5:6 pulldown MUST classify as duplication-masked: {v:?}"
        );
    }

    #[test]
    fn free_running_beat_baseline_below_the_floor_is_not_masked() {
        // #674 ~4.3% baseline: a duplicate roughly every ~23 frames (1/23 ≈ 0.043) — a real
        // free-running-clock beat, NOT a pulldown. Even though the synthetic spacing here is
        // regular and window-wide, the RATE alone sits below the floor, so it must NOT be flagged.
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
    fn localized_freeze_run_above_the_floor_is_not_masked_by_coverage() {
        // A genuine FREEZE: a RUN of ~12 consecutive identical frames concentrated in one region
        // of a 60-frame window. Local rate clears the floor and the (all-1) inter-dup gaps are
        // perfectly regular, but the dupes cover only a SLICE of the window — a freeze (frozen_leg's
        // domain, #895), not a pulldown. The COVERAGE bound must veto it.
        let dup_at: Vec<usize> = (25..=36).collect(); // 12 consecutive dups
        let v = measure_dup_cadence(&hashes_with_dups_at(60, &dup_at)).expect("60 frames");
        assert!(
            v.duplicate_fraction > DUP_RATE_PULLDOWN_MIN,
            "the freeze run clears the rate floor: {v:?}"
        );
        assert_eq!(
            v.gap_cv,
            Some(0.0),
            "a consecutive run has perfectly regular (all-1) inter-dup gaps: {v:?}"
        );
        assert!(
            v.coverage < DUP_COVERAGE_MIN,
            "a localized freeze covers only a slice of the window: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "a localized freeze must NOT be classified a pulldown (coverage veto): {v:?}"
        );
    }

    #[test]
    fn irregular_burst_across_window_is_not_masked_by_the_regularity_gate() {
        // High-rate dupes that DO span the window but are UNEVENLY spaced — a decode-glitch burst,
        // not a steady pulldown. Coverage passes; the spacing regularity (cv) bound must veto it.
        let dup_at = [6usize, 12, 30, 31, 40, 55, 56]; // spans 6..56, but gaps 6,18,1,9,15,1 vary
        let v = measure_dup_cadence(&hashes_with_dups_at(60, &dup_at)).expect("60 frames");
        assert!(
            v.duplicate_fraction >= DUP_RATE_PULLDOWN_MIN,
            "the burst clears the rate floor: {v:?}"
        );
        assert!(
            v.coverage >= DUP_COVERAGE_MIN,
            "the burst spans the window (coverage would pass): {v:?}"
        );
        assert!(
            v.gap_cv.is_some_and(|cv| cv > DUP_GAP_CV_MAX),
            "unevenly-spaced dupes have a high cv: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "an irregular high-rate burst must NOT be classified a pulldown (cv veto): {v:?}"
        );
    }

    #[test]
    fn single_isolated_duplicate_is_not_masked() {
        // Exactly one duplicate in a long clean run: below the rate floor AND there is no gap to
        // measure regularity from (fewer than two dups → gap_cv None, coverage 0).
        let v = measure_dup_cadence(&hashes_with_dups_at(60, &[30])).expect("60 frames");
        assert_eq!(v.exact_duplicates, 1);
        assert_eq!(v.gap_cv, None, "one dup has no inter-dup gap: {v:?}");
        assert_eq!(v.coverage, 0.0, "one dup spans nothing: {v:?}");
        assert!(
            !v.duplication_masked,
            "a single dup is never a pulldown: {v:?}"
        );
    }

    #[test]
    fn a_faster_pulldown_ratio_is_also_detected_and_infers_a_lower_fps() {
        // A more aggressive pulldown (dup every 4th frame ≈ 25% → a 3:4 pulldown, ~45 fps source):
        // still regular, window-wide, well over the floor → masked, with a lower inferred fps.
        let v = measure_dup_cadence(&pulldown_hashes(120, 4)).expect("plenty");
        assert!(
            v.duplicate_fraction > 0.2,
            "aggressive pulldown rate: {v:?}"
        );
        assert!(
            v.duplication_masked,
            "a regular 3:4 pulldown is masked: {v:?}"
        );
        assert!(
            v.inferred_source_fps < 50.0,
            "a faster dup rate infers a lower source fps: {v:?}"
        );
    }

    // ---- the report-only gate seam (both directions) -----------------------------------

    #[test]
    fn default_constants_are_the_calibrated_values() {
        assert_eq!(DUP_RATE_PULLDOWN_MIN, 0.10);
        assert_eq!(DUP_GAP_CV_MAX, 0.35);
        assert_eq!(DUP_COVERAGE_MIN, 0.5);
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
        assert!(dup_cadence_gate_pass(
            Some(0.05),
            Some(DUP_RATE_PULLDOWN_MIN)
        ));
        assert!(
            dup_cadence_gate_pass(Some(0.10), Some(0.10)),
            "exactly at the bound passes (strict >)"
        );
        assert!(!dup_cadence_gate_pass(
            Some(0.1667),
            Some(DUP_RATE_PULLDOWN_MIN)
        ));
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
