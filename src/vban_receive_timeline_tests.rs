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

/// ROZHODNUTÉ 6345255813 (2): a fixed latency is a fixed jitter buffer. A packet past its stamp
/// is LOST, never re-timed: it is dropped and counted `late`, its slot goes out as silence at once
/// on the unchanged line, and the next packet is stamped on that same line. Arriving exactly at its
/// stamp is in time.
#[test]
fn a_packet_past_its_stamp_is_dropped_and_its_slot_is_silence_on_the_same_line() {
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
    let pos2 = t.next_pos;
    let stamp2 = t.stamp_at(pos2);
    let d = t.packet(stamp2 + 1, 2 + H as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.reason), (Event::Late, Reason::None), "{d:?}");
    assert_eq!(d.late_ns, 1, "late by exactly the one ns past its stamp");
    assert_eq!(
        (d.silence_samples, d.silence_pos),
        (256, pos2),
        "its own slot is silence"
    );
    assert_eq!(t.stamp_at(d.silence_pos), stamp2, "on the unchanged line");
    assert_eq!((t.late, t.re_anchors, t.lost), (1, 0, 0));
    // the next packet: the same line, nothing more to fill
    let stamp3 = stamp2 + samples_to_ns(256, RATE);
    let d = t.packet(stamp3 - NS_PER_MS, 3 + H as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 0), "{d:?}");
    assert!(
        d.stamp_ns.abs_diff(stamp3) <= 1,
        "{} vs {stamp3}",
        d.stamp_ns
    );
}

/// ROZHODNUTÉ 6345255813 (2): the late shift of 6343922049 item 2 is withdrawn (a runtime latency
/// change). A packet even 60 ms late (more than R) once the fit is valid is dropped; the line, the
/// fit and the latency stay where the anchor put them, and nothing re-anchors.
#[test]
fn a_late_packet_never_moves_the_line_even_more_than_r_late() {
    let mut t = Timeline::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    assert!(t.fit_valid);
    let d = t.packet(
        on_time(n_fit) + 60 * NS_PER_MS,
        n_fit as u32,
        256,
        RATE,
        2,
        R40,
    );
    assert_eq!(d.event, Event::Late, "{d:?}");
    assert_eq!(d.silence_samples, 256);
    assert!(t.fit_valid, "the fit is kept");
    for k in n_fit + 1..n_fit + 30 * 48_000 / 256 {
        let d = t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet, "packet {k}: {d:?}");
        assert!(
            d.stamp_ns.abs_diff(line(k)) <= samples_to_ns(1, RATE),
            "packet {k}: the anchored line must be held: {} vs {}",
            d.stamp_ns,
            line(k)
        );
    }
    assert_eq!((t.late, t.re_anchors), (1, 0));
}

/// ROZHODNUTÉ 6345255813 (2): the slots lost right before a late packet are silence together with
/// its own slot, contiguous on the unchanged line; the lost ones count `lost`, it counts `late`.
#[test]
fn the_slots_lost_before_a_late_packet_are_silence_with_its_own() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let pos = t.next_pos;
    // packets n_fit and n_fit + 1 lost, n_fit + 2 arrives 50 ms late
    let k = n_fit + 2;
    let d = t.packet(on_time(k) + 50 * NS_PER_MS, k as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Late, "{d:?}");
    assert_eq!(
        (d.silence_samples, d.silence_pos),
        (3 * 256, pos),
        "two lost slots + its own"
    );
    assert_eq!((t.lost, t.late, t.re_anchors), (2, 1, 0));
    assert_eq!(t.next_pos, pos + 3 * 256, "the line runs on past its slot");
}

/// ROZHODNUTÉ 6345255813 (2): only a PERSISTENT condition re-anchors: no packet in time for one
/// second (a starve). Every packet late on a network delay step is one: the late packets are
/// dropped until one arrives more than a second after the last in-time packet did, and that one is
/// ONE full re-anchor (reason starve, review round 4: measured on our clock, so an outage or a
/// pause is the same event). The re-anchor holds its first second, then emits on the new line: the
/// new network delay + R.
#[test]
fn every_packet_late_for_one_second_is_one_re_anchor_through_the_hold() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let last_in_time = on_time(n_fit - 1);
    // from packet n_fit on, every packet 90 ms late (a delay step on the network)
    let step = 90 * NS_PER_MS;
    let mut k = n_fit;
    loop {
        let at = on_time(k) + step;
        let d = t.packet(at, k as u32, 256, RATE, 2, R40);
        if at - last_in_time <= NS_PER_SEC {
            assert_eq!(d.event, Event::Late, "packet {k}: {d:?}");
        } else {
            assert_eq!(
                (d.event, d.reason),
                (Event::Reanchor, Reason::Starve),
                "{d:?}"
            );
            break;
        }
        k += 1;
    }
    assert_eq!(t.re_anchors, 1);
    assert_eq!(
        t.late,
        k - n_fit,
        "every packet before the re-anchor dropped late"
    );
    // the re-anchor holds its first second, then the new line: arrival + 90 ms - one packet + R
    for j in 1..H {
        let d = t.packet(on_time(k + j) + step, (k + j) as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Hold, "{d:?}");
    }
    for j in H..H + 400 {
        let d = t.packet(on_time(k + j) + step, (k + j) as u32, 256, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet, "{d:?}");
        let want = on_time(k + j) + step - samples_to_ns(256, RATE) + R40;
        assert!(d.stamp_ns.abs_diff(want) <= 2, "{} vs {want}", d.stamp_ns);
    }
    assert_eq!(t.re_anchors, 1, "one delay step, one re-anchor");
}

/// Review round 4 (the starve of ROZHODNUTÉ 6345255813 item 2): a network OUTAGE of 1.5 s -- the
/// packets lost, the sender running on -- is no packet in time for a second. The first packet after
/// it, even arriving on time, is ONE re-anchor (no 1.5 s of silence filled after the fact), then the
/// hold. A 0.9 s outage is a loss: silence for its slots, no re-anchor.
#[test]
fn a_network_outage_of_more_than_a_second_is_one_re_anchor() {
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    for (gap_packets, re_anchor) in [(281u64, true), (168, false)] {
        let mut t = Timeline::new();
        let n_fit = 15 * 48_000 / 256u64;
        for k in 0..n_fit {
            t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        }
        let k = n_fit + gap_packets;
        let d = t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        if re_anchor {
            assert_eq!(
                (d.event, d.reason),
                (Event::Reanchor, Reason::Starve),
                "{d:?}"
            );
            assert_eq!(
                (d.silence_samples, t.lost),
                (0, 0),
                "nothing filled after the fact"
            );
            let d = t.packet(on_time(k + 1), (k + 1) as u32, 256, RATE, 2, R40);
            assert_eq!(d.event, Event::Hold, "the re-anchor holds: {d:?}");
        } else {
            assert_eq!(d.event, Event::Packet, "{d:?}");
            assert_eq!(
                d.silence_samples,
                gap_packets * 256,
                "a 0.9 s outage is a loss"
            );
            assert!(d.stamp_ns.abs_diff(line(k)) <= 2);
        }
        assert_eq!(t.re_anchors, u64::from(re_anchor));
    }
}

/// Review round 4: a sender that PAUSES for 1.5 s (its index runs on from where it stopped, so the
/// packets after it arrive 1.5 s late against the line) is the same starve: the first packet after
/// the pause is ONE re-anchor at once, never a second of late drops first.
#[test]
fn a_sender_pause_of_more_than_a_second_is_one_re_anchor_with_no_drops() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let pause = 1_500 * NS_PER_MS;
    let d = t.packet(on_time(n_fit) + pause, n_fit as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert_eq!(
        (t.late, t.re_anchors),
        (0, 1),
        "no second of late drops before it"
    );
}

/// Review round 5: a STARVE re-anchor carries how long no packet came in time (its arrival − the
/// last in-time arrival) in `starve_ns` (review round 6: its own field, `late_ns` stays the
/// lateness of a LATE packet), so the source logs the starve span (it logged `late_ms=0.0`). Every
/// other re-anchor carries none.
#[test]
fn a_starve_re_anchor_carries_how_long_no_packet_came_in_time() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let d = t.packet(on_time(H), H as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    let at = on_time(H) + 1_500 * NS_PER_MS;
    let d = t.packet(at, (H + 1) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert_eq!(
        (d.starve_ns, d.late_ns),
        ((1_500 * NS_PER_MS) as i64, 0),
        "the starve span: {d:?}"
    );
    // another re-anchor (a channel change) carries no span
    let d = t.packet(at + MS_256, (H + 2) as u32, 256, RATE, 1, R40);
    assert_eq!(
        (d.event, d.reason, d.starve_ns),
        (Event::Reanchor, Reason::Format, 0)
    );
}

/// One 256-frame packet at 48 kHz, ns.
const MS_256: u64 = 5_333_333;

/// Review round 5: the first output of every anchored stretch carries its LEAD-IN -- silence from
/// the packet's arrival up to that output's stamp, ending exactly there -- and no later output does.
/// After a starve the re-anchored stretch's first output carries its own: libobs resets its timing
/// after a gap over 2 s and puts the first chunk at the instant it arrives, so without the lead-in
/// the new stretch would start one R early.
#[test]
fn the_first_output_of_a_stretch_carries_a_lead_in_up_to_its_stamp() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let check = |d: Decision, now: u64| {
        assert_eq!(d.event, Event::Packet, "{d:?}");
        // on the grid the stamp is R − one packet after the arrival: 34.67 ms = 1664 whole frames
        assert_eq!(d.lead_in_samples, 1664, "{d:?}");
        assert_eq!(
            d.lead_in_stamp_ns + samples_to_ns(d.lead_in_samples, RATE),
            d.stamp_ns,
            "the lead-in ends where the output starts: {d:?}"
        );
        assert!(
            d.lead_in_stamp_ns >= now && d.lead_in_stamp_ns - now < samples_to_ns(1, RATE),
            "the lead-in starts at the arrival (within a frame): {d:?}"
        );
    };
    check(
        t.packet(on_time(H), H as u32, 256, RATE, 2, R40),
        on_time(H),
    );
    assert!(t.emitted);
    for k in H + 1..H + 50 {
        let d = t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        assert_eq!((d.event, d.lead_in_samples), (Event::Packet, 0), "{d:?}");
    }
    // a 1.6 s outage: a starve re-anchor, the hold, then the new stretch's own lead-in
    let k0 = H + 50 + 300;
    let d = t.packet(on_time(k0), k0 as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert!(
        !t.emitted,
        "a re-anchor starts a stretch that has sent nothing"
    );
    for j in 1..H {
        let d = t.packet(on_time(k0 + j), (k0 + j) as u32, 256, RATE, 2, R40);
        assert_eq!((d.event, d.lead_in_samples), (Event::Hold, 0), "{d:?}");
    }
    check(
        t.packet(on_time(k0 + H), (k0 + H) as u32, 256, RATE, 2, R40),
        on_time(k0 + H),
    );
}

/// Review round 6: a stretch's first output is always an in-time PACKET with its lead-in. Late
/// packets past the hold but before it are dropped and counted with NOTHING filled: a fill whose
/// stamp had passed would be the first output with no lead-in, and libobs, which resets its timing
/// after a starve's gap, would start the stretch at that late fill's arrival. The slots before the
/// first packet are not filled either; from it on a late slot is silence.
#[test]
fn late_packets_before_a_stretch_s_first_output_are_dropped_without_a_fill() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let late = 60 * NS_PER_MS;
    for k in H..H + 2 {
        let d = t.packet(on_time(k) + late, k as u32, 256, RATE, 2, R40);
        assert_eq!(
            (d.event, d.silence_samples, d.lead_in_samples),
            (Event::Late, 0, 0),
            "packet {k}: {d:?}"
        );
        assert!(!t.emitted, "packet {k}: nothing is output yet");
    }
    let d = t.packet(on_time(H + 20), (H + 20) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.silence_samples, d.lead_in_samples),
        (Event::Packet, 0, 1664),
        "the first output: an in-time packet with its lead-in, nothing filled before it: {d:?}"
    );
    assert!(t.emitted);
    assert_eq!((t.late, t.lost, t.silence_samples), (2, 0, 0));
    let d = t.packet(on_time(H + 21) + late, (H + 21) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.silence_samples, d.lead_in_samples),
        (Event::Late, 256, 0),
        "after the first output a late slot is silence: {d:?}"
    );
}

/// Review round 4: the starve boundary on our clock. A late packet arriving exactly one second
/// after the last in-time packet did is still a drop; one ns more is the re-anchor.
#[test]
fn a_late_packet_exactly_one_second_after_the_last_in_time_one_is_still_a_drop() {
    let mut t = Timeline::new();
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let last = on_time(n_fit - 1);
    let d = t.packet(last + NS_PER_SEC, n_fit as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Late, "{d:?}");
    let d = t.packet(last + NS_PER_SEC + 1, (n_fit + 1) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert_eq!((t.late, t.re_anchors), (1, 1));
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
    // the index jumps while the packets keep arriving (a sender that skips its counter); a 2 s gap
    // in ARRIVALS is a starve instead (no packet in time for a second, review round 4)
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    t.packet(on_time(H), H as u32, 256, RATE, 2, R40);
    // 375 skipped indexes = exactly 2 s of audio: still a loss.
    let d = t.packet(on_time(H + 1), (H + 376) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.silence_samples), (Event::Packet, 375 * 256));
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

/// Review round 5: the FROZEN fit (display only) and its boundary. 240-frame packets, 200 a
/// second: after 12 s on the grid the fit is valid; packets then 20 ms later stay in time but are
/// not folded -- the 600th of them sits exactly `FROZEN_S` of audio past the last fold (not frozen),
/// the 601st is frozen; back on the grid, the next folded packet thaws it. Nothing re-anchors.
#[test]
fn a_fit_with_nothing_folded_for_three_seconds_is_frozen_and_thaws_on_a_fold() {
    let mut t = Timeline::new();
    let per_s = 48_000 / 240u64;
    let at = |k: u64| T0 + samples_to_ns((k + 1) * 240, RATE);
    for k in 0..12 * per_s {
        t.packet(at(k), k as u32, 240, RATE, 2, R40);
    }
    assert!(t.fit_valid && !t.fit_frozen());
    let off = 20 * NS_PER_MS;
    let k0 = 12 * per_s;
    for j in 0..600 {
        let d = t.packet(at(k0 + j) + off, (k0 + j) as u32, 240, RATE, 2, R40);
        assert_eq!(d.event, Event::Packet, "{d:?}");
        assert!(!t.fit_frozen(), "packet {j}: not yet past three seconds");
    }
    assert_eq!(t.next_pos - t.last_fold_end, FROZEN_S * u64::from(RATE));
    let d = t.packet(at(k0 + 600) + off, (k0 + 600) as u32, 240, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(t.fit_frozen(), "one packet past three seconds: frozen");
    // back on the grid (after the 20 ms the arrivals catch up): a fold thaws it
    let mut k = k0 + 601;
    let mut prev = at(k0 + 600) + off;
    while t.fit_frozen() {
        let now = at(k).max(prev + 1_000);
        prev = now;
        t.packet(now, k as u32, 240, RATE, 2, R40);
        k += 1;
        assert!(k < k0 + 700, "the fit never thawed");
    }
    assert!(t.fit_valid);
    assert_eq!((t.re_anchors, t.late), (0, 0));
}

/// Review round 5: a starve can come on the FIRST packet after an anchor (it arrives more than a
/// second after the anchor did) and INSIDE a hold (a gap of more than a second in the first
/// second): each is one re-anchor that holds again; nothing was ever output.
#[test]
fn a_starve_on_the_first_packet_after_an_anchor_or_inside_the_hold_re_anchors() {
    let mut t = Timeline::new();
    let d = t.packet(on_time(0), 0, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Anchor);
    let t1 = on_time(1) + 1_500 * NS_PER_MS;
    let d = t.packet(t1, 1, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason, d.starve_ns),
        (
            Event::Reanchor,
            Reason::Starve,
            (on_time(1) + 1_500 * NS_PER_MS - on_time(0)) as i64
        ),
        "{d:?}"
    );
    for k in 2..101u64 {
        let d = t.packet(
            t1 + samples_to_ns((k - 1) * 256, RATE),
            k as u32,
            256,
            RATE,
            2,
            R40,
        );
        assert_eq!(d.event, Event::Hold, "{d:?}");
    }
    let last = t1 + samples_to_ns(99 * 256, RATE);
    let t2 = last + 1_500 * NS_PER_MS;
    let d = t.packet(t2, 101, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason, d.starve_ns),
        (Event::Reanchor, Reason::Starve, (1_500 * NS_PER_MS) as i64),
        "a starve inside the hold: {d:?}"
    );
    for k in 102..101 + H {
        let d = t.packet(
            t2 + samples_to_ns((k - 101) * 256, RATE),
            k as u32,
            256,
            RATE,
            2,
            R40,
        );
        assert_eq!(d.event, Event::Hold, "the new anchor holds: {d:?}");
    }
    let d = t.packet(
        t2 + samples_to_ns(H * 256, RATE),
        (101 + H) as u32,
        256,
        RATE,
        2,
        R40,
    );
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(d.lead_in_samples > 0, "its own lead-in: {d:?}");
    assert_eq!((t.re_anchors, t.late, t.lost), (2, 0, 0));
}
