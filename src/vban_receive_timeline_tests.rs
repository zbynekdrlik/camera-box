//! Issue 1439 — the unit tests of `vban_receive_timeline` (the Tier-0 authority of the obs-vban
//! receive timeline), a test-only `#[path]` child like the bench, so the module itself stays the
//! decision (review round 3: the file had grown past its line budget with the tests inline).

use super::worker_model::{Sink, Worker};
use super::*;
use std::collections::VecDeque;

pub(super) const RATE: u32 = 48_000;
pub(super) const R40: u64 = 40 * NS_PER_MS;
pub(super) const T0: u64 = 5_000_000_000;

/// The ideal arrival of the packet `k` packets after the first one (256 frames, nominal rate).
pub(super) fn on_time(k: u64) -> u64 {
    T0 + samples_to_ns(k * 256, RATE)
}

/// The first packet past the first second of an anchor (256 frames at 48 kHz): packet k ends at
/// (k + 1) x 256, so packets 0..=186 end within 48 000 and are HELD (ROZHODNUTÉ 6345255813 item 1).
pub(super) const H: u64 = 187;

/// The anchor + the held first second, on time: packets 0..H with indexes `nu0 + k`.
pub(super) fn through_the_hold(t: &mut Timeline, nu0: u32) {
    for k in 0..H {
        t.packet(on_time(k), nu0.wrapping_add(k as u32), 256, RATE, 2, R40);
    }
}

/// The timeline driven like the shipped source drives it (ROZHODNUTÉ 6347872706): the store of
/// in-time packets, the playout worker of [`super::worker_model`] (it wakes only when it scheduled
/// itself, or idle when a packet is stored), and every emission with the instant it went out. At an
/// arrival that falls exactly on a wake the packet is handled first.
pub(super) struct Rig {
    pub(super) t: Timeline,
    pub(super) store: VecDeque<u64>,
    pub(super) outs: Vec<(u64, DueDecision)>,
    worker: Worker,
    /// Every wake runs this much late (the worker's scheduling latency), 0 = on time.
    pub(super) wake_late_ns: u64,
}

/// The [`Rig`]'s store and emission record as the worker sees them.
struct RigSink<'a> {
    store: &'a mut VecDeque<u64>,
    outs: &'a mut Vec<(u64, DueDecision)>,
    wake_late_ns: u64,
}

impl Sink for RigSink<'_> {
    fn head(&self) -> Option<u64> {
        self.store.front().copied()
    }

    fn emit(&mut self, at: u64, d: DueDecision) {
        if d.kind == Due::Packet {
            assert_eq!(
                self.store.pop_front(),
                Some(d.pos),
                "the slot is the store's head"
            );
        }
        self.outs.push((at, d));
    }

    fn wake_late(&mut self, _at: u64) -> u64 {
        self.wake_late_ns
    }
}

impl Rig {
    pub(super) fn new() -> Self {
        Rig {
            t: Timeline::new(),
            store: VecDeque::new(),
            outs: Vec::new(),
            worker: Worker::default(),
            wake_late_ns: 0,
        }
    }

    /// The worker's wakes before `until` (and at `until` itself when `inclusive`).
    pub(super) fn run_until(&mut self, until: u64, inclusive: bool) {
        let mut sink = RigSink {
            store: &mut self.store,
            outs: &mut self.outs,
            wake_late_ns: self.wake_late_ns,
        };
        self.worker
            .run_until(&mut self.t, &mut sink, until, inclusive);
    }

    /// One 256-frame stereo 48 kHz packet at R = 40 ms.
    pub(super) fn packet(&mut self, now: u64, nu: u32) -> Decision {
        self.packet_as(now, nu, 256, RATE, 2, R40)
    }

    pub(super) fn packet_as(
        &mut self,
        now: u64,
        nu: u32,
        frames: u32,
        rate: u32,
        channels: u32,
        r: u64,
    ) -> Decision {
        self.run_until(now, false);
        let d = self.t.packet(now, nu, frames, rate, channels, r);
        match d.event {
            Event::Anchor | Event::Reanchor => self.store.clear(),
            Event::Packet => {
                self.store.push_back(d.pos);
                let mut sink = RigSink {
                    store: &mut self.store,
                    outs: &mut self.outs,
                    wake_late_ns: self.wake_late_ns,
                };
                self.worker.stored(&mut sink, now);
            }
            _ => {}
        }
        self.run_until(now, true);
        d
    }

    /// The emissions of one kind, with their instants.
    pub(super) fn of(&self, kind: Due) -> Vec<(u64, DueDecision)> {
        self.outs
            .iter()
            .copied()
            .filter(|(_, d)| d.kind == kind)
            .collect()
    }
}

/// The anchor + the held first second through a [`Rig`], on time: packets 0..H, indexes `nu0 + k`.
pub(super) fn rig_through_the_hold(r: &mut Rig, nu0: u32) {
    for k in 0..H {
        r.packet(on_time(k), nu0.wrapping_add(k as u32));
    }
}

/// ROZHODNUTÉ 6345255813 (1): after every anchor the source emits NOTHING while the packet's end
/// is within the first second (the earliest-arrival window): the anchor and those packets are HELD.
/// The first in-time packet past it starts the output on the final (earliest-arrival) line, so
/// libobs never sees a stamp step; a loss inside the hold, or just before that packet, is never
/// filled. From then on (ROZHODNUTÉ 6347872706) the worker hands every slot to libobs by its
/// deadline: the packet at its next wake once stored, or one slot of silence at the deadline. A
/// full re-anchor holds again.
#[test]
fn the_source_emits_nothing_until_the_first_second_after_an_anchor_is_over() {
    let mut r = Rig::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    // the first packet ends a 20 ms gap, packet 2 is lost
    let d = r.packet(on_time(0) + 20 * NS_PER_MS, 0);
    assert_eq!(d.event, Event::Anchor);
    let mut prev = on_time(0) + 20 * NS_PER_MS;
    for k in (1..H).filter(|&k| k != 2) {
        let at = on_time(k).max(prev + 1_000);
        prev = at;
        let d = r.packet(at, k as u32);
        assert_eq!(
            d.event,
            Event::Hold,
            "packet {k} ends within the first second: {d:?}"
        );
    }
    assert!(r.outs.is_empty(), "nothing is emitted in the hold");
    // packet H is lost too: H + 1 starts the output, on the earliest-arrival line, with its lead-in
    let d = r.packet(on_time(H + 1), (H + 1) as u32);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(
        d.stamp_ns.abs_diff(line(H + 1)) <= 2,
        "{} vs {}",
        d.stamp_ns,
        line(H + 1)
    );
    assert_eq!(r.of(Due::LeadIn).len(), 1, "{:?}", r.outs);
    // H + 2 is lost, H + 3 comes: each slot goes out at its deadline, the slot before H + 1 never
    r.packet(on_time(H + 3), (H + 3) as u32);
    r.run_until(r.t.deadline_at((H + 3) * 256), true);
    let slots: Vec<(u64, DueDecision)> = r
        .outs
        .iter()
        .copied()
        .filter(|(_, d)| d.kind != Due::LeadIn)
        .collect();
    assert_eq!(
        slots
            .iter()
            .map(|(_, d)| (d.kind, d.pos, d.frames))
            .collect::<Vec<_>>(),
        vec![
            (Due::Packet, (H + 1) * 256, 256),
            (Due::Silence, (H + 2) * 256, 256),
            (Due::Packet, (H + 3) * 256, 256),
        ],
        "{:?}",
        r.outs
    );
    // review round 5: a stored packet goes to libobs at the worker's next wake (the store woke the
    // idle worker for H + 1), never held to its deadline; the missing slot's silence goes out
    // exactly at its deadline, and H + 3, stored by then, right behind it at the same wake
    let silence_at = r.t.deadline_at((H + 2) * 256);
    assert_eq!(
        slots.iter().map(|(at, d)| (*at, d.pos)).collect::<Vec<_>>(),
        vec![
            (on_time(H + 1), (H + 1) * 256),
            (silence_at, (H + 2) * 256),
            (silence_at, (H + 3) * 256),
        ],
        "{slots:?}"
    );
    for (at, d) in &slots {
        assert_eq!(d.deadline_ns + DEADLINE_MARGIN_MS * NS_PER_MS, d.stamp_ns);
        assert!(*at <= d.deadline_ns, "nothing after its deadline: {d:?}");
    }
    assert_eq!((r.t.lost, r.t.silence_samples), (1, 256));
    // a full re-anchor (another channel count) holds again for its own first second
    let d = r.packet_as(on_time(H + 4), (H + 4) as u32, 256, RATE, 1, R40);
    assert_eq!((d.event, d.reason), (Event::Reanchor, Reason::Format));
    let n = r.outs.len();
    for k in 1..H {
        let d = r.packet_as(on_time(H + 4 + k), (H + 4 + k) as u32, 256, RATE, 1, R40);
        assert_eq!(d.event, Event::Hold, "re-anchored packet {k}: {d:?}");
    }
    assert_eq!(r.outs.len(), n, "nothing goes out in the new hold");
    let d = r.packet_as(on_time(2 * H + 4), (2 * H + 4) as u32, 256, RATE, 1, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
}

#[test]
fn the_reserve_clamps_like_the_source_setting() {
    assert_eq!(clamp_reserve_ms(0), 40);
    assert_eq!(clamp_reserve_ms(-3), 40);
    assert_eq!(clamp_reserve_ms(9), 10);
    assert_eq!(clamp_reserve_ms(10), 10);
    assert_eq!(clamp_reserve_ms(40), 40);
    assert_eq!(clamp_reserve_ms(500), 500);
    assert_eq!(clamp_reserve_ms(501), 500);
}

#[test]
fn the_anchor_is_its_arrival_minus_its_length_plus_r() {
    let mut t = Timeline::new();
    let d = t.packet(on_time(0), 7, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Anchor);
    assert_eq!(d.stamp_ns, on_time(0) - samples_to_ns(256, RATE) + R40);
    assert_eq!(
        d.lateness_ns,
        -(R40 as i64 - samples_to_ns(256, RATE) as i64)
    );
    assert_eq!(t.received, 1);
    assert_eq!(t.re_anchors, 0);
}

/// ROZHODNUTÉ 6343922049 (1): the anchor is the EARLIEST arrival of the first second of the
/// sender's audio (the least-delayed packet, so later packets are never early against it). A
/// first packet that came at the end of a 25 ms gap must not set the line for the whole run, and
/// past the first second an earlier packet no longer moves it.
#[test]
fn the_anchor_is_the_earliest_arrival_of_the_first_second() {
    let mut t = Timeline::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    // the first packet ends a 25 ms gap: the packets held with it arrive right behind it (the
    // receive thread hands them over in order, never before the first), then the stream is on time
    let first = on_time(0) + 25 * NS_PER_MS;
    let d = t.packet(first, 0, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Anchor);
    assert_eq!(
        d.stamp_ns,
        line(0) + 25 * NS_PER_MS,
        "the first packet anchors where it came"
    );
    let mut prev = first;
    // packet k ends at (k + 1) x 256: packets 1..=186 end within the first second (48 000)
    for k in 1..=186u64 {
        let at = on_time(k).max(prev + 1_000);
        prev = at;
        let d = t.packet(at, k as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Hold, "packet {k}: {d:?}");
        if on_time(k) >= at {
            assert!(
                d.stamp_ns.abs_diff(line(k)) <= 2,
                "packet {k}: the line moved to the earliest arrival: {} vs {}",
                d.stamp_ns,
                line(k)
            );
        }
    }
    for k in 187..600u64 {
        let early = if k == 187 || k == 400 {
            3 * NS_PER_MS
        } else {
            0
        };
        let d = t.packet(on_time(k) - early, k as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet, "packet {k}: {d:?}");
        assert!(
            d.stamp_ns.abs_diff(line(k)) <= 2,
            "packet {k}: past the first second the line is fixed: {} vs {}",
            d.stamp_ns,
            line(k)
        );
    }
    assert_eq!(t.re_anchors, 0);
}

/// ROZHODNUTÉ 6343922049 (1): inside the dead band (|stamp − (fit + R)| <= R/4) the stamp step
/// is the fitted RATE only; outside it the phase term moves the step by at most 5 ppm per fitted
/// bucket, toward the side that brings the stamps back into the band.
#[test]
fn the_phase_term_acts_only_outside_the_dead_band() {
    let mut t = Timeline::new();
    for k in 0..(15 * 48_000 / 256) {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    assert!(t.fit_valid);
    assert_eq!(
        (t.fit_ppb, t.step_ppb),
        (0, 0),
        "an ideal sender: rate 0, no phase"
    );
    let at = t.next_pos;
    // the line moved earlier by `ms` = the stamps that much before fit + R (phase +ms)
    let probe = |ms: i64| {
        let mut p = t.clone();
        p.base_stamp_ns = (p.base_stamp_ns as i64 - ms * NS_PER_MS as i64) as u64;
        assert!(p.update(at), "{ms} ms is within the hold");
        p
    };
    for ms in [-9, -5, 5, 9] {
        let p = probe(ms);
        assert_eq!(
            p.step_ppb, p.fit_ppb,
            "a phase of {ms} ms is inside the band: the step is the rate only"
        );
    }
    assert_eq!(
        probe(11).step_ppb,
        PPB_SLEW,
        "11 ms early: the phase term speeds the stamps up, 5 ppm per bucket"
    );
    assert_eq!(
        probe(-11).step_ppb,
        -PPB_SLEW,
        "11 ms late: the phase term slows the stamps down, 5 ppm per bucket"
    );
}

/// ROZHODNUTÉ 6343922049 (1): a steady delay inside the band (every packet after the anchor
/// 6 ms later than it) is arrival jitter, never a sender-clock event: the stamps stay on the
/// anchored line to the sample (one sample = 20.8 us at 48 kHz) for two minutes (the old phase
/// term walked them 6 ms later). What is left is the RATE the fit reads while its window still
/// holds the anchor's own bucket.
#[test]
fn a_steady_delay_inside_the_band_never_moves_the_stamps() {
    let mut t = Timeline::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    for k in 0..(120 * 48_000 / 256) {
        let late = if k == 0 { 0 } else { 6 * NS_PER_MS };
        let d = t.packet(on_time(k) + late, k as u32, 256, RATE, 2, R40);
        let want = match k {
            0 => Event::Anchor,
            k if k < H => Event::Hold,
            _ => Event::Packet,
        };
        assert_eq!(d.event, want, "packet {k}");
        assert!(
            d.stamp_ns.abs_diff(line(k)) <= samples_to_ns(1, RATE),
            "packet {k}: the stamp left the anchored line by {} ns",
            d.stamp_ns as i64 - line(k) as i64
        );
    }
    assert!(t.fit_valid);
    assert!(
        (5 * NS_PER_MS as i64..=7 * NS_PER_MS as i64).contains(&t.phase_ns),
        "the stamps sit ~6 ms before fit + R, inside the band: {}",
        t.phase_ns
    );
}

/// ROZHODNUTÉ 6343922049 (3): the effective Receive Buffer is max(R, one packet + 10 ms). R is
/// measured from the packet's START (its arrival is its END), so R no longer than a packet left no
/// margin at all and re-anchored every packet. The setting keeps its meaning above that floor.
#[test]
fn the_receive_buffer_is_at_least_one_packet_plus_10_ms() {
    let len = samples_to_ns(256, RATE);
    let r10 = 10 * NS_PER_MS;
    let mut t = Timeline::new();
    let d = t.packet(on_time(0), 1, 256, RATE, 2, r10);
    assert_eq!(
        d.stamp_ns,
        on_time(0) + 10 * NS_PER_MS,
        "R = 10 ms with 5.33 ms packets: the effective R is 15.33 ms, a 10 ms margin"
    );
    assert_eq!(d.lateness_ns, -(10 * NS_PER_MS as i64));
    for k in 1..400u64 {
        // the margin a packet has is R - one packet (10 ms) - the deadline margin (2 ms)
        let d = t.packet(on_time(k) + 7 * NS_PER_MS, 1 + k as u32, 256, RATE, 2, r10);
        assert_eq!(
            d.event,
            if k < H { Event::Hold } else { Event::Packet },
            "packet {k}: 7 ms late is inside the margin: {d:?}"
        );
    }
    assert_eq!(t.re_anchors, 0);
    // above the floor the setting is the reserve: R = 40 ms with the same packets
    let mut t = Timeline::new();
    let d = t.packet(on_time(0), 1, 256, RATE, 2, R40);
    assert_eq!(d.stamp_ns, on_time(0) - len + R40);
    // a long packet (2048 frames = 42.7 ms) lifts even R = 40 ms
    let mut t = Timeline::new();
    let d = t.packet(on_time(0), 1, 2048, RATE, 2, R40);
    assert_eq!(d.stamp_ns, on_time(0) + 10 * NS_PER_MS);
}

/// Review round 3: the C twin computes the arrival offset and the bucket sums in unsigned
/// arithmetic that wraps; the Rust twin panicked there in a debug build (CI's profile) on an
/// arrival before the anchor or before its bucket's first packet. The real source never sees one
/// (`os_gettime_ns` is monotonic and the receive thread is serial), but the twins must agree on
/// every input and the Rust must never panic: it wraps exactly like the C.
#[test]
fn an_arrival_out_of_order_wraps_like_the_c_twin_and_never_panics() {
    let mut t = Timeline::new();
    t.packet(on_time(0), 0, 256, RATE, 2, R40);
    let d = t.packet(on_time(0) - NS_PER_MS, 1, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Hold, "{d:?}");
    // the first packet of bucket 1 (packet 187 ends at 48 128), then one that came before it
    for k in 2..=187u64 {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let d = t.packet(on_time(187) - 1_000, 188, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
}

#[test]
fn contiguous_packets_get_contiguous_stamps() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 1);
    let first = on_time(0) - samples_to_ns(256, RATE) + R40;
    for k in H..H + 20 {
        let d = t.packet(on_time(k), 1 + k as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet);
        assert_eq!(d.stamp_ns, first + samples_to_ns(k * 256, RATE));
        assert_eq!(d.pos, k * 256);
    }
}

/// ROZHODNUTÉ 6347872706: a lost packet's slot goes out at its deadline as silence of exactly one
/// slot, stamped on the same line; the packets around it go out as themselves.
#[test]
fn a_loss_is_silence_for_exactly_the_lost_slots() {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 10);
    let first = on_time(0) - samples_to_ns(256, RATE) + R40;
    r.packet(on_time(H), 10 + H as u32);
    let d = r.packet(on_time(H + 3), 13 + H as u32);
    assert_eq!((d.event, d.pos), (Event::Packet, (H + 3) * 256));
    assert_eq!(d.stamp_ns, first + samples_to_ns((H + 3) * 256, RATE));
    r.run_until(r.t.deadline_at((H + 3) * 256), true);
    let silence = r.of(Due::Silence);
    assert_eq!(silence.len(), 2, "{:?}", r.outs);
    for (j, (_, d)) in silence.iter().enumerate() {
        let pos = (H + 1 + j as u64) * 256;
        assert_eq!((d.pos, d.frames), (pos, 256), "{d:?}");
        assert_eq!(d.stamp_ns, first + samples_to_ns(pos, RATE));
    }
    assert_eq!(r.of(Due::Packet).len(), 2);
    assert_eq!((r.t.lost, r.t.silence_samples, r.t.re_anchors), (2, 512, 0));
}

/// An index far behind the last one (more than a reorder could be: 65 packets) is a sender
/// restart: one counted re-anchor.
#[test]
fn an_index_far_behind_re_anchors_as_backward() {
    let mut t = Timeline::new();
    t.packet(on_time(0), 1000, 256, RATE, 2, R40);
    let d = t.packet(on_time(1), 1000 - 65, 256, RATE, 2, R40);
    assert_eq!((d.event, d.reason), (Event::Reanchor, Reason::Backward));
    assert_eq!(t.re_anchors, 1);
    assert_eq!(t.late, 0);
}

/// Review round 1: one reordered or duplicated packet (a UDP network effect) restarted the
/// timeline and reported a loss that never happened. A packet behind the last one is dropped as
/// STALE (never a re-anchor), and its slot goes out at its deadline as the silence it is.
#[test]
fn a_reordered_or_duplicated_packet_is_dropped_never_a_re_anchor() {
    let mut r = Rig::new();
    // the held first second, then packets 100 .. 111 (the index below names their offset)
    rig_through_the_hold(&mut r, 100u32.wrapping_sub(H as u32));
    for k in H..H + 12 {
        r.packet(on_time(k), (100 + k - H) as u32);
    }
    let on_time = |k: u64| on_time(H + k);
    // 112 overtaken by 113 on the way
    let d = r.packet(on_time(13), 113);
    assert_eq!(d.event, Event::Packet);
    // then 112 arrives
    let d = r.packet(on_time(13) + 100_000, 112);
    assert_eq!(
        (d.event, d.stamp_ns, d.pos),
        (Event::Stale, 0, 0),
        "a reordered packet must not restart the timeline, and is not stored"
    );
    // a network duplicate of 113, and one exactly STALE_PACKETS behind: still a reorder
    assert_eq!(r.packet(on_time(13) + 200_000, 113).event, Event::Stale);
    assert_eq!(
        r.packet(on_time(13) + 300_000, 113 - STALE_PACKETS).event,
        Event::Stale
    );
    assert_eq!(
        r.t.stale, 3,
        "the reordered 112, the duplicate 113, the one 64 behind"
    );
    // the stream continues on the same line
    let d = r.packet(on_time(14), 114);
    assert_eq!(d.event, Event::Packet);
    assert_eq!(d.stamp_ns, on_time(14) - samples_to_ns(256, RATE) + R40);
    r.run_until(r.t.deadline_at(d.pos), true);
    assert_eq!(r.t.re_anchors, 0);
    assert_eq!(
        r.of(Due::Silence)
            .iter()
            .map(|(_, d)| d.pos)
            .collect::<Vec<_>>(),
        vec![(H + 12) * 256],
        "only the slot of 112 is silent"
    );
    assert_eq!(r.t.lost, 1);
}

#[test]
fn the_index_wraps_without_a_re_anchor() {
    let mut t = Timeline::new();
    // the held first second ends at index u32::MAX - 2, then the emitted stream wraps
    through_the_hold(&mut t, (u32::MAX - 1).wrapping_sub(H as u32));
    let on_time = |k: u64| on_time(H + k);
    t.packet(on_time(0), u32::MAX - 1, 256, RATE, 2, R40);
    let d = t.packet(on_time(1), u32::MAX, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet);
    let d = t.packet(on_time(2), 0, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet);
    let d = t.packet(on_time(4), 2, 256, RATE, 2, R40);
    assert_eq!((d.event, d.pos), (Event::Packet, (H + 4) * 256), "{d:?}");
    assert_eq!(t.re_anchors, 0);
}

#[test]
fn a_jump_over_two_seconds_re_anchors_and_two_seconds_does_not() {
    // the index jumps while the packets keep arriving (a sender that skips its counter); a 2 s gap
    // in ARRIVALS is a starve instead (no packet in time for a second, review round 4)
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    t.packet(on_time(H), H as u32, 256, RATE, 2, R40);
    // 375 skipped indexes = exactly 2 s of audio: still a loss.
    let d = t.packet(on_time(H + 1), (H + 376) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.pos), (Event::Packet, (H + 376) * 256));
    // 376 skipped indexes = more than 2 s: a sender restart.
    let d = t.packet(on_time(H + 2), (H + 753) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.reason), (Event::Reanchor, Reason::Jump));
}

#[test]
fn another_format_or_reserve_re_anchors() {
    let mut t = Timeline::new();
    t.packet(on_time(0), 1, 256, RATE, 2, R40);
    let d = t.packet(on_time(1), 2, 256, RATE, 1, R40);
    assert_eq!(d.reason, Reason::Format);
    let d = t.packet(on_time(2), 3, 256, 44_100, 1, R40);
    assert_eq!(d.reason, Reason::Format);
    let d = t.packet(on_time(3), 4, 256, 44_100, 1, 2 * R40);
    assert_eq!(d.reason, Reason::Config);
    // a slot is one packet: another packet size is another format (round 5)
    let d = t.packet(on_time(4), 5, 128, 44_100, 1, 2 * R40);
    assert_eq!(d.reason, Reason::Format);
    assert_eq!(t.re_anchors, 4);
}

#[test]
fn scale_ppb_truncates_toward_zero_and_never_overflows() {
    assert_eq!(scale_ppb(1_000_000_000, 50_000), 50_000);
    assert_eq!(scale_ppb(1_000_000_000, -50_000), -50_000);
    assert_eq!(scale_ppb(999, 1_000), 0);
    assert_eq!(scale_ppb(999, -1_000), 0);
    let eight_hours = 8 * 3600 * NS_PER_SEC;
    assert_eq!(scale_ppb(eight_hours, PPB_CLAMP), 14_400_000_000);
    assert_eq!(round_half_away(2.5), 3);
    assert_eq!(round_half_away(-2.5), -3);
    assert_eq!(round_half_away(-2.4), -2);
}

/// Review round 1: a double out of the int64 range (or NaN) cast to int64_t is undefined in C,
/// while Rust `as` saturates, so the twins could disagree on a degenerate fit. Both saturate at
/// +-2^62 (far beyond any sane fit, still clear of overflow when the anchor and R are added) and
/// read NaN as 0.
#[test]
fn rounding_saturates_at_two_to_the_62_and_reads_nan_as_zero() {
    let lim = 1i64 << 62;
    assert_eq!(round_half_away(1e300), lim);
    assert_eq!(round_half_away(-1e300), -lim);
    assert_eq!(round_half_away(1e19), lim);
    assert_eq!(round_half_away(lim as f64), lim);
    assert_eq!(round_half_away(-(lim as f64)), -lim);
    assert_eq!(round_half_away(f64::NAN), 0);
    assert_eq!(round_half_away(4.0e18), 4_000_000_000_000_000_000);
    assert_eq!(round_half_away(-4.0e18), -4_000_000_000_000_000_000);
}

#[test]
fn the_fit_follows_a_slow_sender_and_the_stamps_never_jump() {
    // A sender 100 ppm slow: each 256-frame packet spans 5.333 ms x 1.0001 of our time.
    let mut t = Timeline::new();
    let span = |k: u64| (samples_to_ns(k * 256, RATE) as f64 * 1.0001) as u64;
    let mut prev: Option<(u64, u64)> = None;
    for k in 0..(120 * 48_000 / 256) {
        let d = t.packet(T0 + span(k), k as u32, 256, RATE, 2, R40);
        assert_ne!(d.event, Event::Reanchor, "packet {k}: {d:?}");
        if let Some((ps, pk)) = prev {
            let step = d.stamp_ns as i64 - ps as i64;
            let nominal = samples_to_ns((k - pk) * 256, RATE) as i64;
            assert!(
                (step - nominal).abs() <= nominal / 1000,
                "packet {k}: a stamp step of {step} ns against {nominal} nominal"
            );
        }
        prev = Some((d.stamp_ns, k));
    }
    assert!(t.fit_valid);
    assert!((t.fit_ppb - 100_000).abs() < 200, "fit {}", t.fit_ppb);
    assert!((t.step_ppb - 100_000).abs() < 2_000, "step {}", t.step_ppb);
    // ROZHODNUTÉ 6343922049: the phase left by the anchor (here ~2 ms: 10 s at the nominal
    // rate before the fit, then the 5 ppm/bucket slew to it) is HELD inside the dead band of
    // R / 4, never walked to fit + R
    assert!(t.phase_ns.abs() <= 10_000_000, "phase {}", t.phase_ns);
}

#[test]
fn a_sender_beyond_the_clamp_cannot_hold() {
    // 600 ppm fast: the first valid fit reads beyond +-500 ppm.
    let mut t = Timeline::new();
    let span = |k: u64| (samples_to_ns(k * 256, RATE) as f64 / 1.0006) as u64;
    let mut fit_reanchor = false;
    for k in 0..(20 * 48_000 / 256) {
        let d = t.packet(T0 + span(k), k as u32, 256, RATE, 2, R40);
        if d.reason == Reason::Fit {
            fit_reanchor = true;
            break;
        }
    }
    assert!(
        fit_reanchor,
        "a 600 ppm sender must read as a fit that cannot hold"
    );
}

/// ROZHODNUTÉ 6345255813 (2): packets that stay IN TIME are never a re-anchor, even when none of
/// them folds into the fit (every one 20 ms after the anchored line: within the margin, but more
/// than the 10 ms outlier limit). The 3-s fold-starve re-anchor is gone; the fit just learns nothing
/// from them and the line stays where the anchor put it.
#[test]
fn packets_in_time_but_off_the_line_never_re_anchor() {
    let mut t = Timeline::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    t.packet(on_time(0), 0, 256, RATE, 2, R40);
    for k in 1..(30 * 48_000 / 256u64) {
        let d = t.packet(on_time(k) + 20 * NS_PER_MS, k as u32, 256, RATE, 2, R40);
        assert_eq!(
            d.event,
            if k < H { Event::Hold } else { Event::Packet },
            "packet {k}: {d:?}"
        );
        assert!(d.stamp_ns.abs_diff(line(k)) <= 2, "packet {k}");
    }
    assert_eq!((t.re_anchors, t.late), (0, 0));
}
