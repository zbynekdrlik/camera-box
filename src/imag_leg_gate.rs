//! issue 798 (path A) → #1142 STRICT flip — the imag-leg recording verdict is now SPLIT into a
//! BLOCKING presence/verification term and a REPORT-ONLY per-frame content term.
//!
//! ## Why this exists
//!
//! The imag leg's frame-by-frame zero-loss verdict is computed by `node_verdict_for_imag` in
//! `recording-verdict.rs`: the cam2 OPTICAL tick contiguity (the #580v2 beat-aware verdict) ANDed
//! with imag's own digital corner-burn contiguity (`imag_burn_ok`, issue 463) and the cam2 optical
//! undecodable moiré floor (#376), plus its analyzed-span floor (#373). Under issue 798 path A the
//! WHOLE term folded REPORT-ONLY (`gates_overall_pass()==false`) because an imag partial reached
//! the merge in 0/76 runs — there was zero green distribution to gate against.
//!
//! ## #1142 — the owner mandate + the #1130 observer effect refine the flip (do NOT flip blind)
//!
//! Owner mandate (2026-08-19): flip the report-only seams BLOCKING so visual misery can never hide
//! behind a green gate again. But #1130 comment 5347311707 proved the imag ~19.5%
//! `imag_optical_stuck_density` (and the overlapping 19.21% send-burn Δ0) is an **OBSERVER EFFECT**:
//! the E2E x264 software encode starves the imag iGPU (package PL1 30W clamp forces it below its
//! pinned 1400 MHz floor) past the 16.7 ms graphics-thread budget, so OBS repeats whole RENDERS —
//! ONLY while the recording runs (idle baseline ~0.1%, `lagged=0`). It is "churn, not loss"
//! (`avg_step` 1.006, +0.6% surplus). So EVERY imag PER-FRAME term (optical beat AND digital-burn
//! contiguity) is confounded by the recorder's own load; gating them now would false-red every run
//! on x264 load, not on the delivery chain.
//!
//! ### The split (#1142)
//!
//! - **PRESENCE / VERIFICATION → BLOCKING** ([`gates_overall_pass`] flips to `true`): the terms
//!   that are honest signals of a real full-chain proof and are NOT confounded by frame-repeat —
//!   `imag_leg_verified` (an imag partial actually reached the merge; the #1118 schema-degrade
//!   drops the leg and sets `imag_leg_verified=false`, so a degraded run now REDs, not silently
//!   passes), the analyzed-span floor (#373), the cam2 optical undecodable moiré floor (#376 — a
//!   repeated decodable frame is still decodable, so this rate is NOT inflated by the repeats), and
//!   `colour_fail`. The ONE sanctioned skip is an operator-acknowledged offline imag (#1013) — see
//!   [`verified_leg_ok`].
//! - **PER-FRAME CONTENT → REPORT-ONLY** ([`content_gates_overall_pass`] returns `false`): the
//!   digital-burn contiguity (`imag_burn_ok`) and the optical-beat freeze/stuck verdict
//!   (`optical_ok`), both confounded by the observer effect. Surfaced but never reds a run.
//!
//! TODO(#1143 imag encoder fix): flip [`content_gates_overall_pass`] to `true` once the imag
//! E2E-recording encoder fix lands (VAAPI/QSV hardware encode or a cheaper x264 preset, sized so
//! imag `avg_frame_ms` stays < 16.7 ms with burns on) AND a green imag per-frame distribution
//! accumulates. Until then the per-frame terms stay report-only, with the Tier-0 tests below
//! pinning the split so a naive blind flip cannot slip in.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::e2e_latency_gate`] / [`crate::optical_floor`]: the whole `probe`
//! module is `#[cfg(feature = "probe")]`, so `recording-verdict.rs` is CI-only. These are the PURE
//! fold decisions — no probe deps — so they unit-test Tier-0 (default features). `recording-verdict`
//! (probe-gated) only CALLS these; it never re-derives a toggle.

/// #1142 — does the imag leg's PRESENCE / VERIFICATION verdict fold into `overall_pass`?
///
/// `true` since #1142 (BLOCKING): a silently-skipped imag leg (`imag_leg_verified=false` and not
/// operator-offline-acked), a schema-degraded leg (#1118, which sets `imag_leg_verified=false`), a
/// sub-floor analyzed span (#373), an above-moiré-floor cam2 undecodable rate (#376), or a colour
/// failure now REDs the run. These are honesty signals NOT confounded by the #1130 observer effect.
pub fn gates_overall_pass() -> bool {
    // #1142 — BLOCKING: the imag presence/verification terms gate overall_pass (owner mandate
    // 2026-08-19). Was `false` (issue 798 path A report-only); the per-frame content terms stay
    // report-only via `content_gates_overall_pass` below (issue 1130 observer effect).
    true
}

/// #1142 — does the imag leg's PER-FRAME CONTENT verdict (digital-burn contiguity + optical-beat
/// freeze/stuck) fold into `overall_pass`?
///
/// `false` today (REPORT-ONLY): #1130 comment 5347311707 proved the imag per-frame repetition is an
/// OBSERVER EFFECT of the E2E x264 recording starving the imag iGPU (~18–20% lagged renders ONLY
/// during the record window; idle baseline ~0.1%). Both the optical beat (`imag_optical_stuck`) and
/// the digital-burn Δ0 are confounded by it (overlap 3697/3700), so gating them now would false-red
/// every run on the recorder's own load, not on the delivery chain. Flip to `true` once the encoder
/// fix lands (VAAPI/QSV or a cheaper x264 preset sized under the 16.7 ms budget with burns on) AND a
/// green imag per-frame distribution accumulates. TODO(#1143 imag encoder fix).
pub fn content_gates_overall_pass() -> bool {
    // #1142 — REPORT-ONLY (pending the #1143 imag encoder fix): the imag per-frame content terms are
    // confounded by the imag OBS record-load observer effect, so they flow + are surfaced but never
    // red a run. Flip to `true` (blocking) when the encoder fix lands + green per-frame runs exist.
    false
}

/// #1142 — the imag PRESENCE gate's verified/offline-ack decision: is the imag leg's PRESENCE
/// acceptable this run? `true` iff an imag partial actually reached the merge (`verified`) OR imag
/// was operator-acknowledged offline (`offline_acked`, #1013 — the ONE sanctioned skip: an absent
/// leg is EXPECTED and must not red). A run that silently skipped imag (verified=false and NOT
/// acked) — the "hidden partial" the "ONE full test, no partials" doctrine (#798) bans — fails.
pub fn verified_leg_ok(verified: bool, offline_acked: bool) -> bool {
    verified || offline_acked
}

/// Pure fold: does an outcome (`node_ok`) pass `overall_pass`, given whether the seam is live
/// (`gates_overall`)? Shared by BOTH the presence ([`gates_overall_pass`]) and content
/// ([`content_gates_overall_pass`]) seams:
/// - report-only (`gates_overall == false`): ALWAYS passes (a failing term never reds a run);
/// - blocking (`gates_overall == true`): passes iff `node_ok`.
pub fn fold(node_ok: bool, gates_overall: bool) -> bool {
    node_ok || !gates_overall
}

/// Call-site helper: fold a PRESENCE/VERIFICATION outcome against the LIVE presence seam
/// ([`gates_overall_pass`], BLOCKING since #1142). `recording-verdict.rs` calls exactly this and
/// never re-derives the toggle, so the whole decision is Tier-0 verifiable despite the probe gate on
/// that binary.
pub fn folds_into_overall_pass(node_ok: bool) -> bool {
    fold(node_ok, gates_overall_pass())
}

/// #1142 — call-site helper: fold a PER-FRAME CONTENT outcome against the REPORT-ONLY content seam
/// ([`content_gates_overall_pass`]). Mirrors [`folds_into_overall_pass`] but for the report-only
/// per-frame terms (digital-burn contiguity + optical-beat). A no-op today (never reds a run) until
/// the #1143 imag encoder fix lets the content seam flip blocking.
pub fn content_folds_into_overall_pass(node_ok: bool) -> bool {
    fold(node_ok, content_gates_overall_pass())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_seam_is_blocking_since_1142() {
        // #1142 — the imag PRESENCE/VERIFICATION seam now gates overall_pass (owner mandate
        // 2026-08-19): a silently-skipped / schema-degraded / sub-floor-span / above-undecodable
        // imag leg reds the run. Was report-only (issue 798 path A); this is the intended flip.
        assert!(
            gates_overall_pass(),
            "#1142: the imag presence/verification seam must be BLOCKING (gates_overall_pass()==true)"
        );
    }

    #[test]
    fn content_seam_stays_report_only_pending_the_1130_encoder_fix() {
        // #1142 — the imag PER-FRAME CONTENT seam (burn contiguity + optical beat) stays
        // report-only: #1130 comment 5347311707 proved those terms are confounded by the E2E x264
        // record-load observer effect (~18-20% lagged renders only during the record window). A
        // blind flip to blocking here would false-red every run on the recorder's own load. Flip
        // to true only once the encoder fix lands + a green per-frame distribution accumulates.
        assert!(
            !content_gates_overall_pass(),
            "#1142: the imag per-frame content seam must stay REPORT-ONLY (false) pending the #1143 imag encoder fix"
        );
    }

    #[test]
    fn presence_fold_reds_a_failing_presence_term() {
        // Blocking presence seam: a failing presence term (e.g. an above-moiré-floor undecodable
        // rate, or a sub-floor span) reds the run; a passing one passes.
        assert!(
            !folds_into_overall_pass(false),
            "#1142 blocking presence: a failing presence term reds the run"
        );
        assert!(folds_into_overall_pass(true));
        // …and the pure fold pins BOTH seam states explicitly.
        assert!(!fold(false, true), "blocking: failing term reds");
        assert!(fold(true, true), "blocking: passing term passes");
    }

    #[test]
    fn content_fold_never_reds_a_run_even_when_the_per_frame_term_fails() {
        // Report-only content seam: a FAILING per-frame content term (burn/beat) still passes
        // overall — surfaced, never red — because the observer effect confounds it.
        assert!(
            content_folds_into_overall_pass(false),
            "#1142 report-only content: a failing per-frame term must NOT red the run"
        );
        assert!(content_folds_into_overall_pass(true));
        assert!(fold(false, false), "report-only: failing term passes");
    }

    #[test]
    fn verified_leg_ok_reds_a_silent_skip_but_exempts_an_offline_ack() {
        // #1142 — imag_leg_verified now blocks. A run that silently skipped imag (verified=false,
        // NOT acked) — the "hidden partial" #798 bans — fails; a genuinely present leg passes.
        assert!(verified_leg_ok(true, false), "a present imag leg passes");
        assert!(
            !verified_leg_ok(false, false),
            "#1142: a silently-skipped imag leg (verified=false, not acked) reds the run"
        );
        // The ONE sanctioned skip (#1013): an operator-acknowledged offline imag — an absent leg is
        // EXPECTED, so it must NOT red, verified or not.
        assert!(
            verified_leg_ok(false, true),
            "#1013: an operator-offline-acked absent imag leg is the ONE sanctioned skip — no red"
        );
        assert!(verified_leg_ok(true, true), "present + acked also passes");
    }
}
