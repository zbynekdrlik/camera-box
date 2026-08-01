//! #930 — lipsync cross-validation gate: does the QR/QPSK A/V-sync calibration agree with what a
//! NEURAL lipsync detector (SyncNet, the #917 per-shift distance-curve engine) sees on a real
//! talking-face clip?
//!
//! WHY (the failure this prevents — user, issue 930): the rig's `--av-sync` calibration measures
//! offset from cam2's dual-QR video tick vs its QPSK audio marker. An OPERATOR during a real
//! event has no QR/QPSK to look at — they judge sync by eye, watching real faces and real speech.
//! If the two methods silently disagree, the operator would "correct" sync during a live event to
//! a DIFFERENT reference point than the QR/QPSK calibration already set. This module is the PURE
//! decision kernel that catches that disagreement: given SyncNet's aggregated offset (from
//! `scripts/av_sync_measure.py` + `scripts/av_sync_calibrate.py --calibrate`, run on a
//! lipsync-test-mode recording — see `scripts/lipsync-test-mode.sh` / `scripts/lipsync-asset.sh`)
//! and the QR/QPSK offset (from `recording-verdict --av-sync`, run on a PAIRED TEST-mode
//! recording of the SAME rig state), decide whether they agree within [`LIPSYNC_CROSS_CHECK_TOLERANCE_MS`].
//!
//! No probe deps, so this unit-tests Tier-0 (default features) — the project's CLAUDE.md "Local
//! Build Policy" mandate (a pure seam at the crate root, mirroring `av_window.rs` / `optical_floor.rs`).
//! `src/bin/recording-verdict.rs`'s `run_av_sync` (the `--av-sync` CLI mode, itself probe-gated)
//! is the ONLY caller — it already reports the QR/QPSK offset for one recording (exactly "the
//! QR/QPSK-measured offset from a paired run" issue 930's own scope item 4 names), so a new
//! optional `--syncnet-offset-ms` input there is the natural home for this cross-check. Rejected:
//! folding it into the fused `all_cambox_av_sync` verdict (`av_window.rs`) — that path decodes
//! PER-CAMERA offsets from ONE recording's own QR/QPSK data; this cross-check inherently compares
//! TWO SEPARATE runs (a lipsync-mode recording vs a QR/QPSK TEST-mode recording), which has no
//! natural per-camera home in a fused single-run verdict.
//!
//! ## [`gates_overall_pass`] — REPORT-ONLY from day one (issue 930, 2026-08-01)
//!
//! Unlike `optical_floor`/`window_gate` (which started STRICT and were later relaxed), this gate
//! term is born report-only: [`evaluate`] is still computed, still fully reported (both offsets,
//! the delta, the tolerance, the verdict), but the CALLER never folds it into any pass/fail
//! exit code yet — mirrors the exact seam shape issue 915/889/861 already established
//! (`crate::optical_floor::gates_overall_pass`). Restore path: once a real paired-run evidence
//! set (the SUPERVISOR's job, not this ticket's — see the ticket's own scope boundary) shows N
//! consecutive clean agreements, flip this to `true`.

/// The cross-check tolerance (ms), justified as the sum of THREE independently-documented
/// terms — never invented, per issue 930's own acceptance criterion ("tolerance derived from
/// SyncNet's measured resolution, not invented"):
///
/// 1. **~20ms SyncNet SEM-shrunk residual.** `scripts/av_sync_calibrate.py`'s own doc comment
///    (`aggregate_syncnet_windows`, issue 805) documents that quantization noise (SyncNet's
///    native ±40ms/frame @25fps, sub-frame-refined via `parabolic_subframe_offset`) "šum
///    +/-40ms sa priemerom zráža na +/-10-20ms" once averaged over ≥2 confident windows — the
///    conservative (wider) end of that documented range is used here.
/// 2. **~10ms QR/QPSK cluster-measurement precision.** The QPSK marker is a digital correlation
///    detector (`crate::qpsk_marker::cluster_offset_ms`), tighter than SyncNet's neural estimate
///    in practice, but budgeted non-zero here rather than assumed perfect.
/// 3. **~20ms lipsync-test playback-path skew budget.** issue 930's own new player
///    (`scripts/lipsync-test-mode.sh`, one ffmpeg process feeding both `/dev/fb0` and ALSA from a
///    single demux/decode timeline) — one video frame at the test clip's own frame rate, plus
///    fbdev/ALSA buffering margin. A live 2s sanity test on cam2 showed near-real-time pacing
///    (speed≈0.949x) with no observed catch-up drift, but only a short clip was exercised, so a
///    full frame + margin is kept rather than assuming zero.
///
/// Sum: 20 + 10 + 20 = 50ms.
pub const LIPSYNC_CROSS_CHECK_TOLERANCE_MS: f64 = 50.0;

/// Report-only seam (issue 930, mirrors `crate::optical_floor::gates_overall_pass` exactly): the
/// pure decision in [`evaluate`] is unaffected — still computed, still reported — only the
/// CALLER decides whether [`LipsyncCrossCheck::verdict`] folds into any pass/fail exit code.
/// Hardcoded `false` until a real paired-run evidence set (supervisor-driven, outside this
/// ticket's own scope) shows N consecutive clean agreements.
pub fn gates_overall_pass() -> bool {
    false
}

/// The cross-check verdict for one paired measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LipsyncCrossCheckVerdict {
    /// Both offsets were measured and their absolute difference is within tolerance.
    Agree,
    /// Both offsets were measured but their absolute difference exceeds tolerance.
    Disagree,
    /// Either offset is missing (no confident SyncNet windows, or no QR/QPSK cluster) — NEVER a
    /// fabricated agreement. Mirrors `av_window::AvSyncVerdict::Unknown`'s fail-closed shape:
    /// a paired run with nothing to compare proves nothing about cross-validation.
    Unknown,
}

/// One paired lipsync-vs-QR/QPSK cross-check measurement, fully reported regardless of verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LipsyncCrossCheck {
    pub syncnet_offset_ms: Option<f64>,
    pub qr_qpsk_offset_ms: Option<f64>,
    /// `|syncnet_offset_ms - qr_qpsk_offset_ms|`, `None` when either input is missing.
    pub delta_ms: Option<f64>,
    pub tolerance_ms: f64,
    pub verdict: LipsyncCrossCheckVerdict,
}

/// THE pure decision: given the SyncNet-aggregated offset and the QR/QPSK-measured offset (both
/// `video - audio`, ms, so they are directly comparable — see `av_sync_measure.py`'s and
/// `qpsk_marker`'s own doc comments for each side's sign convention), decide agreement.
///
/// Either input absent ⇒ [`LipsyncCrossCheckVerdict::Unknown`], regardless of the other — a
/// cross-check needs BOTH sides measured. The boundary `delta_ms == tolerance_ms` is inclusive
/// (Agree), matching this repo's own `av_window::av_offset_gate_pass` convention (`<=`, not `<`).
pub fn evaluate(
    syncnet_offset_ms: Option<f64>,
    qr_qpsk_offset_ms: Option<f64>,
    tolerance_ms: f64,
) -> LipsyncCrossCheck {
    let delta_ms = match (syncnet_offset_ms, qr_qpsk_offset_ms) {
        (Some(s), Some(q)) => Some((s - q).abs()),
        _ => None,
    };
    let verdict = match delta_ms {
        None => LipsyncCrossCheckVerdict::Unknown,
        Some(d) if d <= tolerance_ms => LipsyncCrossCheckVerdict::Agree,
        Some(_) => LipsyncCrossCheckVerdict::Disagree,
    };
    LipsyncCrossCheck {
        syncnet_offset_ms,
        qr_qpsk_offset_ms,
        delta_ms,
        tolerance_ms,
        verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f64 = LIPSYNC_CROSS_CHECK_TOLERANCE_MS;

    #[test]
    fn tolerance_is_the_documented_50ms_sum() {
        assert_eq!(
            TOL, 50.0,
            "20 (SyncNet SEM) + 10 (QR/QPSK precision) + 20 (playback skew)"
        );
    }

    #[test]
    fn gates_overall_pass_is_report_only_from_day_one_930() {
        assert!(
            !gates_overall_pass(),
            "issue 930: this gate term is born report-only — restore-gated on real paired-run \
             evidence, never flip silently"
        );
    }

    #[test]
    fn identical_offsets_agree() {
        let r = evaluate(Some(12.0), Some(12.0), TOL);
        assert_eq!(r.delta_ms, Some(0.0));
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Agree);
    }

    #[test]
    fn small_disagreement_within_tolerance_agrees() {
        let r = evaluate(Some(10.0), Some(-15.0), TOL); // delta = 25.0 < 50.0
        assert_eq!(r.delta_ms, Some(25.0));
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Agree);
    }

    #[test]
    fn boundary_delta_equals_tolerance_is_inclusive_agree() {
        let r = evaluate(Some(50.0), Some(0.0), TOL); // delta == tolerance exactly
        assert_eq!(r.delta_ms, Some(50.0));
        assert_eq!(
            r.verdict,
            LipsyncCrossCheckVerdict::Agree,
            "boundary must be inclusive, mirrors av_window::av_offset_gate_pass's <= convention"
        );
    }

    #[test]
    fn just_over_tolerance_disagrees() {
        let r = evaluate(Some(50.1), Some(0.0), TOL);
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Disagree);
    }

    #[test]
    fn large_disagreement_fails() {
        let r = evaluate(Some(200.0), Some(-40.0), TOL); // delta = 240
        assert_eq!(r.delta_ms, Some(240.0));
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Disagree);
    }

    #[test]
    fn missing_syncnet_offset_is_unknown_never_fabricated_pass() {
        let r = evaluate(None, Some(5.0), TOL);
        assert_eq!(r.delta_ms, None);
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Unknown);
        assert_eq!(
            r.qr_qpsk_offset_ms,
            Some(5.0),
            "still reported even though Unknown"
        );
    }

    #[test]
    fn missing_qr_qpsk_offset_is_unknown_never_fabricated_pass() {
        let r = evaluate(Some(5.0), None, TOL);
        assert_eq!(r.delta_ms, None);
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Unknown);
        assert_eq!(
            r.syncnet_offset_ms,
            Some(5.0),
            "still reported even though Unknown"
        );
    }

    #[test]
    fn both_missing_is_unknown() {
        let r = evaluate(None, None, TOL);
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Unknown);
        assert_eq!(r.delta_ms, None);
    }

    #[test]
    fn negative_offsets_use_absolute_delta() {
        // syncnet reads video-lags-audio (+), qr/qpsk reads video-leads-audio (-) by a small
        // amount -- must not silently cancel via signed subtraction without abs().
        let r = evaluate(Some(-30.0), Some(30.0), TOL); // delta = 60 > 50 tolerance
        assert_eq!(r.delta_ms, Some(60.0));
        assert_eq!(r.verdict, LipsyncCrossCheckVerdict::Disagree);
    }
}
