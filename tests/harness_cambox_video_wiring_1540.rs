//! Issue 1540 slice 2: the capture loop feeds the served video counters. Static anchors over
//! `src/main.rs` (the binary's loop is not unit-testable): the endpoint is started once, each 5-s
//! report window is folded in once, and the JPEG XS window is polled from the loop.

fn main_rs() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")).expect("main.rs")
}

#[test]
fn the_endpoint_is_started_once_before_the_capture_thread() {
    let s = main_rs();
    assert_eq!(s.matches("cambox_video_http::start_from_env(").count(), 1);
    let start = s.find("cambox_video_http::start_from_env(").unwrap();
    let capture = s
        .find("tokio::task::spawn_blocking(move ||")
        .expect("capture spawn");
    assert!(
        start < capture,
        "the endpoint starts before the capture thread"
    );
}

#[test]
fn every_report_window_is_folded_once_inside_the_five_second_block() {
    let s = main_rs();
    assert_eq!(s.matches("video_counters.note_capture_window(").count(), 1);
    let fold = s.find("video_counters.note_capture_window(").unwrap();
    let block = s.find("if elapsed.as_secs() >= 5 {").expect("report block");
    let reset = s[block..].find("emit_count = 0;").expect("window reset") + block;
    assert!(
        block < fold && fold < reset,
        "the fold sits inside the 5-s block, before the window counters reset"
    );
}

#[test]
fn the_jxs_window_is_polled_at_most_once_a_second() {
    let s = main_rs();
    assert_eq!(s.matches("video_counters.note_jxs_window(").count(), 1);
    let poll = s
        .find("if last_jxs_poll.elapsed() >= std::time::Duration::from_secs(1) {")
        .expect("the 1-s poll gate");
    let take = s.find("h.take_window()").expect("take_window");
    let fold = s.find("video_counters.note_jxs_window(").unwrap();
    assert!(poll < take && take < fold);
}
