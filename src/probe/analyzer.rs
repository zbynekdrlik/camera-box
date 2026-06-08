//! Correlate emitted vs observed frame IDs → loss / freeze / reorder + latency.

use serde::Serialize;
use std::collections::HashSet;

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
    pub freeze_periods: f64,
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

pub fn analyze(input: AnalysisInput) -> AnalysisReport {
    let emitted_set: HashSet<u32> = input.emitted_ids.iter().copied().collect();
    let observed_set: HashSet<u32> = input.observed.iter().map(|o| o.frame_id).collect();

    let missing_ids: Vec<u32> = input
        .emitted_ids
        .iter()
        .copied()
        .filter(|id| !observed_set.contains(id))
        .collect();

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
    let verdict_pass = match input.mode {
        PaintMode::Coverage => {
            !emitted_set.is_empty() && missing_ids.is_empty() && reorders.is_empty()
        }
        PaintMode::FullRate => reorders.is_empty(),
    };

    AnalysisReport {
        mode: input.mode,
        emitted_count: emitted_set.len(),
        observed_count: input.observed.len(),
        unique_observed: observed_set.len(),
        missing_ids,
        reorders,
        freezes,
        latency,
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
}
