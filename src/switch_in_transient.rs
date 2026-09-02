//! issue 1144 — SWITCH-IN TRANSIENT classifier (REPORT-ONLY attribution for the imag content gate).
//!
//! ## What this is
//!
//! The all-cambox per-segment sweep gates each imag ~28 s window with the honest #580v2 content gate
//! (burn-id contiguity + optical beat). A **switch-in transient** is the imag NDI-receiver spin-up
//! right after the program cuts to a camera: for ~10 s the receiver has not locked, so its digital
//! corner-burn ids are missing (and the optical tick stalls), THEN it recovers and the rest of the
//! window is clean. The real example (verdict-276174336, CAM3 window 1): `burn_first_id=146197`, the
//! FIRST missing id is at offset 1 (right at the cut), 320 of 326 missing ids sit in the dense leading
//! block 146198–146781 (~35 % of the 1673-id span), the body is clean but for 6 trailing ids,
//! `optical_avg_step=1.010`, `stuck_density=0.201` (≈ the 326 missing burns), `undecodable=0`. Strih's
//! own CAM3 window is 0/0 — so this is purely an imag-branch spin-up transient, NOT a delivery loss.
//! It is sporadic (0 of 3 healthy runs reproduced it), which is why a blind boundary trim (branch a of
//! issue 1144) would wrongly cut healthy windows too and HIDE real leading loss — the reason branch
//! (b) attributes the transient to the cold-cut measurement instead.
//!
//! ## The gate stays REPORT-ONLY
//!
//! `imag_leg_gate::content_gates_overall_pass()` is `false` today, so an excused segment changes NO
//! blocking outcome — this is preparatory for the later blocking flip (issue 1144 item 2, a deliberate
//! sick-camera rig run, supervisor scope). The classifier is deliberately CONSERVATIVE / FAIL-CLOSED:
//! any criterion failing leaves the segment a content failure (the cheap direction while report-only).
//! Only ONE real positive example exists (n=1); the thresholds are calibrated from it + the 38 healthy
//! zero-loss segments, and the later flip MUST re-validate against the item-2 sick-camera run that a
//! mid-window / sustained / frozen fault is NOT excused.
//!
//! ## Why it lives at the crate root (default features)
//!
//! Same reasoning as `imag_leg_gate` / `tear_detect`: the whole `probe` module is
//! `#[cfg(feature = "probe")]`, so `recording-verdict.rs` is CI-only. This is the PURE shape decision
//! (no probe deps), so it unit-tests Tier-0. `recording-verdict` only CALLS `classify`; the excusal
//! (`content_pass = raw_pass || is_transient`) folds ONLY through the REPORT-ONLY content seam, never
//! the blocking presence/verification side of the #1142 split.

/// Within-burst grouping distance AND the burst-vs-residual separation, in burn ids. The real
/// transient's internal gaps are <= 7 ids while the gap to any trailing residual is hundreds, so 60
/// (~1 s at 60 fps with burn step 1) groups the spin-up burst without swallowing a later event.
pub const RECOVERY_GAP_IDS: u32 = 60;

/// The loss ONSET must sit within this many ids of the window start (the program cut). The receiver
/// spin-up begins the moment the program cuts; the real positive's first missing id is at offset 1.
/// 60 (~1 s) gives margin without admitting a mid-window burst. n=1 calibrated — re-validate at flip.
pub const ONSET_OFFSET_MAX_IDS: u32 = 60;

/// The leading burst must be SUBSTANTIAL — a genuine seconds-long spin-up, not a handful of real
/// drops. The real positive's burst is 320 ids; a single dropped frame (#583) is 1. 30 (~0.5 s) is a
/// conservative floor well below the positive and well above any few-frame delivery drop.
pub const MIN_TRANSIENT_MISSING: usize = 30;

/// The leading burst must be BOUNDED to the leading region — a camera dead/flapping for the first
/// half of a window is NOT a switch-in transient. The real positive's burst ends at ~35 % of the
/// span; 0.40 gives a small margin. n=1 calibrated — re-validate at flip.
pub const MAX_TRANSIENT_SPAN_FRAC: f64 = 0.40;

/// The leading burst must be DENSE (a real receiver-not-locked stretch drops a large fraction of
/// burns) — a sparse leading loss is a different fault. The real positive's burst density is ~0.55;
/// 0.30 separates it from a sparse-but-grouped leading loss.
pub const BURST_DENSITY_MIN: f64 = 0.30;

/// After the leading burst the window must RECOVER — near-zero residual missing ids. The baseline is
/// ZERO missing across all 38 healthy segments; the real positive has 6 trailing residual ids. 10 is
/// a tight recovery bound just above the observed residual. n=1 calibrated — re-validate at flip.
pub const MAX_RESIDUAL_AFTER_BURST: usize = 10;

/// `avg_step` guards optical DRIFT only (not a localized freeze): a genuinely collapsed/net-advancing
/// leg reads far from the expected step. Healthy spread is +-0.0012, the positive is +0.010; 0.05 is
/// a coarse guard. NOT freeze protection — the stuck-vs-missing consistency below is what ties the
/// optical stuck to the burn transient.
pub const AVG_STEP_DEV_MAX: f64 = 0.05;

/// The optical stuck must be EXPLAINED by the burn transient (each spin-up frame that is repeated is
/// also a missing burn), not an independent freeze. `stuck_density * span_frames` (~ stuck frames)
/// must not exceed the missing-burn count by more than this fraction. The real positive: ~338 stuck
/// frames vs 326 missing burns (ratio 1.03) << 1.25.
pub const STUCK_VS_MISSING_TOL: f64 = 0.25;

/// The classification of one imag content-gate segment. `is_transient == true` means the segment's
/// content failure is a switch-in transient (attributed to the cold-cut measurement, excused from the
/// REPORT-ONLY content fold); every other field is the measured extent, surfaced for attribution so
/// the transient is never silently dropped. A `false` verdict carries the first failing `reason`.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchInTransient {
    /// Is this segment's content failure a switch-in transient (excused from the content gate)?
    pub is_transient: bool,
    /// The first criterion that failed (or the positive label) — surfaced in the segment JSON note.
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
/// cut-adjacent window; burns present + nothing undecodable; optical not drifting; loss onset at the
/// cut; a substantial + bounded + dense leading burst; a clean recovery after it; and the optical
/// stuck explained by (not exceeding) the burn transient. See the module docstring for the calibration
/// and the flip-time re-validation requirement.
///
/// `first_id`/`last_id` are the DECODED burn range (as `BurnStepContiguity` reports). A total-black
/// onset with zero decodes before the first lock is invisible here (no missing ids in the pre-lock
/// gap) — that shape is handled by the span / undecodable terms, not this classifier, and is a
/// flip-time re-validation item.
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
        out.reason = "leading burst too small (a few drops, not a spin-up)";
        return out;
    }
    if out.burst_end_offset as f64 > MAX_TRANSIENT_SPAN_FRAC * span as f64 {
        out.reason = "transient region exceeds the leading bound";
        return out;
    }
    if out.burst_density < BURST_DENSITY_MIN {
        out.reason = "leading burst not dense (sparse leading loss)";
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
    out.reason = "switch-in transient (NDI receiver spin-up after program cut)";
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_STEP: u32 = 1;

    /// Build the real CAM3-window-1 switch-in transient shape (verdict-276174336): a dense every-2
    /// leading burst starting at the cut, then a clean body but for 6 trailing residual ids.
    fn positive_missing(first: u32) -> Vec<u32> {
        let mut m: Vec<u32> = ((first + 1)..=(first + 585)).step_by(2).collect(); // ~293 ids, density ~0.5
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
            1682,  // span_frames
            true,  // cut_adjacent
        );
        assert!(
            sit.is_transient,
            "issue 1144: the real CAM3-w1 spin-up burst must classify as a switch-in transient: {sit:?}"
        );
        assert_eq!(sit.first_offset, 1);
        assert!(sit.burst_len >= MIN_TRANSIENT_MISSING);
        assert_eq!(sit.residual, 6);
        assert!(sit.burst_density >= BURST_DENSITY_MIN);
    }

    #[test]
    fn healthy_zero_loss_is_not_transient() {
        let sit = classify(
            Some(1000),
            Some(2673),
            &[],
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.005,
            1682,
            true,
        );
        assert!(!sit.is_transient);
        assert_eq!(sit.reason, "no burn loss");
    }

    #[test]
    fn single_frame_drop_is_not_transient() {
        // #583 shape: one dropped frame near the window start — a REAL loss, must NOT be excused.
        let sit = classify(
            Some(1000),
            Some(2673),
            &[1015],
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.005,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a single real drop must stay a content failure: {sit:?}"
        );
        assert_eq!(
            sit.reason,
            "leading burst too small (a few drops, not a spin-up)"
        );
    }

    #[test]
    fn half_window_dead_camera_is_not_transient() {
        // Dense loss through the first ~60 % of the span — bounded check must reject it.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 1)..=(first + 1000)).step_by(2).collect();
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.30,
            1682,
            true,
        );
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
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.12,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a mid-window burst must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "loss onset not at the window start");
    }

    #[test]
    fn scattered_loss_is_not_transient() {
        // ~3 % loss spread across the whole window with big gaps — not a leading burst.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 5)..=(first + 1650)).step_by(33).collect(); // ~50 ids, gaps 33
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.03,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "scattered whole-window loss must not be excused: {sit:?}"
        );
    }

    #[test]
    fn net_frozen_leg_is_not_transient() {
        // avg_step far from expected — drift guard rejects it before any burst analysis.
        let first = 146197u32;
        let m = positive_missing(first);
        let sit = classify(
            Some(first),
            Some(first + 1673),
            &m,
            0,
            true,
            1.6,
            EXPECTED_STEP,
            0.201,
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
        // A small leading burst but a large optical stuck NOT explained by the missing burns.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 1)..=(first + 80)).step_by(2).collect(); // 40 ids
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.60,
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
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.30,
            1682,
            true,
        );
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
            1682,
            false,
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
            3,
            true,
            1.010,
            EXPECTED_STEP,
            0.201,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "undecodable frames present must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "undecodable frames present");
    }

    #[test]
    fn sparse_leading_within_gap_is_not_transient() {
        // 35 missing grouped into one leading burst (gaps 14 <= RECOVERY_GAP, within the bound) but
        // NOT dense (~0.07) — the density check must reject it even though it is substantial + bounded.
        let first = 1000u32;
        let last = first + 1673;
        let m: Vec<u32> = ((first + 1)..=(first + 477)).step_by(14).collect(); // 35 ids over a 476 span
        let sit = classify(
            Some(first),
            Some(last),
            &m,
            0,
            true,
            1.0,
            EXPECTED_STEP,
            0.01,
            1682,
            true,
        );
        assert!(
            !sit.is_transient,
            "a sparse leading loss must not be excused: {sit:?}"
        );
        assert_eq!(sit.reason, "leading burst not dense (sparse leading loss)");
    }
}
