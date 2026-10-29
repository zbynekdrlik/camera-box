//! Issue 1387 slice 2: a C ABI over camera-box's JPEG XS receiver.
//!
//! The strih OBS source (`vendor/distroav/src/jxs-source.cpp`) links this crate's static library
//! and calls the functions declared in `include/camera_box_jxs.h` (written by hand; `tests/abi.rs`
//! pins every size, offset and name against this file). Every call wraps ONE existing type,
//! `camera_box::jxs_recv::JxsReceiver`, the receive loop `jxs-recv-probe` runs too: reassembly,
//! slice-wise decode, concealment and the 32-byte receiver report back to the sender all happen
//! inside it, so there is one receiver implementation for the probe and for OBS.
//!
//! No panic crosses the boundary: each call runs under `catch_unwind`. A panic marks the receiver
//! unusable (`-2` from then on); the caller closes it and opens a new one.
#![cfg(target_os = "linux")]

use std::ffi::c_char;
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use camera_box::jxs_recv::{JxsReceiver, ReceiverConfig};

/// `CBJXS_ABI_VERSION` of `include/camera_box_jxs.h`.
pub const CBJXS_ABI_VERSION: u32 = 1;

/// `cbjxs_frame`: one decoded frame, planar 4:2:2 8 bit, the planes valid until the next
/// `cbjxs_next_frame` / `cbjxs_close` on the receiver.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CbjxsFrame {
    pub width: u32,
    pub height: u32,
    pub y: *const u8,
    pub u: *const u8,
    pub v: *const u8,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub timecode_100ns: i64,
    pub first_arrival_100ns: i64,
    pub last_arrival_100ns: i64,
    pub decoded_at_100ns: i64,
    pub lost_slices: u32,
    pub camera_id: u8,
    pub reserved: [u8; 3],
}

impl Default for CbjxsFrame {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            y: std::ptr::null(),
            u: std::ptr::null(),
            v: std::ptr::null(),
            y_stride: 0,
            uv_stride: 0,
            timecode_100ns: 0,
            first_arrival_100ns: 0,
            last_arrival_100ns: 0,
            decoded_at_100ns: 0,
            lost_slices: 0,
            camera_id: 0,
            reserved: [0; 3],
        }
    }
}

/// `cbjxs_stats`: the counters since the receiver opened.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CbjxsStats {
    pub frames: u64,
    pub concealed_frames: u64,
    pub lost_slices: u64,
    pub undecodable_frames: u64,
    pub datagrams: u64,
    pub bytes: u64,
    pub duplicate: u64,
    pub stale: u64,
    pub malformed: u64,
    pub resyncs: u64,
    pub foreign: u64,
    pub reports_sent: u64,
    pub report_errors: u64,
    pub header_changes: u64,
    pub dropped_unread: u64,
    pub camera_id: u8,
    pub reserved: [u8; 7],
}

/// `cbjxs_receiver`: opaque to C.
pub struct CbjxsReceiver {
    rx: JxsReceiver,
    port: u16,
    error: Option<String>,
    poisoned: bool,
}

/// Copy `msg` into a C buffer, truncated, always NUL-terminated when `len > 0`. Returns the bytes
/// written without the NUL.
///
/// # Safety
/// `buf` is NULL or valid for `len` bytes.
unsafe fn write_c(buf: *mut c_char, len: usize, msg: &str) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let n = msg.len().min(len - 1);
    std::ptr::copy_nonoverlapping(msg.as_ptr().cast::<c_char>(), buf, n);
    *buf.add(n) = 0;
    n
}

#[no_mangle]
pub extern "C" fn cbjxs_abi_version() -> u32 {
    CBJXS_ABI_VERSION
}

/// # Safety
/// `err` is NULL or valid for `err_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_open(
    port: u16,
    camera: u8,
    max_bpp: u8,
    err: *mut c_char,
    err_len: usize,
) -> *mut CbjxsReceiver {
    let opened = catch_unwind(|| {
        let mut cfg = ReceiverConfig::new(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)));
        cfg.camera = (camera != 0).then_some(camera);
        cfg.max_bpp = max_bpp;
        JxsReceiver::bind(cfg)
            .and_then(|rx| Ok((rx.local_addr()?.port(), rx)))
            .map_err(|e| format!("UDP 0.0.0.0:{port}: {e}"))
    });
    match opened {
        Ok(Ok((bound, rx))) => Box::into_raw(Box::new(CbjxsReceiver {
            rx,
            port: bound,
            error: None,
            poisoned: false,
        })),
        Ok(Err(msg)) => {
            write_c(err, err_len, &msg);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_c(err, err_len, "the receiver panicked while opening");
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `rx` is NULL or a live receiver from `cbjxs_open`.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_local_port(rx: *const CbjxsReceiver) -> u16 {
    rx.as_ref().map_or(0, |r| r.port)
}

/// # Safety
/// `rx` is NULL or a live receiver; `out` is NULL or valid for one `CbjxsFrame`.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_next_frame(
    rx: *mut CbjxsReceiver,
    timeout_ms: u32,
    out: *mut CbjxsFrame,
) -> i32 {
    let (Some(r), Some(out)) = (rx.as_mut(), out.as_mut()) else {
        return -2;
    };
    if r.poisoned {
        return -2;
    }
    let polled = catch_unwind(AssertUnwindSafe(|| {
        match r.rx.poll(Duration::from_millis(timeout_ms as u64)) {
            Ok(Some(f)) => {
                let p = &f.planes;
                *out = CbjxsFrame {
                    width: p.width(),
                    height: p.height(),
                    y: p.y().as_ptr(),
                    u: p.u().as_ptr(),
                    v: p.v().as_ptr(),
                    y_stride: p.width(),
                    uv_stride: p.chroma_width(),
                    timecode_100ns: f.timecode_100ns as i64,
                    first_arrival_100ns: f.first_arrival_100ns,
                    last_arrival_100ns: f.last_arrival_100ns,
                    decoded_at_100ns: f.decoded_at_100ns,
                    lost_slices: f.lost_slices,
                    camera_id: f.camera_id,
                    reserved: [0; 3],
                };
                Ok(1)
            }
            Ok(None) => Ok(0),
            Err(e) => Err(e.to_string()),
        }
    }));
    match polled {
        Ok(Ok(n)) => {
            if let Some(e) = r.rx.take_last_error() {
                r.error = Some(format!("decoder: {e}"));
            }
            n
        }
        Ok(Err(e)) => {
            r.error = Some(format!("socket: {e}"));
            -1
        }
        Err(_) => {
            r.poisoned = true;
            r.error = Some("the receiver panicked; close and reopen it".into());
            -2
        }
    }
}

/// # Safety
/// `rx` is NULL or a live receiver; `out` is NULL or valid for one `CbjxsStats`.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_get_stats(rx: *const CbjxsReceiver, out: *mut CbjxsStats) -> i32 {
    let (Some(r), Some(out)) = (rx.as_ref(), out.as_mut()) else {
        return -1;
    };
    let t = r.rx.totals();
    *out = CbjxsStats {
        frames: t.frames,
        concealed_frames: t.concealed_frames,
        lost_slices: t.lost_slices,
        undecodable_frames: t.undecodable_frames,
        datagrams: t.datagrams,
        bytes: t.bytes,
        duplicate: t.duplicate,
        stale: t.stale,
        malformed: t.malformed,
        resyncs: t.resyncs,
        foreign: t.foreign,
        reports_sent: t.reports_sent,
        report_errors: t.report_errors,
        header_changes: t.header_changes,
        dropped_unread: t.dropped_unread,
        camera_id: r.rx.camera().unwrap_or(0),
        reserved: [0; 7],
    };
    0
}

/// # Safety
/// `rx` is NULL or a live receiver; `buf` is NULL or valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_take_error(
    rx: *mut CbjxsReceiver,
    buf: *mut c_char,
    len: usize,
) -> usize {
    let Some(r) = rx.as_mut() else {
        return 0;
    };
    if buf.is_null() || len == 0 {
        return 0;
    }
    match r.error.take() {
        Some(e) => write_c(buf, len, &e),
        None => 0,
    }
}

/// # Safety
/// `rx` is NULL or a live receiver from `cbjxs_open`, not used again afterwards.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_close(rx: *mut CbjxsReceiver) {
    if !rx.is_null() {
        // a panic in a destructor must not unwind into C either
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(rx))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_c_string_is_truncated_and_always_terminated() {
        let mut b = [1 as c_char; 5];
        assert_eq!(unsafe { write_c(b.as_mut_ptr(), b.len(), "abcdefgh") }, 4);
        assert_eq!(b, [b'a', b'b', b'c', b'd', 0].map(|x| x as c_char));
        let mut one = [9 as c_char; 1];
        assert_eq!(unsafe { write_c(one.as_mut_ptr(), 1, "x") }, 0);
        assert_eq!(one[0], 0);
        assert_eq!(unsafe { write_c(std::ptr::null_mut(), 9, "x") }, 0);
    }

    #[test]
    fn take_error_hands_each_error_out_once() {
        let rx = unsafe { cbjxs_open(0, 0, 0, std::ptr::null_mut(), 0) };
        assert!(!rx.is_null());
        unsafe { (*rx).error = Some("socket: boom".into()) };
        let mut b = [0 as c_char; 32];
        assert_eq!(unsafe { cbjxs_take_error(rx, b.as_mut_ptr(), b.len()) }, 12);
        assert_eq!(unsafe { cbjxs_take_error(rx, b.as_mut_ptr(), b.len()) }, 0);
        unsafe { cbjxs_close(rx) };
    }

    #[test]
    fn a_poisoned_receiver_refuses_every_poll() {
        let rx = unsafe { cbjxs_open(0, 0, 0, std::ptr::null_mut(), 0) };
        unsafe { (*rx).poisoned = true };
        let mut f = CbjxsFrame::default();
        assert_eq!(unsafe { cbjxs_next_frame(rx, 0, &mut f) }, -2);
        unsafe { cbjxs_close(rx) };
    }
}
