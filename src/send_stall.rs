//! #707 — NDI blocking-send STALL diagnostic (pure decision).
//!
//! `NdiSender::send_frame_data_with_timecode` (src/ndi.rs) calls the NDI SDK's SYNCHRONOUS
//! `NDIlib_send_send_video_v2` — a call the surrounding code already documents as
//! "SYNCHRONOUS send - blocks until NDI accepts frame (lowest latency)". That block duration is
//! NOT bounded by our own code: the NDI SDK returns only once the previous frame's send buffer
//! can be reused, which is governed by how fast the network / receiver(s) can consume data.
//!
//! The #656/#663/#665/#666/#707 emit-rate-deficit investigation family (cam2/cam5/cam6
//! chronically emitting BELOW their configured genlock send rate — `#666 emit-delivery-rate
//! DEFECTIVE` warnings — while capture itself stays perfectly healthy, 0 capture-dropped) has
//! never had DIRECT proof of WHERE the missing time goes: only the downstream 5-second-averaged
//! emitted-fps symptom (`capture_rate_health::is_rate_deviant`) is measured. Root-causing has
//! repeatedly stalled on "not confirmed" (#665's own still-open cable/port-vs-contention
//! question; #666's "network/genlock-gate hiccup, not root-caused"; #707's rescoping comment).
//!
//! This module is the pure decision for a NEW, complementary diagnostic that closes that gap:
//! given how long a SINGLE blocking send call actually took (wall-clock, measured at the call
//! site in `ndi.rs`) and the sender's own configured frame interval, decide whether THIS ONE
//! call is a genuine stall worth a WARN. If a future #666-class recurrence lights up THIS WARN
//! at the same time, the blocking SDK call (i.e. network/receiver backpressure) is the confirmed
//! mechanism; if the emit deficit recurs with NO stall warnings, the stall lies elsewhere
//! (scheduling, capture-side queuing, a different code path) — either way the next investigation
//! has direct evidence instead of another round of fps-delta archaeology.

/// A single blocking send call counts as a "stall" once it takes at least this many multiples of
/// the sender's own configured frame interval. 1.5x keeps ordinary call-overhead / scheduling
/// jitter (a healthy blocking call on an unloaded network returns in a small fraction of one
/// frame interval — sub-millisecond to a few ms at 60fps/16.7ms) well clear of the floor, while
/// still catching a call that visibly ate into (or blew past) the NEXT frame's own budget —
/// exactly the shape of stall that, if it recurred across a report window, would show up as the
/// #666 emitted-fps deficit downstream.
pub const SEND_STALL_FACTOR: f64 = 1.5;

/// Pure decision: did this ONE blocking send call stall? `duration_ms` is the measured
/// wall-clock time the call took; `frame_interval_ms` is `1000.0 / configured_send_fps`.
///
/// A non-positive `frame_interval_ms` (genlock/send rate off, fps 0 or unset) never stalls —
/// there is no per-frame budget to have blown, mirroring the zero-interval guard
/// `genlock_pacing::genlock_emit_gate` already uses for the same "genlock off" case. A non-finite or
/// negative `duration_ms` (should never happen from a real `Instant::elapsed()` reading, but a
/// defensive guard costs nothing) also never stalls — never fabricate a WARN from a bad
/// measurement.
pub fn is_send_stall(duration_ms: f64, frame_interval_ms: f64) -> bool {
    if frame_interval_ms <= 0.0 || !duration_ms.is_finite() || duration_ms < 0.0 {
        return false;
    }
    duration_ms >= frame_interval_ms * SEND_STALL_FACTOR
}

/// Build the WARN message for a confirmed stall — pure string formatting so the exact wording is
/// unit-tested here rather than only visible in a live log stream. `sender_name` is the sender's
/// own NDI name (e.g. "CAM5 (usb)") so a stall is attributable to a SPECIFIC box's sender in a
/// shared log/journal, matching the #666 WARN's own convention of naming what it's about.
pub fn send_stall_warning(
    sender_name: &str,
    duration_ms: f64,
    frame_interval_ms: f64,
    configured_fps: f64,
) -> String {
    format!(
        "#707 NDI blocking send STALL on '{sender_name}': {duration_ms:.1}ms (configured frame \
         interval {frame_interval_ms:.1}ms @ {configured_fps:.1}fps, >= {SEND_STALL_FACTOR:.1}x \
         budget) — the SYNCHRONOUS NDIlib_send_send_video_v2 call itself blocked, i.e. this frame's \
         delay traces to network/receiver backpressure on this send, not capture or scheduling \
         upstream of it (see #707)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // is_send_stall — the pure threshold decision.

    #[test]
    fn fast_call_well_under_one_frame_interval_is_not_a_stall() {
        // A healthy blocking send on an unloaded network: 60fps -> 16.7ms interval, call
        // returns in 1ms.
        assert!(!is_send_stall(1.0, 16.7));
    }

    #[test]
    fn call_at_exactly_the_factor_boundary_is_a_stall() {
        // 16.7ms interval * 1.5 = 25.05ms — AT the boundary counts as a stall (inclusive,
        // matches the project's own `av_offset_gate_pass` inclusive-tolerance convention).
        let interval = 16.7;
        assert!(is_send_stall(interval * SEND_STALL_FACTOR, interval));
    }

    #[test]
    fn call_just_under_the_factor_boundary_is_not_a_stall() {
        let interval = 16.7;
        assert!(!is_send_stall(
            interval * SEND_STALL_FACTOR - 0.01,
            interval
        ));
    }

    #[test]
    fn call_that_doubles_the_interval_is_a_stall() {
        assert!(is_send_stall(33.4, 16.7));
    }

    #[test]
    fn call_that_blows_past_several_frame_intervals_is_a_stall() {
        // The #666 live finding shape: a SUSTAINED deficit implies individual sends eating
        // multiple frame budgets, not just one.
        assert!(is_send_stall(120.0, 16.7));
    }

    #[test]
    fn zero_frame_interval_never_stalls_genlock_off_case() {
        // Mirrors `genlock_pacing::genlock_emit_gate`'s own zero-interval guard (genlock/send-rate off).
        assert!(!is_send_stall(1000.0, 0.0));
    }

    #[test]
    fn negative_frame_interval_never_stalls() {
        assert!(!is_send_stall(1000.0, -5.0));
    }

    #[test]
    fn negative_duration_never_stalls_defensive_guard() {
        assert!(!is_send_stall(-1.0, 16.7));
    }

    #[test]
    fn nan_duration_never_stalls_defensive_guard() {
        assert!(!is_send_stall(f64::NAN, 16.7));
    }

    #[test]
    fn thirty_fps_interval_boundary() {
        // 30fps -> 33.33ms interval; 1.5x = 50.0ms.
        let interval = 1000.0 / 30.0;
        assert!(is_send_stall(50.0, interval));
        assert!(!is_send_stall(49.9, interval));
    }

    // send_stall_warning — pure message formatting.

    #[test]
    fn warning_message_names_the_sender_and_carries_the_numbers() {
        let msg = send_stall_warning("CAM5 (usb)", 45.2, 16.7, 60.0);
        assert!(msg.contains("CAM5 (usb)"));
        assert!(msg.contains("45.2"));
        assert!(msg.contains("16.7"));
        assert!(msg.contains("60.0"));
        assert!(msg.contains("#707"));
    }

    #[test]
    fn warning_message_is_never_empty_and_mentions_ndi() {
        let msg = send_stall_warning("cam2", 100.0, 16.7, 60.0);
        assert!(!msg.is_empty());
        assert!(msg.to_lowercase().contains("ndi"));
    }
}
