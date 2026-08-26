//! issue 1033 — fold the ALL-CAMBOX cross-camera DELIVERY-latency spread into the verdict, but
//! ship it REPORT-ONLY first (the data says it is not tight-green yet).
//!
//! ## Why this exists
//!
//! `all_cambox_delivery_latency.cross_camera_spread_ms` — `max(p50) − min(p50)` of every camera's
//! own `strih_burn − camera_burn` DELIVERY latency (issue 286) — is already computed + emitted by
//! `recording-verdict.rs` (`switch_latency::spread_verdict(&delivery_p50s_ms)`), with a
//! `spread_gate_pass = spread_ms <= `[`crate::switch_latency::SPREAD_THRESHOLD_MS`] (24 ms since
//! issue 1120). But
//! that block, UNLIKE the source-side sweep right above it (which does `all_pass &= sv.pass`),
//! never touched `all_pass` — the metric was *computed + structurally forbidden from gating*, with
//! a test pinning the no-fold. There was no gate seam at all — not even flip-ready. Issue 1033
//! replaces that with THIS report-only seam (the fold now flows through it, one line from LIVE).
//!
//! ## Why it shipped REPORT-ONLY (issue 1033) — and why #1142 flips it BLOCKING
//!
//! Issue 1033 mined all 78 local `/tmp/recording-e2e-*/verdict-*.json` and found the delivery
//! spread NOT tight-green: recent GREEN runs carried ~66–81 ms spreads (10+ runs), ~2.7–3.4× over
//! the 24 ms bound, driven by cam1's bimodal delivery latency (the issue-909 grabber class). So it
//! shipped report-only under the standard gates-green-first philosophy ("a bound that would have
//! failed a recent green run is wrong"). #1142 (owner mandate 2026-08-19) OVERRIDES that here: those
//! "green" runs were FALSELY green — the phase lottery (a good-phase 3.97 ms vs a bad-phase 85 ms on
//! otherwise-identical runs) was hiding a real delivery-spread failure behind a green gate, which is
//! exactly the "z mesiac prace ze to vlastne nejde" the owner is furious about. The SOURCE-side
//! spread already BLOCKS at the same bound; #1142 makes the DELIVERY side block too, so a bad-phase
//! run REDs honestly. The re-tighten of the shared bound toward the 16 ms half-frame stays a
//! separate follow-up (issue 1121), gated on the cam1 grabber SWAP.
//!
//! ## The blocking / revert seam
//!
//! [`gates_overall_pass`] mirrors [`crate::imag_leg_gate::gates_overall_pass`] /
//! [`crate::optical_floor::gates_overall_pass`] / [`crate::e2e_latency_gate::gates_overall_pass`]:
//! a one-line-restorable toggle deciding whether the delivery-spread term folds into
//! `overall_pass`. It is `true` (BLOCKING) since #1142. Flip back to `false` for a one-line revert
//! to report-only ONLY if a rig change proves the bound false-reds a genuinely-clean run. The bound
//! needs no new constant: it reuses [`crate::switch_latency::SPREAD_THRESHOLD_MS`] (24 ms since
//! issue 1120; was the 16 ms half-frame of issue 624), the same bound `spread_gate_pass` is already
//! computed against — both spreads are driven by the SAME CAM1 grabber, so they share ONE constant
//! and re-tighten together (issue 1121).
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::imag_leg_gate`] / [`crate::e2e_latency_gate`]: the whole `probe`
//! module is `#[cfg(feature = "probe")]`, so `recording-verdict.rs` is CI-only. This is the PURE
//! fold decision — no probe deps — so it unit-tests Tier-0 (default features). `recording-verdict`
//! (probe-gated) only CALLS these; it never re-derives the toggle.

/// The pinned cross-camera DELIVERY-spread bound, in milliseconds. Re-exported from
/// [`crate::switch_latency::SPREAD_THRESHOLD_MS`] (24 ms since issue 1120; was the 16 ms
/// half-frame of issue 624) so a caller/reader of THIS gate names one bound; there is no second,
/// drifting constant.
pub const DELIVERY_SPREAD_BOUND_MS: f64 = crate::switch_latency::SPREAD_THRESHOLD_MS;

/// Does the ALL-CAMBOX delivery cross-camera-spread term fold into `overall_pass`?
///
/// `true` since #1142 (BLOCKING, owner mandate 2026-08-19): a delivery spread over
/// [`DELIVERY_SPREAD_BOUND_MS`] (24 ms) now REDs the run. Was report-only (issue 1033) on the theory
/// "wait for a tight-green fleet before gating" — but the owner mandate is the opposite: the "green"
/// runs it passed were FALSELY green (the phase lottery, 3.97 vs 85 ms, hid a real delivery-spread
/// failure behind a green gate), and the point of gating is to STOP that. The SOURCE-side spread
/// already blocks at the same bound; #1142 makes the DELIVERY side block too. Flip back to `false`
/// for a one-line revert to report-only ONLY if a rig change proves the bound false-reds a
/// genuinely-clean run (then RE-TIGHTEN the shared bound per issue 1121, never just relax).
pub fn gates_overall_pass() -> bool {
    // #1142 — BLOCKING: the delivery cross-camera-spread term folds into overall_pass at the shared
    // SPREAD_THRESHOLD_MS bound. Was `false` (issue 1033 report-only). The owner mandate flips it
    // LIVE so a bad-phase delivery spread REDs the run instead of hiding behind a green gate.
    true
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
