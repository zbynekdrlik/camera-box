//! #68 Task C: discard a LEADING window so a post-reset genlock-FIFO prime cannot
//! mask steady-state loss. The harness re-points the OBS receiver right before a
//! run; that transiently primes the FIFO and the first seconds can look cleaner
//! than steady state. Trimming the first N seconds (measured from the run's
//! earliest tap recv) leaves only the steady-state window for the loss check.

use camera_box::probe::analyzer::Observed;
use camera_box::probe::differ::{earliest_recv_ns, lead_cutoff_ns, trim_after};

fn o(frame_id: u32, recv_ms: i64) -> Observed {
    Observed {
        frame_id,
        gen_ts_ns: 0,
        recv_ts_ns: recv_ms * 1_000_000,
    }
}

#[test]
fn earliest_recv_is_the_min_across_taps() {
    let a = vec![o(0, 500), o(1, 600)];
    let b = vec![o(0, 300), o(1, 700)]; // 300 ms is the global earliest
    let c: Vec<Observed> = vec![];
    assert_eq!(
        earliest_recv_ns(&[&a, &b, &c]),
        Some(300 * 1_000_000)
    );
}

#[test]
fn earliest_recv_none_when_all_empty() {
    let e: Vec<Observed> = vec![];
    assert_eq!(earliest_recv_ns(&[&e, &e]), None);
}

#[test]
fn lead_cutoff_adds_the_discard_window() {
    // earliest 300 ms + 2 s discard = 2300 ms cutoff (in ns).
    assert_eq!(
        lead_cutoff_ns(300 * 1_000_000, 2000),
        2300 * 1_000_000
    );
}

#[test]
fn trim_after_keeps_only_steady_state() {
    // discard everything received before the cutoff (the post-reset prime window).
    let obs = vec![o(0, 100), o(1, 1900), o(2, 2400), o(3, 5000)];
    let kept = trim_after(&obs, 2300 * 1_000_000);
    let ids: Vec<u32> = kept.iter().map(|o| o.frame_id).collect();
    assert_eq!(ids, vec![2, 3]); // 0,1 (before 2300 ms) dropped
}

#[test]
fn trim_after_at_exact_cutoff_is_kept() {
    // recv exactly at the cutoff is steady-state (inclusive `>=`).
    let obs = vec![o(5, 2300)];
    let kept = trim_after(&obs, 2300 * 1_000_000);
    assert_eq!(kept.len(), 1);
}

#[test]
fn trim_after_zero_cutoff_keeps_all() {
    let obs = vec![o(0, 0), o(1, 100)];
    assert_eq!(trim_after(&obs, 0).len(), 2);
}
