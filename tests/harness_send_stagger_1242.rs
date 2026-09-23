//! #1242 — static anchors for the per-camera NDI SEND stagger wiring in `src/main.rs`.
//!
//! The pure decision (`camera_box::send_stagger`) is unit-tested in its own module. This file pins
//! the WIRING the pure tests cannot see:
//!
//! * the camera number comes from the box's resolved OS hostname, and the startup line is logged;
//! * the stagger sleep runs AFTER the frame's genlock timecode is computed (so the FLOOR-boundary
//!   timecode never moves) and BEFORE the first send of the iteration (the starvation repeats and
//!   the current frame share ONE delay, never one each);
//! * the emit-gate poll that grids the next boundary runs BEFORE the sleep (the grid is computed
//!   from the wall clock, never from the send instant);
//! * the #1131 buffered-queue signal adds the stagger sleep back (`idle_wait_ms`);
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
fn camera_number_comes_from_the_resolved_os_hostname_1242() {
    let s = main_rs();
    unique(
        &s,
        "camera_box::send_stagger::camera_number_from_hostname(&resolved_hostname)",
    );
    unique(&s, "camera_box::send_stagger::send_offset_us(");
    unique(&s, "camera_box::send_stagger::startup_log_line(");
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
fn buffered_queue_signal_adds_the_stagger_back_1242() {
    let s = main_rs();
    let q = unique(
        &s,
        "let queue_had_frame = if configured_capture_fps > 0.0 {",
    );
    let idle = unique(&s, "camera_box::send_stagger::idle_wait_ms(");
    let poll = unique(&s, "let emit = decimation_gate.poll(");
    assert!(
        q < idle && idle < poll,
        "queue_had_frame must be computed from idle_wait_ms before the gate poll"
    );
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
