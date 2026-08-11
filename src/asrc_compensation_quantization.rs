//! camera-box #929 -- pure-Rust mirror of `audio_resampler_set_compensation_ppm()`'s integer
//! sample-delta rounding (`vendor/obs-studio/libobs/media-io/audio-resampler-ffmpeg.c` ~L221-238),
//! extracted so the compensation-quantization threshold discovered while measuring issue 929's
//! ASRC resampling-quality A/B (`scripts/asrc-quality-bench/`) is pinned by a Tier-0 test, not
//! just a one-off C-harness printout.
//!
//! ## Why this lives here (not `src/probe/`)
//!
//! Same rationale as `src/asrc_bench.rs`/`src/reannounce.rs`: pure closed-form arithmetic, no
//! hardware, no `probe` feature deps -- unit-tests on default features (`cargo test`, Tier-0,
//! bypass documented in project CLAUDE.md's Local Build Policy).
//!
//! ## What this documents
//!
//! `swr_set_compensation(ctx, sample_delta, compensation_distance)` takes an INTEGER sample
//! count, not a ppm float. The vendor wrapper computes that integer via:
//!
//! ```text
//! distance_samples = output_freq * distance_ms / 1000
//! sample_delta = round(ppm / 1_000_000.0 * distance_samples)
//! ```
//!
//! The ONE real caller (`obs-source.c`'s `asrc_process_audio()`) always passes `distance_ms =
//! 1000`, so at the fleet's fixed `output_freq = 48000`, `distance_samples` is always `48000`.
//! [`compensation_sample_delta`] shows that any `|ppm| < ~10.4167` therefore rounds to
//! `sample_delta = 0` -- i.e. the servo's requested correction NEVER reaches the resampler for
//! typical single-digit-ppm drift (issue 929's own Context section describes real observed
//! steady-state drift as "typically single-digit ppm"). This was measured end-to-end through the
//! real `libswresample` too (issue 929 review comment); this module pins the pure arithmetic that
//! explains WHY, and is the reference for the follow-up fix (issue 1016, out of scope here).

/// Mirrors `audio_resampler_set_compensation_ppm()` exactly: converts a requested drift-rate
/// (parts per million) into the integer sample-delta `swr_set_compensation()` actually receives,
/// given the fixed `distance_ms=1000` cadence the real caller always uses.
///
/// Returns `0` whenever `distance_samples <= 0` (mirrors the vendor function's own early return).
///
/// Returns `i64` where the vendor C casts to a 32-bit `int` -- immaterial at the servo's real
/// range (`ASRC_MAX_PPM=300`, `distance_samples` fixed at 48000, so `sample_delta` never exceeds
/// the low tens; see `src/asrc_bench.rs`'s `RealtimeAsrcCompensator`), but this mirror is not
/// bit-exact for a `ppm`/`distance_ms`/`output_freq` combination large enough to overflow `i32`.
pub fn compensation_sample_delta(ppm: f64, distance_ms: u32, output_freq: u32) -> i64 {
    let distance_samples = (output_freq as i64 * distance_ms as i64) / 1000;
    if distance_samples <= 0 {
        return 0;
    }
    (ppm / 1_000_000.0 * distance_samples as f64).round() as i64
}

/// The magnitude (ppm) below which [`compensation_sample_delta`] rounds to zero at the fleet's
/// real cadence (`output_freq=48000`, `distance_ms=1000`) -- the exact quantization floor
/// measured live against real `libswresample` in issue 929's review comment.
pub const REAL_CADENCE_ZERO_EFFECT_FLOOR_PPM: f64 = 10.4167;

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_OUTPUT_FREQ: u32 = 48000;
    const REAL_DISTANCE_MS: u32 = 1000;

    #[test]
    fn typical_single_digit_ppm_drift_is_a_complete_no_op() {
        // Issue 929's own Context section: real observed steady-state drift is "typically
        // single-digit ppm". At the real cadence, every one of these rounds to zero.
        for ppm in [0.5, 1.0, 2.0, 5.0, 8.0, 9.9] {
            assert_eq!(
                compensation_sample_delta(ppm, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
                0,
                "ppm={ppm} should round to a zero sample_delta (measured floor is ~10.42 ppm)"
            );
        }
    }

    #[test]
    fn floor_boundary_matches_the_measured_10_42_ppm_threshold() {
        // 10.4 ppm -> 0.4992 samples -> rounds to 0; 10.5 ppm -> 0.504 samples -> rounds to 1.
        // Matches the live libswresample measurement in issue 929's review comment exactly
        // (requested 10.4 -> achieved 0.0000 ppm; requested 10.5 -> achieved 20.8333 ppm).
        assert_eq!(
            compensation_sample_delta(10.4, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
        assert_eq!(
            compensation_sample_delta(10.5, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            1
        );
    }

    #[test]
    fn above_floor_ppm_produces_a_nonzero_but_coarsely_quantized_delta() {
        // requested 50 ppm -> sample_delta=2 -> achieved = 2/48000 * 1e6 = 41.6667 ppm (17% under
        // target) -- matches the live measurement exactly.
        let delta = compensation_sample_delta(50.0, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ);
        assert_eq!(delta, 2);
        let achieved_ppm = delta as f64 / REAL_OUTPUT_FREQ as f64 * 1_000_000.0;
        assert!(
            (achieved_ppm - 41.6667).abs() < 0.001,
            "achieved_ppm={achieved_ppm}"
        );
    }

    #[test]
    fn the_documented_floor_constant_sits_exactly_on_the_rounding_boundary() {
        // Below the constant -> 0; at/above -> nonzero. Keeps the doc constant honest against
        // the real rounding formula instead of drifting out of sync with it.
        let just_below = REAL_CADENCE_ZERO_EFFECT_FLOOR_PPM - 0.01;
        let just_above = REAL_CADENCE_ZERO_EFFECT_FLOOR_PPM + 0.01;
        assert_eq!(
            compensation_sample_delta(just_below, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
        assert_ne!(
            compensation_sample_delta(just_above, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
    }

    #[test]
    fn zero_distance_ms_is_a_safe_no_op_not_a_panic_or_division_surprise() {
        assert_eq!(compensation_sample_delta(300.0, 0, REAL_OUTPUT_FREQ), 0);
    }
}
