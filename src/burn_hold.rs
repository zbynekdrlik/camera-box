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
//! **#1260 (post-deploy):** the DistroAV burn is now prepped ONCE per video tick, so `frame_id`
//! advances ~1/tick (≈ the emit rate), not per draw — the 41.7–147.9 ids/s figure above is the
//! PRE-#1260 per-draw cadence and drops to ~emit-rate once the within-tick-cache build is live.
//! This metric is UNAFFECTED either way: it counts REPEATED ids (a duplicate delivered frame),
//! which is rate-independent — the reasoning below holds at any cadence.
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
//! ## LIVE today (issue 870)
//!
//! [`gates_overall_pass`] returns `true`: the term is fully computed, serialized into the verdict
//! JSON (`full_chain.loss.<node>.hold.*`), AND folds into `overall_pass` — a hop that re-delivers
//! one burn id past [`MAX_HOLD_FRAMES`] now FAILS the run. Per `verdict-gate-seam-calibration.md`
//! §5 the flip was held until the `max_hold_frames` field's green-run distribution accumulated and
//! proved LIVE-safe: across the 6 green E2E runs carrying the field the worst `max_hold_frames` is
//! 2 (bound 4 => a 2-frame headroom on every green run — gates-green-first), the pathology (run
//! 396782734, hold >=5) fails, and cam1's node burn — subject to the issue-909 grabber defect — is
//! INCLUDED in that green set yet reaches only hold 2, so LIVE-safety is empirical. Flip back to
//! `false` for a one-line revert to report-only if a future rig change ever trips it.
//!
//! This is the PURE decision core; it compiles + unit-tests on DEFAULT features (the whole `probe`
//! module is CI-only per CLAUDE.md's Local Build Policy). The probe-gated consumer
//! (`src/bin/recording-verdict.rs`) feeds it
//! `recording_latency::burn_ids_with_frame_index_in(stream_frames, run_id)` — the recorded-ORDER
//! `(frame_index, id)` extractor already used for boundary-trimmed contiguity, so no new decode; the
//! `frame_index` lets a recorded gap (an undecodable burn in between) break a run instead of merging
//! two separate holds.

use std::collections::{BTreeMap, BTreeSet};

/// The maximum number of consecutive recorded frames a single burn id may legitimately occupy
/// before it is a REPEAT (a frozen / re-delivered rendered image). See the module doc for the
/// calibration: legit hold is 1 in the decimated stream recording; 4 clears any transient with
/// margin, matches the ticket's "may hold for 2, never 5", and mirrors
/// [`crate::imag_tick_gate::IMAG_OPTICAL_MAX_STUCK_RUN`] (= 4 frames).
pub const MAX_HOLD_FRAMES: u32 = 4;

/// The consecutive-duplicate run-length distribution of one node's burn-id sequence in a recording
/// (#870). Built from the recorded-ORDER `(frame_index, id)` pairs by [`burn_hold_distribution`].
/// All counts are `u32`; the duplicate FRACTION is a derived method so the struct carries no float
/// (keeps `PartialEq` exact and avoids the `Eq`-on-float trap). Not `Serialize`d — the probe
/// consumer hand-builds its verdict JSON so it can add the derived fraction/bound/gate-flag
/// fields the struct itself does not carry.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldDistribution {
    /// Node label, e.g. `"strih"`, `"stream"`, `"cam2"`.
    pub node: String,
    /// Total burn frames analyzed (one entry per recorded frame carrying this node's burn).
    pub total_burn_frames: u32,
    /// Distinct burn ids seen.
    pub distinct_ids: u32,
    /// The longest run of RECORDING-ADJACENT frames sharing one burn id, in FRAMES. `0` for an
    /// empty sequence, `1` for a clean sequence where every id is distinct (the legit decimated
    /// case).
    pub max_hold_frames: u32,
    /// The burn id that held longest (first one on a tie — strict `>`). `None` for an empty
    /// sequence.
    pub max_hold_id: Option<u32>,
    /// Recording-adjacent identical pairs (same id on frames `i` and `i+1`) — the "byte-identical
    /// consecutive recorded frames" numerator. For a run of length L this contributes L-1.
    pub duplicate_pairs: u32,
    /// Recording-adjacent pairs walked (consecutive recorded frames, both carrying this node's
    /// burn) — the [`duplicate_pair_fraction`](Self::duplicate_pair_fraction) denominator. A
    /// recorded gap between two decoded burns (an undecodable frame in between) is NOT a
    /// consecutive-recorded-frame pair and is excluded from BOTH this and `duplicate_pairs`.
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

/// Build the run-length distribution for one node's recorded-ORDER `(frame_index, id)` burn pairs
/// (the [`crate::probe::recording_latency::burn_ids_with_frame_index_in`] output — ascending
/// `frame_index`, one entry per recorded frame whose burn decoded).
///
/// ONE walk groups the pairs into maximal runs where BOTH the id is identical AND the frames are
/// RECORDING-ADJACENT (`frame_index` steps by exactly 1). A "hold" is the identical rendered image
/// delivered on consecutive RECORDED frames, so a recorded gap — an undecodable/absent burn on the
/// frame in between (`frame_index` jumps by >1) — breaks the run rather than silently merging two
/// separate holds into one longer (inflated) one. Each run of length L contributes one histogram
/// entry at `L` and `L-1` duplicate pairs; the longest run wins `max_hold_frames` / `max_hold_id`
/// (first one on a tie — strict `>`). `adjacent_pairs` counts only recording-adjacent pairs, so
/// `duplicate_pair_fraction` is exactly "fraction of consecutive recorded frames byte-identical"
/// (the ticket's headline metric), never diluted by decode gaps.
///
/// Program-switch segmentation is inherent on top of this: a switch changes the filter instance so
/// the `frame_id` jumps/resets ⇒ a different id ⇒ the run breaks. The only residual over-count risk
/// (two segments' counters coincidentally EQUAL on two recording-adjacent frames across the
/// boundary) is bounded to +1 per switch — negligible against the bound of 4 (a +1 near a handful
/// of program switches cannot lift a legit hold of 1–2 to the bound), so it does not threaten the
/// LIVE gate ([`gates_overall_pass`]).
///
/// An empty input ⇒ an all-zero distribution (`max_hold_frames == 0`, [`HoldDistribution::
/// measured_max_hold`] `== None`) — nothing was proven, and the gate PASSES it (see
/// [`hold_gate_pass`]).
pub fn burn_hold_distribution(
    node: &str,
    frames_in_recorded_order: &[(u64, u32)],
) -> HoldDistribution {
    let total_burn_frames = frames_in_recorded_order.len() as u32;
    let distinct_ids = frames_in_recorded_order
        .iter()
        .map(|&(_, id)| id)
        .collect::<BTreeSet<u32>>()
        .len() as u32;

    let mut histogram_map: BTreeMap<u32, u32> = BTreeMap::new();
    let mut max_hold_frames = 0u32;
    let mut max_hold_id: Option<u32> = None;
    let mut duplicate_pairs = 0u32;
    let mut adjacent_pairs = 0u32;

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

    let mut iter = frames_in_recorded_order.iter().copied();
    if let Some((first_idx, first_id)) = iter.next() {
        let mut run_id = first_id;
        let mut run_len = 1u32;
        let mut prev_idx = first_idx;
        for (idx, id) in iter {
            let recording_adjacent = idx == prev_idx.saturating_add(1);
            if recording_adjacent {
                adjacent_pairs += 1;
            }
            if id == run_id && recording_adjacent {
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
            prev_idx = idx;
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

/// #870 LIVE / restore seam — mirrors [`crate::presentation_cadence::gates_overall_pass`] /
/// [`crate::e2e_latency_gate::gates_overall_pass`] (both `true`). Whether [`hold_gate_pass`]'s
/// result folds into the fused verdict's `overall_pass`. `true` today (the bound is LIVE — it
/// passes every recent green run with honest margin): across the 6 green E2E runs that carry the
/// `full_chain.loss.<node>.hold.max_hold_frames` field the worst `max_hold_frames` is 2, so bound
/// [`MAX_HOLD_FRAMES`] (4) clears every green run with a 2-frame headroom (gates-green-first — no
/// green run would have been failed) while the pathology (run 396782734, hold >=5) fails. That
/// green set INCLUDES cam1's node burn, subject to the issue-909 ShadowCast-grabber defect (cam1
/// reaches only hold 2 in green runs), so LIVE-safety is empirical, not a mechanical claim
/// (verdict-gate-seam-calibration.md §5). Flip back to `false` for a one-line revert to report-only
/// if a future rig change ever trips it.
pub fn gates_overall_pass() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pair a plain id list with contiguous recorded frame indices `0,1,2,…` — the common case
    /// (every recorded frame in the span carried a readable burn). The gap tests build explicit
    /// `(frame_index, id)` pairs instead.
    fn contig(ids: &[u32]) -> Vec<(u64, u32)> {
        ids.iter()
            .enumerate()
            .map(|(i, &id)| (i as u64, id))
            .collect()
    }

    /// A clean decimated stream sequence — consecutive kept ids STEP by 2, every id distinct ⇒
    /// hold = 1, within bound, zero duplicate pairs. The legit topology-v2 baseline.
    #[test]
    fn clean_stepping_sequence_is_hold_one_within_bound() {
        let d = burn_hold_distribution("strih", &contig(&[1000, 1002, 1004, 1006, 1008]));
        assert_eq!(d.max_hold_frames, 1, "distinct stepping ids ⇒ max hold 1");
        assert_eq!(d.duplicate_pairs, 0);
        assert_eq!(d.distinct_ids, 5);
        assert_eq!(d.total_burn_frames, 5);
        assert_eq!(d.adjacent_pairs, 4);
        assert!(d.within_bound(MAX_HOLD_FRAMES));
        assert_eq!(d.duplicate_pair_fraction(), 0.0);
        assert_eq!(d.histogram, vec![(1, 5)]);
    }

    /// A single legit 2-frame hold (a genuine 30→60 upsample / one FIFO convergence tick) stays
    /// within bound — the gate must not fire on the legitimate render:source ratio.
    #[test]
    fn legit_transient_hold_two_is_within_bound() {
        let d = burn_hold_distribution("stream", &contig(&[1000, 1000, 1002, 1004]));
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
        let d = burn_hold_distribution("strih", &contig(&[98, 100, 100, 100, 100, 100, 102, 104]));
        assert_eq!(d.max_hold_frames, 5, "the 5-frame freeze must be measured");
        assert_eq!(d.max_hold_id, Some(100));
        assert!(
            !d.within_bound(MAX_HOLD_FRAMES),
            "max hold 5 > bound 4 ⇒ the per-hop assertion FIRES (the #870 blindness fixed)"
        );
    }

    /// Reference SHAPE of run 396782734 (9487 frames, 3703 distinct, 61% consecutive pairs
    /// identical): the recordings are purged, so a representative sequence with the same
    /// majority-duplicate character (over 50% of adjacent pairs) and a long max hold. The
    /// duplicate fraction is the ticket's headline; the assertion is on max hold.
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
        let d = burn_hold_distribution("strih", &contig(&ids));
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
        let d = burn_hold_distribution("stream", &contig(&[100, 102, 102, 102, 104, 106, 106]));
        assert_eq!(d.histogram, vec![(1, 2), (2, 1), (3, 1)]);
        assert_eq!(d.max_hold_frames, 3);
        assert_eq!(d.max_hold_id, Some(102));
    }

    /// A recorded GAP (an undecodable burn on the frame in between) must BREAK a run, never merge
    /// two separate deliveries of the same id into one inflated hold. `[100@0, 100@1, 100@3]`:
    /// frames 0-1 are recording-adjacent (a real hold of 2), then frame 2 is undecodable so 3 is
    /// NOT adjacent to 1 ⇒ the third 100 starts a fresh run ⇒ max hold 2, not 3.
    #[test]
    fn recorded_gap_breaks_a_run_never_merges_it() {
        let d = burn_hold_distribution("strih", &[(0, 100), (1, 100), (3, 100)]);
        assert_eq!(d.max_hold_frames, 2, "the gap at frame 2 breaks the run");
        assert_eq!(
            d.duplicate_pairs, 1,
            "only the 0→1 pair is recording-adjacent"
        );
        assert_eq!(
            d.adjacent_pairs, 1,
            "1→3 is not a consecutive-recorded-frame pair"
        );
        assert_eq!(d.histogram, vec![(1, 1), (2, 1)]);
        // Fraction denominator excludes the decode gap: 1 identical / 1 adjacent = 1.0, never
        // diluted to 1/2 by counting the non-adjacent 1→3 pair.
        assert_eq!(d.duplicate_pair_fraction(), 1.0);
    }

    /// `max_hold_id` is the FIRST id to reach the maximal run length on a tie (strict `>`), pinning
    /// the tie-break against a future `>=` mutation. `[1@0, 1@1, 2@2, 3@3, 3@4]`: two runs of
    /// length 2 (ids 1 and 3); the FIRST (id 1) wins.
    #[test]
    fn max_hold_id_is_first_on_a_tie() {
        let d = burn_hold_distribution("stream", &contig(&[1, 1, 2, 3, 3]));
        assert_eq!(d.max_hold_frames, 2);
        assert_eq!(
            d.max_hold_id,
            Some(1),
            "first run to reach the max wins the tie"
        );
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
        let d = burn_hold_distribution("strih", &[(0, 7)]);
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

    /// The seam is LIVE today (#870): the calibrated max-hold bound folds into `overall_pass`. It
    /// passes every recent green run with honest margin — the worst `max_hold_frames` across the 6
    /// green E2E runs carrying the field is 2 (bound 4 ⇒ a 2-frame headroom), and that INCLUDES
    /// cam1's node burn, subject to the issue-909 ShadowCast-grabber defect (cam1 reaches only 2 in
    /// green runs, so LIVE-safety is empirical, not a mechanical claim). Flip `gates_overall_pass` to
    /// `false` for a one-line revert to report-only if a future rig change ever trips it.
    #[test]
    fn gate_is_live_today_870() {
        assert!(
            gates_overall_pass(),
            "#870: the calibrated per-hop max-hold bound must gate overall_pass (LIVE)"
        );
    }

    /// The LIVE flip actually GATES: an over-bound hold now contributes FAIL to the fused verdict.
    /// This replicates the exact `recording-verdict.rs` fold expression
    /// (`hold_within || !gates_overall_pass()`) so the pure module pins the ticket's core promise —
    /// a repeating hop that exceeds the bound FAILS the run instead of being silently reported.
    /// RED against the report-only seam (the fold was always `true`); GREEN once LIVE.
    #[test]
    fn live_flip_makes_a_pathology_hold_fail_the_fold_870() {
        // A max-hold-5 pathology (run 396782734 shape): id 100 re-delivered on 5 consecutive frames.
        let d = burn_hold_distribution("strih", &contig(&[98, 100, 100, 100, 100, 100, 102, 104]));
        let hold_within = d.within_bound(MAX_HOLD_FRAMES);
        assert!(
            !hold_within,
            "max hold 5 > bound 4 ⇒ the hop is out of bound"
        );
        // The recording-verdict fold: this term's contribution to `all_pass`.
        let contributes_pass = hold_within || !gates_overall_pass();
        assert!(
            !contributes_pass,
            "#870 LIVE: an over-bound hold must FAIL the fused verdict (report-only would pass it)"
        );
    }

    /// A #575 recording-BOUNDARY artifact (the final frame held for several frames during mux
    /// finalization at StopRecord — a KNOWN non-loss class) must NOT trip the LIVE max-hold gate.
    /// The probe glue trims the recording boundary off the hold input
    /// ([`crate::recording_boundary_trim::trim_boundary_pairs`], lead/tail 3) BEFORE this walk;
    /// this pins that composition — the SAME boundary freeze reads as an over-bound hold UNtrimmed
    /// but clears the bound once trimmed. Without the trim, a boundary freeze would falsely fail a
    /// run now that the gate is LIVE (the review finding that gated the flip).
    #[test]
    fn recording_boundary_freeze_is_trimmed_below_the_hold_gate_575() {
        use crate::recording_boundary_trim::{
            trim_boundary_pairs, BOUNDARY_TRIM_LEAD_FRAMES, BOUNDARY_TRIM_TAIL_FRAMES,
        };
        // A clean stepping run on frames 0..=6, then the final id (108) held on the last 5 frames
        // (7..=11): a mux-finalization tail freeze on a 0..=11 recording.
        let mut pairs: Vec<(u64, u32)> = (0..7u64).map(|i| (i, 1000 + 2 * i as u32)).collect();
        pairs.extend((7..12u64).map(|i| (i, 108)));
        // UNtrimmed: the tail freeze is a 5-frame hold ⇒ over the bound (would FALSELY fail LIVE).
        let untrimmed = burn_hold_distribution("stream", &pairs);
        assert_eq!(untrimmed.max_hold_frames, 5);
        assert!(
            !untrimmed.within_bound(MAX_HOLD_FRAMES),
            "untrimmed, the boundary freeze trips the bound (the false-fire the trim prevents)"
        );
        // Trimmed on the recording's OWN bounds (0..=11, lead/tail 3): frames 0..=2 and 9..=11 are
        // dropped, so the tail freeze shrinks to 2 recorded frames (7,8) ⇒ within bound, PASS.
        let trimmed = trim_boundary_pairs(
            &pairs,
            0,
            11,
            BOUNDARY_TRIM_LEAD_FRAMES,
            BOUNDARY_TRIM_TAIL_FRAMES,
        );
        let d = burn_hold_distribution("stream", &trimmed);
        assert_eq!(
            d.max_hold_frames, 2,
            "the tail freeze shrinks below the bound once the recording boundary is trimmed"
        );
        assert!(d.within_bound(MAX_HOLD_FRAMES));
    }
}
