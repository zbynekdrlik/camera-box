//! issue 798 (path A) — the imag-leg recording verdict is REPORT-ONLY first.
//!
//! ## Why this exists
//!
//! The imag leg's frame-by-frame zero-loss verdict already exists and is computed
//! ([`crate`]-external `node_verdict_for_imag` in `recording-verdict.rs`): the cam2 OPTICAL tick
//! contiguity ANDed with imag's own digital corner-burn contiguity (`imag_burn_ok`, issue 463) and
//! the optical beat freeze/copy gate (issue 580v2). Historically it folded HARD into
//! `overall_pass` (`all_pass &= nv.is_zero() && span_ok`) — but with a fatal gap: the imag partial
//! has NEVER actually reached the merge on the live rig (0 of 76 recent `recording-e2e` runs
//! produced an `imag-partial-*.json`), because `recording-e2e.sh` `[8/8c]` degrades gracefully on
//! any imag-side StopRecord / reachability / decode failure and the merge silently omits
//! `--merge-partials imag=...`. So a green run silently did NOT prove the imag leg (a HIDDEN
//! partial — the "ONE full test, no partials" doctrine's banned outcome), and turning the hard
//! gate on would immediately RED the first run that ever produced an imag partial: there is zero
//! green imag distribution to calibrate against, and the issue-887 produced-vs-presented advisory
//! already shows a real ~7% deficit.
//!
//! Path A (supervisor decision, 2026-08-17): make the imag verdict genuinely FLOW into the merged
//! report and be SURFACED, but ship it REPORT-ONLY first — so it never reds a run until its own
//! green distribution accumulates — then a separate follow-up ticket flips it blocking and folds
//! in the issue-887 produced-vs-presented deficit.
//!
//! ## The report-only / restore seam
//!
//! [`gates_overall_pass`] mirrors [`crate::optical_floor::gates_overall_pass`] /
//! [`crate::burn_hold::gates_overall_pass`] / [`crate::e2e_latency_gate::gates_overall_pass`]: a
//! one-line-restorable toggle deciding whether the imag-leg term folds into `overall_pass`. It is
//! `false` (REPORT-ONLY) today. The follow-up flips it to `true` to make the imag leg gate for
//! real once healthy imag runs exist.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::e2e_latency_gate`] / [`crate::optical_floor`]: the whole `probe`
//! module is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/`drm`), so `recording-verdict.rs`
//! is CI-only. This is the PURE fold decision — no probe deps — so it unit-tests Tier-0 (default
//! features). `recording-verdict` (probe-gated) only CALLS these; it never re-derives the toggle.

/// Does the imag-leg recording verdict fold into `overall_pass`?
///
/// `false` today (REPORT-ONLY, path A): the imag verdict flows + is surfaced but never fails a run.
/// The ONE line a follow-up flips to `true` to promote the imag leg to a LIVE blocking gate (and
/// then fold in the issue-887 produced-vs-presented deficit) once healthy imag runs accumulate.
pub fn gates_overall_pass() -> bool {
    // #798 path A — REPORT-ONLY today: the imag verdict flows + is surfaced but never reds a run.
    // The ONE line a follow-up flips to `true` to promote the imag leg to a LIVE blocking gate
    // (and fold in the issue-887 produced-vs-presented deficit) once healthy imag runs accumulate.
    false
}

/// Pure fold: does an imag-leg outcome (`node_ok`) pass `overall_pass`, given whether the seam is
/// live (`gates_overall`)? Mirrors every other report-only seam's `pass || !gates_overall` fold:
/// - report-only (`gates_overall == false`): ALWAYS passes (a failing imag leg never reds a run);
/// - blocking (`gates_overall == true`): passes iff `node_ok`.
pub fn fold(node_ok: bool, gates_overall: bool) -> bool {
    node_ok || !gates_overall
}

/// Call-site helper: fold an imag-leg outcome against the LIVE seam state
/// ([`gates_overall_pass`]). `recording-verdict.rs` calls exactly this and never re-derives the
/// toggle, so the whole decision is Tier-0 verifiable despite the probe gate on that binary.
pub fn folds_into_overall_pass(node_ok: bool) -> bool {
    fold(node_ok, gates_overall_pass())
}
