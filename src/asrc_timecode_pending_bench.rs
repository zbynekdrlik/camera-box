//! Issue 1381 (design 5901213031, receiver slice 2) — the two-clock bench of a PENDING relabel and of
//! the relabel remainder repaid on the placement slew. A test-only child of
//! `asrc_timecode_step_bench` (declared there with `#[path]`), so it drives the same OBS ingest replay
//! (`Obs`) with the same senders.
//!
//! ## What it reproduces
//!
//! - **Sender first.** A cross-box sender (the cg OBS feed into strih-lx) whose box steps its date
//!   BEFORE the receiver's: the contract's relabel moves its stamps N slots while this box's live
//!   offset has not moved. On the slice-1 path that packet took the stock OBS path by size (placed
//!   N slots late with a zero-filled gap from 70 ms to 2 s, the whole buffer dropped over 2 s), and
//!   the receiver's later step was placed a second time by the skew hold (measured on 7e76d8262:
//!   +682 ms, sender first by 0.5 s = a 666.7 ms gap then 682.5 ms overwritten; ticket comment
//!   5901349351).
//! - **The remainder.** A relabel lands r = S − N·slot earlier than its continuation. The slice-1
//!   path left a remainder under the timecode ASRC's half-packet band to the level loop (15.8 ms of
//!   the +682 ms step: back within 2 ms only at +725 s).
//!
//! ## Acceptance (design 5901213031)
//!
//! +260 ms, +682 ms, −1.5 s, +2.5 s with the receiver stepping 0.5 s and 3 s after the sender: zero
//! audio overwritten, dropped or zero-filled, no departure from the continuation, one
//! `relabel-pending` release at the receiver's step, nothing booked as a placement jump, and the
//! remainder repaid within r seconds + 1 s. A receiver that never steps: released once at the
//! 10 s bound, J applied once. A sender pause or restart: today's path byte for byte.

use super::*;

/// The receiver's step after the sender's: 0.5 s and 3 s (both inside the 10 s bound).
const RECEIVER_LAGS: [u64; 2] = [500_000_000, 3 * NS_PER_S];

fn sender_first(wall_ns: i64, lag_ns: u64, burst_jitter_ns: u64) -> StepCase {
    StepCase {
        wall_ns,
        lag_ns: 0,
        follow: Follow::Relabel,
        sender_first_ns: lag_ns,
        burst_jitter_ns,
        connect_backlog: 0,
    }
}

/// The contract's slot jump N·slot of a step, ms.
fn jump_ms(wall_ns: i64) -> f64 {
    relabel_slots(wall_ns) as f64 * PACKET_FRAMES as f64 * NS_PER_S as f64 / RATE as f64 / 1e6
}

#[test]
fn a_sender_first_relabel_is_appended_and_its_remainder_repaid_at_the_receiver_step_1381() {
    let mut cases: Vec<StepCase> = Vec::new();
    for wall_ns in RELABEL_STEPS {
        for lag in RECEIVER_LAGS {
            cases.push(sender_first(wall_ns, lag, 0));
        }
    }
    // 10 ms of extra arrival jitter stays inside the one-packet + 15 ms arrival bound
    cases.push(sender_first(STEP_682_NS, RECEIVER_LAGS[0], 10_000_000));
    for case in cases {
        let r = run_step(case, Variant::Production);
        let rem = remainder_ms(case.wall_ns);
        // zero samples overwritten, dropped or zero-filled from the SENDER's step on: every packet
        // appended back to back, through the pending window and the receiver's step
        assert!(
            r.pendings == 1
                && r.relabels == 1
                && r.overwritten_ms == 0.0
                && r.dropped_ms == 0.0
                && r.gap_ms == 0.0
                && r.discs.is_empty(),
            "issue 1381: {case:?}: a relabel whose sender stepped first must be appended -- never \
             placed N slots late, never reset past 2 s, never placed again at the receiver's step: \
             {r:?}"
        );
        // one release, the pending one, on the receiver's step packet; nothing held after it
        assert!(
            r.releases == [(r.releases[0].0, AudioStepRelease::RelabelPending)]
                && r.releases[0].0 < 0.1
                && !r.holding_at_end,
            "issue 1381: {case:?}: the receiver's own step must resolve the pending relabel: {r:?}"
        );
        // the audio trails its true landing by at most r (through the pending window too), and the
        // placement slew repays r at 1000 ppm: nothing booked as a placement jump
        assert!(
            r.av_max_all_ms <= rem + AV_BOUND_MS
                && r.jumps == 0
                && r.av_settle_s <= rem + REPAY_MARGIN_S
                && r.av_tail_ms <= AV_BOUND_MS
                && r.est_max_ppm <= RATE_BOUND_PPM,
            "issue 1381: {case:?}: at most r = {rem:.1} ms off, repaid within {:.1} s of the \
             receiver's step, the rate estimate untouched: {r:?}",
            rem + REPAY_MARGIN_S
        );
    }
}

#[test]
fn a_sender_first_relabel_the_receiver_never_follows_is_placed_once_at_the_bound_1381() {
    // the receiver never steps: the pending relabel runs to AUDIO_STEP_HOLD_MAX_NS and the timeout
    // applies J once (a placement at the packet's raw-stamp landing), like the skew hold's timeout
    let bound_s = AUDIO_STEP_HOLD_MAX_NS as f64 / 1e9;
    for wall_ns in RELABEL_STEPS {
        let case = sender_first(wall_ns, RECEIVER_NEVER_NS, 0);
        let r = run_step(case, Variant::Production);
        assert!(
            r.pendings == 1
                && r.relabels == 0
                && r.releases == [(r.releases[0].0, AudioStepRelease::Timeout)]
                && (r.releases[0].0 - bound_s).abs() < 0.1
                && r.discs.len() == 1
                && r.jumps == 0
                && !r.holding_at_end,
            "issue 1381: {case:?}: a pending relabel no receiver step follows is released once at \
             the bound, J ({:.1} ms) applied once: {r:?}",
            jump_ms(wall_ns)
        );
        // forward, the placement lands on the relabelled stamps (the video's landing); a backward
        // placement lies before the mix window (OBS resets the buffer there), so only forward is
        // held to the A/V bound
        if wall_ns > 0 {
            assert!(
                r.av_settle_s <= bound_s + 0.1 && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: on the relabelled landing once J is applied: {r:?}"
            );
        }
    }
}

#[test]
fn a_sender_pause_restart_or_catch_up_keeps_the_stock_path_byte_for_byte_1381() {
    // a pause or a restart: the stamps jump by the pause and so does the arrival -- no pending
    // relabel, no relabel, the ingest runs exactly the path without them. A catch-up sender that
    // stepped first (continuous stamps) likewise.
    let mut cases: Vec<StepCase> = [100_000_000_i64, 500_000_000, 3_000_000_000]
        .iter()
        .map(|&pause| StepCase {
            wall_ns: pause,
            lag_ns: 0,
            follow: Follow::Pause,
            sender_first_ns: 0,
            burst_jitter_ns: 0,
            connect_backlog: 0,
        })
        .collect();
    for wall_ns in [STEP_682_NS, -STEP_682_NS] {
        cases.push(StepCase {
            wall_ns,
            lag_ns: 0,
            follow: Follow::Burst,
            sender_first_ns: RECEIVER_LAGS[0],
            burst_jitter_ns: 0,
            connect_backlog: 0,
        });
    }
    for case in cases {
        let (prod, prod_trace) = run_step_traced(case, Variant::Production);
        let (before, before_trace) = run_step_traced(case, Variant::NoRelabel);
        assert!(
            prod.pendings == 0
                && prod.relabels == 0
                && prod_trace.len() > 1000
                && prod_trace == before_trace,
            "issue 1381: {case:?}: a sender whose stamps jump with an arrival gap (or never jump) \
             must take today's path byte for byte: {prod:?} vs {before:?}"
        );
    }
}

#[test]
fn without_the_slew_booking_a_sub_band_remainder_takes_minutes_1381() {
    // anti-tautology: the same relabels with the remainder left to the timecode ASRC (the slice-1
    // path). Under its half-packet band (the +682 ms step's 15.8 ms) only the level loop repays it,
    // in minutes; over it (the +260 ms step's 26.7 ms) it is booked as a placement jump.
    let joint = |wall_ns| StepCase {
        wall_ns,
        lag_ns: 0,
        follow: Follow::Relabel,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
        connect_backlog: 0,
    };
    for case in [
        joint(STEP_682_NS),
        sender_first(STEP_682_NS, RECEIVER_LAGS[0], 0),
    ] {
        let without = run_step(case, Variant::NoBook);
        let with = run_step(case, Variant::Production);
        let rem = remainder_ms(case.wall_ns);
        assert!(
            without.av_settle_s > 600.0 && with.av_settle_s <= rem + REPAY_MARGIN_S,
            "issue 1381: {case:?}: without the slew booking the {rem:.1} ms remainder must take \
             minutes (the bench would otherwise prove nothing): {without:?} vs {with:?}"
        );
    }
    let over = run_step(joint(260_000_000), Variant::NoBook);
    assert!(
        over.jumps == 1,
        "issue 1381: without the slew booking a remainder over the band is a booked placement \
         jump: {over:?}"
    );
}
