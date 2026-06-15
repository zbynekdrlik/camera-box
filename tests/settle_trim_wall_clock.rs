//! Regression guard for the #63 wall-clock settle-trim bug.
//!
//! multitap-probe (#7 absolute-latency path) stamps each tap's `recv_ts_ns` on
//! CLOCK_REALTIME when `--wall-clock` is set — epoch nanoseconds, ~1.8e18. The
//! settle-window trim that drops the trailing in-flight frames must compute its
//! cutoff in the SAME clock domain. The binary previously derived the stop time
//! from `Instant::elapsed()` (monotonic, ~1e10 ns), so the cutoff
//! `stop_ns - settle` was astronomically smaller than every wall-clock
//! `recv_ts_ns`, and the filter `recv_ts_ns <= cutoff` rejected EVERY frame.
//! Result: `unique = 0` at every tap even when frames decoded perfectly — a
//! live OBS ingest that had just been fixed still read as "0 decoded".
//!
//! These tests pin the clock-domain contract on the pure `settle_cutoff_ns` /
//! `trim_to_settle` helpers so the bug cannot return without a live rig.

use camera_box::probe::analyzer::Observed;
use camera_box::probe::differ::{settle_cutoff_ns, trim_to_settle};

const MS: i64 = 1_000_000;

fn obs(frame_id: u32, recv_ts_ns: i64) -> Observed {
    Observed {
        frame_id,
        gen_ts_ns: 0,
        recv_ts_ns,
    }
}

/// A wall-clock run: recv timestamps are epoch ns (~1.8e18). The cutoff MUST be
/// taken from a wall-clock stop time so all but the trailing settle window
/// survive. A monotonic stop time (the old bug) yields a cutoff ~1e8 smaller
/// than any frame, trimming everything to zero.
#[test]
fn wall_clock_settle_trim_keeps_decoded_frames() {
    // Epoch-magnitude timestamps, 30 fps (~33ms apart), spanning ~1s up to stop.
    let stop_wall_ns: i64 = 1_781_524_000_000_000_000; // ~2026 epoch ns
    let frames: Vec<Observed> = (0..30)
        .map(|i| obs(1000 + i as u32, stop_wall_ns - (1000 - i as i64 * 33) * MS))
        .collect();

    // CORRECT: cutoff in the wall-clock domain (stop minus 500ms settle).
    let good_cutoff = settle_cutoff_ns(stop_wall_ns, 500);
    let kept = trim_to_settle(&frames, good_cutoff);
    assert!(
        !kept.is_empty(),
        "#63: wall-clock frames must survive a wall-clock-domain settle cutoff; \
         got 0 kept of {} (the exact zeroing bug)",
        frames.len()
    );

    // THE BUG: a monotonic stop time (~elapsed) makes the cutoff astronomically
    // smaller than every epoch recv_ts_ns, so the OLD code trimmed everything.
    let monotonic_stop_ns: i64 = 60 * 1_000 * MS; // ~60s elapsed, monotonic
    let bug_cutoff = settle_cutoff_ns(monotonic_stop_ns, 500);
    let bug_kept = trim_to_settle(&frames, bug_cutoff);
    assert_eq!(
        bug_kept.len(),
        0,
        "sanity: the monotonic-cutoff bug must trim ALL wall-clock frames \
         (this is what produced unique=0); if this isn't 0 the test setup is wrong"
    );
}

/// The trailing settle window is genuinely excluded: frames within `settle_ms`
/// of the stop are dropped, earlier frames kept — in the wall-clock domain.
#[test]
fn settle_window_excludes_only_trailing_inflight_frames() {
    let stop_wall_ns: i64 = 1_781_524_000_000_000_000;
    let early = obs(1, stop_wall_ns - 800 * MS); // 800ms before stop -> kept
    let late = obs(2, stop_wall_ns - 100 * MS); // 100ms before stop -> trimmed (in flight)
    let at_cutoff = obs(3, stop_wall_ns - 500 * MS); // exactly at cutoff -> kept (<=)
    let frames = vec![early, late, at_cutoff];

    let cutoff = settle_cutoff_ns(stop_wall_ns, 500);
    let kept = trim_to_settle(&frames, cutoff);
    let kept_ids: Vec<u32> = kept.iter().map(|o| o.frame_id).collect();

    assert!(
        kept_ids.contains(&1),
        "frame 800ms before stop must be kept"
    );
    assert!(
        kept_ids.contains(&3),
        "frame exactly at the cutoff must be kept (<=)"
    );
    assert!(
        !kept_ids.contains(&2),
        "frame 100ms before stop is in the 500ms settle window and must be trimmed"
    );
}
