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

/// issue #1084: span cap of the sliding least-squares RATE regression, in seconds of master-clock
/// time. The pre-#1084 estimator was a fixed-gain time-EMA (`TIME_CONSTANT_S=20 s`) over 1 s
/// windows; live on the `mbc` source its `estimated` sd was 178 ppm (→ `applied` sd 20–28 ppm ≈
/// ±75–103 ms/h of global A/V wander) because the 1 s window master time TELESCOPES to two wall
/// reads and the audio-thread scheduling jitter in those endpoints does NOT average down with more
/// callbacks per window (see issue #1084's design comment). A regression over the cumulative
/// (master-time, audio-minus-master) points uses that endpoint noise near-optimally — slope sd ≈
/// σ_t·1e6·√(12/N)/L — and is robust to BOTH the white and the anti-correlated MA(1) window-noise
/// colors an EMA retune could only cover one of. 600 s (~600 one-per-1 s-window points) drives the
/// steady `applied` sd well under the 2.8 ppm (= 10 ms/h) acceptance while an EXPANDING-then-sliding
/// window keeps convergence fast-early / precise-late (tracks a drift STEP within ~10 min in the
/// bench). Mirror of asrc-compensator.h ASRC_REGRESSION_SPAN_S — keep numerically identical.
pub const REGRESSION_SPAN_S: f64 = 600.0;

/// issue #1084: minimum number of accepted-window points before the regression computes a slope at
/// all (a fit through fewer points is dominated by the endpoint noise). Mirror of
/// asrc-compensator.h ASRC_REGRESSION_MIN_POINTS.
pub const REGRESSION_MIN_POINTS: usize = 30;

/// issue #1084: minimum buffer SPAN (seconds of master-clock time between the oldest and newest
/// point) before ANY compensation is applied — the "default-safe: zero compensation when the servo
/// has no lock" guarantee (replaces the pre-#1084 `MIN_LOCK_S=5 s`, which was calibrated to the
/// EMA's fast convergence; the noise-limited regression needs a longer baseline before its slope is
/// trustworthy). Mirror of asrc-compensator.h ASRC_REGRESSION_LOCK_SPAN_S.
pub const REGRESSION_LOCK_SPAN_S: f64 = 60.0;

/// issue #1084: capacity of the point ring buffer. A 600 s span of windows that each close at ≥1 s
/// of master time holds ≤ ~601 points; 640 leaves headroom so age-based eviction, never a capacity
/// overflow, bounds the buffer. The C mirror uses a fixed array of this size + a head/count ring;
/// the Rust authority uses a `Vec` that pushes+evicts in the identical oldest→newest order, so both
/// feed the LS sums the SAME point sequence in the SAME iteration order (the numerical contract —
/// memory layout need not match). Mirror of asrc-compensator.h ASRC_REGRESSION_CAP.
pub const REGRESSION_CAP: usize = 640;

/// Hard bound on the OUTER-loop (issue #806) bias this servo will accept, in ppm — the ticket's
/// own "max +/-10 ppm uprava od inner-loop odhadu" safety rail. Applied at BOTH the setter (here)
/// and, redundantly, at the [`crate::asrc_outer_loop::OuterLoopGuard`] that produces the bias
/// value in the first place — belt+suspenders, since this field is also settable directly from
/// outside this crate (the vendored C mirror / the obs-websocket control channel), and neither
/// caller should be trusted alone to have already clamped.
pub const OUTER_BIAS_MAX_PPM: f64 = 10.0;

/// camera-box #1335: integral gain of the buffer-LEVEL holding term, in ppm per (ms of level error
/// × second of closed-window master time). The issue #1084 regression is a pure RATE loop — it
/// never reads the mix-buffer LEVEL, so any residual rate error the regression cannot remove (live:
/// the 600 s window lagging a ±1 ppm wandering true rate ⇒ ~0.8 ppm mean error) INTEGRATES into the
/// buffer and drifts it ~3 ms/h until an underrun. This slow integral, driven by `buffered_ms`,
/// nulls exactly that residual. 0.0002 ⇒ a 30 ms level error moves the correction 0.36 ppm/min; the
/// ±3 ms level noise floor moves it ±0.04 ppm/min (below the issue #1016 quantization resolution),
/// so it never fights the fast rate loop. Deliberately slow (an I-only loop on the integrator plant
/// that is the buffer holds the level BOUNDED — period ~3.9 h, amplitude ~2.24·residual_ppm ms — not
/// critically damped; that is the design's intent, "pomalá slučka … drží ±5 ms"). Mirror of
/// asrc-compensator.h ASRC_LEVEL_KI_PPM_PER_MS_S — keep numerically identical.
pub const LEVEL_KI_PPM_PER_MS_S: f64 = 0.0002;

/// camera-box #1335: hard clamp on the buffer-LEVEL integral, in ppm (±). Bounds the level term far
/// below the rate loop's own MAX_PPM so a stuck/misreported buffer level can never rail the servo;
/// the live residual it corrects is ~0.8 ppm, well inside ±3. Anti-windup pairs with this clamp: the
/// integral is not advanced while the composite rate target is saturated at ±MAX_PPM. Mirror of
/// asrc-compensator.h ASRC_LEVEL_INTEGRAL_MAX_PPM — keep numerically identical.
pub const LEVEL_INTEGRAL_MAX_PPM: f64 = 3.0;

/// camera-box #1335 follow-up 2: residual threshold, in ms, above which a newly-closed window is
/// treated as a STEP (a permanent sample-loss/dup or a wall-clock jump) rather than a real rate
/// point. Live 17.9. 18:52: an OBS StartStream stall lost ~50 ms of `mbc` input samples permanently
/// (buffered_ms 108 → 51, starved_blocks=0); that 50 ms step entered the 600 s regression and biased
/// the slope by ~= step/span = 50 ms / 600 s = 83 ppm (est +16 → -83 → -152 after a 2nd step). When
/// `|(y_actual - y_fit)·1000| > 10 ms` the servo RE-BASEs (shifts the cumulative anchor onto the
/// pre-step fit, keeps the lock+applied, does NOT insert the step point) instead of flushing (which
/// would enshrine the shifted level). 10 ms sits well above the residual noise floor of the 1 s
/// windows (ASIO callback jitter ~1-2 ms; ranné dáta reziduály < 3 ms) so ordinary noise never
/// re-bases (bench (c) pins 0 steps on 3 ms noise). Mirror of asrc-compensator.h
/// ASRC_STEP_RESIDUAL_MS — keep numerically identical.
pub const STEP_RESIDUAL_MS: f64 = 10.0;

/// camera-box #1335 follow-up 2: proportional gain of the FAST bounded level-RESTORE burst, in ppm
/// per ms of level error. Entered only when a re-base's step is corroborated by the buffer level
/// (a real sample loss/dup, not a wall-clock-only jump); adds `clamp(Kr·(buffered − target), ±100)`
/// to the correction target so a 50 ms deficit drives a ~-100 ppm stretch = ~8 min to refill (100 ppm
/// = 0.17 cent, inaudible), decaying as the buffer refills; exits at `|buffered − target| < 5 ms`.
/// SIGN follows the proven #1335 integral convention — a DEFICIT (buffered < target) yields a NEGATIVE
/// contribution (stretch, raises the buffer). NOTE (see the issue-1335-follow-up-2 anchors-confirmed
/// comment): the main design wrote `-Kr·(level − target)`, which for a deficit is POSITIVE = compress =
/// LOWERS the buffer — the opposite of its own stated "level below target => stretch" intent; the
/// implemented form `Kr·(buffered − target)` = the negation, matching the integral. Mirror of
/// asrc-compensator.h ASRC_LEVEL_RESTORE_K_PPM_PER_MS — keep numerically identical.
pub const LEVEL_RESTORE_K_PPM_PER_MS: f64 = 2.0;

/// camera-box #1335 follow-up 2: hard clamp on the fast level-RESTORE burst, in ppm (±). 100 ppm is
/// large enough to refill a ~50 ms sample-loss step in ~8 min yet is a 0.17-cent pitch nudge (below
/// audibility). Bounds the restore far below the rate loop's own MAX_PPM. Mirror of
/// asrc-compensator.h ASRC_LEVEL_RESTORE_MAX_PPM — keep numerically identical.
pub const LEVEL_RESTORE_MAX_PPM: f64 = 100.0;

/// camera-box #1335 follow-up 3: arm band (ms) for the FAST level restore when a DELIBERATE setpoint
/// shift ([`RealtimeAsrcCompensator::shift_level_target`]) moves the setpoint. A shift whose |delta|
/// is at least this arms the restore burst, so a deliberate audio sync-offset trim settles in minutes
/// with the integral frozen (follow-up 2) rather than the ~1 h the +/-3 ppm I term needs (the 18.9.
/// 12 h series: a 12 ms shift railed the integral for ~1 h and rang for hours). Equals the restore's
/// own exit band (`|buffered - target| < 5 ms`); arming below it would exit on the first tick. Mirror
/// of asrc-compensator.h ASRC_LEVEL_RESTORE_ARM_MS -- keep numerically identical.
pub const LEVEL_RESTORE_ARM_MS: f64 = 5.0;

/// camera-box #1335 follow-up 4: sustained-level-error band (ms) that arms the FAST bounded level
/// restore in the accepted-window branch, whatever caused the error. A level disturbance that
/// arrives with NO same-window residual step (an OBS StartStream input-sample loss, a mic/Dante
/// re-plug, a mixer hiccup) is invisible to the step arm (follow-up 2) and the shift arm
/// (follow-up 3), so the +/-3 ppm I term alone would take hours; this arm catches it. 12 ms sits
/// well above the +/-8 ms 1-s level scatter so ordinary noise never arms, yet below the ~14-32 ms
/// StartStream drops the 18.9. live incident produced. Mirror of asrc-compensator.h
/// ASRC_LEVEL_RESTORE_ARM_ERR_MS -- keep numerically identical.
pub const LEVEL_RESTORE_ARM_ERR_MS: f64 = 12.0;

/// camera-box #1335 follow-up 4: number of CONSECUTIVE accepted windows whose |level - target| is
/// at least LEVEL_RESTORE_ARM_ERR_MS required before the sustained-error arm fires (10 windows =
/// 10 s at WINDOW_S 1.0). A below-band window resets the count, so a false arm needs 10 consecutive
/// windows each >= 12 ms in MAGNITUDE (either sign -- the arm is on |level - target|, not a
/// direction), which the +/-8 ms scatter cannot produce (8 < 12); the 10 s detection delay plus a
/// bounded burst is the trade-off, and the burst only brings the level back within 5 ms of the
/// setpoint (harmless). Mirror of asrc-compensator.h ASRC_LEVEL_RESTORE_ARM_WINDOWS -- keep
/// numerically identical.
pub const LEVEL_RESTORE_ARM_WINDOWS: u32 = 10;

/// camera-box #1335 follow-up 5: proportional gain of the buffer-LEVEL P term, in ppm per ms of
/// level error — now the NORMAL LAW of the level loop, folded into the correction target every call
/// (once locked) as `clamp(Kp·level_err_ema_ms, ±LEVEL_KP_MAX_PPM)`, driven by the SMOOTHED error
/// ([`RealtimeAsrcCompensator::level_err_ema_ms`], an EMA with time constant [`LEVEL_EMA_TAU_S`])
/// rather than the raw per-window level. SIGN matches the proven #1335 integral (deficit ⇒ negative
/// ⇒ stretch). At Kp=2.0 the loop time constant is ≈ 1/(Kp·1e-3) = 500 s: a 15 ms error is within
/// ~3 ms in ~13 min (measured ~777 s), at inaudible rates (a 25 ms error saturates at
/// [`LEVEL_KP_MAX_PPM`] = 50 ppm = 3 ms/min). The 18.9. live test showed the raw-error 0.03/±1 term
/// (follow-ups 2-4) could not hold the level: the mean wandered ±10-15 ms around the setpoint over
/// hour-scale spans and the E2E A/V reading inherited it (−0.6 ms vs +15.0 ms 40 min apart, identical
/// pins). The 66x stronger gain is usable only BECAUSE the error is smoothed first — a raw 2 ppm/ms
/// on the ±10 ms mixer-tick phase noise would jitter the rate by ±20 ppm/s; the EMA attenuates that
/// below the ±50 clamp's own resolution. The integral (Ki, ±3) is kept for the DC residual only; the
/// restore paths (follow-ups 2-4) become rare backstops. Mirror of asrc-compensator.h
/// ASRC_LEVEL_KP_PPM_PER_MS — keep numerically identical.
pub const LEVEL_KP_PPM_PER_MS: f64 = 2.0;

/// camera-box #1335 follow-up 5: hard clamp on the buffer-LEVEL P term, in ppm (±). Replaces the
/// follow-ups 2-4 literal ±1.0 clamp. A 25 ms error saturates it (Kp·25 = 50 ppm = 3 ms/min = a
/// 0.005 % pitch offset while a large error decays, well inside the ±300 ppm ASRC envelope and the
/// 100 ppm bursts follow-up 2 already accepts). Bounds the P term far below the rate loop's own
/// MAX_PPM. Mirror of asrc-compensator.h ASRC_LEVEL_KP_MAX_PPM — keep numerically identical.
pub const LEVEL_KP_MAX_PPM: f64 = 50.0;

/// camera-box #1335 follow-up 5: time constant, in seconds of master-clock time, of the EMA that
/// smooths the per-window level error before the P term ([`LEVEL_KP_PPM_PER_MS`]) reads it. The
/// per-window level carries ±10 ms mixer-tick phase noise (18.9. live: consecutive 1-min samples
/// 75.2 / 95.3 / 85.1 / 96.2 around a ~86 mean); a 10 s EMA kills that noise while adding only ~10 s
/// of lag, irrelevant at the loop's 500 s time constant. Each accepted window blends with
/// `alpha = window_master_s / (LEVEL_EMA_TAU_S + window_master_s)` (a ~1 s window ⇒ alpha ≈ 0.091).
/// Mirror of asrc-compensator.h ASRC_LEVEL_EMA_TAU_S — keep numerically identical.
pub const LEVEL_EMA_TAU_S: f64 = 10.0;

/// issue #960: sanity ceiling on the (issue #962: WINDOWED, duration-weighted-summed) measured
/// ppm, in ppm — above this, the measurement carries no real timing information (a starved or
/// bursting audio source, e.g. a muted/idle device path delivering near-zero samples) and must be
/// REJECTED rather than folded into the estimate. Live incident (under the pre-#1084 EMA): a
/// starved source (~26.24% of the samples its elapsed wall-clock window implies) produced a measured
/// ppm of ~-737,600, and with no gate the estimator converged toward it and the servo railed at
/// `-MAX_PPM` permanently.
///
/// 100,000 ppm (10%) is chosen to clear three boundaries with margin: (1) ~333x (roughly 2.5
/// orders of magnitude) above `MAX_PPM` (300, itself already "an order of magnitude above any
/// measured worst case ~25-50ppm"), so no real clock plausibly reaches it; (2) a clean 2x above
/// the largest SYNTHETIC stress value this file's own tests already feed to exercise the
/// hard-clamp/slew-limit logic
/// (50,000 ppm, a deliberately extreme but non-starved "outlier measurement" in
/// `realtime_compensator_never_exceeds_the_slew_limit_per_call`) — those tests keep proving the
/// clamp/slew math, not this guard; (3) more than 7x below the observed live defect (737,600
/// ppm), so the reported bug is caught with comfortable margin.
pub const MAX_SANE_INSTANTANEOUS_PPM: f64 = 100_000.0;

/// issue #962: duration of the measurement WINDOW, in seconds of master-clock time, over which
/// `raw_advance_s` and `master_block_s` are duration-weighted SUMMED before computing a single
/// windowed ppm value (issue #1084: one point fed to the regression; pre-#1084: fed to the EMA) and
/// to gate against `MAX_SANE_INSTANTANEOUS_PPM` — see the module's #962 design comment
/// (`gh issue view 962 --comments`) for the full mechanism this fixes (per-block instantaneous ppm
/// is unmeasurable noise for small, bursty-delivery blocks, e.g. mbc's 128-sample Dante VSC blocks,
/// 2.667ms each).
///
/// `1.0` second is chosen so that: (1) it spans ~375 of mbc's 2.667ms blocks — ample
/// duration-weighted averaging for arrival-timing jitter within a window to cancel (summing physical
/// durations first is EXACT regardless of how unevenly the underlying blocks are chunked, unlike
/// dividing one block's own small, individually-noisy raw/master pair); (2) it is small relative to
/// the issue #1084 regression span (`REGRESSION_SPAN_S`, 600s), so a full span holds ~600 evenly
/// spaced points — a fine-grained, near-optimal least-squares fit — while each window is still large
/// enough that its own endpoint-jitter contribution is a single point's `y`-noise the regression
/// averages over N points (see [`REGRESSION_SPAN_S`]); (3) it degenerates EXACTLY to the pre-#962
/// per-block behavior for any call whose OWN `master_block_s` already reaches 1.0s (the window
/// closes on that single call, the windowed ppm reduces algebraically to that block's own
/// instantaneous ratio) — every existing servo test in this file already calls `compensate()` with
/// blocks that sum to exactly 1.0s per window, so the window mechanism itself changed none of their
/// per-window inputs.
pub const WINDOW_S: f64 = 1.0;

/// The REAL per-source ASRC servo issue #803 ports into vendored libobs
/// (`vendor/obs-studio/libobs/media-io/asrc-compensator.c` — kept a line-by-line equivalent
/// mirror of this struct's logic; see that file's own doc comment). Unlike [`EmaRateCompensator`]
/// above (the bench's original teaching/proof-of-mechanism model, block-count-based), this is the
/// actual production design. issue #1084 replaced the inner rate ESTIMATOR (previously a fixed-gain
/// time-EMA) with a sliding least-squares RATE REGRESSION — the EMA's variance under the 1 s
/// window's endpoint wall-jitter was the global A/V-wander root cause (see [`REGRESSION_SPAN_S`] and
/// issue #1084's design comment); everything AROUND the estimator (the #962 windowed data source,
/// the #960 starvation rail, the `MAX_PPM` clamp, the `MAX_SLEW_PPM_PER_S` slew limiter, the #806
/// outer bias) is unchanged:
///
/// - a WINDOWED, duration-weighted measurement (issue #962, [`WINDOW_S`]) is the DATA SOURCE — one
///   ppm-bearing point `(cum_master_s, cum_raw_s − cum_master_s)` is produced per accepted window
///   close, exactly as before; only what CONSUMES those points changed;
/// - the #960 starvation rail still REJECTS a window whose aggregate ppm clears
///   `MAX_SANE_INSTANTANEOUS_PPM`, now ALSO flushing the regression buffer (a starved window is a
///   level shift that would poison the slope for a full span — issue #1084);
/// - a sliding least-squares regression over the last [`REGRESSION_SPAN_S`] of points estimates the
///   rate slope directly; `estimated_ppm = 1e6·slope` once at least [`REGRESSION_MIN_POINTS`] points
///   exist, and any compensation is applied only once the buffer SPAN reaches
///   [`REGRESSION_LOCK_SPAN_S`] (the "default-safe: zero before lock" guarantee, replacing the
///   pre-#1084 `MIN_LOCK_S`);
/// - a hard ppm clamp (`MAX_PPM`) on the estimate+bias used as the correction TARGET, and a slew
///   limiter (`MAX_SLEW_PPM_PER_S`) on the APPLIED correction — both unchanged.
///
/// Validated against the SAME `simulate_offset_trace_ms` gate issue #804 built PLUS a two-clock-
/// domain endpoint-jitter bench (issue #1084) the old exact-per-window bench could not exercise.
#[derive(Debug, Clone)]
pub struct RealtimeAsrcCompensator {
    /// Running rate estimate of the source's true offset from master, in ppm — issue #1084: the
    /// least-squares slope of the point buffer times 1e6 (was the EMA estimate pre-#1084).
    estimated_ppm: f64,
    /// The correction actually being applied right now (post-clamp, post-slew), in ppm.
    applied_ppm: f64,
    /// The issue #806 OUTER-loop bias, in ppm — folded additively into `estimated_ppm` before the
    /// `MAX_PPM` clamp (see [`Self::compensate`]). Zero (no-op) until something calls
    /// [`Self::set_outer_bias_ppm`]; a fresh compensator behaves EXACTLY as before #806.
    outer_bias_ppm: f64,
    /// issue #960: cumulative count of blocks REJECTED as starved/bursting (see
    /// [`Self::compensate`]) — exposed for tests/telemetry, mirrors the C side's periodic ~60s
    /// log line reporting `starved_blocks=N`. Zero for a fresh compensator; never decreases on
    /// the Rust side (the C mirror resets its own copy on each telemetry read — a C-only,
    /// logging-cadence concern this bench has no equivalent of). issue #962: on a rejected WINDOW
    /// close, incremented by `window_block_count` (every block that fed the rejected window),
    /// preserving the pre-#962 telemetry meaning at window granularity.
    starved_block_count: u32,
    /// issue #962: duration-weighted sum of `raw_advance_s` observed in the CURRENT (not yet
    /// closed) measurement window.
    window_raw_s: f64,
    /// issue #962: duration-weighted sum of `master_block_s` observed in the CURRENT window —
    /// once this reaches [`WINDOW_S`], the window closes: a single ppm-bearing point is produced
    /// from `window_raw_s`/`window_master_s`, pushed to the regression (or rejected under the #960
    /// ceiling), and both sums reset to 0.0 for the next window.
    window_master_s: f64,
    /// issue #962: count of individual audio blocks folded into the CURRENT (not yet closed)
    /// window — reset to 0 alongside the sums above whenever the window closes.
    window_block_count: u32,
    /// issue #1084: the regression point buffer's x-axis — cumulative ACCEPTED-window master time,
    /// oldest→newest. The C mirror is a fixed [`REGRESSION_CAP`] array + head/count ring; this `Vec`
    /// pushes+age-evicts in the identical order, so both feed the LS sums the SAME sequence.
    reg_x: Vec<f64>,
    /// issue #1084: the regression point buffer's y-axis — cumulative (raw − master) at each
    /// accepted window close (its slope vs `reg_x` is the rate ratio − 1 = ppm/1e6).
    reg_y: Vec<f64>,
    /// issue #1084: running cumulative accepted-window master time (the newest `reg_x` value).
    cum_master_s: f64,
    /// issue #1084: running cumulative (raw − master) (the newest `reg_y` value).
    cum_ymm_s: f64,
    /// issue #1084: whether the buffer span has reached [`REGRESSION_LOCK_SPAN_S`] and the servo may
    /// apply compensation (replaces the pre-#1084 elapsed-lock gate). Cleared by a buffer flush.
    reg_locked: bool,
    /// issue #1335: the buffer-LEVEL setpoint, in ms — captured the FIRST time the rate regression
    /// locks (the depth the mixer had settled at), re-captured after every flush/relock (see
    /// [`Self::regression_flush`] + [`Self::level_captured`]). The level integral drives
    /// `buffered_ms` back toward this.
    level_target_ms: f64,
    /// issue #1335: the integral of the level error, in ppm, folded ADDITIVELY into the correction
    /// target INSIDE the servo loop (alongside `estimated_ppm` + `outer_bias_ppm`), clamped to
    /// ±[`LEVEL_INTEGRAL_MAX_PPM`]. Reset to 0 on a flush/relock. Zero (no-op) on the rate-only
    /// entry ([`AsrcCompensator::compensate`], `buffered_ms == None`).
    level_integral_ppm: f64,
    /// issue #1335: the most recent `buffered_ms` observed at an accepted window close — telemetry
    /// only (the C mirror prints it as the `asrc:` line's `level=` field).
    level_last_ms: f64,
    /// issue #1335: whether [`Self::level_target_ms`] has been captured since the last (re)lock —
    /// gates the one-shot setpoint capture. Cleared by a flush so a relock re-captures.
    level_captured: bool,
    /// issue #1335 follow-up 2: cumulative count of STEP re-base events (a closed window whose
    /// residual vs the current fit exceeded [`STEP_RESIDUAL_MS`], re-based instead of inserted) —
    /// exposed for tests/telemetry (the C mirror prints it as the `asrc:` line's `steps=` field).
    /// Never reset (a running total, like the estimate); a healthy source reads a stable count.
    step_count: u32,
    /// issue #1335 follow-up 2: the residual (ms) of the most recent re-base — telemetry only (the
    /// C mirror prints it as `last_step_ms=`). Sign preserved (a lost-samples step is negative).
    last_step_ms: f64,
    /// issue #1335 follow-up 2: whether the FAST bounded level-restore burst is currently active —
    /// entered on a level-corroborated re-base, exited at `|buffered − target| < 5 ms`. Cleared by a
    /// flush (the setpoint re-captures) and reset on construction. Telemetry: the C mirror prints it
    /// as `restore=0|1`.
    level_restore: bool,
    /// issue #1335 follow-up 4: count of CONSECUTIVE accepted windows whose |level - target| >=
    /// [`LEVEL_RESTORE_ARM_ERR_MS`] -- reaching [`LEVEL_RESTORE_ARM_WINDOWS`] arms the FAST level
    /// restore from a SUSTAINED level error (a disturbance with no same-window residual step). Reset
    /// on a below-band window, on arm, and wherever `level_restore` is reset (flush/new/restore-exit).
    level_err_windows: u32,
    /// issue #1335 follow-up 5: the EMA (time constant [`LEVEL_EMA_TAU_S`]) of the per-window level
    /// error (`buffered_ms − level_target_ms`), in ms — the SMOOTHED error the P term reads so the
    /// 66x stronger Kp=2.0 gain does not amplify the ±10 ms mixer-tick phase noise. Seeded with the
    /// first error after capture (`level_err_ema_seeded`), reset on flush/relock. A deliberate
    /// setpoint shift moves BOTH `level_target_ms` AND the buffer level by the same delta, so the
    /// error (`buffered − target`) is unchanged and this EMA is left untouched there (see
    /// [`RealtimeAsrcCompensator::shift_level_target`]). Mirror of the C `level_err_ema_ms`.
    level_err_ema_ms: f64,
    /// issue #1335 follow-up 5: whether `level_err_ema_ms` has been seeded since the last (re)lock —
    /// gates the one-shot EMA seed (first accepted window seeds `ema = err`, later windows blend).
    /// Cleared by a flush so a relock re-seeds. Mirror of the C `level_err_ema_seeded`.
    level_err_ema_seeded: bool,
}

impl RealtimeAsrcCompensator {
    /// Build a compensator with no prior observations — starts at 0 ppm (assume locked) and
    /// applies no correction until the regression buffer span reaches `REGRESSION_LOCK_SPAN_S`.
    pub fn new() -> Self {
        Self {
            estimated_ppm: 0.0,
            applied_ppm: 0.0,
            outer_bias_ppm: 0.0,
            starved_block_count: 0,
            window_raw_s: 0.0,
            window_master_s: 0.0,
            window_block_count: 0,
            reg_x: Vec::new(),
            reg_y: Vec::new(),
            cum_master_s: 0.0,
            cum_ymm_s: 0.0,
            reg_locked: false,
            level_target_ms: 0.0,        // issue #1335
            level_integral_ppm: 0.0,     // issue #1335
            level_last_ms: 0.0,          // issue #1335
            level_captured: false,       // issue #1335
            step_count: 0,               // issue #1335 follow-up 2
            last_step_ms: 0.0,           // issue #1335 follow-up 2
            level_restore: false,        // issue #1335 follow-up 2
            level_err_windows: 0,        // issue #1335 follow-up 4
            level_err_ema_ms: 0.0,       // issue #1335 follow-up 5
            level_err_ema_seeded: false, // issue #1335 follow-up 5
        }
    }

    /// issue #1084: discard the whole regression point buffer and its cumulative anchors, and drop
    /// the lock. Called on any LEVEL SHIFT — a #960 starved-window rejection or a non-positive
    /// `master_block_s` (a backward/duplicate wall read, e.g. an NTP step) — because a step in the
    /// cumulative would corrupt the slope for a full `REGRESSION_SPAN_S` as it slides through the
    /// window; re-converging from scratch is bounded (~a minute) and level shifts are rare on this
    /// source. Deliberately does NOT reset `estimated_ppm`/`applied_ppm` directly — so applied is
    /// HELD on the flushing call itself (no slew step runs that call). But because the flush DROPS
    /// the lock, every subsequent call sees `!reg_locked` → target 0 → applied SLEWS back to 0 (at
    /// `MAX_SLEW_PPM_PER_S`) over the ~`REGRESSION_LOCK_SPAN_S` re-lock window, then re-converges to
    /// the new slope once the buffer re-fills. This decay-to-zero-then-reconverge is default-safe (a
    /// level shift invalidates the old correction) and bounded (one spurious 1 s starved window ≈ a
    /// few ms of A/V step). Mirror of the C `asrc_regression_flush()`.
    fn regression_flush(&mut self) {
        self.reg_x.clear();
        self.reg_y.clear();
        self.cum_master_s = 0.0;
        self.cum_ymm_s = 0.0;
        self.reg_locked = false;
        // issue #1335: a level shift invalidates the captured setpoint AND the integral it built up;
        // drop both so a relock re-captures the setpoint and re-integrates from 0 (default-safe).
        self.level_integral_ppm = 0.0;
        self.level_captured = false;
        // issue #1335 follow-up 2: a flush is an UNINTENDED discontinuity that re-captures the
        // setpoint from the post-relock depth, so any in-progress fast level-restore is abandoned
        // (the buffer self-heals to whatever depth it re-locks at). step_count/last_step_ms are
        // running telemetry — never reset here.
        self.level_restore = false;
        // issue #1335 follow-up 4: a flush abandons any in-progress restore, so the sustained-error
        // window counter resets too.
        self.level_err_windows = 0;
        // issue #1335 follow-up 5: a flush re-captures the setpoint from the post-relock depth, so
        // the smoothed level error re-seeds from the first post-relock window.
        self.level_err_ema_ms = 0.0;
        self.level_err_ema_seeded = false;
    }

    /// The current rate estimate, in ppm (issue #1084: the least-squares regression slope times
    /// 1e6) — exposed for tests/telemetry (mirrors the C side's periodic ~60s log line, issue
    /// #803's telemetry requirement).
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
    /// into a target that is forced to 0.0) until the inner loop's own regression lock
    /// ([`REGRESSION_LOCK_SPAN_S`]) has been reached.
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

    /// issue #1335: the buffer-LEVEL integral currently in effect, in ppm — exposed for
    /// tests/telemetry (the C mirror prints it as the `asrc:` line's `integral=` field).
    pub fn level_integral_ppm(&self) -> f64 {
        self.level_integral_ppm
    }

    /// issue #1335: the captured buffer-LEVEL setpoint, in ms — exposed for tests/telemetry (the C
    /// mirror prints it as the `asrc:` line's `target=` field). 0.0 until the first lock.
    pub fn level_target_ms(&self) -> f64 {
        self.level_target_ms
    }

    /// issue #1335: the most recent `buffered_ms` the level loop observed, in ms — exposed for
    /// tests/telemetry (the C mirror prints it as the `asrc:` line's `level=` field).
    pub fn level_last_ms(&self) -> f64 {
        self.level_last_ms
    }

    /// issue #1335 follow-up 2: cumulative count of STEP re-base events — exposed for tests/telemetry
    /// (the C mirror prints it as the `asrc:` line's `steps=` field).
    pub fn step_count(&self) -> u32 {
        self.step_count
    }

    /// issue #1335 follow-up 2: the residual (ms) of the most recent re-base — exposed for
    /// tests/telemetry (the C mirror prints it as `last_step_ms=`).
    pub fn last_step_ms(&self) -> f64 {
        self.last_step_ms
    }

    /// issue #1335 follow-up 2: whether the fast bounded level-restore burst is currently active —
    /// exposed for tests/telemetry (the C mirror prints it as `restore=0|1`).
    pub fn level_restore(&self) -> bool {
        self.level_restore
    }

    /// issue #1335 follow-up: shift the captured buffer-LEVEL setpoint by `delta_ms` to track a
    /// DELIBERATE audio sync-offset change. A sync-offset change of Δ (ns→ms) moves this source's
    /// audio placement — and therefore its mix-buffer depth — by exactly Δ (obs-source.c applies
    /// `in.timestamp += sync_offset` BEFORE placement). Without this, the level integral would keep
    /// the OLD setpoint and REFILL the buffer back toward it, silently cancelling the deliberate
    /// audio trim (issue 1333) within ~1–2 h. Move the setpoint by the SAME Δ so the integral holds
    /// the NEW depth. `level_last_ms` is shifted too so the telemetry `level=` reads consistently
    /// with the shifted `target=` until the next accepted window refreshes it (it is telemetry-only —
    /// the error term reads the LIVE `buffered_ms`, never `level_last_ms`). The integral itself is
    /// left untouched (no windup).
    /// UNINTENDED discontinuities (dropout/relock) must NOT call this — they go through
    /// [`Self::regression_flush`], which drops `level_captured` so the setpoint re-captures and the
    /// buffer self-heals its calibrated depth. No-op until the setpoint has been captured (first
    /// rate lock). Exact mirror of the C `asrc_compensator_shift_level_target()`.
    ///
    /// issue #1335 follow-up 3: a shift whose `|delta| >= LEVEL_RESTORE_ARM_MS` (5 ms) ALSO arms the
    /// fast bounded level restore, so the level reaches the new depth in minutes with the integral
    /// frozen (follow-up 2) instead of the ~1 h at the +-3 ppm rail the plain I term needs (the 18.9.
    /// 12 h series). A sub-band shift arms nothing — the gentle I+P loop absorbs it.
    pub fn shift_level_target(&mut self, delta_ms: f64) {
        if self.level_captured {
            self.level_target_ms += delta_ms;
            self.level_last_ms += delta_ms;
            // camera-box #1335 follow-up 5: the deliberate shift moves BOTH level_target_ms (+delta,
            // this line) AND the buffer level itself (+delta, via the sync-offset re-stamp — the 18.9.
            // live test: level 80 → 108 ms in the same second as a +12 ms shift), so the smoothed
            // error (buffered − target) is UNCHANGED and level_err_ema_ms needs NO adjustment —
            // leaving it alone is exactly what "the smoothed error must not see a false transient"
            // requires. (The design's Architektúra wrote `level_err_ema_ms -= delta_ms` here; a
            // standalone-rustc probe shows that INJECTS a −delta transient, spiking the P term to the
            // ±5 ppm/window slew cap on the next window and FAILING the design's own follow-up-5 test
            // (c) `|Δapplied| ≤ 2 ppm`; with no adjustment the swing is 0. Same class as the
            // load-bearing follow-up-2 SIGN CORRECTION — flagged for the main's review on the ticket.)
            // Exact mirror of the C shift.
            // camera-box #1335 follow-up 3: a deliberate setpoint shift of at least the restore's
            // exit band arms the FAST bounded level restore, so the level reaches the new depth in
            // minutes (integral frozen per follow-up 2) rather than the ~1 h / hours-of-ringing the
            // +/-3 ppm I term needs (the 18.9. 12 h series). Below the band the existing I+P loop
            // settles the small shift without a restore burst. Exact mirror of the C arming line.
            if delta_ms.abs() >= LEVEL_RESTORE_ARM_MS {
                self.level_restore = true;
            }
        }
    }

    /// The audio-timeline advance AFTER applying whatever `applied_ppm` is CURRENTLY in effect —
    /// the single formula both the starved-rejection path and the normal (post-EMA/slew) path in
    /// [`Self::compensate`] return. Factored out (`/review` finding on issue #960) so the two call
    /// sites can never silently drift apart the way this project's C/Rust mirrors have before —
    /// see the top-level CLAUDE.md's repeated "mirror drifted apart" GOTCHAs.
    fn corrected_advance(&self, raw_advance_s: f64) -> f64 {
        raw_advance_s / (1.0 + self.applied_ppm / 1_000_000.0)
    }
}

impl Default for RealtimeAsrcCompensator {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrcCompensator for RealtimeAsrcCompensator {
    /// The RATE-only servo entry (the issue #804 bench seam) — runs the #962 windowing + #1084
    /// regression + #806 outer bias + clamp/slew, but NOT the issue #1335 buffer-LEVEL integral
    /// (which needs a `buffered_ms` this trait has no argument for). This entry has NO C analogue —
    /// the vendored C `asrc_compensator_compensate()` ALWAYS receives `buffered_ms`; use
    /// [`RealtimeAsrcCompensator::compensate_with_level`] for the C-equivalent full path. Keeping
    /// this rate-only so the shared trait / `simulate_offset_trace_ms` gate + every pre-#1335 test
    /// stay unchanged.
    fn compensate(&mut self, raw_advance_s: f64, master_block_s: f64) -> f64 {
        self.compensate_core(raw_advance_s, master_block_s, None)
    }
}

impl RealtimeAsrcCompensator {
    /// The C-equivalent FULL servo call: identical to [`AsrcCompensator::compensate`] PLUS the issue
    /// #1335 buffer-LEVEL holding integral, driven by the source's current mix-buffer depth
    /// `buffered_ms` (obs-source.c reads it from `audio_input_buf[0].size` via the shared
    /// `obs_source_input_buf_ms()` helper). Exact mirror of the C
    /// `asrc_compensator_compensate(c, raw, master, buffered_ms, &applied)`.
    pub fn compensate_with_level(
        &mut self,
        raw_advance_s: f64,
        master_block_s: f64,
        buffered_ms: f64,
    ) -> f64 {
        self.compensate_core(raw_advance_s, master_block_s, Some(buffered_ms))
    }

    /// Shared servo body. `buffered_ms == Some(_)` runs the issue #1335 level integral (the C path);
    /// `None` is the rate-only bench entry. Keep numerically identical to the C
    /// `asrc_compensator_compensate()`.
    fn compensate_core(
        &mut self,
        raw_advance_s: f64,
        master_block_s: f64,
        buffered_ms: Option<f64>,
    ) -> f64 {
        if master_block_s <= 0.0 {
            // A non-positive block duration carries no timing information (e.g. a duplicate or
            // backward wall-clock read — an NTP step) and, because the regression accumulates a
            // CUMULATIVE master time, it is also a level shift that would corrupt the slope. Flush
            // the buffer and pass through unchanged; applied_ppm is HELD (see regression_flush).
            self.regression_flush();
            return raw_advance_s;
        }

        // issue #962: accumulate this block's DURATION-WEIGHTED contribution into the current
        // measurement window -- summing first (rather than ratio-ing this one block alone) is
        // what cancels arrival-timing jitter: a genuinely bursty-but-otherwise-healthy source
        // (e.g. mbc's 128-sample Dante VSC blocks) delivers real samples at an uneven wall-clock
        // cadence, but the SUM of delivered-sample-duration over the SUM of elapsed wall time
        // still converges to the source's true clock ratio, regardless of how unevenly the
        // underlying blocks were chunked. This WINDOWED measurement is the unchanged DATA SOURCE
        // the issue #1084 regression consumes; see the module's #962 / WINDOW_S doc comments.
        self.window_raw_s += raw_advance_s;
        self.window_master_s += master_block_s;
        self.window_block_count += 1;

        if self.window_master_s >= WINDOW_S {
            // This window closes -- compute ONE windowed ppm value from the duration-weighted
            // sums (not this block's own instantaneous ratio); a valid window becomes ONE
            // regression point below, exactly the shape the pre-#1084 code fed to the EMA.
            let window_ppm = (self.window_raw_s / self.window_master_s - 1.0) * 1_000_000.0;
            let window_raw_s = self.window_raw_s;
            let window_master_s = self.window_master_s;
            let window_block_count = self.window_block_count;
            self.window_raw_s = 0.0;
            self.window_master_s = 0.0;
            self.window_block_count = 0;

            // issue #960 (applied to the WINDOW value, not a single block's instantaneous ratio --
            // issue #962): a window whose aggregate ppm magnitude clears the sanity ceiling carries
            // no real timing information (the source was genuinely starved/bursting for MOST of this
            // window) -- REJECT the whole window: no regression point. issue #1084: a starved window
            // is a LEVEL SHIFT (the source delivered a wrong sample count), so also FLUSH the
            // regression buffer -- keeping the pre-starvation points would corrupt the slope for a
            // full span as the shift slides through. applied_ppm is HELD at its pre-rejection value
            // (the early return skips the slew step below), even mid-slew toward an already-decided
            // target. Attribute every block that fed this window to starved_block_count, preserving
            // the pre-#962 telemetry meaning ("how many audio blocks were part of an unusable
            // measurement") at window granularity.
            if window_ppm.abs() > MAX_SANE_INSTANTANEOUS_PPM {
                self.starved_block_count =
                    self.starved_block_count.saturating_add(window_block_count);
                self.regression_flush();
                return self.corrected_advance(raw_advance_s);
            }

            // issue #1335 follow-up 2: STEP DETECTION -> RE-BASE. The prospective new cumulative
            // point (advance the anchors by this closed window). A window whose residual against the
            // CURRENT (pre-insert) fit exceeds STEP_RESIDUAL_MS is a permanent input sample-loss/dup
            // or a wall-clock jump, NOT a real rate change — inserting it would bias the 600 s slope
            // by step/span (the live 17.9. 18:52 est +16 -> -83 -> -152 swing). RE-BASE instead:
            // shift cum_ymm_s onto the pre-step fit (cum_ymm_s -= r), do NOT insert the point, keep
            // the lock + slope + applied (no 60 s decay). Gated to the C path (buffered_ms.is_some())
            // + reg_locked so the rate-only bench trait entry (buffered_ms == None) keeps the legacy
            // insert -- the slew/clamp tests and the #1084 endpoint-jitter gate feed that path with
            // deliberate outliers and must stay unchanged; the C mirror ALWAYS has buffered_ms, so
            // there the re-base is unconditional.
            let pt_master = self.cum_master_s + window_master_s;
            let pt_ymm = self.cum_ymm_s + (window_raw_s - window_master_s);
            // The single-window RESIDUAL vs the locked fit's RATE: how far THIS window's own advance
            // increment (raw − master) deviates from the slope's expected increment for the window.
            // It is a per-window quantity — the cumulative noise cancels (pt_ymm − cum_ymm_s == this
            // window's increment) — so ordinary ±1-3 ms window jitter stays well under
            // STEP_RESIDUAL_MS, while a real sample-loss/dup or wall-clock step (tens of ms in ONE
            // window) exceeds it. (A cumulative-point-vs-OLS-line residual would instead fire on the
            // random-walk excursion of accumulated noise — wrong signal; bench (c) pins 0 steps.)
            let r_s = (window_raw_s - window_master_s)
                - (self.estimated_ppm / 1_000_000.0) * window_master_s;
            let mut rebased = false;
            if buffered_ms.is_some() && self.reg_locked && (r_s * 1000.0).abs() > STEP_RESIDUAL_MS {
                // RE-BASE: cum_ymm_s -= r (the design's exact form) leaves the anchor on the pre-step
                // fit line (cum_ymm_before + slope·window_master), so future points align; do NOT
                // insert the step point; keep the lock + slope + applied (no 60 s decay).
                self.cum_master_s = pt_master;
                self.cum_ymm_s = pt_ymm - r_s;
                self.step_count = self.step_count.saturating_add(1);
                self.last_step_ms = r_s * 1000.0;
                if let Some(buf_ms) = buffered_ms {
                    self.level_last_ms = buf_ms;
                    // FAST bounded level restore, but ONLY if the buffer level corroborates a real
                    // sample loss/dup (|level err| >= half the residual magnitude). A wall-clock-only
                    // jump leaves buffered_ms unchanged => re-base only, no restore. Keeps the
                    // captured setpoint + integral (the design's intent).
                    if self.level_captured
                        && (buf_ms - self.level_target_ms).abs() >= 0.5 * (r_s * 1000.0).abs()
                    {
                        self.level_restore = true;
                    }
                }
                rebased = true;
            }

            if !rebased {
                // issue #1084: push one regression point -- (cumulative accepted-window master time,
                // cumulative raw-minus-master) -- then slide the buffer to the last REGRESSION_SPAN_S
                // and re-fit the rate slope. (The evict-before-append guard mirrors the C ring's fixed
                // capacity; age eviction already bounds a >=1 s-window buffer well under it.)
                self.cum_master_s = pt_master;
                self.cum_ymm_s = pt_ymm;
                // Defensive capacity guard (mirror of the C ring's fixed capacity): evict the oldest
                // point BEFORE appending if the buffer is already full, so the newest point never
                // overwrites a live slot. Age eviction (below) keeps a >=1 s-window buffer at ~601
                // points, well under REGRESSION_CAP, so this never fires in practice.
                while self.reg_x.len() >= REGRESSION_CAP {
                    self.reg_x.remove(0);
                    self.reg_y.remove(0);
                }
                self.reg_x.push(self.cum_master_s);
                self.reg_y.push(self.cum_ymm_s);
                let cutoff = self.cum_master_s - REGRESSION_SPAN_S;
                while self.reg_x.len() > 1 && self.reg_x[0] < cutoff {
                    self.reg_x.remove(0);
                    self.reg_y.remove(0);
                }

                let n = self.reg_x.len();
                if n >= REGRESSION_MIN_POINTS {
                    // Re-anchor to the oldest point (bounded magnitudes -> no catastrophic
                    // cancellation over a long run) and recompute the five ordinary-least-squares
                    // sums in FULL, in a fixed oldest->newest iteration order -- deterministic and
                    // bit-identically mirrorable by the C ring (no incremental subtract-on-evict,
                    // whose FP rounding would drift the two apart). slope = (n*Sxy - Sx*Sy) / (n*Sxx
                    // - Sx*Sx); the rate offset in ppm is slope * 1e6.
                    let x0 = self.reg_x[0];
                    let y0 = self.reg_y[0];
                    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
                    for i in 0..n {
                        let x = self.reg_x[i] - x0;
                        let y = self.reg_y[i] - y0;
                        sx += x;
                        sy += y;
                        sxx += x * x;
                        sxy += x * y;
                    }
                    let nf = n as f64;
                    let denom = nf * sxx - sx * sx;
                    if denom.abs() > 1e-9 {
                        let slope = (nf * sxy - sx * sy) / denom;
                        self.estimated_ppm = slope * 1_000_000.0;
                    }
                    if self.reg_x[n - 1] - self.reg_x[0] >= REGRESSION_LOCK_SPAN_S {
                        self.reg_locked = true;
                    }
                }

                // issue #1335: buffer-LEVEL holding integral, updated ONCE per closed ACCEPTED window
                // (a rejected window early-returned above; a re-based window took the branch above;
                // an unlocked servo skips the update). The C mirror reads buffered_ms from
                // source->audio_input_buf[0].size every callback; the rate-only bench entry passes
                // None and never runs this.
                if let Some(buf_ms) = buffered_ms {
                    self.level_last_ms = buf_ms;
                    if self.reg_locked {
                        if !self.level_captured {
                            // setpoint = the buffer depth the mixer had settled at when the rate loop
                            // first locked; re-captured after every flush/relock.
                            self.level_target_ms = buf_ms;
                            self.level_captured = true;
                        }
                        // issue #1335 follow-up 5: SMOOTH the per-window level error with an EMA (time
                        // constant LEVEL_EMA_TAU_S) BEFORE the P term reads it, so the 66x stronger
                        // Kp=2.0 gain does not amplify the ±10 ms mixer-tick phase noise (the 18.9.
                        // live test: the raw-error term could not hold the level, the mean wandered
                        // ±10-15 ms). Seed with the first error after capture; later windows blend
                        // with alpha = window_master_s / (tau + window_master_s). A deliberate setpoint
                        // shift moves BOTH the target and the buffer by the same delta, so the error is
                        // unchanged and this EMA is left untouched there (see shift_level_target).
                        // Exact mirror of the C accepted-window branch.
                        let level_err = buf_ms - self.level_target_ms;
                        if !self.level_err_ema_seeded {
                            self.level_err_ema_ms = level_err;
                            self.level_err_ema_seeded = true;
                        } else {
                            let alpha = window_master_s / (LEVEL_EMA_TAU_S + window_master_s);
                            self.level_err_ema_ms += alpha * (level_err - self.level_err_ema_ms);
                        }
                        // Anti-windup: integrate only while the composite rate target is not clamped
                        // at the hard ±MAX_PPM bound AND the fast level-restore burst is not active
                        // (issue #1335 follow-up 2: freeze the integral during a restore so the two
                        // level correctors don't wind against each other). err_ms = target -
                        // buffered; a DEFICIT (buffer below setpoint) drives the integral MORE
                        // NEGATIVE => more-negative applied => STRETCH => raises the buffer (sign
                        // confirmed by the issue-1335 live -5 ppm outer-bias test, 17.9.).
                        // window_master_s is this closed window's master duration (~1 s).
                        let rate_target =
                            self.estimated_ppm + self.outer_bias_ppm + self.level_integral_ppm;
                        let saturated = rate_target <= -MAX_PPM || rate_target >= MAX_PPM;
                        if !saturated && !self.level_restore {
                            let err_ms = self.level_target_ms - buf_ms;
                            self.level_integral_ppm = (self.level_integral_ppm
                                - LEVEL_KI_PPM_PER_MS_S * err_ms * window_master_s)
                                .clamp(-LEVEL_INTEGRAL_MAX_PPM, LEVEL_INTEGRAL_MAX_PPM);
                        }
                        // issue #1335 follow-up 4: arm the FAST bounded level restore on a SUSTAINED
                        // level error, whatever caused it (a StartStream input-sample loss, a
                        // mic/Dante re-plug, a mixer hiccup) — the case the step arm (follow-up 2) and
                        // the shift arm (follow-up 3) both miss because it arrives with no same-window
                        // residual step (the 18.9. 12:00 StartStream: level 100 -> 68 ms,
                        // steps=1 last_step_ms=-14.3 detected before the level drained, restore never
                        // armed, run A/V +15 ms). Count consecutive accepted windows >= the band; a
                        // below-band window resets; reaching the window threshold arms and resets.
                        // The integral above is frozen while restoring so the two level correctors
                        // never wind against each other; the restore burst/exit are unchanged.
                        if self.level_captured && !self.level_restore {
                            if (buf_ms - self.level_target_ms).abs() >= LEVEL_RESTORE_ARM_ERR_MS {
                                self.level_err_windows += 1;
                                if self.level_err_windows >= LEVEL_RESTORE_ARM_WINDOWS {
                                    self.level_restore = true;
                                    self.level_err_windows = 0;
                                }
                            } else {
                                self.level_err_windows = 0;
                            }
                        }
                    }
                }
            }
        }

        // Default-safe: no lock yet -> target zero compensation, never guess from a
        // still-converging (short-baseline) slope (issue #806: the outer-loop bias is folded in
        // HERE, so it is just as inert as the inner estimate before lock — never applied on its
        // own). Once locked, add the outer-loop bias to the slope estimate and clamp the SUM to the
        // hard ppm bound before ever using it as a target.
        let target_ppm = if !self.reg_locked {
            0.0
        } else {
            // issue #1335: the buffer-LEVEL integral is folded in alongside the outer bias.
            let mut t = self.estimated_ppm + self.outer_bias_ppm + self.level_integral_ppm;
            // issue #1335 follow-up 2: the LEVEL P term + the fast bounded restore burst, both driven
            // by the LIVE buffered_ms (C path only; inert on the rate-only bench entry, where
            // buffered_ms == None and the level terms have no captured setpoint to act on). SIGN
            // matches the proven #1335 integral: (buffered - target) < 0 (deficit) => negative =>
            // stretch => raises the buffer. (The main design wrote these with the opposite argument
            // order; see the issue-1335-follow-up-2 anchors-confirmed comment for the derivation.)
            if let Some(buf_ms) = buffered_ms {
                if self.level_captured {
                    let err = buf_ms - self.level_target_ms;
                    // issue #1335 follow-up 5: P term is now the NORMAL LAW of the level loop —
                    // Kp=2.0 on the SMOOTHED error (level_err_ema_ms) clamped ±LEVEL_KP_MAX_PPM, loop
                    // time constant ~500 s. The raw err below still drives the restore burst/exit.
                    t += (LEVEL_KP_PPM_PER_MS * self.level_err_ema_ms)
                        .clamp(-LEVEL_KP_MAX_PPM, LEVEL_KP_MAX_PPM);
                    // Fast bounded restore: a big proportional stretch/compress that refills a
                    // sample-loss step in minutes, then exits once the buffer is back within 5 ms.
                    if self.level_restore {
                        if err.abs() < 5.0 {
                            self.level_restore = false;
                            // issue #1335 follow-up 4: the restore just brought the level within band;
                            // reset the sustained-error counter alongside clearing level_restore.
                            self.level_err_windows = 0;
                        } else {
                            t += (LEVEL_RESTORE_K_PPM_PER_MS * err)
                                .clamp(-LEVEL_RESTORE_MAX_PPM, LEVEL_RESTORE_MAX_PPM);
                        }
                    }
                }
            }
            // The SUM is clamped to the hard ppm bound before ever being used as a target.
            t.clamp(-MAX_PPM, MAX_PPM)
        };

        // Slew-limit the APPLIED correction toward the target — caps how fast the resample-ratio
        // nudge may change, independent of how fast the estimate itself moves.
        let max_step = MAX_SLEW_PPM_PER_S * master_block_s;
        let delta = (target_ppm - self.applied_ppm).clamp(-max_step, max_step);
        self.applied_ppm += delta;

        self.corrected_advance(raw_advance_s)
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

    /// Default-safe requirement: before the regression buffer span reaches `REGRESSION_LOCK_SPAN_S`
    /// (issue #1084), the servo must apply EXACTLY zero compensation — no lock yet means never guess.
    #[test]
    fn realtime_compensator_applies_zero_compensation_before_lock() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // One block, well inside the startup window (a single point can never span the lock).
        let raw = DriftingAudioClock::new(WORST_CASE_PPM).raw_advance(1.0);
        let corrected = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected zero applied compensation before the {REGRESSION_LOCK_SPAN_S}s lock span is reached"
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
        // the regression slope estimate and the slew-limited applied value have every chance to
        // converge.
        let extreme_clock = DriftingAudioClock::new(10_000.0);
        for _ in 0..7200 {
            // 7200 * 1.0s = 2h of 1s blocks — ample time for the regression to lock+converge AND
            // for the slew limiter (5 ppm/s) to catch up to a clamped 300ppm target.
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
        // Get PAST the regression lock (>= REGRESSION_LOCK_SPAN_S = 60s span, >= 30 points) with a
        // converged near-zero estimate first — 65 x 1s blocks of a perfectly-matched clock.
        for _ in 0..65 {
            let _ = compensator.compensate(1.0, 1.0);
        }
        let applied_before = compensator.applied_ppm();
        // One abrupt 1-second block reporting an enormous instantaneous rate (a single outlier
        // window, e.g. a scheduling hiccup) — it is below the #960 ceiling so it enters the
        // regression as a high-leverage newest point and yanks the slope target far up; the APPLIED
        // value must still not jump further than the slew limit allows in that one second.
        let raw = DriftingAudioClock::new(50_000.0).raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        let step = (compensator.applied_ppm() - applied_before).abs();
        assert!(
            step <= MAX_SLEW_PPM_PER_S + 1e-9,
            "expected the applied ppm to change by at most {MAX_SLEW_PPM_PER_S}ppm in a single \
             1s block, got a step of {step}ppm"
        );
        // ... and the slew limiter must genuinely BIND here (the outlier target far exceeds
        // MAX_SLEW_PPM_PER_S), else the test would pass vacuously without exercising the clamp.
        assert!(
            step >= MAX_SLEW_PPM_PER_S - 1e-6,
            "expected the outlier to drive the target past the slew limit so the clamp binds, got \
             a step of only {step}ppm (the slew limiter was not exercised)"
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
                                                  // 120 x 1s blocks: past REGRESSION_LOCK_SPAN_S (60s span) AND enough slew headroom
                                                  // (5 ppm/s) to fully reach a 7ppm target (needs >=1.4s; ample margin).
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

    /// The outer bias is INERT before the inner loop's own regression lock (issue #1084:
    /// `REGRESSION_LOCK_SPAN_S`) — same default-safe guarantee the inner estimate itself already
    /// has, now proven to also cover the bias term.
    #[test]
    fn outer_bias_is_inert_before_lock() {
        let mut compensator = RealtimeAsrcCompensator::new();
        compensator.set_outer_bias_ppm(OUTER_BIAS_MAX_PPM);
        // One block, well inside the startup window (a single point can never span the lock).
        let raw = DriftingAudioClock::new(0.0).raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);
        assert_eq!(
            compensator.applied_ppm(),
            0.0,
            "expected zero applied compensation before the {REGRESSION_LOCK_SPAN_S}s lock span is \
             reached, even with a nonzero outer bias set"
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
        // Converge + LOCK on a perfectly-matched (ppm=0) clock first, past the regression lock
        // (REGRESSION_LOCK_SPAN_S = 60s span, >= 30 points): with no drift every window point is
        // exactly (k, 0.0), so the least-squares slope is exactly 0.0 and estimated/applied stay
        // exactly 0.0 (bit-for-bit) -- not merely "close to zero".
        for _ in 0..65 {
            let _ = compensator.compensate(1.0, 1.0);
        }
        assert_eq!(compensator.estimated_ppm(), 0.0);
        assert_eq!(compensator.applied_ppm(), 0.0);

        // issue #960: the exact live incident — a starved block reporting -737,600ppm.
        let starved = DriftingAudioClock::new(-737_600.0);
        let raw = starved.raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);

        // The guard's rejection path is an EARLY RETURN that flushes the buffer but never touches
        // estimated_ppm/applied_ppm -- so both must stay EXACTLY at their pre-block value (not just
        // "close"), which is what makes this assertion non-tautological: any change that lets the
        // garbage ppm leak even partially into the estimate (a weakened guard, an off-by-one
        // threshold, a partial slew step) would move these away from bit-exact 0.0.
        assert_eq!(
            compensator.estimated_ppm(),
            0.0,
            "expected a starved block to be REJECTED (estimate left at its pre-block value), got \
             estimated_ppm={} — the -737,600ppm garbage was folded into the estimate, exactly the \
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
    /// source's true clock rate, so it must not advance the regression toward its lock span
    /// (issue #1084: a starved window FLUSHES the buffer, so no points ever accumulate from
    /// starved data). Feed far more than `REGRESSION_LOCK_SPAN_S` worth of STARVED blocks, then
    /// one healthy block — if starved blocks wrongly granted lock credit, the servo would already
    /// be "locked" and immediately apply a nonzero correction; if they don't, applied_ppm must
    /// still be exactly 0 (pre-lock).
    #[test]
    fn starved_blocks_grant_no_lock_credit_960() {
        let mut compensator = RealtimeAsrcCompensator::new();
        let starved = DriftingAudioClock::new(-737_600.0);
        for _ in 0..80 {
            // 80 x 1s = 80s of starved blocks — well past REGRESSION_LOCK_SPAN_S (60s) if the
            // flush-on-rejection wrongly let starved windows accumulate lock credit.
            let raw = starved.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        // One healthy block, 1s — a single point can never span the lock on its own.
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

    /// Boundary check for the `>` comparison itself (`/review` finding on issue #960): a block
    /// just BELOW the sanity ceiling is still treated as real (pushed to the regression, never
    /// flagged), and a block just ABOVE it is rejected. Pins the strict `>` (not `>=`) choice
    /// explicitly, on values comfortably clear of floating-point rounding noise (±1ppm at this
    /// magnitude), so a future edit can't silently flip which side of the ceiling is "sane"
    /// without a test noticing.
    #[test]
    fn threshold_boundary_960() {
        let mut below = RealtimeAsrcCompensator::new();
        let raw_below = DriftingAudioClock::new(MAX_SANE_INSTANTANEOUS_PPM - 1.0).raw_advance(1.0);
        let _ = below.compensate(raw_below, 1.0);
        assert_eq!(
            below.starved_block_count(),
            0,
            "expected a block just BELOW MAX_SANE_INSTANTANEOUS_PPM to be treated as real, not \
             starved"
        );

        let mut above = RealtimeAsrcCompensator::new();
        let raw_above = DriftingAudioClock::new(MAX_SANE_INSTANTANEOUS_PPM + 1.0).raw_advance(1.0);
        let _ = above.compensate(raw_above, 1.0);
        assert_eq!(
            above.starved_block_count(),
            1,
            "expected a block just ABOVE MAX_SANE_INSTANTANEOUS_PPM to be rejected as starved"
        );
    }

    // ---- #962: per-block instantaneous ppm is unmeasurable noise for small, bursty-delivery
    // blocks (the live mbc incident: 128-sample Dante VSC blocks, 100% starved-rejected under the
    // pre-#962 per-block guard). These are RED against the CURRENT (per-block) estimator -- see
    // the module's #962 design comment (`gh issue view 962 --comments`) for the full mechanism. --

    /// Feed `n_pairs` PAIRS of tiny (128-sample @ 48kHz, the live mbc Dante-VSC block size) blocks
    /// into `compensator`, each pair sharing a fixed total wall-clock duration corresponding to
    /// `true_ppm` but split UNEVENLY (10%/90%) between the two blocks -- reproducing REAL bursty
    /// delivery (some blocks arrive almost back-to-back, the next "catches up"), while the pair's
    /// AGGREGATE wall time still correctly totals what `true_ppm` implies. `raw_advance_s` is the
    /// FIXED per-block sample-count-stamped duration for every block (OBS stamps sample count 1:1
    /// -- drift never shows up per block, only in how much real wall time it took to deliver a
    /// fixed sample count); the injected ppm and the burst/catch-up jitter both live entirely in
    /// `master_block_s`. This is exactly the #962 live mechanism: dividing two small, individually
    /// jittery numbers (one block's own raw_advance_s / master_block_s) amplifies the jitter into
    /// an implausible instantaneous ppm, even though the AGGREGATE (summed) ratio over many blocks
    /// correctly reflects the true, small underlying drift.
    fn feed_bursty_small_blocks(
        compensator: &mut RealtimeAsrcCompensator,
        true_ppm: f64,
        n_pairs: u32,
    ) {
        const NOMINAL_BLOCK_S: f64 = 128.0 / 48_000.0; // #962: the live mbc Dante-VSC block size
        const SMALL_SPLIT: f64 = 0.1; // 10%/90% burst/catch-up wall-clock split
        let true_ratio = 1.0 + true_ppm / 1_000_000.0;
        let pair_master_s = 2.0 * NOMINAL_BLOCK_S / true_ratio;
        for _ in 0..n_pairs {
            let _ = compensator.compensate(NOMINAL_BLOCK_S, pair_master_s * SMALL_SPLIT);
            let _ = compensator.compensate(NOMINAL_BLOCK_S, pair_master_s * (1.0 - SMALL_SPLIT));
        }
    }

    /// THE gate for issue #962 itself: a small ("a few ppm") TRUE drift, delivered as tiny bursty
    /// blocks matching the live mbc incident exactly (128 samples @ 48kHz, uneven wall-clock
    /// delivery timing per block), must be MEASURED -- not just safely ignored -- by the
    /// estimator. Runs long enough (~2min) for convergence per the SAME bound
    /// `estimator_converges_within_the_tickets_own_bounds` already established. RED against the
    /// current per-block estimator: every individual block's own instantaneous ppm (~+-4,000,000
    /// from the 10%/90% split alone, regardless of the injected true ppm) wildly clears the #960
    /// ceiling, so the current code rejects essentially every block and never measures the true
    /// drift at all -- exactly the live mbc defect (`starved_blocks=22500/60s` = 100%).
    #[test]
    fn windowed_estimator_measures_a_small_drift_from_tiny_bursty_blocks_962() {
        const TRUE_PPM: f64 = 5.0; // "a few ppm" per issue #962's own framing
        let mut compensator = RealtimeAsrcCompensator::new();
        // ~2 minutes of pairs (pair wall time ~= 2*128/48000 =~ 5.333ms at this small ppm) --
        // trivial deterministic loop, no real sleep.
        let pair_wall_s = 2.0 * (128.0 / 48_000.0) / (1.0 + TRUE_PPM / 1_000_000.0);
        let n_pairs = (120.0 / pair_wall_s).ceil() as u32;
        feed_bursty_small_blocks(&mut compensator, TRUE_PPM, n_pairs);

        assert_eq!(
            compensator.starved_block_count(),
            0,
            "expected zero blocks flagged as starved -- the AGGREGATE ppm from real (if unevenly \
             delivered) audio is nowhere near the #960 sanity ceiling, only each individual \
             block's own instantaneous ratio is, got {} starved blocks out of {} fed",
            compensator.starved_block_count(),
            n_pairs * 2
        );
        let err = (compensator.estimated_ppm() - TRUE_PPM).abs();
        assert!(
            err < 1.0,
            "expected the estimator to measure the true {TRUE_PPM}ppm drift from tiny bursty \
             (mbc-sized) blocks within 1ppm after ~2min, got estimated_ppm={} (err={err}ppm)",
            compensator.estimated_ppm()
        );
    }

    /// The #960 sanity ceiling must still catch a GENUINELY starved source (not just jittery
    /// delivery of otherwise-real samples) even when it arrives as tiny blocks -- the exact live
    /// #960 incident (a source delivering ~26.24% of the samples its elapsed wall-clock window
    /// implies, i.e. instantaneous ppm ~=-737,600) must still be rejected, whether the source
    /// uses large paced blocks (already proven by the three #960 tests above) or tiny bursty
    /// ones. RED against the current per-block estimator for the same reason as the test above:
    /// the healthy baseline phase alone already gets ~100% rejected by 10%/90%-split jitter, so
    /// `starved_block_count()` is nonzero before the deliberately-starved phase even starts.
    #[test]
    fn windowed_estimator_still_rejects_a_genuinely_starved_tiny_block_source_962() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // Converge on a healthy small-block source first (reuses the #962 fixture at 0 true ppm).
        // ~400 pairs * ~5.33ms/pair =~ 2.13s -- below REGRESSION_LOCK_SPAN_S (60s); this phase only
        // needs to establish "no spurious starvation on healthy small blocks", not reach lock.
        feed_bursty_small_blocks(&mut compensator, 0.0, 400);
        assert_eq!(
            compensator.starved_block_count(),
            0,
            "expected a healthy (if unevenly-delivered) tiny-block source to never be flagged as \
             starved, got {} starved blocks",
            compensator.starved_block_count()
        );

        // Now genuinely starved: #960's own -737,600ppm case, still as tiny blocks -- this must
        // be rejected, and the (converged, ~0) estimate must not move.
        let before = compensator.estimated_ppm();
        feed_bursty_small_blocks(&mut compensator, -737_600.0, 400);
        assert_eq!(
            compensator.estimated_ppm(),
            before,
            "expected a genuinely starved tiny-block source to be rejected, leaving the estimate \
             exactly where it was ({before}), got {}",
            compensator.estimated_ppm()
        );
        assert!(
            compensator.starved_block_count() > 0,
            "expected the genuinely starved window(s) to be counted as starved"
        );
    }

    /// #962 review finding: a REJECTED window must HOLD applied_ppm bit-exact -- including when
    /// the servo is still mid-SLEW toward an already-decided (legitimate, pre-starvation) target,
    /// not just when it has already converged (every other test above only exercises the case
    /// where applied_ppm is ALREADY at target when starvation hits, which masks this). Mirrors
    /// the pre-#962 per-block early-return exactly, now at window granularity: a starved
    /// measurement must not even continue advancing an already-approved slew transition.
    #[test]
    fn rejected_window_holds_applied_ppm_even_mid_slew_962() {
        let mut compensator = RealtimeAsrcCompensator::new();
        // 70 x 1.0s blocks of an extreme (10,000ppm) clock -- past the regression lock
        // (REGRESSION_LOCK_SPAN_S = 60s span, >= 30 points) with a slope estimate far past MAX_PPM,
        // so the target clamps to 300 -- but the slew limiter (5ppm/s) has only had ~10 locked
        // blocks to move applied a few tens of ppm, nowhere near the 300 target. Genuinely mid-slew.
        let extreme_clock = DriftingAudioClock::new(10_000.0);
        for _ in 0..70 {
            let raw = extreme_clock.raw_advance(1.0);
            let _ = compensator.compensate(raw, 1.0);
        }
        let applied_before = compensator.applied_ppm();
        assert!(
            applied_before > 0.0 && applied_before < 300.0,
            "expected applied_ppm to be genuinely mid-slew (0 < applied < 300 target) after 70 \
             locked calls, got {applied_before} -- test setup assumption broken"
        );

        // ONE starved window (the #960 live incident value) -- must be REJECTED and must NOT let
        // the slew step continue advancing toward the (unaffected) target.
        let starved = DriftingAudioClock::new(-737_600.0);
        let raw = starved.raw_advance(1.0);
        let _ = compensator.compensate(raw, 1.0);

        assert_eq!(
            compensator.applied_ppm(),
            applied_before,
            "expected a REJECTED window to HOLD applied_ppm exactly, even mid-slew toward an \
             already-decided target -- got applied_ppm={} (was {applied_before}), meaning the \
             starved window was allowed to continue advancing an in-progress slew transition",
            compensator.applied_ppm()
        );
    }

    /// issue #1335: the buffer-LEVEL holding integral must keep the mix buffer FLAT despite a
    /// RESIDUAL the rate loop structurally cannot remove. Models the live root cause (issue body):
    /// on `mbc` the rate servo settled ~0.8 ppm short of the true source-vs-mixer mismatch, so
    /// WITHOUT a level term the buffer drained ~3 ms/h (105 -> 68 ms over 12.5 h) toward an eventual
    /// underrun. Here `HIDDEN_PPM` is a fill/drain the RATE regression cannot see (it measures
    /// raw-vs-master only); only the LEVEL integral (which reads `buffered_ms`) can null it.
    ///
    /// An I-only controller on the integrator plant that is the buffer is marginally-stable: it holds
    /// the level BOUNDED (period ~3.9 h, amplitude ~2.24*HIDDEN_PPM ms for the lock-time step), not
    /// critically damped -- the design's stated intent ("pomala slucka ... drzi +-5 ms"). So the
    /// GREEN assertions are: over the settled SECOND HALF the buffer MEAN returns to the captured
    /// setpoint (+-2 ms) and the PEAK stays inside +-5 ms; the integral moved NEGATIVE to counter the
    /// drain without railing at its clamp. The rate-only path (the trait `compensate`, no level
    /// integral) drains ~34 ms and FAILS -- the in-test anti-tautology proves the integral does the
    /// work.
    #[test]
    fn realtime_compensator_holds_buffer_level_with_integral_1335() {
        const TRUE_PPM: f64 = -5.0; // healthy mbc source-vs-mixer floor (post-#1325)
        const HIDDEN_PPM: f64 = 0.8; // the residual the rate loop mis-reads (issue body ~0.8 ppm)
        const BLOCK_S: f64 = 1.0;
        const SIM_S: f64 = 12.0 * 3600.0;
        const START_BUF_MS: f64 = 100.0;

        // Run the closed-loop buffer simulation; returns (trace of (t_s, buffer_ms), final servo).
        // `use_level` selects the #1335 level path vs the rate-only trait entry.
        fn simulate(
            use_level: bool,
            true_ppm: f64,
            hidden_ppm: f64,
            sim_s: f64,
            block_s: f64,
            start_ms: f64,
        ) -> (Vec<(f64, f64)>, RealtimeAsrcCompensator) {
            let mut c = RealtimeAsrcCompensator::new();
            let clock = DriftingAudioClock::new(true_ppm);
            let mut buffer_ms = start_ms;
            let mut trace = Vec::new();
            let mut t = 0.0;
            while t < sim_s {
                let raw = clock.raw_advance(block_s);
                let corrected = if use_level {
                    c.compensate_with_level(raw, block_s, buffer_ms)
                } else {
                    c.compensate(raw, block_s)
                };
                // Physical buffer: fills by the corrected OUTPUT-seconds the resampler produces,
                // drains by the mixer's master block, MINUS a hidden residual the rate loop cannot
                // measure (models the live ~0.8 ppm mis-read the level integral must null). Modelling
                // it as a hidden DRAIN the rate loop can't see is buffer-level-equivalent to the
                // production mode (the regression settling ~0.8 ppm short of the true rate): both
                // leave the SAME uncorrected ppm error integrating into the buffer, which is the only
                // thing that reaches this simulation.
                buffer_ms += (corrected - block_s) * 1000.0 - (hidden_ppm / 1e6) * block_s * 1000.0;
                t += block_s;
                trace.push((t, buffer_ms));
            }
            (trace, c)
        }

        let (trace, c) = simulate(true, TRUE_PPM, HIDDEN_PPM, SIM_S, BLOCK_S, START_BUF_MS);

        // The servo captured its setpoint at the lock instant (~65 s in), a hair below START (the
        // pre-lock drain), and tracked the live depth in level_last_ms.
        let setpoint = c.level_target_ms();
        assert!(
            (setpoint - START_BUF_MS).abs() < 2.0,
            "issue #1335: the level setpoint must be captured near the depth at lock (~{START_BUF_MS} ms), got {setpoint:.3} ms"
        );
        assert!(
            (c.level_last_ms() - trace.last().unwrap().1).abs() < 1.5,
            "issue #1335: level_last_ms must track the live buffer depth, got {:.3} vs {:.3}",
            c.level_last_ms(),
            trace.last().unwrap().1
        );

        // Second half = well past the ~1 h settle + a couple oscillation periods.
        let half = SIM_S / 2.0;
        let second: Vec<f64> = trace
            .iter()
            .filter(|(t, _)| *t >= half)
            .map(|(_, b)| *b)
            .collect();
        assert!(!second.is_empty());
        let mean: f64 = second.iter().sum::<f64>() / second.len() as f64;
        let peak = second
            .iter()
            .fold(0.0_f64, |m, b| m.max((b - setpoint).abs()));

        assert!(
            (mean - setpoint).abs() < 2.0,
            "issue #1335: with the level integral the buffer MEAN over the settled second half must \
             return to the setpoint within +-2 ms, got mean={mean:.3} ms (setpoint {setpoint:.3} ms)"
        );
        assert!(
            peak < 5.0,
            "issue #1335: with the level integral the buffer PEAK deviation over the settled second \
             half must stay inside the +-5 ms hold band, got peak={peak:.3} ms"
        );
        // The integral settled into its NEGATIVE equilibrium band (to counter the drain, deficit ->
        // stretch) and never railed at its ±clamp. Phase-robust bound: the equilibrium is
        // -HIDDEN_PPM and the undamped transient swings ±HIDDEN_PPM about it, so the value lives in
        // [-2*HIDDEN_PPM, 0] regardless of where SIM_S lands in the ~3.9 h cycle -- assert that band
        // (with margin), never the last sample against a hand-tuned 0.2.
        assert!(
            c.level_integral_ppm() <= 0.1
                && c.level_integral_ppm() > -(2.0 * HIDDEN_PPM + 0.5)
                && c.level_integral_ppm().abs() < LEVEL_INTEGRAL_MAX_PPM,
            "issue #1335: the level integral must sit in its negative equilibrium band \
             (~[-2*{HIDDEN_PPM}, 0] ppm) without railing at ±{LEVEL_INTEGRAL_MAX_PPM} ppm, got \
             integral={:.4} ppm",
            c.level_integral_ppm()
        );

        // Anti-tautology: the SAME residual with the rate-only path (no level integral) must DRIFT
        // far out of band -- proving the integral, not the rate loop, is what holds the level.
        let (rate_only, _) = simulate(false, TRUE_PPM, HIDDEN_PPM, SIM_S, BLOCK_S, START_BUF_MS);
        let end_drift = (rate_only.last().unwrap().1 - START_BUF_MS).abs();
        assert!(
            end_drift > 10.0,
            "issue #1335: the rate-only path must drift far out of band with the hidden residual \
             (proving this test CAN fail), got end drift={end_drift:.3} ms -- too small to \
             discriminate; re-check HIDDEN_PPM / SIM_S"
        );
    }

    /// issue #1335 follow-up: a DELIBERATE audio sync-offset change shifts the source's audio
    /// placement (obs-source.c `in.timestamp += sync_offset`, applied BEFORE placement) and
    /// therefore its mix-buffer depth by the SAME Δ. `shift_level_target(Δ)` must move the captured
    /// LEVEL setpoint by Δ so the holding integral keeps the NEW depth instead of refilling toward
    /// the old one and silently cancelling the deliberate audio trim (issue 1333) — the exact
    /// servo-vs-actuator fight the owner reported (A/V floats by the trim size between E2E runs).
    ///
    /// Bench: lock + settle at a depth L with NO hidden residual (so the buffer holds perfectly flat
    /// once locked), then model the placement change as an instantaneous −14 ms WITHDRAWAL from the
    /// buffer (the live offset −4 → −18 ms), then observe 2 h.
    ///   GREEN (shift announced): the setpoint moves to L−14, so the buffer settles at L−14 and the
    ///   level integral stays within ±0.05 ppm of its pre-jump value — it never has to fight the trim.
    ///   RED (no shift, in-test anti-tautology): the setpoint stays L, so the integral WINDS (≥0.3 ppm)
    ///   and drives the buffer back toward L, cancelling the trim — proving the shift does the work.
    #[test]
    fn shift_level_target_holds_setpoint_after_offset_jump_1335() {
        const TRUE_PPM: f64 = -5.0; // healthy mbc floor; no hidden residual → buffer holds flat once locked
        const BLOCK_S: f64 = 1.0; // one 1 s accepted window per block (matches the #1335 level test)
        const WARMUP_S: f64 = 2400.0; // well past lock (~65 s) + settle; the integral parks near 0
        const POST_S: f64 = 2.0 * 3600.0; // the design's 2 h observation window
        const START_BUF_MS: f64 = 100.0;
        const JUMP_MS: f64 = -14.0; // the live offset trim, modelled as a placement withdrawal

        // Closed-loop buffer sim with a one-shot placement jump at the end of warmup. `use_shift`
        // selects whether the offset change is ANNOUNCED to the servo via shift_level_target.
        // Returns (post-jump trace of (buffer_ms, integral_ppm), pre_jump_integral, final servo).
        fn simulate(use_shift: bool) -> (Vec<(f64, f64)>, f64, RealtimeAsrcCompensator) {
            let mut c = RealtimeAsrcCompensator::new();
            let clock = DriftingAudioClock::new(TRUE_PPM);
            let mut buffer_ms = START_BUF_MS;
            // Warmup: lock the rate loop and let the level integral park at ~0 (no hidden residual).
            let mut t = 0.0;
            while t < WARMUP_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                t += BLOCK_S;
            }
            let pre_jump_integral = c.level_integral_ppm();
            // The deliberate offset change: an instantaneous placement withdrawal from the buffer,
            // then (optionally) announce the SAME Δ to the servo. A deliberate change never flushes
            // the regression (the rate inputs are untouched), so level_captured stays true.
            buffer_ms += JUMP_MS;
            if use_shift {
                c.shift_level_target(JUMP_MS);
            }
            // Observe 2 h.
            let mut post = Vec::new();
            t = 0.0;
            while t < POST_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                t += BLOCK_S;
                post.push((buffer_ms, c.level_integral_ppm()));
            }
            (post, pre_jump_integral, c)
        }

        // GREEN: with the shift, the setpoint moved by Δ, so the buffer holds the NEW depth and the
        // integral never winds.
        let (post, pre_jump_integral, c) = simulate(true);
        let target_after = c.level_target_ms();
        assert!(
            (target_after - (START_BUF_MS + JUMP_MS)).abs() < 2.0,
            "issue #1335 follow-up: shift_level_target must move the setpoint by Δ to ~{:.1} ms, \
             got {:.3} ms",
            START_BUF_MS + JUMP_MS,
            target_after
        );
        let mean: f64 = post.iter().map(|(b, _)| *b).sum::<f64>() / post.len() as f64;
        let peak = post
            .iter()
            .fold(0.0_f64, |m, (b, _)| m.max((b - target_after).abs()));
        assert!(
            (mean - target_after).abs() < 2.0 && peak < 5.0,
            "issue #1335 follow-up: with the shift the buffer must settle at the NEW setpoint L−14 \
             (mean within ±2 ms, peak inside ±5 ms), got mean={mean:.3} peak={peak:.3} \
             (setpoint {target_after:.3})"
        );
        let integral_drift = post
            .iter()
            .fold(0.0_f64, |m, (_, i)| m.max((i - pre_jump_integral).abs()));
        assert!(
            integral_drift < 0.05,
            "issue #1335 follow-up: with the shift the level integral must stay within ±0.05 ppm of \
             its pre-jump value over the 2 h (it never fights the trim), got max drift={integral_drift:.4} ppm"
        );

        // RED (anti-tautology): WITHOUT the shift the servo fights the trim — the integral winds and
        // the buffer is dragged back toward the ORIGINAL setpoint L, silently undoing the offset.
        let (post_red, pre_red, c_red) = simulate(false);
        let target_orig = c_red.level_target_ms(); // unchanged — never shifted
        let red_integral_wind = post_red
            .iter()
            .fold(0.0_f64, |m, (_, i)| m.max((i - pre_red).abs()));
        assert!(
            red_integral_wind >= 0.3,
            "issue #1335 follow-up: without the shift the level integral must WIND ≥0.3 ppm to \
             refill the buffer (proving the fix, not the plant, holds the level), got max wind={red_integral_wind:.4} ppm"
        );
        let red_return_to_l = post_red
            .iter()
            .fold(f64::INFINITY, |m, (b, _)| m.min((b - target_orig).abs()));
        assert!(
            red_return_to_l < 2.0,
            "issue #1335 follow-up: without the shift the buffer must climb back to the ORIGINAL \
             setpoint L (the cancelled trim), got closest approach={red_return_to_l:.3} ms to {target_orig:.3}"
        );
    }

    /// issue #1335 follow-up 3: a DELIBERATE setpoint shift of at least the restore's exit band must
    /// ARM the fast bounded level restore, so the level reaches the new depth in minutes with the
    /// integral frozen (follow-up 2) — NOT the ~1 h at the ±3 ppm rail the plain I term needs (the
    /// live 18.9. 12 h series: a +12 ms sync-offset trim railed the integral for ~1 h and rang for
    /// hours). A sub-band shift arms nothing (the gentle I+P loop absorbs it). RED before this fix:
    /// `shift_level_target` moved the setpoint but never armed the restore, so `level_restore()` was
    /// false, the buffer never reached ±5 ms of the new target inside the window, and the integral
    /// railed at ±LEVEL_INTEGRAL_MAX_PPM.
    #[test]
    fn shift_level_target_arms_fast_restore_on_deliberate_shift_1335() {
        const TRUE_PPM: f64 = -5.0; // healthy mbc floor; buffer holds flat once locked
        const BLOCK_S: f64 = 1.0; // one accepted 1 s window per block
        const WARMUP_S: f64 = 2400.0; // past lock (~65 s); the integral parks near 0
        const START_BUF_MS: f64 = 100.0;
        const POST_S: f64 = 900.0; // > the Kr=2 restore settle for a 12 ms move (~433 s, below)

        // Closed-loop buffer sim: warm to lock, then apply a DELIBERATE setpoint shift of `shift_ms`
        // (announce Δ to the servo; the ALREADY-buffered samples do NOT jump — obs applies the offset
        // to FUTURE placement — so the level error is now Δ and the only fast actuator is the restore
        // this follow-up arms). Returns (armed_right_after_shift, restore_active_at_end,
        // target_before, target_after, post-shift trace of (buffer_ms, integral_ppm)).
        fn run(shift_ms: f64) -> (bool, bool, f64, f64, Vec<(f64, f64)>) {
            let mut c = RealtimeAsrcCompensator::new();
            let clock = DriftingAudioClock::new(TRUE_PPM);
            let mut buffer_ms = START_BUF_MS;
            let mut t = 0.0;
            while t < WARMUP_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                t += BLOCK_S;
            }
            let target_before = c.level_target_ms();
            c.shift_level_target(shift_ms);
            let armed = c.level_restore();
            let target_after = c.level_target_ms();
            let mut post = Vec::new();
            t = 0.0;
            while t < POST_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                t += BLOCK_S;
                post.push((buffer_ms, c.level_integral_ppm()));
            }
            (armed, c.level_restore(), target_before, target_after, post)
        }

        // (a) a +12 ms shift (the live 18.9. trim) arms the restore, which drives the level to within
        // 5 ms of the NEW setpoint fast and WITHOUT railing the integral, then EXITS.
        let (armed, active_at_end, tb, ta, post) = run(12.0);
        assert!(
            (ta - (tb + 12.0)).abs() < 1e-9,
            "issue #1335 follow-up 3: the shift must move the setpoint by exactly Δ (from {tb:.3} to \
             {:.3} ms), got {ta:.3}",
            tb + 12.0
        );
        assert!(
            armed,
            "issue #1335 follow-up 3: a +12 ms deliberate shift (≥ the 5 ms arm band) must ARM the \
             fast level restore (level_restore() == true right after the shift), got false — the \
             plain ±3 ppm I term would take ~1 h and rail"
        );
        // settle time: first window whose buffer is within 5 ms of the new target.
        let settle_s = post
            .iter()
            .position(|(b, _)| (b - ta).abs() < 5.0)
            .map(|i| (i + 1) as f64)
            .unwrap_or(-1.0);
        // The DESIGN quoted ~2 min for a 12 ms move; that assumes the ±100 ppm restore CLAMP binds
        // (i.e. Kr ~ 20). At the shipped Kr=2 the restore is Kr·err (24 ppm at 12 ms, never near the
        // clamp), so it decays with a ~500 s time constant — 12 ms → ±5 ms in ~433 s (the same
        // clamp-does-not-bind calibration the follow-up-2 step test documents for its 50 ms case).
        // Well under an hour and monotonic, so consecutive E2E offset applies settle between runs.
        assert!(
            settle_s > 0.0 && settle_s <= 600.0,
            "issue #1335 follow-up 3: the armed restore must bring the level within 5 ms of the new \
             target in ≤ 600 s (measured ~433 s at Kr=2), got settle_s={settle_s:.0}"
        );
        let integral_peak = post.iter().fold(0.0_f64, |m, (_, i)| m.max(i.abs()));
        assert!(
            integral_peak < LEVEL_INTEGRAL_MAX_PPM - 0.5,
            "issue #1335 follow-up 3: with the restore doing the work the level integral must NEVER \
             rail at ±{LEVEL_INTEGRAL_MAX_PPM} ppm (RED: it railed at 3.0), got peak {integral_peak:.3} ppm"
        );
        let final_buf = post.last().unwrap().0;
        assert!(
            (final_buf - ta).abs() < 5.5,
            "issue #1335 follow-up 3: the level must stay converged at the new setpoint {ta:.3} ms \
             (within ±5.5 ms), got {final_buf:.3}"
        );
        assert!(
            !active_at_end,
            "issue #1335 follow-up 3: the restore burst must EXIT once the buffer is back within 5 ms \
             (the existing |err| < 5 ms exit is unchanged)"
        );

        // (b) a +3 ms shift is below the 5 ms arm band — it must arm NOTHING; the gentle I+P loop
        // absorbs it as before (no restore burst, no windup).
        let (armed_small, _, tb_s, ta_s, post_small) = run(3.0);
        assert!(
            (ta_s - (tb_s + 3.0)).abs() < 1e-9,
            "issue #1335 follow-up 3: the sub-band shift must still move the setpoint by Δ"
        );
        assert!(
            !armed_small,
            "issue #1335 follow-up 3: a +3 ms shift (< the 5 ms arm band) must NOT arm the restore, got true"
        );
        let small_integral_peak = post_small.iter().fold(0.0_f64, |m, (_, i)| m.max(i.abs()));
        assert!(
            small_integral_peak < LEVEL_INTEGRAL_MAX_PPM,
            "issue #1335 follow-up 3: the sub-band shift must not rail the integral either, got peak {small_integral_peak:.3} ppm"
        );
    }

    /// issue #1335 follow-up 2 (a): a permanent 50 ms INPUT sample-loss step (the live 17.9. 18:52
    /// StartStream stall: mbc buffered_ms 108 → 51, starved_blocks=0) must NOT bias the rate slope —
    /// the servo RE-BASEs the step out of the regression (estimate stays put) AND fast-restores the
    /// lost 50 ms of buffer within ~12 min, then exits the restore. The anti-tautology: the rate-only
    /// path (which inserts the step) swings the slope ≥40 ppm — the −83 ppm class this fix kills.
    #[test]
    fn step_tolerant_rebase_holds_estimate_and_restores_level_1335() {
        const TRUE_PPM: f64 = -5.0; // healthy mbc floor
        const BLOCK_S: f64 = 1.0;
        const START_BUF_MS: f64 = 100.0;
        const WARMUP_S: f64 = 200.0; // past lock (~65 s) + settle
        const STEP_MS: f64 = -50.0; // input sample loss: raw short by 50 ms in one window

        // The restore is PROPORTIONAL (Kr*err, clamped +-100) so it decays with a ~500 s time
        // constant (k*Kr = 2e-3/s) -- it returns a 50 ms step to within +-5 ms in ~19-25 min, NOT
        // the design's stated "+-5 ms inside 12 min" (that needs the +-100 clamp to bind for most of
        // the return, i.e. Kr~20 so it stays near-constant 100 ppm -- see the issue-1335-follow-up-2
        // anchors-confirmed comment; the specified Kr=2 is used as-is here). Observe 30 min so the
        // restore completes; a 12-min checkpoint documents the ~76% progress the design assumed done.
        const POST_S: f64 = 30.0 * 60.0;

        // Closed-loop buffer sim. `use_level` selects the C-equivalent level path (which re-bases)
        // vs the rate-only trait entry (which inserts the step — the anti-tautology). Returns
        // (est_pre, target, final_buffer, max |est − est_pre| over recovery, restore_active, servo).
        fn run(use_level: bool) -> (f64, f64, f64, f64, f64, bool, RealtimeAsrcCompensator) {
            let mut c = RealtimeAsrcCompensator::new();
            let clock = DriftingAudioClock::new(TRUE_PPM);
            let mut buffer_ms = START_BUF_MS;
            let step = |c: &mut RealtimeAsrcCompensator, raw: f64, buf: &mut f64| {
                let corrected = if use_level {
                    c.compensate_with_level(raw, BLOCK_S, *buf)
                } else {
                    c.compensate(raw, BLOCK_S)
                };
                *buf += (corrected - BLOCK_S) * 1000.0;
            };
            let mut t = 0.0;
            while t < WARMUP_S {
                let raw = clock.raw_advance(BLOCK_S);
                step(&mut c, raw, &mut buffer_ms);
                t += BLOCK_S;
            }
            let est_pre = c.estimated_ppm();
            let target = c.level_target_ms();
            // The step: ONE window whose input is short by 50 ms of samples (a permanent loss). Those
            // 50 ms never entered the mix buffer, so buffered_ms is ALREADY 50 ms lower when the ASRC
            // reads it on this window (live 17.9.: buffered_ms 108 → 51). Model it as a direct buffer
            // withdrawal read on the step window, plus the short raw that carries the −50 ms rate
            // residual; the withdrawal replaces the normal fill/drain accounting for the loss window.
            buffer_ms += STEP_MS;
            let raw_step = clock.raw_advance(BLOCK_S) + STEP_MS / 1000.0;
            let _ = if use_level {
                c.compensate_with_level(raw_step, BLOCK_S, buffer_ms)
            } else {
                c.compensate(raw_step, BLOCK_S)
            };
            // Observe recovery, capturing the buffer at the design's 12-min checkpoint.
            let mut est_dev_max = 0.0_f64;
            let mut buf_at_12min = buffer_ms;
            t = 0.0;
            while t < POST_S {
                let raw = clock.raw_advance(BLOCK_S);
                step(&mut c, raw, &mut buffer_ms);
                est_dev_max = est_dev_max.max((c.estimated_ppm() - est_pre).abs());
                t += BLOCK_S;
                if (t - 12.0 * 60.0).abs() < BLOCK_S / 2.0 {
                    buf_at_12min = buffer_ms;
                }
            }
            let restore_active = c.level_restore();
            (
                est_pre,
                target,
                buffer_ms,
                buf_at_12min,
                est_dev_max,
                restore_active,
                c,
            )
        }

        let (est_pre, target, final_buf, buf_12, est_dev_max, restore_active, c) = run(true);
        // 1) The step was detected and re-based (not inserted), and recorded in telemetry.
        assert!(
            c.step_count() >= 1,
            "issue #1335 f2: the 50 ms input-loss step must re-base (step_count>=1), got {}",
            c.step_count()
        );
        assert!(
            (c.last_step_ms() - STEP_MS).abs() < 5.0,
            "issue #1335 f2: last_step_ms must record the ~{STEP_MS} ms residual, got {:.2}",
            c.last_step_ms()
        );
        // 2) The estimate stays put — the step never entered the slope (RED: swings tens of ppm).
        assert!(
            est_dev_max < 2.0,
            "issue #1335 f2: with re-base the estimate must stay within +-2 ppm of pre-step \
             ({est_pre:.2}), got max dev {est_dev_max:.3} ppm"
        );
        // 3a) By the design's 12-min checkpoint the restore has recovered the bulk of the 50 ms loss
        //     (data: ~-12 ms of the -50 ms remains, ~76% back) — proving the fast restore is doing
        //     real work early, even though at Kr=2 it has not yet reached the design's stated ±5 ms.
        assert!(
            (buf_12 - target).abs() < 0.5 * STEP_MS.abs(),
            "issue #1335 f2: by 12 min the restore must have recovered >half the loss (within \
             {:.0} ms of target {target:.1}), got {buf_12:.2}",
            0.5 * STEP_MS.abs()
        );
        // 3b) The restore returns the level within +-5.5 ms of target and EXITS (data: ~19-25 min at
        //     Kr=2; the last few ms are then closed by the slow integral over hours).
        assert!(
            (final_buf - target).abs() < 5.5,
            "issue #1335 f2: the fast restore must return the buffer within ~+-5 ms of target \
             ({target:.1}), got {final_buf:.2}"
        );
        assert!(
            !restore_active,
            "issue #1335 f2: the restore burst must have EXITED once the buffer was back within 5 ms"
        );
        // 4) Anti-tautology: the rate-only path (no re-base) lets the step corrupt the slope.
        let (rate_est_pre, _, _, _, rate_dev_max, _, _) = run(false);
        assert!(
            rate_dev_max >= 40.0,
            "issue #1335 f2: the rate-only path must swing the estimate >=40 ppm (proving re-base, \
             not the plant, holds it), got max dev {rate_dev_max:.2} ppm from {rate_est_pre:.2}"
        );
    }

    /// issue #1335 follow-up 2 (b): a +50 ms WALL-CLOCK jump (master advanced, input samples normal,
    /// buffer level UNCHANGED) must re-base the slope out but must NOT engage the fast level restore
    /// (there was no sample loss to refill) — the "wall-clock skok ⇒ re-base only" case.
    #[test]
    fn wall_clock_jump_rebases_without_level_restore_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const TARGET_MS: f64 = 100.0;
        const WARMUP_S: f64 = 200.0;
        const JUMP_MS: f64 = 50.0; // master reads +50 ms longer for one window

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        // Warmup with the buffer held exactly at target (no level error) so the setpoint captures at
        // TARGET_MS and the integral/P stay at 0 — isolating the rate-side re-base.
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let _ = c.compensate_with_level(raw, BLOCK_S, TARGET_MS);
            t += BLOCK_S;
        }
        let est_pre = c.estimated_ppm();
        let steps_pre = c.step_count();
        // The wall-clock jump: master_block is +50 ms longer, but the input delivered a normal ~1 s
        // of samples and the buffer level is unchanged.
        let raw = clock.raw_advance(BLOCK_S);
        let _ = c.compensate_with_level(raw, BLOCK_S + JUMP_MS / 1000.0, TARGET_MS);

        assert_eq!(
            c.step_count(),
            steps_pre + 1,
            "issue #1335 f2: a +50 ms wall-clock jump must re-base (one step), got step_count {} (was {})",
            c.step_count(),
            steps_pre
        );
        assert!(
            !c.level_restore(),
            "issue #1335 f2: a wall-clock-only jump (buffer unchanged) must NOT engage the fast \
             level restore"
        );
        assert!(
            (c.estimated_ppm() - est_pre).abs() < 2.0,
            "issue #1335 f2: the re-base must keep the estimate within +-2 ppm of pre-jump \
             ({est_pre:.2}), got {:.2}",
            c.estimated_ppm()
        );
        // The residual telemetry records the ~−50 ms step (master went up ⇒ raw−master dropped).
        assert!(
            (c.last_step_ms() + JUMP_MS).abs() < 5.0,
            "issue #1335 f2: last_step_ms must record the ~-{JUMP_MS} ms wall-jump residual, got {:.2}",
            c.last_step_ms()
        );
    }

    /// issue #1335 follow-up 2 (c): ordinary bounded residual noise (±3 ms per 1 s window — the ASIO
    /// callback jitter floor) must produce ZERO re-bases over 2 h; the 10 ms threshold sits well
    /// above the noise. Discriminator: a single 15 ms outlier DOES re-base (the detector is live).
    #[test]
    fn residual_noise_never_false_rebases_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const START_BUF_MS: f64 = 100.0;
        const WARMUP_S: f64 = 200.0;
        const NOISE_MS: f64 = 3.0; // hard bound on the per-window residual noise
        const OBSERVE_S: f64 = 2.0 * 3600.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        // Deterministic bounded noise in [-NOISE_MS, +NOISE_MS] ms (a splitmix-style LCG, no crate).
        let mut seed: u64 = 0x0BAD_F00D_1335;
        let mut noise_s = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let u = (seed >> 33) as f64 / (1u64 << 31) as f64; // [0,1)
            (u * 2.0 - 1.0) * NOISE_MS / 1000.0 // [-NOISE_MS, +NOISE_MS] ms in seconds
        };
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let steps_pre = c.step_count();
        t = 0.0;
        while t < OBSERVE_S {
            let raw = clock.raw_advance(BLOCK_S) + noise_s();
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        assert_eq!(
            c.step_count(),
            steps_pre,
            "issue #1335 f2: +-{NOISE_MS} ms residual noise must NOT re-base over 2 h, got {} spurious steps",
            c.step_count() - steps_pre
        );
        // Discriminator: a genuine 15 ms residual step DOES re-base (the detector is not dead).
        let raw_outlier = clock.raw_advance(BLOCK_S) + 15.0 / 1000.0;
        let _ = c.compensate_with_level(raw_outlier, BLOCK_S, buffer_ms);
        assert_eq!(
            c.step_count(),
            steps_pre + 1,
            "issue #1335 f2: a 15 ms residual outlier MUST re-base (proving the detector discriminates)"
        );
    }

    /// issue #1335 follow-up 2 (d): the P term DAMPS the I-only level loop's marginal oscillation. A
    /// pure LEVEL disturbance (no rate step ⇒ no re-base, no restore) in the loop's LINEAR regime (a
    /// 5 ms bump — small enough that the ±3 ppm integral never rails, so the damping is not masked by
    /// the clamp's own bang-bang) must (i) keep the integral off its ±3 rail and (ii) DECAY: the
    /// 2nd-half peak deviation is measurably smaller than the 1st. Measured discriminator (4 h,
    /// deterministic): correct P ⇒ ratio 0.89; I-only (the RED, before the P term) ⇒ ratio 1.00
    /// (undamped, constant amplitude); WRONG-sign P ⇒ ratio 1.11 (amplifies). With Kp=0.03 the
    /// damping is gentle (ζ≈0.034), so at the design's 20 ms disturbance the ±3 clamp bang-bangs and
    /// masks the P entirely (P and I-only both ratio ~0.32) — this is NOT the design's stated
    /// `<3 ms overshoot for a 20 ms disturbance` (that needs Kp≈0.6 + a wider clamp; see the
    /// issue-1335-follow-up-2 anchors-confirmed comment). This test proves the P has the CORRECT
    /// (damping) sign and reduces the oscillation, which is its load-bearing purpose.
    #[test]
    fn p_term_damps_the_level_loop_oscillation_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const START_BUF_MS: f64 = 100.0;
        const WARMUP_S: f64 = 300.0;
        const DISTURB_MS: f64 = 5.0; // linear regime — the integral stays off its ±3 rail
        const OBSERVE_S: f64 = 4.0 * 3600.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let target = c.level_target_ms();
        let steps_pre = c.step_count();
        // A pure LEVEL disturbance (raw/master untouched ⇒ no rate residual ⇒ no re-base): the
        // buffer jumps by DISTURB_MS (+5 ms) and only the I+P level loop responds.
        buffer_ms += DISTURB_MS;
        let mut trace: Vec<f64> = Vec::new();
        let mut integral_peak = 0.0_f64;
        t = 0.0;
        while t < OBSERVE_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            trace.push(buffer_ms - target);
            integral_peak = integral_peak.max(c.level_integral_ppm().abs());
            t += BLOCK_S;
        }
        assert_eq!(
            c.step_count(),
            steps_pre,
            "issue #1335 f2: a pure LEVEL disturbance must NOT trigger a rate re-base"
        );
        assert!(
            !c.level_restore(),
            "issue #1335 f2: a pure LEVEL disturbance must NOT engage the fast restore (that is \
             step-corroborated only)"
        );
        assert!(
            integral_peak < LEVEL_INTEGRAL_MAX_PPM - 0.1,
            "issue #1335 f2: in the linear regime the level integral must stay off its \
             +-{LEVEL_INTEGRAL_MAX_PPM} ppm rail, got peak {integral_peak:.3} ppm"
        );
        let half = trace.len() / 2;
        let peak1 = trace[..half].iter().fold(0.0_f64, |m, d| m.max(d.abs()));
        let peak2 = trace[half..].iter().fold(0.0_f64, |m, d| m.max(d.abs()));
        // The P term (correct sign) DECAYS the oscillation (measured ratio ~0.89); the undamped
        // I-only RED holds it (~1.00) and a wrong-sign P GROWS it (~1.11) — 0.95 sits cleanly between.
        assert!(
            peak2 < 0.95 * peak1,
            "issue #1335 f2: the P term must DECAY the level oscillation (2nd-half peak < 0.95x \
             1st-half; RED I-only holds ~1.0, wrong-sign grows), got peak1={peak1:.2} peak2={peak2:.2} ms"
        );
    }

    /// issue #1335 follow-up 4 (a): a SUSTAINED buffer-level error must ARM the fast bounded level
    /// restore even when NO residual step accompanies it — the case the step arm (follow-up 2) and
    /// the shift arm (follow-up 3) both miss. Live 18.9. 12:00 StartStream: the `mbc` level dropped
    /// 100 → 68 ms with `steps=1 last_step_ms=-14.3` detected BEFORE the level had drained, so the
    /// step corroboration failed and `restore=0`; the level then sat 10–25 ms low for 40 min at the
    /// ±3 ppm I rail and the run measured +15 ms rig-wide. This test drops the level 25 ms with NO
    /// rate residual on a locked compensator: the sustained-error arm must fire within ≤12 accepted
    /// windows (the 10-window band + settle margin), the restore must bring the level back within
    /// 5 ms, and the integral must NEVER reach its ±LEVEL_INTEGRAL_MAX_PPM rail. RED before this fix:
    /// nothing arms the restore from a level error alone, so level_restore() stays false and the ±3
    /// ppm I term alone leaves the level low for ~an hour (the incident).
    #[test]
    fn sustained_level_error_arms_fast_restore_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0; // one accepted 1 s window per block
        const WARMUP_S: f64 = 2400.0; // past lock; the integral parks near 0
        const START_BUF_MS: f64 = 100.0;
        const DROP_MS: f64 = 25.0; // the live StartStream deficit band (14–32 ms), no rate step
        const OBSERVE_S: f64 = 1200.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let target = c.level_target_ms();
        // A PURE level drop: the mix buffer loses 25 ms of depth with the rate (raw vs master)
        // untouched ⇒ no re-base (steps stays put) and no shift ⇒ the ONLY path that can arm the
        // restore is the new sustained-error counter.
        buffer_ms -= DROP_MS;
        let steps_pre = c.step_count();
        let mut arm_window: i64 = -1;
        let mut settle_window: i64 = -1;
        let mut integral_peak = 0.0_f64;
        let mut i: i64 = 0;
        while (i as f64) * BLOCK_S < OBSERVE_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            i += 1;
            if arm_window < 0 && c.level_restore() {
                arm_window = i;
            }
            if settle_window < 0 && (buffer_ms - target).abs() < 5.0 {
                settle_window = i;
            }
            integral_peak = integral_peak.max(c.level_integral_ppm().abs());
        }
        assert_eq!(
            c.step_count(),
            steps_pre,
            "issue #1335 f4: a PURE level drop (no rate step) must NOT re-base — the sustained-error \
             arm, not the step arm, is under test here, got {} spurious steps",
            c.step_count() - steps_pre
        );
        assert!(
            arm_window > 0 && arm_window <= 12,
            "issue #1335 f4: a sustained 25 ms level error must ARM the fast restore within ≤12 \
             accepted windows (10-window band + settle margin), got arm_window={arm_window}"
        );
        // Settle: the DESIGN quoted ≤600 s; at the shipped Kr=2 the restore is Kr·err (24–50 ppm for
        // a 12–25 ms error, below the ±100 clamp), decaying with a ~500 s time constant, so a 25 ms
        // drop reaches ±5 ms in ~804 s (measured) — the SAME clamp-does-not-bind calibration the
        // follow-up-2/3 tests document for their 50/12 ms cases (NOT the design's aspirational ~2
        // min). Well under an hour and monotonic, so consecutive E2E runs stop walking with the
        // buffer level — the ticket's whole point ("minutes instead of hours").
        assert!(
            settle_window > 0 && settle_window <= 900,
            "issue #1335 f4: the armed restore must bring the level within 5 ms of the target in \
             ≤ 900 s (measured ~804 s at Kr=2 for a 25 ms move), got settle_window={settle_window}"
        );
        assert!(
            integral_peak < LEVEL_INTEGRAL_MAX_PPM,
            "issue #1335 f4: the fast restore (not the integral) does the heavy lifting, so the \
             integral must NEVER reach its ±LEVEL_INTEGRAL_MAX_PPM rail (the incident sat railed \
             there for ~an hour), got peak {integral_peak:.3} ppm"
        );
    }

    /// issue #1335 follow-up 4 (b): ±8 ms per-window level scatter around the target (the 1-s window
    /// noise floor, alternating sign) must NEVER arm the sustained-error restore over 600 s — the 12
    /// ms band sits comfortably above the scatter, so no single window reaches it and the consecutive
    /// counter never advances. Guards against lowering the band into the noise (a false arm needs 10
    /// consecutive ≥12 ms magnitude readings (either sign — the arm is on |level − target|), which
    /// ±8 ms scatter cannot produce).
    #[test]
    fn level_scatter_never_arms_fast_restore_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const WARMUP_S: f64 = 2400.0;
        const START_BUF_MS: f64 = 100.0;
        const SCATTER_MS: f64 = 8.0; // the ±8 ms 1-s level scatter floor
        const OBSERVE_S: f64 = 600.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let mut i: i64 = 0;
        while (i as f64) * BLOCK_S < OBSERVE_S {
            // Report a scattered level (±8 ms around the true buffer, alternating sign) to the servo;
            // the true buffer is integrated from the servo's response, so it stays near target.
            let scatter = if i % 2 == 0 { SCATTER_MS } else { -SCATTER_MS };
            let reported = (buffer_ms + scatter).max(0.0);
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, reported);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            i += 1;
            assert!(
                !c.level_restore(),
                "issue #1335 f4: ±8 ms level scatter (below the 12 ms band) must NEVER arm the fast \
                 restore, but it armed at window {i}"
            );
        }
    }

    /// issue #1335 follow-up 4 (d): a SUSTAINED sub-band level offset (6 ms, and 11 ms — just under
    /// the 12 ms band) must NEVER arm the fast restore over 600 s; the gentle I+P loop absorbs it.
    /// Discriminator (the detector is LIVE, not dead or set too high): a 13 ms sustained offset — one
    /// millisecond over the band — DOES arm within the window budget. Together with the ±8 ms scatter
    /// guard (b) and the 25 ms arm (a), this pins the band to 12 ms: (8, 11] never arm, [12, …] arm.
    #[test]
    fn sub_band_level_offset_never_arms_but_band_is_live_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const WARMUP_S: f64 = 2400.0;
        const START_BUF_MS: f64 = 100.0;
        const OBSERVE_S: f64 = 600.0;

        // Warm to lock, drop the level by `drop_ms` with no rate step, run OBSERVE_S; return whether
        // the restore ever armed.
        fn ran_and_armed(drop_ms: f64) -> bool {
            let mut c = RealtimeAsrcCompensator::new();
            let clock = DriftingAudioClock::new(TRUE_PPM);
            let mut buffer_ms = START_BUF_MS;
            let mut t = 0.0;
            while t < WARMUP_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                t += BLOCK_S;
            }
            buffer_ms -= drop_ms;
            let mut armed = false;
            let mut i: i64 = 0;
            while (i as f64) * BLOCK_S < OBSERVE_S {
                let raw = clock.raw_advance(BLOCK_S);
                let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
                buffer_ms += (corrected - BLOCK_S) * 1000.0;
                i += 1;
                if c.level_restore() {
                    armed = true;
                }
            }
            armed
        }

        assert!(
            !ran_and_armed(6.0),
            "issue #1335 f4: a sustained 6 ms level offset (well below the 12 ms band) must NEVER arm \
             the fast restore — the gentle I+P loop absorbs it"
        );
        assert!(
            !ran_and_armed(11.0),
            "issue #1335 f4: a sustained 11 ms level offset (just BELOW the 12 ms band) must NEVER \
             arm the fast restore — the band is 12 ms, not lower"
        );
        assert!(
            ran_and_armed(13.0),
            "issue #1335 f4: a sustained 13 ms level offset (just OVER the 12 ms band) MUST arm the \
             fast restore — proving the detector is live and the band sits at 12 ms"
        );
    }

    /// issue #1335 follow-up 5 (a): the SMOOTHED proportional term is now the NORMAL LAW of the level
    /// loop. A 15 ms level deficit reported with ±10 ms per-window mixer-tick phase noise must be
    /// HELD back to target within minutes — the MEAN of the last 60 windows within ±3 ms of target —
    /// with the integral doing almost none of the work (never near its ±3 rail). RED before follow-up
    /// 5: the old Kp=0.03/±1 raw-error P term (τ ~ hours) leaves the mean 10+ ms low for ~an hour (the
    /// 18.9. live wander of ±10-15 ms; measured on the base code: dev 14.3 ms at 600 s, never within
    /// ±3 ms). NOTE the design's aspirational ≤600 s is NOT met at the shipped Kp=2 (loop time constant
    /// ≈ 1/(Kp·1e-3) = 500 s ⇒ a 15 ms error is ~4.5 ms at 600 s, ~3 ms at ~777 s) — the SAME
    /// clamp-does-not-bind / ~500 s calibration follow-ups 2-4 document for their settle times, flagged
    /// for main ratification; the loop still holds the level to a few ms in ~13 min, vs the old
    /// hour-scale wander, which is the ticket's whole point.
    #[test]
    fn smoothed_p_term_holds_level_against_tick_noise_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const WARMUP_S: f64 = 2400.0; // past lock; the integral parks near 0
        const START_BUF_MS: f64 = 100.0;
        const DEFICIT_MS: f64 = 15.0;
        const NOISE_MS: f64 = 10.0; // the ±10 ms 1-s mixer-tick phase noise (18.9. live)
        const OBSERVE_S: f64 = 1200.0;
        const SETTLE_WINDOW_CAP: usize = 900; // ~15 min; the design's ≤600 s is aspirational (doc above)

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let target = c.level_target_ms();
        // A pure level deficit; the per-window reading carries ±10 ms alternating tick noise, so a
        // RAW P gain of 2 ppm/ms would jitter the rate ±20 ppm/s — only the EMA makes Kp=2 usable.
        buffer_ms -= DEFICIT_MS;
        let mut levels: Vec<f64> = Vec::new();
        let mut integral_peak = 0.0_f64;
        let mut i: i64 = 0;
        while (i as f64) * BLOCK_S < OBSERVE_S {
            let noise = if i % 2 == 0 { NOISE_MS } else { -NOISE_MS };
            let reported = (buffer_ms + noise).max(0.0);
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, reported);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            levels.push(buffer_ms);
            integral_peak = integral_peak.max(c.level_integral_ppm().abs());
            i += 1;
        }
        // MEAN of the last 60 windows (the ±10 ms alternating noise cancels over 60), tracked window
        // by window; find where it first sits within ±3 ms of target.
        let last60_mean = |upto: usize| -> f64 {
            let s = upto - 60;
            levels[s..upto].iter().sum::<f64>() / 60.0
        };
        let mut first_within3: Option<usize> = None;
        for w in 60..=levels.len() {
            if (last60_mean(w) - target).abs() < 3.0 {
                first_within3 = Some(w);
                break;
            }
        }
        let first = first_within3.unwrap_or(usize::MAX);
        assert!(
            first <= SETTLE_WINDOW_CAP,
            "issue #1335 f5: the smoothed P law must hold the last-60-window mean within ±3 ms of \
             target within ≤{SETTLE_WINDOW_CAP} windows (measured ~777 at Kp=2; the design's ≤600 s \
             is aspirational — see the doc comment), got first-within-3ms at window {first}"
        );
        let final_dev = (last60_mean(levels.len()) - target).abs();
        assert!(
            final_dev < 3.0,
            "issue #1335 f5: once settled the last-60-window mean must HOLD within ±3 ms of target \
             (measured ~0.9), got dev {final_dev:.3} ms"
        );
        assert!(
            integral_peak < LEVEL_INTEGRAL_MAX_PPM,
            "issue #1335 f5: the P term (not the integral) does the work — the integral must NEVER \
             reach its ±{LEVEL_INTEGRAL_MAX_PPM} ppm rail (measured peak ~1.3), got {integral_peak:.3} ppm"
        );
    }

    /// issue #1335 follow-up 5 (b): with the level AT target, ±10 ms alternating per-window tick noise
    /// must NOT make the strong Kp=2 P term chatter the rate — the EMA attenuates the noise below the
    /// clamp's resolution. The mean |applied − estimated| over 600 s (the level term's net
    /// contribution, once slewed into applied) stays ≤ 3 ppm (measured ~0.95). GUARD: a regression
    /// that dropped the EMA and fed the RAW ±10 ms error to Kp=2 would jitter the rate ±20 ppm every
    /// second (mean |applied − estimated| ≫ 3) — this pins the EMA in. The ±10 ms noise also sits
    /// below the 12 ms follow-up-4 band, so the sustained-error restore must never arm.
    #[test]
    fn smoothed_p_term_does_not_chatter_on_tick_noise_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const WARMUP_S: f64 = 2400.0;
        const START_BUF_MS: f64 = 100.0;
        const NOISE_MS: f64 = 10.0;
        const OBSERVE_S: f64 = 600.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let mut sum_abs = 0.0_f64;
        let mut count = 0.0_f64;
        let mut i: i64 = 0;
        while (i as f64) * BLOCK_S < OBSERVE_S {
            let noise = if i % 2 == 0 { NOISE_MS } else { -NOISE_MS };
            let reported = (buffer_ms + noise).max(0.0);
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, reported);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            sum_abs += (c.applied_ppm() - c.estimated_ppm()).abs();
            count += 1.0;
            assert!(
                !c.level_restore(),
                "issue #1335 f5: ±10 ms tick noise around target (below the 12 ms band) must not arm \
                 the fast restore, but it armed at window {i}"
            );
            i += 1;
        }
        let mean_abs = sum_abs / count;
        assert!(
            mean_abs <= 3.0,
            "issue #1335 f5: the EMA must keep the strong Kp=2 P term from chattering on ±10 ms tick \
             noise — the mean |applied − estimated| over the run must stay ≤ 3 ppm (measured ~0.95; a \
             raw-error Kp=2 would be ~±20), got {mean_abs:.3} ppm"
        );
    }

    /// issue #1335 follow-up 5 (c): a DELIBERATE setpoint shift (`shift_level_target(+12)`) with the
    /// true buffer level jumping the same +12 ms in the same window must NOT spike the P term — the
    /// smoothed error (buffered − target) is UNCHANGED (both jumped +12), so the applied correction
    /// barely moves (|Δapplied| ≤ 2 ppm per window; measured 0). GUARD against the design's literal
    /// `level_err_ema_ms -= delta` in `shift_level_target`, which injects a −delta transient and slews
    /// applied to the ±5 ppm/window cap on the next window — the load-bearing follow-up-2 SIGN-
    /// CORRECTION class (see `shift_level_target`'s comment + the ticket's follow-up-5 sign note).
    #[test]
    fn deliberate_shift_does_not_spike_the_p_term_1335() {
        const TRUE_PPM: f64 = -5.0;
        const BLOCK_S: f64 = 1.0;
        const WARMUP_S: f64 = 2400.0;
        const START_BUF_MS: f64 = 100.0;
        const SHIFT_MS: f64 = 12.0;

        let mut c = RealtimeAsrcCompensator::new();
        let clock = DriftingAudioClock::new(TRUE_PPM);
        let mut buffer_ms = START_BUF_MS;
        let mut t = 0.0;
        while t < WARMUP_S {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            t += BLOCK_S;
        }
        let mut applied_prev = c.applied_ppm();
        // The deliberate shift, and the buffer level jumps by the same +12 ms in the same window (the
        // sync-offset re-stamp moves the depth — 18.9. live: level 80 → 108 ms in one second).
        c.shift_level_target(SHIFT_MS);
        buffer_ms += SHIFT_MS;
        // Follow-up 3 arms the restore (|12| ≥ 5 ms) but it exits on the first window (|err| ≈ 0 < 5).
        let mut max_swing = 0.0_f64;
        for _ in 0..8 {
            let raw = clock.raw_advance(BLOCK_S);
            let corrected = c.compensate_with_level(raw, BLOCK_S, buffer_ms);
            buffer_ms += (corrected - BLOCK_S) * 1000.0;
            let applied = c.applied_ppm();
            max_swing = max_swing.max((applied - applied_prev).abs());
            applied_prev = applied;
        }
        assert!(
            max_swing <= 2.0,
            "issue #1335 f5: a deliberate shift where the level jumps WITH the target must NOT spike \
             the P term (the smoothed error is unchanged) — |Δapplied| per window must stay ≤ 2 ppm \
             (measured 0; the design's `ema -= delta` gives 5 = the slew cap), got {max_swing:.3} ppm"
        );
    }
}
