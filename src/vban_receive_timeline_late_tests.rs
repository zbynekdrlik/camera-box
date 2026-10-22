//! Issue 1439 — the unit tests of `vban_receive_timeline` for the playout deadline, late packets,
//! the starve, the lead-in and the frozen fit (ROZHODNUTÉ 6345255813 + 6347872706, review rounds
//! 4-6), split out of `vban_receive_timeline_tests.rs` before it passed its line budget. A test-only
//! `#[path]` child like the others; the shared helpers (the [`Rig`] that drives the worker at its
//! deadlines) live in that file.

use super::tests::{on_time, rig_through_the_hold, through_the_hold, Rig, H, R40, RATE, T0};
use super::*;

/// The packets of the first 15 s on the grid through a [`Rig`]: the fit is valid after them.
fn rig_with_a_fit(r: &mut Rig) -> u64 {
    let n_fit = 15 * 48_000 / 256u64;
    for k in 0..n_fit {
        r.packet(on_time(k), k as u32);
    }
    assert!(r.t.fit_valid);
    n_fit
}

/// ROZHODNUTÉ 6345255813 (2) + 6347872706: a fixed latency is a fixed jitter buffer. A slot goes to
/// libobs at its deadline (its stamp − the margin): its packet when it came by then, else silence
/// of exactly that slot; a packet that comes after its slot went out is LOST, never re-timed:
/// dropped and counted `late`. Arriving exactly at its deadline is in time.
#[test]
fn a_slot_whose_packet_misses_its_deadline_is_silence_and_the_packet_is_late() {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 1);
    r.packet(on_time(H), 1 + H as u32);
    let pos1 = (H + 1) * 256;
    let deadline1 = r.t.deadline_at(pos1);
    let d = r.packet(deadline1, 2 + H as u32);
    assert_eq!(
        d.event,
        Event::Packet,
        "arriving exactly at its deadline is on time"
    );
    let pos2 = pos1 + 256;
    let deadline2 = r.t.deadline_at(pos2);
    let stamp2 = r.t.stamp_at(pos2);
    let d = r.packet(deadline2 + 1, 3 + H as u32);
    assert_eq!((d.event, d.reason), (Event::Late, Reason::None), "{d:?}");
    assert_eq!(d.late_ns, 1, "late by exactly the one ns past its deadline");
    let silence = r.of(Due::Silence);
    assert_eq!(silence.len(), 1, "{:?}", r.outs);
    let (at, s) = silence[0];
    assert_eq!(
        (at, s.pos, s.frames, s.stamp_ns),
        (deadline2, pos2, 256, stamp2),
        "its slot went out as silence at its deadline, on the unchanged line"
    );
    assert_eq!((r.t.late, r.t.re_anchors, r.t.lost), (1, 0, 1));
    // the next packet: the same line, nothing more to fill
    let stamp3 = stamp2 + samples_to_ns(256, RATE);
    let d = r.packet(stamp3 - 3 * NS_PER_MS, 4 + H as u32);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(
        d.stamp_ns.abs_diff(stamp3) <= 1,
        "{} vs {stamp3}",
        d.stamp_ns
    );
    r.run_until(r.t.deadline_at(d.pos), true);
    assert_eq!(r.of(Due::Silence).len(), 1);
}

/// ROZHODNUTÉ 6345255813 (2): the late shift of 6343922049 item 2 is withdrawn (a runtime latency
/// change). A packet even 60 ms late (more than R) once the fit is valid is dropped -- with the
/// packets right behind it, which the receive thread hands over after it -- and the line, the fit
/// and the latency stay where the anchor put them; nothing re-anchors.
#[test]
fn a_late_packet_never_moves_the_line_even_more_than_r_late() {
    let mut r = Rig::new();
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    let n_fit = rig_with_a_fit(&mut r);
    let mut prev = on_time(n_fit) + 60 * NS_PER_MS;
    let d = r.packet(prev, n_fit as u32);
    assert_eq!(d.event, Event::Late, "{d:?}");
    assert!(r.t.fit_valid, "the fit is kept");
    let mut caught_up = false;
    for k in n_fit + 1..n_fit + 30 * 48_000 / 256 {
        let at = on_time(k).max(prev + 1_000);
        prev = at;
        let d = r.packet(at, k as u32);
        if !caught_up && d.event == Event::Late {
            continue;
        }
        caught_up = true;
        assert_eq!(d.event, Event::Packet, "packet {k}: {d:?}");
        assert!(
            d.stamp_ns.abs_diff(line(k)) <= samples_to_ns(1, RATE),
            "packet {k}: the anchored line must be held: {} vs {}",
            d.stamp_ns,
            line(k)
        );
    }
    assert!(r.t.late >= 1 && r.t.late < 10, "{}", r.t.late);
    assert_eq!(r.t.re_anchors, 0);
}

/// ROZHODNUTÉ 6347872706: every slot whose packet is not there at its deadline goes out as one
/// slot of silence at that deadline (never several slots merged); a packet that comes after them
/// is late. `lost` counts the silent slots, `late` the packet.
#[test]
fn the_slots_missing_at_their_deadline_are_silence_one_slot_each() {
    let mut r = Rig::new();
    let n_fit = rig_with_a_fit(&mut r);
    let pos = n_fit * 256;
    // packets n_fit and n_fit + 1 lost, n_fit + 2 arrives 50 ms late
    let k = n_fit + 2;
    let d = r.packet(on_time(k) + 50 * NS_PER_MS, k as u32);
    assert_eq!(d.event, Event::Late, "{d:?}");
    // (the sender sends nothing more, so every slot whose deadline passed by then is silent too)
    let silence = r.of(Due::Silence);
    assert!(silence
        .iter()
        .all(|(at, d)| *at == d.deadline_ns && d.frames == 256));
    assert_eq!(
        silence[..3]
            .iter()
            .map(|(at, d)| (*at == d.deadline_ns, d.pos, d.frames))
            .collect::<Vec<_>>(),
        vec![
            (true, pos, 256),
            (true, pos + 256, 256),
            (true, pos + 512, 256)
        ],
        "two lost slots + the late one's, each at its own deadline"
    );
    assert_eq!(
        (r.t.lost, r.t.late, r.t.re_anchors),
        (silence.len() as u64, 1, 0)
    );
    assert_eq!(
        r.t.next_pos,
        pos + 3 * 256,
        "the line runs on past its slot"
    );
}

/// ROZHODNUTÉ 6345255813 (2): only a PERSISTENT condition re-anchors: no packet in time for one
/// second (a starve). Every packet late on a network delay step is one: their slots go out as
/// silence, the late packets are dropped until one arrives more than a second after the last
/// in-time packet did, and that one is ONE full re-anchor (reason starve, review round 4). The
/// worker stops at the starve: no slot whose deadline is more than a second after the last in-time
/// arrival goes out. The re-anchor holds its first second, then the new line: the new network
/// delay + R, with its own lead-in.
#[test]
fn every_packet_late_for_one_second_is_one_re_anchor_through_the_hold() {
    let mut r = Rig::new();
    let n_fit = rig_with_a_fit(&mut r);
    let last_in_time = on_time(n_fit - 1);
    // from packet n_fit on, every packet 90 ms late (a delay step on the network)
    let step = 90 * NS_PER_MS;
    let mut k = n_fit;
    loop {
        let at = on_time(k) + step;
        let d = r.packet(at, k as u32);
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
    assert_eq!(r.t.re_anchors, 1);
    assert!(
        r.of(Due::Silence)
            .iter()
            .all(|(_, d)| d.deadline_ns <= last_in_time + NS_PER_SEC),
        "nothing goes out past the starve"
    );
    let leads = r.of(Due::LeadIn).len();
    // the re-anchor holds its first second, then the new line: arrival + 90 ms - one packet + R
    for j in 1..H {
        let d = r.packet(on_time(k + j) + step, (k + j) as u32);
        assert_eq!(d.event, Event::Hold, "{d:?}");
    }
    for j in H..H + 400 {
        let d = r.packet(on_time(k + j) + step, (k + j) as u32);
        assert_eq!(d.event, Event::Packet, "{d:?}");
        let want = on_time(k + j) + step - samples_to_ns(256, RATE) + R40;
        assert!(d.stamp_ns.abs_diff(want) <= 2, "{} vs {want}", d.stamp_ns);
    }
    assert_eq!(
        r.of(Due::LeadIn).len(),
        leads + 1,
        "the new stretch's lead-in"
    );
    assert_eq!(r.t.re_anchors, 1, "one delay step, one re-anchor");
}

/// Review round 4 (the starve of ROZHODNUTÉ 6345255813 item 2): a network OUTAGE of 1.5 s -- the
/// packets lost, the sender running on -- is no packet in time for a second: the worker sends the
/// slots of that second as silence, then stops, and the first packet after the outage, even on
/// time, is ONE re-anchor, then the hold. A 0.9 s outage is a loss: one slot of silence per lost
/// slot at its deadline, and the packet after it is in time.
#[test]
fn a_network_outage_of_more_than_a_second_is_one_re_anchor() {
    let line = |k: u64| on_time(k) - samples_to_ns(256, RATE) + R40;
    for (gap_packets, re_anchor) in [(281u64, true), (168, false)] {
        let mut r = Rig::new();
        let n_fit = rig_with_a_fit(&mut r);
        let last_in_time = on_time(n_fit - 1);
        let k = n_fit + gap_packets;
        let d = r.packet(on_time(k), k as u32);
        let silence = r.of(Due::Silence);
        assert!(
            silence.iter().all(|(_, d)| d.frames == 256),
            "one slot each"
        );
        if re_anchor {
            assert_eq!(
                (d.event, d.reason),
                (Event::Reanchor, Reason::Starve),
                "{d:?}"
            );
            assert!(
                silence
                    .iter()
                    .all(|(_, d)| d.deadline_ns <= last_in_time + NS_PER_SEC),
                "the worker stopped at the starve"
            );
            let d = r.packet(on_time(k + 1), (k + 1) as u32);
            assert_eq!(d.event, Event::Hold, "the re-anchor holds: {d:?}");
        } else {
            assert_eq!(d.event, Event::Packet, "{d:?}");
            // the rest of the lost slots go out at their deadlines, after this arrival
            r.run_until(r.t.deadline_at(d.pos), true);
            let silence = r.of(Due::Silence);
            assert!(silence.iter().all(|(_, d)| d.frames == 256));
            assert_eq!(
                silence.len() as u64,
                gap_packets,
                "a 0.9 s outage is a loss"
            );
            assert!(d.stamp_ns.abs_diff(line(k)) <= 2);
        }
        assert_eq!(r.t.re_anchors, u64::from(re_anchor));
    }
}

/// Review round 4: a sender that PAUSES for 1.5 s (its index runs on from where it stopped, so the
/// packets after it arrive 1.5 s late against the line) is the same starve: the first packet after
/// the pause is ONE re-anchor at once, never a second of late drops first.
#[test]
fn a_sender_pause_of_more_than_a_second_is_one_re_anchor_with_no_drops() {
    let mut r = Rig::new();
    let n_fit = rig_with_a_fit(&mut r);
    let d = r.packet(on_time(n_fit) + 1_500 * NS_PER_MS, n_fit as u32);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert_eq!(
        (r.t.late, r.t.re_anchors),
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
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 0);
    let d = r.packet(on_time(H), H as u32);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    let at = on_time(H) + 1_500 * NS_PER_MS;
    let d = r.packet(at, (H + 1) as u32);
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
    let d = r.packet_as(at + MS_256, (H + 2) as u32, 256, RATE, 1, R40);
    assert_eq!(
        (d.event, d.reason, d.starve_ns),
        (Event::Reanchor, Reason::Format, 0)
    );
}

/// One 256-frame packet at 48 kHz, ns.
const MS_256: u64 = 5_333_333;

/// Review round 5 + ROZHODNUTÉ 6347872706: every stretch's output starts with its LEAD-IN, once:
/// the worker, at the instant the stretch's first in-time packet arrives (the store wakes it),
/// sends silence from its own now + the margin (so a mix tick between its clock read and the
/// handover never finds the lead-in behind the window) up to that packet's slot's stamp, ending
/// exactly there; the slots follow. After a starve the re-anchored stretch gets its own.
#[test]
fn every_stretch_starts_with_one_lead_in_up_to_its_first_stamp() {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 0);
    let check = |r: &Rig, i: usize, now: u64, first: u64| {
        let leads = r.of(Due::LeadIn);
        let (at, l) = leads[i];
        assert_eq!(at, now, "the lead-in goes out at the arrival: {l:?}");
        // on the grid the stamp is R − one packet after the arrival, 34.67 ms; the lead-in starts
        // the 2 ms margin after the worker's now: 32.67 ms = 1568 whole frames
        assert_eq!(l.frames, 1568, "{l:?}");
        assert_eq!(
            l.stamp_ns + samples_to_ns(l.frames, RATE),
            first,
            "the lead-in ends where the first slot starts: {l:?}"
        );
        let start = now + DEADLINE_MARGIN_MS * NS_PER_MS;
        assert!(
            l.stamp_ns >= start && l.stamp_ns - start < samples_to_ns(1, RATE),
            "the lead-in starts the margin after the worker's now: {l:?}"
        );
    };
    let d = r.packet(on_time(H), H as u32);
    check(&r, 0, on_time(H), d.stamp_ns);
    assert!(r.t.emitted);
    for k in H + 1..H + 50 {
        r.packet(on_time(k), k as u32);
    }
    assert_eq!(r.of(Due::LeadIn).len(), 1, "one per stretch");
    // a 1.6 s outage: a starve re-anchor, the hold, then the new stretch's own lead-in
    let k0 = H + 50 + 300;
    let d = r.packet(on_time(k0), k0 as u32);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert!(
        !r.t.emitted,
        "a re-anchor starts a stretch that has sent nothing"
    );
    for j in 1..H {
        assert_eq!(
            r.packet(on_time(k0 + j), (k0 + j) as u32).event,
            Event::Hold
        );
    }
    assert_eq!(r.of(Due::LeadIn).len(), 1);
    let d = r.packet(on_time(k0 + H), (k0 + H) as u32);
    check(&r, 1, on_time(k0 + H), d.stamp_ns);
}

/// Review round 6: a stretch's output always starts with an in-time packet. Late packets past the
/// hold but before it are dropped and counted with NOTHING sent: no slot goes out before the first
/// in-time packet, so the stretch always starts on its lead-in and its stamps. From then on a late
/// packet's slot goes out as silence.
#[test]
fn late_packets_before_a_stretch_s_first_output_send_nothing() {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 0);
    let late = 60 * NS_PER_MS;
    for k in H..H + 2 {
        let d = r.packet(on_time(k) + late, k as u32);
        assert_eq!(d.event, Event::Late, "packet {k}: {d:?}");
        assert!(!r.t.emitted, "packet {k}: nothing is output yet");
    }
    assert!(r.outs.is_empty(), "{:?}", r.outs);
    let d = r.packet(on_time(H + 20), (H + 20) as u32);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert!(r.t.emitted);
    assert_eq!(r.of(Due::LeadIn).len(), 1);
    assert_eq!((r.t.late, r.t.lost, r.t.silence_samples), (2, 0, 0));
    let d = r.packet(on_time(H + 21) + late, (H + 21) as u32);
    assert_eq!(d.event, Event::Late, "{d:?}");
    // (nothing is sent after it, so the slots after it whose deadline passed are silent too)
    let silence = r.of(Due::Silence);
    assert_eq!(
        (silence[0].1.pos, silence[0].1.frames),
        ((H + 21) * 256, 256),
        "after the first output a late packet's slot went out as silence: {silence:?}"
    );
}

/// Review round 4: the starve boundary on our clock. A late packet arriving exactly one second
/// after the last in-time packet did is still a drop; one ns more is the re-anchor.
#[test]
fn a_late_packet_exactly_one_second_after_the_last_in_time_one_is_still_a_drop() {
    let mut r = Rig::new();
    let n_fit = rig_with_a_fit(&mut r);
    let last = on_time(n_fit - 1);
    let d = r.packet(last + NS_PER_SEC, n_fit as u32);
    assert_eq!(d.event, Event::Late, "{d:?}");
    let d = r.packet(last + NS_PER_SEC + 1, (n_fit + 1) as u32);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
    assert_eq!((r.t.late, r.t.re_anchors), (1, 1));
}

/// ROZHODNUTÉ 6347872706: the deadlines run on the fitted SENDER rate. A sender 100 ppm slow for a
/// minute: every slot goes out as its packet (no silence, nothing late) by its deadline -- at the
/// worker's first wake after the packet arrived, at most [`POLL_MS`] later (review round 5) -- and
/// the emitted stamps are one continuous sequence: each slot's stamp follows the previous one by
/// one packet at the line's rate, never a step.
#[test]
fn the_worker_emits_every_slot_by_its_deadline_on_the_fitted_rate() {
    let mut r = Rig::new();
    let span = |k: u64| (samples_to_ns((k + 1) * 256, RATE) as f64 * 1.0001) as u64;
    for k in 0..(60 * 48_000 / 256) {
        let d = r.packet(T0 + span(k), k as u32);
        assert!(
            matches!(d.event, Event::Anchor | Event::Hold | Event::Packet),
            "packet {k}: {d:?}"
        );
    }
    assert!((r.t.fit_ppb - 100_000).abs() < 200, "fit {}", r.t.fit_ppb);
    let slots = r.of(Due::Packet);
    assert!(slots.len() > 10_000, "{}", slots.len());
    assert!(r.of(Due::Silence).is_empty() && r.t.late == 0);
    let nominal = samples_to_ns(256, RATE) as i64;
    for w in slots.windows(2) {
        let ((at, a), (_, b)) = (w[0], w[1]);
        let arrival = T0 + span(a.pos / 256);
        assert!(
            at >= arrival && at <= arrival + POLL_MS * NS_PER_MS && at < a.deadline_ns,
            "each slot at the worker's next wake after its arrival {arrival}, long before its \
             deadline: {at} {a:?}"
        );
        assert_eq!(b.pos, a.pos + 256, "every slot in order");
        let step = b.stamp_ns as i64 - a.stamp_ns as i64;
        assert!(
            (step - nominal).abs() <= nominal / 1000,
            "a stamp step of {step} ns against {nominal} nominal: {a:?} -> {b:?}"
        );
    }
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
    let mut r = Rig::new();
    let d = r.packet(on_time(0), 0);
    assert_eq!(d.event, Event::Anchor);
    let t1 = on_time(1) + 1_500 * NS_PER_MS;
    let d = r.packet(t1, 1);
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
        let d = r.packet(t1 + samples_to_ns((k - 1) * 256, RATE), k as u32);
        assert_eq!(d.event, Event::Hold, "{d:?}");
    }
    let last = t1 + samples_to_ns(99 * 256, RATE);
    let t2 = last + 1_500 * NS_PER_MS;
    let d = r.packet(t2, 101);
    assert_eq!(
        (d.event, d.reason, d.starve_ns),
        (Event::Reanchor, Reason::Starve, (1_500 * NS_PER_MS) as i64),
        "a starve inside the hold: {d:?}"
    );
    for k in 102..101 + H {
        let d = r.packet(t2 + samples_to_ns((k - 101) * 256, RATE), k as u32);
        assert_eq!(d.event, Event::Hold, "the new anchor holds: {d:?}");
    }
    assert!(r.outs.is_empty(), "nothing was ever output: {:?}", r.outs);
    let d = r.packet(t2 + samples_to_ns(H * 256, RATE), (101 + H) as u32);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert_eq!(r.of(Due::LeadIn).len(), 1, "its own lead-in");
    assert_eq!((r.t.re_anchors, r.t.late, r.t.lost), (2, 0, 0));
}

/// Review round 5: the deadline is the DECISION point, not the handover time. A slot whose packet
/// is stored goes to libobs whenever the worker looks (its `late_ns` negative: that far ahead of its
/// deadline); while the slot's packet is missing the worker looks again after at most [`POLL_MS`],
/// the last look being the deadline itself, where the slot is one slot of silence.
#[test]
fn a_stored_slot_goes_out_when_the_worker_looks_a_missing_one_at_its_deadline() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let now = on_time(H);
    let d = t.packet(now, H as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Packet, "{d:?}");
    assert_eq!(t.due(now, Some(d.pos)).kind, Due::LeadIn);
    let p = t.due(now, Some(d.pos));
    assert_eq!(
        (p.kind, p.pos, p.late_ns),
        (Due::Packet, d.pos, now as i64 - t.deadline_at(d.pos) as i64),
        "the stored slot goes out at once: {p:?}"
    );
    assert!(p.late_ns < -30 * NS_PER_MS as i64, "{p:?}");
    let next = d.pos + 256;
    let deadline = t.deadline_at(next);
    let w = t.due(now + 1, None);
    assert_eq!(
        (w.kind, w.next_ns),
        (Due::None, now + 1 + POLL_MS * NS_PER_MS),
        "no packet yet: look again after the poll"
    );
    let w = t.due(deadline - NS_PER_MS, None);
    assert_eq!(
        (w.kind, w.next_ns),
        (Due::None, deadline),
        "the last look is the deadline itself"
    );
    let s = t.due(deadline, None);
    assert_eq!(
        (s.kind, s.pos, s.frames, s.late_ns),
        (Due::Silence, next, 256, 0)
    );
    assert_eq!((t.lost, t.missed), (1, 0));
}

/// Review round 5: a slot handed to libobs AFTER its own stamp (the worker woke later than the
/// margin) may have missed the window that mixes it: counted `missed`, silence or packet. One
/// handed exactly at its stamp is not.
#[test]
fn a_slot_handed_after_its_stamp_is_counted_missed() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let d = t.packet(on_time(H), H as u32, 256, RATE, 2, R40);
    assert_eq!(t.due(on_time(H), Some(d.pos)).kind, Due::LeadIn);
    // at its stamp exactly: not missed
    let p = t.due(d.stamp_ns, Some(d.pos));
    assert_eq!((p.kind, t.missed), (Due::Packet, 0), "{p:?}");
    // the next packet never comes: its silence one ns after its stamp is missed
    let pos = d.pos + 256;
    let s = t.due(t.stamp_at(pos) + 1, None);
    assert_eq!((s.kind, s.pos, t.missed), (Due::Silence, pos, 1), "{s:?}");
    // the one after it arrives after its deadline yet before its slot went out (in time), and goes
    // out after its stamp
    let pos = pos + 256;
    let stamp = t.stamp_at(pos);
    let d = t.packet(stamp - NS_PER_MS, (H + 2) as u32, 256, RATE, 2, R40);
    assert_eq!((d.event, d.pos), (Event::Packet, pos), "{d:?}");
    let p = t.due(stamp + 5, Some(pos));
    assert_eq!((p.kind, t.missed, t.lost), (Due::Packet, 2, 1), "{p:?}");
}

/// Review round 5: a stretch whose first packet arrives at its deadline has no time left before
/// its stamp for a lead-in that starts the margin after the worker's now: none is sent, the slot
/// goes straight out.
#[test]
fn no_lead_in_when_the_first_packet_leaves_no_room_before_its_stamp() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let deadline = t.deadline_at(H * 256);
    let d = t.packet(deadline, H as u32, 256, RATE, 2, R40);
    assert_eq!(
        d.event,
        Event::Packet,
        "arriving at its deadline is in time: {d:?}"
    );
    let p = t.due(deadline, Some(d.pos));
    assert_eq!((p.kind, p.pos, p.late_ns), (Due::Packet, d.pos, 0), "{p:?}");
    assert!(!t.lead_in_pending);
}
