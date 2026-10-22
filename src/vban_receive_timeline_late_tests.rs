//! Issue 1439 — the unit tests of `vban_receive_timeline` for late packets, the starve, the lead-in
//! and the frozen fit (ROZHODNUTÉ 6345255813 + review rounds 4-6), split out of
//! `vban_receive_timeline_tests.rs` before it passed its line budget (review round 6). A test-only
//! `#[path]` child like the others; the shared helpers live in that file.

use super::tests::{on_time, through_the_hold, H, R40, RATE, T0};
use super::*;

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
