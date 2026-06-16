//! #81 dead-output liveness pre-check.
//!
//! A downstream OBS output can go silently dead mid-run — e.g. a GPU device-removed
//! (TDR) crash wedges the compositor so it cannot make textures and its NDI Main
//! Output emits nothing — while the camera-box/genlock ingest is perfectly healthy.
//! When that happens the harness tap on that output captures ~0 frames for the WHOLE
//! run (#81: stream tap captured≈1 over 1800s), and the run only fails 30 minutes
//! later as a generic under-min-frames mystery.
//!
//! This module turns that into a FAST + LOUD failure: looking at each tap's captured
//! count in an EARLY window (the run's first ~30s), if one tap captured at-or-below a
//! near-zero floor while at least one PEER captured plenty, it returns a DISTINCT
//! `DeadOutput` verdict that names the tap and points an operator straight at a dead
//! downstream OBS / GPU device-removed — so #81 can never recur as a silent 30-min
//! zero-frames mystery.
//!
//! Pure / unit-tested: the caller (multitap-probe) feeds the per-tap captured counts;
//! no NDI rig is needed to test the decision.

/// One tap's captured-frame count within the early liveness window.
#[derive(Debug, Clone)]
pub struct TapLiveness {
    /// Tap name (e.g. "cam", "strih", "stream").
    pub name: String,
    /// Raw NDI frames this tap pulled off the wire within the liveness window
    /// (decoded or not — a dead output sends nothing at all, so the raw count is
    /// the right signal; a torn-but-arriving output is NOT dead).
    pub captured_in_window: u64,
}

/// The liveness pre-check verdict.
#[derive(Debug)]
pub enum LivenessVerdict {
    /// Every tap captured above the dead floor — proceed with the run.
    AllAlive,
    /// One tap captured ~0 frames while a peer captured plenty: its downstream
    /// output is dead. `message` is the distinct, operator-facing diagnosis.
    DeadOutput { tap: String, message: String },
}

/// Whether the liveness capture window may START yet, given how many taps have
/// finished connecting and how long we have already waited.
///
/// The capture window must only judge taps that have actually had a chance to
/// capture — i.e. taps whose `NdiReceiver::connect` has returned. NDI discovery
/// can legitimately take up to `connect_timeout_secs`, so a healthy but
/// slow-to-advertise source can still be inside `connect` (captured == 0) well
/// into the run. Starting the capture window before every tap is connected would
/// read that 0 and FALSELY flag the tap as a dead output (#81 race: a 30 s
/// window starting at spawn vs a 30 s connect timeout).
///
/// Returns `true` (start the window now) when EITHER every tap is connected OR
/// the connect budget (`connect_timeout_secs`) has elapsed — past that budget a
/// tap that still isn't connected is a genuine connect failure that the tap
/// thread surfaces as an error on join, so the dead-output check no longer needs
/// to wait on it.
pub fn window_may_start(
    connected_taps: usize,
    total_taps: usize,
    connect_budget_elapsed: bool,
) -> bool {
    connected_taps >= total_taps || connect_budget_elapsed
}

/// Decide whether any single tap's downstream output is dead, given each tap's
/// captured-frame count over the first `window_secs` of the run.
///
/// A tap is DEAD when `captured_in_window <= dead_floor` (inclusive — a frame or two
/// in 30s is not "emitting"). The check ONLY fires when at least one OTHER tap is
/// ABOVE the floor: if EVERY tap captured ~0 (painter never started, whole rig down)
/// that is a different failure — a setup/source problem owned by the generic
/// min-frames Fail — and blaming one tap's OBS would mis-diagnose it.
///
/// Returns the FIRST dead tap found (taps are checked in the given order, which is
/// the source→…→endpoint order, so the most-downstream dead output is reported
/// after any upstream one). In the #81 case only the endpoint (stream) is dead, so
/// the result is unambiguous.
pub fn check_tap_liveness(
    taps: &[TapLiveness],
    window_secs: u64,
    dead_floor: u64,
) -> LivenessVerdict {
    let any_alive = taps.iter().any(|t| t.captured_in_window > dead_floor);
    if !any_alive {
        // Whole-rig outage (or window too short for anyone) — not a single dead
        // downstream output. Leave it to the generic min-frames Fail.
        return LivenessVerdict::AllAlive;
    }
    for t in taps {
        if t.captured_in_window <= dead_floor {
            let message = format!(
                "tap '{}' NDI output emitting nothing — downstream OBS dead? \
                 (GPU device-removed?): captured {} frame(s) in the first {} s \
                 (dead_floor {}) while a peer tap captured frames. The downstream \
                 compositor is not producing output (e.g. a GPU TDR / \
                 DXGI_ERROR_DEVICE_REMOVED wedged OBS). Aborting early instead of \
                 running the full duration and reporting a silent zero-frames result. \
                 Audit the downstream OBS log for 'Device Removed' / 887A000x (#81).",
                t.name, t.captured_in_window, window_secs, dead_floor
            );
            return LivenessVerdict::DeadOutput {
                tap: t.name.clone(),
                message,
            };
        }
    }
    LivenessVerdict::AllAlive
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, c: u64) -> TapLiveness {
        TapLiveness {
            name: name.to_string(),
            captured_in_window: c,
        }
    }

    #[test]
    fn flags_the_single_dead_endpoint() {
        let taps = [t("cam", 800), t("strih", 790), t("stream", 0)];
        match check_tap_liveness(&taps, 30, 2) {
            LivenessVerdict::DeadOutput { tap, .. } => assert_eq!(tap, "stream"),
            v => panic!("expected DeadOutput, got {v:?}"),
        }
    }

    #[test]
    fn all_alive_passes() {
        let taps = [t("cam", 800), t("stream", 700)];
        assert!(matches!(
            check_tap_liveness(&taps, 30, 2),
            LivenessVerdict::AllAlive
        ));
    }

    #[test]
    fn all_dead_is_not_single_output() {
        let taps = [t("cam", 0), t("stream", 1)];
        assert!(matches!(
            check_tap_liveness(&taps, 30, 2),
            LivenessVerdict::AllAlive
        ));
    }

    #[test]
    fn window_waits_until_all_taps_connected() {
        // Not every tap connected yet and the connect budget has NOT elapsed:
        // do NOT start the capture window (a slow tap is still connecting, its
        // captured == 0 must not be read as a dead output).
        assert!(!window_may_start(2, 3, false));
    }

    #[test]
    fn window_starts_once_all_taps_connected() {
        assert!(window_may_start(3, 3, false));
    }

    #[test]
    fn window_starts_when_connect_budget_elapsed_even_if_not_all_connected() {
        // Past the connect budget a still-unconnected tap is a genuine connect
        // failure (surfaced on join), so the window may start without it.
        assert!(window_may_start(2, 3, true));
    }

    #[test]
    fn reports_first_dead_in_order() {
        // Two dead taps: the upstream one is reported first (source→endpoint order).
        let taps = [t("cam", 0), t("strih", 800), t("stream", 0)];
        match check_tap_liveness(&taps, 30, 2) {
            LivenessVerdict::DeadOutput { tap, .. } => assert_eq!(tap, "cam"),
            v => panic!("expected DeadOutput, got {v:?}"),
        }
    }
}
