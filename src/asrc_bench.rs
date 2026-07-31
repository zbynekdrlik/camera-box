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
    fn compensate(&mut self, raw_advance_s: f64, _master_block_s: f64) -> f64 {
        // TODO(#804 green commit): not yet implemented — currently a bare pass-through, i.e.
        // performs NO compensation at all. This is the RED state: the gate test below expects
        // the compensated offset to stay bounded, which a pass-through cannot satisfy.
        let _ = self.alpha;
        raw_advance_s
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
}
