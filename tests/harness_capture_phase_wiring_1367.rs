//! Issue 1367 slice D2 — static anchors for the capture phase tracker's WIRING in `src/main.rs`.
//!
//! The tracker (`camera_box::capture_phase`), the stamp decision
//! (`camera_box::dupe_decimation::stamp_slot_action`) and the two-clock bench are unit-tested in
//! their modules; `main.rs` compiles first on CI and has no unit test of its own. This file pins
//! the wiring those tests cannot see:
//!
//! * every good frame is tracked (`stamp_frame` with the V4L2 `sequence`) before the gate;
//! * the tracked slot is STAGED (`note_stamp_slot`) right before the ONE gate poll, so the gate
//!   decides on it;
//! * the NDI timecode floors the stamp instant `capture_phase::stamp_instant_100ns` returns (the
//!   slot middle while the tracker drives, else today's raw capture instant);
//! * every emitted stamp is recorded back to the gate (`note_emitted_stamp_100ns`) after the
//!   timecode and before the first send;
//! * the phase tokens ride the 5 s `#707 emit-1s` line;
//! * `capture.rs` hands the dequeued buffer's sequence number to the callback.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The byte offset of the ONE occurrence of `needle` in `hay` (fails on 0 or 2+).
fn unique(hay: &str, needle: &str) -> usize {
    let n = hay.matches(needle).count();
    assert_eq!(n, 1, "expected exactly one {needle:?}, found {n}");
    hay.find(needle).unwrap()
}

#[test]
fn every_good_frame_is_tracked_before_the_gate_decides_on_its_slot_1367() {
    let s = read("src/main.rs");
    let track = unique(&s, "let phase_slot_ns = capture_phase.stamp_frame(");
    let call = &s[track..(track + 320).min(s.len())];
    assert!(
        call.contains("info.sequence") && call.contains("out_interval_ns"),
        "stamp_frame must see the V4L2 sequence and the emit interval: {call}"
    );
    let luma = unique(&s, "decimation_gate.note_frame_luma(content_luma);");
    let stage = unique(&s, "decimation_gate.note_stamp_slot(slot_ns);");
    let poll = unique(&s, "let emit = decimation_gate.poll(");
    assert!(
        track < luma && luma < stage && stage < poll,
        "the slot is tracked, then staged right before the ONE gate poll"
    );
}

#[test]
fn the_ndi_timecode_floors_the_tracked_stamp_instant_1367() {
    let s = read("src/main.rs");
    let instant = unique(
        &s,
        "let capture_realtime_100ns = camera_box::capture_phase::stamp_instant_100ns(",
    );
    let call = &s[instant..(instant + 260).min(s.len())];
    assert!(
        call.contains("phase_slot_ns") && call.contains("mono_to_real_offset_100ns"),
        "the stamp instant takes the tracked slot and the raw fallback inputs: {call}"
    );
    let timecode = unique(
        &s,
        "let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(",
    );
    let tc_call = &s[timecode..(timecode + 200).min(s.len())];
    assert!(tc_call.contains("capture_realtime_100ns,"), "{tc_call}");
    let noted = unique(
        &s,
        ".note_emitted_stamp_100ns(capture_timecode_100ns, out_interval_ns);",
    );
    let first_send = unique(
        &s,
        "let starvation_repeats = decimation_gate.last_poll_starvation_repeats();",
    );
    assert!(
        instant < timecode && timecode < noted && noted < first_send,
        "stamp instant -> timecode -> the emitted stamp recorded -> the first send"
    );
}

#[test]
fn the_phase_tokens_ride_the_707_bucket_line_1367() {
    let s = read("src/main.rs");
    let line = unique(
        &s,
        "\"#707 emit-1s: {:?} cap-1s: {:?} (1-second buckets, oldest first){}\",",
    );
    let call = &s[line..(line + 260).min(s.len())];
    assert!(
        call.contains("capture_phase.status_tokens(out_interval_ns)"),
        "{call}"
    );
}

#[test]
fn capture_hands_the_sequence_number_to_the_callback_1367() {
    let c = read("src/capture.rs");
    let dq = unique(&c, "let seq = metadata.sequence;\n\n        // #696");
    let info = unique(&c, "            sequence: seq,\n");
    let cb = unique(&c, "callback(&buffer, info);");
    assert!(dq < info && info < cb);
}
