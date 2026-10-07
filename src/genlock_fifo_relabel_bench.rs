//! Issue 1372 part B — the two-clock bench of the receive-FIFO relabel: the nightly fleet date
//! step replayed against the three genlock input kinds of the rig, frame by frame, through the
//! production release decisions.
//!
//! - **Steps:** +1543.16 ms (6.10.2026, dantesync 1.12, the whole day's error) and +1600 ms (the
//!   same night quantized to 200 ms by dantesync 1.16.0, part A).
//! - **Clocks:** every box's wall clock steps by the same `S`; the sender steps 0–30 ms before or
//!   after the receiver (`sender_offset`, positive = after). The monotonic clocks never step. Both
//!   OBS boxes run the production render tick (the one-tick re-grid of `crate::genlock_wall_step`).
//! - **Inputs:**
//!   - the deep N==1 `NDI 2ME PGM` (strih-lx program → stream, pin 1026 ms): the production N==1
//!     FIFO port (`crate::genlock_grid_bench::Fifo`) fed by a 30 fps OBS sender that floors its wall
//!     at send (20.7 ± 1.8 ms after its tick, the #1355 measurement) + 1 ms of network;
//!   - the shallow cg feed (resolume cg OBS → strih-lx, pin 3 ms): the same port, emits 0–30 ms
//!     after the sender's tick, 1–3 ms of network;
//!   - an N>=2 60 → 30 camera (a cambox → strih-lx, pin 3 ms): the production grid release
//!     (`crate::genlock_n2_grid`), captures on the 60 fps per-second grid, 17–50 ms arrival lag
//!     delivered in order (inside the 66.7 ms target, like the measured strih-lx distribution).
//! - **Booking:** at the release, from the release's own bracketed wall read
//!   (`crate::genlock_fifo_relabel::Booking`), then `RelabelState::apply` / `receive` exactly as
//!   the C FIFO wires them. The anti-tautology runs book from the render tick's end-of-tick detector
//!   instead (one release late) and run with no relabel at all (today).
//! - **What the viewer sees:** every frame carries the sender's content id; per tick the on-air id
//!   must advance by one source frame (N==1) or two (N>=2). A smaller advance is a REPEAT, a larger
//!   one a SKIP, a negative one a backward present. The step's cost is the run with the step minus
//!   the identical run without it (same seeds, same frames).
//!
//! A cambox stamps through a monotonic-to-realtime offset it re-samples every 100 frames
//! (`crate::genlock_stamp::OFFSET_RESAMPLE_INTERVAL_FRAMES`), so its stamps follow the box's own
//! date step up to ~1.67 s late. `cambox_stale_offset_cost_is_reported_1372` reports what that lag
//! still costs with the one-latency-window rule (Design-question on the ticket).

use crate::genlock_backlog::{phase_pinned_deadline, source_interval_from_stamps};
use crate::genlock_fifo_relabel::{Booking, RelabelState};
use crate::genlock_grid::{
    grid_advance_ns, grid_next_boundary_ns, per_second_floor, StampTrack, NS_PER_SECOND,
    UNITS_100NS_PER_SECOND,
};
use crate::genlock_grid_bench::{BenchConfig, Fifo, GridModel, TickCounters, CANVAS_INTERVAL_NS};
use crate::genlock_n1_depth::n1_tick_is_on_grid;
use crate::genlock_n2_grid::{
    n2_select, n2_source_interval_ns, n2_target_stamp_ns, n2_tick_ns, N2Kind,
};
use crate::genlock_wall_step::{deadline_ns, WallStepState};
use std::collections::VecDeque;

const I30: u64 = CANVAS_INTERVAL_NS;
const I60: u64 = NS_PER_SECOND / 60;
const MONO0: u64 = 50_000_000_000_000;
/// 20 s before the 7.10.2026 02:00:00 UTC step second.
const WALL0: u64 = (1_791_338_400 - 20) * NS_PER_SECOND;
/// The receiver's step: 02:00:11.549, mid-frame (the logged instant).
const STEP_AT: u64 = MONO0 + 20 * NS_PER_SECOND + 11_549_000;
const END: u64 = STEP_AT + 32 * NS_PER_SECOND;
const MEASURE_FROM: u64 = STEP_AT - 2 * NS_PER_SECOND;

/// The measured 6.10.2026 step and the dantesync 1.16.0 quantized one.
const S_MEASURED: i64 = 1_543_160_000;
const S_QUANTIZED: i64 = 1_600_000_000;
const SENDER_OFFSETS_MS: [i64; 5] = [-30, -15, 0, 15, 30];

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn uniform(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }

    fn normal(&mut self) -> f64 {
        (0..12)
            .map(|_| (self.next() >> 11) as f64 / (1u64 << 53) as f64)
            .sum::<f64>()
            - 6.0
    }
}

/// A box's wall clock at a monotonic instant: the date steps by `step` at `at`.
fn wall(mono: u64, step: i64, at: u64) -> u64 {
    let w = WALL0 + (mono - MONO0);
    if mono >= at {
        w.wrapping_add(step as u64)
    } else {
        w
    }
}

/// One OBS box's render tick (obs-video.c `genlock_next_deadline` + `video_sleep`), production
/// re-grid on.
struct Ticker {
    det: WallStepState,
    sched: u64,
}

impl Ticker {
    fn new(step: i64, at: u64) -> Self {
        let w = wall(MONO0, step, at);
        Ticker {
            det: WallStepState::new(),
            sched: MONO0 + (grid_next_boundary_ns(w, I30) - w),
        }
    }

    fn advance(&mut self, call: u64, step: i64, at: u64) {
        let wall_read = wall(call + 1_000, step, at);
        let mono = call + 2_000;
        let s = self.det.observe(call, wall_read, mono);
        let target = mono + (grid_next_boundary_ns(wall_read, I30) - wall_read);
        let stock = self.sched + I30;
        let regrid = self.det.regrid_due(s, target, stock);
        let deadline = deadline_ns(target, stock, regrid);
        let now = mono + 50_000;
        self.sched = if deadline > now {
            deadline
        } else {
            self.sched + I30 * ((now - self.sched) / I30).max(1)
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    /// The deep N==1 stream `NDI 2ME PGM`.
    Deep,
    /// The shallow N==1 cg feed on strih-lx.
    Shallow,
    /// An N>=2 60 → 30 cambox input on strih-lx.
    Camera,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Book {
    /// No relabel (today).
    Off,
    /// The production booking: at the release, from its own wall read.
    Release,
    /// Booked by the render tick's end-of-tick detector (one release late).
    TickEnd,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    input: Input,
    step: i64,
    /// The sender's date step relative to the receiver's, ns (positive = after).
    sender_offset: i64,
    /// How long after its box's step a cambox's STAMPS follow (the stale mono→real offset).
    stamp_lag: u64,
    book: Book,
}

/// One received frame: arrival (receiver monotonic), stamp, content id.
#[derive(Clone, Copy)]
struct Frame {
    arrival: u64,
    stamp: u64,
    id: u64,
}

fn sender_step_at(c: &Case) -> u64 {
    STEP_AT.wrapping_add(c.sender_offset as u64)
}

/// A 30 fps OBS program sender: its own render tick (re-grid on), the frame handed to NDI
/// `delay` after the tick fires and stamped with the per-second floor of the wall at that instant.
fn obs_sender(c: &Case, seed: u64, deep: bool) -> Vec<Frame> {
    let at = sender_step_at(c);
    let mut rng = Rng(seed);
    let mut tx = Ticker::new(c.step, at);
    let mut out = Vec::new();
    let mut last_arrival = 0;
    let mut id = 0;
    while tx.sched < END + NS_PER_SECOND {
        let fire = tx.sched + rng.uniform(0, 200_000);
        let delay = if deep {
            (20_700_000.0 + 1_800_000.0 * rng.normal()).max(0.0) as u64
        } else {
            rng.uniform(0, 30_000_000)
        };
        let net = if deep {
            1_000_000
        } else {
            rng.uniform(1_000_000, 3_000_000)
        };
        let emit = fire + delay;
        let stamp =
            per_second_floor(wall(emit, c.step, at) / 100, 30, UNITS_100NS_PER_SECOND) * 100;
        let arrival = (emit + net).max(last_arrival + 1);
        last_arrival = arrival;
        out.push(Frame { arrival, stamp, id });
        id += 1;
        tx.advance(fire + rng.uniform(3_000_000, 10_000_000), c.step, at);
    }
    out
}

/// A 60 fps cambox: captures 1 µs after each per-second 60 fps grid point (of the OLD epoch; a
/// quantized step keeps that phase), stamps the per-second floor of the capture on its wall clock —
/// which follows the box's step `stamp_lag` late — and delivers 17–50 ms later, in order.
fn camera_sender(c: &Case, seed: u64) -> Vec<Frame> {
    let at = sender_step_at(c);
    let stamp_at = at + c.stamp_lag;
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    let mut last_arrival = 0;
    let first = grid_next_boundary_ns(WALL0, I60);
    for k in 0.. {
        let capture = MONO0 + (grid_advance_ns(first, k, I60) - WALL0) + 1_000;
        if capture > END + NS_PER_SECOND {
            break;
        }
        let stamp = per_second_floor(
            wall(capture, c.step, stamp_at) / 100,
            60,
            UNITS_100NS_PER_SECOND,
        ) * 100;
        let arrival = (capture + rng.uniform(17_000_000, 50_000_000)).max(last_arrival + 1);
        last_arrival = arrival;
        out.push(Frame {
            arrival,
            stamp,
            id: k,
        });
    }
    out
}

/// The grid-exact N>=2 release (obs-source.c `genlock_release_tick_n2_grid` + the sticky multiple),
/// on stamps with their content ids.
#[derive(Default)]
struct N2Rx {
    q: VecDeque<(u64, u64)>,
    boundary: u64,
    last_known_n: u32,
    late_holds: u64,
}

impl N2Rx {
    fn tick(&mut self, tick_wall: u64, wall_now: u64, pin_ns: u64) -> Option<u64> {
        if self.q.is_empty() {
            return None;
        }
        let window: Vec<u64> = self.q.iter().take(6).map(|f| f.0).collect();
        let n = match source_interval_from_stamps(&window) {
            Some(si) => {
                self.last_known_n = ((I30 + si / 2) / si).max(1) as u32;
                self.last_known_n
            }
            None => self.last_known_n.max(1),
        };
        if n < 2 {
            // an inconclusive start-up tick: present the head (never reached after warm-up)
            let (stamp, id) = self.q.pop_front().expect("non-empty");
            self.boundary = stamp + I30;
            return Some(id);
        }
        let on_grid = n1_tick_is_on_grid(tick_wall, I30);
        let target = n2_target_stamp_ns(
            n2_tick_ns(tick_wall, wall_now, I30, on_grid),
            pin_ns,
            I30,
            n,
        );
        let stamps: Vec<u64> = self.q.iter().map(|f| f.0).collect();
        let pick = n2_select(&stamps, target, n2_source_interval_ns(I30, n));
        if pick.kind == N2Kind::Hold {
            let present_ts = phase_pinned_deadline(wall_now.saturating_sub(pin_ns), I30);
            if self.boundary != 0 && present_ts >= self.boundary {
                self.late_holds += 1;
            }
            return None;
        }
        self.q.drain(..pick.index);
        let (stamp, id) = self.q.pop_front().expect("the pick");
        self.boundary = stamp + I30;
        Some(id)
    }
}

/// What the viewer saw from `MEASURE_FROM` to the end, plus the FIFO counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Seen {
    /// Source frames shown again / never shown, against one (N==1) or two (N>=2) per tick.
    repeats: i64,
    skips: i64,
    backward: i64,
    late_holds: i64,
    dropped_due: i64,
    relocks: i64,
    underruns: i64,
    relabelled: i64,
}

impl Seen {
    fn minus(self, b: Seen) -> Seen {
        Seen {
            repeats: self.repeats - b.repeats,
            skips: self.skips - b.skips,
            backward: self.backward - b.backward,
            late_holds: self.late_holds - b.late_holds,
            dropped_due: self.dropped_due - b.dropped_due,
            relocks: self.relocks - b.relocks,
            underruns: self.underruns - b.underruns,
            relabelled: self.relabelled - b.relabelled,
        }
    }

    fn visible(&self) -> i64 {
        self.repeats + self.skips + self.backward
    }
}

/// Run one case: the receiver ticks, receives every frame that arrived before the tick, books and
/// applies the step at the release (or not), releases, and scores the on-air id.
fn run(c: &Case) -> Seen {
    let seed = 0x1372_b000 ^ (c.sender_offset as u64);
    let frames = match c.input {
        Input::Deep => obs_sender(c, seed, true),
        Input::Shallow => obs_sender(c, seed, false),
        Input::Camera => camera_sender(c, seed),
    };
    let expect: i64 = if c.input == Input::Camera { 2 } else { 1 };
    let latency_ms: u32 = if c.input == Input::Deep { 1026 } else { 3 };
    let reserve_ns = u64::from(latency_ms) * 1_000_000;
    let mut cfg = BenchConfig::live_2026_09_24(GridModel::Production);
    cfg.latency_ms = latency_ms;

    let mut fifo = Fifo::default();
    let mut ids: VecDeque<u64> = VecDeque::new();
    let mut n2 = N2Rx::default();
    let mut track = StampTrack::default();
    let mut relabel = RelabelState::default();
    let mut booking = Booking::new();
    let mut rng = Rng(seed ^ 0x00ff_00ff);
    let mut rx = Ticker::new(c.step, STEP_AT);
    let mut next = 0usize;
    let mut on_air: Option<u64> = None;
    let mut seen = Seen::default();
    while rx.sched < END {
        let fire = rx.sched + rng.uniform(0, 300_000);
        // the producer: every frame that arrived by now, relabelled on the push like the C FIFO
        while next < frames.len() && frames[next].arrival <= fire {
            let f = frames[next];
            next += 1;
            let stamp = if c.book == Book::Off {
                f.stamp
            } else {
                relabel.receive(track.last_ts, f.stamp, track.min_delta_ns, f.arrival)
            };
            track.observe(stamp);
            if c.input == Input::Camera {
                n2.q.push_back((stamp, f.id));
            } else {
                fifo.receive(wall(f.arrival, c.step, STEP_AT), stamp);
                ids.push_back(f.id);
            }
        }
        // the release: the bracketed wall read, the booking, the apply, then the release itself
        let wall_now = wall(fire + 1_000, c.step, STEP_AT);
        let scheduled = rx.sched.wrapping_add(wall_now.wrapping_sub(fire));
        let queued = if c.input == Input::Camera {
            !n2.q.is_empty()
        } else {
            !fifo.queue.is_empty()
        };
        if queued && c.book != Book::Off {
            if c.book == Book::Release {
                booking.observe(fire, wall_now, fire + 2_000);
            }
            if c.input == Input::Camera {
                let mut q: Vec<u64> = n2.q.iter().map(|f| f.0).collect();
                relabel.apply(
                    &booking,
                    &mut q,
                    &mut n2.boundary,
                    &mut track.last_ts,
                    I30,
                    track.min_delta_ns,
                    reserve_ns,
                    wall_now,
                    fire + 2_000,
                );
                for (slot, ts) in n2.q.iter_mut().zip(q) {
                    slot.0 = ts;
                }
            } else {
                let mut boundary = *fifo.locked_boundary_mut();
                relabel.apply(
                    &booking,
                    fifo.queue.make_contiguous(),
                    &mut boundary,
                    &mut track.last_ts,
                    I30,
                    track.min_delta_ns,
                    reserve_ns,
                    wall_now,
                    fire + 2_000,
                );
                *fifo.locked_boundary_mut() = boundary;
            }
        }
        let presented = if c.input == Input::Camera {
            n2.tick(scheduled, wall_now, reserve_ns)
        } else {
            let before = fifo.queue.len();
            let mut tc = TickCounters::default();
            fifo.tick(&cfg, wall_now, scheduled, &mut tc);
            let mut last = None;
            for _ in 0..before - fifo.queue.len() {
                last = ids.pop_front();
            }
            if rx.sched >= MEASURE_FROM {
                seen.late_holds += tc.late_holds as i64;
                seen.dropped_due += tc.dropped_due as i64;
                seen.relocks += tc.relocks as i64;
                seen.underruns += tc.underruns as i64;
            }
            if fifo.presented_now {
                last
            } else {
                None
            }
        };
        if let Some(id) = presented {
            if rx.sched >= MEASURE_FROM {
                if let Some(prev) = on_air {
                    let d = id as i64 - prev as i64;
                    if d < 0 {
                        seen.backward += 1;
                    } else if d < expect {
                        seen.repeats += expect - d;
                    } else {
                        seen.skips += d - expect;
                    }
                }
            }
            on_air = Some(id);
        } else if rx.sched >= MEASURE_FROM && on_air.is_some() {
            // a held tick shows the previous frame again
            seen.repeats += expect;
        }
        if c.book == Book::TickEnd {
            let call = fire + 5_000_000;
            booking.observe(call, wall(call + 1_000, c.step, STEP_AT), call + 2_000);
        }
        rx.advance(fire + rng.uniform(3_000_000, 10_000_000), c.step, STEP_AT);
    }
    if c.input == Input::Camera {
        seen.late_holds = n2.late_holds as i64;
    }
    seen.relabelled = relabel.relabelled as i64;
    seen
}

/// The step's cost: the case minus the identical run with no step.
fn cost(c: &Case) -> Seen {
    let base = Case { step: 0, ..*c };
    run(c).minus(run(&base))
}

fn case(input: Input, step: i64, offset_ms: i64, book: Book) -> Case {
    Case {
        input,
        step,
        sender_offset: offset_ms * 1_000_000,
        stamp_lag: 0,
        book,
    }
}

const INPUTS: [Input; 3] = [Input::Deep, Input::Shallow, Input::Camera];

/// THE CLAIM: the quantized +1600 ms step costs NOTHING on any input, whichever box steps first
/// (0–30 ms either way): no repeated, skipped or backward frame, no late hold, relock or underrun.
#[test]
fn the_quantized_step_costs_no_frame_on_any_input_1372() {
    for input in INPUTS {
        for off in SENDER_OFFSETS_MS {
            let c = cost(&case(input, S_QUANTIZED, off, Book::Release));
            assert_eq!(
                (c.repeats, c.skips, c.backward),
                (0, 0, 0),
                "issue 1372: {input:?}, sender {off:+} ms: the quantized step must cost no frame: \
                 {c:?}"
            );
            assert_eq!(
                (c.late_holds, c.relocks, c.underruns),
                (0, 0, 0),
                "issue 1372: {input:?}, sender {off:+} ms: {c:?}"
            );
            assert!(
                c.relabelled > 0,
                "{input:?}, sender {off:+} ms: the step must be relabelled: {c:?}"
            );
        }
    }
}

/// Anti-tautology: the same quantized step with no relabel (today) and with the step booked one
/// release late (from the render tick's end-of-tick detector) costs frames on every input.
#[test]
fn without_the_relabel_or_booked_late_the_step_costs_frames_1372() {
    for input in INPUTS {
        let off = cost(&case(input, S_QUANTIZED, 0, Book::Off));
        eprintln!("{input:?} today: {off:?}");
        assert!(
            off.visible() > 0,
            "{input:?}: today the quantized step must cost the viewer frames: {off:?}"
        );
    }
    for input in [Input::Deep, Input::Camera] {
        let late = cost(&case(input, S_QUANTIZED, 0, Book::TickEnd));
        eprintln!("{input:?} booked one release late: {late:?}");
        assert!(
            late.visible() > 0,
            "{input:?}: a booking one release late must cost frames: {late:?}"
        );
    }
}

/// The measured +1543.16 ms step (not a whole number of frames): report it next to today's cost
/// (run with `--nocapture` for the table). Measured: both N==1 inputs 0 / 0 at every sender offset
/// (today 1 / 1, the live `late_holds` +1 / `dropped_due` +1); the N>=2 camera 1 / 1 (2 / 2 with
/// the sender 30 ms late) — the 0.59-frame phase move the quantum removes, one more than today
/// when the sender steps 30 ms first (today's grid release absorbs that case).
#[test]
fn the_unquantized_step_cost_is_reported_1372() {
    eprintln!("input    sender  S         relabel(rep/skip/back late)  today(rep/skip/back late)");
    for input in INPUTS {
        for off in SENDER_OFFSETS_MS {
            for step in [S_MEASURED, S_QUANTIZED] {
                let on = cost(&case(input, step, off, Book::Release));
                let today = cost(&case(input, step, off, Book::Off));
                eprintln!(
                    "{:<8} {off:+4} ms {:>8.2} ms  {:>3}/{:>3}/{:>2} {:>3}            \
                     {:>3}/{:>3}/{:>2} {:>3}",
                    format!("{input:?}"),
                    step as f64 / 1e6,
                    on.repeats,
                    on.skips,
                    on.backward,
                    on.late_holds,
                    today.repeats,
                    today.skips,
                    today.backward,
                    today.late_holds
                );
                // The N==1 conveyors present by FIFO order, so the fraction never shows there; an
                // N>=2 camera picks by stamp on the grid, and a relabel by the exact (off-grid) step
                // moves its phase by the fraction: at most two repeats + two skips.
                if input == Input::Camera {
                    assert!(
                        on.repeats <= 2 && on.skips <= 2 && on.backward == 0,
                        "{input:?}, sender {off:+} ms, step {step}: {on:?}"
                    );
                } else {
                    assert_eq!(
                        on.visible(),
                        0,
                        "{input:?}, sender {off:+} ms, step {step}: {on:?}"
                    );
                }
            }
        }
    }
}

/// A cambox stamps through a mono→real offset re-sampled every 100 frames, so its stamps follow its
/// own step up to ~1.67 s late (60 fps). Report what that costs a strih-lx camera input (pin 3 ms,
/// one latency window ~67 ms) with the relabel, against today. Measured (+1600 ms): lag 0 → 0 / 0,
/// 400 → 4 / 4, 800 → 6 / 6, 1200 → 9 / 9, 1600 → 11 / 11 (today one frame more each); a 2 s
/// window gives 0 / 0 at every lag — the Design-question on the ticket.
#[test]
fn cambox_stale_offset_cost_is_reported_1372() {
    eprintln!("cambox stamp lag  relabel(rep/skip/back late)  today(rep/skip/back late)");
    for lag_ms in [0u64, 400, 800, 1_200, 1_600] {
        let c = Case {
            stamp_lag: lag_ms * 1_000_000,
            ..case(Input::Camera, S_QUANTIZED, 0, Book::Release)
        };
        let on = cost(&c);
        let today = cost(&Case {
            book: Book::Off,
            ..c
        });
        eprintln!(
            "{lag_ms:>6} ms        {:>3}/{:>3}/{:>2} {:>3}            {:>3}/{:>3}/{:>2} {:>3}",
            on.repeats,
            on.skips,
            on.backward,
            on.late_holds,
            today.repeats,
            today.skips,
            today.backward,
            today.late_holds
        );
        if lag_ms == 0 {
            assert_eq!(on.visible(), 0, "a prompt cambox costs nothing: {on:?}");
        }
        assert!(
            on.visible() <= today.visible(),
            "lag {lag_ms}: {on:?} vs {today:?}"
        );
    }
}
