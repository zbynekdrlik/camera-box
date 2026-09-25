//! Issue 1367 — the unit tests of the stale-burst backlog relock reset
//! ([`relock_anchor_is_stale`] against the EXPECTED-depth pick, ROZHODNUTÉ 5840479751), a
//! `#[path]` child of `genlock_backlog` kept out of that already-long module. They compile with the
//! module.

use super::*;
use crate::genlock_n1_depth::{n1_base_frames, n1_expected_depth_frames};

/// Source interval of a 60 fps sender (ns).
const I60: u64 = 16_666_667;
/// Canvas interval of the 30 fps canvas, also the 30 fps source interval (ns).
const I30: u64 = 33_333_333;
/// The strih camera pin (ms) — the logged relock lines carried `latency_ms=3`.
const LATENCY_MS: u32 = 3;
/// A wall instant well clear of zero.
const W0: u64 = 1_800_000_000_000_000_000;

/// A queue of `depth` frames, oldest first, whose OLDEST frame is `head_age_ns` old at `wall` and
/// whose frames sit `grid` apart.
fn queue_at(wall: u64, depth: usize, head_age_ns: u64, grid: u64) -> Vec<u64> {
    (0..depth as u64)
        .map(|i| wall - (head_age_ns - i * grid))
        .collect()
}

/// A 60-into-30 queue (the strih camera ingest).
fn burst_queue(wall: u64, depth: usize, head_age_ns: u64) -> Vec<u64> {
    queue_at(wall, depth, head_age_ns, I60)
}

/// The two picks the BACKLOG branch compares: against the tracked anchor, and against the depth
/// the source is supposed to hold (`expected_frames`, 0 = the configured latency).
fn picks(
    q: &[u64],
    wall: u64,
    anchor_ns: u64,
    latency_ms: u32,
    expected_frames: u64,
    interval_ns: u64,
) -> (usize, usize) {
    (
        relock_select_nearest(q, wall, relock_anchor_age_ns(anchor_ns, latency_ms)),
        relock_select_nearest(
            q,
            wall,
            relock_expected_age_ns(expected_frames, latency_ms, interval_ns),
        ),
    )
}

#[test]
fn the_logged_cam6_burst_reads_stale_1367() {
    // Live 25.9.2026 20:50:58 on strih-lx:
    // genlock-relock 'NDI cam6': depth=17 due=17 erased=1 head_skew_ms=300
    //                            anchor_ns=283636965 sel_vs_newest_due=-15
    // A 60-into-30 camera has no N==1 governor depth, so the expected pick is the configured one.
    let q = burst_queue(W0, 17, 300_000_000);
    let (sel_anchor, sel_expected) = picks(&q, W0, 283_636_965, LATENCY_MS, 0, I30);
    // The replay reproduces the logged line exactly: erased=1, and 1 - (17 - 1) = -15.
    assert_eq!(sel_anchor, 1, "the anchor pick must be the logged erased=1");
    assert_eq!(
        sel_expected, 16,
        "the configured-latency pick is the newest due frame"
    );
    assert_eq!(
        sel_anchor as i64 - (17 - 1),
        -15,
        "sel_vs_newest_due must match the log"
    );
    assert!(
        relock_anchor_is_stale(sel_anchor, sel_expected, 2),
        "an anchor 15 source frames behind the expected phase is an arrival-burst phase, \
         not jitter — the relock must drop it and shed to the configured latency"
    );
}

#[test]
fn the_logged_cam7_burst_reads_stale_1367() {
    // genlock-relock 'NDI cam7': depth=15 due=15 erased=1 head_skew_ms=267
    //                            anchor_ns=250786331 sel_vs_newest_due=-13
    let q = burst_queue(W0, 15, 266_666_672);
    let (sel_anchor, sel_expected) = picks(&q, W0, 250_786_331, LATENCY_MS, 0, I30);
    assert_eq!(sel_anchor, 1);
    assert_eq!(sel_anchor as i64 - (15 - 1), -13);
    assert!(relock_anchor_is_stale(sel_anchor, sel_expected, 2));
}

#[test]
fn ordinary_jitter_within_one_canvas_tick_keeps_the_anchor_1367() {
    // A healthy 60-into-30 ingest: newest frame ~33 ms old, the anchor one source frame and one
    // full canvas tick deeper than the configured pick. Both are jitter.
    let q = burst_queue(W0, 14, 250_000_000);
    let newest_age = 250_000_000 - 13 * I60;
    for (extra_ns, expect_gap) in [(I60 + 1_000_000, 1usize), (I30 + 1_000_000, 2)] {
        let (sel_anchor, sel_expected) = picks(&q, W0, newest_age + extra_ns, LATENCY_MS, 0, I30);
        assert_eq!(
            sel_expected - sel_anchor,
            expect_gap,
            "fixture: the anchor {extra_ns} ns deeper sits {expect_gap} source frame(s) back"
        );
        assert!(
            !relock_anchor_is_stale(sel_anchor, sel_expected, 2),
            "a {expect_gap}-frame gap on a 60-into-30 source is jitter — the anchor must be kept \
             (#1003 phase continuity)"
        );
    }
}

/// The stream `NDI 2ME PGM` (30-into-30, pin 987 ms) held by the N==1 governor at `base + 1` =
/// 31 frames, relocking on a render tick 5 ms (and 10 ms) late: the anchor is CORRECT and must be
/// kept. Against the bare configured pick it reads 2 frames behind (the lane's review finding);
/// against the governor's expected depth it reads 0.
#[test]
fn a_deep_base_plus_one_anchor_on_a_late_tick_is_kept_1367() {
    const PIN: u32 = 987;
    assert_eq!(n1_base_frames(PIN, I30), 30, "fixture: base at pin 987");
    for eps in [5_000_000u64, 10_000_000] {
        // 40 frames aged 40..1 frames, all read `eps` late.
        let q = queue_at(W0, 40, 40 * I30 + eps, I30);
        let floor = I30 + eps; // the newest queued frame's age
        let expected = n1_expected_depth_frames(floor, PIN, I30, 0);
        assert_eq!(expected, 31, "a deep N==1 source is held at base + 1");
        let anchor = 31 * I30 + 2_000_000; // sampled on a STEADY tick 2 ms late
        let (sel_anchor, sel_expected) = picks(&q, W0, anchor, PIN, expected, I30);
        let (_, sel_configured) = picks(&q, W0, anchor, PIN, 0, I30);
        assert_eq!(
            sel_configured - sel_anchor,
            2,
            "fixture (eps {eps}): against the bare pin a correct anchor reads 2 frames back"
        );
        assert!(
            !relock_anchor_is_stale(sel_anchor, sel_expected, 1),
            "eps {eps}: a base + 1 anchor is where the governor holds the 2ME PGM — keep it"
        );
    }
    // The pre-1367 absorbing 32-frame state is still within tolerance of base + 1.
    let q = queue_at(W0, 40, 40 * I30 + 5_000_000, I30);
    let (a, e) = picks(&q, W0, 32 * I30, PIN, 31, I30);
    assert!(!relock_anchor_is_stale(a, e, 1));
}

/// A deep N==1 anchor far behind `base + 1` (a burst on the 2ME PGM) is still reset.
#[test]
fn a_deep_anchor_far_behind_base_plus_one_is_stale_1367() {
    let q = queue_at(W0, 40, 40 * I30 + 5_000_000, I30);
    let (a, e) = picks(&q, W0, 36 * I30, 987, 31, I30);
    assert_eq!(e - a, 5);
    assert!(relock_anchor_is_stale(a, e, 1));
}

/// A shallow governed N==1 source (a cg feed at pin 3 ms, base 1) latched at the clamp
/// D = base + 3 = 4 frames, relocking on a 10 ms late tick while its arrival floor has fallen to
/// one frame: the anchor at D is correct and must be kept. Against the configured pick (the
/// newest frame) it reads 3 frames behind, which `n + 1` alone would still reset.
#[test]
fn a_shallow_latched_d_anchor_on_a_late_tick_is_kept_1367() {
    let base = n1_base_frames(LATENCY_MS, I30);
    assert_eq!(base, 1, "fixture: base at pin 3");
    let d = base + 3;
    let eps = 10_000_000u64;
    let q = queue_at(W0, 8, 8 * I30 + eps, I30);
    let floor = I30 + eps;
    let expected = n1_expected_depth_frames(floor, LATENCY_MS, I30, d);
    assert_eq!(
        expected, d,
        "a governed shallow source is held at its latched D"
    );
    let anchor = d * I30 + 3_000_000;
    let (sel_anchor, sel_expected) = picks(&q, W0, anchor, LATENCY_MS, expected, I30);
    let (_, sel_configured) = picks(&q, W0, anchor, LATENCY_MS, 0, I30);
    assert_eq!(
        sel_configured - sel_anchor,
        3,
        "fixture: 3 frames behind the bare pin"
    );
    assert!(
        !relock_anchor_is_stale(sel_anchor, sel_expected, 1),
        "an anchor at the latched D is where the governor holds the source — keep it"
    );
}

#[test]
fn the_expected_depth_comes_from_the_n1_governor_1367() {
    // Deep (pin 987): base + 1, whatever the shallow state says.
    assert_eq!(n1_expected_depth_frames(I30, 987, I30, 0), 31);
    assert_eq!(n1_expected_depth_frames(I30, 987, I30, 4), 31);
    // Shallow and governed: the latched D.
    assert_eq!(n1_expected_depth_frames(I30, 3, I30, 3), 3);
    // Shallow with no latched D, or a degenerate interval: no governor depth.
    assert_eq!(n1_expected_depth_frames(I30, 3, I30, 0), 0);
    assert_eq!(n1_expected_depth_frames(I30, 987, 0, 4), 0);
    // A deep pin whose arrival floor is too close to the pin is not deep: the latched D, if any.
    assert_eq!(n1_expected_depth_frames(29 * I30, 987, I30, 0), 0);
    assert_eq!(n1_expected_depth_frames(29 * I30, 987, I30, 31), 31);
}

#[test]
fn the_expected_age_is_the_expected_depth_floored_at_the_pin_1367() {
    assert_eq!(relock_expected_age_ns(0, 3, I30), 3_000_000);
    assert_eq!(relock_expected_age_ns(31, 987, I30), 31 * I30);
    assert_eq!(relock_expected_age_ns(1, 987, I30), 987_000_000);
    assert_eq!(relock_expected_age_ns(u64::MAX, 3, I30), u64::MAX);
}

#[test]
fn stale_boundary_is_strictly_more_than_n_plus_one_source_frames_1367() {
    // n = 2 (60-into-30): a gap of 3 is kept, 4 is past it.
    assert!(!relock_anchor_is_stale(10, 13, 2));
    assert!(relock_anchor_is_stale(10, 14, 2));
    // n = 1 (30-into-30): 2 kept, 3 stale.
    assert!(!relock_anchor_is_stale(10, 12, 1));
    assert!(relock_anchor_is_stale(10, 13, 1));
    // n = 3 scales the same way.
    assert!(!relock_anchor_is_stale(0, 4, 3));
    assert!(relock_anchor_is_stale(0, 5, 3));
}

#[test]
fn an_unmeasured_multiple_counts_as_one_1367() {
    assert!(!relock_anchor_is_stale(5, 7, 0));
    assert!(relock_anchor_is_stale(5, 8, 0));
}

#[test]
fn an_anchor_pick_at_or_after_the_expected_pick_is_never_stale_1367() {
    // The anchor is floored at the configured latency; a caller that hands in the reverse order
    // still gets "keep", never an underflow.
    assert!(!relock_anchor_is_stale(7, 7, 2));
    assert!(!relock_anchor_is_stale(9, 4, 1));
    assert!(!relock_anchor_is_stale(usize::MAX, 0, 1));
    assert!(relock_anchor_is_stale(0, usize::MAX, u32::MAX - 2));
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
/// `stale_check` is on — the issue-1367 reset, or presents the newer frame of the steady
/// 60-into-30 pair (the older one is retired) and re-samples the anchor from it. A 60-into-30
/// source has no N==1 governor depth, so the expected pick is the configured one.
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
            let (mut sel, sel_expected) = picks(&q, wall, anchor, LATENCY_MS, 0, I30);
            let stale = stale_check && relock_anchor_is_stale(sel, sel_expected, 2);
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
            // STEADY 60-into-30: retire the older frame of the pair and present the NEWER one,
            // as the C N>=2 steady path does (it presents the newest matured frame).
            if due >= 2 {
                queue.pop_front();
            }
            let ts = queue.pop_front().expect("non-empty");
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
