//! issue 1118 -> #1142 — the imag leg's schema-mismatched partial must DEGRADE (drop it), not kill
//! the merge; #1142 makes the dropped leg RED the run instead of silently passing.
//!
//! Tier-0 (default features): pins the PURE decision seam `camera_box::partial_schema_gate`, which
//! the probe-gated `recording-verdict.rs::run_merge` calls (that binary has no local type-check —
//! CLAUDE.md Tier-0/#477). The bug: a stale imag partial (schema v3 after the #1112 v3->v4 bump)
//! made `RecordingPartial::load(...)?` abort the WHOLE merge with no verdict JSON. The imag leg's
//! INPUT error must never zero the hard gate by ABORTING — instead it degrades (drop the leg), and
//! the dropped leg REDs via the now-BLOCKING `imag_leg_verified` fold (#1142).

use camera_box::partial_schema_gate::{
    box_degrades_on_schema_mismatch, classify_load_failure, peek_schema_version,
    PartialLoadDisposition,
};

fn is_degrade(d: &PartialLoadDisposition) -> bool {
    matches!(d, PartialLoadDisposition::Degrade { .. })
}

#[test]
fn peek_schema_version_reads_just_the_field() {
    // A real (compact) partial JSON — the merge's on-disk shape.
    let v3 = r#"{"schema_version":3,"box":"imag","recording":"imag-1.mkv","expected_burns":[],"frames":[]}"#;
    let v4 = r#"{"schema_version":4,"box":"imag","recording":"imag-1.mkv","expected_burns":[],"frames":[]}"#;
    assert_eq!(peek_schema_version(v3), Some(3));
    assert_eq!(peek_schema_version(v4), Some(4));
    // Not valid JSON at all -> None (a NON-schema failure; the caller keeps it fatal).
    assert_eq!(peek_schema_version("not json at all {"), None);
    // Valid JSON but no numeric schema_version -> None.
    assert_eq!(peek_schema_version(r#"{"box":"imag"}"#), None);
    assert_eq!(peek_schema_version(r#"{"schema_version":"three"}"#), None);
}

#[test]
fn only_the_imag_leg_degrades_on_schema_mismatch() {
    // Only imag degrades on a schema mismatch (its on-box binary can legitimately be stale after a
    // PARTIAL_SCHEMA_VERSION bump). strih/stream are the hard gate's own inputs (fresh from CI).
    assert!(box_degrades_on_schema_mismatch("imag"));
    assert!(!box_degrades_on_schema_mismatch("strih"));
    assert!(!box_degrades_on_schema_mismatch("stream"));
    assert!(!box_degrades_on_schema_mismatch("unknown"));
    // #1142 — the degrade is DECOUPLED from the imag gate flip (owner mandate): the imag PRESENCE
    // seam is now BLOCKING (gates_overall_pass()==true), yet a schema-degraded imag leg must STILL
    // degrade (drop the leg + write a verdict) rather than hard-abort — the RED comes from
    // `imag_leg_verified=false` via the BLOCKING verified fold, not from aborting the merge. This
    // guards against re-coupling `box_degrades_on_schema_mismatch` to `gates_overall_pass` (which
    // would wrongly turn a stale-emitter imag partial into a fatal no-verdict crash).
    assert!(
        camera_box::imag_leg_gate::gates_overall_pass(),
        "sanity: the imag presence seam is BLOCKING since #1142"
    );
    assert!(
        box_degrades_on_schema_mismatch("imag"),
        "#1142: imag still degrades on a schema mismatch even though its presence seam is BLOCKING"
    );
}

#[test]
fn imag_schema_mismatch_degrades_not_dies() {
    // THE regression: a v3 imag partial against a v4-expecting build must DEGRADE (drop the leg,
    // keep merging strih+stream), NEVER abort the whole verdict.
    let d = classify_load_failure("imag", Some(3), 4);
    assert!(
        is_degrade(&d),
        "issue 1118: a schema-mismatched imag partial must DEGRADE, got {d:?}"
    );
    if let PartialLoadDisposition::Degrade { reason } = &d {
        // The reason must be a real, mineable string naming the box + the version delta.
        assert!(
            reason.contains("imag"),
            "reason must name the box: {reason:?}"
        );
        assert!(
            reason.contains('3') && reason.contains('4'),
            "reason must name the found vs expected schema: {reason:?}"
        );
    }
    // Any future report-only version delta on imag degrades too (not just 3->4).
    assert!(is_degrade(&classify_load_failure("imag", Some(2), 5)));
    assert!(is_degrade(&classify_load_failure("imag", Some(99), 4)));
}

#[test]
fn strih_and_stream_schema_mismatch_stay_fatal() {
    // The hard gate's own inputs — a schema mismatch there stays FATAL (their binaries come fresh
    // from CI each run, so a mismatch is a genuine defect, never a stale-emitter degrade case).
    assert_eq!(
        classify_load_failure("strih", Some(3), 4),
        PartialLoadDisposition::Fatal
    );
    assert_eq!(
        classify_load_failure("stream", Some(3), 4),
        PartialLoadDisposition::Fatal
    );
}

#[test]
fn same_schema_or_non_schema_failures_stay_fatal_even_for_imag() {
    // A same-schema (found == expected) load failure is NOT a schema mismatch — some OTHER
    // corruption — and must stay fatal even on the imag leg.
    assert_eq!(
        classify_load_failure("imag", Some(4), 4),
        PartialLoadDisposition::Fatal
    );
    // A failure where we could not even peek a schema_version (unreadable / corrupt JSON) is a
    // non-schema failure and stays fatal even for imag — the degrade is scoped to a CLEAN
    // forward-compat schema mismatch, not "swallow every imag error".
    assert_eq!(
        classify_load_failure("imag", None, 4),
        PartialLoadDisposition::Fatal
    );
}
