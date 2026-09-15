//! #904 — present-by-adjacency inference for a single decoder-missed burn on a DELIVERED frame.
//!
//! The zero-loss gate treats every missing burn id on a delivered frame as `burn_unreadable`
//! (a hard fail, correctly — a burn that will not decode is a defect to fix). But the #264 class
//! is different: the digital burn WAS rendered crisp and present, the frame WAS delivered, and
//! the *decoder* returned no read on that one frame (1 of ~9800). This module decides, PURELY and
//! evidence-gated, when such a single miss is uniquely inferable as PRESENT — so the gate can
//! stay green on an otherwise-perfect run without ever excusing a genuine loss.
//!
//! It is deliberately narrow and FAIL-CLOSED: it fires only for a UNIQUELY inferable single id
//! (`next == prev + 2`) on a frame proven delivered by the caller, at most [`INFERRED_MISS_CAP_PER_NODE`]
//! such frames per node, and never two on adjacent recorded frames. Anything else — a run of two
//! missing ids, a non-adjacent neighbour (the #24/#356 decimated-hop real-drop signature), an
//! undelivered frame, or a pattern of misses — stays `burn_unreadable` at today's full strictness.
//! Whether the caller lets an inferred miss clear the headline is a SEPARATE decision (the
//! `burn_unreadable() - burn_unreadable_inferred() == 0` fold in `recording-verdict.rs`); this
//! module only answers "is THIS miss uniquely inferable as present-by-adjacency?".

/// The per-node cap on how many burn misses may be inferred present-by-adjacency in one run. A
/// SINGLE decoder miss (like the #264 incident, 1 of ~9800 frames) is the justified case; 3+ is a
/// pattern of misses — a real readability defect — and the whole set is then kept strict.
pub const INFERRED_MISS_CAP_PER_NODE: usize = 2;

/// A single missing burn id is uniquely inferable as PRESENT when:
/// 1. the frame carrying the miss was proven DELIVERED (`frame_delivered` — the caller establishes
///    this from the painter tick pin + at least one OTHER node's burn decoded on the same frame),
///    and
/// 2. the node's decoded ids on the two IMMEDIATELY-adjacent recorded frames bracket exactly one
///    integer: `next == prev + 2`, so the single missing id is unambiguously `prev + 1`.
///
/// Returns the inferred id (`prev + 1`), or `None` when either neighbour did not decode, the gap is
/// wider than one id (`next != prev + 2` — a real drop / decimated hop, kept strict), or the frame
/// was not delivered. `next < prev` (a backward counter jump) also returns `None`.
pub fn inferable_single_miss(
    prev: Option<u64>,
    next: Option<u64>,
    frame_delivered: bool,
) -> Option<u64> {
    if !frame_delivered {
        return None;
    }
    let prev = prev?;
    let next = next?;
    if next == prev + 2 {
        Some(prev + 1)
    } else {
        None
    }
}

/// The per-node cap + no-two-adjacent rule over the ORDERED per-frame sequence
/// `(frame_index, decoded_id, frame_delivered)` — one entry per recorded frame in the node's
/// analyzed window, `decoded_id == None` where the node's burn did not decode on that frame.
///
/// Returns the `frame_index`es whose miss is inferable-present, but ONLY when the whole inferred
/// set is small and isolated: at most `cap` frames AND never two on adjacent `frame_index`es.
/// Otherwise NONE are inferred (a larger / clustered set is a real readability defect, kept
/// strict — the fail-closed rule). The neighbours used for each miss are the PREVIOUS and NEXT
/// entries in this ordered slice, so the caller must present the frames in recorded order.
pub fn inferable_frame_indices(seq: &[(u64, Option<u64>, bool)], cap: usize) -> Vec<u64> {
    let mut hits: Vec<u64> = Vec::new();
    if seq.len() < 3 {
        return hits;
    }
    for i in 1..seq.len() - 1 {
        let (frame_index, decoded_id, delivered) = seq[i];
        if decoded_id.is_some() {
            continue; // the burn decoded here — nothing to infer
        }
        let prev = seq[i - 1].1;
        let next = seq[i + 1].1;
        if inferable_single_miss(prev, next, delivered).is_some() {
            hits.push(frame_index);
        }
    }
    // FAIL-CLOSED: a pattern of misses (more than the cap) is a real readability defect.
    if hits.len() > cap {
        return Vec::new();
    }
    // FAIL-CLOSED: never infer two on adjacent recorded frames (a two-frame cluster is not a
    // lone #264 decoder miss). `inferable_single_miss` already rejects a run of two (a None
    // neighbour), but this is the explicit guard for any other adjacency shape.
    let mut sorted = hits.clone();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[1] == w[0] + 1) {
        return Vec::new();
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_delivered_gap_of_one_is_inferable() {
        // prev=179076, next=179078 → the single missing 179077 is uniquely inferable.
        assert_eq!(
            inferable_single_miss(Some(179076), Some(179078), true),
            Some(179077)
        );
    }

    #[test]
    fn undelivered_frame_is_never_inferable() {
        assert_eq!(inferable_single_miss(Some(100), Some(102), false), None);
    }

    #[test]
    fn missing_neighbour_is_never_inferable() {
        // a run of two (next did not decode) — the #904 "never hides a real drop" core.
        assert_eq!(inferable_single_miss(Some(100), None, true), None);
        assert_eq!(inferable_single_miss(None, Some(102), true), None);
    }

    #[test]
    fn a_gap_wider_than_one_is_never_inferable() {
        // next == prev + 3 (two ids missing, or a decimated hop) — kept strict (#24/#356).
        assert_eq!(inferable_single_miss(Some(100), Some(103), true), None);
        // next == prev + 4
        assert_eq!(inferable_single_miss(Some(100), Some(104), true), None);
    }

    #[test]
    fn backward_neighbour_is_never_inferable() {
        assert_eq!(inferable_single_miss(Some(102), Some(100), true), None);
    }

    #[test]
    fn one_isolated_delivered_miss_qualifies() {
        // frames 10,11,12 with the burn missing on 11 (delivered); ids 500,_,502.
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, true),
            (12u64, Some(502u64), true),
        ];
        assert_eq!(
            inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE),
            vec![11]
        );
    }

    #[test]
    fn a_two_frame_run_infers_nothing() {
        // frames 10..13, burns missing on 11 AND 12 (a run) → neither neighbour brackets one id.
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, true),
            (12u64, None, true),
            (13u64, Some(503u64), true),
        ];
        assert!(inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE).is_empty());
    }

    #[test]
    fn an_undelivered_miss_infers_nothing() {
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, false), // frame not delivered → not inferable
            (12u64, Some(502u64), true),
        ];
        assert!(inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE).is_empty());
    }

    #[test]
    fn two_isolated_misses_are_within_cap() {
        // misses on 11 and 21, each a delivered single gap, far apart → both inferable (cap 2).
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, true),
            (12u64, Some(502u64), true),
            (20u64, Some(600u64), true),
            (21u64, None, true),
            (22u64, Some(602u64), true),
        ];
        let mut got = inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE);
        got.sort_unstable();
        assert_eq!(got, vec![11, 21]);
    }

    #[test]
    fn three_isolated_misses_exceed_the_cap_and_infer_nothing() {
        // three delivered single gaps → over the cap → NONE inferred (a pattern, kept strict).
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, true),
            (12u64, Some(502u64), true),
            (20u64, Some(600u64), true),
            (21u64, None, true),
            (22u64, Some(602u64), true),
            (30u64, Some(700u64), true),
            (31u64, None, true),
            (32u64, Some(702u64), true),
        ];
        assert!(inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE).is_empty());
    }

    #[test]
    fn two_non_adjacent_single_gaps_both_infer_within_cap() {
        // 10:500, 11:None(→501), 12:502, 13:None(→503), 14:504 — two isolated single gaps whose
        // frame_indexes (11, 13) are NOT adjacent → both inferred. `inferable_single_miss` cannot
        // ever produce two ADJACENT inferable frame_indexes (a run of two Nones breaks bracketing),
        // so the `windows()` adjacency guard is belt-and-suspenders; this pins the non-adjacent
        // pair still passing.
        let seq = vec![
            (10u64, Some(500u64), true),
            (11u64, None, true),
            (12u64, Some(502u64), true),
            (13u64, None, true),
            (14u64, Some(504u64), true),
        ];
        let mut got = inferable_frame_indices(&seq, INFERRED_MISS_CAP_PER_NODE);
        got.sort_unstable();
        assert_eq!(got, vec![11, 13], "two non-adjacent single gaps both infer");
    }
}
