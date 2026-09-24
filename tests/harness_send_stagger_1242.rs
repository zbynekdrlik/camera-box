//! #1242 — static anchors for the per-camera NDI SEND stagger wiring in `src/main.rs` and the
//! production send thread (`src/ndi_send_thread.rs`).
//!
//! The pure pieces (`camera_box::send_stagger`, `camera_box::send_handoff`) are unit-tested in
//! their own modules. This file pins the WIRING those tests cannot see:
//!
//! * the stagger is resolved ONCE from the box's resolved OS hostname plus the genlock and capture
//!   rates (`send_stagger::plan`), and its one startup line is logged;
//! * the capture callback NEVER sleeps: the wait for the offset lives on the send thread;
//! * the send deadline is anchored at the emit-gate decision, AFTER the gate polled the wall clock;
//! * the frame is handed off AFTER every timecode of the iteration is fixed (the starvation repeats
//!   and the current frame travel in ONE job, so they share one deadline);
//! * a job replaced unsent in the single slot is counted;
//! * the send thread owns the sender, the synchronous send, the #944 heartbeat and the #297
//!   re-announce, and runs pinned + SCHED_FIFO like the burn thread;
//! * the E2E burn thread waits for the SAME deadline before its send;
//! * the #1131 buffered-queue signal reads the raw dequeue again (no capture sleep to add back);
//! * the window accounting is reported on the routine 5 s cadence (`send_handoff::window_summary`);
//! * shutdown closes the hand-off and joins the send thread;
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
    assert_eq!(n, 1, "expected exactly one {needle:?}, found {n}");
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
fn the_capture_callback_never_sleeps_1242() {
    let s = main_rs();
    let start = unique(&s, "let result = capture.process_frame(|data, info| {");
    let end = unique(&s, "wedge_heartbeat_ns.store(");
    assert!(start < end);
    let callback = &s[start..end];
    assert!(
        !callback.contains("thread::sleep"),
        "the capture callback must not sleep: the stagger wait belongs to the send thread"
    );
    for gone in [
        "send_stagger::remaining_sleep(",
        "send_stagger::should_sleep(",
        "send_stagger::idle_wait_ms(",
        "last_stagger_sleep_ms",
    ] {
        assert!(
            !s.contains(gone),
            "{gone:?} is the retired capture-loop sleep and must be gone"
        );
    }
}

#[test]
fn the_deadline_is_anchored_at_the_emit_gate_decision_1242() {
    let s = main_rs();
    let poll = unique(&s, "let emit = decimation_gate.poll(");
    let anchor = unique(&s, "let stagger_anchor = std::time::Instant::now();");
    let deadline = unique(
        &s,
        "camera_box::send_handoff::send_deadline(stagger_anchor, send_stagger_offset)",
    );
    assert!(
        poll < anchor && anchor < deadline,
        "the gate grids the boundary from the wall clock BEFORE the anchor; the deadline is anchor + offset"
    );
}

#[test]
fn the_frame_is_handed_off_after_every_timecode_is_fixed_1242() {
    let s = main_rs();
    let timecode = unique(
        &s,
        "let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(",
    );
    let repeats = unique(
        &s,
        "let starvation_repeats = decimation_gate.last_poll_starvation_repeats();",
    );
    let last_emit = unique(&s, "emit_one(capture_timecode_100ns);");
    let hand_off = unique(&s, "send_thread.hand_off(");
    assert!(
        timecode < repeats && repeats < last_emit && last_emit < hand_off,
        "the hand-off comes after the timecode, the starvation repeats and the current frame"
    );
    let call = &s[hand_off..(hand_off + 240).min(s.len())];
    assert!(
        call.contains("std::mem::take(&mut production_timecodes)")
            && call.contains("send_deadline"),
        "ONE job carries every timecode of the iteration and the deadline: {call}"
    );
    unique(&s, "production_timecodes.push(emit_timecode_100ns);");
}

#[test]
fn a_replaced_frame_is_counted_1242() {
    let s = main_rs();
    let hand_off = unique(&s, "send_thread.hand_off(");
    let counted = unique(&s, "capture_window.note_replaced(replaced);");
    assert!(hand_off < counted);
}

#[test]
fn the_send_thread_owns_the_sender_the_wait_and_the_heartbeat_1242() {
    let s = main_rs();
    unique(&s, "camera_box::ndi_send_thread::NdiSendThread::spawn(");
    assert!(
        !s.contains("send_frame_zero_copy("),
        "the production send moved off the capture thread"
    );
    assert!(
        !s.contains("maybe_reannounce("),
        "the #297 re-announce runs on the send thread, which owns the sender"
    );
    let t = read("src/ndi_send_thread.rs");
    for needle in [
        "send_frame_zero_copy(",
        "maybe_reannounce(",
        "crate::send_handoff::run_send_loop(",
        "crate::affinity::pin_capture_thread();",
        "crate::affinity::set_current_thread_realtime(",
        "if any_ok {",
        "\"Failed to send frame: {}\"",
        "std::thread::yield_now();",
    ] {
        assert!(
            t.contains(needle),
            "src/ndi_send_thread.rs must contain {needle:?}"
        );
    }
}

#[test]
fn the_burn_thread_waits_for_the_same_deadline_1242() {
    let s = main_rs();
    unique(&s, "send_deadline: std::time::Instant,");
    let wait = unique(
        &s,
        "camera_box::send_handoff::sleep_until(job.send_deadline)",
    );
    let send = unique(&s, "burn_sender.send_frame_data_with_timecode(");
    assert!(
        wait < send,
        "the burn thread waits for the frame's deadline before its send"
    );
}

#[test]
fn the_buffered_queue_signal_reads_the_raw_dequeue_1242() {
    let s = main_rs();
    let at = unique(&s, "camera_box::capture_stall::frame_from_nonempty_queue(");
    let call = &s[at..(at + 160).min(s.len())];
    assert!(
        call.contains("info.dequeue_duration_ms,"),
        "no capture sleep to add back any more: {call}"
    );
}

#[test]
fn the_window_accounting_is_reported_every_5s_1242() {
    let s = main_rs();
    let streaming = unique(
        &s,
        "\"Streaming: {:.1} fps emitted / {:.1} fps captured ({} sent, {} captured, {} capture-dropped, {} corrupted)\",",
    );
    let summary = unique(&s, "camera_box::send_handoff::window_summary(");
    assert!(
        streaming < summary,
        "the stagger summary rides the routine 5 s Streaming report"
    );
    unique(&s, "capture_window.note_work(");
}

#[test]
fn shutdown_joins_the_send_thread_1242() {
    let s = main_rs();
    let loop_start = unique(&s, "while running_capture.load(Ordering::Relaxed) {");
    let join = unique(&s, "send_thread.shutdown();");
    assert!(
        loop_start < join,
        "the send thread is joined after the capture loop ends"
    );
}

#[test]
fn ndi_send_path_and_timecode_are_untouched_1242() {
    let ndi = read("src/ndi.rs");
    assert!(
        !ndi.contains("send_stagger") && !ndi.contains("send_handoff"),
        "the stagger lives in the hand-off, never in the NDI timecode/send path"
    );
    assert!(ndi.contains("timecode: timecode_100ns,"));
}
