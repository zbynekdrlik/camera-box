//! issue #1084 — the two-clock-domain ENDPOINT-JITTER acceptance gate for the per-source ASRC
//! servo (`camera_box::asrc_bench::RealtimeAsrcCompensator`).
//!
//! The pre-#1084 bench (`simulate_offset_trace_ms` in `src/asrc_bench.rs`) drove the servo with a
//! clock whose per-window aggregate wall time was EXACT — so it exercised the estimator's response
//! to a clean, constant drift but never the dominant REAL noise: audio-thread scheduling jitter in
//! the two wall reads that bound each 1 s measurement window. That jitter telescopes onto the
//! window ENDPOINTS and does NOT average down with more callbacks per window (see issue #1084's
//! design comment), which is exactly why the old bench passed while the live `mbc` source drifted.
//!
//! This gate adds that missing term. It models the endpoint jitter as a STATIONARY per-read
//! Gaussian (`sigma_t`) — the "latency is stationary-iid" noise color, whose window errors are the
//! anti-correlated MA(1) first difference `eps_close − eps_open`. This is the harder-to-reject color
//! AND the one the live stream-OBS log matches (the `estimated` series' lag-1 autocorr was 0.033 =
//! white). A regression estimator (issue #1084) nulls a constant inter-domain drift under it to
//! well inside the acceptance; the pre-#1084 fixed-gain EMA does not (it chases the noise into a
//! ±75–103 ms/h `applied` random walk — the observed global A/V wander), which is what makes this a
//! genuine RED→GREEN gate rather than a tautology.
//!
//! Deterministic (seeded PRNG) and fast (a ~5 h simulated run is ~860k float iterations, well under
//! a second) — no rig, no real audio, same "pure closed-form two-domain simulation" spirit as
//! `src/asrc_bench.rs` itself, per `.claude/rules/asrc-bench-harness.md`.

use camera_box::asrc_bench::{AsrcCompensator, NoCompensation, RealtimeAsrcCompensator};

/// Acceptance: consecutive E2E verdicts (~1 h apart) must have per-cam A/V offset |Δ| < 10 ms/h,
/// i.e. the servo's net correction must be STABLE and near the true drift to within 2.78 ppm.
const ACCEPT_APPLIED_PPM: f64 = 2.8;
/// Acceptance: the A/V offset must not wander more than this over any 1 h window (the |Δ| bound).
const ACCEPT_MAX_1H_DRIFT_MS: f64 = 10.0;
/// The `mbc` audio callback size — 960 samples @ 48 kHz ≈ 20 ms (~48 callbacks/s, matching the live
/// cadence: ~2900 callbacks / 60 s in the stream-OBS `starved_blocks` telemetry).
const BLOCK_FRAMES: f64 = 960.0;
const SAMPLE_RATE: f64 = 48_000.0;

/// Deterministic splitmix64 PRNG + Box–Muller Gaussian — no external crate, bit-reproducible.
struct Rng {
    state: u64,
}
impl Rng {
    fn new(seed: u64) -> Self {
        Rng { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// One simulated run against a `RealtimeAsrcCompensator`. The audio device drifts at `ppm_before`
/// until `t_step_s`, then `ppm_after` (a constant run just sets both equal). Each callback delivers
/// a FIXED sample count (`BLOCK_FRAMES`, sample-count-stamped as `raw_advance_s`); its TRUE master
/// duration is `raw / (1 + ppm/1e6)`, and the servo MEASURES that duration plus a stationary
/// per-read Gaussian jitter `sigma_t` on each of the two wall reads bounding the callback
/// (`meas = true_wall + eps_i − eps_{i-1}`) — the endpoint-jitter model. The A/V offset trace is
/// `(corrected_audio_timeline − true_master_timeline)`, sampled once per master second.
struct RunStats {
    applied_sd: f64,
    applied_mean: f64,
    est_sd: f64,
    max_1h_drift_ms: f64,
}

fn run_realtime(
    sigma_t: f64,
    ppm_before: f64,
    ppm_after: f64,
    t_step_s: f64,
    dur_s: f64,
    warmup_s: f64,
    seed: u64,
) -> RunStats {
    let mut c = RealtimeAsrcCompensator::new();
    let mut rng = Rng::new(seed);
    let raw = BLOCK_FRAMES / SAMPLE_RATE;
    let mut prev_eps = 0.0_f64;
    let mut master_true = 0.0_f64;
    let mut audio = 0.0_f64;
    let mut ideal = 0.0_f64;
    let mut n = 0u64;
    let (mut sa, mut sa2, mut se, mut se2) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    let mut offset_per_sec: Vec<f64> = Vec::new();
    let mut next_sec = 0.0_f64;
    while master_true < dur_s {
        let ppm = if master_true < t_step_s {
            ppm_before
        } else {
            ppm_after
        };
        let true_wall = raw / (1.0 + ppm / 1e6);
        let eps = rng.gauss() * sigma_t;
        let meas = true_wall + (eps - prev_eps);
        prev_eps = eps;
        let corrected = c.compensate(raw, meas);
        audio += corrected;
        ideal += true_wall;
        master_true += true_wall;
        let offset_ms = (audio - ideal) * 1000.0;
        if master_true >= next_sec {
            offset_per_sec.push(offset_ms);
            next_sec += 1.0;
        }
        if master_true >= warmup_s {
            n += 1;
            sa += c.applied_ppm();
            sa2 += c.applied_ppm() * c.applied_ppm();
            se += c.estimated_ppm();
            se2 += c.estimated_ppm() * c.estimated_ppm();
        }
    }
    let nf = n as f64;
    let applied_mean = sa / nf;
    let applied_sd = (sa2 / nf - applied_mean * applied_mean).max(0.0).sqrt();
    let est_mean = se / nf;
    let est_sd = (se2 / nf - est_mean * est_mean).max(0.0).sqrt();
    let window = 3600usize;
    let start = warmup_s as usize;
    let mut max_1h_drift_ms = 0.0_f64;
    if offset_per_sec.len() > start + window {
        for i in start..offset_per_sec.len() - window {
            let d = (offset_per_sec[i + window] - offset_per_sec[i]).abs();
            if d > max_1h_drift_ms {
                max_1h_drift_ms = d;
            }
        }
    }
    RunStats {
        applied_sd,
        applied_mean,
        est_sd,
        max_1h_drift_ms,
    }
}

/// The A/V offset trace of ANY `AsrcCompensator` under the same endpoint-jitter clock — used for
/// the anti-tautology guard (a compensator that does not estimate leaves the offset drifting).
fn max_1h_offset_drift_ms(
    comp: &mut impl AsrcCompensator,
    sigma_t: f64,
    ppm: f64,
    dur_s: f64,
    seed: u64,
) -> f64 {
    let mut rng = Rng::new(seed);
    let raw = BLOCK_FRAMES / SAMPLE_RATE;
    let true_wall = raw / (1.0 + ppm / 1e6);
    let mut prev_eps = 0.0_f64;
    let mut master_true = 0.0_f64;
    let mut audio = 0.0_f64;
    let mut ideal = 0.0_f64;
    let mut offset_per_sec: Vec<f64> = Vec::new();
    let mut next_sec = 0.0_f64;
    while master_true < dur_s {
        let eps = rng.gauss() * sigma_t;
        let meas = true_wall + (eps - prev_eps);
        prev_eps = eps;
        let corrected = comp.compensate(raw, meas);
        audio += corrected;
        ideal += true_wall;
        master_true += true_wall;
        if master_true >= next_sec {
            offset_per_sec.push((audio - ideal) * 1000.0);
            next_sec += 1.0;
        }
    }
    let window = 3600usize;
    let mut m = 0.0_f64;
    if offset_per_sec.len() > window {
        for i in 0..offset_per_sec.len() - window {
            let d = (offset_per_sec[i + window] - offset_per_sec[i]).abs();
            if d > m {
                m = d;
            }
        }
    }
    m
}

/// THE issue #1084 acceptance gate: a CONSTANT inter-domain drift, delivered through realistic
/// stationary endpoint jitter (`sigma_t = 2.6 ms`, which reproduces the live `estimated` sd of
/// ~178 ppm at the pre-#1084 EMA), must be NULLED — the steady `applied` correction must sit within
/// 2.8 ppm (= 10 ms/h) of the true drift with < 2.8 ppm of variance, and the A/V offset must not
/// wander more than 10 ms over any 1 h window. Worst of 5 seeds, so the pass is not seed-luck.
///
/// RED against the pre-#1084 fixed-gain EMA (its `applied` random-walks at ~15 ppm sd ≈ ±55 ms/h,
/// far past both bounds); GREEN against the issue #1084 sliding-regression estimator.
#[test]
fn servo_nulls_a_constant_drift_under_endpoint_jitter_1084() {
    const SIGMA_T: f64 = 2.6e-3;
    const TRUE_PPM: f64 = 12.0;
    const DUR_S: f64 = 5.0 * 3600.0;
    const WARMUP_S: f64 = 1500.0;
    let (mut worst_sd, mut worst_mean_err, mut worst_drift, mut worst_est_sd) =
        (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for &seed in &[1u64, 2, 7, 42, 99] {
        let s = run_realtime(SIGMA_T, TRUE_PPM, TRUE_PPM, 1e18, DUR_S, WARMUP_S, seed);
        worst_sd = worst_sd.max(s.applied_sd);
        worst_mean_err = worst_mean_err.max((s.applied_mean - TRUE_PPM).abs());
        worst_drift = worst_drift.max(s.max_1h_drift_ms);
        worst_est_sd = worst_est_sd.max(s.est_sd);
    }
    assert!(
        worst_sd < ACCEPT_APPLIED_PPM,
        "steady applied ppm sd {worst_sd:.2} must be < {ACCEPT_APPLIED_PPM} ppm (a stable net \
         correction, not a noise-chasing random walk) — the pre-#1084 EMA fails this at ~15 ppm"
    );
    assert!(
        worst_est_sd < ACCEPT_APPLIED_PPM,
        "the regression ESTIMATE itself must be stable (sd {worst_est_sd:.2} < {ACCEPT_APPLIED_PPM} \
         ppm) — the pre-#1084 EMA's estimate sd was ~178 ppm under this jitter"
    );
    assert!(
        worst_mean_err < ACCEPT_APPLIED_PPM,
        "steady applied ppm must sit within {ACCEPT_APPLIED_PPM} ppm of the true {TRUE_PPM} ppm \
         drift (the servo must NULL it), got a mean error of {worst_mean_err:.2} ppm"
    );
    assert!(
        worst_drift < ACCEPT_MAX_1H_DRIFT_MS,
        "the A/V offset must not wander more than {ACCEPT_MAX_1H_DRIFT_MS} ms over any 1 h window \
         (the consecutive-verdict |Δ| acceptance), got {worst_drift:.2} ms"
    );
}

/// The pre-#1084 spec's "<5 ppm @ 2 min" was unachievable on a noise-limited source; issue #1084
/// respecs convergence to ≤10 ppm by 3 min and ≤3 ppm by 10 min (an expanding-then-sliding
/// regression is fast-early / precise-late). Checks the estimate error at each horizon against a
/// constant drift under the same endpoint jitter, worst of 5 seeds.
#[test]
fn servo_converges_within_respec_horizons_under_jitter_1084() {
    const SIGMA_T: f64 = 2.6e-3;
    const TRUE_PPM: f64 = 12.0;
    let raw = BLOCK_FRAMES / SAMPLE_RATE;
    let true_wall = raw / (1.0 + TRUE_PPM / 1e6);
    let (mut worst_3min, mut worst_10min) = (0.0_f64, 0.0_f64);
    for &seed in &[1u64, 2, 7, 42, 99] {
        let mut c = RealtimeAsrcCompensator::new();
        let mut rng = Rng::new(seed);
        let mut prev_eps = 0.0_f64;
        let mut master_true = 0.0_f64;
        let (mut e3, mut e10) = (None, None);
        while master_true < 601.0 {
            let eps = rng.gauss() * SIGMA_T;
            let meas = true_wall + (eps - prev_eps);
            prev_eps = eps;
            let _ = c.compensate(raw, meas);
            master_true += true_wall;
            if e3.is_none() && master_true >= 180.0 {
                e3 = Some((c.applied_ppm() - TRUE_PPM).abs());
            }
            if e10.is_none() && master_true >= 600.0 {
                e10 = Some((c.applied_ppm() - TRUE_PPM).abs());
            }
        }
        worst_3min = worst_3min.max(e3.expect("3min horizon reached"));
        worst_10min = worst_10min.max(e10.expect("10min horizon reached"));
    }
    assert!(
        worst_3min <= 10.0,
        "expected applied within 10 ppm of the true drift by ~3 min, got {worst_3min:.2} ppm"
    );
    assert!(
        worst_10min <= 3.0,
        "expected applied within 3 ppm of the true drift by ~10 min, got {worst_10min:.2} ppm"
    );
}

/// The regression's key advantage over a high-τ EMA: it TRACKS a genuine drift STEP (e.g. a clock
/// reconfiguration like the issue-1073 GM switch) within ~10 min, then holds stable — a slow EMA
/// sized to the same steady variance would take ~1 h. Drift steps 12 → 30 ppm at t = 2 h; assert
/// `applied` reaches within 3 ppm of the new drift by t = 2 h 15 min and holds it at t = 3 h.
#[test]
fn servo_tracks_a_drift_step_within_fifteen_minutes_1084() {
    const SIGMA_T: f64 = 2.6e-3;
    let s = run_realtime(SIGMA_T, 12.0, 30.0, 7200.0, 3.0 * 3600.0, 8100.0, 1);
    // The stats window starts at 2 h 15 min (t_step + 15 min), so applied_mean is the POST-step
    // steady value and applied_sd its post-step stability.
    assert!(
        (s.applied_mean - 30.0).abs() < 3.0,
        "expected applied to re-track the 30 ppm post-step drift within ~15 min, got a steady \
         post-step mean of {:.2} ppm",
        s.applied_mean
    );
    assert!(
        s.applied_sd < ACCEPT_APPLIED_PPM,
        "expected applied to hold the post-step drift stably (sd < {ACCEPT_APPLIED_PPM} ppm), got \
         {:.2} ppm",
        s.applied_sd
    );
}

/// Anti-tautology guard: a compensator that does NOT estimate the drift (a pass-through
/// `NoCompensation`) must FAIL the offset-wander bound under the same clock — proving the GREEN
/// result above depends on genuine estimation, not on a bound loose enough to pass regardless.
#[test]
fn a_non_estimating_compensator_fails_the_acceptance_1084() {
    let mut stub = NoCompensation;
    let drift = max_1h_offset_drift_ms(&mut stub, 2.6e-3, 12.0, 5.0 * 3600.0, 1);
    assert!(
        drift > ACCEPT_MAX_1H_DRIFT_MS,
        "a non-estimating pass-through must blow past the {ACCEPT_MAX_1H_DRIFT_MS} ms/h offset \
         bound (12 ppm ≈ 43 ms/h of uncorrected drift), got {drift:.2} ms — the gate would be \
         tautological otherwise"
    );
}
