//! The cambox power button (split out of `intercom.rs`, issue 1539). A single press toggles the
//! cameraman's microphone, the ONE shared `muted` flag the intercom loop gates its VBAN send on; a
//! double press flips the cameraman preview (issue 1501). Device discovery opens every
//! `/dev/input/eventN` that supports KEY_POWER; [`run_power_button_loop`] reads them, so a test can
//! drive the real loop through pipe fds (`tests/intercom_power_button_1539.rs`).

use evdev::{Device, Key};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn find_power_buttons() -> Vec<(String, i32)> {
    let mut devices = Vec::new();
    for i in 0..10 {
        let path = format!("/dev/input/event{}", i);
        if let Ok(device) = Device::open(&path) {
            if let Some(keys) = device.supported_keys() {
                if keys.contains(Key::KEY_POWER) {
                    let name = device.name().unwrap_or("unknown").to_string();
                    tracing::info!("Found power button: {} ({})", name, path);
                    use std::os::unix::io::AsRawFd;
                    let fd = device.as_raw_fd();
                    let dup_fd = unsafe { libc::dup(fd) };
                    if dup_fd >= 0 {
                        devices.push((path, dup_fd));
                    }
                }
            }
        }
    }
    devices
}

/// Issue 1539: the most input events one power-button pass reads from one device.
const MAX_EVENTS_PER_PASS: usize = 256;

/// Issue 1539: a 24-byte x86_64 `struct input_event` that is a KEY_POWER press (EV_KEY 116, value
/// 1; the release 0 and the autorepeat 2 are not presses).
fn is_power_key_press(event: &[u8; 24]) -> bool {
    let event_type = u16::from_ne_bytes([event[16], event[17]]);
    let event_code = u16::from_ne_bytes([event[18], event[19]]);
    let event_value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
    event_type == 1 && event_code == 116 && event_value == 1
}

/// Issue 1501: the power button. A single press toggles the microphone (after the double-press
/// window, `crate::button_press`), a double press flips the cameraman preview between the strih NDI
/// feed and the local camera (`crate::preview_tap::PreviewSwitch`). Spawned by main.rs for every
/// box, intercom or not.
pub fn run_power_button_monitor(
    muted: Arc<AtomicBool>,
    preview: Arc<crate::preview_tap::PreviewSwitch>,
    running: Arc<AtomicBool>,
) {
    let devices = find_power_buttons();
    if devices.is_empty() {
        tracing::warn!("No power button found - mute toggle disabled");
        return;
    }
    run_power_button_loop(devices, muted, preview, running);
}

/// The power-button loop over already-open input-event fds (`(path, fd)`, the fds are owned and
/// closed here). Split from [`run_power_button_monitor`] so a test can drive the real loop through
/// pipe fds carrying real 24-byte `input_event` structs (issue 1539).
pub fn run_power_button_loop(
    devices: Vec<(String, i32)>,
    muted: Arc<AtomicBool>,
    preview: Arc<crate::preview_tap::PreviewSwitch>,
    running: Arc<AtomicBool>,
) {
    for (_path, fd) in &devices {
        unsafe {
            let flags = libc::fcntl(*fd, libc::F_GETFL);
            libc::fcntl(*fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    tracing::info!(
        "Power button mute toggle enabled ({} devices)",
        devices.len()
    );
    let mut event_buf = [0u8; 24];
    let mut classifier = crate::button_press::PressClassifier::default();
    let mut echo = crate::button_press::PressEcho::default();
    let mut read_failed = vec![false; devices.len()];
    let clock = std::time::Instant::now();
    let mut last_path = String::new();
    let act = |action: crate::button_press::PressAction, path: &str| match action {
        crate::button_press::PressAction::Single => {
            let was_muted = muted.fetch_xor(true, Ordering::Relaxed);
            let now_muted = !was_muted;
            tracing::info!(
                "🎤 Microphone {} (via {}, single press)",
                if now_muted { "MUTED" } else { "UNMUTED" },
                path
            );
        }
        crate::button_press::PressAction::Double => {
            let now = preview.toggle();
            tracing::info!(
                "preview: double press (via {}) -> the cameraman preview shows {} (issue 1501)",
                path,
                match now {
                    crate::preview_tap::PreviewSource::Local => "the LOCAL camera",
                    crate::preview_tap::PreviewSource::Strih => "the strih NDI feed",
                }
            );
        }
    };

    crate::preview_tap::install_preview_toggle_signal();
    while running.load(Ordering::Relaxed) {
        let now_ms = clock.elapsed().as_millis() as u64;
        if let Some(a) = classifier.tick(now_ms) {
            act(a, &last_path);
        }
        // owner 13.11.2026: `systemctl kill -s SIGUSR1 --kill-whom=main camera-box` = one double press
        if crate::preview_tap::take_preview_toggle_request() {
            act(crate::button_press::PressAction::Double, "SIGUSR1");
        }
        // Issue 1539: drain EVERY queued event of each device per pass (one event per pass
        // delayed a press behind EV_MSC/EV_SYN), and count a press another device echoes once.
        // Bounded per pass so a device that never stops reporting cannot hold the loop.
        for (index, (path, fd)) in devices.iter().enumerate() {
            for _ in 0..MAX_EVENTS_PER_PASS {
                let n = unsafe {
                    libc::read(
                        *fd,
                        event_buf.as_mut_ptr() as *mut libc::c_void,
                        event_buf.len(),
                    )
                };
                if n != 24 {
                    if n < 0 && !read_failed[index] {
                        let err = std::io::Error::last_os_error();
                        if err.kind() != std::io::ErrorKind::WouldBlock
                            && err.kind() != std::io::ErrorKind::Interrupted
                        {
                            read_failed[index] = true;
                            tracing::warn!(
                                "power button: reading {} failed ({}), its presses are lost (issue 1539)",
                                path,
                                err
                            );
                        }
                    }
                    break;
                }
                if !is_power_key_press(&event_buf) {
                    continue;
                }
                let at = clock.elapsed().as_millis() as u64;
                if !echo.accept(at, index) {
                    tracing::info!(
                        "power button: the same press also reported by {}, counted once (issue 1539)",
                        path
                    );
                    continue;
                }
                if let Some(a) = classifier.press(at) {
                    act(a, &last_path);
                }
                last_path.clone_from(path);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    for (_path, fd) in devices {
        unsafe { libc::close(fd) };
    }
}
