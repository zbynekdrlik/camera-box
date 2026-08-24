//! Projection-tap scanout-TEAR detector (issue 781) — PURE, report-only.
//!
//! ## What it measures
//!
//! cam2's USB grabber is fed by imag-nb's HDMI output (owner-confirmed 2026-08-24), so cam2's leg
//! in the all-cambox E2E sweep already captures the physical projection path (imag render → DRM
//! scanout → HDMI → grabber) — "what the audience sees". This module formalizes the tear check the
//! ticket asks for: a captured frame that carries HALVES of two DIFFERENT consecutive painted ticks
//! (top half tick N, bottom half tick N+1) is a scanout TEAR event.
//!
//! ## The signal, derived from the REAL painted content (not geometry-only)
//!
//! The painted source is cam2's optical **dual-QR Vernier**: the LEFT QR carries the latest EVEN
//! tick, the RIGHT the latest ODD tick (`probe::recording_latency::split_payloads`,
//! `RecordingFrame::tick`). A HEALTHY captured frame therefore decodes exactly two cam2-optical
//! payloads whose `frame_id`s are adjacent — `max(frame_id) - min(frame_id) == 1`
//! ([`VERNIER_MAX_SPREAD`]). A frame that captured TWO distinct paint GENERATIONS (a scanout tear
//! straddling a page-flip) carries a WIDER optical span — `max - min > VERNIER_MAX_SPREAD`. That
//! wider span IS the ticket's "two different consecutive painted ticks in one frame", generalized
//! correctly to the Vernier's even/odd pair. Node digital burns (`probe::recording::NODE_BURN_RUN_IDS`)
//! are NOT the optical Vernier and MUST be excluded by the caller before the ids reach this module.
//!
//! ## Report-only, and WHY (a proven-blind signal on the CURRENT content)
//!
//! Measured across 5 real `stream-partial-*.json` (~48 000 frames), the per-frame optical span is
//! exclusively {0,1} and the optical-QR count per frame never exceeds 2 — the "two generations in
//! one frame" signal NEVER fires on the current content. The reason is structural (confirmed by
//! reading real captured frames): both dual-QR halves sit in ONE vertical band (top ~60%), so a
//! horizontal scanout tear crossing that band corrupts BOTH QRs at the same height → the frame goes
//! `undecodable` (tick=None) rather than yielding two clean generations. A tear cannot manufacture a
//! second, older/newer generation of a QR that exists at only one vertical position. So an all-zero
//! `tear_fraction` on this content means EITHER "no tears occurred" (e.g. post the issue-1107
//! render-side fix) OR "the signal is blind here" — the two are indistinguishable without a
//! known-torn run. Per the "a gate that can never fire is worse than no gate" doctrine (issue
//! 1101/1088), this module is REPORT-ONLY ([`gates_overall_pass`] returns `false`) and carries a
//! computed [`TearSignalViability`] so an all-zero reading can never be mistaken for a promotable
//! green.
//!
//! ## Precondition for a future LIVE gate (follow-up)
//!
//! For the ticket's tear model to be decodable, the painted pattern would need VERTICAL tick
//! redundancy — a tick indicator in BOTH the top and bottom halves (a second dual-QR row lower down,
//! or a full-height tick strip). That is a painter change (rig-side) plus a decode change, filed as a
//! follow-up. Until the signal is observed to actually fire ([`TearSignalViability::Observed`]) on a
//! known-torn run and a bound is calibrated, [`gates_overall_pass`] stays `false`.
//!
//! Mirrors the crate-root `gates_overall_pass()` seam pattern shared by `presentation_cadence` /
//! `optical_floor` / `e2e_latency_gate` / `imag_leg_gate`: PURE (default features, Tier-0
//! unit-testable); the probe-gated `recording-verdict.rs` consumer only feeds it the per-frame
//! optical ids and folds the report-only verdict.

use serde::Serialize;

/// The by-design optical span of ONE healthy captured frame: the dual-QR Vernier's LEFT (latest
/// even) and RIGHT (latest odd) halves differ by exactly one tick, so `max(frame_id) - min(frame_id)
/// == 1`. A wider span means the frame captured >= 2 distinct paint generations — a scanout tear.
pub const VERNIER_MAX_SPREAD: u32 = 1;

/// Provisional report-only ceiling for [`tear_gate_pass`]. This module is report-only
/// ([`gates_overall_pass`] returns `false`), so this value does NOT gate today; it is `0.0` as a
/// placeholder. RECALIBRATE from a real known-torn run's distribution (per
/// `verdict-gate-seam-calibration.md`) before any LIVE flip.
pub const TEAR_FRACTION_CEILING: f64 = 0.0;

/// The optical `frame_id` span within ONE captured frame — `max - min` over the cam2-optical
/// Vernier payloads (node burns already excluded by the caller). `None` when the frame carries no
/// optical payload at all (an undecodable frame — counted elsewhere as `undecodable`, never a tear).
pub fn frame_optical_spread(optical_ids: &[u32]) -> Option<u32> {
    let min = *optical_ids.iter().min()?;
    let max = *optical_ids.iter().max()?;
    Some(max - min)
}

/// A captured frame is TORN when its cam2-optical Vernier payloads span more than the by-design
/// even/odd adjacency ([`VERNIER_MAX_SPREAD`]) — i.e. it carries >= 2 distinct paint generations.
pub fn is_torn_frame(optical_ids: &[u32]) -> bool {
    frame_optical_spread(optical_ids).is_some_and(|s| s > VERNIER_MAX_SPREAD)
}

/// Whether the tear signal has DEMONSTRABLY fired on the analyzed data — the machine-checked
/// promotion-readiness property (mirrors `dup_cadence`'s viability classifier, issue 1101).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TearSignalViability {
    /// >= 1 torn frame observed: the signal provably CAN fire on this content/run.
    Observed,
    /// No torn frame observed: cannot distinguish "no tears" from "signal blind on this content"
    /// (the single-vertical-band dual-QR layout, issue 781). A LIVE flip stays gated on `Observed`.
    Unproven,
}

/// Per-window tear report (report-only). Derives only `PartialEq` (not `Eq`) — `tear_fraction` is
/// `f64` (the #726 Eq-on-f64 trap).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TearStats {
    /// In-window frames that carried at least one optical Vernier payload (undecodable frames
    /// excluded — a tear is measured only where a tick decoded).
    pub decodable_frames: u32,
    /// Frames whose optical span exceeded [`VERNIER_MAX_SPREAD`] (>= 2 paint generations captured).
    pub tear_frames: u32,
    /// `tear_frames / decodable_frames` (0.0 when no decodable frame).
    pub tear_fraction: f64,
    /// The largest optical span observed in the window (0 or 1 = clean; >= 2 = a tear occurred).
    pub max_spread: u32,
    /// Whether the signal fired on this window (see [`TearSignalViability`]).
    pub viability: TearSignalViability,
}

/// Aggregate per-window tear stats from each in-window frame's cam2-optical `frame_id`s (node burns
/// already excluded, undecodable frames passed as empty slices).
pub fn window_tear_stats(per_frame_optical_ids: &[Vec<u32>]) -> TearStats {
    let mut decodable_frames = 0u32;
    let mut tear_frames = 0u32;
    let mut max_spread = 0u32;
    for ids in per_frame_optical_ids {
        if let Some(spread) = frame_optical_spread(ids) {
            decodable_frames += 1;
            if spread > max_spread {
                max_spread = spread;
            }
            if spread > VERNIER_MAX_SPREAD {
                tear_frames += 1;
            }
        }
    }
    let tear_fraction = if decodable_frames > 0 {
        tear_frames as f64 / decodable_frames as f64
    } else {
        0.0
    };
    let viability = if tear_frames > 0 {
        TearSignalViability::Observed
    } else {
        TearSignalViability::Unproven
    };
    TearStats {
        decodable_frames,
        tear_frames,
        tear_fraction,
        max_spread,
        viability,
    }
}

/// Per-window report-only pass: `tear_fraction <= TEAR_FRACTION_CEILING`. Does NOT gate while
/// [`gates_overall_pass`] is `false`.
pub fn tear_gate_pass(stats: &TearStats) -> bool {
    stats.tear_fraction <= TEAR_FRACTION_CEILING
}

/// Whether the tear gate folds into the fused `overall_pass`. REPORT-ONLY (`false`): flip to `true`
/// (one line) only after the signal is [`TearSignalViability::Observed`] on a known-torn run AND a
/// bound is calibrated (see the module-level "Precondition" note + `verdict-gate-seam-calibration.md`).
pub fn gates_overall_pass() -> bool {
    false
}

/// All windows pass — the run-level report-only fold helper for the probe consumer.
pub fn run_tear_gate_pass(stats: &[TearStats]) -> bool {
    stats.iter().all(tear_gate_pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_vernier_pair_is_not_torn() {
        // LEFT=even 100, RIGHT=odd 101 -> span 1 -> the by-design adjacency, NOT a tear.
        assert!(!is_torn_frame(&[100, 101]));
        assert!(!is_torn_frame(&[101, 100]));
        assert_eq!(frame_optical_spread(&[100, 101]), Some(1));
    }

    #[test]
    fn single_optical_half_is_not_torn() {
        // Only one half decoded (span 0) — clean, not a tear.
        assert!(!is_torn_frame(&[100]));
        assert_eq!(frame_optical_spread(&[100]), Some(0));
    }

    #[test]
    fn undecodable_frame_has_no_spread_and_is_not_torn() {
        assert_eq!(frame_optical_spread(&[]), None);
        assert!(!is_torn_frame(&[]));
    }

    #[test]
    fn two_generations_in_one_frame_is_torn() {
        // A scanout tear captured gen G (even 100, odd 101) AND gen G+1 (even 102, odd 103):
        // span = 103-100 = 3 > VERNIER_MAX_SPREAD -> TORN.
        assert!(is_torn_frame(&[100, 101, 102, 103]));
        assert_eq!(frame_optical_spread(&[100, 101, 102, 103]), Some(3));
        // Minimal tear: span exactly 2 (one generation step beyond the even/odd pair).
        assert!(is_torn_frame(&[100, 102]));
        assert_eq!(frame_optical_spread(&[100, 102]), Some(2));
    }

    #[test]
    fn window_all_healthy_is_unproven_zero_tears() {
        let frames = vec![vec![100, 101], vec![102, 103], vec![104], vec![]];
        let s = window_tear_stats(&frames);
        assert_eq!(s.decodable_frames, 3, "undecodable frame excluded");
        assert_eq!(s.tear_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.max_spread, 1);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn window_with_a_tear_is_observed() {
        let frames = vec![vec![100, 101], vec![102, 103, 104, 105], vec![106, 107]];
        let s = window_tear_stats(&frames);
        assert_eq!(s.decodable_frames, 3);
        assert_eq!(s.tear_frames, 1);
        assert_eq!(s.max_spread, 3);
        assert!((s.tear_fraction - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(s.viability, TearSignalViability::Observed);
        assert!(
            !tear_gate_pass(&s),
            "a nonzero tear fraction fails the (report-only) gate"
        );
    }

    #[test]
    fn empty_window_is_unproven_and_passes() {
        let s = window_tear_stats(&[]);
        assert_eq!(s.decodable_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn report_only_seam_is_disarmed() {
        assert!(!gates_overall_pass(), "issue 781 ships report-only");
    }
}
