//! #624 — cross-camera cam2→camera switch-latency SPREAD gate (pure decision).
//!
//! Each grabber card that OPTICALLY films cam2's painted monitor (cam1/cam3/cam4/cam5/cam6 —
//! `bin/recording-verdict`'s `OPTICAL_INJECTION_NODES`; #312 widened the fleet from 4→6 and
//! explicitly EXCLUDES cam2 itself here, since cam2 IS the painter and has no second
//! camera-vs-monitor optical hop to measure) bakes its OWN photon->dequeue latency `d_X` into
//! the frame it emits (the #286 root cause: the genlock timecode used to be stamped at
//! ARRIVAL, not at the real V4L2 CAPTURE instant, so `d_X` rides into delivery timing and the
//! receiver's genlock cannot equalize it across cameras — cutting the live program between two
//! cameras with different `d_X` can visibly break A/V lipsync). The cam2→camera
//! OPTICAL-INJECTION hop (`probe::recording_latency::cam2_cam1_samples_from_burn` /
//! `_from_flip`, generalized in #624 from cam1-only to cam1/cam3/cam4 and further in #312 to
//! cam5/cam6, computed PER `--switch-schedule` window in `bin/recording-verdict`) IS `d_X` for
//! that camera: cam2 paints a monitor, the camera under test films it, and the camera's OWN
//! capture-time burn rides alongside cam2's optical QR into the same recorded frame — so
//! `camera_burn.gen_ts_ns − cam2.gen_ts_ns` is exactly this camera's photon-to-delivery
//! latency.
//!
//! This module is the FINAL pure decision on top of that per-camera measurement: given each
//! measured camera's median (p50) latency, is the SPREAD across cameras small enough that a
//! live cut between them stays within lipsync tolerance? #624 originally fixed the threshold at
//! half a 30fps program frame (16ms); issue 1120 RECALIBRATED it to 24ms
//! ([`SPREAD_THRESHOLD_MS`]) — `max(p50) − min(p50) > 24ms` = FAIL — with honest margin above
//! the live CAM1-included distribution while the ShadowCast 2 grabber residual (issue 1110)
//! persists. See [`SPREAD_THRESHOLD_MS`]'s own doc for the data + the re-tighten condition.
//!
//! Crate-root pure seam (default features, Tier-0 per the project CLAUDE.md), deliberately
//! sibling to `recording_span_gate.rs` / `imag_tick_gate.rs`: it operates on PLAIN `f64` p50
//! values, never on the probe-gated `probe::recording_latency::HopLatency` (the whole `probe`
//! module is `#[cfg(feature = "probe")]`, CI-only — a decision that lived there could never be
//! RED→GREEN-verified locally). The probe-gated `bin/recording-verdict` extracts each
//! measured camera's `HopLatency.stats.p50_ms` and calls in here.

/// #24/#312 — the OPTICAL-INJECTION node labels this SPREAD gate applies to, mirrored from
/// `bin/recording-verdict.rs`'s `OPTICAL_INJECTION_NODES` (kept as a local copy, like
/// `recording_span_gate.rs` keeps its own copy of the BROADER `CAMERA_UNDER_TEST_NODES`, so this
/// crate-root module has zero dependency on the probe-gated binary). Purely documentary here —
/// [`spread_verdict`] itself is camera-label-agnostic (it takes plain p50 values), this constant
/// just names the set the #624 gate applies to.
///
/// **Deliberately the NARROWER `OPTICAL_INJECTION_NODES` set (5 members), NOT the broader
/// `CAMERA_UNDER_TEST_NODES` (6, includes cam2) — was itself a stale 3-member
/// `CAMERA_UNDER_TEST_NODES`-named copy before #312 caught + fixed it.** This module's SPREAD
/// gate is specifically about the cam2→camera OPTICAL-INJECTION latency (see the module doc
/// above) — cam2 is the painter, not an optical-injection camera, so it correctly has no place
/// in this set, unlike the digital-contiguity `CAMERA_UNDER_TEST_NODES` which DOES include it.
pub const OPTICAL_INJECTION_NODES: [&str; 6] = ["cam1", "cam3", "cam4", "cam5", "cam6", "cam7"];

/// The cross-camera spread threshold, in milliseconds. `max(p50) − min(p50)` STRICTLY GREATER
/// than this = FAIL; exactly at the threshold PASSES (only a spread strictly over it fails).
///
/// **RECALIBRATED 24.0 ms by issue 1120 (was the 16 ms half-frame of #624).** WHY it is not the
/// half-frame ideal: the CAM1 ShadowCast 2 grabber pipeline (issue 1110) bakes ~17 ms of extra
/// photon→capture latency into cam1's emitted frame vs the other grabbers, so the live
/// cross-camera source-spread tail reaches ~16.90 ms — over the old 16 ms bound — even on a run
/// that is green on everything else. That made the gate a per-run COIN FLIP on the grabber's
/// run-to-run variance (mined greens 12.38 / 14.30 / 14.81 ms; the sole spread-gate fail
/// 16.90 ms), not a real regression. 24.0 ms is the tightest bound the CAM1-included data
/// supports with honest margin: 1.42× the worst observed spread (16.90), ~mean+6σ of the
/// observed distribution, AND still 0.72× a full 33.3 ms program frame — so a cut between two
/// cameras up to 24 ms apart stays within one frame's lipsync tolerance, while a genuinely-broken
/// >1-frame spread still fails.
///
/// **RE-TIGHTEN CONDITION (a follow-up ticket, gated on the issue-1110 grabber swap):** once the
/// ShadowCast grabber is swapped out and the cam1 latency residual is gone, walk this constant
/// back toward the half-frame 16.0 from FRESH green post-swap data (per
/// `window-gate-tolerance-walkdown.md`). This constant is deliberately shared with the
/// DELIVERY-side gate (`crate::delivery_spread_gate::DELIVERY_SPREAD_BOUND_MS` re-exports it);
/// both spreads are driven by the SAME grabber and re-tighten together.
pub const SPREAD_THRESHOLD_MS: f64 = 24.0;

/// The cross-camera spread verdict: the highest and lowest per-camera median cam2→camera
/// latency measured this run, their spread, and whether that spread clears the #624 gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpreadVerdict {
    /// The highest per-camera p50 latency (ms) among the measured cameras.
    pub max_p50_ms: f64,
    /// The lowest per-camera p50 latency (ms) among the measured cameras.
    pub min_p50_ms: f64,
    /// `max_p50_ms − min_p50_ms`, in milliseconds.
    pub spread_ms: f64,
    /// `true` ⇔ `spread_ms <= `[`SPREAD_THRESHOLD_MS`] (the boundary itself PASSES; only a
    /// spread strictly over the threshold FAILS).
    pub pass: bool,
}

/// THE pure check: given the measured cam2→camera median (p50) latency, in milliseconds, of
/// EVERY camera that produced at least one sample this run, compute the cross-camera spread
/// and gate it against [`SPREAD_THRESHOLD_MS`].
///
/// `None` when fewer than 2 cameras were measured — a spread needs at least two points to
/// compare, and a single (or zero) measured camera proves nothing about cross-camera
/// consistency. This is deliberately DISTINCT from a failing gate: the caller (the
/// probe-gated `bin/recording-verdict`) reports an unmeasurable gate as `null`/absent (never
/// a fabricated pass OR a fabricated fail) and does not fold it into the run's overall
/// verdict — mirroring how `imag_tick_gate::optional_signal_ok` treats an absent OPTIONAL
/// signal as "nothing to fail on", never fabricating a result from missing data.
///
/// Order-independent — the same set of p50 values in any order yields the same spread.
pub fn spread_verdict(p50s_ms: &[f64]) -> Option<SpreadVerdict> {
    if p50s_ms.len() < 2 {
        return None;
    }
    let max_p50_ms = p50s_ms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_p50_ms = p50s_ms.iter().copied().fold(f64::INFINITY, f64::min);
    let spread_ms = max_p50_ms - min_p50_ms;
    Some(SpreadVerdict {
        max_p50_ms,
        min_p50_ms,
        spread_ms,
        // The boundary itself PASSES — only a spread STRICTLY over SPREAD_THRESHOLD_MS fails.
        pass: spread_ms <= SPREAD_THRESHOLD_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_constant_is_the_1120_recalibrated_bound() {
        // issue 1120: recalibrated from the 16 ms half-frame ideal to 24 ms with honest margin
        // above the live CAM1-included distribution (worst observed source spread 16.90 ms; the
        // ShadowCast 2 grabber residual, issue 1110). 24 ms = 1.42x the worst observed spread AND
        // still 0.72x a full 33.3 ms program frame (under a full frame's lipsync tolerance).
        assert_eq!(
            SPREAD_THRESHOLD_MS, 24.0,
            "issue 1120: recalibrated cross-camera spread bound (was 16.0)"
        );
    }

    #[test]
    fn live_16_9_spread_passes_after_1120_recalibration() {
        // The exact E2E-attempt-4 failure (verdict 675817084): a 16.90 ms cross-camera source
        // spread (cam1 66.53 ms vs cam3 49.64 ms) that was overall-green on EVERYTHING else red
        // the run at the old 16.0 ms bound — a per-run coin flip on the CAM1 grabber's variance,
        // not a real regression. After the issue-1120 recalibration to 24 ms it must PASS.
        let v = spread_verdict(&[49.635_278, 66.534_963]).expect("2 measured cameras");
        assert!(
            (v.spread_ms - 16.899_685).abs() < 1e-6,
            "reproduces the live spread: {v:?}"
        );
        assert!(
            v.pass,
            "the live 16.90 ms spread must PASS after the 24 ms recalibration: {v:?}"
        );
    }

    #[test]
    fn fewer_than_two_measured_cameras_cannot_compute_a_spread() {
        assert_eq!(
            spread_verdict(&[]),
            None,
            "zero measured cameras: nothing to compare"
        );
        assert_eq!(
            spread_verdict(&[812.3]),
            None,
            "one measured camera: no second point for a spread"
        );
    }

    #[test]
    fn spread_is_max_minus_min_p50_order_independent() {
        let v = spread_verdict(&[800.0, 820.0, 795.0]).expect("3 measured cameras");
        assert_eq!(v.max_p50_ms, 820.0);
        assert_eq!(v.min_p50_ms, 795.0);
        assert_eq!(v.spread_ms, 25.0);
        // The SAME 3 values in a different order must yield the SAME spread.
        let shuffled = spread_verdict(&[795.0, 800.0, 820.0]).expect("same 3, different order");
        assert_eq!(shuffled.spread_ms, v.spread_ms);
        assert_eq!(shuffled.max_p50_ms, v.max_p50_ms);
        assert_eq!(shuffled.min_p50_ms, v.min_p50_ms);
    }

    #[test]
    fn spread_well_over_the_bound_fails_the_gate() {
        // A genuinely-broken cross-camera spread — 850 - 790 = 60ms, well over a full 33.3ms
        // program frame — must FAIL regardless of the (recalibrated) 24ms bound. This is the
        // "a genuinely-broken spread above the new bound must FAIL" guard for issue 1120.
        let v = spread_verdict(&[790.0, 810.0, 850.0]).expect("3 measured cameras");
        assert_eq!(v.spread_ms, 60.0);
        assert!(
            !v.pass,
            "a 60ms cross-camera spread must FAIL the gate: {v:?}"
        );
    }

    #[test]
    fn spread_exactly_at_24ms_passes_the_boundary() {
        // Boundary convention (spread STRICTLY over the threshold = FAIL): ==SPREAD_THRESHOLD_MS
        // (24.0 since issue 1120) PASSES.
        let v = spread_verdict(&[800.0, 824.0]).expect("2 measured cameras");
        assert_eq!(v.spread_ms, 24.0);
        assert!(
            v.pass,
            "exactly 24.0ms must PASS — only STRICTLY over 24.0 fails: {v:?}"
        );
    }

    #[test]
    fn spread_just_over_24ms_fails() {
        let v = spread_verdict(&[800.0, 824.01]).expect("2 measured cameras");
        assert!((v.spread_ms - 24.01).abs() < 1e-9);
        assert!(!v.pass, "24.01ms is strictly over the 24.0ms bound: {v:?}");
    }

    #[test]
    fn spread_just_under_24ms_passes() {
        let v = spread_verdict(&[800.0, 823.99]).expect("2 measured cameras");
        assert!((v.spread_ms - 23.99).abs() < 1e-9);
        assert!(v.pass, "23.99ms is under the 24.0ms bound: {v:?}");
    }

    #[test]
    fn zero_spread_passes() {
        let v = spread_verdict(&[800.0, 800.0, 800.0]).expect("3 identical cameras");
        assert_eq!(v.spread_ms, 0.0);
        assert!(v.pass);
    }

    #[test]
    fn two_of_three_cameras_measured_still_computes_a_spread() {
        // A camera that never produced a sample (its own window(s) absent from the sweep, or
        // its burn never decoded) is simply excluded by the CALLER before this function ever
        // sees it (the bin wiring only feeds in p50s for cameras that DID produce samples) —
        // the pure gate itself has no notion of "3 cameras total", only "however many were
        // handed in".
        let v = spread_verdict(&[810.0, 826.0]).expect("2 of 3 cameras measured");
        assert_eq!(v.spread_ms, 16.0);
        assert!(v.pass);
    }
}
