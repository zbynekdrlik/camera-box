//! #726 — presentation-cadence EVENNESS metric.
//!
//! Mirrors the `painted_tick_gaps.rs` / `reannounce.rs` / `colour_scale.rs` Tier-0 seam pattern:
//! the WHOLE `probe` module is `#[cfg(feature = "probe")]` (CI-only, never compiled/tested
//! locally per CLAUDE.md's Local Build Policy), so the PURE decision logic lives here at the
//! crate root where it unit-tests on DEFAULT features.
//!
//! ## The symptom this measures (live event, 2026-07-12)
//!
//! strih/stream's output stuttered "like 15fps" at its normal 30fps canvas during a broadcast;
//! switching strih to 60fps mitigated it live (strih is designed to run 30fps — Topology v2,
//! #459/#466 — and was switched back afterward). The existing zero-loss / A/V-sync / continuity
//! gates (`full_chain`, `all_cambox_continuity`) were BLIND to this failure: they prove no NET
//! frame was lost and no A/V drift happened, but say nothing about whether the 30 frames/sec that
//! DID arrive were presented EVENLY. A perfectly loss-free recording can still look like half its
//! real frame rate if every other canvas tick silently re-presents the PREVIOUS frame's content
//! instead of a fresh one — mechanically 30fps, visually ~15fps.
//!
//! ## What "even" means here
//!
//! Given the per-frame painted-tick VALUES of a recording in RECORDED order (the same
//! `SegmentFrame.tick` sequence `probe::recording_segments::window_segment` already extracts from
//! cam2's own 60fps painter, decimated into the canvas at `expected_step` painted-ticks per
//! recorded frame — 2 for a 60fps painter into a 30fps canvas), a SMOOTH downsample advances the
//! tick by exactly `expected_step` on EVERY consecutive recorded frame (uniform cadence: the
//! canvas always shows the newest available source frame). A JUDDERY ("15fps-like") downsample
//! instead re-presents the SAME tick value on one canvas frame (delta 0 — a visual duplicate,
//! already flagged by `CamboxSegment.copies`) immediately followed by a compensating DOUBLE jump
//! (delta `2 * expected_step`) that catches the tick count back up to the true net position —
//! mechanically still 30 distinct output frames/sec (no NET loss, `copies`/`gaps` alone can look
//! clean), but visually pairs every two canvas frames into one presented image, halving the
//! perceived motion rate — exactly the "paired spacing" the issue names.
//!
//! [`measure_cadence_evenness`] classifies every consecutive-frame delta and reports the
//! fractions — this module NEVER gates on its own (the threshold is not yet calibrated against a
//! known-healthy run); callers report the numbers first (issue #726 deliverable 1) and add a gate
//! threshold once a healthy baseline is measured on the rig.

/// Per-recording (or per-window) presentation-cadence classification, built from a sequence of
/// painted-tick values in RECORDED (delivery) order — NOT sorted, unlike
/// `painted_tick_gaps::painted_tick_gaps`'s net-loss accounting, because cadence EVENNESS is
/// inherently an order-dependent question (a sorted sequence cannot tell smooth from paired).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct CadenceEvenness {
    /// The nominal painted-tick step per recorded frame this recording was decimated at (e.g. 2
    /// for a 60fps painter into a 30fps canvas). Echoed for the report.
    pub expected_step: i64,
    /// Number of consecutive-frame deltas evaluated (`ticks.len() - 1`).
    pub sample_deltas: usize,
    /// Deltas exactly `expected_step` — a fresh, on-cadence frame.
    pub uniform_steps: usize,
    /// Deltas exactly `0` — the SAME tick re-presented (a visual duplicate of the prior frame).
    pub duplicate_steps: usize,
    /// Deltas exactly `2 * expected_step` — a compensating double jump (net position recovered).
    pub catchup_steps: usize,
    /// Anything else — decode noise, a genuinely missing/extra tick, or a bigger irregular jump.
    /// Never itself proof of loss (see `painted_tick_gaps` for the net-loss accounting); just not
    /// one of the three clean cadence shapes above.
    pub other_steps: usize,
    /// Adjacent `(duplicate, catchup)` pairs — i.e. `deltas[i] == 0 && deltas[i+1] == 2 *
    /// expected_step` — the SPECIFIC "15fps-like" signature: a held frame immediately compensated
    /// by a double-step jump, as opposed to an isolated duplicate with no adjacent catch-up
    /// (which may be a genuine unrecovered stutter rather than this pacing artifact).
    pub paired_events: usize,
    pub uniform_fraction: f64,
    pub duplicate_fraction: f64,
    /// Fraction of ALL deltas that participate in a paired duplicate+catchup event
    /// (`paired_events * 2 / sample_deltas`) — both halves of the pair are "judder", not just the
    /// duplicate half.
    pub paired_fraction: f64,
    /// The headline number: `uniform_fraction`, restated for readability. `1.0` = every frame was
    /// perfectly on-cadence (smooth); `0.0` = no frame was ever on-cadence.
    pub evenness_score: f64,
}

/// Classify the presentation cadence of `ticks` (painted-tick values in RECORDED order).
///
/// Returns `None` when there isn't enough data to say anything (`ticks.len() < 2`) or
/// `expected_step` is non-positive (not a valid decimation ratio) — e.g. a schedule window where
/// this cambox never carried the painted tick (a non-cam2 window: `ticks` is empty because every
/// frame's `tick` was `None`). A caller should treat `None` as "not applicable to this window",
/// never as a failure.
pub fn measure_cadence_evenness(ticks: &[u32], expected_step: i64) -> Option<CadenceEvenness> {
    if ticks.len() < 2 || expected_step <= 0 {
        return None;
    }

    let deltas: Vec<i64> = ticks
        .windows(2)
        .map(|w| i64::from(w[1]) - i64::from(w[0]))
        .collect();
    let n = deltas.len();

    let mut uniform = 0usize;
    let mut duplicate = 0usize;
    let mut catchup = 0usize;
    let mut other = 0usize;
    let mut paired = 0usize;

    for i in 0..n {
        let d = deltas[i];
        if d == expected_step {
            uniform += 1;
        } else if d == 0 {
            duplicate += 1;
            if i + 1 < n && deltas[i + 1] == 2 * expected_step {
                paired += 1;
            }
        } else if d == 2 * expected_step {
            catchup += 1;
        } else {
            other += 1;
        }
    }

    let nf = n as f64;
    Some(CadenceEvenness {
        expected_step,
        sample_deltas: n,
        uniform_steps: uniform,
        duplicate_steps: duplicate,
        catchup_steps: catchup,
        other_steps: other,
        paired_events: paired,
        uniform_fraction: uniform as f64 / nf,
        duplicate_fraction: duplicate as f64 / nf,
        paired_fraction: (paired * 2) as f64 / nf,
        evenness_score: uniform as f64 / nf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- degenerate inputs -------------------------------------------------------------

    #[test]
    fn too_few_ticks_returns_none() {
        assert_eq!(measure_cadence_evenness(&[], 2), None);
        assert_eq!(measure_cadence_evenness(&[10], 2), None);
    }

    #[test]
    fn non_positive_expected_step_returns_none() {
        assert_eq!(measure_cadence_evenness(&[0, 2, 4], 0), None);
        assert_eq!(measure_cadence_evenness(&[0, 2, 4], -2), None);
    }

    #[test]
    fn empty_window_no_painted_tick_present_is_none() {
        // e.g. a non-cam2 CAMBOX_SWEEP window: every SegmentFrame.tick is None, so the caller's
        // `present_ticks` filter produces an empty slice — this must read as "not applicable",
        // never as a judder verdict.
        let ticks: [u32; 0] = [];
        assert_eq!(measure_cadence_evenness(&ticks, 2), None);
    }

    // ---- the two named reference patterns (issue #726, verbatim) ---------------------------

    #[test]
    fn smooth_30_from_60fps_source_is_uniform_2_tick_cadence() {
        // 60fps painter decimated smoothly into a 30fps canvas: every recorded frame advances by
        // exactly `expected_step` (2) — the "smooth 30" reference shape.
        let ticks: Vec<u32> = (0..60).step_by(2).collect(); // 0,2,4,...,58 (30 values)
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.sample_deltas, 29);
        assert_eq!(v.uniform_steps, 29);
        assert_eq!(v.duplicate_steps, 0);
        assert_eq!(v.catchup_steps, 0);
        assert_eq!(v.other_steps, 0);
        assert_eq!(v.paired_events, 0);
        assert_eq!(v.uniform_fraction, 1.0);
        assert_eq!(v.duplicate_fraction, 0.0);
        assert_eq!(v.paired_fraction, 0.0);
        assert_eq!(v.evenness_score, 1.0);
    }

    #[test]
    fn fifteen_fps_like_judder_is_paired_spacing() {
        // Every source frame held for TWO canvas ticks then a compensating double jump: the SAME
        // tick value presented twice (0, then 4, then 4 again is wrong — must be: 0,0,4,4,8,8,...)
        // — deltas: 0,4,0,4,0,4,... — the exact "paired spacing" the issue names, and mechanically
        // still 30 distinct recorded frames (no net loss) despite halving perceived motion.
        let mut ticks = Vec::new();
        for k in 0..15u32 {
            let t = k * 4;
            ticks.push(t);
            ticks.push(t); // held (duplicate content)
        }
        // ticks = [0,0,4,4,8,8,...,56,56] (30 values) -> deltas = [0,4,0,4,...,0,4] minus a
        // trailing element (29 deltas: last delta is between the final pair's second 56 and...
        // there is no further element, so the sequence naturally ends on a duplicate delta 0).
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.sample_deltas, 29);
        assert_eq!(
            v.uniform_steps, 0,
            "a paired-judder recording has NO on-cadence frames"
        );
        assert_eq!(v.duplicate_steps, 15);
        assert_eq!(v.catchup_steps, 14); // the last duplicate (idx 28) has no following delta
        assert_eq!(v.paired_events, 14);
        assert_eq!(v.evenness_score, 0.0);
        assert!(
            (v.paired_fraction - (28.0 / 29.0)).abs() < 1e-9,
            "28 of 29 deltas participate in a paired duplicate+catchup event, got {}",
            v.paired_fraction
        );
    }

    // ---- mixed / partial patterns ------------------------------------------------------

    #[test]
    fn partially_juddery_recording_reports_fractional_evenness() {
        // First half smooth, second half paired — proves the metric reports a genuine MIX, not
        // just the two extremes.
        let mut ticks: Vec<u32> = (0..20).step_by(2).collect(); // 10 smooth values, 0..18
        let base = *ticks.last().unwrap();
        for k in 1..=5u32 {
            let t = base + k * 4;
            ticks.push(t);
            ticks.push(t);
        }
        // smooth run: 9 uniform deltas. Then transition delta (18 -> 22) is itself uniform (=2)
        // too since base+4 - base = 4 != 2... let's not hand-derive it in the assertion; just
        // assert the mix is neither 0 nor 1 and the counts add up.
        let v = measure_cadence_evenness(&ticks, 2).expect("enough samples");
        assert!(v.evenness_score > 0.0 && v.evenness_score < 1.0);
        assert_eq!(
            v.uniform_steps + v.duplicate_steps + v.catchup_steps + v.other_steps,
            v.sample_deltas
        );
    }

    #[test]
    fn isolated_duplicate_with_no_adjacent_catchup_is_not_counted_paired() {
        // A single held frame that does NOT get a clean compensating double-jump right after it
        // (e.g. the run just continues on-cadence from the held value, net-losing one tick
        // somewhere else) must NOT be misclassified as the paired "15fps" signature — it's some
        // OTHER shape and paired_events must stay 0 for this local pattern.
        let ticks: Vec<u32> = vec![0, 0, 2, 4, 6]; // deltas: 0, 2, 2, 2
        let v = measure_cadence_evenness(&ticks, 2).unwrap();
        assert_eq!(v.duplicate_steps, 1);
        assert_eq!(
            v.paired_events, 0,
            "delta after the duplicate is 2 (uniform), not a 4 catchup"
        );
        assert_eq!(v.uniform_steps, 3);
    }

    #[test]
    fn reports_are_stable_regardless_of_absolute_tick_offset() {
        // The metric must depend only on RELATIVE spacing, not on the absolute starting tick
        // value (a painted tick is a free-running counter that never resets to 0 mid-recording).
        let a = measure_cadence_evenness(&[0, 2, 4, 6, 8], 2).unwrap();
        let b =
            measure_cadence_evenness(&[100_000, 100_002, 100_004, 100_006, 100_008], 2).unwrap();
        assert_eq!(a.uniform_steps, b.uniform_steps);
        assert_eq!(a.evenness_score, b.evenness_score);
    }

    #[test]
    fn other_bucket_catches_irregular_jumps_that_are_neither_clean_shape() {
        // A genuinely irregular delta (neither 0, nor expected_step, nor 2*expected_step) — e.g.
        // decode noise producing an odd tick value — must land in `other`, not silently vanish
        // into one of the clean buckets.
        let ticks: Vec<u32> = vec![0, 2, 5, 7]; // deltas: 2, 3, 2 -> the middle one is irregular
        let v = measure_cadence_evenness(&ticks, 2).unwrap();
        assert_eq!(v.uniform_steps, 2);
        assert_eq!(v.other_steps, 1);
        assert_eq!(v.duplicate_steps, 0);
        assert_eq!(v.catchup_steps, 0);
    }
}
