//! Per-hop NDI-output differencing: detect real single-frame loss between two
//! NDI taps sharing one upstream capture. Pure / unit-tested.
//!
//! Both taps are downstream of the same HDMI/ShadowCast resample, so the
//! resample cancels: `IDs(upstream) − IDs(downstream)` is a clean count of the
//! frames that the hop between the two taps actually dropped — the single-frame
//! accuracy Phase 1's single point could not provide.

use crate::probe::analyzer::{
    detect_freezes, detect_reorders, latency_freeze_gate_pass, latency_stats, Freeze, LatencyStats,
    Observed,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub struct HopInput<'a> {
    pub name: String,
    pub upstream: &'a [Observed],
    pub downstream: &'a [Observed],
    pub capture_fps: f64,
    pub freeze_periods: f64,
    /// A tap that saw fewer than this many run_id-matching frames is treated as
    /// disconnected — the hop FAILS rather than vacuously passing on no data.
    pub min_frames: usize,
    /// Hard gate: fail this hop if its per-hop relative-latency `p99_ms` exceeds
    /// this. `None` ⇒ report-only (the Phase-2 default), mirroring the Phase-1
    /// #10 `max_p99_latency_ms` convention. Strict `>` — a value exactly at the
    /// bound passes.
    pub max_p99_latency_ms: Option<f64>,
    /// Hard gate: fail this hop if any detected freeze's `repeat_count` exceeds
    /// this. `None` ⇒ report-only. Strict `>`.
    pub max_freeze_periods_gate: Option<f64>,
    /// Loss-gate mode. `None` ⇒ STRICT zero-loss: the hop fails on ANY in-span
    /// dropped id (the Phase-2 default, correct for genlocked / local hops).
    /// `Some(pct)` ⇒ DOCUMENTED-BOUND mode: the hop is judged by its
    /// oversample-independent single-copy frame-loss percentage and passes while
    /// that stays `<= pct`. This is for hops with a known, quantified, currently
    /// irreducible loss (e.g. strih→stream's OBS render-clock drop pending
    /// genlock, #8) — it accepts the documented floor yet still catches a
    /// regression past it. `dropped_ids` is always reported either way.
    pub max_loss_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HopReport {
    pub name: String,
    pub upstream_unique: usize,
    pub downstream_unique: usize,
    pub dropped_ids: Vec<u32>,
    pub reorders: Vec<(u32, u32)>,
    pub freezes: Vec<Freeze>,
    pub latency: Option<LatencyStats>,
    /// Upstream ids within the downstream active span carried by exactly ONE
    /// frame (no oversample redundancy). Their loss rate is the unmasked,
    /// oversample-independent estimate of the hop's per-frame drop probability —
    /// the figure that would apply to real 60 fps content where every frame is
    /// unique. A trustworthy zero-loss verdict needs a meaningful count here; a
    /// run with very few single-copy frames cannot certify zero-loss.
    pub single_copy_total: usize,
    /// Of `single_copy_total`, how many are absent downstream.
    pub single_copy_dropped: usize,
    pub pass: bool,
}

fn first_recv(observed: &[Observed]) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for o in observed {
        m.entry(o.frame_id).or_insert(o.recv_ts_ns);
    }
    m
}

pub fn diff_hop(input: HopInput) -> HopReport {
    let up_unique: HashSet<u32> = input.upstream.iter().map(|o| o.frame_id).collect();
    let down_unique: HashSet<u32> = input.downstream.iter().map(|o| o.frame_id).collect();

    // A frame counts as dropped at this hop only if it falls within the
    // downstream tap's *active span* [first id, last id] yet is absent there.
    // The two taps connect and disconnect independently — the OBS-forwarded tap
    // establishes its NDI receive seconds after the direct tap — so ids the
    // downstream tap had no chance to see (before its first decode or after its
    // last) are tap start/stop skew, not hop drops. This is the id-granular
    // complement to the trailing settle-window the orchestrator already applies.
    // A real mid-stream drop still lies inside [lo, hi] and is still flagged.
    let dropped_ids: Vec<u32> = match (down_unique.iter().min(), down_unique.iter().max()) {
        (Some(&lo), Some(&hi)) => {
            let mut v: Vec<u32> = up_unique
                .iter()
                .copied()
                .filter(|id| *id >= lo && *id <= hi && !down_unique.contains(id))
                .collect();
            v.sort_unstable();
            v
        }
        _ => Vec::new(),
    };

    // Single-copy (no-oversample) per-frame loss: of the upstream ids carried by
    // exactly one frame AND lying within the downstream active span, how many are
    // absent downstream. This is the oversample-independent per-frame drop
    // estimate — multi-copy ids survive whenever any one copy lands on a render
    // tick, so only single-copy ids expose the true drop probability.
    let mut up_mult: HashMap<u32, usize> = HashMap::new();
    for o in input.upstream {
        *up_mult.entry(o.frame_id).or_insert(0) += 1;
    }
    let (single_copy_total, single_copy_dropped) =
        match (down_unique.iter().min(), down_unique.iter().max()) {
            (Some(&lo), Some(&hi)) => {
                let singles = up_mult
                    .iter()
                    .filter(|(id, m)| **m == 1 && **id >= lo && **id <= hi);
                let total = singles.clone().count();
                let dropped = singles.filter(|(id, _)| !down_unique.contains(id)).count();
                (total, dropped)
            }
            _ => (0, 0),
        };
    let single_copy_loss_pct = if single_copy_total > 0 {
        100.0 * single_copy_dropped as f64 / single_copy_total as f64
    } else {
        0.0
    };

    let reorders = detect_reorders(input.downstream);
    let freezes = detect_freezes(input.downstream, input.capture_fps, input.freeze_periods);

    // Per-hop latency: downstream arrival − upstream arrival on dev1's single
    // clock, per id present in both taps. First occurrence of each id.
    let up_first = first_recv(input.upstream);
    let down_first = first_recv(input.downstream);
    let mut deltas: Vec<f64> = Vec::new();
    for (id, d_recv) in &down_first {
        if let Some(u_recv) = up_first.get(id) {
            deltas.push((d_recv - u_recv) as f64 / 1_000_000.0);
        }
    }
    let latency = latency_stats(&deltas);

    // Loss gate: STRICT zero (any in-span drop fails) by default; or, when a
    // documented per-hop bound is set, judged by the oversample-independent
    // single-copy loss percentage staying `<= max_loss_pct` (strict `<=`).
    // Reorder is always hard; latency-p99 and freeze are gated only when a
    // per-hop bound is set (None ⇒ report-only).
    let loss_ok = match input.max_loss_pct {
        None => dropped_ids.is_empty(),
        Some(bound) => single_copy_loss_pct <= bound,
    };
    let pass = up_unique.len() >= input.min_frames
        && down_unique.len() >= input.min_frames
        && loss_ok
        && reorders.is_empty()
        && latency_freeze_gate_pass(
            &latency,
            &freezes,
            input.max_p99_latency_ms,
            input.max_freeze_periods_gate,
        );

    HopReport {
        name: input.name,
        upstream_unique: up_unique.len(),
        downstream_unique: down_unique.len(),
        dropped_ids,
        reorders,
        freezes,
        latency,
        single_copy_total,
        single_copy_dropped,
        pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(frame_id: u32, recv_ms: i64) -> Observed {
        Observed {
            frame_id,
            gen_ts_ns: 0,
            recv_ts_ns: recv_ms * 1_000_000,
        }
    }

    fn input<'a>(up: &'a [Observed], down: &'a [Observed]) -> HopInput<'a> {
        HopInput {
            name: "cam→strih".to_string(),
            upstream: up,
            downstream: down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct: None,
        }
    }

    /// A hop whose downstream per-id relative latency p99 == `hi_ms`: four ids at
    /// 10 ms and one at `hi_ms`, so nearest-rank p99 over n=5 lands on `hi_ms`.
    /// `max_p99` is the latency gate under test; the freeze gate is left off.
    fn hop_with_p99(hi_ms: i64, max_p99: Option<f64>) -> HopReport {
        let up = vec![o(0, 0), o(1, 0), o(2, 0), o(3, 0), o(4, 0)];
        let down = vec![o(0, 10), o(1, 10), o(2, 10), o(3, 10), o(4, hi_ms)];
        diff_hop(HopInput {
            name: "cam→strih".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: max_p99,
            max_freeze_periods_gate: None,
            max_loss_pct: None,
        })
    }

    /// A hop whose downstream holds id 2 for `repeats` consecutive frames — a
    /// freeze of `repeat_count == repeats` (freeze_periods = 3 < repeats). No
    /// drop, no reorder, so the freeze gate is the only possible failure cause.
    fn hop_with_freeze(repeats: usize, max_freeze: Option<f64>) -> HopReport {
        let up = vec![o(0, 0), o(1, 1), o(2, 2), o(3, 3)];
        let mut down = vec![o(0, 10), o(1, 11)];
        for k in 0..repeats {
            down.push(o(2, 12 + k as i64));
        }
        down.push(o(3, 99));
        diff_hop(HopInput {
            name: "cam→strih".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: max_freeze,
            max_loss_pct: None,
        })
    }

    #[test]
    fn per_hop_latency_p99_over_bound_fails() {
        // p99 = 300 ms, bound 250 ms → FAIL despite zero loss/reorder.
        let r = hop_with_p99(300, Some(250.0));
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 300.0);
        assert!(r.dropped_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(!r.pass, "p99 300 > bound 250 must FAIL the hop");
    }

    #[test]
    fn per_hop_latency_p99_at_bound_passes() {
        // p99 = 250 ms, bound 250 ms → PASS (strict `>`, not `>=`).
        let r = hop_with_p99(250, Some(250.0));
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 250.0);
        assert!(r.pass, "p99 250 == bound 250 must PASS (strict >)");
    }

    #[test]
    fn per_hop_freeze_over_bound_fails() {
        // freeze repeat_count 6, bound 5 → FAIL despite zero loss/reorder.
        let r = hop_with_freeze(6, Some(5.0));
        assert_eq!(r.freezes.len(), 1);
        assert_eq!(r.freezes[0].repeat_count, 6);
        assert!(r.dropped_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(!r.pass, "freeze repeat_count 6 > bound 5 must FAIL the hop");
    }

    #[test]
    fn per_hop_freeze_at_bound_passes() {
        // freeze repeat_count 5, bound 5 → PASS (strict `>`, not `>=`).
        let r = hop_with_freeze(5, Some(5.0));
        assert_eq!(r.freezes[0].repeat_count, 5);
        assert!(
            r.pass,
            "freeze repeat_count 5 == bound 5 must PASS (strict >)"
        );
    }

    #[test]
    fn none_bounds_are_report_only_phase2_default() {
        // High latency AND a real freeze, but both bounds None → report-only;
        // the hop still PASSes, preserving the Phase-2 loss+reorder-only default.
        let r_lat = hop_with_p99(9999, None);
        assert!(r_lat.latency.as_ref().unwrap().p99_ms > 250.0);
        assert!(r_lat.pass, "None latency bound must stay report-only");
        let r_frz = hop_with_freeze(99, None);
        assert!(!r_frz.freezes.is_empty());
        assert!(r_frz.pass, "None freeze bound must stay report-only");
    }

    /// Build a hop with one heavily-oversampled anchor id (3 copies, always
    /// delivered) plus `n` single-copy upstream ids, of which the first `drop_k`
    /// are absent downstream (their sole frame dropped). Models the real pipeline:
    /// per-frame loss is only exposed on frames that lack oversample redundancy,
    /// so the masking-aware metric must count exactly those.
    fn hop_single_copy(n: usize, drop_k: usize, max_loss_pct: Option<f64>) -> HopReport {
        let mut up = vec![o(0, 0), o(0, 1), o(0, 2)]; // oversampled anchor, never lost
        for k in 1..=n {
            up.push(o(k as u32, (2 + k) as i64));
        }
        let mut down = vec![o(0, 10)];
        for k in 1..=n {
            if k > drop_k {
                down.push(o(k as u32, (12 + k) as i64));
            }
        }
        diff_hop(HopInput {
            name: "strih→stream".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct,
        })
    }

    #[test]
    fn single_copy_loss_is_counted_unmasked() {
        // 10 single-copy ids, 2 dropped. The 3-copy anchor id 0 survives and must
        // NOT inflate the single-copy denominator.
        let r = hop_single_copy(10, 2, None);
        assert_eq!(r.single_copy_total, 10);
        assert_eq!(r.single_copy_dropped, 2);
    }

    #[test]
    fn loss_pct_bound_accepts_documented_irreducible_loss() {
        // 2/100 single-copy frames dropped = 2%; bound 5% → PASS even though
        // dropped_ids is non-empty. This is the documented-bound gate mode (#21):
        // strih→stream's genlock-bound per-frame loss is accepted up to the bound.
        let r = hop_single_copy(100, 2, Some(5.0));
        assert!(!r.dropped_ids.is_empty());
        assert_eq!(r.single_copy_total, 100);
        assert_eq!(r.single_copy_dropped, 2);
        assert!(r.pass, "2% single-copy loss under a 5% bound must PASS");
    }

    #[test]
    fn loss_pct_bound_fails_on_regression_above_bound() {
        // 8/100 = 8% > 5% bound → FAIL (catches regression past the documented bound).
        let r = hop_single_copy(100, 8, Some(5.0));
        assert!(!r.pass, "8% single-copy loss over a 5% bound must FAIL");
    }

    #[test]
    fn none_loss_bound_keeps_strict_zero_loss() {
        // Default (None) keeps the strict dropped_ids-empty gate: ANY drop FAILs,
        // preserving the Phase-2 zero-loss default for hops without a bound.
        let r = hop_single_copy(10, 1, None);
        assert!(!r.pass, "None bound must stay strict zero-loss");
    }

    #[test]
    fn loss_pct_bound_passes_when_no_single_copy_frames() {
        // Every upstream id is oversampled (×2) and delivered → single_copy_total=0.
        // The documented-bound gate must PASS on 0 loss, NOT divide 0/0 into a NaN
        // that fails the comparison. Pins `single_copy_total > 0` (not `>= 0`).
        let up = vec![o(0, 0), o(0, 1), o(1, 2), o(1, 3), o(2, 4), o(2, 5)];
        let down = vec![o(0, 10), o(1, 12), o(2, 14)];
        let r = diff_hop(HopInput {
            name: "strih→stream".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct: Some(5.0),
        });
        assert_eq!(r.single_copy_total, 0);
        assert!(
            r.pass,
            "no single-copy frames + zero loss under bound must PASS, not NaN-fail"
        );
    }

    #[test]
    fn loss_pct_bound_at_exact_bound_passes() {
        // 5/100 single-copy frames dropped = exactly 5.0%; bound 5.0 → PASS
        // (strict `<=`, not `<`). Pins the bound comparison boundary.
        let r = hop_single_copy(100, 5, Some(5.0));
        assert_eq!(r.single_copy_dropped, 5);
        assert_eq!(r.single_copy_total, 100);
        assert!(
            r.pass,
            "single-copy loss exactly at the bound must PASS (<=)"
        );
    }

    #[test]
    fn clean_hop_passes_no_drops() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(r.pass);
        assert!(r.dropped_ids.is_empty());
        assert_eq!(r.upstream_unique, 4);
        assert_eq!(r.downstream_unique, 4);
    }

    #[test]
    fn single_frame_drop_downstream_is_detected() {
        // id 2 present upstream, absent downstream → the hop dropped it.
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.dropped_ids, vec![2]);
    }

    #[test]
    fn resample_dups_present_in_both_are_not_drops() {
        // id 1 duplicated by the resample at both taps → no drop, PASS.
        let up = vec![o(0, 0), o(1, 33), o(1, 40), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(1, 50), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(r.pass);
        assert!(r.dropped_ids.is_empty());
    }

    #[test]
    fn unequal_dup_counts_are_not_a_drop() {
        // id 1 twice upstream, once downstream — still PRESENT downstream, so it
        // is not a drop. Pins set-difference (membership) semantics: a multiset /
        // count-based differ would wrongly flag id 1 here.
        let up = vec![o(0, 0), o(1, 33), o(1, 40), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(r.pass);
        assert!(r.dropped_ids.is_empty());
    }

    #[test]
    fn reorder_on_downstream_fails() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(2, 43), o(1, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.reorders, vec![(2, 1)]);
    }

    #[test]
    fn empty_downstream_fails_min_frames_not_vacuous() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down: Vec<Observed> = vec![];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.downstream_unique, 0);
    }

    #[test]
    fn per_hop_latency_is_downstream_minus_upstream() {
        // each id arrives 10 ms later downstream → mean 10 ms.
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        let l = r.latency.unwrap();
        assert_eq!(l.samples, 3);
        assert!((l.mean_ms - 10.0).abs() < 0.001);
        assert!((l.max_ms - 10.0).abs() < 0.001);
    }

    #[test]
    fn startup_skew_before_downstream_first_id_is_not_a_drop() {
        // Downstream tap (OBS-forwarded) connected late: it never saw ids 0..4.
        // Those are start skew, not drops — only ids within [5, 9] are judged.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(5, 15), o(6, 16), o(7, 17), o(8, 18), o(9, 19)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }

    #[test]
    fn shutdown_skew_after_downstream_last_id_is_not_a_drop() {
        // Downstream stopped at id 5 (in-flight tail); ids 6..9 are end skew.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(0, 10), o(1, 11), o(2, 12), o(3, 13), o(4, 14), o(5, 15)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }

    #[test]
    fn real_drop_inside_active_span_still_fails() {
        // Skew at both ends (down starts at 2, ends at 8) AND a genuine drop of
        // id 5 in the middle. The skew is excluded; the real drop is caught.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(2, 12), o(3, 13), o(4, 14), o(6, 16), o(7, 17), o(8, 18)];
        let r = diff_hop(input(&up, &down));
        assert_eq!(r.dropped_ids, vec![5]);
        assert!(!r.pass);
    }

    #[test]
    fn drop_exactly_at_span_boundaries_is_excluded() {
        // Pins the inclusive bounds: ids equal to lo (2) and hi (8) that are
        // present downstream are not drops; the `>= lo` / `<= hi` comparisons
        // must stay inclusive (kills boundary mutants).
        let up = vec![
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
        ];
        let down = vec![
            o(2, 12),
            o(3, 13),
            o(4, 14),
            o(5, 15),
            o(6, 16),
            o(7, 17),
            o(8, 18),
        ];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }
}
