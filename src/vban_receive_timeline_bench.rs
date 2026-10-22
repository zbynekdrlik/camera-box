//! Issue 1439 — bench: a jittered network VBAN sender on its own clock through the receive
//! timeline, the shape of lv1's `stream` into the Linux stream box.
//!
//! A test-only `#[path]` child of `vban_receive_timeline`. Two clocks:
//!
//! * **The sender.** Its sample clock runs `ppm` off ours (positive = fast). Packet `k` holds
//!   samples `k·F .. (k+1)·F` and is ready when its last sample is, at
//!   `S0 + (k+1)·F / (48 kHz·(1 + ppm·1e-6))` of OUR time. lv1 sends in bursts: a ready packet
//!   waits for the next burst instant (a burst every 6 ms ± 1 ms). Every 402 ms ± 40 ms a GAP of
//!   5..=30 ms sends nothing, and the packets that became ready in it leave together at its end
//!   (the hub measured lv1's largest 10-min gap at 29.1 ms on 18.10 and 26.3 ms on 21.10).
//! * **The network + the OBS receive thread.** A fixed path delay of 0.3 ms, up to 0.4 ms of
//!   per-packet jitter, and every 50th packet or so the receive thread wakes up to 2 ms late.
//!   The thread handles packets in order, so an arrival is never before the previous one.
//!
//! What it measures for itself (never from the timeline's own counters alone): each packet's
//! stamp against the true instant its first sample was produced (the latency), the margin
//! (stamp − arrival), the largest inter-arrival gap the run really contained, and the stamp step
//! between consecutive packets (no jump).

use super::*;

const F: u32 = 256;
const S0: u64 = 2_000_000_000;
const MS: f64 = 1_000_000.0;
const TEN_MIN_PACKETS: u64 = 600 * 48_000 / F as u64;

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

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One packet the sender produced: its index, the true instant its first sample was produced, and
/// its arrival at the receiver (`None` = lost on the network).
struct Sent {
    k: u64,
    first_sample_ns: f64,
    arrival_ns: Option<u64>,
}

/// The sender + network schedule of `n` packets.
fn schedule(seed: u64, ppm: f64, n: u64, loss: f64) -> Vec<Sent> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let pkt_ns = F as f64 * 1e9 / (48_000.0 * (1.0 + ppm * 1e-6));
    let mut out = Vec::with_capacity(n as usize);
    let mut burst = S0 as f64;
    let mut next_gap = S0 as f64 + 402.0 * MS * rng.unit();
    let mut last_arrival = 0u64;
    for k in 0..n {
        let ready = S0 as f64 + (k + 1) as f64 * pkt_ns;
        // the next burst at or after the packet is ready: bursts 5..7 ms apart, and every
        // 362..442 ms one GAP, a burst interval of 5..30 ms (the packets ready in it leave together)
        while burst < ready {
            if burst >= next_gap {
                burst += 5.0 * MS + 25.0 * MS * rng.unit();
                next_gap += 362.0 * MS + 80.0 * MS * rng.unit();
            } else {
                burst += 5.0 * MS + 2.0 * MS * rng.unit();
            }
        }
        let mut arrival = burst + 0.3 * MS + 0.4 * MS * rng.unit();
        if rng.next().is_multiple_of(50) {
            arrival += 2.0 * MS * rng.unit();
        }
        let lost = rng.unit() < loss;
        let arrival_ns = if lost {
            None
        } else {
            let a = (arrival as u64).max(last_arrival + 1_000);
            last_arrival = a;
            Some(a)
        };
        out.push(Sent {
            k,
            first_sample_ns: S0 as f64 + k as f64 * pkt_ns,
            arrival_ns,
        });
    }
    out
}

#[derive(Debug, Default)]
struct Run {
    sent: u64,
    dropped: u64,
    late: u64,
    re_anchors: u64,
    lost: u64,
    silence_samples: u64,
    /// The smallest stamp − arrival over the run, ns.
    min_margin_ns: i64,
    /// The largest arrival − previous arrival the run contained, ns.
    max_gap_ns: u64,
    /// stamp − first-sample instant over the last half of the run: min and max, ns.
    latency_min_ns: f64,
    latency_max_ns: f64,
    /// The largest deviation of a stamp step from its nominal span, ppm.
    max_step_dev_ppm: f64,
    sender_ppm: f64,
}

fn replay(sched: &[Sent], reserve_ms: u64) -> Run {
    let mut t = Timeline::new();
    let reserve = reserve_ms * NS_PER_MS;
    let mut run = Run {
        min_margin_ns: i64::MAX,
        latency_min_ns: f64::MAX,
        latency_max_ns: f64::MIN,
        ..Run::default()
    };
    let half = sched.len() as u64 / 2;
    let mut prev_arrival: Option<u64> = None;
    let mut prev_stamp: Option<(u64, u64)> = None;
    for s in sched {
        run.sent += 1;
        let Some(a) = s.arrival_ns else {
            run.dropped += 1;
            continue;
        };
        if let Some(p) = prev_arrival {
            run.max_gap_ns = run.max_gap_ns.max(a - p);
        }
        prev_arrival = Some(a);
        let d = t.packet(a, s.k as u32, F, 48_000, 2, reserve);
        run.min_margin_ns = run.min_margin_ns.min(-d.lateness_ns);
        if d.event == Event::Reanchor {
            prev_stamp = None;
            continue;
        }
        if let Some((ps, pk)) = prev_stamp {
            let nominal = samples_to_ns((s.k - pk) * u64::from(F), 48_000) as f64;
            let dev = ((d.stamp_ns - ps) as f64 - nominal).abs() / nominal * 1e6;
            run.max_step_dev_ppm = run.max_step_dev_ppm.max(dev);
        }
        prev_stamp = Some((d.stamp_ns, s.k));
        if s.k >= half {
            let lat = d.stamp_ns as f64 - s.first_sample_ns;
            run.latency_min_ns = run.latency_min_ns.min(lat);
            run.latency_max_ns = run.latency_max_ns.max(lat);
        }
    }
    run.late = t.late;
    run.re_anchors = t.re_anchors;
    run.lost = t.lost;
    run.silence_samples = t.silence_samples;
    run.sender_ppm = t.sender_ppm();
    run
}

/// The acceptance (design revision 6338127636): a jittered sender at +-50 ppm with gaps up to
/// 30 ms, ten minutes, three seeds each: zero late slots and zero re-anchors at R = 40 ms.
#[test]
fn a_jittered_sender_at_50_ppm_with_30_ms_gaps_has_no_late_slot_at_40_ms_1439() {
    for ppm in [50.0, -50.0, 0.0] {
        for seed in 1..=3u64 {
            let run = replay(&schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 40);
            let tag = format!("ppm {ppm} seed {seed}: {run:?}");
            println!("{tag}");
            assert!(
                run.max_gap_ns > 28_000_000,
                "the run must really carry a gap near 30 ms: {tag}"
            );
            assert_eq!(run.late, 0, "a late slot: {tag}");
            assert_eq!(run.re_anchors, 0, "a re-anchor: {tag}");
            assert_eq!((run.dropped, run.lost), (0, 0), "{tag}");
            assert!(
                run.min_margin_ns > 0,
                "a packet arrived after its stamp: {tag}"
            );
            // the timeline follows the sender's clock: the fit reads its offset, the latency of
            // the second half stays inside one band, and no stamp step ever jumps
            // the fit's own noise on this burst pattern is about 1 ppm (one sigma)
            assert!(
                (run.sender_ppm - ppm).abs() < 3.0,
                "the fit missed the sender's clock: {tag}"
            );
            assert!(
                run.latency_max_ns - run.latency_min_ns < 2.0 * MS,
                "the latency wandered in the second half: {tag}"
            );
            assert!(run.max_step_dev_ppm < 300.0, "a stamp step jumped: {tag}");
        }
    }
}

/// The bench can fail: the same sender with a 20 ms Receive Buffer, below its 30 ms gaps, re-anchors
/// on late packets.
#[test]
fn the_same_sender_at_a_20_ms_buffer_goes_late_1439() {
    let run = replay(&schedule(1, 50.0, TEN_MIN_PACKETS / 4, 0.0), 20);
    assert!(run.late > 0, "R below the gaps must read late: {run:?}");
    assert!(run.re_anchors >= run.late, "{run:?}");
}

/// Network loss: every lost packet is exactly one packet of silence, and a loss is never a
/// re-anchor or a late slot.
#[test]
fn network_loss_is_silence_and_never_a_re_anchor_1439() {
    for seed in 1..=3u64 {
        let run = replay(&schedule(seed, -50.0, TEN_MIN_PACKETS / 2, 0.002), 40);
        let tag = format!("seed {seed}: {run:?}");
        assert!(run.dropped > 50, "the run must lose packets: {tag}");
        // a loss at the very end of the run is never seen (no packet after it), at most one run
        assert!(
            run.lost <= run.dropped && run.lost + 3 >= run.dropped,
            "{tag}"
        );
        assert_eq!(run.silence_samples, run.lost * u64::from(F), "{tag}");
        assert_eq!((run.late, run.re_anchors), (0, 0), "{tag}");
    }
}
