//! Issue 1539: pins the intercom playback self-heal WIRING in `src/intercom.rs` (the decisions
//! are unit-tested in `src/playback_stall.rs`; the ALSA I/O cannot run without the headset).

const INTERCOM: &str = include_str!("../src/intercom.rs");
const HEAL: &str = include_str!("../src/intercom/playback_heal.rs");

fn slice_after<'a>(hay: &'a str, anchor: &str, len: usize) -> &'a str {
    let at = hay
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor not found: {anchor}"));
    &hay[at..(at + len).min(hay.len())]
}

#[test]
fn period_and_buffer_stay_as_before_1539() {
    assert!(INTERCOM.contains("const PERIOD_SIZE: u32 = 256;"));
    assert!(INTERCOM.contains("const BUFFER_PERIODS: u32 = 4;"));
    // the re-open goes through the one unchanged opener
    assert!(INTERCOM.contains("playback_heal.reopen(playback, &reason, open_alsa_playback)?"));
}

#[test]
fn an_unrecoverable_write_error_reopens_playback_only_1539() {
    assert!(
        !INTERCOM.contains("return Err(anyhow!(\"ALSA playback error: {}\", e));"),
        "a playback write error must not restart capture + the receiver while the budget lasts"
    );
    let write = slice_after(INTERCOM, "if !recover_alsa(&playback, e.errno()) {", 200);
    assert!(write.contains("reopen_reason = Some(format!(\"write error: {e}\"))"));
    let refused = slice_after(INTERCOM, "Reopen::Refused(_) if write_failed =>", 200);
    assert!(refused.contains("return Err(anyhow!("));
}

#[test]
fn a_granted_reopen_rebuilds_the_detector_and_the_window_1539() {
    let done = slice_after(INTERCOM, "Reopen::Done(fresh, identity) => {", 500);
    for needle in [
        "playback = fresh;",
        "PlaybackStallDetector::new(",
        "identity,",
        "window_start = std::time::Instant::now();",
        "window_written = 0;",
        "window_received_start = frames_received.load(Ordering::Relaxed);",
    ] {
        assert!(done.contains(needle), "missing after a reopen: {needle}");
    }
}

#[test]
fn the_status_line_carries_playback_progress_and_reopens_1539() {
    assert!(INTERCOM.contains("playback {} samp/s, playback_reopens {}"));
    assert!(INTERCOM.contains("playback_heal.reopens()"));
}

#[test]
fn the_old_handle_closes_before_the_new_open_1539() {
    let drop_at = HEAL.find("drop(old);").expect("old handle dropped");
    let open_at = HEAL.find("let fresh = open()?;").expect("fresh open");
    assert!(drop_at < open_at, "hw: is exclusive: close before re-open");
}
