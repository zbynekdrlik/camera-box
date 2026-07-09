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
//! **PR A does NOT gate the headline on this** — `all_cambox_av_sync` is reported (offsets,
//! sample counts, any UNKNOWN cameras) but the run's overall PASS/FAIL is untouched. PR B (#312
//! item 2 / #624 deliverable 4) wires the ±20ms cross-window bound on top of [`CameraAvSync`].

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
}
