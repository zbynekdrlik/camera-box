//! issue 798 (path A) → #1142 STRICT flip — the imag-leg recording verdict is now SPLIT into a
//! BLOCKING presence/verification seam and a REPORT-ONLY per-frame content seam.
//!
//! Tier-0 (default features): pins the #1142 split contract of `camera_box::imag_leg_gate`, the
//! pure fold seams `recording-verdict.rs` (probe-gated, no local type-check) calls. The RED commit
//! keeps the presence seam at the OLD report-only value while these tests already assert the new
//! blocking contract (so `presence_seam_is_blocking` FAILS); the GREEN commit flips it.

use camera_box::imag_leg_gate;

#[test]
fn presence_seam_is_blocking_since_1142() {
    // #1142 — the imag PRESENCE/VERIFICATION seam now gates overall_pass (owner mandate
    // 2026-08-19): a silently-skipped / schema-degraded / sub-floor-span / above-undecodable imag
    // leg reds the run. Was report-only under issue 798 path A.
    assert!(
        imag_leg_gate::gates_overall_pass(),
        "#1142: imag presence/verification seam must be BLOCKING (gates_overall_pass()==true)"
    );
}

#[test]
fn content_seam_stays_report_only_pending_the_1130_encoder_fix() {
    // #1142 — the imag PER-FRAME CONTENT seam (digital-burn contiguity + optical beat) stays
    // report-only: #1130 comment 5347311707 proved those terms are confounded by the E2E x264
    // record-load observer effect (~18-20% lagged renders only during the record window; idle
    // baseline ~0.1%). Flipping it blocking now would false-red every run on the recorder's own
    // load. Flip to true only once the encoder fix lands + a green per-frame distribution exists.
    assert!(
        !imag_leg_gate::content_gates_overall_pass(),
        "#1142: imag per-frame content seam must stay REPORT-ONLY (false) pending the #1143 imag encoder fix"
    );
}

#[test]
fn presence_fold_reds_a_failing_presence_term_content_fold_never_does() {
    // The presence seam is blocking, the content seam report-only — pinned via BOTH the live
    // call-site helpers AND the pure `fold` so the two seam states are locked independent of the
    // live toggles.
    assert!(
        !imag_leg_gate::fold(false, true),
        "blocking: a failing presence term reds the run"
    );
    assert!(
        imag_leg_gate::fold(true, true),
        "blocking: a passing term passes"
    );
    assert!(
        imag_leg_gate::content_folds_into_overall_pass(false),
        "report-only content: a failing per-frame term must NOT red the run"
    );
    assert!(imag_leg_gate::content_folds_into_overall_pass(true));
}

#[test]
fn verified_leg_ok_reds_a_silent_skip_but_exempts_an_offline_ack() {
    // #1142 — imag_leg_verified now blocks. A run that silently skipped imag (verified=false, NOT
    // acked) — the "hidden partial" the "ONE full test, no partials" doctrine (#798) bans — fails;
    // a genuinely present leg passes. The ONE sanctioned skip is an operator-acknowledged offline
    // imag (#1013): an absent leg is EXPECTED there, so it must not red.
    assert!(
        imag_leg_gate::verified_leg_ok(true, false),
        "a present imag leg passes"
    );
    assert!(
        !imag_leg_gate::verified_leg_ok(false, false),
        "#1142: a silently-skipped imag leg (verified=false, not acked) reds the run"
    );
    assert!(
        imag_leg_gate::verified_leg_ok(false, true),
        "#1013: an operator-offline-acked absent imag leg is the ONE sanctioned skip — no red"
    );
}
