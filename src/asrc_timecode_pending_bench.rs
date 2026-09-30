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
//!
//! ## Slice 3 (design 5902870861, ROZHODNUTÉ 5902983227)
//!
//! A sender-first step of ONE slot (N = +1, +35 … +66 ms) on every 100 ns grid position (its stamp
//! jump is one packet + 34 ns or − 66 ns), the receiver 0.5 s and 3 s later: the same acceptance as
//! above. Never followed: one `timeout`, J applied once. A skipped block, a duplicated block and an
//! N = −1 relabel: today's path byte for byte on every position.

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
        step_offset_ns: 0,
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
            step_offset_ns: 0,
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
            step_offset_ns: 0,
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
        step_offset_ns: 0,
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

#[test]
fn a_pause_inside_the_pending_window_keeps_the_pending_resolvable_1381() {
    // review round 1: the pending reused the skew hold's fold, so a 500 ms pause inside the pending
    // window moved the held offset by the pause; this box's step then missed it and the pending ran
    // to the bound, placing ~484 ms. Now only a relabel-shaped jump moves it: the pause takes the
    // stock path (placed at its stamp: the sender's own 500 ms of silence, the queued audio runs out
    // and the rest is a zero-filled gap -- which also proves the losses are counted from the sender's
    // step on), and this box's own step still resolves the pending with the remainder slewed.
    let pause_ms = RELABEL_PAUSE_NS as f64 / 1e6;
    for wall_ns in [STEP_682_NS, 260_000_000] {
        let case = StepCase {
            wall_ns,
            lag_ns: 0,
            follow: Follow::RelabelPause,
            sender_first_ns: RECEIVER_LAGS[1],
            burst_jitter_ns: 0,
            connect_backlog: 0,
            step_offset_ns: 0,
        };
        let r = run_step(case, Variant::Production);
        let rem = remainder_ms(wall_ns);
        assert!(
            r.pendings == 1
                && r.relabels == 1
                && r.releases == [(r.releases[0].0, AudioStepRelease::RelabelPending)]
                && r.releases[0].0 < 0.1
                && !r.holding_at_end
                && r.overwritten_ms == 0.0
                && r.dropped_ms == 0.0
                && r.gap_ms > 0.0
                && r.gap_ms <= pause_ms
                && r.jumps == 0
                && r.av_max_all_ms <= rem + AV_BOUND_MS
                && r.av_settle_s <= rem + REPAY_MARGIN_S
                && r.av_tail_ms <= AV_BOUND_MS,
            "issue 1381: {case:?}: a pause inside the pending window must keep the pending \
             resolvable at the receiver's step (only the sender's own {pause_ms} ms of silence): \
             {r:?}"
        );
    }
}

#[test]
fn without_the_hold_a_sender_first_relabel_loses_audio_at_the_sender_step_1381() {
    // anti-tautology (review round 1): the zero-loss assertions above count from the SENDER's step
    // on, so the same relabels without the hold (and so without the pending relabel it starts) must
    // show a loss there -- placed N slots late (+260 / +682 ms: 233 / 667 ms zero-filled), dropped
    // (-1.5 s) or reset (+2.5 s). `NoRelabel` is no such check: it keeps the pure hold, whose pending
    // start alone keeps a jump under 2 s appended (only the +2.5 s timeline reset loses there).
    for wall_ns in RELABEL_STEPS {
        let case = sender_first(wall_ns, RECEIVER_LAGS[0], 0);
        let without = run_step(case, Variant::NoStepHold);
        let with = run_step(case, Variant::Production);
        let lost = without.overwritten_ms + without.dropped_ms + without.gap_ms;
        assert!(
            without.pendings == 0
                && lost > 100.0
                && with.overwritten_ms + with.dropped_ms + with.gap_ms == 0.0,
            "issue 1381: {case:?}: without the hold a sender-first relabel must lose audio from the \
             sender's step on (the bench would otherwise prove nothing): {without:?} vs {with:?}"
        );
    }
}

#[test]
fn a_raw_clock_senders_late_step_packet_leaves_no_residual_1381() {
    // review round 2: a raw-clock sender (its stamps are its wall at emit) whose box steps first
    // submits the step-carrying packet 8 ms late -- its stamp and its arrival. The pending starts on
    // S + 8 ms and the next on-time packet moves the stamps back by -8 ms: that sub-packet move folds
    // back like the skew hold's, so this box's own step resolves the pending with no residual. Left
    // in the held offset, the +8 ms was booked on the slew at the release and then sat under the
    // timecode ASRC's booking band for the level loop (minutes).
    for wall_ns in [STEP_682_NS, -STEP_682_NS, STEP_90_NS] {
        for lag in RECEIVER_LAGS {
            let case = StepCase {
                wall_ns,
                lag_ns: 0,
                follow: Follow::JumpLate,
                sender_first_ns: lag,
                burst_jitter_ns: 0,
                connect_backlog: 0,
                step_offset_ns: 0,
            };
            let r = run_step(case, Variant::Production);
            assert!(
                r.pendings == 1
                    && r.relabels == 1
                    && r.releases == [(r.releases[0].0, AudioStepRelease::RelabelPending)]
                    && r.releases[0].0 < 0.1
                    && !r.holding_at_end
                    && r.jumps == 0
                    && r.overwritten_ms == 0.0
                    && r.dropped_ms == 0.0
                    && r.gap_ms == 0.0
                    && r.av_settle_s <= 1.0
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: a late step-carrying packet's lateness must fold back, not \
                 stay in the held offset as a residual: {r:?}"
            );
        }
    }
}

// Slice 3 (design 5902870861, ROZHODNUTÉ 5902983227): a sender-first step of ONE slot (N = +1).

/// The one-slot sender-first steps: S from just over one slot to just under two (N = +1).
const ONE_SLOT_STEPS_MS: [i64; 5] = [35, 40, 50, 60, 66];

/// The case with both steps moved onto 100 ns grid position `position` (0..3).
fn on_grid_position(mut case: StepCase, position: u64) -> StepCase {
    case.step_offset_ns = slot_ns(position);
    case
}

/// The stamp jump J the case's sender makes at its relabel: the one packet whose stamp moved more
/// than 1 ms against its continuation.
fn relabel_stamp_jump(case: StepCase) -> i64 {
    let packet = frames_ns(PACKET_FRAMES);
    let jumps: Vec<i64> = step_sender_packets(case)
        .windows(2)
        .map(|w| w[1].stamp.wrapping_sub(w[0].stamp + packet) as i64)
        .filter(|j| j.unsigned_abs() > 1_000_000)
        .collect();
    assert_eq!(
        jumps.len(),
        1,
        "{case:?}: exactly one relabelled stamp jump: {jumps:?}"
    );
    jumps[0]
}

#[test]
fn a_one_slot_relabel_jumps_one_packet_minus_66_ns_on_one_grid_position_1381() {
    // the premise (ROZHODNUTÉ 5902983227): the sender stamps in 100 ns units, so its 30 fps slots are
    // 33 333 300 / 33 333 300 / 33 333 400 ns, and an N = +1 relabel's stamp jump (two slots minus
    // the 33 333 333 ns packet) is one packet + 34 ns on two grid positions and one packet − 66 ns
    // on the third -- which neither "more than one packet" nor "one packet − 1 ns" catches
    let packet = frames_ns(PACKET_FRAMES) as i64;
    for s_ms in ONE_SLOT_STEPS_MS {
        let jumps: Vec<i64> = (0..3)
            .map(|position| {
                relabel_stamp_jump(on_grid_position(
                    sender_first(s_ms * 1_000_000, RECEIVER_LAGS[0], 0),
                    position,
                ))
            })
            .collect();
        assert_eq!(
            jumps,
            [packet + 34, packet - 66, packet + 34],
            "issue 1381: +{s_ms} ms: the N = +1 stamp jump on grid positions 0, 1, 2"
        );
    }
}

#[test]
fn a_one_slot_sender_first_step_is_appended_and_repaid_on_every_grid_position_1381() {
    // slice 3: a forward stamp jump of one packet (at least one packet − 100 ns, one NDI timecode
    // unit) with continuous arrival is a pending relabel and the age band is half a packet, so this
    // box's own step of −(one slot + r) resolves it. Measured on slice 2 (grid position 1): no
    // pending, the receiver's step released a zero-length hold with residual −S and PLACED the
    // packet, overwriting queued audio (+40 ms: 7.2 / 9.7 ms).
    for s_ms in ONE_SLOT_STEPS_MS {
        let wall_ns = s_ms * 1_000_000;
        let rem = remainder_ms(wall_ns);
        for position in 0..3 {
            for lag in RECEIVER_LAGS {
                let case = on_grid_position(sender_first(wall_ns, lag, 0), position);
                let r = run_step(case, Variant::Production);
                assert!(
                    r.pendings == 1
                        && r.relabels == 1
                        && r.overwritten_ms == 0.0
                        && r.dropped_ms == 0.0
                        && r.gap_ms == 0.0
                        && r.discs.is_empty(),
                    "issue 1381: {case:?}: a one-slot sender-first step must be a pending relabel, \
                     appended -- never placed at the receiver's step: {r:?}"
                );
                assert!(
                    r.releases == [(r.releases[0].0, AudioStepRelease::RelabelPending)]
                        && r.releases[0].0 < 0.1
                        && !r.holding_at_end,
                    "issue 1381: {case:?}: the receiver's own step must resolve it: {r:?}"
                );
                assert!(
                    r.av_max_all_ms <= rem + AV_BOUND_MS
                        && r.jumps == 0
                        && r.av_settle_s <= rem + REPAY_MARGIN_S
                        && r.av_tail_ms <= AV_BOUND_MS
                        && r.est_max_ppm <= RATE_BOUND_PPM,
                    "issue 1381: {case:?}: at most r = {rem:.1} ms off, repaid within {:.1} s of \
                     the receiver's step: {r:?}",
                    rem + REPAY_MARGIN_S
                );
            }
        }
    }
}

#[test]
fn a_one_slot_sender_first_step_the_receiver_never_follows_applies_the_jump_once_1381() {
    // no receiver step within the run: the pending runs to the 10 s bound and releases `timeout`
    // with the whole jump J as its residual, applied ONCE. Over one packet (J = packet + 34 ns) the
    // release places the packet (a zero-filled gap of J, the skew hold's rule); within one packet
    // (J = packet − 66 ns) it appends and the timecode ASRC books J. Nothing is overwritten or
    // dropped either way.
    let packet = frames_ns(PACKET_FRAMES) as i64;
    let bound_s = AUDIO_STEP_HOLD_MAX_NS as f64 / 1e9;
    for s_ms in [40_i64, 60] {
        for position in 0..3 {
            let case = on_grid_position(
                sender_first(s_ms * 1_000_000, RECEIVER_NEVER_NS, 0),
                position,
            );
            let jump = relabel_stamp_jump(case);
            let r = run_step(case, Variant::Production);
            assert!(
                r.pendings == 1
                    && r.relabels == 0
                    && r.releases == [(r.releases[0].0, AudioStepRelease::Timeout)]
                    && (r.releases[0].0 - bound_s).abs() < 0.1
                    && r.overwritten_ms == 0.0
                    && r.dropped_ms == 0.0
                    && !r.holding_at_end
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: a one-slot pending no receiver step follows is released \
                 once at the bound: {r:?}"
            );
            let once = if jump > packet {
                r.discs.len() == 1 && r.jumps == 0
            } else {
                r.discs.is_empty() && r.jumps == 1 && r.gap_ms == 0.0
            };
            assert!(
                once,
                "issue 1381: {case:?}: J = {jump} ns applied exactly once (placed over one packet, \
                 booked within it): {r:?}"
            );
        }
    }
}

#[test]
fn a_skipped_or_duplicated_block_and_a_one_slot_backward_step_keep_their_path_byte_for_byte_1381() {
    // slice 3 widens the pending start only by a FORWARD jump of one packet (down to one packet −
    // 100 ns) with continuous arrival and by the age band (half a packet): a skipped block jumps its
    // stamps exactly like an N = +1 relabel but its arrival gaps by one block; a duplicated block,
    // and an N = −1 relabel (byte-identical to it: one packet back), jump BACKWARD by one packet,
    // which still needs more than one packet. None of them is a pending or a relabel on any grid
    // position, so the ingest runs exactly its path without them -- and, the slice-3 start being a
    // superset of slice 2's (the unit test
    // `the_slice_3_start_only_adds_the_one_slot_forward_jump_and_the_half_packet_age_1381`),
    // exactly the slice-2 path.
    let mut cases: Vec<StepCase> = Vec::new();
    for position in 0..3 {
        for follow in [Follow::SkipBlock, Follow::DupBlock] {
            cases.push(on_grid_position(
                StepCase {
                    wall_ns: 0,
                    lag_ns: 0,
                    follow,
                    sender_first_ns: 0,
                    burst_jitter_ns: 0,
                    connect_backlog: 0,
                    step_offset_ns: 0,
                },
                position,
            ));
        }
        for s_ms in [-10_i64, -20, -30] {
            for lag in RECEIVER_LAGS {
                cases.push(on_grid_position(
                    sender_first(s_ms * 1_000_000, lag, 0),
                    position,
                ));
            }
        }
    }
    for case in cases {
        let (prod, prod_trace) = run_step_traced(case, Variant::Production);
        let (before, before_trace) = run_step_traced(case, Variant::NoRelabel);
        assert!(
            prod.pendings == 0
                && prod.relabels == 0
                && prod_trace.len() > 1000
                && prod_trace == before_trace,
            "issue 1381: {case:?}: a skipped or duplicated block (or an N = −1 relabel) must take \
             today's path byte for byte: {prod:?} vs {before:?}"
        );
    }
}

#[test]
fn a_late_relabel_after_a_timed_out_hold_never_starts_a_pending_1381() {
    // review round 1 (slice 3): this box steps first and the relabelling sender follows only after
    // the skew hold's 10 s bound. Its late relabel moves the stamps' age back TOWARD the nominal (r
    // off on its first relabelled block); with r between half a packet and one packet the half-packet
    // band alone started a false pending there -- a second 10 s hold, then J applied once (a 33.3 ms
    // zero-filled gap at +50 … +66 ms). The one timeout of the receiver's own step is all there is.
    for s_ms in [50_i64, 55, 60, 66, 260] {
        for position in 0..3 {
            for lag in [12 * NS_PER_S, 20 * NS_PER_S] {
                let case = on_grid_position(
                    StepCase {
                        wall_ns: s_ms * 1_000_000,
                        lag_ns: lag,
                        follow: Follow::Relabel,
                        sender_first_ns: 0,
                        burst_jitter_ns: 0,
                        connect_backlog: 0,
                        step_offset_ns: 0,
                    },
                    position,
                );
                let r = run_step(case, Variant::Production);
                assert!(
                    r.pendings == 0
                        && r.releases.len() == 1
                        && r.releases[0].1 == AudioStepRelease::Timeout
                        && !r.holding_at_end,
                    "issue 1381: {case:?}: a late relabel after a timed-out hold is a follow, never \
                     a pending: {r:?}"
                );
                if s_ms < 67 {
                    assert!(
                        r.gap_ms == 0.0 && r.dropped_ms == 0.0,
                        "issue 1381: {case:?}: a one-slot late follow opens no gap: {r:?}"
                    );
                }
            }
        }
    }
}
