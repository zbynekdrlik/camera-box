//! #68 Task B: verify the ENDPOINT tap received EVERY id the generator emitted in
//! its contiguous range, in monotonic order — the generator's contiguity is the
//! source of truth, NOT the source tap.
//!
//! The painter (`run_painter`) emits a strictly monotonic, contiguous id sequence
//! `0,1,2,…`. So for any window the endpoint decoded, every integer in
//! `[first_decoded..=last_decoded]` was DEFINITELY generated; an absent integer is
//! a real generator→endpoint drop (which includes generator→cam loss the
//! tap-vs-tap diff is blind to). An id that arrives after a higher id is a real
//! reorder. This is stronger than `full_span_diff` (source-tap vs endpoint-tap
//! set difference), which can never see a frame the source tap also missed.
//!
//! RED before the `endpoint_sequence_check` impl exists; GREEN after.

use camera_box::probe::analyzer::Observed;
use camera_box::probe::differ::{decompose_missing, endpoint_sequence_check};

fn o(frame_id: u32, recv_ms: i64) -> Observed {
    Observed {
        frame_id,
        gen_ts_ns: 0,
        recv_ts_ns: recv_ms * 1_000_000,
        node_emit_tc_ns: 0,
    }
}

#[test]
fn contiguous_in_order_is_clean() {
    // Endpoint decoded 5..=9 contiguously, in order → no missing, no reorder.
    let ep = vec![o(5, 50), o(6, 60), o(7, 70), o(8, 80), o(9, 90)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.first_id, 5);
    assert_eq!(r.last_id, 9);
    assert_eq!(r.expected_count, 5);
    assert_eq!(r.delivered_count, 5);
    assert!(r.missing_ids.is_empty());
    assert!(r.out_of_order_ids.is_empty());
    assert!(r.is_clean());
}

#[test]
fn internal_gap_is_a_missing_id() {
    // id 7 absent inside [5..=9] — the generator emitted it (contiguous), so this
    // is a real generator→endpoint drop the tap-vs-tap diff would miss if the
    // source tap also dropped 7.
    let ep = vec![o(5, 50), o(6, 60), o(8, 80), o(9, 90)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.first_id, 5);
    assert_eq!(r.last_id, 9);
    assert_eq!(r.expected_count, 5);
    assert_eq!(r.delivered_count, 4);
    assert_eq!(r.missing_ids, vec![7]);
    assert!(r.out_of_order_ids.is_empty());
    assert!(!r.is_clean());
}

#[test]
fn multiple_internal_gaps_all_reported() {
    // ids 6 and 8 absent inside [5..=9].
    let ep = vec![o(5, 50), o(7, 70), o(9, 90)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.missing_ids, vec![6, 8]);
    assert_eq!(r.expected_count, 5);
    assert_eq!(r.delivered_count, 3);
    assert!(!r.is_clean());
}

#[test]
fn out_of_order_id_is_flagged() {
    // 5,6,8,7,9 — id 7 arrives AFTER id 8 (a reorder). No id is missing (all of
    // 5..=9 present), so this isolates the order check from the gap check.
    let ep = vec![o(5, 50), o(6, 60), o(8, 70), o(7, 80), o(9, 90)];
    let r = endpoint_sequence_check(&ep);
    assert!(r.missing_ids.is_empty());
    assert_eq!(r.out_of_order_ids, vec![7]);
    assert!(!r.is_clean());
}

#[test]
fn oversample_duplicates_in_order_are_not_reorders() {
    // The pipeline oversamples: the same id can repeat consecutively. A held id
    // (5,5,6,6) is NOT a reorder (equal, not decreasing) and not a gap.
    let ep = vec![o(5, 50), o(5, 51), o(6, 60), o(6, 61), o(7, 70)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.first_id, 5);
    assert_eq!(r.last_id, 7);
    assert!(r.missing_ids.is_empty());
    assert!(r.out_of_order_ids.is_empty());
    assert!(r.is_clean());
}

#[test]
fn empty_endpoint_is_not_clean() {
    // No frames decoded → cannot certify anything → not clean (never vacuous-pass).
    let ep: Vec<Observed> = vec![];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.delivered_count, 0);
    assert_eq!(r.expected_count, 0);
    assert!(!r.is_clean());
}

#[test]
fn single_frame_is_not_clean() {
    // One decoded id proves nothing about contiguity/order → not clean (a span of
    // length 1 cannot demonstrate zero loss).
    let ep = vec![o(42, 100)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.first_id, 42);
    assert_eq!(r.last_id, 42);
    assert_eq!(r.expected_count, 1);
    assert_eq!(r.delivered_count, 1);
    assert!(r.missing_ids.is_empty());
    assert!(!r.is_clean(), "a 1-frame span cannot certify zero-loss");
}

#[test]
fn gap_and_reorder_together_both_reported() {
    // 5,7,6,9 over implied [5..=9]: id 8 missing AND id 6 out of order (after 7).
    let ep = vec![o(5, 50), o(7, 60), o(6, 70), o(9, 90)];
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.missing_ids, vec![8]);
    assert_eq!(r.out_of_order_ids, vec![6]);
    assert!(!r.is_clean());
}

// ---- #68: decompose missing ids into source-emission artifact vs pipeline loss ----
//
// The fb-loopback painter (discrete writes through a 60→30 genlock decimation) does
// NOT emit every generated id at the SOURCE NDI — ~5-13% of painted ids are never
// sampled into NDI (proven live). So an endpoint "missing" id is one of two very
// different things: (1) it was never at the SOURCE either (a generator→source-NDI
// emission artifact of the QR rig, NOT a pipeline drop), or (2) it WAS at the source
// but vanished downstream (a REAL source→endpoint pipeline drop — the only kind that
// must fail a zero-loss gate). `decompose_missing` splits them using the source tap.

#[test]
fn missing_absent_at_source_is_emission_artifact() {
    // Endpoint missing ids {7, 9}; the SOURCE tap also never had them → both are
    // generator→source emission artifacts, NOT pipeline loss.
    let endpoint_missing = vec![7u32, 9];
    let source = vec![o(5, 50), o(6, 60), o(8, 80), o(10, 100)]; // no 7, no 9
    let (artifact, pipeline) = decompose_missing(&endpoint_missing, &source);
    assert_eq!(artifact, vec![7, 9]);
    assert!(pipeline.is_empty());
}

#[test]
fn missing_present_at_source_is_pipeline_loss() {
    // Endpoint missing id 8; the SOURCE tap HAD 8 → it was dropped DOWNSTREAM →
    // real pipeline loss, the kind a zero-loss gate must fail on.
    let endpoint_missing = vec![8u32];
    let source = vec![o(7, 70), o(8, 80), o(9, 90)]; // 8 present at source
    let (artifact, pipeline) = decompose_missing(&endpoint_missing, &source);
    assert!(artifact.is_empty());
    assert_eq!(pipeline, vec![8]);
}

#[test]
fn missing_split_into_both_classes() {
    // Endpoint missing {7, 8}: 7 absent at source (artifact), 8 present at source
    // (pipeline loss). Both classes reported distinctly.
    let endpoint_missing = vec![7u32, 8];
    let source = vec![o(6, 60), o(8, 80), o(9, 90)]; // has 8, not 7
    let (artifact, pipeline) = decompose_missing(&endpoint_missing, &source);
    assert_eq!(artifact, vec![7]);
    assert_eq!(pipeline, vec![8]);
}

#[test]
fn one_lost_frame_per_window_is_caught() {
    // Mirrors the user's persistence-test reality: a long contiguous run with a
    // single dropped id deep inside. The contiguity check MUST flag it — this is
    // the gap the tap-vs-tap 0-loss verdict was blind to.
    let mut ep: Vec<Observed> = (0u32..360).map(|i| o(i, i as i64)).collect();
    ep.remove(180); // drop id 180 from a 0..=359 contiguous endpoint stream
    let r = endpoint_sequence_check(&ep);
    assert_eq!(r.first_id, 0);
    assert_eq!(r.last_id, 359);
    assert_eq!(r.expected_count, 360);
    assert_eq!(r.delivered_count, 359);
    assert_eq!(r.missing_ids, vec![180]);
    assert!(!r.is_clean());
}
