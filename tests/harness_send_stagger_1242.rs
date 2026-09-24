//! #1242 — static anchors for the per-camera NDI SEND stagger wiring in `src/main.rs`.
//!
//! The pure decision (`camera_box::send_stagger`) is unit-tested in its own module. This file pins
//! the WIRING the pure tests cannot see:
//!
//! * the stagger is resolved ONCE from the box's resolved OS hostname plus the genlock and capture
//!   rates (`send_stagger::plan`), and its one startup line is logged;
//! * the sleep runs AFTER the frame's genlock timecode is computed (so the FLOOR-boundary timecode
//!   never moves) and BEFORE the first send of the iteration (the starvation repeats and the
//!   current frame share ONE delay, never one each);
//! * the emit-gate poll that grids the next boundary runs BEFORE the sleep (the grid is computed
//!   from the wall clock, never from the send instant);
//! * a frame that already came from a non-empty queue skips the sleep (`should_sleep`);
//! * the previous sleep is taken BEFORE the dequeue, so it always pairs with the very next dequeue
//!   (a corrupted buffer never reaches the callback), and the #1131 buffered-queue signal adds it
//!   back (`idle_wait_ms`);
//! * the window accounting is reported on the routine 5 s cadence (`window_summary`);
//! * `src/ndi.rs` (the timecode stamp + the SDK call) is untouched by the stagger.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn main_rs() -> String {
    read("src/main.rs")
}

/// The byte offset of the ONE occurrence of `needle` in `hay` (fails on 0 or 2+).
fn unique(hay: &str, needle: &str) -> usize {
    let n = hay.matches(needle).count();
    assert_eq!(
        n, 1,
        "expected exactly one {needle:?} in src/main.rs, found {n}"
    );
    hay.find(needle).unwrap()
}

#[test]
fn stagger_is_planned_once_from_the_resolved_os_hostname_1242() {
    let s = main_rs();
    let at = unique(&s, "camera_box::send_stagger::plan(");
    let call = &s[at..(at + 200).min(s.len())];
    assert!(
        call.contains("&resolved_hostname"),
        "plan() must take the resolved OS hostname: {call}"
    );
    assert!(
        call.contains("genlock_fps") && call.contains("frame_rate.numerator"),
        "plan() must see both the genlock rate and the capture rate: {call}"
    );
    unique(&s, "tracing::info!(\"{}\", send_stagger.log_line);");
}

#[test]
fn stagger_sleep_sits_between_the_timecode_and_the_first_send_1242() {
    let s = main_rs();
    let timecode = unique(
        &s,
        "let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(",
    );
    let sleep = unique(&s, "camera_box::send_stagger::remaining_sleep(");
    let first_send = unique(
        &s,
        "let starvation_repeats = decimation_gate.last_poll_starvation_repeats();",
    );
    assert!(
        timecode < sleep,
        "the stagger must run AFTER the genlock timecode is computed (the timecode must not move)"
    );
    assert!(
        sleep < first_send,
        "the stagger must run BEFORE the first send of the iteration (one delay per iteration)"
    );
}

#[test]
fn the_emit_grid_is_polled_before_the_stagger_1242() {
    let s = main_rs();
    let poll = unique(&s, "let emit = decimation_gate.poll(");
    let anchor = unique(&s, "let stagger_anchor = std::time::Instant::now();");
    let sleep = unique(&s, "camera_box::send_stagger::remaining_sleep(");
    assert!(
        poll < anchor && anchor < sleep,
        "the decimation gate grids the boundary from the wall clock BEFORE the stagger anchor"
    );
}

#[test]
fn a_backlogged_frame_skips_the_sleep_1242() {
    let s = main_rs();
    let q = unique(
        &s,
        "let queue_had_frame = if configured_capture_fps > 0.0 {",
    );
    let backlog = unique(&s, "frame_backlogged = queue_had_frame;");
    let decide = unique(&s, "camera_box::send_stagger::should_sleep(");
    let sleep = unique(&s, "camera_box::send_stagger::remaining_sleep(");
    assert!(
        q < backlog && backlog < decide && decide < sleep,
        "the backlog signal must feed should_sleep() before any sleep"
    );
}

#[test]
fn only_a_real_sleep_counts_as_slept_1242() {
    let s = main_rs();
    let sleep_call = unique(&s, "std::thread::sleep(remaining);");
    let slept = unique(&s, "stagger_window.note_slept(");
    let past = unique(&s, "stagger_window.note_past_offset();");
    let skipped = unique(&s, "stagger_window.note_skipped();");
    assert!(
        sleep_call < slept && slept < past && past < skipped,
        "a sleep is counted right after it happens (with its requested + measured ms); \
         a frame whose work already ate the offset counts as past-offset, a backlogged one as skipped"
    );
    let call = &s[slept..(slept + 160).min(s.len())];
    assert!(
        call.contains("remaining") && call.contains("stagger_slept_now_ms"),
        "note_slept() needs the requested and the measured sleep: {call}"
    );
}

#[test]
fn buffered_queue_signal_adds_the_stagger_back_1242() {
    let s = main_rs();
    let take = unique(
        &s,
        "let stagger_slept_ms = std::mem::take(&mut last_stagger_sleep_ms);",
    );
    let dequeue = unique(&s, "let result = capture.process_frame(");
    let q = unique(
        &s,
        "let queue_had_frame = if configured_capture_fps > 0.0 {",
    );
    let idle = unique(&s, "camera_box::send_stagger::idle_wait_ms(");
    let poll = unique(&s, "let emit = decimation_gate.poll(");
    assert!(
        take < dequeue,
        "the previous sleep must be taken BEFORE the dequeue it shortened"
    );
    assert!(
        q < idle && idle < poll,
        "queue_had_frame must be computed from idle_wait_ms before the gate poll"
    );
}

#[test]
fn the_window_accounting_is_reported_every_5s_1242() {
    let s = main_rs();
    let streaming = unique(
        &s,
        "\"Streaming: {:.1} fps emitted / {:.1} fps captured ({} sent, {} captured, {} capture-dropped, {} corrupted)\",",
    );
    let summary = unique(&s, "camera_box::send_stagger::window_summary(");
    assert!(
        streaming < summary,
        "the stagger summary rides the routine 5 s Streaming report"
    );
    unique(&s, "stagger_window.note_work(");
}

#[test]
fn ndi_send_path_and_timecode_are_untouched_1242() {
    let ndi = read("src/ndi.rs");
    assert!(
        !ndi.contains("send_stagger"),
        "the stagger lives in the capture loop, never in the NDI timecode/send path"
    );
    assert!(ndi.contains("timecode: timecode_100ns,"));
}
