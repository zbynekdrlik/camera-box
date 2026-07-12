//! #312 item 2 (PR A) — per-camera A/V-sync window pooling (pure decision).
//!
//! Fuses the #188 A/V-sync measurement (cam2's dual-QR video tick vs its QPSK audio marker,
//! `crate::qpsk_marker::{av_offset_candidates, cluster_offset_ms}`) into the SAME `--switch-schedule`
//! ALL-CAMBOX sweep the #186 continuity check and the #624 per-camera latency check already
//! partition (`bin/recording-verdict.rs`'s `all_cambox_continuity` / `all_cambox_latency`).
//!
//! The per-camera candidate LISTS themselves are produced window-by-window in the probe-gated
//! `bin/recording-verdict` (it needs the decoded `RecordingFrame`s to build each window's
//! `(tick, video_ts)` samples — see [`window_ticks`]). This module holds the PURE, Tier-0
//! testable part: given a camera's per-window candidate lists (already computed), POOL them
//! into one cluster (denser than clustering each window separately — a real offset piles into
//! the SAME narrow band across every window a camera contributes, while false CRC-4 decodes and
//! wrong-lap matches keep scattering) and decide the fail-closed verdict. No probe deps, so it
//! unit-tests Tier-0 (default features) — the project's CLAUDE.md "Local Build Policy" mandate
//! (a pure seam at the crate root, mirroring `switch_latency.rs` / `colour_scale.rs`).
//!
//! **PR B wires the ±20ms cross-window bound on top of [`CameraAvSync`]** (#312 item 2 / #624
//! deliverable 4) — see [`av_offset_gate_pass`]. PR A only reported `all_cambox_av_sync` (offsets,
//! sample counts, any UNKNOWN cameras); this module now also decides the per-camera PASS/FAIL that
//! the caller (`bin/recording-verdict.rs`) folds into the run's overall verdict.

use crate::qpsk_marker::cluster_offset_ms;

/// Fail-closed floor: fewer than this many pooled candidate offsets (across every window this
/// camera contributed to) — or no dominant cluster of at least this size — and the camera's
/// verdict is [`AvSyncVerdict::Unknown`], never a fabricated number. Starting value; calibrate
/// against this PR's own live-rig data (#312 item 2 PR A live-verification step).
pub const MIN_AV_SAMPLES: usize = 8;

/// One camera's fail-closed A/V-sync verdict for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvSyncVerdict {
    /// Enough pooled candidates formed a dominant cluster — `av_offset_ms`/`mad_ms` are real.
    Measured,
    /// Zero contributing windows, too few pooled candidates, or no dominant cluster — NEVER a
    /// fabricated pass. Mirrors `colour_verify::PatchOutcome::Unsamplable` /
    /// `imag_tick_gate`'s "absent ⇒ nothing to fail on" convention: absence is reported
    /// honestly, not silently upgraded to a number.
    Unknown,
}

/// A camera's pooled-and-clustered A/V-sync measurement for one `--switch-schedule` run.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraAvSync {
    /// How many schedule windows this camera's label matched (0 ⇒ the camera never appeared in
    /// this run's sweep, e.g. a box that was down).
    pub windows: usize,
    /// Total candidate offsets pooled across every matched window, BEFORE clustering (includes
    /// the false-decode scatter — see `qpsk_marker::av_offset_candidates`'s doc comment).
    pub candidates: usize,
    /// Size of the winning (densest) cluster — 0 when [`AvSyncVerdict::Unknown`].
    pub cluster_samples: usize,
    /// `video − audio` offset in ms (>0 ⇒ video lags audio), `None` when [`AvSyncVerdict::Unknown`].
    pub av_offset_ms: Option<f64>,
    /// Median absolute deviation (ms) of the winning cluster, `None` when [`AvSyncVerdict::Unknown`].
    pub mad_ms: Option<f64>,
    pub verdict: AvSyncVerdict,
}

/// THE pure pooling + fail-closed decision: given how many schedule windows matched this camera
/// and each matched window's own candidate-offset list (already produced by
/// `qpsk_marker::av_offset_candidates` on that window's `(tick, video_ts)` samples), pool every
/// candidate into ONE list and cluster it (`qpsk_marker::cluster_offset_ms`).
///
/// `windows_matched == 0` is authoritative UNKNOWN regardless of `per_window_candidates` content
/// (a camera that never appeared in the sweep proves nothing, even if a stray candidate list were
/// passed in by mistake) — the "zero contributing windows" fail-closed case from the PR's own
/// spec. Otherwise the candidates are pooled and `cluster_offset_ms` decides: too few pooled
/// candidates OR no window of width `2 * cluster_tol_ms` containing at least `min_samples` of
/// them ⇒ [`AvSyncVerdict::Unknown`] (covers BOTH "too few pooled samples" and "no dominant
/// cluster" from the same spec, via the SAME pure kernel the whole-recording `--av-sync` mode
/// already uses — never a second decision rule that could disagree).
pub fn pool_camera_av_sync(
    windows_matched: usize,
    per_window_candidates: &[Vec<f64>],
    min_samples: usize,
    cluster_tol_ms: f64,
) -> CameraAvSync {
    let candidates: Vec<f64> = per_window_candidates.iter().flatten().copied().collect();
    let n_candidates = candidates.len();
    if windows_matched == 0 {
        return CameraAvSync {
            windows: 0,
            candidates: n_candidates,
            cluster_samples: 0,
            av_offset_ms: None,
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        };
    }
    match cluster_offset_ms(&candidates, min_samples, cluster_tol_ms) {
        Some(off) => CameraAvSync {
            windows: windows_matched,
            candidates: n_candidates,
            cluster_samples: off.matched,
            av_offset_ms: Some(off.offset_ms),
            mad_ms: Some(off.mad_ms),
            verdict: AvSyncVerdict::Measured,
        },
        None => CameraAvSync {
            windows: windows_matched,
            candidates: n_candidates,
            cluster_samples: 0,
            av_offset_ms: None,
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        },
    }
}

/// #624 deliverable 4 / #312 item 2 PR B — the per-camera A/V-offset gate tolerance: every
/// camera's measured `video − audio` offset must land within this many ms of the
/// expected/dialed value (`--av-expected-ms`, the operator's live #398 dock reading — nominally
/// ~0 since the dock is dialed to align video and audio). This exact ±20ms bound is issue #624's
/// own deliverable-4 text: "every camera's end-to-end A/V offset ... within ±20ms of every other
/// AND of the dialed 2ME value".
pub const AV_OFFSET_GATE_TOLERANCE_MS: f64 = 20.0;

/// PASS iff `sync` is [`AvSyncVerdict::Measured`] AND its offset is within
/// [`AV_OFFSET_GATE_TOLERANCE_MS`] of `expected_ms`. [`AvSyncVerdict::Unknown`] — whether from
/// zero contributing windows (the camera never appeared in this sweep) or thin/scattered data
/// (too few pooled candidates, no dominant cluster) — NEVER passes: this is a real gate now
/// (#312 item 2 PR B), same fail-closed severity as the loss/latency-spread gates, no "advisory"
/// tier. A camera with nothing to measure proves nothing about its A/V sync — it cannot pass.
///
/// Checks `sync.verdict == Measured` EXPLICITLY, not merely `av_offset_ms.is_some()` — the two
/// currently always agree ([`pool_camera_av_sync`] is the sole real producer and keeps them in
/// lockstep), but this function does not rely on that convention holding forever: a future
/// producer that ever desyncs the pair (e.g. `Unknown` with a stray `Some(x)`, or `Measured` with
/// `None`) must still resolve to fail-closed here, not silently pass on the strength of one field
/// alone.
pub fn av_offset_gate_pass(sync: &CameraAvSync, expected_ms: f64) -> bool {
    match (sync.verdict, sync.av_offset_ms) {
        (AvSyncVerdict::Measured, Some(off)) => {
            (off - expected_ms).abs() <= AV_OFFSET_GATE_TOLERANCE_MS
        }
        _ => false,
    }
}

/// Build `(tick, video_ts_s)` samples for `qpsk_marker::av_offset_candidates`'s third argument,
/// from a decoded window's `(frame_index, tick)` pairs — pure (no `RecordingFrame` dependency,
/// so it stays Tier-0 testable) mirroring the identical construction
/// `probe::av_sync_recording::av_sync_from_recording` already does for the whole-recording
/// `--av-sync` mode (LEFT UNTOUCHED): dedup to the FIRST occurrence of each tick, in ascending
/// tick order (`qpsk_marker::interp_video_ts`'s precondition — it binary-searches this list and
/// never extrapolates past its range, which is exactly what keeps a per-window restriction of
/// this list from ever mis-pairing a marker to a tick outside that window/lap).
///
/// `frames` need not be pre-sorted by `frame_index` — the tick VALUES are what get sorted at the
/// end, not the input order.
pub fn window_ticks(
    frames: &[(u64, Option<u32>)],
    fps: f64,
    video_start_s: f64,
) -> Vec<(u32, f64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(u32, f64)> = Vec::new();
    for &(frame_index, tick) in frames {
        if let Some(t) = tick {
            if seen.insert(t) {
                out.push((t, video_start_s + frame_index as f64 / fps));
            }
        }
    }
    out.sort_by_key(|&(t, _)| t);
    out
}

/// #714 — a per-camera A/V-sync estimate DERIVED (never fabricated) for a camera whose own
/// per-window pooling is sample-starved ([`AvSyncVerdict::Unknown`]), from cam2's own MEASURED
/// whole-recording offset plus the delta between this camera's OWN `#286` delivery-latency p50
/// (`all_cambox_delivery_latency`, `strih_burn − camera_burn`) and the mean p50 across every
/// camera that produced a delivery sample this run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerivedAvSync {
    pub derived_offset_ms: f64,
    /// The #286 delivery-latency cross-camera spread this run — the honest DIAGNOSTIC margin on
    /// the derivation (report-only field; see [`derive_camera_av_sync`]'s doc comment for why the
    /// gate itself still applies the SAME ±20ms tolerance as a real measurement).
    pub delivery_spread_ms: f64,
    pub gate_pass: bool,
}

/// THE pure derivation (#714).
///
/// ## Why this is sound, not fabricated — grounded in the #689 live-data finding
///
/// Every cam box receives the IDENTICAL optically-injected picture (cam2's dual-QR) via the
/// shared HDMI splitter (this project's own documented rig topology — one broadcast camera films
/// cam2's monitor, its HDMI output is physically split into every cam box's own USB capture
/// card). Live evidence (#689, 3 gate runs, 2026-07-12): the 5 non-cam2 cameras' per-window
/// candidate counts ARE non-zero and roughly proportional to their own schedule-window duration
/// (103-988 candidates vs cam2's 2013-3256 over the whole ~10x-longer recording) — the video
/// ticks genuinely decode fine inside every camera's own window. The reason none of the 5 ever
/// clears [`MIN_AV_SAMPLES`] is NOT missing signal — a single ~30-60s schedule window is simply
/// too short to accumulate enough REAL (non-false-CRC-decode) marker matches, given the QPSK
/// marker's finite occurrence rate: cam2's own whole-300s pool clears the floor at only 22-31
/// matched samples (roughly one real match every ~10-13s) — well under 8 in a single short
/// window, structurally, regardless of raw candidate count.
///
/// cam2's own whole-recording offset therefore implicitly BLENDS together whichever camera's
/// receiver-side delivery latency (`strih_burn − camera_burn`, #286's OWN Verify metric) was
/// active at each sampled instant across the whole run — cam2 has no camera-under-test delivery
/// latency of its own, so its number is an average over all 6 cameras' delivery times as they
/// rotated through program. Re-centering that blended number on THIS camera's own delivery p50,
/// relative to the mean the blend implicitly averages over, is the direct algebraic correction
/// for the one variable that genuinely differs per camera (#286's own per-source genlock hold) —
/// never a guess, and never a re-derivation of the A/V relationship itself (still cam2's own
/// measured paint-tick-vs-marker offset, only re-centered).
///
/// ## Fail-closed — `None` (never a fabricated number) when
///
/// - cam2 itself did not reach [`AvSyncVerdict::Measured`] this run (nothing to re-center), or
/// - this camera produced no #286 delivery-latency sample this run (no p50 to re-center on), or
/// - fewer than 2 cameras produced a delivery sample this run (no meaningful mean to re-center
///   against — a single-sample "mean" would just reproduce cam2's own number with zero
///   correction, silently hiding that nothing was actually derived).
pub fn derive_camera_av_sync(
    cam2_offset_ms: Option<f64>,
    camera_delivery_p50_ms: Option<f64>,
    all_delivery_p50s_ms: &[f64],
    expected_ms: f64,
) -> Option<DerivedAvSync> {
    let cam2_offset = cam2_offset_ms?;
    let cam_p50 = camera_delivery_p50_ms?;
    if all_delivery_p50s_ms.len() < 2 {
        return None;
    }
    let mean_p50 = all_delivery_p50s_ms.iter().sum::<f64>() / all_delivery_p50s_ms.len() as f64;
    let derived_offset_ms = cam2_offset + (cam_p50 - mean_p50);
    let max_p50 = all_delivery_p50s_ms
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let min_p50 = all_delivery_p50s_ms
        .iter()
        .cloned()
        .fold(f64::MAX, f64::min);
    let delivery_spread_ms = max_p50 - min_p50;
    // #714 spec: "the spread folded into the tolerance check (±20ms gate then applies to the
    // DERIVED value)" — the delivery delta is already folded INTO derived_offset_ms itself (the
    // re-centering above); the SAME tolerance the measured path uses then applies to that
    // now-per-camera-specific value. delivery_spread_ms is kept as a separate, honest DIAGNOSTIC
    // field (never silently widening or narrowing the tolerance) so a report reader can judge the
    // derivation's own confidence margin independently of the pass/fail call.
    let gate_pass = (derived_offset_ms - expected_ms).abs() <= AV_OFFSET_GATE_TOLERANCE_MS;
    Some(DerivedAvSync {
        derived_offset_ms,
        delivery_spread_ms,
        gate_pass,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // pool_camera_av_sync
    // ---------------------------------------------------------------------

    #[test]
    fn zero_windows_matched_is_unknown_even_with_stray_candidates() {
        // The "zero contributing windows" fail-closed case: even if a candidate list were handed
        // in (should never happen in real wiring — the caller only pushes a window's candidates
        // when it actually matched), windows_matched=0 is authoritative.
        let v = pool_camera_av_sync(0, &[vec![10.0, 11.0, 9.0, 10.5]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.windows, 0);
        assert_eq!(v.av_offset_ms, None);
        assert_eq!(v.mad_ms, None);
        assert_eq!(v.cluster_samples, 0);
    }

    #[test]
    fn too_few_pooled_candidates_is_unknown() {
        // 3 candidates pooled, need MIN_AV_SAMPLES(8) — Unknown, not a fabricated number from 3.
        let v = pool_camera_av_sync(2, &[vec![10.0, 11.0], vec![9.5]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.candidates, 3);
        assert_eq!(v.windows, 2);
        assert_eq!(v.av_offset_ms, None);
    }

    #[test]
    fn dense_cluster_pooled_across_windows_is_measured() {
        // Two windows each contributing a few real candidates (tight cluster around ~800ms) plus
        // scattered false decodes — pooling across windows is what gets to MIN_AV_SAMPLES(8) real
        // candidates; neither window alone would clear the floor.
        let win_a = vec![798.0, 801.0, 799.5, 800.5, 5000.0]; // 4 real + 1 scattered false decode
        let win_b = vec![800.0, 802.0, 797.0, 799.0, -3000.0]; // 4 real + 1 scattered false decode
        let v = pool_camera_av_sync(2, &[win_a, win_b], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Measured);
        assert_eq!(v.windows, 2);
        assert_eq!(v.candidates, 10);
        assert_eq!(
            v.cluster_samples, 8,
            "the 8 real candidates form the dense cluster"
        );
        let off = v.av_offset_ms.expect("measured");
        assert!(
            (799.0..=801.0).contains(&off),
            "offset should land in the real cluster's range: {off}"
        );
    }

    #[test]
    fn scattered_candidates_with_no_dominant_cluster_is_unknown() {
        // 8 candidates (clears the count floor) but spread out with no window containing 8 of
        // them within ±60ms of each other — no dominant cluster ⇒ Unknown, never a wrong guess.
        let scattered = vec![0.0, 200.0, 400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0];
        let v = pool_camera_av_sync(1, &[scattered], MIN_AV_SAMPLES, 60.0);
        assert_eq!(v.verdict, AvSyncVerdict::Unknown);
        assert_eq!(v.candidates, 8);
    }

    #[test]
    fn min_av_samples_constant_is_the_documented_starting_value() {
        // Locks the constant so a casual future edit is visible in the diff/review, not silent.
        assert_eq!(MIN_AV_SAMPLES, 8);
    }

    // ---------------------------------------------------------------------
    // window_ticks
    // ---------------------------------------------------------------------

    #[test]
    fn window_ticks_dedups_to_first_occurrence_sorted_ascending() {
        // frame_index 5 and 9 both decode tick 100 — only the FIRST (by input order) survives,
        // matching interp_video_ts's precondition of one video_ts per tick. Input order here is
        // deliberately NOT frame-index-sorted, to prove the function sorts the OUTPUT by tick,
        // not by input position.
        let frames = vec![(9, Some(100)), (5, Some(100)), (7, Some(101)), (3, None)];
        let ticks = window_ticks(&frames, 30.0, 0.0);
        assert_eq!(
            ticks,
            vec![(100, 9.0 / 30.0), (101, 7.0 / 30.0)],
            "kept the FIRST occurrence per tick (input order), sorted ascending by tick value"
        );
    }

    #[test]
    fn window_ticks_excludes_frames_with_no_decoded_tick() {
        let frames = vec![(0, None), (1, None), (2, Some(50))];
        let ticks = window_ticks(&frames, 60.0, 0.0);
        assert_eq!(ticks, vec![(50, 2.0 / 60.0)]);
    }

    #[test]
    fn window_ticks_applies_the_video_start_offset() {
        let frames = vec![(0, Some(1))];
        let ticks = window_ticks(&frames, 30.0, 12.5);
        assert_eq!(
            ticks,
            vec![(1, 12.5)],
            "frame_index 0 lands exactly at video_start_s"
        );
    }

    #[test]
    fn window_ticks_on_empty_input_is_empty() {
        assert_eq!(window_ticks(&[], 60.0, 0.0), Vec::<(u32, f64)>::new());
    }

    // ---------------------------------------------------------------------
    // av_offset_gate_pass — #624 deliverable 4 / #312 item 2 PR B
    // ---------------------------------------------------------------------

    fn measured(av_offset_ms: f64) -> CameraAvSync {
        CameraAvSync {
            windows: 2,
            candidates: 10,
            cluster_samples: 10,
            av_offset_ms: Some(av_offset_ms),
            mad_ms: Some(1.0),
            verdict: AvSyncVerdict::Measured,
        }
    }

    #[test]
    fn gate_passes_when_offset_within_tolerance_of_expected() {
        // 15ms deviation from 0 is well inside the ±20ms bound.
        assert!(av_offset_gate_pass(&measured(15.0), 0.0));
        // Negative deviation, same bound.
        assert!(av_offset_gate_pass(&measured(-15.0), 0.0));
    }

    #[test]
    fn gate_fails_when_offset_outside_tolerance_of_expected() {
        assert!(!av_offset_gate_pass(&measured(25.0), 0.0));
        assert!(!av_offset_gate_pass(&measured(-25.0), 0.0));
    }

    #[test]
    fn gate_measures_deviation_from_a_nonzero_expected_value_not_hardcoded_zero() {
        // The operator's live #398 dock may be dialed to a nonzero value — the gate must measure
        // deviation FROM THAT expected value, never from a hardcoded 0.
        assert!(av_offset_gate_pass(&measured(55.0), 50.0));
        assert!(!av_offset_gate_pass(&measured(55.0), 0.0));
    }

    #[test]
    fn gate_boundary_at_exactly_plus_tolerance_passes() {
        assert!(
            av_offset_gate_pass(&measured(AV_OFFSET_GATE_TOLERANCE_MS), 0.0),
            "the bound is <=, not <"
        );
    }

    #[test]
    fn gate_boundary_at_exactly_minus_tolerance_passes() {
        assert!(av_offset_gate_pass(
            &measured(-AV_OFFSET_GATE_TOLERANCE_MS),
            0.0
        ));
    }

    #[test]
    fn gate_just_outside_the_boundary_fails() {
        assert!(!av_offset_gate_pass(
            &measured(AV_OFFSET_GATE_TOLERANCE_MS + 0.01),
            0.0
        ));
    }

    #[test]
    fn gate_fails_closed_on_unknown_verdict_from_thin_data() {
        // The camera DID appear in the sweep (windows_matched=1) but too few pooled candidates to
        // clear MIN_AV_SAMPLES — Unknown. Must fail, never a fabricated pass.
        let thin = pool_camera_av_sync(1, &[vec![10.0, 11.0]], MIN_AV_SAMPLES, 60.0);
        assert_eq!(thin.verdict, AvSyncVerdict::Unknown);
        assert!(!av_offset_gate_pass(&thin, 0.0));
    }

    #[test]
    fn gate_fails_closed_when_camera_never_appeared_in_the_sweep() {
        let absent = pool_camera_av_sync(0, &[], MIN_AV_SAMPLES, 60.0);
        assert_eq!(absent.verdict, AvSyncVerdict::Unknown);
        assert!(!av_offset_gate_pass(&absent, 0.0));
    }

    /// Code-review finding: `av_offset_gate_pass` must check `verdict == Measured` EXPLICITLY,
    /// not merely `av_offset_ms.is_some()` — the two fields currently always agree in real
    /// producers, but this locks the gate against a hand-constructed (or future-buggy) mismatch:
    /// an `Unknown` verdict carrying a stray `Some(offset)` must still fail closed.
    #[test]
    fn gate_fails_closed_on_unknown_verdict_even_with_a_stray_offset_value() {
        let mismatched = CameraAvSync {
            windows: 1,
            candidates: 3,
            cluster_samples: 0,
            av_offset_ms: Some(10.0), // stray value inconsistent with Unknown
            mad_ms: None,
            verdict: AvSyncVerdict::Unknown,
        };
        assert!(
            !av_offset_gate_pass(&mismatched, 0.0),
            "an Unknown verdict must never pass the gate, even if av_offset_ms happens to be Some"
        );
    }

    // ---------------------------------------------------------------------
    // derive_camera_av_sync (#714)
    // ---------------------------------------------------------------------

    #[test]
    fn derive_none_when_cam2_itself_is_not_measured() {
        let v = derive_camera_av_sync(None, Some(980.0), &[960.0, 970.0, 980.0], 0.0);
        assert_eq!(
            v, None,
            "no cam2 offset to re-center ⇒ never fabricate a number"
        );
    }

    #[test]
    fn derive_none_when_this_camera_has_no_delivery_sample() {
        let v = derive_camera_av_sync(Some(-12.0), None, &[960.0, 970.0, 980.0], 0.0);
        assert_eq!(
            v, None,
            "no delivery p50 for THIS camera ⇒ nothing to re-center against"
        );
    }

    #[test]
    fn derive_none_when_fewer_than_two_cameras_have_delivery_samples() {
        // Only 1 camera's delivery sample exists this run — a "mean" of one value would just
        // reproduce cam2's own number with zero correction, silently hiding that nothing was
        // actually derived.
        let v = derive_camera_av_sync(Some(-12.0), Some(980.0), &[980.0], 0.0);
        assert_eq!(v, None);
        let v_empty = derive_camera_av_sync(Some(-12.0), Some(980.0), &[], 0.0);
        assert_eq!(v_empty, None);
    }

    #[test]
    fn derive_recenters_cam2_offset_on_this_cameras_own_delivery_delta() {
        // mean p50 across the 6 cameras = 970ms; this camera's own p50 = 980ms (10ms ABOVE
        // the mean, i.e. this camera's frames sat in strih's queue 10ms longer than average) ⇒
        // derived = cam2_offset + (980 - 970) = cam2_offset + 10.
        let p50s = [960.0, 965.0, 970.0, 975.0, 980.0, 970.0]; // mean = 970.0
        let v = derive_camera_av_sync(Some(-12.0), Some(980.0), &p50s, 0.0)
            .expect("all inputs present ⇒ a derived estimate");
        assert!(
            (v.derived_offset_ms - (-2.0)).abs() < 1e-9,
            "expected -12.0 + (980.0 - 970.0) = -2.0, got {}",
            v.derived_offset_ms
        );
        assert!(
            (v.delivery_spread_ms - 20.0).abs() < 1e-9,
            "spread = max(980) - min(960) = 20.0, got {}",
            v.delivery_spread_ms
        );
    }

    #[test]
    fn derive_gate_pass_boundary_matches_the_measured_paths_own_tolerance() {
        let p50s = [970.0, 970.0]; // mean = 970.0, zero delta for THIS camera below
                                   // derived offset lands EXACTLY on the ±20ms boundary ⇒ still PASS (<=, matching
                                   // av_offset_gate_pass's own inclusive boundary).
        let at_boundary =
            derive_camera_av_sync(Some(20.0), Some(970.0), &p50s, 0.0).expect("inputs present");
        assert!(
            (at_boundary.derived_offset_ms - 20.0).abs() < 1e-9,
            "zero delivery delta ⇒ derived == cam2's own offset"
        );
        assert!(
            at_boundary.gate_pass,
            "exactly at tolerance must still PASS"
        );

        let just_over =
            derive_camera_av_sync(Some(20.1), Some(970.0), &p50s, 0.0).expect("inputs present");
        assert!(!just_over.gate_pass, "just over tolerance must FAIL");
    }

    #[test]
    fn derive_gate_fails_when_the_re_centered_offset_exceeds_tolerance() {
        // cam2's own offset is safely inside tolerance (5ms), but this camera's delivery p50 is
        // 30ms above the mean ⇒ the re-centered estimate (35ms) exceeds ±20ms — the derivation
        // must FAIL here even though cam2's own measured number would have passed.
        let p50s = [940.0, 970.0, 1000.0]; // mean = 970.0
        let v = derive_camera_av_sync(Some(5.0), Some(1000.0), &p50s, 0.0).expect("inputs present");
        assert!(
            (v.derived_offset_ms - 35.0).abs() < 1e-9,
            "expected 5.0 + (1000.0 - 970.0) = 35.0, got {}",
            v.derived_offset_ms
        );
        assert!(
            !v.gate_pass,
            "a re-centered offset outside ±20ms must FAIL, independent of cam2's own PASS"
        );
    }

    #[test]
    fn derive_respects_a_nonzero_expected_ms() {
        // expected_ms is the operator's dialed value (nominally ~0, but the gate must honor
        // whatever --av-expected-ms was actually passed).
        let p50s = [970.0, 970.0];
        let v =
            derive_camera_av_sync(Some(500.0), Some(970.0), &p50s, 500.0).expect("inputs present");
        assert!(
            v.gate_pass,
            "derived offset == expected_ms exactly ⇒ PASS regardless of the absolute value"
        );
    }
}
