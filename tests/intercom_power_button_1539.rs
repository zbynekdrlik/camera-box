//! Issue 1539 (the power-button finding of 25.11.2026: every PP cambox `send 0.0 pkt/s` all day):
//! drives the REAL power-button loop `camera_box::intercom::run_power_button_loop` through pipe fds
//! that carry real 24-byte Linux `input_event` structs, and reads the ONE shared `muted` flag the
//! intercom loop gates its VBAN send on (`if !is_muted` in `run_intercom_inner`) and the preview
//! switch. A pipe stands in for `/dev/input/eventN` (the loop only `read`s 24-byte events from the
//! fd); `/dev/uinput` is root-only, so a virtual input device could not run here or in CI.

use camera_box::intercom::run_power_button_loop;
use camera_box::preview_tap::{PreviewSource, PreviewSwitch};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;

/// One `struct input_event` on x86_64: timeval (16 bytes), type, code, value.
fn event(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[16..18].copy_from_slice(&kind.to_ne_bytes());
    b[18..20].copy_from_slice(&code.to_ne_bytes());
    b[20..24].copy_from_slice(&value.to_ne_bytes());
    b
}

struct Rig {
    writers: Vec<i32>,
    muted: Arc<AtomicBool>,
    preview: Arc<PreviewSwitch>,
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Rig {
    /// `n` power-button "devices", the mic starting MUTED like main.rs creates it.
    fn start(n: usize) -> Rig {
        let mut devices = Vec::new();
        let mut writers = Vec::new();
        for i in 0..n {
            let mut fds = [0i32; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
            devices.push((format!("/dev/input/event{}", i + 1), fds[0]));
            writers.push(fds[1]);
        }
        let muted = Arc::new(AtomicBool::new(true));
        let preview = PreviewSwitch::new();
        let running = Arc::new(AtomicBool::new(true));
        let (m, p, r) = (
            Arc::clone(&muted),
            Arc::clone(&preview),
            Arc::clone(&running),
        );
        let thread = std::thread::spawn(move || run_power_button_loop(devices, m, p, r));
        Rig {
            writers,
            muted,
            preview,
            running,
            thread: Some(thread),
        }
    }

    fn write(&self, device: usize, events: &[[u8; 24]]) {
        for e in events {
            let n = unsafe { libc::write(self.writers[device], e.as_ptr() as *const _, e.len()) };
            assert_eq!(n, 24, "pipe write");
        }
    }

    /// The events one physical press + release puts on an evdev power-button device.
    fn press(&self, device: usize) {
        self.write(
            device,
            &[
                event(EV_KEY, KEY_POWER, 1),
                event(EV_SYN, 0, 0),
                event(EV_KEY, KEY_POWER, 0),
                event(EV_SYN, 0, 0),
            ],
        );
    }

    fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Waits up to `within` for the mic flag to read `want`.
    fn wait_muted(&self, want: bool, within: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < within {
            if self.muted() == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.muted() == want
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        for fd in &self.writers {
            unsafe { libc::close(*fd) };
        }
    }
}

/// The 400 ms double-press window + the 20 ms poll + slack.
const TOGGLE_WITHIN: Duration = Duration::from_millis(1500);

#[test]
fn a_single_press_unmutes_the_shared_mic_flag_and_the_next_press_mutes_it_1539() {
    let rig = Rig::start(3);
    assert!(rig.muted(), "starts MUTED like main.rs");
    rig.press(0);
    assert!(
        rig.wait_muted(false, TOGGLE_WITHIN),
        "one press must UNMUTE the mic the intercom send reads"
    );
    assert_eq!(
        rig.preview.source(),
        PreviewSource::Strih,
        "a single press never flips the preview"
    );
    // the next press, after the double-press window, from another of the box's devices
    std::thread::sleep(Duration::from_millis(500));
    rig.press(2);
    assert!(
        rig.wait_muted(true, TOGGLE_WITHIN),
        "the next press MUTES it again"
    );
    assert_eq!(rig.preview.source(), PreviewSource::Strih);
}

#[test]
fn a_quick_double_press_flips_only_the_preview_1539() {
    let rig = Rig::start(3);
    rig.press(1);
    std::thread::sleep(Duration::from_millis(150));
    rig.press(1);
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(1000) && rig.preview.source() == PreviewSource::Strih
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        rig.preview.source(),
        PreviewSource::Local,
        "double press = preview switch"
    );
    std::thread::sleep(Duration::from_millis(600));
    assert!(rig.muted(), "a double press never touches the mic");
}

#[test]
fn autorepeat_and_release_events_alone_never_toggle_1539() {
    let rig = Rig::start(1);
    rig.write(
        0,
        &[
            event(EV_KEY, KEY_POWER, 2),
            event(EV_KEY, KEY_POWER, 0),
            event(EV_SYN, 0, 0),
        ],
    );
    std::thread::sleep(Duration::from_millis(800));
    assert!(rig.muted());
    assert_eq!(rig.preview.source(), PreviewSource::Strih);
}
