//! #1088 — duplication-masked 50→60 source-cadence detector (per-frame near-duplicate cadence).
//!
//! The #794 hard layer. `src/cadence-health` (#794) reads strih's genlock-fifo `received=`
//! counter and pages when a camera's DELIVERED rate sits away from 60 fps. That covers a camera
//! genuinely delivering a non-60 NDI rate (50/43 fps → `received=` advances at 50/43 per second).
//! It is STRUCTURALLY BLIND to the case where a grabber upconverts a 50 fps source to 60 by frame
//! DUPLICATION (5:6 pulldown): the grabber delivers a padded genuine 60 NDI frames/s, so
//! `received=` reads a clean 60 and the receiver-side rate tap sees nothing — even though 1 in
//! every 6 delivered frames is a content-duplicate of the one before it and the motion judders at
//! the real 50 fps.
//!
//! ## #1166 — the signal is a CODEC-TOLERANT near-duplicate, NOT a byte-exact hash
//!
//! The ONLY signal that survives the duplication is per-frame CONTENT identity — but the #1088/#1112
//! first cut hashed each recorded frame with a BYTE-EXACT row-sampled FNV-1a, computed on the STREAM
//! box's LOSSY `.mp4`. Byte-exact frame identity does NOT survive lossy H.264 encode+decode
//! (inter-prediction residuals + quantization make every decoded frame byte-UNIQUE), so a genuine
//! duplicate camera frame is not byte-identical after the recording round-trips. #1101 measured this
//! live: across 18 production verdicts + their stream partials, 147 tick-proven copies produced only
//! 2 byte-exact content-hash duplicates (≈1.4%) — the byte-exact signal was structurally BLIND, and
//! `signal_viability` correctly read `Blind` (a LIVE gate on it would be a permanent false-green).
//!
//! #1166 FIXES the signal: the duplicate test is now a codec-tolerant NEAR-duplicate — a per-frame
//! row-sampled mean-abs-luma-DIFFERENCE (MAD) to the recording predecessor, thresholded at
//! [`NEAR_DUP_MAD_MAX`]. A byte-duplicate source frame survives the lossy encode as a LOW-MAD pair
//! (only global quantization noise between the two), while genuine motion moves image content and
//! produces a far higher MAD. Validated on the retained REAL lossy diagnostic frame PNGs (32
//! tick-proven copy pairs across 16 runs vs 381 genuine-motion pairs): at `MAD <= 10.0` the signal
//! observes 26/32 = 81% of the tick-proven copies with 0/381 = 0.0% false positives on genuine
//! motion — where the byte-exact hash observed 0/32. DOWNSCALED thumbnails destroy the separation
//! (averaging washes out localised motion); full-WIDTH row-sampled MAD preserves the full-resolution
//! separation at a fraction of the cost, so the MAD is computed on the box between consecutive
//! full-resolution decoded frames (see `probe::recording::frame_prev_diffs`) and carried per frame.
//!
//! ## Mirrors the crate-root verdict-gate seam pattern
//!
//! Like `presentation_cadence.rs` / `optical_floor.rs`, the WHOLE `probe` module is
//! `#[cfg(feature = "probe")]` (CI-only, never compiled/tested locally per CLAUDE.md's Local Build
//! Policy), so the PURE decision logic lives here at the crate root where it unit-tests on DEFAULT
//! features. The probe-gated glue (`bin/recording-verdict.rs`) computes the per-frame MAD-to-prev
//! from the offline recording's decoded luma frames, carries the per-frame vector in the partial,
//! slices it per cambox window ([`window_prev_mads`]), calls [`measure_dup_cadence`], and surfaces
//! the result REPORT-ONLY.
//!
//! ## Why the diffing runs OFFLINE (the design fork resolved)
//!
//! The receiver-side rate tap is blind; diffing every frame on the LIVE strih/stream box would
//! perturb a broadcast render, and diffing on the cam-box side is a rig write out of scope for the
//! dev1-side read-only #794 family. The offline `recording-verdict` worker path already decodes
//! every recorded frame, so the MAD is computed there — on the worker, once per verdict — which is
//! neither a rig write nor a live-box perturbation.
//!
//! ## Distinguishing a pulldown from the non-pulldown patterns
//!
//! Several other duplication patterns must NOT be confused with a 50→60 pulldown:
//! - the free-running-clock beat baseline (`#674` measured ~4.3% on a ShadowCast), and the
//!   over-rate grabber's isolated dupes (`dupe_decimation.rs` #889: a ~64 fps grabber repeats its
//!   buffer ~1-in-15 ≈ 6.7%, ISOLATED pairs, already SHED by the cam-box decimation gate) — both
//!   sit BELOW the pulldown RATE and are rejected by [`DUP_RATE_PULLDOWN_MIN`];
//! - a genuine content FREEZE / stall — a run of near-identical frames CONCENTRATED in one part of
//!   the window (frozen_leg's job, #895) — which can carry a high local rate but does NOT span the
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
//! [`gates_overall_pass`] is `false`: the metric ships REPORT-ONLY. The #1166 near-duplicate signal
//! is validated (Viable) on a BIASED sample of retained diagnostic PNGs, not on a full green run,
//! and [`DUP_RATE_PULLDOWN_MIN`] is still a PRINCIPLED first-cut (above the two measured baselines,
//! below the pulldown) — not yet calibrated against a real 50→60-grabber run nor against the
//! healthy full-run near-duplicate distribution. The LIVE-flip precondition is therefore
//! [`signal_promotable`]([`signal_viability`]) == `true` on ≥2 consecutive real runs AND a
//! recalibrated bound — the same discipline as #1036 / #915.

/// Target canvas rate the source is padded UP to. A duplication-masked source runs at
/// `TARGET_FPS * (1 - duplicate_fraction)`; for a 5:6 pulldown that is `60 * (1 - 1/6) = 50`.
pub const TARGET_FPS: f64 = 60.0;

/// The rate floor above which a sustained near-duplicate fraction is treated as a candidate
/// pulldown. `0.10` sits above BOTH known baselines — the `#674` free-running beat (~0.043 on a
/// ShadowCast) and the `#889` over-rate grabber's isolated dupes (~0.067) — and comfortably below
/// the 5:6 pulldown's ≈0.167. A first-cut PRINCIPLED bound (report-only), to be recalibrated once
/// the healthy full-run near-duplicate distribution is measured from real verdict runs (#1166).
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

/// #1166 — the near-duplicate threshold on the per-pair row-sampled mean-abs-luma-difference (MAD):
/// a consecutive frame pair whose [`frame_row_sampled_mad`] is `<= NEAR_DUP_MAD_MAX` is a
/// CONTENT NEAR-DUPLICATE (a byte-duplicate source frame survives the lossy encode as a low-MAD
/// pair — only global quantization noise between the two). Calibrated from the retained real lossy
/// diagnostic frame PNGs: 32 tick-proven copy pairs cluster at MAD [1.37, 20.34] (median ~7.4)
/// while 381 genuine-motion pairs sit at [10.79, 36.25] (median ~27); `10.0` observes 26/32 = 81%
/// of the tick-proven copies with 0/381 = 0.0% motion false-positives — the tight-green,
/// zero-false-positive point below the genuine-motion floor. A FIRST-CUT bound (report-only) on a
/// BIASED PNG-dump sample; the full-run recalibration + the LIVE flip are gated on `signal_promotable`
/// over real runs (#1166).
pub const NEAR_DUP_MAD_MAX: f64 = 10.0;

/// How many rows of each frame [`frame_row_sampled_mad`] samples — a set of full-WIDTH rows spread
/// evenly across the height, NOT a downscale. Downscaling AVERAGES away the fine spatial detail that
/// distinguishes localised genuine motion from a duplicate's global quantization noise (measured:
/// 8×8/16×16 thumbnail MAD ranges OVERLAP copy-vs-motion, destroying the separation); full-width
/// row-sampling keeps each sampled row at full horizontal resolution, so it preserves the
/// full-resolution copy/motion separation at ~6% of the pixel cost over a 54k-frame recording.
pub const MAD_SAMPLE_ROWS: usize = 64;

/// Row-sampled mean-abs-luma-DIFFERENCE between two tightly-packed gray8 frames — the ENCODER half
/// of this metric (#1166), kept beside the classifier so the whole thing is self-contained and
/// Tier-0 testable. `prev`/`cur` are each `width * height` (gray8, tightly packed as ffmpeg's
/// `-pix_fmt gray` emits). Samples a set of full-WIDTH rows spread evenly across the height
/// ([`MAD_SAMPLE_ROWS`]) and returns the mean absolute per-pixel luma difference over them. Two
/// byte-identical frames → `0.0` (correctly a near-duplicate — that IS the signal). The two
/// no-comparable-data degenerate cases also return `0.0`: a zero width/height frame, or one where no
/// sampled row is fully present in both buffers. Neither can arise in production — the producer
/// (`probe::recording::frame_prev_diffs`) only ever passes two FULL `width*height` buffers of a
/// non-zero-dimension recording: `read_frames` reads by `read_exact`, so a truncated tail is dropped
/// (`UnexpectedEof`), never delivered as a short frame. (Were a short/degenerate frame ever passed
/// for a recording-adjacent in-range position, its `0.0` WOULD count as a near-duplicate — this is a
/// no-op guard against a caller that does not exist, not a case the window's None-gating catches.)
/// Computed on the box between consecutive decoded frames; carried per frame and thresholded in the
/// merge.
pub fn frame_row_sampled_mad(prev: &[u8], cur: &[u8], width: usize, height: usize) -> f64 {
    if width == 0 || height == 0 {
        return 0.0;
    }
    // A set of full-WIDTH rows spread evenly across the height (NOT a downscale — see
    // MAD_SAMPLE_ROWS): mirrors the row-selection of the retired `frame_content_hash` (step =
    // height/rows, y = 0, step, 2*step, …).
    let step = (height / MAD_SAMPLE_ROWS).max(1);
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    let mut y = 0usize;
    while y < height {
        let row_start = y * width;
        let row_end = row_start + width;
        // Only sample a row fully present in BOTH buffers (a truncated trailing/short frame skips
        // that row rather than panicking or comparing past the end).
        if row_end <= prev.len() && row_end <= cur.len() {
            for x in row_start..row_end {
                sum += u64::from((i32::from(prev[x]) - i32::from(cur[x])).unsigned_abs());
                count += 1;
            }
        }
        y += step;
    }
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

/// #1166 — whether a per-pair row-sampled [`frame_row_sampled_mad`] counts as a content
/// NEAR-DUPLICATE (`mad <= NEAR_DUP_MAD_MAX`). A precondition helper the classifier + the
/// viability cross-check share, so both apply the SAME threshold to the SAME per-window sequence.
pub fn is_near_duplicate(mad: f64) -> bool {
    mad <= NEAR_DUP_MAD_MAX
}

/// Per-window duplication-masked-cadence classification, built from a sequence of per-frame
/// near-duplicate signals (MAD-to-window-predecessor) in recorded (delivery) order.
// #1088: carries `f64` fractions (no `Eq` impl — NaN) + a `Vec`, so this derives `PartialEq`/
// `Debug`/`Clone`/`Serialize` only, never `Copy`/`Eq`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DupCadence {
    /// Number of per-frame samples evaluated (the window's frame count).
    pub sample_frames: usize,
    /// Consecutive-frame pairs compared (`sample_frames - 1`).
    pub compared_pairs: usize,
    /// #1166 — pairs whose MAD-to-predecessor was `<= NEAR_DUP_MAD_MAX` (a codec-tolerant content
    /// near-duplicate of the prior delivered frame). Renamed from the byte-exact `exact_duplicates`.
    pub near_duplicates: usize,
    /// `near_duplicates / compared_pairs`. ≈0.167 for a 5:6 pulldown, ~0.043 for the `#674` beat.
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
    /// DUP_COVERAGE_MIN`) content near-duplication pattern, with at least two duplicates to
    /// characterize — the duplication-masked non-60 cadence this module exists to catch. `false` for
    /// the healthy baselines (below the rate floor), a localized freeze (below coverage), and an
    /// irregular burst (over the cv bound).
    pub duplication_masked: bool,
}

/// Classify the duplication-masked cadence of `prev_mads` — the per-window sequence of per-frame
/// near-duplicate signals in recorded order. Position `i` is `Some(mad)` when frame `i` is
/// recording-adjacent to its window-predecessor (`i-1`) and its MAD is known, else `None` (position
/// 0, a decode gap, or a non-adjacent window boundary — none of which can be a duplicate). Built by
/// [`window_prev_mads`] from the carried per-frame MAD vector.
///
/// Returns `None` when there is not enough data to say anything (`prev_mads.len() <
/// MIN_SAMPLE_FRAMES`). A caller treats `None` as "not applicable to this window", never a
/// failure — exactly like [`crate::presentation_cadence::measure_cadence_evenness`]'s `None`
/// contract.
pub fn measure_dup_cadence(prev_mads: &[Option<f64>]) -> Option<DupCadence> {
    let sample_frames = prev_mads.len();
    if sample_frames < MIN_SAMPLE_FRAMES {
        return None;
    }
    let compared_pairs = sample_frames - 1;

    // Positions (index `i` in `1..n`) where frame `i` is a content near-duplicate of its
    // window-predecessor (`Some` MAD at or below the near-dup threshold). A `None` (gap / first
    // frame / non-adjacent boundary) is never a duplicate.
    let dup_positions: Vec<usize> = (1..sample_frames)
        .filter(|&i| prev_mads[i].is_some_and(is_near_duplicate))
        .collect();
    let near_duplicates = dup_positions.len();
    let duplicate_fraction = near_duplicates as f64 / compared_pairs as f64;

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
    let duplication_masked = near_duplicates >= 2
        && duplicate_fraction >= DUP_RATE_PULLDOWN_MIN
        && regular
        && spans_window;

    Some(DupCadence {
        sample_frames,
        compared_pairs,
        near_duplicates,
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
/// folds into the fused verdict's `overall_pass`. `false` today: the metric ships REPORT-ONLY.
///
/// **#1166 signal fix — the flip is still NOT merely a threshold change.** #1101 proved the OLD
/// byte-exact tap was [`SignalViability::Blind`] (observed 2 of 147 tick-proven copies). #1166
/// replaces it with the codec-tolerant near-duplicate MAD signal (validated on the retained real
/// lossy PNGs: 81% of tick-proven copies observed, 0% motion false-positives), which turns the
/// viability cross-check toward `Viable`. But the flip stays blocked because (1) that validation is
/// on a BIASED PNG-dump sample, not a full green run; (2) [`DUP_RATE_PULLDOWN_MIN`] is still a
/// principled first-cut, not calibrated against the new signal's healthy full-run distribution; and
/// (3) the precondition is [`signal_promotable`]([`signal_viability`]) == `true` on ≥2 consecutive
/// REAL runs emitting the new signal (the existing partials carry the old byte-exact hashes, so they
/// cannot supply it). Until a fresh green run shows all three, this stays `false`.
pub fn gates_overall_pass() -> bool {
    false
}

// ── #1101 (signal fix #1166): signal-viability self-diagnosis ────────────────────────────────────
//
// The #1088 surface reports a per-window `duplicate_fraction`, but an all-zero distribution is
// AMBIGUOUS: it means either "healthy rig, no pulldown" (promotable) OR "the content signal is
// blind, sees nothing" (a false-green if gated). These fns DISAMBIGUATE the two by cross-checking
// the content near-duplicate signal against Vernier-tick copies over the SAME consecutive-frame
// basis: a STRICT-ADJACENT tick repeat (frame `i` and `i-1` BOTH decoded and equal-ticked) is a
// tick-proven byte-duplicate camera frame. This is a SUBSET of the canonical `copies` metric
// (`probe::recording_segments`), which additionally bridges an undecodable gap between two equal
// ticks (its `prev_recorded` skips `None`), so `tick_copies` here is `<=` canonical `copies`; the
// strict definition is the right one for a consecutive-pair cross-check against the near-duplicate
// signal (both sides on the same adjacency basis), and it is conservative (an undercount only ever
// yields MORE `Indeterminate`, never a false `Viable`). A signal that observes ~none of the
// tick-proven copies is structurally blind and MUST NOT be promoted to a LIVE gate. (#1101 measured
// 2 of 147 observed on the BYTE-EXACT lossy tap; #1166 measured 26 of 32 = 81% observed with the
// near-duplicate MAD signal on the retained real lossy frame PNGs.)

/// Minimum tick-proven copies in a run before "zero content near-duplicates" is CONCLUSIVE evidence
/// the content signal is blind (below it → [`SignalViability::Indeterminate`], not a false
/// [`SignalViability::Blind`]).
pub const MIN_TICK_COPIES_FOR_VIABILITY: usize = 3;

/// Minimum fraction of tick-proven copies the content signal must ALSO observe for it to be
/// [`SignalViability::Viable`] (able to see frame duplication at all). A precondition on gate-ability,
/// NOT a threshold on the defect.
pub const COPY_OBSERVATION_RATE_MIN: f64 = 0.5;

/// How well the per-frame content near-duplicate signal TRACKS the ground-truth duplication the
/// Vernier-tick decoder proves is present. Built from parallel per-window (tick, near-dup MAD)
/// sequences.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CopyObservation {
    /// Consecutive-frame pairs whose Vernier tick REPEATED — a tick-proven byte-duplicate camera
    /// frame (the ground truth the content signal should also see). Counted ONLY over pairs the
    /// content signal COULD observe, i.e. RECORDING-ADJACENT ones (`prev_mads[i].is_some()` — the
    /// exact adjacency `window_prev_mads` gates the MAD on), so this is the honest DENOMINATOR for
    /// the observation rate: a tick-copy across a within-window attribution gap (which the MAD side
    /// can never see) does not inflate it. Only `Some`-tick pairs count.
    pub tick_copies: usize,
    /// Of those tick-copy pairs, how many ALSO registered a content NEAR-duplicate (MAD ≤
    /// [`NEAR_DUP_MAD_MAX`]) — copies the content signal actually observed.
    pub copies_observed_by_content: usize,
    /// Total consecutive-frame pairs that registered a content near-duplicate (incl. any not aligned
    /// to a tick-copy) — informational; the raw firing count of the content signal. NOTE: computed
    /// over the SAME None-padded, position-aligned per-window sequence ([`window_prev_mads`]) that
    /// [`measure_dup_cadence`]'s `duplicate_fraction` consumes — the #1101 review's content/duplicate
    /// sequence-mismatch is resolved by feeding BOTH from that one sequence. Both are report-only.
    pub content_near_dup_pairs: usize,
    /// `copies_observed_by_content / tick_copies` — the observation RATE. `None` when there were no
    /// tick-copies (nothing to observe → not a judgement of the signal).
    pub copy_observation_rate: Option<f64>,
}

/// Build a [`CopyObservation`] from ONE window's parallel per-frame `ticks` and near-duplicate
/// `prev_mads` (index `i` is the same recorded frame in both; a `None` on either side never forms a
/// duplicate). The caller builds both aligned from the same window frames (`prev_mads` via
/// [`window_prev_mads`], so its near-dup positions match `measure_dup_cadence`'s exactly).
///
/// A tick-copy is counted ONLY where `prev_mads[i].is_some()` — i.e. the pair is RECORDING-ADJACENT
/// (the exact gate `window_prev_mads` applies: `Some` at position `i` iff frames `i-1`,`i` are
/// consecutive in the recording and in range). This puts BOTH sides of the cross-check on the SAME
/// pair basis, so `copy_observation_rate` measures "of the copies the content signal COULD observe,
/// how many did it" — a tick-copy across a within-window attribution gap (which the MAD side can
/// never see, its `prev_mads` there being `None`) is excluded from BOTH numerator and denominator,
/// never depressing the rate below what the signal can actually achieve.
pub fn copy_observation(ticks: &[Option<u64>], prev_mads: &[Option<f64>]) -> CopyObservation {
    let n = ticks.len().min(prev_mads.len());
    let mut tick_copies = 0usize;
    let mut copies_observed_by_content = 0usize;
    let mut content_near_dup_pairs = 0usize;
    for i in 1..n {
        // Only pairs the content signal could observe (recording-adjacent, in-range) are eligible
        // as tick-copies — `prev_mads[i].is_some()` is exactly that gate (see the fn doc).
        let recording_adjacent = prev_mads[i].is_some();
        let tick_copy = recording_adjacent && ticks[i].is_some() && ticks[i] == ticks[i - 1];
        let near_dup = prev_mads[i].is_some_and(is_near_duplicate);
        if tick_copy {
            tick_copies += 1;
        }
        if near_dup {
            content_near_dup_pairs += 1;
        }
        if tick_copy && near_dup {
            copies_observed_by_content += 1;
        }
    }
    let copy_observation_rate = if tick_copies > 0 {
        Some(copies_observed_by_content as f64 / tick_copies as f64)
    } else {
        None
    };
    CopyObservation {
        tick_copies,
        copies_observed_by_content,
        content_near_dup_pairs,
        copy_observation_rate,
    }
}

/// Fold per-window [`CopyObservation`]s into one run-level observation (sum the counts, recompute
/// the rate).
pub fn aggregate_copy_observations(observations: &[CopyObservation]) -> CopyObservation {
    let mut tick_copies = 0usize;
    let mut copies_observed_by_content = 0usize;
    let mut content_near_dup_pairs = 0usize;
    for o in observations {
        tick_copies += o.tick_copies;
        copies_observed_by_content += o.copies_observed_by_content;
        content_near_dup_pairs += o.content_near_dup_pairs;
    }
    let copy_observation_rate = if tick_copies > 0 {
        Some(copies_observed_by_content as f64 / tick_copies as f64)
    } else {
        None
    };
    CopyObservation {
        tick_copies,
        copies_observed_by_content,
        content_near_dup_pairs,
        copy_observation_rate,
    }
}

/// Whether the content near-duplicate signal can be trusted to OBSERVE frame duplication — the
/// #1101 promotion-readiness precondition (#1166 signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalViability {
    /// The content signal observed at least [`COPY_OBSERVATION_RATE_MIN`] of the tick-proven copies —
    /// it demonstrably tracks duplication (NECESSARY, not sufficient, for a LIVE flip).
    Viable,
    /// At least [`MIN_TICK_COPIES_FOR_VIABILITY`] tick-proven copies occurred but the content signal
    /// observed fewer than [`COPY_OBSERVATION_RATE_MIN`] of them — structurally blind. NOT
    /// promotable: a LIVE gate would be a false-green.
    Blind,
    /// Fewer than [`MIN_TICK_COPIES_FOR_VIABILITY`] tick-proven copies — too little ground-truth
    /// duplication to judge. Not proven blind, not proven viable.
    Indeterminate,
}

/// Classify a run's aggregate [`CopyObservation`] into a [`SignalViability`].
pub fn signal_viability(observation: &CopyObservation) -> SignalViability {
    if observation.tick_copies < MIN_TICK_COPIES_FOR_VIABILITY {
        SignalViability::Indeterminate
    } else if observation
        .copy_observation_rate
        .is_some_and(|r| r >= COPY_OBSERVATION_RATE_MIN)
    {
        SignalViability::Viable
    } else {
        SignalViability::Blind
    }
}

/// Whether the surface is eligible for a LIVE-gate promotion AT ALL — true ONLY when the signal is
/// [`SignalViability::Viable`]. The precondition [`gates_overall_pass`] must satisfy on real runs
/// before its one-line flip. On the OLD byte-exact tap it was `false` (#1101); #1166's
/// near-duplicate signal turns it toward `true`, but the flip still needs ≥2 consecutive real runs
/// reading `viable` plus a recalibrated bound.
pub fn signal_promotable(viability: SignalViability) -> bool {
    matches!(viability, SignalViability::Viable)
}

/// #1112 (signal #1166) — slice a carried per-frame MAD-to-predecessor vector into ONE cambox
/// window's near-duplicate sequence, in the window's own frame order, ready for
/// [`measure_dup_cadence`] AND [`copy_observation`] (the SAME sequence feeds both, resolving the
/// #1101 content/duplicate sequence-mismatch).
///
/// `frame_indices` are the `RecordingFrame::frame_index` values of the frames the merge attributed
/// to this window (`partition_frames_by_window`), IN window order (ascending). `frame_prev_mads` is
/// the full per-recording vector the stream box computed during `--extract-partial stream` and
/// CARRIED in the partial (`RecordingPartial::frame_prev_diffs`): index `i` is `Some(MAD(frame i,
/// frame i-1))` and index 0 is `None` (no predecessor) — the same 0-based-by-`frame_index` contract
/// `probe::recording::frame_prev_diffs` holds. (Plain reference, not an intra-doc link: that item is
/// behind `#[cfg(feature = "probe")]`, unresolvable from this default-feature module.)
///
/// The output is POSITION-ALIGNED to `frame_indices` (`out.len() == frame_indices.len()`): position
/// `j` is `Some(mad)` ONLY when frame `j` is RECORDING-ADJACENT to its window-predecessor
/// (`frame_indices[j] == frame_indices[j-1] + 1`, so the carried `MAD(frame j, frame j-1)` genuinely
/// measures the window pair) AND that carried MAD is `Some`. Position 0, a decode gap, a
/// non-adjacent window boundary, or an out-of-range index all yield `None` — a near-duplicate can
/// never be manufactured across a gap or a window seam (two skipped frames must not look identical).
pub fn window_prev_mads(
    frame_indices: &[u64],
    frame_prev_mads: &[Option<f64>],
) -> Vec<Option<f64>> {
    frame_indices
        .iter()
        .enumerate()
        .map(|(j, &fi)| {
            if j == 0 {
                return None;
            }
            let adjacent = fi == frame_indices[j - 1] + 1;
            if !adjacent {
                return None;
            }
            frame_prev_mads.get(fi as usize).copied().flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers -----------------------------------------------------------------------

    /// A per-window near-duplicate sequence of `n` frames with a duplicate inserted every `period`
    /// frames (a clean M:(M+1) pulldown when `period == M+1`): the duplicate positions carry a LOW
    /// MAD (near-duplicate), every other position a HIGH MAD (genuine motion). Position 0 is `None`
    /// (no predecessor).
    fn pulldown_mads(n: usize, period: usize) -> Vec<Option<f64>> {
        (0..n)
            .map(|i| {
                if i == 0 {
                    None
                } else if period > 0 && i % period == 0 {
                    Some(2.0) // a byte-duplicate survives lossy encode as a low-MAD pair
                } else {
                    Some(25.0) // genuine motion
                }
            })
            .collect()
    }

    /// All-motion frames (no near-duplicates at all) — a smooth true-60 source.
    fn smooth_mads(n: usize) -> Vec<Option<f64>> {
        (0..n)
            .map(|i| if i == 0 { None } else { Some(25.0) })
            .collect()
    }

    /// `n` motion frames, then force each index in `dup_at` to a low (near-duplicate) MAD. Lets a
    /// test place duplicates at arbitrary positions (for the irregular-spacing / freeze cases).
    fn mads_with_dups_at(n: usize, dup_at: &[usize]) -> Vec<Option<f64>> {
        let mut m: Vec<Option<f64>> = (0..n)
            .map(|i| if i == 0 { None } else { Some(25.0) })
            .collect();
        for &i in dup_at {
            assert!(i >= 1 && i < n, "dup index in range");
            m[i] = Some(2.0);
        }
        m
    }

    // ---- frame_row_sampled_mad (the encoder half) --------------------------------------

    #[test]
    fn identical_frames_have_zero_mad() {
        // A grabber duplicate is byte-for-byte identical → MAD 0.0, which makes it a near-duplicate
        // downstream regardless of threshold.
        let w = 64;
        let h = 128;
        let a: Vec<u8> = (0..(w * h)).map(|i| (i % 251) as u8).collect();
        let b = a.clone();
        assert_eq!(
            frame_row_sampled_mad(&a, &b, w, h),
            0.0,
            "byte-identical frames must have MAD 0"
        );
    }

    #[test]
    fn a_difference_in_a_sampled_row_raises_the_mad() {
        // Real content (sensor noise + motion) differs frame-to-frame; a change in a SAMPLED row
        // must raise the MAD so a genuinely-different frame is not miscounted as a near-duplicate.
        let w = 64;
        let h = 128;
        let a: Vec<u8> = vec![7u8; w * h];
        let mut b = a.clone();
        // saturate every pixel of row 0 (always sampled — y starts at 0) to force a big MAD.
        for v in b.iter_mut().take(w) {
            *v = 250;
        }
        assert!(
            frame_row_sampled_mad(&a, &b, w, h) > 0.0,
            "a sampled-row difference must raise the MAD above 0"
        );
    }

    #[test]
    fn a_big_uniform_shift_yields_a_mad_near_the_shift() {
        // A uniform +40 luma shift on every pixel → MAD ≈ 40 across all sampled rows.
        let w = 32;
        let h = 128;
        let a: Vec<u8> = vec![100u8; w * h];
        let b: Vec<u8> = vec![140u8; w * h];
        let mad = frame_row_sampled_mad(&a, &b, w, h);
        assert!(
            (mad - 40.0).abs() < 1e-9,
            "uniform +40 shift → MAD 40, got {mad}"
        );
    }

    #[test]
    fn degenerate_dimensions_have_zero_mad() {
        assert_eq!(frame_row_sampled_mad(&[1, 2, 3], &[4, 5, 6], 0, 32), 0.0);
        assert_eq!(frame_row_sampled_mad(&[1, 2, 3], &[4, 5, 6], 64, 0), 0.0);
    }

    #[test]
    fn a_short_buffer_never_panics() {
        // Truncated buffers (fewer bytes than width*height) must be handled, not panic — the
        // row-bounds guard skips rows that would read past the end of EITHER buffer.
        let _ = frame_row_sampled_mad(&[1, 2, 3, 4], &[5, 6], 64, 32);
    }

    // ---- is_near_duplicate -------------------------------------------------------------

    #[test]
    fn is_near_duplicate_at_below_and_above_the_threshold() {
        assert!(is_near_duplicate(0.0), "identical → near-duplicate");
        assert!(
            is_near_duplicate(NEAR_DUP_MAD_MAX),
            "exactly at the bound is a near-duplicate (inclusive)"
        );
        assert!(
            is_near_duplicate(NEAR_DUP_MAD_MAX - 0.01),
            "just below the bound is a near-duplicate"
        );
        assert!(
            !is_near_duplicate(NEAR_DUP_MAD_MAX + 0.01),
            "just above the bound is NOT a near-duplicate"
        );
        assert!(
            !is_near_duplicate(25.0),
            "genuine motion is not a near-duplicate"
        );
    }

    // ---- degenerate inputs -------------------------------------------------------------

    #[test]
    fn too_few_frames_returns_none() {
        assert_eq!(measure_dup_cadence(&[]), None);
        assert_eq!(measure_dup_cadence(&[None, Some(2.0), Some(2.0)]), None);
        let just_under = smooth_mads(MIN_SAMPLE_FRAMES - 1);
        assert_eq!(measure_dup_cadence(&just_under), None);
    }

    #[test]
    fn at_the_sample_floor_produces_a_reading() {
        let at_floor = smooth_mads(MIN_SAMPLE_FRAMES);
        assert!(measure_dup_cadence(&at_floor).is_some());
    }

    // ---- the reference patterns --------------------------------------------------------

    #[test]
    fn smooth_true_60_source_has_zero_duplicates_and_is_not_masked() {
        let v = measure_dup_cadence(&smooth_mads(60)).expect("60 frames is plenty");
        assert_eq!(v.sample_frames, 60);
        assert_eq!(v.compared_pairs, 59);
        assert_eq!(v.near_duplicates, 0);
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
        let v = measure_dup_cadence(&pulldown_mads(120, 6)).expect("120 frames is plenty");
        // duplicates land at indices 6,12,...,114 → 19 duplicates over 119 pairs.
        assert_eq!(
            v.near_duplicates, 19,
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
        assert_eq!(
            v.gap_cv,
            Some(0.0),
            "a clean pulldown is perfectly regular: {v:?}"
        );
        assert!(v.coverage >= DUP_COVERAGE_MIN, "spans the window: {v:?}");
        assert!(v.duplication_masked, "a 5:6 pulldown IS masked: {v:?}");
        assert!(
            (v.inferred_source_fps - 60.0 * (1.0 - 19.0 / 119.0)).abs() < 1e-9,
            "inferred source fps ≈ 50: {v:?}"
        );
    }

    #[test]
    fn a_localized_freeze_is_not_masked_coverage_veto() {
        // A run of near-duplicates CONCENTRATED in one slice (positions 5..20) — high local rate,
        // regular local spacing, but does NOT span the window → coverage veto (frozen_leg's job).
        let dup_at: Vec<usize> = (5..20).collect();
        let v = measure_dup_cadence(&mads_with_dups_at(120, &dup_at)).expect("plenty");
        assert!(v.near_duplicates >= 2, "has duplicates: {v:?}");
        assert!(
            v.duplicate_fraction >= DUP_RATE_PULLDOWN_MIN,
            "high local rate: {v:?}"
        );
        assert!(
            v.coverage < DUP_COVERAGE_MIN,
            "localized, does not span: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "a localized freeze is NOT masked: {v:?}"
        );
    }

    #[test]
    fn an_irregular_burst_is_not_masked_cv_veto() {
        // Duplicates spread across the window but UNEVENLY spaced (gaps 3,30,3,30,...) → high cv →
        // rejected as an irregular glitch burst, not a regular pulldown.
        let dup_at = [3usize, 6, 40, 43, 80, 83, 116];
        let v = measure_dup_cadence(&mads_with_dups_at(120, &dup_at)).expect("plenty");
        assert!(v.near_duplicates >= 2, "has duplicates: {v:?}");
        assert!(v.coverage >= DUP_COVERAGE_MIN, "spans the window: {v:?}");
        assert!(
            v.gap_cv.is_some_and(|cv| cv > DUP_GAP_CV_MAX),
            "irregular spacing exceeds the cv bound: {v:?}"
        );
        assert!(
            !v.duplication_masked,
            "an irregular burst is NOT masked: {v:?}"
        );
    }

    #[test]
    fn none_positions_and_gaps_never_form_a_duplicate() {
        // A None (decode gap / non-adjacent window boundary) between two would-be duplicates must
        // not be counted as a near-duplicate — only a Some MAD at/under the threshold is.
        let mut m = smooth_mads(60);
        m[10] = None; // a gap
        m[11] = None;
        let v = measure_dup_cadence(&m).expect("plenty");
        assert_eq!(v.near_duplicates, 0, "None never forms a duplicate: {v:?}");
        assert!(!v.duplication_masked, "{v:?}");
    }

    // ---- copy_observation (the #1101 viability cross-check on the #1166 signal) ----------

    #[test]
    fn a_tick_copy_with_a_near_duplicate_is_observed() {
        // The FIXED-signal case: a tick-copy pair whose content MAD is low → observed.
        let ticks = [Some(1u64), Some(1), Some(2)]; // frame 1 repeats frame 0's tick
        let mads = [None, Some(3.0), Some(25.0)]; // frame 1 is a content near-duplicate
        let o = copy_observation(&ticks, &mads);
        assert_eq!(o.tick_copies, 1);
        assert_eq!(
            o.copies_observed_by_content, 1,
            "the near-dup observed the tick-copy"
        );
        assert_eq!(o.content_near_dup_pairs, 1);
        assert_eq!(o.copy_observation_rate, Some(1.0));
    }

    #[test]
    fn a_tick_copy_with_a_high_mad_is_blind_not_observed() {
        // The BLIND-signal case (what the byte-exact tap did to nearly every copy): a tick-copy
        // whose content MAD is HIGH (lossy encode destroyed the identity) → not observed.
        let ticks = [Some(1u64), Some(1), Some(2)];
        let mads = [None, Some(25.0), Some(25.0)]; // high MAD on the tick-copy pair
        let o = copy_observation(&ticks, &mads);
        assert_eq!(o.tick_copies, 1);
        assert_eq!(
            o.copies_observed_by_content, 0,
            "a high MAD does not observe the copy"
        );
        assert_eq!(o.copy_observation_rate, Some(0.0));
    }

    #[test]
    fn none_tick_or_none_mad_never_counts_as_a_copy() {
        let ticks = [Some(1u64), None, Some(1)];
        let mads = [None, Some(2.0), Some(2.0)];
        let o = copy_observation(&ticks, &mads);
        assert_eq!(o.tick_copies, 0, "a None tick is never a tick-copy");
        // position 2: tick 1 vs None → not a tick-copy; content near-dup still counted raw.
        assert_eq!(o.content_near_dup_pairs, 2);
        assert_eq!(o.copy_observation_rate, None, "no tick-copies → no rate");
    }

    #[test]
    fn a_tick_copy_across_a_non_adjacent_boundary_is_not_counted() {
        // #1166 review — the tick-copy basis must match the MAD's recording-adjacency basis: a
        // repeated tick at a window position the MAD side gated to None (a non-adjacent boundary /
        // gap) is NOT observable by the content signal, so it must not count as a tick-copy and
        // depress the observation rate. `window_prev_mads` yields None at such a position.
        let ticks = [Some(5u64), Some(5)]; // same tick both frames
        let mads = [None, None]; // position 1 gated to None (non-adjacent / gap)
        let o = copy_observation(&ticks, &mads);
        assert_eq!(
            o.tick_copies, 0,
            "a tick-copy at a non-recording-adjacent position (MAD None) is not counted"
        );
        assert_eq!(o.copy_observation_rate, None);

        // Contrast: the SAME repeated tick at a recording-adjacent position (MAD Some, even if the
        // MAD itself is high = blind) DOES count as a tick-copy — the denominator the signal is
        // judged against.
        let ticks2 = [Some(5u64), Some(5)];
        let mads2 = [None, Some(25.0)]; // adjacent, but high MAD → blind
        let o2 = copy_observation(&ticks2, &mads2);
        assert_eq!(
            o2.tick_copies, 1,
            "an adjacent tick-copy counts even when the MAD is blind"
        );
        assert_eq!(o2.copies_observed_by_content, 0);
        assert_eq!(o2.copy_observation_rate, Some(0.0));
    }

    #[test]
    fn aggregate_sums_counts_and_recomputes_the_rate() {
        let a = CopyObservation {
            tick_copies: 4,
            copies_observed_by_content: 3,
            content_near_dup_pairs: 5,
            copy_observation_rate: Some(0.75),
        };
        let b = CopyObservation {
            tick_copies: 6,
            copies_observed_by_content: 5,
            content_near_dup_pairs: 7,
            copy_observation_rate: Some(0.833),
        };
        let agg = aggregate_copy_observations(&[a, b]);
        assert_eq!(agg.tick_copies, 10);
        assert_eq!(agg.copies_observed_by_content, 8);
        assert_eq!(agg.content_near_dup_pairs, 12);
        // recomputed, NOT averaged: 8/10 = 0.8
        assert_eq!(agg.copy_observation_rate, Some(0.8));
    }

    #[test]
    fn aggregate_of_empty_has_no_rate() {
        let agg = aggregate_copy_observations(&[]);
        assert_eq!(agg.tick_copies, 0);
        assert_eq!(agg.copy_observation_rate, None);
    }

    // ---- signal_viability / signal_promotable ------------------------------------------

    #[test]
    fn viability_boundaries() {
        // Too few tick-copies → Indeterminate (not proven blind).
        let indet = CopyObservation {
            tick_copies: MIN_TICK_COPIES_FOR_VIABILITY - 1,
            copies_observed_by_content: 0,
            content_near_dup_pairs: 0,
            copy_observation_rate: None,
        };
        assert_eq!(signal_viability(&indet), SignalViability::Indeterminate);
        assert!(!signal_promotable(signal_viability(&indet)));

        // Enough tick-copies but low observation → Blind (the old byte-exact tap).
        let blind = CopyObservation {
            tick_copies: 147,
            copies_observed_by_content: 2,
            content_near_dup_pairs: 2,
            copy_observation_rate: Some(2.0 / 147.0),
        };
        assert_eq!(signal_viability(&blind), SignalViability::Blind);
        assert!(!signal_promotable(signal_viability(&blind)));

        // Enough tick-copies AND observation ≥ floor → Viable (the #1166 fixed signal).
        let viable = CopyObservation {
            tick_copies: 32,
            copies_observed_by_content: 26,
            content_near_dup_pairs: 26,
            copy_observation_rate: Some(26.0 / 32.0),
        };
        assert_eq!(signal_viability(&viable), SignalViability::Viable);
        assert!(signal_promotable(signal_viability(&viable)));

        // Exactly at the floor is Viable (inclusive).
        let at_floor = CopyObservation {
            tick_copies: 10,
            copies_observed_by_content: 5,
            content_near_dup_pairs: 5,
            copy_observation_rate: Some(0.5),
        };
        assert_eq!(signal_viability(&at_floor), SignalViability::Viable);
    }

    // ---- window_prev_mads (#1112/#1166 — the carry→slice glue) --------------------------

    #[test]
    fn window_prev_mads_gates_on_recording_adjacency() {
        // frame_prev_mads is 0-based by frame_index; index 0 is None (no predecessor).
        let prev = vec![None, Some(2.0), Some(25.0), Some(3.0), Some(25.0)];
        // A contiguous window [1,2,3,4]: position 0 → None; positions 1..3 → the carried MADs
        // (each recording-adjacent to its predecessor).
        let out = window_prev_mads(&[1, 2, 3, 4], &prev);
        assert_eq!(out, vec![None, Some(25.0), Some(3.0), Some(25.0)]);
    }

    #[test]
    fn window_prev_mads_none_across_a_non_adjacent_boundary() {
        // A window whose first two frames are NOT recording-adjacent (2 then 5): the carried MAD at
        // index 5 measures MAD(frame5, frame4), NOT the window pair, so it must be dropped to None.
        let prev = vec![None, Some(2.0), Some(2.0), Some(2.0), Some(2.0), Some(2.0)];
        let out = window_prev_mads(&[2, 5], &prev);
        assert_eq!(
            out,
            vec![None, None],
            "non-adjacent boundary → None, never a false dup"
        );
    }

    #[test]
    fn window_prev_mads_out_of_range_index_is_none() {
        let prev = vec![None, Some(2.0)];
        // frame_index 9 is past the vector → None (a hash/MAD gap must not manufacture a duplicate).
        let out = window_prev_mads(&[8, 9], &prev);
        assert_eq!(out, vec![None, None]);
    }

    // ---- worst_masked_duplicate_fraction / dup_cadence_gate_pass ------------------------

    #[test]
    fn worst_masked_fraction_is_none_when_no_window_is_masked() {
        let smooth = measure_dup_cadence(&smooth_mads(60));
        assert_eq!(worst_masked_duplicate_fraction(&[smooth]), None);
    }

    #[test]
    fn worst_masked_fraction_ignores_a_higher_raw_fraction_from_an_unmasked_window() {
        // A localized freeze has a HIGH raw fraction but is NOT masked (coverage veto) → excluded.
        let freeze_dup_at: Vec<usize> = (5..25).collect();
        let freeze = measure_dup_cadence(&mads_with_dups_at(120, &freeze_dup_at));
        let pulldown = measure_dup_cadence(&pulldown_mads(120, 6));
        let pf = pulldown.as_ref().unwrap().duplicate_fraction;
        let worst = worst_masked_duplicate_fraction(&[freeze, pulldown]);
        assert_eq!(
            worst,
            Some(pf),
            "only the masked pulldown's fraction counts"
        );
    }

    #[test]
    fn worst_masked_fraction_takes_the_max_across_multiple_masked_windows() {
        let a = measure_dup_cadence(&pulldown_mads(120, 6)); // ~0.167
        let b = measure_dup_cadence(&pulldown_mads(120, 5)); // ~0.20 (denser dups)
        let fa = a.as_ref().unwrap().duplicate_fraction;
        let fb = b.as_ref().unwrap().duplicate_fraction;
        let worst = worst_masked_duplicate_fraction(&[a, b]).unwrap();
        assert!((worst - fa.max(fb)).abs() < 1e-9);
    }

    #[test]
    fn worst_below_bound_passes_over_bound_fails() {
        assert!(
            dup_cadence_gate_pass(None, Some(0.10)),
            "no masked window → pass"
        );
        assert!(
            dup_cadence_gate_pass(Some(0.09), Some(0.10)),
            "below bound → pass"
        );
        assert!(
            dup_cadence_gate_pass(Some(0.10), Some(0.10)),
            "at bound → pass"
        );
        assert!(
            !dup_cadence_gate_pass(Some(0.11), Some(0.10)),
            "over bound → fail"
        );
        assert!(
            dup_cadence_gate_pass(Some(0.99), None),
            "no bound (report-only) → pass"
        );
    }

    #[test]
    fn gates_overall_pass_is_report_only() {
        assert!(
            !gates_overall_pass(),
            "the dup-cadence surface ships REPORT-ONLY until the #1166 promotion gate is met"
        );
    }

    // ---- #1166 REAL-DATA fixture: the fix turns the viability from Blind to Viable ------

    /// The MEASURED row-sampled MAD (rows=64) between every retained adjacent diagnostic-frame PNG
    /// pair across the 22 runs that retained pixels, split by whether the Vernier tick proved a copy
    /// (the 32 COPY pairs come from 16 of those runs; the 381 MOTION pairs from all 22).
    /// COPY = a tick-proven byte-duplicate camera frame (the ground truth); MOTION = genuine motion.
    /// (The byte-exact FNV-1a observed 0 of these 32 copies — structurally blind, #1101.)
    const REAL_COPY_MADS: &[f64] = &[
        1.37, 2.0, 2.34, 3.83, 3.92, 3.93, 4.12, 4.71, 4.81, 5.21, 5.33, 5.36, 6.03, 6.3, 7.04,
        7.16, 7.35, 7.4, 7.52, 7.69, 7.69, 7.86, 8.46, 8.8, 9.43, 9.58, 10.62, 11.39, 17.3, 17.93,
        19.03, 20.34,
    ];
    /// The genuine-motion floor: a representative LOW sample of the 381 real motion MADs, near the
    /// copy cluster (the ones that would false-positive at too high a threshold) and LED BY the true
    /// minimum. Every value listed is a real measured motion MAD; the full motion set's min is 10.79
    /// and every one of the 381 motion pairs is ABOVE NEAR_DUP_MAD_MAX (so 0 motion false-positives).
    const REAL_MOTION_MADS_LOW: &[f64] = &[
        10.79, 12.23, 12.7, 12.75, 13.8, 13.84, 13.89, 13.97, 14.33, 14.51, 14.62, 15.5, 16.24,
        17.08, 18.38, 19.1, 20.0, 21.04, 22.07, 25.01,
    ];

    #[test]
    fn real_lossy_copy_mads_make_the_signal_viable() {
        // Feed the MEASURED real copy MADs as tick-copy pairs and the motion MADs as motion pairs,
        // and assert the #1166 near-duplicate signal (a) observes >= COPY_OBSERVATION_RATE_MIN of
        // the tick-proven copies (→ Viable) and (b) fires on 0 of the genuine-motion pairs.
        let observed = REAL_COPY_MADS
            .iter()
            .filter(|&&m| is_near_duplicate(m))
            .count();
        let rate = observed as f64 / REAL_COPY_MADS.len() as f64;
        assert!(
            rate >= COPY_OBSERVATION_RATE_MIN,
            "the near-duplicate signal must observe >= {COPY_OBSERVATION_RATE_MIN} of real copies, \
             got {observed}/{} = {rate:.3}",
            REAL_COPY_MADS.len()
        );
        let motion_fp = REAL_MOTION_MADS_LOW
            .iter()
            .filter(|&&m| is_near_duplicate(m))
            .count();
        assert_eq!(
            motion_fp, 0,
            "the near-duplicate signal must NOT fire on any genuine-motion pair (real data)"
        );

        // And the full viability classification reads Viable on the real aggregate.
        let obs = CopyObservation {
            tick_copies: REAL_COPY_MADS.len(),
            copies_observed_by_content: observed,
            content_near_dup_pairs: observed,
            copy_observation_rate: Some(rate),
        };
        assert_eq!(
            signal_viability(&obs),
            SignalViability::Viable,
            "on the real lossy copy MADs the #1166 signal is Viable (the fix)"
        );
        assert!(signal_promotable(signal_viability(&obs)));
    }
}
