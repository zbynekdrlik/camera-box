//! Issue 1381 (design 5882391108) — the WALL-STEP bench of the genlock timecode audio: a test-only
//! child of `asrc_timecode_bench` (declared there with `#[path]`), so it drives the same OBS ingest
//! replay (`Obs`: TS smoothing, reset, push-back, place / append, the mixer, the timecode ASRC).
//!
//! ## What it reproduces
//!
//! The cg OBS on resolume, 29.9.2026 (comment 5882384071): the nightly dantesync date step moved the
//! receiver's wall clock by +682.474 ms at once, and SongPlayer (on the same box) followed it only
//! seconds later. Every packet in that window read the whole step as its placement error; the booking
//! capped it at 100 ms, the excess leaked into the level loop's smoothed error, and every later
//! placement re-booked that stale error as a phantom jump: a sawtooth of ~65 ms skips every ~80 s for
//! 18 min (`place_jumps` 0 → 468, `applied` ~151 ppm, `restore=1`). The 89.7 ms step of the same night
//! left the audio ~87 ms off its stamps for ~45 s (`place_jumps` +4).
//!
//! ## Senders
//!
//! - [`Follow::Jump`] — the design's model: the sender's stamps (its wall at emit) jump by the step
//!   `lag` after the receiver's; its content never skips, so the TRUE landing never moves.
//! - [`Follow::Burst`] — SongPlayer's audio emitter as read in `sp-server/src/playback/audio_emitter.rs`:
//!   the grid stamps stay continuous (it re-anchors only past 1 s), and at `lag` it catches up — a
//!   forward step emits the missed slots at once, a backward one pauses. Its content advanced with
//!   the wall, so from then on the true landing is the step earlier.
//! - [`Follow::Never`] — a sender whose stamps never follow (its content keeps the old wall, so the
//!   true landing moves at the step).
//!
//! ## Acceptance (design 5882391108)
//!
//! Production: the 682 ms and 89.7 ms steps (lag 3.3 s and 6.7 s, the two SongPlayer re-anchor
//! bounds) cost at most ONE event — a landing that departs from the continuation by more than 1 ms,
//! or a booked placement jump — and the audio sits within ±2 ms of its true landing from the step on
//! (the servo's own level too). A sender that never jumps its stamps is released after
//! `AUDIO_STEP_HOLD_MAX_NS` with one placement. No step: nothing happens at all.

use super::*;
use crate::genlock_audio_pairing::AUDIO_STEP_HOLD_MAX_NS;

const STEP_AT_NS: u64 = 300 * NS_PER_S;
/// 20 min after the step: the live sawtooth ran 18 min.
const STEP_RUN_NS: u64 = 1500 * NS_PER_S;
/// The live steps of 29.9.2026.
const STEP_682_NS: i64 = 682_474_000;
const STEP_90_NS: i64 = 89_703_000;
/// SongPlayer follows a step after two confirming wall resamples, every ~3.3 s.
const LAG_MIN_NS: u64 = 3_300_000_000;
const LAG_MAX_NS: u64 = 6_700_000_000;
/// A landing that departs from the previous packet's continuation by more than this is an event.
const EVENT_DISC_NS: u64 = 1_000_000;
/// A skip at least this large is one of the sawtooth's audible jumps.
const SKIP_MS: f64 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Follow {
    Jump,
    Burst,
    Never,
    /// No step at all (steady state).
    NoStep,
}

#[derive(Debug, Clone, Copy)]
struct StepCase {
    wall_ns: i64,
    lag_ns: u64,
    follow: Follow,
    /// Review round 1: > 0 = the SENDER steps first, at the step time, and the receiver this long
    /// after it (`lag_ns` is then unused). Everything is measured from the receiver's step.
    sender_first_ns: u64,
    /// Review round 1: extra arrival jitter, uniform 0..this (0 = the default 2-5 ms delivery only).
    burst_jitter_ns: u64,
}

/// When the receiver's own wall steps.
fn recv_step_at(case: StepCase) -> u64 {
    STEP_AT_NS + case.sender_first_ns
}

/// The sender's packets for one case; `leap_ns` carries the TRUE landing's move (two's complement).
fn step_sender_packets(case: StepCase) -> Vec<Packet> {
    let mut rng = Lcg(0x1381_5882);
    let follow_at = if case.sender_first_ns > 0 {
        STEP_AT_NS
    } else {
        STEP_AT_NS + case.lag_ns
    };
    let wall = case.wall_ns as u64;
    let mut out = Vec::new();
    let mut prev_arrival = 0_u64;
    let mut k = 0_u64;
    loop {
        let nominal = slot_ns(k);
        if nominal >= STEP_RUN_NS {
            break;
        }
        let jitter = rng.ns(0, 1_000_000);
        let (emit, stamp_wall, leap_ns) = match case.follow {
            Follow::NoStep => (nominal + jitter, WALL0 + nominal + jitter, 0),
            Follow::Jump => {
                let emit = nominal + jitter;
                let followed = if emit >= follow_at { wall } else { 0 };
                (emit, (WALL0 + emit).wrapping_add(followed), 0)
            }
            Follow::Burst => {
                // the grid stamp is exact; the emit thread wakes up to 1 ms late
                if nominal < follow_at {
                    (nominal + jitter, WALL0 + nominal, 0)
                } else {
                    let due = nominal.wrapping_sub(wall).max(follow_at);
                    (due + jitter, WALL0 + nominal, 0_u64.wrapping_sub(wall))
                }
            }
            Follow::Never => {
                let emit = nominal + jitter;
                let leap = if emit >= STEP_AT_NS {
                    0_u64.wrapping_sub(wall)
                } else {
                    0
                };
                (emit, WALL0 + emit, leap)
            }
        };
        let extra = if case.burst_jitter_ns > 0 {
            rng.ns(0, case.burst_jitter_ns)
        } else {
            0
        };
        let arrival_ns = (emit + 2_000_000 + rng.ns(0, 3_000_000) + extra).max(prev_arrival);
        prev_arrival = arrival_ns;
        out.push(Packet {
            slot: k,
            leap_ns,
            stamp: stamp_wall / 100 * 100,
            arrival_ns,
        });
        k += 1;
    }
    out
}

fn step_receiver_wall(t_ns: u64, case: StepCase) -> u64 {
    if case.follow != Follow::NoStep && t_ns >= recv_step_at(case) {
        (WALL0 + t_ns).wrapping_add(case.wall_ns as u64)
    } else {
        WALL0 + t_ns
    }
}

#[derive(Debug, Default)]
struct StepRun {
    /// Landings that departed from the continuation after the step: (s after the step, ms).
    discs: Vec<(f64, f64)>,
    /// Booked placement jumps after the step (a backstop placement is counted once, as its
    /// discontinuity, not here).
    jumps: u32,
    /// Skips of at least [`SKIP_MS`] after the step, and the span from the first to the last (s).
    skips: usize,
    skip_span_s: f64,
    /// The last time after the step |A/V| was over the bound, s after the step (0 = never).
    av_settle_s: f64,
    /// The same for the servo's own level (the placement error it is fed).
    level_settle_s: f64,
    /// max |A/V| after the step (ms).
    av_max_ms: f64,
    /// max |A/V| over the last minute (ms).
    av_tail_ms: f64,
    /// Every hold release: (s after the step, reason).
    releases: Vec<(f64, AudioStepRelease)>,
    /// A hold still running at the end.
    holding_at_end: bool,
}

impl StepRun {
    fn events(&self) -> usize {
        self.discs.len() + self.jumps as usize
    }
}

fn run_step(case: StepCase, variant: Variant) -> StepRun {
    let mut obs = Obs::new(variant);
    let mut r = StepRun::default();
    let mut jumps_at_step = None;
    let mut backstops_at_step = 0;
    let mut continuation: Option<u64> = None;
    let mut first_skip = None;
    let measure_at = recv_step_at(case);
    for pkt in step_sender_packets(case) {
        let mono_now = MONO0 + pkt.arrival_ns;
        obs.mix_until(mono_now);
        let dur = obs.asrc_process(mono_now);
        let wall = step_receiver_wall(pkt.arrival_ns, case);
        if pkt.arrival_ns >= measure_at {
            // taken BEFORE the first measured packet, so a booking on the step packet itself counts
            if jumps_at_step.is_none() {
                jumps_at_step = Some(obs.c.place_jump_count());
                backstops_at_step = obs.backstops;
            }
        }
        let landed = obs.ingest(&pkt, mono_now, wall, HOLD_MS, dur);
        let t = pkt.arrival_ns;
        let truth =
            (MONO0 + slot_ns(pkt.slot) + genlock_audio_delay_ns(HOLD_MS)).wrapping_add(pkt.leap_ns);
        let av_ms = landed.actual.wrapping_sub(truth) as i64 as f64 / 1e6;
        if t >= measure_at {
            let after_s = (t - measure_at) as f64 / 1e9;
            if let Some(cont) = continuation {
                let disc = landed.actual.wrapping_sub(cont) as i64;
                if disc.unsigned_abs() > EVENT_DISC_NS {
                    let disc_ms = disc as f64 / 1e6;
                    r.discs.push((after_s, disc_ms));
                    if disc_ms.abs() >= SKIP_MS {
                        r.skips += 1;
                        let first = *first_skip.get_or_insert(after_s);
                        r.skip_span_s = after_s - first;
                    }
                }
            }
            if landed.release != AudioStepRelease::None {
                r.releases.push((after_s, landed.release));
            }
            if av_ms.abs() > AV_BOUND_MS {
                r.av_settle_s = after_s;
            }
            if landed.appended && landed.err_ms.abs() > AV_BOUND_MS {
                r.level_settle_s = after_s;
            }
            r.av_max_ms = r.av_max_ms.max(av_ms.abs());
            if t + 60 * NS_PER_S >= STEP_RUN_NS {
                r.av_tail_ms = r.av_tail_ms.max(av_ms.abs());
            }
        }
        continuation = Some(landed.actual + dur);
    }
    r.jumps =
        obs.c.place_jump_count() - jumps_at_step.unwrap_or(0) - (obs.backstops - backstops_at_step);
    r.holding_at_end = obs.step_hold.active;
    r
}

fn jump(wall_ns: i64, lag_ns: u64) -> StepCase {
    StepCase {
        wall_ns,
        lag_ns,
        follow: Follow::Jump,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
    }
}

#[test]
fn a_682_ms_wall_step_costs_at_most_one_event_and_never_moves_the_audio_1381() {
    for lag in [LAG_MIN_NS, LAG_MAX_NS] {
        for wall_ns in [STEP_682_NS, -STEP_682_NS] {
            let r = run_step(jump(wall_ns, lag), Variant::Production);
            assert!(
                r.events() <= 1 && r.av_max_ms <= AV_BOUND_MS && r.level_settle_s == 0.0,
                "issue 1381: a {} ms wall step the sender follows {} s later must cost at most one \
                 event and keep the audio on its true landing (the live sawtooth: ~65 ms skips every \
                 ~80 s for 18 min): {r:?}",
                wall_ns as f64 / 1e6,
                lag as f64 / 1e9
            );
            assert!(
                r.releases == [(r.releases[0].0, AudioStepRelease::Followed)]
                    && r.releases[0].0 < (lag as f64 / 1e9) + 0.1
                    && !r.holding_at_end,
                "issue 1381: the hold must end once, when the stamps follow: {r:?}"
            );
        }
    }
}

#[test]
fn the_89_7_ms_step_costs_at_most_one_event_and_is_never_late_1381() {
    // both sender shapes; the live one is the catch-up (its audio stamps never jumped: ~87 ms off
    // them for ~45 s, place_jumps +4, paid at 1000 ppm)
    for follow in [Follow::Jump, Follow::Burst] {
        for lag in [LAG_MIN_NS, LAG_MAX_NS] {
            let case = StepCase {
                wall_ns: STEP_90_NS,
                lag_ns: lag,
                follow,
                sender_first_ns: 0,
                burst_jitter_ns: 0,
            };
            let r = run_step(case, Variant::Production);
            let released_s = r.releases.first().map_or(0.0, |x| x.0);
            assert!(
                r.releases.len() == 1
                    && r.events() <= 1
                    && r.jumps == 0
                    && r.av_settle_s <= released_s + 0.1
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: the 89.7 ms step must cost at most one event, book nothing \
                 and put the audio within ±{AV_BOUND_MS} ms of its true landing once the hold ends: \
                 {r:?}"
            );
        }
    }
}

#[test]
fn without_the_hold_the_step_is_read_as_a_placement_error_1381() {
    // anti-tautology: the same step with the placement re-seed and the backstop but no skew hold. The
    // backstop places the first packet on the stepped offset, the follow places it back: two
    // step-sized events, and the audio sits off its true landing until the sender follows.
    let r = run_step(jump(STEP_682_NS, LAG_MAX_NS), Variant::NoStepHold);
    assert!(
        r.events() >= 2 && r.av_max_ms > 600.0,
        "issue 1381: without the skew hold the bench must show the step as a placement error, or it \
         proves nothing about the hold: {r:?}"
    );
    // ... yet no limit cycle: the placements re-seed the level loop and the audio settles with them.
    assert!(
        r.skip_span_s < 10.0 && r.av_tail_ms <= AV_BOUND_MS,
        "issue 1381: the re-seed and the backstop alone must already end the sawtooth: {r:?}"
    );
}

#[test]
fn a_sender_that_catches_up_is_released_at_once_and_one_that_never_follows_at_the_bound_1381() {
    // SongPlayer's audio emitter: continuous grid stamps, a forward step caught up by a burst, a
    // backward one by a pause. The hold ends the moment the stamps are back on the live wall and the
    // release applies the step ONCE (it drops the burst's excess / lands after the pause).
    for wall_ns in [STEP_682_NS, -STEP_682_NS, STEP_90_NS] {
        for lag in [LAG_MIN_NS, LAG_MAX_NS] {
            let case = StepCase {
                wall_ns,
                lag_ns: lag,
                follow: Follow::Burst,
                sender_first_ns: 0,
                burst_jitter_ns: 0,
            };
            let r = run_step(case, Variant::Production);
            let caught_up_s = (lag as f64 + wall_ns.min(0).unsigned_abs() as f64) / 1e9;
            assert!(
                r.releases.len() == 1
                    && r.releases[0].1 == AudioStepRelease::Followed
                    && (r.releases[0].0 - caught_up_s).abs() < 0.1
                    && r.jumps == 0
                    && r.av_settle_s <= r.releases[0].0 + 0.1
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: released once, when the sender caught up, nothing booked, the \
                 audio on its true landing from then on: {r:?}"
            );
            // one event: the release's placement (a backward step's pause is the sender's own gap,
            // the release lands the next packet after it)
            assert!(
                r.events() == 1,
                "issue 1381: {case:?}: exactly one event: {r:?}"
            );
        }
    }
    // a sender whose stamps never follow: bounded -- released once at the bound, one placement
    let bound_s = AUDIO_STEP_HOLD_MAX_NS as f64 / 1e9;
    let never = StepCase {
        wall_ns: STEP_682_NS,
        lag_ns: 0,
        follow: Follow::Never,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
    };
    let r = run_step(never, Variant::Production);
    assert!(
        r.releases.len() == 1
            && r.releases[0].1 == AudioStepRelease::Timeout
            && (r.releases[0].0 - bound_s).abs() < 0.1
            && r.jumps == 0
            && r.events() == 1
            && r.av_settle_s <= bound_s + 0.1
            && r.av_tail_ms <= AV_BOUND_MS,
        "issue 1381: a sender that never follows is released once at the bound: {r:?}"
    );
}

#[test]
fn steady_state_and_a_small_step_never_hold_1381() {
    let steady = run_step(
        StepCase {
            wall_ns: 0,
            lag_ns: 0,
            follow: Follow::NoStep,
            sender_first_ns: 0,
            burst_jitter_ns: 0,
        },
        Variant::Production,
    );
    assert!(
        steady.events() == 0 && steady.releases.is_empty() && steady.av_max_ms <= AV_BOUND_MS,
        "issue 1381: a steady feed never holds, never places, never books: {steady:?}"
    );
    // a step up to one packet is left to the existing path (no hold, no log line)
    let small = run_step(jump(20_000_000, LAG_MIN_NS), Variant::Production);
    assert!(
        small.releases.is_empty(),
        "issue 1381: a step within one packet never starts a hold: {small:?}"
    );
}

#[test]
fn a_sender_that_stepped_first_costs_one_placement_at_the_receiver_step_1381() {
    // review round 1: a cross-box sender whose box steps 2 s BEFORE the receiver's. Its stamps (a
    // jump, or a caught-up burst / pause) are already on the new wall when the receiver's step lands,
    // so the receiver's step puts them back on its wall: nothing held, the receiver-step packet
    // released at once and placed (one event, nothing booked), the audio on its true landing within
    // 0.2 s of the receiver's step -- for a step under the owed cap too (appended, it would be booked
    // and paid over ~90 s with a 70 ms TS-smoothing re-placement on the way).
    for follow in [Follow::Jump, Follow::Burst] {
        for wall_ns in [STEP_682_NS, -STEP_682_NS, STEP_90_NS] {
            let case = StepCase {
                wall_ns,
                lag_ns: 0,
                follow,
                sender_first_ns: 2 * NS_PER_S,
                burst_jitter_ns: 0,
            };
            let r = run_step(case, Variant::Production);
            assert!(
                r.releases.len() == 1
                    && r.releases[0].1 == AudioStepRelease::Followed
                    && r.releases[0].0 < 0.1
                    && !r.holding_at_end
                    && r.events() <= 1
                    && r.jumps == 0
                    && r.av_settle_s <= 0.2
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: a sender that stepped first must never be held -- the \
                 receiver's step puts its stamps back on the receiver's wall: {r:?}"
            );
        }
    }
}

#[test]
fn heavy_arrival_jitter_bounds_a_small_step_to_two_events_1381() {
    // review round 1: the age test reads one packet's age against the smoothed nominal. Under 20 ms
    // of extra arrival jitter a step under ~55 ms can read as followed early: the release places it,
    // and a sender whose stamps then JUMP has that jump booked and paid at 1000 ppm -- two events,
    // settled within the step's own payment time. A catch-up sender, a 60 ms step and the 682 ms one
    // keep at most one event; a steady feed never holds.
    for (wall_ns, follow) in [
        (40_000_000_i64, Follow::Jump),
        (40_000_000, Follow::Burst),
        (60_000_000, Follow::Jump),
        (60_000_000, Follow::Burst),
        (-50_000_000, Follow::Jump),
        (STEP_682_NS, Follow::Jump),
    ] {
        let case = StepCase {
            wall_ns,
            lag_ns: LAG_MIN_NS,
            follow,
            sender_first_ns: 0,
            burst_jitter_ns: 20_000_000,
        };
        let r = run_step(case, Variant::Production);
        let pay_s = wall_ns.unsigned_abs() as f64 / 1e6;
        let max_events = if follow == Follow::Jump && wall_ns.unsigned_abs() < 55_000_000 {
            2
        } else {
            1
        };
        assert!(
            r.events() <= max_events
                && r.releases.len() == 1
                && !r.holding_at_end
                && r.av_settle_s <= LAG_MIN_NS as f64 / 1e9 + pay_s + 5.0
                && r.av_tail_ms <= AV_BOUND_MS,
            "issue 1381: {case:?}: under 20 ms arrival jitter a step costs at most {max_events} \
             event(s) and settles within its own payment time: {r:?}"
        );
    }
    let steady = run_step(
        StepCase {
            wall_ns: 0,
            lag_ns: 0,
            follow: Follow::NoStep,
            sender_first_ns: 0,
            burst_jitter_ns: 20_000_000,
        },
        Variant::Production,
    );
    assert!(
        steady.events() == 0 && steady.releases.is_empty(),
        "issue 1381: a steady feed under 20 ms arrival jitter never holds: {steady:?}"
    );
}
