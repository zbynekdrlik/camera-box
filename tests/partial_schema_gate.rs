//! issue 1118 — a REPORT-ONLY leg's schema-mismatched partial must DEGRADE, not kill the merge.
//!
//! Tier-0 (default features): pins the PURE decision seam `camera_box::partial_schema_gate`, which
//! the probe-gated `recording-verdict.rs::run_merge` calls (that binary has no local type-check —
//! CLAUDE.md Tier-0/#477). The bug: a stale imag partial (schema v3 after the #1112 v3->v4 bump)
//! made `RecordingPartial::load(...)?` abort the WHOLE merge with no verdict JSON — even though the
//! imag leg is report-only (`imag_leg_gate::gates_overall_pass()==false`) and a strih+stream-only
//! merge of the same run passes. A report-only leg's INPUT error must never zero the hard gate.

use camera_box::partial_schema_gate::{
    box_is_report_only, classify_load_failure, peek_schema_version, PartialLoadDisposition,
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
fn only_the_imag_leg_is_report_only_today() {
    // imag_leg_gate::gates_overall_pass()==false — imag is the report-only leg. strih/stream are
    // the hard gate's own inputs.
    assert!(box_is_report_only("imag"));
    assert!(!box_is_report_only("strih"));
    assert!(!box_is_report_only("stream"));
    assert!(!box_is_report_only("unknown"));
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
        assert!(reason.contains("imag"), "reason must name the box: {reason:?}");
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
