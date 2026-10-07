//! Issue 1372 (ROZHODNUTÉ 6033853074) — static anchors for the cambox stamp-offset WIRING in
//! `src/main.rs`.
//!
//! The decision (`camera_box::genlock_stamp::offset_resample_decision` + `StampOffset`) and the two
//! benches that drive it are unit-tested in their modules; `main.rs` compiles first on CI and has no
//! unit test of its own. This file pins the wiring those tests cannot see:
//!
//! * ONE offset reader: the bracketed `read_mono_wall_mono_ns` (mono, wall, mono), taken for the
//!   seed and once per captured frame; the old unbracketed `sample_mono_to_real_offset_100ns` and the
//!   loop's own cadence counter are gone (the decision owns the cadence);
//! * every captured frame feeds `StampOffset::observe_frame` BEFORE the capture-phase tracker sees
//!   the offset, so the tracked slot, the gate and the stamp change epoch in the frame the wall
//!   steps;
//! * the one `#1372 stamp offset re-sampled on a wall step of <ms>` line per step.

use std::path::PathBuf;

fn main_rs() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The byte offset of the ONE occurrence of `needle` in `hay` (fails on 0 or 2+).
fn unique(hay: &str, needle: &str) -> usize {
    let n = hay.matches(needle).count();
    assert_eq!(n, 1, "expected exactly one {needle:?}, found {n}");
    hay.find(needle).unwrap()
}

#[test]
fn one_bracketed_offset_reader_for_the_seed_and_every_frame_1372() {
    let s = main_rs();
    let def = unique(&s, "fn read_mono_wall_mono_ns() -> (u64, u64, u64) {");
    let body: String = s[def..].lines().take(6).collect::<Vec<_>>().join("\n");
    let before = body.find("let mono_before = monotonic_clock_ns();");
    let wall = body.find("let wall = wall_clock_ns();");
    let after = body.find("let mono_after = monotonic_clock_ns();");
    assert!(
        matches!((before, wall, after), (Some(b), Some(w), Some(a)) if b < w && w < a),
        "the read must be mono, wall, mono in that order: {body}"
    );
    assert_eq!(
        s.matches("read_mono_wall_mono_ns()").count(),
        3,
        "the definition, the seed and the per-frame read, nothing else"
    );
    for gone in [
        "sample_mono_to_real_offset_100ns",
        "should_resample_mono_to_real_offset",
        "frames_since_offset_sample",
    ] {
        assert!(
            !s.contains(gone),
            "{gone}: a second offset reader / cadence outside StampOffset"
        );
    }
}

#[test]
fn every_frame_feeds_the_stamp_offset_before_the_tracker_sees_it_1372() {
    let s = main_rs();
    let seed = unique(
        &s,
        "camera_box::genlock_stamp::StampOffset::seed(mono_before, wall, mono_after)",
    );
    let callback = unique(&s, "let result = capture.process_frame(");
    let observe = unique(
        &s,
        "stamp_offset.observe_frame(mono_before, wall, mono_after)",
    );
    let offset = unique(
        &s,
        "let mono_to_real_offset_100ns = stamp_offset.offset_100ns();",
    );
    let track = unique(&s, "let phase_slot_ns = capture_phase.stamp_frame(");
    let instant = unique(
        &s,
        "let capture_realtime_100ns = camera_box::capture_phase::stamp_instant_100ns(",
    );
    assert!(
        seed < callback && callback < observe && observe < offset && offset < track,
        "seeded before the loop, then per frame: observe -> the offset -> the tracker"
    );
    assert!(
        track < instant,
        "the stamp instant reads the same frame's offset"
    );
}

#[test]
fn one_line_per_wall_step_1372() {
    let s = main_rs();
    let step = unique(
        &s,
        "if let camera_box::genlock_stamp::OffsetResample::WallStep { step_ns } =",
    );
    let line = unique(
        &s,
        "\"#1372 stamp offset re-sampled on a wall step of {:+.3} ms\",",
    );
    let observe = unique(
        &s,
        "stamp_offset.observe_frame(mono_before, wall, mono_after)",
    );
    assert!(
        step < observe && observe < line && line - step < 400,
        "the line is logged for the WallStep decision of this frame's observe"
    );
    let call: String = s[line..].lines().take(2).collect::<Vec<_>>().join("\n");
    assert!(call.contains("step_ns as f64 / 1e6"), "{call}");
}
