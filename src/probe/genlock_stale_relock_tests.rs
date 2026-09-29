//! issue 1367 — the `ReleaseCadence` probe mirror of the BACKLOG relock's stale-burst anchor
//! reset (the C `if ((sel_1003 == 0 || stale_1367) && ...)`, authority
//! `crate::genlock_backlog::relock_anchor_is_stale`). Split out of `genlock.rs` to keep the lane
//! from growing that file further.

use super::*;
use std::collections::VecDeque;

// Issue 1367 D1: the logged 20:50:58 cam6 burst was a 60-into-30 source; an N>=2 source takes
// the grid-exact release now and never relocks, so its lock is
// `n2_burst_sheds_to_the_target_in_one_tick_1367` in `genlock_n2_tests.rs`. The two N==1 cases
// below still exercise the stale-anchor reset.

/// The pre-1367 #1037 demonstrative scenario — a 900 ms anchor tracked over a 3 ms configured
/// latency (27 frames behind the configured pick on a 1:1 source) — is exactly that stale phase
/// now: the relock sheds to the configured latency instead of inheriting it.
#[test]
fn an_anchor_far_behind_the_configured_phase_is_dropped_1367() {
    const I: u64 = 33_333_333;
    let base = 1_000_000_000_000u64;
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(base);
    cadence.last_known_n = 1;
    cadence.phase_anchor_ns = 900_000_000;
    let mut queue: VecDeque<u64> = (0..40u64).map(|i| base + i * I).collect();
    let wall_now = base + 39 * I + 5_000_000;

    let out = cadence.tick(wall_now, 3, I, &mut queue);

    assert!(out.relocked);
    assert_eq!(
        out.presented,
        Some(base + 39 * I),
        "the configured pick, the newest frame"
    );
    assert_eq!(out.dropped.len(), 39);
    assert_eq!(cadence.phase_anchor_ns, 0);
}

/// ROZHODNUTÉ 5840479751: the stream `NDI 2ME PGM` (30-into-30, pin 987 ms) held by the N==1
/// governor at `base + 1` = 31 frames, relocking on a render tick 5 ms late. Against the bare
/// configured pick its correct anchor reads 2 frames behind; against the governor's expected depth
/// it reads 0 — the anchor is KEPT and the relock presents the base + 1 frame.
#[test]
fn a_deep_base_plus_one_anchor_on_a_late_tick_is_kept_1367() {
    const I: u64 = 33_333_333;
    let wall = 1_000_000_000_000u64;
    let eps = 5_000_000u64;
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(wall - 2_000_000_000); // past ACQUIRE
    cadence.last_known_n = 1;
    let anchor = 31 * I + 2_000_000;
    cadence.phase_anchor_ns = anchor;
    // 40 frames aged 40..1 frames, all read `eps` late: index k is (40 - k) frames old.
    let mut queue: VecDeque<u64> = (0..40u64).map(|k| wall - ((40 - k) * I + eps)).collect();

    let out = cadence.tick(wall, 987, I, &mut queue);

    assert!(
        out.relocked,
        "40 queued frames exceed the 36-frame backlog threshold at pin 987"
    );
    assert_eq!(
        out.presented,
        Some(wall - (31 * I + eps)),
        "the relock must present the base + 1 frame (index 9), not reset to the configured pick"
    );
    assert_eq!(out.dropped.len(), 9);
    assert_eq!(
        cadence.phase_anchor_ns, anchor,
        "a base + 1 anchor is where the governor holds the 2ME PGM — it must be kept"
    );
}
