//! Issue 1439 — the bench's grades of a playout worker that wakes LATE (review round 5), a
//! test-only `#[path]` child of the bench so the bench stays under its line budget.
//!
//! The worker is a normal thread (rtprio stays off on the OBS boxes), so every wake comes some
//! scheduling latency after the time it asked for. A slot handed to libobs after the window that
//! mixes it was taken makes libobs drop that window from the source and play everything after it
//! one window (21.3 ms) later -- the latency step the owner's rule forbids. So the worker hands a
//! STORED packet at its next wake (about R ahead of its stamp, whatever the wake jitter), and only
//! a missing slot's silence waits for its deadline, the stamp − the 2 ms margin: a wake later than
//! that is the known limit, and it is counted (`missed`, the status line's `emit_missed`).

use super::*;

/// The worker waking late by `min_ms` + up to `max_ms − min_ms` more with a tail ([`WakeLate`]).
fn late(seed: u64, min_ms: f64, max_ms: f64) -> Option<WakeLate> {
    Some(WakeLate {
        rng: Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1),
        min_ns: (min_ms * MS) as u64,
        max_ns: (max_ms * MS) as u64,
    })
}

/// Every row of the late-fill acceptance (ROZHODNUTÉ 6347872706: loss of 40-900 ms, the 90 ms
/// hiccup, the 90 ms persistent step) and the jittered 50 ppm sender with 30 ms gaps, with the
/// worker waking up to 1.9 ms late on EVERY wake (inside the 2 ms margin), encoder running and not:
/// the latency OBS realizes holds within ±1 ms after the first second, the mix's buffering never
/// grows and no slot reaches libobs after its stamp.
#[test]
fn every_late_fill_row_holds_with_the_worker_waking_up_to_1_9_ms_late_1439() {
    for no_encoder in [false, true] {
        for (ms, seed) in [(40u64, 1u64), (60, 2), (80, 3), (90, 4), (500, 5), (900, 6)] {
            let base = schedule(seed, 0.0, TEN_MIN_PACKETS / 2, 0.0);
            let run = replay_late(&with_outage(base, ms), 40, no_encoder, late(seed, 0.0, 1.9));
            let tag =
                format!("wake <= 1.9 ms late, loss {ms} ms, no encoder {no_encoder}: {run:?}");
            assert_eq!((run.re_anchors, run.late), (0, 0), "{tag}");
            assert_eq!(run.lost, run.dropped, "every lost slot is silence: {tag}");
            assert_eq!(
                run.missed, 0,
                "a slot reached libobs after its stamp: {tag}"
            );
            assert_latency_held(&run, &tag);
        }
        let sched = with_network_event(schedule(7, 30.0, TEN_MIN_PACKETS, 0.0), 90, false);
        let run = replay_late(&sched, 40, no_encoder, late(7, 0.0, 1.9));
        let tag = format!("wake <= 1.9 ms late, hiccup 90 ms, no encoder {no_encoder}: {run:?}");
        assert!(
            run.late > 0,
            "the hiccup must really drop late packets: {tag}"
        );
        assert_eq!((run.re_anchors, run.missed), (0, 0), "{tag}");
        assert_latency_held(&run, &tag);
        let sched = with_network_event(schedule(8, -30.0, TEN_MIN_PACKETS, 0.0), 90, true);
        let event_k = k_at(&sched, S0 + 120_000_000_000) - 20;
        let run = replay_late(&sched, 40, no_encoder, late(8, 0.0, 1.9));
        let tag = format!("wake <= 1.9 ms late, step 90 ms, no encoder {no_encoder}: {run:?}");
        assert_eq!((run.re_anchors, run.missed), (1, 0), "{tag}");
        let (_, al, _, ar) = held(&run, 0, 0..event_k);
        let (_, bl, _, br) = held(&run, 1, 0..u64::MAX);
        assert!(al <= 1.0 * MS && ar <= 1.0 * MS, "the old stretch: {tag}");
        assert!(bl <= 1.0 * MS && br <= 1.0 * MS, "the new stretch: {tag}");
        assert_eq!(run.mix_buffering_ns, 0.0, "the mix's buffering grew: {tag}");
        for seed in 1..=2u64 {
            let sched = schedule(seed, 50.0, TEN_MIN_PACKETS, 0.0);
            let run = replay_late(&sched, 40, no_encoder, late(seed, 0.0, 1.9));
            let tag = format!(
                "wake <= 1.9 ms late, 50 ppm seed {seed}, no encoder {no_encoder}: {run:?}"
            );
            assert!(run.max_gap_ns > 28_000_000, "a gap near 30 ms: {tag}");
            assert_eq!((run.late, run.re_anchors, run.missed), (0, 0, 0), "{tag}");
            assert_latency_held(&run, &tag);
        }
    }
}

/// A stored packet never waits for its deadline: with stream-snv's Receive Buffer (80 ms) against
/// the jittered sender with gaps up to 30 ms, a worker waking up to 15 ms late on every wake still
/// hands every packet to libobs long before the window that mixes it -- the realized latency holds
/// within ±1 ms and nothing reaches libobs after its stamp. (A worker that held each packet to its
/// deadline put every one of them behind its window.)
#[test]
fn a_stored_packet_reaches_libobs_long_before_its_stamp_with_the_worker_15_ms_late_1439() {
    for no_encoder in [false, true] {
        for (ppm, seed) in [(0.0, 1u64), (50.0, 2), (-50.0, 3)] {
            let sched = schedule(seed, ppm, TEN_MIN_PACKETS / 2, 0.0);
            let run = replay_late(&sched, 80, no_encoder, late(seed, 0.0, 15.0));
            let tag =
                format!("wake <= 15 ms late, R 80, ppm {ppm}, no encoder {no_encoder}: {run:?}");
            assert_eq!((run.late, run.re_anchors, run.lost), (0, 0, 0), "{tag}");
            assert_eq!(
                run.missed, 0,
                "a packet reached libobs after its stamp: {tag}"
            );
            assert_latency_held(&run, &tag);
        }
    }
}

/// Known limit (review round 5), pinned: a lost slot's silence waits for its deadline, so a worker
/// that wakes later than the 2 ms margin hands it after its stamp -- here every wake is exactly
/// 3 ms late on a 60 ms loss. Each such slot is COUNTED (`missed` = every lost slot; the stored
/// packets around the loss, handed long before their stamps, are not), and the window it missed
/// moves the latency OBS realizes, which is why the status line and verify item 9 carry the count.
#[test]
fn known_limit_a_lost_slot_handed_after_its_stamp_is_counted_missed_1439() {
    let base = schedule(4, 0.0, TEN_MIN_PACKETS / 4, 0.0);
    let run = replay_late(&with_outage(base, 60), 40, false, late(4, 3.0, 3.0));
    let tag = format!("wake 3 ms late, loss 60 ms: {run:?}");
    assert!(run.lost > 0, "the outage is a loss: {tag}");
    assert_eq!(
        run.missed, run.lost,
        "every lost slot, and only those: {tag}"
    );
    assert!(
        run.realized_dev_max_ns > 1.0 * MS,
        "a slot after its window moves the realized latency: {tag}"
    );
}
