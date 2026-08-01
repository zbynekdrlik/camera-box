//! #806 (epic #800 A/V-desync endgame round) — the OUTER-loop guard: a slow, SyncNet-driven
//! feedback loop that corrects the inner ASRC servo's (#803, live since #912) own long-term
//! residual, audio-side only, never touching the video genlock-latency knob.
//!
//! ## Root cause this closes
//!
//! The inner loop locks a source's audio timeline to the video master clock by comparing
//! delivered SAMPLE COUNT against wall-clock time (`RealtimeAsrcCompensator` in
//! `src/asrc_bench.rs`) — but per this ticket's own text it still leaves a measured long-term
//! residual of ~3-20 ms/hour. That residual is invisible to the inner loop BY CONSTRUCTION: it
//! only ever sees sample counts vs wall-clock time, never the actual perceptual audio/video
//! alignment, so any small systematic bias in the estimation itself (EMA steady-state error,
//! sample-count rounding, a driver quirk) accumulates silently. SyncNet
//! (`scripts/av_sync_measure.py`, issue #801) is the only thing that measures REAL audio-video
//! alignment, at a ~7 minute cadence (heavy neural-net inference) — far too slow/coarse to be the
//! primary corrector, but exactly right as a slow OUTER loop correcting the inner loop's own bias.
//!
//! ## The guard, in one line
//!
//! [`OuterLoopGuard::observe`] takes one CONFIDENT SyncNet residual measurement at a time (the
//! caller — `scripts/av_sync_measure.py`'s own `CONF_MIN` gate — has already filtered out
//! unmeasurable windows), keeps a fixed-size sliding window, and once the window is full, nudges a
//! persistent `bias_ppm` by [`STEP_PPM`] whenever the window's average |residual| stays above
//! [`RESIDUAL_THRESHOLD_MS`] — hard-clamped to `+/-`[`crate::asrc_bench::OUTER_BIAS_MAX_PPM`] (the
//! ticket's own "max +/-10 ppm od inner-loop odhadu" bound). The produced `bias_ppm` is meant to
//! be fed straight into `RealtimeAsrcCompensator::set_outer_bias_ppm` (or, in production, its C
//! mirror via `obs_source_set_asrc_outer_bias_ppm`, reached over a purpose-built obs-websocket
//! request — see issue #806's design comment, `gh issue view 806 --comments`).
//!
//! No decay: this is an INTEGRAL corrector for a systematic bias, not a transient one — the
//! ticket's own text only asks for a nudge when the sustained average exceeds the threshold, never
//! for the bias to relax back toward 0 once it doesn't. At the ticket's own ~7 min measurement
//! cadence, [`WINDOW_N`] samples = ~21 minutes sustained before any action, and reaching the full
//! `+/-10ppm` bound takes up to 10 corrections (~3.5h) — a deliberately slow, bounded, rate-limited
//! feedback loop ("pomaly feedback", per the ticket).
//!
//! ## Sign convention (a DELIBERATE choice — not live-validated, see issue #806's design comment)
//!
//! `residual_ms` uses the SAME sign as `scripts/av_sync_measure.py`'s `offset_ms`: **positive =
//! audio LEADS video** (the audio track plays too far ahead of the picture). A sustained positive
//! residual therefore nudges `bias_ppm` UP (same sign, not negated) — a larger `applied_ppm` in
//! `RealtimeAsrcCompensator::compensate` produces a SMALLER corrected audio-timeline advance per
//! callback (see that function's own doc comment), which pulls the audio timeline back toward — and
//! eventually behind — the raw one, reducing how far it leads. This has NOT been observed on a
//! live rig — the bound (`+/-10ppm`, `STEP_PPM=1.0`/window) is deliberately small enough that even
//! a wrong-signed correction only ever makes the residual worse by a bounded, safe amount (at most
//! the same order of magnitude the inner loop already clamps to), never an unbounded regression.
//! If the first live watchdog run shows the residual growing FASTER after a correction than
//! before, invert the sign in [`OuterLoopGuard::observe`] (the affected line is intentionally a
//! single `residual_avg.signum()` term) and re-run the tests below, which pin the CURRENT choice
//! explicitly and would need updating alongside it.
//!
//! ## Mirrors
//!
//! `scripts/av_sync_outer_loop_guard.py` is a literal Python port of this module (same constants,
//! same formula, own pytest suite proving numeric parity) for the actual watchdog process, which
//! is Python (it drives the SyncNet/ffmpeg pipeline `av_sync_measure.py` already owns) — keep the
//! two in lock-step, same convention `asrc-compensator.c` already follows for `src/asrc_bench.rs`.

use crate::asrc_bench::OUTER_BIAS_MAX_PPM;

/// Number of most-recent confident SyncNet residual samples averaged before the guard will act.
/// At the ticket's own ~7 minute measurement cadence this is ~21 minutes of SUSTAINED residual
/// before any correction — long enough that a single noisy/transient measurement can never trigger
/// a nudge on its own (the guard requires the window to be FULL of samples, not just one).
pub const WINDOW_N: usize = 3;

/// The ticket's own threshold, in ms: only a sustained average residual whose magnitude exceeds
/// this triggers a correction.
pub const RESIDUAL_THRESHOLD_MS: f64 = 40.0;

/// Rate limit: the maximum `bias_ppm` change per correction event (the ticket's own "rate limit"
/// safety rail) — never a jump straight to the target, always one small step per sustained window.
pub const STEP_PPM: f64 = 1.0;

/// One correction the guard decided to apply — the telemetry record a caller Discord-reports and
/// logs (the ticket's own "plna telemetria + Discord hlasenie kazdej upravy" safety rail).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorrectionEvent {
    /// The sustained window average (ms) that triggered this correction.
    pub avg_residual_ms: f64,
    /// `bias_ppm` before this correction.
    pub previous_bias_ppm: f64,
    /// `bias_ppm` after this correction (what the caller should now apply downstream).
    pub new_bias_ppm: f64,
}

/// The outer-loop guard itself — cadence-agnostic (it does not know or care how far apart in wall
/// time successive [`Self::observe`] calls are; the caller supplies confident SyncNet
/// measurements at whatever cadence it runs, ~7 min in production).
#[derive(Debug, Clone)]
pub struct OuterLoopGuard {
    /// Sliding window of the most recent confident residual samples, oldest first. Never exceeds
    /// `WINDOW_N` entries.
    window: Vec<f64>,
    /// The persistent bias this guard has converged on so far, in ppm. Does NOT decay.
    bias_ppm: f64,
}

impl OuterLoopGuard {
    /// A fresh guard: empty window, zero bias (no-op until the window fills and a sustained
    /// residual is observed).
    pub fn new() -> Self {
        Self {
            window: Vec::with_capacity(WINDOW_N),
            bias_ppm: 0.0,
        }
    }

    /// Restore a guard from a previously persisted bias (e.g. `scripts/av_sync_outer_loop_guard.py`
    /// reloading its JSON state after a watchdog restart) — the window itself is intentionally NOT
    /// persisted (a restart naturally re-warms it from fresh measurements; carrying over stale
    /// pre-restart samples would let old data influence a fresh session's decisions).
    pub fn from_bias_ppm(bias_ppm: f64) -> Self {
        Self {
            window: Vec::with_capacity(WINDOW_N),
            bias_ppm: bias_ppm.clamp(-OUTER_BIAS_MAX_PPM, OUTER_BIAS_MAX_PPM),
        }
    }

    /// The bias currently in effect, in ppm — feed this into
    /// `RealtimeAsrcCompensator::set_outer_bias_ppm` (or its C/obs-websocket equivalent).
    pub fn bias_ppm(&self) -> f64 {
        self.bias_ppm
    }

    /// Observe one CONFIDENT SyncNet residual measurement (ms, same sign convention as
    /// `scripts/av_sync_measure.py`'s `offset_ms` — see the module doc comment). Returns
    /// `Some(CorrectionEvent)` exactly when the bias actually changed this call, `None` otherwise
    /// (window not yet full, average within bounds, or already saturated at the clamp in the
    /// needed direction).
    pub fn observe(&mut self, residual_ms: f64) -> Option<CorrectionEvent> {
        // TEMP RED STUB (#806): window bookkeeping kept (so the field stays genuinely used), but
        // the actual sustained-threshold decision is not yet implemented.
        self.window.push(residual_ms);
        if self.window.len() > WINDOW_N {
            self.window.remove(0);
        }
        None
    }
}

impl Default for OuterLoopGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window that never fills must never act, however extreme the individual samples are.
    #[test]
    fn a_partial_window_never_triggers_a_correction() {
        let mut guard = OuterLoopGuard::new();
        for _ in 0..(WINDOW_N - 1) {
            assert_eq!(guard.observe(1_000.0), None);
        }
        assert_eq!(guard.bias_ppm(), 0.0);
    }

    /// Every sample under the threshold, even once the window is full, must never trigger a
    /// correction — this is the "sub-threshold residuals are simply fine" default-safe case.
    #[test]
    fn sub_threshold_residuals_never_trigger_a_correction() {
        let mut guard = OuterLoopGuard::new();
        for _ in 0..WINDOW_N {
            assert_eq!(guard.observe(10.0), None);
        }
        assert_eq!(guard.bias_ppm(), 0.0);
    }

    /// A single huge outlier sample, averaged together with otherwise-quiet samples, must not push
    /// the WINDOW AVERAGE past the threshold (proves the guard reacts to sustained averages, not
    /// to any one raw sample) — the actual anti-noise property the moving average exists for.
    #[test]
    fn one_transient_outlier_averaged_with_quiet_samples_stays_under_threshold() {
        let mut guard = OuterLoopGuard::new();
        // One large transient (120ms) then two quiet windows (0ms) -> avg = 40ms exactly at the
        // boundary; use a value that keeps the average STRICTLY under 40 to assert the sub-40
        // (inclusive-boundary) branch cleanly.
        guard.observe(90.0);
        guard.observe(0.0);
        let result = guard.observe(0.0); // avg = 30.0, under RESIDUAL_THRESHOLD_MS
        assert_eq!(result, None);
        assert_eq!(guard.bias_ppm(), 0.0);
    }

    /// THE gate target: a sustained positive residual (audio leads video, per the module's own
    /// sign convention) nudges the bias UP by exactly `STEP_PPM`, and reports a correct telemetry
    /// event.
    #[test]
    fn sustained_positive_residual_nudges_bias_up_by_one_step() {
        let mut guard = OuterLoopGuard::new();
        guard.observe(60.0);
        guard.observe(60.0);
        let event = guard
            .observe(60.0)
            .expect("a sustained 60ms average must trigger a correction");
        assert_eq!(event.avg_residual_ms, 60.0);
        assert_eq!(event.previous_bias_ppm, 0.0);
        assert_eq!(event.new_bias_ppm, STEP_PPM);
        assert_eq!(guard.bias_ppm(), STEP_PPM);
    }

    /// Mirror of the above for a sustained NEGATIVE residual (video leads audio) — the bias must
    /// move the OTHER way.
    #[test]
    fn sustained_negative_residual_nudges_bias_down_by_one_step() {
        let mut guard = OuterLoopGuard::new();
        guard.observe(-60.0);
        guard.observe(-60.0);
        let event = guard
            .observe(-60.0)
            .expect("a sustained -60ms average must trigger a correction");
        assert_eq!(event.avg_residual_ms, -60.0);
        assert_eq!(event.new_bias_ppm, -STEP_PPM);
        assert_eq!(guard.bias_ppm(), -STEP_PPM);
    }

    /// Repeated sustained corrections in the same direction must never push `bias_ppm` past
    /// `OUTER_BIAS_MAX_PPM` — the ticket's own hard bound — and once saturated, further sustained
    /// windows in the SAME direction report no further correction (nothing new happened).
    #[test]
    fn bias_never_exceeds_the_ticket_max_and_stops_reporting_once_saturated() {
        let mut guard = OuterLoopGuard::new();
        // Enough sustained 60ms windows to reach (and try to exceed) the 10ppm bound: 10 steps
        // needed, each step needs WINDOW_N=3 fresh observations after the window first fills.
        let mut last_event = None;
        for _ in 0..(WINDOW_N + 20) {
            last_event = guard.observe(60.0).or(last_event);
        }
        assert_eq!(guard.bias_ppm(), OUTER_BIAS_MAX_PPM);
        // One more sustained window in the same direction: already saturated -> no event.
        guard.observe(60.0);
        guard.observe(60.0);
        assert_eq!(guard.observe(60.0), None);
        assert_eq!(guard.bias_ppm(), OUTER_BIAS_MAX_PPM);
    }

    /// `from_bias_ppm` restores a persisted bias (watchdog restart) with the SAME hard clamp the
    /// setter applies, and starts with an empty (not pre-warmed) window.
    #[test]
    fn from_bias_ppm_restores_and_clamps() {
        let guard = OuterLoopGuard::from_bias_ppm(9_999.0);
        assert_eq!(guard.bias_ppm(), OUTER_BIAS_MAX_PPM);
        let mut guard = OuterLoopGuard::from_bias_ppm(3.0);
        assert_eq!(guard.bias_ppm(), 3.0);
        // The window is empty -- one sample alone must not be enough to trigger a correction.
        assert_eq!(guard.observe(1_000.0), None);
    }

    /// Anti-tautology guard: a residual sitting EXACTLY on the threshold boundary is defined as
    /// NOT triggering (`> RESIDUAL_THRESHOLD_MS`, not `>=`) — pins the boundary explicitly so a
    /// future refactor cannot silently flip it either way unnoticed.
    #[test]
    fn residual_exactly_at_the_threshold_does_not_trigger() {
        let mut guard = OuterLoopGuard::new();
        for _ in 0..WINDOW_N {
            assert_eq!(guard.observe(RESIDUAL_THRESHOLD_MS), None);
        }
        assert_eq!(guard.bias_ppm(), 0.0);
    }
}
