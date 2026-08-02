//! #945 — capture/emit-thread WEDGE self-watchdog (pure decision, Tier-0).
//!
//! `VideoCapture::process_frame` (`src/capture.rs`) calls exactly ONE blocking V4L2 dequeue per
//! capture-loop iteration (`self.stream.next()?`, a `VIDIOC_DQBUF`). Every observability check
//! this appliance already has — the 5s "Streaming:" stats tick, [`crate::capture_stall`]'s
//! per-dequeue timing WARN, [`crate::capture_rate_health`]/[`crate::capture_rate_selfheal`]'s
//! rate/self-heal logic — is invoked from INSIDE the closure `process_frame` runs once that
//! blocking call *returns*. Live incident (cam4, 10.77.9.64, 2026-08-02): a `-71` USB isochronous
//! completion-handler burst wedged the dequeue so it never returned again — the whole capture
//! loop (a `tokio::task::spawn_blocking` thread) froze at that one call site, and every one of
//! those in-loop checks froze with it, for 48 CONSECUTIVE minutes. Meanwhile the process's other
//! threads (intercom, the NDI listen sockets) kept running/logging normally and systemd saw
//! `active (running)` throughout — nothing detected the loop was dead. The only thing that
//! noticed was the E2E gate's frozen-camera preflight, 43 minutes late.
//!
//! The fix is an OUT-OF-BAND watchdog: the capture loop writes a monotonic "last progress"
//! timestamp to a shared atomic immediately after EVERY `process_frame()` call returns — on both
//! the `Ok` and the `Err` arm, unconditional on whether a frame was actually emitted or
//! "interesting". That is the discriminator the ticket asks for: a genuine no-signal condition
//! (an unplugged HDMI source) still returns from `VIDIOC_DQBUF` regularly — a delivered
//! blank/black frame, or a fast, repeating error — so the heartbeat keeps advancing; only a TRUE
//! wedge (the syscall never returning at all) lets it go stale. A SEPARATE thread (which can never
//! itself be blocked by the same dequeue) polls that heartbeat and, once it has gone stale for at
//! least `CAPTURE_WEDGE_THRESHOLD_S`, treats the loop as provably dead.
//!
//! This module is the PURE decision + message-formatting half (Tier-0, default features, no I/O)
//! — mirrors [`crate::capture_stall`]'s pure-decision/impure-call-site split. The atomic
//! heartbeat, the actual watchdog thread, and the `std::process::exit` call live in `src/main.rs`.
//!
//! Per `.claude/rules/self-heal-frozen-leg-attribution.md`'s explicit warning against a second,
//! competing attribution parser: this module adds exactly ONE new, distinctly-worded event (its
//! own CRITICAL log line + its own dedicated exit code, `CAPTURE_WEDGE_EXIT_CODE`, distinct from
//! `capture_rate_selfheal`'s existing 77/78) rather than reusing or overloading the `#663
//! self-heal` log shape existing forensics already key on.
//!
/// How many seconds the capture loop's blocking dequeue may go WITHOUT RETURNING AT ALL before
/// the watchdog treats it as WEDGED. The existing "Streaming:" stats tick fires every 5s, so this
/// is ~5 ticks (25s) — long enough that one unlucky slow tick's jitter never trips it, short
/// enough to catch a genuine wedge (the live #945 incident: 48 CONSECUTIVE minutes of total
/// silence) in well under a minute instead of 43+ minutes late.
pub const CAPTURE_WEDGE_THRESHOLD_S: f64 = 25.0;

/// Process exit code used when the watchdog forces a restart because the capture loop is
/// provably wedged. Distinct from `capture_rate_selfheal::SELF_HEAL_EXIT_CODE` (77) and
/// `SELF_HEAL_RESET_FAILED_EXIT_CODE` (78) so `systemctl status`/journal forensics can always
/// tell a capture-wedge restart apart from a `#663` USB-reset restart or a genuine crash.
pub const CAPTURE_WEDGE_EXIT_CODE: i32 = 79;

/// The watchdog's verdict for THIS poll, given how long it has been since the capture loop last
/// proved it made progress (a `process_frame()` call returning, Ok or Err).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WedgeVerdict {
    /// The loop returned from its blocking dequeue recently enough — alive.
    Alive,
    /// No progress for at least the threshold — the loop is provably dead.
    Wedged,
}

/// Pure decision: given `seconds_since_last_progress` (wall/monotonic elapsed time since the
/// capture loop's heartbeat last advanced) and `threshold_s` (the configured wedge threshold),
/// decide whether the capture loop is WEDGED.
///
/// A non-finite or negative `seconds_since_last_progress` (should never happen from a real
/// `Instant::elapsed()` reading, but a defensive guard costs nothing — mirrors
/// `capture_stall::is_capture_stall`'s own guard shape) never wedges. A non-positive `threshold_s`
/// (a disabled/misconfigured watchdog) also never wedges — it must never be interpreted as
/// "always wedged".
pub fn evaluate_wedge(seconds_since_last_progress: f64, threshold_s: f64) -> WedgeVerdict {
    if !seconds_since_last_progress.is_finite()
        || seconds_since_last_progress < 0.0
        || threshold_s <= 0.0
    {
        return WedgeVerdict::Alive;
    }
    if seconds_since_last_progress >= threshold_s {
        WedgeVerdict::Wedged
    } else {
        WedgeVerdict::Alive
    }
}

/// Build the CRITICAL, uniquely grep-able message the watchdog logs right before it exits the
/// process. Pure string formatting so the exact wording is unit-tested here, mirroring
/// `capture_stall::capture_stall_warning`'s convention. Deliberately names `#945`, `CRITICAL`, and
/// the exact exit code so this event can never be confused with a `#663 self-heal` restart or a
/// generic crash in the journal/forensics.
pub fn capture_wedge_message(seconds_since_last_progress: f64, threshold_s: f64) -> String {
    format!(
        "CRITICAL #945: capture/emit thread WEDGED — the blocking V4L2 dequeue (VIDIOC_DQBUF) has \
         not returned in {seconds_since_last_progress:.1}s (>= {threshold_s:.1}s threshold); the \
         process is alive (other threads keep running) but the capture loop is provably dead — \
         exiting now (code {CAPTURE_WEDGE_EXIT_CODE}) so systemd's Restart=always recovers it. See #945."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // evaluate_wedge — the pure threshold decision.

    #[test]
    fn recent_heartbeat_is_not_wedged() {
        assert_eq!(
            evaluate_wedge(0.0, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
        assert_eq!(
            evaluate_wedge(5.0, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
    }

    #[test]
    fn just_under_the_threshold_is_not_wedged() {
        assert_eq!(
            evaluate_wedge(CAPTURE_WEDGE_THRESHOLD_S - 0.01, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
    }

    #[test]
    fn exactly_at_the_threshold_is_wedged_inclusive() {
        assert_eq!(
            evaluate_wedge(CAPTURE_WEDGE_THRESHOLD_S, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Wedged
        );
    }

    #[test]
    fn well_past_the_threshold_is_wedged() {
        // The live #945 incident: 48 minutes (2880s) of total silence.
        assert_eq!(
            evaluate_wedge(2880.0, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Wedged
        );
    }

    #[test]
    fn the_stats_tick_clock_driven_forward_with_no_progress_eventually_wedges() {
        // Simulates the exact scenario the ticket describes: drive a clock forward in 5s
        // "stats tick" increments (the existing report cadence) with NO capture progress at
        // all, and confirm the watchdog decides WEDGED only once the threshold is actually
        // crossed — never earlier.
        let tick_s = 5.0;
        let mut elapsed = 0.0;
        let mut wedged_tick = None;
        for tick in 1..=10 {
            elapsed += tick_s;
            if evaluate_wedge(elapsed, CAPTURE_WEDGE_THRESHOLD_S) == WedgeVerdict::Wedged {
                wedged_tick = Some(tick);
                break;
            }
        }
        assert_eq!(
            wedged_tick,
            Some(5),
            "must wedge at exactly the 5th 5s tick (25s), matching CAPTURE_WEDGE_THRESHOLD_S"
        );
    }

    #[test]
    fn non_finite_or_negative_elapsed_never_wedges_defensive_guard() {
        assert_eq!(
            evaluate_wedge(f64::NAN, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
        assert_eq!(
            evaluate_wedge(f64::INFINITY, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
        assert_eq!(
            evaluate_wedge(-1.0, CAPTURE_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
    }

    #[test]
    fn non_positive_threshold_never_wedges_disabled_watchdog() {
        assert_eq!(evaluate_wedge(999_999.0, 0.0), WedgeVerdict::Alive);
        assert_eq!(evaluate_wedge(999_999.0, -5.0), WedgeVerdict::Alive);
    }

    // capture_wedge_message — pure message formatting.

    #[test]
    fn message_carries_the_numbers_and_mentions_945_and_critical() {
        let msg = capture_wedge_message(48.0 * 60.0, CAPTURE_WEDGE_THRESHOLD_S);
        assert!(msg.contains("2880.0"));
        assert!(msg.contains("25.0"));
        assert!(msg.contains("#945"));
        assert!(msg.contains("CRITICAL"));
        assert!(msg.contains(&CAPTURE_WEDGE_EXIT_CODE.to_string()));
    }

    #[test]
    fn message_mentions_the_dequeue_and_is_never_empty() {
        let msg = capture_wedge_message(30.0, CAPTURE_WEDGE_THRESHOLD_S);
        assert!(!msg.is_empty());
        assert!(msg.to_lowercase().contains("dequeue"));
        assert!(msg.to_lowercase().contains("wedged"));
    }

    #[test]
    fn message_never_reads_as_a_663_self_heal_reset() {
        // The whole point of a distinct exit code + wording: forensics must never confuse this
        // with the #663 self-heal USB-reset path.
        let msg = capture_wedge_message(30.0, CAPTURE_WEDGE_THRESHOLD_S);
        assert!(!msg.contains("#663"));
        assert!(!msg.to_lowercase().contains("self-heal"));
        assert!(!msg.to_lowercase().contains("usb reset"));
    }
}
