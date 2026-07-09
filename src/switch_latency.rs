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
//! live cut between them stays within lipsync tolerance? The #624 issue body fixes the
//! threshold at half a 30fps program frame (16ms, [`SPREAD_THRESHOLD_MS`]) —
//! `max(p50) − min(p50) > 16ms` = FAIL. This is a PRE-SPECIFIED constant, never derived or
//! overridden.
//!
//! Crate-root pure seam (default features, Tier-0 per the project CLAUDE.md), deliberately
//! sibling to `recording_span_gate.rs` / `imag_tick_gate.rs`: it operates on PLAIN `f64` p50
//! values, never on the probe-gated `probe::recording_latency::HopLatency` (the whole `probe`
//! module is `#[cfg(feature = "probe")]`, CI-only — a decision that lived there could never be
//! RED→GREEN-verified locally). The probe-gated `bin/recording-verdict` extracts each
//! measured camera's `HopLatency.stats.p50_ms` and calls in here.

/// #24 — the camera-under-test node labels, mirrored from `bin/recording-verdict.rs`'s
/// `CAMERA_UNDER_TEST_NODES` (kept as a local copy, like `recording_span_gate.rs`'s own copy,
/// so this crate-root module has zero dependency on the probe-gated binary). Purely
/// documentary here — [`spread_verdict`] itself is camera-label-agnostic (it takes plain p50
/// values), this constant just names the set the #624 gate applies to.
pub const CAMERA_UNDER_TEST_NODES: [&str; 3] = ["cam1", "cam3", "cam4"];

/// The #624 issue's fixed cross-camera spread threshold, in milliseconds: half a 30fps
/// program frame (`1000.0 / 30.0 / 2.0 ≈ 16.667`, rounded down to the issue's literal `16ms`).
/// `max(p50) − min(p50)` STRICTLY GREATER than this = FAIL; exactly at the threshold PASSES
/// (the issue text is "> 16 ms ... = FAIL", so `==16.0` is not yet over the line).
pub const SPREAD_THRESHOLD_MS: f64 = 16.0;

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
        // The boundary itself PASSES — only STRICTLY over the threshold fails (issue text:
        // "> 16 ms ... = FAIL").
        pass: spread_ms <= SPREAD_THRESHOLD_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_constant_matches_the_624_issue_text() {
        assert_eq!(
            SPREAD_THRESHOLD_MS, 16.0,
            "#624 issue body: half a 30fps program frame"
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
    fn spread_over_16ms_fails_the_gate() {
        // 825 - 795 = 30ms > 16ms => FAIL.
        let v = spread_verdict(&[795.0, 810.0, 825.0]).expect("3 measured cameras");
        assert_eq!(v.spread_ms, 30.0);
        assert!(
            !v.pass,
            "a 30ms cross-camera spread must FAIL the #624 gate: {v:?}"
        );
    }

    #[test]
    fn spread_exactly_at_16ms_passes_the_boundary() {
        // Boundary convention (issue text "> 16 ms ... = FAIL"): ==16.0 PASSES.
        let v = spread_verdict(&[800.0, 816.0]).expect("2 measured cameras");
        assert_eq!(v.spread_ms, 16.0);
        assert!(
            v.pass,
            "exactly 16.0ms must PASS — only STRICTLY over 16.0 fails: {v:?}"
        );
    }

    #[test]
    fn spread_just_over_16ms_fails() {
        let v = spread_verdict(&[800.0, 816.01]).expect("2 measured cameras");
        assert!((v.spread_ms - 16.01).abs() < 1e-9);
        assert!(!v.pass, "16.01ms is strictly over the 16.0ms floor: {v:?}");
    }

    #[test]
    fn spread_just_under_16ms_passes() {
        let v = spread_verdict(&[800.0, 815.99]).expect("2 measured cameras");
        assert!((v.spread_ms - 15.99).abs() < 1e-9);
        assert!(v.pass, "15.99ms is under the 16.0ms floor: {v:?}");
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
