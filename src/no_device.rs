//! #828 — no-capture-device startup handling (pure seam).
//!
//! A cam box whose USB grabber is absent (physically removed, dead, or not yet fitted)
//! used to `bail!` out of `main()` immediately (`config::find_capture_device`), which — with
//! the unit's `Restart=always` + `RestartSec=3` and no `StartLimit*` — produced a ~3 s restart
//! storm (cam4 incident 2026-07-27: `NRestarts=27719`) with the "no device" reason buried in
//! the multi-line startup logs of every cycle.
//!
//! Instead, the auto-detect path now settles into a SLOW, clearly-logged in-process retry: on
//! each no-device cycle it logs ONE clear line and sleeps a fixed backoff before re-probing,
//! so the process NEVER exits on this path (0 restarts, not 28k) and auto-recovers within one
//! interval when a USB grabber (re-)appears — while `RestartSec=3` stays fast for a genuine
//! mid-stream transient crash.
//!
//! This module holds the PURE loop decision (parameterised over a device-probe closure and a
//! sleep closure) so it unit-tests Tier-0 on default features. The real probe
//! (`config::find_capture_device_opt`, a `v4l` scan) and the real sleep (`std::thread::sleep`)
//! are injected by `src/main.rs`.

use std::time::Duration;

/// The single clear one-line state logged on each no-device retry cycle. Deliberately short and
/// operator-actionable — this is what a `journalctl -u camera-box` glance must show as the cause.
pub const NO_CAPTURE_DEVICE_MSG: &str = "no capture device — check the grabber";

/// Backoff between no-device re-probes, in seconds. Small enough that a (re-)fitted USB grabber
/// is picked up "within a minute", large enough that the box is quiet (one log line / probe per
/// interval) instead of storming.
pub const NO_DEVICE_RETRY_SECS: u64 = 30;

/// Resolve a capture device, waiting (slow, clearly-logged retry) instead of failing when none
/// is present. Returns as soon as `detect` yields a device path.
///
/// - `detect`: probes for a capture device, `Some(path)` when one is present, `None` otherwise.
/// - `sleep`: called with the backoff `Duration` on each no-device cycle (real: `thread::sleep`).
pub fn wait_for_capture_device<D, S>(mut detect: D, mut sleep: S) -> String
where
    D: FnMut() -> Option<String>,
    S: FnMut(Duration),
{
    loop {
        if let Some(path) = detect() {
            return path;
        }
        // One clear, operator-actionable line per cycle — this is the cause a
        // `journalctl -u camera-box` glance must show, instead of it being buried in the
        // multi-line startup logs of a ~3 s restart storm.
        tracing::warn!("{}", NO_CAPTURE_DEVICE_MSG);
        sleep(Duration::from_secs(NO_DEVICE_RETRY_SECS));
    }
}
