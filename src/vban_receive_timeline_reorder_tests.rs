//! Issue 1439 — the unit tests of `vban_receive_timeline` for a REORDERED packet (ROZHODNUTÉ
//! 6352253147 item (a)): live on stream-snv (R 80 ms) a packet that a router reorder delivered right
//! behind the packet that overtook it was dropped as stale while its slot was still ~80 ms in the
//! future, and the slot went out as silence -- a click. A test-only `#[path]` child like the others;
//! the shared helpers live in `vban_receive_timeline_tests.rs`.

use super::tests::{on_time, rig_through_the_hold, through_the_hold, Rig, H, R40, RATE};
use super::*;

/// The fit, the line and the index state a reorder must never move (everything but the counters
/// and the arrival mask).
fn line_and_fit(t: &Timeline) -> String {
    let buckets: Vec<String> = t
        .buckets
        .iter()
        .map(|b| {
            format!(
                "{} {} {} {} {} {} {} {}",
                b.k,
                b.n,
                b.x0,
                b.y0,
                b.su.to_bits(),
                b.sv.to_bits(),
                b.suu.to_bits(),
                b.suv.to_bits()
            )
        })
        .collect();
    format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} | {}",
        t.last_nu,
        t.next_pos,
        t.base_stamp_ns,
        t.base_pos,
        t.step_ppb,
        t.last_k,
        t.last_intime_ns,
        t.last_fold_end,
        u8::from(t.fit_valid),
        t.fit_ppb,
        t.fit_b.to_bits(),
        t.fit_xbar.to_bits(),
        t.fit_ybar.to_bits(),
        t.phase_ns,
        t.emit_pos,
        buckets.join(" ")
    )
}

/// ROZHODNUTÉ 6352253147 (a): a packet overtaken by the next one on the way arrives right behind it,
/// ~R before its own slot goes out. It is stored for that slot and played in it, in order: no
/// silence, no loss. A duplicate of it, or of the last packet, is still dropped as stale.
#[test]
fn a_reordered_packet_whose_slot_is_still_ahead_plays_in_its_slot() {
    let mut r = Rig::new();
    // the held first second, then packets 100 .. 111 (the index below names their offset)
    rig_through_the_hold(&mut r, 100u32.wrapping_sub(H as u32));
    for k in H..H + 12 {
        r.packet(on_time(k), (100 + k - H) as u32);
    }
    let at = |k: u64| on_time(H + k);
    // 112 overtaken by 113 on the way (the router's reorder)
    assert_eq!(r.packet(at(13), 113).event, Event::Packet);
    let pos = (H + 12) * 256;
    let d = r.packet(at(13) + 100_000, 112);
    assert_eq!(
        (d.event, d.pos, d.stamp_ns),
        (Event::Reorder, pos, r.t.stamp_at(pos)),
        "a reordered packet whose slot is still ahead is stored for that slot: {d:?}"
    );
    assert_eq!(
        d.lateness_ns,
        (at(13) + 100_000) as i64 - r.t.stamp_at(pos) as i64
    );
    assert_eq!((r.t.reordered, r.t.stale), (1, 0));
    // its index came now: a duplicate of it is stale, so is a duplicate of the last packet
    assert_eq!(r.packet(at(13) + 200_000, 112).event, Event::Stale);
    assert_eq!(r.packet(at(13) + 300_000, 113).event, Event::Stale);
    assert_eq!((r.t.reordered, r.t.stale), (1, 2));
    let d = r.packet(at(14), 114);
    assert_eq!(d.event, Event::Packet);
    r.run_until(r.t.deadline_at(d.pos), true);
    assert_eq!(
        r.of(Due::Packet)
            .iter()
            .map(|(_, d)| d.pos)
            .collect::<Vec<_>>(),
        (H..=H + 14).map(|k| k * 256).collect::<Vec<_>>(),
        "every slot went out as its packet, 112 between 111 and 113: {:?}",
        r.outs
    );
    assert!(r.of(Due::Silence).is_empty(), "{:?}", r.outs);
    assert_eq!((r.t.lost, r.t.late, r.t.re_anchors), (0, 0, 0));
}

/// ROZHODNUTÉ 6352253147 (a): the reorder never moves the line, the fit, `last_nu`, `next_pos` or
/// `last_intime_ns` -- its arrival is out of order, so it is never folded (a fold before the
/// bucket's first packet would also wrap the bucket's unsigned offsets). A stream with reorders
/// leaves the timeline bit-identical to the same stream without the reordered packets.
#[test]
fn a_reorder_never_moves_the_line_the_fit_or_the_index() {
    let mut with = Timeline::new();
    let mut without = Timeline::new();
    let n = 20 * 48_000 / 256u64;
    let mut swaps = 0;
    let mut k = 0u64;
    while k < n {
        // every 50th packet comes 0.1 ms behind the next one (after the hold, the fit then valid)
        if k > H && k % 50 == 0 && k + 1 < n {
            for t in [&mut with, &mut without] {
                t.packet(on_time(k + 1), (k + 1) as u32, 256, RATE, 2, R40);
            }
            let d = with.packet(on_time(k + 1) + 100_000, k as u32, 256, RATE, 2, R40);
            assert_eq!(d.event, Event::Reorder, "packet {k}: {d:?}");
            swaps += 1;
            assert_eq!(
                line_and_fit(&with),
                line_and_fit(&without),
                "the reorder of packet {k} moved the timeline"
            );
            k += 2;
            continue;
        }
        for t in [&mut with, &mut without] {
            t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
        }
        k += 1;
    }
    assert!(with.fit_valid, "the fit became valid with the reorders in");
    assert_eq!(with.reordered, swaps);
    assert_eq!(line_and_fit(&with), line_and_fit(&without));
}

/// ROZHODNUTÉ 6352253147 (a): STALE stays for a packet whose slot already went out, for a duplicate
/// of a packet that came (its slot is stored or out), and for an index exactly `STALE_PACKETS`
/// behind (beyond the 64 indexes the timeline remembers). None of them is stored.
#[test]
fn a_reorder_whose_slot_went_out_a_duplicate_and_the_window_edge_stay_stale() {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 100u32.wrapping_sub(H as u32));
    for k in H..H + 12 {
        r.packet(on_time(k), (100 + k - H) as u32);
    }
    let at = |k: u64| on_time(H + k);
    // 113 arrives, 112 not: its slot goes out at its deadline (empty) -- then 112 comes
    r.packet(at(13), 113);
    let pos = (H + 12) * 256;
    let deadline = r.t.deadline_at(pos);
    r.run_until(deadline, true);
    assert!(r.t.emit_pos > pos, "the slot of 112 went out");
    let d = r.packet(deadline + 1, 112);
    assert_eq!(
        (d.event, d.pos),
        (Event::Stale, 0),
        "its slot already went out: {d:?}"
    );
    // 114 and 115 in time, then a duplicate of 114: its slot is stored, not out
    r.packet(at(14), 114);
    r.packet(at(15), 115);
    assert!(
        r.t.emit_pos <= (H + 14) * 256,
        "the slot of 114 is still ahead"
    );
    assert_eq!(r.packet(at(15) + 1_000, 114).event, Event::Stale);
    // a duplicate of the last packet, and one exactly STALE_PACKETS behind
    assert_eq!(r.packet(at(15) + 2_000, 115).event, Event::Stale);
    assert_eq!(
        r.packet(at(15) + 3_000, 115 - STALE_PACKETS).event,
        Event::Stale
    );
    assert_eq!((r.t.stale, r.t.reordered, r.t.late), (4, 0, 0));
    assert_eq!(r.t.re_anchors, 0);
}

/// ROZHODNUTÉ 6352253147 (a): the timeline remembers the last 64 indexes; a packet up to 63 behind
/// whose index never came is a reorder (here on a bare timeline: no worker has handed a slot out),
/// one 64 behind is stale (`STALE_PACKETS`), never a re-anchor.
#[test]
fn the_reorder_window_is_the_63_indexes_behind_the_last() {
    let mut t = Timeline::new();
    through_the_hold(&mut t, 0);
    let last = H + 80;
    for k in H..=last {
        if k == last - 63 || k == last - 64 {
            continue;
        }
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    let now = on_time(last) + 1_000;
    let d = t.packet(now, (last - 63) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.pos),
        (Event::Reorder, (last - 63) * 256),
        "63 behind: {d:?}"
    );
    let d = t.packet(now, (last - 64) as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Stale, "64 behind: {d:?}");
    assert_eq!((t.reordered, t.stale, t.re_anchors), (1, 1, 0));
}

/// ROZHODNUTÉ 6352253147 (a): nothing is stored before a stretch's first output (the hold), and a
/// stretch that STARVED (no packet in time for `PERSIST_S`) takes no reorder either -- the next
/// in-order packet re-anchors, as before; a reorder never does.
#[test]
fn a_reorder_in_the_hold_or_in_a_starved_stretch_is_stale() {
    let mut t = Timeline::new();
    for k in 0..5u64 {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    // 6 overtakes 5 inside the hold
    t.packet(on_time(6), 6, 256, RATE, 2, R40);
    let d = t.packet(on_time(6) + 100_000, 5, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Stale, "inside the hold: {d:?}");
    for k in 7..H + 4 {
        t.packet(on_time(k), k as u32, 256, RATE, 2, R40);
    }
    assert!(t.emitted);
    // H + 4 never comes in time; H + 5 does, then nothing for 1.5 s; H + 4 shows up after that
    t.packet(on_time(H + 5), (H + 5) as u32, 256, RATE, 2, R40);
    let later = on_time(H + 5) + 1_500 * NS_PER_MS;
    let d = t.packet(later, (H + 4) as u32, 256, RATE, 2, R40);
    assert_eq!(d.event, Event::Stale, "the stretch starved: {d:?}");
    assert_eq!(t.reordered, 0);
    let d = t.packet(later + 1_000, (H + 6) as u32, 256, RATE, 2, R40);
    assert_eq!(
        (d.event, d.reason),
        (Event::Reanchor, Reason::Starve),
        "{d:?}"
    );
}
