//! Per-hop NDI-output differencing: detect real single-frame loss between two
//! NDI taps sharing one upstream capture. Pure / unit-tested.
//!
//! Both taps are downstream of the same HDMI/ShadowCast resample, so the
//! resample cancels: `IDs(upstream) − IDs(downstream)` is a clean count of the
//! frames that the hop between the two taps actually dropped — the single-frame
//! accuracy Phase 1's single point could not provide.

use crate::probe::analyzer::{
    detect_freezes, detect_reorders, latency_stats, Freeze, LatencyStats, Observed,
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

    let pass = up_unique.len() >= input.min_frames
        && down_unique.len() >= input.min_frames
        && dropped_ids.is_empty()
        && reorders.is_empty();

    HopReport {
        name: input.name,
        upstream_unique: up_unique.len(),
        downstream_unique: down_unique.len(),
        dropped_ids,
        reorders,
        freezes,
        latency,
        pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(frame_id: u32, recv_ms: i64) -> Observed {
        Observed { frame_id, gen_ts_ns: 0, recv_ts_ns: recv_ms * 1_000_000 }
    }

    fn input<'a>(up: &'a [Observed], down: &'a [Observed]) -> HopInput<'a> {
        HopInput {
            name: "cam→strih".to_string(),
            upstream: up,
            downstream: down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
        }
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
        let up = vec![o(0, 0), o(1, 1), o(2, 2), o(3, 3), o(4, 4),
                      o(5, 5), o(6, 6), o(7, 7), o(8, 8), o(9, 9)];
        let down = vec![o(5, 15), o(6, 16), o(7, 17), o(8, 18), o(9, 19)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }

    #[test]
    fn shutdown_skew_after_downstream_last_id_is_not_a_drop() {
        // Downstream stopped at id 5 (in-flight tail); ids 6..9 are end skew.
        let up = vec![o(0, 0), o(1, 1), o(2, 2), o(3, 3), o(4, 4), o(5, 5),
                      o(6, 6), o(7, 7), o(8, 8), o(9, 9)];
        let down = vec![o(0, 10), o(1, 11), o(2, 12), o(3, 13), o(4, 14), o(5, 15)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }

    #[test]
    fn real_drop_inside_active_span_still_fails() {
        // Skew at both ends (down starts at 2, ends at 8) AND a genuine drop of
        // id 5 in the middle. The skew is excluded; the real drop is caught.
        let up = vec![o(0, 0), o(1, 1), o(2, 2), o(3, 3), o(4, 4), o(5, 5),
                      o(6, 6), o(7, 7), o(8, 8), o(9, 9)];
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
        let up = vec![o(2, 2), o(3, 3), o(4, 4), o(5, 5), o(6, 6), o(7, 7), o(8, 8)];
        let down = vec![o(2, 12), o(3, 13), o(4, 14), o(5, 15), o(6, 16), o(7, 17), o(8, 18)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.pass);
    }
}
