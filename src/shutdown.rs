//! #1186 — graceful in-process shutdown on SIGTERM/SIGINT/SIGHUP for the
//! frame-probe painter, so a `systemctl stop cam2-painter.service` (or any other
//! signal-driven stop) runs the SAME issue-660 framebuffer blank a clean
//! `--duration-secs` self-exit already does.
//!
//! ## Why this exists
//!
//! The issue-660 blank (`probe::fb::blank_fbdev`) runs ONLY inside
//! `probe::kms::KmsPresenter`'s `Drop` — which fires only when the painter's
//! paint loop breaks and `run_painter` returns (a clean `--duration-secs`
//! self-exit, or the outer loop setting the shared `stop` flag). A
//! `systemctl stop cam2-painter.service` sends SIGTERM, whose DEFAULT
//! disposition terminates the process immediately with NO stack unwind — so
//! `Drop` never runs, `/dev/fb0` keeps the last painted frame, and the kernel
//! fbdev emulation reveals that stale frame on cam2's HDMI monitor (the #660
//! mechanism). Per the owner (issue 1176) the unit stop is the most common exit
//! path since issue 892, so this in-process handler "remains necessary
//! regardless" of the rig-mode.sh EVENT-path blank (issue 1176 prong 2).
//!
//! ## Design (issue 1176 prong 1)
//!
//! An async-signal-safe handler for SIGTERM/SIGINT/SIGHUP does the ONLY safe
//! kind of work a signal handler may do: a single atomic store into a
//! process-global flag ([`request_shutdown`]). The frame-probe painter loops
//! ([`crate::probe::painter::run_painter`] and the outer
//! [`crate::probe::run::run_paint_only`] / [`crate::probe::run::run`] loops)
//! poll that flag ([`is_shutdown_requested`] / [`painter_should_continue`]) and
//! break, so the EXISTING, tested graceful teardown runs: the painter returns,
//! its `KmsPresenter` drops, `blank_fbdev` clears `/dev/fb0` BEFORE releasing
//! DRM master, and the fbdev emulation reveals a deterministic black frame. The
//! blank stays inside the presenter's `Drop`, preserving the #660
//! blank-before-release-master ordering; the handler never touches the
//! framebuffer or DRM itself (that would be neither async-signal-safe nor
//! correctly ordered against a still-page-flipping painter).
//!
//! The PURE half (the flag + [`painter_should_continue`]) is `std`-only and
//! Tier-0-verifiable via a `rustc --test` replica (the repo pattern —
//! `src/fb_blank.rs`, `src/reannounce.rs`, `src/colour_scale.rs`); the OS glue
//! ([`install`]) uses the crate's existing `libc` syscall layer and is
//! `#[cfg(target_os = "linux")]` (the cameras are x86_64 Ubuntu), compiled by
//! the default-feature CI build but with no local run path.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-global "a graceful shutdown was requested" flag. Set (only ever true,
/// never reset in production) by the async-signal-safe signal handler; polled by
/// the frame-probe painter loops.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Whether a SIGTERM/SIGINT/SIGHUP has been received. Polled by the painter
/// loops so they break and let the presenter's `Drop` run the #660 blank.
pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Request a graceful shutdown — the ENTIRE body of the signal handler. An
/// atomic store is the only async-signal-safe work a handler may do (it is a
/// single lock-free instruction on x86_64, the only target the cameras run; no
/// allocation, no locks, no I/O).
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Test-only reset of the global flag so a unit test can observe the
/// clear→set→clear round-trip without a cross-test race (only one test touches
/// the global). `#[cfg(test)]` so it is never present in a release build (no
/// `dead_code` warning under `-D warnings`).
#[cfg(test)]
fn reset_for_test() {
    SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
}

/// Pure loop-decision: the painter keeps painting iff it was NOT locally stopped
/// AND no shutdown signal has been requested. The deployed `run_painter` loop
/// calls exactly this, so the Tier-0-tested logic is the shipped logic.
pub fn painter_should_continue(local_stop: bool, shutdown_requested: bool) -> bool {
    !local_stop && !shutdown_requested
}

/// Install the SIGTERM/SIGINT/SIGHUP handlers (idempotent). Call once at painter
/// start. A no-op on non-Linux (the cameras are Linux; the #193 Windows
/// recording-verdict build never runs the painter).
pub fn install() {
    #[cfg(target_os = "linux")]
    INSTALLED.call_once(install_posix_handlers);
}

#[cfg(target_os = "linux")]
static INSTALLED: std::sync::Once = std::sync::Once::new();

/// The signal handler: async-signal-safe — its whole body is [`request_shutdown`]
/// (a single atomic store). `extern "C"` because the kernel calls it directly.
#[cfg(target_os = "linux")]
extern "C" fn handle_shutdown_signal(_sig: libc::c_int) {
    request_shutdown();
}

/// Register [`handle_shutdown_signal`] for SIGTERM (systemd default
/// `KillSignal`), SIGINT (Ctrl-C), and SIGHUP (terminal close / reload) via
/// `sigaction`. `SA_RESTART` so restartable syscalls resume; the painter's KMS
/// present() uses a non-blocking fd + `std::thread::sleep` (itself EINTR-safe),
/// so an interrupted flip does not error — the loop simply re-checks the flag
/// and breaks within one frame. Even were a presenter to return `Err` on
/// interruption, its `Drop` still runs the blank, so the #660 clear is
/// guaranteed on every signal path.
#[cfg(target_os = "linux")]
fn install_posix_handlers() {
    let signals = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];
    // SAFETY: `sigaction` is a standard POSIX syscall; `sa` is a fully
    // zero-initialised C struct with only the handler + flags + (emptied) mask
    // set, and the handler itself does only an async-signal-safe atomic store.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_shutdown_signal as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in signals {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_shutdown_requested, painter_should_continue, request_shutdown, reset_for_test};

    /// The whole flag contract in one test (only this test touches the global,
    /// so there is no cross-test race): starts clear, `request_shutdown` sets it
    /// (this is the exact body the signal handler runs), reset clears it again.
    #[test]
    fn flag_round_trip() {
        reset_for_test();
        assert!(!is_shutdown_requested(), "flag must start clear");
        request_shutdown();
        assert!(
            is_shutdown_requested(),
            "request_shutdown() must set the shutdown flag"
        );
        reset_for_test();
        assert!(!is_shutdown_requested(), "reset must clear the flag");
    }

    /// The painter keeps painting only while neither a local stop nor a shutdown
    /// signal is pending — the new behavior is that a shutdown signal alone
    /// stops it (so its presenter drops and the #660 blank runs).
    #[test]
    fn painter_stops_on_shutdown() {
        assert!(
            painter_should_continue(false, false),
            "running normally: keep painting"
        );
        assert!(
            !painter_should_continue(true, false),
            "local stop flag set: stop"
        );
        assert!(
            !painter_should_continue(false, true),
            "a shutdown signal must stop the painter so its presenter drops + blanks fb0"
        );
    }
}
