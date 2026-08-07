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

/// The emitted frame's genlock NDI `timecode` (100ns units): the frame boundary AT OR
/// BEFORE the frame's real CAPTURE instant (#1009 — floor, never the future-dated ceil). `arrival_realtime_100ns` is the wall-clock the send
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
    // #1009: FLOOR — the boundary AT-OR-BEFORE the capture instant, never the strictly-next
    // (ceil) one, which dated every stamp 0..1 interval into the receiver's future and armed
    // the issue-147 backward-step hair-trigger (issue 1007: 0.3 ms measured margin). Grid
    // alignment and the same-instant-same-stamp property are preserved (same per-second grid).
    let _ = arrival_realtime_100ns;
    crate::ndi::floor_boundary_100ns(capture_realtime_100ns, fps)
}

/// The whole-frame divergence between where the ARRIVAL-based stamp would land and where the
/// CAPTURE-based stamp lands, in 100ns units — a per-frame proxy for this camera's grabber
/// latency `d_X` residue.
///
/// NOT currently called from production code or wired into any gate: the #624 cross-camera
/// latency gate that shipped alongside this function (`switch_latency::spread_verdict`, fed by
/// `bin/recording-verdict`) measures `d_X` a different way — from the RECORDED stream, pairing
/// each camera's own capture-time burn against cam2's optical QR per `--switch-schedule`
/// window — not from this in-process arrival-vs-capture proxy. This function is kept as a
/// tested, ready-to-wire building block for a future LIVE (no recording needed) per-camera
/// divergence diagnostic; it is not dead in the sense of "safe to delete", but it has no caller
/// today.
#[inline]
pub fn stamp_arrival_divergence_100ns(
    capture_realtime_100ns: i64,
    arrival_realtime_100ns: i64,
    fps: i64,
) -> i64 {
    // #1009: floor on both sides, in lock-step with the stamp itself (the proxy compares
    // where the two stamps LAND, so it must use the same boundary convention).
    crate::ndi::floor_boundary_100ns(arrival_realtime_100ns, fps)
        - crate::ndi::floor_boundary_100ns(capture_realtime_100ns, fps)
}

/// How many CONSECUTIVE captured frames elapse before the monotonic->realtime offset
/// (`capture_realtime_100ns`'s `mono_to_real_offset_100ns` input) is due for a re-sample.
/// 100 frames is ~1.7s at 60fps / ~3.3s at 30fps — frequent enough to track NTP/PTP
/// realtime-clock slew (per this module's doc: "re-sampled periodically so a realtime
/// step/slew does not skew the stamp"), rare enough that the back-to-back
/// `clock_gettime(CLOCK_REALTIME)` + `clock_gettime(CLOCK_MONOTONIC)` pair never
/// meaningfully taxes the per-frame capture hot path. See
/// [`should_resample_mono_to_real_offset`].
pub const OFFSET_RESAMPLE_INTERVAL_FRAMES: u64 = 100;

/// Pure cadence decision: is the monotonic->realtime offset due for a re-sample, given how
/// many captured frames have elapsed since the last sample? The capture thread increments
/// `frames_since_last_sample` once per captured frame and resets it to 0 right after taking
/// a fresh sample.
#[inline]
pub fn should_resample_mono_to_real_offset(frames_since_last_sample: u64) -> bool {
    frames_since_last_sample >= OFFSET_RESAMPLE_INTERVAL_FRAMES
}

/// Convert a raw V4L2 buffer timestamp (`sec` whole seconds, `usec` microseconds — the
/// `v4l::timestamp::Timestamp` fields the UVC driver stamps on dequeue, `CLOCK_MONOTONIC`
/// domain by V4L2 default) into monotonic 100ns units, ready to feed
/// [`capture_realtime_100ns`]. Saturating so an implausible/huge driver value can never
/// wrap into a bogus timecode.
#[inline]
pub fn v4l_timestamp_to_monotonic_100ns(sec: i64, usec: i64) -> i64 {
    // 1 sec = 10_000_000 (100ns units); 1 usec = 10 (100ns units).
    sec.saturating_mul(10_000_000)
        .saturating_add(usec.saturating_mul(10))
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

    /// The stamped timecode is the frame boundary at/BEFORE the CAPTURE instant (#1009 —
    /// floor; the pre-#1009 revision of this test pinned the ceil boundary, which is the
    /// future-bias defect itself, so its expectation changed WITH the behavior).
    #[test]
    fn timecode_is_the_capture_boundary() {
        let fps = 30;
        let base = 100 * SEC_100NS;
        let capture = base + 100_000; // 10 ms in -> inside 30fps frame 0 (0..33.33 ms)
        let expected = base; // frame 0's own boundary — at-or-before the capture instant
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

    /// #1009 (defect B): the emitted genlock stamp must be the frame boundary AT OR BEFORE
    /// the capture instant — NEVER in the future. The ceil stamp (the strictly-NEXT
    /// boundary) put every emitted frame 0..1 interval in the RECEIVER'S FUTURE by
    /// construction, which is what armed the issue-147 backward-step hair-trigger overnight
    /// (issue 1007 forensics: measured margin at trigger min 0.3 ms — network delay was the
    /// ONLY headroom). Floor preserves the shared grid (still an exact boundary, so two
    /// cameras capturing the same instant still stamp identically) while guaranteeing a
    /// receiver comparing the stamp against its own wall clock never sees the future in
    /// normal operation.
    #[test]
    fn stamp_is_at_or_before_the_capture_instant_never_future_1009() {
        let fps = 30;
        let base = 100 * SEC_100NS;
        // Offsets across the second: exactly on a boundary (0), just after (1), mid-frame,
        // just under / just over the first boundary, and the last 100ns of the second.
        for off in [0i64, 1, 100_000, 333_332, 333_334, 5_000_000, 9_999_999] {
            let capture = base + off;
            let tc = genlock_emit_timecode_100ns(capture, capture, fps);
            assert!(
                tc <= capture,
                "off {off}: stamp {tc} is IN THE FUTURE of the capture instant {capture} — \
                 the ceil-to-boundary bias hands the receiver future-stamped frames in \
                 normal operation (the issue-1007 hair-trigger arming, #1009)"
            );
            // Never more than one frame interval behind either (floor of the CURRENT
            // interval, not some older boundary).
            assert!(
                capture - tc < SEC_100NS / fps + 1,
                "off {off}: stamp {tc} fell more than one interval behind capture {capture}"
            );
            // Grid alignment preserved: the stamp is an exact boundary of the shared
            // per-second grid (frame_k = second_start + k*UNITS/fps, multiply-then-divide).
            // The inverse must be the CEIL division: boundary_k = floor(k*UNITS/fps) sits
            // just UNDER k*UNITS/fps whenever fps does not divide UNITS evenly (e.g.
            // boundary_1 at 30 fps = 333_333, not 333_333.33), so a floor inverse
            // (in_second*fps/UNITS) under-recovers k by one and falsely flags an exact
            // boundary as off-grid.
            let cs = (tc / SEC_100NS) * SEC_100NS;
            let in_second = tc - cs;
            let k = (in_second * fps + SEC_100NS - 1) / SEC_100NS;
            assert_eq!(
                cs + k * SEC_100NS / fps,
                tc,
                "off {off}: stamp {tc} is not on the shared boundary grid"
            );
        }
    }

    /// The monotonic->realtime offset must be re-sampled once the cadence interval has
    /// elapsed since the last sample — never immediately, never permanently skipped.
    #[test]
    fn resample_offset_due_once_interval_elapsed() {
        assert!(
            !should_resample_mono_to_real_offset(0),
            "must not resample immediately after taking a sample"
        );
        assert!(!should_resample_mono_to_real_offset(
            OFFSET_RESAMPLE_INTERVAL_FRAMES - 1
        ));
        assert!(
            should_resample_mono_to_real_offset(OFFSET_RESAMPLE_INTERVAL_FRAMES),
            "due exactly once the cadence interval elapses"
        );
        assert!(
            should_resample_mono_to_real_offset(OFFSET_RESAMPLE_INTERVAL_FRAMES + 50),
            "stays due past the interval (a missed sample must not un-arm it)"
        );
    }

    /// The V4L2 sec+usec pair must convert to 100ns units at FULL precision (dropping the
    /// usec term would silently truncate every capture timestamp to whole seconds).
    #[test]
    fn v4l_timestamp_converts_full_sec_and_usec_precision() {
        // 5.5 seconds = 5 * 1e7 (100ns/sec) + 500_000 usec * 10 (100ns/usec).
        assert_eq!(v4l_timestamp_to_monotonic_100ns(5, 500_000), 55_000_000);
        assert_eq!(v4l_timestamp_to_monotonic_100ns(0, 0), 0);
        // Saturating: an implausible huge sec must not overflow/wrap.
        assert_eq!(
            v4l_timestamp_to_monotonic_100ns(i64::MAX, 999_999),
            i64::MAX
        );
    }
}
