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
//! [`compensation_sample_delta`] shows that the achievable resolution ("quantum") of this whole
//! mechanism is `1_000_000.0 / distance_samples` ppm, and any `|ppm|` under HALF a quantum
//! rounds to `sample_delta = 0` -- a complete no-op for that magnitude of drift. There is nothing
//! wrong with the rounding formula itself; the quantum is set entirely by the CALLER's choice of
//! `distance_ms` (at a fixed `output_freq`).
//!
//! ## camera-box #1016 -- the caller's `distance_ms` choice was the bug, not the formula
//!
//! The ONE real caller (`obs-source.c`'s `asrc_process_audio()`) originally always passed
//! `distance_ms = 1000` (a 1-second window), which at the fleet's fixed `output_freq = 48000`
//! made `distance_samples` a constant `48000` and the zero-effect floor `~10.4167 ppm` --
//! squarely inside issue 929's own characterization of real observed steady-state drift as
//! "typically single-digit ppm" (measured end-to-end through real `libswresample`: requested
//! 5ppm -> achieved 0.0000ppm; issue 929 review comment). Issue 1016 fixed this by widening the
//! caller's window to `distance_ms = 10_000` (`ASRC_COMPENSATION_DISTANCE_MS` in
//! `obs-source.c`) -- a purely STATELESS constant change (no new cross-call state; the achieved
//! rate depends only on `distance_samples`, confirmed empirically against real libswresample at
//! several `distance_ms` values via `scripts/asrc-quality-bench/asrc_ab_harness.c --distance-ms`,
//! see that directory's `RESULTS-1016.md`) -- which lowers the floor to `~1.0417 ppm`, covering
//! essentially all of the "typical single-digit ppm" range. [`REAL_DISTANCE_MS`] /
//! [`REAL_CADENCE_ZERO_EFFECT_FLOOR_PPM`] below mirror the FIXED (post-#1016) caller constant;
//! [`PRE_1016_CALLER_DISTANCE_MS`] / [`PRE_1016_ZERO_EFFECT_FLOOR_PPM`] keep the OLD, now-fixed
//! floor documented and tested for historical/regression-proof purposes. See issue 1016's own
//! design comment (`gh issue view 1016 --comments`) for the rejected alternatives (a cross-call
//! fractional accumulator; also fixing the re-trigger cadence in the same change, split into
//! issue 1019 instead) and the disclosed audio-quality trade-off of this fix.

/// Mirrors `audio_resampler_set_compensation_ppm()` exactly: converts a requested drift-rate
/// (parts per million) into the integer sample-delta `swr_set_compensation()` actually receives,
/// for a given `distance_ms`/`output_freq` pair.
///
/// Returns `0` whenever `distance_samples <= 0` (mirrors the vendor function's own early return).
///
/// Returns `i64` where the vendor C casts to a 32-bit `int` -- immaterial at the servo's real
/// range (`ASRC_MAX_PPM=300`, `distance_samples` fixed at 480000 post-#1016, so `sample_delta`
/// never exceeds the low hundreds; see `src/asrc_bench.rs`'s `RealtimeAsrcCompensator`), but this
/// mirror is not bit-exact for a `ppm`/`distance_ms`/`output_freq` combination large enough to
/// overflow `i32`.
pub fn compensation_sample_delta(ppm: f64, distance_ms: u32, output_freq: u32) -> i64 {
    let distance_samples = (output_freq as i64 * distance_ms as i64) / 1000;
    if distance_samples <= 0 {
        return 0;
    }
    (ppm / 1_000_000.0 * distance_samples as f64).round() as i64
}

/// The fleet's real (and only) audio mix rate this servo runs at.
pub const REAL_OUTPUT_FREQ: u32 = 48_000;

/// `obs-source.c`'s `asrc_process_audio()` -> `audio_resampler_set_compensation_ppm()`'s
/// `distance_ms` argument, POST camera-box #1016 (was `1000` pre-fix; widened to lower the
/// zero-effect floor -- see the module doc comment).
pub const REAL_DISTANCE_MS: u32 = 10_000;

/// The magnitude (ppm) below which [`compensation_sample_delta`] rounds to zero at the fleet's
/// real POST-#1016 cadence ([`REAL_DISTANCE_MS`], [`REAL_OUTPUT_FREQ`]) -- half of the
/// achievable quantum (`1_000_000.0 * 1000.0 / (REAL_OUTPUT_FREQ as f64 * REAL_DISTANCE_MS as
/// f64)`), verified live against real libswresample in issue 1016's design comment.
pub const REAL_CADENCE_ZERO_EFFECT_FLOOR_PPM: f64 = 1.04167;

/// The PRE-#1016 caller cadence (`distance_ms=1000`) -- kept only so the OLD, now-fixed floor
/// stays documented and tested; nothing in the real caller uses this value anymore.
pub const PRE_1016_CALLER_DISTANCE_MS: u32 = 1_000;

/// The PRE-#1016 zero-effect floor (at [`PRE_1016_CALLER_DISTANCE_MS`], [`REAL_OUTPUT_FREQ`]) --
/// issue 1016's own starting-point measurement, kept for historical/regression-proof purposes.
pub const PRE_1016_ZERO_EFFECT_FLOOR_PPM: f64 = 10.4167;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_single_digit_ppm_drift_now_reaches_the_resampler_1016() {
        // issue 1016: previously a complete no-op at ALL of these (see
        // pre_1016_cadence_still_reproduces_the_original_no_op below) -- the widened window
        // resolves every one of them to a nonzero, correctly-averaged compensation.
        for ppm in [2.0, 5.0, 8.0, 9.9] {
            assert_ne!(
                compensation_sample_delta(ppm, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
                0,
                "ppm={ppm} should now reach the resampler post-#1016 (floor is ~1.04ppm)"
            );
        }
    }

    #[test]
    fn sub_floor_ppm_still_rounds_to_zero_at_the_new_cadence() {
        // Only genuinely negligible sub-~1ppm drift still floors to zero post-#1016.
        assert_eq!(
            compensation_sample_delta(0.5, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
    }

    #[test]
    fn floor_boundary_matches_the_measured_1_04_ppm_threshold() {
        // 1.0 ppm -> 0.48 samples -> rounds to 0; 1.1 ppm -> 0.528 samples -> rounds to 1.
        // Matches the live libswresample measurement in issue 1016's design comment (achieved_ppm
        // sweep at distance_ms=10000).
        assert_eq!(
            compensation_sample_delta(1.0, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
        assert_eq!(
            compensation_sample_delta(1.1, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ),
            1
        );
    }

    #[test]
    fn above_floor_ppm_produces_the_measured_achieved_rate() {
        // requested 50 ppm -> sample_delta=24 -> achieved = 24/480000 * 1e6 = 50.0 EXACTLY
        // (480000 is an exact multiple of 20000 = 1e6/50) -- matches the live measurement exactly
        // (issue 1016 design comment: --ppm 50 --distance-ms 10000 measured achieved_ppm=50.0000).
        let distance_samples = REAL_OUTPUT_FREQ as i64 * REAL_DISTANCE_MS as i64 / 1000;
        let delta = compensation_sample_delta(50.0, REAL_DISTANCE_MS, REAL_OUTPUT_FREQ);
        assert_eq!(delta, 24);
        let achieved_ppm = delta as f64 / distance_samples as f64 * 1_000_000.0;
        assert!(
            (achieved_ppm - 50.0).abs() < 0.001,
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

    // Historical/regression proof (issue 1016): the pure `compensation_sample_delta` formula was
    // ALWAYS correct -- only the caller's chosen `distance_ms` was the bug. These pin the OLD
    // pre-#1016 caller cadence so a future revert of REAL_DISTANCE_MS back toward 1000 is caught
    // by the tests ABOVE (which use REAL_DISTANCE_MS directly and would go red), while these stay
    // green regardless, documenting what the formula does at ANY distance_ms.

    #[test]
    fn pre_1016_cadence_still_reproduces_the_original_no_op() {
        for ppm in [0.5, 1.0, 2.0, 5.0, 8.0, 9.9] {
            assert_eq!(
                compensation_sample_delta(ppm, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ),
                0,
                "ppm={ppm} at the OLD pre-#1016 cadence should still round to zero"
            );
        }
    }

    #[test]
    fn pre_1016_floor_boundary_matches_the_measured_10_42_ppm_threshold() {
        // 10.4 ppm -> 0.4992 samples -> rounds to 0; 10.5 ppm -> 0.504 samples -> rounds to 1.
        // Matches the live libswresample measurement in issue 929's review comment exactly
        // (requested 10.4 -> achieved 0.0000 ppm; requested 10.5 -> achieved 20.8333 ppm).
        assert_eq!(
            compensation_sample_delta(10.4, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
        assert_eq!(
            compensation_sample_delta(10.5, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ),
            1
        );
    }

    #[test]
    fn pre_1016_above_floor_ppm_produced_a_nonzero_but_coarsely_quantized_delta() {
        // requested 50 ppm -> sample_delta=2 -> achieved = 2/48000 * 1e6 = 41.6667 ppm (17% under
        // target) -- matches the live pre-fix measurement exactly.
        let delta = compensation_sample_delta(50.0, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ);
        assert_eq!(delta, 2);
        let achieved_ppm = delta as f64 / REAL_OUTPUT_FREQ as f64 * 1_000_000.0;
        assert!(
            (achieved_ppm - 41.6667).abs() < 0.001,
            "achieved_ppm={achieved_ppm}"
        );
    }

    #[test]
    fn the_pre_1016_floor_constant_sits_exactly_on_the_rounding_boundary() {
        let just_below = PRE_1016_ZERO_EFFECT_FLOOR_PPM - 0.01;
        let just_above = PRE_1016_ZERO_EFFECT_FLOOR_PPM + 0.01;
        assert_eq!(
            compensation_sample_delta(just_below, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
        assert_ne!(
            compensation_sample_delta(just_above, PRE_1016_CALLER_DISTANCE_MS, REAL_OUTPUT_FREQ),
            0
        );
    }
}
