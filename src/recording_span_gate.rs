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

/// #373 — the node labels whose burn is read from the CLEAN strih recording (#133) at
/// `capture_fps`, rather than from the stream recording at `stream_capture_fps`. cam1 is the
/// original "camera under test"; #24 extends the SAME role to cam3/cam4 (mutually exclusive in
/// any real run — see `recording-verdict.rs`'s `CAMERA_UNDER_TEST_NODES`, which this mirrors).
const CAMERA_UNDER_TEST_NODES: [&str; 3] = ["cam1", "cam3", "cam4"];

/// #461 — the imag-nb node label (EPIC #466 Topology v2). imag records ON ITS OWN recording at
/// its own rate (60fps, low-latency IMAG) — neither the camera-under-test's `capture_fps` (read
/// from the strih recording) nor the stream box's `stream_capture_fps` (30). A third named rate
/// slot, mirroring the same "nodes do NOT share one rate" reasoning `node_capture_fps` already
/// applies to cam1/cam3/cam4 vs strih/stream.
const IMAG_NODE: &str = "imag";

/// #373 — the CAPTURE rate to divide a node's `optical_span_frames` by, when converting to the
/// analyzed-span SECONDS the duration floor gates on. The nodes do NOT share one rate: the
/// camera-under-test's burn (and its optical span) is read from the CLEAN strih recording (#133),
/// captured at `capture_fps` (60 on the rig); strih and stream are read from the stream recording,
/// captured at `stream_capture_fps` (30); imag (#461) is read from its OWN recording, captured at
/// `imag_capture_fps` (60, low-latency IMAG — its own box, its own rate, never the stream box's).
/// Dividing every node by ONE rate mis-scales the others: with the rig's `--capture-fps 60`, a
/// real 300 s strih/stream span (9000 frames @ 30 fps) would read 150 s and FALSE-FAIL the
/// 300-second-minimum floor — failing exactly the genuine zero-loss runs the floor must pass. So
/// the camera-under-test (cam1/cam3/cam4, #24) uses `capture_fps`; strih and stream use
/// `stream_capture_fps`; imag (#461) uses `imag_capture_fps`.
pub fn node_capture_fps(
    node: &str,
    capture_fps: f64,
    stream_capture_fps: f64,
    imag_capture_fps: f64,
) -> f64 {
    if node == IMAG_NODE {
        imag_capture_fps
    } else if CAMERA_UNDER_TEST_NODES.contains(&node) {
        capture_fps
    } else {
        stream_capture_fps
    }
}

/// #467 — the painted (cam2 optical Vernier) tick's by-design STEP for a node's OWN recording,
/// derived from that recording's native capture rate relative to the 60Hz painter (`refresh_hz`):
/// `round(refresh_hz / capture_fps)`, floored at 1 — a `capture_fps >= refresh_hz` (or a
/// non-positive rate) is never decimated, so full-rate is step 1.
///
/// This is the SAME formula `recording-verdict`'s #312 ALL-CAMBOX `--switch-schedule` sweep
/// already computed INLINE (un-extracted, untestable under the probe feature gate) for the stream
/// recording's step (60Hz painter -> 30fps stream capture = step 2); pulled out here so it is
/// unit-tested ONCE, Tier-0, on default features, and reused for a SECOND node whose own
/// recording runs at a DIFFERENT native rate — imag-nb (#467, EPIC #466 Topology v2), which
/// records the SAME 60Hz painter at its own 60fps low-latency rate (`imag_capture_fps`), so its
/// by-design step is 1 (no decimation), never the stream recording's 2. `recording-verdict` calls
/// this for BOTH: the stream sweep (`painted_tick_step(refresh_hz, stream_capture_fps)`) and
/// imag's own per-segment sweep (`painted_tick_step(refresh_hz, imag_capture_fps)`).
pub fn painted_tick_step(refresh_hz: f64, capture_fps: f64) -> i64 {
    if capture_fps > 0.0 {
        (refresh_hz / capture_fps).round().max(1.0) as i64
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 30.0;
    const MIN: f64 = 300.0; // the user's hard >= 300 s zero-loss floor
    const IMAG_FPS: f64 = 60.0; // #461 imag-nb's own recording rate

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
            node_capture_fps("cam1", cap, stream_cap, IMAG_FPS),
            60.0,
            "cam1's optical span comes from the 60 fps strih recording"
        );
        assert_eq!(
            node_capture_fps("strih", cap, stream_cap, IMAG_FPS),
            30.0,
            "strih is read from the 30 fps stream recording"
        );
        assert_eq!(
            node_capture_fps("stream", cap, stream_cap, IMAG_FPS),
            30.0,
            "stream is read from the 30 fps stream recording"
        );
        // The bug this locks: a real 300 s strih span = 9000 frames @ 30 fps. Scaled by the WRONG
        // rate (60) it reads 150 s and FALSE-FAILS the floor; by the right rate (30) it reads 300 s
        // and PASSES.
        let strih_frames = 9000;
        assert!(
            !analyzed_span_long_enough(
                span_secs(strih_frames, node_capture_fps("strih", cap, cap, IMAG_FPS)), // WRONG: 60
                MIN
            ),
            "wrong shared rate would false-fail a real 300 s strih run"
        );
        assert!(
            analyzed_span_long_enough(
                span_secs(
                    strih_frames,
                    node_capture_fps("strih", cap, stream_cap, IMAG_FPS)
                ), // RIGHT: 30
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
            node_capture_fps("cam3", cap, stream_cap, IMAG_FPS),
            60.0,
            "cam3's optical span ALSO comes from the 60 fps strih recording, like cam1"
        );
        assert_eq!(
            node_capture_fps("cam4", cap, stream_cap, IMAG_FPS),
            60.0,
            "cam4's optical span ALSO comes from the 60 fps strih recording, like cam1"
        );
    }

    #[test]
    fn node_capture_fps_returns_imags_own_rate_461() {
        // #461 — imag-nb (EPIC #466 Topology v2) records ON ITS OWN box at ITS OWN rate: neither
        // the camera-under-test's `capture_fps` (60, but read from the STRIH recording) nor the
        // stream box's `stream_capture_fps` (30). Using either would mis-scale imag's #373
        // analyzed-span floor exactly like the single-shared-rate bug this module exists to fix.
        let (cap, stream_cap, imag_cap) = (60.0, 30.0, 60.0);
        assert_eq!(
            node_capture_fps("imag", cap, stream_cap, imag_cap),
            60.0,
            "imag's optical span comes from ITS OWN 60 fps recording, not strih's or stream's"
        );
        // Even when capture_fps/stream_capture_fps are set to something else entirely, imag must
        // resolve to imag_capture_fps — never fall through to either other rate.
        assert_eq!(
            node_capture_fps("imag", 15.0, 7.5, 60.0),
            60.0,
            "imag must resolve to imag_capture_fps regardless of the other two rates"
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

    // ---- #467 painted_tick_step ----

    #[test]
    fn painted_tick_step_60_to_30_is_2_the_stream_recording_decimation() {
        assert_eq!(
            painted_tick_step(60.0, 30.0),
            2,
            "the stream recording captures the 60Hz painter at 30fps -> decimation step 2"
        );
    }

    #[test]
    fn painted_tick_step_60_to_60_is_1_imags_own_native_rate_467() {
        // #467 — imag-nb records the SAME 60Hz painter at ITS OWN 60fps native rate: no
        // decimation, step 1 (unlike the stream recording's step-2 above).
        assert_eq!(
            painted_tick_step(60.0, 60.0),
            1,
            "imag captures the 60Hz painter 1:1 at its own 60fps -> no decimation, step 1"
        );
    }

    #[test]
    fn painted_tick_step_non_positive_capture_fps_floors_to_1() {
        assert_eq!(
            painted_tick_step(60.0, 0.0),
            1,
            "zero capture_fps -> step 1"
        );
        assert_eq!(
            painted_tick_step(60.0, -5.0),
            1,
            "negative capture_fps -> step 1"
        );
    }

    #[test]
    fn painted_tick_step_capture_faster_than_refresh_still_floors_to_1() {
        // A capture rate AT OR ABOVE the painter's refresh (e.g. captured at 120 while the
        // painter runs at 60) rounds to 0 or below -- must floor at 1, never 0 (a step of 0 would
        // divide-by-zero-equivalent every gap in `segment_continuity`'s excess-gap math).
        assert_eq!(
            painted_tick_step(60.0, 120.0),
            1,
            "capture_fps >= refresh_hz must never floor below 1"
        );
    }

    #[test]
    fn painted_tick_step_rounds_to_nearest_not_truncates() {
        // 60/45 = 1.333.. rounds to 1 (nearest), not truncated differently.
        assert_eq!(painted_tick_step(60.0, 45.0), 1);
        // 60/40 = 1.5 rounds to 2 (round-half-away-from-zero, Rust's f64::round default).
        assert_eq!(painted_tick_step(60.0, 40.0), 2);
    }
}
