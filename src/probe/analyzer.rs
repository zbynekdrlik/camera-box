//! Correlate emitted vs observed frame IDs → loss / freeze / reorder + latency.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Oversample floor: a coverage run is *designed* to capture every painted id at
/// least this many times (at 60 fps capture / ~12 fps coverage paint the real
/// oversample is ~5x). The run is judged "oversampled" when its median decoded
/// samples/id reaches this floor; only then is the torn-QR allowance for lone
/// gaps applied. Spec §coverage: ">= 2 samples/id".
const MIN_CONFIRM_SAMPLES: usize = 2;

/// Isolated single-frame gaps are tolerated as torn-QR artifacts only up to
/// `emitted / this` of them (one per 1000 emitted ids = 0.1%, floor 1). Above the
/// cap, scattered single-frame loss is real (periodic/alternating drop) and is
/// reclassified as confirmed loss.
const INCONCLUSIVE_TOLERANCE_DIVISOR: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaintMode {
    Coverage,
    FullRate,
}

/// One decoded frame, in capture order.
#[derive(Debug, Clone, Copy)]
pub struct Observed {
    pub frame_id: u32,
    pub gen_ts_ns: i64,
    pub recv_ts_ns: i64,
}

pub struct AnalysisInput {
    pub mode: PaintMode,
    pub emitted_ids: Vec<u32>,
    pub observed: Vec<Observed>,
    pub capture_fps: f64,
    /// Detection threshold: a run of more than this many consecutive-equal ids
    /// is listed as a freeze. Populates the `freezes` list; does not by itself
    /// fail the verdict.
    pub freeze_periods: f64,
    /// Hard gate: fail the verdict if measured `latency.p99_ms` exceeds this.
    /// `None` ⇒ latency is report-only (Phase-1 behavior). Set from a baseline
    /// plus margin once the rig is characterized (spec §9/§14/§15).
    pub max_p99_latency_ms: Option<f64>,
    /// Hard gate: fail the verdict if any detected freeze's `repeat_count`
    /// exceeds this. Distinct from `freeze_periods` (which only *detects*).
    /// `None` ⇒ freezes are report-only.
    pub max_freeze_periods_gate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub samples: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Freeze {
    pub frame_id: u32,
    pub repeat_count: usize,
    pub duration_ms: f64,
}

/// #20 oversample discriminator: per-emitted-id decoded-sample multiplicity,
/// surfaced so a single "missing" frame can be classified as a genuine pipeline
/// drop or a torn/illegible-QR artifact (which the analyzer cannot re-decode).
#[derive(Debug, Clone, Serialize)]
pub struct CoverageStats {
    /// The oversample floor (see MIN_CONFIRM_SAMPLES) the run median must reach
    /// for the torn-QR allowance to apply.
    pub min_confirm_samples: usize,
    /// Median decoded samples per emitted id — the run-health signal.
    pub oversample_p50: usize,
    /// `oversample_p50 >= min_confirm_samples`: the lone-gap torn-QR allowance is
    /// only applied to genuinely oversampled runs; otherwise strict membership.
    pub run_oversampled: bool,
    /// Emitted ids with exactly 1 decoded sample: present but torn-prone.
    pub low_coverage_ids: Vec<u32>,
    /// Emitted ids with 0 decoded samples judged genuine drops (== `missing_ids`):
    /// any absent id in a non-oversampled run, or any run of >= 2 CONSECUTIVE
    /// absent ids (a burst loss tearing cannot produce). Fails the coverage gate.
    pub confirmed_drops: Vec<u32>,
    /// Lone (single, isolated) absent ids in an oversampled run, while their
    /// total stays within the torn-QR tolerance (INCONCLUSIVE_TOLERANCE_DIVISOR)
    /// — indistinguishable
    /// from a torn-on-every-sample QR, so report-only rather than failing the
    /// gate (#20). NOTE: a genuine *isolated single-frame* drop (a handful, under
    /// the cap) therefore does not fail the gate; it is still surfaced here and
    /// as a frame-probe WARN. Bursts, non-oversampled gaps, and scattered gaps
    /// over the cap (periodic/alternating loss) are always confirmed.
    pub inconclusive_gaps: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    pub mode: PaintMode,
    pub emitted_count: usize,
    pub observed_count: usize,
    pub unique_observed: usize,
    pub missing_ids: Vec<u32>,
    pub reorders: Vec<(u32, u32)>,
    pub freezes: Vec<Freeze>,
    pub latency: Option<LatencyStats>,
    pub coverage: CoverageStats,
    pub verdict_pass: bool,
}

pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q * (sorted.len() as f64)).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Backwards-going adjacent pairs in capture order = reordering.
pub fn detect_reorders(observed: &[Observed]) -> Vec<(u32, u32)> {
    let mut reorders = Vec::new();
    for w in observed.windows(2) {
        if w[1].frame_id < w[0].frame_id {
            reorders.push((w[0].frame_id, w[1].frame_id));
        }
    }
    reorders
}

/// Runs of consecutive-equal frame IDs longer than `freeze_periods` capture
/// periods. `chunk_by` avoids a manual index loop (no infinite-loop mutants).
pub fn detect_freezes(observed: &[Observed], capture_fps: f64, freeze_periods: f64) -> Vec<Freeze> {
    let period_ms = 1000.0 / capture_fps;
    observed
        .chunk_by(|a, b| a.frame_id == b.frame_id)
        .filter(|run| (run.len() as f64) > freeze_periods)
        .map(|run| Freeze {
            frame_id: run[0].frame_id,
            repeat_count: run.len(),
            duration_ms: run.len() as f64 * period_ms,
        })
        .collect()
}

/// min/mean/p50/p95/p99/max over a set of millisecond samples (None if empty).
pub fn latency_stats(samples_ms: &[f64]) -> Option<LatencyStats> {
    if samples_ms.is_empty() {
        return None;
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum: f64 = sorted.iter().sum();
    Some(LatencyStats {
        samples: sorted.len(),
        min_ms: sorted[0],
        mean_ms: sum / sorted.len() as f64,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        max_ms: *sorted.last().unwrap(),
    })
}

/// Hard gate on latency p99 and freeze severity. A `None` bound leaves that
/// dimension report-only (Phase-1 behavior); a `Some` bound fails the verdict
/// when exceeded. Both comparisons use strict `>` so a value exactly at the
/// bound passes.
pub fn latency_freeze_gate_pass(
    latency: &Option<LatencyStats>,
    freezes: &[Freeze],
    max_p99_latency_ms: Option<f64>,
    max_freeze_periods_gate: Option<f64>,
) -> bool {
    if let (Some(bound), Some(l)) = (max_p99_latency_ms, latency) {
        if l.p99_ms > bound {
            return false;
        }
    }
    if let Some(gate) = max_freeze_periods_gate {
        if freezes.iter().any(|f| f.repeat_count as f64 > gate) {
            return false;
        }
    }
    true
}

pub fn analyze(input: AnalysisInput) -> AnalysisReport {
    let emitted_set: HashSet<u32> = input.emitted_ids.iter().copied().collect();
    let observed_set: HashSet<u32> = input.observed.iter().map(|o| o.frame_id).collect();

    // #20: decoded-sample count per emitted id (the oversample discriminator).
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for o in &input.observed {
        *counts.entry(o.frame_id).or_insert(0) += 1;
    }
    let samples = |id: u32| counts.get(&id).copied().unwrap_or(0);
    let oversample_p50 = {
        let mut per: Vec<usize> = input.emitted_ids.iter().map(|&id| samples(id)).collect();
        per.sort_unstable();
        if per.is_empty() {
            0
        } else {
            per[per.len() / 2]
        }
    };
    let run_oversampled = oversample_p50 >= MIN_CONFIRM_SAMPLES;

    // Emitted ids with exactly one decoded sample: present but torn-prone.
    // (Precondition: emitted_ids are unique per run — the painter increments
    // frame_id monotonically — so each id maps to one decoded-sample count.)
    let low_coverage_ids: Vec<u32> = input
        .emitted_ids
        .iter()
        .copied()
        .filter(|&id| samples(id) == 1)
        .collect();

    // Classify zero-sample emitted ids by the length of each maximal run of
    // CONSECUTIVE absent ids. Tearing corrupts at most a few capture frames
    // around a paint transition, so at real oversample it can zero out at most
    // ONE emitted id in isolation — a lone gap is a torn-QR *candidate*. Two or
    // more ADJACENT absent ids cannot be tearing (that would have to black out
    // multiple full hold-windows) — it is a real burst loss -> confirmed, fails
    // the gate (review C1). A run that is not oversampled keeps strict
    // membership: every absent id is confirmed. `chunk_by` groups maximal runs
    // of equal zero-ness, avoiding a manual index loop and its infinite-loop
    // mutants (same reason as `detect_freezes`).
    let mut confirmed_drops: Vec<u32> = Vec::new();
    let mut isolated_candidates: Vec<u32> = Vec::new();
    for run in input
        .emitted_ids
        .chunk_by(|a, b| (samples(*a) == 0) == (samples(*b) == 0))
    {
        if samples(run[0]) != 0 {
            continue;
        }
        if run_oversampled && run.len() == 1 {
            isolated_candidates.push(run[0]);
        } else {
            confirmed_drops.extend_from_slice(run);
        }
    }
    // Torn-QR gaps are RARE (measured ~0 zero-sample ids per 7196 on the cam2
    // 1080p60 rig; the original #20 false-"missing" was 1 in 7196 = 0.014%).
    // Tolerate isolated gaps as torn-prone only up to emitted /
    // INCONCLUSIVE_TOLERANCE_DIVISOR of them — above that, periodic/scattered
    // single-frame loss (alternating-frame drop, 1-in-N) is real and MUST fail,
    // not hide as inconclusive (review round 2). The cap is many times the
    // observed torn rate yet far below any real periodic-loss rate.
    let tolerance = (input.emitted_ids.len() / INCONCLUSIVE_TOLERANCE_DIVISOR).max(1);
    let inconclusive_gaps = if isolated_candidates.len() <= tolerance {
        isolated_candidates
    } else {
        confirmed_drops.extend_from_slice(&isolated_candidates);
        Vec::new()
    };
    // Emitted ids are ascending (monotonic paint), so sorting confirmed restores
    // emitted order after appending the reclassified isolated gaps.
    confirmed_drops.sort_unstable();
    let missing_ids = confirmed_drops.clone();
    let coverage = CoverageStats {
        min_confirm_samples: MIN_CONFIRM_SAMPLES,
        oversample_p50,
        run_oversampled,
        low_coverage_ids,
        confirmed_drops,
        inconclusive_gaps,
    };

    let reorders = detect_reorders(&input.observed);
    let freezes = detect_freezes(&input.observed, input.capture_fps, input.freeze_periods);

    let mut seen = HashSet::new();
    let mut lat_ms: Vec<f64> = Vec::new();
    for o in &input.observed {
        if seen.insert(o.frame_id) {
            lat_ms.push((o.recv_ts_ns - o.gen_ts_ns) as f64 / 1_000_000.0);
        }
    }
    let latency = latency_stats(&lat_ms);

    // A coverage PASS requires that we actually tested frames: an empty emitted
    // set (e.g. settle window >= run duration) must FAIL, never pass vacuously.
    let base_pass = match input.mode {
        PaintMode::Coverage => {
            !emitted_set.is_empty() && missing_ids.is_empty() && reorders.is_empty()
        }
        PaintMode::FullRate => reorders.is_empty(),
    };
    // Latency/freeze hard gates apply to BOTH modes: full-rate loss is
    // report-only, but a latency or freeze regression there still fails the run.
    let verdict_pass = base_pass
        && latency_freeze_gate_pass(
            &latency,
            &freezes,
            input.max_p99_latency_ms,
            input.max_freeze_periods_gate,
        );

    AnalysisReport {
        mode: input.mode,
        emitted_count: emitted_set.len(),
        observed_count: input.observed.len(),
        unique_observed: observed_set.len(),
        missing_ids,
        reorders,
        freezes,
        latency,
        coverage,
        verdict_pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(frame_id: u32, gen: i64, recv: i64) -> Observed {
        Observed {
            frame_id,
            gen_ts_ns: gen,
            recv_ts_ns: recv,
        }
    }

    fn input(mode: PaintMode, emitted: Vec<u32>, observed: Vec<Observed>) -> AnalysisInput {
        AnalysisInput {
            mode,
            emitted_ids: emitted,
            observed,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
        }
    }

    fn input_gated(
        mode: PaintMode,
        emitted: Vec<u32>,
        observed: Vec<Observed>,
        max_p99_latency_ms: Option<f64>,
        max_freeze_periods_gate: Option<f64>,
    ) -> AnalysisInput {
        AnalysisInput {
            max_p99_latency_ms,
            max_freeze_periods_gate,
            ..input(mode, emitted, observed)
        }
    }

    #[test]
    fn healthy_coverage_passes() {
        let emitted = vec![0, 1, 2, 3, 4];
        let observed = vec![
            obs(0, 0, 10_000_000),
            obs(1, 33_000_000, 43_000_000),
            obs(1, 33_000_000, 43_000_000),
            obs(2, 66_000_000, 76_000_000),
            obs(3, 99_000_000, 109_000_000),
            obs(4, 132_000_000, 142_000_000),
        ];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.verdict_pass);
        assert!(r.missing_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(r.freezes.is_empty());
        let lat = r.latency.unwrap();
        assert_eq!(lat.samples, 5);
        assert!((lat.mean_ms - 10.0).abs() < 0.001);
    }

    #[test]
    fn missing_frame_fails_coverage() {
        let emitted = vec![0, 1, 2, 3];
        let observed = vec![obs(0, 0, 1), obs(1, 1, 2), obs(3, 3, 4)];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(!r.verdict_pass);
        assert_eq!(r.missing_ids, vec![2]);
    }

    #[test]
    fn freeze_is_detected_but_not_gated() {
        let emitted = vec![0, 1, 2];
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(2, 20, 21),
        ];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.freezes.len(), 1);
        assert_eq!(r.freezes[0].frame_id, 1);
        assert_eq!(r.freezes[0].repeat_count, 5);
        // duration = run (5) * period_ms (1000/30) = 166.667 ms. Pins period_ms
        // and the run*period multiplication.
        assert!((r.freezes[0].duration_ms - 166.6667).abs() < 0.01);
        assert!(r.verdict_pass);
    }

    #[test]
    fn run_equal_to_threshold_is_not_a_freeze() {
        // A run of exactly freeze_periods (3) must NOT flag (uses `>`, not `>=`).
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(2, 20, 21),
        ];
        let r = analyze(input(PaintMode::Coverage, vec![0, 1, 2], observed));
        assert!(r.freezes.is_empty());
    }

    #[test]
    fn percentile_clamps_out_of_range_quantile() {
        // q > 1 must clamp to the last element, never index out of bounds.
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&s, 1.5), 5.0);
    }

    #[test]
    fn reorder_fails_both_modes() {
        let emitted = vec![0, 1, 2];
        let observed = vec![obs(0, 0, 1), obs(2, 2, 3), obs(1, 1, 4)];
        let cov = analyze(input(
            PaintMode::Coverage,
            emitted.clone(),
            observed.clone(),
        ));
        let full = analyze(input(PaintMode::FullRate, emitted, observed));
        assert!(!cov.verdict_pass);
        assert!(!full.verdict_pass);
        assert_eq!(cov.reorders, vec![(2, 1)]);
    }

    #[test]
    fn fullrate_missing_is_report_only() {
        let emitted = vec![0, 1, 2, 3];
        let observed = vec![obs(0, 0, 1), obs(2, 2, 3), obs(3, 3, 4)];
        let r = analyze(input(PaintMode::FullRate, emitted, observed));
        assert!(r.verdict_pass);
        assert_eq!(r.missing_ids, vec![1]);
    }

    #[test]
    fn empty_emitted_coverage_fails() {
        // No frames tested (e.g. settle window >= duration) must never pass.
        let r = analyze(input(PaintMode::Coverage, vec![], vec![]));
        assert!(!r.verdict_pass);
        assert_eq!(r.emitted_count, 0);
    }

    #[test]
    fn percentile_nearest_rank() {
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&s, 0.50), 5.0);
        assert_eq!(percentile(&s, 0.95), 10.0);
        assert_eq!(percentile(&s, 0.99), 10.0);
    }

    #[test]
    fn detect_reorders_flags_backwards_pairs() {
        let obs = vec![obs(0, 0, 1), obs(2, 0, 2), obs(1, 0, 3), obs(3, 0, 4)];
        assert_eq!(detect_reorders(&obs), vec![(2, 1)]);
    }

    #[test]
    fn detect_reorders_empty_when_monotonic() {
        let obs = vec![obs(0, 0, 1), obs(1, 0, 2), obs(1, 0, 3), obs(2, 0, 4)];
        assert!(detect_reorders(&obs).is_empty());
    }

    #[test]
    fn detect_freezes_groups_runs_over_threshold() {
        // id 1 repeats 5x (> 3) at 30 fps -> one freeze, 5*33.333ms.
        let obs = vec![
            obs(0, 0, 1),
            obs(1, 0, 2),
            obs(1, 0, 3),
            obs(1, 0, 4),
            obs(1, 0, 5),
            obs(1, 0, 6),
            obs(2, 0, 7),
        ];
        let f = detect_freezes(&obs, 30.0, 3.0);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].frame_id, 1);
        assert_eq!(f[0].repeat_count, 5);
        assert!((f[0].duration_ms - 166.6667).abs() < 0.01);
    }

    #[test]
    fn latency_stats_none_on_empty() {
        assert!(latency_stats(&[]).is_none());
    }

    #[test]
    fn latency_stats_computes_fields() {
        let s = latency_stats(&[10.0, 20.0, 30.0]).unwrap();
        assert_eq!(s.samples, 3);
        assert_eq!(s.min_ms, 10.0);
        assert!((s.mean_ms - 20.0).abs() < 0.001);
        assert_eq!(s.max_ms, 30.0);
    }

    // --- #10: hard latency + freeze gates folded into the verdict ---

    /// Five unique ids 0..4, ascending (no reorder/loss), with one id at `hi_ms`
    /// latency and the rest at 100 ms. p99 (nearest-rank, n=5) == `hi_ms`.
    fn obs_with_p99(hi_ms: i64) -> Vec<Observed> {
        vec![
            obs(0, 0, 100_000_000),
            obs(1, 0, 100_000_000),
            obs(2, 0, 100_000_000),
            obs(3, 0, 100_000_000),
            obs(4, 0, hi_ms * 1_000_000),
        ]
    }

    #[test]
    fn latency_p99_over_bound_fails_coverage() {
        // p99 = 300 ms, bound 250 ms -> FAIL despite zero loss/reorder.
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2, 3, 4],
            obs_with_p99(300),
            Some(250.0),
            None,
        ));
        assert!(r.missing_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 300.0);
        assert!(!r.verdict_pass);
    }

    #[test]
    fn latency_p99_at_bound_passes() {
        // p99 = 250 ms, bound 250 ms -> PASS (uses `>`, not `>=`).
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2, 3, 4],
            obs_with_p99(250),
            Some(250.0),
            None,
        ));
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 250.0);
        assert!(r.verdict_pass);
    }

    #[test]
    fn latency_gate_also_applies_to_fullrate() {
        // Full-rate loss is report-only, but the latency gate still fails the run.
        // emitted has a missing id (5) to prove loss is ignored while p99 gates.
        let r = analyze(input_gated(
            PaintMode::FullRate,
            vec![0, 1, 2, 3, 4, 5],
            obs_with_p99(300),
            Some(250.0),
            None,
        ));
        assert_eq!(r.missing_ids, vec![5]);
        assert!(r.reorders.is_empty());
        assert!(!r.verdict_pass);
    }

    #[test]
    fn freeze_over_gate_fails() {
        // id 1 repeats 5x (detected at freeze_periods=3); gate 4 -> 5 > 4 -> FAIL.
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(2, 20, 21),
        ];
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2],
            observed,
            None,
            Some(4.0),
        ));
        assert_eq!(r.freezes[0].repeat_count, 5);
        assert!(!r.verdict_pass);
    }

    #[test]
    fn freeze_at_gate_passes() {
        // repeat_count 5, gate 5 -> 5 > 5 is false -> PASS (uses `>`, not `>=`).
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(2, 20, 21),
        ];
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2],
            observed,
            None,
            Some(5.0),
        ));
        assert_eq!(r.freezes[0].repeat_count, 5);
        assert!(r.verdict_pass);
    }

    #[test]
    fn freeze_gate_fails_when_any_one_run_exceeds() {
        // Two freezes (id1 x5, id3 x8); gate 6 -> 8 > 6 -> FAIL even though x5
        // is under. Proves the gate trips on ANY over-bound run, not all.
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 0, 1),
            obs(1, 0, 1),
            obs(1, 0, 1),
            obs(1, 0, 1),
            obs(1, 0, 1),
            obs(2, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(3, 0, 1),
            obs(4, 0, 1),
        ];
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2, 3, 4],
            observed,
            None,
            Some(6.0),
        ));
        assert_eq!(r.freezes.len(), 2);
        assert!(!r.verdict_pass);
    }

    #[test]
    fn no_gate_thresholds_preserve_behavior() {
        // None/None: a high-latency, frozen run still PASSES (report-only) — the
        // Phase-1 contract. Supersedes the intent of freeze_is_detected_but_not_gated.
        let observed = vec![
            obs(0, 0, 100_000_000),
            obs(1, 0, 999_000_000),
            obs(1, 0, 999_000_000),
            obs(1, 0, 999_000_000),
            obs(1, 0, 999_000_000),
            obs(1, 0, 999_000_000),
            obs(2, 0, 100_000_000),
        ];
        let r = analyze(input_gated(
            PaintMode::Coverage,
            vec![0, 1, 2],
            observed,
            None,
            None,
        ));
        assert_eq!(r.freezes.len(), 1);
        assert!(r.latency.as_ref().unwrap().p99_ms > 250.0);
        assert!(r.verdict_pass);
    }

    #[test]
    fn gate_helper_none_bounds_always_pass() {
        let lat = latency_stats(&[1000.0]);
        let freezes = vec![Freeze {
            frame_id: 1,
            repeat_count: 99,
            duration_ms: 0.0,
        }];
        assert!(latency_freeze_gate_pass(&lat, &freezes, None, None));
    }

    #[test]
    fn gate_helper_latency_bound() {
        let lat = latency_stats(&[300.0]); // p99 = 300
        assert!(!latency_freeze_gate_pass(&lat, &[], Some(250.0), None));
        assert!(latency_freeze_gate_pass(&lat, &[], Some(300.0), None));
        // No samples -> latency None -> cannot exceed -> pass.
        assert!(latency_freeze_gate_pass(&None, &[], Some(1.0), None));
    }

    #[test]
    fn gate_helper_freeze_bound() {
        let freezes = vec![Freeze {
            frame_id: 1,
            repeat_count: 5,
            duration_ms: 0.0,
        }];
        assert!(!latency_freeze_gate_pass(&None, &freezes, None, Some(4.0)));
        assert!(latency_freeze_gate_pass(&None, &freezes, None, Some(5.0)));
        assert!(latency_freeze_gate_pass(&None, &[], None, Some(0.0)));
    }

    // --- #20: per-id oversample instrumentation + drop confirmation ---

    /// Build observed for `(id, sample_count)` pairs in ascending id order so
    /// there is no reorder noise; each sample gets a fixed 10 ms latency. Counts
    /// stay <= 3 (under the freeze_periods=3 detector) unless a test wants one.
    fn oversampled(ids_counts: &[(u32, usize)]) -> Vec<Observed> {
        let mut v = Vec::new();
        for &(id, n) in ids_counts {
            let gen = id as i64 * 16_000_000;
            for _ in 0..n {
                v.push(obs(id, gen, gen + 10_000_000));
            }
        }
        v
    }

    #[test]
    fn isolated_single_drop_is_inconclusive() {
        // Oversampled run (2 samples/id, exactly at the floor), id 2 the lone
        // absent id. Indistinguishable from a torn-on-every-sample QR, so it is
        // inconclusive (report-only), NOT a gate failure (#20). p50 == 2 also
        // pins the `>=` oversample bound (a `>` mutant -> not oversampled ->
        // strict -> would wrongly confirm).
        let emitted = vec![0, 1, 2, 3, 4];
        let observed = oversampled(&[(0, 2), (1, 2), (3, 2), (4, 2)]); // id 2 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.oversample_p50, 2);
        assert_eq!(r.coverage.inconclusive_gaps, vec![2]);
        assert!(r.coverage.confirmed_drops.is_empty());
        assert!(r.missing_ids.is_empty());
        assert!(r.verdict_pass);
    }

    #[test]
    fn zero_sample_in_degraded_region_is_inconclusive() {
        // Oversampled run overall, but a local torn span: ids 2,3,4 = 1,0,1
        // samples. The 0-sample id 3 sits between two torn (1-sample) neighbors
        // -> ambiguous, NOT a confirmed pipeline drop -> coverage gate must pass
        // (path b: a torn-QR artifact no longer flakes the zero-loss gate).
        let emitted = vec![0, 1, 2, 3, 4, 5, 6];
        let observed = oversampled(&[(0, 3), (1, 3), (2, 1), (4, 1), (5, 3), (6, 3)]); // 3 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.oversample_p50, 3);
        assert_eq!(r.coverage.inconclusive_gaps, vec![3]);
        assert!(r.coverage.confirmed_drops.is_empty());
        assert!(r.missing_ids.is_empty());
        assert!(r.coverage.low_coverage_ids.contains(&2));
        assert!(r.coverage.low_coverage_ids.contains(&4));
        assert!(r.verdict_pass);
    }

    #[test]
    fn consecutive_drop_is_confirmed() {
        // CRITICAL (#20 review C1): two ADJACENT 0-sample ids in an oversampled
        // run = a real burst loss, not a torn-QR artifact (tearing cannot black
        // out two full hold-windows). Must be CONFIRMED and FAIL the gate, never
        // hidden as inconclusive.
        let emitted = vec![0, 1, 2, 3, 4, 5];
        let observed = oversampled(&[(0, 3), (1, 3), (4, 3), (5, 3)]); // ids 2,3 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.confirmed_drops, vec![2, 3]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert_eq!(r.missing_ids, vec![2, 3]);
        assert!(!r.verdict_pass);
    }

    #[test]
    fn long_burst_is_confirmed() {
        // A 5-frame contiguous loss in an otherwise pristine oversampled run must
        // be fully confirmed (every id), not silently passed.
        let emitted = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let observed = oversampled(&[(0, 3), (1, 3), (2, 3), (8, 3), (9, 3)]); // 3..=7 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.coverage.confirmed_drops, vec![3, 4, 5, 6, 7]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert!(!r.verdict_pass);
    }

    #[test]
    fn alternating_loss_is_confirmed() {
        // CRITICAL (#20 review round 2): every-other-frame loss is a catastrophic
        // real loss, NOT torn-QR noise. Each gap is a length-1 run, but 5 of 10
        // emitted are absent — far over the inconclusive cap -> all confirmed,
        // gate FAILS. (Was silently passing as all-inconclusive.)
        let emitted = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let observed = oversampled(&[(0, 3), (2, 3), (4, 3), (6, 3), (8, 3)]); // odds absent
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.coverage.confirmed_drops, vec![1, 3, 5, 7, 9]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert!(!r.verdict_pass);
    }

    #[test]
    fn tolerance_scales_with_run_length() {
        // Large run (2000 emitted -> tolerance = 2000/1000 = 2). A SINGLE isolated
        // gap (1 < 2, strictly under the cap) stays inconclusive. Pins the cap as
        // `<=`/`<` (vs an `==` that would only tolerate exactly-`tolerance` gaps)
        // and the emitted/DIVISOR scaling (vs the floor-1 small-run case).
        let mut pairs: Vec<(u32, usize)> = (0..2000u32).map(|id| (id, 3)).collect();
        pairs.remove(1000); // id 1000 absent (0 samples)
        let emitted: Vec<u32> = (0..2000u32).collect();
        let r = analyze(input(PaintMode::Coverage, emitted, oversampled(&pairs)));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.inconclusive_gaps, vec![1000]);
        assert!(r.coverage.confirmed_drops.is_empty());
        assert!(r.verdict_pass);
    }

    #[test]
    fn scattered_single_gaps_over_cap_are_confirmed() {
        // Two non-adjacent isolated gaps in a 10-frame run (20% loss). The cap is
        // max(1, emitted/1000) = 1 here, so 2 isolated gaps exceed it -> the gaps
        // are reclassified confirmed (real loss), gate FAILS. A torn-QR run has
        // FAR fewer than 0.1% such gaps.
        let emitted = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let observed = oversampled(&[
            (0, 3),
            (1, 3),
            (3, 3),
            (4, 3),
            (5, 3),
            (6, 3),
            (8, 3),
            (9, 3),
        ]); // 2,7 absent
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.coverage.confirmed_drops, vec![2, 7]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert!(!r.verdict_pass);
    }

    #[test]
    fn boundary_burst_is_confirmed() {
        // A burst at the very start of the sequence (ids 0,1 absent) — the run
        // begins mid-gap. Length 2 -> confirmed, fails the gate. Pins the
        // while-loop's leading-run handling (start index 0).
        let emitted = vec![0, 1, 2, 3, 4];
        let observed = oversampled(&[(2, 3), (3, 3), (4, 3)]); // ids 0,1 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.confirmed_drops, vec![0, 1]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert_eq!(r.missing_ids, vec![0, 1]);
        assert!(!r.verdict_pass);
    }

    #[test]
    fn trailing_single_drop_is_inconclusive() {
        // A lone absent id at the very END of the sequence (run ends mid-gap):
        // still a single isolated gap -> inconclusive. Pins the while-loop's
        // trailing-run handling (run reaching emitted_ids.len()).
        let emitted = vec![0, 1, 2, 3, 4];
        let observed = oversampled(&[(0, 3), (1, 3), (2, 3), (3, 3)]); // id 4 omitted
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.coverage.run_oversampled);
        assert_eq!(r.coverage.inconclusive_gaps, vec![4]);
        assert!(r.coverage.confirmed_drops.is_empty());
        assert!(r.verdict_pass);
    }

    #[test]
    fn oversample_p50_even_length_uses_upper_middle() {
        // Even emitted count: median index len/2 (upper-middle). counts
        // [1,2,3,4] -> sorted same -> index 2 == 3 (not index 1 == 2). Pins the
        // `len()/2` median index against an off-by-one mutant.
        let emitted = vec![0, 1, 2, 3];
        let observed = oversampled(&[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.coverage.oversample_p50, 3);
        assert!(r.coverage.run_oversampled);
        assert!(r.coverage.low_coverage_ids.contains(&0));
    }

    #[test]
    fn coverage_instrumentation_reports_counts() {
        let emitted = vec![0, 1, 2, 3];
        let observed = oversampled(&[(0, 3), (1, 3), (2, 3), (3, 3)]);
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.coverage.min_confirm_samples, 2);
        assert_eq!(r.coverage.oversample_p50, 3);
        assert!(r.coverage.run_oversampled);
        assert!(r.coverage.confirmed_drops.is_empty());
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert!(r.coverage.low_coverage_ids.is_empty());
        assert!(r.verdict_pass);
    }

    #[test]
    fn low_oversample_run_keeps_strict_membership() {
        // p50 < 2 (single-sample synthetic run): the neighbor rule is OFF, every
        // 0-sample id is a confirmed drop (preserves Phase-1 strict semantics and
        // every existing minimal-stream test above).
        let emitted = vec![0, 1, 2, 3];
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 16_000_000, 16_010_000),
            obs(3, 48_000_000, 48_010_000),
        ];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(!r.coverage.run_oversampled);
        assert_eq!(r.coverage.oversample_p50, 1);
        assert_eq!(r.missing_ids, vec![2]);
        assert_eq!(r.coverage.confirmed_drops, vec![2]);
        assert!(r.coverage.inconclusive_gaps.is_empty());
        assert!(!r.verdict_pass);
    }
}
