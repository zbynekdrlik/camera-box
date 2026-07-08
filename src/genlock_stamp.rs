//! #286 — pure genlock timecode-stamp decision (the A/V-cut root fix).
//!
//! The emitted NDI frame's genlock `timecode` MUST be derived from the frame's real
//! CAPTURE instant (the V4L2 buffer timestamp, mapped into the DanteSync `CLOCK_REALTIME`
//! domain), NOT the arrival/send wall-clock. Every grabber card adds its own
//! photon->dequeue latency `d_X` (ShadowCast on cam1-3, NZXT Signal HD60 on cam4, ...); if
//! the timecode is stamped at ARRIVAL, `d_X` is baked into it and the receiver's genlock
//! FIFO cannot equalize it across cameras — so cutting from one camera to another shifts
//! the video timing and breaks A/V lip-sync at the stream program (the recurring live-event
//! failure). Stamping the CAPTURE instant makes two cameras that filmed the same real
//! moment emit the SAME timecode, so the genlock presents them together — provided the
//! receiver's reserve/latency >= max(d_X).
//!
//! Pure crate-root seam (default features, Tier-0 per the project CLAUDE.md): the realtime
//! capture path in `main.rs`/`ndi.rs` (probe-independent) feeds these functions; the
//! DECISION is unit-tested here without a camera. The production wiring (extracting
//! `v4l2_buffer.timestamp`, sampling the monotonic->realtime offset, and calling these)
//! is the follow-up increment, certified on the rig. Root: #286 / #145 / #188. Gate: #624.

/// Map a `CLOCK_MONOTONIC` capture timestamp (what the V4L2 UVC driver stamps on the
/// dequeued buffer) into the `CLOCK_REALTIME` (DanteSync-disciplined) domain that the
/// genlock boundaries live in. `mono_to_real_offset_100ns = realtime_now - monotonic_now`,
/// sampled on the capture thread and re-sampled periodically so a realtime step/slew does
/// not skew the stamp. Saturating so a bogus/huge timestamp can never wrap.
#[inline]
pub fn capture_realtime_100ns(capture_monotonic_100ns: i64, mono_to_real_offset_100ns: i64) -> i64 {
    capture_monotonic_100ns.saturating_add(mono_to_real_offset_100ns)
}

/// The emitted frame's genlock NDI `timecode` (100ns units): the frame boundary at/after
/// the frame's real CAPTURE instant. `arrival_realtime_100ns` is the wall-clock the send
/// thread observes for the SAME frame (post-dequeue); it is retained in the signature so
/// callers can also compute the capture-vs-arrival divergence
/// ([`stamp_arrival_divergence_100ns`], a per-camera latency proxy for the #624 gate), but
/// it is DELIBERATELY not the basis of the timecode — keying the timecode on arrival is the
/// #286 defect this function exists to eliminate.
#[inline]
pub fn genlock_emit_timecode_100ns(
    capture_realtime_100ns: i64,
    arrival_realtime_100ns: i64,
    fps: i64,
) -> i64 {
    // #286 FIX: key the emitted timecode on the real CAPTURE instant so each grabber card's
    // photon->dequeue latency d_X does NOT leak into the stamp — two cameras that filmed the
    // same real moment then emit the same timecode and the receiver genlock presents them
    // together. `arrival` is retained only for the divergence proxy below, never the basis.
    let _ = arrival_realtime_100ns;
    crate::ndi::next_boundary_100ns(capture_realtime_100ns, fps)
}

/// The whole-frame divergence between where the ARRIVAL-based stamp would land and where the
/// CAPTURE-based stamp lands, in 100ns units — a per-frame proxy for this camera's grabber
/// latency `d_X` residue. Aggregated per camera it feeds the #624 cross-camera latency gate.
#[inline]
pub fn stamp_arrival_divergence_100ns(
    capture_realtime_100ns: i64,
    arrival_realtime_100ns: i64,
    fps: i64,
) -> i64 {
    crate::ndi::next_boundary_100ns(arrival_realtime_100ns, fps)
        - crate::ndi::next_boundary_100ns(capture_realtime_100ns, fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 100ns units: 1 second = 1e9 ns / 100 = 10_000_000.
    const SEC_100NS: i64 = 10_000_000;

    #[test]
    fn capture_realtime_maps_monotonic_by_offset() {
        assert_eq!(capture_realtime_100ns(500, 1_000), 1_500);
        assert_eq!(capture_realtime_100ns(-200, 1_000), 800);
        // Saturating: a bogus huge monotonic value must not wrap into a negative timecode.
        assert_eq!(capture_realtime_100ns(i64::MAX, 10), i64::MAX);
    }

    /// THE #286 GATE: two frames that captured the SAME real instant but ARRIVED at different
    /// times (different grabber-card latency `d_X`) MUST emit the SAME genlock timecode — else
    /// the receiver presents them at different instants and a camera cut breaks A/V lip-sync.
    #[test]
    fn timecode_keys_on_capture_not_arrival() {
        let fps = 30;
        let base = 100 * SEC_100NS; // start of a whole second
        let capture = base + 100_000; // 10 ms into the second (inside 30fps frame 0)
                                      // Two arrivals for the SAME captured frame that straddle the next 30fps boundary
                                      // (33.33 ms): a "fast card" arrives at 15 ms, a "slow card" at 40 ms.
        let arrival_fast_card = base + 150_000; // 15 ms
        let arrival_slow_card = base + 400_000; // 40 ms
        let tc_fast = genlock_emit_timecode_100ns(capture, arrival_fast_card, fps);
        let tc_slow = genlock_emit_timecode_100ns(capture, arrival_slow_card, fps);
        assert_eq!(
            tc_fast, tc_slow,
            "same capture instant must emit the same timecode regardless of arrival \
             (per-card latency d_X must NOT leak into the genlock stamp — #286)"
        );
    }

    /// The stamped timecode is the frame boundary at/after the CAPTURE instant.
    #[test]
    fn timecode_is_the_capture_boundary() {
        let fps = 30;
        let base = 100 * SEC_100NS;
        let capture = base + 100_000; // 10 ms in -> next 30fps boundary is 33.33 ms
        let expected = base + SEC_100NS / fps; // first 30fps boundary after the second start
                                               // Even with a LATE arrival (next frame over), the timecode tracks capture, not arrival.
        assert_eq!(
            genlock_emit_timecode_100ns(capture, base + 400_000, fps),
            expected
        );
    }

    /// The divergence diagnostic is zero when the card is instant (capture == arrival) and
    /// grows by whole genlock frames as the arrival lags the capture across boundaries.
    #[test]
    fn divergence_is_zero_for_instant_card_and_positive_for_a_lagging_one() {
        let fps = 30;
        let base = 100 * SEC_100NS;
        let capture = base + 100_000; // 10 ms
        assert_eq!(
            stamp_arrival_divergence_100ns(capture, capture, fps),
            0,
            "no divergence when the frame arrives at its capture instant"
        );
        // Arrival one 30fps frame later than the capture boundary -> one-frame divergence.
        let arrival_next_frame = base + 400_000; // 40 ms (past the 33.33 ms boundary)
        assert_eq!(
            stamp_arrival_divergence_100ns(capture, arrival_next_frame, fps),
            SEC_100NS / fps,
            "a card that lags into the next genlock frame diverges by exactly one frame"
        );
    }
}
