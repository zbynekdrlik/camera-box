//! #1035 — the absolute end-to-end latency BOUND for the MAIN E2E (`recording-verdict`).
//!
//! ## Why this exists
//!
//! The main E2E run (`scripts/recording-e2e.sh` → `recording-verdict`) computed the per-hop
//! latency (`report["latency"]["cam_strih"]` etc.) and REPORTED it, but no absolute-latency bound
//! ever folded into `overall_pass` — latency was report-only-always-pass. Umbrella issue 406's
//! standing #1 requirement is "zero-loss + BOUNDED-latency + zero A/V-desync"; the bounded-latency
//! half was unenforced in the main E2E. (The separate `frame-probe`/`differ` LOOPBACK path
//! (`scripts/loopback-e2e.sh`) already sets `--max-p99-latency-ms 350`; that mechanism is
//! structurally not part of the recorded-file verdict, where `frame-probe` runs only as the cam2
//! `--paint-only` painter.)
//!
//! ## What is bounded — cam→strih, NOT cam→stream
//!
//! `latency.strih_stream` / `full_chain.latency.cam1_stream` measure ~1000-1150 ms **by design**:
//! the intentional genlock hold that aligns the program video to the ~1s-late mastered audio (the
//! standing A/V-align design — that latency is the operator's alignment domain and is NEVER to be
//! reduced or tightly bounded). The honest, gate-able "absolute latency" is the production
//! **camera→strih** delivery latency BEFORE that hold: `latency.cam_strih` (cam2 paint `gen_ts` →
//! strih program, both on the shared DanteSync wall clock). That is what this module bounds.
//!
//! ## The bound
//!
//! [`CAM_STRIH_P99_LATENCY_MAX_MS`] = 400 ms, derived from 20 recent GREEN `recording-e2e`
//! verdict JSONs (`/tmp/recording-e2e-*/verdict-*.json`): `latency.cam_strih.p99_ms` measured
//! min 210.9 / max 240.7 / mean 227.9 ms, worst single-frame `max_ms` 259.6 ms. 400 ms is 1.66x
//! the worst observed p99 and 1.54x the worst single-frame max — it passes EVERY one of those 20
//! green runs with honest margin while still catching a genuine ~2x regression. Tightening path:
//! toward ~300 ms as more runs characterize the tail.
//!
//! ## The freeze bound is elsewhere and already report-only
//!
//! The recording-path freeze concept is `frozen_leg` (`recording-verdict.rs`), ALREADY wired as a
//! report-only seam (`gates_overall_pass=false`) by explicit user decision issue 914 (cam1
//! ShadowCast grabber defect, issue 909), restore path issue 905. Green runs exist with
//! `frozen>0`, so a hard freeze gate cannot be live now and must not be duplicated here — the
//! freeze bound is that existing seam, untouched. This module wires the genuinely-missing LATENCY
//! bound only.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::optical_floor`] / [`crate::self_heal_attribution`]: the whole
//! `probe` module is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/`drm`), so its logic is
//! CI-only. This is the PURE decision seam — no probe deps — so it unit-tests Tier-0 (default
//! features, no framebuffer, no QR decode). `recording-verdict` (probe-gated) only CALLS these;
//! it never re-derives the threshold.
//!
//! ## The report-only / restore seam
//!
//! [`gates_overall_pass`] mirrors [`crate::optical_floor::gates_overall_pass`]: a one-line-
//! restorable toggle deciding whether the bound folds into `overall_pass`. It is `true` (LIVE)
//! today — unlike the optical/freeze floors, this bound genuinely passes every green run, so it
//! gates for real. Flip to `false` to make it report-only if a future rig change trips it.

/// The absolute cam→strih p99 latency (ms) above which the main-E2E verdict FAILS. Calibrated
/// from 20 green `recording-e2e` runs (worst observed p99 240.7 ms, worst single-frame max
/// 259.6 ms) — see the module doc. 1.66x margin over the worst green p99; tighten toward ~300 ms
/// as the tail is better characterized.
pub const CAM_STRIH_P99_LATENCY_MAX_MS: f64 = 400.0;

/// Does the measured absolute cam→strih p99 latency satisfy the bound?
///
/// Semantics mirror the established [`crate::probe`] loopback convention
/// (`differ::absolute_latency_gate_pass`):
/// - `None` bound ⇒ report-only, always passes.
/// - `Some` bound but `None` measured p99 ⇒ **FAIL** — a requested gate that could not measure
///   (strih recording present but zero paired cam→strih samples) must never report green
///   (test-strictness).
/// - `Some` bound, `Some` p99 ⇒ pass iff `p99 <= bound` (strict `>`: a p99 exactly at the bound
///   passes).
pub fn cam_strih_latency_gate_pass(p99_ms: Option<f64>, max_p99_ms: Option<f64>) -> bool {
    todo!("#1035 GREEN")
}

/// #1035 report-only / restore seam — mirrors [`crate::optical_floor::gates_overall_pass`].
/// Whether [`cam_strih_latency_gate_pass`]'s result folds into the fused verdict's `overall_pass`.
/// `true` today (the bound is LIVE — it passes every green run with margin). Flip to `false` for a
/// one-line revert to report-only if a future rig change ever trips it.
pub fn gates_overall_pass() -> bool {
    todo!("#1035 GREEN")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_bound_is_report_only_always_passes() {
        assert!(cam_strih_latency_gate_pass(Some(9_999.0), None));
        assert!(cam_strih_latency_gate_pass(None, None));
    }

    #[test]
    fn requested_bound_with_no_samples_fails() {
        // strih recording present but zero paired cam→strih samples: a gate that could not run
        // must not report green (test-strictness).
        assert!(!cam_strih_latency_gate_pass(None, Some(CAM_STRIH_P99_LATENCY_MAX_MS)));
    }

    #[test]
    fn boundary_at_bound_passes_just_over_fails() {
        assert!(
            cam_strih_latency_gate_pass(Some(400.0), Some(400.0)),
            "exactly at the bound passes (strict >)"
        );
        assert!(
            !cam_strih_latency_gate_pass(Some(400.1), Some(400.0)),
            "just over the bound fails"
        );
    }

    #[test]
    fn worst_observed_green_run_p99_passes_the_default_bound() {
        // The load-bearing calibration test: the worst p99 measured across the 20 green
        // recording-e2e runs (240.7 ms) MUST pass the default bound — a bound that would fail a
        // recent green run is not a valid bound.
        assert!(
            cam_strih_latency_gate_pass(Some(240.7), Some(CAM_STRIH_P99_LATENCY_MAX_MS)),
            "the worst observed green p99 (240.7 ms) must pass the {CAM_STRIH_P99_LATENCY_MAX_MS} ms bound"
        );
        // ...and the worst single-frame max (259.6) is also comfortably under the p99 bound.
        assert!(cam_strih_latency_gate_pass(Some(259.6), Some(CAM_STRIH_P99_LATENCY_MAX_MS)));
    }

    #[test]
    fn a_genuine_2x_regression_fails() {
        // ~2x the worst green p99 (a real cam→strih delivery regression) must FAIL.
        assert!(!cam_strih_latency_gate_pass(Some(481.4), Some(CAM_STRIH_P99_LATENCY_MAX_MS)));
    }

    #[test]
    fn default_bound_constant_is_the_calibrated_value() {
        assert_eq!(CAM_STRIH_P99_LATENCY_MAX_MS, 400.0);
    }

    #[test]
    fn gate_is_live_today() {
        // #1035: the latency bound folds into overall_pass (LIVE), unlike the optical/freeze
        // floors which are report-only pending hardware. It passes every green run with margin.
        assert!(
            gates_overall_pass(),
            "#1035: the absolute cam→strih latency bound must gate overall_pass (LIVE)"
        );
    }
}
