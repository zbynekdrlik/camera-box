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
//!
//! The latency is graded over the WHOLE run (ROZHODNUTÉ 6343922049): the anchored value is the
//! latency of the first packet past the first second of the sender's audio (the anchor is the
//! earliest arrival of that second), and every later packet must stay within ±1 ms of it.
//!
//! What it grades is the STAMP latency (the stamp against the true production instant). libobs
//! realizes stamps through two more steps the bench does not model (review round 3): a stamp that
//! steps by less than 70 ms is APPENDED to the previous packet (`TS_SMOOTHING_THRESHOLD`), and the
//! genlock ASRC on the stamp basis then pulls the placement onto the raw stamps at most
//! `ASRC_MAX_PPM` (300 ppm). So a first-second refinement of X ms, or a late shift under 70 ms, is
//! realized in OBS over about X / 300 ppm (25 ms: ~83 s); the rig reads it as the `asrc:` placement
//! error of the input right after an anchor.
//!
//! Known limit: before the fit (10 s) the line runs at the nominal rate and then slews to the fit at
//! 5 ppm per bucket, so the stamp latency moves about the sender's ppm x 20 s: ±1 ms holds to about
//! ±55 ppm (lv1 measured ~1.4 ppm), ~2 ms at ±100 ppm (pinned below).

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
    schedule_from(seed, ppm, n, loss, 0.0)
}

/// [`schedule`], with the receiver's first packet arriving at the END of a network gap of
/// `first_gap_ms`: the packets ready in it leave together, in the burst that ends it (the round-2
/// reviewer's case, a first packet ~25 ms later than the stream's least-delayed packets).
fn schedule_from(seed: u64, ppm: f64, n: u64, loss: f64, first_gap_ms: f64) -> Vec<Sent> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let pkt_ns = F as f64 * 1e9 / (48_000.0 * (1.0 + ppm * 1e-6));
    let mut out = Vec::with_capacity(n as usize);
    let mut burst = S0 as f64 + pkt_ns + first_gap_ms * MS;
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

/// libobs as the source's output meets it (review round 3, ROZHODNUTÉ 6345255813 item 1): a
/// chunk stamped within 70 ms of where the source's previous chunk ended is APPENDED there
/// (`TS_SMOOTHING_THRESHOLD`); further off it is PLACED at its stamp. The genlock ASRC on the stamp
/// basis then pulls the placement error (where the audio sits − its raw stamp) out at most 300 ppm
/// (`ASRC_MAX_PPM`) of the audio it plays.
#[derive(Debug, Default)]
struct Obs {
    next: Option<f64>,
    err: f64,
}

impl Obs {
    /// One chunk of `dur` ns stamped `stamp`: the instant libobs plays its first sample.
    fn chunk(&mut self, stamp: f64, dur: f64) -> f64 {
        self.err = match self.next {
            Some(n) if (stamp - n).abs() < 70.0 * MS => n - stamp,
            _ => 0.0,
        };
        let at = stamp + self.err;
        let pull = self.err.clamp(-300e-6 * dur, 300e-6 * dur);
        self.next = Some(at + dur - pull);
        at
    }
}

/// One anchor's stretch of the run: from an anchor (the first packet or a full re-anchor) to the
/// next one.
#[derive(Debug, Clone)]
struct Segment {
    /// The packet index of the anchor.
    anchor_k: u64,
    /// The STAMP latency (stamp − the true instant its first sample was produced) of the first
    /// emitted packet past the anchor's first second, ns; NaN until there is one.
    anchored_latency_ns: f64,
    /// The largest |stamp latency − anchored| after it, ns.
    latency_dev_max_ns: f64,
    /// The same for the latency libobs REALIZES (where it plays the sample, [`Obs`]), ns.
    anchored_realized_ns: f64,
    realized_dev_max_ns: f64,
}

#[derive(Debug, Default)]
struct Run {
    sent: u64,
    dropped: u64,
    late: u64,
    re_anchors: u64,
    lost: u64,
    silence_samples: u64,
    /// The smallest stamp − arrival of an emitted packet over the run, ns.
    min_margin_ns: i64,
    /// The largest arrival − previous arrival the run contained, ns.
    max_gap_ns: u64,
    /// The first anchor's stretch: the latency it anchored and how far it moved, ns.
    anchored_latency_ns: f64,
    latency_dev_max_ns: f64,
    anchored_realized_ns: f64,
    realized_dev_max_ns: f64,
    /// Every anchor's stretch.
    segments: Vec<Segment>,
    /// Packets emitted before their anchor's first second was over (the hold forbids any).
    emitted_in_first_second: u64,
    /// The largest deviation of an emitted stamp step from its nominal span, ppm.
    max_step_dev_ppm: f64,
    sender_ppm: f64,
}

fn replay(sched: &[Sent], reserve_ms: u64) -> Run {
    let mut t = Timeline::new();
    let reserve = reserve_ms * NS_PER_MS;
    let mut run = Run {
        min_margin_ns: i64::MAX,
        ..Run::default()
    };
    let mut obs = Obs::default();
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
        if d.event == Event::Anchor || d.event == Event::Reanchor {
            run.segments.push(Segment {
                anchor_k: s.k,
                anchored_latency_ns: f64::NAN,
                latency_dev_max_ns: 0.0,
                anchored_realized_ns: f64::NAN,
                realized_dev_max_ns: 0.0,
            });
            prev_stamp = None;
        }
        // a LATE packet: dropped, its slot (and the lost ones before it) silence on the line
        if d.event == Event::Late {
            if d.silence_samples > 0 {
                obs.chunk(
                    t.stamp_at(d.silence_pos) as f64,
                    samples_to_ns(d.silence_samples, 48_000) as f64,
                );
            }
            continue;
        }
        // otherwise the source emits only a PACKET (the silence of the slots lost before it first)
        if d.event != Event::Packet {
            continue;
        }
        run.min_margin_ns = run.min_margin_ns.min(-d.lateness_ns);
        if d.silence_samples > 0 {
            obs.chunk(
                t.stamp_at(d.silence_pos) as f64,
                samples_to_ns(d.silence_samples, 48_000) as f64,
            );
        }
        let at = obs.chunk(
            d.stamp_ns as f64,
            samples_to_ns(u64::from(F), 48_000) as f64,
        );
        let seg = run
            .segments
            .last_mut()
            .expect("an anchor before any packet");
        let first_second = (s.k - seg.anchor_k + 1) * u64::from(F) <= 48_000;
        if first_second {
            run.emitted_in_first_second += 1;
            prev_stamp = Some((d.stamp_ns, s.k));
            continue;
        }
        if let Some((ps, pk)) = prev_stamp {
            let nominal = samples_to_ns((s.k - pk) * u64::from(F), 48_000) as f64;
            let dev = (d.stamp_ns as f64 - ps as f64 - nominal).abs() / nominal * 1e6;
            run.max_step_dev_ppm = run.max_step_dev_ppm.max(dev);
        }
        prev_stamp = Some((d.stamp_ns, s.k));
        let lat = d.stamp_ns as f64 - s.first_sample_ns;
        let real = at - s.first_sample_ns;
        if seg.anchored_latency_ns.is_nan() {
            seg.anchored_latency_ns = lat;
            seg.anchored_realized_ns = real;
        } else {
            seg.latency_dev_max_ns = seg
                .latency_dev_max_ns
                .max((lat - seg.anchored_latency_ns).abs());
            seg.realized_dev_max_ns = seg
                .realized_dev_max_ns
                .max((real - seg.anchored_realized_ns).abs());
        }
    }
    if let Some(seg) = run.segments.first() {
        run.anchored_latency_ns = seg.anchored_latency_ns;
        run.latency_dev_max_ns = seg.latency_dev_max_ns;
        run.anchored_realized_ns = seg.anchored_realized_ns;
        run.realized_dev_max_ns = seg.realized_dev_max_ns;
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
            // the timeline follows the sender's clock: the fit reads its offset, the latency holds
            // its anchored value over the whole run, and no stamp step ever jumps
            // the fit's own noise on this burst pattern is about 1 ppm (one sigma)
            assert!(
                (run.sender_ppm - ppm).abs() < 3.0,
                "the fit missed the sender's clock: {tag}"
            );
            assert_latency_held(&run, &tag);
            assert!(run.max_step_dev_ppm < 300.0, "a stamp step jumped: {tag}");
        }
    }
}

/// ROZHODNUTÉ 6343922049 + 6345255813: after the first second both the stamp latency and the
/// latency libobs REALIZES stay within ±1 ms of their anchored value for the WHOLE run (the owner's
/// rule: OBS never changes its latency after sync), and nothing is emitted inside the first second
/// (the hold), so libobs never sees a stamp step.
fn assert_latency_held(run: &Run, tag: &str) {
    assert!(
        !run.anchored_latency_ns.is_nan(),
        "the run never got past its first second: {tag}"
    );
    assert_eq!(
        run.emitted_in_first_second, 0,
        "emitted inside an anchor's first second (the hold): {tag}"
    );
    assert!(
        run.latency_dev_max_ns <= 1.0 * MS,
        "the stamp latency moved {:.3} ms from its anchored {:.3} ms after the first second: {tag}",
        run.latency_dev_max_ns / MS,
        run.anchored_latency_ns / MS
    );
    assert!(
        run.realized_dev_max_ns <= 1.0 * MS,
        "the latency OBS realizes moved {:.3} ms from its anchored {:.3} ms after the first second: \
         {tag}",
        run.realized_dev_max_ns / MS,
        run.anchored_realized_ns / MS
    );
}

/// The round-2 reviewer's case (design question 6341158534): the receiver's first packet is the
/// last one of a 25 ms network gap. Anchored on it and walked onto fit + R, the latency fell from
/// 66 to 44 ms over minutes. Anchored on the earliest arrival of the first second, it is set once.
#[test]
fn a_first_packet_at_the_end_of_a_25_ms_gap_never_walks_the_stamp_latency_1439() {
    for ppm in [0.0, 50.0, -50.0] {
        for seed in 1..=3u64 {
            let sched = schedule_from(seed, ppm, TEN_MIN_PACKETS / 2, 0.0, 25.0);
            // the scenario itself: the first packet's network delay against the least-delayed
            // packet of the first second (the delay = arrival − the instant its last sample was
            // produced, one packet after its first)
            let pkt_ns = F as f64 * 1e9 / (48_000.0 * (1.0 + ppm * 1e-6));
            let delay = |s: &Sent| s.arrival_ns.unwrap() as f64 - (s.first_sample_ns + pkt_ns);
            let first_second = &sched[..187];
            let least = first_second.iter().map(delay).fold(f64::MAX, f64::min);
            let first_late_ms = (delay(&sched[0]) - least) / MS;
            let run = replay(&sched, 40);
            let tag = format!(
                "first gap 25 ms (first packet {first_late_ms:.1} ms later than the least-delayed), \
                 ppm {ppm} seed {seed}: {run:?}"
            );
            println!("{tag}");
            assert!(
                first_late_ms > 15.0,
                "the first packet must really come late against the stream: {tag}"
            );
            assert_eq!((run.late, run.re_anchors), (0, 0), "{tag}");
            assert_latency_held(&run, &tag);
        }
    }
}

/// The schedule with a network event two minutes in: from 120 s every arrival that falls within
/// `hold_ms` is held back to its end (a one-off hiccup), or, when `persistent`, every later arrival
/// is `hold_ms` later (a delay step). The receive thread hands packets over in order.
fn with_network_event(mut sched: Vec<Sent>, hold_ms: u64, persistent: bool) -> Vec<Sent> {
    let h0 = S0 + 120_000_000_000;
    let h1 = h0 + hold_ms * NS_PER_MS;
    let mut prev = 0u64;
    for s in sched.iter_mut() {
        if let Some(a) = s.arrival_ns.as_mut() {
            if persistent && *a >= h0 {
                *a += hold_ms * NS_PER_MS;
            } else if !persistent && (h0..h1).contains(a) {
                *a = h1;
            }
            *a = (*a).max(prev + 1_000);
            prev = *a;
        }
    }
    sched
}

/// ROZHODNUTÉ 6345255813 (2): a fixed latency is a fixed jitter buffer. A one-off 90 ms hiccup
/// (everything sent in it arrives together at its end, more than R late) costs late DROPS -- their
/// slots silence -- and ZERO re-anchors; the latency (stamp and realized) holds its anchored value
/// over the whole run.
#[test]
fn a_90_ms_hiccup_is_late_drops_and_zero_re_anchors_1439() {
    for (ppm, seed) in [(0.0, 1u64), (30.0, 2), (-30.0, 3)] {
        let run = replay(
            &with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, false),
            40,
        );
        let tag = format!("hiccup 90 ms, ppm {ppm} seed {seed}: {run:?}");
        println!("{tag}");
        assert!(
            run.late > 0,
            "the hiccup must really drop late packets: {tag}"
        );
        assert_eq!(run.re_anchors, 0, "{tag}");
        assert_eq!(
            run.silence_samples,
            run.late * u64::from(F),
            "a late slot is silence: {tag}"
        );
        assert_latency_held(&run, &tag);
    }
}

/// ROZHODNUTÉ 6345255813 (2 + 3): a PERSISTENT 90 ms delay step two minutes in: one second of late
/// drops, then exactly ONE re-anchor through the hold, and the new stretch holds its own anchored
/// latency -- the old one + the 90 ms the network added -- to ±1 ms (stamp and realized) for the
/// rest of the run.
#[test]
fn a_90_ms_persistent_delay_step_is_one_re_anchor_1439() {
    for (ppm, seed) in [(0.0, 1u64), (30.0, 2), (-30.0, 3)] {
        let run = replay(
            &with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, true),
            40,
        );
        let tag = format!("delay step 90 ms, ppm {ppm} seed {seed}: {run:?}");
        println!("{tag}");
        assert_eq!(run.re_anchors, 1, "one delay step, one re-anchor: {tag}");
        assert_eq!(run.segments.len(), 2, "{tag}");
        assert_latency_held(&run, &tag);
        let (a, b) = (&run.segments[0], &run.segments[1]);
        assert_eq!(run.emitted_in_first_second, 0, "the re-anchor held: {tag}");
        assert!(
            b.latency_dev_max_ns <= 1.0 * MS && b.realized_dev_max_ns <= 1.0 * MS,
            "the new stretch must hold its anchored latency: {tag}"
        );
        let added = (b.anchored_realized_ns - a.anchored_realized_ns) / MS;
        assert!(
            (85.0..=95.0).contains(&added),
            "the new latency is the old one + the network's 90 ms, got +{added:.2} ms: {tag}"
        );
    }
}

/// The documented limit of the held latency: before the fit the line runs at the nominal rate, then
/// slews to the fit at 5 ppm per bucket, so a sender far off our clock moves the stamp latency by
/// about its ppm x 20 s. At ±100 ppm that is more than the ±1 ms bound (the bound holds to ~±55 ppm)
/// and stays under 2.5 ms; no late slot, no re-anchor.
#[test]
fn known_limit_a_100_ppm_sender_moves_the_held_stamp_latency_about_2_ms_1439() {
    for ppm in [100.0, -100.0] {
        let run = replay(&schedule(1, ppm, TEN_MIN_PACKETS / 2, 0.0), 40);
        let tag = format!("ppm {ppm}: {run:?}");
        println!("{tag}");
        assert_eq!((run.late, run.re_anchors), (0, 0), "{tag}");
        assert!(
            run.latency_dev_max_ns > 1.0 * MS && run.latency_dev_max_ns < 2.5 * MS,
            "the documented limit moved: {tag}"
        );
    }
}

/// The bench can fail: the same sender with a 20 ms Receive Buffer, below its 30 ms gaps, re-anchors
/// on late packets.
#[test]
fn the_same_sender_at_a_20_ms_buffer_goes_late_1439() {
    let run = replay(&schedule(1, 50.0, TEN_MIN_PACKETS / 4, 0.0), 20);
    assert!(run.late > 0, "R below the gaps must read late: {run:?}");
    // its late runs are far shorter than a second: drops, never a re-anchor (ROZHODNUTÉ 6345255813)
    assert_eq!(run.re_anchors, 0, "{run:?}");
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
