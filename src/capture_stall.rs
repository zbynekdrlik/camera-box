//! #707 — V4L2 capture DEQUEUE stall diagnostic (pure decision).
//!
//! `VideoCapture::process_frame` (src/capture.rs) calls the blocking V4L2 `self.stream.next()`
//! (a `VIDIOC_DQBUF` under the hood) to obtain the next captured buffer. That call is NOT bounded
//! by our own code: the kernel driver only returns once a buffer completes, which is normally
//! sub-millisecond on a healthy USB isochronous stream but can block far longer when the driver's
//! own URB completion handler hits a transfer error and has to recover (confirmed live on CAM1's
//! Elgato 4K S, 2026-07-14: `dmesg` shows recurring `uvcvideo … Non-zero status (-71) in video
//! completion handler` — a USB isochronous completion error — roughly every 20-90 minutes across
//! a ~30h uptime window, with NO corresponding `v4l2_dropped` increment, i.e. the driver always
//! eventually delivers the buffer, just sometimes late).
//!
//! This closes the SAME observability gap [`crate::send_stall`] closed for the NDI send side, but
//! for the OTHER end of the pipeline: given how long a SINGLE `process_frame` dequeue actually
//! took (wall-clock, measured at the call site in `capture.rs`) and the capture device's own
//! configured frame interval, decide whether THIS ONE dequeue is a genuine stall worth a WARN. The
//! #707 CAM1 residual investigation (2026-07-14) found `v4l2_dropped=0` on every real gate run AND
//! zero [`crate::send_stall`] WARNs during a run that still showed a real `all_cambox_continuity`
//! copies/gaps residual — ruling out both true V4L2 frame loss and NDI-send blocking as the
//! CURRENT dominant mechanism for at least that occurrence. This diagnostic is the missing piece
//! that can confirm (or rule out) a capture-side dequeue stall on the NEXT natural recurrence:
//! if it fires at the same time as a fresh `all_cambox_delivery_latency` spike, the blocking V4L2
//! dequeue (i.e. the USB/driver layer) is the confirmed mechanism; if the residual recurs with NO
//! dequeue-stall WARNs either, the cause lies elsewhere entirely (strih-side presentation cadence,
//! per #726's own already-established correlation with this exact ticket).

/// A single blocking dequeue call counts as a "stall" once it takes at least this many multiples
/// of the capture device's own configured frame interval. Reuses [`crate::send_stall::
/// SEND_STALL_FACTOR`]'s exact value (1.5x) — the SAME kind of "did this ONE blocking call take
/// too long" decision, on the other end of the same per-frame pipeline, so the same margin above
/// ordinary scheduling jitter applies for the same reason.
pub const CAPTURE_STALL_FACTOR: f64 = 1.5;

/// Pure decision: did this ONE blocking dequeue call stall? `duration_ms` is the measured
/// wall-clock time `process_frame`'s `self.stream.next()` call took; `frame_interval_ms` is
/// `1000.0 / configured_capture_fps`.
///
/// A non-positive `frame_interval_ms` (capture fps unknown/zero) never stalls — there is no
/// per-frame budget to have blown. A non-finite or negative `duration_ms` (should never happen
/// from a real `Instant::elapsed()` reading, but a defensive guard costs nothing) also never
/// stalls — never fabricate a WARN from a bad measurement. Mirrors [`crate::send_stall::
/// is_send_stall`]'s exact guard shape.
pub fn is_capture_stall(duration_ms: f64, frame_interval_ms: f64) -> bool {
    if frame_interval_ms <= 0.0 || !duration_ms.is_finite() || duration_ms < 0.0 {
        return false;
    }
    duration_ms >= frame_interval_ms * CAPTURE_STALL_FACTOR
}

/// Build the WARN message for a confirmed capture-dequeue stall — pure string formatting so the
/// exact wording is unit-tested here rather than only visible in a live log stream. No per-box
/// source-name parameter (unlike [`crate::send_stall::send_stall_warning`]) — this fires from
/// `main.rs`'s own capture loop, which already logs on that box's own per-host journal (mirrors
/// the existing `#707 genlock emit-gate SKIPPED …` WARN's convention, which is also unattributed
/// for the same reason).
pub fn capture_stall_warning(
    duration_ms: f64,
    frame_interval_ms: f64,
    configured_fps: f64,
) -> String {
    format!(
        "#707 V4L2 capture DEQUEUE STALL: {duration_ms:.1}ms (configured frame interval \
         {frame_interval_ms:.1}ms @ {configured_fps:.1}fps, >= {CAPTURE_STALL_FACTOR:.1}x budget) \
         — the blocking V4L2 dequeue (VIDIOC_DQBUF) itself took this long, i.e. this frame's delay \
         traces to the capture device/driver/USB layer, not NDI send or scheduling downstream of \
         it (see #707)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // is_capture_stall — the pure threshold decision.

    #[test]
    fn fast_dequeue_well_under_one_frame_interval_is_not_a_stall() {
        // A healthy V4L2 dequeue with a buffer already ready: 60fps -> 16.7ms interval, call
        // returns in under 1ms.
        assert!(!is_capture_stall(0.5, 16.7));
    }

    #[test]
    fn call_at_exactly_the_factor_boundary_is_a_stall() {
        // 16.7ms interval * 1.5 = 25.05ms — AT the boundary counts as a stall (inclusive, matches
        // send_stall's own inclusive-tolerance convention).
        let interval = 16.7;
        assert!(is_capture_stall(interval * CAPTURE_STALL_FACTOR, interval));
    }

    #[test]
    fn call_just_under_the_factor_boundary_is_not_a_stall() {
        let interval = 16.7;
        assert!(!is_capture_stall(
            interval * CAPTURE_STALL_FACTOR - 0.01,
            interval
        ));
    }

    #[test]
    fn call_that_doubles_the_interval_is_a_stall() {
        assert!(is_capture_stall(33.4, 16.7));
    }

    #[test]
    fn call_that_blows_past_several_frame_intervals_is_a_stall() {
        // The #707 live finding shape: a multi-hundred-ms to multi-second delivery-latency spike
        // implies the dequeue itself ate far more than one frame budget, not just one.
        assert!(is_capture_stall(2200.0, 16.7));
    }

    #[test]
    fn zero_frame_interval_never_stalls_fps_unknown_case() {
        assert!(!is_capture_stall(1000.0, 0.0));
    }

    #[test]
    fn negative_frame_interval_never_stalls() {
        assert!(!is_capture_stall(1000.0, -5.0));
    }

    #[test]
    fn negative_duration_never_stalls_defensive_guard() {
        assert!(!is_capture_stall(-1.0, 16.7));
    }

    #[test]
    fn nan_duration_never_stalls_defensive_guard() {
        assert!(!is_capture_stall(f64::NAN, 16.7));
    }

    #[test]
    fn thirty_fps_interval_boundary() {
        // 30fps -> 33.33ms interval; 1.5x = 50.0ms.
        let interval = 1000.0 / 30.0;
        assert!(is_capture_stall(50.0, interval));
        assert!(!is_capture_stall(49.9, interval));
    }

    // capture_stall_warning — pure message formatting.

    #[test]
    fn warning_message_carries_the_numbers_and_mentions_707() {
        let msg = capture_stall_warning(2200.4, 16.7, 60.0);
        assert!(msg.contains("2200.4"));
        assert!(msg.contains("16.7"));
        assert!(msg.contains("60.0"));
        assert!(msg.contains("#707"));
    }

    #[test]
    fn warning_message_is_never_empty_and_mentions_dequeue() {
        let msg = capture_stall_warning(100.0, 16.7, 60.0);
        assert!(!msg.is_empty());
        assert!(msg.to_lowercase().contains("dequeue"));
    }

    #[test]
    fn warning_message_distinguishes_capture_from_ndi_send() {
        // The whole point of this diagnostic vs `send_stall`'s is to tell the two mechanisms
        // apart in the log — the message must say so explicitly.
        let msg = capture_stall_warning(100.0, 16.7, 60.0);
        assert!(msg.to_lowercase().contains("not ndi send"));
    }
}
