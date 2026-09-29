//! Issue 1367 slice D2 — the per-stream capture PHASE tracker: the camera's frame period and phase
//! fitted from the V4L2 sequence number + the `CLOCK_MONOTONIC` capture timestamp, so the genlock
//! stamp and the emit gate decide on a SMOOTHED capture instant instead of each frame's own jittery
//! timestamp.
//!
//! ## Why this module exists
//!
//! A cambox stamps each frame on the per-second genlock grid at the floor of its own V4L2 capture
//! time (`genlock_stamp::genlock_emit_timecode_100ns`), and the emit gate decides on the poll wall
//! clock after the dequeue (`main.rs`). The camera free-runs against that grid (the Cam Link boxes
//! read ~16 ppm), so its phase slides through a slot edge once every ~17 min. uvcvideo stamps the
//! host time of the first USB packet, and the dequeue adds its own jitter (the live bursts fit a
//! core of a few us with rare excursions of ~0.1 ms; the design allows up to ~0.3 ms), so for
//! 15-20 s around each edge the frames land on either side at random: every flip is a shed plus a
//! repeat, one unique frame lost and the next one shown twice (live CAM5 29.9.2026: 36 sheds and
//! 35 repeats in one burst). The physical truth is ONE extra or ONE missing frame per crossing.
//!
//! ## What it does
//!
//! - [`CapturePhaseTracker`] fits `t = a + P * seq` over the last [`FIT_WINDOW_FRAMES`] frames by
//!   least squares, with EXACT `i128` running sums re-anchored on the oldest sample (no float drift,
//!   so the same timestamps always give the same fit). The fit runs over the sequence number, so a
//!   frame the HOST dropped (uvcvideo counts every frame the device sent, buffer free or not) is a
//!   sequence step of 2 and costs nothing. A frame the DEVICE skipped is invisible to the
//!   sequence: its residual is a whole number of periods, so it is re-indexed as that many extra
//!   frames. From [`OUTLIER_MIN_FRAMES`] samples on, a sample farther than
//!   [`RESEED_JITTER_MULTIPLE`] x the fit's own jitter (at least [`RESEED_FLOOR_NS`]) is not folded:
//!   under half a period it is stamped from the prediction, and [`RESEED_CONSECUTIVE_OUTLIERS`] of
//!   them in a row re-seed; half a period or more re-seeds at once (a real discontinuity, never a
//!   stamp from the old phase). A backward or huge sequence step (a device re-open), a timestamp
//!   that does not advance, or a gap longer than [`MAX_FRAME_PERIOD_NS`] per frame re-seed too; the
//!   last one also bounds the window's time span, which keeps every `i128` sum in range. The fit is
//!   LOCKED once it holds [`LOCK_MIN_FRAMES`] samples, its RMS residual is at most
//!   [`LOCK_MAX_JITTER_NS`] and its residuals are white (no quarter of the window sits off the line:
//!   a step folded while seeding would otherwise lock a tilted fit).
//! - [`SlotHysteresis`] turns the smoothed realtime instant into a grid slot that advances by
//!   exactly the sequence advance. A slot one earlier or one later than that is accepted only when
//!   the smoothed instant is more than [`SLOT_HYSTERESIS_NS`] past the edge, so a crossing costs
//!   exactly one duplicate slot (camera faster than the grid) or one missing slot (slower).
//! - [`CapturePhase`] combines both. It drives the stamp only while the fit is locked AND the camera
//!   runs within [`STAMP_MODE_MAX_RATE_PPM`] of the emit rate (the 1:1 regime, where a crossing is a
//!   single event). Anything else (seeding, a re-seed, an over-rate or under-rate grabber whose
//!   surplus the `dupe_decimation` machinery absorbs) returns `None`, and the caller keeps today's
//!   raw stamp and poll-time gate: the fail-safe is the current behaviour.
//!
//! The emit gate side (`dupe_decimation::DecimationGate::note_stamp_slot`) decides on this slot:
//! emit on a one-slot advance, drop a real duplicate slot, fill a real missing slot with the
//! existing starvation repeat. The receiver sees clean stamps.
//!
//! Pure `std` + [`crate::genlock_grid`], no I/O — Tier-0 testable. The two-clock bench is
//! `crate::capture_phase_bench`.

use crate::genlock_grid::{
    grid_advance_ns, grid_floor_ns, integer_fps, per_second_floor, NS_PER_SECOND,
    UNITS_100NS_PER_SECOND,
};
use std::collections::VecDeque;

/// Frames in the least-squares window: ~4.3 s at 60 fps. The prediction error at the newest frame
/// is `2 * sigma / sqrt(n)`, 1/8 of the raw timestamp jitter at a full window.
pub const FIT_WINDOW_FRAMES: usize = 256;

/// Samples the fit needs before it may lock (2 s at 60 fps).
pub const LOCK_MIN_FRAMES: usize = 120;

/// The largest RMS residual a locked fit may carry. A stream noisier than this never drives the
/// stamp (its hysteresis could not be trusted).
pub const LOCK_MAX_JITTER_NS: u64 = 1_000_000;

/// From this many samples on the fit checks every new sample against its prediction (outliers,
/// frames the device skipped, discontinuities). Below it the slope is too loose to judge by.
pub const OUTLIER_MIN_FRAMES: usize = 30;

/// A sample farther than this multiple of the fit's RMS residual from the prediction is an
/// outlier: not folded, stamped from the prediction (under half a period) or a re-seed.
pub const RESEED_JITTER_MULTIPLE: u64 = 8;

/// The outlier bound never falls below this, so a very clean stream does not re-seed on a
/// sub-millisecond interrupt hiccup.
pub const RESEED_FLOOR_NS: u64 = 1_000_000;

/// This many outliers in a row is a real phase step (a grabber re-lock): re-seed.
pub const RESEED_CONSECUTIVE_OUTLIERS: u32 = 3;

/// A forward sequence step above this (more than 8 frames lost at once) re-seeds: that is a device
/// hiccup, not a dropped frame. It also bounds the window's sequence span, which keeps every `i128`
/// sum far inside its range. A frame the device skipped counts toward it too.
pub const MAX_SEQ_ADVANCE: u32 = 8;

/// The longest time a delivered frame may take per sequence step (10 fps). A longer gap is a device
/// pause, not a frame, and re-seeds at any sample count — this is also what bounds the window's time
/// span, and so the `i128` sums, while the fit is still seeding.
pub const MAX_FRAME_PERIOD_NS: u64 = 100_000_000;

/// A quarter of the window whose mean residual is more than this many standard errors off the line
/// keeps the fit unlocked (the whiteness check). The standard error of a quarter mean of white
/// residuals is `2 * sigma / sqrt(n)`, with sigma the LOCAL jitter (consecutive differences).
pub const WHITENESS_SIGMAS: f64 = 5.0;

/// The whiteness bound never falls below this (a very clean stream).
pub const WHITENESS_FLOOR_NS: f64 = 20_000.0;

/// After a failed whiteness check, the next one waits this many frames (a lock is delayed by at
/// most this much; the O(window) check never runs per frame on a stream that stays non-white).
pub const WHITENESS_CHECK_EVERY: u32 = 16;

/// How far past a slot edge the smoothed instant must be before the slot sequence breaks from the
/// sequence advance. Far above the locked prediction noise (tens of us), and the stamp moves by at
/// most this much (sub-ms). Clamped to a quarter of the interval for fast rates.
pub const SLOT_HYSTERESIS_NS: u64 = 500_000;

/// The 1:1 regime: the tracked frame rate within this many ppm of the emit rate. Covers a free-
/// running camera (tens of ppm) and 59.94 into 60 (-1000 ppm); excludes the over-rate grabbers
/// (61+ fps, ~17 000 ppm) and stays below the gate's over-rate takt threshold (60.3 fps,
/// ~4975 ppm, pinned in `lib.rs`), so a stamp-driven stream never reads as over-rate.
pub const STAMP_MODE_MAX_RATE_PPM: u64 = 2_000;

const _: () = assert!(LOCK_MIN_FRAMES >= 3 && LOCK_MIN_FRAMES <= FIT_WINDOW_FRAMES);
const _: () = assert!(OUTLIER_MIN_FRAMES >= 3 && OUTLIER_MIN_FRAMES <= LOCK_MIN_FRAMES);
const _: () = assert!(RESEED_CONSECUTIVE_OUTLIERS >= 2);
const _: () = assert!(MAX_SEQ_ADVANCE >= 2);

/// Exact least-squares sums over the window, relative to the OLDEST sample (`dx = x - x0`,
/// `dy = t - t0`). With `n <= 256`, a sequence span `<= 256 * MAX_SEQ_ADVANCE` and a time span
/// `<= 256 * (MAX_SEQ_ADVANCE + 1) * MAX_FRAME_PERIOD_NS`, the largest product (`nb^2`) stays
/// below ~1e38, inside `i128`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FitSums {
    n: i128,
    sx: i128,
    sy: i128,
    sxx: i128,
    sxy: i128,
    syy: i128,
}

impl FitSums {
    fn add(&mut self, dx: i128, dy: i128) {
        self.n += 1;
        self.sx += dx;
        self.sy += dy;
        self.sxx += dx * dx;
        self.sxy += dx * dy;
        self.syy += dy * dy;
    }

    /// Move the origin by `(ddx, ddy)`: every `dx` becomes `dx - ddx`, every `dy` becomes
    /// `dy - ddy`. Exact.
    fn shift(&mut self, ddx: i128, ddy: i128) {
        let (n, sx, sy) = (self.n, self.sx, self.sy);
        self.sxx += n * ddx * ddx - 2 * ddx * sx;
        self.sxy += n * ddx * ddy - ddx * sy - ddy * sx;
        self.syy += n * ddy * ddy - 2 * ddy * sy;
        self.sx -= n * ddx;
        self.sy -= n * ddy;
    }

    /// `n * Sxx - Sx^2` (n^2 x the variance of x). `> 0` once two distinct samples exist.
    fn d(&self) -> i128 {
        self.n * self.sxx - self.sx * self.sx
    }

    /// `n * Sxy - Sx * Sy`: the slope is `nb / d`.
    fn nb(&self) -> i128 {
        self.n * self.sxy - self.sx * self.sy
    }
}

/// Round `num / den` to the nearest integer, `den > 0`.
fn div_round(num: i128, den: i128) -> i128 {
    (2 * num + den).div_euclid(2 * den)
}

/// One observed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseObservation {
    /// The smoothed `CLOCK_MONOTONIC` capture instant (ns), `Some` only while the fit is locked.
    pub smoothed_mono_ns: Option<u64>,
    /// Frames advanced since the previous observed frame: the sequence step (`2` = one frame the
    /// host dropped) plus any frames the device skipped without a sequence step. `0` for the first
    /// frame of a (re-)seed or a frame without a timestamp.
    pub seq_advance: u32,
}

/// The least-squares capture-phase fit of ONE stream. See the module doc.
#[derive(Debug, Clone, Default)]
pub struct CapturePhaseTracker {
    /// `(x, t)`: the unwrapped sequence position and the monotonic capture time, oldest first.
    window: VecDeque<(i64, u64)>,
    sums: FitSums,
    last_seq: Option<u32>,
    last_x: i64,
    /// The previous observed frame's raw capture time (a stall / non-advance check).
    last_t: u64,
    outlier_run: u32,
    /// Sticky: set once the fit passes the lock checks (whiteness included), cleared by a re-seed
    /// or by the jitter leaving [`LOCK_MAX_JITTER_NS`].
    locked: bool,
    /// Frames until the next whiteness check while unlocked (`0` = check now).
    white_check_countdown: u32,
    reseeds: u64,
    hidden_drops: u64,
}

impl CapturePhaseTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one delivered frame (its V4L2 `sequence` and `CLOCK_MONOTONIC` capture time in ns).
    /// `capture_mono_ns == 0` (no V4L2 timestamp) changes nothing and returns no estimate.
    pub fn observe(&mut self, seq: u32, capture_mono_ns: u64) -> PhaseObservation {
        let none = |seq_advance| PhaseObservation {
            smoothed_mono_ns: None,
            seq_advance,
        };
        if capture_mono_ns == 0 {
            return none(0);
        }
        let Some(prev) = self.last_seq else {
            self.seed(seq, capture_mono_ns);
            return none(0);
        };
        let delta = seq.wrapping_sub(prev) as i32;
        let dt = capture_mono_ns.saturating_sub(self.last_t);
        if delta <= 0
            || delta as u32 > MAX_SEQ_ADVANCE
            || capture_mono_ns <= self.last_t
            || dt > (delta as u64 + 1) * MAX_FRAME_PERIOD_NS
        {
            // A backward / stalled / huge sequence step (a device re-open), a timestamp that does
            // not advance, or a pause longer than any frame: not this fit's stream any more.
            self.reseed(seq, capture_mono_ns);
            return none(0);
        }
        let mut seq_advance = delta as u32;
        let mut x = self.last_x + i64::from(delta);
        self.last_seq = Some(seq);
        self.last_t = capture_mono_ns;
        if self.window.len() >= OUTLIER_MIN_FRAMES {
            if let (Some(rms), Some(period)) = (self.jitter_rms_ns(), self.period_ns()) {
                let pred = self.predict_ns(x);
                let resid = i128::from(capture_mono_ns) - i128::from(pred);
                let bound = (RESEED_JITTER_MULTIPLE as f64 * rms).max(RESEED_FLOOR_NS as f64);
                let r = resid as f64;
                if r.abs() > bound {
                    let skipped = (r / period).round();
                    if skipped >= 1.0
                        && (r - skipped * period).abs() <= bound
                        && seq_advance as f64 + skipped <= MAX_SEQ_ADVANCE as f64
                    {
                        // The device skipped frames the sequence does not show: the residual is a
                        // whole number of periods. Re-index; the gate fills the missing slots.
                        let k = skipped as u32;
                        seq_advance += k;
                        x += i64::from(k);
                        self.hidden_drops += u64::from(k);
                    } else if r.abs() * 2.0 >= period {
                        // Half a frame or more: a real discontinuity, never a stamp from the old
                        // phase.
                        self.reseed(seq, capture_mono_ns);
                        return none(0);
                    } else {
                        self.last_x = x;
                        self.outlier_run += 1;
                        if self.outlier_run >= RESEED_CONSECUTIVE_OUTLIERS {
                            self.reseed(seq, capture_mono_ns);
                            return none(0);
                        }
                        // One late/early timestamp: never folded, stamped from the prediction.
                        return PhaseObservation {
                            smoothed_mono_ns: self.locked.then_some(pred),
                            seq_advance,
                        };
                    }
                }
            }
        }
        self.last_x = x;
        self.outlier_run = 0;
        self.push(x, capture_mono_ns);
        self.update_lock();
        PhaseObservation {
            smoothed_mono_ns: self.locked.then(|| self.predict_ns(x)),
            seq_advance,
        }
    }

    /// True once the fit holds [`LOCK_MIN_FRAMES`] samples, its RMS residual is at most
    /// [`LOCK_MAX_JITTER_NS`] and its residuals are white.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// The fitted frame period (ns), once three samples exist.
    pub fn period_ns(&self) -> Option<f64> {
        let d = self.sums.d();
        (self.window.len() >= 3 && d > 0).then(|| self.sums.nb() as f64 / d as f64)
    }

    /// The RMS residual of the fit (ns): the raw timestamp jitter, once three samples exist.
    pub fn jitter_rms_ns(&self) -> Option<f64> {
        let s = &self.sums;
        let d = s.d();
        if self.window.len() < 3 || d <= 0 {
            return None;
        }
        // n * SSE = (n*Syy - Sy^2) - nb^2 / d, exact up to the one integer division.
        let a = s.n * s.syy - s.sy * s.sy;
        let nb = s.nb();
        let sse_n = (a - nb * nb / d).max(0);
        Some((sse_n as f64 / (s.n * s.n) as f64).sqrt())
    }

    /// How many times the fit re-seeded after its first seed.
    pub fn reseeds(&self) -> u64 {
        self.reseeds
    }

    /// Frames the device skipped that the sequence did not show (re-indexed, not re-seeded).
    pub fn hidden_drops(&self) -> u64 {
        self.hidden_drops
    }

    fn seed(&mut self, seq: u32, t: u64) {
        self.window.clear();
        self.sums = FitSums::default();
        self.outlier_run = 0;
        self.locked = false;
        self.white_check_countdown = 0;
        self.last_seq = Some(seq);
        self.last_x = 0;
        self.last_t = t;
        self.push(0, t);
    }

    fn reseed(&mut self, seq: u32, t: u64) {
        self.reseeds += 1;
        self.seed(seq, t);
    }

    fn push(&mut self, x: i64, t: u64) {
        let (x0, t0) = self.window.front().copied().unwrap_or((x, t));
        self.window.push_back((x, t));
        self.sums.add(
            i128::from(x) - i128::from(x0),
            i128::from(t) - i128::from(t0),
        );
        if self.window.len() > FIT_WINDOW_FRAMES {
            // The oldest sample is the origin (dx = dy = 0): drop it, then move the origin to
            // the new oldest sample.
            self.window.pop_front();
            self.sums.n -= 1;
            if let Some(&(x1, t1)) = self.window.front() {
                self.sums.shift(
                    i128::from(x1) - i128::from(x0),
                    i128::from(t1) - i128::from(t0),
                );
            }
        }
    }

    /// Lock once the fit is long enough, quiet enough and white; unlock when the jitter leaves the
    /// bound. The whiteness check is O(window): it runs only while unlocked, and after a failed
    /// check only every [`WHITENESS_CHECK_EVERY`] frames (a stream that stays non-white, a wandering
    /// grabber, never pays it per frame).
    fn update_lock(&mut self) {
        let quiet = self.window.len() >= LOCK_MIN_FRAMES
            && self
                .jitter_rms_ns()
                .is_some_and(|rms| rms <= LOCK_MAX_JITTER_NS as f64);
        if !quiet {
            self.locked = false;
        } else if !self.locked {
            if self.white_check_countdown == 0 {
                self.locked = self.residuals_are_white();
                self.white_check_countdown = WHITENESS_CHECK_EVERY;
            } else {
                self.white_check_countdown -= 1;
            }
        }
    }

    /// No quarter of the window sits off the fitted line by more than [`WHITENESS_SIGMAS`] standard
    /// errors of its mean (at least [`WHITENESS_FLOOR_NS`]). A phase step folded while seeding
    /// leaves the quarters on either side of it systematically off; white jitter does not.
    ///
    /// The standard error comes from the LOCAL jitter (the RMS of consecutive residual differences
    /// over sqrt 2), not from the fit's RMS: a folded step inflates the fit's RMS and would widen
    /// its own bound, while one jump among n differences barely moves the local estimate.
    fn residuals_are_white(&self) -> bool {
        let n = self.window.len();
        if n < 4 {
            return false;
        }
        let mut sum = [0.0f64; 4];
        let mut count = [0u32; 4];
        let mut diff_sq = 0.0f64;
        let mut prev: Option<f64> = None;
        for (i, &(x, t)) in self.window.iter().enumerate() {
            let r = (i128::from(t) - i128::from(self.predict_ns(x))) as f64;
            if let Some(p) = prev {
                diff_sq += (r - p) * (r - p);
            }
            prev = Some(r);
            let q = (i * 4 / n).min(3);
            sum[q] += r;
            count[q] += 1;
        }
        let local = (diff_sq / (2.0 * (n - 1) as f64)).sqrt();
        let bound = (WHITENESS_SIGMAS * 2.0 * local / (n as f64).sqrt()).max(WHITENESS_FLOOR_NS);
        (0..4).all(|q| count[q] > 0 && (sum[q] / f64::from(count[q])).abs() <= bound)
    }

    /// The fitted capture time at sequence position `x` (the caller guarantees `d() > 0`).
    fn predict_ns(&self, x: i64) -> u64 {
        let s = &self.sums;
        let (x0, t0) = self.window.front().copied().unwrap_or((x, 0));
        let d = s.d();
        if d <= 0 {
            return t0;
        }
        let dx = i128::from(x) - i128::from(x0);
        let num = s.sy * d + s.nb() * (s.n * dx - s.sx);
        let dy = div_round(num, s.n * d);
        (i128::from(t0) + dy).clamp(0, i128::from(u64::MAX)) as u64
    }
}

/// How [`SlotHysteresis::choose`] placed a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotEvent {
    /// The first frame after a (re)start: the plain grid floor.
    Start,
    /// The slot the sequence advance predicts.
    OnTime,
    /// The instant sat within the hysteresis of an edge: kept on the predicted slot.
    Held,
    /// A real crossing: one slot earlier (camera faster) or later (slower) than predicted.
    Crossing,
    /// More than one slot off the prediction: a clock step or a re-seed, not a crossing.
    Jump,
}

/// The slot chooser with edge hysteresis. See the module doc.
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotHysteresis {
    last_slot_ns: Option<u64>,
}

impl SlotHysteresis {
    /// Place the frame whose smoothed realtime instant is `t_ns` and whose sequence advanced
    /// `seq_advance` since the previous chosen frame. Returns the grid point of its slot.
    pub fn choose(&mut self, t_ns: u64, seq_advance: u32, interval_ns: u64) -> (u64, SlotEvent) {
        let raw = grid_floor_ns(t_ns, interval_ns);
        let h = SLOT_HYSTERESIS_NS.min(interval_ns / 4);
        let placed = match self.last_slot_ns {
            None => (raw, SlotEvent::Start),
            Some(last) => {
                let expected = grid_advance_ns(last, u64::from(seq_advance), interval_ns);
                if raw == expected {
                    (expected, SlotEvent::OnTime)
                } else if expected > 0 && raw == grid_floor_ns(expected - 1, interval_ns) {
                    // One slot early: the camera phase crossed the edge from above.
                    if t_ns.saturating_add(h) < expected {
                        (raw, SlotEvent::Crossing)
                    } else {
                        (expected, SlotEvent::Held)
                    }
                } else if raw == grid_advance_ns(expected, 1, interval_ns) {
                    // One slot late: the camera phase crossed the edge from below.
                    if t_ns >= raw.saturating_add(h) {
                        (raw, SlotEvent::Crossing)
                    } else {
                        (expected, SlotEvent::Held)
                    }
                } else {
                    (raw, SlotEvent::Jump)
                }
            }
        };
        self.last_slot_ns = Some(placed.0);
        placed
    }

    /// Forget the previous slot (the stream stopped driving the stamp).
    pub fn reset(&mut self) {
        self.last_slot_ns = None;
    }
}

/// Which path the stream is on, for the `phase_lock=` log token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhaseMode {
    /// The fit is (re-)seeding: raw stamp + poll-time gate (today).
    #[default]
    Seed,
    /// Locked, but outside the 1:1 regime (an over/under-rate grabber): raw stamp + poll-time gate.
    Band,
    /// Locked and 1:1: the stamp and the gate are driven by the tracked slot.
    Stamp,
    /// Genlock is off: there is no emit grid to drive.
    Off,
}

impl PhaseMode {
    pub fn token(self) -> &'static str {
        match self {
            PhaseMode::Seed => "seed",
            PhaseMode::Band => "band",
            PhaseMode::Stamp => "stamp",
            PhaseMode::Off => "off",
        }
    }
}

/// The exact nominal frame period of `interval_ns` (`1e9 / fps` for an integer rate).
fn nominal_period_ns(interval_ns: u64) -> f64 {
    match integer_fps(interval_ns) {
        Some(fps) => NS_PER_SECOND as f64 / fps as f64,
        None => interval_ns as f64,
    }
}

/// The tracker + slot chooser of one capture stream, as the capture loop uses it.
#[derive(Debug, Clone, Default)]
pub struct CapturePhase {
    tracker: CapturePhaseTracker,
    slots: SlotHysteresis,
    crossings: u64,
    mode: PhaseMode,
}

impl CapturePhase {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track one delivered frame and return the grid point of its stamp slot while the tracker
    /// drives the stamp (locked + 1:1), else `None` (use today's raw stamp and poll-time gate).
    /// `mono_to_real_offset_ns` maps `CLOCK_MONOTONIC` to the `CLOCK_REALTIME` grid (the capture
    /// loop's periodically re-sampled offset); `interval_ns == 0` (genlock off) never drives.
    pub fn stamp_frame(
        &mut self,
        seq: u32,
        capture_mono_ns: u64,
        mono_to_real_offset_ns: i64,
        interval_ns: u64,
    ) -> Option<u64> {
        let obs = self.tracker.observe(seq, capture_mono_ns);
        let smoothed = match obs.smoothed_mono_ns {
            Some(t) if interval_ns > 0 => t,
            _ => {
                self.mode = if interval_ns == 0 {
                    PhaseMode::Off
                } else if self.tracker.locked() {
                    PhaseMode::Band
                } else {
                    PhaseMode::Seed
                };
                self.slots.reset();
                return None;
            }
        };
        let t_real = i128::from(smoothed) + i128::from(mono_to_real_offset_ns);
        let in_band = self
            .rate_ppm(interval_ns)
            .is_some_and(|ppm| ppm.abs() <= STAMP_MODE_MAX_RATE_PPM as f64);
        if !in_band || t_real <= 0 || t_real > i128::from(u64::MAX) {
            self.mode = PhaseMode::Band;
            self.slots.reset();
            return None;
        }
        self.mode = PhaseMode::Stamp;
        let (slot, event) = self
            .slots
            .choose(t_real as u64, obs.seq_advance, interval_ns);
        if event == SlotEvent::Crossing {
            self.crossings += 1;
        }
        Some(slot)
    }

    /// The path the most recent frame took.
    pub fn mode(&self) -> PhaseMode {
        self.mode
    }

    /// The camera frame RATE offset from the emit rate in ppm (positive = the camera runs faster
    /// than the grid, so its crossings drop a duplicate slot).
    pub fn rate_ppm(&self, interval_ns: u64) -> Option<f64> {
        let p = self.tracker.period_ns()?;
        (interval_ns > 0 && p > 0.0).then(|| (nominal_period_ns(interval_ns) / p - 1.0) * 1e6)
    }

    /// The fit's RMS residual in microseconds.
    pub fn jitter_rms_us(&self) -> Option<f64> {
        self.tracker.jitter_rms_ns().map(|ns| ns / 1000.0)
    }

    /// Crossings the slot chooser has placed since the start (one per camera-edge crossing).
    pub fn crossings(&self) -> u64 {
        self.crossings
    }

    /// Re-seeds of the fit since the start.
    pub fn reseeds(&self) -> u64 {
        self.tracker.reseeds()
    }

    /// Frames the device skipped that the sequence did not show, since the start.
    pub fn hidden_drops(&self) -> u64 {
        self.tracker.hidden_drops()
    }

    /// The tokens the 5 s `#707 emit-1s/cap-1s` line appends (leading space). Every key is
    /// mutually non-substring with every other token on that line.
    pub fn status_tokens(&self, interval_ns: u64) -> String {
        let ppm = self
            .rate_ppm(interval_ns)
            .map_or_else(|| "na".to_string(), |v| format!("{v:+.1}"));
        let jitter = self
            .jitter_rms_us()
            .map_or_else(|| "na".to_string(), |v| format!("{v:.0}"));
        format!(
            " phase_lock={} phase_ppm={ppm} jitter_us={jitter} crossings={} reseeds={}",
            self.mode.token(),
            self.crossings,
            self.reseeds()
        )
    }
}

/// The realtime instant (100 ns units) the capture loop floors into the frame's NDI timecode:
/// the middle of the tracked slot while [`CapturePhase::stamp_frame`] drives the stream, else
/// today's raw capture instant mapped into the realtime domain
/// (`genlock_stamp::capture_realtime_100ns`).
#[cfg(target_os = "linux")]
pub fn stamp_instant_100ns(
    phase_slot_ns: Option<u64>,
    interval_ns: u64,
    capture_monotonic_100ns: i64,
    mono_to_real_offset_100ns: i64,
) -> i64 {
    match phase_slot_ns {
        Some(slot_ns) => slot_mid_realtime_100ns(slot_ns, interval_ns),
        None => crate::genlock_stamp::capture_realtime_100ns(
            capture_monotonic_100ns,
            mono_to_real_offset_100ns,
        ),
    }
}

/// The realtime instant (100 ns units) in the middle of the grid slot `slot_ns`: fed to
/// `genlock_stamp::genlock_emit_timecode_100ns` it floors to exactly that slot's sender stamp
/// (a sender stamp sits at most 99 ns before its slot's ns grid point, see `genlock_grid`).
pub fn slot_mid_realtime_100ns(slot_ns: u64, interval_ns: u64) -> i64 {
    (slot_ns.saturating_add(interval_ns / 2) / 100) as i64
}

/// The grid point (ns) of the slot a sender stamp (100 ns units) belongs to.
pub fn stamp_slot_ns(stamp_100ns: i64, interval_ns: u64) -> u64 {
    let stamp_ns = (stamp_100ns.max(0) as u64).saturating_mul(100);
    grid_floor_ns(stamp_ns.saturating_add(interval_ns / 2), interval_ns)
}

/// The sender stamp (100 ns units) of the grid slot `slot_ns` at `fps` — what the NDI send
/// carries for a frame placed in that slot.
pub fn slot_stamp_100ns(slot_ns: u64, interval_ns: u64, fps: u64) -> i64 {
    per_second_floor(
        slot_mid_realtime_100ns(slot_ns, interval_ns) as u64,
        fps,
        UNITS_100NS_PER_SECOND,
    ) as i64
}

#[cfg(test)]
mod tests;
