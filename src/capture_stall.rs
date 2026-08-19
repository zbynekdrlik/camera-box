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

/// (#1131) Fraction of the capture device's own frame interval below which a blocking VIDIOC_DQBUF
/// return proves this frame came from a NON-EMPTY V4L2 queue (the driver already had it buffered),
/// as opposed to the loop genuinely WAITING for the device to complete the next frame. A buffered
/// frame returns in well under one capture interval (measured sub-millisecond on a healthy stream —
/// see [`is_capture_stall`]'s own "0.5ms" test); a freshly-awaited frame returns in ~one full
/// capture interval (the emit loop out-runs the capture rate in steady state) or, on a stall, far
/// longer. Half the interval cleanly separates the two with wide margin for ordinary scheduling
/// jitter, and is deliberately the OTHER side of the same `dequeue_duration_ms` signal from
/// [`CAPTURE_STALL_FACTOR`] (1.5x): `(0, 0.5x)` = buffered, `[0.5x, 1.5x)` = a normal single-frame
/// wait, `>= 1.5x` = a stall — all three read off the ONE measurement.
pub const BUFFERED_DEQUEUE_FRACTION: f64 = 0.5;

/// Pure decision: did THIS captured frame come from a NON-EMPTY V4L2 queue (i.e. the driver already
/// had it buffered when we asked)? `duration_ms` is the measured wall-clock time `process_frame`'s
/// blocking `self.stream.next()` (VIDIOC_DQBUF) took; `frame_interval_ms` is
/// `1000.0 / configured_capture_fps`.
///
/// This is the queue-occupancy signal the #1131 emit-gate robustness fix needs: a frame that
/// returned in under [`BUFFERED_DEQUEUE_FRACTION`] of one capture interval was ALREADY waiting in
/// the queue, which PROVES a real captured frame exists to fill the next un-emitted emit boundary —
/// so `genlock_pacing::genlock_emit_gate` must catch up one interval at a time and NEVER grid-resync
/// past it (the issue-1131 multi-slot-skip judder: buffered captured frames leaped-past and
/// discarded in a run). A frame the loop had to WAIT for (`>= BUFFERED_DEQUEUE_FRACTION` of the
/// interval — an EMPTY queue: a normal single-frame wait, a device stall, or a real clock gap) does
/// NOT prove buffered content, so the gate keeps its pre-existing #131 forward-resync there.
///
/// A non-positive `frame_interval_ms` (capture fps unknown/zero) or a non-finite/negative
/// `duration_ms` (a bad measurement) returns `false` — assume freshly-awaited, i.e. keep the
/// queue-blind resync behaviour, so a bad reading can never SUPPRESS an honest skip. Mirrors
/// [`is_capture_stall`]'s exact guard shape, on the SAME `dequeue_duration_ms` measurement.
pub fn frame_from_nonempty_queue(duration_ms: f64, frame_interval_ms: f64) -> bool {
    // Guard `frame_interval_ms` for finiteness too, not just sign (review #1131 🔵1): a +inf
    // interval passes a bare `<= 0.0` check and then `duration_ms < inf * 0.5 == inf` is true for
    // any finite duration — the UNSAFE direction (falsely "buffered" → suppresses an honest skip),
    // the one outcome this fail-safe must never produce. (`is_capture_stall`'s mirror guard errs the
    // opposite, SAFE way — a +inf interval there just never WARNs — so it needs no such change.)
    // Unreachable from the sole caller (`main.rs` computes the interval only under
    // `configured_capture_fps > 0.0`), but a fail-safe with a hole is not a fail-safe.
    if !frame_interval_ms.is_finite()
        || frame_interval_ms <= 0.0
        || !duration_ms.is_finite()
        || duration_ms < 0.0
    {
        return false;
    }
    // A genuinely-measured 0.0 here is a legitimately-instant dequeue (buffer already ready) →
    // buffered, correctly. The `FrameInfo::dequeue_duration_ms == 0.0` "no real measurement"
    // sentinel (review #1131 🔵2) never reaches this gate: the production poll (`main.rs`
    // `process_frame`) always feeds a real `Instant::elapsed()` reading; the sentinel exists only on
    // `FrameInfo`'s static getters/fixtures, which are not on the emit path.
    duration_ms < frame_interval_ms * BUFFERED_DEQUEUE_FRACTION
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

    // #1131 — frame_from_nonempty_queue: the queue-occupancy decision.

    #[test]
    fn buffered_frame_returns_well_under_one_interval_is_nonempty_queue() {
        // A healthy V4L2 dequeue with a buffer ALREADY ready: 60fps -> 16.7ms interval, the
        // blocking dequeue returns sub-millisecond -> the queue was non-empty (frame buffered).
        assert!(frame_from_nonempty_queue(0.5, 16.7));
        assert!(frame_from_nonempty_queue(0.0, 16.7));
    }

    #[test]
    fn normal_single_frame_wait_is_not_a_nonempty_queue() {
        // A dequeue that took ~one full capture interval means the loop WAITED for the device to
        // complete the next frame (an EMPTY queue in steady state, the loop out-running capture).
        assert!(!frame_from_nonempty_queue(16.7, 16.7));
        assert!(!frame_from_nonempty_queue(15.0, 16.7)); // just under a full interval, still a wait
    }

    #[test]
    fn just_below_half_interval_is_buffered_at_or_above_is_not() {
        // The BUFFERED_DEQUEUE_FRACTION (0.5) boundary is exclusive-below = buffered.
        let interval = 16.7;
        let half = interval * BUFFERED_DEQUEUE_FRACTION;
        assert!(frame_from_nonempty_queue(half - 0.1, interval));
        assert!(!frame_from_nonempty_queue(half, interval)); // AT the threshold = not buffered
        assert!(!frame_from_nonempty_queue(half + 0.1, interval));
    }

    #[test]
    fn a_long_stall_dequeue_is_never_read_as_buffered() {
        // A frame delivered after a genuine stall (>= the capture-stall factor) is emphatically
        // NOT buffered — the loop waited far past one interval. Keeps the gate's honest resync.
        assert!(!frame_from_nonempty_queue(26.4, 16.7)); // the live #707 CAM1 dequeue-stall value
        assert!(!frame_from_nonempty_queue(150.0, 16.7));
    }

    #[test]
    fn unknown_or_bad_measurement_is_not_buffered_fail_safe() {
        // Capture fps unknown/zero, or a non-finite/negative reading -> assume freshly-awaited
        // (queue-blind resync preserved), so a bad measurement can never SUPPRESS an honest skip.
        assert!(!frame_from_nonempty_queue(0.5, 0.0));
        assert!(!frame_from_nonempty_queue(0.5, -16.7));
        assert!(!frame_from_nonempty_queue(f64::NAN, 16.7));
        assert!(!frame_from_nonempty_queue(-1.0, 16.7));
        assert!(!frame_from_nonempty_queue(f64::INFINITY, 16.7));
        // (review #1131 🔵1) a non-finite INTERVAL must also fail-safe to NOT-buffered — the unsafe
        // direction: a +inf interval would make `duration < inf*0.5` true for any finite duration
        // and falsely SUPPRESS an honest skip.
        assert!(!frame_from_nonempty_queue(0.5, f64::INFINITY));
        assert!(!frame_from_nonempty_queue(0.5, f64::NAN));
    }
}
