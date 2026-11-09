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
//! ## [`gates_overall_pass`] — born REPORT-ONLY (issue 930), LIVE since issue 1032 (9.11.2026)
//!
//! Unlike `optical_floor`/`window_gate` (which started STRICT and were later relaxed), this gate
//! term was born report-only: [`evaluate`] was computed and fully reported (both offsets, the
//! delta, the tolerance, the verdict) while the CALLER folded nothing into an exit code — the seam
//! shape of `crate::optical_floor::gates_overall_pass`. Issue 1032 made [`gates_overall_pass`]
//! DERIVED from an evidence counter, and recorded the evidence: the 7.11 green E2E against the
//! 8.11 production's SyncNet clips (27 within tolerance, none outside, issue 1032 comment
//! 6753309112), so [`RECORDED_CLEAN_PAIRED_RUNS`] = [`REQUIRED_CLEAN_PAIRED_RUNS`] and a Disagree
//! now fails the `--av-sync --syncnet-offset-ms` run. Back to report-only = set the counter below
//! the bar, with a ticket citing live evidence; never a silent revert.

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

/// How many CLEAN paired cross-check runs (SyncNet vs QR/QPSK agreeing within
/// [`LIPSYNC_CROSS_CHECK_TOLERANCE_MS`]) must be recorded before the fold goes LIVE. Issue 1032's
/// own acceptance ("N (>=5) clean paired runs"): N = 5. This is the fixed evidence BAR — never
/// bumped by a worker; it encodes the ticket's requirement.
pub const REQUIRED_CLEAN_PAIRED_RUNS: u32 = 5;

/// How many clean paired runs are recorded (issue 1032). The evidence set, linked on issue 1032
/// (comment 6753309112): green Full-path E2E run 42395687259 (7.11.2026, production-equivalent
/// A/V −1.1 ms = the direct feed's residual against its measured −46 ms leg) paired with the
/// dev2 SyncNet measurer's clips of the 8.11 SNV production (conf ≥ 4.0): 25 × 0 ms and
/// 2 × +40 ms, all 27 within [`LIPSYNC_CROSS_CHECK_TOLERANCE_MS`], none outside. Per the closing
/// procedure (issue 1032 comment 5830076613) one production's ≥ 5 clips against its own green
/// E2E are the N clean paired runs, so the counter sits at [`REQUIRED_CLEAN_PAIRED_RUNS`] and
/// [`gates_overall_pass`] is `true` with NO consumer edit. Pairs across a rig change are not
/// evidence (1.11 against the 30.10 E2E: the stream pin moved one frame in between). Lower it
/// only with a ticket citing live evidence that the two methods no longer agree.
pub const RECORDED_CLEAN_PAIRED_RUNS: u32 = 5;

/// Pure: is the fold EARNED at `recorded_clean_paired_runs` clean paired runs? `recorded >=
/// REQUIRED`. Param-based so both directions unit-test Tier-0 without touching the module constant.
pub fn fold_is_earned(recorded_clean_paired_runs: u32) -> bool {
    recorded_clean_paired_runs >= REQUIRED_CLEAN_PAIRED_RUNS
}

/// The fold seam (issue 930, mirrors `crate::optical_floor::gates_overall_pass`): the pure
/// decision in [`evaluate`] is unaffected — still computed, still reported — only the CALLER
/// decides whether [`LipsyncCrossCheck::verdict`] folds into any pass/fail exit code. Issue 1032
/// makes this DERIVED from the recorded evidence count: `true` since [`RECORDED_CLEAN_PAIRED_RUNS`]
/// (5) reached [`REQUIRED_CLEAN_PAIRED_RUNS`] (5) on the 7.11 -> 8.11 pair, so a Disagree folds to
/// a failure of the `--av-sync --syncnet-offset-ms` run (the JSON is printed first).
pub fn gates_overall_pass() -> bool {
    fold_is_earned(RECORDED_CLEAN_PAIRED_RUNS)
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

/// THE pass decision from a measured cross-check verdict (issue 1032), for the CALLER's fold. A
/// [`LipsyncCrossCheckVerdict::Disagree`] (the two methods contradict beyond tolerance) is the ONLY
/// verdict that FAILS — exactly the ticket's "disagreement beyond tolerance fails the verdict".
/// [`Agree`](LipsyncCrossCheckVerdict::Agree) passes; [`Unknown`](LipsyncCrossCheckVerdict::Unknown)
/// also PASSES (calibration-rule gotcha #6: a `None`-measured side proves nothing about
/// disagreement — no double-jeopardy; and the real consumer path `lipsync_cross_check_for` always
/// supplies both offsets, so Unknown never arises there anyway).
pub fn lipsync_cross_check_gate_pass(verdict: LipsyncCrossCheckVerdict) -> bool {
    !matches!(verdict, LipsyncCrossCheckVerdict::Disagree)
}

/// The single decision the CONSUMER folds on (issue 1032): does this `--av-sync` cross-check fold
/// to a FAILURE? Only when the fold is EARNED at `recorded_clean_paired_runs` clean runs AND the
/// measured verdict FAILS [`lipsync_cross_check_gate_pass`]. Param-based so both directions test
/// Tier-0: fold earned + Disagree ⇒ `true`; fold not earned (recorded < 5) ⇒ always `false`
/// regardless of verdict; Agree/Unknown ⇒ `false`. The consumer passes
/// [`RECORDED_CLEAN_PAIRED_RUNS`], which reached [`REQUIRED_CLEAN_PAIRED_RUNS`] in issue 1032, so a
/// Disagree fails the run.
pub fn folds_to_failure(
    verdict: LipsyncCrossCheckVerdict,
    recorded_clean_paired_runs: u32,
) -> bool {
    fold_is_earned(recorded_clean_paired_runs) && !lipsync_cross_check_gate_pass(verdict)
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
    fn gates_overall_pass_is_live_once_the_paired_evidence_is_recorded_1032() {
        // Born report-only in issue 930; LIVE since issue 1032 recorded the 7.11 -> 8.11 pair
        // (comment 6753309112). A silent revert to report-only must fail here.
        assert!(
            gates_overall_pass(),
            "issue 1032: the fold is live — the 7.11 -> 8.11 paired evidence is recorded"
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

    // --- issue 1032: the N-clean-paired-runs fold mechanism (live once earned) ----------------

    #[test]
    fn required_clean_paired_runs_is_the_ticket_bar_of_5_1032() {
        assert_eq!(
            REQUIRED_CLEAN_PAIRED_RUNS, 5,
            "issue 1032 acceptance: N (>=5) clean paired runs"
        );
    }

    #[test]
    fn recorded_clean_paired_runs_meets_the_bar_on_the_8_11_pair_1032() {
        // 27 measurable SyncNet clips of the 8.11 production vs E2E 42395687259's A/V (-1.1 ms):
        // 27 Agree, 0 Disagree (issue 1032 comment 6753309112).
        assert_eq!(
            RECORDED_CLEAN_PAIRED_RUNS, REQUIRED_CLEAN_PAIRED_RUNS,
            "issue 1032: the recorded evidence meets the bar, recorded on the ticket"
        );
        assert!(fold_is_earned(RECORDED_CLEAN_PAIRED_RUNS));
    }

    #[test]
    fn fold_not_earned_below_required_1032() {
        assert!(!fold_is_earned(0));
        assert!(!fold_is_earned(4));
    }

    #[test]
    fn fold_earned_at_and_above_required_1032() {
        assert!(fold_is_earned(5));
        assert!(fold_is_earned(6));
        assert!(fold_is_earned(100));
    }

    #[test]
    fn gates_overall_pass_live_derived_from_count_1032() {
        // derived: RECORDED (5) >= REQUIRED (5) -> true, and a Disagree now folds to a failure.
        assert_eq!(
            gates_overall_pass(),
            fold_is_earned(RECORDED_CLEAN_PAIRED_RUNS)
        );
        assert!(gates_overall_pass());
        assert!(folds_to_failure(
            LipsyncCrossCheckVerdict::Disagree,
            RECORDED_CLEAN_PAIRED_RUNS
        ));
    }

    #[test]
    fn gate_pass_agree_passes_1032() {
        assert!(lipsync_cross_check_gate_pass(
            LipsyncCrossCheckVerdict::Agree
        ));
    }

    #[test]
    fn gate_pass_disagree_fails_1032() {
        assert!(!lipsync_cross_check_gate_pass(
            LipsyncCrossCheckVerdict::Disagree
        ));
    }

    #[test]
    fn gate_pass_unknown_passes_no_double_jeopardy_1032() {
        assert!(lipsync_cross_check_gate_pass(
            LipsyncCrossCheckVerdict::Unknown
        ));
    }

    #[test]
    fn folds_to_failure_only_when_earned_and_disagree_1032() {
        // fold earned (>=5) + disagree = the ONE failing case
        assert!(folds_to_failure(LipsyncCrossCheckVerdict::Disagree, 5));
        // fold earned but agree/unknown = pass (never folds to failure)
        assert!(!folds_to_failure(LipsyncCrossCheckVerdict::Agree, 5));
        assert!(!folds_to_failure(LipsyncCrossCheckVerdict::Unknown, 5));
        // fold NOT earned = never folds to failure, even on Disagree
        assert!(!folds_to_failure(LipsyncCrossCheckVerdict::Disagree, 0));
        assert!(!folds_to_failure(LipsyncCrossCheckVerdict::Disagree, 4));
    }

    #[test]
    fn folds_to_failure_is_live_at_the_recorded_count_1032() {
        // the consumer passes RECORDED_CLEAN_PAIRED_RUNS -> a Disagree fails the run, an Agree or
        // an Unknown never does.
        assert!(folds_to_failure(
            LipsyncCrossCheckVerdict::Disagree,
            RECORDED_CLEAN_PAIRED_RUNS
        ));
        assert!(!folds_to_failure(
            LipsyncCrossCheckVerdict::Agree,
            RECORDED_CLEAN_PAIRED_RUNS
        ));
        assert!(!folds_to_failure(
            LipsyncCrossCheckVerdict::Unknown,
            RECORDED_CLEAN_PAIRED_RUNS
        ));
    }
}
