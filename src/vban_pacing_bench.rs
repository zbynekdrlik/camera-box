//! Issues 1372 + 1381 — bench: the logged resolume audio-callback pattern, mixer stalls and OBS
//! buffering holes through the VBAN send pacing, with the MEASURED send-thread wake lateness.
//!
//! A test-only `#[path]` child of `vban_pacing`. It simulates the two threads of the vendored
//! obs-vban output on one timeline:
//!
//! * **The OBS audio thread.** Callback `k` starts at its tick (`k × 1024 / 48000 s`) or when
//!   callback `k − 1` finished, whichever is later, and the 1024-sample mix block is handed to the
//!   output at the END of the callback. The costs are the logged ones (finding 5845239583): 14–21 ms
//!   per callback, one 28 ms callback a second. A **stall** is one callback of the given length;
//!   the callbacks after it run back to back until they are on their ticks again (the backlog
//!   arrives at the thread's own pace, 1.0–1.5× real time). A **buffering hole** is OBS raising
//!   its audio buffering by N ticks: no block for N ticks and every later tick N ticks later, so
//!   no backlog ever comes (the 27.9 holes: 85, 42, 106, 128, 106 and 490 ms).
//! * **The send thread.** It sleeps to its deadline, or waits on the audio event for at most
//!   [`wait_ms`], exactly as `vban_out_loop` does. A timed wake (a deadline, or an event wait
//!   that timed out) is 0–30 ms late: the `late_max_ms` measured on resolume on 27.9 reached
//!   30.8 ms (the issue-1372 bench assumed 0.3 ms). An audio arrival ends an event wait within
//!   0–2 ms, so the anchor and a late packet start from the real arrival (review round 1: a
//!   30 ms-late anchor gave the bench more margin than the thread has).
//!
//! What the receiver would see is recorded: every packet (audio or silence) with its send instant,
//! the VBAN frame counter, and every sample produced, sent, dropped or still buffered.

use super::*;

const RATE: u32 = 48_000;
const BLOCK: u64 = 1024;
const PS: u32 = 239;
const P: u64 = PS as u64;
const MS: u64 = 1_000_000;
/// The simulation starts 1 s into the clock so that every instant is > 0.
const BASE_NS: u64 = 1_000_000_000;
/// Ten minutes of audio blocks.
const BLOCKS_10_MIN: usize = 28_125;
const EVENT_BLOCK: usize = 15_000;
const LOGGED_STALL_NS: u64 = 55_300_000;
/// The send thread's measured timed-wake lateness (resolume, 27.9: `late_max_ms` up to 30.8 ms).
const WAKE_LATE_MAX_NS: u64 = 30 * MS;
/// How late an audio arrival wakes a thread waiting on the audio event.
const EVENT_WAKE_LATE_MAX_NS: u64 = 2 * MS;
/// The OBS buffering holes of 27.9 in ticks of 1024 samples: 85, 42, 106, 128, 106, 490 ms.
const HOLES_27_9: [(usize, u64); 6] = [
    (3_000, 4),
    (6_000, 2),
    (9_000, 5),
    (12_000, 6),
    (15_000, 5),
    (20_000, 23),
];

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

fn rng(seed: u64) -> Rng {
    Rng(0x9E37_79B9_7F4A_7C15 ^ seed)
}

/// The instant each mix block reaches the output (the end of its callback). `stalls` are
/// `(block, callback ns)`, `holes` are `(block, ticks)`.
fn block_arrivals(
    n_blocks: usize,
    rng: &mut Rng,
    stalls: &[(usize, u64)],
    holes: &[(usize, u64)],
) -> Vec<u64> {
    let mut out = Vec::with_capacity(n_blocks);
    let (mut prev_end, mut shift) = (0u64, 0u64);
    for k in 0..n_blocks {
        shift += holes
            .iter()
            .filter(|(b, _)| *b == k)
            .map(|(_, t)| *t)
            .sum::<u64>();
        let tick = BASE_NS + samples_to_ns((k as u64 + shift) * BLOCK, RATE);
        let stall = stalls.iter().find(|(b, _)| *b == k).map(|(_, ns)| *ns);
        let cost = if k == 0 {
            14 * MS
        } else if let Some(ns) = stall {
            ns
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
    /// The send instant of every packet, in order, and whether it carried audio.
    sends: Vec<(u64, bool)>,
    /// The instant of the first audio packet after the last silence, and of the last drop.
    resumed_at: u64,
    dropped_at: u64,
    produced: u64,
    sent_samples: u64,
    dropped: u64,
    left: u64,
    /// The largest number of packets sent in one wake.
    max_send_per_wake: u32,
    /// Per audio packet: its send instant and the latency the receiver sees (send instant minus
    /// the tick of its first sample on the producer's own sample count).
    latency: Vec<(u64, u64)>,
}

impl Sim {
    fn audio_packets(&self) -> u64 {
        self.sends.iter().filter(|(_, a)| *a).count() as u64
    }

    fn max_gap(&self) -> u64 {
        self.sends
            .windows(2)
            .map(|w| w[1].0 - w[0].0)
            .max()
            .unwrap_or(0)
    }

    /// The median latency of the audio packets sent in `[from, to)`.
    fn median_latency(&self, from: u64, to: u64) -> u64 {
        let mut v: Vec<u64> = self
            .latency
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .map(|(_, l)| *l)
            .collect();
        assert!(!v.is_empty(), "no audio sent in the window");
        v.sort_unstable();
        v[v.len() / 2]
    }
}

/// The paced send thread of `vban_out_loop` driven by `Pacing::step`, every wake up to
/// `wake_late_max` late.
fn run_paced(p: &mut Pacing, arrivals: &[u64], rng: &mut Rng, wake_late_max: u64) -> Sim {
    let end_ns = arrivals.last().copied().unwrap_or(BASE_NS);
    let ps = u64::from(p.packet_samples);
    let mut sim = Sim::default();
    let (mut now, mut next, mut buffered, mut consumed) = (BASE_NS, 0usize, 0u64, 0u64);
    let mut s = Step {
        wait_audio: true,
        ..Step::default()
    };
    loop {
        let t = if s.wait_audio {
            let limit = now + u64::from(wait_ms(now, s.wake_ns)) * MS;
            let event = arrivals.get(next).copied().unwrap_or(u64::MAX);
            if event <= limit {
                event.max(now) + rng.uniform(0, EVENT_WAKE_LATE_MAX_NS)
            } else {
                limit + rng.uniform(0, wake_late_max)
            }
        } else {
            s.wake_ns.max(now) + rng.uniform(0, wake_late_max)
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
        s = p.step(now, buffered);
        assert_eq!(s.drop_samples % ps, 0, "a drop of part of a packet");
        assert!(s.drop_samples + u64::from(s.send) * ps <= buffered);
        buffered -= s.drop_samples + u64::from(s.send) * ps;
        sim.dropped += s.drop_samples;
        consumed += s.drop_samples;
        if s.drop_samples > 0 {
            sim.dropped_at = now;
        }
        if s.send > 0 && sim.sends.last().is_some_and(|&(_, audio)| !audio) {
            sim.resumed_at = now;
        }
        sim.max_send_per_wake = sim.max_send_per_wake.max(s.send + s.silence);
        for _ in 0..s.send {
            sim.latency
                .push((now, now - (BASE_NS + samples_to_ns(consumed, RATE))));
            consumed += ps;
            sim.sent_samples += ps;
            sim.sends.push((now, true));
        }
        for _ in 0..s.silence {
            sim.sends.push((now, false));
        }
    }
    sim.left = buffered;
    sim
}

/// A model of the obs-vban 0.3.1 loop: wake on the audio event or after `packet × 1000 / rate`
/// truncated milliseconds (100 ms before the first packet), send at most one packet per wake.
fn run_upstream_031(arrivals: &[u64]) -> Sim {
    let end_ns = arrivals.last().copied().unwrap_or(BASE_NS) + 50 * MS;
    let mut sim = Sim::default();
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
        if buffered >= P {
            buffered -= P;
            sim.sent_samples += P;
            sim.sends.push((now, true));
            wait_ms = P * 1000 / u64::from(RATE);
        }
    }
    sim.max_send_per_wake = 1;
    sim.left = buffered;
    sim
}

/// The spread of `send_i − i × packet_duration` over all packets (the send-time jitter against
/// the sample clock).
fn send_spread(sim: &Sim) -> u64 {
    let resid: Vec<i128> = sim
        .sends
        .iter()
        .enumerate()
        .map(|(i, &(t, _))| i128::from(t) - i128::from(samples_to_ns(i as u64 * P, RATE)))
        .collect();
    (resid.iter().max().unwrap() - resid.iter().min().unwrap()) as u64
}

fn assert_accounted(p: &Pacing, sim: &Sim, what: &str) {
    assert_eq!(
        sim.produced,
        sim.sent_samples + sim.dropped + sim.left,
        "{what}: every produced sample is sent, dropped (counted) or still buffered; none fabricated"
    );
    // Every drop is whole packets (asserted per wake in `run_paced`), so the frame-counter skip
    // `drop / packet_samples` the thread applies before its sends (pinned by the wiring test) is
    // exact: the receiver's loss counter sees every dropped sample.
    assert_eq!(
        sim.dropped, p.discarded_samples,
        "{what}: every drop is counted"
    );
    assert_eq!(
        (sim.sends.len() as u64 - sim.audio_packets()) * P,
        p.silence_samples,
        "{what}: every silence packet is counted"
    );
}

/// One scenario at 64 ms with the measured wake lateness.
fn scenario(seed: u64, stalls: &[(usize, u64)], holes: &[(usize, u64)]) -> (Pacing, Sim) {
    let mut rng = rng(seed);
    let arrivals = block_arrivals(BLOCKS_10_MIN, &mut rng, stalls, holes);
    let mut p = Pacing::new(64, PS, RATE);
    let sim = run_paced(&mut p, &arrivals, &mut rng, WAKE_LATE_MAX_NS);
    (p, sim)
}

fn event_ns() -> u64 {
    BASE_NS + samples_to_ns(EVENT_BLOCK as u64 * BLOCK, RATE)
}

fn ms_of(samples: u64) -> f64 {
    samples as f64 * 1000.0 / f64::from(RATE)
}

#[test]
fn logged_pattern_at_64ms_never_loses_audio_with_the_measured_wake_lateness() {
    let dur = samples_to_ns(P, RATE);
    for seed in 1..=8u64 {
        let (p, sim) = scenario(seed, &[(EVENT_BLOCK, LOGGED_STALL_NS)], &[]);
        let what = format!("seed {seed}");
        assert_accounted(&p, &sim, &what);
        assert_eq!(
            (
                p.late_sends,
                p.discontinuities,
                p.silence_samples,
                p.discarded_samples
            ),
            (0, 0, 0, 0),
            "{what}: the logged pattern lost or delayed audio at 64 ms"
        );
        assert_eq!(p.resyncs, 0, "{what}");
        assert!(
            sim.left < ms_to_samples(200, RATE),
            "{what}: {} samples never sent",
            sim.left
        );
        // The wire can only be as late as the thread wakes; a late wake sends every due packet.
        assert!(
            sim.max_gap() <= dur + WAKE_LATE_MAX_NS + MS,
            "{what}: a {} ns gap between packets",
            sim.max_gap()
        );
        assert!(p.late_max_ns <= WAKE_LATE_MAX_NS, "{what}");
        assert!(
            sim.max_send_per_wake >= 2,
            "{what}: the bench never woke late enough to send twice"
        );
    }
}

#[test]
fn the_031_loop_is_bursty_on_the_same_pattern() {
    let mut rng = rng(1);
    let arrivals = block_arrivals(
        BLOCKS_10_MIN,
        &mut rng,
        &[(EVENT_BLOCK, LOGGED_STALL_NS)],
        &[],
    );
    let sim = run_upstream_031(&arrivals);
    assert_eq!(sim.produced, sim.sent_samples + sim.left);
    let spread = send_spread(&sim);
    assert!(
        spread > 20 * MS,
        "the 0.3.1 model should wander by tens of ms, got {spread} ns — the bench no longer bites"
    );
    assert!(
        sim.max_gap() > 20 * MS,
        "the 0.3.1 model should leave gaps, got {} ns",
        sim.max_gap()
    );
}

#[test]
fn every_mixer_stall_up_to_target_plus_grace_loses_nothing_1381() {
    // 64 + 100 = 164 ms. The 70 ms stall is the worst of the quiet regime, 150 ms the list's.
    let limit_ms = 64 + GRACE_MS;
    for stall_ms in [55, 70, 100, 150, limit_ms] {
        for seed in 1..=4u64 {
            let (p, sim) = scenario(seed, &[(EVENT_BLOCK, stall_ms * MS)], &[]);
            let what = format!("{stall_ms} ms stall, seed {seed}");
            assert_accounted(&p, &sim, &what);
            assert_eq!(
                (
                    p.discontinuities,
                    p.silence_samples,
                    p.discarded_samples,
                    p.resyncs
                ),
                (0, 0, 0, 0),
                "{what}: audio was replaced or thrown away"
            );
            if stall_ms >= 150 {
                assert!(p.late_sends > 0, "{what}: the late-audio path never ran");
            }
            // The fixed timeline: the latency after the stall is the latency before it.
            let t = event_ns();
            let before = sim.median_latency(t - 5_000 * MS, t);
            let end = sim.sends.last().unwrap().0;
            let after = sim.median_latency(end - 5_000 * MS, end + 1);
            assert!(
                before.abs_diff(after) <= 5 * MS,
                "{what}: latency before {before} ns, after {after} ns"
            );
        }
    }
}

#[test]
fn every_buffering_hole_up_to_target_plus_grace_loses_nothing_1381() {
    // 2..=7 ticks = 42.7..149.3 ms, the 27.9 holes of 42, 85, 106 and 128 ms among them.
    for ticks in 2..=7u64 {
        for seed in 1..=4u64 {
            let (p, sim) = scenario(seed, &[], &[(EVENT_BLOCK, ticks)]);
            let what = format!("{ticks}-tick hole, seed {seed}");
            assert_accounted(&p, &sim, &what);
            assert_eq!(
                (
                    p.discontinuities,
                    p.silence_samples,
                    p.discarded_samples,
                    p.resyncs
                ),
                (0, 0, 0, 0),
                "{what}: audio was replaced or thrown away"
            );
            // A hole brings no backlog and t0 never moves: once the hole is longer than the
            // target the sender stays late by the rest, and every later packet says so.
            if ticks >= 4 {
                assert!(p.late_sends > 10_000, "{what}: {} late sends", p.late_sends);
            } else if ticks == 2 {
                assert_eq!(p.late_sends, 0, "{what}");
            }
        }
    }
}

#[test]
fn a_stall_beyond_target_plus_grace_is_one_silence_episode_and_one_counted_repay_1381() {
    // The shape of a stall past the grace on the logged (1.0-1.5x catch-up) pattern: a silence
    // episode, then the stalled audio plays late by the silence length while the backlog comes in,
    // then ONE drop of exactly the silence brings the latency back. Two audible splices (the
    // pause, the forward skip), each counted: `discontinuities` and `repays` (review round 1).
    for stall_ms in [250u64, 290, 600] {
        for seed in 1..=4u64 {
            let (p, sim) = scenario(seed, &[(EVENT_BLOCK, stall_ms * MS)], &[]);
            let what = format!("{stall_ms} ms stall, seed {seed}");
            assert_accounted(&p, &sim, &what);
            assert_eq!(
                (p.discontinuities, p.repays, p.resyncs),
                (1, 1, 0),
                "{what}"
            );
            assert!(p.silence_samples > 0, "{what}: no silence past the grace");
            assert_eq!(p.discarded_samples, p.silence_samples, "{what}");
            assert_eq!(p.stale_samples, 0, "{what}");
            // The silence never exceeds the stall itself.
            assert!(
                ms_of(p.silence_samples) < stall_ms as f64,
                "{what}: {:.1} ms of silence",
                ms_of(p.silence_samples)
            );
            // How long the stalled audio plays late before the skip: while the backlog arrives
            // (the audio thread catches up at its own 1.0-1.5x pace).
            let late_play = sim.dropped_at - sim.resumed_at;
            assert!(
                sim.dropped_at > sim.resumed_at && late_play < 5_000 * MS,
                "{what}: the repay came {late_play} ns after the resume"
            );
            let t = event_ns();
            let before = sim.median_latency(t - 5_000 * MS, t);
            let end = sim.sends.last().unwrap().0;
            let after = sim.median_latency(end - 5_000 * MS, end + 1);
            assert!(
                before.abs_diff(after) <= 5 * MS,
                "{what}: latency before {before} ns, after {after} ns"
            );
        }
    }
}

#[test]
fn known_limit_after_an_in_grace_hole_the_grace_left_for_a_stall_is_smaller_1381() {
    // t0 never moves and a buffering hole brings no backlog, so after a 128 ms hole the sender
    // runs about 50 ms late for good and a later stall has only the rest of the grace. A 100 ms
    // stall that a fresh schedule absorbs (0 silence) then cuts a silence episode. Pinned so the
    // follow-up that restores the margin has a RED to flip.
    for seed in 1..=4u64 {
        let (fresh, _) = scenario(seed, &[(EVENT_BLOCK + 2_000, 100 * MS)], &[]);
        assert_eq!(fresh.silence_samples, 0, "seed {seed}: fresh schedule");
        let (p, sim) = scenario(
            seed,
            &[(EVENT_BLOCK + 2_000, 100 * MS)],
            &[(EVENT_BLOCK, 6)],
        );
        let what = format!("128 ms hole then a 100 ms stall, seed {seed}");
        assert_accounted(&p, &sim, &what);
        assert_eq!(p.discontinuities, 1, "{what}");
        assert!(
            p.silence_samples > 0,
            "{what}: the stall was absorbed after all"
        );
        assert_eq!(
            p.discarded_samples, 0,
            "{what}: the hole's debt is never repaid"
        );
    }
}

#[test]
fn a_buffering_hole_beyond_target_plus_grace_is_one_silence_that_discards_nothing_1381() {
    // The 490 ms hole of 27.9 (23 ticks).
    for seed in 1..=4u64 {
        let (p, sim) = scenario(seed, &[], &[(EVENT_BLOCK, 23)]);
        let what = format!("23-tick hole, seed {seed}");
        assert_accounted(&p, &sim, &what);
        assert_eq!(p.discontinuities, 1, "{what}");
        assert!(p.silence_samples > 0, "{what}");
        assert!(
            ms_of(p.silence_samples) <= 23.0 * 1024.0 * 1000.0 / 48_000.0,
            "{what}: more silence than the hole"
        );
        assert_eq!(
            (p.discarded_samples, p.repays, p.resyncs),
            (0, 0, 0),
            "{what}: a hole has no backlog, nothing is stale"
        );
        // The silence rebuilt the target depth: nothing leaves late afterwards.
        assert_eq!(p.late_sends, 0, "{what}");
    }
}

#[test]
fn the_27_9_hole_list_never_discards_audio_1381() {
    let total_hole_ms: f64 = HOLES_27_9
        .iter()
        .map(|(_, t)| *t as f64 * 1024.0 / 48.0)
        .sum();
    for seed in 1..=4u64 {
        let (p, sim) = scenario(seed, &[], &HOLES_27_9);
        let what = format!("27.9 holes, seed {seed}");
        assert_accounted(&p, &sim, &what);
        assert_eq!(
            (p.discarded_samples, p.repays, p.resyncs),
            (0, 0, 0),
            "{what}"
        );
        assert!(
            p.discontinuities <= HOLES_27_9.len() as u64,
            "{what}: {} discontinuities for {} holes",
            p.discontinuities,
            HOLES_27_9.len()
        );
        assert!(
            ms_of(p.silence_samples) < total_hole_ms,
            "{what}: {:.1} ms of silence for {total_hole_ms:.1} ms of holes",
            ms_of(p.silence_samples)
        );
    }
}

#[test]
fn a_frozen_send_thread_catches_up_without_dropping_and_a_long_freeze_resyncs_once() {
    for (freeze_ms, resyncs) in [(400u64, 0u64), (2_500, 1)] {
        let mut rng = rng(11);
        let arrivals = block_arrivals(2_000, &mut rng, &[], &[]);
        let mut p = Pacing::new(64, PS, RATE);
        let (mut buffered, mut next, mut now, mut wake) = (0u64, 0usize, BASE_NS, 0u64);
        let feed = |now: u64, buffered: &mut u64, next: &mut usize| {
            while *next < arrivals.len() && arrivals[*next] <= now {
                *buffered += BLOCK;
                *next += 1;
            }
        };
        // Run normally for 2 s.
        while now < BASE_NS + 2_000 * MS {
            now = if wake == 0 { now + MS } else { wake };
            feed(now, &mut buffered, &mut next);
            let s = p.step(now, buffered);
            buffered -= s.drop_samples + u64::from(s.send) * P;
            wake = s.wake_ns;
        }
        assert_eq!((p.late_sends, p.discontinuities), (0, 0));
        now += freeze_ms * MS;
        feed(now, &mut buffered, &mut next);
        let s = p.step(now, buffered);
        assert_eq!(p.resyncs, resyncs, "{freeze_ms} ms freeze");
        assert_eq!(p.discontinuities, resyncs, "{freeze_ms} ms freeze");
        assert_eq!(p.late_sends, 0, "the audio was there: not a late send");
        if resyncs == 0 {
            assert_eq!(
                s.drop_samples, 0,
                "{freeze_ms} ms: nothing above a ceiling to drop"
            );
            assert!(
                s.send >= 70,
                "every due packet whose audio is buffered goes out in this wake, sent {}",
                s.send
            );
        } else {
            assert!(s.drop_samples > 0 && s.send <= 1, "{freeze_ms} ms: {s:?}");
            assert_eq!(p.discarded_samples, s.drop_samples);
        }
    }
}

/// Producer events of one scenario: `(block, value)` pairs, as `block_arrivals` takes them.
type Events = Vec<(usize, u64)>;

#[test]
fn the_1381_scenario_table() {
    // `cargo test the_1381_scenario_table -- --nocapture` prints what the lane reports.
    let rows: Vec<(&str, Events, Events)> = vec![
        (
            "logged 55.3 ms stall",
            vec![(EVENT_BLOCK, LOGGED_STALL_NS)],
            vec![],
        ),
        ("stall 70 ms", vec![(EVENT_BLOCK, 70 * MS)], vec![]),
        ("stall 100 ms", vec![(EVENT_BLOCK, 100 * MS)], vec![]),
        ("stall 150 ms", vec![(EVENT_BLOCK, 150 * MS)], vec![]),
        ("stall 290 ms", vec![(EVENT_BLOCK, 290 * MS)], vec![]),
        ("hole 42 ms", vec![], vec![(EVENT_BLOCK, 2)]),
        ("hole 85 ms", vec![], vec![(EVENT_BLOCK, 4)]),
        ("hole 106 ms", vec![], vec![(EVENT_BLOCK, 5)]),
        ("hole 128 ms", vec![], vec![(EVENT_BLOCK, 6)]),
        ("hole 490 ms", vec![], vec![(EVENT_BLOCK, 23)]),
        ("27.9 hole list", vec![], HOLES_27_9.to_vec()),
        (
            "hole 128 ms, then stall 100 ms",
            vec![(EVENT_BLOCK + 2_000, 100 * MS)],
            vec![(EVENT_BLOCK, 6)],
        ),
    ];
    println!(
        "scenario | late_sends | discontinuities | repays | silence_ms | discarded_ms | max_wire_gap_ms"
    );
    for (name, stalls, holes) in rows {
        let (p, sim) = scenario(1, &stalls, &holes);
        assert_accounted(&p, &sim, name);
        assert!(p.discarded_samples == 0 || p.discarded_samples == p.silence_samples);
        println!(
            "{name} | {} | {} | {} | {:.1} | {:.1} | {:.1}",
            p.late_sends,
            p.discontinuities,
            p.repays,
            ms_of(p.silence_samples),
            ms_of(p.discarded_samples),
            sim.max_gap() as f64 / 1e6
        );
    }
}
