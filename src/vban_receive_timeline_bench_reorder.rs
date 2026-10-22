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

/// ROZHODNUTÉ 6352253147 (a): at stream-snv's R 80 every overtaken packet arrives long before its
/// slot goes out (the sender's gaps reach 30 ms), so it is stored for that slot and played in it --
/// nothing lost, nothing stale, no late drop, no re-anchor, every packet once in slot order, the
/// latency held -- with the encoder running and not.
#[test]
fn reordered_packets_play_in_their_slot_and_nothing_is_lost_at_80_ms_1439() {
    for (seed, no_encoder) in [(1u64, false), (3, true)] {
        let (sched, swaps) = with_reorders(schedule(seed, -6.5, TEN_MIN_PACKETS / 2, 0.0), 97);
        let run = replay_with(&sched, 80, no_encoder);
        let tag = format!("seed {seed}, no encoder {no_encoder}: {run:?}");
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

/// At the default R 40 the same sender's 30 ms gaps leave about 2.7 ms of margin: a packet
/// overtaken by the one that ENDS such a gap is already past its own deadline when that one arrives
/// (its slot is one packet earlier), so its slot has gone out and it stays stale -- one lost slot,
/// never a re-anchor or a moved latency. Every other overtaken packet plays in its slot.
#[test]
fn at_40_ms_only_a_reorder_behind_a_gap_end_misses_its_slot_1439() {
    let (sched, swaps) = with_reorders(schedule(2, -6.5, TEN_MIN_PACKETS / 2, 0.0), 97);
    let run = replay_with(&sched, 40, true);
    let tag = format!("{run:?}");
    assert!(swaps > 500, "the run must reorder: {swaps} {tag}");
    assert_eq!(run.reordered + run.stale, swaps, "{tag}");
    assert_eq!(
        run.lost, run.stale,
        "each stale reorder is one lost slot: {tag}"
    );
    assert!(run.stale * 100 <= swaps, "only behind a gap end: {tag}");
    assert_eq!((run.late, run.re_anchors), (0, 0), "{tag}");
    assert!(
        run.records.windows(2).all(|w| w[1].k > w[0].k),
        "the slots went out in order, each packet at most once: {tag}"
    );
    assert_latency_held(&run, &tag);
}
