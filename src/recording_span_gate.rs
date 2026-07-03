//! #373 — the zero-loss HEADLINE analyzed-span duration gate.
//!
//! The recording verdict's headline PASS proves DELIVERY (per-node burn-id contiguity) plus the
//! #363 cam2 OPTICAL read and the #364 colour gate. None of those terms looks at how LONG the
//! analyzed optical span actually was. That left a vacuous-pass hole (#373): when the cam2 optical
//! dual-QR read COLLAPSES — it dies partway and never recovers, or only decodes a small cluster
//! near the start, or never decodes at all — the analyzed span shrinks to a handful of frames (or
//! to nothing). Over that truncated span the in-span undecodable count is 0 and the burn window is
//! trivially contiguous, so the headline would PASS over a few frames, violating the user's hard
//! ">= 300 s zero-loss" bar. A fake green.
//!
//! This module is the PURE decision that closes the hole: the analyzed optical span must be at
//! least `min_secs` long for a zero-loss headline. It is cross-platform (no probe deps), so it
//! unit-tests Tier-0 on default features; the probe-gated `bin/recording-verdict` computes each
//! node's optical-span frame count and feeds it here to gate the headline.
//!
//! RECONCILIATION with the e2e acceptance criteria (the analyzed span is "DIAGNOSTIC only — it
//! does NOT gate the headline"): that statement guards against FAILING a genuine, full-length run
//! merely because the per-recording 60->30 beat / undecodable diagnostics printed a short-window
//! NOTE. #373 is the OPPOSITE failure — a run whose optical span COLLAPSED (no real signal)
//! vacuously PASSING. A duration FLOOR rejects only the collapsed/partial read; it never fails a
//! real >= `min_secs` run. So both hold: the per-recording beat metrics stay diagnostic, and a
//! collapsed analyzed span can no longer manufacture a zero-loss headline.

/// The analyzed optical span in seconds for a recording node: the number of recorded frames in the
/// cam2 optical span (the FIRST..=LAST frame whose dual-QR decoded) divided by the camera capture
/// rate. Returns 0.0 when there is no optical frame (`span_frames == 0`) or the capture rate is
/// non-positive (a guard so a bad/zero fps can never read as an infinite span).
pub fn span_secs(span_frames: usize, capture_fps: f64) -> f64 {
    if capture_fps > 0.0 {
        span_frames as f64 / capture_fps
    } else {
        0.0
    }
}

/// #373 — the zero-loss HEADLINE duration gate: is the analyzed optical span long enough to declare
/// zero loss? A collapsed or partial optical read (`analyzed_span_secs` far below `min_secs`,
/// including a 0-second / no-signal span) must FAIL so the headline cannot vacuously pass over a
/// handful of frames. Returns `true` only when the span is at least the floor (`>= min_secs`).
pub fn analyzed_span_long_enough(analyzed_span_secs: f64, min_secs: f64) -> bool {
    analyzed_span_secs >= min_secs
}

/// #373 — the CAPTURE rate to divide a node's `optical_span_frames` by, when converting to the
/// analyzed-span SECONDS the duration floor gates on. The nodes do NOT share one rate: cam1's burn
/// (and its optical span) is read from the CLEAN strih recording (#133), captured at `capture_fps`
/// (60 on the rig); strih and stream are read from the stream recording, captured at
/// `stream_capture_fps` (30). Dividing every node by ONE rate mis-scales the others: with the rig's
/// `--capture-fps 60`, a real 300 s strih/stream span (9000 frames @ 30 fps) would read 150 s and
/// FALSE-FAIL the >=300 s floor — failing exactly the genuine zero-loss runs the floor must pass.
/// So cam1 uses `capture_fps`; every other node uses `stream_capture_fps`.
pub fn node_capture_fps(node: &str, capture_fps: f64, stream_capture_fps: f64) -> f64 {
    if node == "cam1" {
        capture_fps
    } else {
        stream_capture_fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 30.0;
    const MIN: f64 = 300.0; // the user's hard >= 300 s zero-loss floor

    #[test]
    fn span_secs_is_frames_over_fps_with_a_zero_fps_guard() {
        assert_eq!(span_secs(0, FPS), 0.0, "no optical frame ⇒ 0 s");
        assert_eq!(span_secs(150, FPS), 5.0, "150 frames @ 30 fps ⇒ 5 s");
        assert_eq!(span_secs(9000, FPS), 300.0, "9000 frames @ 30 fps ⇒ 300 s");
        assert_eq!(
            span_secs(100, 0.0),
            0.0,
            "a non-positive fps must read as 0 s, never an infinite span"
        );
    }

    #[test]
    fn collapsed_optical_span_must_fail_the_headline_373() {
        // The #373 live failure: the cam2 optical read collapsed → 0 in-span frames → 0 s. The
        // contiguity/optical/colour gates are all vacuously satisfied over the empty span, so WITHOUT
        // this duration gate the headline PASSES (a fake green). It MUST fail.
        assert!(
            !analyzed_span_long_enough(span_secs(0, FPS), MIN),
            "a collapsed (0-frame) optical span must NOT clear the >=300 s headline floor (#373)"
        );
    }

    #[test]
    fn partial_early_cluster_below_floor_must_fail_the_headline_373() {
        // The optical read only decoded a small cluster near the start (e.g. 150 frames = 5 s before
        // a green-cast camera killed the read). Over that truncated span undecodable==0 and the burns
        // are contiguous — a vacuous pass without the duration floor. It MUST fail.
        assert!(
            !analyzed_span_long_enough(span_secs(150, FPS), MIN),
            "a 5 s partial optical span (< 300 s) must NOT clear the headline floor (#373)"
        );
        assert!(
            !analyzed_span_long_enough(299.9, MIN),
            "just below the floor must still FAIL"
        );
    }

    #[test]
    fn node_capture_fps_uses_the_per_recording_rate_not_one_shared_rate_373() {
        // The rig passes --capture-fps 60 (the strih recording's cam1 diagnostic rate). cam1 is read
        // from the 60 fps strih recording; strih + stream from the 30 fps stream recording. A single
        // rate for all nodes false-fails strih/stream.
        let (cap, stream_cap) = (60.0, 30.0);
        assert_eq!(
            node_capture_fps("cam1", cap, stream_cap),
            60.0,
            "cam1's optical span comes from the 60 fps strih recording"
        );
        assert_eq!(
            node_capture_fps("strih", cap, stream_cap),
            30.0,
            "strih is read from the 30 fps stream recording"
        );
        assert_eq!(
            node_capture_fps("stream", cap, stream_cap),
            30.0,
            "stream is read from the 30 fps stream recording"
        );
        // The bug this locks: a real 300 s strih span = 9000 frames @ 30 fps. Scaled by the WRONG
        // rate (60) it reads 150 s and FALSE-FAILS the floor; by the right rate (30) it reads 300 s
        // and PASSES.
        let strih_frames = 9000;
        assert!(
            !analyzed_span_long_enough(
                span_secs(strih_frames, node_capture_fps("strih", cap, cap)), // WRONG: 60
                MIN
            ),
            "wrong shared rate would false-fail a real 300 s strih run"
        );
        assert!(
            analyzed_span_long_enough(
                span_secs(strih_frames, node_capture_fps("strih", cap, stream_cap)), // RIGHT: 30
                MIN
            ),
            "the per-recording rate passes the real 300 s strih run"
        );
    }

    #[test]
    fn node_capture_fps_treats_cam3_and_cam4_like_cam1_24() {
        // #24 — cam3/cam4 extend the #186 digital-burn contiguity check and occupy the SAME
        // "camera under test" role as cam1 (recording-verdict.rs's `CAMERA_UNDER_TEST_NODES`):
        // their burn is ALSO read from the clean strih recording (#133), captured at
        // `capture_fps` — never the 30 fps stream recording strih/stream read from. Before this
        // fix only the literal "cam1" got the right rate; cam3/cam4 fell through to
        // `stream_capture_fps`, mis-scaling their #373 analyzed-span floor exactly like the
        // single-shared-rate bug this module's headline doc describes for strih/stream.
        let (cap, stream_cap) = (60.0, 30.0);
        assert_eq!(
            node_capture_fps("cam3", cap, stream_cap),
            60.0,
            "cam3's optical span ALSO comes from the 60 fps strih recording, like cam1"
        );
        assert_eq!(
            node_capture_fps("cam4", cap, stream_cap),
            60.0,
            "cam4's optical span ALSO comes from the 60 fps strih recording, like cam1"
        );
    }

    #[test]
    fn a_real_full_length_run_still_passes_the_floor() {
        // A genuine >= 300 s run must NOT be failed by the duration floor (the reconciliation: the
        // floor only rejects a COLLAPSED span, never a real run).
        assert!(
            analyzed_span_long_enough(span_secs(9000, FPS), MIN),
            "a 300 s run (9000 frames @ 30 fps) clears the floor"
        );
        assert!(
            analyzed_span_long_enough(span_secs(54000, FPS), MIN),
            "a 1800 s (30 min) run clears the floor"
        );
        assert!(
            analyzed_span_long_enough(MIN, MIN),
            "exactly the floor passes (>=)"
        );
    }
}
