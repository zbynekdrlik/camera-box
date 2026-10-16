//! Issue 1425 round 2 (design 6206822792) — the acceptance rows of the STAMP basis under a
//! disturbed delivery, on the plant of `asrc_stamp_bench.rs` (a `#[path]` child of it).
//!
//! - **Delivery stall** of 8 / 12 / 15 / 20 / 30 / 60 / 120 ms at 700 s, 128- and 1024-sample
//!   packets. The driver stamps each packet "arrival minus its own duration", so the burst's stamps
//!   run up to S late while its samples append contiguously: a placement-error transient of about −S
//!   that returns by itself. Required (stalls under OBS's 70 ms `TS_SMOOTHING_THRESHOLD`): 0 booked
//!   jumps, the per-second A/V within 1 ms of its pre-stall mean, and within 0.5 ms from 10 s after.
//! - **A real 44 ms sample loss** at 700 s: booked ONCE within 1.5 s of the loss, repaid at
//!   1000 ppm, the A/V back within 1 ms of its pre-loss mean no later than today's arrival servo on
//!   the same feed + 2 s.
//! - **Equal-stamp bursts**: a quarter of the packets arrive together with the next one (one driver
//!   read, one arrival, equal stamps) at +20 ppm for 8 h: the minute A/V moves ≤ 0.6 ms once the
//!   level loop settled, no booked jump.
//!
//! **A stall of 70 ms or more is not a transient (measured, reported on the ticket).** The burst's
//! equal stamps stay snapped onto OBS's continuous timeline only while the burst is under 70 ms
//! behind; past that OBS PLACES a burst packet at its stamp and overwrites what it had queued, so the
//! samples after the stall land late on EVERY basis (120 ms: +45 ms on `mbc`, +32 ms on PulseAudio;
//! the arrival servo keeps that displacement). That is a real displacement, so the stamp basis books
//! it once and pays it back at 1000 ppm. The 120 ms row asserts exactly that: the displacement equals
//! the arrival servo's, one booking within 1.5 s, the A/V back within 1 ms in about one second per
//! millisecond and within 1.5 ms after that (the burst's compressed stamps stay in the rate
//! regression for its 600 s span: +1.03 ms on `mbc`), while the arrival servo keeps the whole
//! displacement.
//!
//! **Equal-stamp bursts move the A/V by p·d.** The first packet of a pair is stamped one packet (d)
//! late and reaches the level loop with an error of −d; the second, whose stamp is right, has no
//! stamp advance and so no rate point, and its reading never enters the window. The level loop then
//! settles the audio p·d late, p the pair probability (measured: 0.66 ms `mbc`, 5.52 ms PulseAudio
//! at p = 0.25; 1.34 / 11.15 ms at p = 0.5 in the review's scratch run). The row pins that offset
//! (so a change of mechanism shows) and reads the drift from 1800 s on, after the level loop's
//! ~500 s walk there. The arrival servo cannot hold this feed at all (it never locks: the bench's
//! `Arrival` run walks with the source rate). Removing the offset (correcting the early stamps of a
//! run, the same insight as the waiting rate point) is a design question left to the main.

use super::*;

const EVENT_S: f64 = 700.0;
const STALLS_MS: [f64; 7] = [8.0, 12.0, 15.0, 20.0, 30.0, 60.0, 120.0];
const LOSS_MS: f64 = 44.0;
/// Required: |A/V − pre-stall mean| ≤ this from the stall on.
const STALL_EXCURSION_MS: f64 = 1.0;
/// … and from 10 s after the stall, ≤ this.
const STALL_SETTLED_MS: f64 = 0.5;
const STALL_SETTLE_S: usize = 10;
/// A loss is booked within this many seconds of it.
const BOOKED_WITHIN_S: f64 = 1.5;
/// What the 1000 ppm payment (1 ms per second) can already have repaid within that time, ms.
const PAID_WITHIN_BOOKING_MS: f64 = 1.5;
/// Issue 1425 slice 3: the arrival servo holds the depth it locked at, and for a tick-long packet that
/// reading carries the packet-vs-tick phase (−6.5 … +9.8 ms over the four launch phases, the stamp
/// bench). Its stall displacement is compared within that phase on such a source (measured: pulse
/// 120 ms stall, 31.5 stamp vs 39.6 arrival).
const ARRIVAL_PHASE_MS: f64 = 10.0;
/// The A/V of a loss is back within 1 ms no later than the arrival servo's + this.
const LOSS_BACK_SLACK_S: usize = 2;
const BACK_MS: f64 = 1.0;
/// After a stall past the smoothing threshold is paid back, the rate regression still carries the
/// burst's compressed stamps for its 600 s span (measured: +1.03 ms on `mbc`, gone when they leave
/// the span); the A/V must stay within this.
const LATE_WALK_MS: f64 = 1.5;
const BURST_DRIFT_MS: f64 = 0.6;
/// The equal-stamp burst drift is read once the level loop settled (3.6 of its ~500 s constants).
const BURST_SETTLED_FROM_S: usize = 1800;

fn event_case(src: Source, servo: Servo, event: Event) -> Case {
    let mut case = Case::new(src, 20.0, 4, 1, servo);
    case.event = event;
    case
}

/// The mean per-second A/V over the 100 s before the event.
fn pre_mean_ms(r: &Run) -> f64 {
    let s = &r.av_s[600..EVENT_S as usize];
    s.iter().sum::<f64>() / s.len() as f64
}

/// The largest |A/V − pre-event mean| from second `from` on.
fn excursion_from_ms(r: &Run, from: usize) -> f64 {
    let pre = pre_mean_ms(r);
    r.av_s[from..]
        .iter()
        .fold(0.0_f64, |m, v| m.max((v - pre).abs()))
}

/// The first second from which every later second is within `bound_ms` of the pre-event mean.
fn back_within_s(r: &Run, bound_ms: f64) -> usize {
    let pre = pre_mean_ms(r);
    let last_out = r.av_s[EVENT_S as usize..]
        .iter()
        .rposition(|v| (v - pre).abs() > bound_ms);
    EVENT_S as usize + last_out.map_or(0, |i| i + 1)
}

/// The largest move of the minute-averaged A/V away from the first minute after `from`.
fn drift_from_ms(r: &Run, from: usize) -> f64 {
    let minutes: Vec<f64> = r.av_s[from..]
        .chunks(60)
        .filter(|c| c.len() == 60)
        .map(|c| c.iter().sum::<f64>() / 60.0)
        .collect();
    minutes
        .iter()
        .fold(0.0_f64, |m, v| m.max((v - minutes[0]).abs()))
}

#[test]
fn a_delivery_stall_books_nothing_and_keeps_av_1425() {
    let at = EVENT_S as usize;
    let smoothing_ms = TS_SMOOTHING_THRESHOLD as f64 / 1e6;
    for src in SOURCES {
        for ms in STALLS_MS.into_iter().filter(|&ms| ms < smoothing_ms) {
            let r = run(&event_case(
                src,
                Servo::Production,
                Event::Stall { at_s: EVENT_S, ms },
            ));
            let excursion = excursion_from_ms(&r, at);
            let settled = excursion_from_ms(&r, at + STALL_SETTLE_S);
            assert!(
                r.place_jumps == 0 && r.underruns == 0,
                "issue 1425: {} {ms} ms stall: the stamp basis must book no jump: jumps {} \
                 underruns {}",
                src.name,
                r.place_jumps,
                r.underruns
            );
            assert!(
                excursion <= STALL_EXCURSION_MS && settled <= STALL_SETTLED_MS,
                "issue 1425: {} {ms} ms stall: |A/V| must stay within {STALL_EXCURSION_MS} ms of \
                 its pre-stall mean and within {STALL_SETTLED_MS} ms from {STALL_SETTLE_S} s on: \
                 {excursion:.3} / {settled:.3} ms",
                src.name
            );
        }
    }
}

#[test]
fn a_stall_past_the_smoothing_threshold_is_booked_once_and_repaid_1425() {
    let at = EVENT_S as usize;
    let smoothing_ms = TS_SMOOTHING_THRESHOLD as f64 / 1e6;
    for src in SOURCES {
        for ms in STALLS_MS.into_iter().filter(|&ms| ms >= smoothing_ms) {
            let stall = Event::Stall { at_s: EVENT_S, ms };
            let r = run(&event_case(src, Servo::Production, stall));
            let a = run(&event_case(src, Servo::Arrival, stall));
            let moved = r.av_s[at + 1] - pre_mean_ms(&r);
            let arrival_moved = a.av_s[at + 1] - pre_mean_ms(&a);
            // the second after the stall already carries part of the stamp basis's payment; a
            // tick-long packet's arrival reading also carries its packet-vs-tick phase (issue 1425
            // slice 3, ARRIVAL_PHASE_MS)
            let phase_ms = if src.packet_frames >= TICK_FRAMES {
                ARRIVAL_PHASE_MS
            } else {
                0.0
            };
            let same_as_arrival = (moved - arrival_moved).abs()
                <= STALL_EXCURSION_MS + PAID_WITHIN_BOOKING_MS + phase_ms;
            assert!(
                moved > 20.0 && same_as_arrival,
                "issue 1425: {} {ms} ms stall: OBS's own placement displaces the audio, the same on \
                 the arrival servo: {moved:.3} vs {arrival_moved:.3} ms",
                src.name
            );
            let first = r.first_jump_s.unwrap_or(f64::INFINITY);
            assert!(
                r.place_jumps == 1 && (EVENT_S..=EVENT_S + BOOKED_WITHIN_S).contains(&first),
                "issue 1425: {} {ms} ms stall: the displacement is real, booked once within \
                 {BOOKED_WITHIN_S} s: jumps {} first at {first:.3} s",
                src.name,
                r.place_jumps
            );
            let pre = pre_mean_ms(&r);
            let back = at
                + r.av_s[at..]
                    .iter()
                    .position(|v| (v - pre).abs() <= BACK_MS)
                    .unwrap_or(r.av_s.len());
            let paid_by = at + 2 + moved.ceil() as usize;
            let after = excursion_from_ms(&r, back.min(r.av_s.len() - 1));
            assert!(
                back <= paid_by && after <= LATE_WALK_MS,
                "issue 1425: {} {ms} ms stall: the A/V must be back within {BACK_MS} ms once the \
                 booking is paid at 1000 ppm (by {paid_by} s) and stay within {LATE_WALK_MS} ms: \
                 back at {back} s, then {after:.3} ms",
                src.name
            );
            assert!(
                excursion_from_ms(&a, back) > 20.0,
                "issue 1425: {} {ms} ms stall: the arrival servo keeps OBS's displacement (else the \
                 booking proves nothing)",
                src.name
            );
        }
    }
}

#[test]
fn a_real_loss_is_booked_once_and_repaid_like_the_arrival_servo_1425() {
    let loss = Event::Loss {
        at_s: EVENT_S,
        ms: LOSS_MS,
    };
    for src in SOURCES {
        let r = run(&event_case(src, Servo::Production, loss));
        let a = run(&event_case(src, Servo::Arrival, loss));
        let first = r.first_jump_s.unwrap_or(f64::INFINITY);
        assert!(
            r.place_jumps == 1 && (EVENT_S..=EVENT_S + BOOKED_WITHIN_S).contains(&first),
            "issue 1425: {}: a real {LOSS_MS} ms loss is booked once within {BOOKED_WITHIN_S} s: \
             jumps {} first at {first:.3} s",
            src.name,
            r.place_jumps
        );
        // anti-tautology: the loss really moved the A/V
        assert!(
            (r.av_s[EVENT_S as usize + 1] - pre_mean_ms(&r)).abs() > 20.0,
            "issue 1425: {}: the loss must displace the A/V, else the bench proves nothing",
            src.name
        );
        let back = back_within_s(&r, BACK_MS);
        let arrival_back = back_within_s(&a, BACK_MS);
        // absolute too: on PulseAudio the arrival servo never comes back in the run, so the
        // relative bound alone would pass a loss that is never repaid (review round 3)
        let paid_by = EVENT_S as usize + 2 + LOSS_MS.ceil() as usize;
        assert!(
            back <= arrival_back + LOSS_BACK_SLACK_S && back <= paid_by,
            "issue 1425: {}: the A/V must be back within {BACK_MS} ms once paid at 1000 ppm (by \
             {paid_by} s) and no later than the arrival servo's + {LOSS_BACK_SLACK_S} s: {back} s \
             vs {arrival_back} s",
            src.name
        );
    }
}

#[test]
fn equal_stamp_bursts_do_not_drift_over_eight_hours_1425() {
    for src in SOURCES {
        let mut case = event_case(src, Servo::Production, Event::EqualStampBursts);
        case.run_s = EIGHT_HOURS_S;
        let r = run(&case);
        let drift = drift_from_ms(&r, BURST_SETTLED_FROM_S);
        // the settled offset against the same feed without bursts is p·d (module doc)
        let mut plain = event_case(src, Servo::Production, Event::None);
        plain.run_s = 2 * BURST_SETTLED_FROM_S as u64;
        let plain = run(&plain);
        let settled = |run: &Run| {
            let s = &run.av_s[BURST_SETTLED_FROM_S..2 * BURST_SETTLED_FROM_S];
            s.iter().sum::<f64>() / s.len() as f64
        };
        let offset = settled(&r) - settled(&plain);
        let want = BURST_PAIR_P * frames_ns(src.packet_frames) as f64 / 1e6;
        assert!(
            (offset - want).abs() <= (0.1 * want).max(0.1),
            "issue 1425: {} equal-stamp bursts: the A/V must settle p·d = {want:.3} ms late (the \
             early-stamped first packet of each pair is the one the level loop reads): {offset:.3} ms",
            src.name
        );
        assert!(
            drift <= BURST_DRIFT_MS,
            "issue 1425: {} over 8 h with equal-stamp bursts: the minute A/V must move <= \
             {BURST_DRIFT_MS} ms from {BURST_SETTLED_FROM_S} s on: drift {drift:.3} ms",
            src.name
        );
        assert!(
            r.place_jumps == 0 && r.underruns == 0,
            "issue 1425: {} equal-stamp bursts: no booked jump, no underrun: {} / {}",
            src.name,
            r.place_jumps,
            r.underruns
        );
    }
}
