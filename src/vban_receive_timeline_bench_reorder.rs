//! Issue 1439 — the bench's grades of a network that REORDERS packets (ROZHODNUTÉ 6352253147 item
//! (a)), a test-only `#[path]` child of the bench so the bench stays under its line budget.
//!
//! Live on stream-snv (mbc's `stream` crosses the RB4011 router, R 80 ms): a packet overtaken by the
//! next one arrived right behind it and was dropped as stale while its slot was still ~80 ms ahead,
//! so the slot went out as silence (`lost=43 stale=28` in ~35 min). Here the jittered sender's
//! adjacent packets swap places on the wire every so often: the overtaken packet arrives 0.5 us
//! after the one that passed it.

use super::*;

/// `sched` with packet `k` arriving right BEHIND packet `k + 1` for every `k` that is a multiple of
/// `every` past the first two seconds (the hold), both packets present: the one that overtook keeps
/// its arrival, the overtaken one arrives 0.5 us after it (the schedule keeps arrivals at least
/// 1 us apart, so the order of everything else is unchanged). Returns the schedule in ARRIVAL order
/// and how many packets were overtaken.
fn with_reorders(mut sched: Vec<Sent>, every: u64) -> (Vec<Sent>, u64) {
    let mut swaps = 0;
    let mut i = 0;
    while i + 1 < sched.len() {
        let k = sched[i].k;
        if k >= 400 && k % every == 0 {
            if let (Some(_), Some(b)) = (sched[i].arrival_ns, sched[i + 1].arrival_ns) {
                sched[i].arrival_ns = Some(b + 500);
                sched.swap(i, i + 1);
                swaps += 1;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    (sched, swaps)
}

/// ROZHODNUTÉ 6352253147 (a): every overtaken packet arrives ~R before its slot goes out, so it is
/// stored for that slot and played in it -- nothing lost, nothing stale, no late drop, no re-anchor,
/// every packet once in slot order, the latency held -- at stream-snv's R 80 and at the default 40,
/// with the encoder running and not.
#[test]
fn reordered_packets_play_in_their_slot_and_nothing_is_lost_1439() {
    for (seed, reserve_ms, no_encoder) in [(1u64, 80u64, false), (2, 40, true), (3, 80, true)] {
        let (sched, swaps) = with_reorders(schedule(seed, -6.5, TEN_MIN_PACKETS / 2, 0.0), 97);
        let run = replay_with(&sched, reserve_ms, no_encoder);
        let tag = format!("seed {seed}, R {reserve_ms} ms, no encoder {no_encoder}: {run:?}");
        assert!(swaps > 500, "the run must reorder: {swaps} {tag}");
        assert_eq!(
            run.reordered, swaps,
            "every overtaken packet is played: {tag}"
        );
        assert_eq!(
            (run.lost, run.stale, run.late, run.re_anchors),
            (0, 0, 0, 0),
            "{tag}"
        );
        // every packet once, in slot order (the records follow the emission order)
        assert!(
            run.records.windows(2).all(|w| w[1].k == w[0].k + 1),
            "the slots went out in order, each packet once: {tag}"
        );
        assert_latency_held(&run, &tag);
    }
}
