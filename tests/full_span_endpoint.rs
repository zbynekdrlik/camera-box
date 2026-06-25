//! #7 Phase 3: source→endpoint full-span aggregate + ABSOLUTE end-to-end latency.
//!
//! These exercise the pure logic the full-path gate adds on top of Phase 2's
//! adjacent-hop differencing:
//!   * `full_span_diff` — a SINGLE source-tap vs endpoint-tap aggregate (the
//!     headline "every source frame reached the last endpoint" number), not a sum
//!     of adjacent-pair diffs. It delegates to `diff_hop`, so it obeys the SAME
//!     contract as the per-hop gates: strict zero-loss by default, the documented
//!     single-copy bound (`max_loss_pct`), the `min_frames` non-vacuous floor, and
//!     the #29 single-copy INCONCL guard.
//!   * `absolute_latency_stats` — `recv_ts(endpoint) − gen_ts(source)` paired by
//!     frame_id. Sound ONLY when both timestamps share one synced wall clock
//!     (DanteSync CLOCK_REALTIME, strih = master); the arithmetic itself is pure.
//!   * `absolute_latency_gate_pass` — the hard bound on the absolute end-to-end
//!     p99, mirroring the per-hop latency gate convention (None ⇒ report-only).
//!
//! Hardware-free: every Observed is constructed in the test, no NDI/QR/clock.

#![cfg(feature = "probe")]

use camera_box::probe::analyzer::{LatencyStats, Observed};
use camera_box::probe::differ::{
    absolute_latency_gate_pass, absolute_latency_stats, diff_hop, full_span_diff, overall_verdict,
    FullSpanBounds, HopInput, HopReport, HopVerdict,
};

/// Source observation: a painted frame at `gen_ms` on the synced wall clock.
/// `recv_ts_ns` at the SOURCE tap is irrelevant to absolute latency (we pair the
/// endpoint's recv against the source's gen), set it equal to gen for realism.
fn src(frame_id: u32, gen_ms: i64) -> Observed {
    Observed {
        frame_id,
        gen_ts_ns: gen_ms * 1_000_000,
        recv_ts_ns: gen_ms * 1_000_000,
        node_emit_tc_ns: 0,
    }
}

/// Endpoint observation: frame `frame_id` arrived at the endpoint tap at
/// `recv_ms` on the same synced wall clock. `gen_ts_ns` carries the SOURCE's
/// emission stamp end-to-end through the QR payload (unchanged across hops).
fn ep(frame_id: u32, gen_ms: i64, recv_ms: i64) -> Observed {
    Observed {
        frame_id,
        gen_ts_ns: gen_ms * 1_000_000,
        recv_ts_ns: recv_ms * 1_000_000,
        node_emit_tc_ns: 0,
    }
}

/// Strict zero-loss bounds (the default): no documented loss budget, a low
/// `min_frames` floor so small fixtures are not vacuous, no INCONCL guard.
fn strict() -> FullSpanBounds {
    FullSpanBounds {
        min_frames: 1,
        max_loss_pct: None,
        min_single_copy: 0,
    }
}

// ---------------------------------------------------------------------------
// full_span_diff — source→endpoint aggregate (strict default)
// ---------------------------------------------------------------------------

#[test]
fn full_span_passes_when_every_source_id_reaches_endpoint() {
    let source = vec![src(0, 0), src(1, 33), src(2, 66), src(3, 99), src(4, 132)];
    let endpoint = vec![
        ep(0, 0, 120),
        ep(1, 33, 153),
        ep(2, 66, 186),
        ep(3, 99, 219),
        ep(4, 132, 252),
    ];
    let r = full_span_diff(&source, &endpoint, &strict());
    assert_eq!(r.source_unique, 5);
    assert_eq!(r.endpoint_unique, 5);
    assert!(
        r.dropped_ids.is_empty(),
        "no source id should be missing at the endpoint: {:?}",
        r.dropped_ids
    );
    assert_eq!(
        r.verdict,
        HopVerdict::Pass,
        "a complete chain must certify PASS"
    );
}

#[test]
fn full_span_fails_on_a_mid_stream_drop_against_the_endpoint() {
    // Source emits 0..=4; the endpoint never carries id 2 -> headline source→
    // endpoint drop, regardless of which intermediate hop lost it.
    let source = vec![src(0, 0), src(1, 33), src(2, 66), src(3, 99), src(4, 132)];
    let endpoint = vec![
        ep(0, 0, 120),
        ep(1, 33, 153),
        ep(3, 99, 219),
        ep(4, 132, 252),
    ];
    let r = full_span_diff(&source, &endpoint, &strict());
    assert_eq!(
        r.dropped_ids,
        vec![2],
        "id 2 must be flagged source→endpoint"
    );
    assert_eq!(
        r.verdict,
        HopVerdict::Fail,
        "a strict source→endpoint drop must FAIL"
    );
}

#[test]
fn full_span_clips_to_endpoint_active_span_not_tap_startup_skew() {
    // The endpoint tap connects late (first sees id 2) and stops early (last id
    // 4): source ids 0,1 (before its first) and 6,7 (after its last) are tap
    // start/stop skew, NOT hop drops — exactly the diff_hop span-clip semantics.
    // Only an id INSIDE [2,4] that is absent counts as a real drop.
    let source = vec![
        src(0, 0),
        src(1, 33),
        src(2, 66),
        src(3, 99),
        src(4, 132),
        src(6, 198),
        src(7, 231),
    ];
    let endpoint = vec![ep(2, 66, 186), ep(4, 132, 252)]; // missing 3, inside span
    let r = full_span_diff(&source, &endpoint, &strict());
    assert_eq!(
        r.dropped_ids,
        vec![3],
        "only id 3 (inside the endpoint active span) is a real drop; 0,1,6,7 are skew"
    );
    assert_eq!(r.verdict, HopVerdict::Fail);
}

#[test]
fn full_span_empty_endpoint_is_not_a_vacuous_pass() {
    // An endpoint that decoded nothing must NOT certify: there is no evidence any
    // frame arrived. (min_frames floor catches it, like every other hop.)
    let source = vec![src(0, 0), src(1, 33)];
    let endpoint: Vec<Observed> = vec![];
    let r = full_span_diff(&source, &endpoint, &strict());
    assert_eq!(r.endpoint_unique, 0);
    assert_ne!(
        r.verdict,
        HopVerdict::Pass,
        "an endpoint with zero decoded frames must not pass as zero-loss"
    );
}

#[test]
fn full_span_min_frames_floor_rejects_a_one_frame_endpoint() {
    // A near-dead endpoint that decoded a single in-span frame must NOT certify
    // ZERO-LOSS (the #2 review finding): with min_frames=100, one endpoint frame
    // is below the floor and the full span FAILs rather than lying clean.
    let source: Vec<Observed> = (0..200).map(|i| src(i, i as i64 * 33)).collect();
    let endpoint = vec![ep(7, 231, 351)]; // one frame, span [7,7], no in-span drop
    let bounds = FullSpanBounds {
        min_frames: 100,
        max_loss_pct: None,
        min_single_copy: 0,
    };
    let r = full_span_diff(&source, &endpoint, &bounds);
    assert!(
        r.dropped_ids.is_empty(),
        "no id is missing within the 1-wide span"
    );
    assert_ne!(
        r.verdict,
        HopVerdict::Pass,
        "a 1-frame endpoint is below min_frames and must NOT certify ZERO-LOSS"
    );
}

#[test]
fn full_span_honours_the_documented_loss_bound_not_strict_zero() {
    // The #1 review finding (verdict regression): with a documented per-hop loss
    // budget set on the endpoint, a small loss within the budget must PASS the
    // full span — a strict full-span gate must NOT override the deliberately
    // relaxed budget. 100 single-copy source ids, the endpoint drops 3 (3%) ->
    // under a 10% documented bound -> PASS; strict (None) would FAIL.
    let source: Vec<Observed> = (0..100).map(|i| src(i, i as i64 * 33)).collect();
    // Endpoint carries every id EXCEPT 10, 20, 30 (3 drops), spanning [0,99].
    let endpoint: Vec<Observed> = (0..100)
        .filter(|i| ![10u32, 20, 30].contains(i))
        .map(|i| ep(i, i as i64 * 33, i as i64 * 33 + 120))
        .collect();

    let bounded = FullSpanBounds {
        min_frames: 1,
        max_loss_pct: Some(10.0),
        min_single_copy: 0,
    };
    let r = full_span_diff(&source, &endpoint, &bounded);
    assert_eq!(r.single_copy_total, 100);
    assert_eq!(r.single_copy_dropped, 3);
    assert_eq!(
        r.verdict,
        HopVerdict::Pass,
        "3% loss under a 10% documented bound must PASS (no strict override)"
    );

    // Same data, strict (no documented bound) -> the 3 drops FAIL the run.
    let r_strict = full_span_diff(&source, &endpoint, &strict());
    assert_eq!(r_strict.verdict, HopVerdict::Fail);
}

#[test]
fn full_span_inconclusive_when_too_few_single_copy_frames() {
    // The #29 oversample guard applies end-to-end too: a clean run with fewer
    // single-copy source→endpoint frames than the guard cannot be CERTIFIED.
    // Three unique ids, each carried by ONE source frame and present downstream
    // (zero loss), but min_single_copy=10 -> not enough evidence -> INCONCL.
    let source = vec![src(0, 0), src(1, 33), src(2, 66)];
    let endpoint = vec![ep(0, 0, 120), ep(1, 33, 153), ep(2, 66, 186)];
    let bounds = FullSpanBounds {
        min_frames: 1,
        max_loss_pct: None,
        min_single_copy: 10,
    };
    let r = full_span_diff(&source, &endpoint, &bounds);
    assert!(r.dropped_ids.is_empty());
    assert_eq!(
        r.verdict,
        HopVerdict::Inconclusive,
        "zero loss but too few single-copy frames must be INCONCL, not PASS"
    );
}

// ---------------------------------------------------------------------------
// overall_verdict — the central pass/fail fold (the #important review finding)
// ---------------------------------------------------------------------------

/// Build a real `HopReport` with the requested verdict via `diff_hop` (no
/// hand-constructed structs): clean inputs → Pass; an in-span drop → Fail; clean
/// but below the single-copy guard → Inconclusive.
fn hop(verdict: HopVerdict) -> HopReport {
    let up = vec![src(0, 0), src(1, 33), src(2, 66)];
    let (down, max_loss_pct, min_single_copy) = match verdict {
        HopVerdict::Pass => (
            vec![ep(0, 0, 10), ep(1, 33, 43), ep(2, 66, 76)],
            None,
            0usize,
        ),
        HopVerdict::Fail => (vec![ep(0, 0, 10), ep(2, 66, 76)], None, 0), // id 1 dropped in span
        HopVerdict::Inconclusive => {
            (vec![ep(0, 0, 10), ep(1, 33, 43), ep(2, 66, 76)], None, 10) // clean but < guard
        }
    };
    let r = diff_hop(HopInput {
        name: "h".to_string(),
        upstream: &up,
        downstream: &down,
        capture_fps: 30.0,
        freeze_periods: f64::MAX,
        min_frames: 1,
        max_p99_latency_ms: None,
        max_freeze_periods_gate: None,
        max_loss_pct,
        min_single_copy,
    });
    assert_eq!(
        r.verdict, verdict,
        "fixture must yield the requested verdict"
    );
    r
}

/// A full-span report with the requested verdict, built the same way.
fn span(verdict: HopVerdict) -> camera_box::probe::differ::FullSpanReport {
    let source = vec![src(0, 0), src(1, 33), src(2, 66)];
    let (endpoint, bounds) = match verdict {
        HopVerdict::Pass => (vec![ep(0, 0, 10), ep(1, 33, 43), ep(2, 66, 76)], strict()),
        HopVerdict::Fail => (vec![ep(0, 0, 10), ep(2, 66, 76)], strict()),
        HopVerdict::Inconclusive => (
            vec![ep(0, 0, 10), ep(1, 33, 43), ep(2, 66, 76)],
            FullSpanBounds {
                min_frames: 1,
                max_loss_pct: None,
                min_single_copy: 10,
            },
        ),
    };
    let r = full_span_diff(&source, &endpoint, &bounds);
    assert_eq!(r.verdict, verdict);
    r
}

#[test]
fn overall_pass_only_when_every_gate_passes() {
    assert_eq!(
        overall_verdict(&[hop(HopVerdict::Pass)], &span(HopVerdict::Pass), true),
        HopVerdict::Pass
    );
}

#[test]
fn overall_fails_on_a_hop_fail() {
    assert_eq!(
        overall_verdict(
            &[hop(HopVerdict::Pass), hop(HopVerdict::Fail)],
            &span(HopVerdict::Pass),
            true
        ),
        HopVerdict::Fail
    );
}

#[test]
fn overall_fails_on_full_span_loss_even_when_all_hops_pass() {
    // The exact round-1 regression class: hops clean, but the source→endpoint
    // full span lost a frame ⇒ the run must FAIL, not PASS.
    assert_eq!(
        overall_verdict(&[hop(HopVerdict::Pass)], &span(HopVerdict::Fail), true),
        HopVerdict::Fail
    );
}

#[test]
fn overall_fails_when_absolute_latency_gate_fails() {
    assert_eq!(
        overall_verdict(&[hop(HopVerdict::Pass)], &span(HopVerdict::Pass), false),
        HopVerdict::Fail
    );
}

#[test]
fn overall_inconclusive_when_only_inconcl_no_hard_fail() {
    // A hop INCONCL (too few single-copy frames) with no hard FAIL anywhere ⇒
    // INCONCL, not FAIL: "need a longer/denser run", not "the pipeline broke".
    assert_eq!(
        overall_verdict(
            &[hop(HopVerdict::Inconclusive)],
            &span(HopVerdict::Pass),
            true
        ),
        HopVerdict::Inconclusive
    );
}

#[test]
fn overall_hard_fail_outranks_inconclusive() {
    // A real FAIL alongside an INCONCL is reported as FAIL (the worse signal).
    assert_eq!(
        overall_verdict(
            &[hop(HopVerdict::Inconclusive), hop(HopVerdict::Fail)],
            &span(HopVerdict::Pass),
            true
        ),
        HopVerdict::Fail
    );
}

// ---------------------------------------------------------------------------
// absolute_latency_stats — recv(endpoint) − gen(source), wall-clock paired
// ---------------------------------------------------------------------------

#[test]
fn absolute_latency_is_endpoint_recv_minus_source_gen() {
    // Each frame: gen at source wall-clock T, arrives at endpoint T + 120 ms.
    // Absolute end-to-end latency must be exactly 120 ms (NOT a per-hop delta).
    let source = vec![src(0, 0), src(1, 33), src(2, 66)];
    let endpoint = vec![ep(0, 0, 120), ep(1, 33, 153), ep(2, 66, 186)];
    let s = absolute_latency_stats(&source, &endpoint).expect("samples exist");
    assert_eq!(s.samples, 3);
    assert!((s.p50_ms - 120.0).abs() < 1e-6, "p50 was {}", s.p50_ms);
    assert!((s.min_ms - 120.0).abs() < 1e-6);
    assert!((s.max_ms - 120.0).abs() < 1e-6);
}

#[test]
fn absolute_latency_pairs_by_frame_id_uses_source_gen_not_endpoint_gen() {
    // The endpoint Observed also carries gen_ts (propagated through the QR), but
    // the pairing must take the SOURCE tap's gen for the frame and the ENDPOINT
    // tap's recv. Here the source genuinely emitted id 5 at 50 ms; the endpoint
    // saw it at 230 ms -> 180 ms absolute. An id present only at one tap is
    // dropped from the latency set (no pair).
    let source = vec![src(5, 50), src(6, 83)];
    let endpoint = vec![ep(5, 50, 230)]; // id 6 never reached endpoint
    let s = absolute_latency_stats(&source, &endpoint).expect("one pair");
    assert_eq!(s.samples, 1, "only id 5 is paired");
    assert!((s.p50_ms - 180.0).abs() < 1e-6, "p50 was {}", s.p50_ms);
}

#[test]
fn absolute_latency_none_when_no_common_ids() {
    let source = vec![src(0, 0)];
    let endpoint = vec![ep(9, 0, 120)];
    assert!(absolute_latency_stats(&source, &endpoint).is_none());
}

// ---------------------------------------------------------------------------
// absolute_latency_gate_pass — hard bound on the end-to-end p99
// ---------------------------------------------------------------------------

#[test]
fn absolute_latency_gate_none_is_report_only() {
    let source = vec![src(0, 0), src(1, 33)];
    let endpoint = vec![ep(0, 0, 500), ep(1, 33, 533)]; // 500 ms — huge
    let s = absolute_latency_stats(&source, &endpoint);
    assert!(
        absolute_latency_gate_pass(&s, None),
        "no bound ⇒ report-only ⇒ always passes the gate"
    );
}

#[test]
fn absolute_latency_gate_fails_when_p99_exceeds_bound() {
    // Four frames at 120 ms, one at 600 ms -> p99 (nearest-rank, n=5) = 600 ms.
    let source = vec![src(0, 0), src(1, 33), src(2, 66), src(3, 99), src(4, 132)];
    let endpoint = vec![
        ep(0, 0, 120),
        ep(1, 33, 153),
        ep(2, 66, 186),
        ep(3, 99, 219),
        ep(4, 132, 732), // 600 ms
    ];
    let s = absolute_latency_stats(&source, &endpoint);
    assert!(
        !absolute_latency_gate_pass(&s, Some(350.0)),
        "p99 600 ms must fail a 350 ms bound"
    );
    assert!(
        absolute_latency_gate_pass(&s, Some(650.0)),
        "p99 600 ms must pass a 650 ms bound"
    );
}

#[test]
fn absolute_latency_gate_fails_when_no_samples_but_bound_set() {
    // A bound was requested but no end-to-end pair exists -> the gate cannot be
    // satisfied; it must FAIL, never vacuously pass (test-strictness: a gate that
    // could not run must not report green).
    let none: Option<LatencyStats> = None;
    assert!(
        !absolute_latency_gate_pass(&none, Some(350.0)),
        "a requested bound with zero samples must fail, not pass vacuously"
    );
}

#[test]
fn absolute_latency_gate_passes_at_exactly_zero_latency() {
    // A zero (recv == gen) latency is valid/possible — only a NEGATIVE one is
    // impossible. The gate must use strict `< 0.0` (not `<= 0.0`): a min of
    // exactly 0 ms PASSES under a positive bound. Pins the `<` vs `<=` boundary.
    let source = vec![src(0, 0), src(1, 33)];
    let endpoint = vec![ep(0, 0, 0), ep(1, 33, 33)]; // recv == gen ⇒ 0 ms each
    let s = absolute_latency_stats(&source, &endpoint);
    assert_eq!(s.as_ref().unwrap().min_ms, 0.0, "fixture min is exactly 0");
    assert!(
        absolute_latency_gate_pass(&s, Some(350.0)),
        "an exactly-zero latency is valid and must PASS (only negative is impossible)"
    );
}

#[test]
fn absolute_latency_gate_fails_on_negative_latency_even_under_bound() {
    // A negative min (recv before gen) is physically impossible = cluster clock
    // desync; the measurement is untrustworthy so the gate must FAIL even though
    // p99 sits under the bound. Backstop for a probe run without the e2e
    // clock-offset pre-flight (#7/#8).
    let source = vec![src(0, 0), src(1, 33), src(2, 66)];
    // id 0 arrives "before" it was generated (−20 ms) due to a +offset on the
    // camera clock; the rest look fine and p99 stays small.
    let endpoint = vec![ep(0, 0, -20), ep(1, 33, 53), ep(2, 66, 86)];
    let s = absolute_latency_stats(&source, &endpoint);
    assert!(
        s.as_ref().unwrap().min_ms < 0.0,
        "fixture has a negative min"
    );
    assert!(
        !absolute_latency_gate_pass(&s, Some(350.0)),
        "a negative (impossible) latency must FAIL the gate, not pass under the bound"
    );
}
