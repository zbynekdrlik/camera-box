//! issue 1144 — SWITCH-IN TRANSIENT classifier (REPORT-ONLY attribution for the imag content gate).
//!
//! ## What this is
//!
//! The all-cambox per-segment sweep gates each imag ~28 s window with the honest #580v2 content gate
//! (burn-id contiguity + optical beat). A **switch-in transient** is an imag burn-loss burst right
//! after the active program camera changes: for ~10 s the imag corner-burn ids are missing (and the
//! optical tick jitters), THEN it recovers and the rest of the window is clean. The real example
//! (verdict-276174336, CAM3 window 1): `burn_first_id=146197`, the FIRST missing id is at offset 1
//! (right at the boundary), 320 of 326 missing ids sit in the dense leading block 146198–146781
//! (~35 % of the 1673-id span, ~55 % missing), the body is clean but for 6 trailing ids,
//! `optical_avg_step=1.010`, `optical_stuck_density=0.201` (≈ the 326 missing burns),
//! `optical_max_stuck_run=5`, `undecodable=0`. Strih's own CAM3 window is 0/0 — so this is purely an
//! imag-branch artifact, NOT a delivery loss. It is sporadic (0 of 3 healthy runs reproduced it),
//! which is why a blind boundary trim (issue 1144 branch a) would wrongly cut healthy windows too and
//! HIDE real leading loss — branch (b) attributes the transient to the cold-cut measurement instead.
//! (The precise imag-side mechanism is UNVERIFIED — the imag box is routed once and "never
//! scene-switched", #462, so this is a leading burn-loss burst correlated with the program cut, not a
//! confirmed NDI-receiver spin-up. The classifier keys on the observed SHAPE, not the cause.)
//!
//! ## The gate stays REPORT-ONLY
//!
//! `imag_leg_gate::content_gates_overall_pass()` is `false` today, so an excused segment changes NO
//! blocking outcome — this is preparatory for the later blocking flip (issue 1144 item 2, a deliberate
//! sick-camera rig run, supervisor scope). The classifier is deliberately CONSERVATIVE / FAIL-CLOSED:
//! any criterion failing leaves the segment a content failure (the cheap direction while report-only).
//! Only ONE real positive example exists (n=1); the thresholds are calibrated TIGHTLY around it + the
//! 38 healthy zero-loss segments (a review found the first-cut thresholds excused shapes far from the
//! positive — a hard freeze, a small drop, a periodic loss, a total burn blackout, an independent
//! optical stuck — so the constants are now a BAND around the positive, not loose floors). The later
//! flip MUST re-validate against the item-2 sick-camera run that a mid-window / sustained / frozen /
//! blackout fault is NOT excused, and re-confirm the positive's real values before pinning the band.

/// Within-burst grouping distance AND the burst-vs-residual separation, in burn ids. The real
/// transient's internal gaps are <= 7 ids while the gap to any trailing residual is hundreds, so 60
/// (~1 s at 60 fps with burn step 1) groups the spin-up burst without swallowing a later event.
pub const RECOVERY_GAP_IDS: u32 = 60;

/// The loss ONSET must sit within this many ids of the window start (the program cut). The burst
/// begins at the boundary; the real positive's first missing id is at offset 1. 60 (~1 s) gives
/// margin without admitting a mid-window burst. n=1 calibrated — re-validate at flip.
pub const ONSET_OFFSET_MAX_IDS: u32 = 60;

/// The leading burst must be SUBSTANTIAL — a genuine seconds-long transient, not a handful of real
/// drops. The real positive's burst is 320 ids; a single dropped frame (#583) is 1. 100 rejects a
/// sub-second real drop (review shape B: a 30-id contiguous drop) while sitting well below the
/// positive. n=1 calibrated — re-validate at flip.
pub const MIN_TRANSIENT_MISSING: usize = 100;

/// The leading burst must be BOUNDED to the leading region — a camera dead/flapping for the first
/// half of a window is NOT a switch-in transient. The real positive's burst ends at ~35 % of the
/// span; 0.40 gives a small margin. n=1 calibrated — re-validate at flip.
pub const MAX_TRANSIENT_SPAN_FRAC: f64 = 0.40;

/// The leading burst density (`burst_len / burst_id_span`) must sit in a BAND around the positive's
/// ~0.55 (jittery ~half-rate delivery). Below the floor is a sparse / periodic loss (review shape C:
/// every-3rd-frame, density ~0.33), a different fault. Above the ceiling is a near-total burn
/// blackout (a solid contiguous block, density ~1.0 — review shape A/blackout), which is not the
/// observed jittery shape and is left fail-closed. n=1 calibrated — re-validate at flip.
pub const BURST_DENSITY_MIN: f64 = 0.40;
/// Upper edge of the burst-density band (see [`BURST_DENSITY_MIN`]).
pub const BURST_DENSITY_MAX: f64 = 0.75;

/// After the leading burst the window must RECOVER — near-zero residual missing ids. The baseline is
/// ZERO missing across all 38 healthy segments; the real positive has 6 trailing residual ids. 6 is
/// the tightest bound that still accepts the positive (a review noted a looser bound masks that many
/// unrelated real drops appended after a transient). n=1 calibrated — re-validate at flip.
pub const MAX_RESIDUAL_AFTER_BURST: usize = 6;

/// `avg_step` guards optical DRIFT only (not a localized freeze): a net-collapsed / net-advancing leg
/// reads far from the expected step. Healthy spread is +-0.0012, the positive is +0.010; 0.05 is a
/// coarse guard. A localized freeze that CATCHES UP leaves `avg_step ≈ 1` and is caught by
/// [`MAX_STUCK_RUN_ALLOWED`] instead, not this term.
pub const AVG_STEP_DEV_MAX: f64 = 0.05;

/// The optical must not STALL in a long run — a hard freeze/blackout at the cut (review shape A) has
/// a `max_stuck_run` of hundreds even while `avg_step ≈ 1` (it catches up), whereas the positive's
/// jittery half-rate has `max_stuck_run = 5`. 15 (3x the positive) rejects a freeze while accepting
/// the positive. n=1 calibrated — re-validate at flip.
pub const MAX_STUCK_RUN_ALLOWED: u32 = 15;

/// The optical stuck must be EXPLAINED by the burn transient (each transient frame that is repeated
/// is also a missing burn), not an independent freeze. `stuck_density * span_frames` (~ stuck frames)
/// must not exceed the missing-burn count by more than this fraction. The real positive: ~338 stuck
/// frames vs 326 missing burns (ratio 1.037). 0.10 rejects an independent stuck stacked on the burst
/// (review shape E) while accepting the positive. n=1 calibrated — re-validate at flip.
pub const STUCK_VS_MISSING_TOL: f64 = 0.10;

/// The classification of one imag content-gate segment. `is_transient == true` means the segment's
/// content failure is a switch-in transient (attributed to the cold-cut measurement, excused from the
/// REPORT-ONLY content fold); every other field is the measured extent, surfaced for attribution so
/// the transient is never silently dropped. A `false` verdict carries the first failing `reason`.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchInTransient {
    /// Is this segment's content failure a switch-in transient (excused from the content gate)?
    pub is_transient: bool,
    /// The first criterion that failed (or the positive label) — surfaced in the segment JSON.
    pub reason: &'static str,
    /// `min(missing_ids) - first_id` — how far into the window the loss begins (0/1 = at the cut).
    pub first_offset: u32,
    /// Number of missing ids in the leading burst (the maximal prefix run within `RECOVERY_GAP_IDS`).
    pub burst_len: usize,
    /// `burst_end_id - first_id` — how far into the window the burst reaches.
    pub burst_end_offset: u32,
    /// Missing ids after the leading burst (the residual — must be near-zero for a genuine recovery).
    pub residual: usize,
    /// `burst_len / burst_id_span` — the burst's local missing density.
    pub burst_density: f64,
}

impl Default for SwitchInTransient {
    fn default() -> Self {
        SwitchInTransient {
            is_transient: false,
            reason: "not evaluated",
            first_offset: 0,
            burst_len: 0,
            burst_end_offset: 0,
            residual: 0,
            burst_density: 0.0,
        }
    }
}

/// Classify whether one imag content-gate segment's failure is a SWITCH-IN TRANSIENT.
///
/// `missing_ids` MUST be sorted ascending (as `BurnStepContiguity::missing_ids` already is). The
/// verdict is a CONJUNCTION of criteria (fail-closed — the first failing one names the `reason`):
/// cut-adjacent window; burns present + nothing undecodable; optical not drifting AND not stalling in
/// a long run; loss onset at the cut; a substantial + bounded + BAND-dense leading burst; a clean
/// recovery after it; and the optical stuck explained by (not exceeding) the burn transient. See the
/// module docstring for the calibration and the flip-time re-validation requirement.
///
/// `first_id`/`last_id` are the DECODED burn range (as `BurnStepContiguity` reports). A total-black
/// onset with zero decodes before the first lock is invisible here (no missing ids in the pre-lock
/// gap) — that shape is handled by the span / undecodable terms, not this classifier.
#[allow(clippy::too_many_arguments)]
pub fn classify(
    first_id: Option<u32>,
    last_id: Option<u32>,
    missing_ids: &[u32],
    undecodable: u32,
    burn_present_ok: bool,
    avg_step: f64,
    expected_step: u32,
    stuck_density: f64,
    max_stuck_run: u32,
    span_frames: u32,
    cut_adjacent: bool,
) -> SwitchInTransient {
    let mut out = SwitchInTransient::default();

    let (first, last) = match (first_id, last_id) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            out.reason = "no decoded burn range";
            return out;
        }
    };
    if missing_ids.is_empty() {
        out.reason = "no burn loss";
        return out;
    }
    if !cut_adjacent {
        out.reason = "window not cut-adjacent";
        return out;
    }
    if !burn_present_ok {
        out.reason = "burn not present";
        return out;
    }
    if undecodable != 0 {
        out.reason = "undecodable frames present";
        return out;
    }
    if (avg_step - expected_step as f64).abs() > AVG_STEP_DEV_MAX {
        out.reason = "optical drift (avg_step off expected)";
        return out;
    }
    if max_stuck_run > MAX_STUCK_RUN_ALLOWED {
        out.reason = "optical stuck run too long (freeze, not a transient)";
        return out;
    }
    let span = last.saturating_sub(first);
    if span == 0 {
        out.reason = "degenerate id span";
        return out;
    }

    let onset = missing_ids[0].saturating_sub(first);
    out.first_offset = onset;
    if onset > ONSET_OFFSET_MAX_IDS {
        out.reason = "loss onset not at the window start";
        return out;
    }

    // Leading burst = the maximal prefix run of missing ids whose consecutive gap is <= RECOVERY_GAP.
    let mut burst_len = 1usize;
    for w in missing_ids.windows(2) {
        if w[1].saturating_sub(w[0]) <= RECOVERY_GAP_IDS {
            burst_len += 1;
        } else {
            break;
        }
    }
    let burst_end = missing_ids[burst_len - 1];
    let burst_id_span = burst_end.saturating_sub(missing_ids[0]).saturating_add(1);
    out.burst_len = burst_len;
    out.burst_end_offset = burst_end.saturating_sub(first);
    out.residual = missing_ids.len() - burst_len;
    out.burst_density = burst_len as f64 / burst_id_span as f64;

    if burst_len < MIN_TRANSIENT_MISSING {
        out.reason = "leading burst too small (a few drops, not a transient)";
        return out;
    }
    if out.burst_end_offset as f64 > MAX_TRANSIENT_SPAN_FRAC * span as f64 {
        out.reason = "transient region exceeds the leading bound";
        return out;
    }
    if out.burst_density < BURST_DENSITY_MIN {
        out.reason = "leading burst not dense enough (sparse / periodic loss)";
        return out;
    }
    if out.burst_density > BURST_DENSITY_MAX {
        out.reason =
            "leading burst too dense (near-total blackout, not the observed jittery shape)";
        return out;
    }
    if out.residual > MAX_RESIDUAL_AFTER_BURST {
        out.reason = "does not recover (residual loss after the burst)";
        return out;
    }
    let stuck_frames = stuck_density * span_frames as f64;
    if stuck_frames > missing_ids.len() as f64 * (1.0 + STUCK_VS_MISSING_TOL) {
        out.reason = "optical stuck exceeds the burn transient (independent stuck)";
        return out;
    }

    out.is_transient = true;
    out.reason =
        "switch-in transient (leading burn-loss burst that recovers after the program cut)";
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_STEP: u32 = 1;
    // The real positive's optical max_stuck_run (verdict-276174336 CAM3 window 1).
    const POSITIVE_MAX_STUCK_RUN: u32 = 5;

    /// Build the real CAM3-window-1 switch-in transient shape (verdict-276174336): a jittery
    /// (~half-rate) leading burst of ~320 ids starting at the cut, then a clean body but for 6
    /// trailing residual ids. Total 326 missing, matching the real run.
    fn positive_missing(first: u32) -> Vec<u32> {
        let mut m: Vec<u32> = ((first + 1)..=(first + 639)).step_by(2).collect(); // 320 ids, density ~0.5
        m.extend([
            first + 1450,
            first + 1460,
            first + 1470,
            first + 1480,
            first + 1490,
            first + 1500,
        ]);
        m
    }

    #[test]
    fn real_switch_in_transient_is_classified() {
        let first = 146197;
        let last = first + 1673;
        let m = positive_missing(first);
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,     // undecodable
            true,  // burn_present_ok
            1.010, // avg_step
            EXPECTED_STEP,
            0.201, // stuck_density
            POSITIVE_MAX_STUCK_RUN,
            1682, // span_frames
            true, // cut_adjacent
        );
        assert!(
            sit.is_transient,
            "issue 1144: the real CAM3-w1 transient must classify as a switch-in transient: {sit:?}"
        );
        assert_eq!(sit.first_offset, 1);
        assert!(sit.burst_len >= MIN_TRANSIENT_MISSING);
        assert_eq!(sit.residual, 6);
        assert!(sit.burst_density >= BURST_DENSITY_MIN && sit.burst_density <= BURST_DENSITY_MAX);
    }

    fn healthy(first: u32, last: u32, m: &[u32], stuck: f64, run: u32) -> SwitchInTransient {
        classify(
            Some(first),
            Some(last),
            m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            stuck,
            run,
            1682,
            true,
        )
    }

    #[test]
    fn healthy_zero_loss_is_not_transient() {
        let sit = healthy(1000, 2673, &[], 0.005, 1);
        assert!(!sit.is_transient);
        assert_eq!(sit.reason, "no burn loss");
    }

    #[test]
    fn single_frame_drop_is_not_transient() {
        // #583 shape: one dropped frame near the window start — a REAL loss, must NOT be excused.
        let sit = healthy(1000, 2673, &[1015], 0.005, 1);
        assert!(
            !sit.is_transient,
            "a single real drop must stay a content failure: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "leading burst too small (a few drops, not a transient)"
        );
    }

    #[test]
    fn small_hard_drop_at_cut_is_not_transient() {
        // Review shape B: a ~0.5 s hard contiguous drop at the cut (30 ids) — 10x smaller than the
        // positive's 320-id burst. Must NOT be excused.
        let m: Vec<u32> = (1001..=1030).collect();
        let sit = healthy(1000, 1000 + 1673, &m, 0.02, 1);
        assert!(
            !sit.is_transient,
            "a small hard drop must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "leading burst too small (a few drops, not a transient)"
        );
    }

    #[test]
    fn hard_freeze_at_cut_is_not_transient() {
        // Review shape A: a ~10 s hard freeze at the cut — a long contiguous burst whose optical
        // STALLED (large max_stuck_run) even though avg_step catches up. The max-stuck-run cap must
        // reject it. The mandate: a frozen leg must NOT be excused.
        let first = 146197u32;
        let m: Vec<u32> = ((first + 1)..=(first + 600)).step_by(2).collect(); // 300 ids
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.0, // net catches up
            EXPECTED_STEP,
            0.30,
            600, // a long optical stall
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a hard freeze at the cut must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "optical stuck run too long (freeze, not a transient)"
        );
    }

    #[test]
    fn total_burn_blackout_is_not_transient() {
        // A solid contiguous burn blackout at the cut (density ~1.0) — denser than the observed
        // jittery ~0.55 shape. The density ceiling must reject it (fail-closed on an unobserved
        // shape).
        let first = 146197u32;
        let m: Vec<u32> = ((first + 1)..=(first + 600)).collect(); // contiguous, density ~1.0
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.30,
            3, // optical advancing (short stall runs)
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a total burn blackout must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "leading burst too dense (near-total blackout, not the observed jittery shape)"
        );
    }

    #[test]
    fn half_window_dead_camera_is_not_transient() {
        // Dense loss through the first ~60 % of the span — the bounded check must reject it.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 1)..=(first + 1000)).step_by(2).collect();
        let sit = healthy(first, last, &m, 0.30, 3);
        assert!(
            !sit.is_transient,
            "a half-window-dead camera must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "transient region exceeds the leading bound");
    }

    #[test]
    fn mid_window_burst_is_not_transient() {
        // A dense burst that starts at ~40 % of the span — onset not at the cut.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 700)..=(first + 900)).step_by(2).collect();
        let sit = healthy(first, last, &m, 0.12, 1);
        assert!(
            !sit.is_transient,
            "a mid-window burst must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "loss onset not at the window start");
    }

    #[test]
    fn periodic_sparse_loss_is_not_transient() {
        // Review shape C: every-3rd-frame periodic loss over the leading region (density ~0.33) — a
        // bandwidth/cadence fault, not a jittery transient. The density floor must reject it.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 1)..=(first + 660)).step_by(3).collect(); // density ~0.33
        let sit = healthy(first, last, &m, 0.10, 2);
        assert!(
            !sit.is_transient,
            "a periodic sparse loss must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "leading burst not dense enough (sparse / periodic loss)"
        );
    }

    #[test]
    fn net_frozen_leg_is_not_transient() {
        // avg_step far from expected — the drift guard rejects it before any burst analysis.
        let first = 146197u32;
        let m = positive_missing(first);
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.6, // net drift
            EXPECTED_STEP,
            0.201,
            POSITIVE_MAX_STUCK_RUN,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a net-drifting leg must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "optical drift (avg_step off expected)");
    }

    #[test]
    fn independent_optical_stuck_is_not_transient() {
        // The positive burst PLUS extra independent optical stuck NOT explained by the missing burns
        // (review shape E). The stuck-consistency term (tol 0.10) must reject it.
        let first = 146197u32;
        let m = positive_missing(first); // 326 missing
                                         // stuck_density * span_frames = 0.40 * 1682 = 673 stuck frames >> 326 * 1.10 = 358.6.
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.010,
            EXPECTED_STEP,
            0.40, // an independent optical stuck stacked on the transient
            POSITIVE_MAX_STUCK_RUN,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "independent optical stuck must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "optical stuck exceeds the burn transient (independent stuck)"
        );
    }

    #[test]
    fn double_burst_is_not_transient() {
        // A leading burst that recovers, then a SECOND burst later in the window — does not recover.
        let first = 1000u32;
        let last = first + 1673;
        let mut m: Vec<u32> = ((first + 1)..=(first + 400)).step_by(2).collect();
        m.extend(((first + 900)..=(first + 1100)).step_by(2)); // second burst, ~100 residual
        let sit = healthy(first, last, &m, 0.30, 3);
        assert!(
            !sit.is_transient,
            "a double burst must not be excused: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "does not recover (residual loss after the burst)"
        );
    }

    #[test]
    fn not_cut_adjacent_is_not_transient() {
        let first = 146197u32;
        let m = positive_missing(first);
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.010,
            EXPECTED_STEP,
            0.201,
            POSITIVE_MAX_STUCK_RUN,
            1682,
            false, // not cut-adjacent (a consecutive same-cambox window)
        );
        assert!(
            !sit.is_transient,
            "a non-cut-adjacent window must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "window not cut-adjacent");
    }

    #[test]
    fn undecodable_present_is_not_transient() {
        let first = 146197u32;
        let m = positive_missing(first);
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            3, // undecodable frames present
            true,
            1.010,
            EXPECTED_STEP,
            0.201,
            POSITIVE_MAX_STUCK_RUN,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "undecodable frames present must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "undecodable frames present");
    }
}
