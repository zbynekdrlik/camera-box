//! Issue 1367 (design 5844353368) — the unit tests of the shallow latch's STICKY content floor
//! (`n1_shallow_sticky_track` and the tracker latching `max(p90, sticky) + 1`). A `#[path]` child
//! of `genlock_n1_depth_tests.rs`, split out to keep that file under the ~1000-line budget.

use super::*;

/// One on-grid present tick at pin 3 (base 1) whose budgeted latch floor is `latch` frames.
fn at(latch: u64) -> ShallowTick {
    ShallowTick {
        latch_floor_frames: latch,
        ..tick(false, 1)
    }
}

const MIN_NS: u64 = 60 * 1_000_000_000;
const T0: u64 = 1_000_000 * 1_000_000_000;

/// Feed `n` ticks of latch floor `latch`, audio flowing or not, one frame apart from `from_ns`.
fn feed(s: &mut ShallowSticky, latch: u64, audio: bool, n: u32, from_ns: u64) -> u64 {
    let mut out = s.floor_frames;
    for k in 0..u64::from(n) {
        out = n1_shallow_sticky_track(s, &at(latch), audio, from_ns + k * I30);
    }
    out
}

#[test]
fn the_sticky_floor_keeps_a_content_block_observed_while_audio_flows_1367() {
    let mut s = ShallowSticky::default();
    // no audio: nothing is observed, however deep the content.
    assert_eq!(feed(&mut s, 2, false, 3 * N1_SHALLOW_SETTLE_TICKS, T0), 0);
    // one whole block of content (budgeted latch floor 2) while the audio flows: sticky 2.
    assert_eq!(feed(&mut s, 2, true, N1_SHALLOW_SETTLE_TICKS - 1, T0), 0);
    assert_eq!(feed(&mut s, 2, true, 1, T0 + MIN_NS), 2);
    assert_eq!(s.seen_ns, T0 + MIN_NS);
    // idle blocks (latch floor 1) never lower it.
    assert_eq!(
        feed(
            &mut s,
            1,
            true,
            5 * N1_SHALLOW_SETTLE_TICKS,
            T0 + 2 * MIN_NS
        ),
        2
    );
    // a deeper content block raises it.
    assert_eq!(
        feed(&mut s, 3, true, N1_SHALLOW_SETTLE_TICKS, T0 + 3 * MIN_NS),
        3
    );
    // a relock never clears it (it only restarts the observation block).
    let relock = ShallowTick {
        relock: true,
        ..at(1)
    };
    assert_eq!(
        n1_shallow_sticky_track(&mut s, &relock, true, T0 + 4 * MIN_NS),
        3
    );
    assert_eq!(s.obs_ticks, 1);
}

#[test]
fn an_idle_relock_latches_on_the_sticky_content_floor_1367() {
    // the live 26.9.2026 sequence: an idle lock (latch floor 1 -> D 2), a song (content latch floor
    // 2), then an idle relock. Without the sticky floor the relock falls back to D 2 and the next
    // song start re-measures; with it the relock keeps D 3.
    let mut sticky = ShallowSticky::default();
    let mut s = ShallowDepth::default();
    let mut now = T0;
    let mut step =
        |sticky: &mut ShallowSticky, s: &mut ShallowDepth, relock: bool, raw: u64, latch: u64| {
            let mut t = ShallowTick {
                relock,
                latch_floor_frames: latch,
                ..tick(false, raw)
            };
            t.sticky_floor_frames = n1_shallow_sticky_track(sticky, &t, true, now);
            now += I30;
            n1_shallow_track(s, t)
        };
    let mut latches = Vec::new();
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        if step(&mut sticky, &mut s, i == 0, 1, 1) {
            latches.push(s.target_frames);
        }
    }
    // the song: raw floor 2 = D, so the rise re-measures once onto D 3 (the first song after an OBS
    // start may still re-measure once).
    for _ in 0..4 * N1_SHALLOW_SETTLE_TICKS {
        if step(&mut sticky, &mut s, false, 2, 2) {
            latches.push(s.target_frames);
        }
    }
    assert_eq!(sticky.floor_frames, 2, "the content block is sticky");
    // the idle relock keeps D 3, and the next song start never re-measures.
    for i in 0..2 * N1_SHALLOW_SETTLE_TICKS {
        if step(&mut sticky, &mut s, i == 0, 1, 1) {
            latches.push(s.target_frames);
        }
    }
    for _ in 0..4 * N1_SHALLOW_SETTLE_TICKS {
        if step(&mut sticky, &mut s, false, 2, 2) {
            latches.push(s.target_frames);
        }
        assert!(!s.measuring, "the second song start re-measured");
    }
    assert_eq!(
        latches,
        [2, 3, 3],
        "idle lock, the first song's re-measure, the idle relock"
    );
}

#[test]
fn the_sticky_floor_decays_one_frame_per_30_min_without_observation_1367() {
    let mut s = ShallowSticky::default();
    assert_eq!(feed(&mut s, 3, true, N1_SHALLOW_SETTLE_TICKS, T0), 3);
    let seen = s.seen_ns;
    // idle (latch floor 1) for 29 min: kept.
    assert_eq!(feed(&mut s, 1, true, 1, seen + 29 * MIN_NS), 3);
    // 30 min without an observation at its level: one frame.
    assert_eq!(
        feed(&mut s, 1, true, 1, seen + N1_SHALLOW_STICKY_DECAY_NS),
        2
    );
    // and one more per further 30 min (no audio needed for the decay).
    assert_eq!(
        feed(
            &mut s,
            1,
            false,
            1,
            seen + 2 * N1_SHALLOW_STICKY_DECAY_NS - 1
        ),
        2
    );
    assert_eq!(
        feed(&mut s, 1, false, 1, seen + 2 * N1_SHALLOW_STICKY_DECAY_NS),
        1
    );
    // an observation AT the level resets the clock: sticky 2 seen again at +10 min holds past
    // +30 min from the first observation.
    let mut s = ShallowSticky::default();
    assert_eq!(feed(&mut s, 2, true, N1_SHALLOW_SETTLE_TICKS, T0), 2);
    let first = s.seen_ns;
    assert_eq!(
        feed(
            &mut s,
            2,
            true,
            N1_SHALLOW_SETTLE_TICKS,
            first + 10 * MIN_NS
        ),
        2
    );
    assert_eq!(feed(&mut s, 1, true, 1, first + 35 * MIN_NS), 2);
    // a wall clock stepped BACK under the last observation never decays it.
    assert_eq!(feed(&mut s, 1, true, 1, first - MIN_NS), 2);
}

#[test]
fn a_transient_an_over_clamp_block_or_a_floor_at_base_never_becomes_sticky_1367() {
    let mut s = ShallowSticky::default();
    // a block whose p90 - p10 spread exceeds one frame (a transient in progress).
    for k in 0..N1_SHALLOW_SETTLE_TICKS {
        let latch = if k < 30 { 4 } else { 1 };
        n1_shallow_sticky_track(&mut s, &at(latch), true, T0 + u64::from(k) * I30);
    }
    assert_eq!(s.floor_frames, 0, "a transient block became sticky");
    // a whole block in the over-clamp bin (a genuinely slow sender or a long transient): the latch
    // clamps and reports it itself; a sticky floor there would clamp every later lock too.
    assert_eq!(
        feed(&mut s, 11, true, N1_SHALLOW_SETTLE_TICKS, T0 + MIN_NS),
        0
    );
    // a floor at base (every deep source, a lag under one frame) is no content floor at all.
    let deep = ShallowTick {
        base_frames: 30,
        latch_floor_frames: 1,
        ..tick(false, 1)
    };
    for k in 0..u64::from(N1_SHALLOW_SETTLE_TICKS) {
        n1_shallow_sticky_track(&mut s, &deep, true, T0 + 2 * MIN_NS + k * I30);
    }
    assert_eq!(s.floor_frames, 0, "a floor at base became sticky");
    // off-grid ticks are not sampled.
    let off = ShallowTick {
        on_grid: false,
        ..at(2)
    };
    for k in 0..u64::from(N1_SHALLOW_SETTLE_TICKS) {
        n1_shallow_sticky_track(&mut s, &off, true, T0 + 3 * MIN_NS + k * I30);
    }
    assert_eq!((s.floor_frames, s.obs_ticks), (0, 0));
}

#[test]
fn an_n2_tick_or_the_min_latency_marker_clears_the_sticky_floor_1367() {
    let mut s = ShallowSticky::default();
    assert_eq!(feed(&mut s, 2, true, N1_SHALLOW_SETTLE_TICKS, T0), 2);
    let n2 = ShallowTick { n1: false, ..at(2) };
    assert_eq!(n1_shallow_sticky_track(&mut s, &n2, true, T0 + MIN_NS), 0);
    assert_eq!(s, ShallowSticky::default());
    // the imag marker keeps no sticky floor at all (ROZHODNUTÉ 5842848307 -- minimum latency).
    let imag = ShallowTick {
        min_latency_box: true,
        ..at(2)
    };
    for k in 0..u64::from(2 * N1_SHALLOW_SETTLE_TICKS) {
        assert_eq!(
            n1_shallow_sticky_track(&mut s, &imag, true, T0 + k * I30),
            0
        );
    }
    // and the tracker ignores a sticky floor there even if one is passed in.
    let mut d = ShallowDepth::default();
    for i in 0..N1_SHALLOW_SETTLE_TICKS {
        n1_shallow_track(
            &mut d,
            ShallowTick {
                min_latency_box: true,
                sticky_floor_frames: 3,
                ..tick(i == 0, 1)
            },
        );
    }
    assert_eq!((d.target_frames, d.capped), (2, false));
}
