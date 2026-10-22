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
//! The latency is graded over the WHOLE run (ROZHODNUTÉ 6343922049 + 6345255813): per anchor, the
//! anchored value is the latency of the first EMITTED packet (the first one past the held first
//! second), and every later packet must stay within ±1 ms of it -- both the STAMP latency (the
//! stamp against the true production instant) and the latency libobs REALIZES ([`Obs`]: a chunk
//! within 70 ms of the previous one's end is appended, `TS_SMOOTHING_THRESHOLD`, and the stamp-basis
//! ASRC pulls the placement error out at most 300 ppm, `ASRC_MAX_PPM`). The replay emits what the
//! source emits: a PACKET (with the silence of the slots lost before it) and a LATE packet's silence;
//! the anchor, a re-anchor and their held first second emit nothing, so libobs never sees a step.
//!
//! Known limit: before the fit (10 s) the line runs at the nominal rate and then slews to the fit at
//! 5 ppm per bucket, so the latency moves about the sender's ppm x 20 s: ±1 ms holds to about
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

/// libobs at 0 ms audio buffering (the genlock OBS) as the source's output meets it (review round 4):
/// - the mixer pulls one `AUDIO_OUTPUT_FRAMES` (1024-frame) window at a time on our clock; a source
///   whose data does not cover the window UNDERRUNS: `discard_audio` moves its label to the window's
///   end and KEEPS the data, so whatever comes after plays that much later;
/// - a chunk stamped within 70 ms of where the source's previous chunk ended in STAMPS
///   (`next_audio_ts_min`, `TS_SMOOTHING_THRESHOLD`) is APPENDED behind the buffered data, and never
///   before the label the mixer reached when it arrived; further off it is PLACED at its stamp;
/// - the stamp-basis ASRC pulls the placement error out at most 300 ppm (`ASRC_MAX_PPM`) of the audio.
#[derive(Debug, Default)]
struct Obs {
    /// `next_audio_ts_min`: where the source's pushed data ends, in stamps.
    next_ts: Option<f64>,
    /// Where the buffered data ends on the mix clock.
    next_play: f64,
}

/// One libobs audio window (1024 frames at 48 kHz), ns.
const OBS_TICK_NS: f64 = 1024.0 * 1e9 / 48_000.0;

impl Obs {
    /// One chunk of `dur` ns stamped `stamp`, handed over at `arrival`: the instant libobs plays
    /// its first sample.
    fn chunk(&mut self, stamp: f64, dur: f64, arrival: f64) -> f64 {
        // the label the mixer left at the last window it processed before the chunk arrived
        let label = (arrival / OBS_TICK_NS).floor() * OBS_TICK_NS;
        let (ts_used, start) = match self.next_ts {
            Some(n) if (stamp - n).abs() < 70.0 * MS => (n, self.next_play.max(label)),
            _ => (stamp, stamp),
        };
        let err = start - stamp;
        let pull = err.clamp(-300e-6 * dur, 300e-6 * dur);
        self.next_ts = Some(ts_used + dur);
        self.next_play = start + dur - pull;
        start
    }
}

/// One emitted packet: its anchor stretch, its index, its stamp latency and the latency libobs
/// realizes (stamp / play instant − the true instant its first sample was produced), ns.
#[derive(Debug, Clone, Copy)]
struct Rec {
    seg: usize,
    k: u64,
    lat: f64,
    real: f64,
}

/// How far an anchor stretch's latency moved over the emitted packets with `k` in `ks`: (anchored
/// stamp latency, its largest move, anchored realized latency, its largest move), ns; the anchored
/// value is the first emitted packet's (NaN when there is none).
fn held(run: &Run, seg: usize, ks: std::ops::Range<u64>) -> (f64, f64, f64, f64) {
    let mut recs = run
        .records
        .iter()
        .filter(|r| r.seg == seg && ks.contains(&r.k));
    let Some(first) = recs.next() else {
        return (f64::NAN, 0.0, f64::NAN, 0.0);
    };
    let (mut dl, mut dr) = (0.0f64, 0.0f64);
    for r in recs {
        dl = dl.max((r.lat - first.lat).abs());
        dr = dr.max((r.real - first.real).abs());
    }
    (first.lat, dl, first.real, dr)
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
    /// The first anchor's stretch over the whole run ([`held`]), ns.
    anchored_latency_ns: f64,
    latency_dev_max_ns: f64,
    anchored_realized_ns: f64,
    realized_dev_max_ns: f64,
    /// The anchor packet index of every stretch.
    anchors: Vec<u64>,
    /// Every emitted packet (not printed).
    #[allow(dead_code)]
    records: Vec<Rec>,
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
            run.anchors.push(s.k);
            prev_stamp = None;
        }
        // the source emits a PACKET (the silence of the slots lost before it first) and a LATE
        // packet's silence; nothing else
        if d.event != Event::Packet && d.event != Event::Late {
            continue;
        }
        if d.silence_samples > 0 {
            obs.chunk(
                t.stamp_at(d.silence_pos) as f64,
                samples_to_ns(d.silence_samples, 48_000) as f64,
                a as f64,
            );
        }
        if d.event == Event::Late {
            continue;
        }
        run.min_margin_ns = run.min_margin_ns.min(-d.lateness_ns);
        let at = obs.chunk(
            d.stamp_ns as f64,
            samples_to_ns(u64::from(F), 48_000) as f64,
            a as f64,
        );
        let anchor_k = *run.anchors.last().expect("an anchor before any packet");
        if (s.k - anchor_k + 1) * u64::from(F) <= 48_000 {
            run.emitted_in_first_second += 1;
        }
        if let Some((ps, pk)) = prev_stamp {
            let nominal = samples_to_ns((s.k - pk) * u64::from(F), 48_000) as f64;
            let dev = (d.stamp_ns as f64 - ps as f64 - nominal).abs() / nominal * 1e6;
            run.max_step_dev_ppm = run.max_step_dev_ppm.max(dev);
        }
        prev_stamp = Some((d.stamp_ns, s.k));
        run.records.push(Rec {
            seg: run.anchors.len() - 1,
            k: s.k,
            lat: d.stamp_ns as f64 - s.first_sample_ns,
            real: at - s.first_sample_ns,
        });
    }
    (
        run.anchored_latency_ns,
        run.latency_dev_max_ns,
        run.anchored_realized_ns,
        run.realized_dev_max_ns,
    ) = held(&run, 0, 0..u64::MAX);
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
/// 66 to 44 ms over minutes; refined in the first second but EMITTED there, libobs still walked it
/// at 300 ppm (59.9 ms realized, then 19.3 ms of walk). Held for that second (ROZHODNUTÉ 6345255813),
/// the first emitted packet is already on the final line: realized and stamp latency set once.
#[test]
fn a_first_packet_at_the_end_of_a_25_ms_gap_never_walks_the_realized_latency_1439() {
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

/// The packet index of the first packet the sender produced at or after `t_ns` of its own run.
fn k_at(sched: &[Sent], t_ns: u64) -> u64 {
    sched
        .iter()
        .find(|s| s.first_sample_ns >= t_ns as f64)
        .map_or(u64::MAX, |s| s.k)
}

/// ROZHODNUTÉ 6345255813 (2): a fixed latency is a fixed jitter buffer. A one-off 90 ms hiccup
/// (everything sent in it arrives together at its end, more than R late) costs late DROPS -- their
/// slots silence -- and ZERO re-anchors; the stamps never move.
///
/// Known limit (review round 4): at 0 ms OBS buffering the late slots' silence reaches libobs
/// after their windows were mixed, so libobs underruns and appends it behind its label -- the
/// latency OBS REALIZES steps up by about the lateness and the stamp-basis ASRC walks it back at
/// <= 300 ppm. Pinned here so a cure (a deadline timer, or no fill past the deadline) shows; it is
/// a question for the main.
#[test]
fn a_90_ms_hiccup_is_late_drops_and_zero_re_anchors_1439() {
    for (ppm, seed) in [(0.0, 1u64), (30.0, 2), (-30.0, 3)] {
        let sched = with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, false);
        let event_k = k_at(&sched, S0 + 120_000_000_000) - 20;
        let run = replay(&sched, 40);
        let (_, dl, _, before) = held(&run, 0, 0..event_k);
        let (_, _, _, overall) = held(&run, 0, 0..u64::MAX);
        let tag = format!(
            "hiccup 90 ms, ppm {ppm} seed {seed}: realized moved {:.3} ms before it, {:.3} ms over the \
             run (the known limit): {run:?}",
            before / MS,
            overall / MS
        );
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
        assert_eq!(run.emitted_in_first_second, 0, "{tag}");
        assert!(
            run.latency_dev_max_ns <= 1.0 * MS && dl <= 1.0 * MS,
            "the stamps never move: {tag}"
        );
        assert!(
            before <= 1.0 * MS,
            "before the hiccup the realized latency is held: {tag}"
        );
        assert!(
            overall > 30.0 * MS && overall < 90.0 * MS,
            "the known limit moved (a cure would bring it under 1 ms): {tag}"
        );
    }
}

/// ROZHODNUTÉ 6345255813 (2 + 3): a PERSISTENT 90 ms delay step two minutes in: late drops, then
/// exactly ONE re-anchor through the hold, and the new stretch holds its own anchored latency -- the
/// old one + the 90 ms the network added -- to ±1 ms (stamp and realized) for the rest of the run.
/// The old stretch is graded up to the step.
#[test]
fn a_90_ms_persistent_delay_step_is_one_re_anchor_1439() {
    for (ppm, seed) in [(0.0, 1u64), (30.0, 2), (-30.0, 3)] {
        let sched = with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, true);
        let event_k = k_at(&sched, S0 + 120_000_000_000) - 20;
        let run = replay(&sched, 40);
        let tag = format!("delay step 90 ms, ppm {ppm} seed {seed}: {run:?}");
        println!("{tag}");
        assert_eq!(run.re_anchors, 1, "one delay step, one re-anchor: {tag}");
        assert_eq!(run.anchors.len(), 2, "{tag}");
        assert_eq!(run.emitted_in_first_second, 0, "the re-anchor held: {tag}");
        let (_, al, a_real, ar) = held(&run, 0, 0..event_k);
        let (_, bl, b_real, br) = held(&run, 1, 0..u64::MAX);
        assert!(
            al <= 1.0 * MS && ar <= 1.0 * MS,
            "the old stretch held up to the step: {tag}"
        );
        assert!(
            bl <= 1.0 * MS && br <= 1.0 * MS,
            "the new stretch must hold its anchored latency: {tag}"
        );
        let added = (b_real - a_real) / MS;
        assert!(
            (85.0..=95.0).contains(&added),
            "the new latency is the old one + the network's 90 ms, got +{added:.2} ms: {tag}"
        );
    }
}

/// The schedule with a network OUTAGE two minutes in: every packet sent in the next `ms` is lost.
fn with_outage(mut sched: Vec<Sent>, ms: u64) -> Vec<Sent> {
    let h0 = (S0 + 120_000_000_000) as f64;
    let h1 = h0 + (ms * NS_PER_MS) as f64;
    for s in sched.iter_mut() {
        if (h0..h1).contains(&s.first_sample_ns) {
            s.arrival_ns = None;
        }
    }
    sched
}

/// The schedule with a sender PAUSE two minutes in: the sender stops for `ms` and its index runs
/// on from where it stopped, so every later packet is produced -- and arrives -- `ms` later.
fn with_pause(mut sched: Vec<Sent>, ms: u64) -> Vec<Sent> {
    let h0 = (S0 + 120_000_000_000) as f64;
    for s in sched.iter_mut() {
        if s.first_sample_ns >= h0 {
            s.first_sample_ns += (ms * NS_PER_MS) as f64;
            if let Some(a) = s.arrival_ns.as_mut() {
                *a += ms * NS_PER_MS;
            }
        }
    }
    sched
}

/// Review round 4 (the starve of ROZHODNUTÉ 6345255813 item 2): a 1.5 s network OUTAGE and a 1.5 s
/// sender PAUSE are each no packet in time for a second: exactly ONE re-anchor through the hold, no
/// late drop, nothing filled after the fact, and both stretches hold their latency (stamp and
/// realized); after the pause the latency is the old one (the production paused too), after the
/// outage too (the same network).
#[test]
fn an_outage_or_a_pause_of_1_5_s_is_one_re_anchor_and_the_latency_comes_back_1439() {
    for (what, ppm, seed) in [
        ("outage", 0.0, 1u64),
        ("pause", 30.0, 2),
        ("outage", -30.0, 3),
        ("pause", 0.0, 4),
    ] {
        let base = schedule(seed, ppm, TEN_MIN_PACKETS, 0.0);
        let event_k = k_at(&base, S0 + 120_000_000_000) - 20;
        let sched = if what == "outage" {
            with_outage(base, 1_500)
        } else {
            with_pause(base, 1_500)
        };
        let run = replay(&sched, 40);
        let tag = format!("{what} 1.5 s, ppm {ppm} seed {seed}: {run:?}");
        println!("{tag}");
        assert_eq!(run.re_anchors, 1, "one starve, one re-anchor: {tag}");
        assert_eq!(
            (run.late, run.lost),
            (0, 0),
            "no drop, nothing filled: {tag}"
        );
        assert_eq!(run.emitted_in_first_second, 0, "{tag}");
        let (_, al, a_real, ar) = held(&run, 0, 0..event_k);
        let (_, bl, b_real, br) = held(&run, 1, 0..u64::MAX);
        assert!(
            al <= 1.0 * MS && ar <= 1.0 * MS,
            "the old stretch held: {tag}"
        );
        assert!(
            bl <= 1.0 * MS && br <= 1.0 * MS,
            "the new stretch held: {tag}"
        );
        assert!(
            ((b_real - a_real) / MS).abs() < 5.0,
            "the latency comes back to the anchored one: {tag}"
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
