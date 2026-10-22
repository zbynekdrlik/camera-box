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
//! stamp against the true production instant) and the latency libobs REALIZES ([`Obs`], at 0 ms OBS
//! buffering: 1024-frame mix windows on our clock, an underrun moves the source's label and keeps
//! its partial remainder, a chunk within 70 ms of the previous one's end is appended behind it, a
//! chunk further off is placed at its stamp, a chunk over 2 s off at its arrival, a source stopped
//! for about three windows is cleared and placed at its stamp, the stamp-basis ASRC pulls the error
//! out at 300 ppm).
//! The replay emits what the source emits: a PACKET (with the silence of the slots lost before it)
//! and a LATE packet's silence; the anchor, a re-anchor and their held first second emit nothing.
//!
//! The source emits on its own DEADLINE (ROZHODNUTÉ 6347872706): the replay runs the playout
//! worker at every slot's deadline between arrivals, so libobs gets every slot ahead of the window
//! that mixes it -- the packet, or one slot of silence -- and the late fill of rounds 4-6 (a step of
//! one or two mix windows, or with no encoder a mix buffering that grows) is gone. Every grade runs
//! with the encoder running and not running.
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

/// libobs at 0 ms audio buffering (the genlock OBS) as the source's output meets it (review rounds
/// 4 + 5, `obs-source.c` `source_output_audio_data`):
/// - every stamp the source sends is DIRECT (within `MAX_TS_VAR` = 2 s of the OBS clock), so
///   `timing_adjust` is 0 and the stamp-domain jump reset never applies;
/// - in stamps, a chunk within 70 ms (`TS_SMOOTHING_THRESHOLD`) of where the previous one ended
///   (`next_audio_ts_min`) is snapped onto that end;
/// - in the system domain (`next_audio_sys_ts_min`) a chunk within 70 ms of where the previous one
///   ended is APPENDED behind the buffered data, never before the label the mixer reached when it
///   arrived; more than 2 s off it RESETS the timing (`reset_audio_timing`): `timing_adjust` becomes
///   its arrival − its stamp, so it is PLACED at the instant it arrived, whatever its stamp; anything
///   between is PLACED at its stamp;
/// - the mixer pulls one `AUDIO_OUTPUT_FRAMES` (1024-frame) window at a time on our clock; a source
///   whose data does not cover the window UNDERRUNS: that window is not mixed from it
///   (`audio_buffer_insufficient`), `discard_audio` moves its label to the window's end and KEEPS
///   the partial remainder, which then plays from the label on, so whatever comes after plays a
///   whole number of windows later;
/// - review round 6, `discard_if_stopped` (`obs-audio.c`): a partial remainder that gets no new
///   data for two more windows after the first one it cannot cover is CLEARED and the label reset
///   (`audio_ts = 0`), so the next chunk is PLACED at its stamp even when it would append (`if
///   (push_back && source->audio_ts)`). A placement behind the mix window is dropped up to the
///   window by the frozen-mix guard while an encoder runs (the production case, issue 1425), so the
///   stream plays from its stamps on;
/// - round 5 (ROZHODNUTÉ 6347872706), with NO encoder running (`no_encoder`; also OBS's 30 s
///   launch settle window): a chunk placed behind the mix window grows the WHOLE mix's audio
///   buffering by the whole windows it needs (`add_audio_buffering`), for good: the mixer runs that
///   much behind our clock and everything after it plays that much later;
/// - not modelled: the clear one window earlier when an earlier episode left `pending_stop` set; no
///   clear at all when the data ended exactly on a window boundary (`discard_if_stopped` returns
///   early on an empty buffer, about 1 in 1024); a buffering above 0 ms at launch (64 or 85 ms,
///   issue 1425), under which a late chunk does not underrun at all;
/// - the stamp-basis ASRC pulls the placement error out at most 300 ppm (`ASRC_MAX_PPM`) of the audio.
#[derive(Debug, Default)]
struct Obs {
    /// `next_audio_ts_min`: where the source's pushed data ends, in stamps.
    next_ts: Option<f64>,
    /// `next_audio_sys_ts_min`: the same in the system domain (`+ timing_adjust`).
    next_sys: Option<f64>,
    /// Where the buffered data ends on the mix clock.
    next_play: f64,
    /// No encoder runs: a chunk behind the mix window grows the mix's buffering.
    no_encoder: bool,
    /// The mix's audio buffering (it only grows), ns.
    buffering_ns: f64,
}

/// libobs' `MAX_TS_VAR`, ns.
const OBS_MAX_TS_VAR_NS: f64 = 2e9;

/// One libobs audio window (1024 frames at 48 kHz), ns.
const OBS_TICK_NS: f64 = 1024.0 * 1e9 / 48_000.0;

impl Obs {
    /// One chunk of `dur` ns stamped `stamp`, handed over at `arrival`: the instant libobs plays
    /// its first sample.
    fn chunk(&mut self, stamp: f64, dur: f64, arrival: f64) -> f64 {
        // the label the mixer left at the last window it processed before the chunk arrived (the
        // mix runs `buffering_ns` behind our clock)
        let label = ((arrival - self.buffering_ns) / OBS_TICK_NS).floor() * OBS_TICK_NS;
        let ts = match self.next_ts {
            Some(n) if (stamp - n).abs() < 70.0 * MS => n,
            _ => stamp,
        };
        // the first window end the buffered data cannot reach, and the clear two windows later
        let window_start = (self.next_play / OBS_TICK_NS).floor() * OBS_TICK_NS;
        let first_short = window_start + OBS_TICK_NS;
        let cleared = arrival > first_short + 2.0 * OBS_TICK_NS;
        // an append after an underrun: the window the data could not fill was never mixed, its
        // partial remainder is KEPT and plays from the label on, and the chunk goes behind it, so
        // the step is a whole number of windows (review round 6 re-review)
        let behind_remainder = label + (self.next_play - window_start);
        let (start, adjust) = match self.next_sys {
            Some(n) if (ts - n).abs() < 70.0 * MS && cleared => (ts, 0.0),
            Some(n) if (ts - n).abs() < 70.0 * MS && self.next_play <= label => {
                (behind_remainder, 0.0)
            }
            Some(n) if (ts - n).abs() < 70.0 * MS => (self.next_play, 0.0),
            Some(n) if (ts - n).abs() > OBS_MAX_TS_VAR_NS => (arrival, arrival - stamp),
            _ => (ts, 0.0),
        };
        // a chunk placed behind the mix window: with no encoder running the whole mix's buffering
        // grows by the windows it needs (with one, the frozen-mix guard drops the part behind it)
        if self.no_encoder && start < label {
            self.buffering_ns += ((label - start) / OBS_TICK_NS).ceil() * OBS_TICK_NS;
        }
        let err = start - stamp;
        let pull = err.clamp(-300e-6 * dur, 300e-6 * dur);
        self.next_ts = Some(ts + dur);
        self.next_sys = Some(ts + dur + adjust);
        self.next_play = start + dur - pull;
        start + self.buffering_ns
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
    /// Lead-ins sent (one before each stretch's first output).
    lead_ins: u64,
    /// Silence chunks of a missing slot that were NOT exactly one slot long (must stay 0).
    odd_silence: u64,
    /// The mix's audio buffering at the end of the run (grown only with no encoder running), ns.
    mix_buffering_ns: f64,
    sender_ppm: f64,
}

fn replay(sched: &[Sent], reserve_ms: u64) -> Run {
    replay_with(sched, reserve_ms, false)
}

/// What the replay keeps of one stored (in-time) packet until its slot goes out.
struct Stored {
    pos: u64,
    k: u64,
    first_sample_ns: f64,
}

/// The source as the replay runs it (ROZHODNUTÉ 6347872706): the receive side decides and stores,
/// the playout worker emits at its deadlines into libobs ([`Obs`]).
struct Replay {
    t: Timeline,
    obs: Obs,
    run: Run,
    store: std::collections::VecDeque<Stored>,
    clock: u64,
    prev_stamp: Option<(u64, u64)>,
}

impl Replay {
    /// The worker at every deadline before `until` (and at it when `inclusive`), each slot at its
    /// own deadline.
    fn worker(&mut self, until: u64, inclusive: bool) {
        loop {
            let head = self.store.front().map(|s| s.pos);
            let d = self.t.due(self.clock, head);
            let at = self.clock as f64;
            match d.kind {
                Due::None => {
                    let due =
                        d.next_ns != 0 && (d.next_ns < until || (inclusive && d.next_ns == until));
                    if !due {
                        break;
                    }
                    self.clock = self.clock.max(d.next_ns);
                }
                Due::LeadIn => {
                    let dur = samples_to_ns(d.frames, 48_000) as f64;
                    self.obs.chunk(d.stamp_ns as f64, dur, at);
                    self.run.lead_ins += 1;
                }
                Due::Silence => {
                    let dur = samples_to_ns(d.frames, 48_000) as f64;
                    self.obs.chunk(d.stamp_ns as f64, dur, at);
                    self.run.odd_silence += u64::from(d.frames != u64::from(F));
                }
                Due::Packet => {
                    let s = self.store.pop_front().expect("the head packet");
                    assert_eq!(s.pos, d.pos, "the slot is the store's head");
                    let dur = samples_to_ns(u64::from(F), 48_000) as f64;
                    let play = self.obs.chunk(d.stamp_ns as f64, dur, at);
                    let anchor_k = *self
                        .run
                        .anchors
                        .last()
                        .expect("an anchor before any packet");
                    if (s.k - anchor_k + 1) * u64::from(F) <= 48_000 {
                        self.run.emitted_in_first_second += 1;
                    }
                    if let Some((ps, pk)) = self.prev_stamp {
                        let nominal = samples_to_ns((s.k - pk) * u64::from(F), 48_000) as f64;
                        let dev = (d.stamp_ns as f64 - ps as f64 - nominal).abs() / nominal * 1e6;
                        self.run.max_step_dev_ppm = self.run.max_step_dev_ppm.max(dev);
                    }
                    self.prev_stamp = Some((d.stamp_ns, s.k));
                    self.run.records.push(Rec {
                        seg: self.run.anchors.len() - 1,
                        k: s.k,
                        lat: d.stamp_ns as f64 - s.first_sample_ns,
                        real: play - s.first_sample_ns,
                    });
                }
            }
        }
        self.clock = self.clock.max(until);
    }
}

/// [`replay`] with the encoder running or not (`no_encoder`: libobs grows the mix's buffering for a
/// chunk behind the window).
fn replay_with(sched: &[Sent], reserve_ms: u64, no_encoder: bool) -> Run {
    let reserve = reserve_ms * NS_PER_MS;
    let mut r = Replay {
        t: Timeline::new(),
        obs: Obs {
            no_encoder,
            ..Obs::default()
        },
        run: Run {
            min_margin_ns: i64::MAX,
            ..Run::default()
        },
        store: std::collections::VecDeque::new(),
        clock: 0,
        prev_stamp: None,
    };
    let mut prev_arrival: Option<u64> = None;
    for s in sched {
        r.run.sent += 1;
        let Some(a) = s.arrival_ns else {
            r.run.dropped += 1;
            continue;
        };
        if let Some(p) = prev_arrival {
            r.run.max_gap_ns = r.run.max_gap_ns.max(a - p);
        }
        prev_arrival = Some(a);
        // the worker first (every deadline before this arrival), then the packet, then a lead-in
        r.worker(a, false);
        let d = r.t.packet(a, s.k as u32, F, 48_000, 2, reserve);
        match d.event {
            Event::Anchor | Event::Reanchor => {
                r.run.anchors.push(s.k);
                r.store.clear();
                r.prev_stamp = None;
            }
            Event::Packet => {
                r.run.min_margin_ns = r.run.min_margin_ns.min(-d.lateness_ns);
                r.store.push_back(Stored {
                    pos: d.pos,
                    k: s.k,
                    first_sample_ns: s.first_sample_ns,
                });
            }
            _ => {}
        }
        r.worker(a, true);
    }
    // the slots still stored go out (the end of the run: nothing past the last packet's slot)
    if let Some(last) = r.store.back().map(|s| s.pos) {
        let end = r.t.deadline_at(last);
        r.worker(end, true);
    }
    let mut run = r.run;
    (
        run.anchored_latency_ns,
        run.latency_dev_max_ns,
        run.anchored_realized_ns,
        run.realized_dev_max_ns,
    ) = held(&run, 0, 0..u64::MAX);
    run.late = r.t.late;
    run.re_anchors = r.t.re_anchors;
    run.lost = r.t.lost;
    run.silence_samples = r.t.silence_samples;
    run.sender_ppm = r.t.sender_ppm();
    run.mix_buffering_ns = r.obs.buffering_ns;
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
                run.min_margin_ns > (DEADLINE_MARGIN_MS * NS_PER_MS) as i64,
                "a packet arrived after its deadline: {tag}"
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

/// ROZHODNUTÉ 6343922049 + 6345255813 + 6347872706: after the first second both the stamp latency
/// and the latency libobs REALIZES stay within ±1 ms of their anchored value for the WHOLE run (the
/// owner's rule: OBS never changes its latency after sync), nothing is emitted inside the first
/// second (the hold), the mix's buffering never grows and a missing slot's silence is one slot.
fn assert_latency_held(run: &Run, tag: &str) {
    assert_eq!(
        run.mix_buffering_ns, 0.0,
        "the mix's audio buffering grew: {tag}"
    );
    assert_eq!(run.odd_silence, 0, "a silent slot was not one slot: {tag}");
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
    assert!(
        (run.anchored_realized_ns - run.anchored_latency_ns).abs() <= 1.0 * MS,
        "OBS must realize the anchored stamp latency {:.3} ms, it realized {:.3} ms: {tag}",
        run.anchored_latency_ns / MS,
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

/// ROZHODNUTÉ 6345255813 (2) + 6347872706: a fixed latency is a fixed jitter buffer. A one-off
/// 90 ms hiccup (everything sent in it arrives together at its end, more than R late) costs late
/// DROPS and ZERO re-anchors: their slots went out at their deadlines as one slot of silence each,
/// so libobs never underran, and the latency OBS REALIZES holds within ±1 ms (rounds 4-6 stepped it
/// two mix windows, 42.5-43.2 ms), with the encoder running and not running.
#[test]
fn a_90_ms_hiccup_is_late_drops_and_zero_re_anchors_1439() {
    for no_encoder in [false, true] {
        for (ppm, seed) in [(0.0, 1u64), (30.0, 2), (-30.0, 3)] {
            let sched = with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, false);
            let run = replay_with(&sched, 40, no_encoder);
            let tag =
                format!("hiccup 90 ms, no encoder {no_encoder}, ppm {ppm} seed {seed}: {run:?}");
            println!("{tag}");
            assert!(
                run.late > 0,
                "the hiccup must really drop late packets: {tag}"
            );
            assert_eq!(run.re_anchors, 0, "{tag}");
            assert_eq!(
                run.silence_samples,
                run.lost * u64::from(F),
                "a silent slot is one slot: {tag}"
            );
            assert_latency_held(&run, &tag);
        }
    }
}

/// ROZHODNUTÉ 6347872706, the acceptance: every loss row of the late-fill table (comment
/// 6347409033) -- 40, 60, 80, 90, 500 and 900 ms, all under the 1 s starve -- holds the latency OBS
/// realizes within ±1 ms after the first second, with the encoder running and not running: the
/// missing slots go out at their deadlines as one slot of silence each, so libobs never underruns,
/// never clears the source and never grows the mix's buffering (rounds 4-6: +1 or +2 mix windows
/// for 40-80 ms, and with no encoder +455 / +861 ms of mix buffering for 500 / 900 ms).
#[test]
fn a_loss_of_40_to_900_ms_holds_the_realized_latency_with_and_without_an_encoder_1439() {
    for no_encoder in [false, true] {
        for (ms, seed) in [(40u64, 1u64), (60, 2), (80, 3), (90, 4), (500, 5), (900, 6)] {
            let base = schedule(seed, 0.0, TEN_MIN_PACKETS / 2, 0.0);
            let run = replay_with(&with_outage(base, ms), 40, no_encoder);
            let tag = format!("loss {ms} ms, no encoder {no_encoder}, seed {seed}: {run:?}");
            println!("{tag}");
            assert_eq!((run.re_anchors, run.late), (0, 0), "{tag}");
            assert_eq!(
                run.lost, run.dropped,
                "every lost slot went out as silence: {tag}"
            );
            assert!(run.lost > 0, "the outage is a loss: {tag}");
            assert_eq!(run.lead_ins, 1, "{tag}");
            assert_latency_held(&run, &tag);
        }
    }
}

/// ROZHODNUTÉ 6345255813 (2 + 3): a PERSISTENT 90 ms delay step two minutes in: late drops, then
/// exactly ONE re-anchor through the hold, and the new stretch holds its own anchored latency -- the
/// old one + the 90 ms the network added -- to ±1 ms (stamp and realized) for the rest of the run.
/// The old stretch is graded up to the step.
#[test]
fn a_90_ms_persistent_delay_step_is_one_re_anchor_1439() {
    for (no_encoder, ppm, seed) in [
        (false, 0.0, 1u64),
        (false, 30.0, 2),
        (false, -30.0, 3),
        (true, 0.0, 1),
        (true, 30.0, 2),
        (true, -30.0, 3),
    ] {
        let sched = with_network_event(schedule(seed, ppm, TEN_MIN_PACKETS, 0.0), 90, true);
        let event_k = k_at(&sched, S0 + 120_000_000_000) - 20;
        let run = replay_with(&sched, 40, no_encoder);
        let tag =
            format!("delay step 90 ms, no encoder {no_encoder}, ppm {ppm} seed {seed}: {run:?}");
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
        let (b_lat, _, _, _) = held(&run, 1, 0..u64::MAX);
        assert!(
            (b_real - b_lat).abs() <= 1.0 * MS,
            "OBS realizes the new stretch's stamp latency: {tag}"
        );
        assert_eq!(run.mix_buffering_ns, 0.0, "the mix's buffering grew: {tag}");
        assert_eq!(run.odd_silence, 0, "{tag}");
    }
}

/// The schedule with a one-off hiccup at `h0`: every arrival in the next `hold_ms` is held back to
/// its end; the receive thread hands packets over in order.
fn hiccup_at(mut sched: Vec<Sent>, h0: u64, hold_ms: u64) -> Vec<Sent> {
    let h1 = h0 + hold_ms * NS_PER_MS;
    let mut prev = 0u64;
    for s in sched.iter_mut() {
        if let Some(a) = s.arrival_ns.as_mut() {
            if (h0..h1).contains(a) {
                *a = h1;
            }
            *a = (*a).max(prev + 1_000);
            prev = *a;
        }
    }
    sched
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
///
/// Review round 5: the gap the starve + the hold leave is over libobs' 2 s `MAX_TS_VAR`, so libobs
/// RESETS its timing and puts the new stretch's first chunk at the instant it arrives -- one R
/// early. The stretch's lead-in (silence from that arrival up to the first output's stamp) makes
/// OBS realize the new anchored latency exactly (±1 ms of the stamp latency).
#[test]
fn an_outage_or_a_pause_of_1_5_s_is_one_re_anchor_and_the_latency_comes_back_1439() {
    for (no_encoder, what, ppm, seed) in [
        (false, "outage", 0.0, 1u64),
        (false, "pause", 30.0, 2),
        (true, "outage", -30.0, 3),
        (true, "pause", 0.0, 4),
    ] {
        let base = schedule(seed, ppm, TEN_MIN_PACKETS, 0.0);
        let event_k = k_at(&base, S0 + 120_000_000_000) - 20;
        let sched = if what == "outage" {
            with_outage(base, 1_500)
        } else {
            with_pause(base, 1_500)
        };
        let run = replay_with(&sched, 40, no_encoder);
        let tag = format!("{what} 1.5 s, no encoder {no_encoder}, ppm {ppm} seed {seed}: {run:?}");
        println!("{tag}");
        assert_eq!(run.re_anchors, 1, "one starve, one re-anchor: {tag}");
        // the worker keeps the line for a second (silence), then stops: the next packet re-anchors
        let second = 48_000 / u64::from(F);
        assert_eq!(run.late, 0, "no drop: {tag}");
        assert!(
            run.lost > second * 9 / 10 && run.lost <= second,
            "about a second of silent slots, then nothing: {tag}"
        );
        assert_eq!(run.mix_buffering_ns, 0.0, "{tag}");
        assert_eq!(run.emitted_in_first_second, 0, "{tag}");
        let (_, al, a_real, ar) = held(&run, 0, 0..event_k);
        let (b_lat, bl, b_real, br) = held(&run, 1, 0..u64::MAX);
        assert!(
            al <= 1.0 * MS && ar <= 1.0 * MS,
            "the old stretch held: {tag}"
        );
        assert!(
            bl <= 1.0 * MS && br <= 1.0 * MS,
            "the new stretch held: {tag}"
        );
        assert!(
            (b_real - b_lat).abs() <= 1.0 * MS,
            "OBS realizes the new stretch's anchored stamp latency {:.3} ms, it realized {:.3} ms: \
             {tag}",
            b_lat / MS,
            b_real / MS
        );
        assert!(
            ((b_real - a_real) / MS).abs() < 5.0,
            "the latency comes back to the anchored one: {tag}"
        );
        assert_eq!(run.lead_ins, 2, "one lead-in per stretch: {tag}");
    }
}

/// Review round 6 (the reviewer's repro): a 90 ms hiccup that ends a re-anchored stretch's hold
/// (after a 1.5 s outage) makes the first packets past the hold late. Their silence must not be the
/// stretch's first output -- its stamps have passed, so it would carry no lead-in, and libobs, which
/// reset its timing after the gap, would start the stretch at that late fill's arrival (it realized
/// 6.4 and 36.4 ms off its stamp latency at 950 and 980 ms). The stretch realizes its own stamp
/// latency ±1 ms and holds it, one lead-in per stretch.
#[test]
fn a_hiccup_at_the_end_of_a_re_anchor_s_hold_still_lands_the_stretch_on_its_stamp_1439() {
    // hiccups that end inside or right at the end of the hold (one starting at 1000 ms falls after the
    // first output: the ordinary late-fill limit, pinned by the hiccup test)
    for (no_encoder, off_ms) in [
        (false, 950u64),
        (false, 965),
        (false, 980),
        (true, 950),
        (true, 980),
    ] {
        let sched = with_outage(schedule(1, 0.0, TEN_MIN_PACKETS / 2, 0.0), 1_500);
        let a0 = sched
            .iter()
            .find(|s| s.first_sample_ns >= (S0 + 121_500_000_000) as f64)
            .and_then(|s| s.arrival_ns)
            .expect("a packet after the outage");
        let run = replay_with(
            &hiccup_at(sched, a0 + off_ms * NS_PER_MS, 90),
            40,
            no_encoder,
        );
        let (b_lat, bl, b_real, br) = held(&run, 1, 0..u64::MAX);
        let tag = format!(
            "hiccup {off_ms} ms after the re-anchor: stamp {:.3} ms (moved {:.3}), realized {:.3} ms \
             (moved {:.3}): {run:?}",
            b_lat / MS,
            bl / MS,
            b_real / MS,
            br / MS
        );
        println!("{tag}");
        assert_eq!(run.re_anchors, 1, "{tag}");
        assert!(run.late > 0, "the hiccup must make packets late: {tag}");
        assert_eq!(run.lead_ins, 2, "one lead-in per stretch: {tag}");
        assert!(
            bl <= 1.0 * MS && br <= 1.0 * MS,
            "the new stretch held: {tag}"
        );
        assert!(
            (b_real - b_lat).abs() <= 1.0 * MS,
            "OBS realizes the stretch's stamp latency: {tag}"
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
