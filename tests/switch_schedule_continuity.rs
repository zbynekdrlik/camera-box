//! #312 Phase-1 integration test — the all-cambox per-SEGMENT continuity public API that the
//! harness and CI use: parse a switch-schedule JSON, partition the single continuous stream
//! recording's decoded frames into per-cambox windows (by burn `gen_ts_ns`, minus the transition
//! guard) and verify the painted-tick continuity PER cambox.
//!
//! Synthetic frames only (no rig): realistic nanosecond timescales, step-2 painted tick (the
//! 60→30 stream-recording decimation). The probe-gated pure logic lives in
//! `src/probe/recording_segments.rs` (with its own exhaustive unit tests); this exercises the same
//! logic through the crate's PUBLIC entry points end-to-end, the way `recording-verdict` does.

#![cfg(feature = "probe")]

use camera_box::probe::recording_segments::{
    parse_switch_schedule, segment_continuity, SegmentFrame,
};

const S: i64 = 1_000_000_000; // one second in ns
const GUARD: i64 = S; // 1s transition guard (the production default)
const STEP: i64 = 2; // 60→30 painted-tick decimation in the stream recording

/// A two-cambox schedule: cam1 in program [0s, 5s), cam2 [5s, 10s) — the harness's JSON shape.
fn two_window_schedule_json() -> &'static str {
    r#"[
        {"cambox":"cam1","start_ns":0,          "end_ns":5000000000},
        {"cambox":"cam2","start_ns":5000000000, "end_ns":10000000000}
    ]"#
}

/// Frames at 100ms spacing across `[start_ns, start_ns + 5s)` with a step-2 painted tick — the
/// settled-core (outside the 1s guards) is contiguous, so the cambox passes.
fn clean_window_frames(start_ns: i64, base_index: u64, start_tick: u32) -> Vec<SegmentFrame> {
    (0..50)
        .map(|i| SegmentFrame {
            frame_index: base_index + i as u64,
            gen_ts_ns: start_ns + (i as i64) * (S / 10), // 100ms apart
            tick: Some(start_tick + (i as u32) * STEP as u32),
        })
        .collect()
}

#[test]
fn clean_two_cambox_run_passes_overall() {
    let schedule = parse_switch_schedule(two_window_schedule_json()).expect("schedule parses");
    let mut frames = clean_window_frames(0, 0, 1000);
    frames.extend(clean_window_frames(5 * S, 1000, 9000));

    let v = segment_continuity(&frames, &schedule, GUARD, STEP);

    assert!(v.overall_pass, "a clean all-cambox run is PASS: {v:?}");
    assert_eq!(v.segments.len(), 2);
    assert!(
        v.segments.iter().all(|s| s.pass),
        "every cambox clean: {v:?}"
    );
    // The 1s guards on each side trim the 50 frames (0..5s, 100ms apart) to the 1s..4s core.
    for s in &v.segments {
        assert!(s.frames > 0, "{} has attributed frames: {s:?}", s.cambox);
        assert_eq!(s.undecodable, 0);
        assert_eq!(s.copies, 0);
        assert_eq!(s.gaps, 0);
    }
    assert!(
        v.discarded_guard_frames > 0,
        "guard frames were discarded: {v:?}"
    );
}

#[test]
fn one_cambox_dropping_of_three_exceeds_tolerance_fails_overall_889_regate() {
    // Issue 889 (2026-07-30 user decision on issue 883) originally made `gaps` fully
    // report-only. The 2026-08-05 RE-GATE (ticket 889 comment 5196190653), recalibrated 1 -> 2 on
    // 2026-08-06 (ticket 889 comment 5198131539), re-introduced a per-window tolerance
    // (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`) — this fixture's gap of 3 (the seam
    // skips three painted-tick slots, tolerance+1) EXCEEDS that tolerance, so `overall_pass` now
    // correctly FAILS again, exactly like the STRICT per-cambox verdict already did. Renamed from
    // `one_cambox_dropping_of_two_exceeds_tolerance_..._889_regate` (which was itself renamed
    // from `..._889_relaxes_overall`) — the fixture now sits at tolerance+1 (3, not 2) so this
    // test tracks whatever the tolerance is calibrated to.
    let schedule = parse_switch_schedule(two_window_schedule_json()).expect("schedule parses");
    let mut frames = clean_window_frames(0, 0, 1000); // cam1 clean

    // cam2: inject a REAL gap deep in the settled core (gen_ts 2.5s into its window) — the painted
    // tick jumps by 8 (4 step-2 slots) where it should jump by 2, well past the guard.
    let mut cam2 = clean_window_frames(5 * S, 1000, 9000);
    let mid = cam2.len() / 2;
    for f in cam2.iter_mut().skip(mid) {
        if let Some(t) = f.tick.as_mut() {
            *t += 6; // shift the tail up by 6 → an 8-step jump at the seam (a 3-frame drop)
        }
    }
    frames.extend(cam2);

    let v = segment_continuity(&frames, &schedule, GUARD, STEP);

    assert!(v.segments[0].pass, "cam1 still clean: {:?}", v.segments[0]);
    assert!(
        !v.segments[1].pass,
        "cam2's STRICT verdict still catches the drop: {:?}",
        v.segments[1]
    );
    assert_eq!(
        v.segments[1].gaps, 3,
        "889 re-gate: a 3-slot drop exceeds the tolerance: {:?}",
        v.segments[1]
    );
    assert!(
        !v.segments[1].relaxed_pass,
        "889 re-gate: cam2's gaps=3 exceeds the tolerance -- relaxed must fail: {:?}",
        v.segments[1]
    );
    assert!(
        !v.overall_pass,
        "889 re-gate: a 3-slot drop must fail overall_pass again: {v:?}"
    );
    assert_eq!(v.windows_failed_report_only, 1);
    assert_eq!(
        v.windows_over_copies_gaps_tolerance, 1,
        "889 re-gate: exactly cam2's window exceeds the tolerance: {v:?}"
    );
}

#[test]
fn a_switch_transient_inside_the_guard_is_not_charged_as_loss() {
    // The program switch itself produces undecodable/duplicate frames RIGHT at the boundary. A 1s
    // guard discards them: the same bad frames inside the guard must NOT fail the cambox.
    let schedule = parse_switch_schedule(two_window_schedule_json()).expect("schedule parses");
    let mut frames = clean_window_frames(0, 0, 1000);

    // cam2 clean core + two transient bad frames within the leading 1s guard ([5.0s, 5.3s)).
    let mut cam2 = clean_window_frames(5 * S, 1000, 9000);
    cam2.push(SegmentFrame {
        frame_index: 5000,
        gen_ts_ns: 5 * S + S / 10,
        tick: None,
    }); // undecodable in guard
    cam2.push(SegmentFrame {
        frame_index: 5001,
        gen_ts_ns: 5 * S + S / 5,
        tick: Some(9000),
    }); // copy in guard
    frames.extend(cam2);

    let v = segment_continuity(&frames, &schedule, GUARD, STEP);

    assert!(
        v.overall_pass,
        "switch transients inside the guard must not fail: {v:?}"
    );
    assert!(
        v.segments[1].pass,
        "cam2 passes (transients guarded): {:?}",
        v.segments[1]
    );
    assert_eq!(
        v.segments[1].undecodable, 0,
        "the in-guard None is not charged"
    );
    assert_eq!(v.segments[1].copies, 0, "the in-guard copy is not charged");
    assert!(
        v.discarded_guard_frames >= 2,
        "the transients are guard discards: {v:?}"
    );
}

#[test]
fn an_absent_cambox_is_reported_as_uncovered_not_a_pass() {
    // #312 coverage honesty (#301 CAM3 down): a scheduled cambox with NO frames must FAIL, so the
    // verdict never implies full coverage when a box was absent.
    let schedule = parse_switch_schedule(two_window_schedule_json()).expect("schedule parses");
    let frames = clean_window_frames(0, 0, 1000); // only cam1 captured; cam2 was down

    let v = segment_continuity(&frames, &schedule, GUARD, STEP);

    assert!(
        !v.overall_pass,
        "an absent cambox ⇒ NOT a full-coverage pass: {v:?}"
    );
    assert!(v.segments[0].pass, "cam1 covered + clean");
    assert!(
        !v.segments[1].pass,
        "cam2 absent → FAIL: {:?}",
        v.segments[1]
    );
    assert_eq!(v.segments[1].frames, 0);
}

#[test]
fn an_empty_window_emits_a_painter_no_emit_diagnostic() {
    // #333: a swept cambox window with ZERO in-window frames is most likely the dual-QR PAINTER
    // box (it does NOT emit its own camera NDI while painting — #179) or a down / non-emitting
    // box, NOT a chain frame loss. The verdict still FAILs (frames=0 ⇒ pass=false), but it MUST
    // also carry an explicit diagnostic so an empty window is never mistaken for a continuity
    // break — telling the operator to exclude that box from CAMBOX_SWEEP.
    let schedule = parse_switch_schedule(two_window_schedule_json()).expect("schedule parses");
    let frames = clean_window_frames(0, 0, 1000); // only cam1 captured; cam2's window is empty

    let v = segment_continuity(&frames, &schedule, GUARD, STEP);

    assert!(!v.segments[1].pass, "an empty window FAILs (frames=0)");
    assert_eq!(v.segments[1].frames, 0);
    let note = v.segments[1]
        .note
        .as_deref()
        .expect("#333: a frames=0 window must carry an explicit painter/no-emit diagnostic note");
    let lower = note.to_lowercase();
    assert!(
        lower.contains("0 frames") || lower.contains("no frames"),
        "#333: the note must state the window produced no frames: {note}"
    );
    assert!(
        lower.contains("painter") || lower.contains("emit"),
        "#333: the note must point at the painter / not-emitting cause: {note}"
    );
    assert!(
        lower.contains("cambox_sweep"),
        "#333: the note must tell the operator to exclude it from CAMBOX_SWEEP: {note}"
    );
    // A covered, clean window carries NO such note (the field is empty when frames > 0).
    assert!(
        v.segments[0].note.is_none(),
        "a covered, clean window has no painter note: {:?}",
        v.segments[0]
    );
}

#[test]
fn malformed_or_overlapping_schedule_is_rejected() {
    assert!(parse_switch_schedule("{ not an array }").is_err());
    let overlapping = r#"[
        {"cambox":"cam1","start_ns":0,          "end_ns":6000000000},
        {"cambox":"cam2","start_ns":5000000000, "end_ns":10000000000}
    ]"#;
    assert!(
        parse_switch_schedule(overlapping).is_err(),
        "overlap rejected"
    );
}
