//! Issue 1372 — bench: the logged resolume audio-callback pattern through the VBAN send pacing.
//!
//! A test-only `#[path]` child of `vban_pacing`. It simulates the two threads of the vendored
//! obs-vban output on one timeline:
//!
//! * **The OBS audio thread.** Callback `k` starts at its tick (`k × 1024 / 48000 s`) or when
//!   callback `k − 1` finished, whichever is later, and the 1024-sample mix block is handed to the
//!   output at the END of the callback. The costs are the logged ones (finding 5845239583): 14–21 ms
//!   per callback, one 28 ms callback a second, and one stall callback (55.3 ms logged at
//!   11:10:56). The first block arrives after 14 ms, the earliest arrival, which is the worst
//!   anchor reference for the jitter buffer.
//! * **The send thread.** With a deadline it sleeps to it (`os_sleepto_ns`) and wakes up to 0.3 ms
//!   late. Without one it waits on the audio event with the 10 ms idle timeout.
//!
//! The measured quantity is what the receiver sees: the send instants. Within a run of packets
//! (between underflows) the residual `send_i − i × packet_duration` must stay inside 1 ms, and
//! every sample produced is sent, dropped (counted) or still buffered; nothing is fabricated.
//! The same timeline through a model of the 0.3.1 loop proves the bench can tell the two apart.

use super::*;

const RATE: u32 = 48_000;
const BLOCK: u64 = 1024;
const PS: u32 = 239;
const MS: u64 = 1_000_000;
/// The simulation starts 1 s into the clock so that every instant is > 0.
const BASE_NS: u64 = 1_000_000_000;
/// Ten minutes of audio blocks.
const BLOCKS_10_MIN: usize = 28_125;
const STALL_BLOCK: usize = 15_000;
const LOGGED_STALL_NS: u64 = 55_300_000;
const WAKE_JITTER_MAX_NS: u64 = 300_000;

/// xorshift64*: deterministic, std-only.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn uniform(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// The instant each mix block reaches the output (the end of its callback).
fn block_arrivals(n_blocks: usize, rng: &mut Rng, stall_block: usize, stall_ns: u64) -> Vec<u64> {
    let mut out = Vec::with_capacity(n_blocks);
    let mut prev_end = 0u64;
    for k in 0..n_blocks {
        let tick = BASE_NS + samples_to_ns(k as u64 * BLOCK, RATE);
        let cost = if k == 0 {
            14 * MS
        } else if k == stall_block {
            stall_ns
        } else if k % 47 == 23 {
            28 * MS
        } else {
            rng.uniform(14 * MS, 21 * MS)
        };
        let end = tick.max(prev_end) + cost;
        out.push(end);
        prev_end = end;
    }
    out
}

#[derive(Debug, Default)]
struct Sim {
    /// The send instant of every packet, in order.
    sends: Vec<u64>,
    /// Indexes into `sends` where a new run starts (after an underflow).
    run_starts: Vec<usize>,
    underflows: u64,
    overflows: u64,
    produced: u64,
    sent_samples: u64,
    dropped: u64,
    left: u64,
    /// The largest number of packets sent in one wake.
    max_send_per_wake: u32,
}

/// The patched send thread driven by `Pacing::step`.
fn run_paced(p: &mut Pacing, arrivals: &[u64], rng: &mut Rng) -> Sim {
    let end_ns = arrivals.last().copied().unwrap_or(BASE_NS) + 50 * MS;
    let ps = u64::from(p.packet_samples);
    let mut sim = Sim {
        run_starts: vec![0],
        ..Sim::default()
    };
    let (mut now, mut wake, mut next, mut buffered) = (BASE_NS, 0u64, 0usize, 0u64);
    loop {
        let t = if wake == 0 {
            let event = arrivals.get(next).copied().unwrap_or(u64::MAX);
            event.min(now + 10 * MS) + 20_000
        } else {
            wake.max(now) + rng.uniform(0, WAKE_JITTER_MAX_NS)
        };
        if t > end_ns {
            break;
        }
        now = t;
        while next < arrivals.len() && arrivals[next] <= now {
            buffered += BLOCK;
            sim.produced += BLOCK;
            next += 1;
        }
        let underflows_before = p.underflows;
        let s = p.step(now, buffered);
        assert!(s.drop_samples + u64::from(s.send) * ps <= buffered);
        buffered -= s.drop_samples + u64::from(s.send) * ps;
        sim.dropped += s.drop_samples;
        sim.sent_samples += u64::from(s.send) * ps;
        sim.max_send_per_wake = sim.max_send_per_wake.max(s.send);
        for _ in 0..s.send {
            sim.sends.push(now);
        }
        if p.underflows > underflows_before {
            sim.run_starts.push(sim.sends.len());
        }
        wake = s.wake_ns;
    }
    sim.underflows = p.underflows;
    sim.overflows = p.overflows;
    sim.left = buffered;
    sim
}

/// A model of the obs-vban 0.3.1 loop: wake on the audio event or after `packet × 1000 / rate`
/// truncated milliseconds (100 ms before the first packet), send at most one packet per wake.
fn run_upstream_031(arrivals: &[u64]) -> Sim {
    let end_ns = arrivals.last().copied().unwrap_or(BASE_NS) + 50 * MS;
    let ps = u64::from(PS);
    let mut sim = Sim {
        run_starts: vec![0],
        ..Sim::default()
    };
    let (mut now, mut wait_ms, mut next, mut buffered) = (BASE_NS, 100u64, 0usize, 0u64);
    loop {
        // The auto-reset event is set by every arrival; one wait consumes all of them.
        let event = arrivals.get(next).copied().unwrap_or(u64::MAX);
        let t = event.max(now).min(now + wait_ms * MS);
        if t > end_ns {
            break;
        }
        now = t;
        while next < arrivals.len() && arrivals[next] <= now {
            buffered += BLOCK;
            sim.produced += BLOCK;
            next += 1;
        }
        if buffered >= ps {
            buffered -= ps;
            sim.sent_samples += ps;
            sim.sends.push(now);
            wait_ms = ps * 1000 / u64::from(RATE);
        }
    }
    sim.max_send_per_wake = 1;
    sim.left = buffered;
    sim
}

/// Per run: the spread of `send_i − i × packet_duration` (the send-time jitter against the
/// sample clock) and the largest gap between two sends.
fn run_stats(sim: &Sim) -> Vec<(u64, u64, usize)> {
    let mut bounds = sim.run_starts.clone();
    bounds.push(sim.sends.len());
    bounds
        .windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| {
            let run = &sim.sends[w[0]..w[1]];
            let resid: Vec<i128> = run
                .iter()
                .enumerate()
                .map(|(i, &t)| {
                    i128::from(t) - i128::from(samples_to_ns(i as u64 * u64::from(PS), RATE))
                })
                .collect();
            let spread = (resid.iter().max().unwrap() - resid.iter().min().unwrap()) as u64;
            let max_gap = run.windows(2).map(|g| g[1] - g[0]).max().unwrap_or(0);
            (spread, max_gap, run.len())
        })
        .collect()
}

fn assert_conserved(sim: &Sim) {
    assert_eq!(
        sim.produced,
        sim.sent_samples + sim.dropped + sim.left,
        "every produced sample is sent, dropped (counted) or still buffered; none fabricated"
    );
}

#[test]
fn logged_pattern_at_64ms_is_smooth_with_no_underflow() {
    let dur = samples_to_ns(u64::from(PS), RATE);
    for seed in 1..=8u64 {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed);
        let arrivals = block_arrivals(BLOCKS_10_MIN, &mut rng, STALL_BLOCK, LOGGED_STALL_NS);
        let mut p = Pacing::new(64, PS, RATE);
        let sim = run_paced(&mut p, &arrivals, &mut rng);
        assert_conserved(&sim);
        assert_eq!(
            sim.underflows, 0,
            "seed {seed}: the logged pattern underflowed at 64 ms"
        );
        assert_eq!(sim.overflows, 0, "seed {seed}: overflow at 64 ms");
        let stats = run_stats(&sim);
        assert_eq!(stats.len(), 1, "seed {seed}: one continuous run");
        let (spread, max_gap, sent) = stats[0];
        assert!(
            sim.left < ms_to_samples(200, RATE),
            "seed {seed}: {} samples never sent ({sent} packets went out)",
            sim.left
        );
        assert!(
            spread <= MS,
            "seed {seed}: send-time jitter {spread} ns > 1 ms at 64 ms"
        );
        assert!(
            max_gap <= dur + MS,
            "seed {seed}: a {max_gap} ns gap between packets"
        );
        assert!(p.late_max_ns <= WAKE_JITTER_MAX_NS);
        assert_eq!(
            sim.max_send_per_wake, 1,
            "seed {seed}: on time, one packet per deadline"
        );
    }
}

#[test]
fn the_031_loop_is_bursty_on_the_same_pattern() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ 1);
    let arrivals = block_arrivals(BLOCKS_10_MIN, &mut rng, STALL_BLOCK, LOGGED_STALL_NS);
    let sim = run_upstream_031(&arrivals);
    assert_conserved(&sim);
    let (spread, max_gap, _) = run_stats(&sim)[0];
    assert!(
        spread > 20 * MS,
        "the 0.3.1 model should wander by tens of ms, got {spread} ns — the bench no longer bites"
    );
    assert!(
        max_gap > 20 * MS,
        "the 0.3.1 model should leave gaps, got {max_gap} ns"
    );
}

#[test]
fn a_stall_longer_than_the_buffer_is_counted_never_hidden() {
    let mut rng = Rng(7);
    let arrivals = block_arrivals(BLOCKS_10_MIN / 4, &mut rng, 3_000, 150 * MS);
    let mut p = Pacing::new(64, PS, RATE);
    let sim = run_paced(&mut p, &arrivals, &mut rng);
    assert_conserved(&sim);
    assert!(
        sim.underflows >= 1,
        "a 150 ms stall at 64 ms must count an underflow"
    );
    assert!(
        sim.underflows <= 2,
        "one stall, {} underflows",
        sim.underflows
    );
    assert_eq!(sim.dropped, 0);
    for (spread, _, _) in run_stats(&sim) {
        assert!(
            spread <= MS,
            "each run stays on its own schedule, spread {spread} ns"
        );
    }

    // The same stall inside a 200 ms buffer is absorbed.
    let mut rng = Rng(7);
    let arrivals = block_arrivals(BLOCKS_10_MIN / 4, &mut rng, 3_000, 150 * MS);
    let mut p = Pacing::new(200, PS, RATE);
    let sim = run_paced(&mut p, &arrivals, &mut rng);
    assert_conserved(&sim);
    assert_eq!(sim.underflows, 0);
    assert_eq!(sim.overflows, 0);
    assert!(run_stats(&sim)[0].0 <= MS);
}

#[test]
fn a_frozen_send_thread_drops_counted_and_resumes() {
    // The send thread misses 400 ms of deadlines (descheduled). The backlog exceeds target +
    // 200 ms: the oldest are dropped and counted, then the late packets go out in one wake.
    let mut rng = Rng(11);
    let arrivals = block_arrivals(2_000, &mut rng, usize::MAX, 0);
    let mut p = Pacing::new(64, PS, RATE);
    let (mut buffered, mut next) = (0u64, 0usize);
    let mut now = BASE_NS;
    let feed = |now: u64, buffered: &mut u64, next: &mut usize| {
        while *next < arrivals.len() && arrivals[*next] <= now {
            *buffered += BLOCK;
            *next += 1;
        }
    };
    // Run normally for 2 s.
    let mut wake = 0u64;
    while now < BASE_NS + 2_000 * MS {
        now = if wake == 0 { now + MS } else { wake };
        feed(now, &mut buffered, &mut next);
        let s = p.step(now, buffered);
        buffered -= s.drop_samples + u64::from(s.send) * u64::from(PS);
        wake = s.wake_ns;
    }
    assert_eq!((p.underflows, p.overflows), (0, 0));
    now += 400 * MS;
    feed(now, &mut buffered, &mut next);
    let s = p.step(now, buffered);
    assert_eq!(p.overflows, 1, "the 400 ms backlog is an overflow");
    assert!(s.drop_samples > 0);
    assert!(
        s.send >= 2,
        "every due packet that is buffered goes out in this wake"
    );
}
