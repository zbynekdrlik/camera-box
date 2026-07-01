//! #365 — frozen-camera freshness gate (pure Tier-0 decision).
//!
//! The recording-e2e harness hashes each camera's raw NDI input from OBS
//! `GetSourceScreenshot` at a ~1 s cadence and feeds the ordered per-camera hash
//! timelines here for a verdict. A camera whose hash is **unchanged for >
//! `threshold` consecutive samples** is FROZEN.
//!
//! **Why raw NDI inputs, not the Multiview tile?**
//! The Multiview tiles on strih contain animating overlays (AbleSet spinner, CG
//! text) that keep updating even when the underlying camera NDI is stuck — so
//! hashing a Multiview tile would *false-pass* a genuinely frozen camera.
//! Hashing the raw source (`NDI cam1`, `NDI cam2`, …) via `GetSourceScreenshot`
//! eliminates that ambiguity: a live-but-dark camera still has sensor noise
//! (hash changes 4/4 samples at mean luma ~5–7), while a frozen NDI repeats
//! identical bytes → identical hash → FROZEN.
//!
//! **Fail-closed rule:** a camera with < 2 successful samples (too few data to
//! prove liveness) is also classified FROZEN. An empty timeline or nearly-all-None
//! timeline likely means the source is disconnected or the Multiview projector is
//! not open (#276 precondition).
//!
//! Pure Rust, no probe deps — unit-tests Tier-0 via
//! `cargo test --lib frozen_camera # airuleset:build-ok`.
//! The probe-gated OBS I/O lives in `scripts/frozen-camera-gate.py`;
//! the cross-boundary JSON contract is pinned by `tests/harness_frozen_camera_gate.rs`.

use std::collections::HashMap;

/// Default consecutive-static threshold: a run of **> 3** identical hashes → FROZEN.
///
/// Override at runtime via the `--threshold N` CLI arg (`frozen-camera-gate` binary)
/// or the `FROZEN_CAM_THRESHOLD` env var (`frozen-camera-gate.py`).
pub const FREEZE_THRESHOLD: usize = 3;

/// The ordered hash timeline for a single camera.
///
/// Each entry is either `Some(hash_hex_str)` (successful capture) or `None` (capture
/// failed — screenshot request error or image decode error). A `None` resets the
/// consecutive-static counter, since we cannot know whether the frame changed.
pub type CameraTimeline = Vec<Option<String>>;

/// Returns the **sorted** list of camera names that are FROZEN.
///
/// A camera is FROZEN when:
/// - It has fewer than 2 successful (`Some`) samples (fail-closed — too few data to
///   prove liveness), **OR**
/// - Any consecutive run of identical (non-`None`) hashes exceeds `threshold`.
///
/// The returned list is sorted lexicographically for deterministic CLI output.
/// An empty returned list means every camera is live (PASS).
pub fn frozen_cameras(
    timelines: &HashMap<String, CameraTimeline>,
    threshold: usize,
) -> Vec<String> {
    let mut frozen: Vec<String> = timelines
        .iter()
        .filter(|(_, tl)| is_camera_frozen(tl, threshold))
        .map(|(name, _)| name.clone())
        .collect();
    frozen.sort();
    frozen
}

/// Returns `true` if the single-camera `timeline` indicates a frozen camera.
fn is_camera_frozen(timeline: &CameraTimeline, threshold: usize) -> bool {
    // Fail-closed: a camera with < 2 successful samples cannot prove liveness.
    let successful = timeline.iter().filter(|h| h.is_some()).count();
    if successful < 2 {
        return true;
    }

    // Find the longest consecutive run of identical (non-None) hashes.
    let mut max_run: usize = 0;
    let mut cur_run: usize = 0;
    let mut prev: Option<&str> = None;

    for entry in timeline {
        match entry.as_deref() {
            Some(h) => {
                if prev == Some(h) {
                    cur_run += 1;
                } else {
                    prev = Some(h);
                    cur_run = 1;
                }
                if cur_run > max_run {
                    max_run = cur_run;
                }
            }
            None => {
                // Failed capture — reset the run (cannot confirm same frame).
                prev = None;
                cur_run = 0;
            }
        }
    }

    max_run > threshold
}

// ─── unit tests ──────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn tl(items: &[Option<&str>]) -> CameraTimeline {
        items.iter().map(|o| o.map(str::to_owned)).collect()
    }

    fn verdict(timelines: &[(&str, &[Option<&str>])], threshold: usize) -> Vec<String> {
        let map: HashMap<String, CameraTimeline> = timelines
            .iter()
            .map(|(name, items)| (name.to_string(), tl(items)))
            .collect();
        frozen_cameras(&map, threshold)
    }

    /// Every hash different every sample → no frozen cameras.
    #[test]
    fn all_changing_is_pass() {
        let frozen = verdict(
            &[("NDI cam1", &[Some("a"), Some("b"), Some("c"), Some("d")])],
            3,
        );
        assert!(frozen.is_empty(), "expected PASS, got: {frozen:?}");
    }

    /// Static for exactly threshold consecutive → still OK (must *exceed*, not meet).
    #[test]
    fn static_exactly_threshold_is_ok() {
        // threshold=3 → a run of 3 is still fine
        let frozen = verdict(
            &[("NDI cam1", &[Some("a"), Some("a"), Some("a"), Some("b")])],
            3,
        );
        assert!(
            frozen.is_empty(),
            "run of 3 == threshold should not be frozen (must exceed): {frozen:?}"
        );
    }

    /// Static for > threshold consecutive → FROZEN.
    #[test]
    fn static_exceeds_threshold_is_frozen() {
        // threshold=3 → 4 identical = FROZEN
        let frozen = verdict(
            &[("NDI cam1", &[Some("x"), Some("x"), Some("x"), Some("x")])],
            3,
        );
        assert_eq!(frozen, vec!["NDI cam1"]);
    }

    /// Mixed cameras: some fresh, one frozen → FAIL naming only the frozen camera.
    #[test]
    fn mixed_cameras_names_only_frozen_one() {
        let frozen = verdict(
            &[
                ("NDI cam1", &[Some("a"), Some("b"), Some("c"), Some("d")]), // changing
                ("NDI cam2", &[Some("z"), Some("z"), Some("z"), Some("z")]), // static > 3
                ("NDI cam3", &[Some("p"), Some("q"), Some("r"), Some("s")]), // changing
            ],
            3,
        );
        assert_eq!(frozen, vec!["NDI cam2"]);
    }

    /// Fewer than 2 successful samples → fail-closed (FROZEN).
    #[test]
    fn too_few_samples_is_frozen() {
        // Only 1 successful sample
        let frozen = verdict(&[("NDI cam1", &[Some("a"), None, None, None])], 3);
        assert_eq!(frozen, vec!["NDI cam1"]);

        // Zero successful samples
        let frozen = verdict(&[("NDI cam1", &[None, None, None])], 3);
        assert_eq!(frozen, vec!["NDI cam1"]);
    }

    /// Empty timeline → fail-closed (no data = cannot prove liveness).
    #[test]
    fn empty_timeline_is_frozen() {
        let frozen = verdict(&[("NDI cam1", &[])], 3);
        assert_eq!(frozen, vec!["NDI cam1"]);
    }

    /// A `None` entry breaks the consecutive run (resets the counter).
    #[test]
    fn none_breaks_consecutive_run() {
        // "a", "a", None, "a", "a" → max run = 2 (threshold=3 → NOT frozen)
        let frozen = verdict(
            &[(
                "NDI cam1",
                &[Some("a"), Some("a"), None, Some("a"), Some("a")],
            )],
            3,
        );
        assert!(
            frozen.is_empty(),
            "None should break the run, resetting the counter: {frozen:?}"
        );
    }

    /// Output is sorted lexicographically (deterministic for CLI / assertion).
    #[test]
    fn output_is_sorted_lexicographically() {
        let frozen = verdict(
            &[
                ("NDI cam5", &[Some("x"), Some("x"), Some("x"), Some("x")]),
                ("NDI cam1", &[Some("y"), Some("y"), Some("y"), Some("y")]),
            ],
            3,
        );
        assert_eq!(frozen, vec!["NDI cam1", "NDI cam5"]);
    }

    /// Exactly 2 successful samples (the minimum) → NOT fail-closed.
    #[test]
    fn exactly_two_successful_is_not_fail_closed() {
        let frozen = verdict(&[("NDI cam1", &[Some("a"), Some("b")])], 3);
        assert!(
            frozen.is_empty(),
            "exactly 2 successful samples must not trigger fail-closed: {frozen:?}"
        );
    }

    /// threshold=0 → every run of ≥1 identical hash is FROZEN (edge case).
    #[test]
    fn threshold_zero_any_repeat_is_frozen() {
        let frozen = verdict(&[("NDI cam1", &[Some("a"), Some("a"), Some("b")])], 0);
        assert_eq!(
            frozen,
            vec!["NDI cam1"],
            "threshold=0: any repeat should be FROZEN"
        );
    }

    /// Multiple frozen cameras are ALL listed (none silently dropped).
    #[test]
    fn all_frozen_cameras_are_listed() {
        let frozen = verdict(
            &[
                ("NDI cam1", &[Some("a"), Some("a"), Some("a"), Some("a")]), // frozen
                ("NDI cam2", &[Some("b"), Some("b"), Some("b"), Some("b")]), // frozen
                ("NDI cam5", &[Some("c"), Some("d"), Some("e"), Some("f")]), // ok
            ],
            3,
        );
        assert_eq!(frozen, vec!["NDI cam1", "NDI cam2"]);
    }
}
