//! issue 1367 — the `ReleaseCadence` probe mirror of the BACKLOG relock's stale-burst anchor
//! reset (the C `if ((sel_1003 == 0 || stale_1367) && ...)`, authority
//! `crate::genlock_backlog::relock_anchor_is_stale`). Split out of `genlock.rs` to keep the lane
//! from growing that file further.

use super::*;
use std::collections::VecDeque;

/// The logged 20:50:58 cam6 burst through the probe cadence: 17 queued 60 fps frames, the head
/// 300 ms old, the anchor 283.6 ms (the late phase). The relock must drop the anchor and shed to
/// the configured 3 ms latency — the newest frame — in ONE event.
#[test]
fn backlog_relock_drops_the_logged_burst_anchor_1367() {
    const I30: u64 = 33_333_333;
    const I60: u64 = 16_666_667;
    let wall = 1_000_000_000_000u64;
    let mut cadence = ReleaseCadence::new();
    cadence.locked_next_boundary_ns = Some(wall - 400_000_000); // past ACQUIRE
    cadence.last_known_n = 2;
    cadence.phase_anchor_ns = 283_636_965;
    let mut queue: VecDeque<u64> = (0..17u64).map(|i| wall - 300_000_000 + i * I60).collect();

    let out = cadence.tick(wall, 3, I30, &mut queue);

    assert!(
        out.relocked,
        "issue 1367: the burst depth must hit the relock branch"
    );
    assert_eq!(
        out.presented,
        Some(wall - 300_000_000 + 16 * I60),
        "issue 1367: the relock must shed to the configured-latency phase (the newest frame, \
         index 16), not keep the late anchor phase (index 1, the logged erased=1)"
    );
    assert_eq!(
        out.dropped.len(),
        16,
        "the whole burst is shed in one relock"
    );
    assert!(queue.is_empty(), "nothing of the burst stays queued");
    assert_eq!(
        cadence.phase_anchor_ns, 0,
        "issue 1367: the stale anchor is dropped; it rebuilds from the next STEADY present"
    );
}

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
