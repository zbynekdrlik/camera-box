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

/// How many rows of each frame [`frame_content_hash`] samples — a FEW rows spread evenly across the
/// height, not the whole frame. Mirrors `dupe_decimation::dupe_content_hash`'s (#889) row-sampling
/// cost discipline: a real grabber duplicate reproduces the frame byte-for-byte (sampled rows
/// included), and real content (sensor noise + motion) differs even within a small sampled subset,
/// so byte-exact equality over these rows alone is a reliable "same vs different" test at a
/// fraction of a full-frame hash's cost over a 54k-frame recording.
pub const CONTENT_HASH_SAMPLE_ROWS: usize = 8;

/// Row-sampled FNV-1a content fingerprint of a tightly-packed gray8 frame — the ENCODER half of
/// this metric, kept beside the classifier so the whole thing is self-contained and Tier-0
/// testable. `bytes` is `width * height` (gray8, tightly packed as ffmpeg's `-pix_fmt gray`
/// emits). Mirrors the proven approach of `dupe_decimation::dupe_content_hash` (#889): a fast,
/// deterministic fold for "same vs different" on real content, NOT a cryptographic hash —
/// collision RESISTANCE is irrelevant here (never adversarial), only exact-duplicate
/// discrimination. Two byte-identical frames hash equal; a degenerate (zero width/height) frame
/// hashes to a stable sentinel `0`. A local FNV-1a (no crate dependency) rather than a `std` hasher
/// so the value is stable across toolchain versions, the same reason #889 rolled its own.
pub fn frame_content_hash(bytes: &[u8], width: usize, height: usize) -> u64 {
    if width == 0 || height == 0 {
        return 0;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    let step = (height / CONTENT_HASH_SAMPLE_ROWS).max(1);
    let mut y = 0usize;
    while y < height {
        let row_start = y * width;
        let row_end = row_start + width;
        if row_end <= bytes.len() {
            for &b in &bytes[row_start..row_end] {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
            }
        }
        y += step;
    }
    hash
}

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

/// The worst (max) `duplicate_fraction` among ONLY the windows the classifier flagged as
/// `duplication_masked` — i.e. genuine pulldowns, NOT a localized freeze or an irregular burst
/// (which carry a HIGH raw `duplicate_fraction` but are deliberately vetoed by the coverage /
/// regularity checks). This is what [`dup_cadence_gate_pass`] must bound: gating on the raw worst
/// `duplicate_fraction` across ALL windows would double-jeopardy a freeze (already `frozen_leg`'s
/// domain) or a decode glitch, defeating the very discrimination the classifier builds. `None`
/// when no window is masked (no pulldown detected) — "not applicable", passes the gate.
pub fn worst_masked_duplicate_fraction(cadences: &[Option<DupCadence>]) -> Option<f64> {
    cadences
        .iter()
        .filter_map(|c| c.as_ref())
        .filter(|c| c.duplication_masked)
        .map(|c| c.duplicate_fraction)
        .fold(None::<f64>, |acc, f| Some(acc.map_or(f, |m| m.max(f))))
}

/// Does the run's worst DUPLICATION-MASKED duplicate fraction satisfy the `max` bound? Mirrors
/// [`crate::presentation_cadence::cadence_judder_gate_pass`] arm-for-arm (a per-window RATE, so a
/// single per-window-max term is honest — a real pulldown saturates every affected window, no
/// "spread the budget across windows" loophole). The `worst` argument MUST be
/// [`worst_masked_duplicate_fraction`] (the worst among windows classified `duplication_masked`),
/// NOT the raw worst across all windows — otherwise a freeze/glitch (high raw fraction, not a
/// pulldown) would trip this gate, double-jeopardying `frozen_leg`. The arms:
/// - `None` bound ⇒ report-only, always passes.
/// - `None` worst (no MASKED window ⇒ no pulldown detected) ⇒ PASS — "not applicable"; any
///   condition that would zero out every window is already hard-failed elsewhere (no
///   double-jeopardy).
/// - `Some` bound, `Some` worst ⇒ pass iff `worst <= bound` (strict `>`: a worst exactly at the
///   bound passes).
pub fn dup_cadence_gate_pass(
    worst_masked_duplicate_fraction: Option<f64>,
    max: Option<f64>,
) -> bool {
    match (max, worst_masked_duplicate_fraction) {
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

// ── #1101: signal-viability self-diagnosis ──────────────────────────────────────────────────────
//
// The #1088 surface reports a per-window `duplicate_fraction`, but an all-zero distribution is
// AMBIGUOUS: it means either "healthy rig, no pulldown" (promotable) OR "the content-hash signal
// is blind, sees nothing" (a false-green if gated). These fns DISAMBIGUATE the two by cross-checking
// the content-hash duplicates against the Vernier-tick copies the verdict already proves are present
// (a repeated tick = a byte-duplicate camera frame — the same signal `copies` counts). A signal that
// observes ~none of the tick-proven copies is structurally blind and MUST NOT be promoted to a LIVE
// gate. (#1101 measured 2 of 147 tick-copies observed on the lossy stream tap.)

/// Minimum tick-proven copies in a run before "zero content-hash duplicates" is CONCLUSIVE evidence
/// the content-hash signal is blind (below it → [`SignalViability::Indeterminate`], not a false
/// [`SignalViability::Blind`]).
pub const MIN_TICK_COPIES_FOR_VIABILITY: usize = 3;

/// Minimum fraction of tick-proven copies the content-hash must ALSO observe for the signal to be
/// [`SignalViability::Viable`] (able to see frame duplication at all). A precondition on gate-ability,
/// NOT a threshold on the defect.
pub const COPY_OBSERVATION_RATE_MIN: f64 = 0.5;

/// How well the per-frame content-hash duplicate signal TRACKS the ground-truth duplication the
/// Vernier-tick decoder proves is present. Built from parallel per-window (tick, content-hash)
/// sequences.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CopyObservation {
    /// Consecutive-frame pairs whose Vernier tick REPEATED — a tick-proven byte-duplicate camera
    /// frame (the ground truth the content-hash signal should also see). Only `Some`-tick pairs count.
    pub tick_copies: usize,
    /// Of those tick-copy pairs, how many ALSO had byte-identical content hashes — copies the
    /// content-hash signal actually observed.
    pub copies_observed_by_content_hash: usize,
    /// Total consecutive-frame pairs with byte-identical content hashes (incl. any not aligned to a
    /// tick-copy) — informational; the raw firing count of the content-hash signal.
    pub content_hash_duplicates: usize,
    /// `copies_observed_by_content_hash / tick_copies` — the observation RATE. `None` when there
    /// were no tick-copies (nothing to observe → not a judgement of the signal).
    pub copy_observation_rate: Option<f64>,
}

/// Build a [`CopyObservation`] from ONE window's parallel per-frame `ticks` and `content_hashes`
/// (index `i` is the same recorded frame in both; a `None` on either side never forms a duplicate).
/// The caller builds both aligned from the same window frames.
pub fn copy_observation(ticks: &[Option<u64>], content_hashes: &[Option<u64>]) -> CopyObservation {
    // RED STUB (#1101) — replaced with the real fold in the GREEN commit.
    let _ = (ticks, content_hashes);
    CopyObservation {
        tick_copies: 0,
        copies_observed_by_content_hash: 0,
        content_hash_duplicates: 0,
        copy_observation_rate: None,
    }
}

/// Fold per-window [`CopyObservation`]s into one run-level observation (sum the counts, recompute
/// the rate).
pub fn aggregate_copy_observations(observations: &[CopyObservation]) -> CopyObservation {
    // RED STUB (#1101) — replaced with the real fold in the GREEN commit.
    let _ = observations;
    CopyObservation {
        tick_copies: 0,
        copies_observed_by_content_hash: 0,
        content_hash_duplicates: 0,
        copy_observation_rate: None,
    }
}

/// Whether the content-hash duplication signal can be trusted to OBSERVE frame duplication — the
/// #1101 promotion-readiness precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalViability {
    /// The content-hash observed at least [`COPY_OBSERVATION_RATE_MIN`] of the tick-proven copies —
    /// it demonstrably tracks duplication (NECESSARY, not sufficient, for a LIVE flip).
    Viable,
    /// At least [`MIN_TICK_COPIES_FOR_VIABILITY`] tick-proven copies occurred but the content-hash
    /// observed fewer than [`COPY_OBSERVATION_RATE_MIN`] of them — structurally blind. NOT
    /// promotable: a LIVE gate would be a false-green.
    Blind,
    /// Fewer than [`MIN_TICK_COPIES_FOR_VIABILITY`] tick-proven copies — too little ground-truth
    /// duplication to judge. Not proven blind, not proven viable.
    Indeterminate,
}

/// Classify a run's aggregate [`CopyObservation`] into a [`SignalViability`].
pub fn signal_viability(observation: &CopyObservation) -> SignalViability {
    // RED STUB (#1101) — replaced with the real classification in the GREEN commit.
    let _ = observation;
    SignalViability::Viable
}

/// Whether the surface is eligible for a LIVE-gate promotion AT ALL — true ONLY when the signal is
/// [`SignalViability::Viable`]. The precondition [`gates_overall_pass`] must satisfy on real runs
/// before its one-line flip; on the current lossy stream tap it is `false` (#1101).
pub fn signal_promotable(viability: SignalViability) -> bool {
    matches!(viability, SignalViability::Viable)
}

/// #1112 — slice a carried per-frame content-hash vector into ONE cambox window's hash sequence,
/// in the window's own frame order, ready for [`measure_dup_cadence`].
///
/// `frame_indices` are the `RecordingFrame::frame_index` values of the frames the merge attributed
/// to this window (`partition_frames_by_window`), IN window order. `frame_hashes` is the full
/// per-recording content-hash vector the stream box computed during `--extract-partial stream` and
/// CARRIED in the partial (`RecordingPartial::content_hashes`), 0-based by `frame_index` — the same
/// index contract `probe::recording::hash_recording_frames` and the parallel decode both hold
/// (frame `i` ⇒ `frame_hashes[i]`). (Plain reference, not an intra-doc link: that item is behind
/// `#[cfg(feature = "probe")]`, unresolvable from this default-feature module — same as the
/// `dupe_decimation::dupe_content_hash` references above.)
///
/// This is the ONE genuinely new pure step of the #1112 emit-wiring — the on-box `hash_recording_frames`
/// / on-dev1 `partition_frames_by_window` sides are unchanged; this replaces the old inline
/// `win.iter().filter_map(|f| frame_hashes.get(f.frame_index as usize).copied())` in the merge so it
/// can be Tier-0 tested (no probe compile path exists for the merge consumer). A frame index at or
/// beyond `frame_hashes.len()` is SKIPPED, not defaulted — a hash gap must not manufacture a false
/// "duplicate" (two skipped frames would otherwise look identical); the resulting shorter sequence is
/// exactly what `measure_dup_cadence`'s own `MIN_SAMPLE_FRAMES` / fraction math already handles.
pub fn window_content_hashes(frame_indices: &[u64], frame_hashes: &[u64]) -> Vec<u64> {
    frame_indices
        .iter()
        .filter_map(|&idx| frame_hashes.get(idx as usize).copied())
        .collect()
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

    // ---- frame_content_hash (the encoder half) -----------------------------------------

    #[test]
    fn identical_frames_hash_equal() {
        // A grabber duplicate is byte-for-byte identical → its hash must match its predecessor's,
        // which is exactly what makes it counted as a duplicate downstream.
        let w = 64;
        let h = 32;
        let a: Vec<u8> = (0..(w * h)).map(|i| (i % 251) as u8).collect();
        let b = a.clone();
        assert_eq!(
            frame_content_hash(&a, w, h),
            frame_content_hash(&b, w, h),
            "byte-identical frames must hash equal"
        );
    }

    #[test]
    fn a_difference_in_a_sampled_row_changes_the_hash() {
        // Real content (sensor noise + motion) differs frame-to-frame; a change in a SAMPLED row
        // must move the hash so a genuinely-different frame is not miscounted as a duplicate.
        let w = 64;
        let h = 32;
        let a: Vec<u8> = vec![7u8; w * h];
        let mut b = a.clone();
        b[0] = 8; // row 0 is always sampled (y starts at 0)
        assert_ne!(
            frame_content_hash(&a, w, h),
            frame_content_hash(&b, w, h),
            "a sampled-row difference must change the hash"
        );
    }

    #[test]
    fn degenerate_dimensions_hash_to_a_stable_sentinel() {
        assert_eq!(frame_content_hash(&[1, 2, 3], 0, 32), 0);
        assert_eq!(frame_content_hash(&[1, 2, 3], 64, 0), 0);
    }

    #[test]
    fn a_short_buffer_never_panics() {
        // A truncated buffer (fewer bytes than width*height) must be handled, not panic — the
        // row-bounds guard skips rows that would read past the end.
        let _ = frame_content_hash(&[1, 2, 3, 4], 64, 32);
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

    // ---- worst_masked_duplicate_fraction (the gate feeds on the DISCRIMINATED signal) ---

    #[test]
    fn worst_masked_fraction_ignores_a_higher_raw_fraction_from_an_unmasked_window() {
        // A masked pulldown window sits BELOW a NON-masked freeze window in raw duplicate_fraction.
        // The gate must key on the pulldown (the real defect), never the freeze's higher raw rate —
        // otherwise it double-jeopardies frozen_leg. This is the whole point of the discrimination.
        let pulldown = measure_dup_cadence(&pulldown_hashes(120, 6)).expect("plenty");
        assert!(
            pulldown.duplication_masked,
            "pulldown is masked: {pulldown:?}"
        );
        let freeze_at: Vec<usize> = (25..=44).collect(); // 20 consecutive dups → localized freeze
        let freeze = measure_dup_cadence(&hashes_with_dups_at(60, &freeze_at)).expect("60 frames");
        assert!(
            !freeze.duplication_masked,
            "freeze is NOT masked: {freeze:?}"
        );
        assert!(
            freeze.duplicate_fraction > pulldown.duplicate_fraction,
            "the freeze has a HIGHER raw fraction than the pulldown: {} vs {}",
            freeze.duplicate_fraction,
            pulldown.duplicate_fraction
        );
        let clean = measure_dup_cadence(&smooth_hashes(60)).expect("60 frames");
        let cadences = vec![Some(clean), Some(freeze), Some(pulldown.clone())];
        assert_eq!(
            worst_masked_duplicate_fraction(&cadences),
            Some(pulldown.duplicate_fraction),
            "the worst MASKED fraction is the pulldown's, NOT the freeze's higher raw fraction"
        );
    }

    #[test]
    fn worst_masked_fraction_is_none_when_no_window_is_masked() {
        // No masked window (clean + a non-masked freeze + a None) ⇒ None ⇒ the gate passes.
        let clean = measure_dup_cadence(&smooth_hashes(60)).expect("60 frames");
        let freeze_at: Vec<usize> = (25..=44).collect();
        let freeze = measure_dup_cadence(&hashes_with_dups_at(60, &freeze_at)).expect("60 frames");
        let cadences = vec![Some(clean), Some(freeze), None];
        assert_eq!(worst_masked_duplicate_fraction(&cadences), None);
        assert!(
            dup_cadence_gate_pass(worst_masked_duplicate_fraction(&cadences), Some(0.10)),
            "no masked window ⇒ not applicable ⇒ passes"
        );
    }

    #[test]
    fn worst_masked_fraction_takes_the_max_across_multiple_masked_windows() {
        let slow = measure_dup_cadence(&pulldown_hashes(120, 6)).expect("plenty"); // ~16.7%
        let faster = measure_dup_cadence(&pulldown_hashes(120, 4)).expect("plenty"); // ~25%
        assert!(slow.duplication_masked && faster.duplication_masked);
        assert!(faster.duplicate_fraction > slow.duplicate_fraction);
        assert_eq!(
            worst_masked_duplicate_fraction(&[Some(slow), Some(faster.clone())]),
            Some(faster.duplicate_fraction),
            "the worst is the max across masked windows"
        );
    }

    // ---- the report-only gate seam (both directions) -----------------------------------

    #[test]
    fn default_constants_are_the_documented_first_cut_values() {
        // These are PRINCIPLED, UNCALIBRATED first cuts (see the module + const docs) — this test
        // only pins the documented values so a change is deliberate, it does NOT claim they are
        // calibrated against real runs.
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

    // ---- window_content_hashes (#1112 — the carry→slice glue) --------------------------

    #[test]
    fn window_content_hashes_picks_by_frame_index_not_position() {
        // The full per-recording hash vector (0-based by frame_index).
        let all: Vec<u64> = vec![100, 101, 102, 103, 104, 105, 106];
        // A window whose attributed frames are NON-contiguous, out of the natural 0..n order —
        // exactly what partition_frames_by_window produces when a cambox owns scattered frames.
        let idxs: Vec<u64> = vec![2, 5, 3];
        assert_eq!(
            window_content_hashes(&idxs, &all),
            vec![102, 105, 103],
            "each window frame's hash is looked up by its frame_index, in window order"
        );
    }

    #[test]
    fn window_content_hashes_contiguous_matches_direct_slice() {
        let all: Vec<u64> = (0..50).map(|x| x as u64 * 7 + 1).collect();
        let idxs: Vec<u64> = (10..20).collect();
        let got = window_content_hashes(&idxs, &all);
        assert_eq!(
            got,
            all[10..20].to_vec(),
            "contiguous window == the raw slice"
        );
    }

    #[test]
    fn window_content_hashes_skips_out_of_range_index() {
        // A frame_index past the end of the carried hash vector (a hash gap) must be DROPPED, not
        // defaulted — two dropped-then-defaulted frames would read as a false duplicate downstream.
        let all: Vec<u64> = vec![10, 11, 12];
        let idxs: Vec<u64> = vec![0, 9, 2, 100];
        assert_eq!(
            window_content_hashes(&idxs, &all),
            vec![10, 12],
            "indices 9 and 100 are out of range and skipped; only 0 and 2 survive, in order"
        );
    }

    #[test]
    fn window_content_hashes_empty_inputs_are_empty() {
        assert!(window_content_hashes(&[], &[1, 2, 3]).is_empty());
        assert!(window_content_hashes(&[0, 1, 2], &[]).is_empty());
    }

    #[test]
    fn window_content_hashes_feeds_measure_dup_cadence_unchanged() {
        // End-to-end: a carried pulldown hash vector, sliced 1:1 (window == whole recording),
        // yields the SAME DupCadence as feeding the raw vector — the slice is a faithful pass-through.
        let all = pulldown_hashes(120, 6);
        let idxs: Vec<u64> = (0..all.len() as u64).collect();
        let sliced = window_content_hashes(&idxs, &all);
        assert_eq!(sliced, all, "1:1 window reproduces the input exactly");
        assert_eq!(
            measure_dup_cadence(&sliced),
            measure_dup_cadence(&all),
            "the classifier sees an identical sequence through the slice"
        );
    }

    // ---- #1101 signal-viability self-diagnosis -----------------------------------------

    #[test]
    fn viability_constants_are_the_documented_values() {
        assert_eq!(MIN_TICK_COPIES_FOR_VIABILITY, 3);
        assert_eq!(COPY_OBSERVATION_RATE_MIN, 0.5);
    }

    #[test]
    fn copy_observation_counts_tick_copies_and_the_ones_the_hash_observed() {
        // 6 frames. Tick repeats at i=2 (2==2) and i=4 (3==3) → 2 tick-copies. The content hash
        // repeats ONLY at i=2 (a copy the hash observed); at i=4 the hash differs (a copy the hash
        // MISSED — the lossy-recording blindness this metric exists to surface).
        let ticks = [Some(1), Some(2), Some(2), Some(3), Some(3), Some(5)];
        let hashes = [
            Some(10),
            Some(20),
            Some(20), // i=2: tick copy AND hash dup → observed
            Some(30),
            Some(31), // i=4: tick copy but hash DIFFERS → missed
            Some(40),
        ];
        let obs = copy_observation(&ticks, &hashes);
        assert_eq!(obs.tick_copies, 2, "two repeated-tick pairs: {obs:?}");
        assert_eq!(
            obs.copies_observed_by_content_hash, 1,
            "only the i=2 copy was byte-identical: {obs:?}"
        );
        assert_eq!(
            obs.content_hash_duplicates, 1,
            "one consecutive equal-hash pair total: {obs:?}"
        );
        assert_eq!(
            obs.copy_observation_rate,
            Some(0.5),
            "observed 1 of 2 tick-copies: {obs:?}"
        );
    }

    #[test]
    fn copy_observation_none_tick_or_hash_never_forms_a_copy_or_dup() {
        // Two consecutive None ticks must NOT count as a tick-copy; two consecutive None hashes must
        // NOT count as a hash-dup (a decode/hash gap must not manufacture a false duplicate).
        let ticks = [None, None, Some(5), Some(5)];
        let hashes = [None, None, Some(9), Some(9)];
        let obs = copy_observation(&ticks, &hashes);
        assert_eq!(obs.tick_copies, 1, "only the Some(5),Some(5) pair: {obs:?}");
        assert_eq!(
            obs.copies_observed_by_content_hash, 1,
            "the Some(9),Some(9) coincides with the tick copy: {obs:?}"
        );
        assert_eq!(obs.content_hash_duplicates, 1, "{obs:?}");
    }

    #[test]
    fn copy_observation_no_tick_copies_yields_a_none_rate() {
        let ticks = [Some(1), Some(2), Some(3)];
        let hashes = [Some(1), Some(2), Some(3)];
        let obs = copy_observation(&ticks, &hashes);
        assert_eq!(obs.tick_copies, 0);
        assert_eq!(
            obs.copy_observation_rate, None,
            "no tick-copies ⇒ nothing to observe ⇒ None rate, not 0.0: {obs:?}"
        );
    }

    #[test]
    fn aggregate_sums_counts_and_recomputes_the_rate() {
        let a = CopyObservation {
            tick_copies: 4,
            copies_observed_by_content_hash: 0,
            content_hash_duplicates: 0,
            copy_observation_rate: Some(0.0),
        };
        let b = CopyObservation {
            tick_copies: 2,
            copies_observed_by_content_hash: 1,
            content_hash_duplicates: 1,
            copy_observation_rate: Some(0.5),
        };
        let agg = aggregate_copy_observations(&[a, b]);
        assert_eq!(agg.tick_copies, 6);
        assert_eq!(agg.copies_observed_by_content_hash, 1);
        assert_eq!(agg.content_hash_duplicates, 1);
        assert_eq!(
            agg.copy_observation_rate,
            Some(1.0 / 6.0),
            "run rate is recomputed from the summed counts, not averaged: {agg:?}"
        );
    }

    #[test]
    fn aggregate_of_empty_is_all_zero_none_rate() {
        let agg = aggregate_copy_observations(&[]);
        assert_eq!(agg.tick_copies, 0);
        assert_eq!(agg.copy_observation_rate, None);
    }

    #[test]
    fn viability_blind_when_copies_present_but_unobserved() {
        // The #1101 measured production state: many tick-proven copies, ~zero observed by the hash.
        let obs = CopyObservation {
            tick_copies: 68,
            copies_observed_by_content_hash: 0,
            content_hash_duplicates: 0,
            copy_observation_rate: Some(0.0),
        };
        assert_eq!(signal_viability(&obs), SignalViability::Blind);
        assert!(
            !signal_promotable(signal_viability(&obs)),
            "a blind signal must NOT be promotable — a LIVE gate would be a false-green"
        );
    }

    #[test]
    fn viability_indeterminate_below_the_min_copies_floor() {
        // Fewer than MIN_TICK_COPIES_FOR_VIABILITY copies, none observed → not enough ground truth.
        let obs = CopyObservation {
            tick_copies: 2,
            copies_observed_by_content_hash: 0,
            content_hash_duplicates: 0,
            copy_observation_rate: Some(0.0),
        };
        assert_eq!(signal_viability(&obs), SignalViability::Indeterminate);
        assert!(!signal_promotable(signal_viability(&obs)));
    }

    #[test]
    fn viability_indeterminate_when_no_copies_at_all() {
        let obs = CopyObservation {
            tick_copies: 0,
            copies_observed_by_content_hash: 0,
            content_hash_duplicates: 0,
            copy_observation_rate: None,
        };
        assert_eq!(signal_viability(&obs), SignalViability::Indeterminate);
    }

    #[test]
    fn viability_viable_when_the_hash_observes_enough_copies() {
        // A working (lossless-stage) signal: observes most tick-copies, clears the rate floor.
        let obs = CopyObservation {
            tick_copies: 10,
            copies_observed_by_content_hash: 9,
            content_hash_duplicates: 9,
            copy_observation_rate: Some(0.9),
        };
        assert_eq!(signal_viability(&obs), SignalViability::Viable);
        assert!(signal_promotable(signal_viability(&obs)));
    }

    #[test]
    fn viability_at_the_rate_floor_is_viable() {
        // Exactly at COPY_OBSERVATION_RATE_MIN passes (>= floor).
        let obs = CopyObservation {
            tick_copies: 4,
            copies_observed_by_content_hash: 2,
            content_hash_duplicates: 2,
            copy_observation_rate: Some(0.5),
        };
        assert_eq!(signal_viability(&obs), SignalViability::Viable);
    }

    #[test]
    fn viability_just_below_the_rate_floor_with_enough_copies_is_blind() {
        // >= MIN copies but observation below the floor → blind (the discriminating case).
        let obs = CopyObservation {
            tick_copies: 100,
            copies_observed_by_content_hash: 49,
            content_hash_duplicates: 49,
            copy_observation_rate: Some(0.49),
        };
        assert_eq!(signal_viability(&obs), SignalViability::Blind);
    }
}
