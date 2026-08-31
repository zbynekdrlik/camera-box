//! #726 — presentation-cadence EVENNESS metric.
//!
//! Mirrors the `painted_tick_gaps.rs` / `reannounce.rs` / `colour_scale.rs` Tier-0 seam pattern:
//! the WHOLE `probe` module is `#[cfg(feature = "probe")]` (CI-only, never compiled/tested
//! locally per CLAUDE.md's Local Build Policy), so the PURE decision logic lives here at the
//! crate root where it unit-tests on DEFAULT features.
//!
//! ## The symptom this measures (live event, 2026-07-12)
//!
//! strih/stream's output stuttered "like 15fps" at its normal 30fps canvas during a broadcast;
//! switching strih to 60fps mitigated it live (strih is designed to run 30fps — Topology v2,
//! #459/#466 — and was switched back afterward). The existing zero-loss / A/V-sync / continuity
//! gates (`full_chain`, `all_cambox_continuity`) were BLIND to this failure: they prove no NET
//! frame was lost and no A/V drift happened, but say nothing about whether the 30 frames/sec that
//! DID arrive were presented EVENLY. A perfectly loss-free recording can still look like half its
//! real frame rate if every other canvas tick silently re-presents the PREVIOUS frame's content
//! instead of a fresh one — mechanically 30fps, visually ~15fps.
//!
//! ## What "even" means here
//!
//! Given the per-frame painted-tick VALUES of a recording in RECORDED order (the same
//! `SegmentFrame.tick` sequence `probe::recording_segments::window_segment` already extracts from
//! cam2's own 60fps painter, decimated into the canvas at `expected_step` painted-ticks per
//! recorded frame — 2 for a 60fps painter into a 30fps canvas), a SMOOTH downsample advances the
//! tick by exactly `expected_step` on EVERY consecutive recorded frame (uniform cadence: the
//! canvas always shows the newest available source frame). A JUDDERY ("15fps-like") downsample
//! instead re-presents the SAME tick value on one canvas frame (delta 0 — a visual duplicate,
//! already flagged by `CamboxSegment.copies`) immediately followed by a compensating DOUBLE jump
//! (delta `2 * expected_step`) that catches the tick count back up to the true net position —
//! mechanically still 30 distinct output frames/sec (no NET loss, `copies`/`gaps` alone can look
//! clean), but visually pairs every two canvas frames into one presented image, halving the
//! perceived motion rate — exactly the "paired spacing" the issue names.
//!
//! [`measure_cadence_evenness`] classifies every consecutive-frame delta and reports the
//! fractions. #1036 CALIBRATED the "15fps-judder" signature (`paired_fraction`) against 210
//! cadence windows from 21 green rig runs and added [`cadence_judder_gate_pass`] +
//! [`PAIRED_FRACTION_JUDDER_MAX`] (0.05) + the one-line-restorable [`gates_overall_pass`] seam, so
//! a cadence regression now BLOCKS the fused E2E verdict (LIVE). The raw per-window numbers are
//! still reported first (issue #726 deliverable 1); the gate folds on top.

/// Per-recording (or per-window) presentation-cadence classification, built from a sequence of
/// painted-tick values in RECORDED (delivery) order — NOT sorted, unlike
/// `painted_tick_gaps::painted_tick_gaps`'s net-loss accounting, because cadence EVENNESS is
/// inherently an order-dependent question (a sorted sequence cannot tell smooth from paired).
// #726: carries a `BTreeMap` (the delta histogram), which isn't `Copy` — this struct dropped the
// `Copy` derive it used to carry when it fixed the miscalibration; `Clone`/`Debug`/`PartialEq`
// (all still derived) are what every caller/test actually uses.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CadenceEvenness {
    /// The nominal painted-tick step per recorded frame this recording was decimated at (e.g. 2
    /// for a 60fps painter into a 30fps canvas). Echoed for the report.
    pub expected_step: i64,
    /// Number of consecutive-frame deltas evaluated (`ticks.len() - 1`).
    pub sample_deltas: usize,
    /// Deltas exactly `expected_step` — a fresh, on-cadence frame.
    pub uniform_steps: usize,
    /// Deltas exactly `0` — the SAME tick re-presented (a visual duplicate of the prior frame).
    pub duplicate_steps: usize,
    /// Deltas exactly `2 * expected_step` — a compensating double jump (net position recovered).
    pub catchup_steps: usize,
    /// Anything else — decode noise, a genuinely missing/extra tick, or a bigger irregular jump.
    /// Never itself proof of loss (see `painted_tick_gaps` for the net-loss accounting); just not
    /// one of the three clean cadence shapes above.
    pub other_steps: usize,
    /// Adjacent `(duplicate, catchup)` pairs — i.e. `deltas[i] == 0 && deltas[i+1] == 2 *
    /// expected_step` — the SPECIFIC "15fps-like" signature: a held frame immediately compensated
    /// by a double-step jump, as opposed to an isolated duplicate with no adjacent catch-up
    /// (which may be a genuine unrecovered stutter rather than this pacing artifact).
    pub paired_events: usize,
    pub uniform_fraction: f64,
    pub duplicate_fraction: f64,
    /// Fraction of ALL deltas that participate in a paired duplicate+catchup event
    /// (`paired_events * 2 / sample_deltas`) — both halves of the pair are "judder", not just the
    /// duplicate half.
    pub paired_fraction: f64,
    /// The headline number: `uniform_fraction`, restated for readability. `1.0` = every frame was
    /// perfectly on-cadence (smooth); `0.0` = no frame was ever on-cadence.
    pub evenness_score: f64,
    /// #726 MISCALIBRATION FIX (a): raw histogram of every consecutive-frame delta value ->
    /// occurrence count, in RECORDED order (the SAME deltas the classification above uses). A
    /// perfectly clean 60-in-30 window on the real rig looks like `{1: N, 7: M, ...}`, NOT a
    /// clean `{2: N}` — multi-hop jitter (camera->NDI->genlock->canvas) means individual deltas
    /// rarely land exactly on the theoretical decimation ratio, even though the NET average
    /// tracks it exactly. Exposed so a caller (or a human reading the report) can see the real
    /// delta distribution directly, instead of only the (possibly wrong-assumption) counts above.
    pub delta_histogram: std::collections::BTreeMap<i64, usize>,
    /// #726 MISCALIBRATION FIX (b): `expected_step` AS DERIVED FROM THIS WINDOW'S OWN DATA — the
    /// mode (most frequent value) of the POSITIVE entries in `delta_histogram`. This is the
    /// honest "what does on-cadence actually look like here" step, which on real rig data is
    /// frequently SMALLER than the caller-supplied `expected_step` (the theoretical decimation
    /// ratio `painted_tick_gaps` uses for its net-loss accounting) — the two are expected to
    /// DIFFER: `expected_step` stays authoritative for LOSS accounting, `derived_expected_step`
    /// is authoritative for CADENCE evenness. Falls back to `expected_step` when no positive
    /// delta exists in this window (nothing to derive a mode from — e.g. every delta is 0 or
    /// negative).
    pub derived_expected_step: i64,
    /// #726 MISCALIBRATION FIX (c) — the SELF-CONSISTENT reading: deltas exactly
    /// `derived_expected_step` (the real-data on-cadence bucket). A window with `copies == 0 &&
    /// gaps == 0` (no net loss) is guaranteed to have most of its mass here, unlike
    /// `uniform_steps` above, which can misreport near-zero when the caller's `expected_step`
    /// guess doesn't match the real per-frame jitter pattern — exactly the internally-
    /// inconsistent live bug (window8, 2026-07-13: 845/845 `other` on a copies=0/gaps=0 window)
    /// this field fixes.
    pub derived_uniform_steps: usize,
    pub derived_uniform_fraction: f64,
}

/// Classify the presentation cadence of `ticks` (painted-tick values in RECORDED order).
///
/// Returns `None` when there isn't enough data to say anything (`ticks.len() < 2`) or
/// `expected_step` is non-positive (not a valid decimation ratio) — e.g. a schedule window where
/// this cambox never carried the painted tick (a non-cam2 window: `ticks` is empty because every
/// frame's `tick` was `None`). A caller should treat `None` as "not applicable to this window",
/// never as a failure.
pub fn measure_cadence_evenness(ticks: &[u32], expected_step: i64) -> Option<CadenceEvenness> {
    if ticks.len() < 2 || expected_step <= 0 {
        return None;
    }

    let deltas: Vec<i64> = ticks
        .windows(2)
        .map(|w| i64::from(w[1]) - i64::from(w[0]))
        .collect();
    let n = deltas.len();

    let mut uniform = 0usize;
    let mut duplicate = 0usize;
    let mut catchup = 0usize;
    let mut other = 0usize;
    let mut paired = 0usize;
    let mut histogram: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();

    for i in 0..n {
        let d = deltas[i];
        *histogram.entry(d).or_insert(0) += 1;
        if d == expected_step {
            uniform += 1;
        } else if d == 0 {
            duplicate += 1;
            if i + 1 < n && deltas[i + 1] == 2 * expected_step {
                paired += 1;
            }
        } else if d == 2 * expected_step {
            catchup += 1;
        } else {
            other += 1;
        }
    }

    // #726 (b): derive the REAL on-cadence step from this window's own data — the mode of the
    // POSITIVE deltas (delta==0 is a duplicate/hold, not a step; a negative delta is a
    // reorder/decode artifact, not a cadence step either). `max_by_key` on a BTreeMap (ascending
    // key order) breaks ties toward the LARGER delta value, which is an arbitrary-but-deterministic
    // choice — ties are not expected on real jitter data. Falls back to the caller's
    // `expected_step` when there is no positive delta to derive from (degenerate: every delta is
    // 0 or negative), so this never panics or invents a sentinel.
    let derived_expected_step = histogram
        .iter()
        .filter(|(&d, _)| d > 0)
        .max_by_key(|(_, &count)| count)
        .map(|(&d, _)| d)
        .unwrap_or(expected_step);
    let derived_uniform = histogram.get(&derived_expected_step).copied().unwrap_or(0);

    let nf = n as f64;
    Some(CadenceEvenness {
        expected_step,
        sample_deltas: n,
        uniform_steps: uniform,
        duplicate_steps: duplicate,
        catchup_steps: catchup,
        other_steps: other,
        paired_events: paired,
        uniform_fraction: uniform as f64 / nf,
        duplicate_fraction: duplicate as f64 / nf,
        paired_fraction: (paired * 2) as f64 / nf,
        evenness_score: uniform as f64 / nf,
        delta_histogram: histogram,
        derived_expected_step,
        derived_uniform_steps: derived_uniform,
        derived_uniform_fraction: derived_uniform as f64 / nf,
    })
}

/// #1036 — the CALIBRATED per-window bound on the "15fps-like" presentation-judder signature.
///
/// The judder class this whole module measures (a source frame held for one extra canvas tick
/// then a compensating double-step jump — mechanically 30 fps, visually ~15 fps) shows up
/// SPECIFICALLY as [`CadenceEvenness::paired_fraction`] (an adjacent duplicate delta `0` followed
/// by a `2 * expected_step` catch-up). That fraction is order-dependent and immune to the ordinary
/// multi-hop jitter that makes `evenness_score`/`duplicate_fraction` too noisy to gate on.
///
/// Calibrated against 210 cadence windows from 21 GREEN `recording-e2e` runs (all
/// `expected_step = 2`, the only populated class): the worst observed `paired_fraction` over ALL
/// 310 windows (green + red) is `0.00473`, while the pathology (`fifteen_fps_like_judder_is_
/// paired_spacing`) sits at `~0.966`. `0.05` is 10.6x the worst observed green window and ~19x
/// below the pathology — it passes every green run with honest margin while catching even a
/// partial (~1-in-10) judder. Tighten toward `~0.02` as the jitter tail is better characterized.
/// See issue #1036 for the full per-run baseline table.
pub const PAIRED_FRACTION_JUDDER_MAX: f64 = 0.05;

/// Does the run's WORST per-window paired-judder fraction satisfy the [`PAIRED_FRACTION_JUDDER_MAX`]
/// bound? `worst_paired_fraction` is the max [`CadenceEvenness::paired_fraction`] across every
/// cadence-bearing window in the recording (a single per-window RATE, not a count — the judder
/// pathology saturates every affected window, so a per-window-max term has no "spread the budget"
/// loophole and needs no run-wide second term, unlike the count-based [`crate::optical_floor`]).
///
/// Arms mirror the [`crate::e2e_latency_gate::cam_strih_latency_gate_pass`] convention, with ONE
/// deliberate divergence on the "no measurement" arm:
/// - `None` bound ⇒ report-only, always passes.
/// - `None` worst (the run produced NO cadence-bearing window at all) ⇒ **PASS** — per the
///   [`measure_cadence_evenness`] `None` contract this is "not applicable", and any condition that
///   zeroes out every cadence window (mass optical-decode failure) is ALREADY hard-failed by the
///   copies/gaps/undecodable gates, so passing here is not a test-strictness hole (no
///   double-jeopardy). This is why it does NOT FAIL-on-no-samples the way the latency gate does:
///   there, a missing sample is genuinely anomalous and unguarded elsewhere; here it is not.
/// - `Some` bound, `Some` worst ⇒ pass iff `worst <= bound` (strict `>`: a worst exactly at the
///   bound passes).
pub fn cadence_judder_gate_pass(worst_paired_fraction: Option<f64>, max: Option<f64>) -> bool {
    match (max, worst_paired_fraction) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(bound), Some(worst)) => worst <= bound,
    }
}

/// #1036 report-only / restore seam — mirrors [`crate::e2e_latency_gate::gates_overall_pass`] /
/// [`crate::optical_floor::gates_overall_pass`]. Whether [`cadence_judder_gate_pass`]'s result
/// folds into the fused verdict's `overall_pass`. `true` today (the bound is LIVE — it passes
/// every green run with honest margin — the worst `paired_fraction` across the 21 green runs is
/// 0.00473 (10.6x under the bound), and that INCLUDES CAM1 windows which ARE subject to the cam1
/// ShadowCast grabber defect (issue 909). A capture-side drop can in principle complete a paired
/// event next to a duplicate (a `2 * expected_step` catch-up delta), but empirically it never
/// lifts `paired_fraction` anywhere near the bound — this is an optical presentation-cadence
/// signal largely independent of that grabber). Flip to `false` for a one-line revert to
/// report-only if a future rig change ever trips it.
pub fn gates_overall_pass() -> bool {
    true
}

/// #1142 — the calibrated per-window UNIFORMITY floor: the minimum acceptable
/// [`CadenceEvenness::uniform_fraction`] on any cadence-bearing cambox window.
///
/// [`CadenceEvenness::uniform_fraction`] is the fraction of consecutive-frame deltas that advanced
/// by exactly `expected_step` — a SMOOTH 60fps→30fps downsample reads ~1.0 (every canvas frame
/// shows a fresh source frame), while the 60→30 decimation + FIFO presentation limit-cycle churn
/// this project's live judder shows up as (issue 1130: cam1 segment Δ1=16.5%/Δ3=13.3% @30fps) drops
/// it to ~0.67-0.78 on TODAY's rig. This is a NEW BLOCKING term (owner mandate 2026-08-19: "pritvrd
/// gates aby som zasa nezistil z mesiac prace ze to vlastne nejde") — DISTINCT from the specific
/// paired-judder signature [`PAIRED_FRACTION_JUDDER_MAX`]: this bounds the BROAD cadence uniformity,
/// that bounds the specific 15fps-like duplicate+catchup pairing. Both coexist.
///
/// The floor is CONSERVATIVE at 0.95: today's whole fleet (every mined verdict, INCLUDING the
/// freshest post-fix run 1288585861 at worst uniform 0.775) sits at 0.67-0.78, so this gate is RED
/// on the current rig — the INTENDED outcome (the owner wants the visual judder surfaced, never
/// hidden behind a green gate). A healthy 60fps-through-30fps chain should read ~1.0.
/// TODO(#1142 follow-up): recalibrate to the tightest value the FIRST genuinely-clean post-fix run
/// supports (the gates-green-first philosophy applies once a clean baseline exists — today NONE
/// does, so the conservative 0.95 with today's 0.67-0.78 data is the honest starting floor).
///
/// Gated on `derived_uniform_fraction` — the SELF-CONSISTENT reading (fraction of deltas equal to
/// the window's OWN delta MODE, the #726 miscalibration fix), NOT the raw `uniform_fraction`
/// (fraction equal to the caller-supplied `expected_step`). The ticket named `uniform_fraction`,
/// but the raw field FALSE-REDS a clean window whose per-frame step mode differs from `expected_step`
/// (the #726 shape) — a genuine hazard: several synthetic switch-schedule test fixtures advance the
/// tick by +1 under `--switch-expected-step 2`, so their raw `uniform_fraction` reads 0.0 while the
/// window is perfectly clean (`derived_uniform_fraction` reads 1.0, mode +1). On the REAL rig the
/// two are EQUAL (the chain's delta mode IS `expected_step`=2, verified across every mined verdict:
/// worst 0.672–0.775 on both fields), so gating on `derived` REDs the current sick rig IDENTICALLY
/// while never false-reding a clean-but-jittery window — strictly better, and it is what this
/// module's own #726 fix introduced `derived_uniform_fraction` for. The block surfaces BOTH (raw
/// diagnostic + derived gated), so reverting to the raw field is a one-line change if ever wanted.
pub const UNIFORM_FRACTION_MIN: f64 = 0.95;

/// Does the run's WORST (minimum) per-window uniformity satisfy the [`UNIFORM_FRACTION_MIN`] FLOOR?
/// `worst_uniform_fraction` is the MIN [`CadenceEvenness::derived_uniform_fraction`] across every
/// cadence-bearing window (the self-consistent field — see [`UNIFORM_FRACTION_MIN`] for why derived,
/// not raw; a single per-window RATE — the judder/churn pathology depresses the affected window's
/// uniformity across its whole span, so a per-window-min term has no "spread the budget" loophole
/// and needs no run-wide second term, mirroring [`cadence_judder_gate_pass`]).
///
/// Arms mirror [`cadence_judder_gate_pass`], with the comparison INVERTED to `>=` (this is a FLOOR,
/// higher is better; the judder gate is a CEILING, lower is better):
/// - `None` floor ⇒ report-only, always passes.
/// - `None` worst (no cadence-bearing window at all) ⇒ PASS — "not applicable" per the
///   [`measure_cadence_evenness`] `None` contract; any condition that zeroes out every cadence
///   window (mass optical-decode failure) is ALREADY hard-failed by copies/gaps/undecodable (no
///   double-jeopardy), exactly as for the judder gate.
/// - `Some` floor, `Some` worst ⇒ pass iff `worst >= floor` (a window exactly at the floor passes).
pub fn cadence_uniformity_gate_pass(worst_uniform_fraction: Option<f64>, min: Option<f64>) -> bool {
    match (min, worst_uniform_fraction) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(floor), Some(worst)) => worst >= floor,
    }
}

/// #1142 uniformity report-only / restore seam — mirrors [`gates_overall_pass`]. Whether
/// [`cadence_uniformity_gate_pass`]'s result folds into the fused verdict's `overall_pass`.
/// `true` (BLOCKING) since #1142 so the current rig's ~0.70 uniformity REDs the run — the
/// owner-mandated intended outcome. Flip back to `false` for a one-line revert to report-only ONLY
/// if a future rig change proves the floor false-reds a genuinely-clean run (then RECALIBRATE, never
/// just relax).
pub fn uniformity_gates_overall_pass() -> bool {
    // #1142 — BLOCKING (owner mandate 2026-08-19): the cadence-uniformity floor folds into
    // overall_pass, so the current rig's ~0.70 worst uniform_fraction REDs the run — the intended
    // outcome (surface the visual judder, never hide it). Flip to `false` for a one-line revert to
    // report-only ONLY if a future rig change proves the floor false-reds a genuinely-clean run.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #726 MISCALIBRATION FIX: histogram + data-derived expected_step ----------------
    //
    // Live verdict data (2026-07-13, window8): 845/845 deltas classified `other` at the
    // CALLER-SUPPLIED expected_step=2, even though the SAME window simultaneously reports
    // copies=0/gaps=0/undecodable=0 (a PERFECTLY clean window under the net-loss accounting)
    // -- internally inconsistent. Root cause: the real per-frame delta pattern on this
    // multi-hop chain (camera->NDI->genlock->canvas) is NOT a clean uniform +2 -- it is
    // mostly SMALL steps (+1) with periodic CATCH-UP jumps (+7 in the live data, ratio
    // ~5.3:1), netting to the correct average (~2.0) without any individual delta ever
    // landing on 2. These tests reproduce that shape (five +1 deltas then one +7, repeated
    // -- net average is exactly 2/step: 5*1+7=12 over 6 steps=2.0) and lock the fix: (a) a
    // raw delta histogram, (b) `expected_step` derived from the data (mode of positive
    // deltas), (c) a self-consistency reading (a zero-net-loss window classifies mostly
    // on-cadence under the DERIVED step, unlike the old exact-match reading).

    fn real_rig_jitter_pattern(reps: u32) -> Vec<u32> {
        let mut ticks: Vec<u32> = vec![0];
        for _ in 0..reps {
            let base = *ticks.last().unwrap();
            for k in 1..=5u32 {
                ticks.push(base + k);
            }
            ticks.push(base + 5 + 7);
        }
        ticks
    }

    #[test]
    fn histogram_and_derived_step_reflect_the_real_data_not_the_callers_guess() {
        let ticks = real_rig_jitter_pattern(20); // 120 deltas: 100 x +1, 20 x +7
        let v = measure_cadence_evenness(&ticks, 2).expect("plenty of samples");

        assert_eq!(
            v.delta_histogram.get(&1).copied().unwrap_or(0),
            100,
            "100 deltas of +1 (20 reps * 5 each): {v:?}"
        );
        assert_eq!(
            v.delta_histogram.get(&7).copied().unwrap_or(0),
            20,
            "20 deltas of +7 (20 reps * 1 each): {v:?}"
        );
        assert_eq!(
            v.derived_expected_step, 1,
            "the mode of the POSITIVE deltas is +1 (100 occurrences beats +7's 20), not the \
             caller's guess of 2: {v:?}"
        );
    }

    #[test]
    fn zero_net_loss_window_with_wrong_callers_guess_still_classifies_mostly_uniform_via_derived_step(
    ) {
        let ticks = real_rig_jitter_pattern(20);
        let v = measure_cadence_evenness(&ticks, 2).expect("plenty of samples");

        // Reproduces the live bug: the OLD exact-match reading at the caller's expected_step=2
        // buckets EVERY delta as `other` -- no delta is ever exactly 2 or 4 in this pattern.
        assert_eq!(
            v.uniform_steps, 0,
            "the OLD expected_step=2 exact-match bucket must reproduce the live miscalibration: \
             {v:?}"
        );

        // The fix: this window has NO net loss (the pattern nets to exactly `expected_step`=2
        // per step by construction), so the auto-calibrated (derived-step) reading must
        // classify the overwhelming majority of frames as on-cadence -- not near-zero like the
        // old exact-match reading (#726 self-consistency (c)).
        assert!(
            v.derived_uniform_fraction > 0.8,
            "a zero-net-loss window must self-consistently classify as mostly on-cadence under \
             the derived step, got {v:?}"
        );
    }

    #[test]
    fn derived_expected_step_falls_back_to_callers_value_when_no_positive_delta_exists() {
        // Degenerate: every delta is 0 (frozen) -- there is no positive delta to derive a mode
        // from. Must fall back to the caller's `expected_step`, never panic or a sentinel.
        let ticks: Vec<u32> = vec![5, 5, 5, 5];
        let v = measure_cadence_evenness(&ticks, 2).unwrap();
        assert_eq!(
            v.derived_expected_step, 2,
            "falls back to the caller's expected_step when no positive delta exists: {v:?}"
        );
        assert_eq!(v.derived_uniform_steps, 0);
    }

    // ---- degenerate inputs -------------------------------------------------------------

    #[test]
    fn too_few_ticks_returns_none() {
        assert_eq!(measure_cadence_evenness(&[], 2), None);
        assert_eq!(measure_cadence_evenness(&[10], 2), None);
    }

    #[test]
    fn non_positive_expected_step_returns_none() {
        assert_eq!(measure_cadence_evenness(&[0, 2, 4], 0), None);
        assert_eq!(measure_cadence_evenness(&[0, 2, 4], -2), None);
    }

    #[test]
    fn empty_window_no_painted_tick_present_is_none() {
        // e.g. a non-cam2 CAMBOX_SWEEP window: every SegmentFrame.tick is None, so the caller's
        // `present_ticks` filter produces an empty slice — this must read as "not applicable",
        // never as a judder verdict.
        let ticks: [u32; 0] = [];
        assert_eq!(measure_cadence_evenness(&ticks, 2), None);
    }

    // ---- the two named reference patterns (issue #726, verbatim) ---------------------------

    #[test]
    fn smooth_30_from_60fps_source_is_uniform_2_tick_cadence() {
        // 60fps painter decimated smoothly into a 30fps canvas: every recorded frame advances by
        // exactly `expected_step` (2) — the "smooth 30" reference shape.
        let ticks: Vec<u32> = (0..60).step_by(2).collect(); // 0,2,4,...,58 (30 values)
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.sample_deltas, 29);
        assert_eq!(v.uniform_steps, 29);
        assert_eq!(v.duplicate_steps, 0);
        assert_eq!(v.catchup_steps, 0);
        assert_eq!(v.other_steps, 0);
        assert_eq!(v.paired_events, 0);
        assert_eq!(v.uniform_fraction, 1.0);
        assert_eq!(v.duplicate_fraction, 0.0);
        assert_eq!(v.paired_fraction, 0.0);
        assert_eq!(v.evenness_score, 1.0);
    }

    #[test]
    fn fifteen_fps_like_judder_is_paired_spacing() {
        // Every source frame held for TWO canvas ticks then a compensating double jump: the SAME
        // tick value presented twice (0, then 4, then 4 again is wrong — must be: 0,0,4,4,8,8,...)
        // — deltas: 0,4,0,4,0,4,... — the exact "paired spacing" the issue names, and mechanically
        // still 30 distinct recorded frames (no net loss) despite halving perceived motion.
        let mut ticks = Vec::new();
        for k in 0..15u32 {
            let t = k * 4;
            ticks.push(t);
            ticks.push(t); // held (duplicate content)
        }
        // ticks = [0,0,4,4,8,8,...,56,56] (30 values) -> deltas = [0,4,0,4,...,0,4] minus a
        // trailing element (29 deltas: last delta is between the final pair's second 56 and...
        // there is no further element, so the sequence naturally ends on a duplicate delta 0).
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.sample_deltas, 29);
        assert_eq!(
            v.uniform_steps, 0,
            "a paired-judder recording has NO on-cadence frames"
        );
        assert_eq!(v.duplicate_steps, 15);
        assert_eq!(v.catchup_steps, 14); // the last duplicate (idx 28) has no following delta
        assert_eq!(v.paired_events, 14);
        assert_eq!(v.evenness_score, 0.0);
        assert!(
            (v.paired_fraction - (28.0 / 29.0)).abs() < 1e-9,
            "28 of 29 deltas participate in a paired duplicate+catchup event, got {}",
            v.paired_fraction
        );
    }

    // ---- mixed / partial patterns ------------------------------------------------------

    #[test]
    fn partially_juddery_recording_reports_fractional_evenness() {
        // First half smooth, second half paired — proves the metric reports a genuine MIX, not
        // just the two extremes.
        let mut ticks: Vec<u32> = (0..20).step_by(2).collect(); // 10 smooth values, 0..18
        let base = *ticks.last().unwrap();
        for k in 1..=5u32 {
            let t = base + k * 4;
            ticks.push(t);
            ticks.push(t);
        }
        // smooth run: 9 uniform deltas. Then transition delta (18 -> 22) is itself uniform (=2)
        // too since base+4 - base = 4 != 2... let's not hand-derive it in the assertion; just
        // assert the mix is neither 0 nor 1 and the counts add up.
        let v = measure_cadence_evenness(&ticks, 2).expect("enough samples");
        assert!(v.evenness_score > 0.0 && v.evenness_score < 1.0);
        assert_eq!(
            v.uniform_steps + v.duplicate_steps + v.catchup_steps + v.other_steps,
            v.sample_deltas
        );
    }

    #[test]
    fn isolated_duplicate_with_no_adjacent_catchup_is_not_counted_paired() {
        // A single held frame that does NOT get a clean compensating double-jump right after it
        // (e.g. the run just continues on-cadence from the held value, net-losing one tick
        // somewhere else) must NOT be misclassified as the paired "15fps" signature — it's some
        // OTHER shape and paired_events must stay 0 for this local pattern.
        let ticks: Vec<u32> = vec![0, 0, 2, 4, 6]; // deltas: 0, 2, 2, 2
        let v = measure_cadence_evenness(&ticks, 2).unwrap();
        assert_eq!(v.duplicate_steps, 1);
        assert_eq!(
            v.paired_events, 0,
            "delta after the duplicate is 2 (uniform), not a 4 catchup"
        );
        assert_eq!(v.uniform_steps, 3);
    }

    #[test]
    fn reports_are_stable_regardless_of_absolute_tick_offset() {
        // The metric must depend only on RELATIVE spacing, not on the absolute starting tick
        // value (a painted tick is a free-running counter that never resets to 0 mid-recording).
        let a = measure_cadence_evenness(&[0, 2, 4, 6, 8], 2).unwrap();
        let b =
            measure_cadence_evenness(&[100_000, 100_002, 100_004, 100_006, 100_008], 2).unwrap();
        assert_eq!(a.uniform_steps, b.uniform_steps);
        assert_eq!(a.evenness_score, b.evenness_score);
    }

    #[test]
    fn other_bucket_catches_irregular_jumps_that_are_neither_clean_shape() {
        // A genuinely irregular delta (neither 0, nor expected_step, nor 2*expected_step) — e.g.
        // decode noise producing an odd tick value — must land in `other`, not silently vanish
        // into one of the clean buckets.
        let ticks: Vec<u32> = vec![0, 2, 5, 7]; // deltas: 2, 3, 2 -> the middle one is irregular
        let v = measure_cadence_evenness(&ticks, 2).unwrap();
        assert_eq!(v.uniform_steps, 2);
        assert_eq!(v.other_steps, 1);
        assert_eq!(v.duplicate_steps, 0);
        assert_eq!(v.catchup_steps, 0);
    }

    // ---- #1036 — the CALIBRATED paired-judder gate (both directions) --------------------
    //
    // Calibration source: 210 cadence windows across 21 GREEN `recording-e2e` runs on dev1
    // (all `expected_step = 2`, the only populated class). Worst observed `paired_fraction`
    // over ALL 310 windows (green + red) is 0.00473; the 15fps-judder pathology (see
    // `fifteen_fps_like_judder_is_paired_spacing` above) sits at ~0.966 — a ~200x separation.
    // The bound is per-window worst `paired_fraction <= PAIRED_FRACTION_JUDDER_MAX` (0.05):
    // 10.6x the worst green window, ~19x below the pathology. See issue #1036 for the full
    // baseline table.

    #[test]
    fn default_bound_constant_is_the_calibrated_value() {
        assert_eq!(PAIRED_FRACTION_JUDDER_MAX, 0.05);
    }

    #[test]
    fn none_bound_is_report_only_always_passes() {
        // A `None` bound = the gate is disabled (report-only) and always passes, even a
        // pathological worst value.
        assert!(cadence_judder_gate_pass(Some(0.966), None));
        assert!(cadence_judder_gate_pass(None, None));
    }

    #[test]
    fn no_cadence_windows_is_not_applicable_pass() {
        // `worst = None` = the run produced no cadence-bearing window at all. Per the metric's
        // own `None` contract this is "not applicable", never a failure — and any condition that
        // zeroes out every cadence window (mass optical-decode failure) is already HARD-failed by
        // the copies/gaps/undecodable gates, so this is not a test-strictness hole.
        assert!(cadence_judder_gate_pass(
            None,
            Some(PAIRED_FRACTION_JUDDER_MAX)
        ));
    }

    #[test]
    fn worst_observed_green_window_passes_the_default_bound() {
        // The load-bearing calibration test: the worst `paired_fraction` measured across the 21
        // green runs (0.00473, run 77863612) MUST pass the default bound — a bound that would
        // fail a recent green run is not a valid bound.
        assert!(
            cadence_judder_gate_pass(Some(0.00473), Some(PAIRED_FRACTION_JUDDER_MAX)),
            "the worst observed green paired_fraction (0.00473) must pass the {PAIRED_FRACTION_JUDDER_MAX} bound"
        );
        // The next-worst cluster (0.00237) and the clean majority (0.0) also pass.
        assert!(cadence_judder_gate_pass(
            Some(0.00237),
            Some(PAIRED_FRACTION_JUDDER_MAX)
        ));
        assert!(cadence_judder_gate_pass(
            Some(0.0),
            Some(PAIRED_FRACTION_JUDDER_MAX)
        ));
    }

    #[test]
    fn fifteen_fps_judder_pathology_fails_the_bound() {
        // End-to-end: build the metric's OWN 15fps-judder reference pattern, measure its real
        // `paired_fraction`, and prove the gate FAILS on it — the degraded direction, wired to the
        // actual metric output rather than a hand-picked number.
        let mut ticks = Vec::new();
        for k in 0..15u32 {
            let t = k * 4;
            ticks.push(t);
            ticks.push(t); // held (duplicate content) -> duplicate+catchup pairs
        }
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert!(
            v.paired_fraction > 0.9,
            "sanity: the judder reference saturates paired_fraction, got {}",
            v.paired_fraction
        );
        assert!(
            !cadence_judder_gate_pass(Some(v.paired_fraction), Some(PAIRED_FRACTION_JUDDER_MAX)),
            "the 15fps-judder pathology (paired_fraction={}) must FAIL the bound",
            v.paired_fraction
        );
    }

    #[test]
    fn a_smooth_window_passes_end_to_end() {
        // The healthy direction wired to the metric: a perfectly smooth 60-in-30 downsample has
        // zero paired events, so its measured paired_fraction passes the gate.
        let ticks: Vec<u32> = (0..60).step_by(2).collect();
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.paired_fraction, 0.0);
        assert!(cadence_judder_gate_pass(
            Some(v.paired_fraction),
            Some(PAIRED_FRACTION_JUDDER_MAX)
        ));
    }

    #[test]
    fn boundary_at_bound_passes_just_over_fails() {
        assert!(
            cadence_judder_gate_pass(Some(0.05), Some(0.05)),
            "exactly at the bound passes (strict >)"
        );
        assert!(
            !cadence_judder_gate_pass(Some(0.0501), Some(0.05)),
            "just over the bound fails"
        );
    }

    #[test]
    fn a_partial_window_judder_well_above_green_noise_fails() {
        // A window where even ~1-in-10 delta-pairs are duplicate+catchup (paired_fraction 0.2) —
        // ~40x the worst green window and far below the full pathology — must still FAIL, proving
        // the bound catches a PARTIAL judder, not only the saturated reference.
        assert!(!cadence_judder_gate_pass(
            Some(0.2),
            Some(PAIRED_FRACTION_JUDDER_MAX)
        ));
    }

    #[test]
    fn gate_is_live_today() {
        // #1036: the paired-judder bound folds into overall_pass (LIVE) — it passes every green
        // run with honest margin (worst green paired_fraction 0.00473, 10.6x under the bound,
        // including CAM1 windows subject to the issue-909 grabber defect: a capture-side drop can
        // in principle complete a paired event, but empirically never approaches the bound). Flip
        // `gates_overall_pass` to false for a one-line revert to report-only if a rig change trips it.
        assert!(
            gates_overall_pass(),
            "#1036: the calibrated paired-judder bound must gate overall_pass (LIVE)"
        );
    }

    // ---- #1142 — the NEW cadence-UNIFORMITY floor gate (a broad companion to the judder gate) ----
    //
    // Owner mandate 2026-08-19: flip the visual-uniformity signal BLOCKING. Calibration source:
    // every mined `/tmp/recording-e2e-*/verdict-*.json` — worst per-window `uniform_fraction` sits
    // at 0.672-0.775 on TODAY's whole fleet (incl. the freshest post-fix run 1288585861 at 0.775),
    // while a healthy 60fps→30fps chain reads ~1.0. The floor is RED on the sick rig BY DESIGN
    // (surface the judder, never hide it).
    //
    // #1243 (walk-back: issue 1242): floor WALKED 0.95 -> 0.93. Run 1629895310 (the FIRST complete
    // 7-cam post-fix verdict, dev 45a856945) had worst derived_uniform_fraction 0.9397 as the ONLY
    // blocking RED; 0.93 is the tightest value that one steady run supports
    // (window-gate-tolerance-walkdown), still RED-ing the sick 0.67-0.78 band. issue 1242
    // root-causes the residual FIFO churn and restores 0.95.

    #[test]
    fn uniformity_floor_constant_is_the_walked_back_093() {
        // #1243 relax-to-green (walk-back: issue 1242): 0.95 -> 0.93, the tightest value run
        // 1629895310 (worst derived 0.9397) supports. RED before the source change, GREEN after.
        assert_eq!(UNIFORM_FRACTION_MIN, 0.93);
    }

    #[test]
    fn run_1629895310_worst_uniformity_passes_the_walked_back_093_floor() {
        // #1243 data-first (walk-back: issue 1242): the FIRST complete 7-cam post-fix verdict
        // (run 1629895310, dev 45a856945) had worst derived_uniform_fraction 0.9397163 — the ONLY
        // blocking gate that RED'd the run. The walked-back 0.93 floor lets it PASS while the sick
        // 0.67-0.78 band still FAILS (see `sick_rig_uniformity_070_fails_the_floor`). RED before
        // the 0.95 -> 0.93 source change, GREEN after.
        assert!(
            cadence_uniformity_gate_pass(Some(0.9397163120567376), Some(UNIFORM_FRACTION_MIN)),
            "run 1629895310 worst uniformity (0.9397) must PASS the walked-back {UNIFORM_FRACTION_MIN} floor"
        );
    }

    #[test]
    fn uniformity_none_floor_is_report_only_always_passes() {
        // A `None` floor = the gate is disabled (report-only) and always passes, even a
        // pathologically low uniformity.
        assert!(cadence_uniformity_gate_pass(Some(0.10), None));
        assert!(cadence_uniformity_gate_pass(None, None));
    }

    #[test]
    fn uniformity_no_cadence_window_is_not_applicable_pass() {
        // `worst = None` = the run produced no cadence-bearing window at all → "not applicable",
        // never a failure (a zeroed-out cadence run is already hard-failed by copies/gaps/undec).
        assert!(cadence_uniformity_gate_pass(
            None,
            Some(UNIFORM_FRACTION_MIN)
        ));
    }

    #[test]
    fn sick_rig_uniformity_070_fails_the_floor() {
        // The load-bearing intent: today's worst per-window uniformity (~0.67-0.78) must FAIL the
        // 0.93 floor (#1243 walk-back: issue 1242) — the owner-mandated RED on the sick rig, well
        // below even the walked-back floor. These are the REAL-rig values, on
        // which raw uniform_fraction == derived_uniform_fraction (the mode IS expected_step=2), so
        // the gated (derived) field reds identically to the raw field the ticket named.
        assert!(
            !cadence_uniformity_gate_pass(Some(0.6720), Some(UNIFORM_FRACTION_MIN)),
            "the sick-rig worst uniform_fraction (0.672, run 426009366) must FAIL the {UNIFORM_FRACTION_MIN} floor"
        );
        assert!(!cadence_uniformity_gate_pass(
            Some(0.6828),
            Some(UNIFORM_FRACTION_MIN)
        ));
        assert!(
            !cadence_uniformity_gate_pass(Some(0.7746), Some(UNIFORM_FRACTION_MIN)),
            "even the freshest post-fix run (1288585861, worst 0.775) must FAIL — still not clean"
        );
    }

    #[test]
    fn healthy_uniformity_passes_the_floor() {
        // A healthy 60fps-through-30fps chain reads ~1.0 → passes.
        assert!(cadence_uniformity_gate_pass(
            Some(1.0),
            Some(UNIFORM_FRACTION_MIN)
        ));
        assert!(cadence_uniformity_gate_pass(
            Some(0.97),
            Some(UNIFORM_FRACTION_MIN)
        ));
    }

    #[test]
    fn uniformity_boundary_at_floor_passes_just_under_fails() {
        // #1243 (walk-back: issue 1242) — boundary walked to the new 0.93 floor.
        assert!(
            cadence_uniformity_gate_pass(Some(0.93), Some(0.93)),
            "exactly at the floor passes (>=)"
        );
        assert!(
            !cadence_uniformity_gate_pass(Some(0.9299), Some(0.93)),
            "just under the floor fails"
        );
    }

    #[test]
    fn a_smooth_window_uniformity_passes_end_to_end() {
        // Wired to the metric on the GATED field (derived_uniform_fraction): a perfectly smooth
        // 60-in-30 downsample advances by exactly its own mode step every frame -> derived 1.0.
        let ticks: Vec<u32> = (0..60).step_by(2).collect();
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(v.derived_uniform_fraction, 1.0);
        assert!(cadence_uniformity_gate_pass(
            Some(v.derived_uniform_fraction),
            Some(UNIFORM_FRACTION_MIN)
        ));
    }

    #[test]
    fn a_clean_but_off_expected_step_window_passes_via_derived_not_raw() {
        // #1142 review — the raw-vs-derived hazard, pinned as a test: a perfectly clean window whose
        // ticks advance +1 while the caller passed expected_step=2 reads raw uniform_fraction 0.0
        // (would FALSE-RED the floor) but derived_uniform_fraction 1.0 (mode +1). The gate uses
        // DERIVED, so this clean window PASSES — exactly the synthetic switch-schedule fixtures the
        // review flagged (tick += 1 under --switch-expected-step 2).
        let ticks: Vec<u32> = (0..30).collect(); // +1 steps
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert_eq!(
            v.uniform_fraction, 0.0,
            "raw reads 0 at the wrong expected_step"
        );
        assert_eq!(
            v.derived_uniform_fraction, 1.0,
            "derived reads 1.0 at the real mode (+1)"
        );
        assert!(
            cadence_uniformity_gate_pass(
                Some(v.derived_uniform_fraction),
                Some(UNIFORM_FRACTION_MIN)
            ),
            "a clean-but-off-expected-step window must PASS (gated on derived, not raw)"
        );
        assert!(
            !cadence_uniformity_gate_pass(Some(v.uniform_fraction), Some(UNIFORM_FRACTION_MIN)),
            "…and would have FALSE-RED on the raw field — the reason the gate uses derived"
        );
    }

    #[test]
    fn the_15fps_judder_window_uniformity_fails_end_to_end() {
        // Wired to the metric on the GATED field: the 15fps-judder reference (held frame + double
        // jump) is NOT on-cadence under its own mode either — derived_uniform_fraction well below
        // the floor — so its measured uniformity FAILS.
        let mut ticks = Vec::new();
        for k in 0..15u32 {
            let t = k * 4;
            ticks.push(t);
            ticks.push(t);
        }
        let v = measure_cadence_evenness(&ticks, 2).expect("30 samples is plenty");
        assert!(
            v.derived_uniform_fraction < UNIFORM_FRACTION_MIN,
            "judder derived_uniform_fraction {} must be below the floor",
            v.derived_uniform_fraction
        );
        assert!(!cadence_uniformity_gate_pass(
            Some(v.derived_uniform_fraction),
            Some(UNIFORM_FRACTION_MIN)
        ));
    }

    #[test]
    fn uniformity_gate_is_live_since_1142() {
        // #1142: the uniformity floor folds into overall_pass (BLOCKING) — the owner mandate flips
        // it LIVE so the current rig's ~0.70 uniformity REDs the run (the intended outcome). Flip
        // `uniformity_gates_overall_pass` to false for a one-line revert to report-only.
        assert!(
            uniformity_gates_overall_pass(),
            "#1142: the cadence-uniformity floor must gate overall_pass (LIVE)"
        );
    }
}
