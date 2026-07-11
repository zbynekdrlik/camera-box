//! #656 — capture-delivery-rate sanity check (pure decision, prevention item 1).
//!
//! camera-box already logs `Streaming: X fps emitted / Y fps captured` every ~5s
//! (`src/main.rs`'s capture loop). A capture device that silently starts delivering frames at
//! the WRONG rate is easy to miss in a scrolling log stream unless something explicitly flags
//! it. This module is the PURE decision logic (no I/O) for that flag: given the measured
//! captured fps and the box's configured/negotiated capture rate, decide whether THIS report
//! window is "deviant" — and, given a run of consecutive deviant windows, whether a WARN
//! should fire (a single blip is noise; a SUSTAINED deviation for N consecutive report windows
//! is a real defect).
//!
//! Root cause this guards against (#656, live 2026-07-09/07-10): cam1's Genki ShadowCast 2
//! USB capture dongle silently began delivering ~64 fps instead of its negotiated 60.000 fps,
//! producing a persistent ~4 Hz content-duplicate judder downstream (caught only via
//! after-the-fact tick-pattern archaeology on a recorded E2E run). The live defective log line
//! read `Streaming: 59.4 fps emitted / 63.2 fps captured` — captured deviates from the
//! negotiated 60.0 fps by ~5.3%, sustained for the WHOLE session, not a one-off blip. A WARN
//! fired at the time would have pinpointed this in one run instead of via tick-pattern
//! archaeology across two recordings (see the #656 root-cause comment).

/// Default tolerance: captured fps may deviate from the negotiated/configured capture rate by
/// up to this many PERCENT before a window counts as "deviant". 1% keeps normal measurement
/// jitter (a healthy box reads ~60.0-60.1 fps) well clear of the floor while still catching a
/// real several-percent rate defect (#656's live ~5.3-6.7%).
pub const CAPTURE_RATE_TOLERANCE_PCT: f64 = 1.0;

/// Consecutive 5s report windows a deviation must persist before it's a WARN, not noise.
/// 6 windows @ 5s/window = 30s — long enough to reject a single momentary blip, short enough
/// to catch a real rate-negotiation regression well before a 300s+ E2E recording completes.
pub const CAPTURE_RATE_WARN_WINDOWS: u32 = 6;

/// #666 — EMIT-vs-CAPTURE health tolerance: how many PERCENT the emitted fps may deviate from
/// the box's configured genlock send rate before a report window counts as "deviant". This is a
/// SEPARATE, looser tolerance than `CAPTURE_RATE_TOLERANCE_PCT` (1%, device-level captured-vs-
/// negotiated) because the emit path has more natural per-window jitter (discrete genlock gate
/// boundaries, frame-count rounding over a 5s window) — normal healthy readings observed live
/// (cam5/cam6, 2026-07-11) sit within ~0.5-3% of the target rate. 5% keeps that jitter clear of
/// the floor while still catching a real defect with wide margin: the live #666 finding was a
/// SUSTAINED ~20% deficit (~45-50 fps emitted vs a configured 60 fps send rate) — cam5/cam6
/// transiently emitting well below their healthy captured rate while capture itself stayed clean
/// (0 capture-dropped), self-recovering after ~5 minutes with no restart. Reuses the SAME pure
/// `is_rate_deviant`/`next_consecutive_breaches`/`should_warn` decision as the #656 capture check
/// — only the reference rate (configured SEND fps, not the negotiated CAPTURE fps) and the
/// tolerance differ.
pub const EMIT_RATE_TOLERANCE_PCT: f64 = 5.0;

/// True when `captured_fps` deviates from `configured_fps` by more than `tolerance_pct`
/// percent. `configured_fps <= 0.0` (or any non-finite input) is treated as "no valid
/// configured rate to check against" — never deviant; callers only invoke this once a real
/// negotiated/configured rate is known.
pub fn is_rate_deviant(captured_fps: f64, configured_fps: f64, tolerance_pct: f64) -> bool {
    if configured_fps <= 0.0 || !captured_fps.is_finite() || !configured_fps.is_finite() {
        return false;
    }
    let deviation_pct = ((captured_fps - configured_fps).abs() / configured_fps) * 100.0;
    deviation_pct > tolerance_pct
}

/// Rolling breach counter: pure state transition. Call once per report window with whether
/// THIS window was deviant; returns the new consecutive-breach count (any healthy window
/// resets the count to 0 — this flags a SUSTAINED defect, not a cumulative noisy total).
pub fn next_consecutive_breaches(prev_consecutive: u32, deviant_this_window: bool) -> u32 {
    if deviant_this_window {
        prev_consecutive.saturating_add(1)
    } else {
        0
    }
}

/// True once `consecutive_breaches` has reached (or passed) `warn_windows` — the point (and
/// every window thereafter, per comprehensive-logging: keep warning while a real defect
/// persists) at which a WARN should fire. `warn_windows == 0` never warns (an explicitly
/// disabled check, not "always deviant").
pub fn should_warn(consecutive_breaches: u32, warn_windows: u32) -> bool {
    warn_windows > 0 && consecutive_breaches >= warn_windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_rate_is_not_deviant() {
        assert!(!is_rate_deviant(60.04, 60.0, 1.0));
        assert!(!is_rate_deviant(59.97, 60.0, 1.0));
    }

    #[test]
    fn real_656_defect_rate_is_deviant() {
        // #656 live incident: captured 63.2 / 64.02 fps against a negotiated 60.0 fps.
        assert!(is_rate_deviant(63.2, 60.0, 1.0));
        assert!(is_rate_deviant(64.02, 60.0, 1.0));
    }

    #[test]
    fn exactly_at_tolerance_boundary_is_not_deviant() {
        // 100.0 -> 101.0 is EXACTLY a 1.0% deviation (float-exact, unlike a 60.0 base) -> NOT
        // strictly greater than a 1.0% tolerance -> not deviant.
        assert!(!is_rate_deviant(101.0, 100.0, 1.0));
        // Just over the boundary IS deviant.
        assert!(is_rate_deviant(101.01, 100.0, 1.0));
    }

    #[test]
    fn zero_or_negative_configured_fps_never_deviant() {
        assert!(!is_rate_deviant(64.0, 0.0, 1.0));
        assert!(!is_rate_deviant(64.0, -1.0, 1.0));
    }

    #[test]
    fn non_finite_inputs_never_deviant() {
        assert!(!is_rate_deviant(f64::NAN, 60.0, 1.0));
        assert!(!is_rate_deviant(60.0, f64::NAN, 1.0));
        assert!(!is_rate_deviant(f64::INFINITY, 60.0, 1.0));
        assert!(!is_rate_deviant(60.0, f64::INFINITY, 1.0));
    }

    #[test]
    fn consecutive_breaches_accumulate_and_reset() {
        let mut c = 0;
        c = next_consecutive_breaches(c, true);
        assert_eq!(c, 1);
        c = next_consecutive_breaches(c, true);
        assert_eq!(c, 2);
        c = next_consecutive_breaches(c, false);
        assert_eq!(c, 0);
    }

    #[test]
    fn consecutive_breaches_saturate_never_panic() {
        let c = next_consecutive_breaches(u32::MAX, true);
        assert_eq!(c, u32::MAX);
    }

    #[test]
    fn should_warn_fires_at_and_after_threshold() {
        assert!(!should_warn(5, 6));
        assert!(should_warn(6, 6));
        assert!(should_warn(7, 6));
    }

    #[test]
    fn should_warn_never_fires_when_threshold_is_zero() {
        assert!(!should_warn(0, 0));
        assert!(!should_warn(100, 0));
    }

    #[test]
    fn real_656_scenario_warns_exactly_at_the_configured_window() {
        // Simulate consecutive 5s windows at the #656 defect rate (63.2 vs negotiated 60.0),
        // confirming should_warn fires exactly at window CAPTURE_RATE_WARN_WINDOWS, never
        // earlier, and keeps firing on every window after (comprehensive-logging: don't go
        // silent while the defect persists).
        let mut consecutive = 0u32;
        for i in 1..=(CAPTURE_RATE_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(63.2, 60.0, CAPTURE_RATE_TOLERANCE_PCT);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let warn = should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS);
            if i < CAPTURE_RATE_WARN_WINDOWS {
                assert!(!warn, "window {i} should not warn yet");
            } else {
                assert!(warn, "window {i} should warn");
            }
        }
    }

    #[test]
    fn a_single_blip_never_reaches_the_warn_threshold() {
        // One deviant window surrounded by healthy ones never accumulates to the threshold.
        let mut consecutive = 0u32;
        for deviant in [false, false, true, false, false] {
            consecutive = next_consecutive_breaches(consecutive, deviant);
            assert!(!should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
        }
    }

    #[test]
    fn recovery_after_a_sustained_defect_stops_the_warn() {
        // A defect that persists past the threshold, then recovers, must stop warning (not
        // latch permanently) — the next healthy window resets the run to zero.
        let mut consecutive = 0u32;
        for _ in 0..(CAPTURE_RATE_WARN_WINDOWS + 1) {
            consecutive = next_consecutive_breaches(consecutive, true);
        }
        assert!(should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
        consecutive = next_consecutive_breaches(consecutive, false);
        assert!(!should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS));
    }

    // -----------------------------------------------------------------------------------------
    // #666 — EMIT-vs-CAPTURE health (reuses the SAME pure decision, looser tolerance + a
    // different reference rate: configured SEND fps instead of negotiated CAPTURE fps).
    // -----------------------------------------------------------------------------------------

    #[test]
    fn real_666_emit_deficit_is_deviant_under_the_emit_tolerance() {
        // The live #666 finding: cam5/cam6 emitted ~45-50 fps against a configured 60 fps send
        // rate while captured stayed healthy — a ~17-25% deficit, far past the 5% emit floor.
        assert!(is_rate_deviant(45.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
        assert!(is_rate_deviant(48.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
        assert!(is_rate_deviant(50.0, 60.0, EMIT_RATE_TOLERANCE_PCT));
    }

    #[test]
    fn healthy_emit_jitter_within_5_percent_is_not_deviant() {
        // Live healthy readings (cam5/cam6, 2026-07-11): emitted sits within ~0.5-3% of the
        // configured 60 fps send rate on a normal report window — none of these should WARN.
        for emitted in [60.0, 59.8, 59.6, 60.1, 58.5, 57.5] {
            assert!(
                !is_rate_deviant(emitted, 60.0, EMIT_RATE_TOLERANCE_PCT),
                "emitted={emitted} vs configured=60.0 should be within the {EMIT_RATE_TOLERANCE_PCT}% emit floor"
            );
        }
    }

    // The whole point of a SEPARATE emit constant: the emit path has more natural jitter than
    // the device-level capture check, so its floor must be strictly wider. Both sides are
    // `const`, so this is a compile-time assertion (clippy: assertions_on_constants) rather than
    // a runtime `#[test]`.
    const _: () = assert!(EMIT_RATE_TOLERANCE_PCT > CAPTURE_RATE_TOLERANCE_PCT);

    #[test]
    fn real_666_scenario_warns_exactly_at_the_configured_window() {
        // Sustained ~20% emit deficit (48 fps vs configured 60 fps) must warn exactly at
        // CAPTURE_RATE_WARN_WINDOWS (shared cadence with #656 — reject a blip, catch a real
        // sustained defect), never earlier, and keep warning every window after.
        let mut consecutive = 0u32;
        for i in 1..=(CAPTURE_RATE_WARN_WINDOWS + 2) {
            let deviant = is_rate_deviant(48.0, 60.0, EMIT_RATE_TOLERANCE_PCT);
            assert!(deviant);
            consecutive = next_consecutive_breaches(consecutive, deviant);
            let warn = should_warn(consecutive, CAPTURE_RATE_WARN_WINDOWS);
            if i < CAPTURE_RATE_WARN_WINDOWS {
                assert!(!warn, "window {i} should not warn yet");
            } else {
                assert!(warn, "window {i} should warn");
            }
        }
    }
}
