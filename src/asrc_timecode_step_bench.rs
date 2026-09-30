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
    /// Issue 1381 (design 5900385541): the genlock sender contract's RELABEL (sections 5 and 6,
    /// SongPlayer #224): one block per boundary, stamped on its boundary; at the step content k is
    /// stamped on boundary k + N (N = floor(S / slot)) and emitted when the stepped wall reaches that
    /// boundary -- the samples continuous, the emit re-phased by the remainder r = S − N·slot. The
    /// video is relabelled the same way, so the TRUE landing moves r earlier.
    Relabel,
    /// Issue 1381 (design 5901213031): no wall step at all -- the sender PAUSES for `wall_ns` at the
    /// step time (those slots are never sent, their samples lost) and resumes on its real-time grid:
    /// its stamps jump by the pause and so does its arrival (a pause or a restart).
    Pause,
    /// Issue 1381 (design 5901213031, review round 1): the `Relabel` sender, plus a
    /// [`RELABEL_PAUSE_NS`] pause starting [`RELABEL_PAUSE_AFTER_SLOTS`] slots after its own step --
    /// inside a pending relabel's window when the sender stepped first.
    RelabelPause,
    /// Issue 1381 (review round 2): the `Jump` sender (a raw-clock sender: its stamps are its wall
    /// at emit) whose step-carrying packet is submitted [`STEP_LATE_NS`] late -- its stamp and its
    /// arrival; the next packet is on time again.
    JumpLate,
    /// Slice 3 (design 5902870861): the `Relabel` sender's block-per-boundary stamps with no wall step
    /// at all; it never sends the first block at or after the step time (a SKIPPED slot: its stamps
    /// jump one slot, exactly like an N = +1 relabel, but its arrival gaps by one block too).
    SkipBlock,
    /// Slice 3 (design 5902870861): the same sender RESENDS the block before the step time, 3 ms after
    /// it (a DUPLICATED slot: the same stamp again, the stamp jump − one packet, exactly like an
    /// N = −1 relabel).
    DupBlock,
}

/// Issue 1381 (review round 1): the pause of [`Follow::RelabelPause`] (15 slots) and where it starts.
const RELABEL_PAUSE_NS: u64 = 500_000_000;
const RELABEL_PAUSE_AFTER_SLOTS: u64 = 5;

/// Issue 1381 (review round 2): how late [`Follow::JumpLate`] submits its step-carrying packet
/// (under the pending relabel's 15 ms arrival budget, over the 2 ms step minimum).
const STEP_LATE_NS: u64 = 8_000_000;

/// Issue 1381 (design 5900385541): the slots a relabelling sender moves its stamps by at a wall step
/// of `wall_ns`: N = floor(S / slot), toward −∞ (the contract's floor).
fn relabel_slots(wall_ns: i64) -> i64 {
    (i128::from(wall_ns) * i128::from(RATE))
        .div_euclid(i128::from(PACKET_FRAMES) * i128::from(NS_PER_S)) as i64
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
    /// Review round 2: this many packets were queued when the receiver connected (they arrive together
    /// with the next one, so the first packets read older than the steady transport lag).
    connect_backlog: usize,
    /// Slice 3 (design 5902870861): both steps happen this much after [`STEP_AT_NS`]. A relabel's stamp
    /// jump is quantized to the sender's 100 ns units, so on the 30 fps per-second grid an N = +1
    /// relabel jumps one packet + 34 ns or one packet − 66 ns depending on the grid position of its
    /// first relabelled block: `slot_ns(p)` puts it on position p (position 1 is the − 66 ns one).
    step_offset_ns: u64,
}

/// When the first step happens (the sender's, when it steps first).
fn step_at(case: StepCase) -> u64 {
    STEP_AT_NS + case.step_offset_ns
}

/// When the receiver's own wall steps.
fn recv_step_at(case: StepCase) -> u64 {
    step_at(case) + case.sender_first_ns
}

/// When the sender follows: at the step time when it steps first, else `lag_ns` after the receiver.
fn follow_at(case: StepCase) -> u64 {
    if case.sender_first_ns > 0 {
        step_at(case)
    } else {
        step_at(case) + case.lag_ns
    }
}

/// Issue 1381 (design 5901213031): a `sender_first_ns` this large means the receiver never steps
/// within the run.
const RECEIVER_NEVER_NS: u64 = STEP_RUN_NS;

/// Whether the receiver's own wall steps within the run (a pause is no wall step at all).
fn receiver_steps(case: StepCase) -> bool {
    !matches!(
        case.follow,
        Follow::NoStep | Follow::Pause | Follow::SkipBlock | Follow::DupBlock
    ) && recv_step_at(case) < STEP_RUN_NS
}

/// From when the run's timings, events and A/V are measured: the receiver's step, or the sender's
/// when the receiver never steps. Losses, relabels and pendings always count from the first step.
fn measure_at(case: StepCase) -> u64 {
    if receiver_steps(case) {
        recv_step_at(case)
    } else {
        step_at(case)
    }
}

/// The sender's packets for one case; `leap_ns` carries the TRUE landing's move (two's complement).
fn step_sender_packets(case: StepCase) -> Vec<Packet> {
    let mut rng = Lcg(0x1381_5882);
    let follow_at = follow_at(case);
    let wall = case.wall_ns as u64;
    let mut out = Vec::new();
    let mut prev_arrival = 0_u64;
    let mut late_done = false;
    let mut k = 0_u64;
    loop {
        let nominal = slot_ns(k);
        if nominal >= STEP_RUN_NS {
            break;
        }
        let jitter = rng.ns(0, 1_000_000);
        let (emit, stamp_wall, leap_ns) = match case.follow {
            Follow::NoStep => (nominal + jitter, WALL0 + nominal + jitter, 0),
            Follow::Jump | Follow::JumpLate => {
                let mut emit = nominal + jitter;
                let followed = if emit >= follow_at { wall } else { 0 };
                if case.follow == Follow::JumpLate && followed != 0 && !late_done {
                    late_done = true;
                    emit += STEP_LATE_NS;
                }
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
                let leap = if emit >= step_at(case) {
                    0_u64.wrapping_sub(wall)
                } else {
                    0
                };
                (emit, WALL0 + emit, leap)
            }
            Follow::Relabel | Follow::RelabelPause => {
                let pause_at = follow_at + slot_ns(RELABEL_PAUSE_AFTER_SLOTS);
                if case.follow == Follow::RelabelPause
                    && (pause_at..pause_at + RELABEL_PAUSE_NS).contains(&nominal)
                {
                    k += 1;
                    continue;
                }
                if nominal < follow_at {
                    (nominal + jitter, WALL0 + nominal, 0)
                } else {
                    let boundary = slot_ns(k.wrapping_add(relabel_slots(case.wall_ns) as u64));
                    let due = boundary.wrapping_sub(wall).max(follow_at);
                    // the video follows the relabelled stamps through the receiver's wall: once
                    // that wall stepped too, the true landing moved by N slots - S = -r; while it
                    // never steps, by the whole N slots
                    let recv_wall = if receiver_steps(case) { wall } else { 0 };
                    let leap = boundary.wrapping_sub(nominal).wrapping_sub(recv_wall);
                    (due + jitter, WALL0 + boundary, leap)
                }
            }
            Follow::Pause => {
                if (follow_at..follow_at + wall).contains(&nominal) {
                    k += 1;
                    continue;
                }
                (nominal + jitter, WALL0 + nominal + jitter, 0)
            }
            Follow::SkipBlock | Follow::DupBlock => {
                if case.follow == Follow::SkipBlock
                    && nominal >= follow_at
                    && slot_ns(k.wrapping_sub(1)) < follow_at
                {
                    k += 1;
                    continue;
                }
                (nominal + jitter, WALL0 + nominal, 0)
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
        // slice 3: the duplicated block -- the one before the step time, resent 3 ms after it
        if case.follow == Follow::DupBlock && nominal < follow_at && slot_ns(k + 1) >= follow_at {
            let dup = Packet {
                arrival_ns: arrival_ns + 3_000_000,
                ..out[out.len() - 1]
            };
            prev_arrival = dup.arrival_ns;
            out.push(dup);
        }
        k += 1;
    }
    if case.connect_backlog > 0 && case.connect_backlog < out.len() {
        let at = out[case.connect_backlog].arrival_ns;
        for p in out.iter_mut().take(case.connect_backlog) {
            p.arrival_ns = at;
        }
    }
    out
}

fn step_receiver_wall(t_ns: u64, case: StepCase) -> u64 {
    if receiver_steps(case) && t_ns >= recv_step_at(case) {
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
    /// Issue 1381 (design 5900385541): packets appended as a relabel after the first step (design
    /// 5901213031: and pending relabels resolved).
    relabels: u32,
    /// Issue 1381 (design 5901213031): pending relabels started after the first step.
    pendings: u32,
    /// max |A/V| from the FIRST step on (the sender's, when it stepped first), ms.
    av_max_all_ms: f64,
    /// Queued audio overwritten by a placement inside the buffer, dropped by a buffer reset, and the
    /// zero-filled gap a placement past the end opened -- after the FIRST step (either box's), ms.
    overwritten_ms: f64,
    dropped_ms: f64,
    gap_ms: f64,
    /// Review round 2 (ROZHODNUTÉ 5903945145): the same three losses from the sender's FOLLOW on
    /// (a receiver-first step's late follow must cost nothing), ms.
    follow_overwritten_ms: f64,
    follow_dropped_ms: f64,
    follow_gap_ms: f64,
    /// Review round 3: max |A/V| from the sender's follow on, and the last time from the follow on
    /// that |A/V| was over the bound (s after the follow, 0 = never), ms / s.
    follow_av_max_ms: f64,
    follow_av_settle_s: f64,
    /// max |estimated| of the servo after the step (ppm).
    est_max_ppm: f64,
}

/// Issue 1381: every packet from the step on -- (landing, appended, the servo's error input bits,
/// release) -- for a byte-for-byte comparison of two variants.
type StepTrace = Vec<(u64, bool, u64, u8)>;

impl StepRun {
    fn events(&self) -> usize {
        self.discs.len() + self.jumps as usize
    }
}

fn run_step(case: StepCase, variant: Variant) -> StepRun {
    run_step_traced(case, variant).0
}

fn run_step_traced(case: StepCase, variant: Variant) -> (StepRun, StepTrace) {
    let mut obs = Obs::new(variant);
    let mut r = StepRun::default();
    let mut trace = StepTrace::new();
    let mut jumps_at_step = None;
    let mut backstops_at_step = 0;
    // (relabels, overwritten, dropped, gap, pendings) at the FIRST step (either box's)
    let mut losses_at_step: Option<(u32, u64, u64, u64, u32)> = None;
    let mut losses_at_follow: Option<(u64, u64, u64)> = None;
    let mut continuation: Option<u64> = None;
    let mut first_skip = None;
    let measure_at = measure_at(case);
    for pkt in step_sender_packets(case) {
        let mono_now = MONO0 + pkt.arrival_ns;
        obs.mix_until(mono_now);
        let dur = obs.asrc_process(mono_now);
        let wall = step_receiver_wall(pkt.arrival_ns, case);
        if pkt.arrival_ns >= step_at(case) && losses_at_step.is_none() {
            losses_at_step = Some((
                obs.relabels,
                obs.overwritten_ns,
                obs.dropped_ns,
                obs.gap_ns,
                obs.pendings,
            ));
        }
        if pkt.arrival_ns >= follow_at(case) && losses_at_follow.is_none() {
            losses_at_follow = Some((obs.overwritten_ns, obs.dropped_ns, obs.gap_ns));
        }
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
        if t >= step_at(case) {
            r.av_max_all_ms = r.av_max_all_ms.max(av_ms.abs());
        }
        if t >= follow_at(case) {
            r.follow_av_max_ms = r.follow_av_max_ms.max(av_ms.abs());
            if av_ms.abs() > AV_BOUND_MS {
                r.follow_av_settle_s = (t - follow_at(case)) as f64 / 1e9;
            }
        }
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
            r.est_max_ppm = r.est_max_ppm.max(obs.c.estimated_ppm().abs());
            trace.push((
                landed.actual,
                landed.appended,
                landed.err_ms.to_bits(),
                landed.release as u8,
            ));
        }
        continuation = Some(landed.actual + dur);
    }
    r.jumps =
        obs.c.place_jump_count() - jumps_at_step.unwrap_or(0) - (obs.backstops - backstops_at_step);
    r.holding_at_end = obs.step_hold.active;
    let at = losses_at_step.unwrap_or_default();
    r.relabels = obs.relabels - at.0;
    r.overwritten_ms = (obs.overwritten_ns - at.1) as f64 / 1e6;
    r.dropped_ms = (obs.dropped_ns - at.2) as f64 / 1e6;
    r.gap_ms = (obs.gap_ns - at.3) as f64 / 1e6;
    r.pendings = obs.pendings - at.4;
    let at = losses_at_follow.unwrap_or((obs.overwritten_ns, obs.dropped_ns, obs.gap_ns));
    r.follow_overwritten_ms = (obs.overwritten_ns - at.0) as f64 / 1e6;
    r.follow_dropped_ms = (obs.dropped_ns - at.1) as f64 / 1e6;
    r.follow_gap_ms = (obs.gap_ns - at.2) as f64 / 1e6;
    (r, trace)
}

fn jump(wall_ns: i64, lag_ns: u64) -> StepCase {
    StepCase {
        wall_ns,
        lag_ns,
        follow: Follow::Jump,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
        connect_backlog: 0,
        step_offset_ns: 0,
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
                connect_backlog: 0,
                step_offset_ns: 0,
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
                connect_backlog: 0,
                step_offset_ns: 0,
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
        connect_backlog: 0,
        step_offset_ns: 0,
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
            connect_backlog: 0,
            step_offset_ns: 0,
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
fn a_sender_that_stepped_first_is_resolved_at_the_receiver_step_1381() {
    // review round 1: a cross-box sender whose box steps 2 s BEFORE the receiver's. Its stamps (a
    // jump, or a caught-up burst / pause) are already on the new wall when the receiver's step lands,
    // so the receiver's step puts them back on its wall: never held after it.
    //
    // Design 5901213031: a sender whose stamps JUMPED by the step (its raw wall clock, over one
    // packet, continuous arrival) is a PENDING relabel from its own step on -- appended on its
    // continuous timeline (never placed N slots late, never reset past 2 s: the landing is its true
    // one, its content never skipped) -- and the receiver's step releases it within one packet: no
    // event, nothing booked, nothing lost, for a step under 70 ms and over 2 s too (the round-2
    // cases). Review round 2: a CATCH-UP sender (continuous stamps: a burst forward, a pause back)
    // is no relabel; the receiver-step packet is released at once and placed on its raw-stamp
    // landing (one event, nothing booked), the audio on its true landing within 0.2 s.
    let sub_70 = [
        35_000_000_i64,
        50_000_000,
        65_000_000,
        -50_000_000,
        -65_000_000,
    ];
    let over_2s = [2_500_000_000_i64, -2_500_000_000];
    let cases = [STEP_682_NS, -STEP_682_NS, STEP_90_NS]
        .iter()
        .chain(sub_70.iter())
        .chain(over_2s.iter())
        .map(|&w| (Follow::Jump, w))
        .chain(
            [STEP_682_NS, -STEP_682_NS, STEP_90_NS, 50_000_000]
                .iter()
                .map(|&w| (Follow::Burst, w)),
        );
    for (follow, wall_ns) in cases {
        let case = StepCase {
            wall_ns,
            lag_ns: 0,
            follow,
            sender_first_ns: 2 * NS_PER_S,
            burst_jitter_ns: 0,
            connect_backlog: 0,
            step_offset_ns: 0,
        };
        let r = run_step(case, Variant::Production);
        if follow == Follow::Jump {
            assert!(
                r.pendings == 1
                    && r.relabels == 1
                    && r.releases == [(r.releases[0].0, AudioStepRelease::RelabelPending)]
                    && r.releases[0].0 < 0.1
                    && !r.holding_at_end
                    && r.events() == 0
                    && r.jumps == 0
                    && r.overwritten_ms == 0.0
                    && r.dropped_ms == 0.0
                    && r.gap_ms == 0.0
                    && r.av_max_all_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: a sender whose stamps jumped first must stay on its \
                 continuous timeline until the receiver's step resolves it: {r:?}"
            );
        } else {
            assert!(
                r.pendings == 0
                    && r.releases == [(r.releases[0].0, AudioStepRelease::Followed)]
                    && r.releases[0].0 < 0.1
                    && !r.holding_at_end
                    && r.events() <= 1
                    && r.jumps == 0
                    && r.av_settle_s <= 0.2
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: a catch-up sender that stepped first is released at the \
                 receiver's step and placed once: {r:?}"
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
            connect_backlog: 0,
            step_offset_ns: 0,
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
            connect_backlog: 0,
            step_offset_ns: 0,
        },
        Variant::Production,
    );
    assert!(
        steady.events() == 0 && steady.releases.is_empty(),
        "issue 1381: a steady feed under 20 ms arrival jitter never holds: {steady:?}"
    );
}

#[test]
fn a_connect_backlog_never_misreads_a_receiver_step_1381() {
    // review round 2: two packets queued at connect make the first packets read ~2 slots older than
    // the steady transport lag. The nominal age must not keep that seed: a receiver-first step 300 s
    // later is held until the stamps follow (a jumping sender: no event), or released at the catch-up
    // (a bursting sender: one placement) -- never read as a sender that stepped first.
    for (wall_ns, follow) in [
        (50_000_000_i64, Follow::Jump),
        (STEP_682_NS, Follow::Jump),
        (50_000_000, Follow::Burst),
        (STEP_682_NS, Follow::Burst),
    ] {
        let case = StepCase {
            wall_ns,
            lag_ns: LAG_MIN_NS,
            follow,
            sender_first_ns: 0,
            burst_jitter_ns: 0,
            connect_backlog: 2,
            step_offset_ns: 0,
        };
        let r = run_step(case, Variant::Production);
        let lag_s = LAG_MIN_NS as f64 / 1e9;
        let events = if follow == Follow::Jump { 0 } else { 1 };
        assert!(
            r.releases.len() == 1
                && r.releases[0].1 == AudioStepRelease::Followed
                && (r.releases[0].0 - lag_s).abs() < 0.1
                && r.events() == events
                && r.jumps == 0
                && r.av_tail_ms <= AV_BOUND_MS,
            "issue 1381: {case:?}: a connect backlog must not bias the nominal age: {r:?}"
        );
    }
}

// Issue 1381 (design 5900385541) — a sender that RELABELS its stamps at the step (the contract,
// SongPlayer #224): the receiver appends, never splices or resets.

/// The design's scripted steps: the live +260 ms (29.9 20:58 UTC) and +682 ms, and one past each
/// OBS limit the relabel must survive (-1.5 s, +2.5 s over the 2 s timestamp-jump reset).
const RELABEL_STEPS: [i64; 4] = [260_000_000, STEP_682_NS, -1_500_000_000, 2_500_000_000];
/// The sender relabels within one interval: at once (the stamps and the offset jump on the same
/// packet) or 20 ms later (the receiver's step packet still carries the old stamp and starts the
/// hold, the next one releases it -- the split shape the contract's section 6 names).
const RELABEL_LAGS: [u64; 2] = [0, 20_000_000];
/// Design 5901213031 (ROZHODNUTÉ on finding 5900705310): a relabel remainder of r ms is repaid on
/// the placement slew at 1000 ppm, whatever its size -- back within the A/V bound within r seconds,
/// plus this margin.
const REPAY_MARGIN_S: f64 = 1.0;

fn relabel_case(wall_ns: i64, lag_ns: u64) -> StepCase {
    StepCase {
        wall_ns,
        lag_ns,
        follow: Follow::Relabel,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
        connect_backlog: 0,
        step_offset_ns: 0,
    }
}

/// The remainder r = S − N·slot of a relabel, ms (0 ≤ r < one slot): how much earlier the true
/// landing moves than the appended continuation.
fn remainder_ms(wall_ns: i64) -> f64 {
    let slot_ns = PACKET_FRAMES as f64 * NS_PER_S as f64 / RATE as f64;
    (wall_ns as f64 - relabel_slots(wall_ns) as f64 * slot_ns) / 1e6
}

#[test]
fn a_relabelling_sender_is_appended_and_its_remainder_repaid_1381() {
    for wall_ns in RELABEL_STEPS {
        for lag in RELABEL_LAGS {
            let case = relabel_case(wall_ns, lag);
            let r = run_step(case, Variant::Production);
            let rem = remainder_ms(wall_ns);
            // zero samples overwritten, dropped or zero-filled: every packet appended back to back
            assert!(
                r.relabels == 1
                    && r.overwritten_ms == 0.0
                    && r.dropped_ms == 0.0
                    && r.gap_ms == 0.0
                    && r.discs.is_empty(),
                "issue 1381: {case:?}: a relabelled packet must be appended -- never placed r early \
                 (the live 29.9 splice) and never reset past 2 s: {r:?}"
            );
            // the hold never starts on a joint relabel; on the split shape it releases FOLLOWED on
            // the relabel packet, once
            assert!(
                r.releases.len() == usize::from(lag > 0)
                    && r.releases
                        .iter()
                        .all(|x| x.1 == AudioStepRelease::Followed && x.0 < 0.1)
                    && !r.holding_at_end,
                "issue 1381: {case:?}: the skew hold must release followed on the relabel packet \
                 (or never start): {r:?}"
            );
            // the placement error is at most r, and the placement slew repays ALL of it at 1000 ppm
            // (r ms in r s), under the timecode ASRC's half-packet booking band too (design
            // 5901213031): nothing is booked as a placement jump
            let settle_s = rem + REPAY_MARGIN_S;
            assert!(
                r.av_max_ms <= rem + AV_BOUND_MS
                    && r.jumps == 0
                    && r.av_settle_s <= settle_s
                    && r.av_tail_ms <= AV_BOUND_MS,
                "issue 1381: {case:?}: the audio may trail by at most r = {rem:.1} ms and must be \
                 back within ±{AV_BOUND_MS} ms of its true landing within {settle_s:.1} s: {r:?}"
            );
            // review round 1: a joint relabel's one short stamp advance (dur − r) never reads as a
            // rate -- the timecode ASRC's estimate stays on the correct sender's 0 ppm
            assert!(
                r.est_max_ppm <= RATE_BOUND_PPM,
                "issue 1381: {case:?}: the relabel must not move the rate estimate: {r:?}"
            );
        }
    }
}

#[test]
fn without_the_relabel_the_step_splices_or_resets_the_buffer_1381() {
    // anti-tautology: the same senders on the path before the relabel append. A stamp jump of 70 ms
    // or more is placed at its raw landing, r early (r ms of queued audio overwritten, a departure
    // from the continuation); one over 2 s runs handle_ts_jump and drops the whole queued buffer.
    for lag in RELABEL_LAGS {
        for wall_ns in [260_000_000, STEP_682_NS] {
            let case = relabel_case(wall_ns, lag);
            let r = run_step(case, Variant::NoRelabel);
            let rem = remainder_ms(wall_ns);
            assert!(
                r.relabels == 0
                    && (r.overwritten_ms - rem).abs() < 1.0
                    && r.discs.iter().any(|d| (d.1 + rem).abs() < 1.0),
                "issue 1381: {case:?}: before the relabel append the packet is placed r = \
                 {rem:.1} ms early, or the bench proves nothing: {r:?}"
            );
        }
        let over = relabel_case(2_500_000_000, lag);
        let r = run_step(over, Variant::NoRelabel);
        assert!(
            r.dropped_ms > 50.0,
            "issue 1381: {over:?}: before the relabel append a jump over 2 s resets the whole \
             queued buffer, or the bench proves nothing: {r:?}"
        );
    }
}

#[test]
fn a_sender_that_catches_up_or_pauses_takes_todays_path_byte_for_byte_1381() {
    // the relabel is recognised only when the stamps jumped WITH the wall: a catch-up burst after a
    // forward step, a pause after a backward one, a sender that never follows and a steady feed are
    // never read as one, so the bench's ingest runs exactly its path without the relabel -- every
    // landing, every append/placement, every servo input. (This proves no false relabel on the
    // model; the shipped C branch is pinned by the lift harness tests/genlock_audio_relabel_ingest_1381.rs.)
    let mut cases = vec![StepCase {
        wall_ns: 0,
        lag_ns: 0,
        follow: Follow::NoStep,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
        connect_backlog: 0,
        step_offset_ns: 0,
    }];
    for wall_ns in [STEP_682_NS, -STEP_682_NS, STEP_90_NS, 2_500_000_000] {
        for lag in [LAG_MIN_NS, LAG_MAX_NS] {
            cases.push(StepCase {
                wall_ns,
                lag_ns: lag,
                follow: Follow::Burst,
                sender_first_ns: 0,
                burst_jitter_ns: 0,
                connect_backlog: 0,
                step_offset_ns: 0,
            });
        }
    }
    cases.push(StepCase {
        wall_ns: STEP_682_NS,
        lag_ns: 0,
        follow: Follow::Never,
        sender_first_ns: 0,
        burst_jitter_ns: 0,
        connect_backlog: 0,
        step_offset_ns: 0,
    });
    for case in cases {
        let (prod, prod_trace) = run_step_traced(case, Variant::Production);
        let (before, before_trace) = run_step_traced(case, Variant::NoRelabel);
        assert!(
            prod.relabels == 0 && prod_trace.len() > 1000 && prod_trace == before_trace,
            "issue 1381: {case:?}: a sender whose stamps never jumped with the wall must take \
             today's path byte for byte: {prod:?} vs {before:?}"
        );
    }
}

// Issue 1381 (design 5901213031, receiver slice 2): a PENDING relabel (the sender's box stepped
// first) and the relabel remainder repaid on the placement slew.
#[path = "asrc_timecode_pending_bench.rs"]
mod pending;
