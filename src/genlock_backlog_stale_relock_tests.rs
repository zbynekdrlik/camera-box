//! Issue 1367 — the unit tests of the stale-burst backlog relock reset
//! ([`relock_anchor_is_stale`]), a `#[path]` child of `genlock_backlog` kept out of that
//! already-long module. They compile with the module.

use super::*;

/// Source interval of a 60 fps sender (ns).
const I60: u64 = 16_666_667;
/// Canvas interval of the 30 fps strih canvas (ns).
const I30: u64 = 33_333_333;
/// The strih camera pin (ms) — the logged relock lines carried `latency_ms=3`.
const LATENCY_MS: u32 = 3;
/// A wall instant well clear of zero.
const W0: u64 = 1_800_000_000_000_000_000;

/// A 60-into-30 queue of `depth` frames, oldest first, whose OLDEST frame is `head_age_ns` old
/// at `wall` and whose frames sit one source interval apart.
fn burst_queue(wall: u64, depth: usize, head_age_ns: u64) -> Vec<u64> {
    (0..depth as u64)
        .map(|i| wall - (head_age_ns - i * I60))
        .collect()
}

/// The two picks the BACKLOG branch compares: against the tracked anchor, and against the
/// configured latency (the anchor-unset fallback).
fn picks(q: &[u64], wall: u64, anchor_ns: u64) -> (usize, usize) {
    (
        relock_select_nearest(q, wall, relock_anchor_age_ns(anchor_ns, LATENCY_MS)),
        relock_select_nearest(q, wall, relock_anchor_age_ns(0, LATENCY_MS)),
    )
}

#[test]
fn the_logged_cam6_burst_reads_stale_1367() {
    // Live 25.9.2026 20:50:58 on strih-lx:
    // genlock-relock 'NDI cam6': depth=17 due=17 erased=1 head_skew_ms=300
    //                            anchor_ns=283636965 sel_vs_newest_due=-15
    let q = burst_queue(W0, 17, 300_000_000);
    let (sel_anchor, sel_configured) = picks(&q, W0, 283_636_965);
    // The replay reproduces the logged line exactly: erased=1, and 1 - (17 - 1) = -15.
    assert_eq!(sel_anchor, 1, "the anchor pick must be the logged erased=1");
    assert_eq!(
        sel_configured, 16,
        "the configured-latency pick is the newest due frame"
    );
    assert_eq!(
        sel_anchor as i64 - (17 - 1),
        -15,
        "sel_vs_newest_due must match the log"
    );
    assert!(
        relock_anchor_is_stale(sel_anchor, sel_configured, 2),
        "an anchor 15 source frames behind the configured phase is an arrival-burst phase, \
         not jitter — the relock must drop it and shed to the configured latency"
    );
}

#[test]
fn the_logged_cam7_burst_reads_stale_1367() {
    // genlock-relock 'NDI cam7': depth=15 due=15 erased=1 head_skew_ms=267
    //                            anchor_ns=250786331 sel_vs_newest_due=-13
    let q = burst_queue(W0, 15, 266_666_672);
    let (sel_anchor, sel_configured) = picks(&q, W0, 250_786_331);
    assert_eq!(sel_anchor, 1);
    assert_eq!(sel_anchor as i64 - (15 - 1), -13);
    assert!(relock_anchor_is_stale(sel_anchor, sel_configured, 2));
}

#[test]
fn ordinary_jitter_within_one_canvas_tick_keeps_the_anchor_1367() {
    // A healthy ingest: newest frame ~33 ms old, the anchor one source frame and one full
    // canvas tick deeper than the configured pick. Both are jitter, so #1003's phase
    // continuity must hold.
    let q = burst_queue(W0, 14, 250_000_000);
    let newest_age = 250_000_000 - 13 * I60;
    for (extra_ns, expect_gap) in [(I60 + 1_000_000, 1usize), (I30 + 1_000_000, 2)] {
        let (sel_anchor, sel_configured) = picks(&q, W0, newest_age + extra_ns);
        assert_eq!(
            sel_configured - sel_anchor,
            expect_gap,
            "fixture: the anchor {extra_ns} ns deeper sits {expect_gap} source frame(s) back"
        );
        assert!(
            !relock_anchor_is_stale(sel_anchor, sel_configured, 2),
            "a {expect_gap}-frame gap on a 60-into-30 source is within one canvas tick — \
             the anchor must be kept (#1003 phase continuity)"
        );
    }
}

#[test]
fn stale_boundary_is_strictly_more_than_n_source_frames_1367() {
    // n = 2 (60-into-30): a gap of 2 is one canvas tick, 3 is past it.
    assert!(!relock_anchor_is_stale(10, 12, 2));
    assert!(relock_anchor_is_stale(10, 13, 2));
    // n = 1 (30-into-30): one frame is one tick.
    assert!(!relock_anchor_is_stale(10, 11, 1));
    assert!(relock_anchor_is_stale(10, 12, 1));
    // n = 3: a 90-into-30 multiple scales the same way.
    assert!(!relock_anchor_is_stale(0, 3, 3));
    assert!(relock_anchor_is_stale(0, 4, 3));
}

#[test]
fn an_unmeasured_multiple_counts_as_one_1367() {
    assert!(!relock_anchor_is_stale(5, 6, 0));
    assert!(relock_anchor_is_stale(5, 7, 0));
}

#[test]
fn an_anchor_pick_at_or_after_the_configured_pick_is_never_stale_1367() {
    // The anchor is floored at the configured latency, so it never targets a YOUNGER frame; a
    // caller that hands in the reverse order still gets "keep", never an underflow.
    assert!(!relock_anchor_is_stale(7, 7, 2));
    assert!(!relock_anchor_is_stale(9, 4, 1));
    assert!(!relock_anchor_is_stale(usize::MAX, 0, 1));
    assert!(relock_anchor_is_stale(0, usize::MAX, u32::MAX - 1));
}

/// The outcome of [`run_burst`]: how many ticks took the BACKLOG branch and the queue depth
/// after the run.
struct BurstRun {
    relocks: u64,
    final_depth: usize,
    stale_resets: u64,
}

/// A 60-into-30 BACKLOG-branch replay of the logged burst. The queue starts at the logged
/// 20:50:58 shape (17 frames, head 300 ms old, anchor 283.6 ms); afterwards the sender is clean
/// (a frame every source interval, 33.3 ms transport). Each tick either takes the BACKLOG branch
/// (depth above the issue-859 threshold, a due frame) with the #1003 anchor selection and — when
/// `stale_check` is on — the issue-1367 reset, or presents one frame of the steady 60-into-30
/// pair (the other one is decimated) and re-samples the anchor from it.
fn run_burst(ticks: u64, stale_check: bool) -> BurstRun {
    let threshold = backlog_relock_threshold(LATENCY_MS, 60, 1, 2);
    let head_age = 300_000_000u64;
    let transport = I30;
    let first_stamp = W0 - head_age;
    let mut next_frame = 17u64;
    let mut queue: std::collections::VecDeque<u64> =
        burst_queue(W0, 17, head_age).into_iter().collect();
    let mut anchor = 283_636_965u64;
    let mut relocks = 0u64;
    let mut stale_resets = 0u64;
    for k in 0..ticks {
        let wall = W0 + k * I30;
        loop {
            let stamp = first_stamp + next_frame * I60;
            if stamp + transport > wall {
                break;
            }
            queue.push_back(stamp);
            next_frame += 1;
        }
        let reserve_deadline = wall - LATENCY_MS as u64 * 1_000_000;
        let due = queue
            .iter()
            .take_while(|&&ts| ts <= reserve_deadline)
            .count();
        if queue.is_empty() {
            continue;
        }
        if queue.len() as u64 > threshold && due > 0 {
            relocks += 1;
            let q: Vec<u64> = queue.iter().copied().collect();
            let (mut sel, sel_configured) = picks(&q, wall, anchor);
            let stale = stale_check && relock_anchor_is_stale(sel, sel_configured, 2);
            if anchor != 0 && (sel == 0 || stale) {
                if stale {
                    stale_resets += 1;
                }
                anchor = 0;
                sel = relock_select_nearest(&q, wall, relock_anchor_age_ns(0, LATENCY_MS));
            }
            for _ in 0..sel {
                queue.pop_front();
            }
            queue.pop_front();
        } else {
            // STEADY 60-into-30: present one of the pair, decimate the other.
            let ts = queue.pop_front().expect("non-empty");
            if due >= 2 {
                queue.pop_front();
            }
            anchor = phase_anchor_from_present(wall, ts);
        }
    }
    BurstRun {
        relocks,
        final_depth: queue.len(),
        stale_resets,
    }
}

#[test]
fn the_logged_burst_used_to_relock_every_tick_1367() {
    // Anti-tautology: without the reset the replay reproduces the live loop — the anchor
    // pick sheds only the frames that aged past it and the depth never falls below the
    // threshold, so the BACKLOG branch fires on every tick of the 6 s window.
    let run = run_burst(180, false);
    assert_eq!(
        run.relocks, 180,
        "the pre-1367 selection must relock every tick after the burst (the live loop)"
    );
    assert!(run.final_depth as u64 > backlog_relock_threshold(LATENCY_MS, 60, 1, 2));
}

#[test]
fn the_logged_burst_sheds_in_one_relock_1367() {
    let run = run_burst(180, true);
    assert_eq!(
        run.relocks, 1,
        "after an arrival burst one backlog relock must shed to the configured-latency phase \
         and the branch must not fire again (live: 5028 relocks in 3 minutes)"
    );
    assert_eq!(
        run.stale_resets, 1,
        "the one relock must be the stale-anchor reset"
    );
    assert!(
        run.final_depth <= 2,
        "the queue must settle at the steady 60-into-30 depth, got {}",
        run.final_depth
    );
}
