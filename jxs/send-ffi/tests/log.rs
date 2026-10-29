//! Issue 1387 slice 3 -- the sender's own log lines reach the C caller's callback (the OBS output
//! hands them to the OBS log): `cbjxs_send_set_log` installs the process's subscriber once, the
//! "link ON" and "encoder open" lines arrive with their level, NULL stops the forwarding.
//! Its own test binary: the subscriber is process-global.
#![cfg(target_os = "linux")]

use jxs_send_ffi::*;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static LINES: Mutex<Vec<(c_int, String)>> = Mutex::new(Vec::new());

unsafe extern "C" fn collect(level: c_int, msg: *const c_char) {
    let s = CStr::from_ptr(msg).to_string_lossy().into_owned();
    LINES.lock().unwrap().push((level, s));
}

#[test]
fn the_sender_log_lines_reach_the_callback_and_null_stops_them() {
    assert_eq!(
        cbjxs_send_set_log(Some(collect)),
        1,
        "the subscriber is ours"
    );
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = CString::new(format!("127.0.0.1:{}", rx.local_addr().unwrap().port())).unwrap();
    let cfg = CbjxsSendConfig {
        target: target.as_ptr(),
        cores: std::ptr::null(),
        emit_fps: 30,
        stream_id: 100,
        bpp_min: 6,
        bpp_max: 6,
        reserved: 0,
    };
    let tx = unsafe { cbjxs_send_open(&cfg, std::ptr::null_mut(), 0) };
    assert!(!tx.is_null());
    // one 64x16 I420 picture: the feeder opens the encoder on it
    let y = vec![100u8; 64 * 16];
    let c = vec![128u8; 32 * 8];
    let pic = CbjxsSendPicture {
        format: CBJXS_SEND_I420,
        width: 64,
        height: 16,
        reserved: 0,
        planes: [y.as_ptr(), c.as_ptr(), c.as_ptr()],
        strides: [64, 32, 32],
        reserved2: 0,
        timecode_100ns: 17_617_300_000_000_000,
    };
    assert_eq!(unsafe { cbjxs_send_frame(tx, &pic) }, 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    let has = |needle: &str| {
        LINES
            .lock()
            .unwrap()
            .iter()
            .any(|(l, s)| *l == CBJXS_LOG_INFO && s.contains(needle))
    };
    while !(has("jxs send: link ON") && has("encoder open 64x16 4:2:0, 6 bpp"))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(has("jxs send: link ON"), "{:?}", LINES.lock().unwrap());
    assert!(has("6 bpp (fixed"), "min == max pins the bpp");
    assert!(
        has("encoder open 64x16 4:2:0, 6 bpp"),
        "{:?}",
        LINES.lock().unwrap()
    );
    assert!(
        LINES
            .lock()
            .unwrap()
            .iter()
            .all(|(_, s)| !s.ends_with('\n')),
        "one line per call, no newline"
    );
    // NULL stops it; a second install reports the same subscriber
    assert_eq!(cbjxs_send_set_log(None), 1);
    let before = LINES.lock().unwrap().len();
    unsafe { cbjxs_send_close(tx) };
    assert_eq!(LINES.lock().unwrap().len(), before, "nothing after NULL");
}

/// `CBJXS_SEND_LOG_INFO` of the header.
const CBJXS_LOG_INFO: c_int = 3;
