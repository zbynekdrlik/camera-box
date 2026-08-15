//! #870 — per-hop burn-id UNIQUENESS / MAX-HOLD assertion.
//!
//! The #186 zero-loss check ([`crate::probe::burn_contiguity::burn_contiguity`]) proves each
//! node's burn-id SET is CONTIGUOUS (`first..=last`, every integer present). It collapses the
//! decoded ids into a `BTreeSet` first, so it is presence-only and order-independent: a hop that
//! REPEATS frames — the identical rendered image delivered on many consecutive recorded frames —
//! adds no new missing id, so contiguity stays satisfied and `full_chain.loss.<node>` reads clean
//! (`real_drops == 0`, `present_count == expected_count`). This blindness hid a real defect for
//! three days (#707): run 396782734's stream recording carried the byte-identical strih burn id on
//! 61% of consecutive frames (3703 distinct ids across 9487 frames) while every burn-contiguity
//! term reported zero loss; the only term that noticed was the painted OPTICAL tick, dismissed as
//! an optical-leg artifact.
//!
//! ## Why a DUPLICATE-RUN metric, not a rate
//!
//! The burn `frame_id` (`vendor/distroav/src/ndi-burn-filter.cpp`, `f->frame_id++`) increments
//! once per `video_render` of that filter — Program plus the Multiview grid's own throttled pass —
//! so its rate is unwritable (41.7–147.9 ids/s across the ten program segments of ONE run, #870).
//! But the burn is COMPOSITED into the image, so the SAME `(run_id, frame_id)` pair on two
//! consecutive recorded frames means the identical upstream RENDERED IMAGE was delivered twice.
//! That inference holds regardless of the counter's rate. So the metric is the run-length
//! distribution of consecutive frames sharing one burn id — a per-hop MAX-HOLD.
//!
//! ## The legit bound
//!
//! Topology v2 (#459/#466): both strih and stream RECORD at 30fps and every node burn is DECIMATED
//! 60→30 into the stream recording, so consecutive KEPT ids STEP (distinct) — a burn id
//! legitimately occupies exactly ONE recorded frame. A genuine 30→60 upsample somewhere could hold
//! a frame for 2; it may never hold it for 5 (#870). [`MAX_HOLD_FRAMES`] = 4 clears any legit
//! transient / genlock-FIFO convergence hold with margin and is consistent with
//! [`crate::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN`] (3 Δ0-pairs = 4 frames), the sibling
//! run-length gate for the imag OPTICAL tick — this module is its per-hop NODE-BURN counterpart.
//!
//! ## Report-only today (issue 870)
//!
//! [`gates_overall_pass`] returns `false`: the term is fully computed and serialized into the
//! verdict JSON (`full_chain.loss.<node>.hold.*`) but does NOT fold into `overall_pass` yet. Per
//! `verdict-gate-seam-calibration.md` §5 a LIVE flip needs the field's green-run distribution
//! mined and proven to pass every recent green run with margin AND survive the cam1-grabber defect
//! (issue 909); `max_hold_frames` is a brand-new field absent from every existing verdict JSON, so
//! that proof cannot be assembled today. Flip to `true` (a one-line revert) once accumulated green
//! runs confirm the bound — tracked on #870.
//!
//! This is the PURE decision core; it compiles + unit-tests on DEFAULT features (the whole `probe`
//! module is CI-only per CLAUDE.md's Local Build Policy). The probe-gated consumer
//! (`src/bin/recording-verdict.rs`) feeds it `recording_latency::burn_ids_in(stream_frames,
//! run_id)` — the recorded-ORDER extractor already used for contiguity, so no new decode.

use std::collections::{BTreeMap, BTreeSet};

/// The maximum number of consecutive recorded frames a single burn id may legitimately occupy
/// before it is a REPEAT (a frozen / re-delivered rendered image). See the module doc for the
/// calibration: legit hold is 1 in the decimated stream recording; 4 clears any transient with
/// margin, matches the ticket's "may hold for 2, never 5", and mirrors
/// [`crate::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN`] (= 4 frames).
pub const MAX_HOLD_FRAMES: u32 = 4;

/// The consecutive-duplicate run-length distribution of one node's burn-id sequence in a recording
/// (#870). Built from the recorded-ORDER ids by [`burn_hold_distribution`]. All counts are `u32`;
/// the duplicate FRACTION is a derived method so the struct carries no float (keeps `PartialEq`
/// exact and avoids the `Eq`-on-float trap).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HoldDistribution {
    /// Node label, e.g. `"strih"`, `"stream"`, `"cam2"`.
    pub node: String,
    /// Total burn frames analyzed (one entry per recorded frame carrying this node's burn).
    pub total_burn_frames: u32,
    /// Distinct burn ids seen.
    pub distinct_ids: u32,
    /// The longest run of consecutive frames sharing one burn id, in FRAMES. `0` for an empty
    /// sequence, `1` for a clean sequence where every id is distinct (the legit decimated case).
    pub max_hold_frames: u32,
    /// The burn id that held longest (first one on a tie). `None` for an empty sequence.
    pub max_hold_id: Option<u32>,
    /// Adjacent identical pairs (`ids[i] == ids[i-1]`) — the "byte-identical consecutive frames"
    /// numerator. For a run of length L this contributes L-1.
    pub duplicate_pairs: u32,
    /// Total adjacent pairs walked (`total_burn_frames - 1`, `0` for a 0/1-frame sequence) — the
    /// [`duplicate_pair_fraction`](Self::duplicate_pair_fraction) denominator.
    pub adjacent_pairs: u32,
    /// The full run-length histogram, `(hold_frames, count)` ascending by `hold_frames` — the
    /// distribution the ticket asks to REPORT so a degradation is visible before it crosses the
    /// bound.
    pub histogram: Vec<(u32, u32)>,
}

impl HoldDistribution {
    /// Fraction of adjacent frame pairs that were byte-identical burn ids — run 396782734's
    /// headline "61%". `0.0` when there are no pairs (0/1-frame sequence).
    pub fn duplicate_pair_fraction(&self) -> f64 {
        if self.adjacent_pairs == 0 {
            0.0
        } else {
            self.duplicate_pairs as f64 / self.adjacent_pairs as f64
        }
    }

    /// The measured max hold as an `Option` for the gate: `None` when no burn frame was analyzed
    /// (nothing proven — a hop with no readable burn cannot be judged for repeats), else
    /// `Some(max_hold_frames)`.
    pub fn measured_max_hold(&self) -> Option<u32> {
        (self.total_burn_frames > 0).then_some(self.max_hold_frames)
    }

    /// Does this hop's max hold satisfy the given bound? Convenience over
    /// [`hold_gate_pass`]`(self.measured_max_hold(), Some(bound))`.
    pub fn within_bound(&self, bound: u32) -> bool {
        hold_gate_pass(self.measured_max_hold(), Some(bound))
    }
}

/// Build the consecutive-duplicate run-length distribution for one node's recorded-ORDER burn ids.
///
/// ONE walk over the sequence groups it into maximal runs of an identical id; each run of length L
/// contributes one histogram entry at `L`, `L-1` duplicate pairs, and updates the running max hold.
/// The longest run wins `max_hold_frames` / `max_hold_id` (first one on a tie — strict `>`).
/// Program-switch segmentation is inherent: a switch changes the filter instance so the `frame_id`
/// jumps/resets ⇒ a different id ⇒ the run breaks; the only over-count risk (two segments' counters
/// coincidentally EQUAL across the boundary) is bounded to +1 per switch and is harmless while the
/// term is report-only ([`gates_overall_pass`]).
///
/// An empty input ⇒ an all-zero distribution (`max_hold_frames == 0`, [`HoldDistribution::
/// measured_max_hold`] `== None`) — nothing was proven, and the gate PASSES it (see
/// [`hold_gate_pass`]).
pub fn burn_hold_distribution(node: &str, ids_in_recorded_order: &[u32]) -> HoldDistribution {
    let total_burn_frames = ids_in_recorded_order.len() as u32;
    let distinct_ids = ids_in_recorded_order
        .iter()
        .copied()
        .collect::<BTreeSet<u32>>()
        .len() as u32;
    let adjacent_pairs = total_burn_frames.saturating_sub(1);

    let mut histogram_map: BTreeMap<u32, u32> = BTreeMap::new();
    let mut max_hold_frames = 0u32;
    let mut max_hold_id: Option<u32> = None;
    let mut duplicate_pairs = 0u32;

    // Close a finished run of `run_len` frames all carrying `run_id`: record it in the histogram
    // and promote it if it is the longest hold seen so far.
    let close_run = |run_id: u32,
                     run_len: u32,
                     histogram_map: &mut BTreeMap<u32, u32>,
                     max_hold_frames: &mut u32,
                     max_hold_id: &mut Option<u32>| {
        *histogram_map.entry(run_len).or_insert(0) += 1;
        if run_len > *max_hold_frames {
            *max_hold_frames = run_len;
            *max_hold_id = Some(run_id);
        }
    };

    let mut iter = ids_in_recorded_order.iter().copied();
    if let Some(first) = iter.next() {
        let mut run_id = first;
        let mut run_len = 1u32;
        for id in iter {
            if id == run_id {
                run_len += 1;
                duplicate_pairs += 1;
            } else {
                close_run(
                    run_id,
                    run_len,
                    &mut histogram_map,
                    &mut max_hold_frames,
                    &mut max_hold_id,
                );
                run_id = id;
                run_len = 1;
            }
        }
        close_run(
            run_id,
            run_len,
            &mut histogram_map,
            &mut max_hold_frames,
            &mut max_hold_id,
        );
    }

    HoldDistribution {
        node: node.to_string(),
        total_burn_frames,
        distinct_ids,
        max_hold_frames,
        max_hold_id,
        duplicate_pairs,
        adjacent_pairs,
        histogram: histogram_map.into_iter().collect(),
    }
}

/// The per-hop max-hold assertion. Arms mirror
/// [`crate::presentation_cadence::cadence_judder_gate_pass`] /
/// [`crate::e2e_latency_gate::cam_strih_latency_gate_pass`], with the same "no measurement ⇒ PASS"
/// divergence from the latency gate:
/// - `None` bound ⇒ report-only / bound off, always passes.
/// - `None` measured (no burn frame analyzed) ⇒ **PASS** — nothing was proven, and a hop with no
///   readable burn is already caught by the contiguity/`present_count` terms, so passing here is no
///   double-jeopardy.
/// - `Some` bound, `Some` measured ⇒ pass iff `measured <= bound` (a hold exactly at the bound
///   passes).
pub fn hold_gate_pass(measured_max_hold: Option<u32>, bound: Option<u32>) -> bool {
    match (bound, measured_max_hold) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(bound), Some(measured)) => measured <= bound,
    }
}

/// #870 report-only / restore seam — mirrors [`crate::presentation_cadence::gates_overall_pass`] /
/// [`crate::optical_floor::gates_overall_pass`]. Whether [`hold_gate_pass`]'s result folds into the
/// fused verdict's `overall_pass`. `false` today (report-only): the metric is brand-new and has no
/// green-run distribution to prove LIVE-safe against every recent green run + the cam1-grabber
/// defect (issue 909). Flip to `true` for a one-line promotion once #870's follow-up confirms the
/// bound holds with margin.
pub fn gates_overall_pass() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean decimated stream sequence — consecutive kept ids STEP by 2, every id distinct ⇒
    /// hold = 1, within bound, zero duplicate pairs. The legit topology-v2 baseline.
    #[test]
    fn clean_stepping_sequence_is_hold_one_within_bound() {
        let ids = [1000u32, 1002, 1004, 1006, 1008];
        let d = burn_hold_distribution("strih", &ids);
        assert_eq!(d.max_hold_frames, 1, "distinct stepping ids ⇒ max hold 1");
        assert_eq!(d.duplicate_pairs, 0);
        assert_eq!(d.distinct_ids, 5);
        assert_eq!(d.total_burn_frames, 5);
        assert!(d.within_bound(MAX_HOLD_FRAMES));
        assert_eq!(d.duplicate_pair_fraction(), 0.0);
        assert_eq!(d.histogram, vec![(1, 5)]);
    }

    /// A single legit 2-frame hold (a genuine 30→60 upsample / one FIFO convergence tick) stays
    /// within bound — the gate must not fire on the legitimate render:source ratio.
    #[test]
    fn legit_transient_hold_two_is_within_bound() {
        let ids = [1000u32, 1000, 1002, 1004];
        let d = burn_hold_distribution("stream", &ids);
        assert_eq!(d.max_hold_frames, 2);
        assert_eq!(d.max_hold_id, Some(1000));
        assert_eq!(d.duplicate_pairs, 1);
        assert!(d.within_bound(MAX_HOLD_FRAMES), "hold 2 <= 4 ⇒ pass");
    }

    /// The pathology, shaped like run 396782734: one burn id held for 5 consecutive frames ⇒ max
    /// hold 5 EXCEEDS the bound. This is the RED-defining assertion — the blind stub reports 1.
    #[test]
    fn pathology_max_hold_five_exceeds_bound_396782734() {
        // strih id 100 re-delivered on 5 consecutive stream frames (a frozen/repeated render).
        let ids = [98u32, 100, 100, 100, 100, 100, 102, 104];
        let d = burn_hold_distribution("strih", &ids);
        assert_eq!(d.max_hold_frames, 5, "the 5-frame freeze must be measured");
        assert_eq!(d.max_hold_id, Some(100));
        assert!(
            !d.within_bound(MAX_HOLD_FRAMES),
            "max hold 5 > bound 4 ⇒ the per-hop assertion FIRES (the #870 blindness fixed)"
        );
    }

    /// Reference SHAPE of run 396782734 (9487 frames, 3703 distinct, 61% consecutive pairs
    /// identical): the recordings are purged, so a representative sequence with the same
    /// >50%-duplicate character and a long max hold. The duplicate fraction is the ticket's
    /// headline; the assertion is on max hold.
    #[test]
    fn reference_396782734_shape_is_over_half_duplicate_and_fires() {
        // 20 runs: half of them held for 5 frames (repeats over the bound), half single — the
        // >50%-duplicate character of run 396782734 plus a max hold that exceeds MAX_HOLD_FRAMES.
        let mut ids = Vec::new();
        let mut next = 1000u32;
        for i in 0..20u32 {
            let run_len = if i % 2 == 0 { 5 } else { 1 };
            for _ in 0..run_len {
                ids.push(next);
            }
            next += 2;
        }
        let d = burn_hold_distribution("strih", &ids);
        assert!(
            d.duplicate_pair_fraction() > 0.5,
            "reference shape is majority-duplicate: {}",
            d.duplicate_pair_fraction()
        );
        assert_eq!(d.max_hold_frames, 5);
        assert!(
            !d.within_bound(MAX_HOLD_FRAMES),
            "max hold 5 > bound 4 ⇒ the assertion fires on the reference shape"
        );
        assert_eq!(d.distinct_ids, 20);
    }

    /// The full run-length histogram is reported (the ticket's "report the distribution" ask).
    #[test]
    fn histogram_records_full_run_length_distribution() {
        // runs: 100x1, 102x3, 104x1, 106x2  ⇒ hold lengths {1:2, 2:1, 3:1}
        let ids = [100u32, 102, 102, 102, 104, 106, 106];
        let d = burn_hold_distribution("stream", &ids);
        assert_eq!(d.histogram, vec![(1, 2), (2, 1), (3, 1)]);
        assert_eq!(d.max_hold_frames, 3);
        assert_eq!(d.max_hold_id, Some(102));
    }

    /// An empty burn sequence proves nothing and must PASS (report-only, no double-jeopardy).
    #[test]
    fn empty_sequence_proves_nothing_and_passes() {
        let d = burn_hold_distribution("cam2", &[]);
        assert_eq!(d.max_hold_frames, 0);
        assert_eq!(d.total_burn_frames, 0);
        assert_eq!(d.measured_max_hold(), None);
        assert!(d.within_bound(MAX_HOLD_FRAMES));
        assert_eq!(d.duplicate_pair_fraction(), 0.0);
    }

    /// A single burn frame is trivially hold-1.
    #[test]
    fn single_frame_is_hold_one() {
        let d = burn_hold_distribution("strih", &[7]);
        assert_eq!(d.max_hold_frames, 1);
        assert_eq!(d.measured_max_hold(), Some(1));
        assert_eq!(d.adjacent_pairs, 0);
        assert!(d.within_bound(MAX_HOLD_FRAMES));
    }

    /// The gate arms mirror the cadence/latency convention exactly.
    #[test]
    fn hold_gate_pass_arms() {
        assert!(
            hold_gate_pass(Some(99), None),
            "None bound ⇒ report-only pass"
        );
        assert!(
            hold_gate_pass(None, Some(4)),
            "None measured ⇒ pass (nothing proven)"
        );
        assert!(hold_gate_pass(Some(4), Some(4)), "exactly at bound ⇒ pass");
        assert!(hold_gate_pass(Some(1), Some(4)), "under bound ⇒ pass");
        assert!(!hold_gate_pass(Some(5), Some(4)), "over bound ⇒ fail");
    }

    /// The seam is REPORT-ONLY today (#870) — the metric never folds into overall_pass yet.
    #[test]
    fn gates_overall_pass_is_report_only_870() {
        assert!(!gates_overall_pass());
    }
}
