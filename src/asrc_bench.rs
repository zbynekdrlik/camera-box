//! #804 (epic #800 A/V-desync endgame round) — ASRC bench harness: two independent, free-running
//! clock domains simulated deterministically, with the TDD RED/GREEN gate issue #803's real ASRC
//! (per-source rate estimator + libswresample soft compensation, in the vendored libobs) must
//! satisfy before it is trusted.
//!
//! ## Root cause this simulates (epic #800 forensics, 2026-07-19 finding)
//!
//! Program audio at events arrives from a FOREIGN clock domain (Waves SoundGrid / Dante),
//! independent of the DanteSync/NTP-disciplined video master clock the whole rig is genlocked
//! to. OBS timestamps audio by SAMPLE COUNT — every 48000 samples is stamped as exactly 1 second
//! of internal timeline, regardless of the true real-time rate the foreign device's crystal
//! actually produced them at. When that crystal runs `ppm` parts-per-million off nominal, the
//! audio timeline drifts LINEARLY and UNBOUNDEDLY against the video master clock — measured live
//! at ~25-50 ppm, i.e. ~80-160 ms of A/V shift per hour, exactly matching the day-2/day-3 event
//! operator's repeated manual latency-knob walk-downs. A constant video-delay knob cannot
//! compensate a linearly GROWING offset — it can only zero it at one instant (already accepted
//! as report-only on issue #861, pending ASRC). The decided fix (epic #800) is continuous
//! audio-side ASRC (libswresample soft compensation) in libobs — issue #803. This module is step
//! 1: prove the mechanism and the compensation shape entirely offline, no rig required.
//!
//! ## Why this lives at the crate root (default features), not behind `probe`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (pulls `image`/`rqrr`/`qrcode`/`drm`,
//! which balloon the shared dev1 `target/` per this project's Local Build Policy). This bench
//! needs none of that — the drift mechanism is a closed-form relationship between sample counts
//! and wall-clock time, not a pixel/QR decode — so it lives here as a PURE module, mirroring
//! `src/reannounce.rs` / `src/av_window.rs`: it unit-tests Tier-0 (default features, no hardware,
//! no probe deps), which is exactly the ticket's own requirement ("beh na CI/bench stroji, nie na
//! produkčnom rigu").
//!
//! ## The compensation seam `AsrcCompensator` is meant to be MIRRORED, not reused, by #803
//!
//! [`AsrcCompensator::compensate`] is the same shape #803's real per-source estimator will
//! implement on the libobs side: given the raw (uncompensated) audio-timeline advance for one
//! control block plus the true master-clock duration of that block, return the advance AFTER
//! compensation is applied. Here it is backed by a synthetic EMA rate estimate
//! ([`EmaRateCompensator`]); in libobs #803 it will be backed by a REAL measured
//! samples-produced/wall-clock-elapsed ratio driving `swr_set_compensation`. Keeping the bench at
//! this same level of abstraction (a per-block advance-in/advance-out seam) is what lets #803
//! validate its real resample-ratio logic against this exact harness later, instead of needing a
//! second, unrelated proof.

/// Master-clock block duration used by the simulation, in seconds. 100 ms is the same order of
/// magnitude as an OBS audio callback / control-block interval — small enough that the RAW
/// per-block truncation error is negligible (see module docs), large enough that a >=4h
/// simulated run (`GATE_DURATION_S`) is a fast, deterministic loop (144 000 iterations of plain
/// float arithmetic), never a real sleep.
pub const BLOCK_S: f64 = 0.1;

/// Worst-case drift observed live during the #800 event forensics (the audio/video mismatch that
/// forced the operator's manual knob walk-downs, "+30/+50 ppm voči master clocku") — the bench's
/// acceptance-gate stress ppm.
pub const WORST_CASE_PPM: f64 = 50.0;

/// The epic's own acceptance duration: the bench must prove bounded drift over at least this many
/// simulated hours before ASRC (#803) is considered gate-worthy. 4 hours matches a typical event
/// day's continuous run length.
pub const GATE_DURATION_S: f64 = 4.0 * 3600.0;

/// The epic's own acceptance bound: `|offset_ms|` must stay under this value across the whole
/// `GATE_DURATION_S` run once ASRC compensation is active.
pub const GATE_MAX_OFFSET_MS: f64 = 40.0;

/// A free-running audio clock domain, drifting from the video master clock by `ppm` parts per
/// million. Positive `ppm` means the audio device's crystal runs FAST relative to master (the
/// live-measured direction — "audio leads", growing offset); negative `ppm` models a slow
/// crystal (offset growing the other way). Either sign is handled identically by the maths below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftingAudioClock {
    ppm: f64,
}

impl DriftingAudioClock {
    /// Build a clock drifting at the given `ppm` (parts-per-million) offset from nominal.
    pub fn new(ppm: f64) -> Self {
        Self { ppm }
    }

    /// This clock's true rate as a ratio of nominal (`1.0` = perfectly locked to master).
    pub fn true_ratio(&self) -> f64 {
        1.0 + self.ppm / 1_000_000.0
    }

    /// How much the UNCOMPENSATED audio timeline advances for a given master-clock block of
    /// duration `master_block_s` — this is the literal #800 mechanism: OBS stamps sample COUNT
    /// 1:1 against the timeline while the real device produces those samples at `true_ratio()`
    /// times the nominal rate, so the stamped advance is `master_block_s * true_ratio()`.
    pub fn raw_advance(&self, master_block_s: f64) -> f64 {
        master_block_s * self.true_ratio()
    }
}

/// The compensation seam issue #803's real per-source ASRC (libswresample soft compensation, in
/// the vendored libobs) is meant to mirror. Given the RAW (uncompensated) audio-timeline advance
/// for one control block, plus that block's true master-clock duration, returns the advance AFTER
/// compensation. `compensate(raw, master_block_s) == master_block_s` exactly means perfect lock
/// to master for that block.
pub trait AsrcCompensator {
    fn compensate(&mut self, raw_advance_s: f64, master_block_s: f64) -> f64;
}

/// Models "no ASRC" — the pre-#803 baseline. Passes the raw (drifting) advance through unchanged,
/// so a simulation run with this compensator reproduces the #800 mechanism exactly: unbounded
/// linear growth of `audio_timeline - master_timeline`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCompensation;

impl AsrcCompensator for NoCompensation {
    fn compensate(&mut self, raw_advance_s: f64, _master_block_s: f64) -> f64 {
        raw_advance_s
    }
}

/// Continuous EMA (exponential-moving-average) rate estimator + corrector — the bench's stand-in
/// for #803's real per-source estimator + `swr_set_compensation` resample-ratio application.
///
/// Each block it estimates the audio clock's current rate ratio from the observed raw advance
/// (an EMA over consecutive blocks, so it is robust to a single noisy sample the way a real
/// long-averaging-window rate estimator would be — see #803's plan note "dlhý horizont, robustný
/// na jitter"), then divides the raw advance by that estimate so the CORRECTED audio timeline
/// paces back to 1:1 with master, regardless of the true underlying ppm. No clicks/resets: this
/// mirrors libswresample's continuous soft compensation (`swr_set_compensation`), not a periodic
/// reset-to-zero.
#[derive(Debug, Clone, Copy)]
pub struct EmaRateCompensator {
    /// EMA smoothing factor in `(0.0, 1.0]`. Higher = faster convergence to the true ratio, lower
    /// = smoother/more jitter-robust (at the cost of a longer transient).
    alpha: f64,
    /// Current running estimate of the audio clock's rate ratio (starts at `1.0` = "assume
    /// locked" until the first observations correct it).
    estimated_ratio: f64,
}

impl EmaRateCompensator {
    /// Build a compensator with the given EMA smoothing factor. Panics if `alpha` is not in
    /// `(0.0, 1.0]` — an EMA outside that range is not a valid smoothing factor.
    pub fn new(alpha: f64) -> Self {
        assert!(
            alpha > 0.0 && alpha <= 1.0,
            "EMA smoothing factor must be in (0.0, 1.0], got {alpha}"
        );
        Self {
            alpha,
            estimated_ratio: 1.0,
        }
    }

    /// The compensator's current estimate of the audio clock's rate ratio (`1.0` = believed
    /// locked). Exposed for tests that want to observe convergence directly.
    pub fn estimated_ratio(&self) -> f64 {
        self.estimated_ratio
    }
}

impl AsrcCompensator for EmaRateCompensator {
    fn compensate(&mut self, raw_advance_s: f64, master_block_s: f64) -> f64 {
        // This block's instantaneous rate ratio, straight from the observation.
        let instantaneous_ratio = raw_advance_s / master_block_s;
        // EMA-smooth it into the running estimate (mirrors a real long-averaging-window rate
        // estimator: robust to a single jittery block, converges to the true ratio when it is
        // constant over many blocks).
        self.estimated_ratio =
            self.alpha * instantaneous_ratio + (1.0 - self.alpha) * self.estimated_ratio;
        // Correct: dividing the raw advance by the current estimate re-paces the corrected
        // timeline toward 1:1 with master as the estimate converges to the true ratio.
        raw_advance_s / self.estimated_ratio
    }
}

/// Run the bench: simulate `duration_s` seconds of master-clock time in `BLOCK_S`-sized blocks,
/// with the audio clock drifting at `ppm`, applying `compensator` every block. Returns the
/// `(audio_timeline - master_timeline)` offset trace, in milliseconds, one sample per block.
///
/// Deterministic and fast (no real sleeping) — `duration_s = GATE_DURATION_S` is ~144 000 blocks
/// of plain float arithmetic, well under a second of wall-clock test time.
pub fn simulate_offset_trace_ms(
    ppm: f64,
    duration_s: f64,
    compensator: &mut impl AsrcCompensator,
) -> Vec<f64> {
    let clock = DriftingAudioClock::new(ppm);
    let mut master_s = 0.0_f64;
    let mut audio_s = 0.0_f64;
    let mut trace = Vec::with_capacity((duration_s / BLOCK_S).ceil() as usize);
    while master_s < duration_s {
        let raw = clock.raw_advance(BLOCK_S);
        let applied = compensator.compensate(raw, BLOCK_S);
        audio_s += applied;
        master_s += BLOCK_S;
        trace.push((audio_s - master_s) * 1000.0);
    }
    trace
}

/// The largest `|offset_ms|` seen anywhere in a trace — the acceptance-gate quantity
/// (`GATE_MAX_OFFSET_MS` bounds this, not just the final sample, so a transient excursion counts).
pub fn max_abs_offset_ms(trace: &[f64]) -> f64 {
    trace.iter().fold(0.0_f64, |acc, &v| acc.max(v.abs()))
}

/// Hard bound issue #803's real servo clamps applied compensation to, in parts-per-million — an
/// order of magnitude above any measured worst case (epic #800: ~25-50 ppm), so it only ever
/// engages as a safety backstop against a bad measurement, never in ordinary operation.
pub const MAX_PPM: f64 = 300.0;

/// Hard bound on how fast the APPLIED compensation may change, in ppm per second of master-clock
/// time — keeps the resample-ratio nudge inaudible (issue #803: "nepočuteľné, žiadne kliky") even
/// if the estimator's target jumps abruptly.
pub const MAX_SLEW_PPM_PER_S: f64 = 5.0;

/// EMA time constant, in seconds, of the real per-source rate estimator. Long enough to be
/// robust to per-callback jitter (issue #803: "dlhý horizont, robustný na callback jitter") while
/// still meeting the ticket's own convergence text (<5 ppm at ~2 min, ~1 ppm at ~10 min, at the
/// #800 worst-case 50 ppm — see `estimator_converges_within_the_tickets_own_bounds` below).
pub const TIME_CONSTANT_S: f64 = 20.0;

/// Minimum wall-clock time the estimator must observe before ANY compensation is applied — the
/// "default-safe: zero compensation when the servo has no lock" requirement from issue #803. Below
/// this window the servo has not seen enough of the source's real clock to trust an estimate.
pub const MIN_LOCK_S: f64 = 5.0;

/// Hard bound on the OUTER-loop (issue #806) bias this servo will accept, in ppm — the ticket's
/// own "max +/-10 ppm uprava od inner-loop odhadu" safety rail. Applied at BOTH the setter (here)
/// and, redundantly, at the [`crate::asrc_outer_loop::OuterLoopGuard`] that produces the bias
/// value in the first place — belt+suspenders, since this field is also settable directly from
/// outside this crate (the vendored C mirror / the obs-websocket control channel), and neither
/// caller should be trusted alone to have already clamped.
pub const OUTER_BIAS_MAX_PPM: f64 = 10.0;

/// issue #960: sanity ceiling on a single block's `instantaneous_ppm`, in ppm — above this, the
/// block carries no real timing information (a starved or bursting audio source, e.g. a
/// muted/idle device path delivering near-zero samples) and must be REJECTED rather than folded
/// into the EMA. Live incident: a starved source (~26.24% of the samples its elapsed wall-clock
/// window implies) produced `instantaneous_ppm ≈ -737,600`, and with no gate the EMA converged
/// toward it and the servo railed at `-MAX_PPM` permanently.
///
/// 100,000 ppm (10%) is chosen to clear three boundaries with margin: (1) two orders of magnitude
/// above `MAX_PPM` (300, itself already "an order of magnitude above any measured worst case
/// ~25-50ppm"), so no real clock plausibly reaches it; (2) a clean 2x above the largest SYNTHETIC
/// stress value this file's own tests already feed to exercise the hard-clamp/slew-limit logic
/// (50,000 ppm, a deliberately extreme but non-starved "outlier measurement" in
/// `realtime_compensator_never_exceeds_the_slew_limit_per_call`) — those tests keep proving the
/// clamp/slew math, not this guard; (3) more than 7x below the observed live defect (737,600
/// ppm), so the reported bug is caught with comfortable margin.
pub const MAX_SANE_INSTANTANEOUS_PPM: f64 = 100_000.0;

/// The REAL per-source ASRC servo issue #803 ports into vendored libobs
/// (`vendor/obs-studio/libobs/media-io/asrc-compensator.c` — kept a line-by-line equivalent
/// mirror of this struct's logic; see that file's own doc comment). Unlike [`EmaRateCompensator`]
/// above (the bench's original teaching/proof-of-mechanism model, block-count-based), this is the
/// actual production design:
///
/// - a TIME-based (not block-count-based) EMA — real audio callbacks vary in frame count, so
///   smoothing must key on elapsed wall time, not a fixed block count;
/// - a hard ppm clamp (`MAX_PPM`) on the estimate used as the correction TARGET;
/// - a slew limiter (`MAX_SLEW_PPM_PER_S`) on the APPLIED correction, independent of how fast the
///   raw estimate itself moves, so a single noisy measurement can never produce an audible step;
/// - a minimum-lock startup delay (`MIN_LOCK_S`) before any correction is applied at all;
/// - a starvation/activity guard (`MAX_SANE_INSTANTANEOUS_PPM`, issue #960) rejecting a block
///   whose instantaneous ppm is not a plausible clock-drift measurement at all (a starved/bursting
///   source), holding state rather than folding garbage into the estimate.
///
/// Validated against the SAME `simulate_offset_trace_ms` gate issue #804 built, per that module's
/// own instruction not to invent a second, unrelated proof.
#[derive(Debug, Clone, Copy)]
pub struct RealtimeAsrcCompensator {
    /// Running EMA estimate of the source's true rate offset from master, in ppm.
    estimated_ppm: f64,
    /// The correction actually being applied right now (post-clamp, post-slew), in ppm.
    applied_ppm: f64,
    /// Cumulative master-clock time observed since construction — gates the `MIN_LOCK_S` startup
    /// delay.
    elapsed_lock_s: f64,
    /// The issue #806 OUTER-loop bias, in ppm — folded additively into `estimated_ppm` before the
    /// `MAX_PPM` clamp (see [`Self::compensate`]). Zero (no-op) until something calls
    /// [`Self::set_outer_bias_ppm`]; a fresh compensator behaves EXACTLY as before #806.
    outer_bias_ppm: f64,
    /// issue #960: cumulative count of blocks REJECTED as starved/bursting (see
    /// [`Self::compensate`]) — exposed for tests/telemetry, mirrors the C side's periodic ~60s
    /// log line reporting `starved_blocks=N`. Zero for a fresh compensator; never decreases on
    /// the Rust side (the C mirror resets its own copy on each telemetry read — a C-only,
    /// logging-cadence concern this bench has no equivalent of).
    starved_block_count: u32,
}

impl RealtimeAsrcCompensator {
    /// Build a compensator with no prior observations — starts at 0 ppm (assume locked) and
    /// applies no correction until `MIN_LOCK_S` of master-clock time has been observed.
    pub fn new() -> Self {
        Self {
            estimated_ppm: 0.0,
            applied_ppm: 0.0,
            elapsed_lock_s: 0.0,
            outer_bias_ppm: 0.0,
            starved_block_count: 0,
        }
    }

    /// The current raw EMA rate estimate, in ppm — exposed for tests/telemetry (mirrors the C
    /// side's periodic ~60s log line, issue #803's telemetry requirement).
    pub fn estimated_ppm(&self) -> f64 {
        self.estimated_ppm
    }

    /// The correction actually being applied right now (post-clamp, post-slew), in ppm — exposed
    /// for tests/telemetry.
    pub fn applied_ppm(&self) -> f64 {
        self.applied_ppm
    }

    /// Set the issue #806 outer-loop bias, in ppm — clamped to `+/-OUTER_BIAS_MAX_PPM`
    /// unconditionally (the caller's own clamping, e.g. [`crate::asrc_outer_loop::OuterLoopGuard`],
    /// is never trusted alone). Takes effect on the NEXT [`Self::compensate`] call; inert (folded
    /// into a target that is forced to 0.0) until the inner loop's own `MIN_LOCK_S` has elapsed.
    pub fn set_outer_bias_ppm(&mut self, bias_ppm: f64) {
        self.outer_bias_ppm = bias_ppm.clamp(-OUTER_BIAS_MAX_PPM, OUTER_BIAS_MAX_PPM);
    }

    /// The outer-loop bias currently in effect, in ppm — exposed for tests/telemetry.
    pub fn outer_bias_ppm(&self) -> f64 {
        self.outer_bias_ppm
    }

    /// issue #960: cumulative count of blocks rejected as starved/bursting since construction —
    /// exposed for tests/telemetry (mirrors the C side's `starved_blocks=N` telemetry field).
    pub fn starved_block_count(&self) -> u32 {
        self.starved_block_count
    }
}

impl Default for RealtimeAsrcCompensator {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrcCompensator for RealtimeAsrcCompensator {
    fn compensate(&mut self, raw_advance_s: f64, master_block_s: f64) -> f64 {
        if master_block_s <= 0.0 {
            // A non-positive block duration carries no timing information (e.g. a duplicate or
            // backward wall-clock read) — pass through unchanged rather than divide by a
            // non-positive number.
            return raw_advance_s;
        }

        // This block's instantaneous rate ratio, straight from the observation — exactly the
        // "delivered samples / wall-clock window" measurement issue #803 specifies, using the
        // SAME master-clock basis the video FIFO release already uses (genlock_wall_now_ns() on
        // the C side).
        let instantaneous_ppm = (raw_advance_s / master_block_s - 1.0) * 1_000_000.0;

        // issue #960: a block whose instantaneous ppm magnitude clears the sanity ceiling carries
        // no real timing information (starved/bursting source, not clock drift) — REJECT it: no
        // EMA update, no elapsed_lock_s credit (a garbage block must not count toward "the servo
        // has observed enough real data to trust its estimate"), no slew step. HOLD whatever
        // applied_ppm was already in effect and keep applying just that unchanged correction to
        // this callback's real audio (the samples themselves are real even when the elapsed-time
        // basis used to measure them is garbage).
        if instantaneous_ppm.abs() > MAX_SANE_INSTANTANEOUS_PPM {
            self.starved_block_count += 1;
            return raw_advance_s / (1.0 + self.applied_ppm / 1_000_000.0);
        }

        // TIME-based EMA smoothing factor: alpha = 1 - exp(-block/tau), so convergence speed is
        // independent of how the caller chunks audio callbacks (a real device may deliver
        // anywhere from a few ms to tens of ms per callback, unlike this bench's fixed BLOCK_S).
        let alpha = 1.0 - (-master_block_s / TIME_CONSTANT_S).exp();
        self.estimated_ppm = alpha * instantaneous_ppm + (1.0 - alpha) * self.estimated_ppm;

        self.elapsed_lock_s += master_block_s;

        // Default-safe: no lock yet -> target zero compensation, never guess from a
        // still-converging estimate (issue #806: the outer-loop bias is folded in HERE, so it is
        // just as inert as the inner estimate before lock — never applied on its own). Once
        // locked, add the outer-loop bias to the inner estimate and clamp the SUM to the hard ppm
        // bound before ever using it as a target.
        let target_ppm = if self.elapsed_lock_s < MIN_LOCK_S {
            0.0
        } else {
            (self.estimated_ppm + self.outer_bias_ppm).clamp(-MAX_PPM, MAX_PPM)
        };

        // Slew-limit the APPLIED correction toward the target — caps how fast the resample-ratio
        // nudge may change, independent of how fast the estimate itself moves.
        let max_step = MAX_SLEW_PPM_PER_S * master_block_s;
        let delta = (target_ppm - self.applied_ppm).clamp(-max_step, max_step);
        self.applied_ppm += delta;

        raw_advance_s / (1.0 + self.applied_ppm / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED proof of the #800 mechanism itself: with NO compensation, the worst-case measured ppm
    /// drives the offset past the 40 ms gate bound well before the 4h acceptance window elapses —
    /// this is the ticket's own "bez ASRC offset rastie zhodne s ppm" requirement, reproduced as
    /// a closed-form simulation instead of a live-rig incident.
    #[test]
    fn uncompensated_worst_case_ppm_blows_past_the_gate_bound_within_4h() {
        let mut none = NoCompensation;
        let trace = simulate_offset_trace_ms(WORST_CASE_PPM, GATE_DURATION_S, &mut none);
        let final_offset = *trace.last().expect("non-empty trace");
        // At 50 ppm over 4h the mechanism alone predicts ~720ms of drift (50 * 14400 / 1000) —
        // more than an order of magnitude past the 40ms bound. Assert it clearly fails the gate,
        // not just "differs from zero".
        assert!(
            final_offset.abs() > GATE_MAX_OFFSET_MS * 10.0,
            "expected uncompensated drift to blow well past the {GATE_MAX_OFFSET_MS}ms gate \
             bound over a {GATE_DURATION_S}s run, got {final_offset}ms"
        );
        // The offset must ALSO have grown monotonically in one direction (the "accumulator",
        // never a bounded wobble) — proves this is the linear-drift mechanism, not a fluke.
        assert!(
            max_abs_offset_ms(&trace[..10]) < max_abs_offset_ms(&trace),
            "offset must keep growing past its own early value — a bounded/self-correcting \
             trace would not reproduce the #800 unbounded-accumulator mechanism"
        );
    }

    /// Mechanism-fidelity check (not tautological): the uncompensated final offset must scale
    /// LINEARLY with ppm, matching the closed-form #800 model (`offset_ms = duration_s * ppm /
    /// 1000`) — confirms the simulation reproduces the actual sample-count-timestamping
    /// mechanism, not an arbitrary drift curve that merely happens to exceed the bound.
    #[test]
    fn uncompensated_offset_growth_scales_linearly_with_ppm() {
        let duration_s = 3600.0; // 1h is plenty to observe the linear relationship
        let mut none_a = NoCompensation;
        let mut none_b = NoCompensation;
        let offset_50 = *simulate_offset_trace_ms(50.0, duration_s, &mut none_a)
            .last()
            .unwrap();
        let offset_30 = *simulate_offset_trace_ms(30.0, duration_s, &mut none_b)
            .last()
            .unwrap();
        let ratio = offset_50 / offset_30;
        assert!(
            (ratio - 50.0 / 30.0).abs() < 1e-6,
            "expected offset(50ppm)/offset(30ppm) == 50/30 (linear in ppm), got {ratio}"
        );
    }

    /// THE gate target for this PR: with continuous EMA-based compensation active, the offset
    /// stays within the 40ms bound across the FULL 4h acceptance window at the worst-case
    /// measured ppm — the "s ASRC |offset| < 40 ms počas >=4 h behu" requirement.
    #[test]
    fn ema_compensated_offset_stays_within_gate_bound_over_4h_at_worst_case_ppm() {
        let mut compensator = EmaRateCompensator::new(0.3);
        let trace = simulate_offset_trace_ms(WORST_CASE_PPM, GATE_DURATION_S, &mut compensator);
        let worst = max_abs_offset_ms(&trace);
        assert!(
            worst < GATE_MAX_OFFSET_MS,
            "expected EMA-compensated |offset| to stay under {GATE_MAX_OFFSET_MS}ms across a \
             {GATE_DURATION_S}s run at {WORST_CASE_PPM}ppm, got a peak of {worst}ms"
        );
        // The compensator must actually have converged toward the true ratio (not stayed at its
        // "assume locked" starting value) — proves the bound above was earned by real
        // estimation, not by a ppm too small to matter.
        let true_ratio = DriftingAudioClock::new(WORST_CASE_PPM).true_ratio();
        assert!(
            (compensator.estimated_ratio() - true_ratio).abs() < 1e-6,
            "expected the EMA estimate to converge to the true ratio {true_ratio} over a \
             {GATE_DURATION_S}s run, got {}",
            compensator.estimated_ratio()
        );
    }

    /// Anti-tautology guard: a compensator that never actually estimates anything (a bare
    /// pass-through, i.e. `NoCompensation` used where a real ASRC is claimed) must NOT pass the
    /// bound above — proves the GREEN result depends on genuine estimation converging, not on a
    /// gate loose enough to pass regardless of what "compensation" does.
    #[test]
    fn a_pass_through_stub_does_not_satisfy_the_gate_bound() {
        let mut stub = NoCompensation;
        let trace = simulate_offset_trace_ms(WORST_CASE_PPM, GATE_DURATION_S, &mut stub);
        let worst = max_abs_offset_ms(&trace);
        assert!(
            worst > GATE_MAX_OFFSET_MS,
            "a pass-through stub must FAIL the {GATE_MAX_OFFSET_MS}ms gate bound (it performs no \
             compensation) — got {worst}ms, which would make the GREEN test above tautological"
        );
    }

    /// Compensation direction is symmetric: a SLOW audio crystal (negative ppm) must be bounded
    /// exactly as well as the FAST case the other tests exercise — the epic's own live "+30/+50
    /// ppm" numbers are all positive, but the compensator must not silently assume a sign.
    #[test]
    fn ema_compensated_offset_stays_bounded_for_negative_ppm_too() {
        let mut compensator = EmaRateCompensator::new(0.3);
        let trace = simulate_offset_trace_ms(-WORST_CASE_PPM, GATE_DURATION_S, &mut compensator);
        let worst = max_abs_offset_ms(&trace);
        assert!(
            worst < GATE_MAX_OFFSET_MS,
            "expected EMA-compensated |offset| to stay under {GATE_MAX_OFFSET_MS}ms for a \
             negative-ppm (slow) audio clock too, got a peak of {worst}ms"
        );
    }

    // ---- #803: the REAL per-source servo (RealtimeAsrcCompensator), validated against the SAME
    // gate #804 built, per asrc_bench's own instruction not to invent a second, unrelated proof. ----

    /// THE gate for issue #803 itself: the production-shaped servo (time-based EMA + hard clamp +
    /// slew limit + lock delay — everything the C port in vendor/obs-studio mirrors) must satisfy
    /// the identical 4h/50ppm/40ms bound issue #804 proved the shape against.
    #[test]
    fn realtime_compensator_stays_within_gate_bound_over_4h_at_worst_case_ppm() {
        let mut compensator = RealtimeAsrcCompensator::new();
        let trace = simulate_offset_trace_ms(WORST_CASE_PPM, GATE_DURATION_S, &mut compensator);
        let worst = max_abs_offset_ms(&trace);
        assert!(
            worst < GATE_MAX_OFFSET_MS,
            "expected the realtime servo's |offset| to stay under {GATE_MAX_OFFSET_MS}ms across a \
             {GATE_DURATION_S}s run at {WORST_CASE_PPM}ppm, got a peak of {worst}ms"
        );
        assert!(
            (compensator.applied_ppm() - WORST_CASE_PPM).abs() < 1.0,
            "expected the applied compensation to converge close to the true {WORST_CASE_PPM}ppm \
             offset over a {GATE_DURATION_S}s run, got {}ppm",
            compensator.applied_ppm()
        );
    }

    /// Symmetric for a slow (negative-ppm) crystal too — same reasoning as the EMA teaching model's
    /// own symmetry test above.
    #[test]
    fn realtime_compensator_stays_bounded_for_negative_ppm_too() {
        let mut compensator = RealtimeAsrcCompensator::new();
        let trace = simulate_offset_trace_ms(-WORST_CASE_PPM, GATE_DURATION_S, &mut compensator);
        let worst = max_abs_offset_ms(&trace);
        assert!(
            worst < GATE_MAX_OFFSET_MS,
            "expected the realtime servo's |offset| to stay under {GATE_MAX_OFFSET_MS}ms for a \
             negative-ppm (slow) audio clock too, got a peak of {worst}ms"
        );
    }

    /// Convergence-speed check straight from issue #803's own acceptance text: "<5 ppm za ~2 min,
    /// ~1 ppm za 10 min" at the #800 worst-case ppm. Feeds the servo BLOCK_S-sized ticks (same
    /// cadence the bench uses elsewhere) at a constant WORST_CASE_PPM drift and checks the
    /// estimate's error at each named horizon.
    #[test]
    fn estimator_converges_within_the_tickets_own_bounds() {
        let clock = DriftingAudioClock::new(WORST_CASE_PPM);
        let mut compensator = RealtimeAsrcCompensator::new();
        let mut elapsed_s = 0.0_f64;
        let mut error_at_2min = None;
        let mut error_at_10min = None;
        while elapsed_s < 601.0 {
            let raw = clock.raw_advance(BLOCK_S);
            let _ = compensator.compensate(raw, BLOCK_S);
            elapsed_s += BLOCK_S;
            if error_at_2min.is_none() && elapsed_s >= 120.0 {
                error_at_2min = Some((compensator.estimated_ppm() - WORST_CASE_PPM).abs());
            }
            if error_at_10min.is_none() && elapsed_s >= 600.0 {
                error_at_10min = Some((compensator.estimated_ppm() - WORST_CASE_PPM).abs());
            }
        }
        let err_2min = error_at_2min.expect("2min horizon reached");
        let err_10min = error_at_10min.expect("10min horizon reached");
        assert!(
            err_2min < 5.0,
            "expected estimator error < 5ppm at ~2min, got {err_2min}ppm"
        );
        assert!(
            err_10min < 1.0,
            "expected estimator error < 1ppm at ~10min, got {err_10min}ppm"
        );
    }

    /// Default-safe requirement: before `MIN_LOCK_S` of master-clock time has been observed, the
    /// servo must apply EXACTLY zero compensation — no lock yet means never guess.
    #[test]
    fn realtime_compensator_applies_zero_compensation_before_lock() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // One block well inside the MIN_LOCK_S startup window.
        let raw = DriftingAudioClock::new(WORST_CASE_PPM).raw_advance(1.0);
        let corrected = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected zero applied compensation before the {MIN_LOCK_S}s lock window elapses"
        );
        assert_eq!(
            corrected, raw,
            "expected the pre-lock corrected advance to equal the raw advance exactly (no \
             compensation applied yet)"
        );
    }

    /// Hard-bound requirement: even when the observed instantaneous rate implies an offset far
    /// beyond any realistic crystal drift, the APPLIED compensation must never exceed `MAX_PPM`.
    #[test]
    fn realtime_compensator_clamps_applied_ppm_to_the_hard_bound() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // A synthetic, unrealistically large offset (10,000 ppm) fed for long enough that both
        // the EMA estimate and the slew-limited applied value have every chance to converge.
        let extreme_clock = DriftingAudioClock::new(10_000.0);
        for _ in 0..7200 {
            // 7200 * 1.0s = 2h of 1s blocks — ample time for the 20s time-constant EMA to
            // converge AND for the slew limiter (5 ppm/s) to catch up to a clamped 300ppm target.
            let raw = extreme_clock.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        assert!(
            compensator.applied_ppm() <= MAX_PPM + 1e-6,
            "expected applied compensation to never exceed the {MAX_PPM}ppm hard bound, got {}ppm",
            compensator.applied_ppm()
        );
    }

    /// Slew-limit requirement: the APPLIED ppm may not change faster than `MAX_SLEW_PPM_PER_S`
    /// per second of master-clock time, even when the estimator's target jumps abruptly (a single
    /// noisy/outlier measurement must never produce an audible step).
    #[test]
    fn realtime_compensator_never_exceeds_the_slew_limit_per_call() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // Get past the lock window with a converged near-zero estimate first.
        for _ in 0..10 {
            let _ = compensator.compensate(1.0, 1.0);
        }
        let applied_before = compensator.applied_ppm();
        // One abrupt 1-second block reporting an enormous instantaneous rate (a single outlier
        // measurement, e.g. a scheduling hiccup) — the applied value must not jump further than
        // the slew limit allows in that one second.
        let raw = DriftingAudioClock::new(50_000.0).raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        let step = (compensator.applied_ppm() - applied_before).abs();
        assert!(
            step <= MAX_SLEW_PPM_PER_S + 1e-9,
            "expected the applied ppm to change by at most {MAX_SLEW_PPM_PER_S}ppm in a single \
             1s block, got a step of {step}ppm"
        );
    }

    /// Anti-tautology guard, mirrored for the realtime servo: a pass-through stub must still FAIL
    /// this gate (already proven generically above via `NoCompensation`, restated here so the
    /// realtime-servo test group is self-contained and doesn't rely on a shared fixture living
    /// elsewhere in the file).
    #[test]
    fn realtime_gate_is_not_tautological() {
        let mut stub = NoCompensation;
        let trace = simulate_offset_trace_ms(WORST_CASE_PPM, GATE_DURATION_S, &mut stub);
        assert!(
            max_abs_offset_ms(&trace) > GATE_MAX_OFFSET_MS,
            "a pass-through stub must still fail the realtime servo's gate bound"
        );
    }

    // ---- #806: the OUTER-loop bias extension on RealtimeAsrcCompensator ------------------------

    /// A fresh compensator's outer bias defaults to 0 ppm — a #806-unaware caller (every existing
    /// call site as of this PR) sees EXACTLY the pre-#806 behavior.
    #[test]
    fn outer_bias_defaults_to_zero() {
        let compensator = RealtimeAsrcCompensator::new();
        assert_eq!(compensator.outer_bias_ppm(), 0.0);
    }

    /// The setter clamps to +/-OUTER_BIAS_MAX_PPM even when handed a wildly out-of-range value —
    /// this field is also settable from outside this crate (the C mirror / obs-websocket), so the
    /// clamp must hold regardless of whether the caller already clamped.
    #[test]
    fn set_outer_bias_ppm_clamps_to_the_hard_bound() {
        let mut compensator = RealtimeAsrcCompensator::new();
        compensator.set_outer_bias_ppm(9_999.0);
        assert_eq!(compensator.outer_bias_ppm(), OUTER_BIAS_MAX_PPM);
        compensator.set_outer_bias_ppm(-9_999.0);
        assert_eq!(compensator.outer_bias_ppm(), -OUTER_BIAS_MAX_PPM);
    }

    /// A nonzero outer bias, once the inner loop has locked and converged on a PERFECT (0 ppm)
    /// clock, must show up in `applied_ppm` — proving the bias actually reaches the correction
    /// target rather than being a no-op field.
    #[test]
    fn outer_bias_shifts_applied_ppm_once_locked_on_a_perfect_clock() {
        let mut compensator = RealtimeAsrcCompensator::new();
        compensator.set_outer_bias_ppm(7.0);
        let clock = DriftingAudioClock::new(0.0); // a perfectly-matched clock: estimated_ppm -> 0
                                                  // Long enough for MIN_LOCK_S to elapse AND for the slew limiter (5 ppm/s) to fully catch
                                                  // up to a 7ppm target (needs >=1.4s of slew headroom; give it ample margin).
        for _ in 0..120 {
            let raw = clock.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        assert!(
            (compensator.applied_ppm() - 7.0).abs() < 1e-6,
            "expected the 7ppm outer bias to fully reach applied_ppm on a perfectly-matched \
             clock once converged, got {}ppm",
            compensator.applied_ppm()
        );
    }

    /// The outer bias is INERT before the inner loop's own `MIN_LOCK_S` — same default-safe
    /// guarantee the inner estimate itself already has, now proven to also cover the bias term.
    #[test]
    fn outer_bias_is_inert_before_lock() {
        let mut compensator = RealtimeAsrcCompensator::new();
        compensator.set_outer_bias_ppm(OUTER_BIAS_MAX_PPM);
        // One block, well inside the MIN_LOCK_S startup window.
        let raw = DriftingAudioClock::new(0.0).raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected zero applied compensation before the {MIN_LOCK_S}s lock window elapses, \
             even with a nonzero outer bias set"
        );
    }

    /// The inner estimate and the outer bias combined must still respect the overall `MAX_PPM`
    /// hard clamp — an already-saturated inner estimate plus the full +/-10ppm outer bias must
    /// never push the correction TARGET past `MAX_PPM`.
    #[test]
    fn outer_bias_combined_with_a_saturated_inner_estimate_still_respects_max_ppm() {
        let mut compensator = RealtimeAsrcCompensator::new();
        compensator.set_outer_bias_ppm(OUTER_BIAS_MAX_PPM);
        let extreme_clock = DriftingAudioClock::new(10_000.0);
        for _ in 0..7200 {
            let raw = extreme_clock.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        assert!(
            compensator.applied_ppm() <= MAX_PPM + 1e-6,
            "expected applied compensation to never exceed the {MAX_PPM}ppm hard bound even with \
             the outer bias saturated, got {}ppm",
            compensator.applied_ppm()
        );
    }

    // ---- #960: starvation/activity guard — a block with no real timing information must never
    // be folded into the estimate or rail the servo. -----------------------------------------

    /// THE gate for issue #960 itself: the live incident reproduced exactly (a starved source
    /// delivering ~26.24% of the samples its elapsed wall-clock window implies, i.e.
    /// `DriftingAudioClock::new(-737_600.0)` — the same instantaneous ppm the stream-OBS log
    /// showed for 'ASIO Input Capture'/'test-audio'). One such block must be REJECTED: the
    /// estimate and the applied correction must stay at their pre-block (converged, ~0) value,
    /// never railed toward -ASRC_MAX_PPM.
    #[test]
    fn starved_block_does_not_corrupt_the_estimate_960() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // Converge on a perfectly-matched (ppm=0) clock first, well past MIN_LOCK_S. With no
        // drift, instantaneous_ppm is exactly 0.0 every block, so the EMA/applied stay exactly
        // 0.0 (bit-for-bit) -- not merely "close to zero".
        for _ in 0..10 {
            let _ = compensator.compensate(1.0, 1.0);
        }
        assert_eq!(compensator.estimated_ppm(), 0.0);
        assert_eq!(compensator.applied_ppm(), 0.0);

        // issue #960: the exact live incident — a starved block reporting -737,600ppm.
        let starved = DriftingAudioClock::new(-737_600.0);
        let raw = starved.raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);

        // The guard's rejection path is an EARLY RETURN that never touches estimated_ppm/
        // applied_ppm at all -- so both must stay EXACTLY at their pre-block value (not just
        // "close"), which is what makes this assertion non-tautological: any regression that
        // lets the garbage ppm leak even partially into the EMA (a weakened guard, an off-by-one
        // threshold, a partial slew step) would move these away from bit-exact 0.0.
        assert_eq!(
            compensator.estimated_ppm(),
            0.0,
            "expected a starved block to be REJECTED (estimate left at its pre-block value), got \
             estimated_ppm={} — the -737,600ppm garbage was folded into the EMA, exactly the \
             #960 defect",
            compensator.estimated_ppm()
        );
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected the applied correction to stay HELD at its pre-starvation value, got \
             {}ppm — a starved block must never rail the servo toward -MAX_PPM",
            compensator.applied_ppm()
        );
    }

    /// The rejection must be OBSERVABLE, not just silently protective — issue #960 asks that the
    /// periodic telemetry log be able to report a starved/invalid-block state explicitly. A
    /// sustained starvation (several callbacks in a row, e.g. an ASIO dropout) must keep counting,
    /// and a healthy block afterward must not inflate the count further (proves the guard is
    /// scoped to genuinely-invalid blocks, not a sticky/latched state).
    #[test]
    fn starved_block_is_counted_960() {
        let mut compensator = RealtimeAsrcCompensator::new();
        assert_eq!(compensator.starved_block_count(), 0);

        let starved = DriftingAudioClock::new(-737_600.0);
        let raw = starved.raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.starved_block_count(),
            1,
            "expected one starved block to increment the counter exactly once"
        );

        let _ = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.starved_block_count(),
            2,
            "expected a second consecutive starved block to keep counting"
        );

        let _ = compensator.compensate(1.0, 1.0); // a healthy block
        assert_eq!(
            compensator.starved_block_count(),
            2,
            "expected a healthy block to leave the starved counter unchanged"
        );
    }

    /// A starved block must grant NO lock credit — it carries no real information about the
    /// source's true clock rate, so it must not count toward `MIN_LOCK_S` any more than it counts
    /// toward the estimate. Feed far more than `MIN_LOCK_S` worth of STARVED blocks, then one
    /// healthy block that is on its own well inside the lock window — if starved blocks wrongly
    /// granted lock credit, the servo would already be "locked" and immediately apply a nonzero
    /// correction; if they don't, applied_ppm must still be exactly 0 (pre-lock).
    #[test]
    fn starved_blocks_grant_no_lock_credit_960() {
        let mut compensator = RealtimeAsrcCompensator::new();
        let starved = DriftingAudioClock::new(-737_600.0);
        for _ in 0..20 {
            // 20 x 1s = 20s of starved blocks — well past MIN_LOCK_S (5s) if wrongly credited.
            let raw = starved.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        // One healthy block, 1s — well INSIDE MIN_LOCK_S on its own.
        let raw = DriftingAudioClock::new(WORST_CASE_PPM).raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected starved blocks to grant NO lock credit — one real block afterward should \
             still be pre-lock, got applied_ppm={}",
            compensator.applied_ppm()
        );
    }
}
