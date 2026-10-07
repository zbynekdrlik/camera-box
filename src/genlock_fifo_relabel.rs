//! Issue 1372 part B — the genlock receive FIFO relabels its OLD-EPOCH frames by the booked wall
//! step, so the nightly dantesync fleet date step costs no frame at the receiver on every input
//! whose sender's stamps follow its own step (the OBS senders; a cambox's stamps lag its step by up
//! to ~1.67 s, the open Design-question 6032724613).
//!
//! ## Why this module exists
//!
//! A genlock source's frames carry the sender's wall clock as their stamp (DistroAV floors the wall
//! at emit; a cambox floors its capture instant) and the receive FIFO releases them against the
//! receiver's own wall clock. A coordinated fleet DATE step moves every wall clock by the same `S`
//! at (to the ms) the same instant. The render tick re-grids in one tick (`crate::genlock_wall_step`),
//! but the frames already queued — and the ones a sender stamped just before ITS step — keep the
//! OLD label and suddenly read `S` older (or younger) than the release target. Live, 6.10. and
//! 7.10.2026 (+1543 / +1549 ms): the stream `NDI 2ME PGM` `late_holds` +1 / `dropped_due` +1, the
//! cambox `Zaloha kamera` `late_holds` 0 → 28 / 29, and the stream program recording 5 repeats +
//! 4 skips. The decision (design 6026394143, Approach 1): relabel those frames by `+S`.
//!
//! ## What it decides
//!
//! - **Booking** ([`Booking::observe`]): the release path runs the SAME detector as the render tick
//!   (`WallStepState::observe`: a bracketed mono/wall/mono read, a jump beyond 2 ms between two
//!   trusted reads is a step) on the wall read it releases against, and books the step (`seq`,
//!   `S`, its wall and monotonic instant). The render tick's own detector runs at the END of a tick
//!   (`video_sleep`), after that tick's release, so it cannot book the step for the release that
//!   follows the step; this one books it before it. It is fed only by genlock releases, so a
//!   trusted read more than [`BOOK_MAX_GAP_NS`] after the previous trusted one re-seeds instead of
//!   booking: a step (or a raw-clock drift) from a silent stretch is never booked as new.
//! - **Only a source that was releasing at the step relabels** ([`RelabelState::apply`]): a source
//!   with no previous release, or one more than [`APPLY_MAX_GAP_NS`] ago (a new source, one silent
//!   across the step, a resumed traveling feed), takes the booking without relabelling — its queue
//!   holds frames that arrived after the step.
//! - **Queued frames** ([`plan`], applied once per source per booking by [`RelabelState::apply`]):
//!   in FIFO order the old epoch is the prefix before the first adjacent stamp jump that carries `S`
//!   within one frame ([`delta_carries_step`]); with no such jump in the queue, the queue shares the
//!   epoch of the last presented frame — old, unless a stamp jump that carries `S` arrived recently
//!   ([`sender_stepped_before`]: the sender stepped first and its new-epoch frames were already
//!   presented). The locked boundary moves with the presented frame, the stamp tracker's last stamp
//!   with the newest frame.
//! - **Arriving frames** ([`arrival_add`], applied on every push by [`RelabelState::receive`]): while
//!   the sender is still on the old epoch, a frame whose `stamp + S` continues the relabelled timeline
//!   within one frame — and whose raw stamp does not — is relabelled. A raw stamp that continues the
//!   timeline means the sender has stepped: relabelling ends. A stamp that continues neither (a
//!   sender restart, a song change) is a real jump: never relabelled, relabelling ends. The window is
//!   ONE latency window on the sender's own stamp timeline: a relabelled stamp after the step instant
//!   plus the source's presented age (at most [`WINDOW_MAX_AGE_NS`], at least its pin) ends it
//!   ([`window_ns`]).
//! - **Only a step of [`MIN_STEP_NS`] or more** is relabelled ([`step_relabels`]): below two canvas
//!   frames a continuous stamp and a stepped one cannot be told apart within one frame. dantesync
//!   1.16.0 steps the date by 0 or a multiple of 200 ms; a smaller step keeps the one-tick re-grid.
//!
//! The audio already relabels the same step on its own path (`genlock_audio_relabel` and the skew
//! hold, issue 1381); the video uses the same `S` (the same wall jump against the same media clock)
//! and the same end condition — the sender's stamps following the step — so A/V stay paired.
//!
//! "One frame" is the canvas interval (the receiver's render tick); a source's own stamp step is the
//! stamp tracker's learned step (the C `genlock_rx_min_delta_ns`), the canvas interval when none is
//! learned yet.
//!
//! The C twin is `vendor/obs-studio/libobs/obs-genlock-fifo-relabel.h` (stdint + the wall-step
//! header only); `tests/genlock_fifo_relabel_parity_1372.rs` compiles it and requires byte-identical
//! results. The two-clock bench is the test-only `crate::genlock_fifo_relabel_bench`.

use crate::genlock_wall_step::{wall_offset_ns, WallStepState};

/// The smallest wall step the FIFO relabels, ns. Below two canvas frames (66.7 ms at 30 fps) a
/// stamp that continues the timeline and one that carries the step cannot be told apart within one
/// frame; 100 ms is half the dantesync daily step quantum (200 ms), so every quantized step is above
/// it and a 50 ms legacy micro step keeps the one-tick re-grid.
pub const MIN_STEP_NS: i64 = 100_000_000;

/// A raw stamp delta more than this off the source's own step is remembered as a stamp JUMP (the
/// "sender stepped first" evidence [`sender_stepped_before`] reads), ns. Half of [`MIN_STEP_NS`]: a
/// relabelled step always lands beyond it, a one-slot gap at 30 fps (33 ms off the step) never.
pub const JUMP_RECORD_DEV_NS: u64 = (MIN_STEP_NS / 2) as u64;

/// A trusted booking read more than this after the previous trusted one RE-SEEDS the booking
/// detector instead of booking, ns. The detector is fed only by genlock releases; after a stretch
/// with none (every genlock queue empty) the offset it holds is stale, and a step (or a raw-clock
/// drift) that happened long ago must never be booked as if it happened now.
pub const BOOK_MAX_GAP_NS: u64 = 1_000_000_000;

/// A source whose previous release is more than this before the current one was not releasing at
/// the step (a new source, one silent across it, a resumed traveling feed): it takes the booking
/// WITHOUT relabelling, ns. Its queue holds frames that arrived after the step. A releasing source
/// has a frame queued on nearly every tick, so the bound only absorbs a few empty ticks. Kept short
/// because a shallow source whose sender went silent across the step resumes with post-step
/// frames only, and [`plan`] cannot see the boundary behind the outage (the delta across it is the
/// outage + the step, not "the step within one frame"): past this bound it is not relabelled.
pub const APPLY_MAX_GAP_NS: u64 = 250_000_000;

/// The presented age a window takes is capped here, ns: a stale locked boundary must never open a
/// window of hours. The pin itself is never cut (the window is at least the pin).
pub const WINDOW_MAX_AGE_NS: u64 = 2_000_000_000;

/// How far a stamp delta is from the source's own step: `|delta − src_ns|`, ns. The subtraction
/// wraps like the C `int64_t` arithmetic. `src_ns = 0` gives `|delta|`.
///
/// Mirror of the C `genlock_fifo_relabel_dev_ns()`.
pub fn dev_ns(delta_ns: i64, src_ns: u64) -> u64 {
    delta_ns.wrapping_sub(src_ns as i64).unsigned_abs()
}

/// Whether a stamp delta continues a timeline: within one frame (`frame_ns`) of the source's own
/// step — a duplicate and a one-slot gap of a source no slower than the canvas included.
///
/// Mirror of the C `genlock_fifo_relabel_continuous()`.
pub fn continuous(delta_ns: i64, src_ns: u64, frame_ns: u64) -> bool {
    dev_ns(delta_ns, src_ns) <= frame_ns
}

/// Whether a stamp delta carries the booked step `step_ns`: the delta minus the step continues the
/// timeline within one frame, and closer than the delta itself does.
///
/// Mirror of the C `genlock_fifo_relabel_delta_carries_step()`.
pub fn delta_carries_step(delta_ns: i64, step_ns: i64, src_ns: u64, frame_ns: u64) -> bool {
    let off = dev_ns(delta_ns.wrapping_sub(step_ns), src_ns);
    off <= frame_ns && off < dev_ns(delta_ns, src_ns)
}

/// Whether a booked step is relabelled at all: `|step| >= MIN_STEP_NS`.
///
/// Mirror of the C `genlock_fifo_relabel_step_relabels()`.
pub fn step_relabels(step_ns: i64) -> bool {
    step_ns.unsigned_abs() >= MIN_STEP_NS as u64
}

/// Whether a raw received stamp delta is remembered as a stamp jump.
///
/// Mirror of the C `genlock_fifo_relabel_jump_recorded()`.
pub fn jump_recorded(delta_ns: i64, src_ns: u64) -> bool {
    dev_ns(delta_ns, src_ns) > JUMP_RECORD_DEV_NS
}

/// Whether the sender's own step reached this source BEFORE the receiver booked its step: a
/// remembered stamp jump carries the step, and arrived no more than one latency window
/// (`window_ns`) before the step was booked (`booking_mono_ns`, the monotonic clock: it never
/// steps). `jump_ns = 0` = none remembered.
///
/// Mirror of the C `genlock_fifo_relabel_sender_stepped_before()`.
pub fn sender_stepped_before(
    jump_ns: i64,
    jump_mono_ns: u64,
    step_ns: i64,
    src_ns: u64,
    frame_ns: u64,
    booking_mono_ns: u64,
    window_ns: u64,
) -> bool {
    jump_ns != 0
        && delta_carries_step(jump_ns, step_ns, src_ns, frame_ns)
        && booking_mono_ns <= jump_mono_ns.saturating_add(window_ns)
}

/// ONE latency window, ns: the source's presented age at the booking (at most
/// [`WINDOW_MAX_AGE_NS`]), at least its configured latency (the pin). Old-epoch frames arrive for
/// at most about that long after a sender's step.
///
/// Mirror of the C `genlock_fifo_relabel_window_ns()`.
pub fn window_ns(reserve_ns: u64, presented_age_ns: u64) -> u64 {
    reserve_ns.max(presented_age_ns.min(WINDOW_MAX_AGE_NS))
}

/// The box-wide step booking the release path keeps: the detector, a sequence number every source
/// compares with its own, and the booked step.
///
/// Mirror of the C `struct genlock_fifo_relabel_booking`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Booking {
    detector: WallStepState,
    /// Steps booked so far (`0` = none yet).
    pub seq: u64,
    /// The last booked step, ns (`+` = the wall moved forward).
    pub step_ns: i64,
    /// The wall read that detected it (after the step).
    pub wall_ns: u64,
    /// The monotonic read that closed that bracket.
    pub mono_ns: u64,
    /// The monotonic read that closed the last TRUSTED bracket (`0` = none yet).
    pub last_mono_ns: u64,
}

impl Booking {
    /// A booking with no reading yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one bracketed read (mono, wall, mono) — the one the release reads its `wall_now` with.
    /// Returns `true` when it booked a new step. An untrusted read decides nothing (the detector's
    /// rules, `crate::genlock_wall_step`). A trusted read more than [`BOOK_MAX_GAP_NS`] after the
    /// previous trusted one re-seeds the detector instead: its offset is stale.
    ///
    /// Mirror of the C `genlock_fifo_relabel_book()`.
    pub fn observe(&mut self, mono_before: u64, wall: u64, mono_after: u64) -> bool {
        if wall_offset_ns(mono_before, wall, mono_after).is_some() {
            if self.last_mono_ns != 0
                && mono_after.saturating_sub(self.last_mono_ns) > BOOK_MAX_GAP_NS
            {
                self.detector = WallStepState::new();
            }
            self.last_mono_ns = mono_after;
        }
        let step = self.detector.observe(mono_before, wall, mono_after);
        if step == 0 {
            return false;
        }
        self.seq = self.seq.wrapping_add(1);
        self.step_ns = step;
        self.wall_ns = wall;
        self.mono_ns = mono_after;
        true
    }
}

/// Which of a source's frames are old-epoch at a booking.
///
/// Mirror of the C `struct genlock_fifo_relabel_plan`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The leading queued frames (FIFO order) to relabel.
    pub queue_old: usize,
    /// The last presented frame is old-epoch (its locked boundary moves).
    pub prev_old: bool,
    /// The newest received frame is old-epoch: the sender has not stepped yet (arrivals are judged).
    pub newest_old: bool,
}

/// Split a source's stamp sequence at the booked step: `prev` the last presented stamp (`0` =
/// none), `stamps` the queue in FIFO order. The first adjacent delta (from `prev` on) that carries
/// the step is the epoch boundary; with none, every frame shares one epoch — new when the sender
/// stepped before (`stepped_before`), else old.
///
/// Mirror of the C `genlock_fifo_relabel_plan()`.
pub fn plan(
    prev: u64,
    stamps: &[u64],
    step_ns: i64,
    src_ns: u64,
    frame_ns: u64,
    stepped_before: bool,
) -> Plan {
    let mut pred = prev;
    for (k, &ts) in stamps.iter().enumerate() {
        if pred != 0 && delta_carries_step(ts.wrapping_sub(pred) as i64, step_ns, src_ns, frame_ns)
        {
            return Plan {
                queue_old: k,
                prev_old: prev != 0,
                newest_old: false,
            };
        }
        pred = ts;
    }
    if stepped_before {
        Plan::default()
    } else {
        Plan {
            queue_old: stamps.len(),
            prev_old: prev != 0,
            newest_old: true,
        }
    }
}

/// The arrival window a booking opens on one source.
///
/// Mirror of the C `struct genlock_fifo_relabel_arrival`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// The step arrivals are relabelled by (`0` = no window).
    pub step_ns: i64,
    /// The latest relabelled stamp the window takes (the step instant + one latency window).
    pub until_ns: u64,
    /// One frame (the canvas interval) — the continuity tolerance.
    pub frame_ns: u64,
    /// The sender has not stepped yet: arrivals are judged.
    pub old_epoch: bool,
}

/// What to add to an ARRIVING stamp (`0` or the step). `prev` is the previous received stamp as it
/// was queued (relabelled when it was). Ends the window (`old_epoch = false`) once the sender's raw
/// stamps continue the timeline (it stepped), on a jump that continues neither timeline (a real
/// stamp jump, never relabelled) and once the relabelled stamp passes `until_ns`.
///
/// Mirror of the C `genlock_fifo_relabel_arrival_add()`.
pub fn arrival_add(a: &mut Arrival, prev: u64, stamp: u64, src_ns: u64) -> i64 {
    if !a.old_epoch || a.step_ns == 0 || prev == 0 {
        return 0;
    }
    let relabelled = stamp.wrapping_add(a.step_ns as u64);
    if relabelled > a.until_ns {
        a.old_epoch = false;
        return 0;
    }
    let raw = dev_ns(stamp.wrapping_sub(prev) as i64, src_ns);
    let rel = dev_ns(relabelled.wrapping_sub(prev) as i64, src_ns);
    if raw <= a.frame_ns && raw <= rel {
        a.old_epoch = false;
        return 0;
    }
    if rel <= a.frame_ns {
        return a.step_ns;
    }
    a.old_epoch = false;
    0
}

/// The per-source relabel state (the C `genlock_relabel_*` fields of `obs_source`).
///
/// Mirror of the C `struct genlock_fifo_relabel_state`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelabelState {
    /// The booking this source applied last.
    pub seq: u64,
    /// The open arrival window.
    pub arrival: Arrival,
    /// The last remembered raw stamp jump (`0` = none), and when it arrived (monotonic).
    pub jump_ns: i64,
    pub jump_mono_ns: u64,
    /// Frames relabelled (queued + arrivals) — the audit's `relabelled=`.
    pub relabelled: u64,
    /// The monotonic instant of this source's previous release (`0` = none yet).
    pub last_release_mono_ns: u64,
}

impl RelabelState {
    /// Apply a booking this source has not applied yet, at its release (the C render thread, under
    /// the source's async mutex), BEFORE the release reads the queue: relabel the old-epoch queued
    /// frames, the locked boundary (`0` = unlocked; the presented stamp is `boundary − interval`)
    /// and the stamp tracker's last stamp (`rx_last`, `0` = none), and open the arrival window.
    /// `src_ns` is the source's learned stamp step (`0` = none yet: the canvas interval),
    /// `reserve_ns` its configured latency, `wall_now` the release's (post-step) wall read and
    /// `mono_now` its monotonic clock (every release passes it, applied or not). Returns
    /// the plan when it relabelled anything or opened a window, `None` when the booking was applied
    /// already or does not relabel (a step under [`MIN_STEP_NS`], an unknown interval, or a source
    /// that was not releasing at the step: no previous release, or one more than
    /// [`APPLY_MAX_GAP_NS`] ago — a new source, one silent across the step). A newer booking always
    /// closes the previous window.
    ///
    /// Mirror of the C `genlock_fifo_relabel_apply()`.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        b: &Booking,
        queue: &mut [u64],
        locked_boundary: &mut u64,
        rx_last: &mut u64,
        interval_ns: u64,
        src_ns: u64,
        reserve_ns: u64,
        wall_now: u64,
        mono_now: u64,
    ) -> Option<Plan> {
        let releasing = self.last_release_mono_ns != 0
            && mono_now.saturating_sub(self.last_release_mono_ns) <= APPLY_MAX_GAP_NS;
        self.last_release_mono_ns = mono_now;
        if self.seq == b.seq {
            return None;
        }
        self.seq = b.seq;
        self.arrival = Arrival::default();
        let step = b.step_ns;
        if !releasing || !step_relabels(step) || interval_ns == 0 {
            return None;
        }
        let src = if src_ns != 0 { src_ns } else { interval_ns };
        let prev = locked_boundary.saturating_sub(interval_ns);
        let age = if prev != 0 {
            wall_now.saturating_sub(prev.wrapping_add(step as u64))
        } else {
            0
        };
        let window = window_ns(reserve_ns, age);
        let stepped_before = sender_stepped_before(
            self.jump_ns,
            self.jump_mono_ns,
            step,
            src,
            interval_ns,
            b.mono_ns,
            window,
        );
        let p = plan(prev, queue, step, src, interval_ns, stepped_before);
        for ts in queue.iter_mut().take(p.queue_old) {
            *ts = ts.wrapping_add(step as u64);
        }
        if p.prev_old {
            *locked_boundary = locked_boundary.wrapping_add(step as u64);
        }
        if p.newest_old && *rx_last != 0 {
            *rx_last = rx_last.wrapping_add(step as u64);
        }
        self.relabelled = self.relabelled.wrapping_add(p.queue_old as u64);
        self.arrival = Arrival {
            step_ns: step,
            until_ns: b.wall_ns.saturating_add(window),
            frame_ns: interval_ns,
            old_epoch: p.newest_old,
        };
        self.jump_ns = 0;
        Some(p)
    }

    /// One received frame (the C producer push, under the async mutex, before the stamp tracker
    /// observes it): returns the stamp to queue — relabelled while the arrival window judges it so —
    /// and remembers a raw stamp jump it did not relabel. `rx_last` is the previous received stamp as
    /// queued (`0` = none), `src_ns` the learned stamp step (`0` = none: the window's frame, or
    /// `|delta|` for the jump record), `mono_now` the monotonic clock.
    ///
    /// Mirror of the C `genlock_fifo_relabel_receive()`.
    pub fn receive(&mut self, rx_last: u64, stamp: u64, src_ns: u64, mono_now: u64) -> u64 {
        let src = if src_ns != 0 {
            src_ns
        } else {
            self.arrival.frame_ns
        };
        let add = arrival_add(&mut self.arrival, rx_last, stamp, src);
        if add != 0 {
            self.relabelled = self.relabelled.wrapping_add(1);
            return stamp.wrapping_add(add as u64);
        }
        if rx_last != 0 {
            let delta = stamp.wrapping_sub(rx_last) as i64;
            if jump_recorded(delta, src_ns) {
                self.jump_ns = delta;
                self.jump_mono_ns = mono_now;
            }
        }
        stamp
    }
}

#[cfg(test)]
#[path = "genlock_fifo_relabel_tests.rs"]
mod tests;
