//! #828 — `camera_box::no_device::wait_for_capture_device` slow-retry loop.
//!
//! A box with no capture device must settle into a slow, clearly-logged retry (and auto-recover
//! when a grabber appears) instead of bailing and restart-storming. Proven with injected probe +
//! sleep closures — no hardware, Tier-0.

use std::time::Duration;

#[test]
fn returns_first_device_without_sleeping_when_present_immediately() {
    let mut sleeps = 0usize;
    let got = camera_box::no_device::wait_for_capture_device(
        || Some("/dev/video0".to_string()),
        |_d| sleeps += 1,
    );
    assert_eq!(got, "/dev/video0");
    assert_eq!(sleeps, 0, "a healthy box must not sleep/churn at startup");
}

#[test]
fn retries_until_a_device_appears_then_returns_it() {
    // No device on the first two probes, then a grabber appears (e.g. USB (re-)plugged).
    let mut seq = vec![None, None, Some("/dev/video1".to_string())].into_iter();
    let mut sleeps = 0usize;
    let got =
        camera_box::no_device::wait_for_capture_device(|| seq.next().flatten(), |_d| sleeps += 1);
    assert_eq!(got, "/dev/video1");
    assert_eq!(
        sleeps, 2,
        "must back off once per no-device cycle (2 no-device probes -> 2 sleeps) before recovering"
    );
}

#[test]
fn backoff_uses_the_configured_retry_interval_and_clear_message() {
    assert_eq!(camera_box::no_device::NO_DEVICE_RETRY_SECS, 30);
    assert_eq!(
        camera_box::no_device::NO_CAPTURE_DEVICE_MSG,
        "no capture device — check the grabber"
    );
    // Each no-device cycle sleeps for exactly the configured interval.
    let mut seq = vec![None, Some("/dev/video1".to_string())].into_iter();
    let mut last = Duration::ZERO;
    let _ = camera_box::no_device::wait_for_capture_device(|| seq.next().flatten(), |d| last = d);
    assert_eq!(last, Duration::from_secs(30));
}
