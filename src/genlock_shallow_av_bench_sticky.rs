//! Issue 1367 (design 5844353368) — the shallow A/V bench scenario of the STICKY content floor: an
//! idle feed that plays songs and re-locks on idle between them. A `#[path]` child of
//! `genlock_shallow_av_bench.rs`, split out to keep that file under the ~1000-line budget.

use super::*;

/// The live resolume 26.9.2026 sequence (log `2026-09-26 09-29-12.txt`): an idle feed (lag 8-12 ms,
/// budgeted latch floor 1 -> D 2) plays three songs whose content lag is 40-50 ms (tick floor 2 =
/// D, budgeted latch floor 2), and SongPlayer re-locks on idle at the end of each song. All of it
/// before the bench's OBS restart (1200 s), which resets the in-process sticky floor.
fn idle_songs() -> Scenario {
    Scenario {
        songs: &[(200, 400), (600, 800), (1000, 1150)],
        content_ms: (40, 50),
        ..Scenario::clean(8, 12, 2)
    }
}

#[test]
fn an_idle_relock_keeps_the_content_floor_and_later_song_starts_never_slew_1367() {
    let r = run(idle_songs());
    eprintln!("idle-songs: {r:?}");
    // one lock at the start (2), ONE re-measure at the first song (3), the three idle re-locks at the
    // song ends keep the sticky content floor (3, 3, 3), then the OBS restart (2) and the sender
    // restart (2) on idle start over without it.
    assert_eq!(r.latches, 7, "one re-measure in the OBS session: {r:?}");
    let mut seen = r.latched.clone();
    seen.dedup();
    assert_eq!(seen, [2, 3, 2], "D stays 3 across the idle re-locks: {r:?}");
    // the audio moves exactly once (onto the first song's D), never on a later song start or an
    // idle re-lock, and never steps.
    assert_eq!(
        (r.slews, r.steps),
        (1, 0),
        "later song starts slewed the audio: {r:?}"
    );
    // D 3 is presented from the first song on until the OBS restart, idle and playing alike.
    let at_3 = r.depth_hist.get(&3).copied().unwrap_or(0);
    assert!(
        at_3 > 25_000,
        "idle-songs: D 3 held only {at_3} presents: {:?}",
        r.depth_hist
    );
    assert!(
        r.max_abs_av_ms <= GATE_MAX_AV_MS,
        "idle-songs: |A/V| {:.2} ms",
        r.max_abs_av_ms
    );
}
