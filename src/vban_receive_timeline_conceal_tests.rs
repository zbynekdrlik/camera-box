//! Issue 1439 — the unit tests of `vban_receive_timeline` for the CONCEALMENT decision (ROZHODNUTÉ
//! 6352253147 item (b); the run limit raised to 3 slots on 23.10.2026 from a live wire capture on
//! stream-snv: the network loses packets in bursts of 1 to 3). A test-only `#[path]` child like the
//! others; the shared helpers live in `vban_receive_timeline_tests.rs`. The sample math of a
//! concealed slot is `crate::vban_conceal`.

use super::tests::{on_time, rig_through_the_hold, Rig, H};
use super::*;

/// A stretch on the grid past the hold (indexes = packets), the packets in `lost` never arriving,
/// then the worker run until the last packet's deadline.
fn run_with_losses(last: u64, lost: &[u64]) -> Rig {
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 0);
    for k in H..=last {
        if !lost.contains(&k) {
            r.packet(on_time(k), k as u32);
        }
    }
    r.run_until(r.t.deadline_at(last * 256), true);
    r
}

/// The slots that went out (packets, silences and concealments), as (kind, slot index, toward the
/// next packet).
fn slots(r: &Rig) -> Vec<(Due, u64, bool)> {
    r.outs
        .iter()
        .filter(|(_, d)| d.kind != Due::LeadIn)
        .map(|(_, d)| (d.kind, d.pos / 256, d.toward_next))
        .collect()
}

/// ROZHODNUTÉ 6352253147 (b): one slot missing between two received packets is CONCEALED at its
/// deadline (the previous packet's samples, faded toward the next packet, which is stored by then),
/// never silence: counted in `lost` and in `concealed`, no silence.
#[test]
fn one_missing_slot_between_two_packets_is_concealed_toward_the_next() {
    let k = H + 10;
    let r = run_with_losses(H + 14, &[k]);
    let got = slots(&r);
    let at = got
        .iter()
        .position(|s| s.1 == k)
        .expect("the slot went out");
    assert_eq!(got[at - 1], (Due::Packet, k - 1, false));
    assert_eq!(got[at], (Due::Conceal, k, true), "{got:?}");
    assert_eq!(got[at + 1], (Due::Packet, k + 1, false));
    let (when, d) = r
        .outs
        .iter()
        .find(|(_, d)| d.pos == k * 256 && d.kind == Due::Conceal)
        .copied()
        .unwrap();
    assert_eq!(
        (when, d.frames, d.stamp_ns),
        (r.t.deadline_at(k * 256), 256, r.t.stamp_at(k * 256)),
        "one slot, at its deadline, on the line"
    );
    assert_eq!((r.t.lost, r.t.concealed, r.t.silence_samples), (1, 1, 0));
}

/// Runs of two and three missing slots are concealed, every slot fading in from the last one handed
/// out and only the run's last slot fading toward the packet that ends it.
#[test]
fn runs_of_two_and_three_missing_slots_are_concealed() {
    for run in [2u64, 3] {
        let k = H + 10;
        let lost: Vec<u64> = (k..k + run).collect();
        let r = run_with_losses(H + 20, &lost);
        let got = slots(&r);
        let at = got.iter().position(|s| s.1 == k).expect("the run went out");
        let want: Vec<(Due, u64, bool)> = (k..k + run)
            .map(|j| (Due::Conceal, j, j + 1 == k + run))
            .collect();
        assert_eq!(&got[at..at + run as usize], &want[..], "run {run}: {got:?}");
        assert_eq!(got[at + run as usize], (Due::Packet, k + run, false));
        assert_eq!(
            (r.t.lost, r.t.concealed, r.t.silence_samples),
            (run, run, 0),
            "run {run}"
        );
    }
}

/// A run of four (more than `CONCEAL_MAX_SLOTS`) is silence, every slot of it, as before.
#[test]
fn a_run_of_four_missing_slots_is_silence() {
    let k = H + 10;
    let r = run_with_losses(H + 20, &[k, k + 1, k + 2, k + 3]);
    let got = slots(&r);
    let at = got.iter().position(|s| s.1 == k).expect("the run went out");
    assert_eq!(
        &got[at..at + 5],
        &[
            (Due::Silence, k, false),
            (Due::Silence, k + 1, false),
            (Due::Silence, k + 2, false),
            (Due::Silence, k + 3, false),
            (Due::Packet, k + 4, false)
        ]
    );
    assert_eq!(
        (r.t.lost, r.t.concealed, r.t.silence_samples),
        (4, 0, 4 * 256)
    );
}

/// The decision never waits for a later packet: a slot whose next packets are all held back to just
/// past its deadline finds nothing stored then and is silence. The run that started as silence stays
/// silence even once a packet is stored behind its next slot.
#[test]
fn a_missing_slot_with_nothing_stored_at_its_deadline_is_silence_and_its_run_stays_silence() {
    for run in [1u64, 2] {
        let mut r = Rig::new();
        rig_through_the_hold(&mut r, 0);
        let k = H + 10;
        for j in H..k {
            r.packet(on_time(j), j as u32);
        }
        // k .. k + run lost, the next five held back to 0.1 ms past the deadline of k
        let held = r.t.deadline_at(k * 256) + 100_000;
        for (i, j) in (k + run..k + run + 5).enumerate() {
            r.packet(held + i as u64 * 1_000, j as u32);
        }
        r.run_until(r.t.deadline_at((k + run + 4) * 256), true);
        let got = slots(&r);
        let at = got.iter().position(|s| s.1 == k).expect("the run went out");
        let want: Vec<(Due, u64, bool)> = (k..k + run).map(|j| (Due::Silence, j, false)).collect();
        assert_eq!(&got[at..at + run as usize], &want[..], "run {run}: {got:?}");
        assert_eq!(got[at + run as usize], (Due::Packet, k + run, false));
        assert_eq!((r.t.lost, r.t.concealed), (run, 0), "run {run}");
    }
}

/// Short runs with a packet between them are separate concealed runs: the run count starts again at
/// every packet. A reordered packet that comes before the deadline of the missing slot in front of
/// it ends that run the same way: the missing slot fades toward it, and it plays in its own slot.
#[test]
fn a_packet_ends_a_concealed_run_and_a_reorder_plays_inside_one() {
    let k = H + 10;
    let r = run_with_losses(H + 30, &[k, k + 2, k + 3, k + 4, k + 6]);
    let got = slots(&r);
    let at = got
        .iter()
        .position(|s| s.1 == k)
        .expect("the slots went out");
    assert_eq!(
        &got[at..at + 8],
        &[
            (Due::Conceal, k, true),
            (Due::Packet, k + 1, false),
            (Due::Conceal, k + 2, false),
            (Due::Conceal, k + 3, false),
            (Due::Conceal, k + 4, true),
            (Due::Packet, k + 5, false),
            (Due::Conceal, k + 6, true),
            (Due::Packet, k + 7, false)
        ]
    );
    assert_eq!((r.t.lost, r.t.concealed), (5, 5));
    // k - 1 lost, k overtaken by k + 1 AND k + 2 (it arrives behind k + 2), still before the deadline
    // of k - 1: the next stored packet then is k itself
    let mut r = Rig::new();
    rig_through_the_hold(&mut r, 0);
    for j in H..k - 1 {
        r.packet(on_time(j), j as u32);
    }
    r.packet(on_time(k + 1), (k + 1) as u32);
    r.packet(on_time(k + 2), (k + 2) as u32);
    let d = r.packet(on_time(k + 2) + 100_000, k as u32);
    assert_eq!(d.event, Event::Reorder, "{d:?}");
    r.run_until(r.t.deadline_at((k + 2) * 256), true);
    let got = slots(&r);
    let at = got
        .iter()
        .position(|s| s.1 == k - 1)
        .expect("the slot went out");
    assert_eq!(
        &got[at..at + 4],
        &[
            (Due::Conceal, k - 1, true),
            (Due::Packet, k, false),
            (Due::Packet, k + 1, false),
            (Due::Packet, k + 2, false)
        ],
        "the reorder came before the deadline of k - 1: the run is one slot"
    );
}

/// The run limit is the decision's own, never only the caller's store: whatever head the caller
/// hands in, a run never reaches `CONCEAL_MAX_SLOTS` + 1 concealed slots (in the source the head
/// only ever comes closer, so `run so far + next` never grows; a head that moved away -- a store
/// that lost a packet -- still ends the run in silence).
#[test]
fn a_run_never_passes_the_limit_whatever_head_the_caller_hands_in() {
    let mut t = Timeline::new();
    super::tests::through_the_hold(&mut t, 0);
    let now = on_time(H);
    let d = t.packet(now, H as u32, 256, 48_000, 2, 40 * NS_PER_MS);
    assert_eq!(t.due(now, Some(d.pos)).kind, Due::LeadIn);
    assert_eq!(t.due(now, Some(d.pos)).kind, Due::Packet);
    // slot H + 1 missing, the next stored packet three slots on: concealed
    let s1 = (H + 1) * 256;
    let a = t.due(t.deadline_at(s1), Some(s1 + 3 * 256));
    assert_eq!((a.kind, a.toward_next), (Due::Conceal, false), "{a:?}");
    // the head moved away by a slot (a store that lost a packet): one slot so far + three on = four
    let s2 = s1 + 256;
    let b = t.due(t.deadline_at(s2), Some(s2 + 3 * 256));
    assert_eq!(
        b.kind,
        Due::Silence,
        "a fourth concealed slot would pass the limit: {b:?}"
    );
    assert_eq!((t.lost, t.concealed), (2, 1));
}
