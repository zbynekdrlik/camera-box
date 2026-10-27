//! Issue 1482 step 2 (design 6449255649) — the long-horizon SOURCE phase estimate.
//!
//! A capture card that re-times the frames on its own clock (cam2's Cam Link, live 27.10.2026)
//! delivers a sawtooth around the source's real phase: +45 ppm between steps, then a catch-up step
//! LATER by +1.0..+2.9 ms, one per ~20 s, ~2 ms peak to peak. The short fit
//! ([`super::CapturePhaseTracker`], ~4 s) follows each tooth, so every absorbed step moved the stamp
//! by ~2 ms, four times the slot hysteresis, and near an edge the slot flipped back and forth.
//!
//! [`SourcePhase`] fits `t = a + P * x` over the last [`SOURCE_FIT_SECONDS`] of the samples the
//! short fit FOLDS, raw: a card's steps are NOT removed, so the slope is the source's long-term
//! rate and the intercept the source's mean phase (the sawtooth averaged). The sequence position
//! `x` is the short fit's own, which already counts hidden device drops. One point per
//! [`SOURCE_DECIMATION_FRAMES`] of `x` keeps the window bounded (~1080 points at 60 fps).
//!
//! The short fit owns the resets: every seed and re-seed (a half-period step, an outlier run that
//! is no constant step, a sequence back/jump, a time stall, a gap) starts this fit over; an
//! absorbed card step does not, since absorbed steps are exactly what it averages. Delivery delays
//! the short fit holds (never folds) never reach it either.
//!
//! **`i128` range.** The window spans at most [`SOURCE_FIT_SECONDS`] (`T <= 1.8e11` ns) and
//! [`SOURCE_FIT_MAX_POINTS`] points; its sequence span `X` is generously below `2e5` (a 1000 fps
//! stream over 180 s). Then `n <= 4096`, `|nb| <= n^2 * X * T / 4 ~ 1.5e23`,
//! `d <= n^2 * X^2 / 4 ~ 1.7e17`, and the largest product [`super::line_predict_ns`] forms,
//! `Sy * d`, stays below ~1.3e32 (`i128::MAX` ~1.7e38). No residual sum of squares is formed.

use super::{line_predict_ns, FitSums};
use crate::genlock_grid::NS_PER_SECOND;
use std::collections::VecDeque;

/// The source fit's window: the last this many seconds of folded samples.
pub const SOURCE_FIT_SECONDS: u64 = 180;

/// The source fit drives the stamp only once its window spans at least this many seconds (and the
/// short fit is locked). Before that the stamp comes from the short fit, as before step 2.
pub const SOURCE_FIT_MIN_SECONDS: u64 = 60;

/// One source-fit point per this many frames of sequence position.
pub const SOURCE_DECIMATION_FRAMES: i64 = 10;

/// A hard cap on the window's points, whatever the frame rate (the `i128` range bound).
pub const SOURCE_FIT_MAX_POINTS: usize = 4096;

const _: () = assert!(SOURCE_FIT_MIN_SECONDS < SOURCE_FIT_SECONDS);
const _: () = assert!(SOURCE_DECIMATION_FRAMES >= 1);

/// The long-horizon least-squares fit of `t = a + P * x`. See the module doc.
#[derive(Debug, Clone, Default)]
pub struct SourcePhase {
    /// `(x, t)`, oldest first; the sums are relative to the oldest.
    window: VecDeque<(i64, u64)>,
    sums: FitSums,
}

impl SourcePhase {
    /// Forget everything (a seed or a real discontinuity of the short fit).
    pub(super) fn reset(&mut self) {
        self.window.clear();
        self.sums = FitSums::default();
    }

    /// One sample the short fit folded (raw time, its sequence position). Kept when it is at least
    /// [`SOURCE_DECIMATION_FRAMES`] past the newest point; then points older than
    /// [`SOURCE_FIT_SECONDS`] (or beyond [`SOURCE_FIT_MAX_POINTS`]) leave the window.
    pub(super) fn offer(&mut self, x: i64, t: u64) {
        if let Some(&(last_x, last_t)) = self.window.back() {
            if x < last_x + SOURCE_DECIMATION_FRAMES || t <= last_t {
                return;
            }
        }
        let (x0, t0) = self.window.front().copied().unwrap_or((x, t));
        self.window.push_back((x, t));
        self.sums.add(
            i128::from(x) - i128::from(x0),
            i128::from(t) - i128::from(t0),
        );
        let max_span = SOURCE_FIT_SECONDS * NS_PER_SECOND;
        while self.window.len() > SOURCE_FIT_MAX_POINTS
            || self
                .window
                .front()
                .is_some_and(|&(_, front_t)| t - front_t > max_span)
        {
            self.pop_front();
        }
    }

    /// Drop the oldest point (the origin, `dx = dy = 0`), then move the origin to the new oldest.
    fn pop_front(&mut self) {
        let Some((x0, t0)) = self.window.pop_front() else {
            return;
        };
        self.sums.n -= 1;
        match self.window.front() {
            Some(&(x1, t1)) => self.sums.shift(
                i128::from(x1) - i128::from(x0),
                i128::from(t1) - i128::from(t0),
            ),
            None => self.sums = FitSums::default(),
        }
    }

    /// The time the window spans (ns).
    pub fn span_ns(&self) -> u64 {
        match (self.window.front(), self.window.back()) {
            (Some(&(_, a)), Some(&(_, b))) => b - a,
            _ => 0,
        }
    }

    /// Points in the window.
    pub fn points(&self) -> usize {
        self.window.len()
    }

    /// True once the window spans [`SOURCE_FIT_MIN_SECONDS`] and the line is defined.
    pub fn ready(&self) -> bool {
        self.span_ns() >= SOURCE_FIT_MIN_SECONDS * NS_PER_SECOND && self.sums.d() > 0
    }

    /// The source capture time at sequence position `x`, once [`Self::ready`].
    pub fn predict_ns(&self, x: i64) -> Option<u64> {
        self.ready()
            .then(|| line_predict_ns(&self.sums, self.window.front().copied(), x))
    }

    /// The fitted source frame period (ns), once three points exist.
    pub fn period_ns(&self) -> Option<f64> {
        let d = self.sums.d();
        (self.window.len() >= 3 && d > 0).then(|| self.sums.nb() as f64 / d as f64)
    }
}
