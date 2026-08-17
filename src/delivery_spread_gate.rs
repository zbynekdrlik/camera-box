//! issue 1033 — fold the ALL-CAMBOX cross-camera DELIVERY-latency spread into the verdict, but
//! ship it REPORT-ONLY first (the data says it is not tight-green yet).
//!
//! ## Why this exists
//!
//! `all_cambox_delivery_latency.cross_camera_spread_ms` — `max(p50) − min(p50)` of every camera's
//! own `strih_burn − camera_burn` DELIVERY latency (issue 286) — is already computed + emitted by
//! `recording-verdict.rs` (`switch_latency::spread_verdict(&delivery_p50s_ms)`), with a
//! `spread_gate_pass = spread_ms <= `[`crate::switch_latency::SPREAD_THRESHOLD_MS`] (16 ms). But
//! that block, UNLIKE the source-side sweep right above it (which does `all_pass &= sv.pass`),
//! never touched `all_pass` — the metric was *computed + structurally forbidden from gating*, with
//! a test pinning the no-fold. There was no gate seam at all — not even flip-ready. Issue 1033
//! replaces that with THIS report-only seam (the fold now flows through it, one line from LIVE).
//!
//! ## Why REPORT-ONLY today — the data (issue 1033 design comment, 2026-08-17)
//!
//! Mining all 78 local `/tmp/recording-e2e-*/verdict-*.json`, the delivery spread is NOT in a
//! tight-green band: the most recent GREEN (`overall_pass=true`) runs carry delivery spreads of
//! ~66–81 ms (10+ green runs), 4–5× over the 16 ms bound. The spread is driven ENTIRELY by cam1's
//! bimodal delivery latency (healthy p50 ~47–64 ms → spread 3–22 ms; degraded p50 ~88–144 ms →
//! spread 44–98 ms) while cam2/cam3 stay tight (34–57 ms). This is the same cam1-grabber
//! (issue 909) territory that keeps [`crate::optical_floor`] / [`crate::frozen_leg`] /
//! [`crate::av_window`] report-only. Folding LIVE at 16 ms today would red the majority of recent
//! green runs — the exact anti-pattern `verdict-gate-seam-calibration.md` bans ("a bound that
//! would have failed a recent green run is wrong").
//!
//! ## The report-only / restore seam
//!
//! [`gates_overall_pass`] mirrors [`crate::imag_leg_gate::gates_overall_pass`] /
//! [`crate::optical_floor::gates_overall_pass`] / [`crate::e2e_latency_gate::gates_overall_pass`]:
//! a one-line-restorable toggle deciding whether the delivery-spread term folds into
//! `overall_pass`. It is `false` (REPORT-ONLY) today. The RESTORE CONDITION (a follow-up flips it
//! to `true`): cam1's delivery-latency lottery genuinely killed (the issue-909 grabber) AND ~5
//! consecutive green E2E runs with delivery spread ≤ ~10 ms — the exact precondition the issue-1033
//! validator named. The bound itself needs no new constant: it reuses
//! [`crate::switch_latency::SPREAD_THRESHOLD_MS`] (16 ms — half a 30fps program frame), the same
//! bound `spread_gate_pass` is already computed against.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::imag_leg_gate`] / [`crate::e2e_latency_gate`]: the whole `probe`
//! module is `#[cfg(feature = "probe")]`, so `recording-verdict.rs` is CI-only. This is the PURE
//! fold decision — no probe deps — so it unit-tests Tier-0 (default features). `recording-verdict`
//! (probe-gated) only CALLS these; it never re-derives the toggle.

/// The pinned cross-camera DELIVERY-spread bound, in milliseconds. Re-exported from
/// [`crate::switch_latency::SPREAD_THRESHOLD_MS`] (16 ms — half a 30fps program frame, issue 624)
/// so a caller/reader of THIS gate names one bound; there is no second, drifting constant.
pub const DELIVERY_SPREAD_BOUND_MS: f64 = crate::switch_latency::SPREAD_THRESHOLD_MS;

/// Does the ALL-CAMBOX delivery cross-camera-spread term fold into `overall_pass`?
///
/// `false` today (REPORT-ONLY, issue 1033): the spread flows + is surfaced but never fails a run —
/// because the current fleet data is not tight-green (cam1's delivery lottery, ~66–81 ms spreads
/// on recent green runs; issue-909 grabber class). The ONE line a follow-up flips to `true` to
/// promote it to a LIVE blocking gate once cam1's lottery is killed and ~5 consecutive green runs
/// hold delivery spread ≤ ~10 ms.
pub fn gates_overall_pass() -> bool {
    // issue 1033 — REPORT-ONLY today: the delivery-spread term flows + is surfaced but never reds
    // a run. The ONE line a follow-up flips to `true` to promote it to a LIVE blocking gate, once
    // cam1's delivery lottery is killed (issue-909 grabber) and ~5 consecutive green E2E runs hold
    // the delivery spread ≤ ~10 ms. Shipping `true` today would red the 10+ recent green runs
    // whose delivery spread sits at ~66–81 ms (the fleet is not tight-green yet).
    false
}

/// Pure fold: does a delivery-spread outcome (`spread_ok` = `spread_ms <= DELIVERY_SPREAD_BOUND_MS`)
/// pass `overall_pass`, given whether the seam is live (`gates_overall`)? Mirrors every other
/// report-only seam's `pass || !gates_overall` fold:
/// - report-only (`gates_overall == false`): ALWAYS passes (a wide spread never reds a run);
/// - blocking (`gates_overall == true`): passes iff `spread_ok`.
pub fn fold(spread_ok: bool, gates_overall: bool) -> bool {
    spread_ok || !gates_overall
}

/// Call-site helper: fold a delivery-spread outcome against the LIVE seam state
/// ([`gates_overall_pass`]). `recording-verdict.rs` calls exactly this (never re-deriving the
/// toggle), so the whole decision is Tier-0 verifiable despite the probe gate on that binary.
pub fn folds_into_overall_pass(spread_ok: bool) -> bool {
    fold(spread_ok, gates_overall_pass())
}
