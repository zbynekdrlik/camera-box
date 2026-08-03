//! #936 — painter WEDGE self-watchdog (pure decision + message, Tier-0).
//!
//! `KmsPresenter::present()` (`src/probe/kms.rs`) issues a blocking `drmModePageFlip` ioctl, then
//! waits (bounded, 500ms non-blocking poll) for the flip-complete event. That 500ms bound only
//! covers the EVENT WAIT — it cannot bound the ioctl call itself, nor `map_dumb_buffer()`'s
//! mmap+copy, nor the one-time `acquire_master_lock()` at open. Live incident (issue 930 paired
//! run, 2026-08-02): the rig's `--paint-only --dual-qr` TEST-mode painter on cam2 SURVIVED both a
//! bare SIGTERM and a follow-up SIGKILL (the already-merged `e14bfc432` escalation) while wedged,
//! kept flipping KMS pages with the dual-QR frozen on screen, and the whole paired lipsync
//! recording captured the QR pattern instead of the talking face.
//!
//! Root cause (#936 investigation): a genuine DRM/KMS kernel-level hang (a GPU reset stuck deep
//! inside the ioctl, or a similar hardware/driver fault) parks the calling thread in
//! `TASK_UNINTERRUPTIBLE` ("D state") — by Linux kernel design NO signal, including SIGKILL, can
//! preempt a thread in that state until the blocking kernel call itself returns. This is not a bug
//! in this codebase; it is the exact same class of failure [`crate::capture_wedge`] (#945) already
//! root-caused three days earlier on the V4L2 capture dequeue, and the fix mirrors it exactly: an
//! OUT-OF-BAND watchdog thread (which can never itself be blocked by the same ioctl) polls a
//! monotonic heartbeat the painter loop updates immediately after every successful
//! `paint_one_frame()` call, and forces the process to exit loudly the moment that heartbeat goes
//! stale — because no signal can un-stick the thread, and no in-process code can either, exiting
//! (which the kernel CAN still do for every OTHER, non-stuck thread) plus a distinctly-worded
//! CRITICAL log line is the most this process can do; recovery is systemd's `Restart=always`
//! (`cam2-painter.service`) or the rig operator's own tooling.
//!
//! This module is the PURE decision + message half (Tier-0, default features, no I/O) — it reuses
//! [`crate::capture_wedge::evaluate_wedge`]/[`crate::capture_wedge::WedgeVerdict`] (the threshold
//! math is fully generic, no capture-specific assumption baked in) rather than duplicating it. The
//! atomic heartbeat, the watchdog thread, and the `std::process::exit` call live in
//! `src/probe/run.rs` (`run_paint_only`/`run`); the painter's own heartbeat store lives in
//! `src/probe/painter.rs` (`run_painter`) — both probe-gated (Linux + `--features probe`), so
//! review them by hand (no local build/test path — see CLAUDE.md's Local Build Policy). Per
//! `.claude/rules/self-heal-frozen-leg-attribution.md` this event stays its OWN distinctly-worded
//! CRITICAL line + its OWN exit code (never overloaded onto `#945`'s or `#663`'s wording).
//!
/// How many seconds the painter loop may go WITHOUT a successful `paint_one_frame()` completing
/// before the watchdog treats it as WEDGED. The KMS vblank-locked path ticks every ~16.6ms (60Hz);
/// `KmsPresenter::wait_flip_complete()`'s OWN internal 500ms non-blocking-poll timeout already
/// bails (and unwinds the painter thread with a clean `Err`) well before this fires on an ordinary
/// "flip issued, event never arrived" stall — so this threshold exists for the harder case: a
/// block inside `page_flip()`'s ioctl issuance itself (or a genuine kernel D-state hang) that
/// `wait_flip_complete` never even gets scheduled to reach. ~6x the existing 500ms internal bound
/// gives that ordinary path ample margin to finish unwinding on its own; still catches a genuine
/// indefinite hang in single-digit seconds instead of leaving an entire multi-minute recording
/// silently ruined (issue 930's paired run).
pub const PAINTER_WEDGE_THRESHOLD_S: f64 = 3.0;

// Compile-time guarantee (not a runtime clippy::assertions_on_constants target): the threshold
// must always give the existing 500ms wait_flip_complete() event timeout ample margin to unwind
// on its own first, so this watchdog never double-reports that already-handled stall as a wedge.
const _: () = assert!(PAINTER_WEDGE_THRESHOLD_S >= 2.0);

/// Process exit code used when the watchdog forces an exit because the painter loop is provably
/// wedged. Distinct from `capture_wedge::CAPTURE_WEDGE_EXIT_CODE` (79) and the `#663`/self-heal
/// codes (77/78) so `systemctl status`/journal forensics can always tell a painter-wedge exit
/// apart from a capture-side wedge or a USB self-heal restart.
pub const PAINTER_WEDGE_EXIT_CODE: i32 = 80;

/// Build the CRITICAL, uniquely grep-able message the watchdog logs right before it exits the
/// process. Pure string formatting so the exact wording is unit-tested here, mirroring
/// `capture_wedge::capture_wedge_message`'s convention. Names #936, the DRM/KMS hypothesis, and the
/// exact exit code so this event can never be confused with a `#945` capture wedge or a `#663`
/// self-heal restart.
pub fn painter_wedge_message(seconds_since_last_progress: f64, threshold_s: f64) -> String {
    format!(
        "CRITICAL #936: painter thread WEDGED — no frame painted in \
         {seconds_since_last_progress:.1}s (>= {threshold_s:.1}s threshold); the process is alive \
         (other threads keep running) but the DRM/KMS present() call is provably stuck — likely a \
         kernel-level GPU/display hang (TASK_UNINTERRUPTIBLE / \"D state\"), which NO signal \
         (including SIGKILL) can preempt until the blocking kernel call itself returns. Exiting \
         now (code {PAINTER_WEDGE_EXIT_CODE}) so a supervisor (systemd Restart=always on \
         cam2-painter.service, or the rig operator's own tooling) can recover it. See #936."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_wedge::{evaluate_wedge, WedgeVerdict};

    // Threshold math is reused verbatim from capture_wedge (already tested there) — these tests
    // just confirm THIS module's own constant wires into it correctly.

    #[test]
    fn recent_heartbeat_is_not_wedged() {
        assert_eq!(
            evaluate_wedge(0.0, PAINTER_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
    }

    #[test]
    fn just_under_the_threshold_is_not_wedged() {
        assert_eq!(
            evaluate_wedge(PAINTER_WEDGE_THRESHOLD_S - 0.01, PAINTER_WEDGE_THRESHOLD_S),
            WedgeVerdict::Alive
        );
    }

    #[test]
    fn exactly_at_the_threshold_is_wedged_inclusive() {
        assert_eq!(
            evaluate_wedge(PAINTER_WEDGE_THRESHOLD_S, PAINTER_WEDGE_THRESHOLD_S),
            WedgeVerdict::Wedged
        );
    }

    #[test]
    fn well_past_the_threshold_is_wedged() {
        assert_eq!(
            evaluate_wedge(60.0, PAINTER_WEDGE_THRESHOLD_S),
            WedgeVerdict::Wedged
        );
    }

    // The "threshold is meaningfully above the existing 500ms KMS event timeout" guarantee is now
    // a compile-time `const _: () = assert!(...)` right after PAINTER_WEDGE_THRESHOLD_S's
    // definition (a runtime assert on a const value trips clippy::assertions_on_constants).

    // painter_wedge_message — pure message formatting.

    #[test]
    fn message_carries_the_numbers_and_mentions_936_and_critical() {
        let msg = painter_wedge_message(5.0, PAINTER_WEDGE_THRESHOLD_S);
        assert!(msg.contains("5.0"));
        assert!(msg.contains(&format!("{PAINTER_WEDGE_THRESHOLD_S:.1}")));
        assert!(msg.contains("#936"));
        assert!(msg.contains("CRITICAL"));
        assert!(msg.contains(&PAINTER_WEDGE_EXIT_CODE.to_string()));
    }

    #[test]
    fn message_mentions_the_wedge_and_drm() {
        let msg = painter_wedge_message(4.0, PAINTER_WEDGE_THRESHOLD_S);
        assert!(!msg.is_empty());
        assert!(msg.to_lowercase().contains("wedged"));
        assert!(msg.to_uppercase().contains("DRM"));
    }

    #[test]
    fn message_never_reads_as_a_945_capture_wedge_or_663_self_heal() {
        // The whole point of a distinct exit code + wording: forensics must never confuse this
        // with the #945 capture wedge or the #663 self-heal USB reset.
        let msg = painter_wedge_message(4.0, PAINTER_WEDGE_THRESHOLD_S);
        assert!(!msg.contains("#945"));
        assert!(!msg.contains("#663"));
        assert!(!msg.to_lowercase().contains("self-heal"));
        assert!(!msg.to_lowercase().contains("dequeue"));
    }

    #[test]
    fn exit_code_never_collides_with_existing_self_heal_or_capture_wedge_codes() {
        assert_ne!(
            PAINTER_WEDGE_EXIT_CODE,
            crate::capture_wedge::CAPTURE_WEDGE_EXIT_CODE
        );
        assert_ne!(PAINTER_WEDGE_EXIT_CODE, 77);
        assert_ne!(PAINTER_WEDGE_EXIT_CODE, 78);
    }
}
