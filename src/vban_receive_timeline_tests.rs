//! Issue 1439 — the unit tests of `vban_receive_timeline` (the Tier-0 authority of the obs-vban
//! receive timeline), a test-only `#[path]` child like the bench, so the module itself stays the
//! decision (review round 3: the file had grown past its line budget with the tests inline).

use super::*;

const RATE: u32 = 48_000;
const R40: u64 = 40 * NS_PER_MS;
const T0: u64 = 5_000_000_000;

/// The ideal arrival of the packet `k` packets after the first one (256 frames, nominal rate).
fn on_time(k: u64) -> u64 {
    T0 + samples_to_ns(k * 256, RATE)
}

/// The first packet past the first second of an anchor (256 frames at 48 kHz): packet k ends at
/// (k + 1) x 256, so packets 0..=186 end within 48 000 and are HELD (ROZHODNUTÉ 6345255813 item 1).
const H: u64 = 187;

/// The anchor + the held first second, on time: packets 0..H with indexes `nu0 + k`.
fn through_the_hold(t: &mut Timeline, nu0: u32) {
    for k in 0..H {
        t.packet(on_time(k), nu0.wrapping_add(k as u32), 256, RATE, 2, R40);
    }
}

/// ROZHODNUTÉ 6345255813 (1): after every anchor the source emits NOTHING while the packet's end
/// is within the first second (the earliest-arrival window): the anchor and those packets are HELD.
/// The first packet past it is the first emitted one, on the final (earliest-arrival) line, so
/// libobs never sees a stamp step. A loss inside the hold is never filled; neither are the slots
/// lost right before the first emitted packet (libobs has nothing from the source to keep
/// contiguous yet). A full re-anchor holds again for its own first second.
#[test]
fn the_source_emits_nothing_until_the_first_second_after_an_anchor_is_over() {
    let mut t = Timeline::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    // the first packet ends a 20 ms gap, packet 2 is lost
    let d = t.packet(on_time(0) + 20 * NS_PER_MS, 0, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Anchor);
    let mut prev = on_time(0) + 20 * NS_PER_MS;
    for k in (1..H).filter(|&k| k != 2) {
        let at = on_time(k).max(prev + 1_000);
        prev = at;
        let d = t.packet(at, k as u32, 256, RATE, 2, R40);
        assert_eq!(
            d.event,
            Event::Hold,
            "packet {k} ends within the first second: {d:?}"
        );
        assert_eq!(
            d.silence_samples, 0,
            "packet {k}: nothing is emitted in the hold"
        );
    }
    assert_eq!(t.lost, 0, "a slot lost inside the hold is never filled");
    // packet H is lost too: H + 1 is the first emitted packet, on the earliest-arrival line, and the
    // slot lost right before it is not filled (the stream starts there)
    let d = t.packet(on_time(H + 1), (H + 1) as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(
        d.stamp_ns.abs_diff(line(H + 1)) <= 2,
        "{} vs {}",
        d.stamp_ns,
        line(H + 1)
    );
    assert_eq!(d.silence_samples, 0, "{d:?}");
    // from there on the stream is emitted, losses are silence
    let d = t.packet(on_time(H + 3), (H + 3) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 256));
    // a full re-anchor (another channel count) holds again for its own first second
    let d = t.packet(on_time(H + 4), (H + 4) as u32, 256, RATE, 1, R40);
    assert_eq!((d.event, d.reason), (Event::Reanchor, Reason::Format));
    for k in 1..H {
        let d = t.packet(on_time(H + 4 + k), (H + 4 + k) as u32, 256, RATE, 1, R40);
        assert_eq!(d.event, Event::Hold, "re-anchored packet {k}: {d:?}");
    }
    let d = t.packet(on_time(2 * H + 4), (2 * H + 4) as u32, 256, RATE, 1, R40);
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
        let d = t.packet(on_time(k) + 9 * NS_PER_MS, 1 + k as u32, 256, RATE, 2, r10);
        assert_eq!(
            d.event,
            if k < H { Event::Hold } else { Event::Packet },
            "packet {k}: 9 ms late is inside the margin: {d:?}"
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
        assert_eq!(d.silence_samples, 0);
    }
}

#[test]
fn a_loss_is_silence_for_exactly_the_lost_slots() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 10);
    let first = on_time(0) - samples_to_ns(256, RATE) + R40;
    t.packet(on_time(H), 10 + H as u32, 256, RATE, 2, R40);
    let d = t.packet(on_time(H + 3), 13 + H as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet);
    assert_eq!(d.silence_samples, 2 * 256);
    assert_eq!(d.silence_pos, (H + 1) * 256);
    assert_eq!(
        t.stamp_at(d.silence_pos),
        first + samples_to_ns((H + 1) * 256, RATE)
    );
    assert_eq!(d.stamp_ns, first + samples_to_ns((H + 3) * 256, RATE));
    assert_eq!(t.lost, 2);
    assert_eq!(t.silence_samples, 512);
    assert_eq!(t.re_anchors, 0);
}

#[test]
fn a_packet_after_its_stamp_re_anchors_and_one_on_it_does_not() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 1);
    let stamp1 = t.stamp_at(t.next_pos);
    let d = t.packet(stamp1, 1 + H as u32, 256, RATE, 2, R40);
    assert_eq!(
        d.event,
        Event::Packet,
        "arriving exactly at its stamp is on time"
    );
    assert_eq!(d.lateness_ns, 0);
    let stamp2 = t.stamp_at(t.next_pos);
    let d = t.packet(stamp2 + 1, 2 + H as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Reanchor);
    assert_eq!(d.reason, Reason::Late);
    assert_eq!(d.late_ns, 1, "late by exactly the one ns past its stamp");
    assert_eq!((t.late, t.re_anchors), (1, 1));
    assert_eq!(
        d.stamp_ns,
        stamp2 + 1 - samples_to_ns(256, RATE) + R40,
        "the late packet is the new anchor"
    );
}

/// ROZHODNUTÉ 6343922049 (2): one network event = one re-anchor. A packet past its stamp while
/// the fit is valid shifts the line ONCE, to the stamp that packet would anchor (fit + R + its
/// excess over the fit), counted as one re-anchor; the fit is kept, and no second re-anchor
/// follows when the next fit lands, even for a packet more than R late. The shifted latency is
/// then held.
#[test]
fn a_late_packet_with_a_valid_fit_shifts_the_line_once() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    assert!(t.fit_valid);
    // 60 ms late: more than R after its own slot
    let late_at = on_time(n_fit) + 60 * NS_PER_MS;
    let d = t.packet(late_at, n_fit as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Late),
        "{d:?}"
    );
    assert_eq!((t.late, t.re_anchors), (1, 1));
    assert!(
        t.fit_valid,
        "the fit is kept: the line shifts, the timeline does not restart"
    );
    let shifted = late_at - samples_to_ns(256, RATE) + R40;
    assert!(
        d.stamp_ns.abs_diff(shifted) <= 2,
        "the late packet is stamped where it would anchor: {} vs {shifted}",
        d.stamp_ns
    );
    // the stream goes on on time for 30 s: one event, one re-anchor, the shift held
    for k in n_fit + 1..n_fit + 30 * 48_000 / 256 {
        let d = t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet, "packet {k}: {d:?}");
        let want = shifted + samples_to_ns((k - n_fit) * 256, RATE);
        assert!(
            d.stamp_ns.abs_diff(want) <= samples_to_ns(1, RATE),
            "packet {k}: the shifted latency must be held: {} vs {want}",
            d.stamp_ns
        );
    }
    assert_eq!((t.late, t.re_anchors), (1, 1));
}

/// ROZHODNUTÉ 6343922049 (2): the slots lost right before a late packet are silence on the
/// shifted line, contiguous with the late packet (the timeline runs on, so nothing is dropped).
#[test]
fn the_slots_lost_before_a_late_packet_are_silence_on_the_shifted_line() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    // packets n_fit and n_fit + 1 lost, n_fit + 2 arrives 50 ms late
    let k = n_fit + 2;
    let d = t.packet(on_time(k) + 50 * NS_PER_MS, k as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Late),
        "{d:?}"
    );
    assert_eq!(d.silence_samples, 512, "the two lost slots are silence");
    assert_eq!(t.lost, 2);
    assert_eq!(
        t.stamp_at(d.silence_pos) + samples_to_ns(512, RATE),
        d.stamp_ns,
        "the silence ends where the late packet starts"
    );
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
/// timeline and reported a loss that never happened. A packet whose slot has already passed is
/// dropped: its slot stays the silence it already was, the line runs on.
#[test]
fn a_reordered_or_duplicated_packet_is_dropped_never_a_re_anchor() {
    let mut t = Timeline::new();
    // the held first second, then packets 100 + H .. 100 + H + 11 (the index below names their offset)
    through_the_hold(&mut t, 100u32.wrapping_sub(H as u32));
    for k in H..H + 12 {
        t.packet(on_time(k), (100 + k - H) as u32, 256, RATE, 2, R40);
    }
    let on_time = |k: u64| on_time(H + k);
    // 112 overtaken by 113 on the way: one silent slot
    let d = t.packet(on_time(13), 113, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 256));
    // then 112 arrives: its slot has passed
    let d = t.packet(on_time(13) + 100_000, 112, 256, RATE, 2, R40);
    assert_ne!(
        d.event,
        Event::Reanchor,
        "a reordered packet must not restart the timeline"
    );
    assert_eq!(
        (d.stamp_ns, d.silence_samples),
        (0, 0),
        "a dropped packet is not output"
    );
    // a network duplicate of 113
    let d = t.packet(on_time(13) + 200_000, 113, 256, RATE, 2, R40);
    assert_ne!(d.event, Event::Reanchor);
    assert_eq!(d.event, Event::Stale);
    // a packet exactly STALE_PACKETS behind is still a reorder
    let d = t.packet(
        on_time(13) + 300_000,
        113 - STALE_PACKETS,
        256,
        RATE,
        2,
        R40,
    );
    assert_eq!(d.event, Event::Stale);
    assert_eq!(
        t.stale, 3,
        "the reordered 112, the duplicate 113, the one 64 behind"
    );
    assert_eq!(t.re_anchors, 0);
    assert_eq!(t.lost, 1, "only the slot that was really silent counts");
    // the stream continues on the same line, nothing silent
    let d = t.packet(on_time(14), 114, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 0));
    assert_eq!(d.stamp_ns, on_time(14) - samples_to_ns(256, RATE) + R40);
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
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 256));
    assert_eq!(t.re_anchors, 0);
}

#[test]
fn a_jump_over_two_seconds_re_anchors_and_two_seconds_does_not() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    t.packet(on_time(H), H as u32, 256, RATE, 2, R40);
    // 375 lost packets = exactly 2 s of audio: still a loss.
    let d = t.packet(on_time(H + 376), (H + 376) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 375 * 256));
    // 376 lost packets = more than 2 s: a sender restart.
    let d = t.packet(on_time(H + 753), (H + 753) as u32, 256, RATE, 2, R40);
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
    assert_eq!(t.re_anchors, 3);
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

#[test]
fn packets_late_against_the_line_are_not_folded_and_starve_the_fit() {
    let mut t = Timeline::new();
    t.packet(on_time(0), 0, 256, RATE, 2, R40);
    // Every later packet 20 ms late (within R, never 'late', but an outlier): after 3 s of
    // audio with nothing folded the fit cannot hold.
    let mut reason = Reason::None;
    for k in 1..1000u64 {
        let d = t.packet(on_time(k) + 20 * NS_PER_MS, k as u32, 256, RATE, 2, R40);
        if d.event == Event::Reanchor {
            reason = d.reason;
            // the anchor folded end position 256; packet k ends at (k + 1) x 256, so the first
            // end more than 3 s (144 000 samples) past it is packet 563
            assert_eq!(k, 563, "the starvation re-anchor came at packet {k}");
            break;
        }
    }
    assert_eq!(reason, Reason::Fit);
}
