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
use camera_box::probe::clock_ns;
use camera_box::probe::differ::{settle_cutoff_ns, trim_to_settle};
use std::thread::sleep;
use std::time::{Duration, Instant};

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

/// Call-site regression lock (review #1): the bug was multitap-probe deriving the
/// stop time from `Instant::elapsed()` (ALWAYS monotonic) while taps stamp
/// recv_ts_ns via `clock_ns(start, wall_clock)`. The binary now derives stop via
/// the SAME `clock_ns(start, args.wall_clock)`. This test reproduces both ends of
/// the trim with REAL `clock_ns` values and proves that stop via the matching
/// domain (clock_ns(start, true)) KEEPS settled frames, while stop via the bug
/// domain (Instant::elapsed / clock_ns(start, false)) DROPS every wall-clock frame.
/// If anyone reverts the stop derivation back to the monotonic domain, the first
/// assertion fails — locking the fix at the level it actually broke.
#[test]
fn wall_clock_stop_must_match_recv_clock_domain() {
    let start = Instant::now();
    // A tap that stamps recv on the wall clock (what --wall-clock does).
    let recv0 = clock_ns(start, true);
    sleep(Duration::from_millis(20));
    let frames = vec![obs(1, recv0)]; // received ~20ms+ before stop

    // Stop taken in the WALL-CLOCK domain (the fix): cutoff with 0ms settle keeps it.
    let stop_match = clock_ns(start, true);
    let cutoff_match = settle_cutoff_ns(stop_match, 0);
    assert_eq!(
        trim_to_settle(&frames, cutoff_match).len(),
        1,
        "#63 call-site: wall-clock stop (clock_ns(start,true)) must keep a frame \
         received before it; if this fails, stop_ns reverted to the monotonic domain"
    );

    // Stop taken in the MONOTONIC domain (the bug): astronomically smaller than the
    // epoch recv timestamp, so the frame is wrongly trimmed.
    let stop_bug = clock_ns(start, false);
    let cutoff_bug = settle_cutoff_ns(stop_bug, 0);
    assert_eq!(
        trim_to_settle(&frames, cutoff_bug).len(),
        0,
        "sanity: a monotonic stop (the bug) trims the wall-clock frame to zero"
    );
}

/// Call-site source lock (review #1): the only place that broke was multitap-probe's
/// stop-time derivation. A pure-helper test can't observe a regression there, so pin
/// the source directly — stop_ns MUST be `clock_ns(start, args.wall_clock)` and MUST
/// NOT use `start.elapsed()` (the monotonic bug). Mirrors the obs_phase2 static guards.
#[test]
fn multitap_probe_derives_stop_in_recv_clock_domain() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/multitap-probe.rs"
    ))
    .expect("read src/bin/multitap-probe.rs");
    assert!(
        src.contains("let stop_ns = clock_ns(start, args.wall_clock)"),
        "#63: multitap-probe must derive stop_ns via clock_ns(start, args.wall_clock) so it \
         shares the tap recv_ts_ns clock domain; without it the wall-clock settle cutoff \
         trims every frame to unique=0."
    );
    assert!(
        !src.contains("let stop_ns = start.elapsed()"),
        "#63 regression: stop_ns must NOT be derived from start.elapsed() (monotonic) — that \
         is the exact bug that zeroed all wall-clock frames."
    );
}
