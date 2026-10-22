//! Issue 1439 -- ROZHODNUTÉ 6352253147 item (a) in the SHIPPED source: a reordered VBAN packet whose
//! slot is still to go out is played in its slot, and a loss is counted and logged when its slot
//! goes out empty, never when a gap is first seen.
//!
//! Live on stream-snv (R 80 ms, 23.10.2026): `lost=107 stale=40` in 55 min. A router reorder
//! delivered packet n right behind n + 1; the source dropped n as stale while its slot was ~80 ms
//! ahead, and the slot went out as silence (a click). The receive callback also logged `missing N
//! packet(s)` the moment it saw the gap, so a gap a later packet filled read as a loss.
//!
//! The same lift-and-run driver as `tests/vban_source_loss_1439.rs` (the shipped `vban-source.c`
//! with libobs stubs, its playout worker on simulated time). Standalone, no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_source_reorder_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required; a missing compiler FAILS, never skips.

#[allow(dead_code)]
#[path = "support/vban_source_harness_1439.rs"]
mod harness;

use harness::*;

/// The packets `nus` behind a held first second ([`after_hold`]) with `nu` arriving 0.1 ms after the
/// packet that follows it (the router's reorder): the next packet keeps its arrival.
fn overtaken(nus: &[u32], nu: u32) -> Vec<(u32, u64, i16)> {
    let mut pkts = after_hold(nus);
    let i = pkts
        .iter()
        .position(|p| p.0 == nu)
        .expect("the overtaken packet");
    let next = pkts[i + 1];
    pkts[i + 1] = (pkts[i].0, next.1 + 100_000, pkts[i].2);
    pkts[i] = next;
    pkts
}

/// The plugin's `missing N packet(s)` lines, in order.
fn loss_lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|l| l.split_once("source 'Zvuk z mixu': missing "))
        .map(|(_, rest)| rest)
        .collect()
}

/// ROZHODNUTÉ 6352253147 (a): 305 overtaken by 306 on the way arrives 0.1 ms behind it, ~R before
/// its slot goes out: every packet reaches OBS exactly once, in slot order, with no silence, no
/// loss line, and the source's summary counts the reorder.
#[test]
fn a_reordered_packet_whose_slot_is_still_ahead_plays_in_its_slot_1439() {
    let (outs, log) = run_with_log(&overtaken(&(300..=310).collect::<Vec<u32>>(), 305));
    assert_eq!(
        audio_values(&outs),
        (1..=11).collect::<Vec<i64>>(),
        "issue 1439: the reordered packet must play in its own slot, the others unchanged: {outs:?}"
    );
    assert!(
        after_lead_in(&outs).iter().all(|o| !o.zero),
        "issue 1439: no slot goes out empty: {outs:?}"
    );
    assert_contiguous(&outs);
    assert!(
        loss_lines(&log).is_empty(),
        "issue 1439: a gap the reorder filled is no loss:\n{log}"
    );
    assert!(
        log.contains(", reordered 1,"),
        "issue 1439: the source's summary counts the reorder:\n{log}"
    );
}

/// ROZHODNUTÉ 6352253147 (a): the 10 s status line carries `reordered=`, and a reorder in time is
/// neither stale nor lost.
#[test]
fn the_status_line_counts_the_reorders_1439() {
    // 11 s of an ideal sender: the first packet arms the status clock, the line comes 10 s later
    let nus: Vec<u32> = (1000..1000 + 2063).collect();
    let mut pkts = on_grid(&nus);
    let next = pkts[1001];
    pkts[1001] = (pkts[1000].0, next.1 + 100_000, pkts[1000].2);
    pkts[1000] = next;
    let (outs, log) = run_with_log(&pkts);
    assert!(after_lead_in(&outs).iter().all(|o| !o.zero), "{outs:?}");
    let line = log
        .lines()
        .find_map(|l| l.strip_prefix("blog 300: [obs-vban] obs-vban receive: "))
        .unwrap_or_else(|| panic!("issue 1439: no status line:\n{log}"));
    let field = |k: &str| -> &str {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(k))
            .unwrap_or_else(|| panic!("issue 1439: no `{k}` in the status line `{line}`"))
    };
    for (k, want) in [("reordered=", "1"), ("stale=", "0"), ("lost=", "0")] {
        assert_eq!(field(k), want, "issue 1439: `{k}` in `{line}`");
    }
}

/// ROZHODNUTÉ 6352253147 (a): a loss is counted and logged when its slot goes out empty, one line
/// per run of missing slots -- never when the gap is first seen. 304 is lost and 305 overtaken by
/// 306 (the receive side sees a gap of two at 306, but 305 still comes in time: ONE slot went out
/// empty, before 305); 310 .. 313 are lost (four slots, before 314).
#[test]
fn a_loss_is_logged_when_its_slots_go_out_one_line_per_run_1439() {
    let nus: Vec<u32> = (300..=304)
        .chain(305..=309)
        .chain(314..=320)
        .filter(|&n| n != 304)
        .collect();
    let (outs, log) = run_with_log(&overtaken(&nus, 305));
    let lines = loss_lines(&log);
    assert_eq!(lines.len(), 2, "issue 1439: two runs, two lines:\n{log}");
    assert!(
        lines[0].starts_with("1 packet(s) before frame 305 -- "),
        "issue 1439: only the slot of 304 went out without its packet, before 305:\n{log}"
    );
    assert_eq!(
        lines[1], "4 packet(s) before frame 314 -- silence for those slots",
        "issue 1439: a run of four is one line, before 314:\n{log}"
    );
    assert_eq!(
        audio_values(&outs).iter().filter(|&&v| v == 6).count(),
        1,
        "issue 1439: 305 plays in its slot: {outs:?}"
    );
    assert_contiguous(&outs);
}

/// ROZHODNUTÉ 6352253147 (a): a run of missing slots that a starve ends (a 1.5 s outage: the worker
/// keeps the line for a second, then stops) is logged once, when the stretch ends.
#[test]
fn an_outage_logs_its_missing_slots_once_when_the_stretch_ends_1439() {
    let n = 15 * RATE / u64::from(FRAMES);
    let nus: Vec<u32> = (0..n as u32)
        .chain((n + 282) as u32..(n + 282 + 300) as u32)
        .collect();
    let (_outs, log) = run_with_log(&on_grid(&nus));
    assert_eq!(
        loss_lines(&log),
        vec!["181 packet(s) before the stretch ended -- silence for those slots"],
        "issue 1439: the outage's silent slots (a second of them) on one line:\n{log}"
    );
}
