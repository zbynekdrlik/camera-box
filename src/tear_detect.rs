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
//! ## v2 (issue 1196) — the aux Vernier tick pair makes the signal VIABLE
//!
//! The vertical tick redundancy the paragraph above calls for now exists: the painter additionally
//! blits a small aux QR pair into the bottom burn-free gaps (`crate::aux_tick` geometry; left =
//! latest EVEN tick, right = latest ODD tick, reserved `AUX_TICK_RUN_ID`, `gen_ts_ns = 0`). A
//! horizontal seam between the primary band and the aux band now yields a clean generation in EACH
//! band, so the v2 detector computes the tear span over the UNION of `(primary_ids, aux_ids)`
//! ([`frame_union_spread`]). Two report-only companion fields gate the future promotion honestly:
//! [`TearStats::aux_decode_fraction`] (did the small aux marks actually survive the lossy chain?)
//! and [`TearStats::primary_dark_aux_alive_fraction`] (a seam INSIDE the primary band corrupts both
//! primary halves while both aux marks decode — band-localized corruption vs whole-frame blur).
//!
//! ## Precondition for a LIVE gate (unchanged — still report-only)
//!
//! [`gates_overall_pass`] stays `false` until: (1) the aux marks are proven decodable through the
//! REAL chain via a mined real-captured-frame fixture (the first rig run after the painter
//! redeploy — `pattern-change-needs-decode-fixture`), (2) the signal is observed to actually fire
//! ([`TearSignalViability::Observed`]) on a known-torn calibration run, and (3) a
//! [`TEAR_FRACTION_CEILING`] + an aux-coverage floor are calibrated from real distributions
//! (`verdict-gate-seam-calibration.md`). The flip itself is one line, out of this change's scope.
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

/// The optical `frame_id` span within ONE band of a captured frame — `max - min` over the given
/// payload ids (node burns already excluded by the caller). `None` when the band carries no
/// payload at all (an undecodable band — counted elsewhere as `undecodable`, never a tear).
pub fn frame_optical_spread(optical_ids: &[u32]) -> Option<u32> {
    let min = *optical_ids.iter().min()?;
    let max = *optical_ids.iter().max()?;
    Some(max - min)
}

/// issue 1196 (v2) — the `frame_id` span over the UNION of the primary dual-QR band's ids and the
/// bottom aux tick pair's ids. This is what makes a seam BETWEEN the two bands detectable: the
/// primary pair reads gen G+1 while the aux pair still reads gen G — neither band alone spans
/// more than the Vernier adjacency, but the union does. `None` when NEITHER band decoded.
pub fn frame_union_spread(primary_ids: &[u32], aux_ids: &[u32]) -> Option<u32> {
    let min = *primary_ids.iter().chain(aux_ids).min()?;
    let max = *primary_ids.iter().chain(aux_ids).max()?;
    Some(max - min)
}

/// A captured frame is TORN when the UNION of its primary dual-QR and aux tick-pair `frame_id`s
/// spans more than the by-design even/odd adjacency ([`VERNIER_MAX_SPREAD`]) — i.e. the frame
/// carries >= 2 distinct paint generations (issue 781 within one band; issue 1196 across bands).
pub fn is_torn_frame(primary_ids: &[u32], aux_ids: &[u32]) -> bool {
    frame_union_spread(primary_ids, aux_ids).is_some_and(|s| s > VERNIER_MAX_SPREAD)
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

/// Per-window tear report (report-only). Derives only `PartialEq` (not `Eq`) — the fractions are
/// `f64` (the #726 Eq-on-f64 trap).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TearStats {
    /// EVERY frame attributed to this window, including fully undecodable ones — the denominator
    /// for the aux-coverage fractions (issue 1196: a whole-frame blur kills the aux marks too and
    /// must lower coverage honestly, so coverage is judged against ALL captured frames).
    pub total_frames: u32,
    /// In-window frames whose primary-or-aux UNION carried at least one payload (fully
    /// undecodable frames excluded — a tear is measured only where a tick decoded).
    pub decodable_frames: u32,
    /// Frames whose UNION span exceeded [`VERNIER_MAX_SPREAD`] (>= 2 paint generations captured).
    pub tear_frames: u32,
    /// `tear_frames / decodable_frames` (0.0 when no decodable frame).
    pub tear_fraction: f64,
    /// The largest union span observed in the window (0 or 1 = clean; >= 2 = a tear occurred).
    pub max_spread: u32,
    /// issue 1196 — fraction of ALL in-window frames ([`Self::total_frames`]) that decoded BOTH
    /// aux tick marks (>= 2 aux payloads; the bottom burn-gap pair). The promotion-gating
    /// coverage signal: a LIVE flip additionally requires this above a calibrated floor on the
    /// same run, so a silent aux loss demotes honestly instead of false-greening. 0.0 on pre-aux
    /// content. Known bootstrap nuance: on the painter's very first tick BOTH aux marks carry
    /// frame_id 0, so decode dedup collapses them to ONE payload and that single frame reads as
    /// not-fully-covered — one frame per painter start, irrelevant at window scale.
    pub aux_decode_fraction: f64,
    /// issue 1196 — fraction of ALL in-window frames where the PRIMARY band decoded NOTHING while
    /// BOTH aux marks decoded: band-localized corruption (e.g. a seam inside the 700px primary
    /// band, which corrupts both primary halves at the same height) as opposed to a whole-frame
    /// blur (which kills the aux marks too). Report-only discriminator; 0.0 on pre-aux content.
    pub primary_dark_aux_alive_fraction: f64,
    /// Whether the signal fired on this window (see [`TearSignalViability`]).
    pub viability: TearSignalViability,
}

/// Aggregate per-window tear stats from each in-window frame's `(primary_ids, aux_ids)` —
/// `primary_ids` = the cam2-optical dual-QR Vernier `frame_id`s (node burns already excluded by
/// the caller), `aux_ids` = the bottom aux tick pair's `frame_id`s (`AUX_TICK_RUN_ID` payloads,
/// issue 1196). Undecodable bands are passed as empty slices.
pub fn window_tear_stats(per_frame_ids: &[(Vec<u32>, Vec<u32>)]) -> TearStats {
    let total_frames = per_frame_ids.len() as u32;
    let mut decodable_frames = 0u32;
    let mut tear_frames = 0u32;
    let mut max_spread = 0u32;
    let mut aux_full_frames = 0u32;
    let mut primary_dark_aux_alive = 0u32;
    for (primary, aux) in per_frame_ids {
        if aux.len() >= 2 {
            aux_full_frames += 1;
            if primary.is_empty() {
                primary_dark_aux_alive += 1;
            }
        }
        if let Some(spread) = frame_union_spread(primary, aux) {
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
    let (aux_decode_fraction, primary_dark_aux_alive_fraction) = if total_frames > 0 {
        (
            aux_full_frames as f64 / total_frames as f64,
            primary_dark_aux_alive as f64 / total_frames as f64,
        )
    } else {
        (0.0, 0.0)
    };
    let viability = if tear_frames > 0 {
        TearSignalViability::Observed
    } else {
        TearSignalViability::Unproven
    };
    TearStats {
        total_frames,
        decodable_frames,
        tear_frames,
        tear_fraction,
        max_spread,
        aux_decode_fraction,
        primary_dark_aux_alive_fraction,
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

    /// Shorthand: a per-frame `(primary_ids, aux_ids)` pair for `window_tear_stats`.
    fn f(primary: &[u32], aux: &[u32]) -> (Vec<u32>, Vec<u32>) {
        (primary.to_vec(), aux.to_vec())
    }

    #[test]
    fn healthy_vernier_pair_is_not_torn() {
        // LEFT=even 100, RIGHT=odd 101 -> span 1 -> the by-design adjacency, NOT a tear —
        // with or without the aux pair (issue 1196) echoing the same generation.
        assert!(!is_torn_frame(&[100, 101], &[]));
        assert!(!is_torn_frame(&[101, 100], &[]));
        assert!(!is_torn_frame(&[100, 101], &[100, 101]));
        assert_eq!(frame_optical_spread(&[100, 101]), Some(1));
        assert_eq!(frame_union_spread(&[100, 101], &[100, 101]), Some(1));
    }

    #[test]
    fn single_optical_half_is_not_torn() {
        // Only one half decoded (span 0) — clean, not a tear.
        assert!(!is_torn_frame(&[100], &[]));
        assert_eq!(frame_optical_spread(&[100]), Some(0));
        assert_eq!(frame_union_spread(&[100], &[]), Some(0));
    }

    #[test]
    fn undecodable_frame_has_no_spread_and_is_not_torn() {
        assert_eq!(frame_optical_spread(&[]), None);
        assert_eq!(frame_union_spread(&[], &[]), None);
        assert!(!is_torn_frame(&[], &[]));
    }

    #[test]
    fn two_generations_in_one_frame_is_torn() {
        // A scanout tear captured gen G (even 100, odd 101) AND gen G+1 (even 102, odd 103):
        // span = 103-100 = 3 > VERNIER_MAX_SPREAD -> TORN.
        assert!(is_torn_frame(&[100, 101, 102, 103], &[]));
        assert_eq!(frame_union_spread(&[100, 101, 102, 103], &[]), Some(3));
        // Minimal tear: span exactly 2 (one generation step beyond the even/odd pair).
        assert!(is_torn_frame(&[100, 102], &[]));
        assert_eq!(frame_union_spread(&[100, 102], &[]), Some(2));
    }

    #[test]
    fn cross_band_generation_split_is_torn_1196() {
        // THE issue-1196 capability: a horizontal seam BETWEEN the primary band and the aux
        // band — the primary pair decodes gen G+1 (ticks 102/103) while the aux pair still
        // shows gen G (ticks 100/101). Neither band alone spans > 1, but the UNION does.
        assert!(!is_torn_frame(&[102, 103], &[]), "primary alone is clean");
        assert!(!is_torn_frame(&[], &[100, 101]), "aux alone is clean");
        assert!(
            is_torn_frame(&[102, 103], &[100, 101]),
            "the primary-vs-aux generation split IS the tear"
        );
        assert_eq!(frame_union_spread(&[102, 103], &[100, 101]), Some(3));
        // Minimal cross-band tear: a SINGLE aux mark one generation behind is enough —
        // union {101, 102, 103} spans 2 > VERNIER_MAX_SPREAD.
        assert!(is_torn_frame(&[102, 103], &[101]));
        assert_eq!(frame_union_spread(&[102, 103], &[101]), Some(2));
        assert!(
            is_torn_frame(&[102, 103], &[100]),
            "span 3 via one aux mark"
        );
    }

    #[test]
    fn window_all_healthy_is_unproven_zero_tears() {
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[102, 103], &[102, 103]),
            f(&[104], &[]),
            f(&[], &[]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 4, "every attributed frame is counted");
        assert_eq!(s.decodable_frames, 3, "undecodable frame excluded");
        assert_eq!(s.tear_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.max_spread, 1);
        // 2 of 4 frames decoded BOTH aux marks.
        assert!((s.aux_decode_fraction - 0.5).abs() < 1e-9);
        assert_eq!(s.primary_dark_aux_alive_fraction, 0.0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn window_with_a_tear_is_observed() {
        let frames = vec![
            f(&[100, 101], &[]),
            f(&[102, 103, 104, 105], &[]),
            f(&[106, 107], &[]),
        ];
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
    fn window_cross_band_tear_is_observed_1196() {
        // A cross-band seam frame inside an otherwise healthy window fires the signal.
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[102, 103], &[100, 101]), // primary advanced, aux one generation behind
            f(&[104, 105], &[104, 105]),
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.tear_frames, 1);
        assert_eq!(s.max_spread, 3);
        assert_eq!(s.viability, TearSignalViability::Observed);
    }

    #[test]
    fn primary_dark_aux_alive_discriminator_1196() {
        // A seam INSIDE the 700px primary band corrupts both primary halves (undecodable
        // primary) while BOTH bottom aux marks still decode — band-localized corruption, the
        // exact shape the primary-only v1 detector counted as a plain undecodable. The frame
        // is decodable via the union (aux) and NOT torn (aux span 1); the discriminator
        // fraction counts it against ALL attributed frames.
        let frames = vec![
            f(&[100, 101], &[100, 101]),
            f(&[], &[102, 103]), // primary dark, both aux alive
            f(&[], &[104]),      // primary dark, only ONE aux — NOT the discriminator shape
            f(&[], &[]),         // fully undecodable
        ];
        let s = window_tear_stats(&frames);
        assert_eq!(s.total_frames, 4);
        assert_eq!(s.decodable_frames, 3, "union-decodable: frames 0, 1, 2");
        assert_eq!(s.tear_frames, 0);
        // Frames 0 and 1 decoded both aux marks: 2/4.
        assert!((s.aux_decode_fraction - 0.5).abs() < 1e-9);
        // Only frame 1 is primary-dark with BOTH aux alive: 1/4.
        assert!((s.primary_dark_aux_alive_fraction - 0.25).abs() < 1e-9);
        assert_eq!(s.viability, TearSignalViability::Unproven);
    }

    #[test]
    fn empty_window_is_unproven_and_passes() {
        let s = window_tear_stats(&[]);
        assert_eq!(s.total_frames, 0);
        assert_eq!(s.decodable_frames, 0);
        assert_eq!(s.tear_fraction, 0.0);
        assert_eq!(s.aux_decode_fraction, 0.0);
        assert_eq!(s.primary_dark_aux_alive_fraction, 0.0);
        assert_eq!(s.viability, TearSignalViability::Unproven);
        assert!(tear_gate_pass(&s));
    }

    #[test]
    fn report_only_seam_is_disarmed() {
        assert!(!gates_overall_pass(), "issue 781/1196 ships report-only");
    }
}
